//! The Mixed Mode run thread: it owns the controller's `MixedService`, takes the
//! operator's requests between the controller's chunks (about a quarter second of
//! station time each) and publishes the run's view. It never holds a lock shared
//! with the socket's threads across a chunk. Stop and abort are raised on the run's
//! signals first, so identification ends at its next block and an abort reaches
//! the station at once. When the run ends the service and the station are dropped
//! (the hardware freed) before the end is published.

use super::config::{LawFacts, WireMixed};
use super::record::{self as mrec, PartPoint};
use super::view::{self, End, IdentFacts, LiveChannel, MixedView, State, StationRef};
use super::{refuse, wrong_state};
use crate::protocol::{num, MethodError};
use crate::random::backend::{Interruptible, ABORT, NONE};
use crate::random::record as rrec;
use crate::random::run::{LevelTarget, LogEntry, NO_FRAME};
use crate::random::view::RecordRef;
use crate::record::tap::{self, Recording};
use crate::record::{LogHandle, ProblemFn, RecordOptions, RecordStatus, RunRecord};
use crate::sine::record::Blocks;
use crate::station::NiScanner;
use crate::station::{self, class_name, StationPlan};
use crate::table::unit_name;
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_mixed::service::law_records::MixedLawRecord;
use hertzforge_mixed::{
    LawRole, MixedAdjustRefusal, MixedConfig, MixedDisplay, MixedEventKind, MixedLawFactories,
    MixedOutcome, MixedService, MixedState, MixedStatus, PartKind,
};
use hertzforge_random::RunMode;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// How often a running test publishes its view.
pub const PUBLISH_PERIOD: Duration = Duration::from_millis(200);
const SYSID_SEED: u64 = 0x5EED_0003;
pub const SHUTDOWN_BY: &str = crate::random::run::SHUTDOWN_BY;

/// A publish, as the service hears it: the run id and the view published.
pub type Notice = (String, Arc<MixedView>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// What the run thread and the service share.
pub struct RunShared {
    view: Mutex<Arc<MixedView>>,
    log: Mutex<Vec<LogEntry>>,
    /// The station signal: [`ABORT`] takes the drive down with the station's own
    /// stop at the next engine block.
    pub signal: Arc<AtomicU8>,
    /// A stop asked from the socket's thread: identification ends at its next block.
    pub stop_asked: AtomicBool,
    /// The controller's cancel flag for identification, once the run has armed.
    pub sysid_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Who sent the stop or abort.
    pub ended_by: Mutex<Option<String>>,
    notify: tokio::sync::mpsc::UnboundedSender<Notice>,
    /// The `mixed.live` task has been started for this run.
    pub live_started: AtomicBool,
    /// The run's record from arm on: every log entry is appended to it as it is made.
    pub record: Mutex<Option<LogHandle>>,
    /// The input frames the station has delivered since it started
    /// ([`NO_FRAME`] before the first), for the record's entries.
    pub station_frame: Arc<AtomicU64>,
}

impl RunShared {
    pub fn new(
        initial: MixedView,
        notify: tokio::sync::mpsc::UnboundedSender<Notice>,
    ) -> RunShared {
        RunShared {
            view: Mutex::new(Arc::new(initial)),
            log: Mutex::new(Vec::new()),
            signal: Arc::new(AtomicU8::new(NONE)),
            stop_asked: AtomicBool::new(false),
            sysid_cancel: Mutex::new(None),
            ended_by: Mutex::new(None),
            notify,
            live_started: AtomicBool::new(false),
            record: Mutex::new(None),
            station_frame: Arc::new(AtomicU64::new(NO_FRAME)),
        }
    }

    pub fn view(&self) -> Arc<MixedView> {
        lock(&self.view).clone()
    }

    pub fn log_since(&self, since: u64) -> (Vec<LogEntry>, u64) {
        let log = lock(&self.log);
        let next = log.len() as u64;
        let from = (since as usize).min(log.len());
        (log[from..].to_vec(), next)
    }

    pub fn add_log(&self, kind: &'static str, text: impl Into<String>) {
        self.add_log_data(kind, text, None);
    }

    /// A log entry with structured facts for the record (`data`): the level of a
    /// level change, a part switched, a trip's facts, what an operator did.
    pub fn add_log_data(&self, kind: &'static str, text: impl Into<String>, data: Option<Value>) {
        let mut log = lock(&self.log);
        let seq = log.len() as u64;
        let entry = LogEntry {
            seq,
            at_unix: now_unix(),
            kind,
            text: text.into(),
        };
        // Appended in the order the entries are made: the hand-off never waits.
        if let Some(r) = lock(&self.record).as_ref() {
            r.append(self.record_entry(&entry, data));
        }
        log.push(entry);
    }

    /// An entry as the record holds it: the wire's members, the station frame
    /// when the station has delivered one, and `data` when there is any.
    pub fn record_entry(&self, e: &LogEntry, data: Option<Value>) -> Value {
        let frame = self.station_frame.load(Ordering::Relaxed);
        let mut v = json!({"seq": e.seq, "at_unix": e.at_unix,
                           "frame": if frame == NO_FRAME { Value::Null } else { Value::from(frame) },
                           "kind": e.kind, "text": e.text});
        if let (Some(d), Value::Object(m)) = (data, &mut v) {
            m.insert("data".into(), d);
        }
        v
    }

    /// The entries made so far, as the record holds them, and from now on every
    /// entry appended to the record as it is made. Done under the log's lock, so
    /// none is missed or written twice.
    pub fn attach_record(&self, handle: LogHandle) {
        let log = lock(&self.log);
        for e in log.iter() {
            handle.append(self.record_entry(e, None));
        }
        *lock(&self.record) = Some(handle);
    }

    /// Stop appending to the record (the run has ended).
    pub fn detach_record(&self) {
        let _log = lock(&self.log);
        *lock(&self.record) = None;
    }

    pub fn publish(&self, view: MixedView) {
        let id = view.run_id.clone();
        let view = Arc::new(view);
        *lock(&self.view) = view.clone();
        let _ = self.notify.send((id, view));
    }

    /// Ask the run to stop or abort from the socket's thread, before the request is
    /// queued.
    pub fn signal_end(&self, abort: bool, by: &str) {
        {
            let mut b = lock(&self.ended_by);
            if b.is_none() {
                *b = Some(by.to_owned());
            }
        }
        if abort {
            self.signal.store(ABORT, Ordering::SeqCst);
        }
        let in_sysid = {
            let v = self.view();
            v.state == State::Running && matches!(v.phase, "sysid" | "coherence_gate")
        };
        if abort || in_sysid {
            self.stop_asked.store(true, Ordering::SeqCst);
            if let Some(f) = lock(&self.sysid_cancel).clone() {
                f.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// What a run arms on, taken from the service when the request arrives.
pub struct ArmContext {
    pub table: ChannelTable,
    pub station: StationPlan,
    pub scanner: Arc<dyn NiScanner>,
    pub data_root: PathBuf,
    pub bind: String,
    /// The run's laws, resolved from the catalog; `Err` is why they could not be.
    pub laws: Result<MixedLawFactories, MethodError>,
    /// The table's revision, for the run's record.
    pub revision: u64,
    /// The devices of the last scan, for the record's models and serial numbers.
    pub devices: Vec<hertzforge_hal::DiscoveredDevice>,
}

type Reply<T> = oneshot::Sender<Result<T, MethodError>>;

/// A request to the run thread.
pub enum Cmd {
    Arm {
        ack: Option<String>,
        ctx: ArmContext,
        reply: Reply<()>,
    },
    Start {
        ctx: ArmContext,
        reply: Reply<()>,
    },
    Proceed {
        reply: Reply<()>,
    },
    Level {
        target: LevelTarget,
        reply: Reply<f64>,
    },
    Mode {
        mode: RunMode,
        reply: Reply<()>,
    },
    Hold {
        on: bool,
        reply: Reply<()>,
    },
    Part {
        id: String,
        on: bool,
        by: String,
        reply: Reply<bool>,
    },
    Adjust {
        key: String,
        value: f64,
        reply: Reply<f64>,
    },
    Stop {
        outcome: &'static str,
        by: String,
        reply: Option<Reply<()>>,
    },
    Abort {
        by: String,
        reply: Reply<()>,
    },
}

/// The fixed facts of one run.
pub struct RunPlan {
    pub run_id: String,
    pub wire: Arc<WireMixed>,
    pub core: Arc<MixedConfig>,
    pub law: LawFacts,
    /// Simulated seconds per wall second on the simulator; 0 runs unpaced (tests).
    pub sim_speed: f64,
    /// The data folder the run's record goes under (`None`: the local default).
    pub capture_root: Option<PathBuf>,
    /// The orphan timeout, for the safe stop's message.
    pub timeout_s: f64,
    /// How the run's record is written.
    pub record: RecordOptions,
    /// The configuration's derived values, as configure returned them.
    pub derived: Value,
}

/// What the record is kept with while the run goes.
struct Keeping {
    record: RunRecord,
    status: Arc<RecordStatus>,
    run_uid: String,
    utc_offset_s: Option<i64>,
    table: ChannelTable,
    control_unit: String,
    drive_unit: String,
    /// The control channels, in the order of the control set.
    controls: Vec<u32>,
    /// Every part in parts-list order, and the tones' ids.
    parts: Vec<(String, PartKind)>,
    tones: Vec<String>,
    levels: Blocks<PartPoint>,
    trace: Blocks<MixedLawRecord>,
    /// The background's spectra of the last chunk with its loop, the level in
    /// force and the station frame then; and the rung it was kept at.
    spectra: Option<(MixedDisplay, f64, u64)>,
    rung: Option<usize>,
    spectra_kept: u64,
    /// The station time of the last level point kept.
    last_t: f64,
    identified: bool,
}

struct Actor {
    plan: RunPlan,
    shared: Arc<RunShared>,
    svc: Option<MixedService>,
    lock: Option<hertzforge_station::lock::LockGuard>,
    view: MixedView,
    forwarded: usize,
    stop: Option<(&'static str, String)>,
    stop_at_t: Option<f64>,
    t_before: Option<f64>,
    last_publish: Option<Instant>,
    pace_origin: Option<(Instant, f64)>,
    /// Station time of the first engaged chunk; `elapsed_s` counts holds out.
    held_s: f64,
    last_t: Option<f64>,
    /// The level in force before any ramp down, for the end's `level_db`.
    driving_level: Option<f64>,
    /// The run's record, from arm until the run ends.
    keep: Option<Keeping>,
}

fn arm_refused(words: impl std::fmt::Display) -> MethodError {
    refuse("arm_refused", format!("The run cannot arm: {words}"))
}

/// Start the run thread. Returns once the thread exists; the run is `configured`.
pub fn spawn(
    plan: RunPlan,
    initial: MixedView,
    shared: Arc<RunShared>,
    rx: Receiver<Cmd>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let name = format!("mixed-{}", plan.run_id);
    std::thread::Builder::new().name(name).spawn(move || {
        let mut a = Actor {
            plan,
            shared,
            svc: None,
            lock: None,
            view: initial,
            forwarded: 0,
            stop: None,
            stop_at_t: None,
            t_before: None,
            last_publish: None,
            pace_origin: None,
            held_s: 0.0,
            last_t: None,
            driving_level: None,
            keep: None,
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.run(rx)));
        if r.is_err() {
            a.shared.signal.store(ABORT, Ordering::SeqCst);
            if let Some(svc) = a.svc.as_mut() {
                let _ = svc.abort("the run thread failed");
            }
            a.svc = None;
            a.lock = None;
            let msg = "The run stopped on a fault: its thread failed; the drive was taken \
                       down with the station's own stop."
                .to_owned();
            a.view.end = Some(End {
                outcome: "fault",
                message: msg.clone(),
                at_s: a.view.t_s,
                level_db: a.view.level_db,
                ramp_s: None,
                trip: None,
            });
            a.view.state = State::Finished;
            a.view.phase = "aborted";
            a.shared
                .add_log_data("note", msg, Some(json!({"outcome": "fault"})));
            a.finish_record();
            a.shared.publish(a.view.clone());
        }
    })
}

/// The enabled inputs of `table`, for the live frame.
pub fn live_channels(table: &ChannelTable) -> Vec<LiveChannel> {
    table
        .channels
        .iter()
        .filter(|c| c.enabled && c.role != ChannelRole::Drive)
        .map(|c| LiveChannel {
            index: c.index,
            description: c.description.clone(),
            role: crate::table::role_name(c.role),
            rms: None,
            unit: crate::table::unit_name(&c.units),
            overload: false,
        })
        .collect()
}

impl Actor {
    fn run(&mut self, rx: Receiver<Cmd>) {
        loop {
            match (self.view.state, self.view.phase) {
                (State::Running, "sysid") => self.identify(),
                (State::Running, _) | (State::Stopping, _) => {
                    loop {
                        match rx.try_recv() {
                            Ok(cmd) => self.handle(cmd),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                self.abandon();
                                return;
                            }
                        }
                    }
                    if matches!(self.view.state, State::Running | State::Stopping) {
                        self.pump();
                    }
                }
                _ => match rx.recv() {
                    Ok(cmd) => self.handle(cmd),
                    Err(_) => {
                        self.abandon();
                        return;
                    }
                },
            }
        }
    }

    /// The service dropped this run: release anything held, the drive first.
    fn abandon(&mut self) {
        if let Some(svc) = self.svc.as_mut() {
            self.shared.signal.store(ABORT, Ordering::SeqCst);
            let _ = svc.abort("the service released the run");
        }
        self.svc = None;
        self.lock = None;
        // The record keeps what it has, `complete: false`: the run did not end.
        self.shared.detach_record();
        self.keep = None;
    }

    fn publish(&mut self) {
        if let (Some(k), Some(r)) = (self.keep.as_ref(), self.view.record.as_mut()) {
            r.problem = k.status.problem();
        }
        self.last_publish = Some(Instant::now());
        self.shared.publish(self.view.clone());
    }

    fn set(&mut self, state: State, phase: &'static str) {
        if self.view.phase != phase {
            self.shared.add_log_data(
                "state",
                format!(
                    "{} to {}",
                    self.view.phase.replace('_', " "),
                    phase.replace('_', " ")
                ),
                Some(json!({"from": self.view.phase, "to": phase, "state": state.name()})),
            );
        }
        self.view.state = state;
        self.view.phase = phase;
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Arm { ack, ctx, reply } => {
                let r = self.arm_cmd(ack, ctx);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Start { ctx, reply } => {
                let r = self.start_cmd(ctx);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Proceed { reply } => {
                let r = self.proceed_cmd();
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Level { target, reply } => {
                let r = self.level_cmd(target);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Mode { mode, reply } => {
                let r = self.mode_cmd(mode);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Hold { on, reply } => {
                let r = self.hold_cmd(on);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Part { id, on, by, reply } => {
                let r = self.part_cmd(&id, on, &by);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Adjust { key, value, reply } => {
                let r = self.adjust_cmd(&key, value);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Stop { outcome, by, reply } => {
                self.stop_cmd(outcome, by);
                self.publish();
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            Cmd::Abort { by, reply } => {
                if self.view.state.is_live() {
                    self.abort_now(&by);
                }
                self.publish();
                let _ = reply.send(Ok(()));
            }
        }
    }

    // ---- arm and start -------------------------------------------------------------

    fn finished_refusal(&self) -> MethodError {
        refuse(
            "run_finished",
            "This run has finished; reset or configure again to run.",
        )
    }

    fn arm_cmd(&mut self, ack: Option<String>, ctx: ArmContext) -> Result<(), MethodError> {
        match self.view.state {
            State::Configured | State::Blocked => self.arm(ack, ctx),
            State::Finished => Err(self.finished_refusal()),
            _ => Err(wrong_state(&self.view, "Arming")),
        }
    }

    fn start_cmd(&mut self, ctx: ArmContext) -> Result<(), MethodError> {
        match self.view.state {
            State::Configured => self.arm(None, ctx)?,
            State::Armed => {}
            State::Finished => return Err(self.finished_refusal()),
            _ => return Err(wrong_state(&self.view, "Starting")),
        }
        self.shared.stop_asked.store(false, Ordering::SeqCst);
        self.set(State::Running, "sysid");
        self.view.streaming = true;
        if let Some(k) = self.keep.as_ref() {
            let frame = self.shared.station_frame.load(Ordering::Relaxed);
            let mut patch = Map::new();
            patch.insert(
                "start".into(),
                rrec::start_json(
                    now_unix(),
                    k.utc_offset_s,
                    if frame == NO_FRAME { 0 } else { frame },
                ),
            );
            k.record.patch_manifest(patch);
        }
        Ok(())
    }

    fn arm(&mut self, ack: Option<String>, ctx: ArmContext) -> Result<(), MethodError> {
        let mut laws = ctx.laws?;
        laws.floor_hinv.damping = self.plan.wire.random.control.damping;
        // The feasibility of the configuration as the core predicts it, against the
        // shaker's ratings: exceeded needs the operator's acknowledgement.
        let pred = hertzforge_mixed::predict(&self.plan.core).map_err(arm_refused)?;
        let shaker = self.plan.core.shaker.is_some();
        self.view.feasibility = Some(super::config::feasibility_of(&pred, shaker));
        if shaker && !pred.within_ratings() {
            match &ack {
                Some(a) => self.shared.add_log_data(
                    "override",
                    format!("operator acknowledgement: {a}"),
                    Some(json!({"what": "feasibility_exceeded", "acknowledgement": a})),
                ),
                None => {
                    self.set(State::Blocked, "feasibility_blocked");
                    let f = self
                        .view
                        .feasibility
                        .as_ref()
                        .map(|(v, m)| json!({"verdict": v, "message": m}));
                    return Err(refuse(
                        "feasibility_exceeded",
                        "Feasibility exceeded: the test asks more of the shaker than its \
                         ratings. Review it, or arm with an acknowledgement.",
                    )
                    .with_detail(json!({ "feasibility": f })));
                }
            }
        }
        let table = ctx.table.clone();
        let station = station::build(&ctx.station, &*ctx.scanner)?;
        let hardware = station.station.is_hardware();
        let lock = if hardware {
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
                    hertzforge_station::lock::LockError::Io(m) => arm_refused(m),
                })?,
            )
        } else {
            None
        };
        let class = station.station;
        let provenance = station.provenance.clone();
        let caps = station.backend.caps();
        let wire = self.plan.wire.clone();
        // The streams are taken where the station's blocks pass (record::tap): the
        // inputs as read and the drive as the device accepted it, from the first
        // block the station plays, the identification included. Their buffers are
        // made here, before anything runs.
        let buffers = if wire.record_streams {
            rrec::tap_buffers(wire.fs_hz(), self.plan.record.stream_buffer_s)
        } else {
            1
        };
        let (inputs_tap, inputs_link) = tap::pair(
            rrec::input_rows(&table).len(),
            rrec::TAP_BLOCK_FRAMES,
            buffers,
        );
        let (drives_tap, drives_link) = tap::pair(
            mrec::played_drives(&table).len(),
            rrec::TAP_BLOCK_FRAMES,
            buffers,
        );
        let recorded = Recording::wrap(
            station.backend,
            inputs_tap,
            drives_tap,
            self.shared.station_frame.clone(),
        );
        let backend = Interruptible::wrap(recorded, self.shared.signal.clone());
        let svc = MixedService::new(
            (*self.plan.core).clone(),
            table.clone(),
            backend,
            class,
            SYSID_SEED,
            self.plan.capture_root.clone(),
        )
        .map_err(arm_refused)?;
        // The station is asked for the setup's rate: a device's caps state its
        // highest rate, which is not the one the lines and the tick were set for.
        let svc = svc
            .with_requested_rate(self.plan.wire.fs_hz())
            .map_err(arm_refused)?;
        let svc = if hardware {
            svc
        } else {
            svc.with_block_frames(hertzforge_hal::default_block_frames(self.plan.wire.fs_hz()))
        };
        let mut svc = svc.with_laws(laws);
        svc.set_run_mode(self.view.mode).map_err(arm_refused)?;
        // The laws' calls are kept for the record, taken between chunks.
        svc.keep_law_records(true);
        *lock_cancel(&self.shared) = Some(svc.sysid_cancel_flag());
        svc.arm().map_err(arm_refused)?;
        self.forward_log(&svc);
        let fs = svc.sample_rate_hz();
        let tick_frames = crate::sine::config::tick_frames(fs, self.plan.wire.tick_ms);
        self.view.grid.fs_hz = fs;
        self.view.grid.df_hz = fs / self.view.grid.n_pts.max(1) as f64;
        self.view.grid.tick_frames = tick_frames;
        self.view.grid.actual = true;
        self.view.clock = Some("shared");
        self.view.station = StationRef {
            kind: if matches!(ctx.station, StationPlan::Sim { .. }) {
                "sim"
            } else {
                "ni"
            },
            class: class_name(class),
            devices: provenance
                .device_ids
                .iter()
                .cloned()
                .zip(provenance.serials.iter().cloned())
                .collect(),
        };
        self.view.channels = live_channels(&table);

        // The run's record, before any sample: refused here, the arm is refused and
        // the station released (nothing has been driven).
        let armed_unix = now_unix();
        let utc_offset_s = rrec::utc_offset_s();
        let run_uid = rrec::run_uid(armed_unix, &self.plan.run_id);
        let root = self
            .plan
            .capture_root
            .clone()
            .unwrap_or_else(hertzforge_capture::default_data_root);
        let streams_off = if !wire.record_streams {
            Some("switched off in the setup".to_owned())
        } else {
            match hertzforge_capture::free_space_bytes_for(&root) {
                Ok(free) if free < self.plan.record.free_space_floor_bytes => Some(format!(
                    "the disk had {} MB free, under the {} MB kept free",
                    free / 1_000_000,
                    self.plan.record.free_space_floor_bytes / 1_000_000
                )),
                _ => None,
            }
        };
        let df_hz = self.view.grid.df_hz;
        let facts = mrec::ArmFacts {
            run_id: &self.plan.run_id,
            run_uid: &run_uid,
            cfg: &wire,
            controller: svc.config(),
            table: &table,
            revision: ctx.revision,
            plan: &ctx.station,
            class,
            provenance: &provenance,
            devices: &ctx.devices,
            caps: &caps,
            fs_hz: fs,
            tick_frames,
            df_hz,
            derived: self.plan.derived.clone(),
            feasibility: self.view.feasibility.as_ref(),
            law: self.law_json(&svc),
            armed_unix,
            utc_offset_s,
            streams_off: streams_off.clone(),
        };
        let begin = mrec::begin(&facts, root, (inputs_link, drives_link));
        let shared = self.shared.clone();
        let problem: ProblemFn = Box::new(move |words: String, data: Value| {
            shared.add_log_data("record", words.clone(), Some(data));
            let mut v = (*shared.view()).clone();
            if let Some(r) = v.record.as_mut() {
                r.problem = Some(words);
                shared.publish(v);
            }
        });
        let record = RunRecord::open(begin, &self.plan.record, problem).map_err(|e| {
            refuse(
                "record_unwritable",
                format!(
                    "The run's record could not be written, so the run did not arm: {} ({}). \
                     Choose a data folder that can be written.",
                    e.path.display(),
                    e.reason
                ),
            )
            .with_detail(json!({"path": e.path.display().to_string(), "reason": e.reason}))
        })?;
        self.shared.attach_record(record.log());
        if let Some(why) = &streams_off {
            self.shared.add_log_data(
                "record",
                format!("the run's streams are not recorded: {why}"),
                Some(json!({"stream": null, "failed": why})),
            );
        }
        // The background's reference and its bands at 0 dB on the run's lines, and
        // each limit's ceiling, as the controller builds them.
        let mut loop_cfg = wire.random.loop_config();
        loop_cfg.fs_hz = fs;
        let lines = crate::random::view::lines_on(&loop_cfg, &wire.random);
        let limits = crate::random::run::limit_lines(&loop_cfg, &wire.random);
        for (name, d, arrays) in std::iter::once(rrec::reference_result(&lines))
            .chain(limits.iter().map(|l| rrec::limit_result(l, &table)))
        {
            record.put_result(&name, d, arrays, None);
        }
        self.view.record = Some(RecordRef {
            folder: record.folder().display().to_string(),
            streams: streams_off.is_none(),
            problem: None,
        });
        let control_unit = table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Control)
            .map_or_else(|| "g".to_owned(), |c| unit_name(&c.units).to_string());
        let drive_unit = mrec::played_drives(&table)
            .first()
            .map_or_else(|| "V".to_owned(), |c| unit_name(&c.units).to_string());
        let core = &self.plan.core;
        self.keep = Some(Keeping {
            status: record.status(),
            record,
            run_uid,
            utc_offset_s,
            table: table.clone(),
            control_unit,
            drive_unit,
            controls: core
                .control
                .channels
                .iter()
                .map(|c| c.channel_index)
                .collect(),
            parts: core
                .parts
                .iter()
                .map(|p| {
                    let kind = match p.kind() {
                        "floor" => PartKind::Floor,
                        "tone" => PartKind::Tone,
                        _ => PartKind::Narrowband,
                    };
                    (p.id.clone(), kind)
                })
                .collect(),
            tones: core.tones().map(|(p, _)| p.id.clone()).collect(),
            levels: Blocks::new(mrec::LEVEL_BLOCK),
            trace: Blocks::new(mrec::TRACE_BLOCK),
            spectra: None,
            rung: None,
            spectra_kept: 0,
            last_t: f64::NEG_INFINITY,
            identified: false,
        });

        self.svc = Some(svc);
        self.lock = lock;
        self.set(State::Armed, "armed");
        self.shared.add_log_data(
            "station",
            format!(
                "armed on {} at {} Hz, a {} frame tick, shared clock",
                class_name(class),
                num(super::config::round(fs, 3)),
                tick_frames
            ),
            Some(json!({"class": class_name(class), "fs_hz": fs, "tick_frames": tick_frames})),
        );
        Ok(())
    }

    /// Both roles' laws as the record names them: identity, parameters, guards.
    fn law_json(&self, svc: &MixedService) -> Value {
        let l = &self.plan.law;
        let w = &self.plan.wire;
        json!({
            "id": format!("{}+{}", l.floor.id, l.tones.id),
            "name": "Mixed Mode",
            "builtin": l.floor.builtin && l.tones.builtin,
            "floor": l.floor.to_json(),
            "tones": l.tones.to_json(),
            "params": {"floor": Value::Object(w.law_floor.params.clone()),
                       "tones": Value::Object(w.law_tones.params.clone())},
            "guards": serde_json::to_value(svc.law_guards()).unwrap_or(Value::Null),
        })
    }

    // ---- identification and the gate --------------------------------------------------

    fn identify(&mut self) {
        let Some(svc) = self.svc.as_mut() else {
            self.fault("the run lost its station before identification");
            return;
        };
        let r = svc.run_sysid(0);
        self.forward_log_now();
        if self.shared.signal.load(Ordering::SeqCst) == ABORT {
            self.abort_now(&self.ender());
            return;
        }
        if self.shared.stop_asked.load(Ordering::SeqCst) {
            self.end_stopped(self.stop_outcome(), None);
            return;
        }
        if let Err(e) = r {
            if self.svc.as_ref().map(MixedService::state) == Some(MixedState::Aborted) {
                self.finish_from_controller();
            } else {
                self.fault(&format!("identification failed: {e}"));
            }
            return;
        }
        self.set(State::Running, "coherence_gate");
        self.refresh_display();
        self.publish();
        let Some(svc) = self.svc.as_mut() else { return };
        let gate = svc.coherence_gate();
        self.forward_log_now();
        self.ident_facts();
        self.keep_identification();
        match gate {
            Err(e) => self.fault(&format!("the coherence gate failed: {e}")),
            Ok(false) => {
                let c = self
                    .view
                    .identification
                    .as_ref()
                    .map_or(0.0, |i| i.band_coherence);
                let floor = self.plan.wire.random.control.coherence_floor;
                self.end(
                    "held",
                    "coherence_hold",
                    None,
                    Some(format!(
                        "Coherence gate held: band coherence {c:.2} is below the gate of \
                         {floor:.2}; the run never drove at test level."
                    )),
                    None,
                );
            }
            Ok(true) => {
                // The core always parks at review on a pass; without a review the run
                // proceeds at once.
                if self.plan.wire.random.control.review_after_sysid {
                    self.set(State::Review, "awaiting_review");
                    self.publish();
                } else if let Err(e) = self.proceed() {
                    self.fault(&e.message);
                }
            }
        }
    }

    fn ident_facts(&mut self) {
        let Some(svc) = self.svc.as_ref() else { return };
        let st = svc.live_status();
        let lag = svc.sysid_lag_frames();
        let c = &self.plan.wire.random.control;
        self.view.identification = Some(IdentFacts {
            level_db: c.pretest_level_db,
            band_coherence: st.band_coherence,
            gate_coherence: c.coherence_floor,
            lag_frames: lag,
            trusted_frac: st.trusted_frac,
            passed: st.band_coherence >= c.coherence_floor,
        });
    }

    fn ender(&self) -> String {
        lock(&self.shared.ended_by)
            .clone()
            .unwrap_or_else(|| "the operator".into())
    }

    fn stop_outcome(&self) -> &'static str {
        self.stop.as_ref().map_or("operator_stop", |(o, _)| *o)
    }

    fn fault(&mut self, words: &str) {
        self.shared.signal.store(ABORT, Ordering::SeqCst);
        if let Some(svc) = self.svc.as_mut() {
            let _ = svc.abort(words);
        }
        self.forward_log_now();
        self.end(
            "fault",
            "aborted",
            None,
            Some(format!("The run stopped on a fault: {words}")),
            None,
        );
    }

    // ---- review and engage --------------------------------------------------------

    fn proceed_cmd(&mut self) -> Result<(), MethodError> {
        if self.view.state != State::Review {
            return Err(wrong_state(&self.view, "Proceeding"));
        }
        self.shared.add_log_data(
            "note",
            "the operator reviewed the identification and proceeded",
            Some(json!({"action": "proceed"})),
        );
        self.proceed()
    }

    fn proceed(&mut self) -> Result<(), MethodError> {
        let svc = self
            .svc
            .as_mut()
            .ok_or_else(|| wrong_state(&self.view, "Proceeding"))?;
        let r = svc.proceed_after_review();
        let state = svc.state();
        self.forward_log_now();
        match r {
            Ok(()) => {
                self.engaged();
                Ok(())
            }
            Err(_) if state.is_terminal() => {
                self.finish_from_controller();
                Ok(())
            }
            Err(e) => Err(refuse("wrong_state", e.to_string())),
        }
    }

    fn engaged(&mut self) {
        if let Some(l) = self.view.law.as_mut() {
            l.built = true;
        }
        self.set(State::Running, "floor_only");
        self.refresh();
        self.publish();
    }

    // ---- the loop -----------------------------------------------------------------

    fn pump(&mut self) {
        if self.shared.signal.load(Ordering::SeqCst) == ABORT {
            self.abort_now(&self.ender());
            return;
        }
        self.t_before = self.view.t_s;
        let Some(svc) = self.svc.as_mut() else {
            self.fault("the run lost its station");
            return;
        };
        if svc.state().is_terminal() {
            self.forward_log_now();
            self.refresh();
            self.finish_from_controller();
            return;
        }
        let r = svc.run_chunk();
        // The time plots' tap is drained every chunk; the frames carry spectra only.
        svc.service_analysis_tapped(|_, _| {});
        match r {
            Ok(true) => {
                self.forward_log_now();
                self.refresh();
                self.keep_results();
                self.pace();
                let due = self
                    .last_publish
                    .is_none_or(|t| t.elapsed() >= PUBLISH_PERIOD);
                let shown = self.shared.view();
                if due || view::state_key(Some(&shown)) != view::state_key(Some(&self.view)) {
                    self.refresh_display();
                    self.publish();
                }
            }
            Ok(false) => {
                self.forward_log_now();
                self.refresh();
                self.keep_results();
                self.refresh_display();
                self.finish_from_controller();
            }
            Err(e) => {
                if self.shared.signal.load(Ordering::SeqCst) == ABORT {
                    self.abort_now(&self.ender());
                } else {
                    self.fault(&format!("the loop failed: {e}"));
                }
            }
        }
    }

    fn pace(&mut self) {
        if self.plan.sim_speed <= 0.0 || self.view.station.kind != "sim" {
            return;
        }
        let Some(t) = self.view.t_s else { return };
        let (w0, t0) = *self.pace_origin.get_or_insert((Instant::now(), t));
        let ahead = (t - t0) / self.plan.sim_speed - w0.elapsed().as_secs_f64();
        if ahead > 0.002 {
            std::thread::sleep(Duration::from_secs_f64(ahead.min(0.25)));
        }
    }

    /// Read the controller's live status into the view.
    fn refresh(&mut self) {
        let Some(svc) = self.svc.as_ref() else { return };
        let st = svc.live_status();
        let stopping = self.view.state == State::Stopping;
        if st.state.is_terminal() {
            self.view.status = Some(st);
            return;
        }
        // A ramp down the core starts itself (the schedule finished) is timed
        // from the first chunk that shows it, as one the operator asks for is.
        if st.phase == hertzforge_mixed::MixedPhase::RampingDown && self.stop_at_t.is_none() {
            self.stop_at_t = Some(self.view.t_s.unwrap_or(st.t_s));
        }
        let was = self.view.phase;
        view::apply_status(&mut self.view, &st);
        if stopping {
            self.view.state = State::Stopping;
            self.view.phase = "ramping_down";
        }
        if self.view.phase != was {
            self.shared.add_log_data(
                "state",
                format!(
                    "{} to {}",
                    was.replace('_', " "),
                    self.view.phase.replace('_', " ")
                ),
                Some(json!({"from": was, "to": self.view.phase,
                            "state": self.view.state.name()})),
            );
        }
        if !stopping && st.phase != hertzforge_mixed::MixedPhase::RampingDown {
            self.driving_level = Some(st.level_db);
        }
        if let (Some(last), true) = (self.last_t, st.held) {
            self.held_s += (st.t_s - last).max(0.0);
        }
        self.last_t = Some(st.t_s);
        self.view.elapsed_s = Some((st.t_s - self.held_s).max(0.0));
        self.view.tick_n = (st.t_s / self.view.grid.tick_s().max(1e-9)) as u64;
        self.view.loop_n = (st.t_s / self.view.grid.loop_s.max(1e-9)) as u64;
        if let Some(l) = self.view.law.as_mut() {
            let tones: Vec<&hertzforge_mixed::PartRow> = st
                .parts
                .iter()
                .filter(|r| r.kind == hertzforge_mixed::PartKind::Tone)
                .collect();
            l.tones.misses = tones.iter().map(|r| r.misses).max().unwrap_or(0);
            l.tones.capped = tones.iter().map(|r| r.capped).sum();
            if let Some(f) = st
                .parts
                .iter()
                .find(|r| r.kind == hertzforge_mixed::PartKind::Floor)
            {
                l.floor.misses = f.misses;
                l.floor.capped = f.capped;
            }
        }
        for c in &mut self.view.channels {
            c.overload = st.overloads.contains(&c.index);
            if let Some((_, rms)) = st.input_rms.iter().find(|(i, _)| *i == c.index) {
                c.rms = rms.is_finite().then_some(rms.max(0.0));
            }
        }
        self.view.status = Some(st);
    }

    fn refresh_display(&mut self) {
        if let Some(svc) = self.svc.as_ref() {
            let band = self.plan.core.floor().and_then(|f| f.band());
            self.view.display = Some(Arc::new(super::view::on_band(svc.display(), band)));
        }
    }

    fn driving(&self) -> bool {
        self.view.state == State::Running
            && matches!(self.view.phase, "floor_only" | "engaged" | "full_level")
    }

    // ---- verbs --------------------------------------------------------------------

    fn level_cmd(&mut self, target: LevelTarget) -> Result<f64, MethodError> {
        if !self.driving() {
            return Err(wrong_state(&self.view, "A level change"));
        }
        let svc = self
            .svc
            .as_ref()
            .ok_or_else(|| wrong_state(&self.view, "A level change"))?;
        if svc.run_mode() != RunMode::Manual {
            return Err(refuse(
                "manual_mode_required",
                "The level moves by hand only in manual mode; switch with mixed.mode first.",
            )
            .with_detail(json!({"mode": "auto"})));
        }
        let now = svc.live_status().level_db;
        let levels = &self.view.levels_db;
        let r = match target {
            LevelTarget::Delta(d) => svc.step_level(d).map(|_| now + d),
            LevelTarget::To(t) => match levels.iter().position(|l| (l - t).abs() < 1e-9) {
                Some(i) => svc.jump_to_level(i).map(|_| t),
                None => {
                    return Err(refuse(
                        "not_a_rung",
                        format!("{} dB is not a level of the schedule.", num(t)),
                    )
                    .with_detail(json!({"levels_db": levels})))
                }
            },
        };
        let level = r.map_err(|e| refuse("wrong_state", e.to_string()))?;
        self.shared.add_log_data(
            "level",
            format!("level to {} dB", num(super::config::round(level, 2))),
            Some(json!({"action": "level", "level_db": level})),
        );
        Ok(level)
    }

    fn mode_cmd(&mut self, mode: RunMode) -> Result<(), MethodError> {
        if self.view.state == State::Finished {
            return Err(self.finished_refusal());
        }
        if let Some(svc) = self.svc.as_mut() {
            svc.set_run_mode(mode)
                .map_err(|e| refuse("wrong_state", e.to_string()))?;
        }
        self.view.mode = mode;
        Ok(())
    }

    fn hold_cmd(&mut self, on: bool) -> Result<(), MethodError> {
        if !self.driving() {
            return Err(wrong_state(
                &self.view,
                if on { "Holding" } else { "Resuming" },
            ));
        }
        let svc = self
            .svc
            .as_ref()
            .ok_or_else(|| wrong_state(&self.view, "Holding"))?;
        if on { svc.hold() } else { svc.resume() }
            .map_err(|e| refuse("wrong_state", e.to_string()))?;
        self.view.held = on;
        self.shared.add_log_data(
            "hold",
            if on {
                "progress held"
            } else {
                "progress resumed"
            },
            Some(json!({"action": if on { "hold" } else { "resume" }, "held": on})),
        );
        Ok(())
    }

    fn part_refused(id: &str, why: &'static str, words: String) -> MethodError {
        refuse("part_refused", words).with_detail(json!({"id": id, "why": why}))
    }

    fn part_cmd(&mut self, id: &str, on: bool, by: &str) -> Result<bool, MethodError> {
        let Some(p) = self.plan.core.parts.iter().find(|p| p.id == id) else {
            return Err(Self::part_refused(
                id,
                "unknown_part",
                format!("The test has no part \"{id}\"."),
            ));
        };
        if p.kind() == "floor" && !on {
            return Err(Self::part_refused(
                id,
                "floor",
                "The floor plays for the whole run; it cannot be switched off.".into(),
            ));
        }
        if !matches!(self.view.phase, "engaged" | "full_level") || self.view.state != State::Running
        {
            return Err(wrong_state(&self.view, "Switching a part"));
        }
        let row = self
            .view
            .status
            .as_ref()
            .and_then(|s| s.parts.iter().find(|r| r.id == id).cloned());
        if let Some(r) = &row {
            if r.enabled == on {
                return Ok(on);
            }
            if on && r.done {
                let parts_s = self.view.parts_s.unwrap_or(0.0);
                let closed = p.window.off_s.is_some_and(|off| parts_s >= off);
                return Err(if closed {
                    Self::part_refused(
                        id,
                        "window_closed",
                        format!("Part {id}'s window has closed."),
                    )
                } else {
                    Self::part_refused(
                        id,
                        "schedule_ended",
                        format!("Part {id}'s schedule has ended."),
                    )
                });
            }
        }
        let svc = self
            .svc
            .as_ref()
            .ok_or_else(|| wrong_state(&self.view, "Switching a part"))?;
        svc.set_part_enabled(id, on)
            .map_err(|e| refuse("wrong_state", e.to_string()))?;
        self.shared.add_log_data(
            "part",
            format!(
                "part {id} switched {} by {by}",
                if on { "on" } else { "off" }
            ),
            Some(json!({"action": "part", "part": id, "on": on, "by": by})),
        );
        Ok(on)
    }

    fn adjust_cmd(&mut self, key: &str, value: f64) -> Result<f64, MethodError> {
        let Some(svc) = self.svc.as_mut() else {
            return Err(wrong_state(&self.view, "An adjustment"));
        };
        match svc.adjust(LawRole::Tones, key, value) {
            Ok(v) => {
                if let Some(l) = self.view.law.as_mut() {
                    match l.tones.live.iter_mut().find(|(k, _)| k == key) {
                        Some(slot) => slot.1 = v,
                        None => l.tones.live.push((key.to_owned(), v)),
                    }
                }
                self.shared.add_log_data(
                    "law",
                    format!("tone law {key} to {}", num(v)),
                    Some(json!({"action": "adjust", "role": "tones", "key": key, "value": v})),
                );
                Ok(v)
            }
            Err(MixedAdjustRefusal::NotEngaged) => Err(wrong_state(&self.view, "An adjustment")),
            Err(e) => Err(refuse("adjust_refused", e.to_string())
                .with_detail(json!({"key": key, "why": e.why()}))),
        }
    }

    // ---- the end ------------------------------------------------------------------

    fn stop_cmd(&mut self, outcome: &'static str, by: String) {
        if !self.view.state.is_live() || self.view.state == State::Stopping {
            return;
        }
        self.stop = Some((outcome, by.clone()));
        self.shared.add_log_data(
            "note",
            if outcome == "safe_stop" && by == SHUTDOWN_BY {
                "the control service is shutting down; stopping the run with its controlled stop"
                    .to_owned()
            } else if outcome == "safe_stop" {
                format!(
                    "nobody held control for {} s; stopping the run with its controlled stop",
                    num(self.plan.timeout_s)
                )
            } else {
                format!("stop requested by {by}")
            },
            Some(json!({"action": outcome, "by": by})),
        );
        match self.view.state {
            State::Armed | State::Review => self.end_stopped(outcome, None),
            State::Running if self.driving() => {
                let Some(svc) = self.svc.as_mut() else { return };
                let t = svc.live_status().t_s;
                let r = svc.request_stop();
                self.forward_log_now();
                if r.is_err() {
                    self.end_stopped(outcome, None);
                } else {
                    self.stop_at_t = Some(t);
                    self.set(State::Stopping, "ramping_down");
                }
            }
            _ => {}
        }
    }

    fn abort_now(&mut self, by: &str) {
        self.shared.signal.store(ABORT, Ordering::SeqCst);
        if let Some(svc) = self.svc.as_mut() {
            let _ = svc.abort(&format!("aborted by {by}"));
        }
        {
            let mut b = lock(&self.shared.ended_by);
            if b.is_none() {
                *b = Some(by.to_owned());
            }
        }
        self.end("operator_abort", "aborted", None, None, None);
    }

    fn end_stopped(&mut self, outcome: &'static str, ramp_s: Option<f64>) {
        if let Some(svc) = self.svc.as_mut() {
            let _ = svc.stop();
        }
        self.end(outcome, "stopped", None, None, ramp_s);
    }

    /// The controller ended the run.
    fn finish_from_controller(&mut self) {
        let Some(svc) = self.svc.as_ref() else { return };
        let st: MixedStatus = svc.live_status();
        let t_end = st.t_s;
        let level = st.level_db;
        let trip = svc.trip().cloned();
        let names = super::config::part_names(&self.plan.wire.parts);
        let name_of = |id: &str| {
            self.plan
                .wire
                .parts
                .iter()
                .position(|p| p.id == id)
                .map(|i| names[i].clone())
                .unwrap_or_else(|| id.to_owned())
        };
        let trip_value = trip.as_ref().map(|t| {
            let at_hz = t
                .part
                .as_deref()
                .and_then(|id| st.parts.iter().find(|r| r.id == id).and_then(|r| r.freq_hz));
            let mut v = view::trip_json(t, level, at_hz);
            let row = t
                .part
                .as_deref()
                .and_then(|id| st.parts.iter().find(|r| r.id == id));
            view::fill_trip_facts(
                &mut v,
                t,
                row,
                &self.plan.core,
                self.view.display.as_deref(),
            );
            // The floor's own trips name the floor.
            if v["kind"] == "envelope" && v["part"].is_null() {
                if let Some(f) = self.plan.core.parts.iter().find(|p| p.kind() == "floor") {
                    v["part"] = Value::from(f.id.as_str());
                }
            }
            v
        });
        let trip_words = trip.as_ref().map(|t| {
            let w = t.words();
            match &t.part {
                Some(id) => w.replacen(&format!(" {id} "), &format!(" {} ", name_of(id)), 1),
                None => w,
            }
        });
        // An abort the operator (or the service) raised takes the station down with
        // its own stop; whatever the core records of that is the abort's, not a trip.
        if self.shared.signal.load(Ordering::SeqCst) == ABORT && self.stop.is_none() {
            self.end("operator_abort", "aborted", None, None, None);
            return;
        }
        match (st.outcome, st.state) {
            (Some(MixedOutcome::LawFault), _) => {
                let ramp = trip.as_ref().map(|t| (t_end - t.t_s).max(0.0));
                self.end("law_fault", "stopped", trip_value, trip_words, ramp)
            }
            (Some(MixedOutcome::Aborted), _) | (_, MixedState::Aborted) if trip.is_some() => {
                self.end("aborted", "aborted", trip_value, trip_words, None)
            }
            (_, MixedState::Aborted) => {
                if self.shared.signal.load(Ordering::SeqCst) == ABORT {
                    self.end("operator_abort", "aborted", None, None, None)
                } else {
                    let words = svc
                        .end_reason()
                        .unwrap_or_else(|| "the controller ended the run without a trip".into());
                    self.end(
                        "fault",
                        "aborted",
                        None,
                        Some(format!("The run stopped on a fault: {words}")),
                        None,
                    )
                }
            }
            (_, MixedState::CoherenceHold) => self.end(
                "held",
                "coherence_hold",
                None,
                Some("Coherence gate held; the run never drove at test level.".into()),
                None,
            ),
            (_, MixedState::Stopped) => {
                let ramp = self
                    .stop_at_t
                    .or(self.t_before)
                    .map(|s| (t_end - s).max(0.0));
                match self.stop.clone() {
                    Some((outcome, _)) => self.end(outcome, "stopped", None, None, ramp),
                    None => self.end(
                        "completed",
                        "stopped",
                        None,
                        Some(
                            "Test complete: the schedule finished and every part ramped down \
                             with the controlled stop."
                                .into(),
                        ),
                        ramp,
                    ),
                }
            }
            (_, other) => self.fault(&format!("the loop ended in state {}", other.name())),
        }
    }

    /// End the run, release the service and the station, then say how it ended.
    fn end(
        &mut self,
        outcome: &'static str,
        phase: &'static str,
        trip: Option<Value>,
        message: Option<String>,
        ramp_s: Option<f64>,
    ) {
        if self.view.state == State::Finished {
            return;
        }
        let by = self
            .stop
            .as_ref()
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| self.ender());
        let level = self.driving_level.or(self.view.level_db);
        let at = match level {
            Some(l) => format!(
                "{} dB after {} s",
                num(super::config::round(l, 2)),
                num(super::config::round(self.view.t_s.unwrap_or(0.0), 2))
            ),
            None => "before driving".to_owned(),
        };
        let message = message.unwrap_or_else(|| match outcome {
            "operator_stop" => format!("Stopped by {by} at {at}."),
            "operator_abort" => format!("Aborted by {by} at {at}."),
            "safe_stop" if by == SHUTDOWN_BY => format!(
                "The control service was shut down, so it stopped the run with its controlled \
                 stop, from {at}."
            ),
            "safe_stop" => format!(
                "Nobody held control for {} s, so the service stopped the run with its \
                 controlled stop, from {at}.",
                num(self.plan.timeout_s)
            ),
            _ => "The run ended.".to_owned(),
        });
        self.forward_log_now();
        let unramped =
            ramp_s.is_none() && matches!(outcome, "aborted" | "operator_abort" | "fault");
        if unramped {
            self.shared.signal.store(ABORT, Ordering::SeqCst);
        }
        // The release order: the service and the station go, then the end is said.
        self.svc = None;
        self.lock = None;
        self.view.end = Some(End {
            outcome,
            message: message.clone(),
            at_s: self.view.t_s,
            level_db: level,
            ramp_s: if unramped { None } else { ramp_s },
            trip,
        });
        self.set(State::Finished, phase);
        self.view.held = false;
        let end = self.view.end.as_ref().map(view::end_json);
        self.shared.add_log_data(
            "note",
            message,
            Some(json!({"outcome": outcome, "end": end})),
        );
        self.finish_record();
        self.publish();
    }

    // ---- the record ---------------------------------------------------------------

    /// The identification's transfer function and coherence, into the record, once.
    fn keep_identification(&mut self) {
        let (Some(k), Some(svc)) = (self.keep.as_mut(), self.svc.as_ref()) else {
            return;
        };
        if k.identified {
            return;
        }
        let (Some(h), Some(coh), Some(frf)) =
            (svc.frf_complex(), svc.coherence_by_channel(), svc.frf())
        else {
            return;
        };
        let band = self
            .plan
            .core
            .floor()
            .and_then(|f| f.band())
            .unwrap_or((0.0, f64::INFINITY));
        let table = svc.table().clone();
        for (name, d, arrays) in mrec::identification_results(
            h,
            coh,
            &frf.coherence,
            &frf.trusted,
            self.view.grid.df_hz,
            band,
            frf.lag_frames,
            &table,
        ) {
            k.record.put_result(&name, d, arrays, None);
        }
        k.identified = true;
    }

    /// After a chunk: the laws' calls, the parts' levels and the background's
    /// spectra since the last chunk, into the record a full block at a time. Only
    /// copies and sends: no file is touched here.
    fn keep_results(&mut self) {
        let (Some(k), Some(svc)) = (self.keep.as_mut(), self.svc.as_mut()) else {
            return;
        };
        for call in svc.take_law_records() {
            if let Some(block) = k.trace.push(call) {
                Self::put_trace(k, &block);
            }
        }
        let Some(st) = self.view.status.as_ref() else {
            return;
        };
        // A finished run's status holds no level: the last point is the last chunk's.
        let engaged = svc.law_status().is_some();
        if !engaged || st.state.is_terminal() || st.t_s <= k.last_t {
            return;
        }
        k.last_t = st.t_s;
        let point = PartPoint {
            t_s: st.t_s,
            level_db: st.level_db,
            rung: st.rung_index,
            rows: st.parts.clone(),
            phases_deg: svc.tone_phases_deg(),
        };
        if let Some(block) = k.levels.push(point) {
            Self::put_levels(k, &block);
        }
        // The background's spectra: the last chunk before each level change, and
        // the last of the run.
        let frame = self.shared.station_frame.load(Ordering::Relaxed);
        if k.rung.is_some_and(|r| r != st.rung_index) {
            Self::put_spectra(k, "level");
        }
        k.rung = Some(st.rung_index);
        let band = self.plan.core.floor().and_then(|f| f.band());
        let d = view::on_band(svc.display(), band);
        if !d.control.is_empty() {
            k.spectra = Some((d, st.level_db, if frame == NO_FRAME { 0 } else { frame }));
        }
    }

    fn put_spectra(k: &mut Keeping, why: &str) {
        let Some((d, level, frame)) = k.spectra.take() else {
            return;
        };
        for (name, desc, arrays) in mrec::spectra_results(&d, &k.table, &k.control_unit) {
            let line = json!({"index": k.spectra_kept, "why": why, "level_db": level,
                              "frame": frame});
            k.record.put_result(&name, desc, arrays, Some(line));
        }
        k.spectra_kept += 1;
    }

    fn put_levels(k: &mut Keeping, block: &[PartPoint]) {
        let index = k.levels.kept;
        for (name, d, arrays) in
            mrec::levels_results(block, &k.parts, &k.controls, &k.control_unit, &k.drive_unit)
        {
            let line = json!({"index": index, "points": block.len(),
                              "first_s": block.first().map(|p| p.t_s),
                              "last_s": block.last().map(|p| p.t_s)});
            k.record.put_result(&name, d, arrays, Some(line));
        }
        k.levels.kept += 1;
    }

    fn put_trace(k: &mut Keeping, block: &[MixedLawRecord]) {
        let (name, d, arrays) = mrec::trace_result(block, &k.tones, &k.drive_unit);
        let line = json!({"index": k.trace.kept, "calls": block.len(),
                          "first_s": block.first().map(|r| r.t_s),
                          "last_s": block.last().map(|r| r.t_s)});
        k.record.put_result(&name, d, arrays, Some(line));
        k.trace.kept += 1;
    }

    /// End the record: the part-filled blocks, the last spectra, `end.json` with
    /// the run's end as its state reports it, then `complete`.
    fn finish_record(&mut self) {
        let Some(mut k) = self.keep.take() else {
            return;
        };
        if let Some(svc) = self.svc.as_mut() {
            for call in svc.take_law_records() {
                if let Some(block) = k.trace.push(call) {
                    Self::put_trace(&mut k, &block);
                }
            }
        }
        if let Some(block) = k.levels.rest() {
            Self::put_levels(&mut k, &block);
        }
        if let Some(block) = k.trace.rest() {
            Self::put_trace(&mut k, &block);
        }
        Self::put_spectra(&mut k, "end");
        self.shared.detach_record();
        let state = view::state_json(Some(&self.view));
        let frame = self.shared.station_frame.load(Ordering::Relaxed);
        let mut end = Map::new();
        end.insert("run_uid".into(), Value::from(k.run_uid.as_str()));
        end.insert("end".into(), state["end"].clone());
        end.insert("at_unix".into(), Value::from(now_unix()));
        end.insert(
            "station_frames".into(),
            Value::from(if frame == NO_FRAME { 0 } else { frame }),
        );
        end.insert(
            "summary".into(),
            json!({"elapsed_s": self.view.elapsed_s, "t_s": self.view.t_s,
                   "level_records": k.levels.kept, "trace_records": k.trace.kept,
                   "spectra_records": k.spectra_kept}),
        );
        if let Some(r) = self.view.record.as_mut() {
            r.problem = k.status.problem();
        }
        k.record.finish(end);
    }

    // ---- the controller's log -------------------------------------------------------

    fn forward_log_now(&mut self) {
        if let Some(svc) = self.svc.take() {
            self.forward_log(&svc);
            self.svc = Some(svc);
        }
    }

    /// The controller's events since the last call, into the run log; a driver
    /// warning reaches it as the controller wrote it.
    fn forward_log(&mut self, svc: &MixedService) {
        let events = svc.log().events();
        for e in events.iter().skip(self.forwarded) {
            let at = json!(e.t_s);
            let (kind, text, data) = match &e.kind {
                MixedEventKind::State { .. } | MixedEventKind::Phase { .. } => continue,
                MixedEventKind::Level { level_db } => (
                    "level",
                    format!("level {} dB", num(super::config::round(*level_db, 2))),
                    Some(json!({"level_db": level_db, "t_s": at})),
                ),
                MixedEventKind::Part { part, on } => (
                    "part",
                    format!("part {part} {}", if *on { "on" } else { "off" }),
                    Some(json!({"part": part, "on": on, "t_s": at})),
                ),
                MixedEventKind::Hold { held } => (
                    "hold",
                    if *held {
                        "held".into()
                    } else {
                        "resumed".into()
                    },
                    Some(json!({"held": held, "t_s": at})),
                ),
                MixedEventKind::Trip { trip } => (
                    "trip",
                    trip.words(),
                    Some(serde_json::to_value(trip).unwrap_or(Value::Null)),
                ),
                MixedEventKind::LawMiss { part, reason } => (
                    "law",
                    match part {
                        Some(p) => format!("tone {p}'s law call not used: {reason}"),
                        None => format!("the floor law's call not used: {reason}"),
                    },
                    Some(json!({"part": part, "miss": reason, "t_s": at})),
                ),
                MixedEventKind::Note { text } => ("note", text.clone(), None),
            };
            self.shared.add_log_data(kind, text, data);
        }
        self.forwarded = events.len();
    }
}

fn lock_cancel(s: &RunShared) -> std::sync::MutexGuard<'_, Option<Arc<AtomicBool>>> {
    lock(&s.sysid_cancel)
}
