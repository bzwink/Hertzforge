//! The `mimo_random.status` object, the prediction and the report (schema/PROTOCOL.md,
//! "MIMO Random Run State" and "MIMO Random Report"). Keys are written in the
//! contract's order.

use super::check::sig;
use super::driver::{Gates, LawRef, MimoReport, MimoStatus, OverrideAck, Prediction};
use super::setup::{shape_name, ControlSpace, MimoSetup};
use crate::protocol::{num, opt_num};
use crate::random::config::Grid;
use crate::random::view::{mode_name, End, StationRef};
use serde_json::{json, Map, Value};

/// What the service itself knows of a run, beside the driver's status.
#[derive(Debug, Clone)]
pub struct RunFacts<'a> {
    pub run_id: &'a str,
    pub setup: &'a MimoSetup,
    pub station: &'a StationRef,
    pub grid: &'a Grid,
    pub clock: Option<&'static str>,
}

fn r(x: f64, d: i32) -> Value {
    num(crate::random::config::round(x, d))
}

fn list(xs: &[f64], d: i32) -> Value {
    Value::Array(xs.iter().map(|x| r(*x, d)).collect())
}

pub fn law_json(l: &LawRef) -> Value {
    json!({
        "id": l.id, "name": l.name, "version": l.version,
        "sha256": if l.sha256.is_empty() { Value::Null } else { Value::from(l.sha256.as_str()) },
    })
}

fn gates_json(g: &Option<Gates>) -> Value {
    g.as_ref().map_or(Value::Null, |g| {
        json!({
            "coherence_min": r(g.coherence_min, 3), "coherence_floor": num(g.coherence_floor),
            "condition_max": num(sig(g.condition_max, 4)), "condition_limit": num(g.condition_limit),
            "passed": g.passed(),
            "coherence": list(&g.coherence, 6),
        })
    })
}

fn end_json(e: &Option<End>) -> Value {
    e.as_ref().map_or(Value::Null, |e| {
        json!({
            "outcome": e.outcome, "message": e.message, "at_s": opt_num(e.at_s.map(|x| crate::random::config::round(x, 2))),
            "level_db": opt_num(e.level_db), "trip": e.trip.clone().unwrap_or(Value::Null),
        })
    })
}

fn ack_json(a: &OverrideAck) -> Value {
    json!({"by": a.by, "text": a.text, "reason": a.reason.name()})
}

/// The latest acknowledgement (null when none).
fn override_json(o: &[OverrideAck]) -> Value {
    o.last().map_or(Value::Null, ack_json)
}

/// Every acknowledgement, in order.
fn overrides_json(o: &[OverrideAck]) -> Value {
    Value::Array(o.iter().map(ack_json).collect())
}

/// The prediction object.
pub fn prediction_json(p: &Prediction) -> Value {
    json!({
        "iteration": p.iteration,
        "law": law_json(&p.law),
        "drive_rms_v": list(&p.drive_rms_v, 4),
        "ceiling_vrms": list(&p.ceiling_vrms, 4),
        "error_db": list(&p.error_db, 2),
        "drives_pass": p.drives_pass(),
        "has_achievable": p.has_achievable,
        "unreachable_db": list(&p.unreachable_db, 2),
        "drive_accel": list(&p.drive_accel, 4),
        "accel_control": p.accel_control,
        "feasibility": p.feasibility.iter().map(|v| json!({"verdict": v.verdict, "message": v.message})).collect::<Vec<_>>(),
    })
}

fn grid_json(g: &Grid) -> Value {
    json!({
        "fs_hz": num(g.fs_hz), "n_pts": g.n_pts, "df_hz": num(g.df_hz), "lines": g.lines,
        "loop_s": r(g.loop_s, 3), "actual": g.actual,
    })
}

fn shape_json(s: &MimoSetup) -> Value {
    json!({
        "controls": s.controls.len(),
        "drives": s.drives.len(),
        "space": match s.space { ControlSpace::Physical => "physical", ControlSpace::Virtual { .. } => "virtual" },
        "dofs": match &s.space { ControlSpace::Physical => Value::Null, ControlSpace::Virtual { dofs, .. } => Value::from(dofs.clone()) },
        "shape": shape_name(s.shape()),
        "labels": s.controlled_labels(),
        "drive_labels": s.drives.iter().map(|d| d.label.clone()).collect::<Vec<_>>(),
        "bands": (0..s.controlled()).map(|i| {
            let (a, b) = s.band_of(i);
            json!({"alarm_db": [num(a[0]), num(a[1])], "abort_db": [num(b[0]), num(b[1])]})
        }).collect::<Vec<_>>(),
    })
}

/// The `mimo_random.status` object; `None` is the service with no MIMO setup.
pub fn status_json(f: Option<(&RunFacts<'_>, &MimoStatus)>) -> Value {
    let Some((f, s)) = f else {
        return json!({
            "run_id": null, "state": "unconfigured", "phase": "idle", "title": null,
            "source": null, "station": null, "grid": null, "clock": null, "shape": null,
            "mode": null, "levels_db": null, "level_db": null, "elapsed_s": null,
            "at_level_s": null, "remaining_at_level_s": null, "loop": 0, "alarm": false,
            "control_error_db": [], "gates": null, "caps": [], "drive_rms_v": [], "law": null,
            "faults": [], "override": null, "overrides": [], "prediction": null, "excluded": [], "end": null,
            "record": null,
        });
    };
    let mut m = Map::new();
    m.insert("run_id".into(), Value::from(f.run_id));
    m.insert("state".into(), Value::from(s.state.name()));
    m.insert("phase".into(), Value::from(s.phase));
    m.insert("title".into(), Value::from(f.setup.title.as_str()));
    m.insert("source".into(), Value::from(f.station.source()));
    m.insert(
        "station".into(),
        json!({
            "kind": f.station.kind, "class": f.station.class,
            "devices": f.station.devices.iter().map(|(id, sn)| json!({"id": id, "serial": sn})).collect::<Vec<_>>(),
        }),
    );
    m.insert("grid".into(), grid_json(f.grid));
    m.insert("clock".into(), f.clock.map_or(Value::Null, Value::from));
    m.insert("shape".into(), shape_json(f.setup));
    m.insert("mode".into(), Value::from(mode_name(f.setup.schedule.mode)));
    m.insert(
        "levels_db".into(),
        Value::Array(f.setup.schedule.levels.iter().map(|l| num(l.0)).collect()),
    );
    m.insert("level_db".into(), opt_num(s.level_db));
    m.insert(
        "elapsed_s".into(),
        s.elapsed_s.map_or(Value::Null, |x| r(x, 2)),
    );
    m.insert(
        "at_level_s".into(),
        s.at_level_s.map_or(Value::Null, |x| r(x, 2)),
    );
    m.insert(
        "remaining_at_level_s".into(),
        s.remaining_at_level_s.map_or(Value::Null, |x| r(x, 2)),
    );
    m.insert("loop".into(), Value::from(s.loop_index));
    m.insert("alarm".into(), Value::from(s.alarm));
    m.insert("control_error_db".into(), list(&s.control_error_db, 2));
    m.insert("gates".into(), gates_json(&s.gates));
    m.insert("caps".into(), list(&s.caps, 3));
    m.insert("drive_rms_v".into(), list(&s.drive_rms_v, 4));
    m.insert("law".into(), law_json(&s.law));
    m.insert(
        "faults".into(),
        Value::Array(
            s.faults
                .iter()
                .map(|x| json!({"kind": x.kind, "message": x.message}))
                .collect(),
        ),
    );
    m.insert("override".into(), override_json(&s.override_ack));
    m.insert("overrides".into(), overrides_json(&s.override_ack));
    m.insert(
        "prediction".into(),
        s.prediction.as_ref().map_or(Value::Null, prediction_json),
    );
    m.insert("excluded".into(), Value::from(s.excluded.clone()));
    m.insert("end".into(), end_json(&s.end));
    m.insert(
        "record".into(),
        s.record.as_ref().map_or(Value::Null, |r| r.to_json()),
    );
    Value::Object(m)
}

/// What the status shows that a screen redraws on: a change in it is a
/// `mimo_random.status` event.
pub fn status_key(s: Option<&MimoStatus>) -> String {
    match s {
        None => "unconfigured".into(),
        Some(s) => format!(
            "{}|{}|{:?}|{}|{}|{}|{}|{}|{:?}",
            s.state.name(),
            s.phase,
            s.level_db,
            s.loop_index,
            s.alarm,
            s.faults.len(),
            s.prediction.as_ref().map_or(0, |p| p.iteration),
            s.end.is_some(),
            s.record.as_ref().map(|r| (&r.folder, &r.problem))
        ),
    }
}

/// The `mimo_random.report` object.
pub fn report_json(f: &RunFacts<'_>, rep: &MimoReport) -> Value {
    let labels = f.setup.controlled_labels();
    json!({
        "run_id": f.run_id,
        "title": f.setup.title,
        "source": f.station.source(),
        "grid": grid_json(f.grid),
        "shape": shape_json(f.setup),
        "law": law_json(&rep.law),
        "loops": rep.loops,
        "controls": labels.iter().enumerate().map(|(i, l)| json!({
            "label": l,
            "rms": rep.control_rms.get(i).map_or(Value::Null, |x| num(sig(*x, 4))),
            "reference_rms": rep.reference_rms.get(i).map_or(Value::Null, |x| num(sig(*x, 4))),
            "error_db": rep.control_error_db.get(i).map_or(Value::Null, |x| r(*x, 2)),
        })).collect::<Vec<_>>(),
        "drives": f.setup.drives.iter().enumerate().map(|(k, d)| json!({
            "channel": d.channel, "label": d.label,
            "rms_v": rep.drive_rms_v.get(k).map_or(Value::Null, |x| r(*x, 4)),
            "ceiling_vrms": num(d.ceiling_vrms),
            "min_cap": rep.min_cap.get(k).map_or(Value::Null, |x| r(*x, 3)),
        })).collect::<Vec<_>>(),
        "gates": gates_json(&rep.gates),
        "override": override_json(&rep.override_ack),
        "overrides": overrides_json(&rep.override_ack),
        "end": end_json(&rep.end),
    })
}
