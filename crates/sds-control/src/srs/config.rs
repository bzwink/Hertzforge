//! The SRS shock configuration (schema/PROTOCOL.md, "SRS Shock Configuration").
//!
//! The wire configuration is parsed (a wrong type or a missing part is `bad_params`
//! naming it), then checked so every problem comes back as a named item located by
//! its `field`, then built into the controller's own [`SrsTestConfig`] with its
//! closed-loop [`SrsControl`]. The wire always sets `control` and always
//! identifies: it carries no open-loop SRS shock. Nothing here is control math;
//! the run is the controller's.

use super::preview::{self, Synthesis};
use crate::protocol::{num, ErrorCode, MethodError};
use crate::shock::config::{field_words, param_problem, Obj};
use crate::table::{Item, Report};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_dsp::srs::{octave_freqs, OctaveFraction};
use hertzforge_shaker_db::{MovingMass, ShakerRecord};
use hertzforge_shock_common::{
    pulse_feasibility, DriveLimit, DriveMonitor, DriveMonitorKind, DrivePath,
    FeasibilityTolerances, LadderAdvance, RateModel, ShockLevelSchedule, ShockRunMode,
};
use hertzforge_srs::control::SrsControl;
use hertzforge_srs::iterate::SynthesisOpts;
use hertzforge_srs::service::config::{
    BandFraction, EngineKind, EnvelopeSpec, FeasibilityTol, FrfSpec, KinematicAbort,
    LimitChannelSpec, LimitSet, LimitTarget, MetaField, ShockCaptureRules, ShockCaptureTrigger,
    SrsToleranceBand, SysIdExcitation, SysIdSpec, TargetSrs,
};
use hertzforge_srs::service::plant::channel_display_name;
use hertzforge_srs::table::{
    DampedSineComponent, DampedSineTable, Polarity, WaveletComponent, WaveletTable,
};
use hertzforge_srs::SrsTestConfig;
use serde_json::{json, Map, Value};

pub use crate::shock::config::{
    config_invalid, round, verdict, Advance, Grid, LawFacts, LawSetup, WireMonitor, DEFAULT_FS_HZ,
    DEFAULT_MAX_PULSES_PER_LEVEL, FS_RANGE, LEVEL_DB_RANGE, MAX_DRIVE_V, MAX_PULSES_PER_RUNG,
    MAX_SPACING_S,
};

/// The most bands per octave a grid may have.
pub const MAX_BANDS_PER_OCTAVE: f64 = 24.0;
/// The quality factor's bounds: above the first, at most the second.
pub const Q_RANGE: (f64, f64) = (0.5, 100.0);
/// The longest ring-out after a pulse, seconds.
pub const MAX_RING_OUT_S: f64 = 10.0;
/// The identification level's bounds, dB: at least the first, below the second.
pub const SYSID_LEVEL_DB_RANGE: (f64, f64) = (-60.0, 0.0);
/// Accepted identification FFT block sizes.
pub const SRS_NFFT_RANGE: (u64, u64) = (64, 65_536);
/// The most passes the offline synthesis may make.
pub const MAX_SYNTHESIS_ITERS: u64 = 1000;
/// The most any one step of the synthesis, or of the core's gain cap, may move a
/// component, dB.
pub const MAX_GAIN_STEP_DB: f64 = 20.0;
/// The most components a starting table may hold.
pub const MAX_COMPONENTS: usize = 512;

/// The engine's wire name.
pub fn engine_name(e: EngineKind) -> &'static str {
    match e {
        EngineKind::Wavsyn => "wavsyn",
        EngineKind::Seds => "seds",
    }
}

const ENGINES: [(&str, EngineKind); 2] =
    [("wavsyn", EngineKind::Wavsyn), ("seds", EngineKind::Seds)];

/// The excitation's wire name.
pub fn excitation_name(e: SysIdExcitation) -> &'static str {
    match e {
        SysIdExcitation::Shaped => "shaped",
        SysIdExcitation::StandardPulse => "standard_pulse",
    }
}

/// The target SRS as sent.
#[derive(Debug, Clone, PartialEq)]
pub enum WireTarget {
    Flat { accel_g: f64 },
    Breakpoints { points: Vec<(f64, f64)> },
}

impl WireTarget {
    pub fn to_json(&self) -> Value {
        match self {
            WireTarget::Flat { accel_g } => json!({"kind": "flat", "accel_g": num(*accel_g)}),
            WireTarget::Breakpoints { points } => json!({
                "kind": "breakpoints",
                "points": points.iter().map(|(f, g)| json!([num(*f), num(*g)])).collect::<Vec<_>>(),
            }),
        }
    }

    pub fn crate_target(&self) -> TargetSrs {
        match self {
            WireTarget::Flat { accel_g } => TargetSrs::Flat { accel_g: *accel_g },
            WireTarget::Breakpoints { points } => TargetSrs::Breakpoints {
                points: points.clone(),
            },
        }
    }
}

/// The band grid as sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireBands {
    pub f_lo_hz: f64,
    pub f_hi_hz: f64,
    pub bands_per_octave: f64,
    pub q: f64,
}

impl WireBands {
    /// The controller's band spacing.
    pub fn fraction(&self) -> BandFraction {
        let n = self.bands_per_octave;
        if n == 1.0 {
            BandFraction::Whole
        } else if n == 3.0 {
            BandFraction::Third
        } else if n == 6.0 {
            BandFraction::Sixth
        } else if n == 12.0 {
            BandFraction::Twelfth
        } else {
            BandFraction::Arbitrary(n)
        }
    }

    /// The band centres, Hz: from `f_lo_hz` to the first at or above `f_hi_hz`.
    pub fn centres(&self) -> Vec<f64> {
        if !(self.bands_per_octave.is_finite() && self.bands_per_octave > 0.0) {
            return Vec::new();
        }
        octave_freqs(
            self.f_lo_hz,
            self.f_hi_hz,
            OctaveFraction::from(self.fraction()),
        )
    }
}

/// The synthesis component table a configuration starts from.
#[derive(Debug, Clone, PartialEq)]
pub enum WireComponents {
    Wavsyn(Vec<WaveletComponent>),
    Seds(Vec<DampedSineComponent>),
}

impl WireComponents {
    pub fn engine(&self) -> EngineKind {
        match self {
            WireComponents::Wavsyn(_) => EngineKind::Wavsyn,
            WireComponents::Seds(_) => EngineKind::Seds,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            WireComponents::Wavsyn(r) => r.len(),
            WireComponents::Seds(r) => r.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn to_json(&self) -> Value {
        match self {
            WireComponents::Wavsyn(rows) => json!({
                "engine": "wavsyn",
                "rows": rows.iter().map(|c| json!({
                    "freq_hz": num(c.freq_hz),
                    "amplitude_g": num(c.amplitude_g),
                    "half_cycles": c.half_cycles,
                    "delay_s": num(c.delay_s),
                })).collect::<Vec<_>>(),
            }),
            WireComponents::Seds(rows) => json!({
                "engine": "seds",
                "rows": rows.iter().map(|c| json!({
                    "freq_hz": num(c.freq_hz),
                    "amplitude_g": num(c.amplitude_g),
                    "zeta": num(c.zeta),
                    "delay_s": num(c.delay_s),
                    "polarity": match c.polarity {
                        Polarity::Positive => "positive",
                        Polarity::Negative => "negative",
                    },
                })).collect::<Vec<_>>(),
            }),
        }
    }

    /// Each row's problem, by its place in the table.
    pub fn row_problems(&self) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        match self {
            WireComponents::Wavsyn(rows) => {
                for (i, c) in rows.iter().enumerate() {
                    if let Err(e) = c.validate() {
                        out.push((i, e.to_string()));
                    }
                }
            }
            WireComponents::Seds(rows) => {
                for (i, c) in rows.iter().enumerate() {
                    if let Err(e) = c.validate() {
                        out.push((i, e.to_string()));
                    }
                }
            }
        }
        out
    }

    /// The controller's synthesis table.
    pub fn engine_table(&self) -> hertzforge_srs::SynthesisEngine {
        match self {
            WireComponents::Wavsyn(rows) => hertzforge_srs::SynthesisEngine::Wavsyn(WaveletTable {
                components: rows.clone(),
            }),
            WireComponents::Seds(rows) => hertzforge_srs::SynthesisEngine::Seds(DampedSineTable {
                components: rows.clone(),
            }),
        }
    }
}

/// One band's envelope, `[under, over]` dB for the alarm and the abort.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireEnvelopeBand {
    pub alarm_db: (f64, f64),
    pub abort_db: (f64, f64),
}

/// A limit channel's bound as sent: a per-band `None` is no bound there.
#[derive(Debug, Clone, PartialEq)]
pub enum WireBound {
    Flat { accel_g: f64 },
    PerBand { bounds_g: Vec<Option<f64>> },
}

/// A limit channel as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct WireLimitChannel {
    pub channel: u32,
    pub bound: WireBound,
    /// The supplied drive-to-channel gain, `(freqs_hz, mag)`.
    pub frf: Option<(Vec<f64>, Vec<f64>)>,
}

/// Feasibility inputs as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct WireFeasibility {
    pub shaker: ShakerRecord,
    pub moving_mass: MovingMass,
    pub residual_velocity_ips: f64,
    pub residual_disp_in: f64,
}

/// What `srs.preview` takes, and the part of a configuration the offline synthesis
/// reads.
#[derive(Debug, Clone, PartialEq)]
pub struct WireSynthesis {
    pub target: WireTarget,
    pub bands: WireBands,
    pub engine: EngineKind,
    pub components: Option<WireComponents>,
    pub max_iters: u64,
    pub tol_db: f64,
    pub max_gain_db_per_iter: f64,
    pub fs_hz: f64,
}

impl WireSynthesis {
    pub fn opts(&self) -> SynthesisOpts {
        SynthesisOpts {
            max_iters: usize::try_from(self.max_iters).unwrap_or(usize::MAX),
            tol_db: self.tol_db,
            max_gain_db_per_iter: self.max_gain_db_per_iter,
            q: self.bands.q,
        }
    }
}

/// A parsed SRS shock configuration, every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct WireSrs {
    pub title: String,
    pub synthesis: WireSynthesis,
    pub envelope_alarm_db: (f64, f64),
    pub envelope_abort_db: (f64, f64),
    pub envelope_per_band: Option<Vec<WireEnvelopeBand>>,
    pub ring_out_s: f64,
    pub pvss: bool,
    pub mode: ShockRunMode,
    pub levels_db: Vec<f64>,
    pub pulses: Vec<u32>,
    pub start_db: f64,
    pub advance: Advance,
    pub max_pulses_per_level: u32,
    pub cooling_s: f64,
    pub settling_s: f64,
    pub compute_s: f64,
    pub excitation: SysIdExcitation,
    pub sysid_level_db: f64,
    pub sysid_max_vpk: Option<f64>,
    pub sysid_nfft: u64,
    pub h_floor_frac: f64,
    pub min_mean_coherence: f64,
    pub review: bool,
    pub law: LawSetup,
    pub law_budget: f64,
    pub law_misses: u32,
    pub max_pulses: u64,
    pub max_gain_rise_db: f64,
    pub drive_vpk_ceiling: f64,
    pub drive_vpk_abort: f64,
    pub peak_accel_g: Option<f64>,
    pub abort_factor: f64,
    pub kinematic: (Option<f64>, Option<f64>, Option<f64>),
    pub monitors: Vec<WireMonitor>,
    pub headroom_db: f64,
    pub limit_channels: Vec<WireLimitChannel>,
    pub feasibility: Option<WireFeasibility>,
    pub metadata: Vec<MetaField>,
    /// The operator's words for the run's record: who ran it, on what, and notes.
    pub operator: String,
    pub article: String,
    pub notes: String,
    /// What the test is run to: the document and its clause.
    pub specification: Option<(String, String)>,
    /// Whether the record holds the inputs and the drive as streams.
    pub record_streams: bool,
    /// The configuration as accepted, kept with the run.
    pub raw: Value,
}

/// What a configuration's checks are made against.
pub struct CheckContext<'a> {
    /// The channel table the run would arm with.
    pub table: &'a ChannelTable,
    /// Looks a law up by id.
    pub law: &'a dyn Fn(&str) -> Option<LawFacts>,
    /// The controller timeout, seconds: one pulse's record must be shorter.
    pub timeout_s: f64,
}

/// The derived values of a valid configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    pub grid: Grid,
    pub bands_hz: Vec<f64>,
    pub target_g: Vec<f64>,
    pub synthesis: preview::SynthesisFacts,
    pub reference: preview::ReferenceFacts,
    pub spacing_s: f64,
    pub call_budget_s: f64,
    pub feasibility: (&'static str, String),
}

impl Derived {
    pub fn to_json(&self) -> Value {
        json!({
            "grid": self.grid.to_json(),
            "bands_hz": preview::band_list(&self.bands_hz),
            "target_g": preview::value_list(&self.target_g),
            "synthesis": self.synthesis.to_json(),
            "reference": self.reference.to_json(),
            "spacing_s": num(round(self.spacing_s, 4)),
            "call_budget_s": num(round(self.call_budget_s, 4)),
            "feasibility": {"verdict": self.feasibility.0, "message": self.feasibility.1},
        })
    }
}

// ---------------------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------------------

fn bad(path: &str, message: impl Into<String>) -> MethodError {
    MethodError::bad_params(path, message)
}

/// `[under, over]` dB at `k` of `o`: how far a band may sit under and over the target.
fn db_pair(o: &Obj<'_>, k: &str) -> Result<Option<(f64, f64)>, MethodError> {
    let Some(a) = o.array(k)? else {
        return Ok(None);
    };
    let at = o.at(k);
    let wrong = || bad(&at, format!("\"{at}\" is [under, over] in dB"));
    if a.len() != 2 {
        return Err(wrong());
    }
    let lo = a[0].as_f64().filter(|x| x.is_finite()).ok_or_else(wrong)?;
    let hi = a[1].as_f64().filter(|x| x.is_finite()).ok_or_else(wrong)?;
    Ok(Some((lo, hi)))
}

/// A list of finite numbers at `k` of `o` (`None` when absent).
fn numbers(o: &Obj<'_>, k: &str) -> Result<Option<Vec<f64>>, MethodError> {
    let Some(a) = o.array(k)? else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(a.len());
    for (i, v) in a.iter().enumerate() {
        out.push(v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| {
            bad(
                &format!("{}[{i}]", o.at(k)),
                format!("\"{}\" is a list of numbers", o.at(k)),
            )
        })?);
    }
    Ok(Some(out))
}

/// Parse a `target` object at `path`.
pub fn parse_target(v: &Value, path: &str) -> Result<WireTarget, MethodError> {
    let t = Obj::of(v, path)?;
    match t.opt_str("kind")? {
        Some("flat") => Ok(WireTarget::Flat {
            accel_g: t.f64("accel_g")?,
        }),
        Some("breakpoints") => {
            let a = t.array("points")?.ok_or_else(|| {
                bad(
                    &t.at("points"),
                    "a breakpoint target lists its points, each [frequency_hz, accel_g]",
                )
            })?;
            let mut points = Vec::with_capacity(a.len());
            for (i, p) in a.iter().enumerate() {
                let at = format!("{}[{i}]", t.at("points"));
                let pair = p
                    .as_array()
                    .filter(|p| p.len() == 2)
                    .and_then(|p| {
                        let f = p[0].as_f64().filter(|x| x.is_finite())?;
                        let g = p[1].as_f64().filter(|x| x.is_finite())?;
                        Some((f, g))
                    })
                    .ok_or_else(|| bad(&at, "a breakpoint is [frequency_hz, accel_g]"))?;
                points.push(pair);
            }
            Ok(WireTarget::Breakpoints { points })
        }
        _ => Err(bad(
            &t.at("kind"),
            format!("\"{}\" is \"flat\" or \"breakpoints\"", t.at("kind")),
        )),
    }
}

/// Parse a `bands` object at `path`.
pub fn parse_bands(v: &Value, path: &str) -> Result<WireBands, MethodError> {
    let b = Obj::of(v, path)?;
    Ok(WireBands {
        f_lo_hz: b.f64("f_lo_hz")?,
        f_hi_hz: b.f64("f_hi_hz")?,
        bands_per_octave: b.f64_or("bands_per_octave", 3.0)?,
        q: b.f64_or("q", hertzforge_srs::service::config::DEFAULT_Q)?,
    })
}

/// Parse a component table (`{"engine","rows"}`) at `path`.
pub fn parse_components(v: &Value, path: &str) -> Result<WireComponents, MethodError> {
    let c = Obj::of(v, path)?;
    let engine = c.choice("engine", &ENGINES, None)?;
    let rows = c
        .array("rows")?
        .ok_or_else(|| bad(&c.at("rows"), "a component table lists its rows"))?;
    match engine {
        EngineKind::Wavsyn => {
            let mut out = Vec::with_capacity(rows.len());
            for (i, r) in rows.iter().enumerate() {
                let o = Obj::of(r, &format!("{}[{i}]", c.at("rows")))?;
                let half = o
                    .opt_count("half_cycles")?
                    .ok_or_else(|| bad(&o.at("half_cycles"), "a wavelet gives its half-cycles"))?;
                out.push(WaveletComponent {
                    freq_hz: o.f64("freq_hz")?,
                    amplitude_g: o.f64("amplitude_g")?,
                    half_cycles: u32::try_from(half).unwrap_or(u32::MAX),
                    delay_s: o.f64_or("delay_s", 0.0)?,
                });
            }
            Ok(WireComponents::Wavsyn(out))
        }
        EngineKind::Seds => {
            let mut out = Vec::with_capacity(rows.len());
            for (i, r) in rows.iter().enumerate() {
                let o = Obj::of(r, &format!("{}[{i}]", c.at("rows")))?;
                out.push(DampedSineComponent {
                    freq_hz: o.f64("freq_hz")?,
                    amplitude_g: o.f64("amplitude_g")?,
                    zeta: o.f64("zeta")?,
                    delay_s: o.f64_or("delay_s", 0.0)?,
                    polarity: o.choice(
                        "polarity",
                        &[
                            ("positive", Polarity::Positive),
                            ("negative", Polarity::Negative),
                        ],
                        Some(Polarity::Positive),
                    )?,
                });
            }
            Ok(WireComponents::Seds(out))
        }
    }
}

/// Parse a `synthesis` object at `path` (absent: the defaults) with its target,
/// band grid and rate.
pub fn parse_synthesis(
    target: WireTarget,
    bands: WireBands,
    synthesis: Option<&Value>,
    path: &str,
    fs_hz: f64,
) -> Result<WireSynthesis, MethodError> {
    let d = SynthesisOpts::default();
    let mut w = WireSynthesis {
        target,
        bands,
        engine: EngineKind::Wavsyn,
        components: None,
        max_iters: d.max_iters as u64,
        tol_db: d.tol_db,
        max_gain_db_per_iter: d.max_gain_db_per_iter,
        fs_hz,
    };
    let Some(v) = synthesis.filter(|v| !v.is_null()) else {
        return Ok(w);
    };
    let s = Obj::of(v, path)?;
    w.engine = s.choice("engine", &ENGINES, Some(EngineKind::Wavsyn))?;
    w.components = match s.m.get("components") {
        None | Some(Value::Null) => None,
        Some(c) => Some(parse_components(c, &s.at("components"))?),
    };
    w.max_iters = s.opt_count("max_iters")?.unwrap_or(w.max_iters);
    w.tol_db = s.f64_or("tol_db", w.tol_db)?;
    w.max_gain_db_per_iter = s.f64_or("max_gain_db_per_iter", w.max_gain_db_per_iter)?;
    Ok(w)
}

/// Parse `params.config`.
pub fn parse(config: &Value) -> Result<WireSrs, MethodError> {
    let c = Obj::of(config, "config")?;
    let title = c.opt_str("title")?.unwrap_or("").to_owned();
    let target = parse_target(c.require("target")?, "config.target")?;
    let bands = parse_bands(c.require("bands")?, "config.bands")?;

    let mut fs_hz = DEFAULT_FS_HZ;
    let mut ring_out_s = hertzforge_srs::service::config::DEFAULT_RING_OUT_S;
    let mut pvss = true;
    if let Some(a) = c.opt_obj("acquisition")? {
        fs_hz = a.f64_or("fs_hz", fs_hz)?;
        ring_out_s = a.f64_or("ring_out_s", ring_out_s)?;
        pvss = a.opt_bool("pvss")?.unwrap_or(pvss);
    }
    let synthesis = parse_synthesis(
        target,
        bands,
        c.m.get("synthesis"),
        "config.synthesis",
        fs_hz,
    )?;

    let (mut alarm, mut abort) = (
        hertzforge_srs::service::config::DEFAULT_ALARM_DB,
        hertzforge_srs::service::config::DEFAULT_ABORT_DB,
    );
    let mut per_band = None;
    if let Some(e) = c.opt_obj("envelope")? {
        alarm = db_pair(&e, "alarm_db")?.unwrap_or(alarm);
        abort = db_pair(&e, "abort_db")?.unwrap_or(abort);
        if let Some(list) = e.array("per_band")? {
            let mut out = Vec::with_capacity(list.len());
            for (i, b) in list.iter().enumerate() {
                let o = Obj::of(b, &format!("{}[{i}]", e.at("per_band")))?;
                out.push(WireEnvelopeBand {
                    alarm_db: db_pair(&o, "alarm_db")?.ok_or_else(|| {
                        bad(&o.at("alarm_db"), "a band's envelope gives its alarm_db")
                    })?,
                    abort_db: db_pair(&o, "abort_db")?.ok_or_else(|| {
                        bad(&o.at("abort_db"), "a band's envelope gives its abort_db")
                    })?,
                });
            }
            per_band = Some(out);
        }
    }

    let sch = c.obj("schedule")?;
    let mode = sch.choice(
        "mode",
        &[
            ("auto", ShockRunMode::Auto),
            ("manual", ShockRunMode::Manual),
        ],
        None,
    )?;
    let levels_v = sch.array("levels_db")?.ok_or_else(|| {
        bad(
            "config.schedule.levels_db",
            "the schedule has no \"levels_db\" list",
        )
    })?;
    let mut levels_db = Vec::with_capacity(levels_v.len());
    for (i, l) in levels_v.iter().enumerate() {
        levels_db.push(l.as_f64().filter(|x| x.is_finite()).ok_or_else(|| {
            bad(
                &format!("config.schedule.levels_db[{i}]"),
                "a level is a number of dB",
            )
        })?);
    }
    let pulses_v = sch.array("pulses")?.ok_or_else(|| {
        bad(
            "config.schedule.pulses",
            "the schedule has no \"pulses\" list: one count for every level, or one per level",
        )
    })?;
    let mut pulses = Vec::with_capacity(pulses_v.len());
    for (i, p) in pulses_v.iter().enumerate() {
        pulses.push(
            p.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| {
                    bad(
                        &format!("config.schedule.pulses[{i}]"),
                        "a pulse count is a whole number from 0",
                    )
                })?,
        );
    }
    let start_db = sch.f64_or("start_db", levels_db.first().copied().unwrap_or(0.0))?;
    let advance = sch.choice(
        "advance",
        &[("count", Advance::Count), ("on_pass", Advance::OnPass)],
        Some(Advance::Count),
    )?;
    let max_pulses_per_level = sch
        .opt_count("max_pulses_per_level")?
        .map_or(DEFAULT_MAX_PULSES_PER_LEVEL, |n| {
            u32::try_from(n).unwrap_or(u32::MAX)
        });

    let rate = RateModel::conservative_default();
    let (mut cooling_s, mut settling_s, mut compute_s) = (
        rate.cooling_floor_s,
        rate.settling_floor_s,
        rate.compute_floor_s,
    );
    if let Some(s) = c.opt_obj("spacing")? {
        cooling_s = s.f64_or("cooling_s", cooling_s)?;
        settling_s = s.f64_or("settling_s", settling_s)?;
        compute_s = s.f64_or("compute_s", compute_s)?;
    }

    let limits = c.obj("limits")?;
    let drive_vpk_ceiling = limits.f64("drive_vpk_ceiling")?;
    let drive_vpk_abort = limits.f64("drive_vpk_abort")?;
    let base = SrsControl::new(drive_vpk_ceiling, drive_vpk_abort);
    let sysid_d = SysIdSpec::default();

    let mut excitation = sysid_d.excitation;
    let mut sysid_level_db = sysid_d.level_db;
    let mut sysid_max_vpk = base.sysid_max_vpk;
    let mut sysid_nfft = sysid_d.nfft as u64;
    let mut h_floor_frac = sysid_d.h_floor_frac;
    let mut min_mean_coherence = base.min_mean_coherence;
    let mut review = true;
    if let Some(i) = c.opt_obj("identification")? {
        excitation = i.choice(
            "excitation",
            &[
                ("shaped", SysIdExcitation::Shaped),
                ("standard_pulse", SysIdExcitation::StandardPulse),
            ],
            Some(excitation),
        )?;
        sysid_level_db = i.f64_or("level_db", sysid_level_db)?;
        sysid_max_vpk = i.opt_f64("max_vpk")?.or(sysid_max_vpk);
        sysid_nfft = i.opt_count("nfft")?.unwrap_or(sysid_nfft);
        h_floor_frac = i.f64_or("h_floor_frac", h_floor_frac)?;
        min_mean_coherence = i.f64_or("min_mean_coherence", min_mean_coherence)?;
        review = i.opt_bool("review")?.unwrap_or(review);
    }

    let mut law = LawSetup {
        id: hertzforge_shock_laws::DEFAULT_SRS_LAW_ID.to_owned(),
        params: Map::new(),
    };
    let mut law_budget = base.guards.budget_fraction;
    let mut law_misses = u64::from(base.guards.max_misses);
    let mut max_pulses = base.guards.max_pulses;
    let mut max_gain_rise_db = base.max_gain_rise_db;
    if let Some(o) = c.opt_obj("control")? {
        if let Some(l) = o.opt_obj("law")? {
            let id = l
                .opt_str("id")?
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad(&l.at("id"), "a control law is named by its id"))?
                .to_owned();
            let params = match l.m.get("params") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(p)) => p.clone(),
                Some(_) => {
                    return Err(bad(
                        &l.at("params"),
                        "\"params\" is an object of parameter values by key",
                    ))
                }
            };
            law = LawSetup { id, params };
        }
        law_budget = o.f64_or("law_budget", law_budget)?;
        law_misses = o.opt_count("law_misses")?.unwrap_or(law_misses);
        max_pulses = o.opt_count("max_pulses")?.unwrap_or(max_pulses);
        max_gain_rise_db = o.f64_or("max_gain_rise_db", max_gain_rise_db)?;
    }

    let peak_accel_g = limits.opt_f64("peak_accel_g")?;
    let abort_factor = limits.f64_or(
        "abort_factor",
        hertzforge_srs::service::config::DEFAULT_DRIVE_ABORT_FACTOR,
    )?;
    let kinematic = match limits.opt_obj("kinematic")? {
        None => (None, None, None),
        Some(k) => (
            k.opt_f64("accel_g")?,
            k.opt_f64("velocity_ips")?,
            k.opt_f64("disp_pp_in")?,
        ),
    };
    let mut monitors = Vec::new();
    if let Some(ms) = limits.array("monitors")? {
        for (i, m) in ms.iter().enumerate() {
            let o = Obj::of(m, &format!("config.limits.monitors[{i}]"))?;
            let channel = o
                .opt_count("channel")?
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| bad(&o.at("channel"), "a monitor names a channel index"))?;
            let unit = o
                .opt_str("unit")?
                .ok_or_else(|| bad(&o.at("unit"), "a monitor names its channel's unit"))?
                .to_owned();
            monitors.push(WireMonitor {
                channel,
                unit,
                peak_ceiling: o.f64("peak_ceiling")?,
                kind: o.choice(
                    "kind",
                    &[
                        ("predictive", DriveMonitorKind::Predictive),
                        ("between_pulse", DriveMonitorKind::BetweenPulse),
                    ],
                    None,
                )?,
            });
        }
    }

    let mut headroom_db = 0.0;
    let mut limit_channels = Vec::new();
    if let Some(lc) = c.opt_obj("limit_channels")? {
        headroom_db = lc.f64_or("headroom_db", 0.0)?;
        if let Some(chs) = lc.array("channels")? {
            for (i, ch) in chs.iter().enumerate() {
                let o = Obj::of(ch, &format!("config.limit_channels.channels[{i}]"))?;
                let channel = o
                    .opt_count("channel")?
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| {
                        bad(&o.at("channel"), "a limit channel names a channel index")
                    })?;
                let b = o.obj("bound")?;
                let bound = match b.opt_str("kind")? {
                    Some("flat") => WireBound::Flat {
                        accel_g: b.f64("accel_g")?,
                    },
                    Some("per_band") => {
                        let a = b.array("bounds_g")?.ok_or_else(|| {
                            bad(
                                &b.at("bounds_g"),
                                "a per-band bound lists one bound per band",
                            )
                        })?;
                        let mut out = Vec::with_capacity(a.len());
                        for (k, v) in a.iter().enumerate() {
                            out.push(match v {
                                Value::Null => None,
                                v => {
                                    Some(v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| {
                                        bad(
                                            &format!("{}[{k}]", b.at("bounds_g")),
                                            "a bound is a number of g, or null for none",
                                        )
                                    })?)
                                }
                            });
                        }
                        WireBound::PerBand { bounds_g: out }
                    }
                    _ => {
                        return Err(bad(
                            &b.at("kind"),
                            format!("\"{}\" is \"flat\" or \"per_band\"", b.at("kind")),
                        ))
                    }
                };
                let frf = match o.opt_obj("frf")? {
                    None => None,
                    Some(f) => Some((
                        numbers(&f, "freqs_hz")?.ok_or_else(|| {
                            bad(&f.at("freqs_hz"), "a supplied gain lists its frequencies")
                        })?,
                        numbers(&f, "mag")?.ok_or_else(|| {
                            bad(&f.at("mag"), "a supplied gain lists its magnitudes")
                        })?,
                    )),
                };
                limit_channels.push(WireLimitChannel {
                    channel,
                    bound,
                    frf,
                });
            }
        }
    }

    let feasibility = match c.opt_obj("feasibility")? {
        None => None,
        Some(o) => {
            let shaker: ShakerRecord = serde_json::from_value(o.require("shaker")?.clone())
                .map_err(|e| {
                    MethodError::named(
                        ErrorCode::BadRequest,
                        "shaker_record_invalid",
                        format!(
                            "The feasibility shaker record does not parse ({e}), so the pulse \
                             cannot be checked against it."
                        ),
                    )
                })?;
            let mm = o.obj("moving_mass_lb")?;
            let mass = |k: &str| -> Result<f64, MethodError> {
                let x = mm.f64(k)?;
                if x < 0.0 {
                    return Err(bad(&mm.at(k), "a moving mass is 0 lb or more"));
                }
                Ok(x)
            };
            let tol = FeasibilityTolerances::default();
            Some(WireFeasibility {
                shaker,
                moving_mass: MovingMass {
                    armature_lb: mass("armature")?,
                    slip_table_lb: mass("slip_table")?,
                    drive_adapter_lb: mass("drive_adapter")?,
                    fixturing_lb: mass("fixturing")?,
                    test_article_lb: mass("test_article")?,
                },
                residual_velocity_ips: o
                    .f64_or("residual_velocity_ips", tol.residual_velocity_ips)?,
                residual_disp_in: o.f64_or("residual_disp_in", tol.residual_disp_in)?,
            })
        }
    };

    // Every run is recorded in its run folder (`record`); the module's own
    // capture is gone, and a configuration that still asks for it is told so
    // rather than left to believe it is capturing.
    if !matches!(c.m.get("capture"), None | Some(Value::Null)) {
        return Err(MethodError::bad_params(
            "config.capture",
            "An SRS shock run is always recorded in its run folder; \"capture\" is no longer \
             taken. Use \"record\" to keep or leave out the streams.",
        ));
    }
    let words =
        |k: &str| -> Result<String, MethodError> { Ok(c.opt_str(k)?.unwrap_or("").to_owned()) };
    let (operator, article, notes) = (words("operator")?, words("article")?, words("notes")?);
    let specification = match c.m.get("specification") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let o = Obj::of(v, "config.specification")?;
            Some((
                o.opt_str("document")?.unwrap_or("").to_owned(),
                o.opt_str("clause")?.unwrap_or("").to_owned(),
            ))
        }
    };
    let record_streams = match c.m.get("record") {
        None | Some(Value::Null) => true,
        Some(v) => Obj::of(v, "config.record")?
            .opt_bool("streams")?
            .unwrap_or(true),
    };
    let metadata = match c.array("metadata")? {
        None => MetaField::defaults(),
        Some(list) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, m) in list.iter().enumerate() {
                let o = Obj::of(m, &format!("config.metadata[{i}]"))?;
                let text = |k: &str| -> Result<String, MethodError> {
                    o.opt_str(k)?
                        .map(str::to_owned)
                        .ok_or_else(|| bad(&o.at(k), format!("\"{}\" is text", o.at(k))))
                };
                out.push(MetaField::new(text("key")?, text("label")?, text("value")?));
            }
            out
        }
    };

    let clip32 = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    Ok(WireSrs {
        title,
        synthesis,
        envelope_alarm_db: alarm,
        envelope_abort_db: abort,
        envelope_per_band: per_band,
        ring_out_s,
        pvss,
        mode,
        levels_db,
        pulses,
        start_db,
        advance,
        max_pulses_per_level,
        cooling_s,
        settling_s,
        compute_s,
        excitation,
        sysid_level_db,
        sysid_max_vpk,
        sysid_nfft,
        h_floor_frac,
        min_mean_coherence,
        review,
        law,
        law_budget,
        law_misses: clip32(law_misses),
        max_pulses,
        max_gain_rise_db,
        drive_vpk_ceiling,
        drive_vpk_abort,
        peak_accel_g,
        abort_factor,
        kinematic,
        monitors,
        headroom_db,
        limit_channels,
        feasibility,
        metadata,
        operator,
        article,
        notes,
        specification,
        record_streams,
        raw: config.clone(),
    })
}

/// Parse `srs.preview`'s params: the target, the bands, the synthesis and the rate.
pub fn parse_preview(params: &Map<String, Value>) -> Result<WireSynthesis, MethodError> {
    let target = parse_target(
        params
            .get("target")
            .ok_or_else(|| bad("target", "A preview needs a target."))?,
        "target",
    )?;
    let bands = parse_bands(
        params
            .get("bands")
            .ok_or_else(|| bad("bands", "A preview needs its band grid."))?,
        "bands",
    )?;
    let fs_hz = crate::protocol::named_f64(params, "fs_hz")?.unwrap_or(DEFAULT_FS_HZ);
    parse_synthesis(target, bands, params.get("synthesis"), "synthesis", fs_hz)
}

// ---------------------------------------------------------------------------------
// Checking.
// ---------------------------------------------------------------------------------

fn out_of_range(r: &mut Report, field: &str, value: f64, ok: bool, text: &str) {
    if !ok {
        r.errors.push(Item::field(
            "out_of_range",
            field,
            format!("{} is {}; {text}", srs_field_words(field), num(value)),
        ));
    }
}

/// A configuration field in words, for a message a person reads.
pub fn srs_field_words(field: &str) -> String {
    let last = field.rsplit('.').next().unwrap_or(field);
    let last = last.split('[').next().unwrap_or(last);
    let words = match (field.split('.').next().unwrap_or(""), last) {
        ("target", "accel_g") => "The target",
        ("target", "points") => "A breakpoint",
        ("bands", "f_lo_hz") => "The band grid's low edge",
        ("bands", "f_hi_hz") => "The band grid's high edge",
        ("bands", "bands_per_octave") => "The bands per octave",
        ("bands", "q") => "The quality factor",
        ("synthesis", "max_iters") => "The synthesis passes",
        ("synthesis", "tol_db") => "The synthesis tolerance",
        ("synthesis", "max_gain_db_per_iter") => "The synthesis step",
        ("envelope", "alarm_db") => "The alarm band",
        ("envelope", "abort_db") => "The abort band",
        ("acquisition", "ring_out_s") => "The ring-out",
        ("identification", "level_db") => "The identification level",
        ("identification", "max_vpk") => "The identification peak",
        ("identification", "nfft") => "The identification block size",
        ("identification", "h_floor_frac") => "The gain floor",
        ("control", "max_gain_rise_db") => "The cap on a gain's rise",
        ("limits", "abort_factor") => "The abort factor",
        ("limits", "accel_g") => "The acceleration ceiling",
        ("limits", "velocity_ips") => "The velocity ceiling",
        ("limits", "disp_pp_in") => "The displacement ceiling",
        ("limit_channels", "headroom_db") => "The limit headroom",
        ("limit_channels", "accel_g") => "A limit channel's bound",
        ("limit_channels", "bounds_g") => "A limit channel's bound",
        _ => return field_words(field),
    };
    words.to_owned()
}

impl WireSrs {
    /// The least time between pulse starts: the largest of the floors.
    pub fn spacing_s(&self) -> f64 {
        self.rate_model().effective_min_spacing(DrivePath::HostDac)
    }

    pub fn rate_model(&self) -> RateModel {
        RateModel {
            cooling_floor_s: self.cooling_s,
            settling_floor_s: self.settling_s,
            compute_floor_s: self.compute_s,
            ..RateModel::conservative_default()
        }
    }

    pub fn fs_hz(&self) -> f64 {
        self.synthesis.fs_hz
    }

    /// The controller's schedule.
    pub fn schedule(&self) -> ShockLevelSchedule {
        ShockLevelSchedule {
            mode: self.mode,
            start_db: self.start_db,
            table_db: self.levels_db.clone(),
            pulses_per_level: self.pulses.clone(),
            advance: match self.advance {
                Advance::Count => LadderAdvance::Count,
                Advance::OnPass => LadderAdvance::OnPass,
            },
            max_pulses_per_level: self.max_pulses_per_level,
        }
    }

    pub fn pulses_for(&self, i: usize) -> u32 {
        self.schedule().pulses_for(i)
    }

    /// The identification burst's cap in force, volts.
    pub fn identification_vpk_cap(&self) -> f64 {
        self.control().identification_vpk_cap()
    }

    /// The closed-loop settings.
    pub fn control(&self) -> SrsControl {
        let mut c = SrsControl::new(self.drive_vpk_ceiling, self.drive_vpk_abort);
        c.sysid_max_vpk = self.sysid_max_vpk;
        c.min_mean_coherence = self.min_mean_coherence;
        c.rate_model = self.rate_model();
        c.guards.budget_fraction = self.law_budget;
        c.guards.max_misses = self.law_misses;
        c.guards.max_pulses = self.max_pulses;
        c.max_gain_rise_db = self.max_gain_rise_db;
        c
    }

    /// A row's name as the plant's taps carry it: its description, or
    /// `ch<index>` without one; `None` when the table has no such row.
    pub fn channel_name(table: &ChannelTable, index: u32) -> Option<String> {
        table
            .channels
            .iter()
            .find(|c| c.index == index)
            .map(|c| channel_display_name(c.index, &c.description))
    }

    /// The limit set on the band grid of `bands` points, named from `table`.
    pub fn limit_set(&self, table: &ChannelTable) -> Option<LimitSet> {
        if self.limit_channels.is_empty() {
            return None;
        }
        Some(LimitSet {
            headroom_db: self.headroom_db,
            channels: self
                .limit_channels
                .iter()
                .map(|l| LimitChannelSpec {
                    name: Self::channel_name(table, l.channel)
                        .unwrap_or_else(|| format!("ch{}", l.channel)),
                    limit: match &l.bound {
                        WireBound::Flat { accel_g } => LimitTarget::Flat { accel_g: *accel_g },
                        WireBound::PerBand { bounds_g } => LimitTarget::PerBand {
                            bounds_g: bounds_g
                                .iter()
                                .map(|b| b.unwrap_or(f64::INFINITY))
                                .collect(),
                        },
                    },
                    frf: l.frf.as_ref().map(|(f, m)| FrfSpec {
                        freqs_hz: f.clone(),
                        mag: m.clone(),
                    }),
                })
                .collect(),
        })
    }

    /// The envelope on a grid of `n` bands, as a screen draws it.
    pub fn envelope_bands(&self, n: usize) -> Vec<preview::EnvelopeBand> {
        match &self.envelope_per_band {
            Some(per) => per
                .iter()
                .map(|b| (b.alarm_db.0, b.alarm_db.1, b.abort_db.0, b.abort_db.1))
                .collect(),
            None => preview::uniform_envelope(n, self.envelope_alarm_db, self.envelope_abort_db),
        }
    }

    /// The envelope as the controller takes it.
    pub fn envelope(&self) -> EnvelopeSpec {
        EnvelopeSpec {
            alarm_db: self.envelope_alarm_db,
            abort_db: self.envelope_abort_db,
            per_band: self.envelope_per_band.as_ref().map(|bands| {
                bands
                    .iter()
                    .map(|b| SrsToleranceBand {
                        alarm_lo_db: b.alarm_db.0,
                        alarm_hi_db: b.alarm_db.1,
                        abort_lo_db: b.abort_db.0,
                        abort_hi_db: b.abort_db.1,
                    })
                    .collect()
            }),
        }
    }

    /// The controller's whole test configuration, `control` set, its channels
    /// named from `table`.
    pub fn build(&self, table: &ChannelTable) -> SrsTestConfig {
        let s = &self.synthesis;
        let mut cfg = SrsTestConfig::flat(s.bands.f_lo_hz, s.bands.f_hi_hz, 1.0);
        cfg.fraction = s.bands.fraction();
        cfg.target = s.target.crate_target();
        cfg.q = s.bands.q;
        cfg.engine = s.engine;
        cfg.components = s.components.as_ref().map(WireComponents::engine_table);
        cfg.synthesis = s.opts();
        cfg.schedule = self.schedule();
        cfg.limits = self.limit_set(table);
        if let Some(f) = &self.feasibility {
            cfg.shaker = Some(f.shaker.clone());
            cfg.moving_mass = f.moving_mass;
            cfg.feasibility_tol = FeasibilityTol {
                residual_velocity_ips: f.residual_velocity_ips,
                residual_disp_in: f.residual_disp_in,
            };
        }
        cfg.drive_limit = DriveLimit {
            peak_accel_g: self.peak_accel_g,
            monitors: self
                .monitors
                .iter()
                .map(|m| DriveMonitor {
                    channel: Self::channel_name(table, m.channel)
                        .unwrap_or_else(|| format!("ch{}", m.channel)),
                    eu: m.unit.clone(),
                    peak_ceiling: m.peak_ceiling,
                    kind: m.kind,
                })
                .collect(),
        };
        cfg.drive_abort_factor = self.abort_factor;
        cfg.kinematic_abort = KinematicAbort {
            accel_g: self.kinematic.0,
            velocity_ips: self.kinematic.1,
            disp_pp_in: self.kinematic.2,
        };
        cfg.envelope = self.envelope();
        cfg.sysid = Some(SysIdSpec {
            level_db: self.sysid_level_db,
            excitation: self.excitation,
            nfft: usize::try_from(self.sysid_nfft).unwrap_or(usize::MAX),
            h_floor_frac: self.h_floor_frac,
        });
        // The run's record is the one way an SRS shock run records: the
        // controller's own capture stays off.
        cfg.capture = ShockCaptureRules {
            trigger: ShockCaptureTrigger::Off,
            snapshot_on_stop: false,
        };
        cfg.compute_pvss = self.pvss;
        cfg.ring_out_s = self.ring_out_s;
        cfg.review_after_sysid = self.review;
        cfg.metadata = self.metadata.clone();
        // The operator's words travel with the controller's configuration too, so
        // its own report carries them.
        for field in cfg.metadata.iter_mut() {
            match field.key.as_str() {
                "operator" if !self.operator.is_empty() => field.value = self.operator.clone(),
                "test_article" if !self.article.is_empty() => field.value = self.article.clone(),
                _ => {}
            }
        }
        if !self.notes.is_empty() {
            cfg.metadata
                .push(MetaField::new("notes", "Notes", self.notes.clone()));
        }
        if let Some((document, clause)) = &self.specification {
            cfg.metadata.push(MetaField::new(
                "specification",
                "Specification",
                document.clone(),
            ));
            cfg.metadata
                .push(MetaField::new("clause", "Clause", clause.clone()));
        }
        cfg.control = Some(self.control());
        cfg
    }

    /// The law as a catalog choice: its id and the values its manifest declares.
    pub fn law_choice(&self, facts: &LawFacts) -> hertzforge_random_laws::LawChoice {
        hertzforge_random_laws::LawChoice::new(
            self.law.id.clone(),
            crate::mimo::adapter::params_of(&facts.params, &self.law.params),
        )
    }

    /// Check every field: every problem a named item, and the derived values when
    /// there are no errors.
    pub fn check(&self, ctx: &CheckContext<'_>) -> (Report, Option<Derived>) {
        let mut r = Report::default();
        let synthesis_sound = check_synthesis(&mut r, &self.synthesis, "");
        let pos = |x: f64| x.is_finite() && x > 0.0;
        let bands = self.synthesis.bands.centres();
        let nbands = bands.len();

        // Acquisition.
        out_of_range(
            &mut r,
            "acquisition.ring_out_s",
            self.ring_out_s,
            self.ring_out_s.is_finite() && (0.0..=MAX_RING_OUT_S).contains(&self.ring_out_s),
            &format!("it is from 0 to {} s", num(MAX_RING_OUT_S)),
        );

        // The envelope.
        let pair_ok = |p: (f64, f64)| pos(p.0) && pos(p.1) && p.0 <= 60.0 && p.1 <= 60.0;
        let mut envelope_ok = true;
        for (field, p) in [
            ("envelope.alarm_db", self.envelope_alarm_db),
            ("envelope.abort_db", self.envelope_abort_db),
        ] {
            if !pair_ok(p) {
                envelope_ok = false;
                r.errors.push(Item::field(
                    "out_of_range",
                    field,
                    format!(
                        "{} is [{}, {}] dB; each side is above 0 and at most 60 dB.",
                        srs_field_words(field),
                        num(p.0),
                        num(p.1)
                    ),
                ));
            }
        }
        let wider = |alarm: (f64, f64), abort: (f64, f64)| abort.0 < alarm.0 || abort.1 < alarm.1;
        if envelope_ok
            && self.envelope_per_band.is_none()
            && wider(self.envelope_alarm_db, self.envelope_abort_db)
        {
            r.errors.push(alarm_wider("envelope.abort_db"));
        }
        if let Some(per) = &self.envelope_per_band {
            if synthesis_sound && per.len() != nbands {
                r.errors.push(Item::field(
                    "per_band_length_mismatch",
                    "envelope.per_band",
                    format!(
                        "The envelope has {} bands for a grid of {nbands}; give one per band.",
                        per.len()
                    ),
                ));
            }
            for (i, b) in per.iter().enumerate() {
                let at = format!("envelope.per_band[{i}]");
                if !(pair_ok(b.alarm_db) && pair_ok(b.abort_db)) {
                    r.errors.push(Item::field(
                        "out_of_range",
                        &at,
                        "A band's alarm and abort are [under, over] dB, each side above 0 and at \
                         most 60 dB.",
                    ));
                } else if wider(b.alarm_db, b.abort_db) {
                    r.errors.push(alarm_wider(&format!("{at}.abort_db")));
                }
            }
        }

        // The schedule and the spacing, as classical shock's.
        check_schedule(&mut r, self);
        let floor_ok = |x: f64| x.is_finite() && (0.0..=MAX_SPACING_S).contains(&x);
        for (field, v) in [
            ("spacing.cooling_s", self.cooling_s),
            ("spacing.settling_s", self.settling_s),
            ("spacing.compute_s", self.compute_s),
        ] {
            out_of_range(
                &mut r,
                field,
                v,
                floor_ok(v),
                &format!("it is from 0 to {} s", num(MAX_SPACING_S)),
            );
        }
        let spacing_s = self.spacing_s();
        if floor_ok(self.cooling_s)
            && floor_ok(self.settling_s)
            && floor_ok(self.compute_s)
            && spacing_s <= 0.0
        {
            r.errors.push(Item::field(
                "out_of_range",
                "spacing.cooling_s",
                "The time between pulses is 0 s; at least one of the floors is above 0.",
            ));
        }

        // Identification.
        out_of_range(
            &mut r,
            "identification.level_db",
            self.sysid_level_db,
            self.sysid_level_db.is_finite()
                && self.sysid_level_db >= SYSID_LEVEL_DB_RANGE.0
                && self.sysid_level_db < SYSID_LEVEL_DB_RANGE.1,
            &format!(
                "it is under the full-level synthesis: below {} dB and at least {} dB",
                num(SYSID_LEVEL_DB_RANGE.1),
                num(SYSID_LEVEL_DB_RANGE.0)
            ),
        );
        if let Some(v) = self.sysid_max_vpk {
            out_of_range(
                &mut r,
                "identification.max_vpk",
                v,
                pos(v) && v <= self.drive_vpk_ceiling,
                &format!(
                    "it is above 0 and at most the drive ceiling of {} V",
                    num(self.drive_vpk_ceiling)
                ),
            );
        }
        out_of_range(
            &mut r,
            "identification.nfft",
            self.sysid_nfft as f64,
            self.sysid_nfft.is_power_of_two()
                && (SRS_NFFT_RANGE.0..=SRS_NFFT_RANGE.1).contains(&self.sysid_nfft),
            &format!(
                "it is a power of two from {} to {}",
                SRS_NFFT_RANGE.0, SRS_NFFT_RANGE.1
            ),
        );
        out_of_range(
            &mut r,
            "identification.h_floor_frac",
            self.h_floor_frac,
            pos(self.h_floor_frac) && self.h_floor_frac <= 1.0,
            "it is a fraction of the in-band peak gain, above 0 and at most 1",
        );
        out_of_range(
            &mut r,
            "identification.min_mean_coherence",
            self.min_mean_coherence,
            self.min_mean_coherence.is_finite() && (0.0..1.0).contains(&self.min_mean_coherence),
            "it is at least 0 and below 1",
        );

        // The control law and the core's settings.
        self.check_law(&mut r, ctx);
        out_of_range(
            &mut r,
            "control.law_budget",
            self.law_budget,
            pos(self.law_budget) && self.law_budget <= 1.0,
            "it is a fraction of the time between pulses, above 0 and at most 1",
        );
        out_of_range(
            &mut r,
            "control.law_misses",
            f64::from(self.law_misses),
            (1..=100).contains(&self.law_misses),
            "it is from 1 to 100",
        );
        out_of_range(
            &mut r,
            "control.max_pulses",
            self.max_pulses as f64,
            (1..=100_000).contains(&self.max_pulses),
            "it is from 1 to 100000",
        );
        out_of_range(
            &mut r,
            "control.max_gain_rise_db",
            self.max_gain_rise_db,
            pos(self.max_gain_rise_db) && self.max_gain_rise_db <= MAX_GAIN_STEP_DB,
            &format!("it is above 0 and at most {} dB", num(MAX_GAIN_STEP_DB)),
        );

        // Limits.
        out_of_range(
            &mut r,
            "limits.drive_vpk_ceiling",
            self.drive_vpk_ceiling,
            pos(self.drive_vpk_ceiling) && self.drive_vpk_ceiling <= MAX_DRIVE_V,
            &format!("it is above 0 and at most {} V", num(MAX_DRIVE_V)),
        );
        out_of_range(
            &mut r,
            "limits.drive_vpk_abort",
            self.drive_vpk_abort,
            self.drive_vpk_abort.is_finite()
                && self.drive_vpk_abort > self.drive_vpk_ceiling
                && self.drive_vpk_abort <= MAX_DRIVE_V,
            &format!(
                "it is above the drive ceiling of {} V and at most {} V",
                num(self.drive_vpk_ceiling),
                num(MAX_DRIVE_V)
            ),
        );
        if let Some(a) = self.peak_accel_g {
            out_of_range(&mut r, "limits.peak_accel_g", a, pos(a), "it is above 0 g");
        }
        out_of_range(
            &mut r,
            "limits.abort_factor",
            self.abort_factor,
            self.abort_factor.is_finite() && (1.0..=100.0).contains(&self.abort_factor),
            "it is at least 1 and at most 100 times the ceiling",
        );
        for (field, v, unit) in [
            ("limits.kinematic.accel_g", self.kinematic.0, "g"),
            ("limits.kinematic.velocity_ips", self.kinematic.1, "in/s"),
            ("limits.kinematic.disp_pp_in", self.kinematic.2, "in"),
        ] {
            if let Some(x) = v {
                out_of_range(&mut r, field, x, pos(x), &format!("it is above 0 {unit}"));
            }
        }
        let row = |i: u32| ctx.table.channels.iter().find(|c| c.index == i);
        let is_input = |i: u32| row(i).is_some_and(|c| c.enabled && c.role != ChannelRole::Drive);
        for (i, m) in self.monitors.iter().enumerate() {
            if !is_input(m.channel) {
                r.errors.push(Item::field(
                    "limit_unknown_channel",
                    format!("limits.monitors[{i}].channel"),
                    format!(
                        "Channel {} is not an enabled input of the channel table; monitor an \
                         enabled input, or remove the monitor.",
                        m.channel
                    ),
                ));
            }
            out_of_range(
                &mut r,
                &format!("limits.monitors[{i}].peak_ceiling"),
                m.peak_ceiling,
                pos(m.peak_ceiling),
                &format!("it is above 0 {}", m.unit),
            );
        }

        // Limit channels.
        out_of_range(
            &mut r,
            "limit_channels.headroom_db",
            self.headroom_db,
            self.headroom_db.is_finite() && (0.0..=40.0).contains(&self.headroom_db),
            "it is from 0 to 40 dB",
        );
        let control_index = ctx
            .table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Control)
            .map(|c| c.index);
        for (i, l) in self.limit_channels.iter().enumerate() {
            let at = format!("limit_channels.channels[{i}]");
            if row(l.channel).is_none() || Some(l.channel) == control_index {
                r.errors.push(Item::field(
                    "limit_unknown_channel",
                    format!("{at}.channel"),
                    format!(
                        "Channel {} is not a row of the channel table other than the control \
                         channel; name the row the bound is for.",
                        l.channel
                    ),
                ));
            } else if l.frf.is_none() && !is_input(l.channel) {
                r.errors.push(Item::field(
                    "limit_no_frf",
                    format!("{at}.frf"),
                    format!(
                        "Channel {} has no supplied gain and is not an enabled input to \
                         measure one from; supply its gain, or make it an enabled input.",
                        l.channel
                    ),
                ));
            }
            match &l.bound {
                WireBound::Flat { accel_g } => out_of_range(
                    &mut r,
                    &format!("{at}.bound.accel_g"),
                    *accel_g,
                    pos(*accel_g),
                    "it is above 0 g",
                ),
                WireBound::PerBand { bounds_g } => {
                    if synthesis_sound && bounds_g.len() != nbands {
                        r.errors.push(Item::field(
                            "per_band_length_mismatch",
                            format!("{at}.bound.bounds_g"),
                            format!(
                                "The bound has {} values for a grid of {nbands} bands; give one \
                                 per band (null for no bound at a band).",
                                bounds_g.len()
                            ),
                        ));
                    }
                    for (k, b) in bounds_g.iter().enumerate() {
                        if let Some(b) = b {
                            out_of_range(
                                &mut r,
                                &format!("{at}.bound.bounds_g[{k}]"),
                                *b,
                                pos(*b),
                                "it is above 0 g, or null for no bound",
                            );
                        }
                    }
                }
            }
            if let Some((f, m)) = &l.frf {
                let ascending = f.windows(2).all(|w| w[1] > w[0]);
                let ok = !f.is_empty()
                    && f.len() == m.len()
                    && ascending
                    && f.iter().all(|x| *x >= 0.0)
                    && m.iter().all(|x| *x >= 0.0);
                if !ok {
                    r.errors.push(Item::field(
                        "out_of_range",
                        format!("{at}.frf"),
                        "A supplied gain lists ascending frequencies from 0 Hz and as many \
                         magnitudes, each 0 or more.",
                    ));
                }
            }
        }

        // The table.
        let enabled = |role: ChannelRole| {
            ctx.table
                .channels
                .iter()
                .any(|c| c.enabled && c.role == role)
        };
        if !enabled(ChannelRole::Control) {
            r.errors.push(Item::channel(
                "no_control_channel",
                None,
                "The channel table has no enabled control row; the pulse is controlled on the \
                 first one.",
            ));
        }
        if !enabled(ChannelRole::Drive) {
            r.errors.push(Item::channel(
                "no_drive_channel",
                None,
                "The channel table has no enabled drive row; the pulse is played on the first \
                 one.",
            ));
        }
        let drives = ctx
            .table
            .channels
            .iter()
            .filter(|c| c.enabled && c.role == ChannelRole::Drive)
            .count();
        if drives > 1 {
            r.errors.push(Item::channel(
                "extra_drive_channels",
                None,
                format!(
                    "The channel table has {drives} enabled drive rows; SRS Shock plays one pulse \
                     on one output, and a station opens every drive row as an output. Disable \
                     all but one, or run MIMO Shock."
                ),
            ));
        }

        // Feasibility.
        if let Some(f) = &self.feasibility {
            out_of_range(
                &mut r,
                "feasibility.residual_velocity_ips",
                f.residual_velocity_ips,
                pos(f.residual_velocity_ips),
                "it is above 0 in/s",
            );
            out_of_range(
                &mut r,
                "feasibility.residual_disp_in",
                f.residual_disp_in,
                pos(f.residual_disp_in),
                "it is above 0 in",
            );
        }

        // The synthesis itself: its record and its fit. Only once the fields it is
        // built from are sound.
        if !synthesis_sound {
            return (r, None);
        }
        let syn = preview::synthesize(&self.synthesis);
        let ring = ring_out_samples(self.ring_out_s, self.fs_hz());
        let record_len = syn.waveform_g.len() + ring;
        let record_s = record_len as f64 / self.fs_hz();
        if record_s >= ctx.timeout_s {
            r.errors.push(Item::field(
                "record_too_long",
                "bands.f_lo_hz",
                format!(
                    "One pulse is {} s of record, and the controller timeout is {} s: a safe \
                     stop could not wait for the pulse in flight. Raise the grid's low edge or \
                     shorten the ring-out.",
                    num(round(record_s, 3)),
                    num(round(ctx.timeout_s, 1))
                ),
            ));
        }
        push_not_converged(&mut r, &syn, self.synthesis.tol_db, "synthesis.tol_db");
        if !r.errors.is_empty() {
            return (r, None);
        }
        let cfg = self.build(ctx.table);
        if let Err(e) = cfg.validate() {
            r.errors
                .push(Item::field("out_of_range", "config", e.to_string()));
            return (r, None);
        }
        let feasibility = feasibility_verdict(self, &syn);
        let derived = Derived {
            grid: Grid {
                fs_hz: self.fs_hz(),
                record_len,
                actual: false,
            },
            bands_hz: syn.bands_hz.clone(),
            target_g: syn.target_g.clone(),
            synthesis: syn.facts(),
            reference: syn.reference_facts(),
            spacing_s,
            call_budget_s: self.law_budget * spacing_s,
            feasibility,
        };
        (r, Some(derived))
    }

    fn check_law(&self, r: &mut Report, ctx: &CheckContext<'_>) {
        let Some(facts) = (ctx.law)(&self.law.id) else {
            r.errors.push(Item::field(
                "law_unknown",
                "control.law.id",
                format!(
                    "No control law \"{}\" is in the catalog; choose one laws.list names for \
                     SRS shock.",
                    self.law.id
                ),
            ));
            return;
        };
        if let Err(is_for) = facts.world {
            r.errors.push(Item::field(
                "law_wrong_world",
                "control.law.id",
                format!(
                    "\"{}\" is a control law for {is_for}, not for SRS shock.",
                    facts.name
                ),
            ));
            return;
        }
        for (key, value) in &self.law.params {
            let Some(spec) = facts.params.iter().find(|p| &p.key == key) else {
                r.errors.push(Item::field(
                    "law_param_unknown",
                    format!("control.law.params.{key}"),
                    format!("\"{}\" has no parameter \"{key}\".", facts.name),
                ));
                continue;
            };
            if let Some(why) = param_problem(spec, value) {
                r.errors.push(Item::field(
                    "law_param_invalid",
                    format!("control.law.params.{key}"),
                    format!("{} {why}", spec.label),
                ));
            }
        }
        // The core's gain cap is never tighter than the built-in law's own largest
        // change plus 1 dB, so the built-in never meets the core's cap.
        if facts.builtin && facts.id == hertzforge_shock_law_math::srs::ID {
            let own = self
                .law
                .params
                .get("max_db_per_pulse")
                .and_then(Value::as_f64)
                .unwrap_or(hertzforge_shock_law_math::srs::KNOBS[0].default);
            if self.max_gain_rise_db < own + 1.0 {
                r.errors.push(Item::field(
                    "out_of_range",
                    "control.max_gain_rise_db",
                    format!(
                        "The cap on a gain's rise is {} dB, tighter than the law's own largest \
                         change per pulse plus 1 dB ({} dB); raise the cap or lower the law's.",
                        num(self.max_gain_rise_db),
                        num(own + 1.0)
                    ),
                ));
            }
        }
    }
}

fn alarm_wider(field: &str) -> Item {
    Item::field(
        "alarm_wider_than_abort",
        field,
        "The abort band is narrower than the alarm band on one side; a band would abort \
         without ever alarming. Widen the abort band or narrow the alarm band.",
    )
}

/// `ring_out_s` seconds in samples at `fs_hz`, as the controller counts them.
pub fn ring_out_samples(ring_out_s: f64, fs_hz: f64) -> usize {
    let n = (ring_out_s * fs_hz).round();
    if n.is_finite() && n > 0.0 {
        n as usize
    } else {
        0
    }
}

/// The offline fit's warning when it did not reach its tolerance.
pub fn push_not_converged(r: &mut Report, syn: &Synthesis, tol_db: f64, field: &str) {
    if !syn.converged {
        r.warnings.push(Item::field(
            "synthesis_not_converged",
            field,
            format!(
                "The offline synthesis did not bring every band within {} dB of the target in \
                 {} passes (the worst band is {} dB off); the run's law refines from there.",
                num(round(tol_db, 2)),
                syn.iters,
                num(round(syn.worst_err_db.unwrap_or(0.0), 2))
            ),
        ));
    }
}

/// Check the parts the offline synthesis reads: the target, the band grid, the
/// synthesis settings and the rate. `prefix` is prepended to every field (empty
/// in a configuration, where the fields are top level). True when the synthesis
/// can be run.
pub fn check_synthesis(r: &mut Report, s: &WireSynthesis, prefix: &str) -> bool {
    let before = r.errors.len();
    let at = |f: &str| format!("{prefix}{f}");
    let pos = |x: f64| x.is_finite() && x > 0.0;
    let fs = s.fs_hz;
    let fs_field = if prefix.is_empty() {
        "acquisition.fs_hz".to_owned()
    } else {
        at("fs_hz")
    };
    out_of_range(
        r,
        &fs_field,
        fs,
        fs.is_finite() && (FS_RANGE.0..=FS_RANGE.1).contains(&fs),
        &format!(
            "the station runs from {} to {} Hz",
            num(FS_RANGE.0),
            num(FS_RANGE.1)
        ),
    );
    let b = &s.bands;
    out_of_range(
        r,
        &at("bands.f_lo_hz"),
        b.f_lo_hz,
        pos(b.f_lo_hz),
        "it is above 0 Hz",
    );
    let n_ok = b.bands_per_octave.is_finite()
        && b.bands_per_octave.fract() == 0.0
        && (1.0..=MAX_BANDS_PER_OCTAVE).contains(&b.bands_per_octave);
    out_of_range(
        r,
        &at("bands.bands_per_octave"),
        b.bands_per_octave,
        n_ok,
        "it is a whole number from 1 to 24",
    );
    out_of_range(
        r,
        &at("bands.q"),
        b.q,
        b.q.is_finite() && b.q > Q_RANGE.0 && b.q <= Q_RANGE.1,
        "it is above 0.5 and at most 100",
    );
    let mut centres = Vec::new();
    if !(b.f_hi_hz.is_finite() && b.f_hi_hz > b.f_lo_hz) {
        out_of_range(
            r,
            &at("bands.f_hi_hz"),
            b.f_hi_hz,
            false,
            "it is above the grid's low edge",
        );
    } else if pos(b.f_lo_hz) && n_ok {
        centres = b.centres();
        if centres.is_empty() {
            r.errors.push(Item::field(
                "bands_empty",
                at("bands"),
                "The band grid has no bands; give a low edge above 0 Hz and a high edge above \
                 it.",
            ));
        } else if fs.is_finite() && fs > 0.0 {
            let top = *centres.last().unwrap_or(&0.0);
            if top >= fs / 2.0 {
                r.errors.push(Item::field(
                    "band_above_grid",
                    at("bands.f_hi_hz"),
                    format!(
                        "The band grid's top centre is {} Hz, at or above half the sample rate \
                         ({} Hz).",
                        num(round(top, 2)),
                        num(fs / 2.0)
                    ),
                ));
            }
        }
    }
    match &s.target {
        WireTarget::Flat { accel_g } => out_of_range(
            r,
            &at("target.accel_g"),
            *accel_g,
            pos(*accel_g) && *accel_g <= 1.0e6,
            "it is above 0 g and at most 1000000 g",
        ),
        WireTarget::Breakpoints { points } => {
            if points.len() < 2 {
                r.errors.push(Item::field(
                    "out_of_range",
                    at("target.points"),
                    "A breakpoint target has at least two points.",
                ));
            }
            for (i, (f, g)) in points.iter().enumerate() {
                if !(pos(*f) && pos(*g)) {
                    r.errors.push(Item::field(
                        "out_of_range",
                        format!("{}[{i}]", at("target.points")),
                        format!(
                            "The breakpoint is [{}, {}]; frequency and acceleration are each \
                             above 0.",
                            num(*f),
                            num(*g)
                        ),
                    ));
                }
            }
            if let Some(i) = points.windows(2).position(|w| w[1].0 <= w[0].0) {
                r.errors.push(Item::field(
                    "target_not_ascending",
                    format!("{}[{}]", at("target.points"), i + 1),
                    format!(
                        "The breakpoints go from {} Hz to {} Hz; list them from the lowest \
                         frequency up, each above the one before.",
                        num(points[i].0),
                        num(points[i + 1].0)
                    ),
                ));
            } else if let (Some(first), Some(last), Some(top)) =
                (points.first(), points.last(), centres.last())
            {
                if first.0 > b.f_lo_hz || last.0 < *top {
                    r.errors.push(Item::field(
                        "target_not_covering",
                        at("target.points"),
                        format!(
                            "The breakpoints run from {} to {} Hz; they cover the band grid, \
                             from {} Hz to its top centre at {} Hz.",
                            num(first.0),
                            num(last.0),
                            num(b.f_lo_hz),
                            num(round(*top, 2))
                        ),
                    ));
                }
            }
        }
    }
    out_of_range(
        r,
        &at("synthesis.max_iters"),
        s.max_iters as f64,
        (1..=MAX_SYNTHESIS_ITERS).contains(&s.max_iters),
        &format!("it is from 1 to {MAX_SYNTHESIS_ITERS}"),
    );
    out_of_range(
        r,
        &at("synthesis.tol_db"),
        s.tol_db,
        pos(s.tol_db) && s.tol_db <= 20.0,
        "it is above 0 and at most 20 dB",
    );
    out_of_range(
        r,
        &at("synthesis.max_gain_db_per_iter"),
        s.max_gain_db_per_iter,
        pos(s.max_gain_db_per_iter) && s.max_gain_db_per_iter <= MAX_GAIN_STEP_DB,
        &format!("it is above 0 and at most {} dB", num(MAX_GAIN_STEP_DB)),
    );
    if let Some(c) = &s.components {
        if c.engine() != s.engine {
            r.errors.push(Item::field(
                "components_engine_mismatch",
                at("synthesis.components.engine"),
                format!(
                    "The component table is for {}, and the synthesis engine is {}; import a \
                     table for the engine, or change the engine.",
                    engine_name(c.engine()),
                    engine_name(s.engine)
                ),
            ));
        }
        if c.is_empty() || c.len() > MAX_COMPONENTS {
            r.errors.push(Item::field(
                "out_of_range",
                at("synthesis.components.rows"),
                format!(
                    "The component table has {} rows; it has from 1 to {MAX_COMPONENTS}.",
                    c.len()
                ),
            ));
        }
        for (i, why) in c.row_problems() {
            r.errors.push(Item::field(
                "component_invalid",
                format!("{}[{i}]", at("synthesis.components.rows")),
                format!("Component {} is not usable: {why}.", i + 1),
            ));
        }
    }
    r.errors.len() == before
}

fn check_schedule(r: &mut Report, w: &WireSrs) {
    if w.levels_db.is_empty() {
        r.errors.push(Item::field(
            "levels_empty",
            "schedule.levels_db",
            "The schedule has no levels; give at least the full level.",
        ));
    }
    for (i, l) in w.levels_db.iter().enumerate() {
        out_of_range(
            r,
            &format!("schedule.levels_db[{i}]"),
            *l,
            (LEVEL_DB_RANGE.0..=LEVEL_DB_RANGE.1).contains(l),
            &format!(
                "a level is from {} to {} dB",
                num(LEVEL_DB_RANGE.0),
                num(LEVEL_DB_RANGE.1)
            ),
        );
    }
    for (i, win) in w.levels_db.windows(2).enumerate() {
        if win[1] < win[0] {
            r.errors.push(Item::field(
                "levels_not_ascending",
                format!("schedule.levels_db[{}]", i + 1),
                format!(
                    "The levels go down ({} dB then {} dB); list them from the lowest to the \
                     full level.",
                    num(win[0]),
                    num(win[1])
                ),
            ));
        }
    }
    if !(w.pulses.len() == 1 || w.pulses.len() == w.levels_db.len()) {
        r.errors.push(Item::field(
            "pulses_length_mismatch",
            "schedule.pulses",
            format!(
                "The schedule has {} pulse counts for {} levels; give one count for every \
                 level, or one per level.",
                w.pulses.len(),
                w.levels_db.len()
            ),
        ));
    }
    for (i, p) in w.pulses.iter().enumerate() {
        let min = if w.mode == ShockRunMode::Auto { 1 } else { 0 };
        out_of_range(
            r,
            &format!("schedule.pulses[{i}]"),
            f64::from(*p),
            (min..=MAX_PULSES_PER_RUNG).contains(p),
            &format!("a level fires from {min} to {MAX_PULSES_PER_RUNG} pulses"),
        );
    }
    if let (Some(lo), Some(hi)) = (w.levels_db.first(), w.levels_db.last()) {
        out_of_range(
            r,
            "schedule.start_db",
            w.start_db,
            w.start_db >= *lo && w.start_db <= *hi,
            &format!(
                "it is between the first and last levels ({} to {} dB)",
                num(*lo),
                num(*hi)
            ),
        );
    }
    out_of_range(
        r,
        "schedule.max_pulses_per_level",
        f64::from(w.max_pulses_per_level),
        (1..=100).contains(&w.max_pulses_per_level),
        "it is from 1 to 100",
    );
}

/// The feasibility verdict on the synthesized pulse at full level, with its words.
fn feasibility_verdict(cfg: &WireSrs, syn: &Synthesis) -> (&'static str, String) {
    let Some(f) = &cfg.feasibility else {
        return (
            "not_evaluated",
            "No shaker was given, so the pulse was not checked against shaker ratings.".into(),
        );
    };
    let report = pulse_feasibility(
        &f.shaker,
        &f.moving_mass,
        &syn.waveform_g,
        cfg.fs_hz(),
        FeasibilityTolerances {
            residual_velocity_ips: f.residual_velocity_ips,
            residual_disp_in: f.residual_disp_in,
        },
    );
    verdict(&report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Value {
        json!({
            "target": {"kind": "flat", "accel_g": 50},
            "bands": {"f_lo_hz": 100, "f_hi_hz": 2000},
            "schedule": {"mode": "manual", "levels_db": [0], "pulses": [1]},
            "limits": {"drive_vpk_ceiling": 0.5, "drive_vpk_abort": 0.8}
        })
    }

    #[test]
    fn the_defaults_fill_every_field_the_contract_names() {
        let w = parse(&minimal()).unwrap();
        assert_eq!(w.synthesis.bands.bands_per_octave, 3.0);
        assert_eq!(w.synthesis.bands.q, 10.0);
        assert_eq!(w.synthesis.engine, EngineKind::Wavsyn);
        assert_eq!(w.synthesis.max_iters, 50);
        assert_eq!(w.envelope_alarm_db, (3.0, 3.0));
        assert_eq!(w.envelope_abort_db, (9.0, 6.0));
        assert_eq!(w.ring_out_s, 0.05);
        assert!(w.pvss && w.review);
        assert_eq!(w.excitation, SysIdExcitation::Shaped);
        assert_eq!((w.sysid_level_db, w.sysid_nfft), (-20.0, 1024));
        assert_eq!(w.sysid_max_vpk, None);
        assert!((w.identification_vpk_cap() - 0.05).abs() < 1e-15);
        assert_eq!(w.law.id, "srs_ratio");
        assert_eq!(w.max_gain_rise_db, 6.0);
        assert_eq!(w.abort_factor, 2.0);
        assert_eq!(w.spacing_s(), 1.0);
    }

    #[test]
    fn a_wrong_shape_is_bad_params_naming_the_field() {
        for (patch, field) in [
            (json!({"target": {"kind": "ramp"}}), "config.target.kind"),
            (
                json!({"target": {"kind": "breakpoints", "points": [[1, 2, 3]]}}),
                "config.target.points[0]",
            ),
            (json!({"bands": {"f_lo_hz": 100}}), "config.bands.f_hi_hz"),
            (
                json!({"synthesis": {"engine": "fourier"}}),
                "config.synthesis.engine",
            ),
            (
                json!({"envelope": {"alarm_db": [3]}}),
                "config.envelope.alarm_db",
            ),
            (
                json!({"limit_channels": {"channels": [{"channel": 3, "bound": {"kind": "x"}}]}}),
                "config.limit_channels.channels[0].bound.kind",
            ),
        ] {
            let mut c = minimal();
            for (k, v) in patch.as_object().unwrap() {
                c[k] = v.clone();
            }
            let e = parse(&c).unwrap_err();
            assert_eq!(e.reason, Some("bad_params"), "{patch}");
            assert_eq!(e.detail.as_ref().unwrap()["param"], field, "{patch}");
        }
    }

    #[test]
    fn the_band_spacing_maps_to_the_controllers_named_fractions() {
        let b = |n| WireBands {
            f_lo_hz: 100.0,
            f_hi_hz: 2000.0,
            bands_per_octave: n,
            q: 10.0,
        };
        assert_eq!(b(1.0).fraction(), BandFraction::Whole);
        assert_eq!(b(3.0).fraction(), BandFraction::Third);
        assert_eq!(b(6.0).fraction(), BandFraction::Sixth);
        assert_eq!(b(12.0).fraction(), BandFraction::Twelfth);
        assert_eq!(b(4.0).fraction(), BandFraction::Arbitrary(4.0));
        assert_eq!(b(3.0).centres().len(), 14);
    }
}
