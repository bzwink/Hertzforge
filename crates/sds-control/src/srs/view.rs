//! What an SRS shock run shows: the `srs.state` object, the pulse rows, the
//! `srs.live` frame and the display views (schema/PROTOCOL.md, "SRS Shock Run
//! State", "SRS Shock Pulses" and "SRS Shock Display"), built from an [`SrsView`]
//! the run publishes after every step. Keys are written in the contract's order.
//!
//! The view builders take figures already computed (the measured SRS, the target at
//! the level fired, the envelope, the records) and only lay them out, so the same
//! figures make the same bytes in every language the goldens are read in.

use super::config::{round, Grid};
use super::preview::{self, Edges};
use crate::codec::Arrays;
use crate::protocol::{num, opt_num};
use crate::shock::view::{drive_view, time_axis, time_series};
use crate::views::{self, Axis, Role, Row as ReadingRow, Scale, Series, Status};
use hertzforge_shock_common::ShockRunMode;
use serde_json::{json, Map, Value};

pub use crate::random::view::{State, StationRef};
pub use crate::shock::view::{law_view, mode_name, sig, LawReading, LawRef, Prediction};

fn rn(x: f64, d: i32) -> Value {
    num(round(x, d))
}

fn opt_rn(x: Option<f64>, d: i32) -> Value {
    opt_num(x.map(|v| round(v, d)))
}

// ---------------------------------------------------------------------------------
// Trips, ends, rows.
// ---------------------------------------------------------------------------------

/// What tripped, with its kind's facts (schema/PROTOCOL.md, the SRS trip table).
#[derive(Debug, Clone, PartialEq)]
pub enum TripFacts {
    Envelope {
        abort_bands: usize,
        worst_band_hz: f64,
        worst_err_db: f64,
    },
    Kinematic {
        /// `accel`, `velocity` or `displacement`.
        axis: &'static str,
        value: f64,
        limit: f64,
        predicted: bool,
    },
    DriveVpk {
        vpk: f64,
        abort_vpk: f64,
        predicted: bool,
    },
    Accel {
        predicted_g: f64,
        limit_g: f64,
    },
    Monitor {
        channel: u32,
        value: f64,
        limit: f64,
    },
    NotConverged {
        /// `per_level` or `run`.
        limit: &'static str,
        pulses: u64,
    },
    LawFault {
        /// A `LawFault::reason()` name.
        reason: &'static str,
        consecutive: u32,
    },
}

/// A trip and where it came.
#[derive(Debug, Clone, PartialEq)]
pub struct Trip {
    pub facts: TripFacts,
    /// The pulse it came at (the next pulse's index for one found before it fired).
    pub pulse: u64,
    pub at_level_db: Option<f64>,
}

impl Trip {
    pub fn kind(&self) -> &'static str {
        match self.facts {
            TripFacts::Envelope { .. } => "envelope",
            TripFacts::Kinematic { .. } => "kinematic",
            TripFacts::DriveVpk { .. } => "drive_vpk",
            TripFacts::Accel { .. } => "accel",
            TripFacts::Monitor { .. } => "monitor",
            TripFacts::NotConverged { .. } => "not_converged",
            TripFacts::LawFault { .. } => "law_fault",
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("kind".into(), Value::from(self.kind()));
        m.insert("pulse".into(), Value::from(self.pulse));
        m.insert("at_level_db".into(), opt_rn(self.at_level_db, 2));
        let mut put = |k: &str, v: Value| {
            m.insert(k.into(), v);
        };
        match &self.facts {
            TripFacts::Envelope {
                abort_bands,
                worst_band_hz,
                worst_err_db,
            } => {
                put("abort_bands", Value::from(*abort_bands));
                put("worst_band_hz", rn(*worst_band_hz, 2));
                put("worst_err_db", rn(*worst_err_db, 2));
            }
            TripFacts::Kinematic {
                axis,
                value,
                limit,
                predicted,
            } => {
                put("axis", Value::from(*axis));
                put("value", num(sig(*value, 4)));
                put("limit", num(sig(*limit, 4)));
                put("predicted", Value::from(*predicted));
            }
            TripFacts::DriveVpk {
                vpk,
                abort_vpk,
                predicted,
            } => {
                put("vpk", rn(*vpk, 4));
                put("abort_vpk", rn(*abort_vpk, 4));
                put("predicted", Value::from(*predicted));
            }
            TripFacts::Accel {
                predicted_g,
                limit_g,
            } => {
                put("predicted_g", rn(*predicted_g, 4));
                put("limit_g", rn(*limit_g, 4));
            }
            TripFacts::Monitor {
                channel,
                value,
                limit,
            } => {
                put("channel", Value::from(*channel));
                put("value", num(sig(*value, 4)));
                put("limit", num(sig(*limit, 4)));
            }
            TripFacts::NotConverged { limit, pulses } => {
                put("limit", Value::from(*limit));
                put("pulses", Value::from(*pulses));
            }
            TripFacts::LawFault {
                reason,
                consecutive,
            } => {
                put("reason", Value::from(*reason));
                put("consecutive", Value::from(*consecutive));
            }
        }
        Value::Object(m)
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub struct End {
    pub outcome: &'static str,
    pub message: String,
    pub at_s: Option<f64>,
    pub level_db: Option<f64>,
    pub pulse: Option<u64>,
    pub trip: Option<Trip>,
}

impl End {
    pub fn to_json(&self) -> Value {
        json!({
            "outcome": self.outcome,
            "message": self.message,
            "at_s": opt_rn(self.at_s, 2),
            "level_db": opt_rn(self.level_db, 2),
            "pulse": self.pulse,
            "trip": self.trip.as_ref().map_or(Value::Null, Trip::to_json),
        })
    }
}

/// The law call that produced the gains a pulse played.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LawCall {
    pub used: bool,
    /// Why it was not used: a law-fault reason name.
    pub miss: Option<&'static str>,
    pub call_ms: f64,
}

/// One fired pulse.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub index: u64,
    pub scheduled_db: f64,
    pub commanded_db: f64,
    pub limit_scale: f64,
    pub drive_vpk: f64,
    pub peak_g: Option<f64>,
    pub velocity_pk_ips: Option<f64>,
    pub displacement_pk_pk_in: Option<f64>,
    pub worst_err_db: Option<f64>,
    pub worst_band_hz: Option<f64>,
    pub alarm_bands: Option<usize>,
    pub abort_bands: Option<usize>,
    pub passed: Option<bool>,
    pub limited_components: Option<usize>,
    pub worst_limit_db: Option<f64>,
    /// Seconds from the start of the run.
    pub at_s: f64,
    pub law: Option<LawCall>,
}

impl Row {
    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "origin": "ladder",
            "scheduled_db": rn(self.scheduled_db, 2),
            "commanded_db": rn(self.commanded_db, 2),
            "limit_scale": rn(self.limit_scale, 4),
            "drive_vpk": rn(self.drive_vpk, 4),
            "peak_g": opt_rn(self.peak_g, 4),
            "velocity_pk_ips": opt_rn(self.velocity_pk_ips, 4),
            "displacement_pk_pk_in": opt_rn(self.displacement_pk_pk_in, 6),
            "worst_err_db": opt_rn(self.worst_err_db, 2),
            "worst_band_hz": opt_rn(self.worst_band_hz, 2),
            "alarm_bands": self.alarm_bands,
            "abort_bands": self.abort_bands,
            "passed": self.passed,
            "limited_components": self.limited_components,
            "worst_limit_db": opt_rn(self.worst_limit_db, 2),
            "at_s": rn(self.at_s, 3),
            "law": self.law.map_or(Value::Null, |l| json!({
                "used": l.used,
                "miss": l.miss,
                "call_ms": rn(l.call_ms, 3),
            })),
        })
    }
}

// ---------------------------------------------------------------------------------
// The run's state.
// ---------------------------------------------------------------------------------

/// Pulse counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub fired: u64,
    pub passed: u64,
    pub at_level: u32,
    pub of_level: u32,
}

impl Counts {
    pub fn to_json(self) -> Value {
        json!({
            "fired": self.fired, "passed": self.passed,
            "at_level": self.at_level, "of_level": self.of_level,
        })
    }
}

/// What identification found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdentFacts {
    /// `shaped` or `standard_pulse`.
    pub excitation: &'static str,
    /// The burst's peak as played, volts.
    pub drive_vpk: f64,
    /// The cap it was held under, volts.
    pub max_vpk: f64,
    pub coherence_mean: f64,
    pub min_mean_coherence: f64,
    pub passed: bool,
}

/// The arm-time limit notch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Notch {
    pub notched_bands: usize,
    pub worst_before_db: Option<f64>,
    pub worst_after_db: Option<f64>,
    pub converged: bool,
}

/// Everything the run shows, as of its last step.
#[derive(Debug, Clone)]
pub struct SrsView {
    pub run_id: String,
    pub title: String,
    pub state: State,
    pub phase: &'static str,
    pub station: StationRef,
    pub grid: Grid,
    pub clock: Option<&'static str>,
    pub mode: ShockRunMode,
    pub levels_db: Vec<f64>,
    pub level_db: Option<f64>,
    pub rung: Option<usize>,
    pub pulses: Counts,
    pub next_pulse_s: Option<f64>,
    pub elapsed_s: Option<f64>,
    pub alarm: bool,
    pub identification: Option<IdentFacts>,
    pub prediction: Option<Prediction>,
    pub notch: Option<Notch>,
    pub last_pulse: Option<Row>,
    pub law: Option<LawRef>,
    pub misses: u32,
    pub feasibility: Option<(&'static str, String)>,
    pub end: Option<End>,
    /// The fire in flight: frames delivered of expected.
    pub fire: Option<(u64, u64)>,
    /// `srs.live` frames are due: from arm.
    pub streaming: bool,
    /// The run's record, once it is armed.
    pub record: Option<RecordRef>,
}

/// The run's record, from arm on: its folder, whether it holds the streams, and
/// the latest problem in words.
pub use crate::random::view::RecordRef;

fn record_json(r: &RecordRef) -> Value {
    json!({"folder": r.folder, "streams": if r.streams { "on" } else { "off" },
           "problem": r.problem})
}

/// The `srs.state` object; `None` is the unconfigured service.
pub fn state_json(v: Option<&SrsView>) -> Value {
    let Some(v) = v else {
        return json!({
            "run_id": null, "state": "unconfigured", "phase": "idle", "title": null,
            "source": null, "station": null, "grid": null, "clock": null, "mode": null,
            "levels_db": null, "level_db": null, "rung": null,
            "pulses": Counts::default().to_json(), "next_pulse_s": null, "elapsed_s": null,
            "alarm": false, "identification": null, "prediction": null, "notch": null,
            "last_pulse": null, "law": null, "feasibility": null, "end": null,
        });
    };
    let station = json!({
        "kind": v.station.kind,
        "class": v.station.class,
        "devices": v.station.devices.iter().map(|(id, sn)| json!({"id": id, "serial": sn})).collect::<Vec<_>>(),
    });
    let mut out = json!({
        "run_id": v.run_id,
        "state": v.state.name(),
        "phase": v.phase,
        "title": v.title,
        "source": v.station.source(),
        "station": station,
        "grid": v.grid.to_json(),
        "clock": v.clock,
        "mode": mode_name(v.mode),
        "levels_db": v.levels_db.iter().map(|x| num(*x)).collect::<Vec<_>>(),
        "level_db": opt_rn(v.level_db, 2),
        "rung": v.rung,
        "pulses": v.pulses.to_json(),
        "next_pulse_s": opt_rn(v.next_pulse_s, 2),
        "elapsed_s": opt_rn(v.elapsed_s, 2),
        "alarm": v.alarm,
        "identification": v.identification.map_or(Value::Null, |i| json!({
            "excitation": i.excitation,
            "drive_vpk": rn(i.drive_vpk, 4),
            "max_vpk": rn(i.max_vpk, 4),
            "coherence_mean": rn(i.coherence_mean, 4),
            "min_mean_coherence": rn(i.min_mean_coherence, 4),
            "passed": i.passed,
        })),
        "prediction": v.prediction.map_or(Value::Null, |p| json!({
            "drive_vpk": rn(p.drive_vpk, 4),
            "drive_vpk_ceiling": rn(p.drive_vpk_ceiling, 4),
            "drive_vpk_abort": rn(p.drive_vpk_abort, 4),
            "response_peak_g": rn(p.response_peak_g, 4),
            "velocity_pk_ips": rn(p.velocity_pk_ips, 4),
            "displacement_pk_pk_in": rn(p.displacement_pk_pk_in, 6),
        })),
        "notch": v.notch.map_or(Value::Null, |n| json!({
            "notched_bands": n.notched_bands,
            "worst_before_db": opt_rn(n.worst_before_db, 2),
            "worst_after_db": opt_rn(n.worst_after_db, 2),
            "converged": n.converged,
        })),
        "last_pulse": v.last_pulse.as_ref().map_or(Value::Null, Row::to_json),
        "law": v.law.as_ref().map_or(Value::Null, |l| json!({
            "id": l.id,
            "name": l.name,
            "version": l.version,
            "sha256": if l.sha256.is_empty() { Value::Null } else { Value::from(l.sha256.as_str()) },
            "misses": v.misses,
        })),
        "feasibility": v.feasibility.as_ref().map_or(Value::Null, |(verdict, message)| {
            json!({"verdict": verdict, "message": message})
        }),
        "end": v.end.as_ref().map_or(Value::Null, End::to_json),
    });
    if let (Some(r), Value::Object(m)) = (&v.record, &mut out) {
        m.insert("record".into(), record_json(r));
    }
    out
}

/// What decides whether an `srs.state` event is due: everything but the clocks.
pub fn state_key(v: Option<&SrsView>) -> String {
    let mut s = state_json(v);
    if let Value::Object(m) = &mut s {
        m.remove("next_pulse_s");
        m.remove("elapsed_s");
    }
    s.to_string()
}

/// The `srs.live` data.
pub fn live_json(v: &SrsView) -> Value {
    json!({
        "run_id": v.run_id,
        "source": v.station.source(),
        "state": v.state.name(),
        "phase": v.phase,
        "mode": mode_name(v.mode),
        "level_db": opt_rn(v.level_db, 2),
        "rung": v.rung,
        "pulses": v.pulses.to_json(),
        "next_pulse_s": opt_rn(v.next_pulse_s, 2),
        "fire": v.fire.map_or(Value::Null, |(done, of)| json!({"done": done, "of": of})),
    })
}

/// A freshly configured run's view: nothing opened, nothing fired.
pub fn configured_view(
    run_id: &str,
    cfg: &super::config::WireSrs,
    derived: &super::config::Derived,
    station: StationRef,
    law: LawRef,
) -> SrsView {
    SrsView {
        run_id: run_id.to_owned(),
        title: cfg.title.clone(),
        state: State::Configured,
        phase: "idle",
        station,
        grid: derived.grid,
        clock: None,
        mode: cfg.mode,
        levels_db: cfg.levels_db.clone(),
        level_db: None,
        rung: None,
        pulses: Counts::default(),
        next_pulse_s: None,
        elapsed_s: None,
        alarm: false,
        identification: None,
        prediction: None,
        notch: None,
        last_pulse: None,
        law: Some(law),
        misses: 0,
        feasibility: None,
        end: None,
        fire: None,
        streaming: false,
        record: None,
    }
}

// ---------------------------------------------------------------------------------
// The display views.
// ---------------------------------------------------------------------------------

/// One pulse's SRS figures on the run's band grid: the measured SRS, the target at
/// the level fired and the envelope around it.
#[derive(Debug, Clone, PartialEq)]
pub struct SrsFigures {
    pub bands_hz: Vec<f64>,
    pub target: Vec<f64>,
    pub edges: Edges,
    pub measured: Option<Vec<f64>>,
}

/// The SRS view.
pub fn srs_view(f: &SrsFigures, arrays: &mut Arrays) -> Value {
    preview::srs_bands_view(
        "SRS",
        &f.bands_hz,
        &f.target,
        &f.edges,
        f.measured
            .as_deref()
            .map(|m| ("Measured", Role::Measured, m)),
        arrays,
    )
}

/// The Pulse view: the measured pulse, g, from the record's first sample.
pub fn pulse_view(
    fs_hz: f64,
    measured: &[f64],
    points: Option<usize>,
    arrays: &mut Arrays,
) -> Value {
    preview::time_view(
        "Pulse",
        "Acceleration",
        "g",
        "Measured",
        Role::Measured,
        fs_hz,
        measured,
        points,
        arrays,
    )
}

/// The Velocity or Displacement view of the measured pulse.
pub fn kinematic_view(
    velocity: bool,
    fs_hz: f64,
    y: &[f64],
    points: Option<usize>,
    arrays: &mut Arrays,
) -> Value {
    let (title, unit) = if velocity {
        ("Velocity", "in/s")
    } else {
        ("Displacement", "in")
    };
    preview::time_view(
        title,
        title,
        unit,
        "Measured",
        Role::Measured,
        fs_hz,
        y,
        points,
        arrays,
    )
}

/// The table in force: each component's frequency and drive amplitude, volts,
/// and the gains applied to the table on the law's last call (after the caps).
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentFigures {
    pub freqs_hz: Vec<f64>,
    pub amplitude_v: Vec<f64>,
    /// Empty before the law's first call.
    pub gains: Vec<f64>,
}

/// The Components and Gains views.
pub fn component_views(f: &ComponentFigures, arrays: &mut Arrays) -> Vec<Value> {
    let freq = Axis::new("Frequency", Some("Hz"), Scale::Log);
    let mut out = vec![views::spectrum(
        arrays,
        "Components",
        &freq,
        &Axis::new("Amplitude", Some("V"), Scale::Log),
        vec![Series::new(
            "Drive amplitude",
            Role::Drive,
            f.freqs_hz.clone(),
            f.amplitude_v.iter().map(|a| a.abs()).collect(),
        )],
        Some("frequency"),
    )];
    if !f.gains.is_empty() {
        out.push(views::spectrum(
            arrays,
            "Gains",
            &freq,
            &Axis::new("Gain", None, Scale::Log),
            vec![Series::new(
                "Last gains",
                Role::Other,
                f.freqs_hz.clone(),
                f.gains.clone(),
            )],
            Some("frequency"),
        ));
    }
    out
}

/// One limit channel's prediction for a pulse's drive.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitFigures {
    pub name: String,
    pub bands_hz: Vec<f64>,
    pub predicted: Vec<f64>,
    /// The bound, g (infinite: no bound at a band).
    pub bound: Vec<f64>,
    pub headroom_db: f64,
}

/// One view per limit channel: the predicted SRS, the bound and the bound less the
/// headroom. A band with no bound is left out of the bound's series.
pub fn limit_views(fs: &[LimitFigures], arrays: &mut Arrays) -> Vec<Value> {
    fs.iter()
        .map(|f| {
            let (bx, by): (Vec<f64>, Vec<f64>) = f
                .bands_hz
                .iter()
                .zip(&f.bound)
                .filter(|(_, b)| b.is_finite())
                .map(|(x, b)| (*x, *b))
                .unzip();
            let room = 10f64.powf(-f.headroom_db / 20.0);
            let ry: Vec<f64> = by.iter().map(|b| b * room).collect();
            views::spectrum(
                arrays,
                &format!("Limit {}", f.name),
                &Axis::new("Natural frequency", Some("Hz"), Scale::Log),
                &Axis::new("Peak acceleration", Some("g"), Scale::Log),
                vec![
                    Series::new(
                        "Predicted",
                        Role::Other,
                        f.bands_hz.clone(),
                        f.predicted.clone(),
                    ),
                    Series::new("Bound", Role::Limit, bx.clone(), by),
                    Series::new("Bound less headroom", Role::Tolerance, bx, ry),
                ],
                Some("frequency"),
            )
        })
        .collect()
}

/// A limit channel's drive-to-channel gain, measured or supplied.
#[derive(Debug, Clone, PartialEq)]
pub struct FrfFigures {
    pub name: String,
    pub freqs_hz: Vec<f64>,
    pub mag: Vec<f64>,
    pub measured: bool,
}

/// One view per limit channel's gain.
pub fn frf_views(fs: &[FrfFigures], arrays: &mut Arrays) -> Vec<Value> {
    fs.iter()
        .map(|f| {
            let (name, role) = if f.measured {
                ("Measured", Role::Measured)
            } else {
                ("Supplied", Role::Reference)
            };
            views::spectrum(
                arrays,
                &format!("Gain {}", f.name),
                &Axis::new("Frequency", Some("Hz"), Scale::Log),
                &Axis::new("Gain", Some("g/V"), Scale::Log),
                vec![Series::new(name, role, f.freqs_hz.clone(), f.mag.clone())],
                Some("frequency"),
            )
        })
        .collect()
}

/// The identified gain to the control channel and its coherence.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentFigures {
    pub freqs_hz: Vec<f64>,
    /// `abs(H)`, control unit per volt.
    pub gain: Vec<f64>,
    /// The floor the seed divides by at the least, control unit per volt.
    pub floor: f64,
    pub coherence: Vec<f64>,
    /// The mean coherence under which the run is held.
    pub hold: f64,
    pub unit: String,
    /// Each limit channel's gain.
    pub limit_frf: Vec<FrfFigures>,
}

/// The identification views: the gain with its floor, and the coherence with the
/// hold.
pub fn identification_views(f: &IdentFigures, arrays: &mut Arrays) -> Vec<Value> {
    let freq = Axis::new("Frequency", Some("Hz"), Scale::Log);
    let ends = match (f.freqs_hz.first(), f.freqs_hz.last()) {
        (Some(a), Some(b)) => vec![*a, *b],
        _ => Vec::new(),
    };
    let gain_unit = format!("{}/V", f.unit);
    let gain = views::spectrum(
        arrays,
        "Identification Gain",
        &freq,
        &Axis::new("Gain", Some(&gain_unit), Scale::Log),
        vec![
            Series::new("Gain", Role::Measured, f.freqs_hz.clone(), f.gain.clone()),
            Series::new(
                "Floor",
                Role::Tolerance,
                ends.clone(),
                vec![f.floor; ends.len()],
            ),
        ],
        Some("frequency"),
    );
    let coherence = views::spectrum(
        arrays,
        "Coherence",
        &freq,
        &Axis::new("Coherence", None, Scale::Linear),
        vec![
            Series::new(
                "Coherence",
                Role::Measured,
                f.freqs_hz.clone(),
                f.coherence.clone(),
            ),
            Series::new(
                "Hold",
                Role::Tolerance,
                ends.clone(),
                vec![f.hold; ends.len()],
            ),
        ],
        Some("frequency"),
    );
    vec![gain, coherence]
}

/// The run's numbers, as a reading.
pub fn readings_view(v: &SrsView) -> Value {
    let mut rows = Vec::new();
    if let Some(l) = v.level_db {
        rows.push(ReadingRow::number(
            "Level",
            round(l, 2),
            Some("dB"),
            Status::Info,
        ));
    }
    rows.push(ReadingRow::number(
        "Pulses fired",
        v.pulses.fired as f64,
        None,
        Status::Info,
    ));
    rows.push(ReadingRow::number(
        "Pulses passed",
        v.pulses.passed as f64,
        None,
        Status::Info,
    ));
    if let Some(p) = &v.last_pulse {
        if let Some(e) = p.worst_err_db {
            rows.push(ReadingRow::number(
                "Worst band",
                round(e, 2),
                Some("dB"),
                match p.passed {
                    Some(true) => Status::Pass,
                    Some(false) => Status::Fail,
                    None => Status::Info,
                },
            ));
        }
        if let Some(f) = p.worst_band_hz {
            rows.push(ReadingRow::number(
                "At",
                round(f, 2),
                Some("Hz"),
                Status::Info,
            ));
        }
        if let Some(peak) = p.peak_g {
            rows.push(ReadingRow::number(
                "Last peak",
                round(peak, 4),
                Some("g"),
                Status::Info,
            ));
        }
        rows.push(ReadingRow::number(
            "Last drive peak",
            round(p.drive_vpk, 4),
            Some("V"),
            Status::Info,
        ));
    }
    if let Some(s) = v.next_pulse_s {
        rows.push(ReadingRow::number(
            "Next pulse",
            round(s, 2),
            Some("s"),
            Status::Info,
        ));
    }
    views::reading("Run", rows)
}

/// The Pulses table.
pub fn pulses_view(rows: &[Row]) -> Value {
    let yes = |b: Option<bool>| match b {
        Some(true) => "pass",
        Some(false) => "fail",
        None => "",
    };
    let fmt = |x: Option<f64>, d: i32| x.map_or(String::new(), |v| num(round(v, d)).to_string());
    let count = |x: Option<usize>| x.map_or(String::new(), |n| n.to_string());
    let body: Vec<Value> = rows
        .iter()
        .map(|p| {
            json!([
                (p.index + 1).to_string(),
                num(round(p.commanded_db, 2)).to_string(),
                fmt(p.peak_g, 4),
                fmt(p.worst_err_db, 2),
                fmt(p.worst_band_hz, 2),
                count(p.alarm_bands),
                count(p.abort_bands),
                fmt(Some(p.drive_vpk), 4),
                yes(p.passed),
            ])
        })
        .collect();
    json!({
        "view": "table",
        "title": "Pulses",
        "columns": ["Pulse", "Level (dB)", "Peak (g)", "Worst band (dB)", "At (Hz)",
                    "Alarm bands", "Abort bands", "Drive (V)", "Passed"],
        "rows": body,
    })
}

/// One fired pulse's figures, for its views.
#[derive(Debug, Clone, PartialEq)]
pub struct PulseFigures {
    pub fs_hz: f64,
    pub srs: Option<SrsFigures>,
    /// The measured pulse, g, from the record's first sample.
    pub measured: Option<Vec<f64>>,
    /// The drive as played, volts.
    pub drive: Vec<f64>,
    pub velocity: Option<Vec<f64>>,
    pub displacement: Option<Vec<f64>>,
    /// The table in force after this pulse's law call.
    pub components: Option<ComponentFigures>,
    /// Each limit channel's prediction for this pulse's drive.
    pub limits: Vec<LimitFigures>,
}

/// A pulse's default views: SRS, Pulse and Drive (`srs.pulse` the event).
pub fn default_pulse_views(
    f: &PulseFigures,
    points: Option<usize>,
    arrays: &mut Arrays,
) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(s) = &f.srs {
        out.push(srs_view(s, arrays));
    }
    if let Some(m) = &f.measured {
        out.push(pulse_view(f.fs_hz, m, points, arrays));
    }
    out.push(drive_view(0, f.fs_hz, &f.drive, points, arrays));
    out
}

/// A pulse's views for `srs.pulse`: SRS, Pulse, Drive, Velocity and Displacement.
pub fn all_pulse_views(f: &PulseFigures, points: Option<usize>, arrays: &mut Arrays) -> Vec<Value> {
    let mut out = default_pulse_views(f, points, arrays);
    if let Some(v) = &f.velocity {
        out.push(kinematic_view(true, f.fs_hz, v, points, arrays));
    }
    if let Some(d) = &f.displacement {
        out.push(kinematic_view(false, f.fs_hz, d, points, arrays));
    }
    out
}

/// The times of `n` samples from a record's first sample at `fs_hz`.
pub fn record_times(n: usize, fs_hz: f64) -> Vec<f64> {
    time_axis(0, n, fs_hz)
}

/// A decimated series from the record's first sample (for a caller with its own
/// view layout).
pub fn record_series(
    name: &str,
    role: Role,
    y: &[f64],
    fs_hz: f64,
    points: Option<usize>,
) -> Series {
    time_series(name, role, record_times(y.len(), fs_hz), y, points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_envelope_trip_names_its_kind_first_and_its_facts_after() {
        let t = Trip {
            facts: TripFacts::Envelope {
                abort_bands: 2,
                worst_band_hz: 630.0,
                worst_err_db: -6.4,
            },
            pulse: 3,
            at_level_db: Some(-6.0),
        };
        let j = t.to_json();
        let keys: Vec<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "kind",
                "pulse",
                "at_level_db",
                "abort_bands",
                "worst_band_hz",
                "worst_err_db"
            ]
        );
    }

    #[test]
    fn a_limit_view_leaves_out_the_bands_without_a_bound() {
        let mut a = Arrays::new();
        let v = limit_views(
            &[LimitFigures {
                name: "Fixture".into(),
                bands_hz: vec![100.0, 200.0, 400.0],
                predicted: vec![1.0, 2.0, 3.0],
                bound: vec![5.0, f64::INFINITY, 5.0],
                headroom_db: 6.0,
            }],
            &mut a,
        );
        assert_eq!(v[0]["title"], "Limit Fixture");
        let names: Vec<&str> = v[0]["series"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Predicted", "Bound", "Bound less headroom"]);
    }
}
