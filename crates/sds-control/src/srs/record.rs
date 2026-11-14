//! What an SRS shock run puts in its record (`docs/run-folder.md`, "What an SRS
//! Run Saves"): the manifest, the table and the setup at arm, the two streams of
//! shots (every enabled input and the drive, one shot for the identification and
//! one per pulse), and the results (the target SRS with its alarm and abort bands,
//! the offline synthesis, the identification's transfer function and coherence,
//! and each pulse's SRS, drive table, kinematics, verdict and level).
//!
//! The writing is the record's ([`crate::record`]); this module only says what is
//! written, in the reader's form (`hertzforge.io.run_folder`), and holds the station
//! wrapper that feeds the streams. Values are in the channels' own units, as the
//! table names them: the analysis side converts.

use super::config::{engine_name, excitation_name, WireBound, WireSrs};
use super::preview::{Edges, Synthesis};
use super::view::{mode_name, PulseFigures, Row};
use crate::protocol::num;
use crate::random::record::{self as rec, drive_rows, input_rows, Described};
use crate::record::tap::StreamTap;
use crate::record::{ArrayValues, Begin, ResultArray, StreamSpec};
use crate::shock::record::StationFacts;
use crate::table::unit_name;
use hertzforge_channel_table::{Channel, ChannelRole, ChannelTable};
use hertzforge_hal::{
    AcquisitionConfig, Backend, BackendStatus, Caps, ConfiguredActual, HalError, InputRead,
};
use hertzforge_srs::table::Polarity;
use hertzforge_srs::SynthesisEngine;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// The identification as the record keeps it: the complex transfer function,
/// drive to control response, and its coherence on contiguous lines of the band.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentRecord {
    pub first_hz: f64,
    pub df_hz: f64,
    pub h: Vec<(f64, f64)>,
    pub coherence: Vec<f64>,
}

// ---- the streams --------------------------------------------------------------

/// A station backend that records what passes through it as a stream of shots: the
/// inputs as read and the drive as the device accepted it, each shot from the
/// station's start (the drive from its first accepted frame, which is committed
/// before the start) to its stop, and the station frame.
///
/// A shot whose drive was committed and then withheld before the start (the run
/// was held in the wait before a pulse) still takes its place in both streams, an
/// empty one in the inputs, so the two streams' shots stay in step; `shots` counts
/// every shot begun, and the run names the withheld ones.
///
/// It only watches: every call goes to the station unchanged and its answer comes
/// back unchanged, no recording outcome can fail a call, and it does no file work,
/// takes no lock and never waits ([`StreamTap`]).
pub struct ShotRecording {
    inner: Box<dyn Backend>,
    inputs: StreamTap,
    drives: StreamTap,
    frames: u64,
    station_frame: Arc<AtomicU64>,
    shots: Arc<AtomicU64>,
    inputs_open: bool,
    drives_open: bool,
    /// The open shot has started (the station's start returned).
    started: bool,
}

impl ShotRecording {
    /// Wrap `inner`. `station_frame` is set to the frames read so far after every
    /// read; `shots` to the shots begun so far.
    pub fn wrap(
        inner: Box<dyn Backend>,
        inputs: StreamTap,
        drives: StreamTap,
        station_frame: Arc<AtomicU64>,
        shots: Arc<AtomicU64>,
    ) -> Box<dyn Backend> {
        Box::new(ShotRecording {
            inner,
            inputs,
            drives,
            frames: 0,
            station_frame,
            shots,
            inputs_open: false,
            drives_open: false,
            started: false,
        })
    }

    fn begin(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("first_station_frame".into(), Value::from(self.frames));
        m
    }

    /// A shot begins in whichever stream sees it first; the count goes up once.
    fn opened(&mut self) {
        if !(self.inputs_open || self.drives_open) {
            self.shots.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn end_shots(&mut self) {
        if self.drives_open && !self.inputs_open && !self.started {
            // committed and never started: an empty shot in the inputs keeps the
            // two streams' shots in step, and says nothing was played
            let mut facts = self.begin();
            facts.insert("played".into(), Value::from(false));
            self.inputs.begin_shot(facts);
            self.inputs_open = true;
        }
        if self.inputs_open {
            self.inputs.end_shot();
            self.inputs_open = false;
        }
        if self.drives_open {
            self.drives.end_shot();
            self.drives_open = false;
        }
        self.started = false;
    }
}

impl Backend for ShotRecording {
    fn caps(&self) -> Caps {
        self.inner.caps()
    }
    fn open(&mut self) -> Result<(), HalError> {
        self.inner.open()
    }
    fn close(&mut self) -> Result<(), HalError> {
        self.end_shots();
        self.inner.close()
    }
    fn take_warnings(&mut self) -> Vec<String> {
        self.inner.take_warnings()
    }
    fn configure(
        &mut self,
        table: &ChannelTable,
        acq: &AcquisitionConfig,
    ) -> Result<ConfiguredActual, HalError> {
        self.inner.configure(table, acq)
    }
    fn arm(&mut self) -> Result<(), HalError> {
        self.inner.arm()
    }
    fn start(&mut self) -> Result<(), HalError> {
        self.inner.start()?;
        self.started = true;
        if !self.inputs_open {
            self.opened();
            let facts = self.begin();
            self.inputs.begin_shot(facts);
            self.inputs_open = true;
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), HalError> {
        let r = self.inner.stop();
        self.end_shots();
        r
    }
    fn read_input(&mut self, out: &mut [f32]) -> Result<InputRead, HalError> {
        let r = self.inner.read_input(out)?;
        if let InputRead::Frames(n) = r {
            self.frames += n as u64;
            self.station_frame.store(self.frames, Ordering::Relaxed);
            self.inputs.push(out, n);
        }
        Ok(r)
    }
    fn write_drive(&mut self, drive: &[f32]) -> Result<usize, HalError> {
        let n = self.inner.write_drive(drive)?;
        if n > 0 && !self.drives_open {
            self.opened();
            let facts = self.begin();
            self.drives.begin_shot(facts);
            self.drives_open = true;
        }
        self.drives.push(drive, n);
        Ok(n)
    }
    fn output_space(&mut self) -> Option<usize> {
        self.inner.output_space()
    }
    fn input_overloads(&mut self) -> Vec<bool> {
        self.inner.input_overloads()
    }
    fn poll_status(&mut self) -> BackendStatus {
        self.inner.poll_status()
    }
    fn emergency_stop(&mut self) -> Result<(), HalError> {
        let r = self.inner.emergency_stop();
        self.end_shots();
        r
    }
}

// ---- the record at arm ---------------------------------------------------------

/// The facts of an armed run its record begins with.
pub struct ArmFacts<'a> {
    pub run_id: &'a str,
    pub run_uid: &'a str,
    pub cfg: &'a WireSrs,
    /// The offline synthesis the configuration was accepted with.
    pub synthesis: &'a Synthesis,
    /// The derived values as configure gave them.
    pub derived: &'a Value,
    pub table: &'a ChannelTable,
    pub revision: u64,
    pub station: &'a StationFacts,
    /// The rate the station runs at, and the record every pulse is played as.
    pub fs_hz: f64,
    pub record_len: usize,
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

fn nums(x: &[f64]) -> Vec<Value> {
    x.iter().map(|v| num(*v)).collect()
}

/// A synthesis table in the configuration's form (`target`'s `components`).
pub fn table_json(t: &SynthesisEngine) -> Value {
    match t {
        SynthesisEngine::Wavsyn(w) => json!({
            "engine": "wavsyn",
            "rows": w.components.iter().map(|c| json!({
                "freq_hz": num(c.freq_hz), "amplitude_g": num(c.amplitude_g),
                "half_cycles": c.half_cycles, "delay_s": num(c.delay_s),
            })).collect::<Vec<_>>(),
        }),
        SynthesisEngine::Seds(s) => json!({
            "engine": "seds",
            "rows": s.components.iter().map(|c| json!({
                "freq_hz": num(c.freq_hz), "amplitude_g": num(c.amplitude_g),
                "zeta": num(c.zeta), "delay_s": num(c.delay_s),
                "polarity": match c.polarity {
                    Polarity::Positive => "positive",
                    Polarity::Negative => "negative",
                },
            })).collect::<Vec<_>>(),
        }),
    }
}

/// The setup as accepted and as resolved: the target and its bands, the envelope,
/// the synthesis settings and the synthesized pulse, the level schedule, the
/// spacing, the identification, the control settings, the limits and the rate.
fn setup_json(f: &ArmFacts<'_>) -> Value {
    let cfg = f.cfg;
    let s = &cfg.synthesis;
    let syn = f.synthesis;
    let per_level: Vec<u32> = (0..cfg.levels_db.len())
        .map(|i| cfg.pulses_for(i))
        .collect();
    let control = cfg.control();
    json!({
        "config": cfg.raw,
        "resolved": {
            "target": s.target.to_json(),
            "bands": {
                "f_lo_hz": num(s.bands.f_lo_hz), "f_hi_hz": num(s.bands.f_hi_hz),
                "bands_per_octave": num(s.bands.bands_per_octave), "q": num(s.bands.q),
                "kind": "maximax", "centres_hz": nums(&syn.bands_hz),
                "target_g": nums(&syn.target_g),
            },
            "envelope": {
                "alarm_db": [num(cfg.envelope_alarm_db.0), num(cfg.envelope_alarm_db.1)],
                "abort_db": [num(cfg.envelope_abort_db.0), num(cfg.envelope_abort_db.1)],
                "per_band": cfg.envelope_per_band.as_ref().map(|bands| bands.iter().map(|b| json!({
                    "alarm_db": [num(b.alarm_db.0), num(b.alarm_db.1)],
                    "abort_db": [num(b.abort_db.0), num(b.abort_db.1)],
                })).collect::<Vec<_>>()),
            },
            "synthesis": {
                "engine": engine_name(s.engine),
                "components": s.components.as_ref().map(|c| c.to_json()),
                "max_iters": s.max_iters,
                "tol_db": num(s.tol_db),
                "max_gain_db_per_iter": num(s.max_gain_db_per_iter),
                "fs_hz": num(s.fs_hz),
                "result": syn.facts().to_json(),
                "pulse": syn.reference_facts().to_json(),
                "table": table_json(&syn.components),
            },
            "ring_out_s": num(cfg.ring_out_s),
            "pvss": cfg.pvss,
            "schedule": {
                "mode": mode_name(cfg.mode),
                "levels_db": nums(&cfg.levels_db),
                "pulses": per_level,
                "start_db": num(cfg.start_db),
                "advance": cfg.advance.name(),
                "max_pulses_per_level": cfg.max_pulses_per_level,
                "max_pulses": cfg.max_pulses,
            },
            "spacing": {
                "cooling_s": num(cfg.cooling_s),
                "settling_s": num(cfg.settling_s),
                "compute_s": num(cfg.compute_s),
                "spacing_s": num(cfg.spacing_s()),
            },
            "identification": {
                "excitation": excitation_name(cfg.excitation),
                "level_db": num(cfg.sysid_level_db),
                "max_vpk": cfg.sysid_max_vpk.map(num),
                "vpk_cap": num(cfg.identification_vpk_cap()),
                "nfft": cfg.sysid_nfft,
                "h_floor_frac": num(cfg.h_floor_frac),
                "min_mean_coherence": num(cfg.min_mean_coherence),
                "review": cfg.review,
            },
            "control": {
                "law": {"id": cfg.law.id, "params": cfg.law.params},
                "law_budget": num(cfg.law_budget),
                "law_misses": cfg.law_misses,
                "max_gain_rise_db": num(cfg.max_gain_rise_db),
                "controller": serde_json::to_value(&control).unwrap_or(Value::Null),
            },
            "limits": {
                "drive_vpk_ceiling": num(cfg.drive_vpk_ceiling),
                "drive_vpk_abort": num(cfg.drive_vpk_abort),
                "peak_accel_g": cfg.peak_accel_g.map(num),
                "abort_factor": num(cfg.abort_factor),
                "kinematic": {
                    "accel_g": cfg.kinematic.0.map(num),
                    "velocity_ips": cfg.kinematic.1.map(num),
                    "displacement_pk_pk_in": cfg.kinematic.2.map(num),
                },
                "monitors": cfg.monitors.iter().map(|m| json!({
                    "channel": m.channel, "unit": m.unit, "peak_ceiling": num(m.peak_ceiling),
                    "kind": match m.kind {
                        hertzforge_shock_common::DriveMonitorKind::Predictive => "predictive",
                        hertzforge_shock_common::DriveMonitorKind::BetweenPulse => "between_pulse",
                    },
                })).collect::<Vec<_>>(),
                "headroom_db": num(cfg.headroom_db),
                "limit_channels": cfg.limit_channels.iter().map(|l| json!({
                    "channel": l.channel,
                    "bound": match &l.bound {
                        WireBound::Flat { accel_g } => json!({"accel_g": num(*accel_g)}),
                        WireBound::PerBand { bounds_g } => json!({
                            "bounds_g": bounds_g.iter().map(|b| b.map(num)).collect::<Vec<_>>()
                        }),
                    },
                    "frf": l.frf.as_ref().map(|(f, m)| json!({"freqs_hz": nums(f), "mag": nums(m)})),
                })).collect::<Vec<_>>(),
            },
            "acquisition": {"requested_fs_hz": num(cfg.fs_hz()), "fs_hz": num(f.fs_hz),
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

/// The record an SRS shock run begins with, at arm.
pub fn begin(f: &ArmFacts<'_>, root: PathBuf) -> Begin {
    let cfg = f.cfg;
    let mut manifest = Map::new();
    manifest.insert("test".into(), Value::from("srs"));
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
        json!({"requested_fs_hz": num(cfg.fs_hz()), "fs_hz": num(f.fs_hz),
               "record_len": f.record_len}),
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
    let name = format!("{}_srs_{}", rec::utc_stamp(f.armed_unix), f.run_id);
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

/// What a shot of both streams was: the identification at its level, a pulse
/// with its levels, or a shot no pulse was judged on. Every SRS shot fires at its
/// first frame (`pre_frames` 0).
pub fn shot_label(shot: Shot<'_>) -> Map<String, Value> {
    let mut m = Map::new();
    match shot {
        Shot::Identification { level_db } => {
            m.insert("what".into(), Value::from("identification"));
            m.insert("pulse".into(), Value::Null);
            m.insert("level_db".into(), num(level_db));
        }
        Shot::Pulse(r) => {
            m.insert("what".into(), Value::from("pulse"));
            m.insert("pulse".into(), Value::from(r.index));
            m.insert("origin".into(), Value::from("ladder"));
            m.insert("level_db".into(), num(r.commanded_db));
            m.insert("scheduled_db".into(), num(r.scheduled_db));
        }
        Shot::Unjudged => {
            m.insert("what".into(), Value::from("unjudged"));
            m.insert("pulse".into(), Value::Null);
            m.insert("level_db".into(), Value::Null);
        }
    }
    m.insert("pre_frames".into(), Value::from(0));
    m
}

/// A shot of the streams, as [`shot_label`] names it.
pub enum Shot<'a> {
    Identification {
        level_db: f64,
    },
    Pulse(&'a Row),
    /// A shot no pulse was judged on: its drive committed and withheld in the
    /// wait before a pulse (its inputs then hold no frames and say `played`
    /// false), or a pulse stopped part way.
    Unjudged,
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

fn frequency_axis() -> Value {
    json!({"kind": "frequency", "unit": "Hz", "array": "frequency"})
}

/// The envelope's four edges as arrays of `m` values.
fn edge_arrays(edges: &Edges, m: usize, unit: &str) -> Vec<ResultArray> {
    let cut = |x: &[f64]| x.iter().copied().take(m).collect::<Vec<_>>();
    vec![
        f64s("band_low", vec![m], unit, cut(&edges.alarm_lo)),
        f64s("band_high", vec![m], unit, cut(&edges.alarm_hi)),
        f64s("abort_low", vec![m], unit, cut(&edges.abort_lo)),
        f64s("abort_high", vec![m], unit, cut(&edges.abort_hi)),
    ]
}

/// The target SRS at full level on the run's bands with the envelope's alarm band
/// (`band_low`, `band_high`) and abort band (`abort_low`, `abort_high`): the
/// specification each pulse is judged against (`srs_reference`).
pub fn reference_results(
    bands_hz: &[f64],
    target: &[f64],
    edges: &Edges,
    q: f64,
    control: u32,
    unit: &str,
) -> Vec<Described> {
    let m = [
        bands_hz.len(),
        target.len(),
        edges.alarm_lo.len(),
        edges.alarm_hi.len(),
        edges.abort_lo.len(),
        edges.abort_hi.len(),
    ]
    .into_iter()
    .min()
    .unwrap_or(0);
    if m == 0 {
        return Vec::new();
    }
    let mut d = described(
        "srs_reference",
        frequency_axis(),
        json!({"responses": [control]}),
    );
    d.insert("q".into(), num(q));
    d.insert("srs_kind".into(), Value::from("maximax"));
    d.insert(
        "description".into(),
        Value::from(
            "the target SRS at full level on the run's bands, with the envelope's alarm band \
             (band) and abort band (abort)",
        ),
    );
    let mut arrays = vec![
        f64s("frequency", vec![m], "Hz", bands_hz[..m].to_vec()),
        f64s("reference", vec![m], unit, target[..m].to_vec()),
    ];
    arrays.extend(edge_arrays(edges, m, unit));
    vec![("srs_reference".to_owned(), d, arrays)]
}

/// The offline synthesis the configuration was accepted with, at full level and
/// at the configured rate: the synthesized pulse with its velocity and
/// displacement, and its SRS on the bands (`synthesis`, kept as arrays).
pub fn synthesis_results(syn: &Synthesis, control: u32) -> Vec<Described> {
    let n = syn
        .waveform_g
        .len()
        .min(syn.velocity_ips.len())
        .min(syn.disp_in.len());
    let m = syn.bands_hz.len().min(syn.achieved_g.len());
    if n == 0 {
        return Vec::new();
    }
    let mut d = described(
        "srs_synthesis",
        json!({"kind": "time", "unit": "s", "array": "time"}),
        json!({"responses": [control]}),
    );
    d.insert("fs_hz".into(), num(syn.fs_hz));
    d.insert("engine".into(), Value::from(engine_name(syn.engine)));
    d.insert(
        "description".into(),
        Value::from(
            "the pulse the configuration was synthesized to at full level and at the \
             configured rate, its velocity and displacement, and its SRS on the bands \
             (band_frequency, achieved); a design, not a measurement",
        ),
    );
    let time: Vec<f64> = (0..n).map(|i| i as f64 / syn.fs_hz).collect();
    vec![(
        "synthesis".to_owned(),
        d,
        vec![
            f64s("time", vec![n], "s", time),
            f64s("waveform", vec![n], "g", syn.waveform_g[..n].to_vec()),
            f64s("velocity", vec![n], "in/s", syn.velocity_ips[..n].to_vec()),
            f64s("displacement", vec![n], "in", syn.disp_in[..n].to_vec()),
            f64s("band_frequency", vec![m], "Hz", syn.bands_hz[..m].to_vec()),
            f64s("achieved", vec![m], "g", syn.achieved_g[..m].to_vec()),
        ],
    )]
}

/// The identification's transfer function (drive to control response, in the
/// file's convention) and its coherence.
pub fn identification_results(id: &IdentRecord, table: &ChannelTable) -> Vec<Described> {
    let (Some(control), Some(drive)) = (control_row(table), drive_rows(table).first().copied())
    else {
        return Vec::new();
    };
    let n = id.h.len().min(id.coherence.len());
    if n == 0 {
        return Vec::new();
    }
    let axis = json!({"kind": "frequency", "unit": "Hz", "start": id.first_hz, "step": id.df_hz});
    let channels = json!({"responses": [control.index], "references": [drive.index]});
    let mut frf = described("frf", axis.clone(), channels.clone());
    frf.insert("convention".into(), Value::from("file"));
    frf.insert(
        "description".into(),
        Value::from("identification: drive to control response"),
    );
    let mut coh = described("coherence", axis, channels);
    coh.insert(
        "description".into(),
        Value::from("identification: ordinary coherence, drive to control response"),
    );
    let unit = format!("{}/{}", unit_name(&control.units), unit_name(&drive.units));
    vec![
        (
            "identification_frf".into(),
            frf,
            vec![ResultArray {
                name: "values".into(),
                shape: vec![1, 1, n],
                unit,
                values: ArrayValues::C128(rec::frf_to_file(&id.h[..n])),
            }],
        ),
        (
            "identification_coherence".into(),
            coh,
            vec![f64s(
                "values",
                vec![1, 1, n],
                "",
                id.coherence[..n].to_vec(),
            )],
        ),
    ]
}

fn flag(b: Option<bool>) -> f64 {
    match b {
        Some(true) => 1.0,
        Some(false) => 0.0,
        None => f64::NAN,
    }
}

fn or_nan(x: Option<f64>) -> f64 {
    x.unwrap_or(f64::NAN)
}

fn count(x: Option<usize>) -> f64 {
    x.map_or(f64::NAN, |n| n as f64)
}

/// The places each repeating result has reached: the next record of each.
#[derive(Debug, Clone, Copy, Default)]
pub struct Records {
    pub srs: usize,
    pub tables: usize,
}

/// One pulse's results, each a record of a repeating result written when the
/// pulse is judged: its SRS against the target at the level fired with the
/// envelope (`pulse_srs`, when it was measured), the drive table it played and
/// the law's gains after it (`drive_tables`), and its level, kinematics, error and
/// verdict (`pulses`). `frame` is the station frame when it was kept.
pub fn pulse_results(
    row: &Row,
    fig: &PulseFigures,
    records: &mut Records,
    frame: Option<u64>,
    (control, drive): (u32, Option<u32>),
    unit: &str,
) -> Vec<(Described, Value)> {
    let mut out = Vec::new();
    if let Some((f, measured)) = fig
        .srs
        .as_ref()
        .and_then(|f| f.measured.as_ref().map(|m| (f, m)))
    {
        let m = f.bands_hz.len().min(measured.len()).min(f.target.len());
        let mut d = described("srs", frequency_axis(), json!({"responses": [control]}));
        d.insert("srs_kind".into(), Value::from("maximax"));
        d.insert(
            "description".into(),
            Value::from(
                "each pulse's SRS, and the target at the level fired with the envelope's alarm \
                 band (band) and abort band (abort)",
            ),
        );
        d.insert(
            "records".into(),
            json!({"index": "record", "pulse": "the pulse, from 0",
                   "level_db": "the level fired", "frame": "the station frame when it was kept"}),
        );
        let mut arrays = vec![
            f64s("frequency", vec![m], "Hz", f.bands_hz[..m].to_vec()),
            f64s("values", vec![1, m], unit, measured[..m].to_vec()),
            f64s("reference", vec![m], unit, f.target[..m].to_vec()),
        ];
        arrays.extend(edge_arrays(&f.edges, m, unit));
        out.push((
            ("pulse_srs".to_owned(), d, arrays),
            json!({"index": records.srs, "pulse": row.index,
                   "level_db": num(row.commanded_db), "frame": frame}),
        ));
        records.srs += 1;
    }
    if let Some(c) = &fig.components {
        let n = c.freqs_hz.len().min(c.amplitude_v.len());
        let gains: Vec<f64> = (0..n)
            .map(|k| c.gains.get(k).copied().unwrap_or(f64::NAN))
            .collect();
        let mut d = described(
            "srs_drive_table",
            json!({"kind": "frequency", "unit": "Hz", "array": "frequency"}),
            json!({"references": drive.map_or_else(Vec::new, |d| vec![d])}),
        );
        d.insert(
            "description".into(),
            Value::from(
                "the drive table in force after each pulse's law call: each component's \
                 frequency and drive amplitude, and the gains the law applied (NaN before its \
                 first call)",
            ),
        );
        d.insert(
            "records".into(),
            json!({"index": "record", "pulse": "the pulse, from 0"}),
        );
        out.push((
            (
                "drive_tables".to_owned(),
                d,
                vec![
                    f64s("frequency", vec![n], "Hz", c.freqs_hz[..n].to_vec()),
                    f64s("amplitude", vec![1, n], "V", c.amplitude_v[..n].to_vec()),
                    f64s("gains", vec![1, n], "", gains),
                ],
            ),
            json!({"index": records.tables, "pulse": row.index}),
        ));
        records.tables += 1;
    }
    let mut d = described(
        "pulse_results",
        json!({"kind": "none"}),
        json!({"responses": [control]}),
    );
    d.insert(
        "description".into(),
        Value::from(
            "each pulse as judged: its levels, drive, kinematics, its worst band against the \
             target, its bands outside the alarm and abort bands, the limit channels' cut, and \
             its verdict (1 pass, 0 fail, NaN not judged)",
        ),
    );
    d.insert(
        "records".into(),
        json!({"index": "the pulse, from 0", "level_db": "the level fired",
               "passed": "the verdict: every band inside the alarm band",
               "law": "the law call whose gains it played",
               "frame": "the station frame when it was kept",
               "at_s": "the pulse's start, seconds from the run's start"}),
    );
    let one = |name: &str, u: &str, v: f64| f64s(name, vec![1], u, vec![v]);
    out.push((
        (
            "pulses".to_owned(),
            d,
            vec![
                one("level_db", "dB", row.commanded_db),
                one("scheduled_db", "dB", row.scheduled_db),
                one("limit_scale", "", row.limit_scale),
                one("drive_peak", "V", row.drive_vpk),
                one("peak", unit, or_nan(row.peak_g)),
                one("peak_velocity", "in/s", or_nan(row.velocity_pk_ips)),
                one(
                    "displacement_pk_pk",
                    "in",
                    or_nan(row.displacement_pk_pk_in),
                ),
                one("worst_error", "dB", or_nan(row.worst_err_db)),
                one("worst_band", "Hz", or_nan(row.worst_band_hz)),
                one("alarm_bands", "", count(row.alarm_bands)),
                one("abort_bands", "", count(row.abort_bands)),
                one("passed", "", flag(row.passed)),
                one("limited_components", "", count(row.limited_components)),
                one("worst_limit", "dB", or_nan(row.worst_limit_db)),
            ],
        ),
        json!({"index": row.index, "level_db": num(row.commanded_db),
               "passed": row.passed, "frame": frame, "at_s": num(row.at_s),
               "law": row.law.map_or(Value::Null, |l| json!({
                   "used": l.used, "miss": l.miss, "call_ms": num(l.call_ms)}))}),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::tap::TapLink;
    use crate::record::{RecordOptions, RunRecord};
    use hertzforge_station::{BackendKind, StationSpec};
    use std::time::{Duration, Instant};

    /// The simulator's caps under a station that plays every drive frame offered
    /// and reads back as many frames as are asked for, so the test says exactly
    /// which calls a shot sees.
    struct Plain {
        caps: Caps,
    }

    impl Backend for Plain {
        fn caps(&self) -> Caps {
            self.caps.clone()
        }
        fn open(&mut self) -> Result<(), HalError> {
            Ok(())
        }
        fn close(&mut self) -> Result<(), HalError> {
            Ok(())
        }
        fn configure(
            &mut self,
            _table: &ChannelTable,
            _acq: &AcquisitionConfig,
        ) -> Result<ConfiguredActual, HalError> {
            Err(HalError::Unsupported("configure"))
        }
        fn arm(&mut self) -> Result<(), HalError> {
            Ok(())
        }
        fn start(&mut self) -> Result<(), HalError> {
            Ok(())
        }
        fn stop(&mut self) -> Result<(), HalError> {
            Ok(())
        }
        fn read_input(&mut self, out: &mut [f32]) -> Result<InputRead, HalError> {
            out.fill(1.0);
            Ok(InputRead::Frames(out.len()))
        }
        fn write_drive(&mut self, drive: &[f32]) -> Result<usize, HalError> {
            Ok(drive.len())
        }
        fn poll_status(&mut self) -> BackendStatus {
            BackendStatus::default()
        }
        fn emergency_stop(&mut self) -> Result<(), HalError> {
            Ok(())
        }
    }

    fn stream(kind: &str, link: TapLink) -> StreamSpec {
        let mut d = Map::new();
        d.insert("kind".into(), json!(kind));
        d.insert("fs_hz".into(), json!(1000));
        d.insert("channels".into(), json!([{"index": 1, "unit": "g"}]));
        d.insert("shots".into(), json!([]));
        StreamSpec {
            name: kind.into(),
            description: d,
            link,
        }
    }

    /// A pulse whose drive was committed and then withheld before its start (the
    /// run held in the wait before it) keeps the two streams' shots in step: the
    /// inputs get an empty shot that says nothing was played, the drive keeps what
    /// was committed, and the count of shots is the same for both. Red if the
    /// inputs skipped the withheld shot (the next pulse would be named in the
    /// wrong place in one stream) or if it were counted twice.
    #[test]
    fn an_srs_withheld_shot_keeps_the_inputs_and_the_drive_in_step() {
        let caps = hertzforge_station::try_build_station_backend(
            BackendKind::Sim,
            &StationSpec::default(),
        )
        .unwrap()
        .backend
        .caps();
        let root = std::env::temp_dir().join(format!(
            "srs-shots-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (inputs, inputs_link) = crate::record::tap::pair(1, 8, 64);
        let (drives, drives_link) = crate::record::tap::pair(1, 8, 64);
        let mut manifest = Map::new();
        manifest.insert("test".into(), json!("srs"));
        manifest.insert("source".into(), json!("SIM"));
        let record = RunRecord::open(
            Begin {
                root: root.clone(),
                name: "run".into(),
                manifest,
                table: json!({"rows": []}),
                setup: json!({"config": {}}),
                law: None,
                log: Vec::new(),
                streams: vec![stream("inputs", inputs_link), stream("drives", drives_link)],
            },
            &RecordOptions::default(),
            Box::new(|_, _| {}),
        )
        .unwrap();
        let status = record.status();
        let shots = Arc::new(AtomicU64::new(0));
        let mut b = ShotRecording::wrap(
            Box::new(Plain { caps }),
            inputs,
            drives,
            Arc::new(AtomicU64::new(0)),
            shots.clone(),
        );
        let mut frame = [0.0f32; 4];
        // played, withheld, played
        for played in [true, false, true] {
            b.write_drive(&[0.5; 4]).unwrap();
            if played {
                b.start().unwrap();
                b.read_input(&mut frame).unwrap();
            }
            b.stop().unwrap();
        }
        assert_eq!(shots.load(Ordering::Relaxed), 3);
        drop(b);
        record.finish(Map::new());
        let t0 = Instant::now();
        while !status.finished() {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "the writer did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let side = |name: &str| -> Value {
            serde_json::from_slice(
                &std::fs::read(status.folder.join(format!("streams/{name}/stream.json"))).unwrap(),
            )
            .unwrap()
        };
        let (i, d) = (side("inputs"), side("drives"));
        let frames = |v: &Value| -> Vec<u64> {
            v["shots"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["frames"].as_u64().unwrap())
                .collect()
        };
        assert_eq!(frames(&i), vec![4, 0, 4], "{i}");
        assert_eq!(frames(&d), vec![4, 4, 4], "{d}");
        assert_eq!(i["shots"][1]["played"], false, "{i}");
        assert!(i["shots"][0].get("played").is_none(), "{i}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
