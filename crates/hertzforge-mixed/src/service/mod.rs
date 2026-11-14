//! The Mixed Mode run service: [`MixedService`].
//!
//! Its lifecycle and method set are Sine's (`SineService`), so a control
//! service's run thread drives it the same way: it owns the service outright,
//! builds it with [`MixedService::new`], gives it its laws, arms it, runs the
//! identification and the coherence gate, holds for review, proceeds, then
//! calls [`MixedService::run_chunk`] (about a quarter second of station time
//! each) until it returns `false`, posting the operator's verbs between chunks
//! and reading [`MixedService::live_status`] and [`MixedService::display`].
//!
//! The core always holds at review (when `identification.review_hold` is on);
//! the run thread proceeds. The core keeps its own cancel flag
//! ([`MixedService::sysid_cancel_flag`]) and abort latch, which the run thread
//! bridges from its own.
//!
//! This is the contract the run thread and the core are built against. What
//! the run needs before it can drive is refused [`MixedError::NotBuilt`] until
//! the core lands; signatures do not change when it does.

pub mod display;
pub mod law_records;
mod live;
pub(crate) mod run;
pub mod status;
pub(crate) mod step;
pub use step::{ABORT_RAMP_S, TONE_ABORT_DEBOUNCE_S, TONE_MISS_NAMES};

use crate::parts::{ConfigError, MixedConfig, MixedLawGuards};
use crate::schedule::PartClock;
use hertzforge_channel_table::ChannelTable;
use hertzforge_hal::Backend;
use hertzforge_random::{HinvConfig, RunMode};
use status::{MixedLog, MixedOutcome, MixedState, MixedStatus, MixedTrip};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Why a service operation was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum MixedError {
    /// The configuration was refused (by name: [`ConfigError::name`]).
    Config(ConfigError),
    /// The operation is not available in the core yet. Names the operation.
    NotBuilt(&'static str),
    /// The operation is not allowed in the present state.
    BadState(String),
    /// The station refused an operation.
    Plant(String),
    /// A law could not be built or seeded.
    Law(String),
    /// A capture or snapshot could not be written.
    Io(String),
}

impl MixedError {
    /// The stable wire name.
    pub fn name(&self) -> &'static str {
        match self {
            MixedError::Config(e) => e.name(),
            MixedError::NotBuilt(_) => "not_built",
            MixedError::BadState(_) => "bad_state",
            MixedError::Plant(_) => "station_error",
            MixedError::Law(_) => "law_error",
            MixedError::Io(_) => "io_error",
        }
    }
}

impl std::fmt::Display for MixedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MixedError::Config(e) => write!(f, "{e}"),
            MixedError::NotBuilt(what) => write!(f, "Mixed Mode cannot {what} yet"),
            MixedError::BadState(s) => write!(f, "not now: {s}"),
            MixedError::Plant(s) => write!(f, "station: {s}"),
            MixedError::Law(s) => write!(f, "control law: {s}"),
            MixedError::Io(s) => write!(f, "could not write: {s}"),
        }
    }
}

impl std::error::Error for MixedError {}

impl From<ConfigError> for MixedError {
    fn from(e: ConfigError) -> Self {
        MixedError::Config(e)
    }
}

/// Which of the run's two laws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LawRole {
    /// The floor law (a single-output Random law).
    Floor,
    /// The tone law (a Sine law; one instance per tone).
    Tones,
}

/// The factories the run builds its laws with: one floor law, and one tone law
/// instance per tone from the same factory.
#[derive(Clone)]
pub struct MixedLawFactories {
    /// Builds the floor law.
    pub floor: Arc<dyn hertzforge_random::LawFactory>,
    /// The built-in floor law's settings (Random's `hinv`), as the floor law's
    /// choice set them; the core reads some of them too (the divergence count,
    /// the change cap's floor), so they are the law's.
    pub floor_hinv: HinvConfig,
    /// Builds each tone's law.
    pub tones: Arc<dyn hertzforge_sine::SineLawFactory>,
}

impl MixedLawFactories {
    /// The built-in laws with their defaults: `hinv` for the floor,
    /// `servo_blend` for the tones.
    pub fn builtin() -> Self {
        MixedLawFactories {
            floor: Arc::new(hertzforge_random::HinvLawFactory),
            floor_hinv: HinvConfig::default(),
            tones: Arc::new(hertzforge_sine::law::servo_blend::ServoBlendFactory {
                params: mixed_tone_params(),
            }),
        }
    }
}

/// The tone law's integral gain in a Mixed Mode run, per second (`servo_blend`
/// custom compression). Chosen on the lagged resonant plant: a tone read
/// through a 0.5 Hz tracker held 0.41 dB rms at 0.5 (0.38 at 0.25, 0.46 at 1,
/// the reading's own scatter being about 0.3 dB) and rang at 1.23 dB rms at 2,
/// Sine's "slow"; the tracker's delay is the reading delay of the tone loop's
/// stability bound (the brief's 4.2).
pub const MIXED_TONE_KI: f64 = 0.5;
/// The tone law's rate limit in a Mixed Mode run, dB per second.
pub const MIXED_TONE_MAX_RATE_DB_PER_S: f64 = 20.0;

/// The built-in tone law's parameters in a Mixed Mode run: Sine's defaults with
/// the compression a narrow tracker allows ([`MIXED_TONE_KI`]).
pub fn mixed_tone_params() -> hertzforge_sine::law::servo_blend::ServoBlendParams {
    hertzforge_sine::law::servo_blend::ServoBlendParams {
        compression: hertzforge_sine::control::CompressionPreset::Custom {
            ki: MIXED_TONE_KI,
            max_rate_db_per_s: MIXED_TONE_MAX_RATE_DB_PER_S,
        },
        ..hertzforge_sine::law::servo_blend::ServoBlendParams::default()
    }
}

impl std::fmt::Debug for MixedLawFactories {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixedLawFactories")
            .field("tones", &self.tones.describe())
            .finish_non_exhaustive()
    }
}

/// Why a live change of a law parameter was refused (Sine's reasons and wire
/// names).
#[derive(Debug, Clone, PartialEq)]
pub enum MixedAdjustRefusal {
    /// The law is not built yet (the loop has not engaged).
    NotEngaged,
    /// The law has no parameter by that key.
    UnknownKey,
    /// The parameter cannot change while the test runs.
    NotLive,
    /// The value is outside the parameter's range.
    OutOfRange {
        /// Lowest.
        min: f64,
        /// Highest.
        max: f64,
    },
    /// The law refused it; its reason.
    LawRefused(String),
}

impl MixedAdjustRefusal {
    /// The stable wire name (`adjust_refused`'s `why`).
    pub fn why(&self) -> &'static str {
        match self {
            MixedAdjustRefusal::NotEngaged => "not_engaged",
            MixedAdjustRefusal::UnknownKey => "unknown_key",
            MixedAdjustRefusal::NotLive => "not_live",
            MixedAdjustRefusal::OutOfRange { .. } => "out_of_range",
            MixedAdjustRefusal::LawRefused(_) => "law_refused",
        }
    }
}

impl std::fmt::Display for MixedAdjustRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MixedAdjustRefusal::NotEngaged => f.write_str("the control law is not built yet"),
            MixedAdjustRefusal::UnknownKey => f.write_str("the control law has no such parameter"),
            MixedAdjustRefusal::NotLive => {
                f.write_str("that parameter cannot be changed while the test runs")
            }
            MixedAdjustRefusal::OutOfRange { min, max } => {
                write!(
                    f,
                    "the value is outside the parameter's range, {min} to {max}"
                )
            }
            MixedAdjustRefusal::LawRefused(why) => write!(f, "the control law refused it: {why}"),
        }
    }
}

/// The record of a run: what it was asked, how it went, how it ended.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MixedReport {
    /// The configuration as run.
    pub config: MixedConfig,
    /// The rate the run adopted, Hz.
    pub sample_rate_hz: f64,
    /// The final state.
    pub state: MixedState,
    /// How it ended.
    pub outcome: Option<MixedOutcome>,
    /// The trip, when it tripped.
    pub trip: Option<MixedTrip>,
    /// The status at the end.
    pub status: MixedStatus,
    /// The event log.
    pub events: Vec<status::MixedEvent>,
    /// One line per part, in parts-list order.
    #[serde(default)]
    pub parts: Vec<status::PartSummary>,
}

/// The Mixed Mode run service. One per run; the run thread owns it and drops it
/// (and with it the station) when the run ends.
pub struct MixedService {
    cfg: MixedConfig,
    table: ChannelTable,
    /// The station, until `arm` opens it into the run's engine.
    #[allow(dead_code)]
    backend: Option<Box<dyn Backend>>,
    station: hertzforge_module_abi::StationClass,
    sample_rate_hz: f64,
    requested_rate_hz: f64,
    sysid_seed: u64,
    data_root: Option<PathBuf>,
    state: MixedState,
    run_mode: RunMode,
    log: MixedLog,
    laws: MixedLawFactories,
    floor_clock: Arc<dyn hertzforge_random::LawClock>,
    tone_clock: Arc<dyn hertzforge_sine::law::LawClock>,
    block_frames: Option<usize>,
    sysid_cancel: Arc<AtomicBool>,
    clock: PartClock,
    trip: Option<MixedTrip>,
    /// The run once armed.
    rt: Option<Box<run::Runtime>>,
    phase: status::MixedPhase,
    outcome: Option<MixedOutcome>,
    /// How the run ends once the drive is down (a stop on its way).
    ending: Option<MixedOutcome>,
    /// Verbs posted between chunks, applied at the next batch.
    pending: std::sync::Mutex<Vec<live::Verb>>,
    /// Station frame at engage.
    engaged_at: u64,
    /// Station frame the phase last changed at.
    phase_at: u64,
    /// Station frame the drive reached zero on the way to the end.
    zero_at: Option<u64>,
    histories: live::Histories,
    chunk_peak: f64,
    chunk_rms: f64,
    chunk_clamped: bool,
    /// The random part's sum of squares and frames toward its rms abort.
    rand_acc: (f64, u64),
    /// The level ladder: the rung the level is at or climbing to, whether it
    /// is on that rung, seconds at the present level and at full level (holds
    /// excluded).
    ladder: live::Ladder,
    /// The capture's folder and its law records not yet written.
    capture_dir: Option<PathBuf>,
    law_bytes: Vec<u8>,
    summaries: Vec<status::PartSummary>,
    /// The laws' call records kept for the host since it last took them, when
    /// it asked for them ([`MixedService::keep_law_records`]).
    law_kept: Option<Vec<law_records::MixedLawRecord>>,
}

impl std::fmt::Debug for MixedService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixedService")
            .field("state", &self.state)
            .field("sample_rate_hz", &self.sample_rate_hz)
            .field("parts", &self.cfg.parts.len())
            .finish_non_exhaustive()
    }
}

impl MixedService {
    /// Build a service for `cfg` against `table` on `backend`, the station
    /// bound to `station`. Validates the configuration at the backend's rate
    /// (every part inside the floor's band and under half the rate, tones
    /// spaced, follows resolved) and that every control and limit channel is an
    /// enabled row of `table`, then stores everything; nothing is driven.
    /// `sysid_seed` seeds the identification's noise; `data_root` is the local
    /// capture root (`None`: the default).
    ///
    /// The built-in laws are in force until [`Self::with_laws`].
    pub fn new(
        cfg: MixedConfig,
        table: ChannelTable,
        backend: Box<dyn Backend>,
        station: hertzforge_module_abi::StationClass,
        sysid_seed: u64,
        data_root: Option<PathBuf>,
    ) -> Result<Self, MixedError> {
        let sample_rate_hz = backend.caps().sample_rate_hz;
        cfg.validate(Some(sample_rate_hz))?;
        let enabled = |idx: u32| table.channels.iter().any(|c| c.index == idx && c.enabled);
        for c in &cfg.control.channels {
            if !enabled(c.channel_index) {
                return Err(ConfigError::BadConfig {
                    field: "control",
                    why: format!("channel {} is not an enabled channel", c.channel_index),
                }
                .into());
            }
        }
        for l in &cfg.limits.limit_profiles {
            if !enabled(l.channel_index) {
                return Err(ConfigError::BadConfig {
                    field: "limit_profiles",
                    why: format!("channel {} is not an enabled channel", l.channel_index),
                }
                .into());
            }
        }
        let clock = PartClock::new(&cfg)?;
        Ok(MixedService {
            run_mode: cfg.schedule.mode,
            cfg,
            table,
            backend: Some(backend),
            station,
            sample_rate_hz,
            requested_rate_hz: sample_rate_hz,
            sysid_seed,
            data_root,
            state: MixedState::Idle,
            log: MixedLog::default(),
            laws: MixedLawFactories::builtin(),
            floor_clock: Arc::new(hertzforge_random::MonotonicClock::new()),
            tone_clock: Arc::new(hertzforge_sine::law::MonotonicClock::new()),
            block_frames: None,
            sysid_cancel: Arc::new(AtomicBool::new(false)),
            clock,
            trip: None,
            rt: None,
            phase: status::MixedPhase::Idle,
            outcome: None,
            ending: None,
            pending: std::sync::Mutex::new(Vec::new()),
            engaged_at: 0,
            phase_at: 0,
            zero_at: None,
            histories: live::Histories::default(),
            chunk_peak: 0.0,
            chunk_rms: 0.0,
            chunk_clamped: false,
            rand_acc: (0.0, 0),
            ladder: live::Ladder::default(),
            capture_dir: None,
            law_bytes: Vec::new(),
            summaries: Vec::new(),
            law_kept: None,
        })
    }

    // ---- construction ----------------------------------------------------

    /// The laws the run builds: the floor law's factory and settings, and the
    /// tone law's factory (built once per tone at engage).
    pub fn with_laws(mut self, laws: MixedLawFactories) -> Self {
        self.laws = laws;
        self
    }

    /// The clocks law calls are timed on (Random's for the floor law, Sine's
    /// for the tones'); a test injects a fake to make a call late.
    pub fn with_law_clocks(
        mut self,
        floor: Arc<dyn hertzforge_random::LawClock>,
        tones: Arc<dyn hertzforge_sine::law::LawClock>,
    ) -> Self {
        self.floor_clock = floor;
        self.tone_clock = tones;
        self
    }

    /// The rate the run asks the station for, Hz. Without it the run asks for
    /// the rate the station states in its caps, which on a device that runs at
    /// several rates is its highest, not the one the setup was written for. The
    /// configuration is checked again at this rate, and the station's answer
    /// at arm is still what the run adopts.
    pub fn with_requested_rate(mut self, fs_hz: f64) -> Result<Self, MixedError> {
        if !fs_hz.is_finite() || fs_hz <= 0.0 {
            return Err(crate::parts::ConfigError::BadConfig {
                field: "fs_hz",
                why: format!("{fs_hz} Hz is not a rate a run can be asked at"),
            }
            .into());
        }
        self.cfg.validate(Some(fs_hz))?;
        self.requested_rate_hz = fs_hz;
        self.sample_rate_hz = fs_hz;
        Ok(self)
    }

    /// The block the station is pumped in, frames (the simulator takes the
    /// block a hardware station would at the rate).
    pub fn with_block_frames(mut self, frames: usize) -> Self {
        self.block_frames = Some(frames.max(1));
        self
    }

    // ---- facts -------------------------------------------------------------

    /// The station class the run is bound to.
    pub fn station(&self) -> hertzforge_module_abi::StationClass {
        self.station
    }

    /// The configuration.
    pub fn config(&self) -> &MixedConfig {
        &self.cfg
    }

    /// The channel table.
    pub fn table(&self) -> &ChannelTable {
        &self.table
    }

    /// The laws in force.
    pub fn laws(&self) -> &MixedLawFactories {
        &self.laws
    }

    /// The core's guards on the laws (the configured ones, else the defaults).
    pub fn law_guards(&self) -> MixedLawGuards {
        self.cfg.law_guards.clone().unwrap_or_default()
    }

    /// The tones' control tick, seconds.
    pub fn control_tick_s(&self) -> f64 {
        self.cfg
            .control_tick_s
            .unwrap_or(hertzforge_sine::law::DEFAULT_TICK_S)
    }

    /// The rate the run runs at, Hz: the backend's own answer at construction,
    /// replaced by the station's adopted rate at arm.
    pub fn sample_rate_hz(&self) -> f64 {
        self.sample_rate_hz
    }

    /// Whether arming moved the rate from the one asked for.
    pub fn rate_was_coerced(&self) -> bool {
        self.rt.is_some() && self.requested_rate_hz != self.sample_rate_hz
    }

    /// The station's configured facts after arm; `None` before.
    pub fn configured(&self) -> Option<hertzforge_hal::ConfiguredActual> {
        self.rt.as_ref().map(|r| r.actual)
    }

    /// The engine's health counters; `None` before the loop engages.
    pub fn engine_health(&self) -> Option<hertzforge_engine::EngineHealth> {
        self.rt.as_ref().and_then(|r| r.engine_health())
    }

    /// Frames the engine's raw ring overran (frames the watchdog never saw).
    pub fn blocks_dropped(&self) -> u64 {
        self.engine_health().map_or(0, |h| h.ring_overruns as u64)
    }

    /// The identification's seed.
    pub fn sysid_seed(&self) -> u64 {
        self.sysid_seed
    }

    /// The capture root, if one was given.
    pub fn data_root(&self) -> Option<&PathBuf> {
        self.data_root.as_ref()
    }

    /// The lifecycle state.
    pub fn state(&self) -> MixedState {
        self.state
    }

    /// The event log.
    pub fn log(&self) -> &MixedLog {
        &self.log
    }

    /// The trip, when the run tripped.
    pub fn trip(&self) -> Option<&MixedTrip> {
        self.trip.as_ref()
    }

    /// The run-end reason in plain words (the trip's, naming its part, or the
    /// law fault's); `None` while running or after a clean end.
    pub fn end_reason(&self) -> Option<String> {
        self.trip.as_ref().map(MixedTrip::words)
    }

    /// Whether test progress is held.
    pub fn is_held(&self) -> bool {
        match &self.rt {
            Some(r) => r.step.clock.held(),
            None => self.clock.held(),
        }
    }

    /// Whether the drive is at zero (before engage, after a stop or abort).
    pub fn drive_is_zero(&self) -> bool {
        self.rt.as_ref().is_none_or(|r| r.step.drive_is_zero())
    }

    /// How the level schedule advances.
    pub fn run_mode(&self) -> RunMode {
        self.run_mode
    }

    /// The identification's band coherence; 0 before it runs.
    pub fn band_coherence(&self) -> f64 {
        self.rt.as_ref().map_or(0.0, |r| r.ident.band_coherence)
    }

    /// The identification's measured pipeline lag, frames; 0 before it runs.
    pub fn sysid_lag_frames(&self) -> usize {
        self.rt.as_ref().map_or(0, |r| r.ident.lag)
    }

    /// The flag that cancels a running identification (the run thread bridges
    /// its own stop to it).
    pub fn sysid_cancel_flag(&self) -> Arc<AtomicBool> {
        self.sysid_cancel.clone()
    }

    /// The identification's complex transfer function per control channel, in
    /// the order of the configuration's control set, on the line grid of
    /// [`MixedService::frf`]: drive to response, the response taken
    /// [`MixedService::sysid_lag_frames`] later, in the file's convention
    /// (`conj(X) Y / |X|^2` with the standard transform; a delay lags). `None`
    /// until identification has run. For a host's record; nothing in the run
    /// reads it.
    pub fn frf_complex(&self) -> Option<&[Vec<(f64, f64)>]> {
        let rt = self.rt.as_ref()?;
        (!rt.ident.h.is_empty()).then_some(rt.ident.h.as_slice())
    }

    /// The identification's ordinary coherence per control channel, on the same
    /// grid; `None` until identification has run.
    pub fn coherence_by_channel(&self) -> Option<&[Vec<f64>]> {
        let rt = self.rt.as_ref()?;
        (!rt.ident.coherence_by.is_empty()).then_some(rt.ident.coherence_by.as_slice())
    }

    /// Keep every law call record the service takes between batches for the
    /// host to take ([`MixedService::take_law_records`]), or stop keeping them.
    /// Off by default. The records are taken between batches as before; keeping
    /// them is a copy there, never on the control tick, and no file is written.
    pub fn keep_law_records(&mut self, keep: bool) {
        self.law_kept = keep.then(Vec::new);
    }

    /// The law call records taken since the last take, in order; empty when they
    /// are not kept.
    pub fn take_law_records(&mut self) -> Vec<law_records::MixedLawRecord> {
        match self.law_kept.as_mut() {
            Some(kept) => std::mem::take(kept),
            None => Vec::new(),
        }
    }

    /// Each tone's tracked phase against its drive, degrees, per control channel
    /// in the order of the control set, tones in parts-list order; empty before
    /// the loop engages. Read between chunks; nothing in the run reads it.
    pub fn tone_phases_deg(&self) -> Vec<Vec<f64>> {
        let Some(rt) = self.rt.as_ref().filter(|r| r.floor_law.is_some()) else {
            return Vec::new();
        };
        let channels = self.cfg.control.channels.len();
        (0..rt.step.tones.len())
            .map(|j| {
                (0..channels)
                    .map(|c| rt.step.bank.tracker(j, c).phase().to_degrees())
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
