//! Explorer end to end through the wire on the control service's simulated station (two
//! modes, 200 and 550 Hz, Q 2.5, feedthrough 0.35), the real core in its run host:
//! sim-1 (the survey against the station's own response, the peak at a mode, station
//! time, the intents' frames), sim-2 (steering's clamps and refusals), sim-3 (an
//! observer's abort and steer), sim-4 (a level verb during the stop), sim-5 (the
//! controller lost; a shutdown), sim-7 (an accel limit) and sim-8 (the exclusion with a
//! simulation run, both ways). Judged on the service's own stamps.

mod support;

use hertzforge_hal_sim::Resonator;
use hertzforge_station::StationBuildError;
use sds_control::server::Service;
use sds_control::station::{FixedScanner, SIM_RESONANCES};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{options, serve, Client};

const LONG: Duration = Duration::from_secs(30);
const FS: f64 = 12_800.0;

fn no_driver() -> FixedScanner {
    FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )))
}

fn row(index: u32, role: &str, accel_limit: Option<f64>) -> Value {
    let units = if role == "drive" { "V" } else { "g" };
    json!({
        "index": index, "enabled": true, "description": role,
        "role": role, "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": units, "sensitivity_mv_per_eu": if units == "V" { 1000 } else { 100 },
        "range_max": if units == "V" { 10 } else { 100 }, "polarity": "positive",
        "control_group": null,
        "abort": {"accel_g": accel_limit, "velocity_ips": null, "displacement_mil": null}
    })
}

fn config(slew_oct: f64, slew_db: f64) -> Value {
    json!({
        "title": "Simulated Explorer",
        "acquisition": {"fs_hz": FS},
        "band_hz": [20, 2000], "start_hz": 250, "max_drive_vpk": 0.5,
        "slew": {"freq_oct_per_s": slew_oct, "drive_db_per_s": slew_db},
    })
}

struct Rig {
    svc: Arc<Service>,
    addr: std::net::SocketAddr,
    c: Client,
}

async fn rig(timeout: Duration, accel_limit: Option<f64>, cfg: Value) -> Rig {
    let svc = Service::with_options(options(
        no_driver(),
        1.0,
        timeout,
        Duration::from_millis(300),
    ));
    svc.start_background();
    let addr = serve(svc.clone()).await;
    let mut c = Client::connect(addr).await;
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": [
            row(1, "control", accel_limit), row(2, "drive", None), row(3, "monitor", None)
        ]}),
    )
    .await;
    c.ok("explorer.configure", json!({"config": cfg})).await;
    Rig { svc, addr, c }
}

async fn state_is(c: &mut Client, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let from = c.events.len();
    let i = c
        .wait_for(what, LONG, |e| {
            e.topic() == "explorer.state" && pred(e.data())
        })
        .await;
    let _ = from;
    c.events[i].data().clone()
}

/// Read the connection for `d`, as a screen would, so the service's frames never back up.
async fn listen(c: &mut Client, d: Duration) {
    let until = Instant::now() + d;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        let _ = c.read(left).await;
    }
}

/// The station's own response magnitude at `hz`: the feedthrough plus its two modes.
fn station_gain(hz: f64) -> f64 {
    let mut h = hertzforge_sdm::C64::new(0.35, 0.0);
    for (f, q) in SIM_RESONANCES {
        h += Resonator::new(f, q, FS).response(hz, FS);
    }
    h.norm()
}

#[tokio::test]
async fn explorer_sim_1_dwells_meet_the_stations_response_and_the_peak_is_a_mode() {
    let mut r = rig(Duration::from_secs(30), None, config(4.0, 240.0)).await;
    r.c.ok("explorer.start", json!({})).await;
    let mut frames = Vec::new();
    let first =
        r.c.ok("explorer.steer", json!({"hz": 50, "vpk": 0.2}))
            .await;
    frames.push(first["frame"].as_u64().unwrap());
    listen(&mut r.c, Duration::from_millis(600)).await;
    for hz in [100.0, 200.0, 300.0, 550.0, 1000.0] {
        let s = r.c.ok("explorer.steer", json!({"hz": hz})).await;
        frames.push(s["frame"].as_u64().unwrap());
        listen(&mut r.c, Duration::from_millis(600)).await;
    }
    let sv = r.c.ok("explorer.survey", json!({})).await;
    let pts: Vec<(f64, f64)> = sv["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
        .collect();
    for hz in [50.0, 100.0, 200.0, 300.0, 550.0] {
        let (_, level) = pts
            .iter()
            .find(|(f, _)| (f - hz).abs() < 1e-6)
            .unwrap_or_else(|| panic!("a point at {hz} Hz: {pts:?}"));
        let want = 0.2 * station_gain(hz);
        assert!(
            (level - want).abs() / want < 0.01,
            "{hz} Hz: the survey reads {level}, the station's response {want}"
        );
    }
    // A sweep across the band: the survey's peak is at a mode.
    // A swept point is measured over the freshest 100 ms and set at the tone's newest
    // frequency, so it trails by half a window of travel: 0.05 octave at 1 oct/s.
    r.c.ok("explorer.steer", json!({"hz": 120})).await;
    listen(&mut r.c, Duration::from_millis(300)).await;
    r.c.ok(
        "explorer.sweep",
        json!({"direction": "up", "rate_oct_per_min": 60}),
    )
    .await;
    listen(&mut r.c, Duration::from_millis(1700)).await;
    let sv = r.c.ok("explorer.survey", json!({})).await;
    // The station's own response peak over the band, where its two modes and the
    // feedthrough add (not at either mode's own frequency).
    let (true_hz, true_gain) = (0..2000)
        .map(|i| 20.0 * 100f64.powf(i as f64 / 1999.0))
        .map(|f| (f, station_gain(f)))
        .fold((0.0, 0.0), |b, p| if p.1 > b.1 { p } else { b });
    let peak = sv["peak"]["hz"].as_f64().unwrap();
    let level = sv["peak"]["level"].as_f64().unwrap();
    assert!(
        (peak / true_hz).log2().abs() < 0.1,
        "the survey's peak is at {peak} Hz, the station's at {true_hz} Hz"
    );
    assert!(
        (level - 0.2 * true_gain).abs() / (0.2 * true_gain) < 0.02,
        "the peak reads {level} against {}",
        0.2 * true_gain
    );
    // Station time is the station's frames, and the intents' frames are the answers'.
    let v = r.c.ok("explorer.state", json!({})).await;
    let t = v["t_s"].as_f64().unwrap();
    assert!(((t * FS) - (t * FS).round()).abs() < 1e-6, "{t}");
    r.c.ok("explorer.stop", json!({})).await;
    state_is(&mut r.c, "the end", |d| d["state"] == "finished").await;
    let log = r.c.ok("explorer.log", json!({})).await;
    let folder = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|e| {
            e["text"]
                .as_str()?
                .strip_prefix("records in ")
                .map(str::to_owned)
        })
        .expect("the log names the run folder");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::path::Path::new(&folder).join("end.json").exists() {
        assert!(Instant::now() < deadline);
        listen(&mut r.c, Duration::from_millis(50)).await;
    }
    let recorded: Vec<u64> =
        std::fs::read_to_string(std::path::Path::new(&folder).join("log.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|l| l["kind"] == "intent" && l["data"]["verb"] == "steer")
            .map(|l| l["data"]["frame"].as_u64().unwrap())
            .take(frames.len())
            .collect();
    assert_eq!(recorded, frames);
    drop(r.svc);
}

#[tokio::test]
async fn explorer_sim_2_steering_answers_what_the_step_adopted() {
    let mut r = rig(Duration::from_secs(30), None, config(1.0, 240.0)).await;
    r.c.ok("explorer.start", json!({})).await;
    let e = r.c.refused("explorer.steer", json!({"step_db": 3})).await;
    assert_eq!(e["reason"], "level_zero");
    let s = r.c.ok("explorer.steer", json!({"hz": 5000})).await;
    assert_eq!(
        (s["hz"].clone(), s["clamped"]["hz"].clone()),
        (json!(2000), json!(true))
    );
    let s = r.c.ok("explorer.steer", json!({"hz": 10})).await;
    assert_eq!(
        (s["hz"].clone(), s["clamped"]["hz"].clone()),
        (json!(20), json!(true))
    );
    let s =
        r.c.ok("explorer.steer", json!({"hz": 400, "vpk": 0.9}))
            .await;
    assert_eq!(s["vpk"], json!(0.5));
    assert_eq!(s["clamped"], json!({"hz": false, "vpk": true}));
    let s = r.c.ok("explorer.steer", json!({"step_oct": -1})).await;
    assert_eq!(s["hz"], json!(200));
    assert_eq!(
        s["vpk"],
        json!(0.5),
        "a frequency part keeps the level in force"
    );
    let s = r.c.ok("explorer.steer", json!({"step_db": -6})).await;
    assert!((s["vpk"].as_f64().unwrap() - 0.5 * 10f64.powf(-0.3)).abs() < 1e-9);
    let w =
        r.c.ok(
            "explorer.sweep",
            json!({"direction": "up", "rate_oct_per_min": 600}),
        )
        .await;
    assert_eq!(
        (w["rate_oct_per_min"].clone(), w["clamped"].clone()),
        (json!(60), json!(true))
    );
    let h = r.c.ok("explorer.hold", json!({})).await;
    assert!(h["frame"].as_u64().unwrap() >= w["frame"].as_u64().unwrap());
    r.c.ok("explorer.abort", json!({})).await;
}

#[tokio::test]
async fn explorer_sim_3_an_observer_aborts_and_is_refused_a_steer() {
    let mut r = rig(Duration::from_secs(30), None, config(4.0, 240.0)).await;
    r.c.ok("explorer.start", json!({})).await;
    r.c.ok("explorer.steer", json!({"vpk": 0.2})).await;
    listen(&mut r.c, Duration::from_millis(200)).await;
    let mut o = Client::connect(r.addr).await;
    let e = o.refused("explorer.steer", json!({"hz": 300})).await;
    assert_eq!(e["reason"], "not_controller");
    let asked = Instant::now();
    o.ok("explorer.abort", json!({})).await;
    let end = state_is(&mut o, "the abort", |d| d["state"] == "finished").await;
    assert!(asked.elapsed() < Duration::from_secs(2));
    assert_eq!(end["end"]["outcome"], "operator_abort", "{end}");
    assert_eq!(end["end"]["ramp_s"], Value::Null);
    assert_eq!(end["phase"], "aborted");
}

#[tokio::test]
async fn explorer_sim_4_a_level_during_the_stop_is_refused_and_the_ramp_ends() {
    let mut r = rig(Duration::from_secs(30), None, config(4.0, 60.0)).await;
    r.c.ok("explorer.start", json!({})).await;
    r.c.ok("explorer.steer", json!({"vpk": 0.2})).await;
    state_is(&mut r.c, "steady", |d| d["phase"] == "steady").await;
    r.c.ok("explorer.stop", json!({})).await;
    listen(&mut r.c, Duration::from_millis(300)).await;
    let e = r.c.refused("explorer.steer", json!({"vpk": 0.3})).await;
    assert_eq!(e["reason"], "wrong_state");
    assert_eq!(e["detail"]["state"], "stopping");
    let end = state_is(&mut r.c, "the end", |d| d["state"] == "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_stop");
    let ramp = end["end"]["ramp_s"].as_f64().unwrap();
    assert!(
        (ramp - 40.0 / 60.0).abs() < 0.06,
        "the ramp took {ramp} s of station time"
    );
}

#[tokio::test]
async fn explorer_sim_5_a_lost_controller_and_a_shutdown_end_in_the_safe_stop() {
    // The controller's socket closes mid-sweep: the run goes on through the grace.
    let r = rig(Duration::from_millis(1500), None, config(4.0, 240.0)).await;
    let mut c = r.c;
    c.ok("explorer.start", json!({})).await;
    c.ok("explorer.steer", json!({"vpk": 0.2, "hz": 100})).await;
    c.ok(
        "explorer.sweep",
        json!({"direction": "up", "rate_oct_per_min": 30}),
    )
    .await;
    let mut o = Client::connect(r.addr).await;
    drop(c);
    listen(&mut o, Duration::from_millis(700)).await;
    let mid = o.ok("explorer.state", json!({})).await;
    assert_eq!(mid["state"], "running", "the run goes on through the grace");
    let end = state_is(&mut o, "the safe stop", |d| d["state"] == "finished").await;
    assert_eq!(end["end"]["outcome"], "safe_stop", "{end}");
    assert_eq!(end["stop"], "safe_stop");

    // A shutdown: the drive verbs are refused, and the run ends in the safe stop.
    let mut r = rig(Duration::from_secs(30), None, config(4.0, 60.0)).await;
    r.c.ok("explorer.start", json!({})).await;
    r.c.ok("explorer.steer", json!({"vpk": 0.2})).await;
    let svc = r.svc.clone();
    let down = tokio::spawn(async move { svc.shutdown().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let e = r.c.refused("explorer.steer", json!({"vpk": 0.3})).await;
    assert_eq!(e["reason"], "shutting_down");
    let said = down.await.unwrap();
    assert!(said.is_some_and(|s| s.contains("safe_stop")));
    let v = r.c.ok("explorer.state", json!({})).await;
    assert_eq!(v["end"]["outcome"], "safe_stop");
    assert!(v["end"]["message"]
        .as_str()
        .unwrap()
        .starts_with("The service shut down"));
    assert_eq!(v["stop"], "shutdown");
}

#[tokio::test]
async fn explorer_sim_7_an_accel_limit_aborts_by_the_watchdog() {
    let mut r = rig(Duration::from_secs(30), Some(0.15), config(4.0, 240.0)).await;
    r.c.ok("explorer.start", json!({})).await;
    r.c.ok("explorer.steer", json!({"vpk": 0.5})).await;
    let end = state_is(&mut r.c, "the trip", |d| d["state"] == "finished").await;
    assert_eq!(end["end"]["outcome"], "aborted", "{end}");
    let trip = &end["end"]["trip"];
    assert_eq!(trip["kind"], "watchdog");
    assert_eq!(trip["axis"], "accel");
    assert_eq!(trip["channel"], 1);
    assert!(trip["value"].as_f64().unwrap() >= 0.15);
}

#[tokio::test]
async fn explorer_sim_8_a_simulation_run_and_an_explorer_run_exclude_each_other() {
    let mut r = rig(Duration::from_secs(30), None, config(4.0, 240.0)).await;
    r.c.ok("explorer.start", json!({})).await;
    let e =
        r.c.refused("sim.start", json!({"profile": "ramp_plateau", "speed": 1}))
            .await;
    assert_eq!(e["reason"], "run_active");
    r.c.ok("explorer.abort", json!({})).await;
    state_is(&mut r.c, "the abort", |d| d["state"] == "finished").await;
    r.c.ok("explorer.reset", json!({})).await;
    r.c.ok(
        "sim.start",
        json!({"profile": "ramp_plateau", "speed": 1, "duration_s": 20}),
    )
    .await;
    let e = r.c.refused("explorer.start", json!({})).await;
    assert_eq!(e["reason"], "run_active");
    r.c.ok("sim.stop", json!({})).await;
}
