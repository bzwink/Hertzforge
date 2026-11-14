//! Explorer through the protocol, on a real loopback WebSocket, on the control
//! service's simulated station: configure and its checks, the run from start through
//! steering, a sweep, a hold and the survey to the stop; the verbs' refusals by state;
//! the controller rules (any connection may stop and abort); the run exclusion against
//! another module both ways; the run folder; the log; and the orphan safe stop.

mod support;

use hertzforge_station::StationBuildError;
use sds_control::server::Service;
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::time::Duration;
use support::{options, serve, Client};

const LONG: Duration = Duration::from_secs(30);

fn no_driver() -> FixedScanner {
    FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )))
}

fn row(index: u32, role: &str, description: &str) -> Value {
    let units = if role == "drive" { "V" } else { "g" };
    json!({
        "index": index, "enabled": true, "description": description,
        "role": role, "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": units, "sensitivity_mv_per_eu": if units == "V" { 1000 } else { 100 },
        "range_max": if units == "V" { 10 } else { 100 }, "polarity": "positive",
        "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

fn config() -> Value {
    json!({
        "title": "Protocol Explorer",
        "acquisition": {"fs_hz": 12800},
        "band_hz": [20, 2000], "start_hz": 250, "max_drive_vpk": 0.5,
        "slew": {"freq_oct_per_s": 4, "drive_db_per_s": 240},
        "capture": {"run_name": "explorer_protocol"}
    })
}

async fn controller(c: &mut Client, rows: Value) {
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok("channels.set", json!({"revision": rev, "channels": rows}))
        .await;
}

fn table() -> Value {
    json!([
        row(1, "control", "Control"),
        row(2, "drive", "Shaker"),
        row(3, "monitor", "Fixture")
    ])
}

async fn state_is(c: &mut Client, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let i = c
        .wait_for(what, LONG, |e| {
            e.topic() == "explorer.state" && pred(e.data())
        })
        .await;
    c.events[i].data().clone()
}

#[tokio::test]
async fn explorer_a_run_through_the_protocol_from_configure_to_stop() {
    let opts = options(
        no_driver(),
        1.0,
        Duration::from_secs(30),
        Duration::from_secs(5),
    );
    let root = opts.data_root.clone();
    let svc = Service::with_options(opts);
    svc.start_background();
    let addr = serve(svc.clone()).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table()).await;

    // Configure: a dry run needs no control and answers its derived values.
    let dry = c
        .ok(
            "explorer.configure",
            json!({"config": config(), "dry_run": true}),
        )
        .await;
    assert_eq!(dry["errors"], json!([]));
    assert_eq!(
        dry["derived"]["pipeline_frames"],
        json!(8),
        "the simulator's 2 x 4"
    );
    let mut bad = config();
    bad["level_g"] = json!(1);
    let e = c
        .refused("explorer.configure", json!({"config": bad}))
        .await;
    assert_eq!(
        (e["reason"].clone(), e["detail"]["param"].clone()),
        (json!("bad_params"), json!("config.level_g"))
    );
    let mut bad = config();
    bad["slew"]["drive_db_per_s"] = json!(1);
    let e = c
        .refused("explorer.configure", json!({"config": bad}))
        .await;
    assert_eq!(e["reason"], "config_invalid");
    assert_eq!(e["detail"]["errors"][0]["reason"], "safe_stop_budget");
    let r = c
        .ok("explorer.configure", json!({"config": config()}))
        .await;
    assert_eq!(r["state"]["state"], "configured");
    assert_eq!(r["state"]["t_s"], Value::Null);

    // Steering before the start is refused by state.
    let e = c.refused("explorer.steer", json!({"vpk": 0.2})).await;
    assert_eq!(
        (e["reason"].clone(), e["detail"]["state"].clone()),
        (json!("wrong_state"), json!("configured"))
    );
    let e = c.refused("explorer.survey.clear", json!({})).await;
    assert_eq!(e["reason"], "wrong_state");

    // Start: the tone runs at 0 V.
    let s = c.ok("explorer.start", json!({})).await;
    assert_eq!(s["state"], "running", "{s}");
    assert_eq!(s["drive_vpk"], json!(0));
    assert_eq!(s["grid"]["actual"], json!(true));
    let e = c.refused("explorer.start", json!({})).await;
    assert_eq!(e["reason"], "wrong_state", "a second start");
    let e = c.refused("explorer.reset", json!({})).await;
    assert_eq!(e["reason"], "run_active", "reset while live");
    let e = c.refused("explorer.clear", json!({})).await;
    assert_eq!(e["reason"], "run_active", "clear while live");
    // Another module is refused while the Explorer run is live.
    let e = c
        .refused("sine.configure", json!({"config": {"title": "x"}}))
        .await;
    assert!(
        e["reason"] == "run_active"
            || e["reason"] == "bad_params"
            || e["reason"] == "config_invalid",
        "{e}"
    );

    // Steer: both parts, clamped where they must be.
    let st = c
        .ok("explorer.steer", json!({"hz": 5000, "vpk": 0.9}))
        .await;
    assert_eq!(st["hz"], json!(2000));
    assert_eq!(st["vpk"], json!(0.5));
    assert_eq!(st["clamped"], json!({"hz": true, "vpk": true}));
    assert!(st["frame"].as_u64().unwrap() >= st["input_frame"].as_u64().unwrap());
    let e = c
        .refused("explorer.steer", json!({"hz": 300, "step_hz": 10}))
        .await;
    assert_eq!(e["reason"], "bad_params");
    let e = c.refused("explorer.steer", json!({"level_g": 1})).await;
    assert_eq!(e["reason"], "bad_params");
    let st = c.ok("explorer.steer", json!({"hz": 250, "vpk": 0.2})).await;
    assert_eq!(
        (st["hz"].clone(), st["vpk"].clone()),
        (json!(250), json!(0.2))
    );
    state_is(&mut c, "steady", |d| d["phase"] == "steady").await;

    // A sweep is answered at the slew; a hold answers where the tone is.
    let sw = c
        .ok(
            "explorer.sweep",
            json!({"direction": "up", "rate_oct_per_min": 600}),
        )
        .await;
    assert_eq!(
        (sw["rate_oct_per_min"].clone(), sw["clamped"].clone()),
        (json!(240), json!(true))
    );
    state_is(&mut c, "sweeping", |d| d["phase"] == "sweeping").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let h = c.ok("explorer.hold", json!({})).await;
    assert!(h["hz"].as_f64().unwrap() > 250.0, "{h}");
    // A hold with no sweep answers the target in force.
    let h2 = c.ok("explorer.hold", json!({})).await;
    assert_eq!(h2["hz"], h["hz"]);

    // The survey has points, sorted, and its peak.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let sv = c.ok("explorer.survey", json!({})).await;
    let pts = sv["points"].as_array().unwrap().clone();
    assert!(!pts.is_empty(), "{sv}");
    assert_eq!(
        (sv["channel"].clone(), sv["unit"].clone()),
        (json!(1), json!("g"))
    );
    assert!(sv["peak"].is_object());
    let rev = sv["revision"].clone();
    let again = c.ok("explorer.survey", json!({"since": rev})).await;
    assert!(again["points"].is_null() || again["revision"] != rev);
    let cl = c.ok("explorer.survey.clear", json!({})).await;
    assert!(cl["revision"].as_u64().unwrap() > rev.as_u64().unwrap());

    // The live frame reaches a connection without a subscription.
    c.wait_for("explorer.live", LONG, |e| e.topic() == "explorer.live")
        .await;

    // Stop from an observer: the controlled stop, then the end.
    let mut o = Client::connect(addr).await;
    let e = o.refused("explorer.steer", json!({"vpk": 0.1})).await;
    assert_eq!(e["reason"], "not_controller");
    o.ok("explorer.stop", json!({})).await;
    let end = state_is(&mut c, "the end", |d| d["state"] == "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    assert!(end["end"]["ramp_s"].as_f64().unwrap() > 0.1, "{end}");
    assert_eq!(end["stop"], "operator");
    assert_eq!(end["phase"], "stopped");
    let e = c.refused("explorer.steer", json!({"vpk": 0.1})).await;
    assert_eq!(
        (e["reason"].clone(), e["detail"]["state"].clone()),
        (json!("wrong_state"), json!("finished"))
    );
    let e = c.refused("explorer.start", json!({})).await;
    assert_eq!(e["reason"], "run_finished");

    // The log names the rate, the records and the end.
    let log = c.ok("explorer.log", json!({})).await;
    let texts: Vec<String> = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["text"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("the station programmed 12800 Hz")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.starts_with("ended operator_stop")),
        "{texts:?}"
    );

    // The run folder: the manifest, the table, the setup, the log with every intent,
    // the streams and the end.
    let folder = root.join("explorer_protocol");
    for _ in 0..100 {
        if folder.join("end.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for f in [
        "run.json",
        "table.json",
        "setup.json",
        "log.jsonl",
        "streams/inputs/stream.json",
        "streams/drives/stream.json",
        "end.json",
    ] {
        assert!(folder.join(f).exists(), "{f} in {}", folder.display());
    }
    let log = std::fs::read_to_string(folder.join("log.jsonl")).unwrap();
    let intents = log
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|l| l["kind"] == "intent")
        .count();
    assert!(intents >= 5, "{log}");
    let end: Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("end.json")).unwrap()).unwrap();
    assert_eq!(end["end"]["outcome"], "operator_stop");

    // Reset returns the ended run to configured; a second run with the same name gets
    // the next folder.
    let r = c.ok("explorer.reset", json!({})).await;
    assert_eq!(r["state"], "configured");
    c.ok("explorer.start", json!({})).await;
    let a = c.ok("explorer.abort", json!({})).await;
    assert!(a["state"] == "finished" || a["state"] == "running", "{a}");
    let end = state_is(&mut c, "the abort", |d| {
        d["state"] == "finished" && d["run_id"] != end["run_id"]
    })
    .await;
    assert_eq!(end["end"]["outcome"], "operator_abort", "{end}");
    assert!(root.join("explorer_protocol-2").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn explorer_the_table_and_an_observer_are_refused_by_name() {
    let svc = Service::with_options(options(
        no_driver(),
        1.0,
        Duration::from_secs(30),
        Duration::from_secs(5),
    ));
    svc.start_background();
    let addr = serve(svc.clone()).await;
    let mut c = Client::connect(addr).await;
    controller(
        &mut c,
        json!([
            row(1, "control", "Control"),
            row(2, "drive", "A"),
            row(3, "drive", "B")
        ]),
    )
    .await;
    let e = c
        .refused("explorer.configure", json!({"config": config()}))
        .await;
    assert_eq!(e["reason"], "config_invalid");
    assert_eq!(e["detail"]["errors"][0]["reason"], "extra_drive_channels");
    let mut o = Client::connect(addr).await;
    let e = o
        .refused("explorer.configure", json!({"config": config()}))
        .await;
    assert_eq!(e["reason"], "not_controller");
    // An observer's dry run is a computation.
    let d = o
        .ok(
            "explorer.configure",
            json!({"config": config(), "dry_run": true}),
        )
        .await;
    assert_eq!(d["errors"][0]["reason"], "extra_drive_channels");
    assert_eq!(d["derived"], Value::Null);
}

#[tokio::test]
async fn explorer_nobody_at_the_controls_ends_in_the_safe_stop() {
    let svc = Service::with_options(options(
        no_driver(),
        1.0,
        Duration::from_millis(1500),
        Duration::from_millis(300),
    ));
    svc.start_background();
    let addr = serve(svc.clone()).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table()).await;
    c.ok("explorer.configure", json!({"config": config()}))
        .await;
    c.ok("explorer.start", json!({})).await;
    c.ok("explorer.steer", json!({"vpk": 0.2})).await;
    c.ok(
        "explorer.sweep",
        json!({"direction": "up", "rate_oct_per_min": 30}),
    )
    .await;
    // The controller goes without a goodbye; an observer watches the run end.
    let mut o = Client::connect(addr).await;
    drop(c);
    let i = o
        .wait_for("the safe stop", LONG, |e| {
            e.topic() == "explorer.state" && e.data()["state"] == "finished"
        })
        .await;
    let end = o.events[i].data().clone();
    assert_eq!(end["end"]["outcome"], "safe_stop", "{end}");
    assert_eq!(end["stop"], "safe_stop");
    assert!(end["end"]["message"]
        .as_str()
        .unwrap()
        .starts_with("Nobody held control"));
}
