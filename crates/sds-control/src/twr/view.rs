//! What a TWR run shows: the `twr.state` object, the play rows, the `twr.live`
//! frame and the display views (schema/PROTOCOL.md, "TWR Run State", "TWR Plays"
//! and "TWR Display"), built from a [`TwrView`] the run publishes after every step
//! and the figures it keeps ([`PlayRecords`], [`IdentFigures`], [`MaskFigures`]).
//! Keys are written in the contract's order.
//!
//! The builders take figures already computed and only lay them out, so the same
//! figures make the same bytes in every language the goldens are read in.

use super::config::{mode_name, Grid};
use crate::codec::Arrays;
use crate::protocol::{num, opt_num};
use crate::shock::view::{sig, time_axis, time_series};
use crate::views::{self, Axis, Role, Row as ReadingRow, Scale, Series, Status};
use hertzforge_twr::schedule::TwrRunMode;
use serde_json::{json, Map, Value};

pub use crate::random::view::StationRef;

fn r(x: f64, d: i32) -> f64 {
    crate::shock::config::round(x, d)
}

fn rn(x: f64, d: i32) -> Value {
    num(r(x, d))
}

fn opt_rn(x: Option<f64>, d: i32) -> Value {
    opt_num(x.map(|v| r(v, d)))
}

fn sn(x: f64, d: i32) -> Value {
    num(sig(x, d))
}

/// A TWR run's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Configured,
    Blocked,
    Armed,
    Running,
    Review,
    /// The coherence hold: identification did not reach its coherence.
    Held,
    Stopping,
    Finished,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Configured => "configured",
            State::Blocked => "blocked",
            State::Armed => "armed",
            State::Running => "running",
            State::Review => "review",
            State::Held => "held",
            State::Stopping => "stopping",
            State::Finished => "finished",
        }
    }

    /// Armed, running, holding for review, held or stopping.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            State::Armed | State::Running | State::Review | State::Held | State::Stopping
        )
    }

    /// Driving or about to: the states `twr.reset` and `twr.clear` refuse (the
    /// coherence hold is released by `twr.reset`).
    pub fn is_busy(self) -> bool {
        matches!(self, State::Running | State::Review | State::Stopping)
    }
}

// ---------------------------------------------------------------------------------
// Trips, ends, rows.
// ---------------------------------------------------------------------------------

/// What tripped, with its kind's facts (schema/PROTOCOL.md, the trip table).
#[derive(Debug, Clone, PartialEq)]
pub enum TripFacts {
    Envelope {
        in_play: bool,
        worst_frac: f64,
        abort_frac: f64,
        samples: u64,
        at_sample: u64,
    },
    DriveVpk {
        vpk: f64,
        ceiling_vpk: f64,
        predicted: bool,
    },
    DriveRms {
        vrms: f64,
        limit_vrms: f64,
    },
    ResponseRms {
        rms: f64,
        limit: f64,
    },
    Alignment {
        lag_frames: f64,
        identification_lag_frames: f64,
        drift_frames: f64,
        limit_frames: f64,
    },
    Coverage {
        coverage: f64,
        min_band_energy: f64,
        error_floor_pct: f64,
    },
    Divergence {
        plays: u64,
        error_pct: f64,
    },
    NotConverged {
        iterations: u64,
        error_pct: f64,
        gate_pct: f64,
    },
    LawFault {
        reason: &'static str,
        consecutive: u64,
    },
}

/// A trip: its facts, the play it came at and the level.
#[derive(Debug, Clone, PartialEq)]
pub struct Trip {
    pub facts: TripFacts,
    pub play: Option<u64>,
    pub at_level_db: Option<f64>,
}

impl Trip {
    pub fn kind(&self) -> &'static str {
        match self.facts {
            TripFacts::Envelope { .. } => "envelope",
            TripFacts::DriveVpk { .. } => "drive_vpk",
            TripFacts::DriveRms { .. } => "drive_rms",
            TripFacts::ResponseRms { .. } => "response_rms",
            TripFacts::Alignment { .. } => "alignment",
            TripFacts::Coverage { .. } => "coverage",
            TripFacts::Divergence { .. } => "divergence",
            TripFacts::NotConverged { .. } => "not_converged",
            TripFacts::LawFault { .. } => "law_fault",
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("kind".into(), Value::from(self.kind()));
        m.insert("play".into(), self.play.map_or(Value::Null, Value::from));
        m.insert("at_level_db".into(), opt_rn(self.at_level_db, 2));
        let mut put = |k: &str, v: Value| {
            m.insert(k.into(), v);
        };
        match &self.facts {
            TripFacts::Envelope {
                in_play,
                worst_frac,
                abort_frac,
                samples,
                at_sample,
            } => {
                put("in_play", Value::from(*in_play));
                put("worst_frac", sn(*worst_frac, 4));
                put("abort_frac", sn(*abort_frac, 4));
                put("samples", Value::from(*samples));
                put("at_sample", Value::from(*at_sample));
            }
            TripFacts::DriveVpk {
                vpk,
                ceiling_vpk,
                predicted,
            } => {
                put("vpk", sn(*vpk, 4));
                put("ceiling_vpk", sn(*ceiling_vpk, 4));
                put("predicted", Value::from(*predicted));
            }
            TripFacts::DriveRms { vrms, limit_vrms } => {
                put("vrms", sn(*vrms, 4));
                put("limit_vrms", sn(*limit_vrms, 4));
            }
            TripFacts::ResponseRms { rms, limit } => {
                put("rms", sn(*rms, 4));
                put("limit", sn(*limit, 4));
            }
            TripFacts::Alignment {
                lag_frames,
                identification_lag_frames,
                drift_frames,
                limit_frames,
            } => {
                put("lag_frames", rn(*lag_frames, 3));
                put(
                    "identification_lag_frames",
                    rn(*identification_lag_frames, 3),
                );
                put("drift_frames", rn(*drift_frames, 3));
                put("limit_frames", rn(*limit_frames, 3));
            }
            TripFacts::Coverage {
                coverage,
                min_band_energy,
                error_floor_pct,
            } => {
                put("coverage", sn(*coverage, 4));
                put("min_band_energy", sn(*min_band_energy, 4));
                put("error_floor_pct", sn(*error_floor_pct, 3));
            }
            TripFacts::Divergence { plays, error_pct } => {
                put("plays", Value::from(*plays));
                put("error_pct", rn(*error_pct, 2));
            }
            TripFacts::NotConverged {
                iterations,
                error_pct,
                gate_pct,
            } => {
                put("iterations", Value::from(*iterations));
                put("error_pct", rn(*error_pct, 2));
                put("gate_pct", num(*gate_pct));
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
    /// Station time.
    pub at_s: Option<f64>,
    pub level_db: Option<f64>,
    pub play: Option<u64>,
    pub frames_done: Option<u64>,
    pub trip: Option<Trip>,
}

impl End {
    pub fn to_json(&self) -> Value {
        json!({
            "outcome": self.outcome,
            "message": self.message,
            "at_s": opt_rn(self.at_s, 3),
            "level_db": opt_rn(self.level_db, 2),
            "play": self.play,
            "frames_done": self.frames_done,
            "trip": self.trip.as_ref().map_or(Value::Null, Trip::to_json),
        })
    }
}

/// The law call behind a play's drive.
#[derive(Debug, Clone, PartialEq)]
pub struct LawCall {
    pub used: bool,
    pub miss: Option<&'static str>,
    pub call_ms: f64,
    pub peak_rise_db: Option<f64>,
    pub bin_rise_db: Option<f64>,
    pub predicted_peak_ratio: Option<f64>,
    /// The values the law reported, by key, in its declared order.
    pub values: Vec<(String, f64)>,
}

impl LawCall {
    pub fn to_json(&self) -> Value {
        let mut values = Map::new();
        for (k, v) in &self.values {
            values.insert(k.clone(), sn(*v, 6));
        }
        json!({
            "used": self.used,
            "miss": self.miss,
            "call_ms": rn(self.call_ms, 3),
            "peak_rise_db": opt_num(self.peak_rise_db.map(|v| r(v, 3))),
            "bin_rise_db": opt_num(self.bin_rise_db.map(|v| r(v, 3))),
            "predicted_peak_ratio": opt_num(self.predicted_peak_ratio.map(|v| r(v, 4))),
            "values": values,
        })
    }
}

/// `iteration` or `playback`.
pub fn kind_name(playback: bool) -> &'static str {
    if playback {
        "playback"
    } else {
        "iteration"
    }
}

/// One play (`twr.plays`, `twr.record`, `last_play`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayRow {
    pub index: u64,
    pub playback: bool,
    pub level_db: f64,
    pub rung: Option<usize>,
    /// From 1 at its level; `None` for a playback play.
    pub iteration: Option<u32>,
    pub rms_error_pct: f64,
    pub peak_error_pct: f64,
    pub spectral_error_db: f64,
    pub gated: bool,
    pub clipped_samples: u64,
    pub clamped_bins: u64,
    pub drive_vpk: f64,
    pub response_peak: f64,
    /// The play's fitted lag, frames; `None` when the run does not fit one.
    pub lag_frames: Option<f64>,
    pub drift_frames: Option<f64>,
    pub start_frame: u64,
    pub frames: u64,
    /// The play's start, station time.
    pub at_s: f64,
    pub law: Option<LawCall>,
}

impl PlayRow {
    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "kind": kind_name(self.playback),
            "level_db": rn(self.level_db, 2),
            "rung": self.rung,
            "iteration": self.iteration,
            "rms_error_pct": rn(self.rms_error_pct, 3),
            "peak_error_pct": rn(self.peak_error_pct, 3),
            "spectral_error_db": rn(self.spectral_error_db, 3),
            "gated": self.gated,
            "clipped_samples": self.clipped_samples,
            "clamped_bins": self.clamped_bins,
            "drive_vpk": sn(self.drive_vpk, 6),
            "response_peak": sn(self.response_peak, 6),
            "lag_frames": opt_num(self.lag_frames.map(|v| r(v, 3))),
            "drift_frames": opt_num(self.drift_frames.map(|v| r(v, 3))),
            "start_frame": self.start_frame,
            "frames": self.frames,
            "at_s": rn(self.at_s, 4),
            "law": self.law.as_ref().map_or(Value::Null, LawCall::to_json),
        })
    }

    /// The last play's metrics, for `twr.live`.
    pub fn last_json(&self) -> Value {
        json!({
            "rms_error_pct": rn(self.rms_error_pct, 3),
            "peak_error_pct": rn(self.peak_error_pct, 3),
            "spectral_error_db": rn(self.spectral_error_db, 3),
            "gated": self.gated,
        })
    }
}

/// The plays done, at this level, and of the playback.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Counts {
    pub done: u64,
    pub at_level: u32,
    pub of_level: u32,
    pub playback: u32,
    pub of_playback: u32,
}

impl Counts {
    pub fn to_json(&self) -> Value {
        json!({
            "done": self.done,
            "at_level": self.at_level,
            "of_level": self.of_level,
            "playback": self.playback,
            "of_playback": self.of_playback,
        })
    }
}

/// What identification found.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentFacts {
    pub excitation: &'static str,
    pub drive_vrms: f64,
    pub drive_vpk: f64,
    pub coherence_mean: f64,
    pub min_mean_coherence: f64,
    /// The trusted bins' coverage of the reference; `None` when identification
    /// held before it was found.
    pub coverage: Option<f64>,
    /// `None` when no lag was found.
    pub lag_frames: Option<f64>,
    /// `measured` or `derived`.
    pub lag_source: &'static str,
    pub passed: bool,
}

impl IdentFacts {
    pub fn to_json(&self) -> Value {
        json!({
            "excitation": self.excitation,
            "drive_vrms": sn(self.drive_vrms, 4),
            "drive_vpk": sn(self.drive_vpk, 4),
            "coherence_mean": rn(self.coherence_mean, 7),
            "min_mean_coherence": num(self.min_mean_coherence),
            "coverage": opt_num(self.coverage.map(|c| sig(c, 6))),
            "lag_frames": opt_num(self.lag_frames.map(|v| r(v, 3))),
            "lag_source": self.lag_source,
            "passed": self.passed,
        })
    }
}

/// The drive in force at the present level against the ceiling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prediction {
    pub drive_vpk: f64,
    pub drive_vpk_ceiling: f64,
    pub level_db: f64,
}

/// The run's law and its unusable calls in a row now.
#[derive(Debug, Clone, PartialEq)]
pub struct LawState {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Empty for a built-in law.
    pub sha256: String,
    pub misses: u32,
}

/// The play in flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InFlight {
    pub index: u64,
    pub playback: bool,
    pub iteration: Option<u32>,
    pub frames_done: u64,
    pub frames: u64,
}

impl InFlight {
    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "kind": kind_name(self.playback),
            "iteration": self.iteration,
            "frames_done": self.frames_done,
            "frames": self.frames,
        })
    }
}

/// What a run shows, published after every step.
#[derive(Debug, Clone, PartialEq)]
pub struct TwrView {
    pub run_id: String,
    pub title: String,
    pub state: State,
    pub phase: &'static str,
    pub station: StationRef,
    pub grid: Grid,
    pub clock: Option<&'static str>,
    pub mode: TwrRunMode,
    pub levels_db: Vec<f64>,
    pub level_db: Option<f64>,
    pub rung: Option<usize>,
    pub plays: Counts,
    pub frozen: bool,
    /// Station time since the run armed.
    pub t_s: Option<f64>,
    pub alarm: bool,
    pub identification: Option<IdentFacts>,
    pub prediction: Option<Prediction>,
    pub last_play: Option<PlayRow>,
    pub law: Option<LawState>,
    pub feasibility: Option<(&'static str, String)>,
    pub end: Option<End>,
    /// The play in flight; set as the view is read, from the run's progress.
    pub play: Option<InFlight>,
    /// `twr.live` frames are due: from arm.
    pub streaming: bool,
    /// The unit of the control channel.
    pub units: String,
    /// The drive ceiling, for the readings.
    pub drive_vpk_ceiling: f64,
    /// The gate, for the readings and the convergence view.
    pub gate_pct: f64,
    /// The run's record, once it is armed.
    pub record: Option<RecordRef>,
}

/// The run's record, from arm on: its folder, whether it holds the streams, and
/// the latest problem in words.
pub use crate::random::view::RecordRef;

fn station_json(s: &StationRef) -> Value {
    json!({
        "kind": s.kind,
        "class": s.class,
        "devices": s.devices.iter().map(|(id, sn)| json!({"id": id, "serial": sn})).collect::<Vec<_>>(),
    })
}

/// The `twr.state` object; `None` is the unconfigured service.
pub fn state_json(v: Option<&TwrView>) -> Value {
    let Some(v) = v else {
        return json!({
            "run_id": null, "state": "unconfigured", "phase": "idle", "title": null,
            "source": null, "station": null, "grid": null, "clock": null, "mode": null,
            "levels_db": null, "level_db": null, "rung": null,
            "plays": Counts::default().to_json(), "frozen": false, "t_s": null, "alarm": false,
            "identification": null, "prediction": null, "last_play": null, "law": null,
            "feasibility": null, "end": null,
        });
    };
    let mut out = json!({
        "run_id": v.run_id,
        "state": v.state.name(),
        "phase": v.phase,
        "title": v.title,
        "source": v.station.source(),
        "station": station_json(&v.station),
        "grid": v.grid.to_json(),
        "clock": v.clock,
        "mode": mode_name(v.mode),
        "levels_db": v.levels_db.iter().map(|x| num(*x)).collect::<Vec<_>>(),
        "level_db": opt_rn(v.level_db, 2),
        "rung": v.rung,
        "plays": v.plays.to_json(),
        "frozen": v.frozen,
        "t_s": opt_rn(v.t_s, 3),
        "alarm": v.alarm,
        "identification": v.identification.as_ref().map_or(Value::Null, IdentFacts::to_json),
        "prediction": v.prediction.map_or(Value::Null, |p| json!({
            "drive_vpk": sn(p.drive_vpk, 4),
            "drive_vpk_ceiling": num(p.drive_vpk_ceiling),
            "level_db": rn(p.level_db, 2),
        })),
        "last_play": v.last_play.as_ref().map_or(Value::Null, PlayRow::to_json),
        "law": v.law.as_ref().map_or(Value::Null, |l| json!({
            "id": l.id,
            "name": l.name,
            "version": l.version,
            "sha256": if l.sha256.is_empty() { Value::Null } else { Value::from(l.sha256.as_str()) },
            "misses": l.misses,
        })),
        "feasibility": v.feasibility.as_ref().map_or(Value::Null, |(verdict, message)| {
            json!({"verdict": verdict, "message": message})
        }),
        "end": v.end.as_ref().map_or(Value::Null, End::to_json),
    });
    if let (Some(r), Value::Object(m)) = (&v.record, &mut out) {
        m.insert("record".into(), r.to_json());
    }
    out
}

/// What decides whether a `twr.state` event is due: everything but the clock.
pub fn state_key(v: Option<&TwrView>) -> String {
    let mut s = state_json(v);
    if let Value::Object(m) = &mut s {
        m.remove("t_s");
    }
    s.to_string()
}

/// The fields `twr.live` and `twr.data` share.
pub fn head_json(v: &TwrView) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("run_id".into(), Value::from(v.run_id.as_str()));
    m.insert("source".into(), Value::from(v.station.source()));
    m.insert("state".into(), Value::from(v.state.name()));
    m.insert("phase".into(), Value::from(v.phase));
    m.insert("mode".into(), Value::from(mode_name(v.mode)));
    m.insert("t_s".into(), rn(v.t_s.unwrap_or(0.0), 3));
    m.insert("level_db".into(), opt_rn(v.level_db, 2));
    m.insert("rung".into(), v.rung.map_or(Value::Null, Value::from));
    m.insert("plays".into(), v.plays.to_json());
    m.insert(
        "play".into(),
        v.play.as_ref().map_or(Value::Null, InFlight::to_json),
    );
    m.insert(
        "last".into(),
        v.last_play.as_ref().map_or(Value::Null, PlayRow::last_json),
    );
    m
}

/// The `twr.live` data: the head and the Scope view.
pub fn live_json(v: &TwrView, scope: &Scope, arrays: &mut Arrays) -> Value {
    let mut m = head_json(v);
    let views = match scope_view(v, scope, SCOPE_POINTS, arrays) {
        Some(view) => vec![view],
        None => Vec::new(),
    };
    m.insert("views".into(), Value::Array(views));
    Value::Object(m)
}

// ---------------------------------------------------------------------------------
// The figures and the display views.
// ---------------------------------------------------------------------------------

/// The most points the live Scope carries.
pub const SCOPE_POINTS: usize = 1024;

/// The control channel's most recent samples, from the station's display tap: the
/// first sample is at `t0_s` station time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope {
    pub t0_s: f64,
    pub fs_hz: f64,
    pub samples: Vec<f64>,
}

/// One play's records, at the level it played: the level target, the response
/// and the drive as played, from the record's first sample.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayRecords {
    pub fs_hz: f64,
    pub target: Vec<f64>,
    pub response: Vec<f64>,
    pub drive: Vec<f64>,
    pub alarm_frac: f64,
    pub abort_frac: f64,
    /// `abs(R)`, `abs(Y)` and `abs(D)` on the record grid's bins in the correction
    /// band.
    pub spectra: Option<Spectra>,
}

/// A play's spectra on the record grid's bins in the correction band.
#[derive(Debug, Clone, PartialEq)]
pub struct Spectra {
    pub freqs_hz: Vec<f64>,
    pub target: Vec<f64>,
    pub response: Vec<f64>,
    pub drive: Vec<f64>,
}

/// The identified plant on the identification grid, in its band.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentFigures {
    pub freqs_hz: Vec<f64>,
    pub gain: Vec<f64>,
    pub phase_deg: Vec<f64>,
    pub coherence: Vec<f64>,
    pub coherence_floor: f64,
    pub min_mean_coherence: f64,
    /// Control unit per volt.
    pub unit: String,
}

/// The trusted bins on the record grid, with the reference's spectrum scaled to 1
/// at its peak.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskFigures {
    pub freqs_hz: Vec<f64>,
    pub trusted: Vec<f64>,
    pub reference: Vec<f64>,
}

/// One declared value a law reports, and its words.
#[derive(Debug, Clone, PartialEq)]
pub struct LawValueSpec {
    pub key: String,
    pub label: String,
    pub unit: String,
}

fn time_x() -> Axis {
    Axis::new("Time", Some("s"), Scale::Linear)
}

fn unit_y(label: &str, unit: &str) -> Axis {
    Axis::new(label, Some(unit), Scale::Linear)
}

/// A record against time from its first sample.
fn record_series(name: &str, role: Role, fs: f64, y: &[f64], points: Option<usize>) -> Series {
    time_series(name, role, time_axis(0, y.len(), fs), y, points)
}

/// The Target view: the level target over the record.
pub fn target_view(
    p: &PlayRecords,
    units: &str,
    points: Option<usize>,
    arrays: &mut Arrays,
) -> Value {
    views::spectrum(
        arrays,
        "Target",
        &time_x(),
        &unit_y("Acceleration", units),
        vec![record_series(
            "Target",
            Role::Reference,
            p.fs_hz,
            &p.target,
            points,
        )],
        Some("time"),
    )
}

/// The Response view (`Replication` in `twr.record`): the response with the
/// level target.
pub fn response_view(
    title: &str,
    p: &PlayRecords,
    units: &str,
    points: Option<usize>,
    arrays: &mut Arrays,
) -> Value {
    views::spectrum(
        arrays,
        title,
        &time_x(),
        &unit_y("Acceleration", units),
        vec![
            record_series("Response", Role::Measured, p.fs_hz, &p.response, points),
            record_series("Target", Role::Reference, p.fs_hz, &p.target, points),
        ],
        Some("time"),
    )
}

/// The Error view: the response less the target.
pub fn error_view(
    p: &PlayRecords,
    units: &str,
    points: Option<usize>,
    arrays: &mut Arrays,
) -> Value {
    let e: Vec<f64> = p
        .response
        .iter()
        .zip(&p.target)
        .map(|(y, t)| y - t)
        .collect();
    views::spectrum(
        arrays,
        "Error",
        &time_x(),
        &unit_y("Error", units),
        vec![record_series("Error", Role::Measured, p.fs_hz, &e, points)],
        Some("time"),
    )
}

/// The Drive view: the drive as played.
pub fn drive_view(p: &PlayRecords, points: Option<usize>, arrays: &mut Arrays) -> Value {
    views::spectrum(
        arrays,
        "Drive",
        &time_x(),
        &unit_y("Drive", "V"),
        vec![record_series(
            "Drive",
            Role::Drive,
            p.fs_hz,
            &p.drive,
            points,
        )],
        Some("time"),
    )
}

/// The Envelope view: the response with the alarm and abort bands around the
/// target.
pub fn envelope_view(
    p: &PlayRecords,
    units: &str,
    points: Option<usize>,
    arrays: &mut Arrays,
) -> Value {
    let peak = p.target.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
    let band = |frac: f64, sign: f64| -> Vec<f64> {
        p.target.iter().map(|t| t + sign * frac * peak).collect()
    };
    views::spectrum(
        arrays,
        "Envelope",
        &time_x(),
        &unit_y("Acceleration", units),
        vec![
            record_series("Response", Role::Measured, p.fs_hz, &p.response, points),
            record_series(
                "Alarm high",
                Role::Tolerance,
                p.fs_hz,
                &band(p.alarm_frac, 1.0),
                points,
            ),
            record_series(
                "Alarm low",
                Role::Tolerance,
                p.fs_hz,
                &band(p.alarm_frac, -1.0),
                points,
            ),
            record_series(
                "Abort high",
                Role::Abort,
                p.fs_hz,
                &band(p.abort_frac, 1.0),
                points,
            ),
            record_series(
                "Abort low",
                Role::Abort,
                p.fs_hz,
                &band(p.abort_frac, -1.0),
                points,
            ),
        ],
        Some("time"),
    )
}

/// The Spectra view: `abs(R)` at the level, `abs(Y)` and `abs(D)` of the last play.
pub fn spectra_view(p: &PlayRecords, units: &str, arrays: &mut Arrays) -> Option<Value> {
    let s = p.spectra.as_ref()?;
    let (f, rr, y, d) = (&s.freqs_hz, &s.target, &s.response, &s.drive);
    Some(views::spectrum(
        arrays,
        "Spectra",
        &Axis::new("Frequency", Some("Hz"), Scale::Log),
        &Axis::new("Magnitude", Some(units), Scale::Log),
        vec![
            Series::new("Target", Role::Reference, f.clone(), rr.clone()),
            Series::new("Response", Role::Measured, f.clone(), y.clone()),
            Series::new("Drive", Role::Drive, f.clone(), d.clone()),
        ],
        Some("frequency"),
    ))
}

/// The identification's views: Identification Gain and Identification Phase.
pub fn frf_views(f: &IdentFigures, arrays: &mut Arrays) -> Vec<Value> {
    let x = Axis::new("Frequency", Some("Hz"), Scale::Log);
    let gain = views::spectrum(
        arrays,
        "Identification Gain",
        &x,
        &Axis::new("Gain", Some(&f.unit), Scale::Log),
        vec![Series::new(
            "Gain",
            Role::Measured,
            f.freqs_hz.clone(),
            f.gain.clone(),
        )],
        Some("frequency"),
    );
    let phase = views::spectrum(
        arrays,
        "Identification Phase",
        &x,
        &Axis::new("Phase", Some("deg"), Scale::Linear),
        vec![Series::new(
            "Phase",
            Role::Measured,
            f.freqs_hz.clone(),
            f.phase_deg.clone(),
        )],
        Some("frequency"),
    );
    vec![gain, phase]
}

/// The Coherence view, with the trust floor and the hold.
pub fn coherence_view(f: &IdentFigures, arrays: &mut Arrays) -> Value {
    let ends = || {
        vec![
            f.freqs_hz.first().copied().unwrap_or(0.0),
            f.freqs_hz.last().copied().unwrap_or(0.0),
        ]
    };
    views::spectrum(
        arrays,
        "Coherence",
        &Axis::new("Frequency", Some("Hz"), Scale::Log),
        &Axis::new("Coherence", None, Scale::Linear),
        vec![
            Series::new(
                "Coherence",
                Role::Measured,
                f.freqs_hz.clone(),
                f.coherence.clone(),
            ),
            Series::new(
                "Trust floor",
                Role::Tolerance,
                ends(),
                vec![f.coherence_floor; 2],
            ),
            Series::new("Hold", Role::Abort, ends(), vec![f.min_mean_coherence; 2]),
        ],
        Some("frequency"),
    )
}

/// The Trust view: the trusted bins (1 trusted, 0 not), with the reference's
/// spectrum scaled to 1 at its peak.
pub fn mask_view(f: &MaskFigures, arrays: &mut Arrays) -> Value {
    views::spectrum(
        arrays,
        "Trust",
        &Axis::new("Frequency", Some("Hz"), Scale::Log),
        &Axis::new("Trusted", None, Scale::Linear),
        vec![
            Series::new(
                "Trusted",
                Role::Measured,
                f.freqs_hz.clone(),
                f.trusted.clone(),
            ),
            Series::new(
                "Reference",
                Role::Reference,
                f.freqs_hz.clone(),
                f.reference.clone(),
            ),
        ],
        Some("frequency"),
    )
}

/// The Convergence view: each play's rms error against its index, with the gate.
pub fn convergence_view(rows: &[PlayRow], gate_pct: f64, arrays: &mut Arrays) -> Value {
    let x: Vec<f64> = rows.iter().map(|p| p.index as f64).collect();
    let y: Vec<f64> = rows.iter().map(|p| p.rms_error_pct).collect();
    let ends = vec![
        x.first().copied().unwrap_or(0.0),
        x.last().copied().unwrap_or(0.0),
    ];
    views::spectrum(
        arrays,
        "Convergence",
        &Axis::new("Play", None, Scale::Linear),
        &Axis::new("Error", Some("%"), Scale::Log),
        vec![
            Series::new("Error", Role::Measured, x, y),
            Series::new("Gate", Role::Tolerance, ends, vec![gate_pct; 2]),
        ],
        None,
    )
}

/// The Law views: each declared value against the play index of its call, and the
/// last call's values by their labels with the unusable calls in a row.
pub fn law_views(
    rows: &[PlayRow],
    specs: &[LawValueSpec],
    misses: u32,
    arrays: &mut Arrays,
) -> Vec<Value> {
    let called: Vec<&PlayRow> = rows.iter().filter(|p| p.law.is_some()).collect();
    let series: Vec<Series> = specs
        .iter()
        .map(|s| {
            let mut x = Vec::new();
            let mut y = Vec::new();
            for p in &called {
                if let Some(v) = p
                    .law
                    .as_ref()
                    .and_then(|l| l.values.iter().find(|(k, _)| k == &s.key))
                {
                    x.push(p.index as f64);
                    y.push(v.1);
                }
            }
            Series::new(&s.label, Role::Measured, x, y)
        })
        .collect();
    let law = views::spectrum(
        arrays,
        "Law",
        &Axis::new("Play", None, Scale::Linear),
        &Axis::new("Value", None, Scale::Linear),
        series,
        None,
    );
    // The values of the last call that was used: a missed call has none.
    let last = called
        .iter()
        .rev()
        .filter_map(|p| p.law.as_ref())
        .find(|l| l.used);
    let mut rows_out: Vec<ReadingRow> = specs
        .iter()
        .filter_map(|s| {
            let v = last.and_then(|l| l.values.iter().find(|(k, _)| k == &s.key))?;
            Some(ReadingRow::number(
                &s.label,
                sig(v.1, 6),
                (!s.unit.is_empty()).then_some(s.unit.as_str()),
                Status::Info,
            ))
        })
        .collect();
    rows_out.push(ReadingRow::number(
        "Unusable calls in a row",
        f64::from(misses),
        None,
        if misses == 0 {
            Status::Pass
        } else {
            Status::Warn
        },
    ));
    vec![law, reading("Law Values", rows_out)]
}

/// The Scope view: the control channel during the play in flight; `None` between
/// plays.
pub fn scope_view(v: &TwrView, s: &Scope, points: usize, arrays: &mut Arrays) -> Option<Value> {
    if v.play.is_none() || s.samples.is_empty() || s.fs_hz.is_nan() || s.fs_hz <= 0.0 {
        return None;
    }
    let t: Vec<f64> = (0..s.samples.len())
        .map(|i| s.t0_s + i as f64 / s.fs_hz)
        .collect();
    Some(views::spectrum(
        arrays,
        "Scope",
        &time_x(),
        &unit_y("Acceleration", &v.units),
        vec![time_series(
            "Control",
            Role::Measured,
            t,
            &s.samples,
            Some(points),
        )],
        Some("time"),
    ))
}

/// A `reading` view whose numbers are written as the binary frames' headers write
/// them: a whole number as an integer, so every language's writer gives the same
/// bytes.
fn reading(title: &str, rows: Vec<ReadingRow>) -> Value {
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let mut e = Map::new();
            e.insert("label".into(), Value::from(r.label));
            e.insert(
                "value".into(),
                match r.value {
                    views::RowValue::Number(x) => num(x),
                    views::RowValue::Text(s) => Value::from(s),
                },
            );
            e.insert("unit".into(), r.unit.map_or(Value::Null, Value::from));
            e.insert("status".into(), Value::from(r.status.as_str()));
            Value::Object(e)
        })
        .collect();
    json!({"view": "reading", "title": title, "rows": rows})
}

/// The run's numbers, as a reading.
pub fn readings_view(v: &TwrView) -> Value {
    let mut rows = Vec::new();
    if let Some(l) = v.level_db {
        rows.push(ReadingRow::number(
            "Level",
            r(l, 2),
            Some("dB"),
            Status::Info,
        ));
    }
    rows.push(ReadingRow::number(
        "Plays",
        v.plays.done as f64,
        None,
        Status::Info,
    ));
    if v.plays.of_playback > 0 && v.plays.playback > 0 {
        rows.push(ReadingRow::text(
            "Playback",
            &format!("{} of {}", v.plays.playback, v.plays.of_playback),
            Status::Info,
        ));
    }
    if let Some(p) = &v.last_play {
        rows.push(ReadingRow::number(
            "Last error",
            r(p.rms_error_pct, 3),
            Some("%"),
            if p.gated { Status::Pass } else { Status::Info },
        ));
        rows.push(ReadingRow::number(
            "Gate",
            v.gate_pct,
            Some("%"),
            Status::Info,
        ));
        rows.push(ReadingRow::number(
            "Last drive peak",
            sig(p.drive_vpk, 4),
            Some("V"),
            if p.drive_vpk > v.drive_vpk_ceiling {
                Status::Fail
            } else {
                Status::Info
            },
        ));
        rows.push(ReadingRow::number(
            "Last response peak",
            sig(p.response_peak, 4),
            Some(&v.units),
            Status::Info,
        ));
    }
    if let Some(p) = &v.prediction {
        rows.push(ReadingRow::number(
            "Drive in force",
            sig(p.drive_vpk, 4),
            Some("V"),
            if p.drive_vpk > p.drive_vpk_ceiling {
                Status::Fail
            } else {
                Status::Info
            },
        ));
    }
    if let Some(t) = v.t_s {
        rows.push(ReadingRow::number(
            "Station time",
            r(t, 3),
            Some("s"),
            Status::Info,
        ));
    }
    reading("Run", rows)
}

/// The Plays table.
pub fn plays_view(rows: &[PlayRow]) -> Value {
    let f = |x: f64, d: i32| num(r(x, d)).to_string();
    let body: Vec<Value> = rows
        .iter()
        .map(|p| {
            json!([
                (p.index + 1).to_string(),
                kind_name(p.playback),
                f(p.level_db, 2),
                p.iteration.map_or(String::new(), |i| i.to_string()),
                f(p.rms_error_pct, 2),
                f(p.peak_error_pct, 2),
                f(p.spectral_error_db, 2),
                if p.gated { "yes" } else { "no" },
                num(sig(p.drive_vpk, 4)).to_string(),
                p.clipped_samples.to_string(),
                p.clamped_bins.to_string(),
                p.lag_frames.map_or(String::new(), |v| f(v, 2)),
            ])
        })
        .collect();
    json!({
        "view": "table",
        "title": "Plays",
        "columns": ["Play", "Kind", "Level (dB)", "Iteration", "Error (%)", "Peak error (%)",
                    "Spectral error (dB)", "Gated", "Drive peak (V)", "Clipped samples",
                    "Clamped bins", "Lag (frames)"],
        "rows": body,
    })
}

/// A freshly configured run's view: nothing opened, nothing played.
pub fn configured_view(
    run_id: &str,
    cfg: &super::config::WireTwr,
    derived: &super::config::Derived,
    station: StationRef,
    units: &str,
) -> TwrView {
    TwrView {
        run_id: run_id.to_owned(),
        title: cfg.title.clone(),
        state: State::Configured,
        phase: "idle",
        station,
        grid: derived.preview.grid,
        clock: None,
        mode: cfg.mode,
        levels_db: cfg.levels_db.clone(),
        level_db: None,
        rung: None,
        plays: Counts {
            of_level: cfg.max_iterations.min(u64::from(u32::MAX)) as u32,
            of_playback: derived.plays_target,
            ..Counts::default()
        },
        frozen: false,
        t_s: None,
        alarm: false,
        identification: None,
        prediction: None,
        last_play: None,
        law: Some(LawState {
            id: derived.law.id.clone(),
            name: derived.law.name.clone(),
            version: derived.law.version.clone(),
            sha256: derived.law.sha256.clone(),
            misses: 0,
        }),
        feasibility: None,
        end: None,
        play: None,
        streaming: false,
        units: units.to_owned(),
        drive_vpk_ceiling: cfg.drive_vpk_ceiling,
        gate_pct: cfg.gate_pct,
        record: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trip_names_its_kind_first_and_its_facts_after() {
        let t = Trip {
            facts: TripFacts::DriveVpk {
                vpk: 0.2,
                ceiling_vpk: 0.15,
                predicted: true,
            },
            play: None,
            at_level_db: Some(0.0),
        };
        let j = t.to_json();
        let keys: Vec<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "kind",
                "play",
                "at_level_db",
                "vpk",
                "ceiling_vpk",
                "predicted"
            ]
        );
    }

    #[test]
    fn the_coherence_hold_is_live_and_is_not_busy() {
        assert!(State::Held.is_live());
        assert!(!State::Held.is_busy());
        assert!(!State::Finished.is_live());
    }
}
