//! The Explorer configuration on the wire (schema/PROTOCOL.md, "Explorer
//! Configuration"): parsing (types and structure; a wrong shape, or a key the wire does
//! not have, is `bad_params` naming the field), the checks (content; every problem a
//! named item located by `field` or `channel`), the derived values, and the core's own
//! `ExplorerHostConfig` built from it with every field supplied.

use crate::protocol::{num, ErrorCode, MethodError};
use crate::shock::config::Obj;
use crate::table::{Item, Report};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_explorer_host::host::{acquired_inputs, TableRefusal};
use hertzforge_explorer_host::slew::SlewLimits;
use hertzforge_explorer_host::{AmplitudeRamp, ExplorerHostConfig};
use hertzforge_shock_host::MetaField as WireMeta;
use serde_json::{json, Map, Value};

/// Accepted requested rates, Hz.
pub const FS_RANGE: (f64, f64) = (1000.0, 204_800.0);
/// A band edge must be at most this fraction of the rate.
pub const BAND_FRACTION_OF_RATE: f64 = 0.45;
/// The largest drive ceiling, V pk.
pub const MAX_DRIVE_V: f64 = 10.0;
/// The fastest frequency slew, octaves per second.
pub const MAX_FREQ_OCT_PER_S: f64 = 10.0;
/// The fastest drive slew, dB per second.
pub const MAX_DRIVE_DB_PER_S: f64 = 1000.0;
/// Accepted display FFT sizes (powers of two).
pub const NFFT_RANGE: (u64, u64) = (1024, 16_384);
/// A sweep is answered at no more than this many octaves per minute for every octave
/// per second of frequency slew.
pub const SWEEP_PER_SLEW: f64 = 60.0;
/// The run folder's name when none is given.
pub const DEFAULT_RUN_NAME: &str = "explorer";
/// Blocks of output lookahead an NI station keeps ahead of the clock.
pub const NI_LOOKAHEAD_BLOCKS: u32 = 2;
/// The NI input path's documented group delay: `38.4 / fs + 3.2 us`, so
/// `38.4 + 3.2e-6 x fs` samples.
pub const NI_GROUP_DELAY_SAMPLES: f64 = 38.4;
pub const NI_GROUP_DELAY_FIXED_S: f64 = 3.2e-6;

/// The figures of the station a run would arm on that the derived values need before it
/// is opened: its output lookahead at a block, and its input group delay at a rate.
/// The run state carries the armed station's own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StationFigures {
    /// A simulated station stating `lookahead_frames` of output lookahead, with no group
    /// delay.
    Sim { lookahead_frames: u32 },
    /// An NI station: [`NI_LOOKAHEAD_BLOCKS`] blocks of lookahead and the input path's
    /// documented group delay.
    Ni,
}

impl StationFigures {
    /// The control service's own simulated station.
    pub fn sim() -> StationFigures {
        StationFigures::Sim {
            lookahead_frames: hertzforge_hal_sim::DEFAULT_MIN_LOOKAHEAD_FRAMES,
        }
    }

    pub fn lookahead_frames(&self, block_frames: usize) -> u32 {
        match self {
            StationFigures::Sim { lookahead_frames } => *lookahead_frames,
            StationFigures::Ni => NI_LOOKAHEAD_BLOCKS * block_frames as u32,
        }
    }

    pub fn group_delay_frames(&self, fs_hz: f64) -> Option<u32> {
        match self {
            StationFigures::Sim { .. } => None,
            StationFigures::Ni => {
                Some((NI_GROUP_DELAY_SAMPLES + NI_GROUP_DELAY_FIXED_S * fs_hz).ceil() as u32)
            }
        }
    }
}

/// A parsed Explorer configuration, every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct WireExplorer {
    pub title: String,
    /// The rate as sent; null asks for the core's default.
    pub fs_hz: Option<f64>,
    pub band_hz: (f64, f64),
    pub start_hz: f64,
    pub max_drive_vpk: f64,
    pub freq_oct_per_s: f64,
    pub drive_db_per_s: f64,
    pub nfft: u64,
    pub run_name: String,
    pub metadata: Vec<WireMeta>,
    /// The operator's words for the run's record, as Random's: who ran it, on what,
    /// notes, and the specification it was run to.
    pub operator: String,
    pub article: String,
    pub notes: String,
    pub specification: Option<(String, String)>,
    /// Whether the run's record holds its streams (`record.streams`, default true).
    pub record_streams: bool,
    /// The configuration as sent, for `explorer.config` and `explorer.reset`.
    pub raw: Value,
}

/// Everything a configuration's checks are made against.
pub struct CheckContext<'a> {
    /// The channel table the run would arm with.
    pub table: &'a ChannelTable,
    /// The longest the service waits for a stop before it aborts, seconds: the controlled
    /// stop must reach 0 V inside it.
    pub timeout_s: f64,
    /// The station selected.
    pub station: StationFigures,
}

/// What configure computes from a configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    pub fs_hz: f64,
    pub block_frames: usize,
    pub lookahead_frames: u32,
    /// True once the run has armed and the figures are the station's.
    pub actual: bool,
    pub band_hz: (f64, f64),
    pub pipeline_frames: u64,
    pub rise_s: f64,
    pub stop_s: f64,
    pub stop_wait_s: f64,
    pub sweep_max_oct_per_min: f64,
    pub df_hz: f64,
    pub window_s: f64,
    pub tolerance_hz: f64,
    /// The first survey point after a change of frequency, at the band's low and high
    /// edges, seconds.
    pub first_point_s: (f64, f64),
}

impl Derived {
    pub fn pipeline_s(&self) -> f64 {
        self.pipeline_frames as f64 / self.fs_hz
    }

    pub fn grid_json(&self) -> Value {
        json!({
            "fs_hz": num(self.fs_hz),
            "block_frames": self.block_frames,
            "lookahead_frames": self.lookahead_frames,
            "actual": self.actual,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "grid": self.grid_json(),
            "band_hz": [num(self.band_hz.0), num(self.band_hz.1)],
            "pipeline_frames": self.pipeline_frames,
            "pipeline_s": num(round(self.pipeline_s(), 5)),
            "rise_s": num(round(self.rise_s, 3)),
            "stop_s": num(round(self.stop_s, 3)),
            "stop_wait_s": num(round(self.stop_wait_s, 3)),
            "sweep_max_oct_per_min": num(round(self.sweep_max_oct_per_min, 6)),
            "display": {
                "df_hz": num(round(self.df_hz, 6)),
                "window_s": num(round(self.window_s, 6)),
            },
            "survey": {
                "tolerance_hz": num(round(self.tolerance_hz, 6)),
                "first_point_s": [
                    num(round(self.first_point_s.0, 3)),
                    num(round(self.first_point_s.1, 3)),
                ],
            },
        })
    }
}

pub fn round(x: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    (x * p).round() / p
}

/// The pipeline in input frames: the drive the start writes ahead into the station's
/// output and into the service's own queue (two lookaheads), plus the input group delay.
pub fn pipeline_frames(lookahead_frames: u32, group_delay_frames: Option<u32>) -> u64 {
    2 * u64::from(lookahead_frames) + u64::from(group_delay_frames.unwrap_or(0))
}

/// Input frames from a change of frequency to the first survey point at `hz`: the
/// pipeline, the guard (two blocks, or three cycles of the new tone if longer), then
/// three cycles of window.
pub fn first_point_frames(pipeline: u64, block_frames: usize, fs_hz: f64, hz: f64) -> u64 {
    let cycles = (3.0 * fs_hz / hz).ceil().max(0.0) as u64;
    let guard = (2 * block_frames as u64).max(cycles);
    pipeline + guard + cycles
}

// ---------------------------------------------------------------------------------
// Parsing: types and structure. Content problems are items, not parse errors.
// ---------------------------------------------------------------------------------

fn bad(path: &str, message: impl Into<String>) -> MethodError {
    MethodError::bad_params(path, message)
}

/// Refuse any key of `o` not in `keys`, by its path.
fn only(o: &Obj<'_>, keys: &[&str]) -> Result<(), MethodError> {
    match o.m.keys().find(|k| !keys.contains(&k.as_str())) {
        None => Ok(()),
        Some(k) => Err(bad(
            &o.at(k),
            format!(
                "\"{}\" is not a field of the configuration; {} takes {}",
                o.at(k),
                if o.path == "config" {
                    "it".to_owned()
                } else {
                    format!("\"{}\"", o.path)
                },
                keys.iter()
                    .map(|k| format!("\"{k}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

pub fn parse(config: &Value) -> Result<WireExplorer, MethodError> {
    let core = ExplorerHostConfig::default();
    let c = Obj::of(config, "config")?;
    only(
        &c,
        &[
            "title",
            "acquisition",
            "band_hz",
            "start_hz",
            "max_drive_vpk",
            "slew",
            "display",
            "capture",
            "metadata",
            "operator",
            "article",
            "notes",
            "specification",
            "record",
        ],
    )?;
    let title = c.opt_str("title")?.unwrap_or("").to_owned();

    let fs_hz = match c.opt_obj("acquisition")? {
        None => None,
        Some(a) => {
            only(&a, &["fs_hz"])?;
            a.opt_f64("fs_hz")?
        }
    };
    let band_hz = c.band("band_hz")?.unwrap_or(core.band_hz);
    let start_hz = c.f64_or("start_hz", core.start_hz)?;
    let max_drive_vpk = c.f64("max_drive_vpk")?;

    let (mut freq_oct_per_s, mut drive_db_per_s) =
        (core.slew.freq_oct_per_s, core.slew.drive_db_per_s);
    if let Some(s) = c.opt_obj("slew")? {
        only(&s, &["freq_oct_per_s", "drive_db_per_s"])?;
        freq_oct_per_s = s.f64_or("freq_oct_per_s", freq_oct_per_s)?;
        drive_db_per_s = s.f64_or("drive_db_per_s", drive_db_per_s)?;
    }

    let mut nfft = core.display_nfft as u64;
    if let Some(d) = c.opt_obj("display")? {
        only(&d, &["nfft"])?;
        nfft = d.opt_count("nfft")?.unwrap_or(nfft);
    }

    let mut run_name = DEFAULT_RUN_NAME.to_owned();
    if let Some(o) = c.opt_obj("capture")? {
        only(&o, &["run_name"])?;
        if let Some(n) = o.opt_str("run_name")? {
            run_name = n.to_owned();
        }
    }

    let metadata = match c.array("metadata")? {
        None => WireMeta::defaults(),
        Some(list) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, m) in list.iter().enumerate() {
                let o = Obj::of(m, &format!("config.metadata[{i}]"))?;
                only(&o, &["key", "label", "value"])?;
                let text = |k: &str| -> Result<String, MethodError> {
                    o.opt_str(k)?
                        .map(str::to_owned)
                        .ok_or_else(|| bad(&o.at(k), format!("\"{}\" is text", o.at(k))))
                };
                let key = text("key")?;
                let label = o.opt_str("label")?.unwrap_or(&key).to_owned();
                out.push(WireMeta::new(key, label, text("value")?));
            }
            out
        }
    };

    let words =
        |k: &str| -> Result<String, MethodError> { Ok(c.opt_str(k)?.unwrap_or("").to_owned()) };
    let (operator, article, notes) = (words("operator")?, words("article")?, words("notes")?);
    let specification = match c.m.get("specification") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let o = Obj::of(v, "config.specification")?;
            only(&o, &["document", "clause"])?;
            Some((
                o.opt_str("document")?.unwrap_or("").to_owned(),
                o.opt_str("clause")?.unwrap_or("").to_owned(),
            ))
        }
    };
    let record_streams = match c.m.get("record") {
        None | Some(Value::Null) => true,
        Some(v) => {
            let o = Obj::of(v, "config.record")?;
            only(&o, &["streams"])?;
            o.opt_bool("streams")?.unwrap_or(true)
        }
    };

    Ok(WireExplorer {
        title,
        fs_hz,
        band_hz,
        start_hz,
        max_drive_vpk,
        freq_oct_per_s,
        drive_db_per_s,
        nfft,
        run_name,
        metadata,
        operator,
        article,
        notes,
        specification,
        record_streams,
        raw: config.clone(),
    })
}

// ---------------------------------------------------------------------------------
// Checking: every problem is a named item.
// ---------------------------------------------------------------------------------

/// A configuration field in words, for a message a person reads.
pub fn field_words(field: &str) -> String {
    let words = match field {
        "acquisition.fs_hz" => "The sample rate",
        "band_hz" => "The band",
        "start_hz" => "The start frequency",
        "max_drive_vpk" => "The drive ceiling",
        "slew.freq_oct_per_s" => "The frequency slew",
        "slew.drive_db_per_s" => "The drive slew",
        "display.nfft" => "The display FFT size",
        "capture.run_name" => "The run folder's name",
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

fn hz_words(x: f64) -> String {
    format!("{} Hz", num(round(x, 3)))
}

impl WireExplorer {
    /// The rate the run asks the station for.
    pub fn fs(&self) -> f64 {
        self.fs_hz.unwrap_or(ExplorerHostConfig::default().fs_hz)
    }

    /// The table's drive row, when it has exactly one enabled.
    pub fn drive_row(table: &ChannelTable) -> Option<&hertzforge_channel_table::Channel> {
        let mut drives = table
            .channels
            .iter()
            .filter(|c| c.enabled && c.role == ChannelRole::Drive);
        let first = drives.next()?;
        drives.next().is_none().then_some(first)
    }

    /// The control row: the first enabled control row, else the first enabled input.
    pub fn control_row(table: &ChannelTable) -> Option<&hertzforge_channel_table::Channel> {
        let inputs = || {
            table
                .channels
                .iter()
                .filter(|c| c.enabled && c.role != ChannelRole::Drive)
        };
        inputs()
            .find(|c| c.role == ChannelRole::Control)
            .or_else(|| inputs().next())
    }

    /// The core's configuration, every field supplied.
    pub fn build(&self) -> ExplorerHostConfig {
        let core = ExplorerHostConfig::default();
        ExplorerHostConfig {
            fs_hz: self.fs(),
            band_hz: self.band_hz,
            start_hz: self.start_hz,
            max_drive_vpk: self.max_drive_vpk,
            slew: SlewLimits {
                freq_oct_per_s: self.freq_oct_per_s,
                drive_db_per_s: self.drive_db_per_s,
            },
            ramp: AmplitudeRamp::default(),
            display_nfft: self.nfft as usize,
            ring_blocks: core.ring_blocks,
        }
    }

    /// The table's refusals, as items: exactly one enabled drive row and at least one
    /// enabled input. Checked at configure and again at start.
    pub fn check_table(table: &ChannelTable, r: &mut Report) {
        match acquired_inputs(table) {
            Ok(_) => {}
            Err(TableRefusal::NoDriveRow) => r.errors.push(Item::channel(
                "no_drive_channel",
                None,
                "The channel table has no enabled drive row; the tone needs an output to go on.",
            )),
            Err(TableRefusal::ExtraDriveRows(n)) => r.errors.push(Item::channel(
                "extra_drive_channels",
                None,
                format!(
                    "The channel table has {n} enabled drive rows; Explorer drives one tone on \
                     one output, and the others would stay silent. Disable all but one."
                ),
            )),
            Err(TableRefusal::NoInputs) => r.errors.push(Item::channel(
                "no_input_channel",
                None,
                "The channel table has no enabled input row; nothing would measure the tone.",
            )),
        }
    }

    /// Check against `ctx`: the items, and the derived values when there are no errors.
    pub fn check(&self, ctx: &CheckContext<'_>) -> (Report, Option<Derived>) {
        let mut r = Report::default();
        let fs = self.fs();
        if let Some(f) = self.fs_hz {
            out_of_range(
                &mut r,
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
        let fs_ok = fs.is_finite() && (FS_RANGE.0..=FS_RANGE.1).contains(&fs);

        let (lo, hi) = self.band_hz;
        let band_ok = lo.is_finite() && hi.is_finite() && lo > 0.0 && hi > lo;
        if !band_ok {
            r.errors.push(Item::field(
                "out_of_range",
                "band_hz",
                format!(
                    "The band is {} to {} Hz; its edges are above 0 Hz and the high edge is \
                     above the low.",
                    num(lo),
                    num(hi)
                ),
            ));
        } else if fs_ok && hi > BAND_FRACTION_OF_RATE * fs {
            r.errors.push(Item::field(
                "band_above_grid",
                "band_hz",
                format!(
                    "The band's high edge is {}, over 0.45 times the requested rate ({}); raise \
                     the rate or lower the band.",
                    hz_words(hi),
                    hz_words(BAND_FRACTION_OF_RATE * fs)
                ),
            ));
        }
        if band_ok {
            let s = self.start_hz;
            out_of_range(
                &mut r,
                "start_hz",
                s,
                s.is_finite() && (lo..=hi).contains(&s),
                &format!("it is inside the band, {} to {} Hz.", num(lo), num(hi)),
            );
        }

        let v = self.max_drive_vpk;
        let v_ok = v.is_finite() && v > 0.0 && v <= MAX_DRIVE_V;
        out_of_range(
            &mut r,
            "max_drive_vpk",
            v,
            v_ok,
            &format!("it is above 0 and at most {} V.", num(MAX_DRIVE_V)),
        );
        if v_ok {
            if let Some(d) = Self::drive_row(ctx.table) {
                if v > d.range_max {
                    r.errors.push(Item::field(
                        "ceiling_over_range",
                        "max_drive_vpk",
                        format!(
                            "The drive ceiling is {} V, over the drive row's range of {} V; \
                             lower the ceiling or raise the row's range.",
                            num(v),
                            num(d.range_max)
                        ),
                    ));
                }
            }
        }

        let f = self.freq_oct_per_s;
        out_of_range(
            &mut r,
            "slew.freq_oct_per_s",
            f,
            f.is_finite() && f > 0.0 && f <= MAX_FREQ_OCT_PER_S,
            &format!(
                "it is above 0 and at most {} octaves per second.",
                num(MAX_FREQ_OCT_PER_S)
            ),
        );
        let g = self.drive_db_per_s;
        let g_ok = g.is_finite() && g > 0.0 && g <= MAX_DRIVE_DB_PER_S;
        out_of_range(
            &mut r,
            "slew.drive_db_per_s",
            g,
            g_ok,
            &format!(
                "it is above 0 and at most {} dB per second.",
                num(MAX_DRIVE_DB_PER_S)
            ),
        );
        // The controlled stop inside the service's own stop wait: 40 dB at the drive
        // slew, then the step to 0 V.
        let span_db = AmplitudeRamp::default().ramp_span_db;
        let stop_s = span_db / g;
        if g_ok && stop_s > ctx.timeout_s {
            r.errors.push(Item::field(
                "safe_stop_budget",
                "slew.drive_db_per_s",
                format!(
                    "At {} dB/s the stop takes {} s, longer than the {} s the service waits \
                     for a stop; raise the drive slew above {} dB/s.",
                    num(g),
                    num(round(stop_s, 3)),
                    num(ctx.timeout_s),
                    num(round(span_db / ctx.timeout_s, 3))
                ),
            ));
        }

        let n = self.nfft;
        out_of_range(
            &mut r,
            "display.nfft",
            n as f64,
            n.is_power_of_two() && (NFFT_RANGE.0..=NFFT_RANGE.1).contains(&n),
            &format!(
                "it is a power of two from {} to {}.",
                NFFT_RANGE.0, NFFT_RANGE.1
            ),
        );

        let name = &self.run_name;
        let plain =
            !name.is_empty() && !name.contains(['/', '\\', ':']) && name != "." && name != "..";
        if !plain {
            r.errors.push(Item::field(
                "out_of_range",
                "capture.run_name",
                format!(
                    "The run folder's name is \"{name}\"; it is a plain folder name, with no \
                     path in it."
                ),
            ));
        }

        Self::check_table(ctx.table, &mut r);

        if !r.errors.is_empty() {
            return (r, None);
        }
        let block_frames = hertzforge_hal::default_block_frames(fs);
        let lookahead_frames = ctx.station.lookahead_frames(block_frames);
        let pipeline = pipeline_frames(lookahead_frames, ctx.station.group_delay_frames(fs));
        let df_hz = fs / n as f64;
        let first = |hz: f64| first_point_frames(pipeline, block_frames, fs, hz) as f64 / fs;
        let derived = Derived {
            fs_hz: fs,
            block_frames,
            lookahead_frames,
            actual: false,
            band_hz: self.band_hz,
            pipeline_frames: pipeline,
            rise_s: stop_s,
            stop_s,
            stop_wait_s: ctx.timeout_s,
            sweep_max_oct_per_min: SWEEP_PER_SLEW * f,
            df_hz,
            window_s: n as f64 / fs,
            tolerance_hz: df_hz / 2.0,
            first_point_s: (first(lo), first(hi)),
        };
        (r, Some(derived))
    }
}

/// The refusal of a configuration with errors.
pub fn config_invalid(report: &Report) -> MethodError {
    crate::shock::config::config_invalid(report)
}

/// The refusal of a dB step from a level of 0 V.
pub fn level_zero() -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "level_zero",
        "The target level is 0 V, so a step in dB has nothing to scale; set a level in volts \
         first.",
    )
}

/// `explorer.config`'s answer for a configured run.
pub fn config_result(run_id: &str, w: &WireExplorer, derived: &Derived) -> Value {
    let mut m = Map::new();
    m.insert("run_id".into(), Value::from(run_id));
    m.insert("config".into(), w.raw.clone());
    m.insert("derived".into(), derived.to_json());
    Value::Object(m)
}
