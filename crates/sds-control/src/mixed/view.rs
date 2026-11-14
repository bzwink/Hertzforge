//! What a Mixed Mode run shows: the `mixed.state` object, the numbers every live
//! frame carries, and the views drawn from the controller's display
//! (schema/PROTOCOL.md, "Mixed Mode Run State" and "Mixed Mode Display"). The
//! per-part rows, the floor's lines, the totals and the trip are encoded from the
//! controller's [`MixedStatus`]; the spectra from its [`MixedDisplay`]. Keys are
//! written in the contract's order.

use super::config::{part_names, round, sig, Grid, RoleLaw, WireBody, WireMixed, WireSchedule};
use crate::codec::Arrays;
use crate::protocol::{num, opt_num};
use crate::views::{self, Axis, Role, Row, Scale, Series, Status};
use hertzforge_dsp::sweep::SweepRate;
use hertzforge_mixed::{
    Direction, LawRole, MixedDisplay, MixedStatus, MixedTrip, MixedTripKind, PartRow,
};
use hertzforge_random::RunMode;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub use crate::random::view::State;
pub use crate::sine::view::{IdentFacts, StationRef};

/// One role's law and its counters.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleState {
    pub law: RoleLaw,
    pub misses: u32,
    pub capped: u64,
    /// The live parameters' values in force, by key.
    pub live: Vec<(String, f64)>,
}

/// Both roles' laws and the values each reported on its last call.
#[derive(Debug, Clone, PartialEq)]
pub struct LawState {
    pub floor: RoleState,
    pub tones: RoleState,
    /// The floor law's values on its last call, by key.
    pub floor_values: Vec<(String, f64)>,
    /// Each tone instance's values on its last call: part id, values.
    pub tone_values: Vec<(String, Vec<(String, f64)>)>,
    /// Whether the laws have been built (the live frame's `law` is null until then).
    pub built: bool,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub struct End {
    pub outcome: &'static str,
    pub message: String,
    pub at_s: Option<f64>,
    pub level_db: Option<f64>,
    pub ramp_s: Option<f64>,
    pub trip: Option<Value>,
}

/// One enabled input in the live frame.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveChannel {
    pub index: u32,
    pub description: String,
    pub role: &'static str,
    pub rms: Option<f64>,
    pub unit: String,
    pub overload: bool,
}

/// What the configuration says about each part that the rows need.
#[derive(Debug, Clone, PartialEq)]
pub struct PartFacts {
    pub id: String,
    pub kind: &'static str,
    pub name: String,
    /// The sweep's law, its own or its leader's (`true` log); `None` when it
    /// stands still.
    pub log_rate: Option<bool>,
    /// Whether the part sweeps on its own schedule (a follower does not).
    pub own_sweep: bool,
    /// The sweeps a count asks for.
    pub sweeps_of: Option<u32>,
    /// The part it follows and the ratio it follows at, as configured.
    pub follows: Option<(String, f64)>,
    /// The part whose own sweep moves this one (itself, or the end of its
    /// chain of leaders); `None` when nothing in the chain sweeps.
    pub root: Option<String>,
    /// The rate this part moves at while its root sweeps, unsigned, in its
    /// law's unit: the root's own rate, times the chain's ratio for a linear
    /// sweep (a ratio keeps octaves per minute).
    pub rate_abs: Option<f64>,
}

/// The parts' facts, in the parts' order.
pub fn part_facts(w: &WireMixed) -> Vec<PartFacts> {
    let names = part_names(&w.parts);
    // Walk to the sweep that moves part `i`: its id, its rate and law, the
    // chain's ratio and the sweeps a count asks for.
    let root_of = |mut i: usize| -> Option<(String, SweepRate, f64, Option<u32>)> {
        let mut ratio = 1.0;
        for _ in 0..=w.parts.len() {
            match w.parts[i].schedule() {
                Some(WireSchedule::Sweep { rate, sweeps, .. }) => {
                    return Some((w.parts[i].id.clone(), *rate, ratio, *sweeps))
                }
                Some(WireSchedule::Follow { of, ratio: r }) => {
                    ratio *= r;
                    i = w.parts.iter().position(|p| &p.id == of)?;
                }
                _ => return None,
            }
        }
        None
    };
    w.parts
        .iter()
        .enumerate()
        .zip(names)
        .map(|((i, p), name)| {
            let root = root_of(i);
            let own_sweep = matches!(p.schedule(), Some(WireSchedule::Sweep { .. }));
            let log_rate = root
                .as_ref()
                .map(|(_, r, _, _)| matches!(r, SweepRate::LogOctPerMin(_)));
            let rate_abs = root.as_ref().map(|(_, r, ratio, _)| match r {
                SweepRate::LogOctPerMin(x) => x.abs(),
                SweepRate::LinearHzPerSec(x) => (x * ratio).abs(),
            });
            PartFacts {
                id: p.id.clone(),
                kind: match p.body {
                    WireBody::Floor => "floor",
                    WireBody::Tone { .. } => "tone",
                    _ => "narrowband",
                },
                name,
                log_rate,
                own_sweep,
                sweeps_of: if own_sweep {
                    root.as_ref().and_then(|r| r.3)
                } else {
                    None
                },
                follows: match p.schedule() {
                    Some(WireSchedule::Follow { of, ratio }) => Some((of.clone(), *ratio)),
                    _ => None,
                },
                root: root.map(|r| r.0),
                rate_abs,
            }
        })
        .collect()
}

/// Everything the state and the live frame say, as the run last published it.
#[derive(Debug, Clone, PartialEq)]
pub struct MixedView {
    pub run_id: String,
    pub title: String,
    pub state: State,
    pub phase: &'static str,
    pub station: StationRef,
    pub grid: Grid,
    pub clock: Option<&'static str>,
    pub mode: RunMode,
    pub levels_db: Vec<f64>,
    /// The commanded level.
    pub level_db: Option<f64>,
    /// The level the drive is at now.
    pub achieved_db: Option<f64>,
    pub full_level_s: f64,
    pub t_s: Option<f64>,
    pub elapsed_s: Option<f64>,
    pub parts_s: Option<f64>,
    pub at_level_s: Option<f64>,
    pub remaining_at_level_s: Option<f64>,
    pub level_s: Option<f64>,
    pub loop_n: u64,
    pub tick_n: u64,
    pub held: bool,
    pub parts: Vec<PartFacts>,
    /// The controller's status as of the last chunk; `None` before it exists.
    pub status: Option<MixedStatus>,
    /// The composite's reference rms, g (the configuration's, at the level).
    pub reference_rms_g: f64,
    pub identification: Option<IdentFacts>,
    pub law: Option<LawState>,
    pub feasibility: Option<(&'static str, String)>,
    pub end: Option<End>,
    pub channels: Vec<LiveChannel>,
    /// Whether the run has started streaming (`mixed.live` from identification on).
    pub streaming: bool,
    /// The spectra the views are drawn from, as of the last publish.
    pub display: Option<Arc<MixedDisplay>>,
    /// The run's record, once it is armed.
    pub record: Option<crate::random::view::RecordRef>,
}

fn rows(v: &MixedView) -> Vec<Option<&PartRow>> {
    v.parts
        .iter()
        .map(|f| {
            v.status
                .as_ref()
                .and_then(|s| s.parts.iter().find(|r| r.id == f.id))
        })
        .collect()
}

/// Whether a part is playing now, ramps included: the floor while the core
/// says it plays; a tone or a narrowband while it has drive (its ramp scale above
/// 0), so it reads off while it waits for the floor or its window and once it has
/// ramped out.
pub fn playing(r: &PartRow) -> bool {
    match r.kind {
        hertzforge_mixed::PartKind::Floor => r.on,
        _ => r.scale > 0.0,
    }
}

fn dir_name(d: Direction) -> &'static str {
    match d {
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

/// One part's row, from the controller's row (or the configuration's facts
/// before there is one).
pub fn part_row_json(f: &PartFacts, row: Option<&PartRow>, s: Option<&MixedStatus>) -> Value {
    let is_floor = f.kind == "floor";
    let is_tone = f.kind == "tone";
    let is_band = f.kind == "narrowband";
    let r = row;
    // A part moves with the sweep at the root of its chain (itself, or the part
    // it follows, or that one's leader): the root's direction now, and this
    // part's own rate, signed, 0 while the parts' clock does not run (before the
    // parts come in, at a hold, on a stop) or the sweep has ended.
    let root = f
        .root
        .as_deref()
        .and_then(|id| s.and_then(|s| s.parts.iter().find(|p| p.id == id)));
    let direction = root
        .and_then(|p| p.direction)
        .filter(|_| f.log_rate.is_some());
    let rate = f.rate_abs.zip(direction).map(|(x, d)| {
        let clock_runs = s.is_some_and(|s| {
            use hertzforge_mixed::{MixedPhase as P, MixedState as S};
            !s.held
                && matches!(s.state, S::Engaged | S::FullLevel)
                && matches!(s.phase, P::PartsRampingIn | P::Running)
        });
        let sweeping = root.is_some_and(|p| p.on && !p.done)
            // A tone's own reading of its motion: 0 when the core holds its
            // sweep (the bands' rows carry none).
            && root.is_none_or(|p| p.kind != hertzforge_mixed::PartKind::Tone || p.rate != Some(0.0));
        if !(clock_runs && sweeping) {
            0.0
        } else if d == Direction::Down {
            -x
        } else {
            x
        }
    });
    let unit = if is_band { "g²/Hz" } else { "g" };
    let (lines, dof) = if is_floor {
        let fl = s.map(|s| s.floor);
        (
            fl.map_or(Value::Null, |fl| {
                json!({
                    "in_band": fl.lines_in_band,
                    "clean": fl.lines_in_band.saturating_sub(fl.lines_out),
                    "alarm": fl.alarm_lines,
                    "abort": fl.abort_lines,
                    "rms_judged": fl.rms_judged_lines,
                })
            }),
            fl.map_or(Value::Null, |fl| num(round(fl.dof, 1))),
        )
    } else {
        (
            Value::Null,
            if is_band {
                r.and_then(|r| r.dof)
                    .map_or(Value::Null, |d| num(round(d, 1)))
            } else {
                Value::Null
            },
        )
    };
    let (target, measured, error_db) = if is_floor {
        let fl = s.map(|s| s.floor);
        (
            fl.map(|f| f.rms_target_g),
            fl.and_then(|f| f.rms_g),
            fl.and_then(|f| f.error_db_rms),
        )
    } else {
        (
            r.map(|r| r.target),
            r.and_then(|r| r.measured),
            r.and_then(|r| r.error_db),
        )
    };
    json!({
        "id": f.id,
        "kind": f.kind,
        "name": f.name,
        "enabled": r.is_none_or(|r| r.enabled),
        "on": r.is_some_and(playing),
        "scale": num(round(r.map_or(0.0, |r| r.scale), 4)),
        "freq_hz": if is_floor { Value::Null } else { opt_num(r.and_then(|r| r.freq_hz).map(|x| round(x, 2))) },
        "direction": direction.map(dir_name).map_or(Value::Null, Value::from),
        "follows": f.follows.as_ref().map_or(Value::Null, |(of, ratio)| json!({"of": of, "ratio": num(sig(*ratio, 6))})),
        "sweeps": if f.own_sweep {
            json!({"done": r.map_or(0, |r| r.sweeps_done), "of": f.sweeps_of})
        } else {
            Value::Null
        },
        "rate_oct_per_min": if f.log_rate == Some(true) { opt_num(rate.map(|x| sig(x, 4))) } else { Value::Null },
        "rate_hz_per_s": if f.log_rate == Some(false) { opt_num(rate.map(|x| sig(x, 4))) } else { Value::Null },
        "width_hz": if is_band { opt_num(r.and_then(|r| r.width_hz).map(|x| round(x, 3))) } else { Value::Null },
        "unit": unit,
        "target": opt_num(target.map(|x| sig(x, 4))),
        "measured": opt_num(measured.map(|x| sig(x, 4))),
        "error_db": opt_num(error_db.map(|x| round(x, 2))),
        "judge_s": if is_tone { opt_num(r.and_then(|r| r.judge_s).map(|x| round(x, 3))) } else { Value::Null },
        "judged_error_db": if is_tone { opt_num(r.and_then(|r| r.judged_error_db).map(|x| round(x, 2))) } else { Value::Null },
        "judged_alarm_db": if is_tone { db_pair(r.and_then(|r| r.judged_alarm_db)) } else { Value::Null },
        "judged_abort_db": if is_tone { db_pair(r.and_then(|r| r.judged_abort_db)) } else { Value::Null },
        "tolerance_widened": if is_tone { Value::from(r.is_some_and(|r| r.tolerance_widened)) } else { Value::Null },
        "alarm": r.is_some_and(|r| r.alarm),
        "drive_vpk": if is_tone { opt_num(r.map(|r| sig(r.drive, 4))) } else { Value::Null },
        // The core keeps no drive of its own for a narrowband (its share of the
        // random part is not reported), so a band's is null, never a 0.
        "drive_vrms": if is_floor { opt_num(r.map(|r| sig(r.drive, 4))) } else { Value::Null },
        "gain_db": if is_band { opt_num(r.and_then(|r| r.gain_db).map(|x| round(x, 2))) } else { Value::Null },
        "settled": r.is_some_and(|r| r.settled),
        "limiting": r.is_some_and(|r| r.limiting),
        "dof": dof,
        "lines": lines,
        "law": if is_band { Value::Null } else { json!({"misses": r.map_or(0, |r| r.misses), "capped": r.map_or(0, |r| r.capped)}) },
    })
}

/// A band `[below, above]` in dB, or null.
fn db_pair(b: Option<(f64, f64)>) -> Value {
    b.map_or(Value::Null, |(lo, hi)| {
        json!([num(round(lo, 2)), num(round(hi, 2))])
    })
}

fn parts_json(v: &MixedView) -> Value {
    Value::Array(
        v.parts
            .iter()
            .zip(rows(v))
            .map(|(f, r)| part_row_json(f, r, v.status.as_ref()))
            .collect(),
    )
}

/// The composite's reference rms at the level in force: the configuration's
/// full-level rms scaled by the level, the level the Control view's reference
/// is drawn at. Before anything drives, the full level's.
pub fn reference_rms_at_level(v: &MixedView) -> f64 {
    v.reference_rms_g * 10f64.powf(v.achieved_db.unwrap_or(0.0) / 20.0)
}

fn total_json(v: &MixedView) -> Value {
    let t = v.status.as_ref().map(|s| s.totals);
    json!({
        "rms_g": opt_num(t.and_then(|t| t.control_rms_g).map(|x| sig(x, 4))),
        "reference_rms_g": num(sig(reference_rms_at_level(v), 4)),
        "drive_vpk": opt_num(t.map(|t| sig(t.drive_vpk, 4))),
        "drive_vrms": opt_num(t.map(|t| sig(t.drive_vrms, 4))),
    })
}

fn role_json(r: &RoleState) -> Value {
    let mut live = Map::new();
    for (k, x) in &r.live {
        live.insert(k.clone(), num(*x));
    }
    json!({
        "id": r.law.id,
        "name": r.law.name,
        "version": r.law.version,
        "sha256": if r.law.sha256.is_empty() { Value::Null } else { Value::from(r.law.sha256.as_str()) },
        "misses": r.misses,
        "capped": r.capped,
        "live": live,
    })
}

pub fn end_json(e: &End) -> Value {
    json!({
        "outcome": e.outcome,
        "message": e.message,
        "at_s": opt_num(e.at_s.map(|x| round(x, 3))),
        "level_db": opt_num(e.level_db.map(|x| round(x, 2))),
        "ramp_s": opt_num(e.ramp_s.map(|x| round(x, 3))),
        "trip": e.trip.clone().unwrap_or(Value::Null),
    })
}

fn ident_json(i: &IdentFacts) -> Value {
    json!({
        "level_db": num(i.level_db),
        "band_coherence": num(round(i.band_coherence, 4)),
        "gate_coherence": num(i.gate_coherence),
        "lag_frames": i.lag_frames,
        "trusted_frac": num(round(i.trusted_frac, 4)),
        "passed": i.passed,
    })
}

/// The `mixed.state` object; `None` is the unconfigured service.
pub fn state_json(v: Option<&MixedView>) -> Value {
    let Some(v) = v else {
        return json!({
            "run_id": null, "state": "unconfigured", "phase": "idle", "title": null,
            "source": null, "station": null, "grid": null, "clock": null, "mode": null,
            "levels_db": null, "level_db": null, "full_level_s": null, "t_s": null,
            "elapsed_s": null, "parts_s": null, "at_level_s": null,
            "remaining_at_level_s": null, "held": false, "alarm": false, "limiting": false,
            "parts": [], "total": null, "identification": null, "law": null,
            "feasibility": null, "end": null,
        });
    };
    let mut m = json!({
        "run_id": v.run_id,
        "state": v.state.name(),
        "phase": v.phase,
        "title": v.title,
        "source": v.station.source(),
        "station": v.station.to_json(),
        "grid": v.grid.to_json(),
        "clock": v.clock,
        "mode": crate::random::view::mode_name(v.mode),
        "levels_db": v.levels_db.iter().map(|l| num(*l)).collect::<Vec<_>>(),
        "level_db": opt_num(v.level_db.map(|x| round(x, 2))),
        "full_level_s": num(v.full_level_s),
        "t_s": opt_num(v.t_s.map(|x| round(x, 3))),
        "elapsed_s": opt_num(v.elapsed_s.map(|x| round(x, 3))),
        "parts_s": opt_num(v.parts_s.map(|x| round(x, 3))),
        "at_level_s": opt_num(v.at_level_s.map(|x| round(x, 3))),
        "remaining_at_level_s": opt_num(v.remaining_at_level_s.map(|x| round(x, 3))),
        "held": v.held,
        "alarm": alarm(v),
        "limiting": limiting(v),
        "parts": parts_json(v),
        "total": total_json(v),
        "identification": v.identification.as_ref().map_or(Value::Null, ident_json),
        "law": v.law.as_ref().map_or(Value::Null, |l| json!({
            "floor": role_json(&l.floor),
            "tones": role_json(&l.tones),
        })),
        "feasibility": v.feasibility.as_ref().map_or(Value::Null, |(verdict, message)| {
            json!({"verdict": verdict, "message": message})
        }),
        "end": v.end.as_ref().map_or(Value::Null, end_json),
    });
    if let (Some(r), Value::Object(o)) = (&v.record, &mut m) {
        o.insert("record".into(), r.to_json());
    }
    m
}

/// Any part outside its alarm band.
pub fn alarm(v: &MixedView) -> bool {
    v.status
        .as_ref()
        .is_some_and(|s| s.parts.iter().any(|r| r.alarm) || s.floor.alarm_lines > 0)
}

/// Any part held down by the ceiling, a limit channel or a cap.
pub fn limiting(v: &MixedView) -> bool {
    v.status
        .as_ref()
        .is_some_and(|s| s.parts.iter().any(|r| r.limiting) || s.totals.clamped)
}

/// What `mixed.state` events are sent on a change of.
pub fn state_key(v: Option<&MixedView>) -> String {
    match v {
        None => "unconfigured".into(),
        Some(v) => {
            let parts: String = rows(v)
                .iter()
                .map(|r| match r {
                    Some(r) => format!("{}{}{}", playing(r) as u8, r.alarm as u8, r.settled as u8),
                    None => "---".into(),
                })
                .collect();
            format!(
                "{}|{}|{}|{}|{:?}|{}|{}|{}|{}|{}|{}",
                v.run_id,
                v.state.name(),
                v.phase,
                crate::random::view::mode_name(v.mode),
                v.level_db.map(|x| round(x, 2)),
                v.held,
                alarm(v),
                limiting(v),
                v.end.is_some(),
                parts,
                v.record.as_ref().map_or(Value::Null, |r| r.to_json()),
            )
        }
    }
}

/// The trip object of a controller trip, naming its part (schema/PROTOCOL.md,
/// the trip table). `at_hz` is the part's frequency then.
pub fn trip_json(t: &MixedTrip, level_db: f64, at_hz: Option<f64>) -> Value {
    let mut m = Map::new();
    let (kind, facts): (&str, Value) = match &t.kind {
        MixedTripKind::FloorAbort { lines_out } => (
            "envelope",
            json!({"abort_lines": lines_out, "worst_db": null}),
        ),
        MixedTripKind::FloorRmsAbort { error_db } => (
            "envelope",
            json!({"abort_lines": 0, "worst_db": num(round(*error_db, 2))}),
        ),
        MixedTripKind::ToneAbort { error_db, .. } => (
            "abort_band",
            json!({"side": if *error_db >= 0.0 { "high" } else { "low" }, "measured_g": null, "limit_g": null, "error_db": num(round(*error_db, 2))}),
        ),
        MixedTripKind::BandAbort { error_db } => (
            "band_abort",
            json!({"side": if *error_db >= 0.0 { "high" } else { "low" }, "measured_g2_hz": null, "limit_g2_hz": null, "dof": null, "error_db": num(round(*error_db, 2))}),
        ),
        MixedTripKind::DriveVpk {
            realized_vpk,
            limit_vpk,
        } => (
            "drive_vpk",
            json!({"vpk": num(sig(*realized_vpk, 4)), "abort_vpk": num(*limit_vpk)}),
        ),
        MixedTripKind::Kinematic {
            channel_index,
            axis,
            value,
            limit,
        } => (
            "kinematic",
            json!({"channel": channel_index, "axis": axis, "value": num(sig(*value, 4)), "limit": num(sig(*limit, 4))}),
        ),
        MixedTripKind::LimitChannel {
            channel_index,
            measured,
            limit,
        } => (
            "watchdog",
            json!({"channel": channel_index, "axis": "accel", "value": num(sig(*measured, 4)), "limit": num(sig(*limit, 4)), "measured": num(sig(*measured, 4)), "drive_vpk": null}),
        ),
        MixedTripKind::Overload { channel_index } => {
            ("overload", json!({"channel": channel_index}))
        }
        MixedTripKind::RandomVrms {
            realized_vrms,
            limit_vrms,
        } => (
            "random_vrms",
            json!({"realized_vrms": num(sig(*realized_vrms, 4)), "limit_vrms": num(sig(*limit_vrms, 4))}),
        ),
        MixedTripKind::Divergence => ("divergence", json!({})),
        MixedTripKind::LawFault { role, reason } => (
            "law_fault",
            json!({"role": match role { LawRole::Floor => "floor", LawRole::Tones => "tones" }, "reason": reason, "consecutive": null}),
        ),
        MixedTripKind::Station { message } => ("station", json!({"message": message})),
    };
    m.insert("kind".into(), Value::from(kind));
    m.insert(
        "part".into(),
        t.part.as_deref().map_or(Value::Null, Value::from),
    );
    m.insert("at_level_db".into(), num(round(level_db, 2)));
    m.insert("at_s".into(), num(round(t.t_s, 3)));
    m.insert("at_loop".into(), Value::Null);
    m.insert("at_tick".into(), Value::Null);
    m.insert("at_hz".into(), opt_num(at_hz.map(|x| round(x, 2))));
    if let Value::Object(f) = facts {
        m.extend(f);
    }
    Value::Object(m)
}

/// The numbers every live frame carries, `mixed.live` and `mixed.data` alike.
pub fn live_head(v: &MixedView) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("run_id".into(), Value::from(v.run_id.as_str()));
    m.insert("source".into(), Value::from(v.station.source()));
    m.insert("class".into(), Value::from(v.station.class));
    m.insert("state".into(), Value::from(v.state.name()));
    m.insert("phase".into(), Value::from(v.phase));
    m.insert(
        "mode".into(),
        Value::from(crate::random::view::mode_name(v.mode)),
    );
    m.insert("t_s".into(), opt_num(v.t_s.map(|x| round(x, 3))));
    m.insert(
        "elapsed_s".into(),
        opt_num(v.elapsed_s.map(|x| round(x, 3))),
    );
    m.insert("parts_s".into(), opt_num(v.parts_s.map(|x| round(x, 3))));
    m.insert(
        "level_db".into(),
        opt_num(v.achieved_db.map(|x| round(x, 2))),
    );
    m.insert(
        "commanded_db".into(),
        opt_num(v.level_db.map(|x| round(x, 2))),
    );
    m.insert(
        "at_level_s".into(),
        opt_num(v.at_level_s.map(|x| round(x, 3))),
    );
    m.insert(
        "remaining_at_level_s".into(),
        opt_num(v.remaining_at_level_s.map(|x| round(x, 3))),
    );
    m.insert("level_s".into(), opt_num(v.level_s.map(|x| round(x, 3))));
    m.insert("loop".into(), Value::from(v.loop_n));
    m.insert("tick".into(), Value::from(v.tick_n));
    m.insert("held".into(), Value::from(v.held));
    m.insert("alarm".into(), Value::from(alarm(v)));
    m.insert("limiting".into(), Value::from(limiting(v)));
    m.insert("parts".into(), parts_json(v));
    m.insert("total".into(), total_json(v));
    m.insert(
        "channels".into(),
        Value::Array(
            v.channels
                .iter()
                .map(|c| {
                    json!({
                        "index": c.index,
                        "description": c.description,
                        "role": c.role,
                        "rms": opt_num(c.rms.map(|x| sig(x, 4))),
                        "unit": c.unit,
                        "overload": c.overload,
                    })
                })
                .collect(),
        ),
    );
    m.insert("alarms".into(), alarms_json(v));
    m.insert(
        "law".into(),
        match &v.law {
            Some(l) if l.built => law_frame_json(v, l),
            _ => Value::Null,
        },
    );
    m
}

fn values_json(vals: &[(String, f64)]) -> Value {
    let mut m = Map::new();
    for (k, x) in vals {
        m.insert(k.clone(), num(*x));
    }
    Value::Object(m)
}

fn law_frame_json(v: &MixedView, l: &LawState) -> Value {
    let tones: Vec<Value> = v
        .parts
        .iter()
        .zip(rows(v))
        .filter(|(f, _)| f.kind == "tone")
        .map(|(f, r)| {
            let vals = l
                .tone_values
                .iter()
                .find(|(id, _)| id == &f.id)
                .map(|(_, v)| v.as_slice())
                .unwrap_or(&[]);
            json!({
                "part": f.id,
                "values": values_json(vals),
                "misses": r.map_or(0, |r| r.misses),
                "capped": r.map_or(0, |r| r.capped),
            })
        })
        .collect();
    json!({
        "floor": {
            "values": values_json(&l.floor_values),
            "misses": l.floor.misses,
            "capped": l.floor.capped,
        },
        "tones": tones,
    })
}

/// The floor's worst clean line against its reference, dB (`10 log10` of the
/// residual estimate over the reference, both at the level in force), the line
/// furthest from 0 dB; `None` without a residual and a reference.
pub fn envelope_worst_db(d: &MixedDisplay) -> Option<f64> {
    let n = d.residual.len();
    if n == 0 || d.reference.len() != n {
        return None;
    }
    (0..n)
        .filter(|&k| !d.excluded.get(k).copied().unwrap_or(false))
        .filter_map(|k| {
            let (m, r) = (f64::from(d.residual[k]), f64::from(d.reference[k]));
            (m > 0.0 && r > 0.0 && m.is_finite() && r.is_finite()).then(|| 10.0 * (m / r).log10())
        })
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
}

/// The facts a trip's own record leaves out, filled from what the core reports
/// beside it: a tone's or a narrowband's measured value and the abort limit it
/// crossed (from its row's target at the level in force, the trip's error and the
/// part's abort band), a band's degrees of freedom, and an envelope abort's worst
/// line (from the last display). A fact with nothing to fill it from stays null.
pub fn fill_trip_facts(
    v: &mut Value,
    t: &MixedTrip,
    row: Option<&PartRow>,
    core: &hertzforge_mixed::MixedConfig,
    display: Option<&MixedDisplay>,
) {
    let abort_band = |id: &str| -> Option<(f64, f64)> {
        core.parts
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| match &p.body {
                hertzforge_mixed::parts::PartBody::Tone(x) => Some(x.abort_db),
                hertzforge_mixed::parts::PartBody::Narrowband(x) => Some(x.abort_db),
                _ => None,
            })
    };
    let target = row.map(|r| r.target).filter(|x| x.is_finite() && *x > 0.0);
    // A tone is judged against its judged abort band (its own, or widened to its
    // reading's scatter); a band against its configured one.
    let band = row
        .and_then(|r| r.judged_abort_db)
        .or_else(|| t.part.as_deref().and_then(abort_band));
    let limit = |e: f64, per_db: f64| {
        band.zip(target).map(|((lo, hi), tg)| {
            let db = if e >= 0.0 { hi } else { -lo };
            tg * 10f64.powf(db / per_db)
        })
    };
    match &t.kind {
        MixedTripKind::ToneAbort { error_db, .. } => {
            v["measured_g"] = opt_num(target.map(|tg| sig(tg * 10f64.powf(error_db / 20.0), 4)));
            v["limit_g"] = opt_num(limit(*error_db, 20.0).map(|x| sig(x, 4)));
        }
        MixedTripKind::BandAbort { error_db } => {
            v["measured_g2_hz"] =
                opt_num(target.map(|tg| sig(tg * 10f64.powf(error_db / 10.0), 4)));
            v["limit_g2_hz"] = opt_num(limit(*error_db, 10.0).map(|x| sig(x, 4)));
            v["dof"] = opt_num(row.and_then(|r| r.dof).map(|x| round(x, 1)));
        }
        MixedTripKind::FloorAbort { .. } => {
            v["worst_db"] = opt_num(display.and_then(envelope_worst_db).map(|x| round(x, 2)));
        }
        _ => {}
    }
}

/// What holds now: the floor's envelope, each part's band, overloads.
pub fn alarms_json(v: &MixedView) -> Value {
    let mut out = Vec::new();
    if let Some(s) = &v.status {
        let floor = v.parts.iter().find(|f| f.kind == "floor");
        if s.floor.alarm_lines > 0 {
            out.push(json!({
                "kind": "envelope",
                "part": floor.map(|f| f.id.as_str()),
                "lines": s.floor.alarm_lines,
                "worst_db": opt_num(v.display.as_deref().and_then(envelope_worst_db).map(|x| round(x, 2))),
            }));
        }
        for (f, r) in v.parts.iter().zip(rows(v)) {
            let Some(r) = r else { continue };
            if f.kind != "floor" && r.alarm {
                let e = r.error_db.unwrap_or(0.0);
                out.push(json!({
                    "kind": "band",
                    "part": f.id,
                    "side": if e >= 0.0 { "high" } else { "low" },
                    "error_db": num(round(e, 2)),
                }));
            }
        }
    }
    for c in v.channels.iter().filter(|c| c.overload) {
        out.push(json!({"kind": "overload", "channel": c.index}));
    }
    Value::Array(out)
}

/// The lines of `grid` a band `(lo, hi)` Hz covers, as Random's frames take them:
/// every line whose center rounds into `[lo, hi]`, never the 0 Hz line, at least
/// one. The whole grid without a band.
pub fn band_lines(
    grid: &hertzforge_mixed::LineGrid,
    band: Option<(f64, f64)>,
) -> std::ops::Range<usize> {
    let n = grid.lines;
    let Some((lo, hi)) = band.filter(|(lo, hi)| lo.is_finite() && hi.is_finite() && hi > lo) else {
        return 0..n;
    };
    if n == 0 || grid.df_hz.is_nan() || grid.df_hz <= 0.0 {
        return 0..n;
    }
    let at = |f: f64| ((f - grid.f0_hz) / grid.df_hz).round();
    let dc = usize::from(grid.f0_hz <= 0.0);
    let i0 = (at(lo).max(0.0) as usize).max(dc).min(n - 1);
    let i1 = ((at(hi).max(0.0) as usize) + 1).min(n).max(i0 + 1);
    i0..i1
}

/// `d` held to the floor's band: every per-line spectrum, the lines left out
/// and the identification cut to [`band_lines`], and the grid moved to match,
/// so every view and plot drawn from it carries the band's lines only, as
/// Random's frames do. The histories are not per line and are kept whole.
pub fn on_band(mut d: MixedDisplay, band: Option<(f64, f64)>) -> MixedDisplay {
    let n = d.grid.lines;
    let r = band_lines(&d.grid, band);
    if r == (0..n) {
        return d;
    }
    fn cut<T: Copy>(v: &mut Vec<T>, n: usize, r: &std::ops::Range<usize>) {
        if v.len() == n {
            *v = v[r.clone()].to_vec();
        }
    }
    for v in [
        &mut d.control,
        &mut d.residual,
        &mut d.reference,
        &mut d.alarm_lo,
        &mut d.alarm_hi,
        &mut d.abort_lo,
        &mut d.abort_hi,
        &mut d.drive,
    ] {
        cut(v, n, &r);
    }
    for c in &mut d.channels {
        cut(&mut c.control, n, &r);
        cut(&mut c.residual, n, &r);
    }
    cut(&mut d.excluded, n, &r);
    if let Some(id) = &mut d.identification {
        cut(&mut id.h_mag2, n, &r);
        cut(&mut id.coherence, n, &r);
        cut(&mut id.trusted, n, &r);
    }
    d.grid.f0_hz += r.start as f64 * d.grid.df_hz;
    d.grid.lines = r.len();
    d
}

/// The line centers of the display's grid.
fn centers(d: &MixedDisplay) -> Vec<f64> {
    (0..d.grid.lines)
        .map(|k| d.grid.f0_hz + k as f64 * d.grid.df_hz)
        .collect()
}

fn stepped(name: &str, role: Role, d: &MixedDisplay, y: &[f32]) -> Series {
    let mut s = Series::new(
        name,
        role,
        centers(d),
        y.iter().map(|x| f64::from(*x)).collect(),
    );
    s.edges = Some(d.grid.edges_hz());
    s
}

/// The lines left out of the floor's average as `cut` regions, each naming the
/// part whose frequency lies in it.
fn cut_regions(v: &MixedView, d: &MixedDisplay) -> Value {
    let edges = d.grid.edges_hz();
    let mut out = Vec::new();
    let mut k = 0;
    while k < d.excluded.len() {
        if !d.excluded[k] {
            k += 1;
            continue;
        }
        let start = k;
        while k < d.excluded.len() && d.excluded[k] {
            k += 1;
        }
        let (x0, x1) = (edges[start], edges[k.min(edges.len() - 1)]);
        let name = v
            .parts
            .iter()
            .zip(rows(v))
            .find(|(_, r)| {
                r.and_then(|r| r.freq_hz)
                    .is_some_and(|f| f >= x0 && f <= x1)
            })
            .map(|(f, _)| f.name.clone());
        out.push(json!({
            "x0": num(round(x0, 6)),
            "x1": num(round(x1, 6)),
            "kind": "cut",
            "label": match name { Some(n) => format!("Left out: {n}"), None => "Left out".into() },
        }));
    }
    Value::Array(out)
}

fn psd_axes() -> (Axis, Axis) {
    (
        Axis::new("Frequency", Some("Hz"), Scale::Log),
        Axis::new("PSD", Some("g²/Hz"), Scale::Log),
    )
}

fn reference_series(d: &MixedDisplay) -> Vec<Series> {
    let mut s = Vec::new();
    if !d.reference.is_empty() {
        s.push(stepped("Reference", Role::Reference, d, &d.reference));
    }
    for (name, role, y) in [
        ("Alarm high", Role::Tolerance, &d.alarm_hi),
        ("Alarm low", Role::Tolerance, &d.alarm_lo),
        ("Abort high", Role::Abort, &d.abort_hi),
        ("Abort low", Role::Abort, &d.abort_lo),
    ] {
        if !y.is_empty() {
            s.push(stepped(name, role, d, y));
        }
    }
    s
}

fn spectrum_with_regions(
    v: &MixedView,
    d: &MixedDisplay,
    title: &str,
    first: (&str, Role, &[f32]),
    arrays: &mut Arrays,
) -> Value {
    let (x, y) = psd_axes();
    let mut series = Vec::new();
    if !first.2.is_empty() {
        series.push(stepped(first.0, first.1, d, first.2));
    }
    series.extend(reference_series(d));
    let mut view = views::spectrum(arrays, title, &x, &y, series, Some("frequency"));
    view["regions"] = cut_regions(v, d);
    view
}

/// The **Control** view.
pub fn control_view(v: &MixedView, d: &MixedDisplay, arrays: &mut Arrays) -> Value {
    spectrum_with_regions(
        v,
        d,
        "Control",
        ("Control", Role::Measured, &d.control),
        arrays,
    )
}

/// The **Residual** view.
pub fn residual_view(v: &MixedView, d: &MixedDisplay, arrays: &mut Arrays) -> Value {
    spectrum_with_regions(
        v,
        d,
        "Residual",
        ("Residual", Role::Measured, &d.residual),
        arrays,
    )
}

/// The **Drive** view: the random part's drive spectrum.
pub fn drive_view(d: &MixedDisplay, arrays: &mut Arrays) -> Value {
    let x = Axis::new("Frequency", Some("Hz"), Scale::Log);
    let y = Axis::new("PSD", Some("V²/Hz"), Scale::Log);
    let series = if d.drive.is_empty() {
        Vec::new()
    } else {
        vec![stepped("Drive", Role::Drive, d, &d.drive)]
    };
    views::spectrum(arrays, "Drive", &x, &y, series, Some("frequency"))
}

/// The **Run** reading.
pub fn run_reading(v: &MixedView) -> Value {
    let mut rs = Vec::new();
    if let Some(l) = v.achieved_db {
        rs.push(Row::number("Level", round(l, 2), Some("dB"), Status::Info));
    }
    if let Some(e) = v.elapsed_s {
        rs.push(Row::number("Elapsed", round(e, 1), Some("s"), Status::Info));
    }
    if let Some(r) = v.remaining_at_level_s {
        rs.push(Row::number(
            "Remaining",
            round(r, 1),
            Some("s"),
            Status::Info,
        ));
    }
    if let Some(t) = v.status.as_ref().and_then(|s| s.totals.control_rms_g) {
        rs.push(Row::number("Total rms", sig(t, 4), Some("g"), Status::Info));
    }
    rs.push(Row::number(
        "Reference rms",
        sig(reference_rms_at_level(v), 4),
        Some("g"),
        Status::Info,
    ));
    if let Some(s) = &v.status {
        rs.push(Row::number(
            "Drive peak",
            sig(s.totals.drive_vpk, 4),
            Some("V"),
            Status::Info,
        ));
    }
    for (f, r) in v.parts.iter().zip(rows(v)) {
        let Some(r) = r.filter(|r| playing(r)) else {
            continue;
        };
        let unit = if f.kind == "narrowband" {
            "g²/Hz"
        } else {
            "g"
        };
        let status = if r.alarm { Status::Warn } else { Status::Pass };
        if let Some(fr) = r.freq_hz {
            rs.push(Row::number(
                &format!("{}: Frequency", f.name),
                round(fr, 2),
                Some("Hz"),
                Status::Info,
            ));
        }
        let (target, measured, error) = if f.kind == "floor" {
            let fl = v.status.as_ref().map(|s| s.floor);
            (
                fl.map(|x| x.rms_target_g),
                fl.and_then(|x| x.rms_g),
                fl.and_then(|x| x.error_db_rms),
            )
        } else {
            (Some(r.target), r.measured, r.error_db)
        };
        if let Some(m) = measured {
            rs.push(Row::number(
                &format!("{}: Measured", f.name),
                sig(m, 4),
                Some(unit),
                status,
            ));
        }
        if let Some(t) = target {
            rs.push(Row::number(
                &format!("{}: Target", f.name),
                sig(t, 4),
                Some(unit),
                Status::Info,
            ));
        }
        if let Some(e) = error {
            rs.push(Row::number(
                &format!("{}: Error", f.name),
                round(e, 2),
                Some("dB"),
                status,
            ));
        }
    }
    views::reading("Run", rs)
}

/// The views of a live frame, in the contract's order.
pub fn live_views(v: &MixedView, arrays: &mut Arrays) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(d) = &v.display {
        out.push(control_view(v, d, arrays));
        out.push(residual_view(v, d, arrays));
        out.push(drive_view(d, arrays));
    }
    out.push(run_reading(v));
    out
}

/// `mixed.live`.
pub fn live_json(v: &MixedView, arrays: &mut Arrays) -> Value {
    let mut m = live_head(v);
    m.insert("views".into(), Value::Array(live_views(v, arrays)));
    Value::Object(m)
}

/// A run's view as configured: nothing open, every part's row from the
/// configuration.
pub fn configured_view(
    run_id: &str,
    w: &WireMixed,
    grid: Grid,
    reference_rms_g: f64,
    station: StationRef,
    law: Option<LawState>,
    feasibility: Option<(&'static str, String)>,
) -> MixedView {
    MixedView {
        run_id: run_id.to_owned(),
        title: w.title.clone(),
        state: State::Configured,
        phase: "idle",
        station,
        grid,
        clock: None,
        mode: w.random.mode,
        levels_db: w.random.levels.iter().map(|l| l.0).collect(),
        level_db: None,
        achieved_db: None,
        full_level_s: w.random.full_level_s,
        t_s: None,
        elapsed_s: None,
        parts_s: None,
        at_level_s: None,
        remaining_at_level_s: None,
        level_s: None,
        loop_n: 0,
        tick_n: 0,
        held: false,
        parts: part_facts(w),
        status: None,
        reference_rms_g,
        identification: None,
        law,
        feasibility,
        end: None,
        channels: Vec::new(),
        streaming: false,
        display: None,
        record: None,
    }
}

/// The wire phase of the controller's lifecycle state and start-up phase.
pub fn wire_state(s: &MixedStatus) -> (State, &'static str) {
    use hertzforge_mixed::{MixedPhase as P, MixedState as S};
    match (s.state, s.phase) {
        (S::Idle, _) => (State::Armed, "armed"),
        (S::SysId, _) => (State::Running, "sysid"),
        (S::CoherenceGate, _) => (State::Running, "coherence_gate"),
        (S::CoherenceHold, _) => (State::Finished, "coherence_hold"),
        (S::AwaitingReview, _) => (State::Review, "awaiting_review"),
        (_, P::RampingDown) if !s.state.is_terminal() => (State::Stopping, "ramping_down"),
        (S::Engaged, P::FloorOnly) => (State::Running, "floor_only"),
        (S::Engaged, _) => (State::Running, "engaged"),
        (S::FullLevel, _) => (State::Running, "full_level"),
        (S::Stopped, _) => (State::Finished, "stopped"),
        (S::Aborted, _) => (State::Finished, "aborted"),
    }
}

/// Take the controller's status into the view: the state and phase, the rows,
/// the level and the clocks.
pub fn apply_status(v: &mut MixedView, s: &MixedStatus) {
    let (state, phase) = wire_state(s);
    v.state = state;
    v.phase = phase;
    v.level_db = Some(s.level_db);
    v.achieved_db = Some(s.level_db);
    v.mode = s.run_mode;
    v.held = s.held;
    v.t_s = Some(s.t_s);
    v.parts_s = (s.phase != hertzforge_mixed::MixedPhase::FloorOnly
        && s.phase != hertzforge_mixed::MixedPhase::Idle)
        .then_some(s.test_s);
    v.status = Some(s.clone());
}
