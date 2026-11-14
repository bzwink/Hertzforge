//! The Mixed Mode configuration (schema/PROTOCOL.md, "Mixed Mode Configuration").
//!
//! The wire configuration is parsed (a wrong type or a missing part is `bad_params`
//! naming it as the request's own path), checked so every problem comes back as a
//! named item, and converted to the controller's [`MixedConfig`]. This file is the
//! one place the wire's shapes meet the core's: the wire flattens a part's body
//! into the part and states schedules in Sine's shapes (`freq_hz`, `band_hz`, a
//! `rate` object, `end`); the core nests `body` and names them `hz`,
//! `lo_hz`/`hi_hz`, `target`. [`WireMixed::to_core`] goes one way and
//! [`to_wire`] the other.
//!
//! The floor, the acquisition, the ladder, the control set and the limit rows are
//! Random's on the wire, so they are parsed and checked by Random's own code
//! ([`crate::random::config`]) on a Random configuration made of those parts, and
//! its findings are relocated to where they sit in a Mixed Mode configuration.
//! Nothing here is control math: the run is the controller's.

use crate::protocol::{num, ErrorCode, MethodError};
use crate::random::config::{self as rcfg, WireConfig as RandomWire};
use crate::shock::config::Obj;
use crate::sine::profile::{check_profile, parse_profile, Locate};
use crate::table::{At, Item, Report};
use hertzforge_breakpoints::LevelSpec;
use hertzforge_channel_table::ChannelTable;
use hertzforge_dsp::sweep::SweepRate;
use hertzforge_law_host::ParamSpec;
use hertzforge_mixed::{
    BandWidth, ConfigError, Direction, FloorPart, LawPick, MixedConfig, MixedIdentification,
    MixedLawChoice, MixedLawGuards, MixedLimits, NarrowbandPart, Part, PartBody, PartSchedule,
    PartSweepTarget, PartWindow, SweepLaw, ToneAmplitude, TonePart, MAX_BANDS, MAX_TONES,
    MIN_TONE_SPACING_WIDTHS,
};
use hertzforge_random::service::config::{CaptureRules, MetaField};
use hertzforge_sine::law::{MAX_STEP_DB_RANGE, MIN_TICK_S_BUILTIN, MIN_TICK_S_LAW_FILE};
use hertzforge_sine::profile::SineProfile;
use hertzforge_sine::service::config::{assess_tracking_lag, TrackingLagVerdict};
use hertzforge_sine::BandwidthMode;
use serde_json::{json, Map, Value};

/// The control tick when none is sent, ms (Sine's).
pub const DEFAULT_TICK_MS: f64 = 10.0;
/// The longest tick, ms.
pub const MAX_TICK_MS: f64 = 1000.0;
/// The widest exclusion around a dwelling tone, Hz each side.
pub const MAX_EXCLUSION_HZ: f64 = 50.0;
/// The widest tracking filter, Hz.
pub const MAX_TRACKING_HZ: f64 = 10.0;
/// The longest part ramp, s.
pub const MAX_PART_RAMP_S: f64 = 60.0;
/// The longest the floor plays alone, s.
pub const MAX_FLOOR_SETTLE_S: f64 = 600.0;
/// The largest drive ceiling or abort, V peak.
pub const MAX_DRIVE_VPK: f64 = 10.0;
/// The longest part id.
pub const MAX_ID_LEN: usize = 32;
/// The default tone fault debounce, ticks (Sine's).
pub const DEFAULT_FAULT_DEBOUNCE_TICKS: u64 = 50;

/// A tone's or a narrowband's schedule as sent.
#[derive(Debug, Clone, PartialEq)]
pub enum WireSchedule {
    /// `fixed`: `freq_hz`, `end` `{seconds}` or `{cycles}` or null.
    Fixed { freq_hz: f64, end: Option<DwellEnd> },
    /// `sweep`: Sine's sweep; `end` null sweeps until the window closes.
    Sweep {
        band: (f64, f64),
        rate: SweepRate,
        start_hz: Option<f64>,
        direction: Direction,
        sweeps: Option<u32>,
        duration_s: Option<f64>,
    },
    /// `follow`: another part's frequency times `ratio`.
    Follow { of: String, ratio: f64 },
    /// `segments`: reserved, refused.
    Segments,
}

/// A dwell's end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DwellEnd {
    Seconds(f64),
    Cycles(u64),
}

/// A tone's amplitude as sent: one of the two.
#[derive(Debug, Clone, PartialEq)]
pub enum WireAmplitude {
    Constant(f64),
    Profile(SineProfile),
}

/// A narrowband's width as sent: one of the two.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WireWidth {
    Hz(f64),
    Share(f64),
}

/// What a part is, as sent.
#[derive(Debug, Clone, PartialEq)]
pub enum WireBody {
    /// The floor; its breakpoints and bands are parsed with Random's code and
    /// held in [`WireMixed::random`].
    Floor,
    Tone {
        schedule: WireSchedule,
        amplitude: WireAmplitude,
        alarm_db: (f64, f64),
        abort_db: (f64, f64),
    },
    Narrowband {
        schedule: WireSchedule,
        level_g2_hz: f64,
        width: WireWidth,
        alarm_db: Option<(f64, f64)>,
        abort_db: Option<(f64, f64)>,
    },
    /// A kind this service does not build, kept by name.
    Unknown(String),
}

/// One part as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct WirePart {
    pub id: String,
    /// `None`: the kind and its count.
    pub name: Option<String>,
    pub enabled: bool,
    pub on_s: f64,
    pub off_s: Option<f64>,
    pub body: WireBody,
}

impl WirePart {
    pub fn kind(&self) -> &str {
        match &self.body {
            WireBody::Floor => "floor",
            WireBody::Tone { .. } => "tone",
            WireBody::Narrowband { .. } => "narrowband",
            WireBody::Unknown(k) => k,
        }
    }

    pub fn schedule(&self) -> Option<&WireSchedule> {
        match &self.body {
            WireBody::Tone { schedule, .. } | WireBody::Narrowband { schedule, .. } => {
                Some(schedule)
            }
            _ => None,
        }
    }
}

/// A law choice as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct WireLaw {
    pub id: String,
    pub params: Map<String, Value>,
}

/// The tones' guards as sent (Sine's six).
#[derive(Debug, Clone, PartialEq)]
pub struct ToneGuards {
    pub law_budget: f64,
    pub law_misses: u64,
    pub max_step_db: f64,
    pub max_rise_db_per_s: f64,
    pub divergence_s: f64,
    pub fault_debounce_ticks: u64,
}

impl Default for ToneGuards {
    fn default() -> Self {
        let g = hertzforge_sine::law::LawGuards::default();
        ToneGuards {
            law_budget: g.budget_fraction,
            law_misses: u64::from(g.max_misses),
            max_step_db: g.max_step_db,
            max_rise_db_per_s: g.max_rise_db_per_s,
            divergence_s: g.divergence_s,
            fault_debounce_ticks: DEFAULT_FAULT_DEBOUNCE_TICKS,
        }
    }
}

/// The floor law's guards as sent (Random's core defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct FloorGuards {
    pub law_budget: f64,
    pub law_misses: u64,
    pub max_change_db: f64,
}

impl Default for FloorGuards {
    fn default() -> Self {
        let g = hertzforge_random::service::config::LawGuards::default();
        FloorGuards {
            law_budget: g.budget_fraction,
            law_misses: u64::from(g.max_misses),
            max_change_db: g.max_change_db,
        }
    }
}

/// A parsed Mixed Mode configuration, every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct WireMixed {
    pub title: String,
    pub parts: Vec<WirePart>,
    /// The Random configuration made of the floor's profile, the acquisition, the
    /// ladder, the control set, the limit rows and the feasibility inputs.
    pub random: RandomWire,
    pub exclusion_hz: f64,
    pub tick_ms: f64,
    pub part_ramp_s: f64,
    pub floor_settle_s: f64,
    pub trust_coherence: f64,
    pub trust_h_floor_frac: f64,
    /// Sine's form; only `fixed` is accepted (checked as an item).
    pub tracking_kind: String,
    pub tracking_hz: f64,
    pub law_floor: WireLaw,
    pub law_tones: WireLaw,
    pub guards_floor: FloorGuards,
    pub guards_tones: ToneGuards,
    pub drive_vpk_ceiling: f64,
    pub drive_vpk_abort: f64,
    pub kinematic_monitor: bool,
    /// The operator's words for the run's record: who ran it, on what, and notes.
    pub operator: String,
    pub article: String,
    pub notes: String,
    /// The test document and clause the run is made to, when given.
    pub specification: Option<(String, String)>,
    /// Whether the run's record keeps its streams (`record.streams`).
    pub record_streams: bool,
    pub metadata: Vec<MetaField>,
    /// The configuration as sent, for `mixed.config` and `mixed.reset`.
    pub raw: Value,
}

// ---------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------

fn bad(path: &str, message: impl Into<String>) -> MethodError {
    MethodError::bad_params(path, message)
}

/// `[below, above]` in positive dB.
fn db_band(o: &Obj<'_>, k: &str) -> Result<Option<(f64, f64)>, MethodError> {
    let Some((a, b)) = o.band(k)? else {
        return Ok(None);
    };
    if a < 0.0 || b < 0.0 {
        return Err(bad(
            &o.at(k),
            format!("\"{}\" is [below, above] in positive dB", o.at(k)),
        ));
    }
    Ok(Some((a, b)))
}

const DIRECTIONS: [(&str, Direction); 2] = [("up", Direction::Up), ("down", Direction::Down)];

fn parse_rate(o: &Obj<'_>, k: &str) -> Result<SweepRate, MethodError> {
    let r = o.obj(k)?;
    match r.opt_str("law")? {
        Some("log") => Ok(SweepRate::LogOctPerMin(r.f64("oct_per_min")?)),
        Some("linear") => Ok(SweepRate::LinearHzPerSec(r.f64("hz_per_s")?)),
        _ => Err(bad(
            &r.at("law"),
            format!(
                "\"{}\" is \"log\" (with \"oct_per_min\") or \"linear\" (with \"hz_per_s\")",
                r.at("law")
            ),
        )),
    }
}

fn parse_schedule(v: &Value, path: &str) -> Result<WireSchedule, MethodError> {
    let s = Obj::of(v, path)?;
    match s.opt_str("kind")? {
        Some("fixed") => {
            let end = match s.opt_obj("end")? {
                None => None,
                Some(e) => match (e.opt_f64("seconds")?, e.opt_count("cycles")?) {
                    (Some(x), None) => Some(DwellEnd::Seconds(x)),
                    (None, Some(c)) => Some(DwellEnd::Cycles(c)),
                    _ => {
                        return Err(bad(
                            &e.path,
                            format!(
                                "\"{}\" is {{\"seconds\"}} or {{\"cycles\"}}, one of the two",
                                e.path
                            ),
                        ))
                    }
                },
            };
            Ok(WireSchedule::Fixed {
                freq_hz: s.f64("freq_hz")?,
                end,
            })
        }
        Some("sweep") => {
            let band = s.band("band_hz")?.ok_or_else(|| {
                bad(
                    &s.at("band_hz"),
                    format!("\"{}\" is [low, high] in Hz", s.at("band_hz")),
                )
            })?;
            let (sweeps, duration_s) = match s.opt_obj("end")? {
                None => (None, None),
                Some(e) => {
                    let n = e.opt_count("sweeps")?;
                    let d = e.opt_f64("duration_s")?;
                    if n.is_none() && d.is_none() {
                        return Err(bad(
                            &e.path,
                            "the sweep's \"end\" gives \"sweeps\", \"duration_s\" or both, or is null",
                        ));
                    }
                    (n.map(|n| u32::try_from(n).unwrap_or(u32::MAX)), d)
                }
            };
            Ok(WireSchedule::Sweep {
                band,
                rate: parse_rate(&s, "rate")?,
                start_hz: s.opt_f64("start_hz")?,
                direction: s.choice("direction", &DIRECTIONS, Some(Direction::Up))?,
                sweeps,
                duration_s,
            })
        }
        Some("follow") => Ok(WireSchedule::Follow {
            of: s
                .opt_str("of")?
                .ok_or_else(|| bad(&s.at("of"), "a follow names the part it follows by \"of\""))?
                .to_owned(),
            ratio: s.f64("ratio")?,
        }),
        Some("segments") => Ok(WireSchedule::Segments),
        _ => Err(bad(
            &s.at("kind"),
            format!(
                "\"{}\" is \"fixed\", \"sweep\", \"follow\" or \"segments\"",
                s.at("kind")
            ),
        )),
    }
}

fn parse_part(v: &Value, i: usize) -> Result<WirePart, MethodError> {
    let path = format!("config.parts[{i}]");
    let p = Obj::of(v, &path)?;
    let id = p
        .opt_str("id")?
        .ok_or_else(|| bad(&p.at("id"), format!("\"{}\" names the part", p.at("id"))))?
        .to_owned();
    let kind = p.opt_str("kind")?.ok_or_else(|| {
        bad(
            &p.at("kind"),
            format!("\"{}\" is the part's kind", p.at("kind")),
        )
    })?;
    let (on_s, off_s) = match p.opt_obj("window")? {
        None => (0.0, None),
        Some(w) => (w.f64_or("on_s", 0.0)?, w.opt_f64("off_s")?),
    };
    let body =
        match kind {
            "floor" => {
                // The rows and bands are Random's; a shape error is found here so it is
                // named at the part's own path.
                let rows = p.array("breakpoints")?.ok_or_else(|| {
                    bad(
                        &p.at("breakpoints"),
                        format!("\"{}\" is the floor's list of rows", p.at("breakpoints")),
                    )
                })?;
                for (k, r) in rows.iter().enumerate() {
                    Obj::of(r, &format!("{path}.breakpoints[{k}]"))?;
                }
                db_band(&p, "alarm_db")?;
                db_band(&p, "abort_db")?;
                WireBody::Floor
            }
            "tone" => {
                let schedule = parse_schedule(p.require("schedule")?, &p.at("schedule"))?;
                let amp = p.opt_f64("amplitude_g")?;
                let prof = match p.m.get("profile") {
                    None | Some(Value::Null) => None,
                    Some(pv) => Some(parse_profile(pv, &p.at("profile"))?),
                };
                let amplitude =
                    match (amp, prof) {
                        (Some(a), None) => WireAmplitude::Constant(a),
                        (None, Some(pr)) => WireAmplitude::Profile(pr),
                        _ => return Err(bad(
                            &p.at("amplitude_g"),
                            "a tone's amplitude is \"amplitude_g\" or \"profile\", one of the two",
                        )),
                    };
                WireBody::Tone {
                    schedule,
                    amplitude,
                    alarm_db: db_band(&p, "alarm_db")?
                        .unwrap_or(hertzforge_mixed::parts::DEFAULT_TONE_ALARM_DB),
                    abort_db: db_band(&p, "abort_db")?
                        .unwrap_or(hertzforge_mixed::parts::DEFAULT_TONE_ABORT_DB),
                }
            }
            "narrowband" => {
                let schedule = parse_schedule(p.require("schedule")?, &p.at("schedule"))?;
                let width = match (p.opt_f64("width_hz")?, p.opt_f64("width_share")?) {
                    (Some(w), None) => WireWidth::Hz(w),
                    (None, Some(s)) => WireWidth::Share(s),
                    _ => return Err(bad(
                        &p.at("width_hz"),
                        "a narrowband's width is \"width_hz\" or \"width_share\", one of the two",
                    )),
                };
                WireBody::Narrowband {
                    schedule,
                    level_g2_hz: p.f64("level_g2_hz")?,
                    width,
                    alarm_db: db_band(&p, "alarm_db")?,
                    abort_db: db_band(&p, "abort_db")?,
                }
            }
            other => WireBody::Unknown(other.to_owned()),
        };
    Ok(WirePart {
        id,
        name: p.opt_str("name")?.map(str::to_owned),
        enabled: p.opt_bool("enabled")?.unwrap_or(true),
        on_s,
        off_s,
        body,
    })
}

/// Move a Random path to where it sits in a Mixed Mode configuration.
fn relocate_path(s: &str, floor: usize) -> String {
    s.replace("config.profile.", &format!("config.parts[{floor}]."))
        .replace("config.limits[", "config.limits.channels[")
}

fn relocate_error(mut e: MethodError, floor: usize) -> MethodError {
    e.message = relocate_path(&e.message, floor);
    if let Some(d) = e.detail.as_mut() {
        if let Some(Value::String(p)) = d.get_mut("param") {
            *p = relocate_path(p, floor);
        }
    }
    e
}

/// The Random configuration a Mixed Mode configuration's Random parts make.
fn random_of(c: &Map<String, Value>, floor: Option<&Map<String, Value>>) -> Value {
    let mut profile = Map::new();
    profile.insert(
        "breakpoints".into(),
        floor
            .and_then(|f| f.get("breakpoints").cloned())
            .unwrap_or_else(|| json!([])),
    );
    for k in ["alarm_db", "abort_db"] {
        if let Some(v) = floor.and_then(|f| f.get(k)) {
            profile.insert(k.into(), v.clone());
        }
    }
    let mut r = Map::new();
    r.insert(
        "title".into(),
        c.get("title").cloned().unwrap_or(Value::Null),
    );
    r.insert("profile".into(), Value::Object(profile));
    for k in ["acquisition", "schedule", "control", "feasibility"] {
        if let Some(v) = c.get(k) {
            r.insert(k.into(), v.clone());
        }
    }
    if let Some(ch) = c.get("limits").and_then(|l| l.get("channels")) {
        r.insert("limits".into(), ch.clone());
    }
    Value::Object(r)
}

/// Parse `params.config`.
pub fn parse(config: &Value) -> Result<WireMixed, MethodError> {
    let c = Obj::of(config, "config")?;
    let title = c.opt_str("title")?.unwrap_or("").to_owned();
    let list = c
        .array("parts")?
        .ok_or_else(|| bad("config.parts", "the configuration has no \"parts\" list"))?;
    let mut parts = Vec::with_capacity(list.len());
    for (i, v) in list.iter().enumerate() {
        parts.push(parse_part(v, i)?);
    }
    let floor_at = parts
        .iter()
        .position(|p| p.body == WireBody::Floor)
        .unwrap_or(0);
    let floor_obj = parts
        .iter()
        .position(|p| p.body == WireBody::Floor)
        .and_then(|i| list[i].as_object());
    let random =
        rcfg::parse(&random_of(c.m, floor_obj)).map_err(|e| relocate_error(e, floor_at))?;

    let acq = c.obj("acquisition")?;
    let tick_ms = acq.f64_or("tick_ms", DEFAULT_TICK_MS)?;
    let exclusion_hz = c.f64_or("exclusion_hz", hertzforge_mixed::DEFAULT_EXCLUSION_HZ)?;

    let mut w = WireMixed {
        title,
        parts,
        random,
        exclusion_hz,
        tick_ms,
        part_ramp_s: hertzforge_mixed::DEFAULT_PART_RAMP_S,
        floor_settle_s: hertzforge_mixed::DEFAULT_FLOOR_SETTLE_S,
        trust_coherence: 0.5,
        trust_h_floor_frac: 0.05,
        tracking_kind: "fixed".into(),
        tracking_hz: hertzforge_mixed::DEFAULT_TRACKING_HZ,
        law_floor: WireLaw {
            id: hertzforge_mixed::DEFAULT_FLOOR_LAW_ID.into(),
            params: Map::new(),
        },
        law_tones: WireLaw {
            id: hertzforge_mixed::DEFAULT_TONE_LAW_ID.into(),
            params: Map::new(),
        },
        guards_floor: FloorGuards::default(),
        guards_tones: ToneGuards::default(),
        drive_vpk_ceiling: 0.0,
        drive_vpk_abort: 0.0,
        kinematic_monitor: true,
        operator: String::new(),
        article: String::new(),
        notes: String::new(),
        specification: None,
        record_streams: true,
        metadata: Vec::new(),
        raw: config.clone(),
    };
    {
        let ff = hertzforge_sine::FeedforwardConfig::default();
        w.trust_coherence = ff.coherence_floor;
        w.trust_h_floor_frac = ff.h_floor_frac;
    }
    if let Some(o) = c.opt_obj("control")? {
        w.part_ramp_s = o.f64_or("part_ramp_s", w.part_ramp_s)?;
        w.floor_settle_s = o.f64_or("floor_settle_s", w.floor_settle_s)?;
        w.trust_coherence = o.f64_or("trust_coherence", w.trust_coherence)?;
        w.trust_h_floor_frac = o.f64_or("trust_h_floor_frac", w.trust_h_floor_frac)?;
        if let Some(t) = o.opt_obj("tracking")? {
            match t.opt_str("kind")? {
                Some("fixed") => w.tracking_hz = t.f64("hz")?,
                Some("proportional") => w.tracking_kind = "proportional".into(),
                _ => {
                    return Err(bad(
                        &t.at("kind"),
                        format!("\"{}\" is \"fixed\" or \"proportional\"", t.at("kind")),
                    ))
                }
            }
        }
        if let Some(l) = o.opt_obj("law")? {
            let law = |k: &str, slot: &mut WireLaw| -> Result<(), MethodError> {
                if let Some(x) = l.opt_obj(k)? {
                    slot.id = x
                        .opt_str("id")?
                        .ok_or_else(|| bad(&x.at("id"), "a law is named by its catalog \"id\""))?
                        .to_owned();
                    slot.params = match x.opt_obj("params")? {
                        None => Map::new(),
                        Some(p) => p.m.clone(),
                    };
                }
                Ok(())
            };
            law("floor", &mut w.law_floor)?;
            law("tones", &mut w.law_tones)?;
        }
        if let Some(g) = o.opt_obj("law_guards")? {
            if let Some(f) = g.opt_obj("floor")? {
                let d = &mut w.guards_floor;
                d.law_budget = f.f64_or("law_budget", d.law_budget)?;
                d.law_misses = f.opt_count("law_misses")?.unwrap_or(d.law_misses);
                d.max_change_db = f.f64_or("max_change_db", d.max_change_db)?;
            }
            if let Some(t) = g.opt_obj("tones")? {
                let d = &mut w.guards_tones;
                d.law_budget = t.f64_or("law_budget", d.law_budget)?;
                d.law_misses = t.opt_count("law_misses")?.unwrap_or(d.law_misses);
                d.max_step_db = t.f64_or("max_step_db", d.max_step_db)?;
                d.max_rise_db_per_s = t.f64_or("max_rise_db_per_s", d.max_rise_db_per_s)?;
                d.divergence_s = t.f64_or("divergence_s", d.divergence_s)?;
                d.fault_debounce_ticks = t
                    .opt_count("fault_debounce_ticks")?
                    .unwrap_or(d.fault_debounce_ticks);
            }
        }
    }

    let lim = c.obj("limits")?;
    w.drive_vpk_ceiling = lim.f64("drive_vpk_ceiling")?;
    w.drive_vpk_abort = lim.f64("drive_vpk_abort")?;
    w.kinematic_monitor = lim.opt_bool("kinematic_monitor")?.unwrap_or(true);

    // The module's own capture is gone: every run is recorded in its run folder,
    // written as the run goes, so there is nothing for a periodic save to do.
    if !matches!(c.m.get("capture"), None | Some(Value::Null)) {
        return Err(bad(
            "config.capture",
            "Mixed Mode takes no \"capture\": every run is recorded in its run folder as it \
             goes, and \"record.streams\" says whether the record keeps its time histories."
                .to_owned(),
        ));
    }
    let words =
        |k: &str| -> Result<String, MethodError> { Ok(c.opt_str(k)?.unwrap_or("").to_owned()) };
    (w.operator, w.article, w.notes) = (words("operator")?, words("article")?, words("notes")?);
    w.specification = match c.m.get("specification") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let o = Obj::of(v, "config.specification")?;
            Some((
                o.opt_str("document")?.unwrap_or("").to_owned(),
                o.opt_str("clause")?.unwrap_or("").to_owned(),
            ))
        }
    };
    w.record_streams = match c.m.get("record") {
        None | Some(Value::Null) => true,
        Some(v) => Obj::of(v, "config.record")?
            .opt_bool("streams")?
            .unwrap_or(true),
    };

    w.metadata = match c.array("metadata")? {
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
    Ok(w)
}

// ---------------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------------

/// What a law's role needs to know about it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleLaw {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Empty for a built-in law.
    pub sha256: String,
    pub builtin: bool,
    /// `Err` with the test type it is for when it is of the wrong world.
    pub world: Result<(), &'static str>,
    pub params: Vec<ParamSpec>,
    pub live: Vec<String>,
    /// May it drive hardware in this role (design D15).
    pub hardware: bool,
    /// Why it may not, in words (lower case, no stop), when it may not.
    pub not_cleared: Option<String>,
}

impl RoleLaw {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "sha256": if self.sha256.is_empty() { Value::Null } else { Value::from(self.sha256.as_str()) },
            "live": self.live,
            "hardware": self.hardware,
        })
    }
}

/// Both roles' laws.
#[derive(Debug, Clone, PartialEq)]
pub struct LawFacts {
    pub floor: RoleLaw,
    pub tones: RoleLaw,
}

/// Everything a configuration's checks are made against.
pub struct CheckContext<'a> {
    pub table: &'a ChannelTable,
    /// Looks up the floor's law (world `single`) by id.
    pub floor_law: &'a dyn Fn(&str) -> Option<RoleLaw>,
    /// Looks up the tones' law (world `sine`) by id.
    pub tone_law: &'a dyn Fn(&str) -> Option<RoleLaw>,
}

/// The rate, grid and tick a run would run at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub fs_hz: f64,
    pub n_pts: usize,
    pub df_hz: f64,
    pub lines: usize,
    pub loop_s: f64,
    pub tick_frames: usize,
    pub actual: bool,
}

impl Grid {
    pub fn of(r: &rcfg::Grid, tick_ms: f64) -> Grid {
        Grid {
            fs_hz: r.fs_hz,
            n_pts: r.n_pts,
            df_hz: r.df_hz,
            lines: r.lines,
            loop_s: r.loop_s,
            tick_frames: crate::sine::config::tick_frames(r.fs_hz, tick_ms),
            actual: r.actual,
        }
    }

    pub fn tick_s(&self) -> f64 {
        self.tick_frames as f64 / self.fs_hz
    }

    pub fn to_json(&self) -> Value {
        json!({
            "fs_hz": num(round(self.fs_hz, 4)),
            "n_pts": self.n_pts,
            "df_hz": num(round(self.df_hz, 6)),
            "lines": self.lines,
            "loop_s": num(round(self.loop_s, 4)),
            "tick_frames": self.tick_frames,
            "tick_s": num(round(self.tick_s(), 7)),
            "actual": self.actual,
        })
    }
}

/// One part's row of `derived`.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedPart {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub freq_hz: Option<(f64, f64)>,
    pub width_hz: Option<(f64, f64)>,
    pub ms_g2: f64,
    pub peak_g: Option<f64>,
}

/// One span left out of the floor's average for the whole run.
#[derive(Debug, Clone, PartialEq)]
pub struct Exclusion {
    pub part: String,
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub lines: usize,
}

/// What configure computes from a configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    pub band_hz: (f64, f64),
    pub grid: Grid,
    pub control_channels: Vec<u32>,
    pub parts: Vec<DerivedPart>,
    pub exclusion: Vec<Exclusion>,
    /// The closest pair of tones, their spacing then, and the least allowed.
    pub closest_tones: Option<(String, String, f64, f64)>,
    pub tracking: Vec<(String, crate::sine::config::Tracking)>,
    pub test_s: Option<f64>,
    pub identification_s: f64,
    pub call_budget_floor_s: f64,
    pub call_budget_tones_s: f64,
    pub law: LawFacts,
    /// The predicted peak of the sum, from the core's feasibility prediction;
    /// `None` until the core offers it.
    pub kinematics: Option<Value>,
    pub feasibility: (&'static str, String),
    /// The core's prediction of the time the floor needs to reach the control
    /// degrees of freedom on the grid, s; `None` when it cannot predict.
    pub floor_equalize_s: Option<f64>,
    /// How each tone's alarm and abort will be judged (the core's prediction,
    /// at the point of its motion that needs the longest averaging).
    pub tone_judging: Vec<hertzforge_mixed::predict::ToneJudgingPrediction>,
}

impl Derived {
    pub fn floor_ms(&self) -> f64 {
        self.sum_ms("floor")
    }

    fn sum_ms(&self, kind: &str) -> f64 {
        self.parts
            .iter()
            .filter(|p| p.kind == kind)
            .map(|p| p.ms_g2)
            .sum()
    }

    pub fn to_json(&self) -> Value {
        let pair = |p: Option<(f64, f64)>, d: i32| match p {
            None => Value::Null,
            Some((a, b)) => json!([num(round(a, d)), num(round(b, d))]),
        };
        let (fl, tn, bd) = (
            self.floor_ms(),
            self.sum_ms("tone"),
            self.sum_ms("narrowband"),
        );
        let total = fl + tn + bd;
        json!({
            "band_hz": [num(self.band_hz.0), num(self.band_hz.1)],
            "grid": self.grid.to_json(),
            "control_channels": self.control_channels,
            "parts": self.parts.iter().map(|p| json!({
                "id": p.id,
                "kind": p.kind,
                "name": p.name,
                "freq_hz": pair(p.freq_hz, 3),
                "width_hz": pair(p.width_hz, 3),
                "ms_g2": num(sig(p.ms_g2, 4)),
                "peak_g": p.peak_g.map_or(Value::Null, |g| num(sig(g, 4))),
            })).collect::<Vec<_>>(),
            "reference": {
                "floor_ms_g2": num(sig(fl, 4)),
                "tones_ms_g2": num(sig(tn, 4)),
                "bands_ms_g2": num(sig(bd, 4)),
                "total_ms_g2": num(sig(total, 4)),
                "total_rms_g": num(sig(total.sqrt(), 4)),
                "random_rms_g": num(sig((fl + bd).sqrt(), 4)),
            },
            "kinematics": self.kinematics.clone().unwrap_or(Value::Null),
            "exclusion": self.exclusion.iter().map(|e| json!({
                "part": e.part,
                "lo_hz": num(round(e.lo_hz, 3)),
                "hi_hz": num(round(e.hi_hz, 3)),
                "lines": e.lines,
            })).collect::<Vec<_>>(),
            "closest_tones": match &self.closest_tones {
                None => Value::Null,
                Some((a, b, s, m)) => json!({
                    "parts": [a, b],
                    "spacing_hz": num(round(*s, 3)),
                    "min_hz": num(round(*m, 3)),
                }),
            },
            "tracking": self.tracking.iter().map(|(id, t)| {
                let mut m = Map::new();
                m.insert("part".into(), Value::from(id.as_str()));
                if let Value::Object(o) = t.to_json() {
                    m.extend(o);
                }
                Value::Object(m)
            }).collect::<Vec<_>>(),
            "test_s": self.test_s.map_or(Value::Null, |s| num(round(s, 1))),
            "floor_equalize_s": self.floor_equalize_s.map_or(Value::Null, |s| num(round(s, 1))),
            "tone_judging": self.tone_judging.iter().map(|j| json!({
                "part": j.part,
                "judge_s": num(round(j.judge_s, 2)),
                "sigma_db": num(round(j.sigma_db, 2)),
                "alarm_db": [num(round(j.alarm_db.0, 2)), num(round(j.alarm_db.1, 2))],
                "abort_db": [num(round(j.abort_db.0, 2)), num(round(j.abort_db.1, 2))],
                "widened": j.widened,
            })).collect::<Vec<_>>(),
            "identification_s": num(round(self.identification_s, 1)),
            "call_budget_s": {
                "floor": num(round(self.call_budget_floor_s, 7)),
                "tones": num(round(self.call_budget_tones_s, 7)),
            },
            "law": {"floor": self.law.floor.to_json(), "tones": self.law.tones.to_json()},
            "feasibility": {"verdict": self.feasibility.0, "message": self.feasibility.1},
        })
    }
}

pub fn round(x: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    (x * p).round() / p
}

/// `x` to `digits` significant digits.
pub fn sig(x: f64, digits: i32) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let mag = x.abs().log10().floor() as i32;
    round(x, digits - 1 - mag)
}

/// Where a Random item sits in a Mixed Mode configuration.
fn relocate_item(mut i: Item, floor: usize) -> Item {
    i.at = match i.at {
        At::Row(r) => At::Field(format!("parts[{floor}].breakpoints[{r}]")),
        At::Field(f) => At::Field(if let Some(rest) = f.strip_prefix("profile.") {
            format!("parts[{floor}].{rest}")
        } else if let Some(rest) = f.strip_prefix("limits.") {
            format!("limits.channels.{rest}")
        } else {
            f
        }),
        other => other,
    };
    i
}

fn range(r: &mut Report, field: &str, value: f64, ok: bool, text: &str) {
    if !ok {
        r.errors.push(Item::field(
            "out_of_range",
            field,
            format!("{field} is {}; {text}", num(value)),
        ));
    }
}

fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The name a part has for a person: as given, else its kind and its count.
pub fn part_names(parts: &[WirePart]) -> Vec<String> {
    let (mut t, mut b) = (0, 0);
    parts
        .iter()
        .map(|p| {
            let fallback = match &p.body {
                WireBody::Floor => "Floor".to_owned(),
                WireBody::Tone { .. } => {
                    t += 1;
                    format!("Tone {t}")
                }
                WireBody::Narrowband { .. } => {
                    b += 1;
                    format!("Narrowband {b}")
                }
                WireBody::Unknown(k) => k.clone(),
            };
            p.name.clone().filter(|n| !n.is_empty()).unwrap_or(fallback)
        })
        .collect()
}

impl WireMixed {
    /// The index of the floor part, when there is one.
    pub fn floor_index(&self) -> Option<usize> {
        self.parts.iter().position(|p| p.body == WireBody::Floor)
    }

    /// The rate the configuration asks for.
    pub fn fs_hz(&self) -> f64 {
        self.random.fs_hz
    }

    /// The identification's measuring time: its averages, a loop each.
    pub fn identification_s(&self) -> f64 {
        let l = self.random.loop_config();
        self.random.control.pretest_averages.max(1) as f64 * l.segment_seconds()
    }

    fn tick_ok(&self, builtin: bool) -> bool {
        let lo = if builtin {
            MIN_TICK_S_BUILTIN
        } else {
            MIN_TICK_S_LAW_FILE
        } * 1000.0;
        self.tick_ms >= lo - 1e-9 && self.tick_ms <= MAX_TICK_MS
    }

    fn check_parts(&self, r: &mut Report) {
        for (i, p) in self.parts.iter().enumerate() {
            let at = |k: &str| format!("parts[{i}].{k}");
            if !plain_id(&p.id) {
                r.errors.push(Item::field(
                    "out_of_range",
                    at("id"),
                    format!(
                        "part {i}'s id \"{}\" is 1 to {MAX_ID_LEN} letters, digits, _ or -",
                        p.id
                    ),
                ));
            }
            range(
                r,
                &at("window.on_s"),
                p.on_s,
                p.on_s >= 0.0,
                "it is 0 or more",
            );
            if let Some(off) = p.off_s {
                range(
                    r,
                    &at("window.off_s"),
                    off,
                    off > p.on_s,
                    "it is after on_s",
                );
            }
            match &p.body {
                WireBody::Floor => {
                    if !p.enabled || p.on_s != 0.0 || p.off_s.is_some() {
                        r.errors.push(Item::field(
                            "out_of_range",
                            at(if p.enabled { "window" } else { "enabled" }),
                            "the floor plays for the whole run: enabled, window {\"on_s\":0,\"off_s\":null}",
                        ));
                    }
                }
                WireBody::Tone {
                    schedule,
                    amplitude,
                    ..
                } => {
                    self.check_schedule(r, i, schedule);
                    match amplitude {
                        WireAmplitude::Constant(a) => {
                            range(r, &at("amplitude_g"), *a, *a > 0.0, "it is above 0")
                        }
                        WireAmplitude::Profile(pr) => {
                            let f = at("profile");
                            r.extend(check_profile(pr, Locate::Field(&f)));
                        }
                    }
                }
                WireBody::Narrowband {
                    schedule,
                    level_g2_hz,
                    width,
                    ..
                } => {
                    self.check_schedule(r, i, schedule);
                    range(
                        r,
                        &at("level_g2_hz"),
                        *level_g2_hz,
                        *level_g2_hz > 0.0,
                        "it is above 0",
                    );
                    match *width {
                        WireWidth::Hz(w) => range(r, &at("width_hz"), w, w > 0.0, "it is above 0"),
                        WireWidth::Share(s) => range(
                            r,
                            &at("width_share"),
                            s,
                            s > 0.0 && s <= 1.0,
                            "it is above 0, at most 1",
                        ),
                    }
                }
                WireBody::Unknown(k) => r.errors.push(Item::field(
                    "unknown_part_kind",
                    at("kind"),
                    format!("part {}: there is no part of kind \"{k}\"", p.id),
                )),
            }
        }
    }

    fn check_schedule(&self, r: &mut Report, i: usize, s: &WireSchedule) {
        let at = |k: &str| format!("parts[{i}].schedule.{k}");
        match s {
            WireSchedule::Fixed { freq_hz, end } => {
                range(r, &at("freq_hz"), *freq_hz, *freq_hz > 0.0, "it is above 0");
                match end {
                    Some(DwellEnd::Seconds(x)) => {
                        range(r, &at("end.seconds"), *x, *x > 0.0, "it is above 0")
                    }
                    Some(DwellEnd::Cycles(c)) => {
                        range(r, &at("end.cycles"), *c as f64, *c >= 1, "it is at least 1")
                    }
                    None => {}
                }
            }
            WireSchedule::Sweep {
                band,
                rate,
                start_hz,
                sweeps,
                duration_s,
                ..
            } => {
                range(
                    r,
                    &at("band_hz"),
                    band.0,
                    band.0 > 0.0 && band.1 > band.0,
                    "it is [low, high] with 0 < low < high",
                );
                let v = match *rate {
                    SweepRate::LogOctPerMin(x) | SweepRate::LinearHzPerSec(x) => x,
                };
                range(r, &at("rate"), v, v > 0.0, "a sweep rate is above 0");
                if let Some(f) = start_hz {
                    range(
                        r,
                        &at("start_hz"),
                        *f,
                        *f >= band.0 && *f <= band.1,
                        "it is inside the sweep's band",
                    );
                }
                if let Some(n) = sweeps {
                    range(
                        r,
                        &at("end.sweeps"),
                        f64::from(*n),
                        *n >= 1,
                        "it is at least 1",
                    );
                }
                if let Some(d) = duration_s {
                    range(r, &at("end.duration_s"), *d, *d > 0.0, "it is above 0");
                }
            }
            WireSchedule::Follow { ratio, .. } => {
                range(r, &at("ratio"), *ratio, *ratio > 0.0, "it is above 0")
            }
            WireSchedule::Segments => r.errors.push(Item::field(
                "schedule_unsupported",
                format!("parts[{i}].schedule.kind"),
                "a schedule in segments is not available; use fixed, sweep or follow",
            )),
        }
    }

    fn check_around(&self, r: &mut Report, tones_builtin: bool) {
        range(
            r,
            "exclusion_hz",
            self.exclusion_hz,
            self.exclusion_hz > 0.0 && self.exclusion_hz <= MAX_EXCLUSION_HZ,
            "it is above 0, at most 50 Hz",
        );
        if self.tracking_kind != "fixed" {
            r.errors.push(Item::field(
                "out_of_range",
                "control.tracking",
                "Mixed Mode tracks every tone at one fixed width: {\"kind\":\"fixed\",\"hz\"}",
            ));
        } else {
            range(
                r,
                "control.tracking.hz",
                self.tracking_hz,
                self.tracking_hz > 0.0 && self.tracking_hz <= MAX_TRACKING_HZ,
                "it is above 0, at most 10 Hz",
            );
        }
        range(
            r,
            "control.part_ramp_s",
            self.part_ramp_s,
            self.part_ramp_s > 0.0 && self.part_ramp_s <= MAX_PART_RAMP_S,
            "it is above 0, at most 60 s",
        );
        range(
            r,
            "control.floor_settle_s",
            self.floor_settle_s,
            self.floor_settle_s > 0.0 && self.floor_settle_s <= MAX_FLOOR_SETTLE_S,
            "it is above 0, at most 600 s",
        );
        range(
            r,
            "control.trust_coherence",
            self.trust_coherence,
            (0.0..=1.0).contains(&self.trust_coherence),
            "it runs from 0 to 1",
        );
        range(
            r,
            "control.trust_h_floor_frac",
            self.trust_h_floor_frac,
            (0.0..1.0).contains(&self.trust_h_floor_frac),
            "it runs from 0 to below 1",
        );
        if !self.tick_ok(tones_builtin) {
            let lo = if tones_builtin { 2 } else { 8 };
            r.errors.push(Item::field(
                "tick_out_of_range",
                "acquisition.tick_ms",
                format!(
                    "the control tick is {} ms; it runs from {lo} to 1000 ms for this tone law",
                    num(self.tick_ms)
                ),
            ));
        }
        let gf = &self.guards_floor;
        range(
            r,
            "control.law_guards.floor.law_budget",
            gf.law_budget,
            gf.law_budget > 0.0 && gf.law_budget <= 1.0,
            "it is above 0, at most 1",
        );
        range(
            r,
            "control.law_guards.floor.law_misses",
            gf.law_misses as f64,
            (1..=100).contains(&gf.law_misses),
            "it runs from 1 to 100",
        );
        range(
            r,
            "control.law_guards.floor.max_change_db",
            gf.max_change_db,
            gf.max_change_db > 0.0,
            "it is above 0",
        );
        let gt = &self.guards_tones;
        range(
            r,
            "control.law_guards.tones.law_budget",
            gt.law_budget,
            gt.law_budget > 0.0 && gt.law_budget <= 1.0,
            "it is above 0, at most 1",
        );
        range(
            r,
            "control.law_guards.tones.law_misses",
            gt.law_misses as f64,
            (1..=100).contains(&gt.law_misses),
            "it runs from 1 to 100",
        );
        range(
            r,
            "control.law_guards.tones.max_step_db",
            gt.max_step_db,
            (MAX_STEP_DB_RANGE.0..=MAX_STEP_DB_RANGE.1).contains(&gt.max_step_db),
            &format!(
                "it runs from {} to {} dB",
                MAX_STEP_DB_RANGE.0, MAX_STEP_DB_RANGE.1
            ),
        );
        range(
            r,
            "control.law_guards.tones.max_rise_db_per_s",
            gt.max_rise_db_per_s,
            gt.max_rise_db_per_s > 0.0,
            "it is above 0",
        );
        range(
            r,
            "control.law_guards.tones.divergence_s",
            gt.divergence_s,
            gt.divergence_s > 0.0,
            "it is above 0",
        );
        range(
            r,
            "control.law_guards.tones.fault_debounce_ticks",
            gt.fault_debounce_ticks as f64,
            gt.fault_debounce_ticks >= 1,
            "it is at least 1",
        );
        range(
            r,
            "limits.drive_vpk_ceiling",
            self.drive_vpk_ceiling,
            self.drive_vpk_ceiling > 0.0 && self.drive_vpk_ceiling <= MAX_DRIVE_VPK,
            "it is above 0, at most 10 V",
        );
        if self.drive_vpk_abort < self.drive_vpk_ceiling {
            r.errors.push(Item::field(
                "abort_below_ceiling",
                "limits.drive_vpk_abort",
                format!(
                    "the drive abort, {} V, is below the ceiling, {} V",
                    num(self.drive_vpk_abort),
                    num(self.drive_vpk_ceiling)
                ),
            ));
        }
        range(
            r,
            "limits.drive_vpk_abort",
            self.drive_vpk_abort,
            self.drive_vpk_abort <= MAX_DRIVE_VPK,
            "it is at most 10 V",
        );
    }

    fn check_law(
        r: &mut Report,
        role: &str,
        law: &WireLaw,
        found: Option<RoleLaw>,
    ) -> Option<RoleLaw> {
        let field = format!("control.law.{role}");
        let Some(f) = found else {
            r.errors.push(Item::field(
                "law_unknown",
                field,
                format!("there is no control law \"{}\" in the catalog", law.id),
            ));
            return None;
        };
        if let Err(other) = f.world {
            r.errors.push(Item::field(
                "law_wrong_world",
                field,
                format!(
                    "{} is a law for {other}; the {role} take a law of world \"{}\"",
                    f.name,
                    if role == "floor" { "single" } else { "sine" }
                ),
            ));
            return None;
        }
        for (k, v) in &law.params {
            match f.params.iter().find(|p| &p.key == k) {
                None => r.errors.push(Item::field(
                    "law_param_unknown",
                    format!("{field}.params.{k}"),
                    format!("{} declares no parameter \"{k}\"", f.name),
                )),
                Some(spec) => {
                    if let Some(why) = crate::shock::config::param_problem(spec, v) {
                        r.errors.push(Item::field(
                            "law_param_invalid",
                            format!("{field}.params.{k}"),
                            format!("{}'s {} {why}", f.name, spec.label),
                        ));
                    }
                }
            }
        }
        Some(f)
    }

    /// Check against the context. Returns the report and, when it has no errors,
    /// the derived values.
    pub fn check(&self, ctx: &CheckContext<'_>) -> (Report, Option<Derived>) {
        let mut r = Report::default();
        let floor = self.floor_index();
        let floors = self
            .parts
            .iter()
            .filter(|p| p.body == WireBody::Floor)
            .count();
        // Random's checks on its parts, relocated.
        let (rr, rderived) = self.random.check(ctx.table);
        for i in rr.errors {
            if floor.is_some() || !matches!(i.at, At::Row(_)) && i.reason != "profile_empty" {
                r.errors.push(relocate_item(i, floor.unwrap_or(0)));
            }
        }
        for i in rr.warnings {
            r.warnings.push(relocate_item(i, floor.unwrap_or(0)));
        }
        match floors {
            0 => r
                .errors
                .push(Item::field("no_floor", "parts", "the test needs a floor")),
            1 => {}
            _ => r.errors.push(Item::field(
                "too_many_floors",
                "parts",
                "the test has more than one floor",
            )),
        }
        let tones = self
            .parts
            .iter()
            .filter(|p| matches!(p.body, WireBody::Tone { .. }))
            .count();
        let bands = self
            .parts
            .iter()
            .filter(|p| matches!(p.body, WireBody::Narrowband { .. }))
            .count();
        if tones > MAX_TONES {
            r.errors.push(Item::field(
                "too_many_tones",
                "parts",
                format!("{tones} tones; a test holds at most {MAX_TONES}"),
            ));
        }
        if bands > MAX_BANDS {
            r.errors.push(Item::field(
                "too_many_bands",
                "parts",
                format!("{bands} narrowbands; a test holds at most {MAX_BANDS}"),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, p) in self.parts.iter().enumerate() {
            if !seen.insert(p.id.as_str()) {
                r.errors.push(Item::field(
                    "duplicate_part_id",
                    format!("parts[{i}].id"),
                    format!("part id \"{}\" is used twice", p.id),
                ));
            }
        }
        self.check_parts(&mut r);
        let floor_law = Self::check_law(
            &mut r,
            "floor",
            &self.law_floor,
            (ctx.floor_law)(&self.law_floor.id),
        );
        let tone_law = Self::check_law(
            &mut r,
            "tones",
            &self.law_tones,
            (ctx.tone_law)(&self.law_tones.id),
        );
        self.check_around(&mut r, tone_law.as_ref().is_none_or(|l| l.builtin));
        // With tones, the lines must be no wider than half the exclusion width,
        // so a tone's exclusion covers at least two lines (the core's own rule).
        let has_tones = self
            .parts
            .iter()
            .any(|p| matches!(p.body, WireBody::Tone { .. }));
        if let Some(d) = &rderived {
            if has_tones && d.grid.df_hz > self.exclusion_hz / 2.0 + 1e-12 {
                r.errors.push(Item::field(
                    "out_of_range",
                    "exclusion_hz",
                    format!(
                        "the lines are {} Hz apart; with tones they are at most half the \
                         exclusion width of {} Hz",
                        num(round(d.grid.df_hz, 4)),
                        num(self.exclusion_hz)
                    ),
                ));
            }
        }
        if !r.errors.is_empty() {
            return (r, None);
        }
        // The core's own checks: follows, motion and spacing.
        let core = match self.to_core(ctx.table) {
            Ok(c) => c,
            Err(e) => {
                r.errors.push(self.core_item(&e));
                return (r, None);
            }
        };
        if let Err(e) = core.validate(Some(self.fs_hz())) {
            r.errors.push(self.core_item(&e));
            return (r, None);
        }
        let (Some(rd), Some(floor_law), Some(tone_law)) = (rderived, floor_law, tone_law) else {
            return (r, None);
        };
        let grid = Grid::of(&rd.grid, self.tick_ms);
        let derived = self.derive(
            &core,
            rd,
            grid,
            LawFacts {
                floor: floor_law,
                tones: tone_law,
            },
            &mut r,
        );
        if !r.errors.is_empty() {
            return (r, None);
        }
        (r, Some(derived))
    }

    /// A core refusal as an item, located in the wire configuration.
    pub fn core_item(&self, e: &ConfigError) -> Item {
        let idx = |id: &str| self.parts.iter().position(|p| p.id == id);
        let at_part = |id: &str, k: &str| match idx(id) {
            Some(i) if k.is_empty() => format!("parts[{i}]"),
            Some(i) => format!("parts[{i}].{k}"),
            None => "parts".to_owned(),
        };
        let message = e.to_string();
        match e {
            ConfigError::NoFloor => Item::field("no_floor", "parts", message),
            ConfigError::MoreThanOneFloor => Item::field("too_many_floors", "parts", message),
            ConfigError::UnknownPartKind { part, .. } => {
                Item::field("unknown_part_kind", at_part(part, "kind"), message)
            }
            ConfigError::TooManyTones { .. } => Item::field("too_many_tones", "parts", message),
            ConfigError::TooManyBands { .. } => Item::field("too_many_bands", "parts", message),
            ConfigError::DuplicatePartId { part } => {
                Item::field("duplicate_part_id", at_part(part, "id"), message)
            }
            ConfigError::PartOutOfBand { part, .. } => {
                Item::field("part_out_of_band", at_part(part, ""), message)
            }
            ConfigError::PartsTooClose { a, .. } => {
                Item::field("parts_too_close", at_part(a, ""), message)
            }
            ConfigError::FollowUnknownPart { part, .. }
            | ConfigError::FollowNoFrequency { part, .. } => {
                Item::field("follow_unknown_part", at_part(part, "schedule.of"), message)
            }
            ConfigError::FollowSelf { part } => {
                Item::field("follow_self", at_part(part, "schedule.of"), message)
            }
            ConfigError::FollowLoop { parts } => Item::field(
                "follow_loop",
                at_part(parts.first().map_or("", String::as_str), "schedule.of"),
                message,
            ),
            ConfigError::SegmentsNotBuilt { part } => Item::field(
                "schedule_unsupported",
                at_part(part, "schedule.kind"),
                message,
            ),
            ConfigError::BadPart { part, field, why } => {
                if *field == "amplitude" && why.contains("does not reach") {
                    Item::field("profile_not_covering", at_part(part, "profile"), message)
                } else {
                    Item::field("out_of_range", at_part(part, field), message)
                }
            }
            ConfigError::BadConfig { field, .. } => {
                Item::field("out_of_range", wire_field(field), message)
            }
        }
    }

    fn derive(
        &self,
        core: &MixedConfig,
        rd: rcfg::Derived,
        grid: Grid,
        law: LawFacts,
        r: &mut Report,
    ) -> Derived {
        let names = part_names(&self.parts);
        let motions = core.motions().unwrap_or_default();
        let floor_ms = rd.grms_g * rd.grms_g;
        let mut parts = Vec::new();
        let mut exclusion = Vec::new();
        let lines_in = |lo: f64, hi: f64| -> usize {
            let df = grid.df_hz;
            if df <= 0.0 {
                return 0;
            }
            let k0 = (lo / df).ceil() as i64;
            let k1 = (hi / df).floor() as i64;
            (k1 - k0 + 1).max(0) as usize
        };
        for ((p, cp), (m, name)) in self
            .parts
            .iter()
            .zip(&core.parts)
            .zip(motions.iter().zip(&names))
        {
            let span = m.map(|m| (m.lo_hz, m.hi_hz));
            let still = span.is_some_and(|(a, b)| a == b) && Self::stands_still(core, &p.id);
            let (ms, peak, width) = match &cp.body {
                PartBody::Floor(_) => (floor_ms, None, None),
                PartBody::Tone(t) => {
                    let peak = match &t.amplitude {
                        ToneAmplitude::Constant { g_pk } => *g_pk,
                        a => span.map_or(0.0, |(lo, hi)| peak_over(a, lo, hi)),
                    };
                    if still {
                        let f = span.map_or(0.0, |s| s.0);
                        exclusion.push(Exclusion {
                            part: p.id.clone(),
                            lo_hz: f - self.exclusion_hz,
                            hi_hz: f + self.exclusion_hz,
                            lines: lines_in(f - self.exclusion_hz, f + self.exclusion_hz),
                        });
                    }
                    (peak * peak / 2.0, Some(peak), None)
                }
                PartBody::Narrowband(b) => {
                    let (lo, hi) = span.unwrap_or((0.0, 0.0));
                    let w = |c: f64| {
                        let (a, z) = b.width.edges_at(c);
                        z - a
                    };
                    let (wlo, whi) = (w(lo), w(hi));
                    if still {
                        let (a, z) = b.width.edges_at(lo);
                        let guard = 2.0 * grid.df_hz;
                        exclusion.push(Exclusion {
                            part: p.id.clone(),
                            lo_hz: a - guard,
                            hi_hz: z + guard,
                            lines: lines_in(a - guard, z + guard),
                        });
                    }
                    (b.level_g2_hz * wlo.max(whi), None, Some((wlo, whi)))
                }
                PartBody::Unknown { .. } => (0.0, None, None),
            };
            parts.push(DerivedPart {
                id: p.id.clone(),
                kind: p.kind().to_owned(),
                name: name.clone(),
                freq_hz: span,
                width_hz: width,
                ms_g2: ms,
                peak_g: peak,
            });
        }
        // The closest pair of tones over their whole motion, as the core judges it.
        let min_hz = MIN_TONE_SPACING_WIDTHS * self.tracking_hz;
        let tones: Vec<(usize, &hertzforge_mixed::Motion)> = core
            .parts
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.body, PartBody::Tone(_)))
            .filter_map(|(i, _)| motions.get(i).and_then(|m| m.as_ref()).map(|m| (i, m)))
            .collect();
        let mut closest: Option<(String, String, f64, f64)> = None;
        for (x, (ia, ma)) in tones.iter().enumerate() {
            for (ib, mb) in &tones[x + 1..] {
                if !core.parts[*ia].window.overlaps(&core.parts[*ib].window) {
                    continue;
                }
                let d = if ma.root == mb.root {
                    let root = motions[ma.root].expect("a root has a motion");
                    (ma.factor - mb.factor).abs() * root.lo_hz
                } else {
                    (ma.lo_hz - mb.hi_hz).max(mb.lo_hz - ma.hi_hz).max(0.0)
                };
                if closest.as_ref().is_none_or(|c| d < c.2) {
                    closest = Some((
                        core.parts[*ia].id.clone(),
                        core.parts[*ib].id.clone(),
                        d,
                        min_hz,
                    ));
                }
            }
        }
        // Each tone's sweep against the fixed tracking width, on one block's lag.
        let lag = hertzforge_hal::default_block_frames(grid.fs_hz);
        let mut tracking = Vec::new();
        for (i, p) in core.parts.iter().enumerate() {
            if !matches!(p.body, PartBody::Tone(_)) {
                continue;
            }
            let verdict = match Self::root_sweep(core, i) {
                Some((band, rate)) => crate::sine::config::Tracking::of(&assess_tracking_lag(
                    band,
                    rate,
                    BandwidthMode::FixedHz(self.tracking_hz),
                    lag,
                    grid.fs_hz,
                )),
                None => crate::sine::config::Tracking::of(&TrackingLagVerdict::Ok { ratio: 0.0 }),
            };
            match verdict.verdict {
                "refused" => r.errors.push(Item::field(
                    "tracking_lag",
                    format!("parts[{i}].schedule"),
                    verdict.message.clone().unwrap_or_default(),
                )),
                "advisory" => r.warnings.push(Item::field(
                    "tracking_lag",
                    format!("parts[{i}].schedule"),
                    verdict.message.clone().unwrap_or_default(),
                )),
                _ => {}
            }
            tracking.push((p.id.clone(), verdict));
        }
        let test_s = match self.random.mode {
            hertzforge_random::service::config::RunMode::Auto => Some(
                self.random.levels.iter().filter_map(|l| l.1).sum::<f64>()
                    + self.random.full_level_s,
            ),
            hertzforge_random::service::config::RunMode::Manual => None,
        };
        // The predicted figures are the core's (`predict`): the parts' mean
        // squares, the peaks of the sum and the shaker's ratings.
        let mut floor_equalize_s = None;
        let mut tone_judging = Vec::new();
        let (kinematics, feasibility) = match hertzforge_mixed::predict(core) {
            Ok(pr) => {
                floor_equalize_s = Some(pr.floor_equalize_s);
                tone_judging = pr.tone_judging.clone();
                for dp in &mut parts {
                    if let Some(m) = pr.parts.iter().find(|m| m.part == dp.id) {
                        dp.ms_g2 = m.ms_g2;
                    }
                }
                (
                    Some(kinematics_json(&pr, core.shaker.is_some())),
                    feasibility_of(&pr, core.shaker.is_some()),
                )
            }
            Err(e) => {
                r.errors.push(self.core_item(&e));
                (None, ("not_evaluated", String::new()))
            }
        };
        Derived {
            band_hz: rd.band_hz,
            grid,
            control_channels: rd.control_channels,
            parts,
            exclusion,
            closest_tones: closest,
            tracking,
            test_s,
            identification_s: self.identification_s(),
            call_budget_floor_s: self.guards_floor.law_budget * grid.loop_s,
            call_budget_tones_s: self.guards_tones.law_budget * grid.tick_s(),
            law,
            kinematics,
            feasibility,
            floor_equalize_s,
            tone_judging,
        }
    }

    /// Whether part `id` stands still for the whole run (a `fixed` part, or one
    /// that follows a part standing still).
    fn stands_still(core: &MixedConfig, id: &str) -> bool {
        let mut at = core.index_of(id);
        for _ in 0..=core.parts.len() {
            match at.and_then(|i| core.parts[i].schedule()) {
                Some(PartSchedule::Fixed { .. }) => return true,
                Some(PartSchedule::Follow { of, .. }) => at = core.index_of(of),
                _ => return false,
            }
        }
        false
    }

    /// The band and rate of a tone's motion when it sweeps (its own, or its
    /// root's through the ratio).
    fn root_sweep(core: &MixedConfig, i: usize) -> Option<((f64, f64), SweepRate)> {
        let mut at = i;
        let mut factor = 1.0;
        for _ in 0..=core.parts.len() {
            match core.parts[at].schedule()? {
                PartSchedule::Sweep {
                    lo_hz,
                    hi_hz,
                    law,
                    rate,
                    ..
                } => {
                    let rate = match law {
                        SweepLaw::LogOctPerMin => SweepRate::LogOctPerMin(*rate),
                        SweepLaw::LinearHzPerS => SweepRate::LinearHzPerSec(rate * factor),
                    };
                    return Some(((lo_hz * factor, hi_hz * factor), rate));
                }
                PartSchedule::Follow { of, ratio } => {
                    factor *= ratio;
                    at = core.index_of(of)?;
                }
                _ => return None,
            }
        }
        None
    }

    // -----------------------------------------------------------------------------
    // To the core
    // -----------------------------------------------------------------------------

    /// The controller's configuration for `table`. Every wire field reaches it,
    /// or is held here because the core has no place for it (the requested rate,
    /// which the station settles; the floor law's damping, which reaches the core
    /// through the floor law's settings; the identification's averages, which the
    /// run thread passes to the identification; the tones' fault debounce; a tone
    /// profile's label; the operator's words and the record's settings, which
    /// the run's record keeps).
    pub fn to_core(&self, table: &ChannelTable) -> Result<MixedConfig, ConfigError> {
        let built = self
            .random
            .build(table)
            .map_err(|e| ConfigError::BadConfig {
                field: "control",
                why: e.message,
            })?;
        let t = built.test;
        let parts = self
            .parts
            .iter()
            .map(|p| self.core_part(p, &t.breakpoints))
            .collect();
        let gf = &self.guards_floor;
        let gt = &self.guards_tones;
        Ok(MixedConfig {
            df_hz: t.df_hz,
            lines: t.lines,
            nfft: Some(self.random.n_pts as usize),
            blocks_per_window: self.random.n_avg as usize,
            soft_start_db: t.soft_start_db,
            soft_start_relax_db: t.soft_start_relax_db,
            shaker: t.shaker.clone(),
            moving_mass: t.moving_mass,
            crest_factor: t.crest_factor,
            control_dof: t.control_dof,
            control: t.control.clone(),
            parts,
            exclusion_hz: self.exclusion_hz,
            tracking_hz: self.tracking_hz,
            schedule: t.schedule.clone(),
            full_level_duration_s: (t.full_level_duration_s > 0.0)
                .then_some(t.full_level_duration_s),
            level_ramp_s: t.level_ramp_s,
            shutdown_ramp_s: t.shutdown_ramp_s,
            part_ramp_s: self.part_ramp_s,
            floor_settle_s: self.floor_settle_s,
            identification: MixedIdentification {
                sysid_level_db: t.sysid_level_db,
                coherence_floor: t.coherence_floor,
                pretest_max_vrms: t.pretest_max_vrms,
                pretest_min_vrms: t.pretest_min_vrms,
                shaped_pretest: t.shaped_pretest,
                pretest_reference_db: t.pretest_reference_db,
                pretest_start_vrms: t.pretest_start_vrms,
                review_hold: self.random.control.review_after_sysid,
                trust_coherence: self.trust_coherence,
                trust_h_floor_frac: self.trust_h_floor_frac,
            },
            limits: MixedLimits {
                drive_vpk_ceiling: self.drive_vpk_ceiling,
                drive_vpk_abort: Some(self.drive_vpk_abort),
                drive_ceiling_vrms: Some(t.drive_ceiling_vrms),
                drive_abort_vrms: t.drive_abort_vrms,
                kinematic_monitor: self.kinematic_monitor,
                drive_clip_sigma: t.drive_clip_sigma,
                limit_profiles: t.limit_profiles.clone(),
                max_lines_out: t.max_lines_out,
                divergence_loops: t.divergence_loops,
                overload_fraction: t.overload_fraction,
            },
            capture: CaptureRules::default(),
            law: MixedLawChoice {
                floor: LawPick {
                    id: self.law_floor.id.clone(),
                    params: self.law_floor.params.clone(),
                },
                tones: LawPick {
                    id: self.law_tones.id.clone(),
                    params: self.law_tones.params.clone(),
                },
            },
            law_guards: Some(MixedLawGuards {
                floor: hertzforge_random::service::config::LawGuards {
                    max_change_db: gf.max_change_db,
                    budget_fraction: gf.law_budget,
                    max_misses: u32::try_from(gf.law_misses).unwrap_or(u32::MAX),
                },
                tones: hertzforge_sine::law::LawGuards {
                    max_step_db: gt.max_step_db,
                    max_rise_db_per_s: gt.max_rise_db_per_s,
                    budget_fraction: gt.law_budget,
                    max_misses: u32::try_from(gt.law_misses).unwrap_or(u32::MAX),
                    divergence_s: gt.divergence_s,
                },
            }),
            control_tick_s: Some(self.tick_ms / 1000.0),
            metadata: self.metadata.clone(),
        })
    }

    fn core_part(
        &self,
        p: &WirePart,
        floor_table: &hertzforge_breakpoints::BreakpointTable,
    ) -> Part {
        let body = match &p.body {
            WireBody::Floor => PartBody::Floor(FloorPart {
                breakpoints: floor_table.clone(),
                alarm_db: self.random.alarm_db,
                abort_db: self.random.abort_db,
            }),
            WireBody::Tone {
                schedule,
                amplitude,
                alarm_db,
                abort_db,
            } => PartBody::Tone(TonePart {
                schedule: core_schedule(schedule),
                amplitude: match amplitude {
                    WireAmplitude::Constant(g) => ToneAmplitude::Constant { g_pk: *g },
                    WireAmplitude::Profile(pr) => ToneAmplitude::Profile {
                        rows: pr.rows.clone(),
                    },
                },
                alarm_db: *alarm_db,
                abort_db: *abort_db,
            }),
            WireBody::Narrowband {
                schedule,
                level_g2_hz,
                width,
                alarm_db,
                abort_db,
            } => PartBody::Narrowband(NarrowbandPart {
                schedule: core_schedule(schedule),
                level_g2_hz: *level_g2_hz,
                width: match *width {
                    WireWidth::Hz(hz) => BandWidth::Hz { hz },
                    WireWidth::Share(share) => BandWidth::Share { share },
                },
                alarm_db: alarm_db.unwrap_or(self.random.alarm_db),
                abort_db: abort_db.unwrap_or(self.random.abort_db),
            }),
            WireBody::Unknown(k) => PartBody::Unknown {
                kind: k.clone(),
                fields: Map::new(),
            },
        };
        Part {
            id: p.id.clone(),
            name: p.name.clone().unwrap_or_default(),
            enabled: p.enabled,
            window: PartWindow {
                on_s: p.on_s,
                off_s: p.off_s,
            },
            body,
        }
    }
}

const IN_PER_M: f64 = 1.0 / 0.0254;

/// The predicted peaks of the sum, as `derived.kinematics` states them.
pub fn kinematics_json(pr: &hertzforge_mixed::MixedPrediction, shaker: bool) -> Value {
    let (verdict, message) = feasibility_of(pr, shaker);
    json!({
        "peak_accel_g": num(sig(pr.accel_g_pk, 4)),
        "peak_velocity_ips": num(sig(pr.velocity_m_s_pk * IN_PER_M, 4)),
        "peak_disp_pp_in": num(sig(2.0 * pr.displacement_m_pk * IN_PER_M, 4)),
        "verdict": verdict,
        "message": message,
    })
}

/// The feasibility verdict of a prediction against the shaker's ratings.
pub fn feasibility_of(
    pr: &hertzforge_mixed::MixedPrediction,
    shaker: bool,
) -> (&'static str, String) {
    if !shaker {
        return (
            "not_evaluated",
            "No shaker was given, so the test was not checked against shaker ratings.".into(),
        );
    }
    if pr.ratings.is_empty() {
        return (
            "not_evaluated",
            "The shaker record lacks the ratings to check this test against.".into(),
        );
    }
    let over: Vec<String> = pr
        .ratings
        .iter()
        .filter(|r| !r.within)
        .map(|r| {
            format!(
                "{} {} {} over {} {}",
                r.quantity,
                num(sig(r.predicted, 4)),
                r.unit,
                num(sig(r.rating, 4)),
                r.unit
            )
        })
        .collect();
    if over.is_empty() {
        ("pass", "The test is within the shaker's ratings.".into())
    } else {
        (
            "exceeded",
            format!(
                "The test exceeds the shaker's ratings: {}.",
                over.join(", ")
            ),
        )
    }
}

/// Where a field the core names sits in the wire configuration.
fn wire_field(core: &str) -> &str {
    match core {
        "drive_vpk_ceiling" => "limits.drive_vpk_ceiling",
        "drive_vpk_abort" => "limits.drive_vpk_abort",
        "drive_ceiling_vrms" => "control.drive_ceiling_vrms",
        "drive_abort_vrms" => "control.drive_abort_vrms",
        "drive_clip_sigma" => "control.max_sigma",
        "coherence_floor" => "control.coherence_floor",
        "sysid_level_db" => "control.pretest.level_db",
        "control_dof" => "control.control_dof",
        "control" => "control.weights",
        "limit_profiles" => "limits.channels",
        "level_ramp_s" => "control.level_ramp_s",
        "shutdown_ramp_s" => "control.shutdown_ramp_s",
        "part_ramp_s" => "control.part_ramp_s",
        "floor_settle_s" => "control.floor_settle_s",
        "full_level_duration_s" => "schedule.full_level_s",
        "schedule" => "schedule.levels",
        "control_tick_s" => "acquisition.tick_ms",
        "law_guards" => "control.law_guards",
        "law" => "control.law",
        "tracking_hz" => "control.tracking.hz",
        "df_hz" | "lines" | "nfft" | "blocks_per_window" => "acquisition",
        other => other,
    }
}

/// The largest amplitude a tone's profile asks for over `[lo, hi]`.
fn peak_over(a: &ToneAmplitude, lo: f64, hi: f64) -> f64 {
    const N: usize = 256;
    (0..=N)
        .filter_map(|k| {
            let f = if lo > 0.0 && hi > lo {
                lo * (hi / lo).powf(k as f64 / N as f64)
            } else {
                lo
            };
            a.g_pk_at(f)
        })
        .fold(0.0, f64::max)
}

/// A wire schedule as the core states it. A sweep with no `end` sweeps until
/// its window closes: an unbounded duration.
pub fn core_schedule(s: &WireSchedule) -> PartSchedule {
    match s {
        WireSchedule::Fixed { freq_hz, end } => PartSchedule::Fixed {
            hz: *freq_hz,
            for_s: match end {
                Some(DwellEnd::Seconds(x)) => Some(*x),
                _ => None,
            },
            for_cycles: match end {
                Some(DwellEnd::Cycles(c)) => Some(*c),
                _ => None,
            },
        },
        WireSchedule::Sweep {
            band,
            rate,
            start_hz,
            direction,
            sweeps,
            duration_s,
        } => {
            let (law, r) = match *rate {
                SweepRate::LogOctPerMin(x) => (SweepLaw::LogOctPerMin, x),
                SweepRate::LinearHzPerSec(x) => (SweepLaw::LinearHzPerS, x),
            };
            PartSchedule::Sweep {
                lo_hz: band.0,
                hi_hz: band.1,
                start_hz: start_hz.unwrap_or(band.0),
                direction: *direction,
                law,
                rate: r,
                target: match (*sweeps, *duration_s) {
                    (Some(n), None) => PartSweepTarget::Sweeps { n },
                    (None, Some(s)) => PartSweepTarget::Duration { s },
                    (Some(sweeps), Some(duration_s)) => {
                        PartSweepTarget::WhicheverFirst { sweeps, duration_s }
                    }
                    (None, None) => PartSweepTarget::Duration { s: f64::INFINITY },
                },
            }
        }
        WireSchedule::Follow { of, ratio } => PartSchedule::Follow {
            of: of.clone(),
            ratio: *ratio,
        },
        WireSchedule::Segments => PartSchedule::Segments {
            segments: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------------------
// From the core
// ---------------------------------------------------------------------------------

fn band_json(b: (f64, f64)) -> Value {
    json!([num(b.0), num(b.1)])
}

fn schedule_json(s: &PartSchedule) -> Value {
    match s {
        PartSchedule::Fixed {
            hz,
            for_s,
            for_cycles,
        } => json!({
            "kind": "fixed",
            "freq_hz": num(*hz),
            "end": match (for_s, for_cycles) {
                (Some(x), _) => json!({"seconds": num(*x)}),
                (None, Some(c)) => json!({"cycles": c}),
                _ => Value::Null,
            },
        }),
        PartSchedule::Sweep {
            lo_hz,
            hi_hz,
            start_hz,
            direction,
            law,
            rate,
            target,
        } => json!({
            "kind": "sweep",
            "band_hz": [num(*lo_hz), num(*hi_hz)],
            "rate": match law {
                SweepLaw::LogOctPerMin => json!({"law": "log", "oct_per_min": num(*rate)}),
                SweepLaw::LinearHzPerS => json!({"law": "linear", "hz_per_s": num(*rate)}),
            },
            "start_hz": num(*start_hz),
            "direction": match direction { Direction::Up => "up", Direction::Down => "down" },
            "end": match *target {
                PartSweepTarget::Sweeps { n } => json!({"sweeps": n, "duration_s": null}),
                PartSweepTarget::Duration { s } if !s.is_finite() => Value::Null,
                PartSweepTarget::Duration { s } => json!({"sweeps": null, "duration_s": num(s)}),
                PartSweepTarget::WhicheverFirst { sweeps, duration_s } => {
                    json!({"sweeps": sweeps, "duration_s": num(duration_s)})
                }
            },
        }),
        PartSchedule::Follow { of, ratio } => {
            json!({"kind": "follow", "of": of, "ratio": num(*ratio)})
        }
        PartSchedule::Segments { .. } => json!({"kind": "segments"}),
    }
}

/// The wire configuration of a core configuration. `extras` carries what the core
/// has no place for yet (see [`WireMixed::to_core`]); its own values for every
/// field the core does carry are ignored.
pub fn to_wire(core: &MixedConfig, extras: &WireMixed) -> Value {
    let rw = &extras.random;
    let parts: Vec<Value> = core
        .parts
        .iter()
        .zip(&extras.parts)
        .map(|(p, x)| {
            let mut m = Map::new();
            m.insert("id".into(), Value::from(p.id.as_str()));
            if !p.name.is_empty() {
                m.insert("name".into(), Value::from(p.name.as_str()));
            }
            m.insert("kind".into(), Value::from(p.kind()));
            m.insert("enabled".into(), Value::from(p.enabled));
            m.insert(
                "window".into(),
                json!({"on_s": num(p.window.on_s), "off_s": p.window.off_s.map_or(Value::Null, num)}),
            );
            match &p.body {
                PartBody::Floor(f) => {
                    let rows: Vec<Value> = f
                        .breakpoints
                        .rows
                        .iter()
                        .map(|r| {
                            let mut o = Map::new();
                            o.insert("freq_hz".into(), num(r.freq_hz));
                            match r.level {
                                LevelSpec::Psd(v) => o.insert("psd_g2_hz".into(), num(v)),
                                LevelSpec::SlopeDbPerOct(v) => o.insert("slope_db_oct".into(), num(v)),
                            };
                            if let Some(b) = r.alarm_db {
                                o.insert("alarm_db".into(), band_json(b));
                            }
                            if let Some(b) = r.abort_db {
                                o.insert("abort_db".into(), band_json(b));
                            }
                            Value::Object(o)
                        })
                        .collect();
                    m.insert("breakpoints".into(), Value::Array(rows));
                    m.insert("alarm_db".into(), band_json(f.alarm_db));
                    m.insert("abort_db".into(), band_json(f.abort_db));
                }
                PartBody::Tone(t) => {
                    m.insert("schedule".into(), schedule_json(&t.schedule));
                    match &t.amplitude {
                        ToneAmplitude::Constant { g_pk } => {
                            m.insert("amplitude_g".into(), num(*g_pk));
                        }
                        ToneAmplitude::Profile { rows } => {
                            let label = match &x.body {
                                WireBody::Tone {
                                    amplitude: WireAmplitude::Profile(pr),
                                    ..
                                } => pr.label.clone(),
                                _ => String::new(),
                            };
                            let pr = SineProfile {
                                label,
                                rows: rows.clone(),
                                ..SineProfile::default()
                            };
                            m.insert("profile".into(), crate::sine::profile::profile_json(&pr));
                        }
                    }
                    m.insert("alarm_db".into(), band_json(t.alarm_db));
                    m.insert("abort_db".into(), band_json(t.abort_db));
                }
                PartBody::Narrowband(b) => {
                    m.insert("schedule".into(), schedule_json(&b.schedule));
                    m.insert("level_g2_hz".into(), num(b.level_g2_hz));
                    match b.width {
                        BandWidth::Hz { hz } => m.insert("width_hz".into(), num(hz)),
                        BandWidth::Share { share } => m.insert("width_share".into(), num(share)),
                    };
                    m.insert("alarm_db".into(), band_json(b.alarm_db));
                    m.insert("abort_db".into(), band_json(b.abort_db));
                }
                PartBody::Unknown { kind, fields } => {
                    m.insert("kind".into(), Value::from(kind.as_str()));
                    for (k, v) in fields {
                        m.insert(k.clone(), v.clone());
                    }
                }
            }
            Value::Object(m)
        })
        .collect();
    let s = &core.schedule;
    let last = s.table_db.len().saturating_sub(1);
    let levels: Vec<Value> = s
        .table_db
        .iter()
        .enumerate()
        .map(|(i, db)| {
            if i < last {
                json!({"db": num(*db), "dwell_s": num(s.dwell_s.get(i).copied().unwrap_or(0.0))})
            } else {
                json!({"db": num(*db)})
            }
        })
        .collect();
    let rc = &rw.control;
    let id = &core.identification;
    let lim = &core.limits;
    let g = core.law_guards.clone().unwrap_or_default();
    let composite = match core.control.mode {
        hertzforge_random::CompositeMode::WeightedAvg => "weighted_avg",
        hertzforge_random::CompositeMode::Min => "min",
        hertzforge_random::CompositeMode::Max => "max",
    };
    let pick = |p: &LawPick| json!({"id": p.id, "params": Value::Object(p.params.clone())});
    let mut out = json!({
        "title": extras.title,
        "parts": parts,
        "exclusion_hz": num(core.exclusion_hz),
        "acquisition": {
            "fs_hz": num(rw.fs_hz),
            "n_pts": core.nfft.unwrap_or(rw.n_pts as usize),
            "n_avg": core.blocks_per_window,
            "tick_ms": num(round(core.control_tick_s.unwrap_or(DEFAULT_TICK_MS / 1000.0) * 1000.0, 9)),
        },
        "schedule": {
            "mode": crate::random::view::mode_name(s.mode),
            "levels": levels,
            "full_level_s": num(core.full_level_duration_s.unwrap_or(0.0)),
        },
        "control": {
            "composite": composite,
            "weights": core.control.channels.iter()
                .map(|c| json!({"channel": c.channel_index, "weight": num(c.weight)}))
                .collect::<Vec<_>>(),
            "control_dof": num(core.control_dof),
            "damping": num(rc.damping),
            "max_lines_out": lim.max_lines_out,
            "max_loops_out": lim.divergence_loops,
            "max_sigma": lim.drive_clip_sigma.map_or(Value::Null, num),
            "drive_ceiling_vrms": num(lim.drive_ceiling_vrms.unwrap_or(rc.drive_ceiling_vrms)),
            "drive_abort_vrms": lim.drive_abort_vrms.map_or(Value::Null, num),
            "coherence_floor": num(id.coherence_floor),
            "review_after_sysid": id.review_hold,
            "pretest": {
                "averages": rc.pretest_averages,
                "level_db": num(id.pretest_reference_db.unwrap_or(rc.pretest_level_db)),
                "max_vrms": id.pretest_max_vrms.map_or(Value::Null, num),
                "start_vrms": num(id.pretest_start_vrms.unwrap_or(rc.pretest_start_vrms)),
                "shaped": id.shaped_pretest,
            },
            "soft_start_db": num(core.soft_start_db),
            "soft_start_relax_db": num(core.soft_start_relax_db),
            "level_ramp_s": num(core.level_ramp_s),
            "shutdown_ramp_s": num(core.shutdown_ramp_s),
            "part_ramp_s": num(core.part_ramp_s),
            "floor_settle_s": num(core.floor_settle_s),
            "trust_coherence": num(id.trust_coherence),
            "trust_h_floor_frac": num(id.trust_h_floor_frac),
            "tracking": {"kind": "fixed", "hz": num(core.tracking_hz)},
            "law": {"floor": pick(&core.law.floor), "tones": pick(&core.law.tones)},
            "law_guards": {
                "floor": {
                    "law_budget": num(g.floor.budget_fraction),
                    "law_misses": g.floor.max_misses,
                    "max_change_db": num(g.floor.max_change_db),
                },
                "tones": {
                    "law_budget": num(g.tones.budget_fraction),
                    "law_misses": g.tones.max_misses,
                    "max_step_db": num(g.tones.max_step_db),
                    "max_rise_db_per_s": num(g.tones.max_rise_db_per_s),
                    "divergence_s": num(g.tones.divergence_s),
                    "fault_debounce_ticks": extras.guards_tones.fault_debounce_ticks,
                },
            },
        },
        "limits": {
            "drive_vpk_ceiling": num(lim.drive_vpk_ceiling),
            "drive_vpk_abort": num(lim.drive_vpk_abort.unwrap_or(lim.drive_vpk_ceiling)),
            "kinematic_monitor": lim.kinematic_monitor,
            "channels": rw.limits.iter().map(|l| json!({
                "channel": l.channel,
                "kind": if l.force { "force" } else { "response" },
                "rows": l.rows.iter().map(|(f, c)| json!({"freq_hz": num(*f), "ceiling": num(*c)})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        },
        "operator": extras.operator,
        "article": extras.article,
        "notes": extras.notes,
        "specification": extras.specification.as_ref().map_or(Value::Null, |(d, c)| {
            json!({"document": d, "clause": c})
        }),
        "record": {"streams": extras.record_streams},
        "metadata": core.metadata.iter()
            .map(|m| json!({"key": m.key, "label": m.label, "value": m.value}))
            .collect::<Vec<_>>(),
    });
    if let Some(shaker) = &core.shaker {
        let mm = &core.moving_mass;
        out["feasibility"] = json!({
            "shaker": serde_json::to_value(shaker).unwrap_or(Value::Null),
            "moving_mass_lb": {
                "armature": num(mm.armature_lb),
                "slip_table": num(mm.slip_table_lb),
                "drive_adapter": num(mm.drive_adapter_lb),
                "fixturing": num(mm.fixturing_lb),
                "test_article": num(mm.test_article_lb),
            },
            "crest_factor": num(core.crest_factor),
        });
    }
    out
}

/// The refusal of a configuration with errors.
pub fn config_invalid(report: &Report) -> MethodError {
    crate::shock::config::config_invalid(report)
}

/// `law_not_checked` for a role on a hardware station, with why when known.
pub fn law_not_checked(role: &str, name: &str, why: Option<&str>) -> MethodError {
    let why = why.map_or_else(String::new, |w| format!(" ({w})"));
    MethodError::named(
        ErrorCode::Refused,
        "law_not_checked",
        format!(
            "The {} law \"{name}\" is not cleared for hardware in Mixed Mode{why}; it needs a \
             passed check in its own world and a passed mixed check in this role.",
            if role == "floor" { "floor" } else { "tone" }
        ),
    )
    .with_detail(json!({"role": role}))
}
