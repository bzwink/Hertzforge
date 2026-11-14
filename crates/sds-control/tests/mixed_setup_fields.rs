//! Every Mixed Mode wire field reaches the controller's configuration, or is held
//! by the service where the core has no place for it, and comes back: for each
//! field the golden configuration is changed to another valid value, and the
//! change must show in the core's `MixedConfig` (or in the service's own record of
//! that field) and must survive the trip back to the wire and in again.

use hertzforge_channel_table::{ChannelRole, ChannelTable, Units};
use sds_control::mixed::config::{self, WireMixed};
use serde_json::{json, Value};
use std::path::PathBuf;

fn golden_config() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema/golden/request_mixed_configure.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["params"]["config"].clone()
}

fn table() -> ChannelTable {
    let mut t = sds_control::table::Table::new().table;
    let mut c1 = sds_control::table::new_row(1);
    c1.role = ChannelRole::Control;
    let mut c2 = sds_control::table::new_row(2);
    c2.role = ChannelRole::Drive;
    c2.units = Units::VoltageV;
    let mut c3 = sds_control::table::new_row(3);
    c3.role = ChannelRole::Monitor;
    let mut c4 = sds_control::table::new_row(4);
    c4.role = ChannelRole::Control;
    t.channels = vec![c1, c2, c3, c4];
    t
}

/// Set `value` at a dotted path with `[i]` indexes.
fn set(v: &mut Value, path: &str, value: Value) {
    let mut at = v;
    let parts: Vec<&str> = path.split('.').collect();
    for (n, p) in parts.iter().enumerate() {
        let (key, index) = match p.find('[') {
            Some(b) => (
                &p[..b],
                Some(p[b + 1..p.len() - 1].parse::<usize>().unwrap()),
            ),
            None => (*p, None),
        };
        let last = n + 1 == parts.len();
        if last && index.is_none() {
            at[key] = value;
            return;
        }
        at = &mut at[key];
        if let Some(i) = index {
            if last {
                at[i] = value;
                return;
            }
            at = &mut at[i];
        }
    }
}

/// What the service holds of a field the core has no place for.
fn held(w: &WireMixed) -> Value {
    let label = w
        .parts
        .iter()
        .find_map(|p| match &p.body {
            config::WireBody::Tone {
                amplitude: config::WireAmplitude::Profile(pr),
                ..
            } => Some(pr.label.clone()),
            _ => None,
        })
        .unwrap_or_default();
    json!({
        "title": w.title,
        "fs_hz": w.random.fs_hz,
        "damping": w.random.control.damping,
        "averages": w.random.control.pretest_averages,
        "fault_debounce_ticks": w.guards_tones.fault_debounce_ticks,
        "operator": w.operator,
        "article": w.article,
        "notes": w.notes,
        "specification": w.specification,
        "record_streams": w.record_streams,
        "profile_label": label,
    })
}

/// Every wire field and another valid value for it.
fn mutations() -> Vec<(&'static str, Value)> {
    vec![
        ("title", json!("Another title")),
        ("parts[1].name", json!("Blade")),
        ("parts[1].enabled", json!(false)),
        ("parts[1].window", json!({"on_s": 5, "off_s": 100})),
        ("parts[1].schedule.freq_hz", json!(130)),
        ("parts[1].schedule.end", json!({"seconds": 60})),
        ("parts[1].amplitude_g", json!(1.5)),
        ("parts[1].alarm_db", json!([1, 2])),
        ("parts[1].abort_db", json!([2.5, 3.5])),
        ("parts[2].schedule.band_hz", json!([310, 600])),
        (
            "parts[2].schedule.rate",
            json!({"law": "log", "oct_per_min": 0.5}),
        ),
        ("parts[2].schedule.start_hz", json!(400)),
        ("parts[2].schedule.direction", json!("down")),
        (
            "parts[2].schedule.end",
            json!({"sweeps": 2, "duration_s": 600}),
        ),
        ("parts[2].profile.label", json!("Rotor line")),
        (
            "parts[2].profile.rows[1]",
            json!({"freq_hz": 600, "quantity": "accel_g", "value": 1.25}),
        ),
        ("parts[3].schedule.ratio", json!(2.5)),
        ("parts[4].schedule.freq_hz", json!(260)),
        ("parts[4].level_g2_hz", json!(0.25)),
        ("parts[4].width_hz", json!(16)),
        ("parts[4].alarm_db", json!([2, 2])),
        ("parts[4].abort_db", json!([5, 5])),
        ("parts[5].width_share", json!(0.04)),
        (
            "parts[5].schedule.rate",
            json!({"law": "linear", "hz_per_s": 3}),
        ),
        (
            "parts[5].schedule.end",
            json!({"sweeps": null, "duration_s": 90}),
        ),
        ("parts[0].breakpoints[0].psd_g2_hz", json!(0.02)),
        ("parts[0].breakpoints[1].slope_db_oct", json!(4)),
        ("parts[0].alarm_db", json!([2.5, 2.5])),
        ("parts[0].abort_db", json!([5, 5])),
        ("exclusion_hz", json!(3)),
        ("acquisition.fs_hz", json!(10240)),
        ("acquisition.n_pts", json!(32768)),
        ("acquisition.n_avg", json!(6)),
        ("acquisition.tick_ms", json!(20)),
        ("schedule.mode", json!("manual")),
        ("schedule.levels[0].db", json!(-10)),
        ("schedule.levels[0].dwell_s", json!(20)),
        ("schedule.full_level_s", json!(200)),
        ("control.composite", json!("max")),
        ("control.weights", json!([{"channel": 1, "weight": 2}])),
        ("control.control_dof", json!(150)),
        ("control.damping", json!(3)),
        ("control.max_lines_out", json!(8)),
        ("control.max_loops_out", json!(4)),
        ("control.max_sigma", json!(4)),
        ("control.drive_ceiling_vrms", json!(2)),
        ("control.drive_abort_vrms", json!(3)),
        ("control.coherence_floor", json!(0.6)),
        ("control.review_after_sysid", json!(false)),
        ("control.pretest.averages", json!(6)),
        ("control.pretest.level_db", json!(-18)),
        ("control.pretest.max_vrms", json!(1.5)),
        ("control.pretest.start_vrms", json!(0.02)),
        ("control.pretest.shaped", json!(true)),
        ("control.soft_start_db", json!(8)),
        ("control.soft_start_relax_db", json!(5)),
        ("control.level_ramp_s", json!(4)),
        ("control.shutdown_ramp_s", json!(5)),
        ("control.part_ramp_s", json!(4)),
        ("control.floor_settle_s", json!(40)),
        ("control.trust_coherence", json!(0.6)),
        ("control.trust_h_floor_frac", json!(0.1)),
        ("control.tracking", json!({"kind": "fixed", "hz": 0.75})),
        ("control.law.tones.params.kp", json!(0.25)),
        ("control.law_guards.floor.law_budget", json!(0.5)),
        ("control.law_guards.floor.law_misses", json!(4)),
        ("control.law_guards.floor.max_change_db", json!(7)),
        ("control.law_guards.tones.law_budget", json!(0.5)),
        ("control.law_guards.tones.law_misses", json!(5)),
        ("control.law_guards.tones.max_step_db", json!(2)),
        ("control.law_guards.tones.max_rise_db_per_s", json!(120)),
        ("control.law_guards.tones.divergence_s", json!(2)),
        ("control.law_guards.tones.fault_debounce_ticks", json!(40)),
        ("limits.drive_vpk_ceiling", json!(3)),
        ("limits.drive_vpk_abort", json!(5)),
        ("limits.kinematic_monitor", json!(false)),
        (
            "limits.channels[0].rows",
            json!([{"freq_hz": 20, "ceiling": 0.25}, {"freq_hz": 2000, "ceiling": 0.25}]),
        ),
        ("feasibility.crest_factor", json!(3.5)),
        ("feasibility.moving_mass_lb.fixturing", json!(12)),
        ("operator", json!("B. Operator")),
        ("article", json!("Rotor housing, serial 5")),
        ("notes", json!("second article")),
        ("specification.document", json!("Test procedure 13")),
        ("specification.clause", json!("4.4")),
        ("record.streams", json!(false)),
        ("metadata[0].value", json!("8")),
    ]
}

#[test]
fn every_wire_field_reaches_the_core_or_is_held_by_the_service() {
    let t = table();
    let base = config::parse(&golden_config()).unwrap();
    let base_core = base.to_core(&t).unwrap();
    let mut missed = Vec::new();
    for (path, value) in mutations() {
        let mut c = golden_config();
        set(&mut c, path, value);
        let w = config::parse(&c).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        let core = w.to_core(&t).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        if core == base_core && held(&w) == held(&base) {
            missed.push(path);
        }
    }
    assert!(missed.is_empty(), "these fields reach nothing: {missed:?}");
}

#[test]
fn a_field_the_service_does_not_read_is_caught_by_this_test() {
    // Falsifies the test above: a key nobody reads changes nothing.
    let t = table();
    let base = config::parse(&golden_config()).unwrap();
    let mut c = golden_config();
    set(&mut c, "control.no_such_field", json!(1));
    let w = config::parse(&c).unwrap();
    assert_eq!(w.to_core(&t).unwrap(), base.to_core(&t).unwrap());
    assert_eq!(held(&w), held(&base));
}

#[test]
fn every_field_comes_back_from_the_core_to_the_wire_and_in_again() {
    let t = table();
    let mut configs = vec![golden_config()];
    for (path, value) in mutations() {
        let mut c = golden_config();
        set(&mut c, path, value);
        configs.push(c);
    }
    for (n, c) in configs.iter().enumerate() {
        let w = config::parse(c).unwrap();
        let core = w.to_core(&t).unwrap();
        let wire = config::to_wire(&core, &w);
        let back = config::parse(&wire).unwrap_or_else(|e| panic!("config {n}: {e:?}\n{wire}"));
        let again = back.to_core(&t).unwrap();
        assert_eq!(again, core, "config {n} did not come back from the core");
        assert_eq!(
            held(&back),
            held(&w),
            "config {n}: a held field did not come back"
        );
    }
}

#[test]
fn a_sweep_with_no_end_runs_until_its_window_closes_and_comes_back_null() {
    let c = golden_config();
    let w = config::parse(&c).unwrap();
    let core = w.to_core(&table()).unwrap();
    let b2 = core.parts.iter().find(|p| p.id == "b2").unwrap();
    match b2.schedule() {
        Some(hertzforge_mixed::PartSchedule::Sweep {
            target: hertzforge_mixed::PartSweepTarget::Duration { s },
            ..
        }) => assert!(s.is_infinite()),
        other => panic!("{other:?}"),
    }
    let wire = config::to_wire(&core, &w);
    assert_eq!(wire["parts"][5]["schedule"]["end"], Value::Null);
}
