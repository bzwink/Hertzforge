//! The TWR test configuration on the wire (schema/PROTOCOL.md, "TWR
//! Configuration"): parsing (types and structure; a wrong shape is `bad_params`
//! naming the field), the checks (content; every problem a named item located by
//! `field`), the derived values, and the controller's own `TwrConfig` built from it
//! with every field supplied.

use super::reference::{self, Prepared, Stored};
use crate::protocol::{num, ErrorCode, MethodError};
use crate::shock::config::{param_problem, LawFacts, LawSetup, Obj};
use crate::shock::view::sig;
use crate::table::{self, Item, Report};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_shock_common::law_guard::LawGuards;
use hertzforge_twr::control::{
    TwrControl, CAP_MARGIN_OVER_CLAMP_DB, DEFAULT_ALIGNMENT_FRAMES, DEFAULT_FIRST_DRIVE_RATIO,
    DEFAULT_MAX_BIN_RISE_DB, DEFAULT_MAX_PEAK_RISE_DB, MAX_RECORD_LEN, TWR_CORRECT_ID,
};
use hertzforge_twr::schedule::{GateFailPolicy, TwrRunMode, DEFAULT_LEVEL_TABLE_DB};
use hertzforge_twr::service::config::{CaptureRules, MetaField, PrepConfig};
use hertzforge_twr::{
    Excitation, FeasibilityCeilings, InverseConfig, LawConfig, RunSchedule, Section, SysIdConfig,
    TwrConfig, TwrLevelSchedule, TwrLimits,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The rates a run may ask the station for, Hz.
pub const FS_RANGE: (f64, f64) = (1000.0, 204_800.0);
/// The levels a table may hold, dB.
pub const LEVEL_DB_RANGE: (f64, f64) = (-60.0, 12.0);
/// The largest drive ceiling, volts peak.
pub const MAX_DRIVE_V: f64 = 10.0;
/// The largest per-call cap, dB.
pub const MAX_CAP_DB: f64 = 40.0;
/// The largest alignment drift a run may allow, frames.
pub const MAX_ALIGNMENT_FRAMES: f64 = 16.0;
/// The most playback plays a `loop` schedule may ask for.
pub const MAX_LOOP_COUNT: u64 = 100_000;
/// The longest `duration` schedule, seconds of section time.
pub const MAX_DURATION_S: f64 = 86_400.0;
/// The quiet pads when none are given, samples.
pub const DEFAULT_PADS: (usize, usize) = (512, 1024);

/// The built-in law's parameters, with the values the core runs it at when none
/// is given.
pub const BUILTIN_BETA: (&str, f64) = ("beta", 0.5);
pub const BUILTIN_CLAMP: (&str, f64) = ("max_db_per_iter", 6.0);
pub const BUILTIN_BACKOFF: (&str, f64) = ("initial_backoff_db", 0.0);

/// `prep.detrend` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detrend {
    None,
    Mean,
    Linear,
}

impl Detrend {
    pub fn name(self) -> &'static str {
        match self {
            Detrend::None => "none",
            Detrend::Mean => "mean",
            Detrend::Linear => "linear",
        }
    }

    /// The core's `detrend` field.
    pub fn core(self) -> Option<bool> {
        match self {
            Detrend::None => None,
            Detrend::Mean => Some(false),
            Detrend::Linear => Some(true),
        }
    }
}

/// The preparation, every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct WirePrep {
    pub auto_resample: bool,
    pub detrend: Detrend,
    pub highpass_hz: Option<f64>,
    pub scale: f64,
    pub taper_frac: Option<f64>,
}

impl Default for WirePrep {
    fn default() -> Self {
        WirePrep {
            auto_resample: true,
            detrend: Detrend::Mean,
            highpass_hz: Some(5.0),
            scale: 1.0,
            taper_frac: Some(0.02),
        }
    }
}

impl WirePrep {
    pub fn core(&self) -> PrepConfig {
        PrepConfig {
            auto_resample: self.auto_resample,
            detrend: self.detrend.core(),
            highpass_hz: self.highpass_hz,
            scale: self.scale,
            taper_frac: self.taper_frac,
        }
    }
}

/// The correction band and its trust floors; `band_hz` `None` is the
/// identification's band.
#[derive(Debug, Clone, PartialEq)]
pub struct WireCorrection {
    pub band_hz: Option<(f64, f64)>,
    pub ref_floor_db: f64,
    pub min_band_energy: f64,
}

impl Default for WireCorrection {
    fn default() -> Self {
        let l = LawConfig::default();
        WireCorrection {
            band_hz: None,
            ref_floor_db: l.ref_floor_db,
            min_band_energy: l.min_masked_energy_frac,
        }
    }
}

/// The kinematic ceilings and the high-pass the kinematics are integrated after.
#[derive(Debug, Clone, PartialEq)]
pub struct WireFeasibility {
    pub peak_accel: Option<f64>,
    pub peak_velocity_ips: Option<f64>,
    pub disp_pkpk_in: Option<f64>,
    pub kinematics_hp_hz: f64,
}

impl Default for WireFeasibility {
    fn default() -> Self {
        WireFeasibility {
            peak_accel: None,
            peak_velocity_ips: None,
            disp_pkpk_in: None,
            kinematics_hp_hz: 5.0,
        }
    }
}

impl WireFeasibility {
    pub fn ceilings(&self) -> FeasibilityCeilings {
        FeasibilityCeilings {
            peak_accel: self.peak_accel,
            peak_velocity_ips: self.peak_velocity_ips,
            disp_pkpk_in: self.disp_pkpk_in,
        }
    }

    pub fn any(&self) -> bool {
        self.peak_accel.is_some() || self.peak_velocity_ips.is_some() || self.disp_pkpk_in.is_some()
    }
}

/// The playback once full level has gated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WireRun {
    Once,
    Loop(u64),
    Duration(f64),
}

impl WireRun {
    pub fn core(self) -> RunSchedule {
        match self {
            WireRun::Once => RunSchedule::Once,
            WireRun::Loop(n) => RunSchedule::LoopN(n.min(u64::from(u32::MAX)) as u32),
            WireRun::Duration(s) => RunSchedule::DurationS(s),
        }
    }

    pub fn to_json(self) -> Value {
        match self {
            WireRun::Once => json!({"schedule": "once"}),
            WireRun::Loop(n) => json!({"schedule": "loop", "count": n}),
            WireRun::Duration(s) => json!({"schedule": "duration", "duration_s": num(s)}),
        }
    }
}

/// What a preview and a configuration share: how the stored record is made into
/// the record every play plays.
#[derive(Debug, Clone, PartialEq)]
pub struct WireShape {
    pub sha256: String,
    pub path: Option<String>,
    pub prep: WirePrep,
    /// `start`, `len`; `None` is the whole prepared reference.
    pub section: Option<(usize, usize)>,
    pub pads: (usize, usize),
    /// The rate as sent; `None` is the reference's own.
    pub fs_hz: Option<f64>,
    pub correction: WireCorrection,
    pub feasibility: WireFeasibility,
}

/// A parsed TWR configuration, every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct WireTwr {
    pub title: String,
    pub shape: WireShape,
    pub excitation: Excitation,
    pub sysid_drive_vrms: f64,
    pub sysid_duration_s: f64,
    pub nfft: u64,
    pub sysid_band_hz: (f64, f64),
    pub min_mean_coherence: f64,
    pub review: bool,
    pub tikhonov_floor: f64,
    pub coherence_floor: f64,
    pub law: LawSetup,
    pub law_budget: f64,
    pub law_misses: u64,
    pub max_peak_rise_db: f64,
    pub max_bin_rise_db: f64,
    pub first_drive_ratio: f64,
    pub alignment_frames: f64,
    pub mode: TwrRunMode,
    pub levels_db: Vec<f64>,
    pub gate_pct: f64,
    pub max_iterations: u64,
    pub on_gate_fail: GateFailPolicy,
    pub run: WireRun,
    pub during_playback: bool,
    pub drive_vpk_ceiling: f64,
    pub drive_clip_vpk: Option<f64>,
    pub drive_vrms_limit: Option<f64>,
    pub response_rms_limit: Option<f64>,
    pub envelope_alarm_frac: f64,
    pub envelope_abort_frac: f64,
    pub envelope_debounce_samples: u64,
    pub divergence_plays: u64,
    pub divergence_rise_frac: f64,
    pub metadata: Vec<MetaField>,
    /// The operator's words for the run's record: who ran it, on what, and notes.
    pub operator: String,
    pub article: String,
    pub notes: String,
    /// What the test is run to: the document and its clause.
    pub specification: Option<(String, String)>,
    /// Whether the record holds the inputs and the drive as streams.
    pub record_streams: bool,
    /// The configuration as sent, for `twr.config` and `twr.reset`.
    pub raw: Value,
}

/// Everything a configuration's checks are made against.
pub struct CheckContext<'a> {
    /// The channel table the run would arm with.
    pub table: &'a ChannelTable,
    /// Looks a law up by id.
    pub law: &'a dyn Fn(&str) -> Option<LawFacts>,
    /// Looks a stored reference up by its SHA-256.
    pub reference: &'a dyn Fn(&str) -> Option<Arc<Stored>>,
    /// Checks that the file a configuration names still reads to its record.
    pub file_check: &'a dyn Fn(&Stored, &str) -> Result<(), String>,
}

/// The record a run plays and measures on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub fs_hz: f64,
    pub record_len: usize,
    pub section_start: usize,
    pub section_len: usize,
    /// True once the run has armed and the rate is the station's.
    pub actual: bool,
}

impl Grid {
    pub fn df_hz(&self) -> f64 {
        self.fs_hz / self.record_len.max(1) as f64
    }

    pub fn record_s(&self) -> f64 {
        self.record_len as f64 / self.fs_hz
    }

    pub fn to_json(&self) -> Value {
        json!({
            "fs_hz": num(sig(self.fs_hz, 9)),
            "record_len": self.record_len,
            "df_hz": num(sig(self.df_hz(), 9)),
            "record_s": num(sig(self.record_s(), 9)),
            "section_start": self.section_start,
            "section_len": self.section_len,
            "actual": self.actual,
        })
    }
}

/// What a preview computes: the grid, the section, its kinematics, the coverage
/// and the feasibility verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewDerived {
    pub grid: Grid,
    pub section_peak: f64,
    pub section_rms: f64,
    pub section_s: f64,
    pub peak_accel: f64,
    pub peak_velocity_ips: f64,
    pub disp_pkpk_in: f64,
    pub kinematics_hp_hz: f64,
    pub band_hz: (f64, f64),
    pub ref_floor_db: f64,
    pub coverage: f64,
    pub feasibility: Feasibility,
}

/// The feasibility verdict, its words and the checks behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Feasibility {
    pub verdict: &'static str,
    pub message: String,
    /// `name`, `value`, `limit`, `pass`.
    pub checks: Vec<(String, f64, f64, bool)>,
}

impl Feasibility {
    pub fn blocked(&self) -> bool {
        self.verdict == "exceeded"
    }

    pub fn to_json(&self) -> Value {
        json!({
            "verdict": self.verdict,
            "message": self.message,
            "checks": self.checks.iter().map(|(n, v, l, p)| json!({
                "name": n, "value": num(sig(*v, 6)), "limit": num(sig(*l, 6)), "pass": p,
            })).collect::<Vec<_>>(),
        })
    }
}

impl PreviewDerived {
    pub fn error_floor_pct(&self) -> f64 {
        100.0 * (1.0 - self.coverage).max(0.0).sqrt()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "grid": self.grid.to_json(),
            "section": {
                "peak": num(sig(self.section_peak, 6)),
                "rms": num(sig(self.section_rms, 6)),
                "duration_s": num(sig(self.section_s, 6)),
            },
            "kinematics": {
                "peak_accel": num(sig(self.peak_accel, 6)),
                "peak_velocity_ips": num(sig(self.peak_velocity_ips, 6)),
                "disp_pkpk_in": num(sig(self.disp_pkpk_in, 6)),
                "kinematics_hp_hz": num(self.kinematics_hp_hz),
            },
            "coverage": {
                "band_hz": [num(self.band_hz.0), num(self.band_hz.1)],
                "ref_floor_db": num(self.ref_floor_db),
                "fraction": num(sig(self.coverage, 6)),
                "error_floor_pct": num(sig(self.error_floor_pct(), 4)),
            },
            "feasibility": self.feasibility.to_json(),
        })
    }
}

/// What configure computes from a configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    pub preview: PreviewDerived,
    pub sha256: String,
    pub reference_name: String,
    pub reference_fs_hz: f64,
    pub units: String,
    pub resampled: bool,
    pub call_budget_s: f64,
    pub plays_target: u32,
    pub identification_s: f64,
    pub law: LawRef,
}

/// The run's law, as `derived` and the state name it.
#[derive(Debug, Clone, PartialEq)]
pub struct LawRef {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Empty for a built-in law.
    pub sha256: String,
}

impl LawRef {
    pub fn of(f: &LawFacts) -> LawRef {
        LawRef {
            id: f.id.clone(),
            name: f.name.clone(),
            version: f.version.clone(),
            sha256: f.sha256.clone(),
        }
    }

    pub fn sha_json(&self) -> Value {
        if self.sha256.is_empty() {
            Value::Null
        } else {
            Value::from(self.sha256.as_str())
        }
    }
}

impl Derived {
    pub fn to_json(&self) -> Value {
        let mut v = self.preview.to_json();
        if let Value::Object(m) = &mut v {
            m.insert(
                "reference".into(),
                json!({
                    "sha256": self.sha256,
                    "name": self.reference_name,
                    "fs_hz": num(sig(self.reference_fs_hz, 9)),
                    "units": self.units,
                    "resampled": self.resampled,
                }),
            );
            m.insert("call_budget_s".into(), num(sig(self.call_budget_s, 6)));
            m.insert("plays_target".into(), Value::from(self.plays_target));
            m.insert(
                "identification_s".into(),
                num(sig(self.identification_s, 6)),
            );
        }
        v
    }
}

// ---------------------------------------------------------------------------------
// Parsing: types and structure. Content problems are items, not parse errors.
// ---------------------------------------------------------------------------------

fn bad(path: &str, message: impl Into<String>) -> MethodError {
    MethodError::bad_params(path, message)
}

/// `Some(default)` when `k` is absent, `None` when it is null, else the number.
fn nullable(o: &Obj<'_>, k: &str, default: Option<f64>) -> Result<Option<f64>, MethodError> {
    match o.m.get(k) {
        None => Ok(default),
        Some(Value::Null) => Ok(None),
        Some(_) => o.opt_f64(k),
    }
}

fn count(o: &Obj<'_>, k: &str, default: u64) -> Result<u64, MethodError> {
    Ok(o.opt_count(k)?.unwrap_or(default))
}

const DETRENDS: [(&str, Detrend); 3] = [
    ("none", Detrend::None),
    ("mean", Detrend::Mean),
    ("linear", Detrend::Linear),
];
const EXCITATIONS: [(&str, Excitation); 2] = [
    ("random", Excitation::Random),
    ("log_chirp", Excitation::LogChirp),
];
const MODES: [(&str, TwrRunMode); 2] = [("auto", TwrRunMode::Auto), ("manual", TwrRunMode::Manual)];
const GATE_FAIL: [(&str, GateFailPolicy); 2] = [
    ("abort", GateFailPolicy::Abort),
    ("hold", GateFailPolicy::Hold),
];

pub fn excitation_name(e: Excitation) -> &'static str {
    match e {
        Excitation::Random => "random",
        Excitation::LogChirp => "log_chirp",
    }
}

pub fn mode_name(m: TwrRunMode) -> &'static str {
    match m {
        TwrRunMode::Auto => "auto",
        TwrRunMode::Manual => "manual",
    }
}

/// Parse a `prep` object (absent: the defaults).
pub(crate) fn parse_prep(o: Option<Obj<'_>>) -> Result<WirePrep, MethodError> {
    let d = WirePrep::default();
    let Some(p) = o else {
        return Ok(d);
    };
    Ok(WirePrep {
        auto_resample: p.opt_bool("auto_resample")?.unwrap_or(d.auto_resample),
        detrend: p.choice("detrend", &DETRENDS, Some(d.detrend))?,
        highpass_hz: nullable(&p, "highpass_hz", d.highpass_hz)?,
        scale: p.f64_or("scale", d.scale)?,
        taper_frac: nullable(&p, "taper_frac", d.taper_frac)?,
    })
}

/// Parse a `section` value at `path` (absent or null: the whole reference).
pub fn parse_section(v: Option<&Value>, path: &str) -> Result<Option<(usize, usize)>, MethodError> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let s = Obj::of(v, path)?;
    let start = s.opt_count("start")?.ok_or_else(|| {
        bad(
            &s.at("start"),
            format!("\"{}\" is a sample, from 0", s.at("start")),
        )
    })?;
    let len = s.opt_count("len")?.ok_or_else(|| {
        bad(
            &s.at("len"),
            format!("\"{}\" is a number of samples", s.at("len")),
        )
    })?;
    Ok(Some((start as usize, len as usize)))
}

pub(crate) fn parse_pads(o: Option<Obj<'_>>) -> Result<(usize, usize), MethodError> {
    let Some(p) = o else {
        return Ok(DEFAULT_PADS);
    };
    Ok((
        count(&p, "pre", DEFAULT_PADS.0 as u64)?.min(1 << 40) as usize,
        count(&p, "post", DEFAULT_PADS.1 as u64)?.min(1 << 40) as usize,
    ))
}

pub(crate) fn parse_correction(o: Option<Obj<'_>>) -> Result<WireCorrection, MethodError> {
    let d = WireCorrection::default();
    let Some(c) = o else {
        return Ok(d);
    };
    Ok(WireCorrection {
        band_hz: c.band("band_hz")?,
        ref_floor_db: c.f64_or("ref_floor_db", d.ref_floor_db)?,
        min_band_energy: c.f64_or("min_band_energy", d.min_band_energy)?,
    })
}

pub(crate) fn parse_feasibility(o: Option<Obj<'_>>) -> Result<WireFeasibility, MethodError> {
    let d = WireFeasibility::default();
    let Some(f) = o else {
        return Ok(d);
    };
    Ok(WireFeasibility {
        peak_accel: f.opt_f64("peak_accel")?,
        peak_velocity_ips: f.opt_f64("peak_velocity_ips")?,
        disp_pkpk_in: f.opt_f64("disp_pkpk_in")?,
        kinematics_hp_hz: f.f64_or("kinematics_hp_hz", d.kinematics_hp_hz)?,
    })
}

/// The reference choice: `{"sha256", "path"}`.
pub fn parse_reference(v: &Value, path: &str) -> Result<(String, Option<String>), MethodError> {
    let r = Obj::of(v, path)?;
    let sha = r.opt_str("sha256")?.ok_or_else(|| {
        bad(
            &r.at("sha256"),
            "the reference is named by the SHA-256 twr.reference.import answered",
        )
    })?;
    if !reference::is_sha256(sha) {
        return Err(bad(
            &r.at("sha256"),
            format!(
                "\"{}\" is a SHA-256: 64 lowercase hexadecimal digits",
                r.at("sha256")
            ),
        ));
    }
    Ok((sha.to_owned(), r.opt_str("path")?.map(str::to_owned)))
}

/// Parse the parts of a configuration a preview shares from `o` (the whole
/// configuration or the preview's parameters).
pub(crate) fn parse_shape(o: &Obj<'_>) -> Result<WireShape, MethodError> {
    let (sha256, path) = parse_reference(o.require("reference")?, &o.at("reference"))?;
    let fs_hz = match o.opt_obj("acquisition")? {
        Some(a) => a.opt_f64("fs_hz")?,
        None => None,
    };
    Ok(WireShape {
        sha256,
        path,
        prep: parse_prep(o.opt_obj("prep")?)?,
        section: parse_section(o.m.get("section"), &o.at("section"))?,
        pads: parse_pads(o.opt_obj("pads")?)?,
        fs_hz,
        correction: parse_correction(o.opt_obj("correction")?)?,
        feasibility: parse_feasibility(o.opt_obj("feasibility")?)?,
    })
}

/// Parse `twr.reference.preview`'s parameters.
pub fn parse_preview(params: &Map<String, Value>) -> Result<WireShape, MethodError> {
    let o = Obj {
        m: params,
        path: String::new(),
    };
    // A preview's parameters are at the top: its refusals name `reference.sha256`,
    // not a path under a configuration.
    parse_shape(&o).map_err(|mut e| {
        if let Some(d) = e.detail.as_deref_mut() {
            if let Some(Value::String(p)) = d.get_mut("param") {
                *p = p.trim_start_matches('.').to_owned();
            }
        }
        e.message = e.message.replace("\".", "\"");
        e
    })
}

pub fn parse(config: &Value) -> Result<WireTwr, MethodError> {
    let c = Obj::of(config, "config")?;
    let title = c.opt_str("title")?.unwrap_or("").to_owned();
    let shape = parse_shape(&c)?;

    let sd = SysIdConfig::default();
    let (mut excitation, mut drive_vrms, mut duration_s, mut nfft) =
        (sd.excitation, sd.drive_rms, sd.duration_s, sd.nfft as u64);
    let (mut band, mut min_mean_coherence, mut review) =
        ((sd.f_lo_hz, sd.f_hi_hz), sd.min_mean_coherence, true);
    if let Some(i) = c.opt_obj("identification")? {
        excitation = i.choice("excitation", &EXCITATIONS, Some(excitation))?;
        drive_vrms = i.f64_or("drive_vrms", drive_vrms)?;
        duration_s = i.f64_or("duration_s", duration_s)?;
        nfft = count(&i, "nfft", nfft)?;
        band = i.band("band_hz")?.unwrap_or(band);
        min_mean_coherence = i.f64_or("min_mean_coherence", min_mean_coherence)?;
        review = i.opt_bool("review")?.unwrap_or(review);
    }

    let inv = InverseConfig::default();
    let (mut tikhonov_floor, mut coherence_floor) = (inv.tikhonov_beta_floor, inv.coherence_floor);
    if let Some(i) = c.opt_obj("inverse")? {
        tikhonov_floor = i.f64_or("tikhonov_floor", tikhonov_floor)?;
        coherence_floor = i.f64_or("coherence_floor", coherence_floor)?;
    }

    let g = TwrControl::default();
    let mut law = LawSetup {
        id: TWR_CORRECT_ID.to_owned(),
        params: Map::new(),
    };
    let (mut law_budget, mut law_misses) =
        (g.guards.budget_fraction, u64::from(g.guards.max_misses));
    let (mut max_peak_rise_db, mut max_bin_rise_db) =
        (DEFAULT_MAX_PEAK_RISE_DB, DEFAULT_MAX_BIN_RISE_DB);
    let (mut first_drive_ratio, mut alignment_frames) =
        (DEFAULT_FIRST_DRIVE_RATIO, DEFAULT_ALIGNMENT_FRAMES);
    if let Some(o) = c.opt_obj("control")? {
        if let Some(l) = o.opt_obj("law")? {
            let id = l
                .opt_str("id")?
                .ok_or_else(|| bad(&l.at("id"), "the law is named by its catalog \"id\""))?;
            let params = match l.opt_obj("params")? {
                None => Map::new(),
                Some(p) => p.m.clone(),
            };
            law = LawSetup {
                id: id.to_owned(),
                params,
            };
        }
        law_budget = o.f64_or("law_budget", law_budget)?;
        law_misses = count(&o, "law_misses", law_misses)?;
        max_peak_rise_db = o.f64_or("max_peak_rise_db", max_peak_rise_db)?;
        max_bin_rise_db = o.f64_or("max_bin_rise_db", max_bin_rise_db)?;
        first_drive_ratio = o.f64_or("first_drive_ratio", first_drive_ratio)?;
        alignment_frames = o.f64_or("alignment_frames", alignment_frames)?;
    }

    let ls = TwrLevelSchedule::default();
    let (mut mode, mut levels_db, mut gate_pct) = (
        ls.mode,
        DEFAULT_LEVEL_TABLE_DB.to_vec(),
        ls.gate_rms_error_pct,
    );
    let (mut max_iterations, mut on_gate_fail) =
        (u64::from(ls.max_iterations_per_level), ls.on_gate_fail);
    if let Some(s) = c.opt_obj("schedule")? {
        mode = s.choice("mode", &MODES, Some(mode))?;
        if let Some(list) = s.array("levels_db")? {
            levels_db = list
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| {
                        bad(
                            &format!("config.schedule.levels_db[{i}]"),
                            "a level is a number of dB",
                        )
                    })
                })
                .collect::<Result<_, _>>()?;
        }
        gate_pct = s.f64_or("gate_pct", gate_pct)?;
        max_iterations = count(&s, "max_iterations", max_iterations)?;
        on_gate_fail = s.choice("on_gate_fail", &GATE_FAIL, Some(on_gate_fail))?;
    }

    let run = match c.opt_obj("run")? {
        None => WireRun::Once,
        Some(r) => match r.opt_str("schedule")? {
            None | Some("once") => WireRun::Once,
            Some("loop") => WireRun::Loop(r.opt_count("count")?.ok_or_else(|| {
                bad(
                    &r.at("count"),
                    "a loop schedule gives its \"count\" of plays",
                )
            })?),
            Some("duration") => WireRun::Duration(r.opt_f64("duration_s")?.ok_or_else(|| {
                bad(
                    &r.at("duration_s"),
                    "a duration schedule gives its \"duration_s\", seconds",
                )
            })?),
            Some(other) => {
                return Err(bad(
                    &r.at("schedule"),
                    format!(
                        "\"{}\" is \"once\", \"loop\" or \"duration\", not \"{other}\"",
                        r.at("schedule")
                    ),
                ))
            }
        },
    };

    let during_playback = match c.opt_obj("adaptation")? {
        None => false,
        Some(a) => a.opt_bool("during_playback")?.unwrap_or(false),
    };

    let tl = TwrLimits::default();
    let lim = c.obj("limits")?;
    let drive_vpk_ceiling = lim.f64("drive_vpk_ceiling")?;
    let drive_clip_vpk = lim.opt_f64("drive_clip_vpk")?;
    let drive_vrms_limit = lim.opt_f64("drive_vrms_limit")?;
    let response_rms_limit = lim.opt_f64("response_rms_limit")?;
    let envelope_alarm_frac = lim.f64_or("envelope_alarm_frac", tl.envelope_alarm_frac)?;
    let envelope_abort_frac = lim.f64_or("envelope_abort_frac", tl.envelope_abort_frac)?;
    let envelope_debounce_samples = count(
        &lim,
        "envelope_debounce_samples",
        tl.envelope_debounce_samples as u64,
    )?;
    let divergence_plays = count(&lim, "divergence_plays", tl.divergence_n as u64)?;
    let divergence_rise_frac = lim.f64_or("divergence_rise_frac", tl.divergence_tol_frac)?;

    // Every run is recorded in its run folder (`record`); the module's own
    // capture is gone, and a configuration that still asks for it is told so
    // rather than left to believe it is capturing.
    if !matches!(c.m.get("capture"), None | Some(Value::Null)) {
        return Err(bad(
            "config.capture",
            "A TWR run is always recorded in its run folder; \"capture\" is no longer taken. \
             Use \"record\" to keep or leave out the streams.",
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
        None => default_metadata(),
        Some(list) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, m) in list.iter().enumerate() {
                let o = Obj::of(m, &format!("config.metadata[{i}]"))?;
                let text = |k: &str| -> Result<String, MethodError> {
                    o.opt_str(k)?
                        .map(str::to_owned)
                        .ok_or_else(|| bad(&o.at(k), format!("\"{}\" is text", o.at(k))))
                };
                let key = text("key")?;
                let label = o.opt_str("label")?.unwrap_or(&key).to_owned();
                out.push(MetaField {
                    key,
                    label,
                    value: text("value")?,
                });
            }
            out
        }
    };

    Ok(WireTwr {
        title,
        shape,
        excitation,
        sysid_drive_vrms: drive_vrms,
        sysid_duration_s: duration_s,
        nfft,
        sysid_band_hz: band,
        min_mean_coherence,
        review,
        tikhonov_floor,
        coherence_floor,
        law,
        law_budget,
        law_misses,
        max_peak_rise_db,
        max_bin_rise_db,
        first_drive_ratio,
        alignment_frames,
        mode,
        levels_db,
        gate_pct,
        max_iterations,
        on_gate_fail,
        run,
        during_playback,
        drive_vpk_ceiling,
        drive_clip_vpk,
        drive_vrms_limit,
        response_rms_limit,
        envelope_alarm_frac,
        envelope_abort_frac,
        envelope_debounce_samples,
        divergence_plays,
        divergence_rise_frac,
        metadata,
        operator,
        article,
        notes,
        specification,
        record_streams,
        raw: config.clone(),
    })
}

fn meta(key: &str, label: &str, value: &str) -> MetaField {
    MetaField {
        key: key.into(),
        label: label.into(),
        value: value.into(),
    }
}

/// The core's own metadata set: Run Number, Test Article, SN, Operator, empty.
pub fn default_metadata() -> Vec<MetaField> {
    let r = hertzforge_twr::ReferenceWaveform::from_samples("m", 1.0, vec![0.0]);
    match r {
        Ok(r) => TwrConfig::for_reference(r).metadata,
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------------
// Checking: every problem is a named item.
// ---------------------------------------------------------------------------------

/// A configuration field in words, for a message a person reads.
pub fn field_words(field: &str) -> String {
    let last = field.rsplit('.').next().unwrap_or(field);
    let last = last.split('[').next().unwrap_or(last);
    let parent = field.rsplit('.').nth(1).unwrap_or("");
    let words = match (parent, last) {
        ("prep", "highpass_hz") => "The drift high-pass",
        ("prep", "scale") => "The reference's scale",
        ("prep", "taper_frac") => "The end taper",
        ("section", "len") => "The section's length",
        ("section", "start") => "The section's start",
        ("pads", "pre") => "The lead-in pad",
        ("pads", "post") => "The lead-out pad",
        ("acquisition", "fs_hz") => "The sample rate",
        ("identification", "drive_vrms") => "The identification's drive",
        ("identification", "duration_s") => "The identification's length",
        ("identification", "nfft") => "The identification's block",
        ("identification", "band_hz") => "The identification's band",
        ("identification", "min_mean_coherence") => "The identification's coherence hold",
        ("inverse", "tikhonov_floor") => "The inverse's floor",
        ("inverse", "coherence_floor") => "The coherence the inverse is trusted above",
        ("correction", "band_hz") => "The correction band",
        ("correction", "ref_floor_db") => "The reference floor",
        ("correction", "min_band_energy") => "The trusted bins' coverage floor",
        ("control", "law_budget") => "The law's time budget",
        ("control", "law_misses") => "The unusable calls allowed in a row",
        ("control", "max_peak_rise_db") => "The cap on the drive's peak rise",
        ("control", "max_bin_rise_db") => "The cap on a drive bin's rise",
        ("control", "first_drive_ratio") => "The first drive's bound",
        ("control", "alignment_frames") => "The alignment drift allowed",
        ("schedule", "gate_pct") => "The gate",
        ("schedule", "max_iterations") => "The iterations allowed at a level",
        ("schedule", "levels_db") => "A level",
        ("run", "count") => "The loop's plays",
        ("run", "duration_s") => "The playback's length",
        ("limits", "drive_vpk_ceiling") => "The drive ceiling",
        ("limits", "drive_clip_vpk") => "The drive clip",
        ("limits", "drive_vrms_limit") => "The drive rms limit",
        ("limits", "response_rms_limit") => "The response rms limit",
        ("limits", "envelope_alarm_frac") => "The alarm band",
        ("limits", "envelope_abort_frac") => "The abort band",
        ("limits", "envelope_debounce_samples") => "The envelope debounce",
        ("limits", "divergence_plays") => "The divergence count",
        ("limits", "divergence_rise_frac") => "The divergence rise",
        ("feasibility", "peak_accel") => "The peak acceleration ceiling",
        ("feasibility", "peak_velocity_ips") => "The peak velocity ceiling",
        ("feasibility", "disp_pkpk_in") => "The displacement ceiling",
        ("feasibility", "kinematics_hp_hz") => "The kinematics high-pass",
        _ => return format!("\"{field}\""),
    };
    words.to_owned()
}

fn out_of_range(r: &mut Report, field: &str, value: f64, ok: bool, text: &str) {
    if !ok {
        r.errors.push(Item::field(
            "out_of_range",
            field,
            format!("{} is {}; {text}", field_words(field), num(value)),
        ));
    }
}

fn positive(r: &mut Report, field: &str, x: f64) {
    out_of_range(r, field, x, x.is_finite() && x > 0.0, "it is above 0.");
}

fn band_words(b: (f64, f64)) -> String {
    format!("{} to {} Hz", num(b.0), num(b.1))
}

impl WireShape {
    /// The rate the run asks the station for, for a reference at `reference_fs`.
    pub fn fs_for(&self, reference_fs: f64) -> f64 {
        self.fs_hz.unwrap_or(reference_fs)
    }

    /// The correction band, the identification's when none is given.
    pub fn band(&self, sysid: (f64, f64)) -> (f64, f64) {
        self.correction.band_hz.unwrap_or(sysid)
    }

    /// Check the parts a preview shares and prepare the record: the items, and the
    /// prepared record when there are no errors. `sysid` is the identification's
    /// band (a preview's is the default one).
    pub fn check(
        &self,
        r: &mut Report,
        ctx_ref: &dyn Fn(&str) -> Option<Arc<Stored>>,
        file_check: &dyn Fn(&Stored, &str) -> Result<(), String>,
        sysid: (f64, f64),
    ) -> Option<(Arc<Stored>, Prepared, PreviewDerived)> {
        let before = r.errors.len();
        let stored = ctx_ref(&self.sha256);
        let Some(stored) = stored else {
            r.errors.push(Item::field(
                "reference_unknown",
                "reference.sha256",
                format!(
                    "No stored reference has SHA-256 {}; import it with twr.reference.import \
                     first.",
                    &self.sha256[..12.min(self.sha256.len())]
                ),
            ));
            return None;
        };
        if let Some(path) = &self.path {
            if let Err(why) = file_check(&stored, path) {
                r.errors
                    .push(Item::field("reference_changed", "reference.path", why));
            }
        }
        let p = &self.prep;
        if let Some(hp) = p.highpass_hz {
            positive(r, "prep.highpass_hz", hp);
        }
        out_of_range(
            r,
            "prep.scale",
            p.scale,
            p.scale.is_finite() && p.scale != 0.0,
            "it is a finite number, not 0.",
        );
        if let Some(t) = p.taper_frac {
            out_of_range(
                r,
                "prep.taper_frac",
                t,
                t > 0.0 && t <= 0.5,
                "it is above 0 and at most 0.5.",
            );
        }
        let fs = self.fs_for(stored.fs_hz);
        if let Some(f) = self.fs_hz {
            out_of_range(
                r,
                "acquisition.fs_hz",
                f,
                (FS_RANGE.0..=FS_RANGE.1).contains(&f),
                &format!(
                    "it runs from {} to {} Hz.",
                    num(FS_RANGE.0),
                    num(FS_RANGE.1)
                ),
            );
        }
        if (stored.fs_hz - fs).abs() > 1e-9 {
            if !p.auto_resample {
                r.errors.push(Item::field(
                    "rate_mismatch",
                    "prep.auto_resample",
                    format!(
                        "The reference is at {} Hz and the run at {} Hz, and resampling is off; \
                         turn it on or run at the reference's rate.",
                        num(sig(stored.fs_hz, 9)),
                        num(sig(fs, 9))
                    ),
                ));
            } else if stored.fs_hz > fs {
                r.errors.push(Item::field(
                    "rate_mismatch",
                    "acquisition.fs_hz",
                    format!(
                        "The reference is at {} Hz, above the run's {} Hz; it is resampled \
                         upward only. Run at or above the reference's rate.",
                        num(sig(stored.fs_hz, 9)),
                        num(sig(fs, 9))
                    ),
                ));
            }
        }
        let band = self.band(sysid);
        let c = &self.correction;
        let band_field = if c.band_hz.is_some() {
            "correction.band_hz"
        } else {
            "identification.band_hz"
        };
        let band_ok = band.0 > 0.0 && band.1 > band.0;
        if !band_ok {
            r.errors.push(Item::field(
                "out_of_range",
                band_field,
                format!(
                    "The correction band is {}; its low edge is above 0 and under its high edge.",
                    band_words(band)
                ),
            ));
        } else if band.1 >= fs / 2.0 {
            r.errors.push(Item::field(
                "band_above_grid",
                band_field,
                format!(
                    "The correction band reaches {} Hz, at or above half the {} Hz rate.",
                    num(band.1),
                    num(sig(fs, 9))
                ),
            ));
        }
        if let Some(hp) = p.highpass_hz {
            if hp > 0.0 && band_ok && hp > band.0 {
                r.errors.push(Item::field(
                    "highpass_above_band",
                    "prep.highpass_hz",
                    format!(
                        "The drift high-pass at {} Hz is above the correction band's low edge \
                         ({} Hz); it would take out content the run is asked to replicate.",
                        num(hp),
                        num(band.0)
                    ),
                ));
            }
        }
        out_of_range(
            r,
            "correction.ref_floor_db",
            c.ref_floor_db,
            c.ref_floor_db > 0.0 && c.ref_floor_db <= 200.0,
            "it is above 0 and at most 200 dB.",
        );
        out_of_range(
            r,
            "correction.min_band_energy",
            c.min_band_energy,
            c.min_band_energy > 0.0 && c.min_band_energy < 1.0,
            "it is above 0 and below 1.",
        );
        let f = &self.feasibility;
        for (k, v) in [
            ("feasibility.peak_accel", f.peak_accel),
            ("feasibility.peak_velocity_ips", f.peak_velocity_ips),
            ("feasibility.disp_pkpk_in", f.disp_pkpk_in),
        ] {
            if let Some(x) = v {
                positive(r, k, x);
            }
        }
        positive(r, "feasibility.kinematics_hp_hz", f.kinematics_hp_hz);
        if let Some((_, len)) = self.section {
            out_of_range(
                r,
                "section.len",
                len as f64,
                len >= 2,
                "a section is at least 2 samples.",
            );
        }
        if r.errors.len() > before {
            return None;
        }
        match reference::prepare(&stored, self, fs, band) {
            Err(item) => {
                r.errors.push(item);
                None
            }
            Ok(prepared) => {
                if prepared.grid.n > MAX_RECORD_LEN {
                    r.errors.push(Item::field(
                        "record_too_long",
                        "section.len",
                        format!(
                            "The section and its pads make a record of {} samples; the most a \
                             run takes is {MAX_RECORD_LEN}.",
                            prepared.grid.n
                        ),
                    ));
                    return None;
                }
                let derived = prepared.derived(self);
                Some((stored, prepared, derived))
            }
        }
    }
}

impl WireTwr {
    /// The table's first enabled control row, and its unit.
    pub fn control_row(table: &ChannelTable) -> Option<(u32, String)> {
        table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Control)
            .map(|c| (c.index, table::unit_name(&c.units)))
    }

    /// The table's first enabled drive row.
    pub fn drive_row(table: &ChannelTable) -> Option<u32> {
        table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Drive)
            .map(|c| c.index)
    }

    /// The built-in law's number parameter `key`: given, else its declared
    /// default, else `default`.
    pub fn builtin_number(&self, facts: &LawFacts, key: &str, default: f64) -> f64 {
        if let Some(v) = self.law.params.get(key).and_then(Value::as_f64) {
            return v;
        }
        facts
            .params
            .iter()
            .find(|p| p.key == key)
            .and_then(|p| match &p.kind {
                hertzforge_law_host::ParamKind::Number(s) => Some(s.default),
                _ => None,
            })
            .unwrap_or(default)
    }

    /// Whether `facts` is the built-in law.
    pub fn is_builtin(facts: &LawFacts) -> bool {
        facts.builtin && facts.id == TWR_CORRECT_ID
    }

    pub fn law_choice(&self, facts: &LawFacts) -> hertzforge_random_laws::LawChoice {
        hertzforge_random_laws::LawChoice::new(
            self.law.id.clone(),
            crate::mimo::adapter::params_of(&facts.params, &self.law.params),
        )
    }

    pub fn control(&self) -> TwrControl {
        TwrControl {
            guards: LawGuards {
                budget_fraction: self.law_budget,
                max_misses: self.law_misses.min(u64::from(u32::MAX)) as u32,
                ..LawGuards::default()
            },
            max_peak_rise_db: self.max_peak_rise_db,
            max_bin_rise_db: self.max_bin_rise_db,
            first_drive_ratio: self.first_drive_ratio,
            alignment_frames: self.alignment_frames,
            rest_taper: true,
        }
    }

    pub fn limits(&self) -> TwrLimits {
        TwrLimits {
            envelope_alarm_frac: self.envelope_alarm_frac,
            envelope_abort_frac: self.envelope_abort_frac,
            envelope_debounce_samples: self.envelope_debounce_samples as usize,
            response_rms_limit: self.response_rms_limit,
            drive_clip: self.drive_clip_vpk,
            drive_rms_limit: self.drive_vrms_limit,
            max_drive_vpk: Some(self.drive_vpk_ceiling),
            divergence_n: self.divergence_plays as usize,
            divergence_tol_frac: self.divergence_rise_frac,
            feasibility: self.shape.feasibility.ceilings(),
        }
    }

    /// The identification's length, seconds.
    pub fn identification_s(&self) -> f64 {
        self.sysid_duration_s
    }

    /// The metadata the controller's configuration carries: the configuration's
    /// own, with the operator's words for the record written into it, so the
    /// controller's report carries them too.
    pub fn record_metadata(&self) -> Vec<MetaField> {
        let mut out = self.metadata.clone();
        for field in out.iter_mut() {
            match field.key.as_str() {
                "operator" if !self.operator.is_empty() => field.value = self.operator.clone(),
                "test_article" if !self.article.is_empty() => field.value = self.article.clone(),
                _ => {}
            }
        }
        if !self.notes.is_empty() {
            out.push(meta("notes", "Notes", &self.notes));
        }
        if let Some((document, clause)) = &self.specification {
            out.push(meta("specification", "Specification", document));
            out.push(meta("clause", "Clause", clause));
        }
        out
    }

    /// The controller's configuration, every field supplied (no `..`: a field added
    /// to `TwrConfig` stops this compiling until it is given a meaning here). The
    /// reference is the stored record, raw: the core prepares it. For the built-in
    /// law its gain, clamp and backoff are written into the law configuration; a
    /// law file's parameters reach the law through its choice.
    pub fn build(&self, stored: &Stored, facts: &LawFacts) -> TwrConfig {
        let band = self.shape.band(self.sysid_band_hz);
        let builtin = Self::is_builtin(facts);
        let ld = LawConfig::default();
        let (beta, clamp, backoff) = if builtin {
            (
                self.builtin_number(facts, BUILTIN_BETA.0, BUILTIN_BETA.1),
                self.builtin_number(facts, BUILTIN_CLAMP.0, BUILTIN_CLAMP.1),
                self.builtin_number(facts, BUILTIN_BACKOFF.0, BUILTIN_BACKOFF.1),
            )
        } else {
            (ld.beta, ld.max_db_per_iter, ld.initial_drive_backoff_db)
        };
        TwrConfig {
            reference: stored.waveform(),
            prep: self.shape.prep.core(),
            section: self
                .shape
                .section
                .map(|(start, len)| Section { start, len }),
            pad_pre: self.shape.pads.0,
            pad_post: self.shape.pads.1,
            sysid: SysIdConfig {
                excitation: self.excitation,
                drive_rms: self.sysid_drive_vrms,
                duration_s: self.sysid_duration_s,
                nfft: self.nfft as usize,
                f_lo_hz: self.sysid_band_hz.0,
                f_hi_hz: self.sysid_band_hz.1,
                min_mean_coherence: self.min_mean_coherence,
            },
            inverse: InverseConfig {
                tikhonov_beta_floor: self.tikhonov_floor,
                coherence_floor: self.coherence_floor,
            },
            law: LawConfig {
                beta,
                max_db_per_iter: clamp,
                ref_floor_db: self.shape.correction.ref_floor_db,
                f_lo_hz: band.0,
                f_hi_hz: band.1,
                initial_drive_backoff_db: backoff,
                min_masked_energy_frac: self.shape.correction.min_band_energy,
            },
            schedule: TwrLevelSchedule {
                mode: self.mode,
                table_db: self.levels_db.clone(),
                gate_rms_error_pct: self.gate_pct,
                max_iterations_per_level: self.max_iterations.min(u64::from(u32::MAX)) as u32,
                on_gate_fail: self.on_gate_fail,
            },
            run: self.run.core(),
            adapt_during_playback: self.during_playback,
            limits: self.limits(),
            kinematics_hp_hz: self.shape.feasibility.kinematics_hp_hz,
            review_after_sysid: self.review,
            // The run's record is the one way a TWR run records: the
            // controller's own capture stays off.
            capture: CaptureRules {
                auto_start: false,
                ..CaptureRules::default()
            },
            metadata: self.record_metadata(),
            control: Some(self.control()),
        }
    }

    /// Check the configuration against `ctx`: the items, and the derived values
    /// when there are no errors.
    pub fn check(&self, ctx: &CheckContext<'_>) -> (Report, Option<Derived>, Option<Arc<Stored>>) {
        let mut r = Report::default();
        let shaped = self
            .shape
            .check(&mut r, ctx.reference, ctx.file_check, self.sysid_band_hz);
        if let Some((stored, _, _)) = &shaped {
            if let Some((_, unit)) = Self::control_row(ctx.table) {
                if unit != stored.units {
                    r.errors.push(Item::field(
                        "units_mismatch",
                        "reference.sha256",
                        format!(
                            "The reference is in {} and the control row in {unit}; import it in \
                             the control row's unit.",
                            stored.units
                        ),
                    ));
                }
            }
        }
        let fs = shaped
            .as_ref()
            .map(|(_, p, _)| p.grid.fs_hz)
            .or(self.shape.fs_hz);

        // The identification.
        self.check_identification(&mut r, fs);
        // The inverse.
        out_of_range(
            &mut r,
            "inverse.tikhonov_floor",
            self.tikhonov_floor,
            self.tikhonov_floor > 0.0 && self.tikhonov_floor < 1.0,
            "it is above 0 and below 1.",
        );
        out_of_range(
            &mut r,
            "inverse.coherence_floor",
            self.coherence_floor,
            (0.0..=1.0).contains(&self.coherence_floor),
            "it runs from 0 to 1.",
        );
        // The correction band inside the identification's.
        if let Some(b) = self.shape.correction.band_hz {
            let s = self.sysid_band_hz;
            if b.0 > 0.0 && b.1 > b.0 && (b.0 < s.0 || b.1 > s.1) {
                r.errors.push(Item::field(
                    "band_outside_identification",
                    "correction.band_hz",
                    format!(
                        "The correction band ({}) leaves the identification's ({}); the inverse \
                         is not known outside the band identified.",
                        band_words(b),
                        band_words(s)
                    ),
                ));
            }
        }

        // The law.
        let facts = (ctx.law)(&self.law.id);
        self.check_law(&mut r, facts.as_ref());
        self.check_control(&mut r, facts.as_ref());
        self.check_schedule(&mut r);
        self.check_limits(&mut r);

        // The table.
        if Self::control_row(ctx.table).is_none() {
            r.errors.push(Item::field(
                "no_control_channel",
                "control",
                "The channel table has no enabled control row; the run replicates the reference \
                 at the first one.",
            ));
        }
        if Self::drive_row(ctx.table).is_none() {
            r.errors.push(Item::field(
                "no_drive_channel",
                "control",
                "The channel table has no enabled drive row; the run plays its drive on the \
                 first one.",
            ));
        }

        // The identification's drive under the ceiling and the clip.
        if let Some(fs) = fs {
            self.check_identification_drive(&mut r, fs);
        }

        if let Some((stored, _, preview)) = &shaped {
            if preview.grid.fs_hz != stored.fs_hz && r.errors.is_empty() {
                r.warnings.push(Item::field(
                    "reference_resampled",
                    "acquisition.fs_hz",
                    format!(
                        "The reference is at {} Hz and is resampled to the run's {} Hz.",
                        num(sig(stored.fs_hz, 9)),
                        num(sig(preview.grid.fs_hz, 9))
                    ),
                ));
            }
        }

        if !r.errors.is_empty() {
            return (r, None, shaped.map(|(s, _, _)| s));
        }
        let (Some((stored, _, preview)), Some(facts)) = (shaped, facts) else {
            return (r, None, None);
        };
        let section_s = preview.section_s;
        let record_s = preview.grid.record_s();
        let derived = Derived {
            sha256: stored.sha256.clone(),
            reference_name: stored.name.clone(),
            reference_fs_hz: stored.fs_hz,
            units: stored.units.clone(),
            resampled: preview.grid.fs_hz != stored.fs_hz,
            call_budget_s: self.control().call_budget_s(record_s),
            plays_target: self.run.core().plays_target(section_s),
            identification_s: self.identification_s(),
            law: LawRef::of(&facts),
            preview,
        };
        (r, Some(derived), Some(stored))
    }

    fn check_identification(&self, r: &mut Report, fs: Option<f64>) {
        positive(r, "identification.drive_vrms", self.sysid_drive_vrms);
        positive(r, "identification.duration_s", self.sysid_duration_s);
        out_of_range(
            r,
            "identification.nfft",
            self.nfft as f64,
            self.nfft >= 64 && self.nfft.is_power_of_two() && self.nfft <= 1 << 20,
            "it is a power of two from 64 to 1048576.",
        );
        let b = self.sysid_band_hz;
        if !(b.0 > 0.0 && b.1 > b.0) {
            r.errors.push(Item::field(
                "out_of_range",
                "identification.band_hz",
                format!(
                    "The identification's band is {}; its low edge is above 0 and under its high \
                     edge.",
                    band_words(b)
                ),
            ));
        } else if let Some(fs) = fs {
            if b.1 >= fs / 2.0 {
                r.errors.push(Item::field(
                    "band_above_grid",
                    "identification.band_hz",
                    format!(
                        "The identification's band reaches {} Hz, at or above half the {} Hz \
                         rate.",
                        num(b.1),
                        num(sig(fs, 9))
                    ),
                ));
            }
        }
        out_of_range(
            r,
            "identification.min_mean_coherence",
            self.min_mean_coherence,
            self.min_mean_coherence > 0.0 && self.min_mean_coherence <= 1.0,
            "it is above 0 and at most 1.",
        );
        if let Some(fs) = fs {
            let n = (self.sysid_duration_s * fs) as u64;
            if self.sysid_duration_s > 0.0 && self.nfft >= 64 && n < 2 * self.nfft {
                r.errors.push(Item::field(
                    "identification_too_short",
                    "identification.duration_s",
                    format!(
                        "The identification is {} samples at {} Hz; it needs at least two blocks \
                         of {} for its coherence.",
                        n,
                        num(sig(fs, 9)),
                        self.nfft
                    ),
                ));
            }
        }
    }

    fn check_identification_drive(&self, r: &mut Report, fs: f64) {
        if !(self.sysid_drive_vrms > 0.0 && self.sysid_duration_s > 0.0) {
            return;
        }
        if self.sysid_duration_s * fs > 1e8 || !fs.is_finite() {
            return;
        }
        // The excitation is built only from a block the identification can use:
        // an unusable one is its own item, and building with it would not return.
        let n = (self.sysid_duration_s * fs) as u64;
        if !(self.nfft >= 64 && self.nfft.is_power_of_two() && self.nfft <= 1 << 20)
            || n < 2 * self.nfft
        {
            return;
        }
        let b = self.sysid_band_hz;
        if !(b.0 > 0.0 && b.1 > b.0 && b.1 < fs / 2.0) {
            return;
        }
        let sysid = SysIdConfig {
            excitation: self.excitation,
            drive_rms: self.sysid_drive_vrms,
            duration_s: self.sysid_duration_s,
            nfft: self.nfft as usize,
            f_lo_hz: b.0,
            f_hi_hz: b.1,
            min_mean_coherence: self.min_mean_coherence,
        };
        let x = sysid.build_excitation(fs);
        let peak = x.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
        let bound = match self.drive_clip_vpk {
            Some(c) if c > 0.0 => self.drive_vpk_ceiling.min(c),
            _ => self.drive_vpk_ceiling,
        };
        if self.drive_vpk_ceiling > 0.0 && peak > bound {
            r.errors.push(Item::field(
                "out_of_range",
                "identification.drive_vrms",
                format!(
                    "The identification's drive peaks at {} V, over the drive ceiling{} ({} V); \
                     lower its rms.",
                    num(sig(peak, 3)),
                    if bound < self.drive_vpk_ceiling {
                        " or the clip"
                    } else {
                        ""
                    },
                    num(bound)
                ),
            ));
        }
    }

    fn check_law(&self, r: &mut Report, facts: Option<&LawFacts>) {
        match facts {
            None => r.errors.push(Item::field(
                "law_unknown",
                "control.law.id",
                format!(
                    "No control law \"{}\" is in the catalog; laws.list names the ones there are.",
                    self.law.id
                ),
            )),
            Some(f) => {
                if let Err(test) = f.world {
                    r.errors.push(Item::field(
                        "law_wrong_world",
                        "control.law.id",
                        format!(
                            "{} is a law for {test}, not for TWR; choose a TWR law.",
                            f.name
                        ),
                    ));
                }
                for (k, v) in &self.law.params {
                    let field = format!("control.law.params.{k}");
                    match f.params.iter().find(|p| &p.key == k) {
                        None => r.errors.push(Item::field(
                            "law_param_unknown",
                            field,
                            format!("{} declares no parameter \"{k}\".", f.name),
                        )),
                        Some(spec) => {
                            if let Some(why) = param_problem(spec, v) {
                                r.errors.push(Item::field(
                                    "law_param_invalid",
                                    field,
                                    format!("{}'s {} {why}", f.name, spec.label),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    fn check_control(&self, r: &mut Report, facts: Option<&LawFacts>) {
        out_of_range(
            r,
            "control.law_budget",
            self.law_budget,
            self.law_budget > 0.0 && self.law_budget <= 1.0,
            "it is above 0 and at most 1.",
        );
        out_of_range(
            r,
            "control.law_misses",
            self.law_misses as f64,
            (1..=100).contains(&self.law_misses),
            "it runs from 1 to 100.",
        );
        for (k, v) in [
            ("control.max_peak_rise_db", self.max_peak_rise_db),
            ("control.max_bin_rise_db", self.max_bin_rise_db),
        ] {
            out_of_range(
                r,
                k,
                v,
                v > 0.0 && v <= MAX_CAP_DB,
                &format!("it is above 0 and at most {} dB.", num(MAX_CAP_DB)),
            );
        }
        out_of_range(
            r,
            "control.first_drive_ratio",
            self.first_drive_ratio,
            (1.0..=10.0).contains(&self.first_drive_ratio),
            "it runs from 1 to 10.",
        );
        out_of_range(
            r,
            "control.alignment_frames",
            self.alignment_frames,
            self.alignment_frames > 0.0 && self.alignment_frames <= MAX_ALIGNMENT_FRAMES,
            &format!(
                "it is above 0 and at most {} frames.",
                num(MAX_ALIGNMENT_FRAMES)
            ),
        );
        // The caps are never tighter than the built-in's own clamp plus a margin.
        if let Some(f) = facts.filter(|f| Self::is_builtin(f)) {
            let clamp = self.builtin_number(f, BUILTIN_CLAMP.0, BUILTIN_CLAMP.1);
            let floor = clamp + CAP_MARGIN_OVER_CLAMP_DB;
            for (k, v) in [
                ("control.max_peak_rise_db", self.max_peak_rise_db),
                ("control.max_bin_rise_db", self.max_bin_rise_db),
            ] {
                if v > 0.0 && v <= MAX_CAP_DB && v < floor {
                    r.errors.push(Item::field(
                        "out_of_range",
                        k,
                        format!(
                            "{} is {} dB; with {} it is at least its largest rise per play \
                             ({} dB) plus {} dB.",
                            field_words(k),
                            num(v),
                            f.name,
                            num(clamp),
                            num(CAP_MARGIN_OVER_CLAMP_DB)
                        ),
                    ));
                }
            }
        }
    }

    fn check_schedule(&self, r: &mut Report) {
        if self.levels_db.is_empty() {
            r.errors.push(Item::field(
                "levels_empty",
                "schedule.levels_db",
                "The level table is empty; its last level is full level.",
            ));
        }
        for (i, l) in self.levels_db.iter().enumerate() {
            out_of_range(
                r,
                &format!("schedule.levels_db[{i}]"),
                *l,
                (LEVEL_DB_RANGE.0..=LEVEL_DB_RANGE.1).contains(l),
                &format!(
                    "a level runs from {} to {} dB.",
                    num(LEVEL_DB_RANGE.0),
                    num(LEVEL_DB_RANGE.1)
                ),
            );
        }
        if let Some(i) = self.levels_db.windows(2).position(|w| w[1] < w[0]) {
            r.errors.push(Item::field(
                "levels_not_ascending",
                format!("schedule.levels_db[{}]", i + 1),
                format!(
                    "The level table goes down from {} to {} dB; it rises to full level.",
                    num(self.levels_db[i]),
                    num(self.levels_db[i + 1])
                ),
            ));
        }
        out_of_range(
            r,
            "schedule.gate_pct",
            self.gate_pct,
            self.gate_pct > 0.0 && self.gate_pct <= 100.0,
            "it is above 0 and at most 100 percent.",
        );
        out_of_range(
            r,
            "schedule.max_iterations",
            self.max_iterations as f64,
            (1..=100).contains(&self.max_iterations),
            "it runs from 1 to 100.",
        );
        match self.run {
            WireRun::Once => {}
            WireRun::Loop(n) => out_of_range(
                r,
                "run.count",
                n as f64,
                (1..=MAX_LOOP_COUNT).contains(&n),
                &format!("it runs from 1 to {MAX_LOOP_COUNT}."),
            ),
            WireRun::Duration(s) => out_of_range(
                r,
                "run.duration_s",
                s,
                s > 0.0 && s <= MAX_DURATION_S,
                &format!("it is above 0 and at most {} s.", num(MAX_DURATION_S)),
            ),
        }
    }

    fn check_limits(&self, r: &mut Report) {
        out_of_range(
            r,
            "limits.drive_vpk_ceiling",
            self.drive_vpk_ceiling,
            self.drive_vpk_ceiling > 0.0 && self.drive_vpk_ceiling <= MAX_DRIVE_V,
            &format!("it is above 0 and at most {} V.", num(MAX_DRIVE_V)),
        );
        if let Some(c) = self.drive_clip_vpk {
            out_of_range(
                r,
                "limits.drive_clip_vpk",
                c,
                c > 0.0 && c <= MAX_DRIVE_V,
                &format!("it is above 0 and at most {} V.", num(MAX_DRIVE_V)),
            );
        }
        if let Some(x) = self.drive_vrms_limit {
            positive(r, "limits.drive_vrms_limit", x);
        }
        if let Some(x) = self.response_rms_limit {
            positive(r, "limits.response_rms_limit", x);
        }
        positive(r, "limits.envelope_alarm_frac", self.envelope_alarm_frac);
        positive(r, "limits.envelope_abort_frac", self.envelope_abort_frac);
        if self.envelope_alarm_frac > 0.0
            && self.envelope_abort_frac > 0.0
            && self.envelope_alarm_frac > self.envelope_abort_frac
        {
            r.errors.push(Item::field(
                "alarm_wider_than_abort",
                "limits.envelope_alarm_frac",
                format!(
                    "The alarm band ({}) is wider than the abort band ({}); the alarm comes first.",
                    num(self.envelope_alarm_frac),
                    num(self.envelope_abort_frac)
                ),
            ));
        }
        out_of_range(
            r,
            "limits.envelope_debounce_samples",
            self.envelope_debounce_samples as f64,
            (1..=10_000).contains(&self.envelope_debounce_samples),
            "it runs from 1 to 10000.",
        );
        out_of_range(
            r,
            "limits.divergence_plays",
            self.divergence_plays as f64,
            (1..=100).contains(&self.divergence_plays),
            "it runs from 1 to 100.",
        );
        out_of_range(
            r,
            "limits.divergence_rise_frac",
            self.divergence_rise_frac,
            self.divergence_rise_frac.is_finite() && self.divergence_rise_frac >= 0.0,
            "it is 0 or more.",
        );
    }
}

/// The refusal of a configuration with errors.
pub fn config_invalid(report: &Report) -> MethodError {
    crate::shock::config::config_invalid(report)
}

/// The refusal of a preview whose parts have errors: the same items.
pub fn preview_invalid(report: &Report) -> MethodError {
    MethodError::named(
        ErrorCode::BadRequest,
        "config_invalid",
        format!(
            "The preview has {} error{}.",
            report.errors.len(),
            if report.errors.len() == 1 { "" } else { "s" }
        ),
    )
    .with_detail(report.to_json())
}
