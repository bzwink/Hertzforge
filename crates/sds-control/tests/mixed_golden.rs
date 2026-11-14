//! The Mixed Mode goldens in `schema/golden/`, built by this crate's own builders:
//! the configuration parses and checks to the golden derived values, a
//! configuration with errors is refused as the golden error, the state and the
//! frames are encoded from the controller's status and display types (made here by
//! hand) to the golden bytes, and the subscription parses to the golden result.
//!
//! `MIXED_GOLDEN_DUMP=<file>` writes what the builders make, for
//! `schema/make_golden.py` to be brought in line with; the test then still holds
//! every golden to them.

use hertzforge_channel_table::{ChannelRole, ChannelTable, Units};
use hertzforge_mixed::{
    BandHistory, Direction, LineGrid, MixedDisplay, MixedFrf, MixedPhase, MixedState, MixedStatus,
    ToneHistory,
};
use sds_control::codec::{self, Arrays};
use sds_control::mixed::config::{self, CheckContext, WireMixed};
use sds_control::mixed::subscribe;
use sds_control::mixed::view::{self, IdentFacts, LiveChannel, MixedView, StationRef};
use sds_control::{laws, protocol};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::Arc;

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

/// A control row, the drive and a fixture accelerometer.
fn table() -> ChannelTable {
    let mut t = sds_control::table::Table::new().table;
    let mut c1 = sds_control::table::new_row(1);
    c1.role = ChannelRole::Control;
    c1.description = "Control".into();
    let mut c2 = sds_control::table::new_row(2);
    c2.role = ChannelRole::Drive;
    c2.description = "Shaker".into();
    c2.units = Units::VoltageV;
    let mut c3 = sds_control::table::new_row(3);
    c3.role = ChannelRole::Monitor;
    c3.description = "Fixture".into();
    t.channels = vec![c1, c2, c3];
    t
}

fn catalog_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "sds-control-mixed-golden-{}-{}-{}",
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
    ))
}

/// The golden configuration, checked against the golden table and the catalog.
fn checked() -> (
    WireMixed,
    sds_control::table::Report,
    Option<config::Derived>,
) {
    let request = read_json("request_mixed_configure.json");
    let w = config::parse(&request["params"]["config"]).unwrap();
    let dir = catalog_dir();
    let cat = hertzforge_random_laws::LawCatalog::open(&dir).unwrap();
    let floor =
        |i: &str| laws::mixed_lookup(Some(&cat), i, hertzforge_random_laws::LawWorld::Single);
    let tones = |i: &str| laws::mixed_lookup(Some(&cat), i, hertzforge_random_laws::LawWorld::Sine);
    let t = table();
    let ctx = CheckContext {
        table: &t,
        floor_law: &floor,
        tone_law: &tones,
    };
    let (r, d) = w.check(&ctx);
    drop(cat);
    let _ = std::fs::remove_dir_all(&dir);
    (w, r, d)
}

fn sim_station() -> StationRef {
    StationRef {
        kind: "sim",
        class: "sim",
        devices: Vec::new(),
    }
}

/// The run as configured, with its laws.
fn configured(run_id: &str) -> MixedView {
    let (w, _, d) = checked();
    let d = d.expect("the golden configuration checks");
    let law = view::LawState {
        floor: view::RoleState {
            law: d.law.floor.clone(),
            misses: 0,
            capped: 0,
            live: Vec::new(),
        },
        tones: view::RoleState {
            law: d.law.tones.clone(),
            misses: 0,
            capped: 0,
            live: vec![("blend_w".into(), 0.5)],
        },
        floor_values: Vec::new(),
        tone_values: Vec::new(),
        built: false,
    };
    let rms = d.parts.iter().map(|p| p.ms_g2).sum::<f64>().sqrt();
    view::configured_view(
        run_id,
        &w,
        d.grid,
        rms,
        sim_station(),
        Some(law),
        Some(d.feasibility.clone()),
    )
}

/// A running test at full level: every part's row as the controller states it.
fn running() -> MixedView {
    let (w, _, _) = checked();
    let t = table();
    let core = w.to_core(&t).unwrap();
    let mut s = MixedStatus::idle(&core);
    s.state = MixedState::FullLevel;
    s.phase = MixedPhase::Running;
    s.t_s = 131.5;
    s.test_s = 96.25;
    s.level_db = 0.0;
    s.rung_index = 2;
    s.at_last_level = true;
    s.band_coherence = 0.98;
    s.trusted_frac = 0.97;
    s.floor.rms_g = Some(6.02);
    s.floor.error_db_rms = Some(-0.06);
    s.floor.lines_in_band = 3961;
    s.floor.lines_out = 52;
    s.floor.alarm_lines = 2;
    s.floor.abort_lines = 0;
    s.floor.dof = 120.0;
    s.floor.rms_judged_lines = 3909;
    s.totals.control_rms_g = Some(7.21);
    s.totals.drive_vpk = 2.91;
    s.totals.drive_vrms = 0.66;
    for r in &mut s.parts {
        r.on = true;
        r.enabled = true;
        r.scale = 1.0;
        r.settled = true;
        match r.id.as_str() {
            "floor" => {
                r.drive = 0.58;
            }
            "t1" => {
                r.measured = Some(2.03);
                r.error_db = Some(0.13);
                r.drive = 0.25;
            }
            "t2" => {
                r.freq_hz = Some(412.625);
                r.direction = Some(Direction::Up);
                r.sweeps_done = 1;
                r.rate = Some(1.0);
                r.target = 1.19;
                r.measured = Some(1.21);
                r.error_db = Some(0.15);
                r.drive = 0.31;
                // Far over the floor: judged on each reading, at its own bands.
                r.judge_s = Some(0.5);
                r.judged_error_db = Some(0.125);
                r.judged_alarm_db = Some((1.5, 1.5));
                r.judged_abort_db = Some((3.0, 3.0));
            }
            "t3" => {
                r.freq_hz = Some(825.25);
                r.direction = Some(Direction::Up);
                r.rate = Some(1.0);
                r.measured = Some(0.48);
                r.error_db = Some(-0.35);
                r.drive = 0.0625;
                r.misses = 1;
                // Close to the floor: averaged over the cap, its bands widened.
                r.judge_s = Some(8.0);
                r.judged_error_db = Some(-0.25);
                r.judged_alarm_db = Some((1.75, 1.75));
                r.judged_abort_db = Some((3.5, 3.5));
                r.tolerance_widened = true;
            }
            "b1" => {
                r.target = 0.2425;
                r.measured = Some(0.2375);
                r.error_db = Some(-0.09);
                r.drive = 0.125;
                r.gain_db = Some(0.25);
                r.dof = Some(184.0);
            }
            "b2" => {
                r.freq_hz = Some(1472.5);
                r.direction = Some(Direction::Up);
                r.rate = Some(2.0);
                r.width_hz = Some(73.625);
                r.target = 0.1046;
                r.measured = Some(0.0991);
                r.error_db = Some(-0.23);
                r.alarm = true;
                r.drive = 0.11;
                r.gain_db = Some(0.4);
                r.dof = Some(184.0);
            }
            _ => {}
        }
    }
    let mut v = configured("mixed-0002");
    v.grid.actual = true;
    v.clock = Some("shared");
    v.record = Some(sds_control::random::view::RecordRef {
        folder: "/home/operator/Hertzforge_Data/20261101-142233_mixed_mixed-0002".into(),
        streams: true,
        problem: None,
    });
    view::apply_status(&mut v, &s);
    v.elapsed_s = Some(131.5);
    v.at_level_s = Some(36.25);
    v.remaining_at_level_s = Some(263.75);
    v.level_s = Some(36.25);
    v.loop_n = 63;
    v.tick_n = 13136;
    v.identification = Some(IdentFacts {
        level_db: -20.0,
        band_coherence: 0.98,
        gate_coherence: 0.5,
        lag_frames: 0,
        trusted_frac: 0.97,
        passed: true,
    });
    if let Some(l) = v.law.as_mut() {
        l.built = true;
        l.tones.misses = 1;
        l.floor_values = vec![("damping".into(), 4.0)];
        l.tone_values = vec![("t2".into(), vec![("servo_step_db".into(), 0.03125)])];
    }
    v.channels = vec![
        LiveChannel {
            index: 1,
            description: "Control".into(),
            role: "control",
            rms: Some(7.18),
            unit: "g".into(),
            overload: false,
        },
        LiveChannel {
            index: 3,
            description: "Fixture".into(),
            role: "monitor",
            rms: Some(9.5),
            unit: "g".into(),
            overload: false,
        },
    ];
    v.streaming = true;
    v.display = Some(Arc::new(display()));
    v
}

/// Four lines of a 6.25 Hz grid with dyadic values (exact in f32 and f64), the
/// middle two left out, and each history three points long.
fn display() -> MixedDisplay {
    let grid = LineGrid {
        f0_hz: 100.0,
        df_hz: 6.25,
        lines: 4,
    };
    let v = |a: [f32; 4]| a.to_vec();
    MixedDisplay {
        grid,
        control: v([0.0390625, 0.5, 0.046875, 0.0390625]),
        residual: v([0.0390625, 0.0625, 0.046875, 0.0390625]),
        channels: vec![hertzforge_mixed::MixedDisplayChannel {
            channel_index: 1,
            control: v([0.0390625, 0.5, 0.046875, 0.0390625]),
            residual: v([0.0390625, 0.0625, 0.046875, 0.0390625]),
        }],
        reference: v([0.0390625, 0.0390625, 0.0390625, 0.0390625]),
        alarm_lo: v([0.01953125, 0.01953125, 0.01953125, 0.01953125]),
        alarm_hi: v([0.078125, 0.078125, 0.078125, 0.078125]),
        abort_lo: v([0.009765625, 0.009765625, 0.009765625, 0.009765625]),
        abort_hi: v([0.15625, 0.15625, 0.15625, 0.15625]),
        drive: v([
            (1.0 / 4096.0),
            (1.0 / 2048.0),
            (1.0 / 4096.0),
            (1.0 / 4096.0),
        ]),
        excluded: vec![false, true, true, false],
        tones: vec![ToneHistory {
            part: "t2".into(),
            t_s: vec![131.0, 131.25, 131.5],
            freq_hz: vec![412.0, 412.25, 412.625],
            target_g: vec![1.1875, 1.1875, 1.1875],
            measured_g: vec![1.25, 1.21875, 1.21875],
            drive_vpk: vec![0.3125, 0.3125, 0.3125],
        }],
        bands: vec![BandHistory {
            part: "b2".into(),
            t_s: vec![127.5, 129.5, 131.5],
            center_hz: vec![1464.5, 1468.5, 1472.5],
            reference: vec![0.125, 0.125, 0.125],
            measured: vec![0.125, 0.09375, 0.09375],
            dof: vec![176.0, 180.0, 184.0],
        }],
        identification: Some(MixedFrf {
            h_mag2: v([4.0, 16.0, 4.0, 1.0]),
            coherence: v([0.96875, 0.984375, 0.96875, 0.9375]),
            lag_frames: 0,
            band_coherence: 0.98,
            trusted: vec![true, true, true, true],
            trusted_frac: 1.0,
        }),
    }
}

fn tone_bands(id: &str) -> ((f64, f64), (f64, f64)) {
    let _ = id;
    ((1.5, 1.5), (3.0, 3.0))
}

// ---- the goldens ---------------------------------------------------------------

fn response_mixed_configure() -> Value {
    let (_, r, d) = checked();
    let v = configured("mixed-0001");
    protocol::response(
        91,
        json!({
            "state": view::state_json(Some(&v)),
            "derived": d.unwrap().to_json(),
            "warnings": r.warnings.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        }),
    )
}

fn event_mixed_state() -> Value {
    protocol::event("mixed.state", 190, view::state_json(Some(&running())))
}

fn response_error_mixed_config_invalid() -> Value {
    let request = read_json("request_mixed_configure.json");
    let mut c = request["params"]["config"].clone();
    // The rotor's second harmonic dropped onto the rotor's own sweep: too close.
    c["parts"][3]["schedule"]["ratio"] = json!(1.001);
    let w = config::parse(&c).unwrap();
    let dir = catalog_dir();
    let cat = hertzforge_random_laws::LawCatalog::open(&dir).unwrap();
    let floor =
        |i: &str| laws::mixed_lookup(Some(&cat), i, hertzforge_random_laws::LawWorld::Single);
    let tones = |i: &str| laws::mixed_lookup(Some(&cat), i, hertzforge_random_laws::LawWorld::Sine);
    let t = table();
    let ctx = CheckContext {
        table: &t,
        floor_law: &floor,
        tone_law: &tones,
    };
    let (r, _) = w.check(&ctx);
    let _ = std::fs::remove_dir_all(&dir);
    protocol::error_response(92, &config::config_invalid(&r))
}

fn response_error_mixed_part_refused() -> Value {
    protocol::error_response(
        96,
        &sds_control::mixed::refuse(
            "part_refused",
            "The floor plays for the whole run; it cannot be switched off.",
        )
        .with_detail(json!({"id": "floor", "why": "floor"})),
    )
}

fn subscription() -> subscribe::Subscription {
    let r = read_json("request_mixed_subscribe.json");
    let p = protocol::parse_request(&r).unwrap();
    subscribe::parse(&p.params).unwrap().unwrap()
}

fn event_mixed_live() -> (Value, Arrays) {
    let mut arrays = Arrays::new();
    let data = view::live_json(&running(), &mut arrays);
    (protocol::event("mixed.live", 191, data), arrays)
}

fn event_mixed_data() -> (Value, Arrays) {
    let mut arrays = Arrays::new();
    // Channel 3 is limited flat at 0.5 g^2/Hz from 20 to 2000 Hz, as the golden
    // configuration's limit row says.
    let limits = [(3, vec![(20.0, 0.5), (2000.0, 0.5)])];
    let data = subscribe::data_json(
        &running(),
        &subscription(),
        &tone_bands,
        &limits,
        &mut arrays,
    );
    (protocol::event("mixed.data", 192, data), arrays)
}

const TEXT: [&str; 4] = [
    "response_mixed_configure",
    "event_mixed_state",
    "response_error_mixed_config_invalid",
    "response_error_mixed_part_refused",
];

/// The release the golden frames were written at. A built-in law with no file
/// states the release's own version, which moves with every release; a golden
/// frame states this one, so a release does not rewrite the frames.
const GOLDEN_RELEASE: &str = "0.1.3";

/// Put the golden release where a built-in law states the present one.
fn at_the_golden_release(v: &mut Value) {
    match v {
        Value::Object(m) => {
            let built_in = m.get("sha256").is_some_and(Value::is_null);
            let present =
                m.get("version").and_then(Value::as_str) == Some(env!("CARGO_PKG_VERSION"));
            if built_in && present {
                m.insert("version".into(), GOLDEN_RELEASE.into());
            }
            m.values_mut().for_each(at_the_golden_release);
        }
        Value::Array(a) => a.iter_mut().for_each(at_the_golden_release),
        _ => {}
    }
}

fn build_text(name: &str) -> Value {
    let mut v = match name {
        "response_mixed_configure" => response_mixed_configure(),
        "event_mixed_state" => event_mixed_state(),
        "response_error_mixed_config_invalid" => response_error_mixed_config_invalid(),
        "response_error_mixed_part_refused" => response_error_mixed_part_refused(),
        other => panic!("no builder for {other}"),
    };
    at_the_golden_release(&mut v);
    v
}

const BINARY: [&str; 2] = ["event_mixed_live", "event_mixed_data"];

fn build_binary(name: &str) -> (Value, Arrays) {
    match name {
        "event_mixed_live" => event_mixed_live(),
        "event_mixed_data" => event_mixed_data(),
        other => panic!("no builder for {other}"),
    }
}

fn dump_if_asked() {
    let Ok(path) = std::env::var("MIXED_GOLDEN_DUMP") else {
        return;
    };
    let mut out = Map::new();
    for name in TEXT {
        out.insert(name.into(), build_text(name));
    }
    for name in BINARY {
        let (m, a) = build_binary(name);
        let arrays: Vec<Value> = a
            .iter()
            .map(|(id, x)| json!([id, x.dtype().name(), [x.to_f64().len()], x.to_f64()]))
            .collect();
        out.insert(name.into(), json!({"message": m, "arrays": arrays}));
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(out)).unwrap(),
    )
    .unwrap();
}

#[test]
fn the_golden_configuration_checks_to_the_golden_derived_values() {
    dump_if_asked();
    let (_, r, d) = checked();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(d.is_some());
    same(
        &build_text("response_mixed_configure"),
        &read_json("response_mixed_configure.json"),
        "response_mixed_configure",
    );
}

#[test]
fn every_text_golden_is_its_builders_output() {
    for name in TEXT {
        same(&build_text(name), &read_json(&format!("{name}.json")), name);
    }
}

#[test]
fn encoding_reproduces_the_golden_bytes() {
    for name in BINARY {
        let (message, arrays) = build_binary(name);
        let got = codec::encode(&message, &arrays).unwrap();
        let want = read_bin(&format!("{name}.bin"));
        assert!(
            got == want,
            "{name}: {} bytes encoded vs {} golden\n got: {}\nwant: {}",
            got.len(),
            want.len(),
            String::from_utf8_lossy(&got[..got.len().min(800)]),
            String::from_utf8_lossy(&want[..want.len().min(800)]),
        );
    }
}

#[test]
fn the_binary_goldens_decode_to_their_expected_json_and_re_encode_byte_for_byte() {
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
fn the_subscription_parses_to_the_golden_result_and_the_verbs_parse() {
    same(
        &subscribe::result_json(Some(&subscription())),
        &read_json("response_mixed_subscribe.json")["result"],
        "subscribe result",
    );
    for (name, method) in [
        ("request_mixed_level", "mixed.level"),
        ("request_mixed_part", "mixed.part"),
        ("request_mixed_adjust", "mixed.adjust"),
    ] {
        let r = protocol::parse_request(&read_json(&format!("{name}.json"))).unwrap();
        assert_eq!(r.method, method);
    }
}

#[test]
fn every_mixed_text_golden_is_a_compact_text_frame() {
    for name in [
        "request_mixed_configure",
        "response_mixed_configure",
        "event_mixed_state",
        "request_mixed_subscribe",
        "response_mixed_subscribe",
        "request_mixed_level",
        "request_mixed_part",
        "request_mixed_adjust",
        "response_error_mixed_config_invalid",
        "response_error_mixed_part_refused",
    ] {
        let v = read_json(&format!("{name}.json"));
        let text = serde_json::to_string(&v).unwrap();
        assert!(!text.contains("$array"), "{name} is a text frame");
        assert!(v["type"].is_string(), "{name} has a type");
    }
}
