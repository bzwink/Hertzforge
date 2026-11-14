//! What a TWR run puts in its record (`docs/run-folder.md`, "What a TWR Run
//! Saves"): the manifest, the table and the setup at arm, the two streams of shots
//! (every enabled input and the drive as played, one shot for the identification
//! and one for every play), and the results (the reference as imported, the
//! target it was prepared into at full level, the identification's transfer
//! function and coherence, each play's measures and outcome, and every call of
//! the control law with the drive it returned).
//!
//! The writing is the record's ([`crate::record`]); this module only says what is
//! written, in the reader's form (`hertzforge.io.run_folder`). The station wrapper
//! that feeds the streams is Classical Shock's
//! ([`crate::shock::record::ShotRecording`]): a TWR play is one start and one stop
//! of the station, as a pulse is. Values are in the channels' own units, as the
//! table names them: the analysis side converts.

use super::config::{excitation_name, mode_name, WireTwr};
use super::reference::Stored;
use super::view::{PlayRow, Trip};
use crate::protocol::num;
use crate::random::record::{self as rec, drive_rows, input_rows, Described};
use crate::record::{ArrayValues, Begin, ResultArray, StreamSpec};
use crate::shock::record::StationFacts;
use crate::table::unit_name;
use hertzforge_channel_table::{Channel, ChannelRole, ChannelTable};
use hertzforge_twr::schedule::GateFailPolicy;
use hertzforge_twr::TwrCallRecord;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// The facts of an armed run its record begins with.
pub struct ArmFacts<'a> {
    pub run_id: &'a str,
    pub run_uid: &'a str,
    pub cfg: &'a WireTwr,
    /// The derived values as configure gave them.
    pub derived: &'a Value,
    pub table: &'a ChannelTable,
    pub revision: u64,
    pub station: &'a StationFacts,
    pub stored: &'a Stored,
    /// The rate the station runs at, and the record every play is played as: its
    /// length and its section.
    pub fs_hz: f64,
    pub record_len: usize,
    pub section_start: usize,
    pub section_len: usize,
    pub feasibility: &'a (&'static str, String),
    pub law: Value,
    pub armed_unix: f64,
    pub utc_offset_s: Option<i64>,
    /// `None` when the streams are recorded; otherwise why not.
    pub streams_off: Option<String>,
}

fn control_row(table: &ChannelTable) -> Option<&Channel> {
    table
        .channels
        .iter()
        .find(|c| c.enabled && c.role == ChannelRole::Control)
}

fn gate_fail_name(p: GateFailPolicy) -> &'static str {
    match p {
        GateFailPolicy::Abort => "abort",
        GateFailPolicy::Hold => "hold",
    }
}

fn pair(p: (f64, f64)) -> Value {
    json!([num(p.0), num(p.1)])
}

/// The setup as accepted and as resolved: the reference and the record it was
/// prepared into, the identification, the inverse and the correction, the control
/// law and its guards, the level schedule and the playback, the limits and the
/// feasibility ceilings, and the rate.
fn setup_json(f: &ArmFacts<'_>) -> Value {
    let cfg = f.cfg;
    let shape = &cfg.shape;
    let s = f.stored;
    let band = shape.band(cfg.sysid_band_hz);
    let plays_target = f
        .derived
        .get("plays_target")
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "config": cfg.raw,
        "resolved": {
            "reference": {
                "name": s.name,
                "sha256": s.sha256,
                "format": s.format,
                "fs_hz": num(s.fs_hz),
                "samples": s.samples.len(),
                "duration_s": num(s.duration_s()),
                "units": s.units,
                "prep": {
                    "auto_resample": shape.prep.auto_resample,
                    "detrend": shape.prep.detrend.name(),
                    "highpass_hz": shape.prep.highpass_hz.map(num),
                    "scale": num(shape.prep.scale),
                    "taper_frac": shape.prep.taper_frac.map(num),
                },
                "section": shape.section.map(|(start, len)| json!({"start": start, "len": len})),
                "pads": {"pre": shape.pads.0, "post": shape.pads.1},
            },
            "record": {
                "fs_hz": num(f.fs_hz),
                "record_len": f.record_len,
                "section_start": f.section_start,
                "section_len": f.section_len,
                "section_s": num(f.section_len as f64 / f.fs_hz.max(f64::MIN_POSITIVE)),
            },
            "identification": {
                "excitation": excitation_name(cfg.excitation),
                "drive_vrms": num(cfg.sysid_drive_vrms),
                "duration_s": num(cfg.sysid_duration_s),
                "nfft": cfg.nfft,
                "band_hz": pair(cfg.sysid_band_hz),
                "min_mean_coherence": num(cfg.min_mean_coherence),
                "review": cfg.review,
            },
            "iteration": {
                "inverse": {"tikhonov_floor": num(cfg.tikhonov_floor),
                            "coherence_floor": num(cfg.coherence_floor)},
                "correction": {"band_hz": pair(band),
                               "ref_floor_db": num(shape.correction.ref_floor_db),
                               "min_band_energy": num(shape.correction.min_band_energy)},
                "during_playback": cfg.during_playback,
            },
            "control": {
                "law": {"id": cfg.law.id, "params": cfg.law.params},
                "law_budget": num(cfg.law_budget),
                "law_misses": cfg.law_misses,
                "max_peak_rise_db": num(cfg.max_peak_rise_db),
                "max_bin_rise_db": num(cfg.max_bin_rise_db),
                "first_drive_ratio": num(cfg.first_drive_ratio),
                "alignment_frames": num(cfg.alignment_frames),
                "controller": serde_json::to_value(cfg.control()).unwrap_or(Value::Null),
            },
            "schedule": {
                "mode": mode_name(cfg.mode),
                "levels_db": cfg.levels_db.iter().map(|l| num(*l)).collect::<Vec<_>>(),
                "gate_pct": num(cfg.gate_pct),
                "max_iterations": cfg.max_iterations,
                "on_gate_fail": gate_fail_name(cfg.on_gate_fail),
                "run": cfg.run.to_json(),
                "plays_target": plays_target,
            },
            "limits": {
                "drive_vpk_ceiling": num(cfg.drive_vpk_ceiling),
                "drive_clip_vpk": cfg.drive_clip_vpk.map(num),
                "drive_vrms_limit": cfg.drive_vrms_limit.map(num),
                "response_rms_limit": cfg.response_rms_limit.map(num),
                "envelope_alarm_frac": num(cfg.envelope_alarm_frac),
                "envelope_abort_frac": num(cfg.envelope_abort_frac),
                "envelope_debounce_samples": cfg.envelope_debounce_samples,
                "divergence_plays": cfg.divergence_plays,
                "divergence_rise_frac": num(cfg.divergence_rise_frac),
                "feasibility": {
                    "peak_accel": shape.feasibility.peak_accel.map(num),
                    "peak_velocity_ips": shape.feasibility.peak_velocity_ips.map(num),
                    "disp_pkpk_in": shape.feasibility.disp_pkpk_in.map(num),
                    "kinematics_hp_hz": num(shape.feasibility.kinematics_hp_hz),
                },
            },
            "acquisition": {"requested_fs_hz": num(shape.fs_for(s.fs_hz)), "fs_hz": num(f.fs_hz),
                            "record_len": f.record_len},
            "control_channel": control_row(f.table).map(|c| c.index),
            "drive_channel": drive_rows(f.table).first().map(|c| c.index),
        },
        "derived": f.derived,
        "feasibility": {
            "inputs": cfg.raw.get("feasibility").cloned().unwrap_or(Value::Null),
            "verdict": f.feasibility.0,
            "message": f.feasibility.1,
        },
    })
}

fn stream_channel(c: &Channel) -> Value {
    json!({"index": c.index, "unit": unit_name(&c.units), "description": c.description})
}

/// The record a TWR run begins with, at arm.
pub fn begin(f: &ArmFacts<'_>, root: PathBuf) -> Begin {
    let cfg = f.cfg;
    let mut manifest = Map::new();
    manifest.insert("test".into(), Value::from("twr"));
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
    manifest.insert("source".into(), Value::from(f.station.source));
    if let Some(sim) = &f.station.sim {
        manifest.insert("sim".into(), sim.clone());
    }
    manifest.insert("station".into(), f.station.station.clone());
    manifest.insert(
        "grid".into(),
        json!({"requested_fs_hz": num(cfg.shape.fs_for(f.stored.fs_hz)), "fs_hz": num(f.fs_hz),
               "record_len": f.record_len, "section_start": f.section_start,
               "section_len": f.section_len}),
    );
    manifest.insert("build".into(), f.station.build.clone());
    manifest.insert(
        "armed".into(),
        json!({"unix": f.armed_unix, "utc_offset_s": f.utc_offset_s}),
    );
    manifest.insert(
        "start".into(),
        rec::start_json(f.armed_unix, f.utc_offset_s, 0),
    );
    manifest.insert(
        "recording".into(),
        match &f.streams_off {
            None => json!({"streams": "on"}),
            Some(why) => json!({"streams": "off", "why": why}),
        },
    );
    let mut streams = Vec::new();
    if let (None, Some((inputs_link, drives_link))) = (&f.streams_off, &f.station.links) {
        let shots = |kind: &str, channels: Vec<Value>| {
            let mut d = Map::new();
            d.insert("kind".into(), Value::from(kind));
            d.insert("fs_hz".into(), num(f.fs_hz));
            d.insert("channels".into(), Value::Array(channels));
            d.insert("first_station_frame".into(), Value::from(0));
            d.insert("shots".into(), Value::Array(Vec::new()));
            d
        };
        streams.push(StreamSpec {
            name: "inputs".into(),
            description: shots(
                "inputs",
                input_rows(f.table)
                    .into_iter()
                    .map(stream_channel)
                    .collect(),
            ),
            link: inputs_link.clone(),
        });
        let drives = drive_rows(f.table);
        if !drives.is_empty() {
            let mut d = shots("drives", drives.into_iter().map(stream_channel).collect());
            d.insert(
                "output_latency_frames".into(),
                f.station.output_latency.clone(),
            );
            streams.push(StreamSpec {
                name: "drives".into(),
                description: d,
                link: drives_link.clone(),
            });
        }
    }
    let name = format!("{}_twr_{}", rec::utc_stamp(f.armed_unix), f.run_id);
    Begin {
        root,
        name,
        manifest,
        table: rec::table_json(f.table, f.revision),
        setup: setup_json(f),
        law: Some((f.law.clone(), None)),
        log: Vec::new(),
        streams,
    }
}

/// How a play ended, for its shot and its results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayOutcome {
    /// Played through and judged.
    Judged,
    /// The run ended on it: a protection tripped during the play or on what it
    /// measured. A play cut short is in the streams as far as it was acquired.
    Tripped,
    /// A stop or an abort cut it short.
    Cancelled,
}

impl PlayOutcome {
    pub fn name(self) -> &'static str {
        match self {
            PlayOutcome::Judged => "judged",
            PlayOutcome::Tripped => "tripped",
            PlayOutcome::Cancelled => "cancelled",
        }
    }
}

/// A play as the record keeps it: its row when it was judged, else its index
/// and level, and how it ended.
pub struct PlayFacts<'a> {
    pub index: u64,
    pub level_db: f64,
    pub playback: bool,
    pub iteration: Option<u32>,
    pub row: Option<&'a PlayRow>,
    pub outcome: PlayOutcome,
    pub trip: Option<&'a Trip>,
    /// The frames a cancelled play had played.
    pub frames_done: Option<u64>,
}

/// What shot `ordinal` of both streams was: the identification (shot 0), or a
/// play, with its level, its kind and how it ended, and the frames of the record
/// before the section starts (time zero; the identification starts at its first
/// frame).
pub fn shot_label(play: Option<&PlayFacts<'_>>, pre_frames: usize) -> Map<String, Value> {
    let mut m = Map::new();
    match play {
        None => {
            m.insert("what".into(), Value::from("identification"));
            m.insert("play".into(), Value::Null);
            m.insert("level_db".into(), Value::Null);
            m.insert("pre_frames".into(), Value::from(0));
        }
        Some(p) => {
            m.insert("what".into(), Value::from("play"));
            m.insert("play".into(), Value::from(p.index));
            m.insert(
                "kind".into(),
                Value::from(if p.playback { "playback" } else { "iteration" }),
            );
            m.insert(
                "iteration".into(),
                p.iteration.map_or(Value::Null, Value::from),
            );
            m.insert("level_db".into(), num(p.level_db));
            m.insert("outcome".into(), Value::from(p.outcome.name()));
            m.insert("pre_frames".into(), Value::from(pre_frames));
        }
    }
    m
}

// ---- results ----------------------------------------------------------------------

fn f64s(name: &str, shape: Vec<usize>, unit: &str, values: Vec<f64>) -> ResultArray {
    ResultArray {
        name: name.into(),
        shape,
        unit: unit.into(),
        values: ArrayValues::F64(values),
    }
}

fn described(kind: &str, abscissa: Value, channels: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), Value::from(kind));
    m.insert("abscissa".into(), abscissa);
    m.insert("channels".into(), channels);
    m
}

/// The reference as imported, at its own rate (`reference`), and the target every
/// play is judged against at full level: the prepared section laid on the
/// record's samples with its pads, time from the section's start (`target`).
pub fn reference_results(
    stored: &Stored,
    target: Option<(&[f64], f64, usize, usize)>,
    alarm_frac: f64,
    abort_frac: f64,
    control: u32,
) -> Vec<Described> {
    let mut out = Vec::new();
    let n = stored.samples.len();
    let fs = stored.fs_hz.max(f64::MIN_POSITIVE);
    let mut d = described(
        "waveform",
        json!({"kind": "time", "unit": "s", "array": "time"}),
        json!({"responses": [control]}),
    );
    d.insert("name".into(), Value::from(stored.name.as_str()));
    d.insert("sha256".into(), Value::from(stored.sha256.as_str()));
    d.insert("format".into(), Value::from(stored.format));
    d.insert("fs_hz".into(), num(stored.fs_hz));
    d.insert(
        "description".into(),
        Value::from("the reference as imported, at its own rate, before it was prepared"),
    );
    out.push((
        "reference".to_owned(),
        d,
        vec![
            f64s(
                "time",
                vec![n],
                "s",
                (0..n).map(|i| i as f64 / fs).collect(),
            ),
            f64s("values", vec![n], &stored.units, stored.samples.clone()),
        ],
    ));
    if let Some((values, fs_hz, start, len)) = target {
        let m = values.len();
        let peak = values.iter().fold(0.0f64, |p, v| p.max(v.abs()));
        let mut d = described(
            "transient_reference",
            json!({"kind": "time", "unit": "s", "array": "time"}),
            json!({"responses": [control]}),
        );
        d.insert("fs_hz".into(), num(fs_hz));
        d.insert("section_start".into(), Value::from(start));
        d.insert("section_len".into(), Value::from(len));
        d.insert("peak".into(), num(peak));
        d.insert("alarm_frac".into(), num(alarm_frac));
        d.insert("abort_frac".into(), num(abort_frac));
        d.insert(
            "description".into(),
            Value::from(
                "the target at full level: the prepared section on the record's samples with its \
                 pads, time from the section's start; a play at a level is judged against it \
                 scaled to the level, its alarm and abort bands that share of the peak either \
                 side",
            ),
        );
        let fs = fs_hz.max(f64::MIN_POSITIVE);
        out.push((
            "target".to_owned(),
            d,
            vec![
                f64s(
                    "time",
                    vec![m],
                    "s",
                    (0..m).map(|i| (i as f64 - start as f64) / fs).collect(),
                ),
                f64s("reference", vec![m], &stored.units, values.to_vec()),
            ],
        ));
    }
    out
}

fn or_nan(x: Option<f64>) -> f64 {
    x.unwrap_or(f64::NAN)
}

fn flag(b: Option<bool>) -> f64 {
    match b {
        Some(true) => 1.0,
        Some(false) => 0.0,
        None => f64::NAN,
    }
}

/// One play's results, a record of `plays` written when the play is judged (or
/// ends the run, or is cut short): its level, its error measures, whether it met
/// the gate, its drive and response peaks, its lag and its place in station time,
/// and how it ended. `frame` is the station frame when it was kept.
pub fn play_result(
    p: &PlayFacts<'_>,
    shot: Option<usize>,
    frame: Option<u64>,
    control: u32,
    unit: &str,
) -> (Described, Value) {
    let mut d = described(
        "play_results",
        json!({"kind": "none"}),
        json!({"responses": [control]}),
    );
    d.insert(
        "description".into(),
        Value::from(
            "each play as judged: its level, error measures, gate (1 met, 0 not, NaN not \
             judged), drive and response peaks, lag, and how it ended (tripped and cancelled 1 or 0)",
        ),
    );
    d.insert(
        "records".into(),
        json!({"index": "the play, from 0", "kind": "iteration or playback",
               "level_db": "the level played", "iteration": "from 1 at its level",
               "outcome": "judged, tripped or cancelled", "trip": "what ended the run on it",
               "shot": "its shot in the streams", "frame": "the station frame when it was kept"}),
    );
    let one = |name: &str, u: &str, v: f64| f64s(name, vec![1], u, vec![v]);
    let r = p.row;
    let arrays = vec![
        one("level_db", "dB", p.level_db),
        one("playback", "", if p.playback { 1.0 } else { 0.0 }),
        one("iteration", "", p.iteration.map_or(f64::NAN, f64::from)),
        one("rms_error", "%", r.map_or(f64::NAN, |r| r.rms_error_pct)),
        one("peak_error", "%", r.map_or(f64::NAN, |r| r.peak_error_pct)),
        one(
            "spectral_error",
            "dB",
            r.map_or(f64::NAN, |r| r.spectral_error_db),
        ),
        one("gated", "", flag(r.map(|r| r.gated))),
        one(
            "clipped_samples",
            "",
            r.map_or(f64::NAN, |r| r.clipped_samples as f64),
        ),
        one(
            "clamped_bins",
            "",
            r.map_or(f64::NAN, |r| r.clamped_bins as f64),
        ),
        one("drive_peak", "V", r.map_or(f64::NAN, |r| r.drive_vpk)),
        one(
            "response_peak",
            unit,
            r.map_or(f64::NAN, |r| r.response_peak),
        ),
        one("lag", "frames", or_nan(r.and_then(|r| r.lag_frames))),
        one("drift", "frames", or_nan(r.and_then(|r| r.drift_frames))),
        one(
            "start_frame",
            "frames",
            r.map_or(f64::NAN, |r| r.start_frame as f64),
        ),
        one(
            "frames",
            "frames",
            r.map(|r| r.frames as f64)
                .or(p.frames_done.map(|f| f as f64))
                .unwrap_or(f64::NAN),
        ),
        one("at_s", "s", r.map_or(f64::NAN, |r| r.at_s)),
        one(
            "tripped",
            "",
            if p.outcome == PlayOutcome::Tripped {
                1.0
            } else {
                0.0
            },
        ),
        one(
            "cancelled",
            "",
            if p.outcome == PlayOutcome::Cancelled {
                1.0
            } else {
                0.0
            },
        ),
    ];
    let line = json!({
        "index": p.index,
        "kind": if p.playback { "playback" } else { "iteration" },
        "level_db": num(p.level_db),
        "iteration": p.iteration,
        "gated": r.map(|r| r.gated),
        "outcome": p.outcome.name(),
        "trip": p.trip.map_or(Value::Null, Trip::to_json),
        "frames_done": p.frames_done,
        "shot": shot,
        "frame": frame,
    });
    (("plays".to_owned(), d, arrays), line)
}

/// One call of the control law, a record of `law_calls` written between plays:
/// the drive it returned at full level on the record's samples (NaN when it gave
/// none, or when it gave fewer than `record_len`), the level and error it was
/// handed, its caps and how long it took. `index` is its place in the run's calls.
pub fn law_call_result(
    call: &TwrCallRecord,
    index: usize,
    grid: (f64, usize, usize),
    frame: Option<u64>,
) -> (Described, Value) {
    let p = &call.payload;
    let (fs_hz, record_len, section_start) = grid;
    let fs = fs_hz.max(f64::MIN_POSITIVE);
    let mut d = described(
        "law_calls",
        json!({"kind": "time", "unit": "s", "start": -(section_start as f64) / fs,
               "step": 1.0 / fs}),
        json!({}),
    );
    d.insert(
        "description".into(),
        Value::from(
            "every call of the control law: the drive it returned at full level, on the \
             record's samples, and what it was handed",
        ),
    );
    d.insert(
        "records".into(),
        json!({"index": "the call, from 0", "after_plays": "the plays before it (0 at the seed)",
               "call": "seed or iterate", "kind": "the kind of the play it followed",
               "used": "whether its drive was used", "miss": "why not",
               "values": "the values the law reported",
               "frame": "the station frame when it was kept"}),
    );
    let mut drive: Vec<f64> = p.drive_out.iter().map(|v| f64::from(*v)).collect();
    drive.resize(record_len, f64::NAN);
    let one = |name: &str, u: &str, v: f64| f64s(name, vec![1], u, vec![v]);
    let arrays = vec![
        f64s("drive", vec![record_len], "V", drive),
        one("level_db", "dB", p.level_db),
        one("rms_error", "%", or_nan(p.rms_error_pct)),
        one("peak_rise", "dB", or_nan(p.peak_rise_db)),
        one("bin_rise", "dB", or_nan(p.bin_rise_db)),
        one("predicted_peak_ratio", "", or_nan(p.predicted_peak_ratio)),
        one("lag", "frames", or_nan(p.lag_frames)),
        one("used", "", if call.miss.is_none() { 1.0 } else { 0.0 }),
        one("call_ms", "ms", call.call_us as f64 / 1000.0),
    ];
    let mut values = Map::new();
    for v in &p.values {
        values.insert(v.key.clone(), num(v.value));
    }
    let line = json!({
        "index": index,
        "after_plays": call.pulse_index,
        "call": serde_json::to_value(call.call).unwrap_or(Value::Null),
        "kind": p.kind.as_str(),
        "used": call.miss.is_none(),
        "miss": call.miss.as_ref().map(|m| m.reason()),
        "values": values,
        "frame": frame,
    });
    (("law_calls".to_owned(), d, arrays), line)
}
