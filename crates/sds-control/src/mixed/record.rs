//! What a Mixed Mode run puts in its record (`docs/run-folder.md`, "What a Mixed
//! Mode Run Saves"): the manifest, the table and the setup at arm, the two streams,
//! and the results (the background reference and its bands on the run's lines, the
//! limit lines, the identification's transfer function and coherence, the
//! background's spectra as the module keeps them, each part's level as the run
//! went, and the laws' calls).
//!
//! The writing is the record's ([`crate::record`]); this module only says what is
//! written, in the reader's form (`hertzforge.io.run_folder`). Values are in the
//! channels' own units, as the table names them: the analysis side converts.

use super::config::{DwellEnd, WireAmplitude, WireBody, WireMixed, WireSchedule, WireWidth};
use crate::protocol::num;
use crate::random::record as rrec;
use crate::record::{tap::TapLink, ArrayValues, Begin, ResultArray, StreamSpec};
use crate::station::{source_of, StationPlan};
use crate::table::unit_name;
use hertzforge_channel_table::{Channel, ChannelRole, ChannelTable};
use hertzforge_dsp::sweep::SweepRate;
use hertzforge_hal::{Caps, DiscoveredDevice};
use hertzforge_mixed::service::law_records::{MixedLawRecord, LAW_TICK_FIELDS};
use hertzforge_mixed::{Direction, MixedConfig, MixedDisplay, PartKind, PartRow};
use hertzforge_module_abi::StationClass;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Points of one record of the part levels: the record repeats every this many
/// points, the last one filled out with NaN.
pub const LEVEL_BLOCK: usize = 32;
/// Calls of one record of `control_trace`, the last one filled out with NaN.
pub const TRACE_BLOCK: usize = 64;

/// The facts of an armed run its record begins with.
pub struct ArmFacts<'a> {
    pub run_id: &'a str,
    pub run_uid: &'a str,
    pub cfg: &'a WireMixed,
    /// The controller's configuration as it armed.
    pub controller: &'a MixedConfig,
    pub table: &'a ChannelTable,
    pub revision: u64,
    pub plan: &'a StationPlan,
    pub class: StationClass,
    pub provenance: &'a hertzforge_station::ProvenanceBasis,
    pub devices: &'a [DiscoveredDevice],
    pub caps: &'a Caps,
    /// The rate the station programmed, the control tick in frames, and the line
    /// spacing of the background's grid.
    pub fs_hz: f64,
    pub tick_frames: usize,
    pub df_hz: f64,
    pub derived: Value,
    pub feasibility: Option<&'a (&'static str, String)>,
    pub law: Value,
    pub armed_unix: f64,
    pub utc_offset_s: Option<i64>,
    /// `None` when the streams are recorded; otherwise why not.
    pub streams_off: Option<String>,
}

fn pair(p: (f64, f64)) -> Value {
    json!([num(p.0), num(p.1)])
}

fn rate_json(rate: &SweepRate) -> Value {
    match *rate {
        SweepRate::LogOctPerMin(x) => json!({"law": "log", "oct_per_min": num(x)}),
        SweepRate::LinearHzPerSec(x) => json!({"law": "linear", "hz_per_s": num(x)}),
    }
}

fn schedule_json(s: &WireSchedule) -> Value {
    match s {
        WireSchedule::Fixed { freq_hz, end } => json!({
            "kind": "fixed", "freq_hz": num(*freq_hz),
            "end": match end {
                None => Value::Null,
                Some(DwellEnd::Seconds(x)) => json!({"seconds": num(*x)}),
                Some(DwellEnd::Cycles(n)) => json!({"cycles": n}),
            },
        }),
        WireSchedule::Sweep {
            band,
            rate,
            start_hz,
            direction,
            sweeps,
            duration_s,
        } => json!({
            "kind": "sweep", "band_hz": pair(*band), "rate": rate_json(rate),
            "start_hz": start_hz.map(num),
            "direction": match direction { Direction::Up => "up", Direction::Down => "down" },
            "sweeps": sweeps, "duration_s": duration_s.map(num),
        }),
        WireSchedule::Follow { of, ratio } => {
            json!({"kind": "follow", "of": of, "ratio": num(*ratio)})
        }
        WireSchedule::Segments => json!({"kind": "segments"}),
    }
}

/// Every part as resolved: its identity, its window, its schedule (sweep, dwell
/// or follow), its level, and its alarm and abort bands; the floor's breakpoints
/// are the setup's `resolved.profile`.
fn parts_json(cfg: &WireMixed) -> Value {
    let names = super::config::part_names(&cfg.parts);
    Value::Array(
        cfg.parts
            .iter()
            .zip(names)
            .map(|(p, name)| {
                let mut m = Map::new();
                m.insert("id".into(), Value::from(p.id.as_str()));
                m.insert("kind".into(), Value::from(p.kind()));
                m.insert("name".into(), Value::from(name));
                m.insert("enabled".into(), Value::from(p.enabled));
                m.insert(
                    "window".into(),
                    json!({"on_s": num(p.on_s), "off_s": p.off_s.map(num)}),
                );
                match &p.body {
                    WireBody::Floor => {
                        m.insert("profile".into(), Value::from("resolved.profile"));
                    }
                    WireBody::Tone {
                        schedule,
                        amplitude,
                        alarm_db,
                        abort_db,
                    } => {
                        m.insert("schedule".into(), schedule_json(schedule));
                        match amplitude {
                            WireAmplitude::Constant(g) => {
                                m.insert("amplitude_g_pk".into(), num(*g));
                            }
                            WireAmplitude::Profile(pr) => {
                                m.insert("profile".into(), crate::sine::record::profile_json(pr));
                            }
                        }
                        m.insert("alarm_db".into(), pair(*alarm_db));
                        m.insert("abort_db".into(), pair(*abort_db));
                    }
                    WireBody::Narrowband {
                        schedule,
                        level_g2_hz,
                        width,
                        alarm_db,
                        abort_db,
                    } => {
                        m.insert("schedule".into(), schedule_json(schedule));
                        m.insert("level_g2_hz".into(), num(*level_g2_hz));
                        match width {
                            WireWidth::Hz(w) => m.insert("width_hz".into(), num(*w)),
                            WireWidth::Share(s) => m.insert("width_share".into(), num(*s)),
                        };
                        m.insert("alarm_db".into(), alarm_db.map_or(Value::Null, pair));
                        m.insert("abort_db".into(), abort_db.map_or(Value::Null, pair));
                    }
                    WireBody::Unknown(_) => {}
                }
                Value::Object(m)
            })
            .collect(),
    )
}

fn setup_json(f: &ArmFacts<'_>) -> Value {
    let cfg = f.cfg;
    let rw = &cfg.random;
    let last = rw.levels.len().saturating_sub(1);
    let schedule: Vec<Value> = rw
        .levels
        .iter()
        .enumerate()
        .map(|(i, (db, dwell))| {
            let dwell = if i == last {
                Some(rw.full_level_s)
            } else {
                *dwell
            };
            json!({"db": num(*db), "dwell_s": dwell.map(num)})
        })
        .collect();
    let (limits, mut aborts) = rrec::limits_json(rw, f.table);
    if let Value::Object(a) = &mut aborts {
        a.insert(
            "peak_drive".into(),
            json!({"ceiling_vpk": num(cfg.drive_vpk_ceiling), "abort_vpk": num(cfg.drive_vpk_abort)}),
        );
        a.insert(
            "kinematic_monitor".into(),
            Value::from(cfg.kinematic_monitor),
        );
        let g = &cfg.guards_tones;
        a.insert(
            "tones".into(),
            json!({"divergence_s": num(g.divergence_s),
                   "fault_debounce_ticks": g.fault_debounce_ticks}),
        );
    }
    let mut control = rrec::control_json(rw);
    if let Value::Object(c) = &mut control {
        c.insert("part_ramp_s".into(), num(cfg.part_ramp_s));
        c.insert("floor_settle_s".into(), num(cfg.floor_settle_s));
        c.insert("exclusion_hz".into(), num(cfg.exclusion_hz));
        c.insert("trust_coherence".into(), num(cfg.trust_coherence));
        c.insert("trust_h_floor_frac".into(), num(cfg.trust_h_floor_frac));
        c.insert(
            "tracking".into(),
            json!({"kind": cfg.tracking_kind, "hz": num(cfg.tracking_hz)}),
        );
        let (fl, to) = (&cfg.guards_floor, &cfg.guards_tones);
        c.insert(
            "law_guards".into(),
            json!({
                "floor": {"law_budget": num(fl.law_budget), "law_misses": fl.law_misses,
                          "max_change_db": num(fl.max_change_db)},
                "tones": {"law_budget": num(to.law_budget), "law_misses": to.law_misses,
                          "max_step_db": num(to.max_step_db),
                          "max_rise_db_per_s": num(to.max_rise_db_per_s),
                          "divergence_s": num(to.divergence_s),
                          "fault_debounce_ticks": to.fault_debounce_ticks},
            }),
        );
    }
    let mut derived = f.derived.clone();
    if let Value::Object(m) = &mut derived {
        m.insert(
            "grid".into(),
            json!({"fs_hz": num(f.fs_hz), "df_hz": num(f.df_hz), "tick_frames": f.tick_frames,
                   "tick_s": num(f.tick_frames as f64 / f.fs_hz), "actual": true}),
        );
    }
    json!({
        "config": cfg.raw,
        "resolved": {
            "profile": rrec::profile_json(rw),
            "parts": parts_json(cfg),
            "schedule": {"mode": crate::random::view::mode_name(rw.mode), "levels": schedule,
                         "full_level_s": num(rw.full_level_s)},
            "tolerances": {"alarm_db": pair(rw.alarm_db), "abort_db": pair(rw.abort_db)},
            "limits": limits,
            "aborts": aborts,
            "control": control,
            "control_channels": crate::random::config::WireConfig::control_channels(f.table),
            "acquisition": {"requested_fs_hz": num(cfg.fs_hz()), "fs_hz": num(f.fs_hz),
                            "n_pts": rw.n_pts, "n_avg": rw.n_avg, "tick_ms": num(cfg.tick_ms),
                            "tick_frames": f.tick_frames},
            "controller": serde_json::to_value(f.controller).unwrap_or(Value::Null),
        },
        "derived": derived,
        "feasibility": {
            "inputs": cfg.raw.get("feasibility").cloned().unwrap_or(Value::Null),
            "verdict": f.feasibility.map(|(v, _)| *v),
            "message": f.feasibility.map(|(_, m)| m.clone()),
        },
    })
}

/// The record a Mixed Mode run begins with, at arm.
pub fn begin(f: &ArmFacts<'_>, root: PathBuf, links: (TapLink, TapLink)) -> Begin {
    let cfg = f.cfg;
    let mut manifest = Map::new();
    manifest.insert("test".into(), Value::from("mixed"));
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
    if let Some(sim) = rrec::sim_value(f.plan, f.class) {
        manifest.insert("sim".into(), sim);
    }
    manifest.insert(
        "station".into(),
        rrec::station_member(
            matches!(f.plan, StationPlan::Sim { .. }),
            f.class,
            f.provenance,
            f.devices,
            f.caps,
        ),
    );
    let rw = &cfg.random;
    manifest.insert(
        "grid".into(),
        json!({"requested_fs_hz": num(cfg.fs_hz()), "fs_hz": num(f.fs_hz),
               "n_pts": rw.n_pts, "averages": rw.n_avg, "df_hz": num(f.df_hz),
               "tick_frames": f.tick_frames, "tick_s": num(f.tick_frames as f64 / f.fs_hz)}),
    );
    manifest.insert("build".into(), rrec::build_value(f.provenance.core_build));
    manifest.insert(
        "armed".into(),
        json!({"unix": f.armed_unix, "utc_offset_s": f.utc_offset_s}),
    );
    manifest.insert(
        "start".into(),
        rrec::start_json(f.armed_unix, f.utc_offset_s, 0),
    );
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
        inputs.insert("fs_hz".into(), num(f.fs_hz));
        inputs.insert(
            "channels".into(),
            Value::Array(
                rrec::input_rows(f.table)
                    .into_iter()
                    .map(rrec::stream_channel_value)
                    .collect(),
            ),
        );
        inputs.insert("first_station_frame".into(), Value::from(0));
        streams.push(StreamSpec {
            name: "inputs".into(),
            description: inputs,
            link: links.0,
        });
        let drives = played_drives(f.table);
        if !drives.is_empty() {
            let mut d = Map::new();
            d.insert("kind".into(), Value::from("drives"));
            d.insert("fs_hz".into(), num(f.fs_hz));
            d.insert(
                "channels".into(),
                Value::Array(drives.into_iter().map(rrec::stream_channel_value).collect()),
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
    let name = format!("{}_mixed_{}", rrec::utc_stamp(f.armed_unix), f.run_id);
    Begin {
        root,
        name,
        manifest,
        table: rrec::table_json(f.table, f.revision),
        setup: setup_json(f),
        law: Some((f.law.clone(), None)),
        log: Vec::new(),
        streams,
    }
}

/// The drive rows the run plays: Mixed Mode plays one drive, the first enabled.
pub fn played_drives(table: &ChannelTable) -> Vec<&Channel> {
    rrec::drive_rows(table).into_iter().take(1).collect()
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

fn controls(table: &ChannelTable) -> Vec<&Channel> {
    table
        .channels
        .iter()
        .filter(|c| c.enabled && c.role == ChannelRole::Control)
        .collect()
}

/// The identification's transfer function per control channel (drive to response,
/// the response taken the lag it measured later) and its coherence, on the
/// background's band. `h` and `coherence` are per control channel, in the order of
/// the table's control rows (the configuration's control set), on lines of `df_hz`
/// from 0 Hz; `composite` is the coherence the gate judged, and `trusted` the lines
/// the control trusts.
#[allow(clippy::too_many_arguments)]
pub fn identification_results(
    h: &[Vec<(f64, f64)>],
    coherence: &[Vec<f64>],
    composite: &[f32],
    trusted: &[bool],
    df_hz: f64,
    band: (f64, f64),
    lag_frames: usize,
    table: &ChannelTable,
) -> Vec<rrec::Described> {
    let ctl = controls(table);
    let Some(drive) = played_drives(table).into_iter().next() else {
        return Vec::new();
    };
    let lines = h.iter().map(Vec::len).min().unwrap_or(0);
    if ctl.is_empty() || ctl.len() != h.len() || coherence.len() != h.len() || df_hz <= 0.0 {
        return Vec::new();
    }
    let first = ((band.0 / df_hz).ceil().max(0.0) as usize).min(lines);
    let last = ((band.1 / df_hz).floor().max(0.0) as usize + 1).min(lines);
    if first >= last {
        return Vec::new();
    }
    let n = last - first;
    let m = ctl.len();
    let indexes: Vec<u32> = ctl.iter().map(|c| c.index).collect();
    let channels = json!({"responses": indexes, "references": [drive.index]});
    let axis = json!({"kind": "frequency", "unit": "Hz", "start": first as f64 * df_hz,
                      "step": df_hz});
    let words = format!(
        "identification: drive to each control response, the response taken {lag_frames} \
         frames later (the lag the identification measured)"
    );
    let mut frf = described("frf", axis.clone(), channels.clone());
    frf.insert("convention".into(), Value::from("file"));
    frf.insert("description".into(), Value::from(words.clone()));
    frf.insert("lag_frames".into(), Value::from(lag_frames));
    let mut coh = described("coherence", axis, channels);
    coh.insert(
        "description".into(),
        Value::from(format!(
            "{words}: ordinary coherence per control channel, the composite the gate judged \
             and the lines the control trusts (1) or not (0)"
        )),
    );
    let units: Vec<String> = ctl
        .iter()
        .map(|c| format!("{}/{}", unit_name(&c.units), unit_name(&drive.units)))
        .collect();
    let unit = if units.iter().all(|u| *u == units[0]) {
        units[0].clone()
    } else {
        String::new()
    };
    let values: Vec<(f64, f64)> = h
        .iter()
        .flat_map(|ch| ch[first..last].iter().copied())
        .collect();
    let cvalues: Vec<f64> = coherence
        .iter()
        .flat_map(|ch| ch[first..last].iter().copied())
        .collect();
    let at = |v: &[f32]| -> Vec<f64> {
        (first..last)
            .map(|k| v.get(k).map_or(f64::NAN, |x| f64::from(*x)))
            .collect()
    };
    vec![
        (
            "identification_frf".into(),
            frf,
            vec![ResultArray {
                name: "values".into(),
                shape: vec![m, 1, n],
                unit,
                values: ArrayValues::C128(values),
            }],
        ),
        (
            "identification_coherence".into(),
            coh,
            vec![
                f64s("values", vec![m, 1, n], "", cvalues),
                f64s("composite", vec![n], "", at(composite)),
                f64s(
                    "trusted",
                    vec![n],
                    "",
                    (first..last)
                        .map(|k| f64::from(u8::from(trusted.get(k).copied().unwrap_or(false))))
                        .collect(),
                ),
            ],
        ),
    ]
}

/// The background's spectra of one floor loop, as the module keeps them: each
/// control channel's spectrum with the tones in and with each tracked tone taken
/// out (the residual), their composites, the reference at the level in force, and
/// the random part of the drive. `d` is on the background's band.
pub fn spectra_results(
    d: &MixedDisplay,
    table: &ChannelTable,
    control_unit: &str,
) -> Vec<rrec::Described> {
    let n = d.grid.lines;
    if n == 0 || d.control.len() != n {
        return Vec::new();
    }
    let axis = json!({"kind": "frequency", "unit": "Hz", "start": d.grid.f0_hz,
                      "step": d.grid.df_hz});
    let records = json!({"index": "record", "why": "level (the last chunk before a level \
                         change) or end (the last chunk with the background's loop)",
                         "level_db": "the level in force", "frame": "the station frame when \
                         it was kept"});
    let f = |v: &[f32]| -> Vec<f64> {
        (0..n)
            .map(|k| v.get(k).map_or(f64::NAN, |x| f64::from(*x)))
            .collect()
    };
    let unit = rrec::psd_unit(control_unit);
    let indexes: Vec<u32> = d.channels.iter().map(|c| c.channel_index).collect();
    let m = indexes.len();
    let mut out = Vec::new();
    let mut ctl = described("psd", axis.clone(), json!({"responses": indexes}));
    ctl.insert("records".into(), records.clone());
    ctl.insert(
        "description".into(),
        Value::from(
            "each control channel's spectrum with the tones in, and the composite; \
             the reference at the level in force (the background and each band where it \
             is), with its alarm and abort lines",
        ),
    );
    out.push((
        "control_psd".to_owned(),
        ctl,
        vec![
            f64s(
                "values",
                vec![m, n],
                &unit,
                d.channels.iter().flat_map(|c| f(&c.control)).collect(),
            ),
            f64s("composite", vec![n], &unit, f(&d.control)),
            f64s("reference", vec![n], &unit, f(&d.reference)),
            f64s("alarm_low", vec![n], &unit, f(&d.alarm_lo)),
            f64s("alarm_high", vec![n], &unit, f(&d.alarm_hi)),
            f64s("abort_low", vec![n], &unit, f(&d.abort_lo)),
            f64s("abort_high", vec![n], &unit, f(&d.abort_hi)),
        ],
    ));
    let mut res = described("psd", axis.clone(), json!({"responses": indexes}));
    res.insert("records".into(), records.clone());
    res.insert(
        "description".into(),
        Value::from(
            "each control channel's spectrum with each tracked tone taken out (the \
             residual, what the background and the bands are judged on), the composite, \
             and the lines left out of the background's average (1)",
        ),
    );
    out.push((
        "residual_psd".to_owned(),
        res,
        vec![
            f64s(
                "values",
                vec![m, n],
                &unit,
                d.channels.iter().flat_map(|c| f(&c.residual)).collect(),
            ),
            f64s("composite", vec![n], &unit, f(&d.residual)),
            f64s(
                "excluded",
                vec![n],
                "",
                (0..n)
                    .map(|k| f64::from(u8::from(d.excluded.get(k).copied().unwrap_or(false))))
                    .collect(),
            ),
        ],
    ));
    if let Some(drive) = played_drives(table).into_iter().next() {
        let mut dr = described("psd", axis, json!({"responses": [drive.index]}));
        dr.insert("records".into(), records);
        dr.insert(
            "description".into(),
            Value::from("the random part of the drive (the background and the bands)"),
        );
        out.push((
            "drive_psd".to_owned(),
            dr,
            vec![f64s(
                "values",
                vec![1, n],
                &rrec::psd_unit(&unit_name(&drive.units)),
                f(&d.drive),
            )],
        ));
    }
    out
}

/// One point of the parts' levels as the run went: the status's rows, and each
/// tone's tracked phase per control channel.
#[derive(Debug, Clone, PartialEq)]
pub struct PartPoint {
    pub t_s: f64,
    pub level_db: f64,
    pub rung: usize,
    pub rows: Vec<PartRow>,
    pub phases_deg: Vec<Vec<f64>>,
}

fn padded(mut v: Vec<f64>, n: usize) -> Vec<f64> {
    v.resize(n, f64::NAN);
    v
}

fn opt(x: Option<f64>) -> f64 {
    x.unwrap_or(f64::NAN)
}

fn flag(b: bool) -> f64 {
    f64::from(u8::from(b))
}

/// The record's arrays of the parts of one kind, `[parts, points]`, each from its
/// row (the parts in parts-list order).
fn per_part(
    points: &[PartPoint],
    kind: PartKind,
    g: &dyn Fn(&PartRow, usize, &PartPoint) -> f64,
) -> (usize, Vec<f64>) {
    let n = LEVEL_BLOCK;
    let k = points
        .first()
        .map_or(0, |p| p.rows.iter().filter(|r| r.kind == kind).count());
    let values = (0..k)
        .flat_map(|j| {
            padded(
                points
                    .iter()
                    .map(|p| {
                        p.rows
                            .iter()
                            .filter(|r| r.kind == kind)
                            .nth(j)
                            .map_or(f64::NAN, |r| g(r, j, p))
                    })
                    .collect(),
                n,
            )
        })
        .collect();
    (k, values)
}

/// One record of the parts' levels: `tone_levels`, `band_levels` and
/// `floor_levels`, [`LEVEL_BLOCK`] points each, the unused ones NaN; a kind the
/// test has no part of is left out.
pub fn levels_results(
    points: &[PartPoint],
    parts: &[(String, PartKind)],
    controls: &[u32],
    control_unit: &str,
    drive_unit: &str,
) -> Vec<rrec::Described> {
    let n = LEVEL_BLOCK;
    let col = |g: &dyn Fn(&PartPoint) -> f64| padded(points.iter().map(g).collect(), n);
    let ids = |kind: PartKind| -> Vec<&str> {
        parts
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(id, _)| id.as_str())
            .collect()
    };
    let common = |d: &mut Map<String, Value>, kind: PartKind| {
        d.insert("parts".into(), json!(ids(kind)));
        d.insert(
            "records".into(),
            json!({"index": "record",
                   "points": "the points the record holds before its NaN fill"}),
        );
    };
    let time = || {
        vec![
            f64s("time", vec![n], "s", col(&|p| p.t_s)),
            f64s("level_db", vec![n], "dB", col(&|p| p.level_db)),
            f64s("rung", vec![n], "", col(&|p| p.rung as f64)),
        ]
    };
    let axis = json!({"kind": "time", "unit": "s", "array": "time"});
    let psd = rrec::psd_unit(control_unit);
    let mut out = Vec::new();
    if !ids(PartKind::Tone).is_empty() {
        let mut d = described(
            "mixed_tone_levels",
            axis.clone(),
            json!({"controls": controls}),
        );
        d.insert(
            "description".into(),
            Value::from(
                "each tone's frequency, its target at the level in force, its tracked level \
                 (the control composite, peak), its error, its drive (peak), its tracked phase \
                 against its drive on each control channel, and whether it plays, as the run \
                 went (time from engage); a point is NaN where a record was not filled",
            ),
        );
        common(&mut d, PartKind::Tone);
        let mut arrays = time();
        let mut add = |name: &str, unit: &str, g: &dyn Fn(&PartRow, usize, &PartPoint) -> f64| {
            let (k, v) = per_part(points, PartKind::Tone, g);
            arrays.push(f64s(name, vec![k, n], unit, v));
        };
        add("frequency", "Hz", &|r, _, _| opt(r.freq_hz));
        add("target", control_unit, &|r, _, _| r.target);
        add("measured", control_unit, &|r, _, _| opt(r.measured));
        add("error_db", "dB", &|r, _, _| opt(r.error_db));
        add("drive", drive_unit, &|r, _, _| r.drive);
        add("on", "", &|r, _, _| flag(r.on));
        add("alarm", "", &|r, _, _| flag(r.alarm));
        let tones = ids(PartKind::Tone).len();
        let m = controls.len();
        let phase: Vec<f64> = (0..tones)
            .flat_map(|j| {
                (0..m).flat_map(move |c| {
                    padded(
                        points
                            .iter()
                            .map(|p| {
                                p.phases_deg
                                    .get(j)
                                    .and_then(|v| v.get(c))
                                    .copied()
                                    .unwrap_or(f64::NAN)
                            })
                            .collect(),
                        n,
                    )
                })
            })
            .collect();
        arrays.push(f64s("phase", vec![tones, m, n], "deg", phase));
        out.push(("tone_levels".to_owned(), d, arrays));
    }
    if !ids(PartKind::Narrowband).is_empty() {
        let mut d = described(
            "mixed_band_levels",
            axis.clone(),
            json!({"controls": controls}),
        );
        d.insert(
            "description".into(),
            Value::from(
                "each narrowband's center, its width, its reference at the level in force \
                 (the background and the band), its measured level (the residual's mean over \
                 the band), its error, the degrees of freedom of the reading, its share of the \
                 drive (rms), and whether it plays, as the run went (time from engage)",
            ),
        );
        common(&mut d, PartKind::Narrowband);
        let mut arrays = time();
        let mut add = |name: &str, unit: &str, g: &dyn Fn(&PartRow, usize, &PartPoint) -> f64| {
            let (k, v) = per_part(points, PartKind::Narrowband, g);
            arrays.push(f64s(name, vec![k, n], unit, v));
        };
        add("frequency", "Hz", &|r, _, _| opt(r.freq_hz));
        add("width", "Hz", &|r, _, _| opt(r.width_hz));
        add("target", &psd, &|r, _, _| r.target);
        add("measured", &psd, &|r, _, _| opt(r.measured));
        add("error_db", "dB", &|r, _, _| opt(r.error_db));
        add("dof", "", &|r, _, _| opt(r.dof));
        add("drive", drive_unit, &|r, _, _| r.drive);
        add("on", "", &|r, _, _| flag(r.on));
        add("alarm", "", &|r, _, _| flag(r.alarm));
        out.push(("band_levels".to_owned(), d, arrays));
    }
    if !ids(PartKind::Floor).is_empty() {
        let mut d = described("mixed_floor_levels", axis, json!({"controls": controls}));
        d.insert(
            "description".into(),
            Value::from(
                "the background's rms over its clean lines against its reference's at the \
                 level in force, its error, its drive (rms), and its alarm, as the run went \
                 (time from engage)",
            ),
        );
        common(&mut d, PartKind::Floor);
        let mut arrays = time();
        let mut add = |name: &str, unit: &str, g: &dyn Fn(&PartRow, usize, &PartPoint) -> f64| {
            let (_, v) = per_part(points, PartKind::Floor, g);
            arrays.push(f64s(name, vec![n], unit, v));
        };
        add("target", control_unit, &|r, _, _| r.target);
        add("measured", control_unit, &|r, _, _| opt(r.measured));
        add("error_db", "dB", &|r, _, _| opt(r.error_db));
        add("drive", drive_unit, &|r, _, _| r.drive);
        add("alarm", "", &|r, _, _| flag(r.alarm));
        out.push(("floor_levels".to_owned(), d, arrays));
    }
    out
}

/// One record of `control_trace`: [`TRACE_BLOCK`] law calls, the unused ones NaN.
pub fn trace_result(
    calls: &[MixedLawRecord],
    tones: &[String],
    drive_unit: &str,
) -> rrec::Described {
    let n = TRACE_BLOCK;
    let col = |g: &dyn Fn(&MixedLawRecord) -> f64| padded(calls.iter().map(g).collect(), n);
    let mut d = described(
        "mixed_control",
        json!({"kind": "time", "unit": "s", "array": "time"}),
        json!({}),
    );
    d.insert(
        "description".into(),
        Value::from(
            "every call of the two laws, in the order taken: time from engage, the law \
             (0 the background's, once a loop; 1 a tone's, once a tick), the tone's index \
             among the tones, the drive in force before the call and the drive the core \
             took from it (the background's in-band rms, a tone's amplitude, peak), the \
             call's time, a miss (0 a usable call) and its flags (1 a core cap bound, 2 the \
             engage); a call is NaN where a record was not filled",
        ),
    );
    d.insert("roles".into(), json!(["floor", "tone"]));
    d.insert("tones".into(), json!(tones));
    d.insert(
        "tone_misses".into(),
        json!(hertzforge_mixed::service::TONE_MISS_NAMES),
    );
    d.insert(
        "fields".into(),
        json!(LAW_TICK_FIELDS.iter().map(|(n, _)| n).collect::<Vec<_>>()),
    );
    let arrays = vec![
        f64s("time", vec![n], "s", col(&|r| r.t_s)),
        f64s("role", vec![n], "", col(&|r| f64::from(r.role as u8))),
        f64s("tone", vec![n], "", col(&|r| f64::from(r.tone))),
        f64s("drive_in", vec![n], drive_unit, col(&|r| r.drive_in)),
        f64s("drive_out", vec![n], drive_unit, col(&|r| r.drive_out)),
        f64s("call_us", vec![n], "us", col(&|r| f64::from(r.took_us))),
        f64s("miss", vec![n], "", col(&|r| f64::from(r.miss))),
        f64s("flags", vec![n], "", col(&|r| f64::from(r.flags))),
    ];
    ("control_trace".into(), d, arrays)
}
