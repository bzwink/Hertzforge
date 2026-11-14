//! The MIMO Random setup on the wire (schema/PROTOCOL.md, "MIMO Random Setup"):
//! parsed and checked against the channel table and the law catalog.
//!
//! Parsing refuses a setup whose shape is wrong (`bad_params`, naming the field).
//! Everything about its content (unknown channels, a transformation of the wrong
//! size, a law that does not admit the test's shape) is a validation item, so the
//! operator sees every problem at once in the validation report.

use crate::protocol::{num, ErrorCode, MethodError};
use crate::table::{unit_name, Item, Report};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_law_host::{MimoManifest, ParamKind};
use hertzforge_random::RunMode;
use hertzforge_sdm::law::MimoShape;
use serde_json::{json, Map, Value};

/// The most complex values an inline reference may carry; a larger reference
/// travels as a reference file.
pub const MAX_INLINE_VALUES: usize = 262_144;
/// The most control channels and drives (design section 3).
pub const MAX_CONTROLS: usize = 72;
pub const MAX_DRIVES: usize = 24;
/// The law used when the setup names none.
pub const DEFAULT_LAW: &str = "pinv_closed";

/// One control channel: its table row and its tolerance bands, `[below, above]` in
/// positive dB.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlRow {
    pub channel: u32,
    pub label: String,
    pub unit: String,
    pub alarm_db: [f64; 2],
    pub abort_db: [f64; 2],
}

/// One drive: its table row, its voltage ceiling and peak, and the shaker behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct DriveRow {
    pub channel: u32,
    pub label: String,
    pub ceiling_vrms: f64,
    pub abort_vrms: Option<f64>,
    /// The output's peak voltage: the synthesized stream is clipped here. Defaults
    /// to [`DEFAULT_CREST`] times the ceiling, at most 10 V.
    pub peak_v: f64,
    /// The shaker record (the shaker database's form) for feasibility; `None`: not
    /// evaluated.
    pub shaker: Option<Value>,
    /// This shaker's own moving mass (armature, adapter, fixturing), lb.
    pub moving_mass_lb: f64,
}

/// One limit channel: a monitored response the core holds under a ceiling.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitRow {
    pub channel: u32,
    pub label: String,
    /// The ceiling on the channel's PSD at full level, units squared per hertz.
    pub ceiling: f64,
    /// A measured PSD this many dB above the ceiling on any in-band line aborts.
    pub abort_margin_db: f64,
}

/// A controlled quantity's alarm and abort bands when none are given,
/// `[below, above]` dB.
pub const DEFAULT_ALARM_DB: [f64; 2] = [3.0, 3.0];
pub const DEFAULT_ABORT_DB: [f64; 2] = [6.0, 6.0];

/// The crest factor a drive's default peak assumes.
pub const DEFAULT_CREST: f64 = 3.0;
/// The service's per-loop change caps on each drive's PSD, dB.
pub const DEFAULT_CAP_UP_DB: f64 = 3.5;
pub const DEFAULT_CAP_DOWN_DB: f64 = 6.0;
/// The largest rise cap an operator may set, dB.
pub const MAX_CAP_UP_DB: f64 = 6.0;
/// The law call's time budget as a fraction of the loop, and the unusable calls in
/// a row that stop the run.
pub const DEFAULT_LAW_BUDGET: f64 = 0.25;
pub const DEFAULT_LAW_MISSES: u32 = 3;
/// The soft start's depth when the setup gives none, dB (single-output Random's).
pub const DEFAULT_SOFT_START_DB: f64 = 9.0;
/// The FRF update's blend weight when the FRF is updated during the run.
pub const DEFAULT_FRF_BLEND: f64 = 0.5;

/// A real matrix, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct RealMatrix {
    pub rows: usize,
    pub cols: usize,
    pub values: Vec<f64>,
}

/// Where control happens: on the control channels, or on virtual degrees of
/// freedom through the response transformation (and, optionally, virtual drives
/// through the drive transformation).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlSpace {
    Physical,
    Virtual {
        dofs: Vec<String>,
        /// Degrees of freedom by control channels.
        response: RealMatrix,
        /// Drives by virtual drives, when the drives are transformed too.
        drive: Option<RealMatrix>,
    },
}

/// The reference spectral density matrix.
#[derive(Debug, Clone, PartialEq)]
pub enum Reference {
    /// Small references travel in the setup: one packed Hermitian matrix per line
    /// (upper triangle, row-major, lines in order), real and imaginary parts apart.
    Inline {
        freqs_hz: Vec<f64>,
        n: usize,
        re: Vec<f64>,
        im: Vec<f64>,
    },
    /// Large references travel by path: a native reference file on the computer
    /// that runs the test, named with the SHA-256 of its bytes so the run reads
    /// exactly the file the operator chose.
    File { path: String, sha256: String },
}

/// A law and its parameters as the setup names them.
#[derive(Debug, Clone, PartialEq)]
pub struct LawSetup {
    pub id: String,
    pub params: Map<String, Value>,
}

/// Estimation of the spectral matrices each control loop.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimation {
    pub fs_hz: f64,
    pub n_pts: usize,
    pub frames_per_loop: usize,
    pub window: String,
    pub overlap: f64,
    pub memory_loops: usize,
}

/// System identification and its gates.
#[derive(Debug, Clone, PartialEq)]
pub struct Identification {
    pub level_db: f64,
    pub frames: usize,
    pub noise_frames: usize,
    pub coherence_floor: f64,
    pub condition_limit: f64,
    pub review: bool,
}

/// The loop's own settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    /// `true`: the FRF is re-estimated during the run; `false`: held from
    /// identification.
    pub frf_update: bool,
    /// Under update, the weight of each loop's estimate against identification's.
    pub frf_blend: f64,
    pub level_ramp_s: f64,
    pub shutdown_ramp_s: f64,
    pub max_loops_out: u32,
    /// The core's per-loop rise and fall caps on each drive's PSD, dB.
    pub cap_up_db: f64,
    pub cap_down_db: f64,
    /// The law call's time budget, a fraction of the loop, and the unusable calls
    /// in a row that stop the run.
    pub law_budget: f64,
    pub law_misses: u32,
    /// The soft start: dB below the lowest scheduled level the loop engages at
    /// before it ramps to the first rung (0: none).
    pub soft_start_db: f64,
}

/// The level schedule, as single-output Random's.
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub mode: RunMode,
    /// `(level_db, dwell_s)`; the last level is full level.
    pub levels: Vec<(f64, Option<f64>)>,
    pub full_level_s: f64,
}

/// The operator's words for the run's record, and whether it keeps its streams.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordWords {
    /// Who ran it, on what, and notes.
    pub operator: String,
    pub article: String,
    pub notes: String,
    /// The test document and its clause, when one is named.
    pub specification: Option<(String, String)>,
    /// `false`: the record keeps no time histories (`record.streams`).
    pub streams: bool,
}

impl Default for RecordWords {
    fn default() -> Self {
        RecordWords {
            operator: String::new(),
            article: String::new(),
            notes: String::new(),
            specification: None,
            streams: true,
        }
    }
}

/// A setup as accepted: every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct MimoSetup {
    pub title: String,
    pub controls: Vec<ControlRow>,
    pub drives: Vec<DriveRow>,
    /// Monitored responses held under a ceiling.
    pub limits: Vec<LimitRow>,
    /// The test article's mass, split across the shakers for feasibility, lb.
    pub article_mass_lb: f64,
    pub space: ControlSpace,
    /// Under virtual control, each degree of freedom's alarm and abort bands,
    /// `([below, above], [below, above])` dB, in the order of the degrees of
    /// freedom (empty under physical control, where each control row carries its
    /// own).
    pub dof_bands: Vec<([f64; 2], [f64; 2])>,
    pub reference: Reference,
    pub law: LawSetup,
    pub estimation: Estimation,
    pub identification: Identification,
    pub control: Control,
    pub schedule: Schedule,
    /// `None`: the selected station. Otherwise the station kind the setup was made
    /// for, which must be the one selected.
    pub backend: Option<String>,
    /// The operator's words for the run's record and its streams.
    pub record: RecordWords,
    /// The setup as sent, for `mimo_random.status` readers and a reset.
    pub raw: Value,
}

impl MimoSetup {
    /// The controlled quantities: the control channels, or the virtual degrees of
    /// freedom.
    pub fn controlled(&self) -> usize {
        match &self.space {
            ControlSpace::Physical => self.controls.len(),
            ControlSpace::Virtual { dofs, .. } => dofs.len(),
        }
    }

    /// The labels of the controlled quantities, in order.
    pub fn controlled_labels(&self) -> Vec<String> {
        match &self.space {
            ControlSpace::Physical => self.controls.iter().map(|c| c.label.clone()).collect(),
            ControlSpace::Virtual { dofs, .. } => dofs.clone(),
        }
    }

    /// The alarm and abort bands of controlled quantity `i`, `[below, above]` dB:
    /// its control row's under physical control, its degree of freedom's under
    /// virtual control (the reference and its tolerances are in the control
    /// space).
    pub fn band_of(&self, i: usize) -> ([f64; 2], [f64; 2]) {
        let own = match &self.space {
            ControlSpace::Physical => self.controls.get(i).map(|c| (c.alarm_db, c.abort_db)),
            ControlSpace::Virtual { .. } => self.dof_bands.get(i).copied(),
        };
        own.unwrap_or((DEFAULT_ALARM_DB, DEFAULT_ABORT_DB))
    }

    /// The test's shape, controls against drives.
    pub fn shape(&self) -> MimoShape {
        MimoShape::of(self.controlled(), self.drives.len())
    }

    /// Line spacing, Hz.
    pub fn df_hz(&self) -> f64 {
        self.estimation.fs_hz / self.estimation.n_pts as f64
    }

    /// One control loop's acquisition time, s (frames without overlap counted
    /// as the hop between them).
    pub fn loop_s(&self) -> f64 {
        let hop = (1.0 - self.estimation.overlap) * self.estimation.n_pts as f64;
        (self.estimation.n_pts as f64 + hop * (self.estimation.frames_per_loop as f64 - 1.0))
            / self.estimation.fs_hz
    }

    /// The reference's lines: an inline reference's own, otherwise every line of
    /// the grid up to the analysis bandwidth (`fs / 2.56`).
    pub fn lines_hz(&self) -> Vec<f64> {
        match &self.reference {
            Reference::Inline { freqs_hz, .. } => freqs_hz.clone(),
            Reference::File { .. } => {
                let df = self.df_hz();
                let top = self.estimation.fs_hz / 2.56;
                (1..)
                    .map(|k| k as f64 * df)
                    .take_while(|f| *f <= top + 1e-9)
                    .collect()
            }
        }
    }
}

/// The name of a shape on the wire.
pub fn shape_name(s: MimoShape) -> &'static str {
    match s {
        MimoShape::Square => "square",
        MimoShape::MoreControlsThanDrives => "more_controls_than_drives",
        MimoShape::FewerControlsThanDrives => "fewer_controls_than_drives",
    }
}

// ---- parsing helpers ---------------------------------------------------------------

fn bad(path: &str, message: impl Into<String>) -> MethodError {
    MethodError::bad_params(path, message)
}

fn obj<'a>(v: &'a Value, path: &str) -> Result<&'a Map<String, Value>, MethodError> {
    v.as_object()
        .ok_or_else(|| bad(path, format!("\"{path}\" is an object")))
}

fn opt<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    m.get(key).filter(|v| !v.is_null())
}

fn number(m: &Map<String, Value>, key: &str, path: &str) -> Result<Option<f64>, MethodError> {
    match opt(m, key) {
        None => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite())
            .map(Some)
            .ok_or_else(|| bad(&format!("{path}.{key}"), format!("\"{key}\" is a number"))),
    }
}

fn whole(m: &Map<String, Value>, key: &str, path: &str) -> Result<Option<u64>, MethodError> {
    match opt(m, key) {
        None => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            bad(
                &format!("{path}.{key}"),
                format!("\"{key}\" is a whole number from 0"),
            )
        }),
    }
}

fn text<'a>(
    m: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<&'a str>, MethodError> {
    match opt(m, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(&format!("{path}.{key}"), format!("\"{key}\" is text"))),
    }
}

fn boolean(m: &Map<String, Value>, key: &str, path: &str) -> Result<Option<bool>, MethodError> {
    match opt(m, key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(
            &format!("{path}.{key}"),
            format!("\"{key}\" is true or false"),
        )),
    }
}

fn numbers(v: &Value, path: &str) -> Result<Vec<f64>, MethodError> {
    let a = v
        .as_array()
        .ok_or_else(|| bad(path, format!("\"{path}\" is a list of numbers")))?;
    a.iter()
        .map(|x| {
            x.as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| bad(path, format!("\"{path}\" holds finite numbers only")))
        })
        .collect()
}

fn band(
    m: &Map<String, Value>,
    key: &str,
    path: &str,
    default: [f64; 2],
) -> Result<[f64; 2], MethodError> {
    match opt(m, key) {
        None => Ok(default),
        Some(v) => {
            let p = format!("{path}.{key}");
            let x = numbers(v, &p)?;
            if x.len() != 2 || x.iter().any(|d| *d <= 0.0) {
                return Err(bad(
                    &p,
                    format!("\"{key}\" is [below, above] in positive dB, such as [3, 3]"),
                ));
            }
            Ok([x[0], x[1]])
        }
    }
}

fn matrix(v: &Value, path: &str) -> Result<RealMatrix, MethodError> {
    let m = obj(v, path)?;
    let rows = whole(m, "rows", path)?.unwrap_or(0) as usize;
    let cols = whole(m, "cols", path)?.unwrap_or(0) as usize;
    let values = match opt(m, "values") {
        Some(v) => numbers(v, &format!("{path}.values"))?,
        None => Vec::new(),
    };
    if rows == 0 || cols == 0 {
        return Err(bad(
            path,
            "a transformation names its \"rows\" and \"cols\", each at least 1",
        ));
    }
    if values.len() != rows * cols {
        return Err(bad(
            &format!("{path}.values"),
            format!(
                "a {rows} by {cols} transformation holds {} values, row by row; {} were sent",
                rows * cols,
                values.len()
            ),
        ));
    }
    Ok(RealMatrix { rows, cols, values })
}

fn matrix_json(m: &RealMatrix) -> Value {
    json!({"rows": m.rows, "cols": m.cols, "values": m.values.iter().map(|x| num(*x)).collect::<Vec<_>>()})
}

/// What a setup's parse needs from the service: the channel table and the law
/// catalog's entries for the multiple-output world.
pub struct SetupContext<'a> {
    pub table: &'a ChannelTable,
    pub selected_kind: &'a str,
    pub law: &'a dyn Fn(&str) -> Option<LawInfo>,
}

/// A law as the setup's checks see it.
#[derive(Debug, Clone)]
pub struct LawInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// `None`: a single-output law.
    pub mimo: Option<MimoManifest>,
}

/// Parse `setup` and check it against `ctx`. Returns the setup with every default
/// filled in and the items found, or `bad_params` when its shape is wrong.
pub fn parse(setup: &Value, ctx: &SetupContext<'_>) -> Result<(MimoSetup, Report), MethodError> {
    let s = obj(setup, "setup")?;
    let mut report = Report::default();
    let title = text(s, "title", "setup")?
        .unwrap_or("MIMO Random")
        .to_owned();

    // Channels.
    let ch = opt(s, "channels").ok_or_else(|| {
        bad(
            "setup.channels",
            "a setup names its control channels and drives (\"channels\")",
        )
    })?;
    let ch = obj(ch, "setup.channels")?;
    let row_of = |index: u32| ctx.table.channels.iter().find(|c| c.index == index);
    let mut controls = Vec::new();
    let list = opt(ch, "controls")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("setup.channels.controls", "\"controls\" is a list"))?;
    for (k, c) in list.iter().enumerate() {
        let path = format!("setup.channels.controls[{k}]");
        let c = obj(c, &path)?;
        let channel = whole(c, "channel", &path)?
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| bad(&format!("{path}.channel"), "each control names its channel"))?;
        let alarm_db = band(c, "alarm_db", &path, DEFAULT_ALARM_DB)?;
        let abort_db = band(c, "abort_db", &path, DEFAULT_ABORT_DB)?;
        if alarm_db[0] > abort_db[0] || alarm_db[1] > abort_db[1] {
            report.warnings.push(Item::field(
                "alarm_wider_than_abort",
                format!("{path}.alarm_db"),
                format!("Channel {channel}'s alarm band is wider than its abort band; narrow the alarm band so it warns before the abort."),
            ));
        }
        let (label, unit) = match row_of(channel) {
            Some(r) if r.enabled && r.role == ChannelRole::Control => (
                if r.description.trim().is_empty() {
                    format!("Channel {channel}")
                } else {
                    r.description.trim().to_owned()
                },
                unit_name(&r.units),
            ),
            _ => {
                report.errors.push(Item::channel(
                    "control_unknown_channel",
                    Some(channel),
                    format!("Channel {channel} is not an enabled control row of the channel table; set its role to control, or remove it from the setup."),
                ));
                (format!("Channel {channel}"), "g".to_owned())
            }
        };
        if controls.iter().any(|x: &ControlRow| x.channel == channel) {
            report.errors.push(Item::channel(
                "duplicate_channel",
                Some(channel),
                format!("Channel {channel} is listed twice as a control; list each channel once."),
            ));
        }
        controls.push(ControlRow {
            channel,
            label,
            unit,
            alarm_db,
            abort_db,
        });
    }
    let mut drives = Vec::new();
    let list = opt(ch, "drives")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("setup.channels.drives", "\"drives\" is a list"))?;
    for (k, d) in list.iter().enumerate() {
        let path = format!("setup.channels.drives[{k}]");
        let d = obj(d, &path)?;
        let channel = whole(d, "channel", &path)?
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| bad(&format!("{path}.channel"), "each drive names its channel"))?;
        let ceiling_vrms = number(d, "ceiling_vrms", &path)?.ok_or_else(|| {
            bad(
                &format!("{path}.ceiling_vrms"),
                "each drive has its own voltage ceiling (\"ceiling_vrms\")",
            )
        })?;
        let abort_vrms = number(d, "abort_vrms", &path)?;
        if !(ceiling_vrms > 0.0 && ceiling_vrms <= 10.0) {
            report.errors.push(Item::field(
                "out_of_range",
                format!("{path}.ceiling_vrms"),
                "A drive ceiling is above 0 and at most 10 V rms.",
            ));
        }
        if abort_vrms.is_some_and(|a| a <= ceiling_vrms) {
            report.errors.push(Item::field(
                "out_of_range",
                format!("{path}.abort_vrms"),
                "A drive abort level is above the drive's ceiling; raise it or leave it empty.",
            ));
        }
        let label = match row_of(channel) {
            Some(r) if r.enabled && r.role == ChannelRole::Drive => {
                if r.description.trim().is_empty() {
                    format!("Drive {channel}")
                } else {
                    r.description.trim().to_owned()
                }
            }
            _ => {
                report.errors.push(Item::channel(
                    "drive_unknown_channel",
                    Some(channel),
                    format!("Channel {channel} is not an enabled drive row of the channel table; set its role to drive, or remove it from the setup."),
                ));
                format!("Drive {channel}")
            }
        };
        if drives.iter().any(|x: &DriveRow| x.channel == channel) {
            report.errors.push(Item::channel(
                "duplicate_channel",
                Some(channel),
                format!("Channel {channel} is listed twice as a drive; list each channel once."),
            ));
        }
        let peak_v =
            number(d, "peak_v", &path)?.unwrap_or_else(|| (DEFAULT_CREST * ceiling_vrms).min(10.0));
        if !(peak_v >= ceiling_vrms && peak_v <= 10.0) {
            report.errors.push(Item::field(
                "out_of_range",
                format!("{path}.peak_v"),
                "A drive's peak is at least its rms ceiling and at most 10 V.",
            ));
        }
        let shaker = match opt(d, "shaker") {
            None => None,
            Some(v @ Value::Object(_)) => {
                if let Err(e) =
                    serde_json::from_value::<hertzforge_shaker_db::ShakerRecord>(v.clone())
                {
                    return Err(MethodError::named(
                        ErrorCode::BadRequest,
                        "shaker_record_invalid",
                        format!(
                            "{path}.shaker: the shaker record does not parse ({e}); give the record in the shaker database's form, or leave it out to skip that drive's feasibility check"
                        ),
                    ));
                }
                Some(v.clone())
            }
            Some(_) => {
                return Err(bad(
                    &format!("{path}.shaker"),
                    "a drive's \"shaker\" is a shaker record (an object)",
                ))
            }
        };
        let moving_mass_lb = number(d, "moving_mass_lb", &path)?.unwrap_or(0.0);
        if moving_mass_lb < 0.0 {
            report.errors.push(Item::field(
                "out_of_range",
                format!("{path}.moving_mass_lb"),
                "A moving mass is 0 or more.",
            ));
        }
        drives.push(DriveRow {
            channel,
            label,
            ceiling_vrms,
            abort_vrms,
            peak_v,
            shaker,
            moving_mass_lb,
        });
    }
    let mut limits = Vec::new();
    if let Some(list) = opt(ch, "limits") {
        let list = list
            .as_array()
            .ok_or_else(|| bad("setup.channels.limits", "\"limits\" is a list"))?;
        for (k, l) in list.iter().enumerate() {
            let path = format!("setup.channels.limits[{k}]");
            let l = obj(l, &path)?;
            let channel = whole(l, "channel", &path)?
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| bad(&format!("{path}.channel"), "each limit names its channel"))?;
            let ceiling = number(l, "ceiling", &path)?.ok_or_else(|| {
                bad(
                    &format!("{path}.ceiling"),
                    "each limit has its PSD ceiling (\"ceiling\")",
                )
            })?;
            let abort_margin_db = number(l, "abort_margin_db", &path)?.unwrap_or(3.0);
            if ceiling <= 0.0 {
                report.errors.push(Item::field(
                    "out_of_range",
                    format!("{path}.ceiling"),
                    "A limit's ceiling is above 0.",
                ));
            }
            if abort_margin_db <= 0.0 {
                report.errors.push(Item::field(
                    "out_of_range",
                    format!("{path}.abort_margin_db"),
                    "A limit's abort margin is above 0 dB.",
                ));
            }
            let label = match row_of(channel) {
                Some(r) if r.enabled && r.role == ChannelRole::Monitor => {
                    if r.description.trim().is_empty() {
                        format!("Channel {channel}")
                    } else {
                        r.description.trim().to_owned()
                    }
                }
                _ => {
                    report.errors.push(Item::channel(
                        "limit_unknown_channel",
                        Some(channel),
                        format!("Channel {channel} is not an enabled monitor row of the channel table; a limit is a monitored response. Set its role to monitor, or remove the limit."),
                    ));
                    format!("Channel {channel}")
                }
            };
            if limits.iter().any(|x: &LimitRow| x.channel == channel) {
                report.errors.push(Item::channel(
                    "duplicate_channel",
                    Some(channel),
                    format!("Channel {channel} has two limits; keep one."),
                ));
            }
            limits.push(LimitRow {
                channel,
                label,
                ceiling,
                abort_margin_db,
            });
        }
    }
    let article_mass_lb = number(ch, "article_mass_lb", "setup.channels")?.unwrap_or(0.0);
    if article_mass_lb < 0.0 {
        report.errors.push(Item::field(
            "out_of_range",
            "setup.channels.article_mass_lb",
            "The test article's mass is 0 or more.",
        ));
    }
    if controls.is_empty() {
        report.errors.push(Item::field(
            "no_control_channel",
            "setup.channels.controls",
            "A MIMO test needs at least one control channel; add the table's control rows.",
        ));
    }
    if drives.is_empty() {
        report.errors.push(Item::field(
            "no_drive_channel",
            "setup.channels.drives",
            "A MIMO test needs at least one drive; add the table's drive rows.",
        ));
    }
    if controls.len() > MAX_CONTROLS || drives.len() > MAX_DRIVES {
        report.errors.push(Item::field(
            "out_of_range",
            "setup.channels",
            format!(
                "A MIMO test has at most {MAX_CONTROLS} control channels and {MAX_DRIVES} drives."
            ),
        ));
    }

    // Control space.
    let space = match opt(s, "space") {
        None => ControlSpace::Physical,
        Some(v) => {
            let m = obj(v, "setup.space")?;
            match text(m, "kind", "setup.space")? {
                None | Some("physical") => ControlSpace::Physical,
                Some("virtual") => {
                    let dofs: Vec<String> = match opt(m, "dofs").and_then(Value::as_array) {
                        Some(a) => a
                            .iter()
                            .map(|d| d.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                            .ok_or_else(|| {
                                bad("setup.space.dofs", "each degree of freedom has a name")
                            })?,
                        None => {
                            return Err(bad(
                                "setup.space.dofs",
                                "virtual control names its degrees of freedom (\"dofs\")",
                            ))
                        }
                    };
                    let response = matrix(
                        opt(m, "response_transform").ok_or_else(|| {
                            bad(
                                "setup.space.response_transform",
                                "virtual control needs the response transformation",
                            )
                        })?,
                        "setup.space.response_transform",
                    )?;
                    let drive = opt(m, "drive_transform")
                        .map(|v| matrix(v, "setup.space.drive_transform"))
                        .transpose()?;
                    if response.rows != dofs.len() || response.cols != controls.len() {
                        report.errors.push(Item::field(
                            "transform_shape",
                            "setup.space.response_transform",
                            format!(
                                "The response transformation is {} by {}; it must be degrees of freedom by control channels, {} by {}.",
                                response.rows, response.cols, dofs.len(), controls.len()
                            ),
                        ));
                    }
                    if let Some(d) = &drive {
                        if d.rows != drives.len() {
                            report.errors.push(Item::field(
                                "transform_shape",
                                "setup.space.drive_transform",
                                format!(
                                    "The drive transformation has {} rows; it must have one per drive, {}.",
                                    d.rows,
                                    drives.len()
                                ),
                            ));
                        }
                    }
                    ControlSpace::Virtual {
                        dofs,
                        response,
                        drive,
                    }
                }
                Some(other) => {
                    return Err(bad(
                        "setup.space.kind",
                        format!("\"kind\" is \"physical\" or \"virtual\", not \"{other}\""),
                    ))
                }
            }
        }
    };

    // Tolerance bands per degree of freedom, under virtual control.
    let dof_bands = match (&space, opt(s, "space").and_then(Value::as_object)) {
        (ControlSpace::Virtual { dofs, .. }, Some(m)) => match opt(m, "dof_bands") {
            None => vec![(DEFAULT_ALARM_DB, DEFAULT_ABORT_DB); dofs.len()],
            Some(v) => {
                let list = v.as_array().ok_or_else(|| {
                    bad(
                        "setup.space.dof_bands",
                        "\"dof_bands\" lists one {\"alarm_db\",\"abort_db\"} per degree of freedom",
                    )
                })?;
                let mut out = Vec::new();
                for (k, b) in list.iter().enumerate() {
                    let path = format!("setup.space.dof_bands[{k}]");
                    let o = obj(b, &path)?;
                    let alarm = band(o, "alarm_db", &path, DEFAULT_ALARM_DB)?;
                    let abort = band(o, "abort_db", &path, DEFAULT_ABORT_DB)?;
                    if alarm[0] > abort[0] || alarm[1] > abort[1] {
                        report.warnings.push(Item::field(
                            "alarm_wider_than_abort",
                            &path,
                            format!("The alarm band of degree of freedom {} is wider than its abort band, so it aborts before it alarms; narrow the alarm band or widen the abort band.", dofs.get(k).map(String::as_str).unwrap_or("?")),
                        ));
                    }
                    out.push((alarm, abort));
                }
                if out.len() != dofs.len() {
                    report.errors.push(Item::field(
                        "transform_shape",
                        "setup.space.dof_bands",
                        format!(
                            "There are {} tolerance bands for {} degrees of freedom; give one per degree of freedom, in their order.",
                            out.len(),
                            dofs.len()
                        ),
                    ));
                }
                out
            }
        },
        _ => Vec::new(),
    };

    // Estimation.
    let e = match opt(s, "estimation") {
        Some(v) => obj(v, "setup.estimation")?.clone(),
        None => Map::new(),
    };
    let p = "setup.estimation";
    let estimation = Estimation {
        fs_hz: number(&e, "fs_hz", p)?.unwrap_or(8192.0),
        n_pts: whole(&e, "n_pts", p)?.unwrap_or(4096) as usize,
        frames_per_loop: whole(&e, "frames_per_loop", p)?.unwrap_or(8) as usize,
        window: text(&e, "window", p)?.unwrap_or("hann").to_owned(),
        overlap: number(&e, "overlap", p)?.unwrap_or(0.5),
        memory_loops: whole(&e, "memory_loops", p)?.unwrap_or(4) as usize,
    };
    if !estimation.n_pts.is_power_of_two() || estimation.n_pts < 64 {
        report.errors.push(Item::field(
            "n_pts_not_power_of_two",
            "setup.estimation.n_pts",
            "The frame length is a power of two from 64, such as 4096.",
        ));
    }
    let ranges: [(&str, f64, f64, f64); 4] = [
        ("fs_hz", estimation.fs_hz, 256.0, 204_800.0),
        (
            "frames_per_loop",
            estimation.frames_per_loop as f64,
            1.0,
            1024.0,
        ),
        ("overlap", estimation.overlap, 0.0, 0.75),
        ("memory_loops", estimation.memory_loops as f64, 1.0, 64.0),
    ];
    for (key, x, lo, hi) in ranges {
        if !(lo..=hi).contains(&x) {
            report.errors.push(Item::field(
                "out_of_range",
                format!("setup.estimation.{key}"),
                format!("\"{key}\" is from {} to {}.", num(lo), num(hi)),
            ));
        }
    }
    if !matches!(estimation.window.as_str(), "hann" | "rectangular") {
        report.errors.push(Item::field(
            "out_of_range",
            "setup.estimation.window",
            "The window is \"hann\" or \"rectangular\".",
        ));
    }

    // Reference.
    let r = opt(s, "reference").ok_or_else(|| {
        bad(
            "setup.reference",
            "a setup carries its reference, inline or as a reference file",
        )
    })?;
    let r = obj(r, "setup.reference")?;
    let reference = match text(r, "kind", "setup.reference")? {
        Some("inline") => {
            let p = "setup.reference";
            let freqs_hz = numbers(
                opt(r, "freqs_hz").unwrap_or(&Value::Null),
                "setup.reference.freqs_hz",
            )?;
            let n = whole(r, "n", p)?.unwrap_or(0) as usize;
            let re = numbers(opt(r, "re").unwrap_or(&Value::Null), "setup.reference.re")?;
            let im = numbers(opt(r, "im").unwrap_or(&Value::Null), "setup.reference.im")?;
            let packed = n * (n + 1) / 2;
            if re.len() > MAX_INLINE_VALUES {
                report.errors.push(Item::field(
                    "reference_too_large_inline",
                    "setup.reference",
                    format!("An inline reference holds at most {MAX_INLINE_VALUES} values; save this one as a reference file and send its path."),
                ));
            } else if n == 0 || re.len() != freqs_hz.len() * packed || im.len() != re.len() {
                report.errors.push(Item::field(
                    "reference_shape",
                    "setup.reference",
                    format!(
                        "An inline reference of {} lines and {n} channels carries {} real and {} imaginary values (the upper triangle of each line, row by row); {} and {} were sent.",
                        freqs_hz.len(), freqs_hz.len() * packed, freqs_hz.len() * packed, re.len(), im.len()
                    ),
                ));
            }
            if freqs_hz.len() < 8 {
                report.errors.push(Item::field(
                    "too_few_lines",
                    "setup.reference.freqs_hz",
                    "A reference has at least 8 lines.",
                ));
            }
            if freqs_hz.windows(2).any(|w| w[1] <= w[0])
                || freqs_hz.first().is_some_and(|f| *f <= 0.0)
            {
                report.errors.push(Item::field(
                    "frequency_not_ascending",
                    "setup.reference.freqs_hz",
                    "The reference's frequencies are above 0 and rise strictly.",
                ));
            }
            Reference::Inline {
                freqs_hz,
                n,
                re,
                im,
            }
        }
        Some("file") => {
            let p = "setup.reference";
            let path = text(r, "path", p)?
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    bad(
                        "setup.reference.path",
                        "a reference file is named by its path",
                    )
                })?
                .to_owned();
            let sha256 = text(r, "sha256", p)?.unwrap_or("").to_owned();
            if sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(bad(
                    "setup.reference.sha256",
                    "a reference file is named with the SHA-256 of its bytes, 64 lower-case hex digits",
                ));
            }
            Reference::File { path, sha256 }
        }
        _ => {
            return Err(bad(
                "setup.reference.kind",
                "\"kind\" is \"inline\" or \"file\"",
            ))
        }
    };
    let n_ref = match &reference {
        Reference::Inline { n, .. } => Some(*n),
        Reference::File { .. } => None,
    };
    let controlled = match &space {
        ControlSpace::Physical => controls.len(),
        ControlSpace::Virtual { dofs, .. } => dofs.len(),
    };
    if n_ref.is_some_and(|n| n != controlled && n != 0) {
        report.errors.push(Item::field(
            "reference_shape",
            "setup.reference.n",
            format!(
                "The reference is {} by {}; it must be {controlled} by {controlled}, one row per {}.",
                n_ref.unwrap_or(0),
                n_ref.unwrap_or(0),
                if matches!(space, ControlSpace::Physical) { "control channel" } else { "degree of freedom" }
            ),
        ));
    }

    // Law.
    let law = match opt(s, "law") {
        None => LawSetup {
            id: DEFAULT_LAW.into(),
            params: Map::new(),
        },
        Some(v) => {
            let m = obj(v, "setup.law")?;
            let id = text(m, "id", "setup.law")?
                .unwrap_or(DEFAULT_LAW)
                .to_owned();
            let params = match opt(m, "params") {
                None => Map::new(),
                Some(p) => obj(p, "setup.law.params")?.clone(),
            };
            LawSetup { id, params }
        }
    };
    check_law(
        &law,
        (ctx.law)(&law.id),
        MimoShape::of(controlled, drives.len()),
        controlled,
        &mut report,
    );

    // Identification.
    let i = match opt(s, "identification") {
        Some(v) => obj(v, "setup.identification")?.clone(),
        None => Map::new(),
    };
    let p = "setup.identification";
    let identification = Identification {
        level_db: number(&i, "level_db", p)?.unwrap_or(-20.0),
        frames: whole(&i, "frames", p)?.unwrap_or(32) as usize,
        noise_frames: whole(&i, "noise_frames", p)?.unwrap_or(16) as usize,
        coherence_floor: number(&i, "coherence_floor", p)?.unwrap_or(0.5),
        condition_limit: number(&i, "condition_limit", p)?.unwrap_or(1000.0),
        review: boolean(&i, "review", p)?.unwrap_or(true),
    };
    let ranges: [(&str, f64, f64, f64); 5] = [
        ("level_db", identification.level_db, -60.0, 0.0),
        ("frames", identification.frames as f64, 1.0, 4096.0),
        (
            "noise_frames",
            identification.noise_frames as f64,
            0.0,
            4096.0,
        ),
        ("coherence_floor", identification.coherence_floor, 0.0, 1.0),
        (
            "condition_limit",
            identification.condition_limit,
            1.0,
            1.0e12,
        ),
    ];
    for (key, x, lo, hi) in ranges {
        if !(lo..=hi).contains(&x) {
            report.errors.push(Item::field(
                "out_of_range",
                format!("setup.identification.{key}"),
                format!("\"{key}\" is from {} to {}.", num(lo), num(hi)),
            ));
        }
    }

    // Control.
    let c = match opt(s, "control") {
        Some(v) => obj(v, "setup.control")?.clone(),
        None => Map::new(),
    };
    let p = "setup.control";
    let frf_update = match text(&c, "frf", p)? {
        None | Some("hold") => false,
        Some("update") => true,
        Some(_) => {
            return Err(bad(
                "setup.control.frf",
                "\"frf\" is \"hold\" or \"update\"",
            ))
        }
    };
    let control = Control {
        frf_update,
        level_ramp_s: number(&c, "level_ramp_s", p)?.unwrap_or(3.0),
        shutdown_ramp_s: number(&c, "shutdown_ramp_s", p)?.unwrap_or(3.0),
        max_loops_out: whole(&c, "max_loops_out", p)?.unwrap_or(5) as u32,
        frf_blend: number(&c, "frf_blend", p)?.unwrap_or(DEFAULT_FRF_BLEND),
        cap_up_db: number(&c, "cap_up_db", p)?.unwrap_or(DEFAULT_CAP_UP_DB),
        cap_down_db: number(&c, "cap_down_db", p)?.unwrap_or(DEFAULT_CAP_DOWN_DB),
        law_budget: number(&c, "law_budget", p)?.unwrap_or(DEFAULT_LAW_BUDGET),
        law_misses: whole(&c, "law_misses", p)?.unwrap_or(DEFAULT_LAW_MISSES as u64) as u32,
        soft_start_db: number(&c, "soft_start_db", p)?.unwrap_or(DEFAULT_SOFT_START_DB),
    };
    for (key, x) in [
        ("level_ramp_s", control.level_ramp_s),
        ("shutdown_ramp_s", control.shutdown_ramp_s),
    ] {
        if !(0.0..=60.0).contains(&x) {
            report.errors.push(Item::field(
                "out_of_range",
                format!("setup.control.{key}"),
                format!("\"{key}\" is from 0 to 60."),
            ));
        }
    }
    let checks: [(&str, bool, &str); 6] = [
        (
            "soft_start_db",
            (0.0..=30.0).contains(&control.soft_start_db),
            "The soft start is from 0 (none) to 30 dB below the lowest scheduled level.",
        ),
        (
            "frf_blend",
            control.frf_blend > 0.0 && control.frf_blend <= 1.0,
            "The FRF update's blend is above 0 and at most 1.",
        ),
        (
            "cap_up_db",
            control.cap_up_db > 0.0 && control.cap_up_db <= MAX_CAP_UP_DB,
            "The rise cap is above 0 and at most 6 dB per loop.",
        ),
        (
            "cap_down_db",
            control.cap_down_db > 0.0 && control.cap_down_db <= 60.0,
            "The fall cap is above 0 and at most 60 dB per loop.",
        ),
        (
            "law_budget",
            control.law_budget > 0.0 && control.law_budget <= 1.0,
            "The law's time budget is a fraction of the loop, above 0 and at most 1.",
        ),
        (
            "law_misses",
            control.law_misses >= 1 && control.law_misses <= 100,
            "The misses before a stop are from 1 to 100.",
        ),
    ];
    for (key, ok, message) in checks {
        if !ok {
            report.errors.push(Item::field(
                "out_of_range",
                format!("setup.control.{key}"),
                message,
            ));
        }
    }

    // Schedule.
    let sc = opt(s, "schedule")
        .ok_or_else(|| bad("setup.schedule", "a setup carries its level schedule"))?;
    let sc = obj(sc, "setup.schedule")?;
    let mode = match text(sc, "mode", "setup.schedule")? {
        None | Some("auto") => RunMode::Auto,
        Some("manual") => RunMode::Manual,
        Some(_) => {
            return Err(bad(
                "setup.schedule.mode",
                "\"mode\" is \"auto\" or \"manual\"",
            ))
        }
    };
    let mut levels = Vec::new();
    let list = opt(sc, "levels")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("setup.schedule.levels", "\"levels\" is a list"))?;
    for (k, l) in list.iter().enumerate() {
        let path = format!("setup.schedule.levels[{k}]");
        let l = obj(l, &path)?;
        let db = number(l, "db", &path)?
            .ok_or_else(|| bad(&format!("{path}.db"), "each level has its \"db\""))?;
        levels.push((db, number(l, "dwell_s", &path)?));
    }
    if levels.is_empty() {
        report.errors.push(Item::field(
            "levels_empty",
            "setup.schedule.levels",
            "The schedule has at least one level, the last being full level.",
        ));
    }
    if levels.windows(2).any(|w| w[1].0 < w[0].0) {
        report.errors.push(Item::field(
            "levels_not_ascending",
            "setup.schedule.levels",
            "Levels rise from the first to the last.",
        ));
    }
    for (k, (_, dwell)) in levels
        .iter()
        .enumerate()
        .take(levels.len().saturating_sub(1))
    {
        if dwell.is_none() {
            report.errors.push(Item::field(
                "dwell_missing",
                format!("setup.schedule.levels[{k}].dwell_s"),
                "Every level but the last has a dwell time.",
            ));
        }
    }
    let schedule = Schedule {
        mode,
        levels,
        full_level_s: number(sc, "full_level_s", "setup.schedule")?.unwrap_or(300.0),
    };

    // Backend.
    let backend = match opt(s, "backend") {
        None => None,
        Some(v) => {
            let m = obj(v, "setup.backend")?;
            let kind = text(m, "kind", "setup.backend")?.unwrap_or("").to_owned();
            if !matches!(kind.as_str(), "sim" | "ni") {
                return Err(bad("setup.backend.kind", "\"kind\" is \"sim\" or \"ni\""));
            }
            if kind != ctx.selected_kind {
                report.errors.push(Item::field(
                    "backend_not_selected",
                    "setup.backend.kind",
                    format!("This setup is for the {} station, but the {} station is selected; select the station the setup is for, or change the setup's backend.", station_word(&kind), station_word(ctx.selected_kind)),
                ));
            }
            Some(kind)
        }
    };

    // The run's record: the operator's words, and whether it keeps its streams.
    let words = |k: &str| -> Result<String, MethodError> {
        Ok(text(s, k, "setup")?.unwrap_or("").to_owned())
    };
    let specification = match opt(s, "specification") {
        None => None,
        Some(v) => {
            let o = obj(v, "setup.specification")?;
            Some((
                text(o, "document", "setup.specification")?
                    .unwrap_or("")
                    .to_owned(),
                text(o, "clause", "setup.specification")?
                    .unwrap_or("")
                    .to_owned(),
            ))
        }
    };
    let streams = match opt(s, "record") {
        None => true,
        Some(v) => boolean(obj(v, "setup.record")?, "streams", "setup.record")?.unwrap_or(true),
    };
    let record = RecordWords {
        operator: words("operator")?,
        article: words("article")?,
        notes: words("notes")?,
        specification,
        streams,
    };

    Ok((
        MimoSetup {
            title,
            controls,
            drives,
            limits,
            article_mass_lb,
            space,
            dof_bands,
            reference,
            law,
            estimation,
            identification,
            control,
            schedule,
            backend,
            record,
            raw: setup.clone(),
        },
        report,
    ))
}

fn station_word(kind: &str) -> &'static str {
    match kind {
        "ni" => "NI-DAQmx",
        _ => "simulator",
    }
}

/// Check the law choice: it exists, it is a multiple-output law, it admits the
/// test's shape, and every parameter is one it declares, of the right kind and in
/// range.
pub fn check_law(
    law: &LawSetup,
    info: Option<LawInfo>,
    shape: MimoShape,
    controls: usize,
    report: &mut Report,
) {
    let Some(info) = info else {
        report.errors.push(Item::field(
            "law_unknown",
            "setup.law.id",
            format!("No control law \"{}\" is in the catalog; choose one the law list shows, or import its file first.", law.id),
        ));
        return;
    };
    let Some(m) = info.mimo else {
        report.errors.push(Item::field(
            "law_wrong_world",
            "setup.law.id",
            format!(
                "\"{}\" is a single-output law; choose a multiple-output law.",
                info.name
            ),
        ));
        return;
    };
    if !m.supports(shape) {
        report.errors.push(Item::field(
            "law_shape_not_supported",
            "setup.law.id",
            format!(
                "\"{}\" does not support a test with {} (it supports {}); choose another law, or change the channels.",
                info.name,
                shape_words(shape),
                m.shapes.iter().map(|s| shape_words(*s)).collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    for (key, value) in &law.params {
        let field = format!("setup.law.params.{key}");
        let Some(spec) = m.law.params.iter().find(|p| &p.key == key) else {
            report.errors.push(Item::field(
                "law_param_unknown",
                field,
                format!("\"{}\" has no parameter \"{key}\"; remove it.", info.name),
            ));
            continue;
        };
        if let Err(why) = param_ok(&spec.kind, value, controls) {
            report.errors.push(Item::field(
                "law_param_invalid",
                field,
                format!("{}: {why}", spec.label),
            ));
        }
    }
}

fn shape_words(s: MimoShape) -> &'static str {
    match s {
        MimoShape::Square => "as many controls as drives",
        MimoShape::MoreControlsThanDrives => "more controls than drives",
        MimoShape::FewerControlsThanDrives => "fewer controls than drives",
    }
}

/// Whether `value` is a value of a parameter of `kind`; the reason in words when not.
fn param_ok(kind: &ParamKind, value: &Value, controls: usize) -> Result<(), String> {
    let in_range = |x: f64, lo: f64, hi: f64| {
        if (lo..=hi).contains(&x) {
            Ok(())
        } else {
            Err(format!("enter a value from {} to {}", num(lo), num(hi)))
        }
    };
    match kind {
        ParamKind::Number(s) => match value.as_f64().filter(|x| x.is_finite()) {
            Some(x) => in_range(x, s.min, s.max),
            None => Err("enter a number".into()),
        },
        ParamKind::Integer(s) => match value.as_i64() {
            Some(x) => in_range(x as f64, s.min as f64, s.max as f64),
            None => Err("enter a whole number".into()),
        },
        ParamKind::Toggle(_) => value
            .as_bool()
            .map(|_| ())
            .ok_or_else(|| "choose on or off".into()),
        ParamKind::Choice(s) => match value.as_str() {
            Some(x) if s.options.iter().any(|o| o == x) => Ok(()),
            _ => Err(format!("choose one of {}", s.options.join(", "))),
        },
        ParamKind::Curve(s) => {
            let rows = value
                .as_array()
                .ok_or("enter a list of [frequency, value] points")?;
            let mut last = 0.0;
            for r in rows {
                let p = r
                    .as_array()
                    .filter(|p| p.len() == 2)
                    .ok_or("each point is [frequency, value]")?;
                let (f, v) = (
                    p[0].as_f64().unwrap_or(f64::NAN),
                    p[1].as_f64().unwrap_or(f64::NAN),
                );
                if f.is_nan() || f <= last {
                    return Err("frequencies rise strictly from above 0".into());
                }
                in_range(v, s.min, s.max)?;
                last = f;
            }
            if rows.is_empty() {
                return Err("enter at least one point".into());
            }
            Ok(())
        }
        ParamKind::PerChannel(s) => {
            let a = value
                .as_array()
                .ok_or("enter one value per control channel")?;
            if a.len() != controls {
                return Err(format!("enter {controls} values, one per control channel"));
            }
            for x in a {
                in_range(x.as_f64().unwrap_or(f64::NAN), s.min, s.max)?;
            }
            Ok(())
        }
    }
}

/// The setup as accepted, every default filled in (`mimo_random.setup`'s echo and
/// the status readers' copy). An inline reference is summarized, not echoed.
pub fn setup_json(s: &MimoSetup) -> Value {
    let reference = match &s.reference {
        Reference::Inline { freqs_hz, n, .. } => json!({
            "kind": "inline", "n": n, "lines": freqs_hz.len(),
            "band_hz": [num(*freqs_hz.first().unwrap_or(&0.0)), num(*freqs_hz.last().unwrap_or(&0.0))],
        }),
        Reference::File { path, sha256 } => json!({"kind": "file", "path": path, "sha256": sha256}),
    };
    let space = match &s.space {
        ControlSpace::Physical => json!({"kind": "physical"}),
        ControlSpace::Virtual {
            dofs,
            response,
            drive,
        } => json!({
            "kind": "virtual", "dofs": dofs,
            "response_transform": matrix_json(response),
            "drive_transform": drive.as_ref().map(matrix_json),
            "dof_bands": s.dof_bands.iter().map(|(a, b)| json!({
                "alarm_db": [num(a[0]), num(a[1])],
                "abort_db": [num(b[0]), num(b[1])],
            })).collect::<Vec<_>>(),
        }),
    };
    json!({
        "title": s.title,
        "channels": {
            "controls": s.controls.iter().map(|c| json!({
                "channel": c.channel,
                "alarm_db": [num(c.alarm_db[0]), num(c.alarm_db[1])],
                "abort_db": [num(c.abort_db[0]), num(c.abort_db[1])],
            })).collect::<Vec<_>>(),
            "drives": s.drives.iter().map(|d| json!({
                "channel": d.channel, "ceiling_vrms": num(d.ceiling_vrms),
                "abort_vrms": d.abort_vrms.map(num), "peak_v": num(d.peak_v),
                "shaker": d.shaker, "moving_mass_lb": num(d.moving_mass_lb),
            })).collect::<Vec<_>>(),
            "limits": s.limits.iter().map(|l| json!({
                "channel": l.channel, "ceiling": num(l.ceiling),
                "abort_margin_db": num(l.abort_margin_db),
            })).collect::<Vec<_>>(),
            "article_mass_lb": num(s.article_mass_lb),
        },
        "space": space,
        "reference": reference,
        "law": {"id": s.law.id, "params": s.law.params},
        "estimation": {
            "fs_hz": num(s.estimation.fs_hz), "n_pts": s.estimation.n_pts,
            "frames_per_loop": s.estimation.frames_per_loop, "window": s.estimation.window,
            "overlap": num(s.estimation.overlap), "memory_loops": s.estimation.memory_loops,
        },
        "identification": {
            "level_db": num(s.identification.level_db), "frames": s.identification.frames,
            "noise_frames": s.identification.noise_frames,
            "coherence_floor": num(s.identification.coherence_floor),
            "condition_limit": num(s.identification.condition_limit),
            "review": s.identification.review,
        },
        "control": {
            "frf": if s.control.frf_update { "update" } else { "hold" },
            "level_ramp_s": num(s.control.level_ramp_s),
            "shutdown_ramp_s": num(s.control.shutdown_ramp_s),
            "max_loops_out": s.control.max_loops_out,
            "frf_blend": num(s.control.frf_blend),
            "cap_up_db": num(s.control.cap_up_db),
            "cap_down_db": num(s.control.cap_down_db),
            "law_budget": num(s.control.law_budget),
            "law_misses": s.control.law_misses,
            "soft_start_db": num(s.control.soft_start_db),
        },
        "schedule": {
            "mode": crate::random::view::mode_name(s.schedule.mode),
            "levels": s.schedule.levels.iter().map(|(db, dwell)| {
                let mut m = Map::new();
                m.insert("db".into(), num(*db));
                if let Some(d) = dwell {
                    m.insert("dwell_s".into(), num(*d));
                }
                Value::Object(m)
            }).collect::<Vec<_>>(),
            "full_level_s": num(s.schedule.full_level_s),
        },
        "backend": s.backend.as_ref().map(|k| json!({"kind": k})),
    })
}

/// The `setup_invalid` refusal, carrying the validation report.
pub fn setup_invalid(validation: &Value, errors: usize) -> MethodError {
    MethodError::named(
        crate::protocol::ErrorCode::BadRequest,
        "setup_invalid",
        format!(
            "The MIMO setup has {errors} error{}; correct {} and send it again.",
            if errors == 1 { "" } else { "s" },
            if errors == 1 { "it" } else { "them" }
        ),
    )
    .with_detail(validation.clone())
}
