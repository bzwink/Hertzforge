//! The seam between the run thread and whatever plays the records: [`TwrEngine`].
//!
//! The run thread ([`super::run`]) owns the order of things on the wire (arm, start,
//! review, the plays between verbs, the pause while nobody holds control, stop and
//! abort, every publish); an engine owns the plays themselves. The engine a run
//! uses in service is [`ServiceEngine`], the controller's `TwrService` over the
//! station's backend: identification, the inverse, the law, the guards, the ladder
//! and every judgement are the controller's. Wire tests that exercise the protocol
//! without a controller run give the service [`super::sim::SimulatedEngine`]
//! instead, as the shock wire tests give theirs.

use super::config::{excitation_name, WireTwr};
use super::reference::Stored;
use super::view::{
    IdentFacts, IdentFigures, LawCall, MaskFigures, PlayRecords, PlayRow, Prediction, Spectra,
    StationRef, Trip, TripFacts,
};
use crate::protocol::{num, ErrorCode, MethodError};
use crate::pulse::observe::{Observed, Watch};
use crate::pulse::rails::{Progress, Signals};
use crate::shock::config::LawFacts;
use crate::shock::record::{IdentRecord, ShotRecording, StationFacts};
use crate::shock::view::sig;
use crate::station::{self, class_name, NiScanner, StationPlan};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_twr::control::PlayKind;
use hertzforge_twr::grid::rfft_half;
use hertzforge_twr::player::{BackendReferencePlayer, LagSource, PlayProgress, SampleRing};
use hertzforge_twr::service::Advanced;
use hertzforge_twr::{TripKind, TwrError, TwrLawFactory, TwrService, TwrState, TwrTrip};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

/// What a run arms on, taken from the service when the request arrives.
pub struct ArmPlan {
    pub cfg: Arc<WireTwr>,
    /// The stored reference the run plays.
    pub stored: Arc<Stored>,
    pub facts: LawFacts,
    pub table: ChannelTable,
    pub station: StationPlan,
    pub scanner: Arc<dyn NiScanner>,
    /// The service's local data directory: the hardware lock lives here.
    pub data_root: PathBuf,
    pub bind: String,
    /// The run's law, resolved by the service from the catalog.
    pub law: Result<Arc<dyn TwrLawFactory>, MethodError>,
    /// The operator's acknowledgement to arm past a feasibility exceedance.
    pub ack: Option<String>,
    /// The devices of the last scan, for the record's models and serial numbers.
    pub devices: Vec<hertzforge_hal::DiscoveredDevice>,
    /// Whether the record keeps the streams, and how many seconds of samples
    /// their buffers hold before blocks are dropped.
    pub streams: bool,
    pub stream_buffer_s: f64,
    /// The station frame, set by the station wrapper after every read.
    pub station_frame: Arc<AtomicU64>,
}

/// An armed station's facts.
pub struct Armed {
    /// The rate the station adopted.
    pub fs_hz: f64,
    /// The record every play is played and measured as, at that rate: its length
    /// and its section. All three are the engine's, never the configure-time grid's
    /// counts beside the engine's rate (the mix bench T0 reported: 51 200 Hz beside
    /// a 51 200-sample section configured at 12 800 Hz).
    pub record_len: usize,
    pub section_start: usize,
    pub section_len: usize,
    pub station: StationRef,
    pub feasibility: (&'static str, String),
    /// Held while the run is live on hardware.
    pub lock: Option<hertzforge_station::lock::LockGuard>,
    /// The control channel's display tap, drained while a play is in flight.
    pub tap: Option<Arc<Mutex<SampleRing>>>,
    pub units: String,
}

/// How an arm ended.
pub enum ArmOutcome {
    Armed(Box<Armed>),
    /// Feasibility exceeded and no acknowledgement was given.
    Blocked((&'static str, String)),
}

/// The engine ended the run: its outcome, words and trip.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineEnd {
    pub outcome: &'static str,
    pub message: String,
    pub trip: Option<Trip>,
}

/// How identification ended.
pub enum Identified {
    /// Passed: hold for review, or go on.
    Done { facts: IdentFacts, review: bool },
    /// The coherence hold: nothing drives until a reset.
    Held {
        facts: Option<IdentFacts>,
        message: String,
    },
    /// A protection ended the run (the coverage floor, the drive ceiling).
    Ended {
        facts: Option<IdentFacts>,
        end: EngineEnd,
    },
}

/// How proceeding ended.
pub enum Proceeded {
    Ready(Option<Prediction>),
    Ended(EngineEnd),
}

/// What one play did.
#[derive(Default)]
pub struct Played {
    pub row: Option<PlayRow>,
    pub records: Option<PlayRecords>,
    /// The engine ended the run on it.
    pub end: Option<EngineEnd>,
    /// The play was cancelled after this many frames.
    pub cancelled_at: Option<u64>,
}

/// A level move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LevelMove {
    /// One step, in dB.
    Step(f64),
    /// A level of the table, from 0.
    Rung(usize),
}

/// Where the run is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Position {
    pub level_db: f64,
    pub rung: Option<usize>,
    pub at_level: u32,
    pub of_level: u32,
    pub playback: u32,
    pub of_playback: u32,
    /// The present level has gated.
    pub gated: bool,
    /// The next play is a playback play.
    pub playing: bool,
    /// A level failed its gate under `hold`.
    pub holding: bool,
    pub frozen: bool,
    pub alarm: bool,
}

/// What a run's engine does. Every call is made on the run thread; a play blocks
/// it for the whole record and reads [`Signals::cancel`] at every block.
pub trait TwrEngine: Send {
    /// Feasibility, the station brought up at the rate the run asked, and the record
    /// at that rate. A station that adopts another rate is refused (`arm_refused`).
    /// Nothing is driven.
    fn arm(&mut self, plan: ArmPlan) -> Result<ArmOutcome, MethodError>;
    /// Identification: one cancellable play.
    fn identify(&mut self) -> Result<Identified, MethodError>;
    /// Build the law and seed it.
    fn proceed(&mut self) -> Result<Proceeded, MethodError>;
    /// One play at the present level: a correction play, or a playback play once
    /// playback has begun.
    fn play(&mut self) -> Result<Played, MethodError>;
    /// Begin playback from a level that has gated.
    fn begin_playback(&mut self) -> Result<(), MethodError>;
    /// Move the level; the new level, dB.
    fn set_level(&mut self, to: LevelMove) -> Result<f64, MethodError>;
    /// The drive in force's peak at `level_db`, volts, once seeded.
    fn predicted_vpk_at(&self, level_db: f64) -> Option<f64>;
    /// Hold the law's corrections off, or resume them.
    fn freeze(&mut self, frozen: bool);
    /// The controlled stop: nothing more plays.
    fn stop(&mut self);
    /// The emergency stop.
    fn abort(&mut self, reason: &str);
    fn position(&self) -> Position;
    fn identification(&self) -> Option<IdentFigures>;
    fn mask(&self) -> Option<MaskFigures>;
    /// The level target now, for a plot that shows it before a play.
    fn level_target(&self) -> Option<PlayRecords>;
    /// Unusable law calls in a row now.
    fn misses(&self) -> u32;
    /// Frames the station has clocked over every play and pad since the run armed.
    fn frames_clocked(&self) -> u64;
    /// Log lines since the last call, driver warnings among them.
    fn take_log(&mut self) -> Vec<(&'static str, String)>;
    /// Let the station go after an arm the run refused (its record could not be
    /// written): nothing has been driven.
    fn release(&mut self) {}
    /// What the run's record says of the station armed, and its streams' links;
    /// taken once, after an arm.
    fn take_station_facts(&mut self) -> Option<StationFacts> {
        None
    }
    /// The station's starts since the run armed: each play, the identification's
    /// among them, is one, so after a play the count less one is its shot.
    fn shots(&self) -> u64 {
        0
    }
    /// The identification as the record keeps it, once done.
    fn ident_record(&self) -> Option<IdentRecord> {
        None
    }
    /// The drive-to-response lag every play is aligned at, frames, and whether it
    /// was measured or derived.
    fn lag(&self) -> Option<(f64, &'static str)> {
        None
    }
    /// The target at full level on the record's samples, once armed.
    fn full_level_target(&self) -> Option<Vec<f64>> {
        None
    }
    /// The law's call records since the last take, oldest first; taken between
    /// plays.
    fn take_law_calls(&mut self) -> Vec<hertzforge_twr::TwrCallRecord> {
        Vec::new()
    }
}

/// What an engine is built with: the run's signals and its progress cell, and the
/// simulator's pace.
#[derive(Clone)]
pub struct EngineContext {
    pub signals: Signals,
    pub progress: Arc<Progress>,
    /// Simulated seconds per wall second on the simulator; 0 runs unpaced.
    pub sim_speed: f64,
}

/// Makes a run's engine.
pub type EngineFactory = Arc<dyn Fn(&EngineContext) -> Box<dyn TwrEngine> + Send + Sync>;

/// The engine a run uses when the service is given none: the controller's.
pub fn service_engine_factory() -> EngineFactory {
    Arc::new(|ctx| Box::new(ServiceEngine::new(ctx.clone())))
}

fn arm_refused(words: impl std::fmt::Display) -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "arm_refused",
        format!("The station refused to arm: {words}"),
    )
}

/// The station's own refusal as the run arms, in the controller's words: the arm
/// gate's reason (the rate policy's, when the station adopted another rate than the
/// record's) or the driver's, without the core's error-kind prefix.
fn station_refused(e: TwrError) -> MethodError {
    let words = match &e {
        TwrError::State(s) | TwrError::Play(s) | TwrError::Config(s) => s.as_str(),
        _ => return arm_refused(e),
    };
    arm_refused(words.strip_prefix("arm refused: ").unwrap_or(words))
}

fn fault(words: impl std::fmt::Display) -> MethodError {
    MethodError::internal(format!("the TWR service failed: {words}"))
}

/// A sentence from the controller's words: a capital first letter and a full
/// stop.
pub fn sentence(s: &str) -> String {
    let t = s.trim().trim_end_matches('.');
    let mut c = t.chars();
    match c.next() {
        Some(f) => format!("{}{}.", f.to_uppercase(), c.as_str()),
        None => String::new(),
    }
}

/// The display tap's length, samples of the control channel.
pub const TAP_SAMPLES: usize = 8192;

type Service = TwrService<BackendReferencePlayer<Observed>>;

/// The controller's TWR service over the station's backend.
pub struct ServiceEngine {
    ctx: EngineContext,
    svc: Option<Service>,
    cfg: Option<Arc<WireTwr>>,
    units: String,
    log: Vec<(&'static str, String)>,
    log_seen: usize,
    /// What the record says of the station armed, until the run takes it.
    station_facts: Option<StationFacts>,
    /// The station's starts, counted by the station wrapper.
    starts: Arc<AtomicU64>,
    #[cfg(test)]
    without_law: bool,
    /// A test's wrapper around the station's backend, applied as the run arms (a
    /// station that cannot do what the simulator can, such as change its rate).
    #[cfg(test)]
    wrap_station: Option<StationWrap>,
}

/// A test's wrapper around the station's backend.
#[cfg(test)]
type StationWrap =
    Box<dyn Fn(Box<dyn hertzforge_hal::Backend>) -> Box<dyn hertzforge_hal::Backend> + Send>;

impl ServiceEngine {
    pub fn new(ctx: EngineContext) -> ServiceEngine {
        ServiceEngine {
            ctx,
            svc: None,
            cfg: None,
            units: "g".into(),
            log: Vec::new(),
            log_seen: 0,
            station_facts: None,
            starts: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            without_law: false,
            #[cfg(test)]
            wrap_station: None,
        }
    }

    /// A test engine whose run is the core's own iteration, without the law seam.
    #[cfg(test)]
    pub fn without_law(ctx: EngineContext) -> ServiceEngine {
        let mut e = ServiceEngine::new(ctx);
        e.without_law = true;
        e
    }

    /// A test engine running the core's own iteration, without the law seam.
    fn lawless(&self) -> bool {
        #[cfg(test)]
        {
            self.without_law
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn svc(&mut self) -> Result<&mut Service, MethodError> {
        self.svc
            .as_mut()
            .ok_or_else(|| fault("the run is not armed"))
    }

    fn forward_log(&mut self) {
        let Some(svc) = self.svc.as_mut() else {
            return;
        };
        let events = svc.event_log();
        for e in events.iter().skip(self.log_seen) {
            // The core stamps its own clock in front: the log has its own time.
            let text = match e.find("] ") {
                Some(i) if e.starts_with('[') => &e[i + 2..],
                _ => e.as_str(),
            };
            self.log.push(("note", text.to_owned()));
        }
        self.log_seen = events.len();
        for w in svc.take_driver_warnings() {
            self.log.push(("note", format!("driver: {w}")));
        }
    }

    /// The end a terminal core state means, with its words: completion, or the
    /// core's own trip by its kind.
    fn ended(&self) -> Option<EngineEnd> {
        let svc = self.svc.as_ref()?;
        let st = svc.status();
        match st.state {
            TwrState::Complete => Some(EngineEnd {
                outcome: "completed",
                message: format!(
                    "The playback schedule finished: {} play{} at {}.",
                    st.plays_completed,
                    if st.plays_completed == 1 { "" } else { "s" },
                    level_words(st.level_db)
                ),
                trip: None,
            }),
            TwrState::Aborted => {
                let message = sentence(st.abort_reason.as_deref().unwrap_or("the run aborted"));
                Some(match st.trip.as_ref() {
                    Some(t) => end_of_trip(t, message),
                    None => EngineEnd {
                        outcome: "aborted",
                        message,
                        trip: None,
                    },
                })
            }
            _ => None,
        }
    }

    fn ident_facts(&self, passed: bool) -> Option<IdentFacts> {
        let svc = self.svc.as_ref()?;
        let cfg = svc.config();
        let st = svc.status();
        let fs = st.actual_fs_hz.unwrap_or(svc.grid().fs_hz);
        let peak = {
            let mut x = cfg.sysid.build_excitation(fs);
            cfg.limits.apply_drive_clip(&mut x);
            x.iter().fold(0.0f64, |m, &v| m.max(v.abs()))
        };
        let lag = svc.lag_frames();
        let source = match svc.lag_provenance() {
            Some(LagSource::Measured) => "measured",
            _ => "derived",
        };
        Some(IdentFacts {
            excitation: excitation_name(cfg.sysid.excitation),
            drive_vrms: cfg.sysid.drive_rms,
            drive_vpk: peak,
            coherence_mean: st.sysid_mean_coherence.unwrap_or(f64::NAN),
            min_mean_coherence: cfg.sysid.min_mean_coherence,
            coverage: st.mask_energy_coverage,
            lag_frames: lag.map(|l| l as f64),
            lag_source: source,
            passed,
        })
    }

    /// The level target at `level_db`.
    fn target_at(&self, level_db: f64) -> Vec<f64> {
        let Some(svc) = self.svc.as_ref() else {
            return Vec::new();
        };
        let now = svc.status().level_db;
        let k = 10f64.powf((level_db - now) / 20.0);
        svc.target_time().iter().map(|v| v * k).collect()
    }

    fn band(&self) -> (f64, f64) {
        self.svc.as_ref().map_or((0.0, 0.0), |s| {
            (s.config().law.f_lo_hz, s.config().law.f_hi_hz)
        })
    }

    fn records(&self, level_db: f64) -> Option<PlayRecords> {
        let svc = self.svc.as_ref()?;
        let response = svc.achieved_time().to_vec();
        if response.is_empty() {
            return None;
        }
        let target = self.target_at(level_db);
        let drive = svc.drive_time().to_vec();
        let grid = svc.grid();
        let (lo, hi) = self.band();
        let spectra = (target.len() == grid.n && drive.len() == grid.n).then(|| {
            let mag = |x: &[f64]| -> Vec<f64> {
                rfft_half(x, grid.n)
                    .iter()
                    .map(|&(re, im)| (re * re + im * im).sqrt())
                    .collect()
            };
            let (r, y, d) = (mag(&target), mag(&response), mag(&drive));
            let keep: Vec<usize> = (0..r.len())
                .filter(|&k| {
                    let f = grid.freq(k);
                    f >= lo && f <= hi
                })
                .collect();
            let pick = |v: &[f64]| keep.iter().map(|&k| v[k]).collect::<Vec<f64>>();
            Spectra {
                freqs_hz: keep.iter().map(|&k| grid.freq(k)).collect(),
                target: pick(&r),
                response: pick(&y),
                drive: pick(&d),
            }
        });
        let l = &svc.config().limits;
        Some(PlayRecords {
            fs_hz: grid.fs_hz,
            target,
            response,
            drive,
            alarm_frac: l.envelope_alarm_frac,
            abort_frac: l.envelope_abort_frac,
            spectra,
        })
    }
}

/// A level in words: "full level" at 0 dB, else its dB.
pub fn level_words(level_db: f64) -> String {
    if level_db.abs() < 1e-9 {
        "full level".into()
    } else {
        format!("{} dB", num(crate::shock::config::round(level_db, 2)))
    }
}

/// The end a core trip makes, with its facts on the wire.
pub fn end_of_trip(t: &TwrTrip, message: String) -> EngineEnd {
    let trip = |facts| {
        Some(Trip {
            facts,
            play: t.play,
            at_level_db: Some(t.at_level_db),
        })
    };
    let (outcome, trip) = match &t.kind {
        TripKind::Envelope {
            in_play,
            worst_frac,
            abort_frac,
            samples,
            at_sample,
        } => (
            "aborted",
            trip(TripFacts::Envelope {
                in_play: *in_play,
                worst_frac: *worst_frac,
                abort_frac: *abort_frac,
                samples: *samples as u64,
                at_sample: *at_sample as u64,
            }),
        ),
        TripKind::DriveVpk {
            vpk,
            ceiling_vpk,
            predicted,
        } => (
            "aborted",
            trip(TripFacts::DriveVpk {
                vpk: *vpk,
                ceiling_vpk: *ceiling_vpk,
                predicted: *predicted,
            }),
        ),
        TripKind::DriveRms { vrms, limit_vrms } => (
            "aborted",
            trip(TripFacts::DriveRms {
                vrms: *vrms,
                limit_vrms: *limit_vrms,
            }),
        ),
        TripKind::ResponseRms { rms, limit } => (
            "aborted",
            trip(TripFacts::ResponseRms {
                rms: *rms,
                limit: *limit,
            }),
        ),
        TripKind::Coverage {
            coverage,
            min_band_energy,
            error_floor_pct,
        } => (
            "aborted",
            trip(TripFacts::Coverage {
                coverage: *coverage,
                min_band_energy: *min_band_energy,
                error_floor_pct: *error_floor_pct,
            }),
        ),
        TripKind::Divergence { plays, error_pct } => (
            "aborted",
            trip(TripFacts::Divergence {
                plays: *plays as u64,
                error_pct: *error_pct,
            }),
        ),
        TripKind::NotConverged {
            iterations,
            error_pct,
            gate_pct,
        } => (
            "not_converged",
            trip(TripFacts::NotConverged {
                iterations: u64::from(*iterations),
                error_pct: *error_pct,
                gate_pct: *gate_pct,
            }),
        ),
        TripKind::Alignment {
            lag_frames,
            identification_lag_frames,
            drift_frames,
            limit_frames,
        } => (
            "aborted",
            trip(TripFacts::Alignment {
                lag_frames: *lag_frames,
                identification_lag_frames: *identification_lag_frames,
                drift_frames: *drift_frames,
                limit_frames: *limit_frames,
            }),
        ),
        TripKind::LawFault {
            reason,
            consecutive,
        } => (
            "law_fault",
            trip(TripFacts::LawFault {
                reason: reason.reason(),
                consecutive: u64::from(*consecutive),
            }),
        ),
        TripKind::FirstDrive { .. } => (
            "law_fault",
            trip(TripFacts::LawFault {
                reason: "first_drive",
                consecutive: 1,
            }),
        ),
        TripKind::NoResponse => ("fault", None),
    };
    EngineEnd {
        outcome,
        message,
        trip,
    }
}

/// The in-band mean coherence the core's identification refusal states ("in-band
/// mean coherence 0.240 below floor 0.800 ..."), when it states one.
fn held_coherence(words: &str) -> Option<f64> {
    let at = words.find("in-band mean coherence ")? + "in-band mean coherence ".len();
    let num: String = words[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse().ok().filter(|m: &f64| m.is_finite())
}

/// A core play row on the wire.
pub fn row_of(p: &hertzforge_twr::PlayRow) -> PlayRow {
    PlayRow {
        index: p.index,
        playback: p.kind == PlayKind::Playback,
        level_db: p.level_db,
        rung: p.rung,
        iteration: p.iteration,
        rms_error_pct: p.metrics.rms_error_pct,
        peak_error_pct: p.metrics.peak_error_pct,
        spectral_error_db: p.metrics.spectral_error_db,
        gated: p.gated,
        clipped_samples: p.clipped_samples as u64,
        clamped_bins: p.clamped_bins as u64,
        drive_vpk: p.drive_vpk,
        response_peak: p.response_peak,
        lag_frames: p.lag_frames,
        drift_frames: p.drift_frames,
        start_frame: p.start_frame,
        frames: p.frames,
        at_s: p.at_s,
        law: p.law.as_ref().map(|c| LawCall {
            used: c.used,
            miss: c.miss.as_ref().map(|f| f.reason()),
            call_ms: c.call_ms,
            peak_rise_db: c.peak_rise_db,
            bin_rise_db: c.bin_rise_db,
            predicted_peak_ratio: c.predicted_peak_ratio,
            values: c.values.iter().map(|v| (v.key.clone(), v.value)).collect(),
        }),
    }
}

impl TwrEngine for ServiceEngine {
    fn arm(&mut self, plan: ArmPlan) -> Result<ArmOutcome, MethodError> {
        let law = match plan.law {
            Ok(f) => Some(f),
            Err(e) if !self.lawless() => return Err(e),
            Err(_) => None,
        };
        #[allow(unused_mut)]
        let mut cfg = plan.cfg.build(&plan.stored, &plan.facts);
        if self.lawless() {
            cfg.control = None;
        }
        let built = station::build(&plan.station, &*plan.scanner)?;
        let hardware = built.station.is_hardware();
        let lock = if hardware {
            Some(
                hertzforge_station::lock::acquire(
                    &plan.data_root,
                    &plan.bind,
                    &built.provenance.station_label,
                )
                .map_err(|e| match e {
                    hertzforge_station::lock::LockError::Held(h) => MethodError::named(
                        ErrorCode::Refused,
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
        let class = built.station;
        let provenance = built.provenance.clone();
        self.units = plan
            .table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Control)
            .map_or_else(|| "g".to_owned(), |c| crate::table::unit_name(&c.units));
        let own = Arc::new(AtomicBool::new(false));
        let watch = Watch {
            asked: self.ctx.signals.cancel.clone(),
            own: own.clone(),
            // The play's progress is the player's own (record and rest pad).
            progress: Arc::new(Progress::default()),
            expected: Arc::new(AtomicU64::new(0)),
            speed: if hardware { 0.0 } else { self.ctx.sim_speed },
            abort: Some(self.ctx.signals.abort.clone()),
        };
        let progress = self.ctx.progress.clone();
        let observer = Arc::new(move |p: PlayProgress| {
            progress.block(p.frames_done as u64, p.frames_expected as u64);
        });
        #[allow(unused_mut)]
        let mut backend = built.backend;
        #[cfg(test)]
        if let Some(wrap) = self.wrap_station.as_ref() {
            backend = wrap(backend);
        }
        // The streams are taken where the station's blocks pass: every input as
        // read and the drive as the device accepted it, a shot per start (the
        // identification, then each play). Their buffers are made here, before
        // anything runs. The wrapper only watches: every call reaches the station
        // unchanged.
        let caps = backend.caps();
        let asked = plan.cfg.shape.fs_for(plan.stored.fs_hz);
        let buffers = if plan.streams {
            crate::random::record::tap_buffers(asked, plan.stream_buffer_s)
        } else {
            1
        };
        let (inputs_tap, inputs_link) = crate::record::tap::pair(
            crate::random::record::input_rows(&plan.table).len(),
            crate::random::record::TAP_BLOCK_FRAMES,
            buffers,
        );
        let (drives_tap, drives_link) = crate::record::tap::pair(
            crate::random::record::drive_rows(&plan.table).len(),
            crate::random::record::TAP_BLOCK_FRAMES,
            buffers,
        );
        self.starts = Arc::new(AtomicU64::new(0));
        let backend = ShotRecording::wrap_counted(
            backend,
            inputs_tap,
            drives_tap,
            plan.station_frame.clone(),
            self.starts.clone(),
        );
        self.station_facts = Some(StationFacts {
            source: station::source_of(class),
            station: crate::random::record::station_member(
                matches!(&plan.station, StationPlan::Sim { .. }),
                class,
                &provenance,
                &plan.devices,
                &caps,
            ),
            sim: crate::random::record::sim_value(&plan.station, class),
            build: crate::random::record::build_value(provenance.core_build),
            output_latency: match &caps.output_buffer {
                Some(b) => json!({"frames": b.min_lookahead_frames, "how": "declared"}),
                None => json!({"frames": null, "how": "unknown"}),
            },
            links: Some((inputs_link, drives_link)),
        });
        // THE RATE THE RUN ASKED, the one its configuration derived the record at
        // (`acquisition.fs_hz`, or the reference's own). The player asks the station
        // for it and the core prepares the record at it. The station's caps state
        // the rate it was BUILT at, which on NI is the chassis's 51 200 Hz whatever
        // the run asked: requested from the caps, a run asked at 12 800 Hz ran at
        // 51 200 Hz (bench T0, 2026-11-08).
        let player =
            BackendReferencePlayer::new(Observed::new(backend, watch), plan.table.clone(), class)
                .map_err(arm_refused)?
                .with_requested_rate_hz(asked)
                .with_cancel_flag(own)
                .with_display_tap(TAP_SAMPLES)
                .with_play_observer(observer);
        let tap = player.display_tap_handle();
        let mut svc = TwrService::new(player, cfg).map_err(arm_refused)?;
        if let Some(law) = law {
            svc = svc.with_law_factory(law);
        }
        let feas =
            super::reference::feasibility(svc.feasibility(), plan.cfg.shape.feasibility.any());
        let verdict = (feas.verdict, feas.message.clone());
        if svc.state() == TwrState::FeasibilityBlocked {
            match &plan.ack {
                Some(a) => svc.arm_override(a).map_err(arm_refused)?,
                None => return Ok(ArmOutcome::Blocked(verdict)),
            }
        }
        // THE STATION, brought up here and not as identification starts: open,
        // configure at the asked rate, adopt the rate it programs, the arm gate,
        // park. A station that adopts a rate other than the record's is refused
        // here by the rate policy (`arm_refused`), before anything plays, and the
        // grid this arm reports is at the rate the station runs at.
        svc.arm_station().map_err(station_refused)?;
        let grid = svc.grid().clone();
        let adopted = svc.status().actual_fs_hz.unwrap_or(grid.fs_hz);
        self.cfg = Some(plan.cfg.clone());
        self.svc = Some(svc);
        self.forward_log();
        Ok(ArmOutcome::Armed(Box::new(Armed {
            fs_hz: adopted,
            record_len: grid.n,
            section_start: grid.pad_pre,
            section_len: grid.section_len,
            station: StationRef {
                kind: if matches!(plan.station, StationPlan::Sim { .. }) {
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
            },
            feasibility: verdict,
            lock,
            tap,
            units: self.units.clone(),
        })))
    }

    fn identify(&mut self) -> Result<Identified, MethodError> {
        let review = self
            .cfg
            .as_deref()
            .ok_or_else(|| fault("the run is not armed"))?
            .review;
        self.ctx.progress.start(0);
        let r = self.svc()?.advance();
        self.ctx.progress.end();
        self.forward_log();
        let state = self.svc()?.state();
        match (r, state) {
            (r, TwrState::CoherenceHold) => {
                let min = self
                    .svc
                    .as_ref()
                    .map_or(0.0, |s| s.config().sysid.min_mean_coherence);
                // The core keeps no estimate at the hold; its refusal says the mean
                // it measured, which the hold's facts carry.
                let mut facts = self.ident_facts(false);
                if let (Some(f), Err(e)) = (facts.as_mut(), &r) {
                    if f.coherence_mean.is_nan() {
                        if let Some(m) = held_coherence(&e.to_string()) {
                            f.coherence_mean = m;
                        }
                    }
                }
                Ok(Identified::Held {
                    facts,
                    message: format!(
                        "Identification did not reach its coherence ({} in-band mean); raise its \
                         drive or its length, or check the response path, then reset.",
                        num(min)
                    ),
                })
            }
            (Err(TwrError::Cancelled { .. }), _) | (_, TwrState::Stopped) => {
                // The run thread ends the run on its signal.
                Err(fault("identification was stopped"))
            }
            (_, TwrState::Aborted) => Ok(Identified::Ended {
                facts: self.ident_facts(false),
                end: self
                    .ended()
                    .ok_or_else(|| fault("the run ended without a reason"))?,
            }),
            (Err(e), _) => Err(fault(e)),
            (Ok(Advanced::SysId { .. }), _) => Ok(Identified::Done {
                facts: self
                    .ident_facts(true)
                    .ok_or_else(|| fault("identification left no facts"))?,
                review,
            }),
            (Ok(other), s) => Err(fault(format!("identification ended in {s:?} ({other:?})"))),
        }
    }

    fn proceed(&mut self) -> Result<Proceeded, MethodError> {
        // Under a law the core holds at its review after a passing
        // identification; proceeding builds the law and seeds it there, and in
        // AUTO checks the full level's predicted drive against the ceiling.
        if self.svc()?.state() == TwrState::Review {
            let r = self.svc()?.proceed();
            self.forward_log();
            if let Err(e) = r {
                if self.ended().is_none() {
                    return Err(fault(e));
                }
            }
        }
        if let Some(end) = self.ended() {
            return Ok(Proceeded::Ended(end));
        }
        let level = self.position().level_db;
        Ok(Proceeded::Ready(self.predicted_vpk_at(level).map(|vpk| {
            Prediction {
                drive_vpk: vpk,
                drive_vpk_ceiling: self.cfg.as_ref().map_or(0.0, |c| c.drive_vpk_ceiling),
                level_db: level,
            }
        })))
    }

    fn play(&mut self) -> Result<Played, MethodError> {
        let rows_before = self.svc()?.plays().len();
        self.ctx.progress.start(0);
        let r = self.svc()?.advance();
        self.ctx.progress.end();
        self.forward_log();
        if let Err(TwrError::Cancelled { frames_done, .. }) = &r {
            return Ok(Played {
                cancelled_at: Some(*frames_done as u64),
                ..Played::default()
            });
        }
        if let Err(e) = &r {
            if !matches!(e, TwrError::Aborted(_)) {
                return Err(fault(e));
            }
        }
        let svc = self
            .svc
            .as_ref()
            .ok_or_else(|| fault("the run lost its station"))?;
        let mut played = Played::default();
        let committed = svc.committed_frames();
        if let Some(p) = svc.plays().get(rows_before) {
            let row = row_of(p);
            // What the device took before start: the head of the record, never
            // the backend's start-time silence (the bench reads it every play).
            if let Some(n) = committed {
                self.log.push((
                    "note",
                    format!("Play {}: {n} frames committed before start.", row.index),
                ));
            }
            played.records = self.records(row.level_db);
            played.row = Some(row);
        }
        played.end = self.ended();
        Ok(played)
    }

    fn begin_playback(&mut self) -> Result<(), MethodError> {
        self.svc()?.begin_playback().map_err(|e| {
            MethodError::named(
                ErrorCode::Refused,
                "not_converged",
                format!(
                    "Playback begins once the present level has gated ({}).",
                    sentence(&e.to_string()).trim_end_matches('.')
                ),
            )
        })
    }

    fn set_level(&mut self, to: LevelMove) -> Result<f64, MethodError> {
        let ceiling = self.cfg.as_ref().map_or(0.0, |c| c.drive_vpk_ceiling);
        let target = match to {
            LevelMove::Step(d) => self.position().level_db + d,
            LevelMove::Rung(i) => self
                .svc
                .as_ref()
                .and_then(|s| s.config().schedule.table_db.get(i).copied())
                .unwrap_or(f64::NAN),
        };
        let predicted = self.predicted_vpk_at(target);
        let svc = self.svc()?;
        let r = match to {
            LevelMove::Step(d) if d > 0.0 => svc.step_level_up(),
            LevelMove::Step(_) => svc.step_level_down(),
            LevelMove::Rung(i) => svc.jump_to_level(i),
        };
        self.forward_log();
        match r {
            Ok(()) => Ok(self.position().level_db),
            Err(e) => {
                let words = e.to_string();
                if words.contains("refused") && words.contains("ceiling") {
                    Err(MethodError::named(
                        ErrorCode::Refused,
                        "drive_ceiling",
                        format!(
                            "The drive in force would pass the drive ceiling of {} V pk at {}; \
                             the level and the drive are unchanged.",
                            num(ceiling),
                            level_words(target)
                        ),
                    )
                    .with_detail(json!({
                        "vpk": predicted.map(|v| num(sig(v, 4))),
                        "ceiling_vpk": num(ceiling),
                        "level_db": num(crate::shock::config::round(target, 2)),
                    })))
                } else {
                    Err(MethodError::named(
                        ErrorCode::Refused,
                        "wrong_state",
                        sentence(&words),
                    ))
                }
            }
        }
    }

    fn predicted_vpk_at(&self, level_db: f64) -> Option<f64> {
        // The core states the drive in force at the present level; another level
        // carries it by the exact ratio, as the core's level moves do.
        let svc = self.svc.as_ref()?;
        let now = svc.status().level_db;
        svc.drive_in_force_vpk()
            .map(|v| v * 10f64.powf((level_db - now) / 20.0))
    }

    fn freeze(&mut self, frozen: bool) {
        if let Some(svc) = self.svc.as_mut() {
            svc.freeze_adaptation(frozen);
        }
        self.forward_log();
    }

    fn stop(&mut self) {
        if let Some(svc) = self.svc.as_mut() {
            let _ = svc.stop();
        }
        self.forward_log();
    }

    fn abort(&mut self, reason: &str) {
        if let Some(svc) = self.svc.as_mut() {
            svc.abort(reason);
        }
        self.forward_log();
    }

    fn position(&self) -> Position {
        let Some(svc) = self.svc.as_ref() else {
            return Position::default();
        };
        let st = svc.status();
        let schedule = &svc.config().schedule;
        Position {
            level_db: st.level_db,
            rung: st.rung,
            at_level: st.iterations_at_level,
            of_level: schedule.max_iterations_per_level,
            playback: st.plays_completed,
            of_playback: st.plays_target,
            gated: st.converged_at_level,
            playing: st.state == TwrState::Playing,
            holding: st.state == TwrState::Holding,
            frozen: st.adapt_frozen,
            alarm: st.alarm_active,
        }
    }

    fn identification(&self) -> Option<IdentFigures> {
        let svc = self.svc.as_ref()?;
        let cfg = svc.config();
        let mag = svc.frf_mag();
        if mag.is_empty() {
            return None;
        }
        let coh = svc.coherence();
        let phase = svc.frf_phase_deg();
        let (lo, hi) = (cfg.sysid.f_lo_hz, cfg.sysid.f_hi_hz);
        let mut f = IdentFigures {
            freqs_hz: Vec::new(),
            gain: Vec::new(),
            phase_deg: Vec::new(),
            coherence: Vec::new(),
            coherence_floor: cfg.inverse.coherence_floor,
            min_mean_coherence: cfg.sysid.min_mean_coherence,
            unit: format!("{}/V", self.units),
        };
        for (k, &(fk, g)) in mag.iter().enumerate() {
            if fk < lo || fk > hi {
                continue;
            }
            f.freqs_hz.push(fk);
            f.gain.push(g);
            f.phase_deg.push(phase.get(k).map_or(0.0, |p| p.1));
            f.coherence.push(coh.get(k).map_or(0.0, |c| c.1));
        }
        Some(f)
    }

    fn mask(&self) -> Option<MaskFigures> {
        let svc = self.svc.as_ref()?;
        let mask = svc.trust_mask();
        if mask.is_empty() {
            return None;
        }
        let (lo, hi) = self.band();
        let reference = svc.target_spectrum_mag();
        let peak = reference
            .iter()
            .map(|r| r.1)
            .fold(0.0f64, f64::max)
            .max(f64::MIN_POSITIVE);
        let mut f = MaskFigures {
            freqs_hz: Vec::new(),
            trusted: Vec::new(),
            reference: Vec::new(),
        };
        // The correction band and an octave either side of it.
        for (k, &(fk, m)) in mask.iter().enumerate() {
            if fk < lo / 2.0 || fk > 2.0 * hi || k == 0 {
                continue;
            }
            f.freqs_hz.push(fk);
            f.trusted.push(m);
            f.reference
                .push(reference.get(k).map_or(0.0, |r| r.1) / peak);
        }
        Some(f)
    }

    fn level_target(&self) -> Option<PlayRecords> {
        let svc = self.svc.as_ref()?;
        let l = &svc.config().limits;
        Some(PlayRecords {
            fs_hz: svc.grid().fs_hz,
            target: svc.target_time(),
            response: Vec::new(),
            drive: Vec::new(),
            alarm_frac: l.envelope_alarm_frac,
            abort_frac: l.envelope_abort_frac,
            spectra: None,
        })
    }

    fn misses(&self) -> u32 {
        self.svc.as_ref().map_or(0, |s| s.law_misses_in_a_row())
    }

    fn frames_clocked(&self) -> u64 {
        self.svc.as_ref().map_or(0, |s| s.station_frames())
    }

    fn take_log(&mut self) -> Vec<(&'static str, String)> {
        std::mem::take(&mut self.log)
    }

    fn release(&mut self) {
        if let Some(mut svc) = self.svc.take() {
            let _ = svc.stop();
        }
    }

    fn take_station_facts(&mut self) -> Option<StationFacts> {
        self.station_facts.take()
    }

    fn shots(&self) -> u64 {
        self.starts.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn ident_record(&self) -> Option<IdentRecord> {
        let svc = self.svc.as_ref()?;
        let cfg = svc.config();
        let (lo, hi) = (cfg.sysid.f_lo_hz, cfg.sysid.f_hi_hz);
        let mag = svc.frf_mag();
        let phase = svc.frf_phase_deg();
        let coh = svc.coherence();
        let mask = svc.trust_mask();
        let grid_df = svc.grid().df();
        let bins: Vec<usize> = (1..mag.len().min(phase.len()))
            .filter(|&k| (lo..=hi).contains(&mag[k].0))
            .collect();
        let first = *bins.first()?;
        let df = if mag.len() > 1 {
            mag[1].0 - mag[0].0
        } else {
            0.0
        };
        Some(IdentRecord {
            first_hz: mag[first].0,
            df_hz: df,
            h: bins
                .iter()
                .map(|&k| {
                    let (g, p) = (mag[k].1, phase[k].1.to_radians());
                    (g * p.cos(), g * p.sin())
                })
                .collect(),
            coherence: bins
                .iter()
                .map(|&k| coh.get(k).map_or(0.0, |c| c.1))
                .collect(),
            trusted: bins
                .iter()
                .map(|&k| {
                    let on_grid = (mag[k].0 / grid_df.max(f64::MIN_POSITIVE)).round() as usize;
                    mask.get(on_grid).is_some_and(|m| m.1 > 0.5)
                })
                .collect(),
        })
    }

    fn lag(&self) -> Option<(f64, &'static str)> {
        let svc = self.svc.as_ref()?;
        let lag = svc.lag_frames()? as f64;
        Some((
            lag,
            match svc.lag_provenance() {
                Some(LagSource::Measured) => "measured",
                _ => "derived",
            },
        ))
    }

    fn full_level_target(&self) -> Option<Vec<f64>> {
        self.svc.as_ref()?;
        let t = self.target_at(0.0);
        (!t.is_empty()).then_some(t)
    }

    fn take_law_calls(&mut self) -> Vec<hertzforge_twr::TwrCallRecord> {
        self.svc
            .as_mut()
            .map(|s| s.take_law_records())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    //! The engine against the controller's own TWR service on the simulated station:
    //! what the wire is told of a real run (identification, rows, station time, the
    //! end by its kind), with the core's own iteration in place of the law seam.

    use super::*;
    use crate::station::FixedScanner;
    use crate::twr::reference::{self, ImportOptions, ImportRequest, Source};
    use hertzforge_channel_table::Units;

    fn ctx() -> EngineContext {
        EngineContext {
            signals: Signals::new(),
            progress: Arc::new(Progress::default()),
            sim_speed: 0.0,
        }
    }

    fn table() -> ChannelTable {
        let mut t = crate::table::Table::new().table;
        let mut c = crate::table::new_row(1);
        c.role = ChannelRole::Control;
        let mut d = crate::table::new_row(2);
        d.role = ChannelRole::Drive;
        d.units = Units::VoltageV;
        t.channels = vec![c, d];
        t
    }

    fn plan(cfg: serde_json::Value) -> ArmPlan {
        let root = std::env::temp_dir().join(format!(
            "sds-twr-engine-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let text: String = (0..2048)
            .map(|i| {
                let k = ((i + 16) % 64) as f64;
                format!("{}\n", 0.25 * (1.0 - (k - 32.0).abs() / 16.0))
            })
            .collect();
        let stored = reference::import(
            &root,
            &ImportRequest {
                source: Source::Text(text),
                options: ImportOptions {
                    fs_hz: Some(12_800.0),
                    column: 0,
                    units: None,
                },
                name: None,
            },
        )
        .unwrap();
        let mut cfg = cfg;
        cfg["reference"] = serde_json::json!({"sha256": stored.sha256});
        let w = crate::twr::config::parse(&cfg).unwrap();
        ArmPlan {
            cfg: Arc::new(w),
            stored: Arc::new(stored),
            facts: crate::laws::builtin_twr_facts(),
            table: table(),
            station: StationPlan::sim(12_800.0),
            scanner: Arc::new(FixedScanner(Err(
                hertzforge_station::StationBuildError::NoNiDevices,
            ))),
            data_root: root,
            bind: "ws://127.0.0.1:0/".into(),
            law: Err(MethodError::internal("no law in this test")),
            ack: None,
            devices: Vec::new(),
            streams: false,
            stream_buffer_s: 1.0,
            station_frame: Arc::new(AtomicU64::new(0)),
        }
    }

    fn config() -> serde_json::Value {
        serde_json::json!({
            "reference": {"sha256": "0".repeat(64)},
            "identification": {"duration_s": 1, "drive_vrms": 0.05},
            "schedule": {"levels_db": [-6, 0], "max_iterations": 8},
            "limits": {"drive_vpk_ceiling": 5}
        })
    }

    #[test]
    fn a_run_on_the_core_maps_to_rows_station_time_and_its_end() {
        let mut e = ServiceEngine::without_law(ctx());
        let armed = match e.arm(plan(config())).unwrap() {
            ArmOutcome::Armed(a) => a,
            ArmOutcome::Blocked(b) => panic!("blocked: {b:?}"),
        };
        assert_eq!(armed.fs_hz, 12_800.0);
        assert_eq!(armed.record_len, 4096);
        assert!(armed.tap.is_some());
        let facts = match e.identify().unwrap() {
            Identified::Done { facts, review } => {
                assert!(review);
                facts
            }
            Identified::Held { message, .. } => panic!("held: {message}"),
            Identified::Ended { end, .. } => panic!("ended: {end:?}"),
        };
        assert!(facts.passed && facts.coherence_mean > 0.9, "{facts:?}");
        assert_eq!(facts.lag_source, "measured");
        assert!(facts.coverage.is_some_and(|c| c > 0.9));
        let after_id = e.frames_clocked();
        assert!(
            after_id >= 12_800,
            "the identification's frames count: {after_id}"
        );
        assert!(e.identification().is_some_and(|f| !f.phase_deg.is_empty()));
        assert!(e.mask().is_some_and(|m| m.trusted.contains(&1.0)));
        assert!(matches!(e.proceed().unwrap(), Proceeded::Ready(_)));
        let mut rows = Vec::new();
        let end = loop {
            let p = e.play().unwrap();
            assert!(p.cancelled_at.is_none());
            if let Some(r) = p.row {
                assert!(p.records.is_some());
                rows.push(r);
            }
            if let Some(end) = p.end {
                break end;
            }
            assert!(rows.len() < 40, "the run did not end");
        };
        assert_eq!(end.outcome, "completed", "{end:?}");
        // Rows in order, the first at the first level, the last a playback play,
        // each starting where the station's clock had got to.
        assert_eq!(rows[0].index, 0);
        assert_eq!(rows[0].level_db, -6.0);
        assert_eq!(rows[0].start_frame, after_id);
        assert!(rows
            .windows(2)
            .all(|w| w[1].start_frame == w[0].start_frame + w[0].frames));
        let last = rows.last().unwrap();
        assert!(
            last.playback && last.gated && last.level_db == 0.0,
            "{last:?}"
        );
        assert_eq!(e.frames_clocked(), last.start_frame + last.frames);
        assert!(rows.iter().all(|r| r.lag_frames.is_none()));
        // Every play says what the device took before start, and it is drive.
        let committed: Vec<String> = e
            .take_log()
            .into_iter()
            .filter(|(_, t)| t.contains("frames committed before start"))
            .map(|(_, t)| t)
            .collect();
        assert_eq!(committed.len(), rows.len(), "{committed:?}");
        assert!(
            committed.iter().all(|t| !t.contains(": 0 frames")),
            "{committed:?}"
        );
    }

    #[test]
    fn the_held_coherence_is_read_from_the_cores_words() {
        assert_eq!(
            held_coherence(
                "system identification: in-band mean coherence 0.240 below floor 0.800 - raise"
            ),
            Some(0.24)
        );
        assert_eq!(held_coherence("sysid lag correlation found no peak"), None);
    }

    #[test]
    fn a_coverage_trip_ends_the_run_by_its_kind() {
        let mut cfg = config();
        cfg["correction"] = serde_json::json!({"band_hz": [20, 100], "min_band_energy": 0.95});
        cfg["prep"] = serde_json::json!({"highpass_hz": 5});
        let mut e = ServiceEngine::without_law(ctx());
        assert!(matches!(e.arm(plan(cfg)).unwrap(), ArmOutcome::Armed(_)));
        match e.identify().unwrap() {
            Identified::Ended { end, .. } => {
                assert_eq!(end.outcome, "aborted");
                let trip = end.trip.expect("a trip");
                assert_eq!(trip.kind(), "coverage");
                assert_eq!(trip.play, None);
            }
            _ => panic!("the coverage floor should end the run"),
        }
    }

    #[test]
    fn a_law_that_cannot_be_built_refuses_the_arm() {
        let mut e = ServiceEngine::new(ctx());
        let Err(err) = e.arm(plan(config())) else {
            panic!("armed without a law");
        };
        assert!(err.message.contains("no law"), "{}", err.message);
    }

    // ---- the station's rate (bench T0, 2026-11-08) --------------------------------
    //
    // On the bench the run asked 12 800 Hz of an NI station whose caps state the
    // chassis's default, 51 200 Hz, and the run came up at 51 200 Hz. The simulator
    // never showed it because the service builds the simulated station AT the asked
    // rate. These build it at the default rate instead, as the NI station is built,
    // and ask 12 800 Hz of it: the simulator can run at either, as the 9234 can run at
    // 51 200 / 4.

    /// The rate the NI station's caps state before any configure.
    const DEFAULT_HZ: f64 = 51_200.0;
    /// The rate the run asks (bench T0's).
    const ASKED_HZ: f64 = 12_800.0;

    /// The run's configuration asking `ASKED_HZ` of the station.
    fn asked(mut cfg: serde_json::Value) -> serde_json::Value {
        cfg["acquisition"] = serde_json::json!({"fs_hz": ASKED_HZ});
        cfg
    }

    /// The plan on a simulated station built at the default rate, not the asked one.
    fn on_a_default_rate_station(mut p: ArmPlan) -> ArmPlan {
        p.station = StationPlan::sim(DEFAULT_HZ);
        p
    }

    /// The record the service derived at configure, at the asked rate.
    fn derived(p: &ArmPlan) -> reference::Prepared {
        let shape = &p.cfg.shape;
        let fs = shape.fs_for(p.stored.fs_hz);
        assert_eq!(fs, ASKED_HZ, "the configuration asks the rate");
        reference::prepare(&p.stored, shape, fs, shape.band(p.cfg.sysid_band_hz))
            .expect("the configuration prepares")
    }

    /// A station that runs only at its native rate, whatever it is asked: the shape
    /// of a device that cannot program the rate a run asks. It counts the plays it
    /// is given, so a test can say nothing played.
    struct NativeRateOnly {
        inner: Box<dyn hertzforge_hal::Backend>,
        native_hz: f64,
        starts: Arc<std::sync::atomic::AtomicUsize>,
        written: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl hertzforge_hal::Backend for NativeRateOnly {
        fn caps(&self) -> hertzforge_hal::Caps {
            self.inner.caps()
        }
        fn open(&mut self) -> Result<(), hertzforge_hal::HalError> {
            self.inner.open()
        }
        fn close(&mut self) -> Result<(), hertzforge_hal::HalError> {
            self.inner.close()
        }
        fn take_warnings(&mut self) -> Vec<String> {
            self.inner.take_warnings()
        }
        fn configure(
            &mut self,
            table: &ChannelTable,
            acq: &hertzforge_hal::AcquisitionConfig,
        ) -> Result<hertzforge_hal::ConfiguredActual, hertzforge_hal::HalError> {
            let native = hertzforge_hal::AcquisitionConfig::new(self.native_hz, acq.block_frames);
            self.inner.configure(table, &native)
        }
        fn arm(&mut self) -> Result<(), hertzforge_hal::HalError> {
            self.inner.arm()
        }
        fn start(&mut self) -> Result<(), hertzforge_hal::HalError> {
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.start()
        }
        fn stop(&mut self) -> Result<(), hertzforge_hal::HalError> {
            self.inner.stop()
        }
        fn read_input(
            &mut self,
            out: &mut [f32],
        ) -> Result<hertzforge_hal::InputRead, hertzforge_hal::HalError> {
            self.inner.read_input(out)
        }
        fn write_drive(&mut self, drive: &[f32]) -> Result<usize, hertzforge_hal::HalError> {
            self.written
                .fetch_add(drive.len(), std::sync::atomic::Ordering::SeqCst);
            self.inner.write_drive(drive)
        }
        fn output_space(&mut self) -> Option<usize> {
            self.inner.output_space()
        }
        fn input_overloads(&mut self) -> Vec<bool> {
            self.inner.input_overloads()
        }
        fn poll_status(&mut self) -> hertzforge_hal::BackendStatus {
            self.inner.poll_status()
        }
        fn emergency_stop(&mut self) -> Result<(), hertzforge_hal::HalError> {
            self.inner.emergency_stop()
        }
    }

    /// **The run comes up at the rate it asked, on the record the service derived.**
    ///
    /// Bench T0's configure derived 12 800 Hz and a record of 65 536 samples; the run
    /// came up at 51 200 Hz and 262 144 samples, because the player configured the
    /// station at the rate its caps state and the core prepared the record at that
    /// rate. Here the reference is 2048 samples at 12 800 Hz: a record of 4096 at the
    /// asked rate, of 16 384 at the default one.
    #[test]
    fn a_station_built_at_its_default_rate_runs_the_run_at_the_rate_asked() {
        let p = on_a_default_rate_station(plan(asked(config())));
        let want = derived(&p);
        assert_eq!((want.grid.fs_hz, want.grid.n), (ASKED_HZ, 4096));
        let mut e = ServiceEngine::without_law(ctx());
        let armed = match e.arm(p).unwrap() {
            ArmOutcome::Armed(a) => a,
            ArmOutcome::Blocked(b) => panic!("blocked: {b:?}"),
        };
        assert_eq!(
            (armed.fs_hz, armed.record_len),
            (ASKED_HZ, want.grid.n),
            "the run came up at {} Hz on a record of {} samples; it asked {ASKED_HZ} Hz and \
             the service derived {} samples",
            armed.fs_hz,
            armed.record_len,
            want.grid.n
        );
        assert_eq!(e.svc.as_ref().unwrap().grid(), &want.grid);

        match e.identify().unwrap() {
            Identified::Done { .. } => {}
            Identified::Held { message, .. } => panic!("held: {message}"),
            Identified::Ended { end, .. } => panic!("ended: {end:?}"),
        }
        let st = e.svc.as_ref().unwrap().status();
        assert_eq!(st.requested_fs_hz, ASKED_HZ, "the rate the run asked");
        assert_eq!(
            st.actual_fs_hz,
            Some(ASKED_HZ),
            "the rate the station adopted"
        );
        // One second of identification and its rest pad, at the asked rate; at the
        // default rate it is 51 200 frames and more.
        let clocked = e.frames_clocked();
        assert!(
            (12_800..2 * 12_800).contains(&clocked),
            "the identification clocked {clocked} frames"
        );
    }

    /// **An explicit section is the section configured, in time and in content.**
    ///
    /// A section is a sample count of the prepared reference. Prepared at a rate four
    /// times the asked one, the same count is a quarter of the time: the section
    /// configured as 80 ms of the reference plays as its first 20 ms.
    #[test]
    fn an_explicit_section_keeps_its_length_in_time_on_a_default_rate_station() {
        let mut cfg = asked(config());
        cfg["section"] = serde_json::json!({"start": 256, "len": 1024});
        let p = on_a_default_rate_station(plan(cfg));
        let want = derived(&p);
        let mut e = ServiceEngine::without_law(ctx());
        assert!(matches!(e.arm(p).unwrap(), ArmOutcome::Armed(_)));
        let svc = e.svc.as_ref().unwrap();
        let g = svc.grid();
        assert_eq!(
            g.section_len as f64 / g.fs_hz,
            1024.0 / ASKED_HZ,
            "the section lasts {} s on the run's record; configured it lasts {} s",
            g.section_len as f64 / g.fs_hz,
            1024.0 / ASKED_HZ
        );
        assert_eq!(g, &want.grid, "the record is the record configured");
        // The same samples: the core's full-level target is the service's.
        let k = 10f64.powf(svc.status().level_db / 20.0);
        let core: Vec<f64> = svc.target_time().iter().map(|v| v / k).collect();
        assert_eq!(core.len(), want.target.len());
        let worst = core
            .iter()
            .zip(&want.target)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(worst < 1e-9, "the core's target differs by {worst}");
    }

    /// **A station that cannot run at the asked rate is refused at arm, by name,
    /// before anything plays** - never re-gridded at the rate it can run at.
    #[test]
    fn a_station_that_cannot_run_at_the_asked_rate_is_refused_at_arm() {
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut e = ServiceEngine::without_law(ctx());
        let (s, w) = (starts.clone(), written.clone());
        e.wrap_station = Some(Box::new(move |inner| {
            Box::new(NativeRateOnly {
                inner,
                native_hz: DEFAULT_HZ,
                starts: s.clone(),
                written: w.clone(),
            })
        }));
        let outcome = e.arm(on_a_default_rate_station(plan(asked(config()))));
        let err = match outcome {
            Err(err) => err,
            Ok(ArmOutcome::Armed(a)) => panic!(
                "armed at {} Hz on a record of {} samples, for a run that asked {ASKED_HZ} Hz",
                a.fs_hz, a.record_len
            ),
            Ok(ArmOutcome::Blocked(b)) => panic!("blocked: {b:?}"),
        };
        assert_eq!(err.code, ErrorCode::Refused, "{}", err.message);
        assert_eq!(err.reason, Some("arm_refused"), "{}", err.message);
        assert!(
            err.message.contains("12800") && err.message.contains("51200"),
            "the refusal names both rates: {}",
            err.message
        );
        assert!(
            err.message.contains("never resamples"),
            "the refusal is the rate policy's: {}",
            err.message
        );
        assert!(e.svc.is_none(), "nothing is left armed");
        assert_eq!(
            (
                starts.load(std::sync::atomic::Ordering::SeqCst),
                written.load(std::sync::atomic::Ordering::SeqCst)
            ),
            (0, 0),
            "nothing played"
        );
    }
}
