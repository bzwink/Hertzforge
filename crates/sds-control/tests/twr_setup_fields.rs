//! Every TWR wire field reaches the controller's `TwrConfig` (or the run that
//! carries it), and every content problem is refused by name at its field. Each
//! case changes one field of the golden configuration to a value it does not
//! already hold, builds, and reads the controller's field back; the same check
//! read on the unchanged golden must fail, so no case passes by default.

use hertzforge_channel_table::{ChannelRole, ChannelTable, Units};
use hertzforge_twr::schedule::{GateFailPolicy, TwrRunMode};
use hertzforge_twr::{Excitation, RunSchedule, TwrConfig};
use sds_control::table;
use sds_control::twr::config::{self, CheckContext, WireTwr};
use sds_control::twr::reference::{self, ImportOptions, ImportRequest, Source, Stored};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

fn golden() -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema/golden/request_twr_configure.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v["params"]["config"].clone()
}

fn table() -> ChannelTable {
    let mut t = table::Table::new().table;
    let mut c = table::new_row(1);
    c.role = ChannelRole::Control;
    let mut d = table::new_row(2);
    d.role = ChannelRole::Drive;
    d.units = Units::VoltageV;
    t.channels = vec![c, d];
    t
}

fn stored() -> Stored {
    let samples: Vec<f64> = (0..2048)
        .map(|i| {
            let k = ((i + 16) % 64) as f64;
            0.25 * (1.0 - (k - 32.0).abs() / 16.0)
        })
        .collect();
    let text: String = samples.iter().map(|v| format!("{v}\n")).collect();
    let dir = std::env::temp_dir().join(format!(
        "sds-twr-fields-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let s = reference::import(
        &dir,
        &ImportRequest {
            source: Source::Text(text),
            options: ImportOptions {
                fs_hz: Some(12_800.0),
                column: 0,
                units: None,
            },
            name: Some("r1".into()),
        },
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(dir);
    s
}

fn wire(c: &Value) -> WireTwr {
    config::parse(c).unwrap_or_else(|e| panic!("{e:?}"))
}

fn build(c: &Value) -> TwrConfig {
    let facts = sds_control::laws::builtin_twr_facts();
    wire(c).build(&stored(), &facts)
}

fn check(c: &Value) -> table::Report {
    let w = wire(c);
    let t = table();
    let s = Arc::new(stored());
    let refs = move |sha: &str| (sha == s.sha256).then(|| s.clone());
    let law = |id: &str| sds_control::laws::twr_lookup(None, id);
    let file = |_: &Stored, _: &str| Ok(());
    let ctx = CheckContext {
        table: &t,
        law: &law,
        reference: &refs,
        file_check: &file,
    };
    w.check(&ctx).0
}

fn set(c: &mut Value, path: &str, v: Value) {
    let mut node = c;
    let parts: Vec<&str> = path.split('.').collect();
    for (i, p) in parts.iter().enumerate() {
        let (key, index) = match p.split_once('[') {
            Some((k, rest)) => (
                k,
                Some(rest.trim_end_matches(']').parse::<usize>().unwrap()),
            ),
            None => (*p, None),
        };
        let last = i + 1 == parts.len();
        if last && index.is_none() {
            node[key] = v;
            return;
        }
        node = &mut node[key];
        if let Some(i) = index {
            if last {
                node[i] = v;
                return;
            }
            node = &mut node[i];
        }
    }
}

type Check = fn(&TwrConfig, &WireTwr) -> bool;

#[test]
fn every_wire_field_reaches_the_controllers_configuration() {
    let cases: Vec<(&str, Value, Check)> = vec![
        ("title", json!("Another"), |_, w| w.title == "Another"),
        ("prep.auto_resample", json!(false), |c, _| {
            !c.prep.auto_resample
        }),
        ("prep.detrend", json!("linear"), |c, _| {
            c.prep.detrend == Some(true)
        }),
        ("prep.highpass_hz", json!(null), |c, _| {
            c.prep.highpass_hz.is_none()
        }),
        ("prep.scale", json!(2), |c, _| c.prep.scale == 2.0),
        ("prep.taper_frac", json!(0.1), |c, _| {
            c.prep.taper_frac == Some(0.1)
        }),
        ("section", json!({"start": 16, "len": 1024}), |c, _| {
            c.section.is_some_and(|s| s.start == 16 && s.len == 1024)
        }),
        ("pads.pre", json!(256), |c, _| c.pad_pre == 256),
        ("pads.post", json!(2048), |c, _| c.pad_post == 2048),
        ("acquisition.fs_hz", json!(25600), |_, w| {
            w.shape.fs_hz == Some(25_600.0)
        }),
        ("identification.excitation", json!("log_chirp"), |c, _| {
            c.sysid.excitation == Excitation::LogChirp
        }),
        ("identification.drive_vrms", json!(0.04), |c, _| {
            c.sysid.drive_rms == 0.04
        }),
        ("identification.duration_s", json!(2), |c, _| {
            c.sysid.duration_s == 2.0
        }),
        ("identification.nfft", json!(1024), |c, _| {
            c.sysid.nfft == 1024
        }),
        ("identification.band_hz", json!([10, 2500]), |c, _| {
            c.sysid.f_lo_hz == 10.0 && c.sysid.f_hi_hz == 2500.0
        }),
        ("identification.min_mean_coherence", json!(0.7), |c, _| {
            c.sysid.min_mean_coherence == 0.7
        }),
        ("identification.review", json!(false), |c, w| {
            !c.review_after_sysid && !w.review
        }),
        ("inverse.tikhonov_floor", json!(0.1), |c, _| {
            c.inverse.tikhonov_beta_floor == 0.1
        }),
        ("inverse.coherence_floor", json!(0.8), |c, _| {
            c.inverse.coherence_floor == 0.8
        }),
        ("correction.band_hz", json!([40, 1000]), |c, _| {
            c.law.f_lo_hz == 40.0 && c.law.f_hi_hz == 1000.0
        }),
        ("correction.ref_floor_db", json!(40), |c, _| {
            c.law.ref_floor_db == 40.0
        }),
        ("correction.min_band_energy", json!(0.95), |c, _| {
            c.law.min_masked_energy_frac == 0.95
        }),
        ("control.law.params.beta", json!(0.25), |c, _| {
            c.law.beta == 0.25
        }),
        (
            "control.law.params",
            json!({"beta": 0.5, "max_db_per_iter": 3}),
            |c, _| c.law.max_db_per_iter == 3.0,
        ),
        (
            "control.law.params",
            json!({"initial_backoff_db": 6}),
            |c, _| c.law.initial_drive_backoff_db == 6.0,
        ),
        ("control.law_budget", json!(0.5), |c, _| {
            c.control
                .as_ref()
                .is_some_and(|k| k.guards.budget_fraction == 0.5)
        }),
        ("control.law_misses", json!(5), |c, _| {
            c.control.as_ref().is_some_and(|k| k.guards.max_misses == 5)
        }),
        ("control.max_peak_rise_db", json!(12), |c, _| {
            c.control
                .as_ref()
                .is_some_and(|k| k.max_peak_rise_db == 12.0)
        }),
        ("control.max_bin_rise_db", json!(14), |c, _| {
            c.control
                .as_ref()
                .is_some_and(|k| k.max_bin_rise_db == 14.0)
        }),
        ("control.first_drive_ratio", json!(3), |c, _| {
            c.control
                .as_ref()
                .is_some_and(|k| k.first_drive_ratio == 3.0)
        }),
        ("control.alignment_frames", json!(2), |c, _| {
            c.control
                .as_ref()
                .is_some_and(|k| k.alignment_frames == 2.0)
        }),
        ("schedule.mode", json!("manual"), |c, _| {
            c.schedule.mode == TwrRunMode::Manual
        }),
        ("schedule.levels_db", json!([-6, 0]), |c, _| {
            c.schedule.table_db == vec![-6.0, 0.0]
        }),
        ("schedule.gate_pct", json!(5), |c, _| {
            c.schedule.gate_rms_error_pct == 5.0
        }),
        ("schedule.max_iterations", json!(4), |c, _| {
            c.schedule.max_iterations_per_level == 4
        }),
        ("schedule.on_gate_fail", json!("hold"), |c, _| {
            c.schedule.on_gate_fail == GateFailPolicy::Hold
        }),
        ("run", json!({"schedule": "once"}), |c, _| {
            c.run == RunSchedule::Once
        }),
        ("run", json!({"schedule": "loop", "count": 7}), |c, _| {
            c.run == RunSchedule::LoopN(7)
        }),
        ("run.duration_s", json!(90), |c, _| {
            c.run == RunSchedule::DurationS(90.0)
        }),
        ("adaptation.during_playback", json!(true), |c, _| {
            c.adapt_during_playback
        }),
        ("limits.drive_vpk_ceiling", json!(0.4), |c, _| {
            c.limits.max_drive_vpk == Some(0.4)
        }),
        ("limits.drive_clip_vpk", json!(null), |c, _| {
            c.limits.drive_clip.is_none()
        }),
        ("limits.drive_vrms_limit", json!(0.3), |c, _| {
            c.limits.drive_rms_limit == Some(0.3)
        }),
        ("limits.response_rms_limit", json!(2), |c, _| {
            c.limits.response_rms_limit == Some(2.0)
        }),
        ("limits.envelope_alarm_frac", json!(0.2), |c, _| {
            c.limits.envelope_alarm_frac == 0.2
        }),
        ("limits.envelope_abort_frac", json!(0.4), |c, _| {
            c.limits.envelope_abort_frac == 0.4
        }),
        ("limits.envelope_debounce_samples", json!(16), |c, _| {
            c.limits.envelope_debounce_samples == 16
        }),
        ("limits.divergence_plays", json!(4), |c, _| {
            c.limits.divergence_n == 4
        }),
        ("limits.divergence_rise_frac", json!(0.2), |c, _| {
            c.limits.divergence_tol_frac == 0.2
        }),
        ("feasibility.peak_accel", json!(5), |c, _| {
            c.limits.feasibility.peak_accel == Some(5.0)
        }),
        ("feasibility.peak_velocity_ips", json!(30), |c, _| {
            c.limits.feasibility.peak_velocity_ips == Some(30.0)
        }),
        ("feasibility.disp_pkpk_in", json!(null), |c, _| {
            c.limits.feasibility.disp_pkpk_in.is_none()
        }),
        ("feasibility.kinematics_hp_hz", json!(3), |c, _| {
            c.kinematics_hp_hz == 3.0
        }),
        ("operator", json!("Tester"), |c, _| {
            meta(c, "operator") == Some("Tester")
        }),
        ("article", json!("Housing"), |c, _| {
            meta(c, "test_article") == Some("Housing")
        }),
        ("notes", json!("bolted at four corners"), |c, _| {
            meta(c, "notes") == Some("bolted at four corners")
        }),
        ("specification.clause", json!("2.3"), |c, _| {
            meta(c, "clause") == Some("2.3")
        }),
        ("metadata[0].value", json!("8"), |c, _| {
            c.metadata[0].value == "8"
        }),
    ];
    let base = golden();
    let built = build(&base);
    let w0 = wire(&base);
    assert!(built.control.is_some(), "the wire always sets control");
    assert!(built.control.as_ref().unwrap().rest_taper);
    for (path, value, ok) in &cases {
        assert!(
            !ok(&built, &w0),
            "{path}: the check already holds on the golden; it proves nothing"
        );
        let mut c = base.clone();
        if *path == "run.duration_s" {
            set(&mut c, "run.schedule", json!("duration"));
        }
        set(&mut c, path, value.clone());
        let got = build(&c);
        assert!(
            ok(&got, &wire(&c)),
            "{path} = {value} did not reach the core"
        );
    }
    // The reference the core is given is the stored record, raw: prep is the core's.
    assert_eq!(built.reference.samples, stored().samples);
    assert_eq!(built.reference.fs_hz, 12_800.0);
}

fn meta<'a>(c: &'a TwrConfig, key: &str) -> Option<&'a str> {
    c.metadata
        .iter()
        .find(|m| m.key == key)
        .map(|m| m.value.as_str())
}

/// One way to record: a configuration that still asks for the module's own
/// capture is refused at the field, and the controller's capture never starts,
/// whatever the configuration. Red if `capture` were taken again, or if the
/// controller were handed a capture that starts.
#[test]
fn a_twr_capture_is_refused_and_the_controllers_capture_stays_off() {
    let mut c = golden();
    c["capture"] = json!({"auto_start": true, "run_name": "twr_run"});
    let e = config::parse(&c).expect_err("a capture is refused");
    assert_eq!(e.reason, Some("bad_params"), "{}", e.message);
    assert_eq!(
        e.detail.as_deref().map(|d| d["param"].clone()),
        Some(json!("config.capture")),
        "{}",
        e.message
    );
    let mut c = golden();
    c["record"] = json!({"streams": false});
    let w = wire(&c);
    assert!(!w.record_streams);
    assert!(!build(&c).capture.auto_start);
    assert!(wire(&golden()).record_streams);
}

#[test]
fn the_checks_refuse_each_problem_by_name_at_its_field() {
    let cases: Vec<(&str, Value, &str, &str)> = vec![
        (
            "prep.taper_frac",
            json!(0.6),
            "out_of_range",
            "prep.taper_frac",
        ),
        ("prep.scale", json!(0), "out_of_range", "prep.scale"),
        (
            "prep.highpass_hz",
            json!(30),
            "highpass_above_band",
            "prep.highpass_hz",
        ),
        (
            "section",
            json!({"start": 2000, "len": 100}),
            "section_outside_reference",
            "section.len",
        ),
        (
            "acquisition.fs_hz",
            json!(500),
            "out_of_range",
            "acquisition.fs_hz",
        ),
        (
            "acquisition.fs_hz",
            json!(6400),
            "rate_mismatch",
            "acquisition.fs_hz",
        ),
        (
            "identification.duration_s",
            json!(0.1),
            "identification_too_short",
            "identification.duration_s",
        ),
        (
            "identification.nfft",
            json!(1000),
            "out_of_range",
            "identification.nfft",
        ),
        (
            "identification.band_hz",
            json!([20, 7000]),
            "band_above_grid",
            "identification.band_hz",
        ),
        (
            "identification.drive_vrms",
            json!(0.4),
            "out_of_range",
            "identification.drive_vrms",
        ),
        (
            "inverse.tikhonov_floor",
            json!(1),
            "out_of_range",
            "inverse.tikhonov_floor",
        ),
        (
            "correction.band_hz",
            json!([10, 2000]),
            "band_outside_identification",
            "correction.band_hz",
        ),
        (
            "correction.min_band_energy",
            json!(1),
            "out_of_range",
            "correction.min_band_energy",
        ),
        (
            "control.law.id",
            json!("nope"),
            "law_unknown",
            "control.law.id",
        ),
        (
            "control.law.params",
            json!({"gain": 1}),
            "law_param_unknown",
            "control.law.params.gain",
        ),
        (
            "control.law.params",
            json!({"beta": 2}),
            "law_param_invalid",
            "control.law.params.beta",
        ),
        (
            "control.law_misses",
            json!(0),
            "out_of_range",
            "control.law_misses",
        ),
        (
            "control.max_bin_rise_db",
            json!(6.5),
            "out_of_range",
            "control.max_bin_rise_db",
        ),
        (
            "control.alignment_frames",
            json!(20),
            "out_of_range",
            "control.alignment_frames",
        ),
        (
            "schedule.levels_db",
            json!([]),
            "levels_empty",
            "schedule.levels_db",
        ),
        (
            "schedule.levels_db",
            json!([0, -6]),
            "levels_not_ascending",
            "schedule.levels_db[1]",
        ),
        (
            "schedule.levels_db",
            json!([-70, 0]),
            "out_of_range",
            "schedule.levels_db[0]",
        ),
        (
            "schedule.gate_pct",
            json!(0),
            "out_of_range",
            "schedule.gate_pct",
        ),
        (
            "run",
            json!({"schedule": "loop", "count": 0}),
            "out_of_range",
            "run.count",
        ),
        (
            "limits.envelope_alarm_frac",
            json!(0.6),
            "alarm_wider_than_abort",
            "limits.envelope_alarm_frac",
        ),
        (
            "limits.drive_vpk_ceiling",
            json!(11),
            "out_of_range",
            "limits.drive_vpk_ceiling",
        ),
        (
            "limits.envelope_debounce_samples",
            json!(0),
            "out_of_range",
            "limits.envelope_debounce_samples",
        ),
        (
            "feasibility.disp_pkpk_in",
            json!(0),
            "out_of_range",
            "feasibility.disp_pkpk_in",
        ),
    ];
    assert!(check(&golden()).errors.is_empty());
    for (path, value, reason, field) in cases {
        let mut c = golden();
        set(&mut c, path, value.clone());
        let r = check(&c);
        let found: Vec<(String, String)> = r
            .errors
            .iter()
            .map(|i| {
                let j = i.to_json();
                (
                    j["reason"].as_str().unwrap().to_owned(),
                    j["field"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect();
        assert!(
            found.contains(&(reason.to_owned(), field.to_owned())),
            "{path} = {value}: wanted {reason} at {field}, got {found:?}"
        );
    }
}

#[test]
fn a_wrong_shape_is_refused_naming_the_field() {
    for (path, value, param) in [
        ("prep.detrend", json!("quadratic"), "config.prep.detrend"),
        ("pads.pre", json!(-1), "config.pads.pre"),
        ("section", json!({"start": 0}), "config.section.len"),
        (
            "identification.excitation",
            json!("noise"),
            "config.identification.excitation",
        ),
        ("schedule.mode", json!("step"), "config.schedule.mode"),
        ("run", json!({"schedule": "loop"}), "config.run.count"),
        (
            "reference.sha256",
            json!("r1.csv"),
            "config.reference.sha256",
        ),
        (
            "limits.drive_vpk_ceiling",
            json!("high"),
            "config.limits.drive_vpk_ceiling",
        ),
    ] {
        let mut c = golden();
        set(&mut c, path, value);
        let e = config::parse(&c).unwrap_err();
        assert_eq!(e.reason, Some("bad_params"), "{path}");
        assert_eq!(
            e.detail.as_deref().unwrap()["param"],
            json!(param),
            "{path}"
        );
    }
    let mut c = golden();
    c["limits"]
        .as_object_mut()
        .unwrap()
        .remove("drive_vpk_ceiling");
    let e = config::parse(&c).unwrap_err();
    assert_eq!(
        e.detail.as_deref().unwrap()["param"],
        json!("config.limits.drive_vpk_ceiling")
    );
}
