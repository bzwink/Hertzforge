//! The SRS shock goldens in `schema/golden/`, built by this crate's own builders: a
//! configuration parses and checks to the golden derived values and state, the
//! binary frames encode to the golden bytes exactly, the text frames are the objects
//! the service sends, and the law catalog's SRS entry and check report are the
//! golden ones.

use hertzforge_channel_table::{ChannelRole, ChannelTable, Units};
use hertzforge_random_laws::CheckCase;
use hertzforge_shock_common::ShockRunMode;
use hertzforge_shock_laws::{SrsCaseEnd, SrsCaseMeasure, SrsCheckCase};
use sds_control::codec::{self, Arrays, Frame};
use sds_control::protocol;
use sds_control::srs::config::{self, CheckContext, Grid, LawFacts};
use sds_control::srs::law_check;
use sds_control::srs::preview::{self, Edges, PreviewFigures, ReferenceFacts, SynthesisFacts};
use sds_control::srs::subscribe::{self, DataContext};
use sds_control::srs::view::{
    self, ComponentFigures, Counts, FrfFigures, IdentFacts, IdentFigures, LawCall, LawReading,
    LawRef, LimitFigures, Notch, Prediction, PulseFigures, Row, SrsFigures, SrsView, State,
    StationRef,
};
use sds_control::table;
use serde_json::{json, Value};
use std::path::PathBuf;

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema/golden")
        .join(name)
}

fn read_json(name: &str) -> Value {
    let text = std::fs::read_to_string(golden(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// The built-in SRS law's entry as the catalog lists it, from a catalog on an
/// empty data folder.
fn builtin_srs_entry() -> Value {
    let dir = std::env::temp_dir().join(format!(
        "sds-control-srs-golden-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        // the clock alone repeats: macOS reads it to the microsecond, and two
        // tests running at once could share a folder and delete it under each other
        {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            NEXT.fetch_add(1, Ordering::Relaxed)
        }
    ));
    let cat = hertzforge_random_laws::LawCatalog::open(&dir).unwrap();
    let entry = sds_control::laws::entry_json(&cat.entry("srs_ratio").unwrap());
    let _ = std::fs::remove_dir_all(&dir);
    entry
}

fn read_bin(name: &str) -> Vec<u8> {
    std::fs::read(golden(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
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

fn bytes_equal(name: &str, got: &[u8], want: &[u8]) {
    if got != want {
        let at = got.iter().zip(want).position(|(a, b)| a != b);
        panic!(
            "{name}: {} bytes encoded vs {} golden; first difference at {at:?}\n got: {}\nwant: {}",
            got.len(),
            want.len(),
            String::from_utf8_lossy(&got[..got.len().min(400)]),
            String::from_utf8_lossy(&want[..want.len().min(400)]),
        );
    }
}

/// A control row (1), a current monitor (2), the fixture's accelerometer (3, the
/// limit channel) and a drive (4), as the golden configuration names them.
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

fn law_lookup(id: &str) -> Option<LawFacts> {
    (id == "srs_ratio").then(sds_control::laws::builtin_srs_facts)
}

fn check(config: &Value) -> (sds_control::table::Report, Option<config::Derived>) {
    let w = config::parse(config).unwrap();
    let t = table();
    let ctx = CheckContext {
        table: &t,
        law: &law_lookup,
        timeout_s: 30.0,
    };
    w.check(&ctx)
}

fn sim_station() -> StationRef {
    StationRef {
        kind: "sim",
        class: "sim",
        devices: Vec::new(),
    }
}

fn law_ref() -> LawRef {
    LawRef {
        id: "srs_ratio".into(),
        name: "SRS ratio".into(),
        version: "0.1.0".into(),
        sha256: String::new(),
    }
}

// --- the golden run's figures (schema/make_golden.py, "SRS shock") -------------------

const FS: f64 = 1000.0;
const BANDS: [f64; 3] = [100.0, 200.0, 400.0];
const TARGET: [f64; 3] = [12.5, 15.0, 11.0];
const ALARM_HI: [f64; 3] = [17.5, 21.0, 15.5];
const ALARM_LO: [f64; 3] = [9.0, 10.5, 7.75];
const ABORT_HI: [f64; 3] = [25.0, 30.0, 22.0];
const ABORT_LO: [f64; 3] = [6.25, 7.5, 5.5];
const MEASURED_SRS: [f64; 3] = [12.25, 15.5, 10.5];
const PULSE: [f64; 8] = [0.0, 0.5, -1.25, 2.5, -3.0, 1.5, -0.5, 0.0];
const DRIVE: [f64; 8] = [0.0, 0.0125, -0.03125, 0.0625, -0.075, 0.0375, -0.0125, 0.0];
const VELOCITY: [f64; 8] = [0.0, 0.25, -0.125, 0.5, -0.5, 0.25, 0.0, 0.0];
const DISPLACEMENT: [f64; 8] = [0.0, 0.0001, 0.0002, 0.0003, 0.0002, 0.0001, 0.0, 0.0];

fn edges() -> Edges {
    Edges {
        alarm_hi: ALARM_HI.to_vec(),
        alarm_lo: ALARM_LO.to_vec(),
        abort_hi: ABORT_HI.to_vec(),
        abort_lo: ABORT_LO.to_vec(),
    }
}

fn pulse_figures() -> PulseFigures {
    PulseFigures {
        fs_hz: FS,
        srs: Some(SrsFigures {
            bands_hz: BANDS.to_vec(),
            target: TARGET.to_vec(),
            edges: edges(),
            measured: Some(MEASURED_SRS.to_vec()),
        }),
        measured: Some(PULSE.to_vec()),
        drive: DRIVE.to_vec(),
        velocity: Some(VELOCITY.to_vec()),
        displacement: Some(DISPLACEMENT.to_vec()),
        components: Some(ComponentFigures {
            freqs_hz: BANDS.to_vec(),
            amplitude_v: vec![0.125, -0.25, 0.0625],
            gains: vec![1.0, 1.125, 0.875],
        }),
        limits: vec![LimitFigures {
            name: "Fixture".into(),
            bands_hz: BANDS.to_vec(),
            predicted: vec![10.0, 45.0, 20.0],
            bound: vec![50.0, f64::INFINITY, 50.0],
            headroom_db: 0.0,
        }],
    }
}

fn row() -> Row {
    Row {
        index: 2,
        scheduled_db: -6.0,
        commanded_db: -6.0,
        limit_scale: 1.0,
        drive_vpk: 0.205,
        peak_g: Some(36.3),
        velocity_pk_ips: Some(7.1),
        displacement_pk_pk_in: Some(0.016),
        worst_err_db: Some(-1.2),
        worst_band_hz: Some(630.0),
        alarm_bands: Some(0),
        abort_bands: Some(0),
        passed: Some(true),
        limited_components: Some(2),
        worst_limit_db: Some(-0.7),
        at_s: 7.5,
        law: Some(LawCall {
            used: true,
            miss: None,
            call_ms: 0.2,
        }),
    }
}

/// The golden run between pulses: -6 dB in AUTO, two of three pulses passed.
fn running() -> SrsView {
    SrsView {
        run_id: "srs-0004".into(),
        title: "SRS 100 g, 100 to 2000 Hz".into(),
        state: State::Running,
        phase: "waiting",
        station: sim_station(),
        grid: Grid {
            fs_hz: 51_200.0,
            record_len: 14_336,
            actual: true,
        },
        clock: Some("shared"),
        mode: ShockRunMode::Auto,
        levels_db: vec![-12.0, -6.0, 0.0],
        level_db: Some(-6.0),
        rung: Some(1),
        pulses: Counts {
            fired: 3,
            passed: 2,
            at_level: 1,
            of_level: 2,
        },
        next_pulse_s: Some(0.6),
        elapsed_s: Some(9.5),
        alarm: false,
        identification: Some(IdentFacts {
            excitation: "shaped",
            drive_vpk: 0.05,
            max_vpk: 0.05,
            coherence_mean: 0.97,
            min_mean_coherence: 0.8,
            passed: true,
        }),
        prediction: Some(Prediction {
            drive_vpk: 0.41,
            drive_vpk_ceiling: 0.5,
            drive_vpk_abort: 0.8,
            response_peak_g: 72.1,
            velocity_pk_ips: 14.1,
            displacement_pk_pk_in: 0.031,
        }),
        notch: Some(Notch {
            notched_bands: 2,
            worst_before_db: Some(3.1),
            worst_after_db: Some(-0.6),
            converged: true,
        }),
        last_pulse: Some(row()),
        law: Some(law_ref()),
        misses: 0,
        feasibility: Some(("pass", "The pulse is within the shaker's ratings.".into())),
        end: None,
        fire: None,
        streaming: true,
        record: None,
    }
}

fn identification() -> IdentFigures {
    IdentFigures {
        freqs_hz: vec![100.0, 200.0, 300.0],
        gain: vec![0.5, 1.0, 0.75],
        floor: 0.05,
        coherence: vec![0.99, 0.5, 0.98],
        hold: 0.8,
        unit: "g".into(),
        limit_frf: vec![FrfFigures {
            name: "Fixture".into(),
            freqs_hz: vec![100.0, 500.0, 1000.0],
            mag: vec![1.0, 4.0, 1.0],
            measured: false,
        }],
    }
}

fn law_readings() -> Vec<LawReading> {
    vec![
        LawReading {
            label: "Worst band error".into(),
            value: -1.2,
            unit: "dB".into(),
        },
        LawReading {
            label: "Worst band".into(),
            value: 630.0,
            unit: "Hz".into(),
        },
        LawReading {
            label: "Components at the change limit".into(),
            value: 0.0,
            unit: "components".into(),
        },
    ]
}

fn event_srs_live() -> (Value, Arrays) {
    let mut v = running();
    v.phase = "firing";
    v.next_pulse_s = None;
    v.fire = Some((4096, 14_336));
    (
        protocol::event("srs.live", 149, view::live_json(&v)),
        Arrays::new(),
    )
}

fn event_srs_pulse() -> (Value, Arrays) {
    let mut arrays = Arrays::new();
    let views = view::default_pulse_views(&pulse_figures(), Some(1024), &mut arrays);
    let data = json!({"run_id": "srs-0004", "pulse": row().to_json(), "views": views});
    (protocol::event("srs.pulse", 151, data), arrays)
}

fn event_srs_data() -> (Value, Arrays) {
    let request = read_json("request_srs_subscribe.json");
    let parsed = protocol::parse_request(&request).unwrap();
    let sub = subscribe::parse(&parsed.params).unwrap().unwrap();
    let v = running();
    let figures = pulse_figures();
    let ident = identification();
    let law = law_readings();
    let rows = vec![row()];
    let ctx = DataContext {
        view: &v,
        last: Some(&figures),
        identification: Some(&ident),
        law: &law,
        rows: &rows,
    };
    let mut arrays = Arrays::new();
    let data = subscribe::data_json(&ctx, &sub, &mut arrays);
    (protocol::event("srs.data", 152, data), arrays)
}

fn response_srs_preview() -> (Value, Arrays) {
    let mut arrays = Arrays::new();
    let facts = SynthesisFacts {
        engine: "wavsyn",
        components: 3,
        iters: 6,
        converged: true,
        worst_err_db: Some(1.2),
        duration_s: 0.008,
    };
    let reference = ReferenceFacts {
        peak_g: 3.0,
        velocity_pk_ips: 0.5,
        displacement_pk_pk_in: 0.0003,
        residual_velocity_ips: 0.0,
        residual_disp_in: 0.0,
    };
    let figures = PreviewFigures {
        fs_hz: FS,
        bands_hz: BANDS.to_vec(),
        target: TARGET.to_vec(),
        edges: edges(),
        achieved: vec![12.0, 15.25, 11.5],
        waveform: PULSE.to_vec(),
        velocity: VELOCITY.to_vec(),
        displacement: DISPLACEMENT.to_vec(),
    };
    let result = preview::preview_frame(&facts, &reference, &figures, Some(1024), &mut arrays);
    (protocol::response(63, result), arrays)
}

const BINARY: [&str; 4] = [
    "event_srs_live",
    "event_srs_pulse",
    "event_srs_data",
    "response_srs_preview",
];

fn build(name: &str) -> (Value, Arrays) {
    match name {
        "event_srs_live" => event_srs_live(),
        "event_srs_pulse" => event_srs_pulse(),
        "event_srs_data" => event_srs_data(),
        "response_srs_preview" => response_srs_preview(),
        other => panic!("no builder for {other}"),
    }
}

#[test]
fn encoding_reproduces_the_golden_bytes() {
    for name in BINARY {
        let (message, arrays) = build(name);
        let got = codec::encode(&message, &arrays).unwrap();
        bytes_equal(name, &got, &read_bin(&format!("{name}.bin")));
    }
}

#[test]
fn decoding_matches_the_expected_json_and_re_encodes_byte_for_byte() {
    for name in BINARY {
        let bytes = read_bin(&format!("{name}.bin"));
        let decoded = codec::decode(&bytes).unwrap();
        same(
            &decoded.expanded(),
            &read_json(&format!("{name}.expected.json")),
            name,
        );
        let order = codec::header_array_order(&bytes).unwrap();
        let order: Vec<&str> = order.iter().map(String::as_str).collect();
        assert_eq!(codec::reencode(&decoded, &order).unwrap(), bytes, "{name}");
    }
}

#[test]
fn the_configuration_checks_to_the_golden_derived_values_and_state() {
    let request = read_json("request_srs_configure.json");
    let parsed = protocol::parse_request(&request).unwrap();
    assert_eq!(parsed.method, "srs.configure");
    let config = &parsed.params["config"];
    let (report, derived) = check(config);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let derived = derived.unwrap();
    let want = read_json("response_srs_configure.json");
    same(&derived.to_json(), &want["result"]["derived"], "derived");
    same(
        &Value::Array(report.warnings.iter().map(|i| i.to_json()).collect()),
        &want["result"]["warnings"],
        "warnings",
    );
    let w = config::parse(config).unwrap();
    let v = view::configured_view("srs-0001", &w, &derived, sim_station(), law_ref());
    same(
        &view::state_json(Some(&v)),
        &want["result"]["state"],
        "state",
    );
}

#[test]
fn the_running_state_is_the_golden_state_event() {
    let want = read_json("event_srs_state.json");
    assert_eq!(want["topic"], "srs.state");
    same(&view::state_json(Some(&running())), &want["data"], "state");
}

#[test]
fn the_subscription_parses_to_the_golden_result() {
    let request = read_json("request_srs_subscribe.json");
    let parsed = protocol::parse_request(&request).unwrap();
    assert_eq!(parsed.method, "srs.subscribe");
    let sub = subscribe::parse(&parsed.params).unwrap().unwrap();
    same(
        &subscribe::result_json(Some(&sub)),
        &read_json("response_srs_subscribe.json")["result"],
        "subscribe result",
    );
}

#[test]
fn the_fire_and_level_requests_parse() {
    let fire = protocol::parse_request(&read_json("request_srs_fire.json")).unwrap();
    assert_eq!((fire.method.as_str(), fire.params.len()), ("srs.fire", 0));
    let level = protocol::parse_request(&read_json("request_srs_level.json")).unwrap();
    assert_eq!(level.method, "srs.level");
    assert_eq!(level.params["delta_db"], json!(1));
}

#[test]
fn the_imports_read_the_golden_files_to_the_golden_results() {
    let dir = std::env::temp_dir().join(format!("sds-srs-golden-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("target.csv");
    std::fs::write(&target, "freq_hz,srs_g\n100,40\n1000,100\n2500,100\n").unwrap();
    let got = sds_control::srs::import::target_import(target.to_str().unwrap()).unwrap();
    same(
        &got,
        &read_json("response_srs_target_import.json")["result"],
        "target import",
    );
    let table = dir.join("table.tsv");
    std::fs::write(
        &table,
        "freq\tamp\thalfcycles\tdelay\n100\t1.5\t5\t0.04\n200\t1.25\t7\t0.02\n400\t1\t9\t0\n",
    )
    .unwrap();
    let got = sds_control::srs::import::table_import(
        table.to_str().unwrap(),
        hertzforge_srs::service::config::EngineKind::Wavsyn,
    )
    .unwrap();
    same(
        &got,
        &read_json("response_srs_table_import.json")["result"],
        "table import",
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_srs_law_entry_is_the_golden_one() {
    let want = read_json("response_laws_list_shock_srs.json");
    same(&builtin_srs_entry(), &want["result"]["laws"][0], "entry");
    assert_eq!(want["result"]["problems"], json!([]));
}

/// A golden case, its detail in the words the golden gives it.
#[allow(clippy::too_many_arguments)]
fn measured_case(
    case: SrsCheckCase,
    pulses: u32,
    first: u32,
    worst: f64,
    limit_over: Option<f64>,
    limited: u64,
    calls: u32,
    ms: f64,
    vpk: f64,
) -> Value {
    let m = SrsCaseMeasure {
        end: SrsCaseEnd::Completed,
        pulses,
        full_level_pulses: 3,
        first_within_pulse: Some(first),
        within_after_first: true,
        final_worst_band_db: Some(worst),
        final_worst_outside_notch_db: Some(worst),
        levels: vec![(-12.0, Some(0.5)), (-6.0, Some(0.4)), (0.0, Some(worst))],
        worst_limit_over_db: limit_over,
        calls,
        misses: 0,
        refused: 0,
        limited_components: limited,
        worst_call_ms: Some(ms),
        budget_ms: 250.0,
        max_drive_vpk: vpk,
        drive_ceiling_vpk: 0.5,
    };
    let v = CheckCase {
        name: case.name().into(),
        passed: true,
        detail: format!(
            "completed; {pulses} pulses; every band within the band from full-level pulse \
             {first}; {calls} calls, slowest {ms} ms of 250 ms"
        ),
    };
    law_check::case_json(case, &m, &v)
}

#[test]
fn the_srs_check_report_is_the_golden_one() {
    use SrsCheckCase::*;
    let cases = vec![
        measured_case(OneMode, 9, 1, 0.6, None, 0, 10, 0.1, 0.41),
        measured_case(CloseModes, 9, 2, 1.1, None, 0, 10, 0.1, 0.43),
        measured_case(Noise, 9, 1, 0.9, None, 0, 10, 0.1, 0.41),
        measured_case(Ladder, 9, 1, 0.7, None, 0, 10, 0.1, 0.41),
        measured_case(IdentificationError, 9, 2, 0.8, None, 0, 10, 0.1, 0.42),
        measured_case(Limit, 9, 1, 1.4, Some(0.3), 6, 10, 0.1, 0.38),
    ];
    let want = read_json("response_laws_check_shock_srs.json");
    same(
        &law_check::report_json(&cases, true, false),
        &want["result"]["report"],
        "report",
    );
    same(&builtin_srs_entry(), &want["result"]["law"], "law");
}

#[test]
fn a_configuration_with_errors_is_refused_as_the_golden_error() {
    let mut config = read_json("request_srs_configure.json")["params"]["config"].clone();
    config["limits"]["drive_vpk_abort"] = json!(0.4);
    config["envelope"]["abort_db"] = json!([2, 2]);
    config["limit_channels"]["channels"][0]["channel"] = json!(7);
    let (report, derived) = check(&config);
    assert!(derived.is_none());
    let e = config::config_invalid(&report);
    let want = read_json("response_error_srs_config_invalid.json");
    same(&protocol::error_response(68, &e), &want, "error");
}

#[test]
fn every_srs_text_golden_is_a_compact_text_frame() {
    for name in [
        "request_srs_configure",
        "response_srs_configure",
        "event_srs_state",
        "request_srs_subscribe",
        "response_srs_subscribe",
        "request_srs_fire",
        "request_srs_level",
        "response_srs_target_import",
        "response_srs_table_import",
        "response_laws_list_shock_srs",
        "response_laws_check_shock_srs",
        "response_error_srs_config_invalid",
    ] {
        let text = std::fs::read_to_string(golden(&format!("{name}.json"))).unwrap();
        let message = codec::decode_text(&text).unwrap();
        match codec::encode_frame(&message, &Arrays::new()).unwrap() {
            Frame::Text(t) => same(&serde_json::from_str(&t).unwrap(), &message, name),
            Frame::Binary(_) => panic!("{name}: a message without arrays must be a text frame"),
        }
    }
}
