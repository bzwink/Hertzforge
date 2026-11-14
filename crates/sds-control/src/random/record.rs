//! What a Random run puts in its record (`docs/run-folder.md`, "What a Random Run
//! Saves"): the manifest, the table and the setup at arm, the two streams, and the
//! results (the reference and its bands, the limits, the identification's transfer
//! function and coherence, and the spectra at each level change and at the end).
//!
//! The writing is the record's ([`crate::record`]); this module only says what is
//! written, in the reader's form (`hertzforge.io.run_folder`). Values are in the
//! channels' own units, as the table names them: the analysis side converts.

use super::config::{Derived, Grid, WireConfig};
use super::view::{LimitLines, Lines};
use crate::protocol::num;
use crate::record::{tap::TapLink, ArrayValues, Begin, ResultArray, StreamSpec};
use crate::station::{self, class_name, source_of, StationPlan};
use crate::table::{row_json, unit_name};
use hertzforge_channel_table::{Channel, ChannelRole, ChannelTable};
use hertzforge_hal::{Caps, DiscoveredDevice};
use hertzforge_module_abi::StationClass;
use hertzforge_random::LoopConfig;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Frames in one of a stream tap's buffers.
pub const TAP_BLOCK_FRAMES: usize = 1024;

/// The enabled inputs in the order the station acquires them: every enabled row
/// that is not a drive, in table order.
pub fn input_rows(table: &ChannelTable) -> Vec<&Channel> {
    table
        .channels
        .iter()
        .filter(|c| c.enabled && c.role != ChannelRole::Drive)
        .collect()
}

/// The enabled drives in the order the station plays them.
pub fn drive_rows(table: &ChannelTable) -> Vec<&Channel> {
    table
        .channels
        .iter()
        .filter(|c| c.enabled && c.role == ChannelRole::Drive)
        .collect()
}

/// How many buffers a tap needs to hold `seconds` of samples at `fs_hz`.
pub fn tap_buffers(fs_hz: f64, seconds: f64) -> usize {
    ((seconds.max(0.0) * fs_hz) / TAP_BLOCK_FRAMES as f64).ceil() as usize + 4
}

/// `"<unit>^2/Hz"`: a spectrum's unit for a channel in `unit`.
pub fn psd_unit(unit: &str) -> String {
    format!("{unit}^2/Hz")
}

/// The channel table as the run armed with it: every row in the protocol's own
/// spelling, the table's revision, and the SHA-256 of the rows.
pub fn table_json(table: &ChannelTable, revision: u64) -> Value {
    let rows: Vec<Value> = table.channels.iter().map(row_json).collect();
    let bytes = serde_json::to_vec(&rows).unwrap_or_default();
    let sha = Sha256::digest(&bytes);
    json!({"revision": revision, "sha256": hex(&sha), "rows": rows})
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `unix` as `YYYYMMDD-HHMMSS` in UTC.
pub fn utc_stamp(unix: f64) -> String {
    let secs = unix.floor() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days since 1970-01-01 to a civil date (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The host's offset from UTC, seconds, as its clock says now; `None` when the
/// host does not say.
pub fn utc_offset_s() -> Option<i64> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let t = text.trim();
        if t.len() != 5 {
            return None;
        }
        let sign = match &t[..1] {
            "+" => 1,
            "-" => -1,
            _ => return None,
        };
        let h: i64 = t[1..3].parse().ok()?;
        let m: i64 = t[3..5].parse().ok()?;
        Some(sign * (h * 3600 + m * 60))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// An identity for the run unique across restarts and machines: the time it
/// armed and eight hexadecimal digits drawn from the clock, the process and the
/// run.
pub fn run_uid(unix: f64, run_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .to_le_bytes(),
    );
    h.update(std::process::id().to_le_bytes());
    h.update(run_id.as_bytes());
    h.update(format!("{:?}", std::thread::current().id()).as_bytes());
    format!("{}-{}", utc_stamp(unix), &hex(&h.finalize())[..8])
}

/// The facts of an armed run its record begins with.
pub struct ArmFacts<'a> {
    pub run_id: &'a str,
    pub run_uid: &'a str,
    pub cfg: &'a WireConfig,
    pub table: &'a ChannelTable,
    pub revision: u64,
    pub plan: &'a StationPlan,
    pub class: StationClass,
    pub provenance: &'a hertzforge_station::ProvenanceBasis,
    pub devices: &'a [DiscoveredDevice],
    pub caps: &'a Caps,
    pub loop_cfg: &'a LoopConfig,
    pub grid: &'a Grid,
    pub derived: Option<&'a Derived>,
    pub feasibility: Option<&'a (&'static str, String)>,
    pub law: Value,
    pub armed_unix: f64,
    pub utc_offset_s: Option<i64>,
    /// `None` when the streams are recorded; otherwise why not.
    pub streams_off: Option<String>,
    pub log: Vec<Value>,
}

/// The `start` member: the time, the host's offset from UTC, and the station frame
/// at that instant.
pub fn start_json(unix: f64, utc_offset_s: Option<i64>, station_frame: u64) -> Value {
    json!({"unix": unix, "utc_offset_s": utc_offset_s, "station_frame": station_frame})
}

fn station_json(f: &ArmFacts<'_>) -> Value {
    station_member(
        matches!(f.plan, StationPlan::Sim { .. }),
        f.class,
        f.provenance,
        f.devices,
        f.caps,
    )
}

/// The manifest's `station`: its kind and class, the station's label and backend,
/// every device with its model and serial number, and its clock.
pub fn station_member(
    sim: bool,
    class: StationClass,
    provenance: &hertzforge_station::ProvenanceBasis,
    devices: &[DiscoveredDevice],
    caps: &Caps,
) -> Value {
    let listed: Vec<Value> = provenance
        .device_ids
        .iter()
        .zip(provenance.serials.iter())
        .map(|(id, serial)| {
            let model = devices
                .iter()
                .find(|d| &d.id == id)
                .map_or(Value::Null, |d| Value::from(d.product.as_str()));
            json!({"id": id, "model": model, "serial": serial})
        })
        .collect();
    let clock = match caps.clock {
        hertzforge_hal::ClockDomain::Shared => "shared",
        hertzforge_hal::ClockDomain::Independent => "independent",
        _ => "unknown",
    };
    json!({
        "kind": if sim { "sim" } else { "ni" },
        "class": class_name(class),
        "label": provenance.station_label,
        "backend": provenance.backend_kind,
        "devices": listed,
        "clock": clock,
    })
}

fn sim_json(f: &ArmFacts<'_>) -> Option<Value> {
    sim_value(f.plan, f.class)
}

/// The manifest's `sim` for a simulated run (the plant's model, its parameters
/// and its seed); `None` for any other station.
pub fn sim_value(plan: &StationPlan, class: StationClass) -> Option<Value> {
    match plan {
        StationPlan::Sim {
            fs_hz,
            plant,
            outputs,
        } if source_of(class) == "SIM" => Some(station::sim_identity(&station::sim_spec(
            *fs_hz,
            plant.as_ref(),
            *outputs,
        ))),
        _ => None,
    }
}

/// The manifest's `build`: the version, the commit and whether it was dirty.
pub fn build_value(core_build: &str) -> Value {
    json!({"version": env!("CARGO_PKG_VERSION"),
           "commit": core_build.trim_end_matches("-dirty"),
           "dirty": core_build.ends_with("-dirty")})
}

/// A stream's channel as `stream.json` lists it: its table index, its unit and
/// its description.
pub fn stream_channel_value(c: &Channel) -> Value {
    stream_channel(c)
}

pub fn control_json(cfg: &WireConfig) -> Value {
    let c = &cfg.control;
    json!({
        "composite": match c.composite {
            hertzforge_random::CompositeMode::WeightedAvg => "weighted_avg",
            hertzforge_random::CompositeMode::Min => "min",
            hertzforge_random::CompositeMode::Max => "max",
        },
        "weights": c.weights.iter().map(|(ch, w)| json!({"channel": ch, "weight": num(*w)})).collect::<Vec<_>>(),
        "control_dof": num(c.control_dof),
        "damping": num(c.damping),
        "max_lines_out": c.max_lines_out,
        "max_loops_out": c.max_loops_out,
        "max_sigma": c.max_sigma.map(num),
        "drive_ceiling_vrms": num(c.drive_ceiling_vrms),
        "drive_abort_vrms": c.drive_abort_vrms.map(num),
        "coherence_floor": num(c.coherence_floor),
        "review_after_sysid": c.review_after_sysid,
        "pretest": {
            "averages": c.pretest_averages,
            "level_db": num(c.pretest_level_db),
            "max_vrms": c.pretest_max_vrms.map(num),
            "start_vrms": num(c.pretest_start_vrms),
            "shaped": c.pretest_shaped,
        },
        "soft_start_db": num(c.soft_start_db),
        "soft_start_relax_db": num(c.soft_start_relax_db),
        "level_ramp_s": num(c.level_ramp_s),
        "shutdown_ramp_s": num(c.shutdown_ramp_s),
    })
}

/// The profile as resolved: every breakpoint an anchor in g^2/Hz, each with the
/// alarm and abort bands that hold from it to the next.
pub fn profile_json(cfg: &WireConfig) -> Value {
    let anchors = cfg.breakpoints().resolve().unwrap_or_default();
    let points: Vec<Value> = cfg
        .rows
        .iter()
        .zip(anchors.iter())
        .map(|(row, a)| {
            let (alarm, abort) = (
                row.alarm_db.unwrap_or(cfg.alarm_db),
                row.abort_db.unwrap_or(cfg.abort_db),
            );
            json!({"freq_hz": num(a.freq_hz), "psd_g2_hz": a.level_g2_hz,
                   "alarm_db": [num(alarm.0), num(alarm.1)],
                   "abort_db": [num(abort.0), num(abort.1)]})
        })
        .collect();
    json!({"breakpoints": points,
           "alarm_db": [num(cfg.alarm_db.0), num(cfg.alarm_db.1)],
           "abort_db": [num(cfg.abort_db.0), num(cfg.abort_db.1)],
           "unit": "g^2/Hz"})
}

/// Every limit and abort in force, with its channel.
pub fn limits_json(cfg: &WireConfig, table: &ChannelTable) -> (Value, Value) {
    let unit_of = |ch: u32| {
        table
            .channels
            .iter()
            .find(|c| c.index == ch)
            .map_or_else(|| "g".to_owned(), |c| unit_name(&c.units))
    };
    let limits: Vec<Value> = cfg
        .limits
        .iter()
        .map(|l| {
            json!({"channel": l.channel, "kind": if l.force { "force" } else { "response" },
                   "unit": psd_unit(&unit_of(l.channel)),
                   "rows": l.rows.iter().map(|(f, c)| json!({"freq_hz": num(*f), "ceiling": num(*c)})).collect::<Vec<_>>()})
        })
        .collect();
    let channels: Vec<Value> = table
        .channels
        .iter()
        .filter(|c| {
            c.enabled
                && (c.abort.accel_g.is_some()
                    || c.abort.velocity_ips.is_some()
                    || c.abort.displacement_mil.is_some())
        })
        .map(|c| {
            json!({"channel": c.index, "accel_g": c.abort.accel_g.map(num),
                   "velocity_ips": c.abort.velocity_ips.map(num),
                   "displacement_mil": c.abort.displacement_mil.map(num)})
        })
        .collect();
    let c = &cfg.control;
    let aborts = json!({
        "envelope": {"abort_db": [num(cfg.abort_db.0), num(cfg.abort_db.1)],
                     "max_lines_out": c.max_lines_out},
        "divergence": {"max_loops_out": c.max_loops_out},
        "drive": {"abort_vrms": c.drive_abort_vrms.map(num),
                  "ceiling_vrms": num(c.drive_ceiling_vrms),
                  "max_sigma": c.max_sigma.map(num)},
        "channels": channels,
    });
    (Value::Array(limits), aborts)
}

fn setup_json(f: &ArmFacts<'_>) -> Value {
    let cfg = f.cfg;
    let last = cfg.levels.len().saturating_sub(1);
    let schedule: Vec<Value> = cfg
        .levels
        .iter()
        .enumerate()
        .map(|(i, (db, dwell))| {
            let dwell = if i == last {
                Some(cfg.full_level_s)
            } else {
                *dwell
            };
            json!({"db": num(*db), "dwell_s": dwell.map(num)})
        })
        .collect();
    let (limits, aborts) = limits_json(cfg, f.table);
    let (lo, hi) = f.loop_cfg.band_bins();
    let mut derived = f.derived.map_or_else(|| json!({}), Derived::to_json);
    if let Value::Object(m) = &mut derived {
        m.insert("grid".into(), f.grid.to_json());
        m.insert(
            "band_lines".into(),
            json!({"first_hz": num(lo as f64 * f.grid.df_hz), "lines": hi.saturating_sub(lo)}),
        );
    }
    json!({
        "config": cfg.raw,
        "resolved": {
            "profile": profile_json(cfg),
            "schedule": {"mode": super::view::mode_name(cfg.mode), "levels": schedule,
                         "full_level_s": num(cfg.full_level_s)},
            "tolerances": {"alarm_db": [num(cfg.alarm_db.0), num(cfg.alarm_db.1)],
                           "abort_db": [num(cfg.abort_db.0), num(cfg.abort_db.1)]},
            "limits": limits,
            "aborts": aborts,
            "control": control_json(cfg),
            "control_channels": WireConfig::control_channels(f.table),
            "acquisition": {"requested_fs_hz": num(cfg.fs_hz), "fs_hz": num(f.grid.fs_hz),
                            "n_pts": cfg.n_pts, "n_avg": cfg.n_avg},
        },
        "derived": derived,
        "feasibility": {
            "inputs": cfg.raw.get("feasibility").cloned().unwrap_or(Value::Null),
            "verdict": f.feasibility.map(|(v, _)| *v),
            "message": f.feasibility.map(|(_, m)| m.clone()),
        },
    })
}

fn stream_channel(c: &Channel) -> Value {
    json!({"index": c.index, "unit": unit_name(&c.units), "description": c.description})
}

/// The record a Random run begins with, at arm.
pub fn begin(f: &ArmFacts<'_>, root: PathBuf, links: (TapLink, TapLink)) -> Begin {
    let cfg = f.cfg;
    let mut manifest = Map::new();
    manifest.insert("test".into(), Value::from("random"));
    manifest.insert("run_uid".into(), Value::from(f.run_uid));
    manifest.insert("run_id".into(), Value::from(f.run_id));
    manifest.insert("title".into(), Value::from(cfg.title.as_str()));
    manifest.insert("operator".into(), Value::from(cfg.operator.as_str()));
    manifest.insert("article".into(), Value::from(cfg.article.as_str()));
    manifest.insert("notes".into(), Value::from(cfg.notes.as_str()));
    if let Some((document, clause)) = &cfg.specification {
        manifest.insert(
            "specification".into(),
            json!({"document": document, "clause": clause}),
        );
    }
    manifest.insert("source".into(), Value::from(source_of(f.class)));
    if let Some(sim) = sim_json(f) {
        manifest.insert("sim".into(), sim);
    }
    manifest.insert("station".into(), station_json(f));
    manifest.insert(
        "grid".into(),
        json!({"requested_fs_hz": num(cfg.fs_hz), "fs_hz": num(f.grid.fs_hz),
               "n_pts": f.grid.n_pts, "n_avg": cfg.n_avg, "df_hz": num(f.grid.df_hz),
               "lines": f.grid.lines, "loop_s": num(f.grid.loop_s)}),
    );
    manifest.insert("build".into(), build_value(f.provenance.core_build));
    manifest.insert(
        "armed".into(),
        json!({"unix": f.armed_unix, "utc_offset_s": f.utc_offset_s}),
    );
    manifest.insert("start".into(), start_json(f.armed_unix, f.utc_offset_s, 0));
    manifest.insert(
        "recording".into(),
        match &f.streams_off {
            None => json!({"streams": "on"}),
            Some(why) => json!({"streams": "off", "why": why}),
        },
    );
    let mut streams = Vec::new();
    if f.streams_off.is_none() {
        let mut inputs = Map::new();
        inputs.insert("kind".into(), Value::from("inputs"));
        inputs.insert("fs_hz".into(), num(f.grid.fs_hz));
        inputs.insert(
            "channels".into(),
            Value::Array(
                input_rows(f.table)
                    .into_iter()
                    .map(stream_channel)
                    .collect(),
            ),
        );
        inputs.insert("first_station_frame".into(), Value::from(0));
        streams.push(StreamSpec {
            name: "inputs".into(),
            description: inputs,
            link: links.0,
        });
        let drives = drive_rows(f.table);
        if !drives.is_empty() {
            let mut d = Map::new();
            d.insert("kind".into(), Value::from("drives"));
            d.insert("fs_hz".into(), num(f.grid.fs_hz));
            d.insert(
                "channels".into(),
                Value::Array(drives.into_iter().map(stream_channel).collect()),
            );
            d.insert("first_station_frame".into(), Value::from(0));
            d.insert(
                "output_latency_frames".into(),
                match &f.caps.output_buffer {
                    Some(b) => json!({"frames": b.min_lookahead_frames, "how": "declared"}),
                    None => json!({"frames": null, "how": "unknown"}),
                },
            );
            streams.push(StreamSpec {
                name: "drives".into(),
                description: d,
                link: links.1,
            });
        }
    }
    let name = format!("{}_random_{}", utc_stamp(f.armed_unix), f.run_id);
    Begin {
        root,
        name,
        manifest,
        table: table_json(f.table, f.revision),
        setup: setup_json(f),
        law: Some((f.law.clone(), None)),
        log: f.log.clone(),
        streams,
    }
}

// ---- results ----------------------------------------------------------------------

fn frequency_axis(first_hz: f64, df_hz: f64) -> Value {
    json!({"kind": "frequency", "unit": "Hz", "start": first_hz, "step": df_hz})
}

fn f64s(name: &str, shape: Vec<usize>, unit: &str, values: Vec<f64>) -> ResultArray {
    ResultArray {
        name: name.into(),
        shape,
        unit: unit.into(),
        values: ArrayValues::F64(values),
    }
}

/// A result's description: its kind, abscissa and channels, and anything else.
pub type Described = (String, Map<String, Value>, Vec<ResultArray>);

fn described(kind: &str, abscissa: Value, channels: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), Value::from(kind));
    m.insert("abscissa".into(), abscissa);
    m.insert("channels".into(), channels);
    m
}

/// The reference and its alarm and abort bands at 0 dB on the run's lines.
pub fn reference_result(lines: &Lines) -> Described {
    let n = lines.freqs.len();
    let unit = psd_unit("g");
    let mut d = described(
        "envelope",
        json!({"kind": "frequency", "unit": "Hz", "array": "frequency"}),
        json!({}),
    );
    d.insert(
        "description".into(),
        Value::from("the reference and its alarm and abort bands at 0 dB, on the run's lines"),
    );
    (
        "reference".into(),
        d,
        vec![
            f64s("frequency", vec![n], "Hz", lines.freqs.clone()),
            f64s("reference", vec![n], &unit, lines.reference.clone()),
            f64s("alarm_low", vec![n], &unit, lines.alarm_lo.clone()),
            f64s("alarm_high", vec![n], &unit, lines.alarm_hi.clone()),
            f64s("abort_low", vec![n], &unit, lines.abort_lo.clone()),
            f64s("abort_high", vec![n], &unit, lines.abort_hi.clone()),
        ],
    )
}

/// One channel's limit ceiling on the lines it covers.
pub fn limit_result(limit: &LimitLines, table: &ChannelTable) -> Described {
    let n = limit.freqs.len();
    let unit = table
        .channels
        .iter()
        .find(|c| c.index == limit.channel)
        .map_or_else(|| "g".to_owned(), |c| unit_name(&c.units));
    let d = described(
        "limit",
        json!({"kind": "frequency", "unit": "Hz", "array": "frequency"}),
        json!({"responses": [limit.channel]}),
    );
    (
        format!("limit_ch{}", limit.channel),
        d,
        vec![
            f64s("frequency", vec![n], "Hz", limit.freqs.clone()),
            f64s("ceiling", vec![n], &psd_unit(&unit), limit.ceiling.clone()),
        ],
    )
}

/// A cross term from the controller's estimate to the file's convention,
/// `conj(X) Y` (Bendat and Piersol; `docs/hforge-format.md`, "Conventions").
///
/// The identification's FRF comes from `hertzforge_dsp::frf::h_estimate_hv`,
/// whose forward transform has the positive exponent and whose cross spectrum
/// is `X conj(Y)` on those spectra: the two flips cancel, so its values ARE
/// `conj(X) Y / |X|^2` in standard terms already, and a delay lags. This is the
/// one place that says so; a change of either flip in the estimator must change
/// it, and `a_delay_lags_in_the_recorded_frf` fails until it does.
pub fn frf_to_file(h: &[(f64, f64)]) -> Vec<(f64, f64)> {
    h.to_vec()
}

/// The identification's transfer function (drive to control response) and its
/// coherence, on the band's lines. The response is the composite of the control
/// channels: with one control channel it is that channel; with several, the
/// results name them all and keep the kind `composite_frf`, which the analysis
/// side holds as arrays.
pub fn identification_results(
    h: &[(f64, f64)],
    coherence: &[f64],
    loop_cfg: &LoopConfig,
    table: &ChannelTable,
) -> Vec<Described> {
    let (lo, hi) = loop_cfg.band_bins();
    let hi = hi.min(h.len()).min(coherence.len());
    if lo >= hi {
        return Vec::new();
    }
    let n = hi - lo;
    let df = loop_cfg.df();
    let controls: Vec<&Channel> = table
        .channels
        .iter()
        .filter(|c| c.enabled && c.role == ChannelRole::Control)
        .collect();
    let Some(drive) = drive_rows(table).into_iter().next() else {
        return Vec::new();
    };
    let Some(first) = controls.first() else {
        return Vec::new();
    };
    let single = controls.len() == 1;
    let unit = format!("{}/{}", unit_name(&first.units), unit_name(&drive.units));
    let indexes: Vec<u32> = controls.iter().map(|c| c.index).collect();
    let channels = if single {
        json!({"responses": indexes, "references": [drive.index]})
    } else {
        json!({"controls": indexes, "references": [drive.index]})
    };
    let axis = frequency_axis(lo as f64 * df, df);
    let mut frf = described(
        if single { "frf" } else { "composite_frf" },
        axis.clone(),
        channels.clone(),
    );
    frf.insert("convention".into(), Value::from("file"));
    frf.insert(
        "description".into(),
        Value::from("system identification: drive to control response"),
    );
    let mut coh = described(
        if single {
            "coherence"
        } else {
            "composite_coherence"
        },
        axis,
        channels,
    );
    coh.insert(
        "description".into(),
        Value::from("system identification: ordinary coherence, drive to control response"),
    );
    let shape = vec![1, 1, n];
    vec![
        (
            "identification_frf".into(),
            frf,
            vec![ResultArray {
                name: "values".into(),
                shape: shape.clone(),
                unit,
                values: ArrayValues::C128(frf_to_file(&h[lo..hi])),
            }],
        ),
        (
            "identification_coherence".into(),
            coh,
            vec![f64s("values", shape, "", coherence[lo..hi].to_vec())],
        ),
    ]
}

/// The spectra of one control loop: what the record keeps at a level change and at
/// the end.
#[derive(Debug, Clone, PartialEq)]
pub struct Spectra {
    /// The level the loop ran at, dB.
    pub level_db: f64,
    pub loop_index: u64,
    pub first_hz: f64,
    pub df_hz: f64,
    /// The control channels and each one's spectrum on the band's lines.
    pub controls: Vec<(u32, Vec<f64>)>,
    /// The composite control spectrum.
    pub composite: Vec<f64>,
    /// Every enabled input and its spectrum (`None`: not measured this loop).
    pub inputs: Vec<(u32, Option<Vec<f64>>)>,
    /// The drive channel and its spectrum.
    pub drive: Option<(u32, Vec<f64>)>,
}

/// One record of each repeating spectrum result: `control_psd`, `response_psd`,
/// `drive_psd`.
pub fn spectra_results(s: &Spectra, table: &ChannelTable) -> Vec<Described> {
    let unit_of = |ch: u32| {
        table
            .channels
            .iter()
            .find(|c| c.index == ch)
            .map_or_else(String::new, |c| unit_name(&c.units))
    };
    let common = |units: Vec<String>| -> String {
        match units.first() {
            Some(u) if units.iter().all(|x| x == u) => psd_unit(u),
            _ => String::new(),
        }
    };
    let axis = frequency_axis(s.first_hz, s.df_hz);
    let records = json!({"index": "record", "why": "level (the last loop before a level \
                         change) or end (the last loop)", "level_db": "the level the loop ran at",
                         "loop": "the control loop", "frame": "the station frame when it was kept"});
    let mut out = Vec::new();
    let n = s.composite.len();
    if !s.controls.is_empty() {
        let mut d = described(
            "psd",
            axis.clone(),
            json!({"responses": s.controls.iter().map(|(i, _)| i).collect::<Vec<_>>()}),
        );
        d.insert("records".into(), records.clone());
        let unit = common(s.controls.iter().map(|(i, _)| unit_of(*i)).collect());
        let values: Vec<f64> = s
            .controls
            .iter()
            .flat_map(|(_, p)| p.iter().copied())
            .collect();
        out.push((
            "control_psd".to_owned(),
            d,
            vec![
                f64s("values", vec![s.controls.len(), n], &unit, values),
                f64s("composite", vec![n], &unit, s.composite.clone()),
            ],
        ));
    }
    if !s.inputs.is_empty() {
        let mut d = described(
            "psd",
            axis.clone(),
            json!({"responses": s.inputs.iter().map(|(i, _)| i).collect::<Vec<_>>()}),
        );
        d.insert("records".into(), records.clone());
        let unit = common(s.inputs.iter().map(|(i, _)| unit_of(*i)).collect());
        let values: Vec<f64> = s
            .inputs
            .iter()
            .flat_map(|(_, p)| match p {
                Some(p) if p.len() == n => p.clone(),
                _ => vec![f64::NAN; n],
            })
            .collect();
        out.push((
            "response_psd".to_owned(),
            d,
            vec![f64s("values", vec![s.inputs.len(), n], &unit, values)],
        ));
    }
    if let Some((ch, p)) = &s.drive {
        let mut d = described("psd", axis, json!({"responses": [ch]}));
        d.insert("records".into(), records);
        out.push((
            "drive_psd".to_owned(),
            d,
            vec![f64s(
                "values",
                vec![1, n],
                &psd_unit(&unit_of(*ch)),
                p.clone(),
            )],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utc_stamp_reads_as_the_calendar_does() {
        assert_eq!(utc_stamp(0.0), "19700101-000000");
        // 2026-11-13 17:30:05 UTC
        assert_eq!(utc_stamp(1_794_591_005.7), "20261113-173005");
        // a leap day
        assert_eq!(utc_stamp(951_782_400.0), "20000229-000000");
    }

    /// The identification FRF is written in the file's convention: `conj(X) Y /
    /// |X|^2` with the standard (negative-exponent) transform, so it is `Y/X` and a
    /// delay lags. A known complex plant, a resonance behind a pure delay, is run
    /// through the estimator the service uses and the conversion at the edge, and
    /// compared with the plant's own transfer function line by line. Red if the
    /// conversion conjugated (every phase reversed) or if the estimator's
    /// convention changed without the conversion.
    #[test]
    fn a_delay_lags_in_the_recorded_frf() {
        use std::f64::consts::PI;
        let fs = 1024.0;
        let nfft = 256;
        let delay = 3usize;
        // a two-pole resonance at 100 Hz, r = 0.97
        let (r, w0) = (0.97f64, 2.0 * PI * 100.0 / fs);
        let (a1, a2) = (-2.0 * r * w0.cos(), r * r);
        let b0 = 1.0 + a1 + a2;
        let n = 1 << 16;
        let mut seed = 0x1234_5678_u64;
        let x: Vec<f64> = (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            })
            .collect();
        let mut y = vec![0.0; n];
        for k in 0..n {
            let xd = if k >= delay { x[k - delay] } else { 0.0 };
            let y1 = if k >= 1 { y[k - 1] } else { 0.0 };
            let y2 = if k >= 2 { y[k - 2] } else { 0.0 };
            y[k] = b0 * xd - a1 * y1 - a2 * y2;
        }
        let (h, coh) = hertzforge_dsp::frf::h_estimate_hv_with_coherence(&x, &y, fs, nfft);
        let h = frf_to_file(&h);
        let mut checked = 0;
        for k in 5..100 {
            // the plant: b0 e^{-j w d} / (1 + a1 e^{-j w} + a2 e^{-2j w}), w = 2 pi k / nfft
            let w = 2.0 * PI * k as f64 / nfft as f64;
            let (dr, di) = (
                1.0 + a1 * w.cos() + a2 * (2.0 * w).cos(),
                -a1 * w.sin() - a2 * (2.0 * w).sin(),
            );
            let (nr, ni) = (
                b0 * (w * delay as f64).cos(),
                -b0 * (w * delay as f64).sin(),
            );
            let den = dr * dr + di * di;
            let (tr, ti) = ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den);
            if coh[k] < 0.95 {
                continue;
            }
            let (er, ei) = h[k];
            let phase = (ei * tr - er * ti).atan2(er * tr + ei * ti);
            assert!(phase.abs() < 0.15, "line {k}: phase off by {phase:.3} rad");
            let ratio = (er.hypot(ei)) / tr.hypot(ti);
            assert!(
                (ratio - 1.0).abs() < 0.15,
                "line {k}: magnitude ratio {ratio:.3}"
            );
            checked += 1;
        }
        assert!(checked > 60, "only {checked} lines were coherent");
    }

    #[test]
    fn two_runs_armed_at_one_instant_have_different_identities() {
        let a = run_uid(1_794_591_005.0, "r-0001");
        let b = run_uid(1_794_591_005.0, "r-0001");
        assert_ne!(a, b);
        assert!(a.starts_with("20261113-173005-"), "{a}");
    }
}
