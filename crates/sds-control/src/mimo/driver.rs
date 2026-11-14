//! What the service needs from a MIMO Random run: the [`MimoRunDriver`] trait, and
//! the plain types it trades in.
//!
//! The service owns the socket, the session and the subscriptions; a driver owns
//! the run. The service never reaches past this trait, so the simulated driver
//! ([`super::sim::SimulatedMimoDriver`]) and the controller's MIMO service (through
//! an adapter) are interchangeable behind it. A driver's methods return promptly:
//! a driver whose run works on its own thread answers from its last published
//! state.

use super::setup::MimoSetup;
use crate::protocol::MethodError;
use crate::random::view::{End, State};
use crate::views::Role;
use std::sync::Arc;

/// How a controlled stop was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// An operator's stop, from any connected screen.
    OperatorStop,
    /// Nobody held control for the timeout; the service stops the run.
    SafeStop,
}

impl StopOutcome {
    pub fn name(self) -> &'static str {
        match self {
            StopOutcome::OperatorStop => "operator_stop",
            StopOutcome::SafeStop => "safe_stop",
        }
    }
}

/// The law a run uses, as it names itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LawRef {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Empty for a built-in law.
    pub sha256: String,
}

/// The identification gates, once identification has measured them.
#[derive(Debug, Clone, PartialEq)]
pub struct Gates {
    /// The smallest multiple coherence of any control channel, over the band.
    pub coherence_min: f64,
    pub coherence_floor: f64,
    /// The largest condition number of the FRF over the band.
    pub condition_max: f64,
    pub condition_limit: f64,
    /// Each controlled quantity's multiple coherence over the band, in the setup's
    /// order (NaN for one excluded at the gate); empty when not measured per
    /// control.
    pub coherence: Vec<f64>,
}

impl Gates {
    pub fn passed(&self) -> bool {
        self.coherence_min >= self.coherence_floor && self.condition_max <= self.condition_limit
    }
}

/// A fault the run reports (it may or may not have ended the run).
#[derive(Debug, Clone, PartialEq)]
pub struct Fault {
    pub kind: &'static str,
    pub message: String,
}

/// The prediction made after identification, before the loop engages: what the
/// chosen law would ask of each drive and leave on each control.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    /// How many times the prediction has been computed.
    pub iteration: u32,
    pub law: LawRef,
    /// Per drive: predicted rms, V, and its ceiling.
    pub drive_rms_v: Vec<f64>,
    pub ceiling_vrms: Vec<f64>,
    /// Per controlled quantity: the predicted rms error over the band, dB.
    pub error_db: Vec<f64>,
    /// True when controls outnumber the drives, so only part of the reference is
    /// reachable (the achievable reference, `pred_sdm` on the display).
    pub has_achievable: bool,
    /// Per controlled quantity: the achievable reference's rms against the
    /// reference's over the band, dB (0 where all of it is reachable).
    pub unreachable_db: Vec<f64>,
    /// Per drive: the predicted rms acceleration at its shaker at full level, in
    /// the controls' unit, read at the control that drive moves most. Empty when
    /// the run cannot say.
    pub drive_accel: Vec<f64>,
    /// Per drive: the control position that acceleration is read at.
    pub accel_control: Vec<usize>,
    /// Per drive: the shaker's feasibility against that acceleration.
    pub feasibility: Vec<DriveVerdict>,
}

/// One drive's feasibility verdict, as the wire names it (`pass`, `advisory`,
/// `exceeded`, `not_evaluated`), with what failed in words.
#[derive(Debug, Clone, PartialEq)]
pub struct DriveVerdict {
    pub verdict: &'static str,
    pub message: String,
}

/// What an override acknowledges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideReason {
    /// Identification failed a gate (low coherence, ill-conditioned lines).
    Gate,
    /// The prediction asks a drive for more than its ceiling.
    Prediction,
}

impl OverrideReason {
    pub fn name(self) -> &'static str {
        match self {
            OverrideReason::Gate => "gate_failed",
            OverrideReason::Prediction => "prediction_exceeded",
        }
    }

    pub fn from_name(s: &str) -> Option<OverrideReason> {
        match s {
            "gate_failed" => Some(OverrideReason::Gate),
            "prediction_exceeded" => Some(OverrideReason::Prediction),
            _ => None,
        }
    }
}

/// An operator's acknowledgement: who, their words, and what it overrode.
#[derive(Debug, Clone, PartialEq)]
pub struct OverrideAck {
    pub by: String,
    pub text: String,
    pub reason: OverrideReason,
}

impl Prediction {
    /// Every drive under its ceiling.
    pub fn drives_pass(&self) -> bool {
        self.drive_rms_v
            .iter()
            .zip(&self.ceiling_vrms)
            .all(|(r, c)| r <= c)
    }
}

/// What a run is doing, as of now.
#[derive(Debug, Clone, PartialEq)]
pub struct MimoStatus {
    pub state: State,
    /// The controller's phase in snake case (schema `mimoPhase`).
    pub phase: &'static str,
    pub elapsed_s: Option<f64>,
    /// The commanded level (the identification level during identification).
    pub level_db: Option<f64>,
    pub at_level_s: Option<f64>,
    pub remaining_at_level_s: Option<f64>,
    /// Control loops since the start.
    pub loop_index: u64,
    pub alarm: bool,
    /// Per controlled quantity: rms error over the band against the reference, dB.
    /// Empty before the first loop.
    pub control_error_db: Vec<f64>,
    pub gates: Option<Gates>,
    /// Per drive: the cap factor the core applied this loop (1 when none). Empty
    /// before the first loop.
    pub caps: Vec<f64>,
    /// Per drive: rms, V. Empty before anything drives.
    pub drive_rms_v: Vec<f64>,
    pub law: LawRef,
    pub faults: Vec<Fault>,
    /// The operator's acknowledgements, in order (a failed gate, then a prediction
    /// over a ceiling, each needs its own).
    pub override_ack: Vec<OverrideAck>,
    pub prediction: Option<Prediction>,
    /// Controls taken out of control at the gate, by position in the setup's
    /// order: still measured and recorded, as monitors. Their entries in the
    /// per-control lists are empty (null).
    pub excluded: Vec<usize>,
    pub end: Option<End>,
    /// Display frames are due: from the start of identification.
    pub streaming: bool,
    /// The run's record from arm on: its folder, whether it keeps the streams, and
    /// the latest problem writing it.
    pub record: Option<crate::random::view::RecordRef>,
}

/// One quantity a display asks for, with its selector resolved to positions
/// (controls and drives counted from 0 in the setup's order).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuantitySelector {
    /// A control's PSD (`psd(i)`).
    Psd(usize),
    /// Coherence between two controls (`coh(i,j)`).
    Coh(usize, usize),
    /// Phase between two controls, degrees (`phase(i,j)`).
    Phase(usize, usize),
    /// Cross-spectrum magnitude between two controls (`csd_mag(i,j)`).
    CsdMag(usize, usize),
    /// The dB error against the reference of the listed controls, per line
    /// (`error_db(all)`, the heatmap's rows).
    ErrorDb(Vec<usize>),
    /// The coherence error of the listed controls, per line: each control's rms
    /// over its partners of the measured squared coherence minus the reference's
    /// (`coh_error(all)`, the heatmap's second mode).
    CohError(Vec<usize>),
    /// A drive's PSD (`drive_psd(k)`).
    DrivePsd(usize),
    /// Coherence between two drives (`drive_coh(k,l)`).
    DriveCoh(usize, usize),
    /// The FRF from drive `k` to control `i` (`frf(i,k)`).
    Frf(usize, usize),
    /// The FRF's singular values per line (`sv(all)`).
    Sv,
    /// The FRF's condition number per line (`cond`).
    Cond,
    /// A control's multiple coherence with the drives (`mcoh(i)`).
    Mcoh(usize),
    /// The trace: the sum of the control PSDs against the reference's (`trace`).
    Trace,
    /// The packed response SDM of the listed controls, with the reference's (the
    /// matrix grid's window).
    SdmBlock(Vec<usize>),
    /// The predicted error of a control, per line (`pred_error_db(i)`).
    PredErrorDb(usize),
    /// The achievable reference `P G_ref P^H` of the listed controls, packed, with
    /// the reference's (`pred_sdm`, the Predict page's grid).
    PredSdmBlock(Vec<usize>),
    /// A law diagnostic by its key, at a control or drive for a per-channel
    /// layout.
    LawDiag(String, Option<usize>),
}

/// One named curve on the lines of a [`Series::Lines`].
#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    pub name: String,
    pub role: Role,
    pub y: Vec<f64>,
}

/// A packed Hermitian block: `n` by `n` per line as its upper triangle, row-major,
/// lines in order (hertzforge-sdm's layout).
#[derive(Debug, Clone, PartialEq)]
pub struct PackedBlock {
    pub n: usize,
    pub re: Vec<f64>,
    pub im: Vec<f64>,
}

/// A quantity's values, at full resolution. The service decimates them to the
/// subscribed plot's width.
#[derive(Debug, Clone, PartialEq)]
pub enum Series {
    /// Curves against the run's lines.
    Lines {
        freqs: Arc<Vec<f64>>,
        curves: Vec<Curve>,
    },
    /// Rows of values against the run's lines (a heatmap).
    Rows {
        freqs: Arc<Vec<f64>>,
        rows: Vec<Vec<f64>>,
    },
    /// A packed block of the response SDM, with the reference's.
    Packed {
        freqs: Arc<Vec<f64>>,
        measured: PackedBlock,
        reference: Option<PackedBlock>,
    },
    /// One number (a per-loop scalar diagnostic).
    Scalar(f64),
}

/// The report at the end of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct MimoReport {
    pub law: LawRef,
    pub end: Option<End>,
    pub loops: u64,
    /// Per controlled quantity: rms over the band, its reference's, and the error, dB.
    pub control_rms: Vec<f64>,
    pub reference_rms: Vec<f64>,
    pub control_error_db: Vec<f64>,
    /// Per drive: final rms, V, and the smallest cap factor applied.
    pub drive_rms_v: Vec<f64>,
    pub min_cap: Vec<f64>,
    pub gates: Option<Gates>,
    pub override_ack: Vec<OverrideAck>,
}

/// What a run starts on: the station the service selected.
#[derive(Debug, Clone, PartialEq)]
pub struct StartContext {
    /// `sim`, `ni_simulated` or `ni_real`.
    pub class: &'static str,
    pub kind: &'static str,
    /// The channel table as it stands (`None` where a driver is only checking a
    /// setup).
    pub table: Option<hertzforge_channel_table::ChannelTable>,
    /// The NI devices the station selection names (empty on the simulator).
    pub ni_devices: Vec<String>,
    /// The run's id (`m-0001`), for its record's folder and manifest.
    pub run_id: String,
    /// The channel table's revision, and the devices of the last scan (their models
    /// and serial numbers), for the run's record.
    pub revision: u64,
    pub devices: Vec<hertzforge_hal::DiscoveredDevice>,
}

/// An identification set on disk, as the protocol describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentificationInfo {
    /// The folder holding the set.
    pub path: String,
    /// The setup hash it was measured on.
    pub setup_hash: String,
    pub frames: usize,
    /// The controls' level against the reference during identification, dB.
    pub level_db: f64,
    /// The rms each drive played, V.
    pub drive_vrms: Vec<f64>,
}

impl IdentificationInfo {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "setup_hash": self.setup_hash,
            "frames": self.frames,
            "level_db": crate::protocol::num(self.level_db),
            "drive_vrms": self.drive_vrms.iter().map(|x| crate::protocol::num(*x)).collect::<Vec<_>>(),
        })
    }
}

fn not_offered(what: &str) -> MethodError {
    MethodError::named(
        crate::protocol::ErrorCode::Refused,
        "wrong_state",
        format!("{what} is not offered by this run."),
    )
}

/// What the service needs from a MIMO Random run.
///
/// Calls are made under the service's lock on the driver, one at a time, from a
/// blocking-capable thread; each returns promptly. Refusals are [`MethodError`]s
/// with a `reason` from the protocol's list and a message that says what to do.
pub trait MimoRunDriver: Send {
    /// Take the setup and check what only a run can: ranks and conditioning of the
    /// transformations, the reference's positive semi-definiteness, feasibility per
    /// drive. `items` are the setup's own findings (channels, law, ranges), which the
    /// report carries first; `law` is `None` when the setup's law is not in the
    /// catalog. Nothing is opened or driven.
    fn configure(
        &mut self,
        setup: Arc<MimoSetup>,
        law: Option<LawRef>,
        items: crate::table::Report,
    ) -> super::check::Validation;
    /// Arm and start: identification, its gates, then the review hold or the loop.
    fn start(&mut self, ctx: &StartContext) -> Result<(), MethodError>;
    /// Release the review hold after identification passed its gates.
    fn proceed(&mut self) -> Result<(), MethodError>;
    /// Engage past what holds the run (a failed gate, or a prediction over a
    /// ceiling), with the operator's own words, kept with the run. `reason` names
    /// what the operator acknowledges; `None`: whatever holds the run now. One
    /// acknowledgement passes one hold: after a gate override a prediction over a
    /// ceiling still holds the run until it is acknowledged too.
    fn override_gate(
        &mut self,
        ack: &str,
        by: &str,
        reason: Option<OverrideReason>,
    ) -> Result<(), MethodError>;
    /// Recompute the prediction during the review hold, with a changed law or law
    /// parameters (`None`: the run's own).
    fn iterate_prediction(
        &mut self,
        law: Option<(LawRef, serde_json::Map<String, serde_json::Value>)>,
    ) -> Result<Prediction, MethodError>;
    /// The controlled stop (state `stopping`, then `finished`).
    fn stop(&mut self, outcome: StopOutcome, by: &str);
    /// End within one loop, without the shutdown ramp.
    fn abort(&mut self, by: &str);
    /// The run's state now.
    fn status(&mut self) -> MimoStatus;
    /// Only the run's state, for the service's run-exclusion checks (cheap where
    /// [`Self::status`] is not).
    fn state(&mut self) -> State {
        self.status().state
    }
    /// One quantity at full resolution, or `None` when it is not measured yet or
    /// does not exist on this run (an index past the end, say).
    fn quantity(&mut self, q: &QuantitySelector) -> Option<Series>;
    /// The report, once the run has finished.
    fn report(&mut self) -> Option<MimoReport>;
    /// The controlling client went without a goodbye; the run goes on and the
    /// service's orphan clock decides. Logged by the run.
    fn controller_lost(&mut self, _name: &str) {}
    /// The controlling client came back within the timeout.
    fn controller_back(&mut self, _name: &str) {}
    /// Use the identification set in the folder `path` for this run, before it
    /// starts: the run then skips the noise floor and identification. Refused
    /// (`identification_mismatch`) when it was measured on another setup, checked
    /// against `table` as it stands.
    fn load_identification(
        &mut self,
        _path: &std::path::Path,
        _table: &hertzforge_channel_table::ChannelTable,
    ) -> Result<IdentificationInfo, MethodError> {
        Err(not_offered("Loading an identification set"))
    }
    /// Take the controls at `positions` out of control at a failed gate, with the
    /// operator's words: each is kept as a monitor, the reference is reduced to
    /// the rest, and the gate is judged again on them.
    fn exclude_controls(
        &mut self,
        _positions: &[usize],
        _ack: &str,
        _by: &str,
    ) -> Result<(), MethodError> {
        Err(not_offered("Excluding a control"))
    }
    /// Copy this run's identification set into the folder `path`.
    fn save_identification(
        &mut self,
        _path: &std::path::Path,
    ) -> Result<IdentificationInfo, MethodError> {
        Err(not_offered("Saving the identification set"))
    }
}

/// Makes a driver for a setup on the selected station.
pub type DriverFactory = Arc<dyn Fn(&StartContext) -> Box<dyn MimoRunDriver> + Send + Sync>;
