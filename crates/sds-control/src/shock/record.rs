//! What a Classical Shock run puts in its record (`docs/run-folder.md`, "What a
//! Classical Shock Run Saves"): the manifest, the table and the setup at arm, the
//! two streams of shots (every enabled input and the drive, one shot per pulse and
//! one for the identification), and the results (the nominal pulse and its
//! tolerance band, the reference SRS and its band, the identification's transfer
//! function and coherence, and each pulse's SRS, kinematics, verdicts and level).
//!
//! The writing is the record's ([`crate::record`]); this module only says what is
//! written, in the reader's form (`hertzforge.io.run_folder`), and holds the station
//! wrapper that feeds the streams. Values are in the channels' own units, as the
//! table names them: the analysis side converts.

use super::config::{shape_name, standard_name, WireShock};
use super::preview::Reference;
use super::view::{mode_name, Row, SrsFigures};
use crate::protocol::num;
use crate::random::record::{self as rec, drive_rows, input_rows, Described};
use crate::record::tap::{StreamTap, TapLink};
use crate::record::{ArrayValues, Begin, ResultArray, StreamSpec};
use crate::table::unit_name;
use hertzforge_channel_table::{Channel, ChannelRole, ChannelTable};
use hertzforge_hal::{
    AcquisitionConfig, Backend, BackendStatus, Caps, ConfiguredActual, HalError, InputRead,
};
use hertzforge_shock_common::pulse_store::DEFAULT_SRS_Q;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// What the station tells the record at arm: the source label, the manifest's
/// `station`, `sim` and `build`, the drive's output latency, and the links of the
/// streams when the station's blocks pass through the record.
pub struct StationFacts {
    /// `SIM` or `HARDWARE`, from the station's class; never guessed.
    pub source: &'static str,
    pub station: Value,
    pub sim: Option<Value>,
    pub build: Value,
    pub output_latency: Value,
    /// The inputs' and the drive's links; `None` when nothing passes a station
    /// the record can watch (the wire tests' simulated engine).
    pub links: Option<(TapLink, TapLink)>,
}

/// The identification as the record keeps it: the complex transfer function,
/// drive to control response, and its coherence on contiguous lines.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentRecord {
    pub first_hz: f64,
    pub df_hz: f64,
    pub h: Vec<(f64, f64)>,
    pub coherence: Vec<f64>,
    pub trusted: Vec<bool>,
}

/// One pulse's kinematics as the pulse store measured them: peak acceleration
/// (the control unit), peak velocity (in/s) and peak-to-peak displacement (in).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PulseKinematics {
    pub peak: f64,
    pub peak_velocity_ips: f64,
    pub displacement_pk_pk_in: f64,
}

// ---- the streams --------------------------------------------------------------

/// A station backend that records what passes through it as a stream of shots: the
/// inputs as read and the drive as the device accepted it, each shot from the
/// station's start (the drive from its first accepted frame after an arm) to its
/// stop, and the station frame.
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
    inputs_open: bool,
    drives_open: bool,
    /// Counts the station's starts, each the beginning of a shot, when a run
    /// asks to know which shot a play was ([`ShotRecording::wrap_counted`]).
    starts: Option<Arc<AtomicU64>>,
}

impl ShotRecording {
    /// Wrap `inner`. `station_frame` is set to the frames read so far after every
    /// read.
    pub fn wrap(
        inner: Box<dyn Backend>,
        inputs: StreamTap,
        drives: StreamTap,
        station_frame: Arc<AtomicU64>,
    ) -> Box<dyn Backend> {
        Box::new(ShotRecording {
            inner,
            inputs,
            drives,
            frames: 0,
            station_frame,
            inputs_open: false,
            drives_open: false,
            starts: None,
        })
    }

    /// [`ShotRecording::wrap`], counting in `starts` every start the station
    /// accepted: after a play, the count less one is the shot it was, whether or
    /// not the streams are recorded.
    pub fn wrap_counted(
        inner: Box<dyn Backend>,
        inputs: StreamTap,
        drives: StreamTap,
        station_frame: Arc<AtomicU64>,
        starts: Arc<AtomicU64>,
    ) -> Box<dyn Backend> {
        Box::new(ShotRecording {
            inner,
            inputs,
            drives,
            frames: 0,
            station_frame,
            inputs_open: false,
            drives_open: false,
            starts: Some(starts),
        })
    }

    fn begin(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("first_station_frame".into(), Value::from(self.frames));
        m
    }

    fn end_shots(&mut self) {
        if self.inputs_open {
            self.inputs.end_shot();
            self.inputs_open = false;
        }
        if self.drives_open {
            self.drives.end_shot();
            self.drives_open = false;
        }
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
        if !self.inputs_open {
            let facts = self.begin();
            self.inputs.begin_shot(facts);
            self.inputs_open = true;
            if let Some(n) = self.starts.as_ref() {
                n.fetch_add(1, Ordering::Relaxed);
            }
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
    pub cfg: &'a WireShock,
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

fn compensation_name(s: hertzforge_dsp::shock::CompensationShape) -> &'static str {
    use hertzforge_dsp::shock::CompensationShape as C;
    match s {
        C::HalfSine => "half_sine",
        C::Rectangular => "rectangular",
        C::RoundedRectangular => "rounded_rectangular",
    }
}

/// The setup as accepted and as resolved: the nominal pulse, the tolerance and
/// its bands, the level schedule, the spacing, the identification, the control
/// settings, the limits, the SRS grid and the rate.
fn setup_json(f: &ArmFacts<'_>) -> Value {
    let cfg = f.cfg;
    let p = cfg.standard.params();
    let control = cfg.control();
    let (rise, fall) = match cfg.shape {
        hertzforge_shock_host::ClassicalShape::Trapezoid {
            rise_frac,
            fall_frac,
        } => (Some(rise_frac), Some(fall_frac)),
        _ => (None, None),
    };
    let comp = cfg.compensation;
    let levels: Vec<Value> = cfg.levels_db.iter().map(|l| num(*l)).collect();
    let per_level: Vec<u32> = (0..cfg.levels_db.len())
        .map(|i| cfg.pulses_for(i))
        .collect();
    json!({
        "config": cfg.raw,
        "resolved": {
            "pulse": {
                "shape": shape_name(cfg.shape),
                "amplitude_g": num(cfg.amplitude_g),
                "duration_ms": num(cfg.duration_ms),
                "polarity": match cfg.polarity {
                    hertzforge_shock_host::Polarity::Positive => "positive",
                    hertzforge_shock_host::Polarity::Negative => "negative",
                },
                "rise_frac": rise.map(num),
                "fall_frac": fall.map(num),
                "compensation": {
                    "shape": compensation_name(comp.shape),
                    "pre_frac": num(comp.pre_frac),
                    "post_frac": num(comp.post_frac),
                    "symmetric": comp.symmetric,
                },
                "unit": "g",
            },
            "tolerance": {
                "standard": standard_name(cfg.standard),
                "amplitude_frac_of_peak": num(p.amp_tol_frac_of_peak),
                "velocity_change_frac": num(p.dv_tol_frac),
                "window_pre_frac": num(p.window_pre_frac),
                "window_post_frac": num(p.window_post_frac),
                "tolerance_abort": match cfg.tolerance_abort {
                    hertzforge_shock_host::ToleranceAbort::FullLevel => "full_level",
                    hertzforge_shock_host::ToleranceAbort::EveryLevel => "every_level",
                },
                "overshoot_abort": num(control.overshoot_abort_frac(cfg.standard)),
                "srs_band_db": num(cfg.srs_band_tol_db),
            },
            "schedule": {
                "mode": mode_name(cfg.mode),
                "levels_db": levels,
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
                "drive_vrms": num(cfg.sysid_drive_vrms),
                "duration_s": num(cfg.sysid_duration_s),
                "nfft": cfg.sysid_nfft,
                "band_hz": [num(cfg.sysid_band_hz.0), num(cfg.sysid_band_hz.1)],
                "min_mean_coherence": num(cfg.sysid_min_mean_coherence),
                "review": cfg.review,
            },
            "control": {
                "law": {"id": cfg.law.id, "params": cfg.law.params},
                "band_hz": [num(cfg.band_hz.0), num(cfg.band_hz.1)],
                "coherence_floor": num(cfg.coherence_floor),
                "tikhonov_floor": num(cfg.tikhonov_floor),
                "ref_floor_db": num(cfg.ref_floor_db),
                "min_band_energy": num(cfg.min_band_energy),
                "law_budget": num(cfg.law_budget),
                "law_misses": cfg.law_misses,
                "max_peak_rise_db": num(cfg.max_peak_rise_db),
                "max_bin_rise_db": num(cfg.max_bin_rise_db),
                "first_drive_ratio": num(cfg.first_drive_ratio),
                "divergence_pulses": cfg.divergence_pulses,
                "controller": serde_json::to_value(&control).unwrap_or(Value::Null),
            },
            "limits": {
                "drive_vpk_ceiling": num(cfg.drive_vpk_ceiling),
                "drive_vpk_abort": num(cfg.drive_vpk_abort),
                "peak_accel_g": cfg.peak_accel_g.map(num),
                "monitors": cfg.monitors.iter().map(|m| json!({
                    "channel": m.channel, "unit": m.unit, "peak_ceiling": num(m.peak_ceiling),
                    "kind": match m.kind {
                        hertzforge_shock_common::DriveMonitorKind::Predictive => "predictive",
                        hertzforge_shock_common::DriveMonitorKind::BetweenPulse => "between_pulse",
                    },
                })).collect::<Vec<_>>(),
            },
            "srs": {
                "f_lo_hz": num(cfg.srs_f_lo_hz),
                "f_hi_hz": num(cfg.srs_f_hi_hz),
                "bands_per_octave": num(cfg.srs_bands_per_octave),
                "band_tol_db": num(cfg.srs_band_tol_db),
                "q": num(DEFAULT_SRS_Q),
                "kind": "maximax",
                "pvss": cfg.pvss,
            },
            "train": {"count": cfg.train_count, "spacing_s": num(cfg.train_spacing_s)},
            "gunfire": cfg.gunfire.map(|g| json!({"rounds_per_min": num(g.rounds_per_min),
                                                  "rounds": g.rounds})),
            "acquisition": {"requested_fs_hz": num(cfg.fs_hz), "fs_hz": num(f.fs_hz),
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

/// The record a Classical Shock run begins with, at arm.
pub fn begin(f: &ArmFacts<'_>, root: PathBuf) -> Begin {
    let cfg = f.cfg;
    let mut manifest = Map::new();
    manifest.insert("test".into(), Value::from("classical_shock"));
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
        json!({"requested_fs_hz": num(cfg.fs_hz), "fs_hz": num(f.fs_hz),
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
    let name = format!(
        "{}_classical_shock_{}",
        rec::utc_stamp(f.armed_unix),
        f.run_id
    );
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

/// What the shot `ordinal` of both streams was: the identification (shot 0), or a
/// pulse, with its level and the frames of the record before its firing (the
/// main pulse's start; the identification fires at its first frame).
pub fn shot_label(pulse: Option<&Row>, pre_frames: usize) -> Map<String, Value> {
    let mut m = Map::new();
    match pulse {
        None => {
            m.insert("what".into(), Value::from("identification"));
            m.insert("pulse".into(), Value::Null);
            m.insert("level_db".into(), Value::Null);
            m.insert("pre_frames".into(), Value::from(0));
        }
        Some(r) => {
            m.insert("what".into(), Value::from("pulse"));
            m.insert("pulse".into(), Value::from(r.index));
            m.insert("origin".into(), Value::from(r.origin));
            m.insert("level_db".into(), num(r.commanded_db));
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

/// The nominal pulse at full level and its time-domain tolerance band, on the
/// record's samples from the main pulse's start (`pulse_reference`), and the
/// reference's SRS at full level with its informational band (`srs_reference`).
pub fn reference_results(
    reference: &Reference,
    srs: Option<(&[f64], &[f64])>,
    band_tol_db: f64,
    control: u32,
    unit: &str,
) -> Vec<Described> {
    let mut out = Vec::new();
    let fig = reference.figures(1.0);
    let n = fig.reference.len();
    let time: Vec<f64> = (0..n)
        .map(|i| (fig.start + i as i64) as f64 / fig.fs_hz)
        .collect();
    let (mut lo, mut hi) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    if let Some(b) = &fig.band {
        for (k, (l, h)) in b.lo.iter().zip(&b.hi).enumerate() {
            if let Some(i) = (b.start + k < n).then_some(b.start + k) {
                lo[i] = *l;
                hi[i] = *h;
            }
        }
    }
    let mut d = described(
        "pulse_reference",
        json!({"kind": "time", "unit": "s", "array": "time"}),
        json!({"responses": [control]}),
    );
    d.insert(
        "description".into(),
        Value::from(
            "the nominal pulse at full level and its tolerance band (NaN outside the judged \
             window), time from the main pulse's start",
        ),
    );
    out.push((
        "pulse_reference".to_owned(),
        d,
        vec![
            f64s("time", vec![n], "s", time),
            f64s("reference", vec![n], unit, fig.reference),
            f64s("band_low", vec![n], unit, lo),
            f64s("band_high", vec![n], unit, hi),
        ],
    ));
    if let Some((freqs, values)) = srs {
        let m = freqs.len().min(values.len());
        let tol = 10f64.powf(band_tol_db / 20.0);
        let mut d = described(
            "srs_reference",
            json!({"kind": "frequency", "unit": "Hz", "array": "frequency"}),
            json!({"responses": [control]}),
        );
        d.insert("q".into(), num(DEFAULT_SRS_Q));
        d.insert("srs_kind".into(), Value::from("maximax"));
        d.insert("band_db".into(), num(band_tol_db));
        d.insert(
            "description".into(),
            Value::from(
                "the nominal pulse's SRS at full level and its informational band, the \
                 tolerance each pulse's SRS is counted against",
            ),
        );
        out.push((
            "srs_reference".to_owned(),
            d,
            vec![
                f64s("frequency", vec![m], "Hz", freqs[..m].to_vec()),
                f64s("reference", vec![m], unit, values[..m].to_vec()),
                f64s(
                    "band_low",
                    vec![m],
                    unit,
                    values[..m].iter().map(|v| v / tol).collect(),
                ),
                f64s(
                    "band_high",
                    vec![m],
                    unit,
                    values[..m].iter().map(|v| v * tol).collect(),
                ),
            ],
        ));
    }
    out
}

/// The identification's transfer function (drive to control response, in the
/// file's convention) and its coherence, with the lines the control trusts.
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
        Value::from(
            "identification: ordinary coherence, drive to control response; trusted, the \
             lines the control trusts (1) and not (0)",
        ),
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
            vec![
                f64s("values", vec![1, 1, n], "", id.coherence[..n].to_vec()),
                f64s(
                    "trusted",
                    vec![n],
                    "",
                    (0..n)
                        .map(|k| f64::from(u8::from(id.trusted.get(k).copied().unwrap_or(false))))
                        .collect(),
                ),
            ],
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

/// One pulse's results, each a record of a repeating result written when the
/// pulse is judged: its SRS against the reference's at the level fired
/// (`pulse_srs`, when it was measured) and its kinematics, verdicts, error and
/// level (`pulses`). `srs_record` is the record's place in `pulse_srs`; `frame`
/// the station frame when it was kept.
pub fn pulse_results(
    row: &Row,
    srs: Option<&SrsFigures>,
    kin: Option<PulseKinematics>,
    srs_record: usize,
    frame: Option<u64>,
    control: u32,
    unit: &str,
) -> Vec<(Described, Value)> {
    let mut out = Vec::new();
    let verdicts = row
        .verdicts
        .map(|(e, v, s)| json!({"envelope": e, "velocity": v, "substitution": s}));
    if let Some((f, measured)) = srs.and_then(|f| f.measured.as_ref().map(|m| (f, m))) {
        let m = f.freqs_hz.len().min(measured.len()).min(f.reference.len());
        let mut d = described(
            "srs",
            json!({"kind": "frequency", "unit": "Hz", "array": "frequency"}),
            json!({"responses": [control]}),
        );
        d.insert("q".into(), num(DEFAULT_SRS_Q));
        d.insert("srs_kind".into(), Value::from("maximax"));
        d.insert(
            "description".into(),
            Value::from("each pulse's SRS, and the reference's at the level fired with its band"),
        );
        d.insert(
            "records".into(),
            json!({"index": "record", "pulse": "the pulse, from 0",
                   "level_db": "the level fired", "origin": "ladder, train or gunfire",
                   "frame": "the station frame when it was kept"}),
        );
        out.push((
            (
                "pulse_srs".to_owned(),
                d,
                vec![
                    f64s("frequency", vec![m], "Hz", f.freqs_hz[..m].to_vec()),
                    f64s("values", vec![1, m], unit, measured[..m].to_vec()),
                    f64s("reference", vec![m], unit, f.reference[..m].to_vec()),
                    f64s("band_low", vec![m], unit, f.band_lo[..m].to_vec()),
                    f64s("band_high", vec![m], unit, f.band_hi[..m].to_vec()),
                ],
            ),
            json!({"index": srs_record, "pulse": row.index, "level_db": num(row.commanded_db),
                   "origin": row.origin, "frame": frame}),
        ));
    }
    let mut d = described(
        "pulse_results",
        json!({"kind": "none"}),
        json!({"responses": [control]}),
    );
    d.insert(
        "description".into(),
        Value::from(
            "each pulse as judged: its level, kinematics, error, verdicts (1 pass, 0 fail, NaN \
             not judged) and drive",
        ),
    );
    d.insert(
        "records".into(),
        json!({"index": "the pulse, from 0", "origin": "ladder, train or gunfire",
               "level_db": "the level fired", "passed": "the tolerance verdict",
               "verdicts": "envelope, velocity change, substitution",
               "frame": "the station frame when it was kept",
               "at_s": "the pulse's start, seconds from arm"}),
    );
    let one = |name: &str, u: &str, v: f64| f64s(name, vec![1], u, vec![v]);
    let (peak, vel, disp) = kin.map_or((row.peak_g, None, None), |k| {
        (
            Some(k.peak),
            Some(k.peak_velocity_ips),
            Some(k.displacement_pk_pk_in),
        )
    });
    let (bands, bands_passed) = row
        .srs
        .map_or((f64::NAN, f64::NAN), |(t, ok)| (t as f64, ok as f64));
    out.push((
        (
            "pulses".to_owned(),
            d,
            vec![
                one("level_db", "dB", row.commanded_db),
                one("scheduled_db", "dB", row.scheduled_db),
                one("limit_scale", "", row.limit_scale),
                one("drive_peak", "V", row.drive_vpk),
                one("peak", unit, or_nan(peak)),
                one("peak_ratio", "", or_nan(row.peak_ratio)),
                one("velocity_change", "in/s", or_nan(row.velocity_change_ips)),
                one(
                    "velocity_change_ratio",
                    "",
                    or_nan(row.velocity_change_ratio),
                ),
                one("peak_velocity", "in/s", or_nan(vel)),
                one("displacement_pk_pk", "in", or_nan(disp)),
                one("error", "%", or_nan(row.error_pct)),
                one("passed", "", flag(row.passed)),
                one("envelope_pass", "", flag(row.verdicts.map(|v| v.0))),
                one("velocity_pass", "", flag(row.verdicts.map(|v| v.1))),
                one("substitution_pass", "", flag(row.verdicts.map(|v| v.2))),
                one("srs_bands", "", bands),
                one("srs_bands_passed", "", bands_passed),
            ],
        ),
        json!({"index": row.index, "origin": row.origin, "level_db": num(row.commanded_db),
               "passed": row.passed, "verdicts": verdicts, "frame": frame,
               "at_s": num(row.at_s)}),
    ));
    out
}
