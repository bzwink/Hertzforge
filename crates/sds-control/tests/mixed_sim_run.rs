//! Mixed Mode end to end on the simulator, through the protocol: a sine-on-random
//! run from start to completed with the floor first, a stop, an abort, a level
//! step by hand, a part switched off and on live, hold and resume, and a dropped
//! controller ending in the safe stop. Random-on-random and the combined test wait
//! on the core's narrowbands closed loop and are marked so.

mod support;

use hertzforge_station::StationBuildError;
use sds_control::server::Service;
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::time::Duration;
use support::{options, serve, Client};

const LONG: Duration = Duration::from_secs(240);

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

async fn controller(c: &mut Client) {
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": [row(1, "control", "Control"),
                                             row(2, "drive", "Shaker")]}),
    )
    .await;
}

fn service(timeout: Duration) -> std::sync::Arc<Service> {
    paced(timeout, 0.0)
}

/// A service whose simulator runs `speed` times faster than the wall (0: unpaced).
fn paced(timeout: Duration, speed: f64) -> std::sync::Arc<Service> {
    Service::with_options(options(
        FixedScanner(Err(StationBuildError::NiDriverUnavailable(
            "not installed on this test host".into(),
        ))),
        speed,
        timeout,
        Duration::from_secs(5),
    ))
}

fn floor() -> Value {
    json!({"id": "floor", "kind": "floor",
           "breakpoints": [{"freq_hz": 20, "psd_g2_hz": 0.0005}, {"freq_hz": 1000, "slope_db_oct": 0}]})
}

fn tone(id: &str, hz: f64, g: f64) -> Value {
    json!({"id": id, "kind": "tone", "schedule": {"kind": "fixed", "freq_hz": hz}, "amplitude_g": g})
}

fn band(id: &str, schedule: Value) -> Value {
    json!({"id": id, "kind": "narrowband", "schedule": schedule, "level_g2_hz": 0.002, "width_hz": 20})
}

fn config(parts: Vec<Value>, schedule: Value) -> Value {
    json!({
        "title": "Simulated Mixed Mode",
        "parts": parts,
        "acquisition": {"fs_hz": 4096, "n_pts": 8192, "n_avg": 2, "tick_ms": 10},
        "schedule": schedule,
        "control": {"pretest": {"averages": 2}, "floor_settle_s": 10, "part_ramp_s": 1,
                    "level_ramp_s": 1, "shutdown_ramp_s": 1},
        "limits": {"drive_vpk_ceiling": 5, "drive_vpk_abort": 8}
    })
}

fn auto(full_s: f64) -> Value {
    json!({"mode": "auto", "levels": [{"db": -6, "dwell_s": 2}, {"db": 0}], "full_level_s": full_s})
}

fn manual() -> Value {
    json!({"mode": "manual", "levels": [{"db": -6, "dwell_s": 0}, {"db": 0}], "full_level_s": 0})
}

/// Configure, start, proceed at the review hold; returns at the review's end.
async fn run_to_engaged(c: &mut Client, cfg: Value) {
    let r = c.ok("mixed.configure", json!({"config": cfg})).await;
    assert_eq!(r["state"]["state"], "configured", "{r}");
    c.ok("mixed.start", json!({})).await;
    wait_state(c, "review").await;
    c.ok("mixed.proceed", json!({})).await;
    wait_phase(c, "engaged").await;
}

async fn wait_state(c: &mut Client, state: &str) -> Value {
    let i = c
        .wait_for(&format!("mixed.state {state}"), LONG, |e| {
            e.topic() == "mixed.state" && e.data()["state"] == state
        })
        .await;
    c.events[i].data().clone()
}

async fn wait_phase(c: &mut Client, phase: &str) -> Value {
    let i = c
        .wait_for(&format!("mixed.state phase {phase}"), LONG, |e| {
            e.topic() == "mixed.state" && e.data()["phase"] == phase
        })
        .await;
    c.events[i].data().clone()
}

fn phases_seen(c: &Client) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in c.of_topic("mixed.state") {
        let p = e.data()["phase"].as_str().unwrap_or("").to_owned();
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sine_on_random_runs_floor_first_to_completed() {
    // Paced, so the five-a-second live frames sample the run at full level.
    let addr = serve(paced(Duration::from_secs(30), 8.0)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(vec![floor(), tone("t1", 160.0, 0.5)], auto(20.0)),
    )
    .await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    // The ramp down the finished schedule starts takes the shutdown ramp (1 s
    // here), not the last chunk before the end.
    assert!(
        end["end"]["ramp_s"].as_f64().is_some_and(|r| r >= 0.9),
        "{end}"
    );
    // The end says the level the test ran at, not where the ramp down left it.
    assert_eq!(end["end"]["level_db"], 0, "{end}");
    let seen = phases_seen(&c);
    let at = |p: &str| seen.iter().position(|s| s == p);
    assert!(
        at("floor_only").is_some() && at("floor_only") < at("engaged"),
        "the floor comes first: {seen:?}"
    );
    // Every live frame is this run's; the tone is tracked at full level.
    let lives = c.of_topic("mixed.live");
    assert!(!lives.is_empty());
    assert!(lives.iter().all(|e| e.data()["run_id"] == "mixed-0001"));
    let at_full: Vec<f64> = lives
        .iter()
        .filter(|e| matches!(e.data()["phase"].as_str(), Some("engaged" | "full_level")))
        .filter_map(|e| {
            let p = e.data()["parts"]
                .as_array()?
                .iter()
                .find(|p| p["id"] == "t1")?;
            if p["settled"] == true {
                p["error_db"].as_f64()
            } else {
                None
            }
        })
        .collect();
    let last = (
        phases_seen(&c),
        lives
            .iter()
            .map(|e| {
                (
                    e.data()["parts_s"].clone(),
                    e.data()["level_db"].clone(),
                    e.data()["phase"].clone(),
                )
            })
            .collect::<Vec<_>>(),
    );
    assert!(!at_full.is_empty(), "no settled tone reading: {last:?}");
    let worst = at_full.iter().fold(0.0f64, |a, b| a.max(b.abs()));
    assert!(
        worst <= 1.5,
        "the settled tone within 1.5 dB: worst {worst}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_auto_ladder_reaches_full_level() {
    let addr = serve(paced(Duration::from_secs(30), 8.0)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(vec![floor(), tone("t1", 160.0, 0.5)], auto(10.0)),
    )
    .await;
    let full = wait_phase(&mut c, "full_level").await;
    assert_eq!(full["level_db"], 0, "{full}");
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_ramps_every_part_down_and_ends_operator_stop() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(vec![floor(), tone("t1", 160.0, 0.5)], auto(60.0)),
    )
    .await;
    c.ok("mixed.stop", json!({})).await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    assert!(end["end"]["ramp_s"].as_f64().is_some(), "{end}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abort_ends_operator_abort_without_the_ramp() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    let mut obs = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(vec![floor(), tone("t1", 160.0, 0.5)], auto(60.0)),
    )
    .await;
    // Any screen may abort.
    obs.ok("mixed.abort", json!({})).await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_abort", "{end}");
    assert!(end["end"]["ramp_s"].is_null(), "{end}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_level_hold_and_a_part_switched_live_reach_the_core() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(
            vec![floor(), tone("t1", 160.0, 0.5), tone("t2", 320.0, 0.25)],
            manual(),
        ),
    )
    .await;
    let r = c.ok("mixed.level", json!({"to_db": 0})).await;
    assert_eq!(r["level_db"], 0);
    assert_eq!(
        c.refused("mixed.level", json!({"to_db": -3})).await["reason"],
        "not_a_rung"
    );
    let s = c.ok("mixed.hold", json!({})).await;
    assert_eq!(s["held"], true, "{s}");
    let s = c.ok("mixed.resume", json!({})).await;
    assert_eq!(s["held"], false, "{s}");
    let r = c.ok("mixed.part", json!({"id": "t2", "on": false})).await;
    assert_eq!(r, json!({"id": "t2", "on": false}));
    let i = c
        .wait_for("t2 off", LONG, |e| {
            e.topic() == "mixed.state"
                && e.data()["parts"]
                    .as_array()
                    .and_then(|ps| ps.iter().find(|p| p["id"] == "t2"))
                    .is_some_and(|p| p["on"] == false)
        })
        .await;
    assert!(i > 0);
    // Asking for the state a part is in answers with it, unchanged.
    let r = c.ok("mixed.part", json!({"id": "t2", "on": false})).await;
    assert_eq!(r["on"], false);
    c.ok("mixed.part", json!({"id": "t2", "on": true})).await;
    let e = c
        .refused("mixed.adjust", json!({"key": "kp", "value": 0.4}))
        .await;
    assert_eq!(e["detail"]["why"], "not_live");
    let r = c
        .ok("mixed.adjust", json!({"key": "blend_w", "value": 0.6}))
        .await;
    assert_eq!(r["value"], 0.6);
    c.ok("mixed.stop", json!({})).await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    let log = c.ok("mixed.log", json!({})).await;
    let parts: Vec<&str> = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "part")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert!(
        parts.iter().any(|t| t.contains("t2") && t.contains("off")),
        "{parts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_band_switched_off_live_ramps_out_and_back() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(
            vec![
                floor(),
                band("b1", json!({"kind": "fixed", "freq_hz": 300})),
            ],
            manual(),
        ),
    )
    .await;
    let band_on = |want: bool| {
        move |e: &support::Event| {
            e.topic() == "mixed.state"
                && e.data()["parts"]
                    .as_array()
                    .and_then(|ps| ps.iter().find(|p| p["id"] == "b1"))
                    .is_some_and(|p| p["on"] == want)
        }
    };
    let r = c.ok("mixed.part", json!({"id": "b1", "on": false})).await;
    assert_eq!(r["on"], false);
    // A band's samples are synthesized ahead, so its switch lands about a quarter
    // second of station time late: the wait allows for it.
    c.wait_for("b1 off", LONG, band_on(false)).await;
    c.ok("mixed.part", json!({"id": "b1", "on": true})).await;
    c.wait_for("b1 on again", LONG, band_on(true)).await;
    c.ok("mixed.stop", json!({})).await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_random_parts_rms_abort_trips_and_names_no_part() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let mut cfg = config(vec![floor(), tone("t1", 160.0, 0.5)], auto(20.0));
    cfg["control"]["drive_ceiling_vrms"] = json!(0.02);
    cfg["control"]["drive_abort_vrms"] = json!(0.02);
    run_to_engaged_or_end(&mut c, cfg).await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "aborted", "{end}");
    assert_eq!(end["end"]["trip"]["kind"], "random_vrms", "{end}");
    assert!(end["end"]["trip"]["part"].is_null(), "{end}");
}

/// As [`run_to_engaged`], but the run may end before it engages.
async fn run_to_engaged_or_end(c: &mut Client, cfg: Value) {
    let r = c.ok("mixed.configure", json!({"config": cfg})).await;
    assert_eq!(r["state"]["state"], "configured", "{r}");
    c.ok("mixed.start", json!({})).await;
    wait_state(c, "review").await;
    c.ok("mixed.proceed", json!({})).await;
}

/// The run folder is the one record: the state names it from arm on, and the
/// controller's own capture is never started, so no run leaves a recording of the
/// old kind (its `law_ticks.bin`, written on the pump thread). Red if the run thread
/// still started it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mixed_run_records_to_its_run_folder_and_not_to_the_old_capture() {
    let opts = options(
        FixedScanner(Err(StationBuildError::NiDriverUnavailable(
            "not installed on this test host".into(),
        ))),
        0.0,
        Duration::from_secs(30),
        Duration::from_secs(5),
    );
    let root = opts.records_root();
    let addr = serve(Service::with_options(opts)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(vec![floor(), tone("t1", 160.0, 0.5)], auto(4.0)),
    )
    .await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    let folder = std::path::PathBuf::from(end["record"]["folder"].as_str().unwrap());
    assert!(folder.starts_with(&root), "{folder:?} under {root:?}");
    assert!(folder.join("run.json").is_file(), "{folder:?}");
    let log = c.ok("mixed.log", json!({})).await;
    assert!(
        !log["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["text"].as_str().unwrap_or("").starts_with("recording to")),
        "{log}"
    );
    fn any_law_ticks(dir: &std::path::Path) -> bool {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                let p = e.path();
                p.file_name().is_some_and(|n| n == "law_ticks.bin")
                    || (p.is_dir() && any_law_ticks(&p))
            })
    }
    assert!(
        !any_law_ticks(&root),
        "a recording of the old kind under {root:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_controller_ends_in_the_safe_stop() {
    let addr = serve(service(Duration::from_secs(2))).await;
    let mut obs = Client::connect(addr).await;
    {
        let mut c = Client::connect(addr).await;
        controller(&mut c).await;
        run_to_engaged(
            &mut c,
            config(vec![floor(), tone("t1", 160.0, 0.5)], manual()),
        )
        .await;
    }
    let i = obs
        .wait_for("the safe stop", LONG, |e| {
            e.topic() == "mixed.state" && e.data()["state"] == "finished"
        })
        .await;
    assert_eq!(obs.events[i].data()["end"]["outcome"], "safe_stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_on_random_runs_to_completed() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(
            vec![
                floor(),
                band("b1", json!({"kind": "fixed", "freq_hz": 300})),
            ],
            auto(6.0),
        ),
    )
    .await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_combined_test_runs_to_completed() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(
            vec![
                floor(),
                tone("t1", 160.0, 0.5),
                band(
                    "b1",
                    json!({"kind": "sweep", "band_hz": [400, 600],
                           "rate": {"law": "linear", "hz_per_s": 10}, "end": null}),
                ),
            ],
            auto(6.0),
        ),
    )
    .await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
}

/// At a level below full, the composite's reference is the reference at that
/// level (the level the Control view's reference is drawn at), and every
/// enabled input, a monitor too, has an rms in the live frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_total_reference_is_at_the_level_and_every_input_has_its_rms() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": [row(1, "control", "Control"),
                                             row(2, "drive", "Shaker"),
                                             row(3, "monitor", "Fixture")]}),
    )
    .await;
    let r = c
        .ok(
            "mixed.configure",
            json!({"config": config(vec![floor(), tone("t1", 160.0, 0.5)], manual())}),
        )
        .await;
    let full = r["state"]["total"]["reference_rms_g"]
        .as_f64()
        .expect("full-level reference");
    c.ok("mixed.start", json!({})).await;
    wait_state(&mut c, "review").await;
    c.ok("mixed.proceed", json!({})).await;
    wait_phase(&mut c, "engaged").await;
    c.pump(Duration::from_secs(3)).await;
    let lives = c.of_topic("mixed.live");
    let last = lives
        .iter()
        .rev()
        .map(|e| e.data())
        .find(|d| d["phase"] == "engaged" && d["level_db"] == -6)
        .expect("a live frame at -6 dB");
    let at = last["total"]["reference_rms_g"].as_f64().unwrap();
    let want = full * 10f64.powf(-6.0 / 20.0);
    assert!(
        (at / want - 1.0).abs() < 2e-3,
        "reference {at} at -6 dB, want {want} (full {full})"
    );
    let chans = last["channels"].as_array().unwrap();
    assert_eq!(chans.len(), 2, "{chans:?}");
    for ch in chans {
        assert!(ch["rms"].as_f64().is_some_and(|x| x > 0.0), "no rms: {ch}");
    }
    c.ok("mixed.abort", json!({})).await;
}

/// The flow's sine-on-random test, a standing tone and a tone sweeping up
/// from where the plant's gain climbs, with the shell's acquisition, ramps and
/// tone law settings, through the ladder to completed. Before the tone held
/// its feed-forward until its reading was usable, the sweep carried the drive
/// 0.7 dB off it by the handoff and the servo ramped through that at its rate
/// cap into the tone's abort.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_swept_tone_on_the_floor_runs_to_completed() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let tones = json!({"alarm_db": [1.5, 1.5], "abort_db": [3, 3]});
    let mut fixed = tone("t1", 120.0, 0.3);
    let mut swept = json!({"id": "t2", "kind": "tone", "amplitude_g": 0.3,
           "schedule": {"kind": "sweep", "band_hz": [400, 600],
                        "rate": {"law": "log", "oct_per_min": 1}, "end": null}});
    for t in [&mut fixed, &mut swept] {
        t["alarm_db"] = tones["alarm_db"].clone();
        t["abort_db"] = tones["abort_db"].clone();
    }
    let mut cfg = config(
        vec![
            json!({"id": "floor", "kind": "floor",
                   "breakpoints": [{"freq_hz": 20, "psd_g2_hz": 1e-4}, {"freq_hz": 2000, "psd_g2_hz": 1e-4}]}),
            fixed,
            swept,
        ],
        json!({"mode": "auto", "levels": [{"db": -12, "dwell_s": 5}, {"db": -6, "dwell_s": 5}, {"db": 0}],
               "full_level_s": 15}),
    );
    cfg["acquisition"] = json!({"fs_hz": 8192, "n_pts": 16384, "n_avg": 4, "tick_ms": 10});
    cfg["control"]["part_ramp_s"] = json!(3);
    cfg["control"]["floor_settle_s"] = json!(30);
    cfg["control"]["law"] = json!({"tones": {"id": "servo_blend", "params": {
        "blend_w": 0.5, "compression": "custom", "ki": 0.5, "max_rate_db_per_s": 20, "kp": 0.3}}});
    run_to_engaged(&mut c, cfg).await;
    let end = wait_state(&mut c, "finished").await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
}

/// Every spectrum of a live frame carries the floor's band's lines only, as
/// Random's frames do: the floor here is 20 to 1000 Hz on 0.5 Hz lines, so the
/// views' lines run from 20 Hz to 1000 Hz, not over the whole 2048 Hz grid.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_live_spectra_carry_the_floor_bands_lines_only() {
    let addr = serve(paced(Duration::from_secs(30), 8.0)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_to_engaged(
        &mut c,
        config(vec![floor(), tone("t1", 160.0, 0.5)], manual()),
    )
    .await;
    c.pump(Duration::from_secs(2)).await;
    let live = c
        .of_topic("mixed.live")
        .into_iter()
        .rev()
        .find(|e| e.data()["phase"] == "engaged")
        .expect("a live frame engaged");
    let decoded = live.decoded.as_ref().expect("a binary frame");
    let mut spectra = 0;
    for view in live.data()["views"].as_array().unwrap() {
        if view["view"] != "spectrum" {
            continue;
        }
        for s in view["series"].as_array().unwrap() {
            let x = decoded.array(&s["x"]).unwrap().to_f64();
            let edges = decoded.array(&s["edges"]).unwrap().to_f64();
            assert_eq!(edges.len(), x.len() + 1);
            assert!(
                (x[0] - 20.0).abs() < 1e-9 && (x[x.len() - 1] - 1000.0).abs() < 1e-9,
                "{} {}: {} to {} Hz over {} lines",
                view["title"],
                s["name"],
                x[0],
                x[x.len() - 1],
                x.len()
            );
            spectra += 1;
        }
    }
    assert!(spectra >= 3, "{spectra} series");
    c.ok("mixed.abort", json!({})).await;
}

/// What a moving part's row says while the run is engaged: a follower names the
/// part it follows and its ratio, moves in its leader's direction, and states its
/// own rate in its leader's law (a linear leader's Hz/s times the ratio; a log
/// leader's octaves per minute unchanged), the other rate null; a band sweeping
/// with its width a share of its center states its rate in its own law's field;
/// a band's drive is null (the core reports none for it), not 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_moving_rows_name_their_leader_and_their_own_rate() {
    let addr = serve(paced(Duration::from_secs(30), 8.0)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let mut lin = json!({"id": "t1", "kind": "tone", "amplitude_g": 0.3,
        "schedule": {"kind": "sweep", "band_hz": [100, 140],
                     "rate": {"law": "linear", "hz_per_s": 2}, "end": null}});
    lin["name"] = json!("Lead");
    let follow = |id: &str, of: &str, ratio: f64| {
        json!({"id": id, "kind": "tone", "amplitude_g": 0.1,
               "schedule": {"kind": "follow", "of": of, "ratio": ratio}})
    };
    let share = json!({"id": "b1", "kind": "narrowband", "level_g2_hz": 0.002, "width_share": 0.05,
        "schedule": {"kind": "sweep", "band_hz": [500, 800],
                     "rate": {"law": "log", "oct_per_min": 1}, "end": null}});
    run_to_engaged(
        &mut c,
        config(
            vec![
                floor(),
                lin,
                follow("t2", "t1", 2.0),
                share,
                follow("t3", "b1", 1.2),
            ],
            manual(),
        ),
    )
    .await;
    c.pump(Duration::from_millis(600)).await;
    let frame = c
        .of_topic("mixed.live")
        .into_iter()
        .rev()
        .map(|e| e.data().clone())
        .find(|d| d["phase"] == "engaged")
        .expect("an engaged live frame");
    let row = |id: &str| {
        frame["parts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    let (t1, t2, b1, t3) = (row("t1"), row("t2"), row("b1"), row("t3"));
    assert_eq!(t1["follows"], Value::Null);
    assert_eq!(t1["direction"], "up", "{t1}");
    assert_eq!(t1["rate_hz_per_s"], 2.0, "{t1}");
    assert_eq!(t1["rate_oct_per_min"], Value::Null);
    assert_eq!(t2["follows"], json!({"of": "t1", "ratio": 2}), "{t2}");
    assert_eq!(t2["direction"], "up", "{t2}");
    assert_eq!(t2["rate_hz_per_s"], 4.0, "{t2}");
    assert_eq!(t2["rate_oct_per_min"], Value::Null, "{t2}");
    assert_eq!(b1["direction"], "up", "{b1}");
    assert_eq!(b1["rate_oct_per_min"], 1.0, "{b1}");
    assert_eq!(b1["rate_hz_per_s"], Value::Null, "{b1}");
    assert_eq!(b1["drive_vrms"], Value::Null, "{b1}");
    assert_eq!(t3["follows"], json!({"of": "b1", "ratio": 1.2}), "{t3}");
    assert_eq!(t3["direction"], "up", "{t3}");
    assert_eq!(t3["rate_oct_per_min"], 1.0, "{t3}");
    assert_eq!(t3["rate_hz_per_s"], Value::Null, "{t3}");
    c.ok("mixed.abort", json!({})).await;
}

/// A tone's abort states what the tone measured and the limit it crossed, not
/// only its error: a swept tone with an abort band far tighter than the servo
/// holds trips, and the end's trip carries `measured_g` and `limit_g` that agree
/// with its judged `error_db` and its judged (widened) abort band.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_tone_trip_states_its_measured_value_and_its_limit() {
    let addr = serve(service(Duration::from_secs(30))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let tight = json!({"id": "t1", "kind": "tone", "amplitude_g": 0.3,
        "alarm_db": [0.05, 0.05], "abort_db": [0.1, 0.1],
        "schedule": {"kind": "sweep", "band_hz": [100, 400],
                     "rate": {"law": "log", "oct_per_min": 2}, "end": null}});
    run_to_engaged(&mut c, config(vec![floor(), tight], manual())).await;
    let end = wait_state(&mut c, "finished").await;
    let trip = &end["end"]["trip"];
    assert_eq!(trip["kind"], "abort_band", "{end}");
    let e = trip["error_db"].as_f64().unwrap();
    let m = trip["measured_g"].as_f64().expect("measured_g");
    let l = trip["limit_g"].as_f64().expect("limit_g");
    // The limit is the tone's judged abort band on that side (its own 0.1 dB,
    // or widened to its reading's scatter), as the row states it; measured over
    // limit is the judged error beyond it.
    let row = end["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "t1")
        .unwrap()
        .clone();
    let band = row["judged_abort_db"].as_array().expect("judged band");
    let side = if e >= 0.0 {
        band[1].as_f64().unwrap()
    } else {
        band[0].as_f64().unwrap()
    };
    assert!(side >= 0.1, "{row}");
    let beyond = 20.0 * (m / l).log10();
    assert!(
        (beyond - (e - side.copysign(e))).abs() < 0.02,
        "{trip}: {beyond} dB beyond the limit, band {side}"
    );
}

/// A limit profile on the control channel is accepted, as Random accepts it
/// (Mixed Mode's limit rows are Random's), the run with it engages and the
/// channel's response view draws its Limit; a
/// subscription made before the run starts is accepted and its data flows once
/// the run streams.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_takes_a_limit_on_the_control_channel_and_an_early_subscription() {
    let addr = serve(paced(Duration::from_secs(30), 8.0)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let sub = c
        .ok(
            "mixed.subscribe",
            json!({"rate_hz": 2, "plots": [{"id": "ctl", "quantity": "control"},
                                            {"id": "lim", "quantity": "response", "channels": "limited"}]}),
        )
        .await;
    assert_eq!(sub["default"], false, "{sub}");
    let mut cfg = config(vec![floor(), tone("t1", 160.0, 0.5)], manual());
    cfg["limits"]["channels"] = json!([{"channel": 1, "kind": "response",
        "rows": [{"freq_hz": 20, "ceiling": 1.0}, {"freq_hz": 1000, "ceiling": 1.0}]}]);
    let r = c.ok("mixed.configure", json!({"config": cfg})).await;
    assert_eq!(r["state"]["state"], "configured", "{r}");
    assert!(
        !r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["reason"] == "limit_on_control_channel"),
        "{r}"
    );
    c.ok("mixed.start", json!({})).await;
    wait_state(&mut c, "review").await;
    c.ok("mixed.proceed", json!({})).await;
    wait_phase(&mut c, "engaged").await;
    // The limited control channel's response view carries its Limit, the
    // ceiling stepped on the band's lines its rows cover.
    let i = c
        .wait_for("mixed.data with a response", LONG, |e| {
            e.topic() == "mixed.data"
                && e.data()["plots"][1]["views"][0]["series"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
        })
        .await;
    let e = &c.events[i];
    let plot = &e.data()["plots"][1];
    assert_eq!(plot["channels"], json!([1]), "{plot}");
    let view = &plot["views"][0];
    let limit = view["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Limit")
        .unwrap_or_else(|| panic!("no Limit series: {view}"));
    assert_eq!(limit["role"], "limit");
    let d = e.decoded.as_ref().unwrap();
    let x = d.array(&limit["x"]).unwrap().to_f64();
    let y = d.array(&limit["y"]).unwrap().to_f64();
    let edges = d.array(&limit["edges"]).unwrap().to_f64();
    assert_eq!(edges.len(), x.len() + 1);
    assert!((x[0] - 20.0).abs() < 1e-9 && (x[x.len() - 1] - 1000.0).abs() < 1e-9);
    assert!(
        y.iter().all(|c| (c - 1.0).abs() < 1e-9),
        "a flat 1.0 ceiling"
    );
    c.ok("mixed.abort", json!({})).await;
}

/// While the floor plays alone a tone is not playing: its row reads `on` false
/// with its ramp scale 0, and in the Run reading it has no row; once the parts
/// come in it reads `on` true.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_a_part_waiting_for_the_floor_reads_off() {
    let addr = serve(paced(Duration::from_secs(30), 8.0)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let r = c
        .ok(
            "mixed.configure",
            json!({"config": config(vec![floor(), tone("t1", 160.0, 0.5)], manual())}),
        )
        .await;
    assert_eq!(r["state"]["state"], "configured", "{r}");
    c.ok("mixed.start", json!({})).await;
    wait_state(&mut c, "review").await;
    c.ok("mixed.proceed", json!({})).await;
    let alone = wait_phase(&mut c, "floor_only").await;
    let row = |d: &Value, id: &str| {
        d["parts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    let t1 = row(&alone, "t1");
    assert_eq!(t1["scale"], 0, "{t1}");
    assert_eq!(t1["on"], false, "{t1}");
    assert_eq!(row(&alone, "floor")["on"], true, "{alone}");
    let engaged = wait_phase(&mut c, "engaged").await;
    c.pump(Duration::from_secs(2)).await;
    let live = c
        .of_topic("mixed.live")
        .into_iter()
        .rev()
        .map(|e| e.data().clone())
        .find(|d| d["phase"] == "engaged")
        .unwrap_or(engaged);
    let t1 = row(&live, "t1");
    assert_eq!(t1["on"], true, "{t1}");
    c.ok("mixed.abort", json!({})).await;
}
