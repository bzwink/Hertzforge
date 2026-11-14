//! The Explorer goldens in `schema/golden/`, against this crate's own code where it
//! exists: the configuration parses and checks to the golden derived values on the NI
//! station at 12 800 Hz, a configuration with errors is refused as the golden error, the
//! subscription parses to the golden result, the binary frames decode to their expected
//! JSON and re-encode byte for byte, and the text frames are compact text frames.
//!
//! The state, start, live, data and survey goldens are this crate's view builders' own
//! output for one moment of one run, byte for byte.

use hertzforge_channel_table::{ChannelRole, ChannelTable, Units};
use sds_control::codec::{self, Arrays, Frame};
use sds_control::explorer::config::{self, CheckContext, StationFigures};
use sds_control::explorer::subscribe::{self, DataContext};
use sds_control::explorer::view::{
    self, ControlMotion, ExplorerView, Grid, Health, LiveChannel, RecordRef, State, StationRef,
};
use sds_control::{protocol, table};
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

/// A control row (1) in g, a drive row (2) on `drive_range` V, a monitor row (3).
fn table(drive_range: f64) -> ChannelTable {
    let mut t = table::Table::new().table;
    let mut c = table::new_row(1);
    c.role = ChannelRole::Control;
    c.description = "Control".into();
    let mut d = table::new_row(2);
    d.role = ChannelRole::Drive;
    d.units = Units::VoltageV;
    d.range_max = drive_range;
    let mut m = table::new_row(3);
    m.role = ChannelRole::Monitor;
    m.description = "Fixture".into();
    t.channels = vec![c, d, m];
    t
}

fn golden_config() -> Value {
    let request = read_json("request_explorer_configure.json");
    let parsed = protocol::parse_request(&request).unwrap();
    assert_eq!(parsed.method, "explorer.configure");
    parsed.params["config"].clone()
}

fn check(config: &Value, drive_range: f64) -> (table::Report, Option<config::Derived>) {
    let w = config::parse(config).unwrap();
    w.check(&CheckContext {
        table: &table(drive_range),
        timeout_s: sds_control::session::TIMEOUT.as_secs_f64(),
        station: StationFigures::Ni,
    })
}

#[test]
fn explorer_the_configuration_checks_to_the_golden_derived_values() {
    let (report, derived) = check(&golden_config(), 10.0);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let want = read_json("response_explorer_configure.json");
    same(
        &derived.unwrap().to_json(),
        &want["result"]["derived"],
        "derived",
    );
    same(
        &Value::Array(report.warnings.iter().map(|i| i.to_json()).collect()),
        &want["result"]["warnings"],
        "warnings",
    );
    // The configured state states the derived grid and pipeline.
    let state = &want["result"]["state"];
    let d = check(&golden_config(), 10.0).1.unwrap();
    assert_eq!(state["pipeline_frames"], json!(d.pipeline_frames));
    assert_eq!(state["grid"]["block_frames"], json!(d.block_frames));
    assert_eq!(state["grid"]["lookahead_frames"], json!(d.lookahead_frames));
    assert_eq!(state["max_drive_vpk"], golden_config()["max_drive_vpk"]);
    assert_eq!(state["slew"], golden_config()["slew"]);
}

#[test]
fn explorer_a_configuration_with_errors_is_refused_as_the_golden_error() {
    let mut config = golden_config();
    config["band_hz"] = json!([20, 6000]);
    config["max_drive_vpk"] = json!(1.5);
    config["slew"]["drive_db_per_s"] = json!(1);
    let (report, derived) = check(&config, 1.0);
    assert!(derived.is_none());
    let e = config::config_invalid(&report);
    let want = read_json("response_error_explorer_config_invalid.json");
    same(&protocol::error_response(99, &e), &want, "error");
}

#[test]
fn explorer_the_subscription_parses_to_the_golden_result() {
    let request = read_json("request_explorer_subscribe.json");
    let parsed = protocol::parse_request(&request).unwrap();
    assert_eq!(parsed.method, "explorer.subscribe");
    let sub = subscribe::parse(&parsed.params).unwrap().unwrap();
    same(
        &subscribe::result_json(Some(&sub)),
        &read_json("response_explorer_subscribe.json")["result"],
        "subscribe result",
    );
    // The default subscription.
    let d = subscribe::parse(&serde_json::from_value(json!({"default": true})).unwrap()).unwrap();
    assert!(d.is_none());
    assert_eq!(subscribe::result_json(None)["default"], json!(true));
}

#[test]
fn explorer_a_subscription_refuses_a_key_a_quantity_does_not_take() {
    let refused = |plots: Value, param: &str| {
        let params = serde_json::from_value(json!({"plots": plots})).unwrap();
        let e = subscribe::parse(&params).expect_err(param);
        assert_eq!(e.reason, Some("bad_params"), "{param}");
        assert_eq!(e.detail.unwrap()["param"], json!(param));
    };
    refused(
        json!([{"id": "p", "quantity": "spectrum", "points": 512}]),
        "plots[0].points",
    );
    refused(
        json!([{"id": "s", "quantity": "survey", "channels": [1]}]),
        "plots[0].channels",
    );
    refused(
        json!([{"id": "r", "quantity": "readings", "points": 64}]),
        "plots[0].points",
    );
    refused(
        json!([{"id": "s", "quantity": "survey", "points": 32}]),
        "plots[0].points",
    );
    refused(
        json!([{"id": "x", "quantity": "phase"}]),
        "plots[0].quantity",
    );
    refused(
        json!([{"id": "a", "quantity": "time"}, {"id": "a", "quantity": "time"}]),
        "plots[1].id",
    );
}

const BINARY: [&str; 2] = ["event_explorer_live", "event_explorer_data"];

#[test]
fn explorer_the_binary_goldens_decode_to_their_expected_json_and_re_encode_byte_for_byte() {
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
fn explorer_every_text_golden_is_a_compact_text_frame() {
    for name in [
        "request_explorer_configure",
        "response_explorer_configure",
        "request_explorer_start",
        "response_explorer_start",
        "request_explorer_steer",
        "response_explorer_steer",
        "request_explorer_sweep",
        "response_explorer_sweep",
        "event_explorer_state",
        "request_explorer_subscribe",
        "response_explorer_subscribe",
        "response_explorer_survey",
        "response_error_explorer_config_invalid",
    ] {
        let text = std::fs::read_to_string(golden(&format!("{name}.json"))).unwrap();
        let message = codec::decode_text(&text).unwrap();
        match codec::encode_frame(&message, &Arrays::new()).unwrap() {
            Frame::Text(t) => same(&serde_json::from_str(&t).unwrap(), &message, name),
            Frame::Binary(_) => panic!("{name}: a message without arrays must be a text frame"),
        }
    }
}

fn ni_station() -> StationRef {
    StationRef {
        kind: "ni",
        class: "ni_real",
        devices: vec![
            ("cDAQ2Mod1".into(), "01A2B3C4".into()),
            ("cDAQ2Mod2".into(), "01A2B3C5".into()),
        ],
    }
}

fn configured() -> ExplorerView {
    let config = golden_config();
    let w = config::parse(&config).unwrap();
    let d = check(&config, 10.0).1.unwrap();
    view::configured_view("explorer-0001", &w, &d, ni_station(), &table(10.0))
}

fn health(ticks: u64, tick: f64, last: f64, worst: f64, gap: Option<f64>) -> Health {
    Health {
        engine_ticks: ticks,
        worst_tick_ms: Some(tick),
        budget_ms: Some(20.0),
        health_faults: 0,
        ring_overruns: 0,
        blocks_dropped: 0,
        driver_warnings: 2,
        pass_last_ms: Some(last),
        pass_worst_ms: Some(worst),
        batch_gap_ms: gap,
    }
}

fn armed(mut v: ExplorerView) -> ExplorerView {
    v.state = State::Running;
    v.grid = Grid {
        actual: true,
        ..v.grid
    };
    v.clock = Some("shared");
    v.record = Some(RecordRef {
        folder: "/home/operator/Hertzforge_Data/explorer".into(),
        streams: true,
        problem: None,
    });
    v
}

/// The golden run 41.38 s in: the tone held at 250 Hz and 0.2 V pk.
fn running() -> ExplorerView {
    let mut v = armed(configured());
    v.run_id = "explorer-0002".into();
    v.phase = "steady";
    v.t_s = Some(41.38);
    v.frequency_hz = Some(250.0);
    v.target_hz = Some(250.0);
    v.drive_vpk = Some(0.2);
    v.target_vpk = Some(0.2);
    v.health = Some(health(2069, 0.41, 1.9, 3.4, Some(4.1)));
    v.measured_hz = Some(250.02);
    v.channels = vec![
        LiveChannel {
            index: 1,
            description: "Control".into(),
            role: "control",
            rms: Some(0.1413),
            unit: "g".into(),
        },
        LiveChannel {
            index: 3,
            description: "Fixture".into(),
            role: "monitor",
            rms: Some(0.0706),
            unit: "g".into(),
        },
    ];
    v.control = Some(ControlMotion {
        index: 1,
        velocity_ips: Some(0.04916),
        displacement_mil: Some(0.0313),
        settling: false,
    });
    v.peak = Some((250.0, 0.1999));
    v
}

const SURVEY: [(f64, f64); 4] = [
    (200.0, 0.125),
    (250.0, 0.1999),
    (300.0, 0.15625),
    (400.0, 0.09375),
];

fn bytes_equal(name: &str, got: &[u8], want: &[u8]) {
    if got != want {
        let (a, b) = (
            codec::decode(got).unwrap().expanded().to_string(),
            codec::decode(want).unwrap().expanded().to_string(),
        );
        let at = a
            .bytes()
            .zip(b.bytes())
            .position(|(x, y)| x != y)
            .unwrap_or(0);
        let from = at.saturating_sub(150);
        panic!(
            "{name}: decoded frames differ at {at}\n got: {}\nwant: {}",
            &a[from..(at + 150).min(a.len())],
            &b[from..(at + 150).min(b.len())]
        );
    }
}

#[test]
fn explorer_the_configured_started_and_running_states_are_the_golden_ones() {
    let want = read_json("response_explorer_configure.json");
    same(
        &view::state_json(Some(&configured())),
        &want["result"]["state"],
        "configured",
    );
    let mut started = armed(configured());
    started.phase = "quiet";
    started.t_s = Some(0.04);
    started.frequency_hz = Some(250.0);
    started.target_hz = Some(250.0);
    started.drive_vpk = Some(0.0);
    started.target_vpk = Some(0.0);
    started.health = Some(health(2, 0.38, 1.2, 1.2, None));
    same(
        &view::state_json(Some(&started)),
        &read_json("response_explorer_start.json")["result"],
        "started",
    );
    let event = read_json("event_explorer_state.json");
    assert_eq!(event["topic"], "explorer.state");
    same(
        &view::state_json(Some(&running())),
        &event["data"],
        "running",
    );
    assert_eq!(view::state_json(None)["state"], json!("unconfigured"));
}

#[test]
fn explorer_the_live_and_data_frames_are_the_golden_bytes() {
    let mut arrays = Arrays::new();
    let data = view::live_json(&running(), &SURVEY, &mut arrays);
    let got = codec::encode(&protocol::event("explorer.live", 191, data), &arrays).unwrap();
    bytes_equal(
        "event_explorer_live",
        &got,
        &read_bin("event_explorer_live.bin"),
    );

    let request = read_json("request_explorer_subscribe.json");
    let parsed = protocol::parse_request(&request).unwrap();
    let sub = subscribe::parse(&parsed.params).unwrap().unwrap();
    let spectra = vec![
        (
            1,
            3.125,
            vec![0.0009765625, 0.001953125, 0.00048828125, 0.000244140625],
        ),
        (
            3,
            3.125,
            vec![0.00048828125, 0.0009765625, 0.000244140625, 0.0001220703125],
        ),
    ];
    let time = [0.0, 0.125, 0.1875, 0.125];
    let v = running();
    let ctx = DataContext {
        view: &v,
        survey: &SURVEY,
        spectra: &spectra,
        time: Some((529_408, 12_800.0, &time)),
    };
    let mut arrays = Arrays::new();
    let data = subscribe::data_json(&ctx, &sub, &mut arrays);
    let got = codec::encode(&protocol::event("explorer.data", 192, data), &arrays).unwrap();
    bytes_equal(
        "event_explorer_data",
        &got,
        &read_bin("event_explorer_data.bin"),
    );
}

#[test]
fn explorer_the_survey_is_the_golden_survey() {
    let got = view::survey_result(57, Some(1), Some("g"), &SURVEY, None);
    same(
        &got,
        &read_json("response_explorer_survey.json")["result"],
        "survey",
    );
    let unchanged = view::survey_result(57, Some(1), Some("g"), &SURVEY, Some(57));
    assert_eq!(unchanged["points"], Value::Null);
    assert_eq!(unchanged["peak"], json!({"hz": 250, "level": 0.1999}));
    // One point is carried as itself, drawn or not.
    assert_eq!(
        view::draw_survey(&SURVEY[1..2], (20.0, 2000.0), 256),
        vec![SURVEY[1]]
    );
    // Drawn on log-spaced points, nothing outside the span visited.
    let drawn = view::draw_survey(&SURVEY, (20.0, 2000.0), 256);
    assert!(drawn.first().unwrap().0 >= 200.0 && drawn.last().unwrap().0 <= 400.0);
    assert!(drawn.len() > 30);
}
