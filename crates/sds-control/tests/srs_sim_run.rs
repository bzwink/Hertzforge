//! SRS shock end to end on the simulator, through the wire, on the controller's own
//! SRS shock service: the sds-control Sim station (200 and 550 Hz at Q 2.5), unpaced,
//! through a WebSocket client.
//!
//! Every run here is the closed loop (identification in volts, the seed, the law
//! and its caps and guards). The law files are the ones scripts/build_laws.sh
//! builds; the template's law is built from the SDK template's own source as the
//! law host's `srs-template` fixture.

mod support;

use hertzforge_station::StationBuildError;
use sds_control::server::{Service, ServiceOptions};
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use support::schema::Schema;
use support::{options, serve, sim_config, Client};

const LONG: Duration = Duration::from_secs(120);

fn no_driver() -> FixedScanner {
    FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )))
}

/// The service on the controller's own SRS shock service.
fn real(timeout: Duration) -> ServiceOptions {
    let mut o = options(no_driver(), 0.0, timeout, Duration::from_secs(5));
    o.srs_engine = None;
    o.mimo_driver = None;
    o
}

fn row(index: u32, role: &str) -> Value {
    let units = if role == "drive" { "V" } else { "g" };
    json!({
        "index": index, "enabled": true, "description": role,
        "role": role, "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": units, "sensitivity_mv_per_eu": 1000,
        "range_max": 10, "polarity": "positive", "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

async fn controller(c: &mut Client) {
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": [row(1, "control"), row(2, "drive")]}),
    )
    .await;
}

/// A flat 2 g target, 100 to 2000 Hz at 1/3 octave, Q 10, wavelets, at 20480 Hz;
/// the ladder -12, -6, 0 dB with 2, 2 and 3 pulses advancing on a pass; unpaced
/// spacing; the default law. Identification in 0.2 s blocks (4096 points), as the
/// law check's: the default 1024 points at this rate is shorter than the burst and
/// its ring-out, and on this station it identifies the top bands with coherence
/// near zero (0.05 at 2016 Hz), so the seed's first pulse sits 13 dB low there,
/// past the abort band.
fn config(mode: &str) -> Value {
    json!({
        "title": "Simulated SRS",
        "target": {"kind": "flat", "accel_g": 2},
        "bands": {"f_lo_hz": 100, "f_hi_hz": 2000, "bands_per_octave": 3, "q": 10},
        "acquisition": {"fs_hz": 20480},
        "schedule": {"mode": mode, "levels_db": [-12, -6, 0], "pulses": [2, 2, 3],
                     "advance": "on_pass", "max_pulses_per_level": 5},
        "spacing": {"cooling_s": 0.05, "settling_s": 0, "compute_s": 0},
        "identification": {"nfft": 4096, "review": false},
        "limits": {"drive_vpk_ceiling": 5, "drive_vpk_abort": 8}
    })
}

async fn run_id(c: &mut Client, cfg: Value) -> Value {
    c.ok("srs.configure", json!({"config": cfg})).await["state"]["run_id"].clone()
}

async fn wait_end(c: &mut Client, id: &Value) -> Value {
    let i = c
        .wait_for("the run's end", LONG, |e| {
            e.topic() == "srs.state" && &e.data()["run_id"] == id && e.data()["state"] == "finished"
        })
        .await;
    c.events[i].data().clone()
}

async fn rows(c: &mut Client) -> Vec<Value> {
    c.ok("srs.pulses", json!({})).await["pulses"]
        .as_array()
        .unwrap()
        .clone()
}

/// The law file `<name>.wasm` built by scripts/build_laws.sh.
fn law_file(name: &str) -> PathBuf {
    let file = format!("{name}.wasm");
    let exe = std::env::current_exe().unwrap();
    exe.ancestors()
        .skip(1)
        .take(4)
        .map(|d| d.join("wasm32-wasip2").join("release").join(&file))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{file} is not built; run scripts/build_laws.sh"))
}

async fn import(c: &mut Client, name: &str) -> String {
    let r = c
        .ok(
            "laws.import",
            json!({"path": law_file(name).display().to_string()}),
        )
        .await;
    r["law"]["id"].as_str().unwrap().to_owned()
}

/// Every band of a pulse within `db` of the target at its level.
fn within(r: &Value, db: f64) -> bool {
    r["worst_err_db"].as_f64().is_some_and(|w| w.abs() <= db)
}

/// srs-1: AUTO with the default law completes; every band within ±3 dB at full
/// level.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_with_the_default_law_completes_inside_the_band() {
    let schema = Schema::load();
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let id = run_id(&mut c, config("auto")).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    schema.assert_valid(&end, Some("srsState"));
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    assert_eq!(end["identification"]["passed"], true, "{end}");
    assert!(end["prediction"]["drive_vpk"].as_f64().unwrap() > 0.0);
    let rows = rows(&mut c).await;
    for r in &rows {
        schema.assert_valid(r, Some("srsRow"));
        assert!(r["drive_vpk"].as_f64().unwrap() > 0.0, "{r}");
        assert_eq!(r["law"]["used"], true, "{r}");
    }
    for r in rows.iter().filter(|r| r["scheduled_db"] == 0) {
        assert!(within(r, 3.0), "{r}");
    }
}

/// A service built as the program builds it (a data root and no records root)
/// records every run in its run folder under its data root, at the station's
/// rate, a shot for the identification and one for every pulse. (Before the run
/// record, this was the module's own capture, switched on in the setup.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_srs_run_records_under_the_data_root_at_the_station_rate() {
    let mut o = real(Duration::from_secs(30));
    o.capture_root = None;
    let root = o.data_root.clone();
    let addr = serve(Service::with_options(o)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let id = run_id(&mut c, config("auto")).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    let n = rows(&mut c).await.len();
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    assert_eq!(
        folder.parent(),
        Some(root.as_path()),
        "{}",
        folder.display()
    );
    let t0 = Instant::now();
    while !folder.join("end.json").is_file() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the record did not end"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let side: Value = serde_json::from_slice(
        &std::fs::read(folder.join("streams/inputs/stream.json")).expect("a stream"),
    )
    .unwrap();
    assert_eq!(side["fs_hz"], 20480.0, "{side}");
    let shots = side["shots"].as_array().unwrap();
    assert_eq!(shots.len(), n + 1, "{side}");
    assert_eq!(shots[0]["what"], "identification");
    let frames = side["frames"].as_u64().unwrap();
    let width = side["channels"].as_array().unwrap().len() as u64;
    let bytes = std::fs::metadata(folder.join("streams/inputs/telemetry.f32"))
        .unwrap()
        .len();
    assert_eq!(bytes, frames * width * 4);
    assert!(folder.join("law/law.json").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_moves_and_an_early_fire_then_the_stop() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let mut cfg = config("manual");
    cfg["spacing"]["cooling_s"] = json!(0.5);
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    c.wait_for("waiting", LONG, |e| {
        e.topic() == "srs.state"
            && e.data()["phase"] == "waiting"
            && e.data()["prediction"].is_object()
    })
    .await;
    c.ok("srs.fire", json!({})).await;
    assert_eq!(c.refused("srs.fire", json!({})).await["reason"], "not_yet");
    assert_eq!(
        c.ok("srs.level", json!({"delta_db": 1})).await["level_db"],
        -11
    );
    assert_eq!(
        c.ok("srs.level", json!({"delta_db": -1})).await["level_db"],
        -12
    );
    assert_eq!(c.ok("srs.level", json!({"rung": 2})).await["level_db"], 0);
    tokio::time::sleep(Duration::from_millis(600)).await;
    c.ok("srs.fire", json!({})).await;
    c.wait_for("two pulses", LONG, |e| {
        e.topic() == "srs.state" && e.data()["pulses"]["fired"] == 2
    })
    .await;
    c.ok("srs.stop", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    c.pump(Duration::from_millis(500)).await;
    let rows = rows(&mut c).await;
    assert_eq!(rows.len(), 2, "nothing fired after the stop");
    assert_eq!(rows[1]["scheduled_db"], 0);
    let events = c.of_topic("srs.pulse");
    assert_eq!(events.len(), 2, "one pulse event per row");
    for (e, r) in events.iter().zip(&rows) {
        assert_eq!(&e.data()["pulse"], r);
    }
}

/// srs-3: an observer's abort in the wait ends the run within a poll; nothing
/// after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_observer_abort_in_the_wait() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    let mut obs = Client::connect(addr).await;
    controller(&mut c).await;
    let mut cfg = config("auto");
    cfg["spacing"]["cooling_s"] = json!(5);
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    c.wait_for("the first pulse", LONG, |e| {
        e.topic() == "srs.state"
            && e.data()["pulses"]["fired"] == 1
            && e.data()["phase"] == "waiting"
    })
    .await;
    let t0 = Instant::now();
    obs.ok("srs.abort", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(end["end"]["outcome"], "operator_abort");
    assert_eq!(end["pulses"]["fired"], 1);
}

/// srs-4: a displacement ceiling under the full-level prediction aborts at the
/// first full-level pulse, predicted. The law moves the table between pulses, so
/// the value is the prediction for the table in force then, which the state
/// carries: the full-level prediction is the table in force's, not the seed's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_displacement_ceiling_under_the_prediction_aborts_predicted() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    // The full-level prediction, from a run held at review.
    let mut probe = config("auto");
    probe["identification"]["review"] = json!(true);
    let id = run_id(&mut c, probe).await;
    c.ok("srs.start", json!({})).await;
    c.wait_for("review", LONG, |e| {
        e.topic() == "srs.state" && e.data()["run_id"] == id && e.data()["state"] == "review"
    })
    .await;
    c.ok("srs.proceed", json!({})).await;
    let i = c
        .wait_for("the prediction", LONG, |e| {
            e.topic() == "srs.state"
                && e.data()["run_id"] == id
                && e.data()["prediction"].is_object()
        })
        .await;
    let x0 = c.events[i].data()["prediction"]["displacement_pk_pk_in"]
        .as_f64()
        .unwrap();
    c.ok("srs.abort", json!({})).await;
    wait_end(&mut c, &id).await;
    let mut cfg = config("auto");
    cfg["limits"]["kinematic"] = json!({"disp_pp_in": 0.9 * x0});
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    let trip = &end["end"]["trip"];
    assert_eq!(end["end"]["outcome"], "aborted", "{end}");
    assert_eq!(trip["kind"], "kinematic");
    assert_eq!(trip["axis"], "displacement");
    assert_eq!(trip["predicted"], true);
    assert_eq!(trip["at_level_db"], 0);
    // The state's prediction is the one the trip was judged on.
    let predicted = end["prediction"]["displacement_pk_pk_in"].as_f64().unwrap();
    assert!(
        (trip["value"].as_f64().unwrap() / predicted - 1.0).abs() < 0.005,
        "{trip} against the state's {predicted}"
    );
    assert!(trip["value"].as_f64().unwrap() > 0.9 * x0, "{trip}");
    let rows = rows(&mut c).await;
    assert!(
        rows.iter().all(|r| r["scheduled_db"] != 0),
        "nothing fired at full level"
    );
}

/// srs-5: the controlling socket closed holds firing and safe-stops at the grace;
/// separately, the service's shutdown safe-stops the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_controller_lost_and_the_shutdown_safe_stop() {
    let timeout = Duration::from_millis(1500);
    let svc = Service::with_options(real(timeout));
    let addr = serve(svc.clone()).await;
    let mut obs = Client::connect(addr).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let mut cfg = config("auto");
    cfg["spacing"]["cooling_s"] = json!(0.3);
    let id = run_id(&mut c, cfg.clone()).await;
    c.ok("srs.start", json!({})).await;
    obs.wait_for("a pulse", LONG, |e| {
        e.topic() == "srs.state" && e.data()["pulses"]["fired"] == 1
    })
    .await;
    drop(c);
    let end = wait_end(&mut obs, &id).await;
    assert_eq!(end["end"]["outcome"], "safe_stop");
    assert!(end["pulses"]["fired"].as_u64().unwrap() <= 2);

    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    c.wait_for("a pulse", LONG, |e| {
        e.topic() == "srs.state" && e.data()["run_id"] == id && e.data()["pulses"]["fired"] == 1
    })
    .await;
    svc.shutdown().await;
    let s = c.ok("srs.state", json!({})).await;
    assert_eq!(s["end"]["outcome"], "safe_stop");
}

/// srs-6: an imported law whose later calls run past their budget: three misses
/// in a row end the run (`law_fault`, `over_budget`, 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_law_ends_the_run_after_three_misses() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let law = import(&mut c, "law_fixture_srs_misbehave").await;
    let mut cfg = config("auto");
    cfg["control"] = json!({"law": {"id": law, "params": {"mode": "sleep_after"}}});
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert_eq!(end["end"]["outcome"], "law_fault", "{end}");
    assert_eq!(end["end"]["trip"]["reason"], "over_budget");
    assert_eq!(end["end"]["trip"]["consecutive"], 3);
    assert_eq!(end["law"]["misses"], 3, "{end}");
    let rows = rows(&mut c).await;
    assert!(
        rows.iter().skip(2).all(|r| r["law"]["used"] == false),
        "{rows:?}"
    );
}

/// srs-7: the imported template meets srs-1's criteria; the built-in law and the
/// imported `shock-srs-ratio` drive alike: the same outcome, pulse count and rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_template_meets_the_default_and_the_built_in_law_matches_its_file() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let template = import(&mut c, "law_fixture_srs_template").await;
    let mut cfg = config("auto");
    cfg["control"] = json!({"law": {"id": template}});
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    for r in rows(&mut c).await.iter().filter(|r| r["scheduled_db"] == 0) {
        assert!(within(r, 3.0), "{r}");
    }

    let file = import(&mut c, "hertzforge_law_shock_srs_ratio").await;
    let mut runs = Vec::new();
    for law in ["srs_ratio".to_owned(), file] {
        let mut cfg = config("auto");
        cfg["control"] = json!({"law": {"id": law}});
        let id = run_id(&mut c, cfg).await;
        c.ok("srs.start", json!({})).await;
        let end = wait_end(&mut c, &id).await;
        let rows: Vec<Value> = rows(&mut c)
            .await
            .into_iter()
            .map(|mut r| {
                // The time and the call's length are the run's own.
                r["at_s"] = Value::Null;
                r["law"]["call_ms"] = Value::Null;
                r
            })
            .collect();
        runs.push((
            end["end"]["outcome"].clone(),
            end["pulses"]["fired"].clone(),
            rows,
        ));
    }
    assert_eq!(runs[0], runs[1]);
}

/// srs-8: exclusion against Random and classical shock both ways on the real
/// services, and the drive verbs refused once shutdown begins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exclusion_and_shutting_down() {
    let svc = Service::with_options(real(Duration::from_secs(30)));
    let addr = serve(svc.clone()).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    run_id(&mut c, config("manual")).await;
    c.ok("srs.arm", json!({})).await;
    let random = sim_config("auto", json!([{"db": 0, "dwell_s": null}]), 2.0);
    assert_eq!(
        c.refused("random.configure", json!({"config": random}))
            .await["reason"],
        "run_active"
    );
    c.ok("srs.stop", json!({})).await;
    c.ok("random.configure", json!({"config": random})).await;
    c.ok("random.arm", json!({})).await;
    assert_eq!(
        c.refused("srs.configure", json!({"config": config("manual")}))
            .await["reason"],
        "run_active"
    );
    c.ok("random.stop", json!({})).await;
    svc.shutdown().await;
    for m in ["srs.arm", "srs.start", "srs.fire", "srs.proceed"] {
        assert_eq!(c.refused(m, json!({})).await["reason"], "shutting_down");
    }
}

/// srs-9: the template law with its correction off and its seed 4 dB low cannot
/// bring the bands in: `not_converged` at the iteration limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_law_that_does_not_correct_ends_not_converged() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let law = import(&mut c, "law_fixture_srs_template").await;
    let mut cfg = config("auto");
    cfg["envelope"] = json!({"alarm_db": [3, 3], "abort_db": [9, 6]});
    cfg["control"] = json!({"law": {"id": law, "params": {"correct": false, "seed_error_db": -4}}});
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert_eq!(end["end"]["outcome"], "not_converged", "{end}");
    assert_eq!(end["end"]["trip"]["limit"], "per_level");
    assert_eq!(end["end"]["pulse"], 4);
    assert_eq!(end["end"]["level_db"], -12);
    for r in rows(&mut c).await {
        assert_eq!(r["passed"], false, "{r}");
        assert_eq!(r["abort_bands"], 0, "the bands alarm but do not abort: {r}");
    }
}

/// srs-10: a drive abort under the identified full-level drive ends the run before
/// any pulse, predicted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_drive_abort_under_the_prediction_ends_the_run_before_a_pulse() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let mut cfg = config("auto");
    cfg["limits"] = json!({"drive_vpk_ceiling": 0.05, "drive_vpk_abort": 0.06});
    cfg["identification"]["max_vpk"] = json!(0.005);
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert_eq!(end["end"]["outcome"], "aborted", "{end}");
    assert_eq!(end["end"]["trip"]["kind"], "drive_vpk");
    assert_eq!(end["end"]["trip"]["predicted"], true);
    assert_eq!(end["end"]["trip"]["pulse"], 0);
    assert_eq!(end["end"]["trip"]["abort_vpk"], 0.06);
    assert!(end["end"]["trip"]["vpk"].as_f64().unwrap() > 0.06, "{end}");
    assert!(
        end["end"]["message"]
            .as_str()
            .unwrap()
            .contains("at full level; the drive abort is 0.06 V pk"),
        "{end}"
    );
    assert_eq!(end["pulses"]["fired"], 0);
}

/// srs-11: AUTO pulse starts never closer than the time between pulses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_pulses_are_spaced_at_the_time_between_pulses() {
    let addr = serve(Service::with_options(real(Duration::from_secs(30)))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let mut cfg = config("auto");
    cfg["spacing"]["cooling_s"] = json!(0.4);
    let id = run_id(&mut c, cfg).await;
    c.ok("srs.start", json!({})).await;
    let end = wait_end(&mut c, &id).await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    let starts: Vec<f64> = rows(&mut c)
        .await
        .iter()
        .map(|r| r["at_s"].as_f64().unwrap())
        .collect();
    assert!(starts.len() >= 7);
    for w in starts.windows(2) {
        assert!(w[1] - w[0] >= 0.4 - 0.005, "pulses {} s apart", w[1] - w[0]);
    }
}
