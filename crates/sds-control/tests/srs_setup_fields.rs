//! Every field of the SRS shock configuration on the wire reaches the controller's
//! `SrsTestConfig` and its `SrsControl`, and each refusal comes back by name with
//! the field it is about.
//!
//! Each case sets one wire field to a value no default has and reads it back from
//! the built configuration, so a field the conversion drops fails its own case.

use hertzforge_channel_table::{ChannelRole, ChannelTable, Units};
use hertzforge_shock_common::{DriveMonitorKind, LadderAdvance, ShockRunMode};
use hertzforge_srs::iterate::SynthesisEngine;
use hertzforge_srs::service::config::{
    BandFraction, EngineKind, LimitTarget, ShockCaptureTrigger, SysIdExcitation, TargetSrs,
};
use hertzforge_srs::SrsTestConfig;
use sds_control::srs::config::{self, CheckContext, LawFacts, WireSrs};
use sds_control::table;
use serde_json::{json, Value};
use std::path::PathBuf;

fn golden_config() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema/golden/request_srs_configure.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["params"]["config"].clone()
}

/// The golden table: control 1, a current monitor 2, the fixture 3, the drive 4.
fn table() -> ChannelTable {
    let mut t = table::Table::new().table;
    let mut c1 = table::new_row(1);
    c1.role = ChannelRole::Control;
    let mut c2 = table::new_row(2);
    c2.role = ChannelRole::Monitor;
    c2.units = Units::Custom("A".into());
    let mut c3 = table::new_row(3);
    c3.role = ChannelRole::Monitor;
    c3.description = "Fixture".into();
    let mut c4 = table::new_row(4);
    c4.role = ChannelRole::Drive;
    c4.units = Units::VoltageV;
    t.channels = vec![c1, c2, c3, c4];
    t
}

fn law(id: &str) -> Option<LawFacts> {
    match id {
        "srs_ratio" => Some(sds_control::laws::builtin_srs_facts()),
        "fd_correct" => Some(sds_control::laws::builtin_classical_facts()).map(|mut f| {
            f.world = Err("classical shock");
            f
        }),
        _ => None,
    }
}

fn set(v: &mut Value, path: &str, value: Value) {
    let mut at = v;
    let parts: Vec<&str> = path.split('.').collect();
    for p in &parts[..parts.len() - 1] {
        at = &mut at[*p];
    }
    at[parts[parts.len() - 1]] = value;
}

fn parse_with(path: &str, value: Value) -> WireSrs {
    let mut c = golden_config();
    set(&mut c, path, value);
    config::parse(&c).unwrap_or_else(|e| panic!("{path}: {}", e.message))
}

/// Check and build: the controller's configuration when the configuration is valid.
fn built(w: &WireSrs) -> SrsTestConfig {
    let t = table();
    let ctx = CheckContext {
        table: &t,
        law: &law,
        timeout_s: 30.0,
    };
    let (r, d) = w.check(&ctx);
    assert!(r.errors.is_empty() && d.is_some(), "{:?}", r.errors);
    let cfg = w.build(&t);
    cfg.validate().unwrap();
    cfg
}

type Read = fn(&SrsTestConfig, &WireSrs) -> Value;

fn c(cfg: &SrsTestConfig) -> &hertzforge_srs::SrsControl {
    cfg.control.as_ref().expect("the wire always sets control")
}

fn per_band(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|_| json!({"alarm_db": [2, 2.5], "abort_db": [7, 8]}))
            .collect(),
    )
}

#[test]
fn every_wire_field_reaches_the_controllers_configuration() {
    let cases: Vec<(&str, Value, Read, Value)> = vec![
        (
            "target",
            json!({"kind": "flat", "accel_g": 75}),
            |k, _| json!(k.target == TargetSrs::Flat { accel_g: 75.0 }),
            json!(true),
        ),
        (
            "target.points",
            json!([[100, 30], [3000, 60]]),
            |k, _| {
                json!(
                    k.target
                        == TargetSrs::Breakpoints {
                            points: vec![(100.0, 30.0), (3000.0, 60.0)]
                        }
                )
            },
            json!(true),
        ),
        (
            "bands.f_lo_hz",
            json!(125),
            |k, _| json!(k.f_lo_hz),
            json!(125.0),
        ),
        (
            "bands.f_hi_hz",
            json!(1600),
            |k, _| json!(k.f_hi_hz),
            json!(1600.0),
        ),
        (
            "bands.bands_per_octave",
            json!(6),
            |k, _| json!(k.fraction == BandFraction::Sixth),
            json!(true),
        ),
        (
            "bands.q",
            json!(12),
            |k, _| json!([k.q, k.synthesis.q]),
            json!([12.0, 12.0]),
        ),
        (
            "synthesis.engine",
            json!("seds"),
            |k, _| json!(k.engine == EngineKind::Seds),
            json!(true),
        ),
        (
            "synthesis.components",
            json!({"engine": "wavsyn", "rows": [
                {"freq_hz": 150, "amplitude_g": 2.5, "half_cycles": 7, "delay_s": 0.01},
                {"freq_hz": 900, "amplitude_g": 1.5, "half_cycles": 5, "delay_s": 0}]}),
            |k, _| match &k.components {
                Some(SynthesisEngine::Wavsyn(t)) => json!([
                    t.components.len(),
                    t.components[0].freq_hz,
                    t.components[0].amplitude_g,
                    t.components[0].half_cycles,
                    t.components[0].delay_s
                ]),
                other => json!(format!("{other:?}")),
            },
            json!([2, 150.0, 2.5, 7, 0.01]),
        ),
        (
            "synthesis.max_iters",
            json!(20),
            |k, _| json!(k.synthesis.max_iters),
            json!(20),
        ),
        (
            "synthesis.tol_db",
            json!(2),
            |k, _| json!(k.synthesis.tol_db),
            json!(2.0),
        ),
        (
            "synthesis.max_gain_db_per_iter",
            json!(4),
            |k, _| json!(k.synthesis.max_gain_db_per_iter),
            json!(4.0),
        ),
        (
            "envelope.alarm_db",
            json!([2, 4]),
            |k, _| json!(k.envelope.alarm_db),
            json!([2.0, 4.0]),
        ),
        (
            "envelope.abort_db",
            json!([8, 7]),
            |k, _| json!(k.envelope.abort_db),
            json!([8.0, 7.0]),
        ),
        (
            "envelope.per_band",
            per_band(14),
            |k, _| {
                k.envelope.per_band.as_ref().map_or(Value::Null, |p| {
                    let b = &p[3];
                    json!([b.alarm_lo_db, b.alarm_hi_db, b.abort_lo_db, b.abort_hi_db])
                })
            },
            json!([2.0, 2.5, 7.0, 8.0]),
        ),
        (
            "acquisition.fs_hz",
            json!(40960),
            |_, w| json!(w.fs_hz()),
            json!(40960.0),
        ),
        (
            "acquisition.ring_out_s",
            json!(0.1),
            |k, _| json!(k.ring_out_s),
            json!(0.1),
        ),
        (
            "acquisition.pvss",
            json!(false),
            |k, _| json!(k.compute_pvss),
            json!(false),
        ),
        (
            "schedule.mode",
            json!("manual"),
            |k, _| json!(k.schedule.mode == ShockRunMode::Manual),
            json!(true),
        ),
        (
            "schedule.levels_db",
            json!([-12, -3, 0]),
            |k, _| json!(k.schedule.table_db),
            json!([-12.0, -3.0, 0.0]),
        ),
        (
            "schedule.pulses",
            json!([1, 2, 4]),
            |k, _| json!(k.schedule.pulses_per_level),
            json!([1, 2, 4]),
        ),
        (
            "schedule.start_db",
            json!(-6),
            |k, _| json!(k.schedule.start_db),
            json!(-6.0),
        ),
        (
            "schedule.advance",
            json!("count"),
            |k, _| json!(k.schedule.advance == LadderAdvance::Count),
            json!(true),
        ),
        (
            "schedule.max_pulses_per_level",
            json!(7),
            |k, _| json!(k.schedule.max_pulses_per_level),
            json!(7),
        ),
        (
            "spacing.cooling_s",
            json!(2),
            |k, _| json!(c(k).rate_model.cooling_floor_s),
            json!(2.0),
        ),
        (
            "spacing.settling_s",
            json!(1.5),
            |k, _| json!(c(k).rate_model.settling_floor_s),
            json!(1.5),
        ),
        (
            "spacing.compute_s",
            json!(0.5),
            |k, _| json!(c(k).rate_model.compute_floor_s),
            json!(0.5),
        ),
        (
            "identification.excitation",
            json!("standard_pulse"),
            |k, _| json!(k.sysid.unwrap().excitation == SysIdExcitation::StandardPulse),
            json!(true),
        ),
        (
            "identification.level_db",
            json!(-30),
            |k, _| json!(k.sysid.unwrap().level_db),
            json!(-30.0),
        ),
        (
            "identification.max_vpk",
            json!(0.03),
            |k, _| json!([c(k).sysid_max_vpk, c(k).identification_vpk_cap()]),
            json!([0.03, 0.03]),
        ),
        (
            "identification.nfft",
            json!(2048),
            |k, _| json!(k.sysid.unwrap().nfft),
            json!(2048),
        ),
        (
            "identification.h_floor_frac",
            json!(0.1),
            |k, _| json!(k.sysid.unwrap().h_floor_frac),
            json!(0.1),
        ),
        (
            "identification.min_mean_coherence",
            json!(0.7),
            |k, _| json!(c(k).min_mean_coherence),
            json!(0.7),
        ),
        (
            "identification.review",
            json!(false),
            |k, _| json!(k.review_after_sysid),
            json!(false),
        ),
        (
            "control.law",
            json!({"id": "srs_ratio", "params": {"max_db_per_pulse": 2.5}}),
            |_, w| {
                let choice = w.law_choice(&sds_control::laws::builtin_srs_facts());
                json!([choice.id, format!("{:?}", choice.params)])
            },
            json!([
                "srs_ratio",
                "[Param { key: \"max_db_per_pulse\", value: Number(2.5) }]"
            ]),
        ),
        (
            "control.law_budget",
            json!(0.5),
            |k, _| json!(c(k).guards.budget_fraction),
            json!(0.5),
        ),
        (
            "control.law_misses",
            json!(4),
            |k, _| json!(c(k).guards.max_misses),
            json!(4),
        ),
        (
            "control.max_pulses",
            json!(50),
            |k, _| json!(c(k).guards.max_pulses),
            json!(50),
        ),
        (
            "control.max_gain_rise_db",
            json!(8),
            |k, _| json!(c(k).max_gain_rise_db),
            json!(8.0),
        ),
        (
            "limits.drive_vpk_ceiling",
            json!(0.6),
            |k, _| json!(c(k).drive_vpk_ceiling),
            json!(0.6),
        ),
        (
            "limits.drive_vpk_abort",
            json!(0.9),
            |k, _| json!(c(k).drive_vpk_abort),
            json!(0.9),
        ),
        (
            "limits.peak_accel_g",
            json!(30),
            |k, _| json!(k.drive_limit.peak_accel_g),
            json!(30.0),
        ),
        (
            "limits.abort_factor",
            json!(3),
            |k, _| json!(k.drive_abort_factor),
            json!(3.0),
        ),
        (
            "limits.kinematic",
            json!({"accel_g": 80, "velocity_ips": 30, "disp_pp_in": 0.5}),
            |k, _| {
                let a = k.kinematic_abort;
                json!([a.accel_g, a.velocity_ips, a.disp_pp_in])
            },
            json!([80.0, 30.0, 0.5]),
        ),
        (
            "limits.monitors",
            json!([{"channel": 2, "unit": "V", "peak_ceiling": 5, "kind": "predictive"}]),
            |k, _| {
                let m = &k.drive_limit.monitors[0];
                json!([
                    m.channel,
                    m.eu,
                    m.peak_ceiling,
                    m.kind == DriveMonitorKind::Predictive
                ])
            },
            json!(["ch2", "V", 5.0, true]),
        ),
        (
            "limit_channels.headroom_db",
            json!(2),
            |k, _| json!(k.limits.as_ref().unwrap().headroom_db),
            json!(2.0),
        ),
        (
            "limit_channels.channels",
            json!([{"channel": 3,
                    "bound": {"kind": "per_band", "bounds_g": [
                        50, null, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50]},
                    "frf": {"freqs_hz": [0, 500, 25600], "mag": [1, 4, 1]}}]),
            |k, _| {
                let ch = &k.limits.as_ref().unwrap().channels[0];
                let bounds = match &ch.limit {
                    LimitTarget::PerBand { bounds_g } => bounds_g.clone(),
                    LimitTarget::Flat { accel_g } => vec![*accel_g, *accel_g],
                };
                let frf = ch.frf.as_ref().map(|f| (f.freqs_hz.clone(), f.mag.clone()));
                json!([
                    ch.name,
                    bounds[0],
                    bounds[1].is_infinite(),
                    frf.as_ref().map(|f| f.0.clone()),
                    frf.as_ref().map(|f| f.1.clone())
                ])
            },
            json!([
                "Fixture",
                50.0,
                true,
                [0.0, 500.0, 25600.0],
                [1.0, 4.0, 1.0]
            ]),
        ),
        (
            "feasibility.residual_velocity_ips",
            json!(0.2),
            |k, _| json!(k.feasibility_tol.residual_velocity_ips),
            json!(0.2),
        ),
        (
            "feasibility.residual_disp_in",
            json!(0.002),
            |k, _| json!(k.feasibility_tol.residual_disp_in),
            json!(0.002),
        ),
        (
            "feasibility.moving_mass_lb.fixturing",
            json!(14),
            |k, _| json!(k.moving_mass.fixturing_lb),
            json!(14.0),
        ),
        (
            "feasibility.shaker.name",
            json!("Other shaker"),
            |k, _| json!(k.shaker.as_ref().unwrap().name),
            json!("Other shaker"),
        ),
        (
            "operator",
            json!("Tester"),
            |k, _| json!(meta(k, "operator")),
            json!("Tester"),
        ),
        (
            "article",
            json!("Housing"),
            |k, _| json!(meta(k, "test_article")),
            json!("Housing"),
        ),
        (
            "notes",
            json!("bolted at four corners"),
            |k, _| json!(meta(k, "notes")),
            json!("bolted at four corners"),
        ),
        (
            "specification",
            json!({"document": "Procedure", "clause": "2.3"}),
            |k, _| json!([meta(k, "specification"), meta(k, "clause")]),
            json!(["Procedure", "2.3"]),
        ),
        (
            "metadata",
            json!([{"key": "run_number", "label": "Run Number", "value": "12"}]),
            // the run record's words (the specification here) are added after the fields given
            |k, _| {
                let given: Vec<_> = k
                    .metadata
                    .iter()
                    .filter(|m| !matches!(m.key.as_str(), "notes" | "specification" | "clause"))
                    .collect();
                json!([given.len(), given[0].value])
            },
            json!([1, "12"]),
        ),
    ];
    let base = {
        let w = config::parse(&golden_config()).unwrap();
        (built(&w), w)
    };
    for (path, value, read, want) in cases {
        let w = parse_with(path, value.clone());
        let k = built(&w);
        assert_eq!(read(&k, &w), want, "{path} = {value}");
        assert_ne!(
            read(&base.0, &base.1),
            want,
            "{path}: the golden configuration already has the value the case sets"
        );
    }
}

fn meta(k: &SrsTestConfig, key: &str) -> Option<String> {
    k.metadata
        .iter()
        .find(|m| m.key == key)
        .map(|m| m.value.clone())
}

/// One way to record: a configuration that still asks for the module's own
/// capture (a run name or a start at arm) is refused at the field, and the
/// controller's capture never starts, whatever the configuration. Red if
/// `capture` were taken again, or if the controller were handed a capture that
/// starts.
#[test]
fn an_srs_capture_is_refused_and_the_controllers_capture_stays_off() {
    for capture in [
        json!({"auto_start": true}),
        json!({"run_name": "bracket_x"}),
    ] {
        let mut c = golden_config();
        c["capture"] = capture;
        let e = config::parse(&c).expect_err("a capture is refused");
        assert_eq!(e.reason, Some("bad_params"), "{}", e.message);
        assert_eq!(
            e.detail.as_deref().map(|d| d["param"].clone()),
            Some(json!("config.capture")),
            "{}",
            e.message
        );
    }
    let mut c = golden_config();
    c.as_object_mut().unwrap().remove("capture");
    c["record"] = json!({"streams": false});
    let w = config::parse(&c).unwrap();
    assert!(!w.record_streams);
    let k = built(&w);
    assert_eq!(k.capture.trigger, ShockCaptureTrigger::Off);
    assert!(!k.capture.snapshot_on_stop);
}

fn errors_of(c: &Value) -> Vec<(String, String)> {
    let w = config::parse(c).unwrap();
    let t = table();
    let (r, _) = w.check(&CheckContext {
        table: &t,
        law: &law,
        timeout_s: 30.0,
    });
    r.errors
        .iter()
        .map(|i| {
            (
                i.reason.to_owned(),
                match &i.at {
                    table::At::Field(f) => f.clone(),
                    other => format!("{other:?}"),
                },
            )
        })
        .collect()
}

#[test]
fn each_refusal_comes_back_by_name_with_its_field() {
    let cases: Vec<(&str, Value, &str, &str)> = vec![
        (
            "target",
            json!({"kind": "flat", "accel_g": 0}),
            "out_of_range",
            "target.accel_g",
        ),
        (
            "target.points",
            json!([[100, 40]]),
            "out_of_range",
            "target.points",
        ),
        (
            "target.points",
            json!([[100, 40], [90, 50], [3000, 60]]),
            "target_not_ascending",
            "target.points[1]",
        ),
        (
            "target.points",
            json!([[100, 40], [1900, 100]]),
            "target_not_covering",
            "target.points",
        ),
        ("bands.f_lo_hz", json!(0), "out_of_range", "bands.f_lo_hz"),
        ("bands.f_hi_hz", json!(50), "out_of_range", "bands.f_hi_hz"),
        (
            "bands.f_hi_hz",
            json!(25700),
            "band_above_grid",
            "bands.f_hi_hz",
        ),
        (
            "bands.bands_per_octave",
            json!(2.5),
            "out_of_range",
            "bands.bands_per_octave",
        ),
        ("bands.q", json!(0.5), "out_of_range", "bands.q"),
        (
            "synthesis.max_iters",
            json!(0),
            "out_of_range",
            "synthesis.max_iters",
        ),
        (
            "synthesis.tol_db",
            json!(0),
            "out_of_range",
            "synthesis.tol_db",
        ),
        (
            "synthesis.max_gain_db_per_iter",
            json!(25),
            "out_of_range",
            "synthesis.max_gain_db_per_iter",
        ),
        (
            "synthesis.components",
            json!({"engine": "seds", "rows": [
                {"freq_hz": 150, "amplitude_g": 1, "zeta": 0.05}]}),
            "components_engine_mismatch",
            "synthesis.components.engine",
        ),
        (
            "synthesis.components",
            json!({"engine": "wavsyn", "rows": [
                {"freq_hz": 150, "amplitude_g": 1, "half_cycles": 5},
                {"freq_hz": 300, "amplitude_g": 1, "half_cycles": 4}]}),
            "component_invalid",
            "synthesis.components.rows[1]",
        ),
        (
            "synthesis.components",
            json!({"engine": "wavsyn", "rows": []}),
            "out_of_range",
            "synthesis.components.rows",
        ),
        (
            "envelope.alarm_db",
            json!([0, 3]),
            "out_of_range",
            "envelope.alarm_db",
        ),
        (
            "envelope.abort_db",
            json!([2, 6]),
            "alarm_wider_than_abort",
            "envelope.abort_db",
        ),
        (
            "envelope.per_band",
            per_band(3),
            "per_band_length_mismatch",
            "envelope.per_band",
        ),
        (
            "acquisition.fs_hz",
            json!(500),
            "out_of_range",
            "acquisition.fs_hz",
        ),
        (
            "acquisition.ring_out_s",
            json!(-1),
            "out_of_range",
            "acquisition.ring_out_s",
        ),
        (
            "schedule.levels_db",
            json!([]),
            "levels_empty",
            "schedule.levels_db",
        ),
        (
            "schedule.levels_db",
            json!([-6, -12, 0]),
            "levels_not_ascending",
            "schedule.levels_db[1]",
        ),
        (
            "schedule.pulses",
            json!([1, 1]),
            "pulses_length_mismatch",
            "schedule.pulses",
        ),
        (
            "schedule.start_db",
            json!(3),
            "out_of_range",
            "schedule.start_db",
        ),
        (
            "schedule.max_pulses_per_level",
            json!(0),
            "out_of_range",
            "schedule.max_pulses_per_level",
        ),
        (
            "spacing.cooling_s",
            json!(-1),
            "out_of_range",
            "spacing.cooling_s",
        ),
        (
            "identification.level_db",
            json!(0),
            "out_of_range",
            "identification.level_db",
        ),
        (
            "identification.max_vpk",
            json!(0.6),
            "out_of_range",
            "identification.max_vpk",
        ),
        (
            "identification.nfft",
            json!(1000),
            "out_of_range",
            "identification.nfft",
        ),
        (
            "identification.h_floor_frac",
            json!(0),
            "out_of_range",
            "identification.h_floor_frac",
        ),
        (
            "identification.min_mean_coherence",
            json!(1),
            "out_of_range",
            "identification.min_mean_coherence",
        ),
        (
            "control.law",
            json!({"id": "nope"}),
            "law_unknown",
            "control.law.id",
        ),
        (
            "control.law",
            json!({"id": "fd_correct"}),
            "law_wrong_world",
            "control.law.id",
        ),
        (
            "control.law",
            json!({"id": "srs_ratio", "params": {"beta": 1}}),
            "law_param_unknown",
            "control.law.params.beta",
        ),
        (
            "control.law",
            json!({"id": "srs_ratio", "params": {"max_db_per_pulse": 9}}),
            "law_param_invalid",
            "control.law.params.max_db_per_pulse",
        ),
        (
            "control.law_budget",
            json!(0),
            "out_of_range",
            "control.law_budget",
        ),
        (
            "control.law_misses",
            json!(0),
            "out_of_range",
            "control.law_misses",
        ),
        (
            "control.max_pulses",
            json!(0),
            "out_of_range",
            "control.max_pulses",
        ),
        (
            "control.max_gain_rise_db",
            json!(3.5),
            "out_of_range",
            "control.max_gain_rise_db",
        ),
        (
            "limits.drive_vpk_ceiling",
            json!(12),
            "out_of_range",
            "limits.drive_vpk_ceiling",
        ),
        (
            "limits.drive_vpk_abort",
            json!(0.4),
            "out_of_range",
            "limits.drive_vpk_abort",
        ),
        (
            "limits.peak_accel_g",
            json!(0),
            "out_of_range",
            "limits.peak_accel_g",
        ),
        (
            "limits.abort_factor",
            json!(0.5),
            "out_of_range",
            "limits.abort_factor",
        ),
        (
            "limits.kinematic",
            json!({"velocity_ips": 0}),
            "out_of_range",
            "limits.kinematic.velocity_ips",
        ),
        (
            "limits.monitors",
            json!([{"channel": 9, "unit": "A", "peak_ceiling": 40, "kind": "between_pulse"}]),
            "limit_unknown_channel",
            "limits.monitors[0].channel",
        ),
        (
            "limit_channels.headroom_db",
            json!(-1),
            "out_of_range",
            "limit_channels.headroom_db",
        ),
        (
            "limit_channels.channels",
            json!([{"channel": 1, "bound": {"kind": "flat", "accel_g": 50}}]),
            "limit_unknown_channel",
            "limit_channels.channels[0].channel",
        ),
        (
            "limit_channels.channels",
            json!([{"channel": 4, "bound": {"kind": "flat", "accel_g": 50}}]),
            "limit_no_frf",
            "limit_channels.channels[0].frf",
        ),
        (
            "limit_channels.channels",
            json!([{"channel": 3, "bound": {"kind": "per_band", "bounds_g": [50, 50]}}]),
            "per_band_length_mismatch",
            "limit_channels.channels[0].bound.bounds_g",
        ),
        (
            "limit_channels.channels",
            json!([{"channel": 3, "bound": {"kind": "flat", "accel_g": 50},
                    "frf": {"freqs_hz": [500, 100], "mag": [1, 1]}}]),
            "out_of_range",
            "limit_channels.channels[0].frf",
        ),
        (
            "feasibility.residual_disp_in",
            json!(0),
            "out_of_range",
            "feasibility.residual_disp_in",
        ),
    ];
    for (path, value, reason, field) in cases {
        let mut c = golden_config();
        set(&mut c, path, value.clone());
        let errors = errors_of(&c);
        assert!(
            errors.iter().any(|(r, f)| r == reason && f == field),
            "{path} = {value}: wanted {reason} at {field}, got {errors:?}"
        );
    }
    // A pulse longer than the controller timeout cannot be safe-stopped behind.
    let mut c = golden_config();
    set(&mut c, "acquisition.ring_out_s", json!(9));
    let w = config::parse(&c).unwrap();
    let t = table();
    let (r, _) = w.check(&CheckContext {
        table: &t,
        law: &law,
        timeout_s: 5.0,
    });
    assert!(
        r.errors.iter().any(|i| i.reason == "record_too_long"),
        "{:?}",
        r.errors
    );
}

#[test]
fn the_table_must_have_a_control_and_a_drive() {
    let w = config::parse(&golden_config()).unwrap();
    for (role, reason) in [
        (ChannelRole::Drive, "no_drive_channel"),
        (ChannelRole::Control, "no_control_channel"),
    ] {
        let mut t = table();
        t.channels.retain(|c| c.role != role);
        let (r, _) = w.check(&CheckContext {
            table: &t,
            law: &law,
            timeout_s: 30.0,
        });
        assert!(r.errors.iter().any(|i| i.reason == reason), "{reason}");
    }
}

#[test]
fn a_synthesis_that_does_not_reach_its_tolerance_is_a_warning_not_an_error() {
    let mut c = golden_config();
    set(&mut c, "synthesis.max_iters", json!(1));
    set(&mut c, "synthesis.tol_db", json!(0.05));
    let w = config::parse(&c).unwrap();
    let t = table();
    let (r, d) = w.check(&CheckContext {
        table: &t,
        law: &law,
        timeout_s: 30.0,
    });
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.warnings[0].reason, "synthesis_not_converged");
    assert!(!d.unwrap().synthesis.converged);
}

#[test]
fn each_shape_error_is_bad_params_naming_the_field() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("target.kind", json!("ramp"), "config.target.kind"),
        ("target.points", json!([[100]]), "config.target.points[0]"),
        ("bands.f_lo_hz", json!("low"), "config.bands.f_lo_hz"),
        (
            "synthesis.engine",
            json!("fourier"),
            "config.synthesis.engine",
        ),
        (
            "synthesis.components",
            json!({"engine": "wavsyn", "rows": [{"freq_hz": 100, "amplitude_g": 1}]}),
            "config.synthesis.components.rows[0].half_cycles",
        ),
        ("envelope.alarm_db", json!([3]), "config.envelope.alarm_db"),
        ("acquisition.pvss", json!("yes"), "config.acquisition.pvss"),
        ("schedule.mode", json!("hold"), "config.schedule.mode"),
        (
            "schedule.advance",
            json!("sometimes"),
            "config.schedule.advance",
        ),
        (
            "identification.excitation",
            json!("chirp"),
            "config.identification.excitation",
        ),
        (
            "identification.review",
            json!("yes"),
            "config.identification.review",
        ),
        (
            "control.law",
            json!({"params": {}}),
            "config.control.law.id",
        ),
        (
            "limits.drive_vpk_ceiling",
            Value::Null,
            "config.limits.drive_vpk_ceiling",
        ),
        (
            "limits.monitors",
            json!([{"channel": 2, "unit": "A", "peak_ceiling": 40, "kind": "after"}]),
            "config.limits.monitors[0].kind",
        ),
        (
            "limit_channels.channels",
            json!([{"channel": 3, "bound": {"kind": "per_band", "bounds_g": ["x"]}}]),
            "config.limit_channels.channels[0].bound.bounds_g[0]",
        ),
        (
            "feasibility.moving_mass_lb.armature",
            json!(-1),
            "config.feasibility.moving_mass_lb.armature",
        ),
        (
            "metadata",
            json!([{"key": "a"}]),
            "config.metadata[0].label",
        ),
    ];
    for (path, value, param) in cases {
        let mut c = golden_config();
        set(&mut c, path, value.clone());
        let e = config::parse(&c).map(|_| ()).unwrap_err();
        assert_eq!(e.reason, Some("bad_params"), "{path}");
        assert_eq!(
            e.detail.as_deref().map(|d| d["param"].clone()),
            Some(json!(param)),
            "{path} = {value}: {}",
            e.message
        );
    }
    let mut c = golden_config();
    set(&mut c, "feasibility.shaker", json!({"name": 3}));
    let e = config::parse(&c).map(|_| ()).unwrap_err();
    assert_eq!(e.reason, Some("shaker_record_invalid"));
}

#[test]
fn an_envelope_pair_is_under_then_over_the_target_as_the_controller_judges_it() {
    // abort_db [9, 6]: a band may sit 9 dB under the target and 6 dB over it.
    let mut c = golden_config();
    set(&mut c, "envelope.alarm_db", json!([3, 3]));
    set(&mut c, "envelope.abort_db", json!([9, 6]));
    let w = config::parse(&c).unwrap();
    let k = built(&w);
    let env = k.envelope.resolve(1).unwrap();
    let at = |db: f64| env.check(&[10.0], &[10.0 * 10f64.powf(db / 20.0)], 1.0);
    assert!(at(7.0).aborted(), "7 dB over is past the 6 dB over");
    assert!(
        !at(-8.0).aborted() && at(-8.0).alarmed(),
        "8 dB under alarms only"
    );
    assert!(at(-10.0).aborted(), "10 dB under is past the 9 dB under");
}

#[test]
fn mimo_shock_srs_refuses_a_second_drive_row() {
    // Single-output SRS plays one drive record. With two enabled drive rows a station opens
    // both as outputs and the one record would be split across them as interleaved frames.
    let w = config::parse(&golden_config()).unwrap();
    let mut t = table();
    let mut second = table::new_row(9);
    second.role = ChannelRole::Drive;
    second.units = Units::VoltageV;
    t.channels.push(second);
    let (r, _) = w.check(&CheckContext {
        table: &t,
        law: &law,
        timeout_s: 30.0,
    });
    assert!(
        r.errors.iter().any(|i| i.reason == "extra_drive_channels"),
        "{:?}",
        r.errors
    );
    t.channels.last_mut().unwrap().enabled = false;
    let (r, _) = w.check(&CheckContext {
        table: &t,
        law: &law,
        timeout_s: 30.0,
    });
    assert!(
        !r.errors.iter().any(|i| i.reason == "extra_drive_channels"),
        "{:?}",
        r.errors
    );
}
