//! The Explorer run: the core's `ExplorerHostService` in its `RunHost` on the station
//! the run arms on; a sink that keeps what the pump hands on (each block's control
//! samples with their input frame, each display window, the survey line, the intents,
//! the pump's notes); and the run's own thread, which composes the view from the host's
//! snapshot ten times a second, publishes it and logs. The run's record
//! ([`crate::record`]) is written on its own writer thread: the run's thread hands it
//! entries and results with sends that never wait, and the streams are taken where the
//! station's blocks pass ([`crate::record::tap`]), into buffers made before the run.
//! Nothing here writes a file on the pump: the pump is the station's reading thread
//! and plays the output.
//!
//! The verbs reach the host directly. Each takes the service lock, which the pump holds
//! for one batch and one analysis pass, so the server runs them on the blocking pool
//! with a bounded wait.

use super::config::{round, Derived, WireExplorer};
use super::records::{self, StartFacts};
use super::view::{ControlMotion, End, ExplorerView, Health, RecordRef, Stall, State, Sweep, Trip};
use crate::protocol::{ErrorCode, MethodError};
use crate::pulse::rails::{now_unix, Shared};
use crate::random::record as rrec;
use crate::record::tap::{self, Recording};
use crate::record::{ProblemFn, RecordOptions, RecordStatus, RunRecord};
use crate::sine::profile::sig;
use crate::station::{self, NiScanner, StationPlan};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_explorer_host::analysis::SpectrumFrame;
use hertzforge_explorer_host::host::{HostNote, HostSink, HostSnapshot, HostView, Pacing, RunHost};
use hertzforge_explorer_host::{
    ExplorerHostService, ExplorerStatus, IntentKind, IntentRecord, RunState, StopCause,
    SweepDirection,
};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How often the run's thread composes and publishes the view.
pub const PUBLISH_PERIOD: Duration = Duration::from_millis(50);
/// Control samples kept for the time plots.
pub const TIME_KEEP: usize = 8192;
/// How long a start waits for the station to open and the tone to run.
pub const START_WAIT: Duration = Duration::from_secs(120);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn refuse(reason: &'static str, message: impl Into<String>) -> MethodError {
    MethodError::named(ErrorCode::Refused, reason, message)
}

/// The refusal of a verb in a state it does not apply in.
pub fn wrong_state(v: &ExplorerView, verb: &str) -> MethodError {
    refuse(
        "wrong_state",
        format!(
            "{verb} does not apply while the run is {} ({}).",
            v.state.name(),
            v.phase.replace('_', " ")
        ),
    )
    .with_detail(json!({"state": v.state.name(), "phase": v.phase}))
}

/// What the pump has handed on.
#[derive(Default)]
struct Data {
    survey_revision: u64,
    survey: Vec<(f64, f32)>,
    spectra: Vec<(u32, f64, Vec<f64>)>,
    time: VecDeque<f32>,
    /// The input frame after the last control sample kept.
    time_end: u64,
    intents: Vec<IntentRecord>,
    notes: Vec<(&'static str, String)>,
    /// The input frames acquired when the stop began.
    stop_input_frame: Option<u64>,
    /// The stop intent's generated frame: where the ramp began.
    stop_frame: Option<u64>,
    pump_done: bool,
}

/// The pump's hand-ons, shared with the run's thread and the server.
#[derive(Default)]
pub struct RunData {
    data: Mutex<Data>,
}

impl RunData {
    /// The survey's revision and its points, `(hz, level)`.
    pub fn survey(&self) -> (u64, Vec<(f64, f64)>) {
        let d = lock(&self.data);
        (
            d.survey_revision,
            d.survey.iter().map(|(f, l)| (*f, f64::from(*l))).collect(),
        )
    }

    pub fn spectra(&self) -> Vec<(u32, f64, Vec<f64>)> {
        lock(&self.data).spectra.clone()
    }

    /// The control row's kept samples: `(first input frame, samples)`.
    pub fn time(&self) -> (u64, Vec<f64>) {
        let d = lock(&self.data);
        let n = d.time.len() as u64;
        (
            d.time_end.saturating_sub(n),
            d.time.iter().map(|v| f64::from(*v)).collect(),
        )
    }

    fn take_intents(&self) -> Vec<IntentRecord> {
        std::mem::take(&mut lock(&self.data).intents)
    }

    fn take_notes(&self) -> Vec<(&'static str, String)> {
        std::mem::take(&mut lock(&self.data).notes)
    }

    fn stop_input_frame(&self) -> Option<u64> {
        lock(&self.data).stop_input_frame
    }

    fn stop_frame(&self) -> Option<u64> {
        lock(&self.data).stop_frame
    }

    fn pump_done(&self) -> bool {
        lock(&self.data).pump_done
    }
}

/// What the pump hands on, kept for the views and the records. Every call is short:
/// the pump holds the service lock for the lock-held ones.
struct ViewSink {
    data: Arc<RunData>,
    control_slot: usize,
}

impl HostSink for ViewSink {
    fn armed(&mut self, _fs_hz: f64, _display_nfft: usize) {}

    fn block(&mut self, _samples: &[f32], _stride: usize) {}

    fn block_at(&mut self, first_frame: u64, samples: &[f32], stride: usize) {
        let stride = stride.max(1);
        let frames = samples.len() / stride;
        let mut d = lock(&self.data.data);
        if d.time_end != first_frame {
            d.time.clear();
        }
        let slot = self.control_slot.min(stride - 1);
        d.time
            .extend((0..frames).map(|f| samples[f * stride + slot]));
        let excess = d.time.len().saturating_sub(TIME_KEEP);
        d.time.drain(..excess);
        d.time_end = first_frame + frames as u64;
    }

    fn dropped(&mut self, _frames: u64) {}

    fn spectra(&mut self, frames: &[SpectrumFrame], _profile: Option<&[f32]>) -> bool {
        let mut d = lock(&self.data.data);
        d.spectra = frames
            .iter()
            .map(|f| {
                (
                    f.ch,
                    f.df_hz,
                    f.mags.iter().map(|m| f64::from(*m)).collect(),
                )
            })
            .collect();
        true
    }

    fn survey(&mut self, revision: u64, points: &[(f64, f32)]) {
        let mut d = lock(&self.data.data);
        d.survey_revision = revision;
        d.survey = points.to_vec();
    }

    fn intents(&mut self, intents: &[IntentRecord]) {
        let mut d = lock(&self.data.data);
        for i in intents {
            if matches!(i.kind, IntentKind::Stop { .. }) && d.stop_input_frame.is_none() {
                d.stop_input_frame = Some(i.input_frame);
                d.stop_frame = Some(i.frame);
            }
        }
        d.intents.extend_from_slice(intents);
    }

    fn publish(&mut self) {}

    fn finish(&mut self) {}

    fn note(&mut self, note: HostNote<'_>) {
        let line = match note {
            HostNote::DriverWarning(w) => Some(("note", format!("driver: {w}"))),
            HostNote::LoopFaulted(e) => Some(("trip", format!("the run's loop faulted: {e}"))),
            HostNote::RingDropping(n) => Some((
                "overload",
                format!("the analysis has dropped {n} display blocks so far"),
            )),
            HostNote::PumpPanicked(w) => Some(("trip", format!("the run's loop panicked: {w}"))),
            HostNote::PumpExited { .. } => {
                lock(&self.data.data).pump_done = true;
                None
            }
            _ => None,
        };
        if let Some(l) = line {
            lock(&self.data.data).notes.push(l);
        }
    }
}

/// What a run starts on.
pub struct StartContext {
    pub table: ChannelTable,
    pub station: StationPlan,
    pub scanner: Arc<dyn NiScanner>,
    /// Where the hardware lock lives.
    pub data_root: PathBuf,
    /// Where the run's folder is made.
    pub records_root: PathBuf,
    pub bind: String,
    /// The table's revision, for the run's record.
    pub revision: u64,
    /// The devices of the last scan, for the record's models and serial numbers.
    pub devices: Vec<hertzforge_hal::DiscoveredDevice>,
    /// How the run's record is written.
    pub record: RecordOptions,
}

/// The service's handle on one configured run.
pub struct ExplorerRun {
    pub id: String,
    pub shared: Arc<Shared<ExplorerView>>,
    pub data: Arc<RunData>,
    pub host: Arc<RunHost>,
    /// The configuration as accepted, and its derived values.
    pub config: serde_json::Value,
    pub derived: serde_json::Value,
    pub derived_figures: Derived,
    pub wire: Arc<WireExplorer>,
    /// The configured view every later one is composed from.
    pub base: ExplorerView,
    started: AtomicBool,
    operator_abort: AtomicBool,
}

impl ExplorerRun {
    pub fn new(
        id: String,
        shared: Arc<Shared<ExplorerView>>,
        wire: Arc<WireExplorer>,
        derived: Derived,
        base: ExplorerView,
    ) -> ExplorerRun {
        ExplorerRun {
            id,
            shared,
            data: Arc::new(RunData::default()),
            host: Arc::new(RunHost::new()),
            config: wire.raw.clone(),
            derived: derived.to_json(),
            derived_figures: derived,
            wire,
            base,
            started: AtomicBool::new(false),
            operator_abort: AtomicBool::new(false),
        }
    }

    pub fn view(&self) -> Arc<ExplorerView> {
        self.shared.view()
    }

    /// Whether `explorer.start` has been taken (a run starts once).
    pub fn started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Show the run starting: before anything is opened, under the service's lock, so
    /// no other run can begin beside it.
    pub fn mark_starting(&self) {
        self.started.store(true, Ordering::SeqCst);
        let mut v = (*self.view()).clone();
        v.state = State::Starting;
        v.phase = "priming";
        self.shared.publish(&self.id, v);
    }

    /// An abort from a connection: the end reads `operator_abort`.
    pub fn note_operator_abort(&self) {
        self.operator_abort.store(true, Ordering::SeqCst);
    }

    /// Open the station, install the core, start the pump and the run's thread, and
    /// wait until the tone runs at 0 V (or the start ends the run). Blocking.
    pub fn start(self: &Arc<Self>, ctx: StartContext) -> Result<(), MethodError> {
        let built = (|| {
            let station = station::build(&ctx.station, &*ctx.scanner)?;
            let hardware = station.station.is_hardware();
            let guard = if hardware {
                Some(
                    hertzforge_station::lock::acquire(
                        &ctx.data_root,
                        &ctx.bind,
                        &station.provenance.station_label,
                    )
                    .map_err(|e| match e {
                        hertzforge_station::lock::LockError::Held(h) => refuse(
                            "hardware_busy",
                            format!("Another program holds the NI hardware: {}", h.advice()),
                        )
                        .with_detail(json!({"holder": h.advice()})),
                        hertzforge_station::lock::LockError::Io(m) => {
                            refuse("arm_refused", format!("The station refused to arm: {m}"))
                        }
                    })?,
                )
            } else {
                None
            };
            let caps = station.backend.caps();
            let lookahead = caps
                .output_buffer
                .map(|c| c.min_lookahead_frames)
                .unwrap_or(4);
            let clock = match caps.clock {
                hertzforge_hal::ClockDomain::Shared => "shared",
                hertzforge_hal::ClockDomain::Independent => "independent",
                hertzforge_hal::ClockDomain::Unknown => "unknown",
            };
            let cfg = self.wire.build();
            let fs = cfg.fs_hz;
            // The streams are taken where the station's blocks pass: the inputs as read
            // and the drive as the device accepted it. Their buffers are made here,
            // before anything runs, and the taps never wait.
            let streams_off = self.streams_off(&ctx);
            let buffers = if streams_off.is_none() {
                rrec::tap_buffers(fs, ctx.record.stream_buffer_s)
            } else {
                1
            };
            let (inputs_tap, inputs_link) = tap::pair(
                rrec::input_rows(&ctx.table).len(),
                rrec::TAP_BLOCK_FRAMES,
                buffers,
            );
            let (drives_tap, drives_link) = tap::pair(
                rrec::drive_rows(&ctx.table).len(),
                rrec::TAP_BLOCK_FRAMES,
                buffers,
            );
            let class = station.station;
            let provenance = station.provenance.clone();
            let backend = Recording::wrap(
                station.backend,
                inputs_tap,
                drives_tap,
                self.shared.station_frame.clone(),
            );
            let svc = ExplorerHostService::new(cfg, ctx.table.clone(), backend, class)
                .map_err(|e| refuse("arm_refused", format!("The station refused to arm: {e}")))?
                .with_block_frames(hertzforge_hal::default_block_frames(fs));
            // The run's record, before the station is opened: refused here, the start
            // is refused and nothing has been driven.
            let kept = self.open_record(
                &ctx,
                class,
                &provenance,
                &caps,
                streams_off,
                (inputs_link, drives_link),
            )?;
            Ok::<_, MethodError>((svc, guard, lookahead, clock, hardware, kept))
        })();
        let (svc, guard, lookahead, clock, hardware, kept) = match built {
            Ok(b) => b,
            Err(e) => {
                self.end_before_start(&e);
                return Err(e);
            }
        };
        let control_slot = ctx
            .table
            .channels
            .iter()
            .filter(|c| c.enabled && c.role != ChannelRole::Drive)
            .position(|c| WireExplorer::control_row(&ctx.table).is_some_and(|r| r.index == c.index))
            .unwrap_or(0);
        let pacing = if hardware {
            Pacing::Station
        } else {
            Pacing::WallClock
        };
        self.host.install_paced(svc, None, pacing);
        let sink = ViewSink {
            data: self.data.clone(),
            control_slot,
        };
        if let Err(e) = self.host.start(sink) {
            let e = refuse("arm_refused", format!("The run could not start: {e:?}"));
            self.end_before_start(&e);
            self.shared.detach_record();
            let at = now_unix();
            kept.record
                .finish(records::end_map(&kept.run_uid, &self.view(), at, None));
            return Err(e);
        }
        let run = self.clone();
        let facts = Facts {
            lookahead,
            clock,
            status: kept.record.status(),
            record: Some(kept.record),
            run_uid: kept.run_uid,
            _guard: guard,
        };
        std::thread::Builder::new()
            .name(format!("explorer {}", self.id))
            .spawn(move || run.watch(facts))
            .map_err(|e| MethodError::internal(format!("could not start the run's thread: {e}")))?;
        // Answer once the tone runs at 0 V, or the start ends the run.
        let asked = Instant::now();
        loop {
            let v = self.view();
            match v.state {
                State::Running | State::Stopping => return Ok(()),
                State::Finished => {
                    let end = v.end.clone();
                    return Err(match end {
                        Some(e) if e.outcome == "arm_refused" => refuse("arm_refused", e.message),
                        Some(e) => refuse("arm_refused", e.message),
                        None => refuse("arm_refused", "The run ended as it started."),
                    });
                }
                _ => {}
            }
            if asked.elapsed() > START_WAIT {
                return Err(MethodError::internal(
                    "the station did not open in time; the run is still starting",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Why the run's streams are not recorded, or `None` when they are: switched off in
    /// the setup, or the disk under the free-space floor at start (the run goes on
    /// without them, as a control run does).
    fn streams_off(&self, ctx: &StartContext) -> Option<String> {
        if !self.wire.record_streams {
            return Some("switched off in the setup".to_owned());
        }
        match hertzforge_capture::free_space_bytes_for(&ctx.records_root) {
            Ok(free) if free < ctx.record.free_space_floor_bytes => Some(format!(
                "the disk had {} MB free, under the {} MB kept free",
                free / 1_000_000,
                ctx.record.free_space_floor_bytes / 1_000_000
            )),
            _ => None,
        }
    }

    /// Open the run's record: `run.json`, `table.json`, `setup.json` and the log so
    /// far, on the record's own thread, waiting for them. `record_unwritable` when they
    /// cannot be written. From here on every log entry is appended to it as it is made,
    /// and the state names its folder.
    fn open_record(
        &self,
        ctx: &StartContext,
        class: hertzforge_module_abi::StationClass,
        provenance: &hertzforge_station::ProvenanceBasis,
        caps: &hertzforge_hal::Caps,
        streams_off: Option<String>,
        links: (tap::TapLink, tap::TapLink),
    ) -> Result<Kept, MethodError> {
        let started_unix = now_unix();
        let run_uid = rrec::run_uid(started_unix, &self.id);
        let facts = StartFacts {
            run_id: &self.id,
            run_uid: &run_uid,
            wire: &self.wire,
            table: &ctx.table,
            revision: ctx.revision,
            plan: &ctx.station,
            class,
            provenance,
            devices: &ctx.devices,
            caps,
            derived: &self.derived_figures,
            started_unix,
            utc_offset_s: rrec::utc_offset_s(),
            streams_off: streams_off.clone(),
        };
        let begin = records::begin(&facts, ctx.records_root.clone(), links);
        let shared = self.shared.clone();
        // A write that fails is a `record` entry in the log, and the state's
        // `record.problem` (copied from the record's status by the run's thread); the
        // run goes on.
        let problem: ProblemFn = Box::new(move |words: String, data: Value| {
            shared.add_log_data("record", words, Some(data));
        });
        let record = RunRecord::open(begin, &ctx.record, problem).map_err(|e| {
            refuse(
                "record_unwritable",
                format!(
                    "The run's record could not be written, so the run did not start: {} ({}). \
                     Choose a data folder that can be written.",
                    e.path.display(),
                    e.reason
                ),
            )
            .with_detail(json!({"path": e.path.display().to_string(), "reason": e.reason}))
        })?;
        self.shared.attach_record(record.log());
        let folder = record.folder().display().to_string();
        if let Some(why) = &streams_off {
            self.shared.add_log_data(
                "record",
                format!("the run's streams are not recorded: {why}"),
                Some(json!({"stream": null, "failed": why})),
            );
        }
        self.shared.add_log("note", format!("records in {folder}"));
        let mut v = (*self.view()).clone();
        v.record = Some(RecordRef {
            folder,
            streams: streams_off.is_none(),
            problem: None,
        });
        self.shared.publish(&self.id, v);
        Ok(Kept { record, run_uid })
    }

    /// A start refused before the core was installed: the run ends `arm_refused`.
    fn end_before_start(&self, e: &MethodError) {
        let mut v = (*self.view()).clone();
        v.state = State::Finished;
        v.end = Some(End {
            outcome: "arm_refused",
            message: e.message.clone(),
            at_s: None,
            at_hz: None,
            vpk: None,
            ramp_s: None,
            trip: None,
        });
        v.phase = "faulted";
        self.shared
            .add_log("state", format!("start refused: {}", e.message));
        self.shared.publish(&self.id, v);
    }

    /// The run's own thread: compose and publish the view, log, write the records.
    fn watch(self: Arc<Self>, mut facts: Facts) {
        let mut track = Track::default();
        let mut wrote_head = false;
        let mut finished_at: Option<Instant> = None;
        loop {
            std::thread::sleep(PUBLISH_PERIOD);
            let snap = self.host.snapshot();
            let prev = self.view();
            let mut v = self.compose(&snap, &prev, &facts, &mut track);
            if let Some(r) = v.record.as_mut() {
                r.problem = facts.status.problem();
            }
            if !wrote_head && v.grid.actual {
                wrote_head = true;
                // The station has programmed its rate: the manifest's grid and the
                // streams' rate become the station's own.
                if let Some(record) = &facts.record {
                    let mut grid = Map::new();
                    grid.insert("grid".into(), records::grid_json(&v));
                    record.patch_manifest(grid);
                    let streams = if v.record.as_ref().is_some_and(|r| r.streams) {
                        2
                    } else {
                        0
                    };
                    for k in 0..streams {
                        let mut patch = Map::new();
                        patch.insert("fs_hz".into(), crate::protocol::num(v.grid.fs_hz));
                        record.patch_stream(k, patch);
                    }
                }
                self.shared.add_log(
                    "station",
                    format!(
                        "the station programmed {} Hz (asked {} Hz); the band is {} to {} Hz",
                        crate::protocol::num(v.grid.fs_hz),
                        crate::protocol::num(v.grid.requested_fs_hz),
                        crate::protocol::num(v.band_hz.0),
                        crate::protocol::num(v.band_hz.1)
                    ),
                );
            }
            let fs = v.grid.fs_hz.max(1.0);
            for i in self.data.take_intents() {
                self.shared.add_log_data(
                    "intent",
                    records::intent_words(&i),
                    Some(records::intent_json(&i, fs)),
                );
            }
            for (kind, text) in self.data.take_notes() {
                self.shared.add_log(kind, text);
            }
            if v.stop.is_some() && prev.stop.is_none() {
                self.shared.add_log(
                    "state",
                    format!("stop began: {}", v.stop.unwrap_or("operator")),
                );
            }
            let done = v.state == State::Finished;
            if done && prev.state != State::Finished {
                if let Some(e) = &v.end {
                    self.shared
                        .add_log("state", format!("ended {}: {}", e.outcome, e.message));
                }
            }
            self.shared.publish(&self.id, v.clone());
            if done {
                let at = *finished_at.get_or_insert_with(Instant::now);
                let pump_done = self.data.pump_done();
                if pump_done || at.elapsed() > Duration::from_secs(5) {
                    // The pump has gone: the service goes with the station, and the
                    // streams' taps hand the writer their last frames. A pump that has
                    // not gone is left alone (a stuck driver is not waited on).
                    if pump_done {
                        self.host.release_idle();
                    }
                    self.finish_record(&mut facts, &v);
                    // One last publish, so the final view has every log entry.
                    self.shared.publish(&self.id, v);
                    break;
                }
            }
        }
    }

    /// The run's end, to the record: `end.json`. Sends only; the writer finishes on its
    /// own.
    fn finish_record(&self, facts: &mut Facts, v: &ExplorerView) {
        let Some(record) = facts.record.take() else {
            return;
        };
        self.shared.detach_record();
        let frames = self.shared.station_frame.load(Ordering::Relaxed);
        let frames = (frames != crate::random::run::NO_FRAME).then_some(frames);
        record.finish(records::end_map(&facts.run_uid, v, now_unix(), frames));
    }

    fn compose(
        &self,
        snap: &HostSnapshot,
        prev: &ExplorerView,
        facts: &Facts,
        track: &mut Track,
    ) -> ExplorerView {
        let mut v = prev.clone();
        v.stall = None;
        match &snap.view {
            HostView::Unconfigured | HostView::Configured | HostView::Priming => {
                if v.state != State::Finished {
                    v.state = State::Starting;
                    v.phase = "priming";
                }
            }
            HostView::Faulted(text) => {
                if v.state != State::Finished {
                    let (outcome, message) = match text.strip_prefix("arm_refused: ") {
                        Some(m) => ("arm_refused", format!("The station refused to arm: {m}")),
                        None => ("fault", text.clone()),
                    };
                    v.state = State::Finished;
                    v.end = Some(End {
                        outcome,
                        message,
                        at_s: v.t_s,
                        at_hz: v.frequency_hz,
                        vpk: v.drive_vpk,
                        ramp_s: None,
                        trip: None,
                    });
                    v.phase = "faulted";
                }
            }
            HostView::Stalled {
                stale_ms,
                period_ms,
            } => {
                v.stall = Some(Stall {
                    stale_s: round(*stale_ms as f64 / 1000.0, 3),
                    period_s: round(*period_ms as f64 / 1000.0, 3),
                    message: format!(
                        "The run's loop has not turned for {} s; the station may still be \
                         driving. Abort stays open.",
                        crate::protocol::num(round(*stale_ms as f64 / 1000.0, 1))
                    ),
                });
            }
            HostView::Status(st) => self.apply_status(&mut v, st, facts, track),
        }
        v
    }

    fn apply_status(
        &self,
        v: &mut ExplorerView,
        st: &ExplorerStatus,
        facts: &Facts,
        track: &mut Track,
    ) {
        let fs = st.actual_fs_hz.max(f64::MIN_POSITIVE);
        v.grid.requested_fs_hz = st.requested_fs_hz;
        v.grid.fs_hz = st.actual_fs_hz;
        v.grid.lookahead_frames = facts.lookahead;
        v.grid.actual = true;
        v.clock = Some(facts.clock);
        v.band_hz = (round(st.band_hz.0, 3), round(st.band_hz.1, 3));
        v.pipeline_frames = Some(st.pipeline_frames as u64);
        v.t_s = Some(round(st.frames_in as f64 / fs, 4));
        v.frequency_hz = Some(round(st.commanded_freq_hz, 3));
        v.target_hz = Some(round(st.target_freq_hz, 3));
        v.drive_vpk = Some(round(st.commanded_vpk, 6));
        v.target_vpk = Some(round(st.target_vpk, 6));
        v.sweep = st.sweep.map(|s| Sweep {
            direction: match s.direction {
                SweepDirection::Up => "up",
                SweepDirection::Down => "down",
            },
            rate_oct_per_min: round(s.rate_oct_per_min, 6),
            halted: s.halted,
        });
        let ms = |d: Duration| round(d.as_secs_f64() * 1e3, 2);
        v.health = Some(Health {
            engine_ticks: st.engine_ticks,
            worst_tick_ms: (st.engine_ticks > 0).then(|| ms(st.worst_tick_compute)),
            budget_ms: (st.compute_budget > Duration::ZERO).then(|| ms(st.compute_budget)),
            health_faults: st.health_faults,
            ring_overruns: st.ring_overruns as u64,
            blocks_dropped: st.blocks_dropped,
            driver_warnings: st.driver_warnings_total,
            pass_last_ms: (st.engine_ticks > 0).then(|| ms(st.pass_last)),
            pass_worst_ms: (st.engine_ticks > 0).then(|| ms(st.pass_worst)),
            batch_gap_ms: (st.batch_gap_worst > Duration::ZERO).then(|| ms(st.batch_gap_worst)),
        });
        v.stop = st.stop.map(StopCause::as_str);

        // The measured frequency, held while the tone it measured holds.
        if let Some(m) = st.measured_freq_hz {
            track.measured = Some((st.commanded_freq_hz, m));
        }
        if st.commanded_vpk <= 0.0
            || track
                .measured
                .is_some_and(|(at, _)| at != st.commanded_freq_hz)
        {
            track.measured = None;
        }
        v.measured_hz = track.measured.map(|(_, m)| round(m, 3));
        if !st.per_channel_rms.is_empty() {
            for (c, rms) in v.channels.iter_mut().zip(&st.per_channel_rms) {
                c.rms = Some(sig(*rms, 4));
            }
        }
        if let Some(c) = v.control.as_mut() {
            let in_g = self.base.control_unit == "g";
            *c = ControlMotion {
                index: c.index,
                velocity_ips: in_g.then(|| sig(st.derived_peak.0, 4)),
                displacement_mil: in_g.then(|| sig(st.derived_peak.1, 4)),
                settling: st.derived_settling,
            };
        }
        v.peak = st
            .survey_peak
            .map(|(f, l)| (round(f, 3), sig(f64::from(l), 6)));

        // The state and its phase.
        let edge = |hz: f64| {
            (hz - st.band_hz.0).abs() <= 1e-9 * hz || (hz - st.band_hz.1).abs() <= 1e-9 * hz
        };
        match st.state {
            RunState::Idle | RunState::Armed => {
                v.state = State::Starting;
                v.phase = "priming";
            }
            RunState::Running => {
                v.state = State::Running;
                v.phase = if st.commanded_vpk == 0.0 && st.target_vpk == 0.0 {
                    "quiet"
                } else if st.commanded_vpk != st.target_vpk {
                    "ramping"
                } else {
                    match st.sweep {
                        Some(s) if !s.halted => "sweeping",
                        Some(s) if s.halted && edge(st.commanded_freq_hz) => "at_edge",
                        _ => "steady",
                    }
                };
                track.tone = (v.frequency_hz, v.drive_vpk);
            }
            RunState::Stopping => {
                v.state = State::Stopping;
                v.phase = "ramping_down";
            }
            RunState::Stopped | RunState::Aborted => {
                if v.state != State::Finished {
                    v.end = Some(self.end_of(st, fs, track));
                    v.state = State::Finished;
                    v.phase = v.end.as_ref().map_or("stopped", End::phase);
                }
            }
        }
    }

    /// The end of a run the core ended, in the wire's words.
    fn end_of(&self, st: &ExplorerStatus, fs: f64, track: &Track) -> End {
        let rec = st.end.clone();
        let at_s = Some(round(st.frames_in as f64 / fs, 4));
        let (at_hz, vpk) = track.tone;
        // The ramp's length: from the stop's frame to its first zero, both generated. The
        // run ends once that zero has played, a pipeline later, so the end's input frame
        // less the stop's would add the pipeline; that is the fallback where the zero's
        // frame is not known.
        let ramp_s = match (self.data.stop_frame(), st.stop_zero_frame) {
            (Some(f), Some(z)) => Some(round(z.saturating_sub(f) as f64 / fs, 4)),
            _ => self
                .data
                .stop_input_frame()
                .map(|f| round(st.frames_in.saturating_sub(f) as f64 / fs, 4)),
        };
        let by = self
            .shared
            .ended_by()
            .unwrap_or_else(|| "the operator".into());
        // The core's axis, in the wire's word.
        let axis = |a: &dyn std::fmt::Debug| match format!("{a:?}").as_str() {
            "Velocity" => "velocity",
            "Displacement" => "displacement",
            _ => "accel",
        };
        let mut end = End {
            outcome: "fault",
            message: String::new(),
            at_s,
            at_hz,
            vpk,
            ramp_s: None,
            trip: None,
        };
        let Some(rec) = rec else {
            end.message = "The run ended.".into();
            return end;
        };
        if rec.state == RunState::Stopped {
            end.ramp_s = ramp_s;
            let tail = "the controlled stop ramped the drive to 0 V";
            match rec.stop.unwrap_or(StopCause::Operator) {
                StopCause::Operator => {
                    end.outcome = "operator_stop";
                    end.message = format!("Stopped by {by}: {tail}.");
                }
                StopCause::SafeStop => {
                    end.outcome = "safe_stop";
                    end.message = format!("Nobody held control: {tail}.");
                }
                StopCause::Shutdown => {
                    end.outcome = "safe_stop";
                    end.message = format!("The service shut down: {tail}.");
                }
                StopCause::Limit => {
                    end.outcome = "limit_stop";
                    match rec.trip {
                        Some(t) => {
                            end.message = format!(
                                "Channel {} {} {} passed its limit {}: {tail}.",
                                t.channel_index,
                                axis(&t.axis),
                                crate::protocol::num(sig(t.value, 4)),
                                crate::protocol::num(t.limit)
                            );
                            end.trip = Some(Trip::Limit {
                                channel: t.channel_index,
                                axis: axis(&t.axis),
                                value: sig(t.value, 6),
                                limit: t.limit,
                            });
                        }
                        None => end.message = format!("A limit passed: {tail}."),
                    }
                }
            }
            return end;
        }
        // Aborted.
        if let Some(d) = rec.drive {
            end.outcome = "aborted";
            end.message = format!(
                "A drive sample of {} V pk passed the {} V pk ceiling; it was clipped and the \
                 run aborted.",
                crate::protocol::num(sig(d.vpk, 4)),
                crate::protocol::num(d.ceiling_vpk)
            );
            end.trip = Some(Trip::DriveVpk {
                vpk: sig(d.vpk, 6),
                ceiling_vpk: d.ceiling_vpk,
            });
        } else if let Some(t) = rec.trip {
            end.outcome = "aborted";
            end.message = format!(
                "Channel {} {} {} passed its abort limit {}.",
                t.channel_index,
                axis(&t.axis),
                crate::protocol::num(sig(t.value, 4)),
                crate::protocol::num(t.limit)
            );
            end.trip = Some(Trip::Watchdog {
                channel: t.channel_index,
                axis: axis(&t.axis),
                value: sig(t.value, 6),
                limit: t.limit,
            });
        } else if self.operator_abort.load(Ordering::SeqCst) {
            end.outcome = "operator_abort";
            end.message = format!("Aborted by {by}.");
        } else {
            end.outcome = "fault";
            end.message = rec.reason;
        }
        end
    }
}

/// What the run's thread knows of the armed station, and holds for the run.
struct Facts {
    lookahead: u32,
    clock: &'static str,
    /// The run's record until its end is sent.
    record: Option<RunRecord>,
    status: Arc<RecordStatus>,
    run_uid: String,
    _guard: Option<hertzforge_station::lock::LockGuard>,
}

/// The record opened at start, and the run's identity in it.
struct Kept {
    record: RunRecord,
    run_uid: String,
}

/// What the view composer carries from one snapshot to the next.
#[derive(Default)]
struct Track {
    /// The last measured frequency and the tone it was measured at.
    measured: Option<(f64, f64)>,
    /// The tone at the last running snapshot: where the end began.
    tone: (Option<f64>, Option<f64>),
}
