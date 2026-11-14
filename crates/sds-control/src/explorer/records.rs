//! What an Explorer run puts in its run folder (`docs/run-folder.md`, "What an
//! Explorer Run Saves"): the manifest, the channel table and the setup at start, the
//! log (every steering verb the run took is an entry with its facts), the two streams
//! (every acquired input as read, the drive as the device accepted it), and the end
//! with what the run measured.
//!
//! The writing is the shared record's ([`crate::record`]), on its own thread; this
//! module only says what is written, in the reader's form (`hertzforge.io.run_folder`).
//! Values are in the channels' own units, as the table names them: the analysis side
//! converts.

use super::config::{round, Derived, WireExplorer};
use super::view::{state_json, End, ExplorerView};
use crate::protocol::num;
use crate::random::record::{
    build_value, drive_rows, input_rows, sim_value, start_json, station_member,
    stream_channel_value, table_json,
};
use crate::record::{tap::TapLink, Begin, StreamSpec};
use crate::station::{source_of, StationPlan};
use crate::table::unit_name;
use hertzforge_channel_table::ChannelTable;
use hertzforge_explorer_host::{Adoption, AmplitudeRamp, IntentKind, IntentRecord, SweepDirection};
use hertzforge_hal::{Caps, DiscoveredDevice};
use hertzforge_module_abi::StationClass;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// The facts a run's record begins with, at start, before the station is opened.
pub struct StartFacts<'a> {
    pub run_id: &'a str,
    pub run_uid: &'a str,
    pub wire: &'a WireExplorer,
    pub table: &'a ChannelTable,
    pub revision: u64,
    pub plan: &'a StationPlan,
    pub class: StationClass,
    pub provenance: &'a hertzforge_station::ProvenanceBasis,
    pub devices: &'a [DiscoveredDevice],
    pub caps: &'a Caps,
    /// The derived values as configured: the station's own figures replace them in
    /// `run.json` once it has programmed its rate.
    pub derived: &'a Derived,
    pub started_unix: f64,
    pub utc_offset_s: Option<i64>,
    /// `None` when the streams are recorded; otherwise why not.
    pub streams_off: Option<String>,
}

fn adoption(a: &Adoption) -> Value {
    json!({"asked": num(a.asked), "adopted": num(a.adopted), "clamped": a.clamped})
}

/// One intent's facts, the `data` of its log entry: its place in the run (`t_s`, the
/// generated `frame` it took effect at, the `input_frame` acquired when it was taken),
/// the verb, and what was asked and adopted.
pub fn intent_json(i: &IntentRecord, fs_hz: f64) -> Value {
    let mut v = json!({
        "seq": i.seq,
        "t_s": num(round(i.input_frame as f64 / fs_hz, 6)),
        "frame": i.frame,
        "input_frame": i.input_frame,
    });
    let m = v.as_object_mut().expect("an object");
    match &i.kind {
        IntentKind::Steer { frequency, level } => {
            m.insert("verb".into(), Value::from("steer"));
            m.insert(
                "hz".into(),
                frequency.as_ref().map_or(Value::Null, adoption),
            );
            m.insert("vpk".into(), level.as_ref().map_or(Value::Null, adoption));
        }
        IntentKind::Sweep {
            direction,
            rate_oct_per_min,
        } => {
            m.insert("verb".into(), Value::from("sweep"));
            m.insert("direction".into(), Value::from(direction_name(*direction)));
            m.insert("rate_oct_per_min".into(), adoption(rate_oct_per_min));
        }
        IntentKind::Hold { hz } => {
            m.insert("verb".into(), Value::from("hold"));
            m.insert("hz".into(), num(*hz));
        }
        IntentKind::Stop { cause } => {
            m.insert("verb".into(), Value::from("stop"));
            m.insert("cause".into(), Value::from(cause.as_str()));
        }
    }
    v
}

fn direction_name(d: SweepDirection) -> &'static str {
    match d {
        SweepDirection::Up => "up",
        SweepDirection::Down => "down",
    }
}

/// An intent in the words the log shows.
pub fn intent_words(i: &IntentRecord) -> String {
    let n = |x: f64| crate::protocol::num(round(x, 6));
    let clamped = |a: &Adoption, unit: &str| {
        if a.clamped {
            format!(" (asked {} {unit}, clamped)", n(a.asked))
        } else {
            String::new()
        }
    };
    match &i.kind {
        IntentKind::Steer { frequency, level } => {
            let mut parts = Vec::new();
            if let Some(f) = frequency {
                parts.push(format!("{} Hz{}", n(f.adopted), clamped(f, "Hz")));
            }
            if let Some(l) = level {
                parts.push(format!("{} V pk{}", n(l.adopted), clamped(l, "V pk")));
            }
            format!("steer: {}", parts.join(", "))
        }
        IntentKind::Sweep {
            direction,
            rate_oct_per_min,
        } => format!(
            "sweep {} at {} oct/min{}",
            direction_name(*direction),
            n(rate_oct_per_min.adopted),
            clamped(rate_oct_per_min, "oct/min")
        ),
        IntentKind::Hold { hz } => format!("hold at {} Hz", n(*hz)),
        IntentKind::Stop { cause } => format!("stop: {}", cause.as_str()),
    }
}

/// The manifest's `grid` from the view: the rate asked and the rate in force (the
/// request until the station has programmed its own, `actual`), the block, the output
/// lookahead and the pipeline, frames.
pub fn grid_json(v: &ExplorerView) -> Value {
    json!({
        "requested_fs_hz": num(v.grid.requested_fs_hz),
        "fs_hz": num(v.grid.fs_hz),
        "block_frames": v.grid.block_frames,
        "lookahead_frames": v.grid.lookahead_frames,
        "pipeline_frames": v.pipeline_frames,
        "band_hz": [num(v.band_hz.0), num(v.band_hz.1)],
        "actual": v.grid.actual,
    })
}

fn manifest(f: &StartFacts<'_>) -> Map<String, Value> {
    let w = f.wire;
    let d = f.derived;
    let mut m = Map::new();
    m.insert("test".into(), Value::from("explorer"));
    m.insert("run_uid".into(), Value::from(f.run_uid));
    m.insert("run_id".into(), Value::from(f.run_id));
    m.insert("title".into(), Value::from(w.title.as_str()));
    m.insert("operator".into(), Value::from(w.operator.as_str()));
    m.insert("article".into(), Value::from(w.article.as_str()));
    m.insert("notes".into(), Value::from(w.notes.as_str()));
    if let Some((document, clause)) = &w.specification {
        m.insert(
            "specification".into(),
            json!({"document": document, "clause": clause}),
        );
    }
    m.insert(
        "metadata".into(),
        Value::Array(
            w.metadata
                .iter()
                .map(|x| json!({"key": x.key, "label": x.label, "value": x.value}))
                .collect(),
        ),
    );
    m.insert("source".into(), Value::from(source_of(f.class)));
    if let Some(sim) = sim_value(f.plan, f.class) {
        m.insert("sim".into(), sim);
    }
    m.insert(
        "station".into(),
        station_member(
            matches!(f.plan, StationPlan::Sim { .. }),
            f.class,
            f.provenance,
            f.devices,
            f.caps,
        ),
    );
    m.insert(
        "grid".into(),
        json!({
            "requested_fs_hz": num(w.fs()),
            "fs_hz": num(d.fs_hz),
            "block_frames": d.block_frames,
            "lookahead_frames": d.lookahead_frames,
            "pipeline_frames": d.pipeline_frames,
            "band_hz": [num(d.band_hz.0), num(d.band_hz.1)],
            "actual": false,
        }),
    );
    m.insert("build".into(), build_value(f.provenance.core_build));
    m.insert(
        "armed".into(),
        json!({"unix": f.started_unix, "utc_offset_s": f.utc_offset_s}),
    );
    m.insert(
        "start".into(),
        start_json(f.started_unix, f.utc_offset_s, 0),
    );
    m.insert(
        "recording".into(),
        match &f.streams_off {
            None => json!({"streams": "on"}),
            Some(why) => json!({"streams": "off", "why": why}),
        },
    );
    m
}

/// Every abort in force: the drive ceiling (a generated sample over it aborts) and each
/// enabled row's limits, which the watchdog reads on every sample and the derived
/// motion's second reading against once settled.
fn aborts_json(w: &WireExplorer, table: &ChannelTable) -> Value {
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
    json!({
        "drive": {"ceiling_vpk": num(w.max_drive_vpk)},
        "channels": channels,
    })
}

fn setup(f: &StartFacts<'_>) -> Value {
    let w = f.wire;
    let table = f.table;
    let control = WireExplorer::control_row(table);
    let drive = WireExplorer::drive_row(table);
    json!({
        "config": w.raw,
        "resolved": {
            "acquisition": {"requested_fs_hz": num(w.fs()), "fs_hz": num(f.derived.fs_hz),
                            "block_frames": f.derived.block_frames},
            "band_hz": [num(w.band_hz.0), num(w.band_hz.1)],
            "start_hz": num(w.start_hz),
            "drive": {"channel": drive.map(|c| c.index), "unit": "V",
                      "max_drive_vpk": num(w.max_drive_vpk), "start_vpk": 0},
            "slew": {"freq_oct_per_s": num(w.freq_oct_per_s),
                     "drive_db_per_s": num(w.drive_db_per_s)},
            "ramp": {"span_db": num(AmplitudeRamp::default().ramp_span_db)},
            "display": {"nfft": w.nfft, "window": "hann", "amplitude": "peak"},
            "survey": {
                "channel": control.map(|c| c.index),
                "unit": control.map(|c| unit_name(&c.units)),
                "window_s": num(hertzforge_explorer_host::analysis::PROFILE_WINDOW_SECONDS),
                "level": "peak",
            },
            "inputs": input_rows(table).iter().map(|c| c.index).collect::<Vec<_>>(),
            "aborts": aborts_json(w, table),
            "capture": {"run_name": w.run_name},
        },
        "derived": f.derived.to_json(),
    })
}

/// The record an Explorer run begins with, at start: before the station is opened.
pub fn begin(f: &StartFacts<'_>, root: PathBuf, links: (TapLink, TapLink)) -> Begin {
    let fs = f.derived.fs_hz;
    let mut streams = Vec::new();
    if f.streams_off.is_none() {
        let mut inputs = Map::new();
        inputs.insert("kind".into(), Value::from("inputs"));
        inputs.insert("fs_hz".into(), num(fs));
        inputs.insert(
            "channels".into(),
            Value::Array(
                input_rows(f.table)
                    .into_iter()
                    .map(stream_channel_value)
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
            d.insert("fs_hz".into(), num(fs));
            d.insert(
                "channels".into(),
                Value::Array(drives.into_iter().map(stream_channel_value).collect()),
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
    Begin {
        root,
        name: f.wire.run_name.clone(),
        manifest: manifest(f),
        table: table_json(f.table, f.revision),
        setup: setup(f),
        law: None,
        log: Vec::new(),
        streams,
    }
}

/// What the run measured at the end: each acquired channel's rms, the measured
/// frequency, the control row's derived motion and the survey's peak.
pub fn measured_json(v: &ExplorerView) -> Value {
    json!({
        "channels": v.channels.iter().map(|c| json!({
            "index": c.index, "unit": c.unit, "rms": c.rms.map(num)})).collect::<Vec<_>>(),
        "measured_hz": v.measured_hz.map(num),
        "control": v.control.as_ref().map(|c| json!({
            "index": c.index, "velocity_ips": c.velocity_ips.map(num),
            "displacement_mil": c.displacement_mil.map(num), "settling": c.settling})),
        "peak": v.peak.map(|(hz, level)| json!({"hz": num(hz), "level": num(level)})),
    })
}

/// `end.json` as the run states it: the end, the station frames acquired, the run's
/// state at the end and what it measured. The writer adds each stream's counts.
pub fn end_map(
    run_uid: &str,
    v: &ExplorerView,
    at_unix: f64,
    station_frames: Option<u64>,
) -> Map<String, Value> {
    let mut end = Map::new();
    end.insert("run_uid".into(), Value::from(run_uid));
    end.insert("run_id".into(), Value::from(v.run_id.as_str()));
    end.insert(
        "end".into(),
        v.end.as_ref().map_or(Value::Null, End::to_json),
    );
    end.insert("at_unix".into(), Value::from(at_unix));
    end.insert(
        "station_frames".into(),
        station_frames.map_or(Value::Null, Value::from),
    );
    end.insert("measured".into(), measured_json(v));
    end.insert("state".into(), state_json(Some(v)));
    end
}
