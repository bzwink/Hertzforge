//! The MIMO Random and control-law goldens in `schema/golden/`, built by this crate's
//! own builders: a setup parses and validates to the golden validation, and the status,
//! report and law-catalog objects the service sends are the golden ones.

use hertzforge_channel_table::{ChannelRole, ChannelTable, Units};
use hertzforge_random::RunMode;
use hertzforge_random_laws::LawCatalog;
use sds_control::laws;
use sds_control::mimo::check;
use sds_control::mimo::driver::{
    DriveVerdict, Gates, LawRef, MimoReport, MimoStatus, OverrideReason, Prediction,
};
use sds_control::mimo::setup::{self, LawInfo, SetupContext};
use sds_control::mimo::view::{self, RunFacts};
use sds_control::random::config::Grid;
use sds_control::random::view::{End, State, StationRef};
use sds_control::table;
use serde_json::Value;
use std::path::PathBuf;

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema/golden")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// Structural equality with numbers compared by value and keys in order.
fn same(a: &Value, b: &Value, path: &str) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            assert_eq!(x.as_f64(), y.as_f64(), "number differs at {path}")
        }
        (Value::Object(x), Value::Object(y)) => {
            let kx: Vec<&String> = x.keys().collect();
            let ky: Vec<&String> = y.keys().collect();
            assert_eq!(kx, ky, "keys differ at {path}");
            for (k, v) in x {
                same(v, &y[k], &format!("{path}.{k}"));
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            assert_eq!(x.len(), y.len(), "lengths differ at {path}");
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                same(u, v, &format!("{path}[{i}]"));
            }
        }
        _ => assert_eq!(a, b, "values differ at {path}"),
    }
}

/// Two control rows (1, 2) and two drive rows (3, 4), as the goldens name them.
fn two_by_two() -> ChannelTable {
    let mut t = table::Table::new().table;
    let row = |i: u32, d: &str, role: ChannelRole| {
        let mut c = table::new_row(i);
        c.description = d.into();
        c.role = role;
        if role == ChannelRole::Drive {
            c.units = Units::VoltageV;
            c.sensitivity_mv_per_eu = 1000.0;
            c.range_max = 10.0;
        }
        c
    };
    t.channels = vec![
        row(1, "Control X", ChannelRole::Control),
        row(2, "Control Y", ChannelRole::Control),
        row(3, "Shaker X", ChannelRole::Drive),
        row(4, "Shaker Y", ChannelRole::Drive),
    ];
    t
}

fn catalog() -> LawCatalog {
    let dir = std::env::temp_dir().join(format!("sds-mimo-golden-{}", std::process::id()));
    LawCatalog::open(dir).unwrap()
}

fn law_ref() -> LawRef {
    LawRef {
        id: "pinv_closed".into(),
        name: "pseudo-inverse, closed loop".into(),
        version: "1.0.0".into(),
        sha256: String::new(),
    }
}

fn parse(setup: &Value) -> check::Validation {
    let cat = catalog();
    let lookup = |id: &str| cat.entry(id).map(|e| laws::law_info(&e));
    let table = two_by_two();
    let ctx = SetupContext {
        table: &table,
        selected_kind: "sim",
        law: &lookup,
    };
    let (s, report) = setup::parse(setup, &ctx).unwrap();
    let info: Option<LawInfo> = lookup(&s.law.id);
    check::numeric(&s, report, info.as_ref().map(laws::law_ref))
}

fn grid() -> Grid {
    Grid {
        fs_hz: 8192.0,
        n_pts: 4096,
        df_hz: 2.0,
        lines: 8,
        loop_s: 2.25,
        actual: false,
    }
}

fn station() -> StationRef {
    StationRef {
        kind: "sim",
        class: "sim",
        devices: Vec::new(),
    }
}

fn configured() -> MimoStatus {
    MimoStatus {
        state: State::Configured,
        phase: "idle",
        elapsed_s: None,
        level_db: None,
        at_level_s: None,
        remaining_at_level_s: None,
        loop_index: 0,
        alarm: false,
        control_error_db: Vec::new(),
        gates: None,
        caps: Vec::new(),
        drive_rms_v: Vec::new(),
        law: law_ref(),
        faults: Vec::new(),
        override_ack: Vec::new(),
        prediction: None,
        excluded: Vec::new(),
        end: None,
        streaming: false,
        record: None,
    }
}

fn gates() -> Gates {
    Gates {
        coherence_min: 0.962,
        coherence_floor: 0.5,
        condition_max: 12.5,
        condition_limit: 1000.0,
        coherence: vec![0.962, 0.981],
    }
}

#[test]
fn the_setup_validates_to_the_golden_report_and_state() {
    let request = golden("request_mimo_random_setup.json");
    let parsed = sds_control::protocol::parse_request(&request).unwrap();
    assert_eq!(parsed.method, "mimo_random.setup");
    let setup_value = &parsed.params["setup"];
    let validation = parse(setup_value);
    assert!(validation.ok(), "{:?}", validation.report);
    let want = golden("response_mimo_random_setup.json");
    same(
        &validation.to_json(),
        &want["result"]["validation"],
        "validation",
    );

    let cat = catalog();
    let lookup = |id: &str| cat.entry(id).map(|e| laws::law_info(&e));
    let table = two_by_two();
    let ctx = SetupContext {
        table: &table,
        selected_kind: "sim",
        law: &lookup,
    };
    let (s, _) = setup::parse(setup_value, &ctx).unwrap();
    assert_eq!(s.lines_hz().len(), 8);
    assert_eq!(s.loop_s(), 2.25);
    assert_eq!(s.schedule.mode, RunMode::Auto);
    let facts = RunFacts {
        run_id: "m-0003",
        setup: &s,
        station: &station(),
        grid: &grid(),
        clock: None,
    };
    same(
        &view::status_json(Some((&facts, &configured()))),
        &want["result"]["state"],
        "state",
    );
}

#[test]
fn an_unknown_control_channel_is_refused_with_the_whole_report() {
    let mut setup = golden("request_mimo_random_setup.json")["params"]["setup"].clone();
    setup["channels"]["controls"][0]["channel"] = Value::from(9);
    let validation = parse(&setup);
    assert!(!validation.ok());
    let e = setup::setup_invalid(&validation.to_json(), validation.report.errors.len());
    let want = golden("response_error_setup_invalid.json");
    same(
        &sds_control::protocol::error_response(32, &e),
        &want,
        "error",
    );
}

#[test]
fn a_running_status_is_the_golden_event() {
    let want = golden("event_mimo_random_status.json");
    let setup_value = golden("request_mimo_random_setup.json")["params"]["setup"].clone();
    let cat = catalog();
    let lookup = |id: &str| cat.entry(id).map(|e| laws::law_info(&e));
    let table = two_by_two();
    let ctx = SetupContext {
        table: &table,
        selected_kind: "sim",
        law: &lookup,
    };
    let (s, _) = setup::parse(&setup_value, &ctx).unwrap();
    let status = MimoStatus {
        state: State::Running,
        phase: "engaged",
        elapsed_s: Some(31.5),
        level_db: Some(-6.0),
        at_level_s: Some(4.5),
        remaining_at_level_s: Some(5.5),
        loop_index: 14,
        control_error_db: vec![0.42, -0.31],
        gates: Some(gates()),
        caps: vec![1.0, 0.875],
        drive_rms_v: vec![0.412, 0.388],
        prediction: Some(Prediction {
            iteration: 1,
            law: law_ref(),
            drive_rms_v: vec![0.82, 0.77],
            ceiling_vrms: vec![2.0, 2.0],
            error_db: vec![0.5, -0.5],
            has_achievable: false,
            unreachable_db: vec![0.0, 0.0],
            drive_accel: vec![1.25, 1.1],
            accel_control: vec![0, 1],
            feasibility: vec![
                DriveVerdict {
                    verdict: "pass",
                    message: String::new(),
                },
                DriveVerdict {
                    verdict: "not_evaluated",
                    message: "no shaker record".into(),
                },
            ],
        }),
        streaming: true,
        record: Some(sds_control::random::view::RecordRef {
            folder: "/data/runs/20261113-140000_mimo_random_m-0003".into(),
            streams: true,
            problem: None,
        }),
        ..configured()
    };
    let facts = RunFacts {
        run_id: "m-0003",
        setup: &s,
        station: &station(),
        grid: &grid(),
        clock: None,
    };
    let event = sds_control::protocol::event(
        "mimo_random.status",
        77,
        view::status_json(Some((&facts, &status))),
    );
    same(&event, &want, "status event");
    // The unconfigured service's status validates against the same keys.
    let none = view::status_json(None);
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&none), keys(&want["data"]), "every key, in order");
}

#[test]
fn the_report_is_the_golden_report() {
    let want = golden("response_mimo_random_report.json");
    let setup_value = golden("request_mimo_random_setup.json")["params"]["setup"].clone();
    let cat = catalog();
    let lookup = |id: &str| cat.entry(id).map(|e| laws::law_info(&e));
    let table = two_by_two();
    let ctx = SetupContext {
        table: &table,
        selected_kind: "sim",
        law: &lookup,
    };
    let (s, _) = setup::parse(&setup_value, &ctx).unwrap();
    let report = MimoReport {
        law: law_ref(),
        end: Some(End {
            outcome: "completed",
            message: "The schedule finished.".into(),
            at_s: Some(102.5),
            level_db: Some(0.0),
            trip: None,
        }),
        loops: 52,
        control_rms: vec![0.58214, 0.55279],
        reference_rms: vec![0.565685, 0.565685],
        control_error_db: vec![0.25, -0.2],
        drive_rms_v: vec![0.8231, 0.7764],
        min_cap: vec![0.875, 1.0],
        gates: Some(gates()),
        override_ack: Vec::new(),
    };
    let facts = RunFacts {
        run_id: "m-0003",
        setup: &s,
        station: &station(),
        grid: &grid(),
        clock: None,
    };
    same(
        &serde_json::json!({"report": view::report_json(&facts, &report)}),
        &want["result"],
        "report",
    );
}

#[test]
fn the_catalog_entry_on_the_wire_is_the_golden_one() {
    let want = golden("response_laws_list.json");
    let cat = catalog();
    let entry = cat.entry("pinv_closed").unwrap();
    same(
        &laws::entry_json(&entry),
        &want["result"]["laws"][0],
        "pinv_closed",
    );
    // The list for the MIMO world holds MIMO laws only, the default first.
    let list = laws::list_json(&cat, Some(hertzforge_random_laws::LawWorld::Mimo));
    let laws = list["laws"].as_array().unwrap();
    assert_eq!(laws[0]["id"], "pinv_closed");
    assert!(laws.iter().all(|l| l["world"] == "mimo"));
    let both = laws::list_json(&cat, None);
    let worlds: Vec<&str> = both["laws"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["world"].as_str().unwrap())
        .collect();
    assert_eq!(worlds.first(), Some(&"single"), "single-output laws first");
    assert!(worlds.contains(&"mimo"));
}

#[test]
fn the_law_requests_parse() {
    let r = sds_control::protocol::parse_request(&golden("request_laws_list.json")).unwrap();
    assert_eq!(
        laws::parse_world(&r.params).unwrap(),
        Some(hertzforge_random_laws::LawWorld::Mimo)
    );
    let r = sds_control::protocol::parse_request(&golden("request_laws_import.json")).unwrap();
    assert!(r.params["path"].as_str().unwrap().ends_with(".wasm"));
    let r =
        sds_control::protocol::parse_request(&golden("request_mimo_random_override.json")).unwrap();
    assert!(!r.params["ack"].as_str().unwrap().trim().is_empty());
}

#[test]
fn a_reference_that_is_not_positive_semi_definite_is_refused_by_line() {
    let mut setup = golden("request_mimo_random_setup.json")["params"]["setup"].clone();
    // Line 3: a cross spectrum larger than either PSD allows (coherence 4).
    setup["reference"]["re"][3 * 3 + 1] = Value::from(0.08);
    setup["reference"]["im"][3 * 3 + 1] = Value::from(0.0);
    let v = parse(&setup);
    let item = v
        .report
        .errors
        .iter()
        .find(|i| i.reason == "reference_not_psd")
        .expect("refused");
    assert!(item.message.contains("118.75 Hz"), "{}", item.message);
    // An imaginary diagonal is not a PSD.
    let mut setup = golden("request_mimo_random_setup.json")["params"]["setup"].clone();
    setup["reference"]["im"][0] = Value::from(0.001);
    let v = parse(&setup);
    assert!(v
        .report
        .errors
        .iter()
        .any(|i| i.reason == "reference_not_hermitian"));
}

#[test]
fn virtual_control_reports_rank_and_condition_of_its_transformations() {
    let mut setup = golden("request_mimo_random_setup.json")["params"]["setup"].clone();
    setup["space"] = serde_json::json!({
        "kind": "virtual", "dofs": ["X", "Y"],
        "response_transform": {"rows": 2, "cols": 2, "values": [1.0, 1.0, 1.0, 1.0]},
        "drive_transform": {"rows": 2, "cols": 2, "values": [1.0, 0.0, 0.0, 2.0]},
    });
    let v = parse(&setup);
    let r = v.response_transform.as_ref().unwrap();
    assert_eq!((r.rank, r.rows, r.cols), (1, 2, 2));
    assert!(v
        .report
        .errors
        .iter()
        .any(|i| i.reason == "transform_rank_deficient"));
    let d = v.drive_transform.as_ref().unwrap();
    assert_eq!(d.rank, 2);
    assert!((d.condition - 2.0).abs() < 1e-12);
    let j = v.to_json();
    assert_eq!(j["transforms"]["response"]["condition"], Value::Null);
    assert_eq!(j["shape"]["space"], "virtual");
}

#[test]
fn a_law_that_does_not_admit_the_shape_or_a_parameter_is_named() {
    let mut setup = golden("request_mimo_random_setup.json")["params"]["setup"].clone();
    setup["law"] = serde_json::json!({"id": "hinv"});
    let v = parse(&setup);
    assert!(v
        .report
        .errors
        .iter()
        .any(|i| i.reason == "law_wrong_world"));
    assert!(!v.law_admitted);
    setup["law"] =
        serde_json::json!({"id": "pinv_closed", "params": {"convergence": 7, "speed": 1}});
    let v = parse(&setup);
    let reasons: Vec<&str> = v.report.errors.iter().map(|i| i.reason).collect();
    assert!(reasons.contains(&"law_param_invalid"), "{reasons:?}");
    assert!(reasons.contains(&"law_param_unknown"), "{reasons:?}");
    setup["law"] = serde_json::json!({"id": "nonesuch"});
    let v = parse(&setup);
    assert!(v.report.errors.iter().any(|i| i.reason == "law_unknown"));
}

fn read_bin(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../schema/golden")
            .join(name),
    )
    .unwrap()
}

/// `event_mimo_random_data`, built with the service's own display builders from the
/// quantities a driver returns.
#[test]
fn the_mimo_data_frame_is_built_byte_for_byte() {
    use sds_control::codec::{self, Arrays};
    use sds_control::mimo::display::{self, MimoPlot, PlotContext, Selector};
    use sds_control::mimo::driver::{Curve, PackedBlock, Series};
    use sds_control::views::Role;
    use std::sync::Arc;

    let lines: Vec<f64> = (0..4).map(|k| 100.0 + 6.25 * k as f64).collect();
    let rep = |v: [f64; 3]| v.repeat(3);
    let labels = vec!["Control X".to_owned(), "Control Y".to_owned()];
    let drive_labels = vec!["Shaker X".to_owned(), "Shaker Y".to_owned()];
    let ctx = PlotContext {
        labels: &labels,
        drive_labels: &drive_labels,
        unit: "g",
        bands: vec![([3.0, 3.0], [6.0, 6.0]); 2],
        df_hz: 6.25,
    };
    let plot = |id: &str, quantity: &'static str, selector: Selector| MimoPlot {
        id: id.into(),
        quantity,
        selector,
        points: 64,
    };
    let status = MimoStatus {
        state: State::Running,
        phase: "full_level",
        streaming: true,
        record: None,
        ..configured()
    };
    let plots = [
        (
            plot("grid", "sdm", Selector::Block(vec![0, 1])),
            Series::Packed {
                freqs: Arc::new(lines[..3].to_vec()),
                measured: PackedBlock {
                    n: 2,
                    re: rep([0.0390625, 0.01171875, 0.041015625]),
                    im: rep([0.0, 0.015625, 0.0]),
                },
                reference: Some(PackedBlock {
                    n: 2,
                    re: rep([0.0390625, 0.01171875, 0.0390625]),
                    im: rep([0.0, 0.015625, 0.0]),
                }),
            },
        ),
        (
            plot("coh01", "coh", Selector::Pair(0, 1)),
            Series::Lines {
                freqs: Arc::new(lines.clone()),
                curves: vec![
                    Curve {
                        name: "Measured".into(),
                        role: Role::Measured,
                        y: vec![0.25, 0.2421875, 0.25, 0.234375],
                    },
                    Curve {
                        name: "Reference".into(),
                        role: Role::Reference,
                        y: vec![0.25; 4],
                    },
                ],
            },
        ),
        (
            plot("heat", "error_db", Selector::Rows(None)),
            Series::Rows {
                freqs: Arc::new(lines.clone()),
                rows: vec![
                    vec![0.5, -0.25, 0.125, 0.0],
                    vec![-0.5, 0.25, 0.375, -0.125],
                ],
            },
        ),
    ];
    let mut arrays = Arrays::new();
    let plots: Vec<Value> = plots
        .into_iter()
        .map(|(p, s)| {
            let views = display::plot_views(&p, &ctx, &status, &[Some(s)], &mut arrays);
            display::plot_json(&p, display::plot_labels(&p, &ctx), views)
        })
        .collect();
    let message = serde_json::json!({
        "type": "event", "topic": "mimo_random.data", "seq": 120,
        "data": {
            "run_id": "m-0003", "source": "SIM", "state": "running", "phase": "full_level",
            "loop": 30, "elapsed_s": 70.5, "level_db": 0, "plots": plots,
        },
    });
    let bytes = read_bin("event_mimo_random_data.bin");
    assert_eq!(
        codec::encode(&message, &arrays).unwrap(),
        bytes,
        "byte for byte"
    );
    let d = codec::decode(&bytes).unwrap();
    same(
        &d.expanded(),
        &golden("event_mimo_random_data.expected.json"),
        "expected",
    );
    let order = codec::header_array_order(&bytes).unwrap();
    let order: Vec<&str> = order.iter().map(String::as_str).collect();
    assert_eq!(codec::reencode(&d, &order).unwrap(), bytes, "round trip");
    // The packed block: two rows per line by three values, real and imaginary apart.
    let sdm = &d.message["data"]["plots"][0]["views"][0];
    let re = d.array(&sdm["measured"]["re"]).unwrap();
    assert_eq!(re.shape, vec![3, 3]);
    let heat = d
        .array(&d.message["data"]["plots"][2]["views"][0]["values"])
        .unwrap();
    assert_eq!(heat.shape, vec![2, 4]);
    assert!(matches!(heat.data, sds_control::codec::ArrayData::F32(_)));
}

#[test]
fn the_coherence_error_heatmap_subscribes_by_rows() {
    use sds_control::mimo::display;
    use sds_control::mimo::driver::QuantitySelector;
    let request = golden("request_mimo_random_subscribe_coh_error.json");
    let parsed = sds_control::protocol::parse_request(&request).unwrap();
    assert_eq!(parsed.method, "mimo_random.subscribe");
    let sub = display::parse(&parsed.params).unwrap().expect("plots");
    assert_eq!(sub.plots.len(), 2);
    assert_eq!(sub.plots[1].quantity, "coh_error");
    assert_eq!(
        display::resolve(&sub.plots[1], 3, 2),
        vec![QuantitySelector::CohError(vec![0, 1, 2])]
    );
    assert_eq!(
        display::resolve(&sub.plots[0], 3, 2),
        vec![QuantitySelector::ErrorDb(vec![0, 1, 2])]
    );
    assert!(display::MIMO_QUANTITIES.contains(&"coh_error"));
}

#[test]
fn the_identification_goldens_name_a_folder_and_describe_the_set() {
    let request = golden("request_mimo_random_identification_load.json");
    let parsed = sds_control::protocol::parse_request(&request).unwrap();
    assert_eq!(parsed.method, "mimo_random.identification.load");
    assert!(parsed.params["path"]
        .as_str()
        .is_some_and(|p| !p.is_empty()));
    let info = sds_control::mimo::driver::IdentificationInfo {
        path: "/home/operator/hertzforge/identification/two_axis".into(),
        setup_hash: "9f3c2a7e5b1d0c84".into(),
        frames: 64,
        level_db: -20.3,
        drive_vrms: vec![0.0512, 0.0498],
    };
    let want = golden("response_mimo_random_identification_load.json");
    same(
        &serde_json::json!({ "identification": info.to_json() }),
        &want["result"],
        "identification",
    );
}

#[test]
fn a_prediction_override_names_its_reason() {
    let r = sds_control::protocol::parse_request(&golden(
        "request_mimo_random_override_prediction.json",
    ))
    .unwrap();
    assert_eq!(r.method, "mimo_random.override");
    assert_eq!(
        OverrideReason::from_name(r.params["reason"].as_str().unwrap()),
        Some(OverrideReason::Prediction)
    );
    assert!(!r.params["ack"].as_str().unwrap().trim().is_empty());
}

#[test]
fn the_virtual_setup_golden_carries_every_field_to_the_setup() {
    let request = golden("request_mimo_random_setup_virtual.json");
    let parsed = sds_control::protocol::parse_request(&request).unwrap();
    let cat = catalog();
    let lookup = |id: &str| cat.entry(id).map(|e| laws::law_info(&e));
    let mut table = two_by_two();
    let mut m = table::new_row(5);
    m.description = "Fixture Top".into();
    m.role = ChannelRole::Monitor;
    table.channels.push(m);
    let ctx = SetupContext {
        table: &table,
        selected_kind: "sim",
        law: &lookup,
    };
    let (s, report) = setup::parse(&parsed.params["setup"], &ctx).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        s.dof_bands,
        vec![([2.0, 2.0], [4.0, 4.0]), ([3.0, 3.0], [6.0, 6.0])]
    );
    assert_eq!(s.band_of(0), ([2.0, 2.0], [4.0, 4.0]));
    assert_eq!(s.drives[0].peak_v, 6.0);
    assert_eq!(s.drives[1].moving_mass_lb, 12.5);
    assert_eq!(s.limits.len(), 1);
    assert_eq!(s.limits[0].channel, 5);
    assert_eq!(s.article_mass_lb, 40.0);
    assert_eq!(s.control.frf_blend, 0.25);
    assert_eq!(s.control.law_misses, 2);
    // The setup as the service echoes it carries the bands and the new fields.
    let echo = setup::setup_json(&s);
    assert_eq!(echo["space"]["dof_bands"][0]["abort_db"][0], 4.0);
    assert_eq!(echo["channels"]["limits"][0]["ceiling"], 0.04);
    assert_eq!(echo["control"]["cap_down_db"], 4.0);
}

#[test]
fn the_achievable_reference_subscribes_as_a_block() {
    use sds_control::mimo::display;
    use sds_control::mimo::driver::QuantitySelector;
    let request = golden("request_mimo_random_subscribe_pred_sdm.json");
    let parsed = sds_control::protocol::parse_request(&request).unwrap();
    let sub = display::parse(&parsed.params).unwrap().expect("plots");
    assert_eq!(sub.plots[0].quantity, "pred_sdm");
    assert_eq!(
        display::resolve(&sub.plots[0], 3, 2),
        vec![QuantitySelector::PredSdmBlock(vec![0, 1])]
    );
    assert_eq!(sub.plots[1].quantity, "pred_drive_accel");
}
