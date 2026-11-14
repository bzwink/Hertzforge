//! Every TWR run leaves a run folder (docs/run-folder.md): the record written at
//! arm, the log as it is made, the streams of shots (the identification and every
//! play), each play's results as it is judged, the law's calls, and the end, on the
//! simulator through the protocol, on the controller's own TWR service. The play
//! the run ends on and a play cut short are recorded as far as they went.
//!
//! Every run here is SIMULATED (`hertzforge-hal-sim` through the station factory);
//! nothing in this file is evidence about hardware.

mod support;

use hertzforge_station::StationBuildError;
use sds_control::server::{Service, ServiceOptions};
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use support::schema::Schema;
use support::{options, serve, Client};

const LONG: Duration = Duration::from_secs(120);

/// The reference's rate, Hz.
const FS: f64 = 2048.0;

fn no_driver() -> FixedScanner {
    FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )))
}

/// The service on the controller's own TWR service, at `speed` times real time (0
/// unpaced).
fn real(speed: f64) -> ServiceOptions {
    let mut o = options(
        no_driver(),
        speed,
        Duration::from_secs(60),
        Duration::from_secs(5),
    );
    o.twr_engine = None;
    o.mimo_driver = None;
    o
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn wait_complete(folder: &Path, wait: Duration) {
    let t0 = Instant::now();
    loop {
        let done = folder.join("end.json").is_file()
            && read_json(&folder.join("run.json"))["complete"] == true;
        if done {
            return;
        }
        assert!(
            t0.elapsed() < wait,
            "the record of {} did not complete",
            folder.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn row(index: u32, role: &str) -> Value {
    let drive = role == "drive";
    json!({
        "index": index, "enabled": true, "description": role,
        "role": role, "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": if drive { "V" } else { "g" },
        "sensitivity_mv_per_eu": if drive { 1000 } else { 100 },
        "range_max": if drive { 10 } else { 100 }, "polarity": "positive",
        "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

/// Two decaying tones, `n` samples at 2048 Hz.
fn reference_text(n: usize) -> String {
    let mut text = String::new();
    for i in 0..n {
        let t = i as f64 / FS;
        let tone = |f: f64, tau: f64, a: f64| {
            a * (-t / tau).exp() * (2.0 * std::f64::consts::PI * f * t).sin()
        };
        let v = tone(150.0, 0.03, 0.4) + tone(330.0, 0.02, 0.3);
        text.push_str(&format!("{v}\n"));
    }
    text
}

/// A short automatic run: identification for half a second, two levels gated at
/// 5.5 %, `plays` playback plays.
fn config(sha: &Value, mode: &str, plays: u64) -> Value {
    json!({
        "title": "Recorded TWR",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "two levels",
        "specification": {"document": "Example procedure", "clause": "4.2"},
        "reference": {"sha256": sha},
        "pads": {"pre": 32, "post": 64},
        "identification": {"drive_vrms": 0.1, "duration_s": 0.5, "nfft": 256,
                           "band_hz": [20, 800], "review": false},
        "schedule": {"mode": mode, "levels_db": [-6, 0], "gate_pct": 5.5},
        "run": {"schedule": "loop", "count": plays},
        "limits": {"drive_vpk_ceiling": 5}
    })
}

/// Claim, select the simulator, set the table and import a reference of `n`
/// samples; the reference's SHA-256.
async fn controller(c: &mut Client, n: usize) -> Value {
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": [row(1, "control"), row(2, "monitor"),
                                             row(3, "drive")]}),
    )
    .await;
    c.ok(
        "twr.reference.import",
        json!({"text": reference_text(n), "fs_hz": FS, "name": "field"}),
    )
    .await["reference"]["sha256"]
        .clone()
}

async fn wait_finished(c: &mut Client) -> Value {
    let i = c
        .wait_for("the run's end", LONG, |e| {
            e.topic() == "twr.state" && e.data()["state"] == "finished"
        })
        .await;
    c.events[i].data().clone()
}

async fn run_to_end(
    c: &mut Client,
    samples: usize,
    cfg: impl FnOnce(&Value) -> Value,
) -> (Value, PathBuf) {
    let sha = controller(c, samples).await;
    c.ok("twr.configure", json!({ "config": cfg(&sha) })).await;
    c.ok("twr.start", json!({})).await;
    let end = wait_finished(c).await;
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    (end, folder)
}

/// The standard, item by item, for a TWR run on the simulator: the table, the
/// station, the setup as accepted and resolved, the limits, the law, the timeline
/// with its data, every enabled input and the drive as shots (the identification
/// and every play), every play's results the last included, every law call, the
/// provenance and a unit on every array.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_twr_run_leaves_a_complete_self_describing_folder() {
    let schema = Schema::load();
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    let (end, folder) = run_to_end(&mut c, 160, |sha| config(sha, "auto", 2)).await;
    schema.assert_valid(&end, Some("twrState"));
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    assert_eq!(end["record"]["streams"], "on");
    wait_complete(&folder, LONG);
    let plays = c.ok("twr.plays", json!({})).await["plays"]
        .as_array()
        .unwrap()
        .clone();

    // 9. provenance; 2. the station
    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["schema"], "hertzforge-run/1");
    assert_eq!(run["test"], "twr");
    assert_eq!(run["source"], "SIM");
    assert_eq!(run["operator"], "Operator");
    assert_eq!(run["article"], "Bracket");
    assert_eq!(run["specification"]["clause"], "4.2");
    assert!(run["sim"]["seed"].is_u64(), "{run}");
    assert_eq!(run["station"]["class"], "sim");
    assert!(run["build"]["commit"].is_string());
    assert_eq!(run["grid"]["record_len"], 256);
    assert_eq!(run["complete"], true);
    let names: Vec<&str> = run["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    for want in [
        "reference",
        "target",
        "identification_frf",
        "identification_coherence",
        "plays",
        "law_calls",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }

    // 1. the table, every column
    let table = read_json(&folder.join("table.json"));
    assert_eq!(table["rows"].as_array().unwrap().len(), 3);
    assert_eq!(table["sha256"].as_str().unwrap().len(), 64);

    // 3-6. the setup as accepted and resolved, the limits, the control settings
    let setup = read_json(&folder.join("setup.json"));
    assert_eq!(setup["config"]["title"], "Recorded TWR");
    let r = &setup["resolved"];
    assert_eq!(r["reference"]["samples"], 160);
    assert_eq!(r["reference"]["units"], "g");
    assert_eq!(r["reference"]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(r["reference"]["fs_hz"], 2048);
    assert_eq!(r["record"]["section_start"], 32);
    assert_eq!(r["record"]["section_len"], 160);
    assert_eq!(r["schedule"]["levels_db"], json!([-6, 0]));
    assert_eq!(r["schedule"]["gate_pct"], 5.5);
    assert_eq!(r["schedule"]["plays_target"], 2);
    assert_eq!(r["identification"]["nfft"], 256);
    assert_eq!(r["limits"]["drive_vpk_ceiling"], 5);
    assert_eq!(r["limits"]["envelope_abort_frac"], 0.5);
    assert_eq!(r["control"]["law"]["id"], "twr_correct");
    assert_eq!(r["control_channel"], 1);
    assert_eq!(r["drive_channel"], 3);
    assert!(setup["derived"]["call_budget_s"].is_number(), "{setup}");
    assert_eq!(setup["feasibility"]["verdict"], "not_evaluated");
    let law = read_json(&folder.join("law/law.json"));
    assert_eq!(law["id"], "twr_correct");
    assert_eq!(law["builtin"], true);

    // 7. the timeline: states, every play with its level, error and outcome, the end
    let log = read_lines(&folder.join("log.jsonl"));
    for (k, e) in log.iter().enumerate() {
        assert_eq!(e["seq"], k, "{e}");
    }
    let played: Vec<&Value> = log.iter().filter(|e| e["kind"] == "play").collect();
    assert_eq!(played.len(), plays.len(), "{log:?}");
    for (k, p) in played.iter().enumerate() {
        assert_eq!(p["data"]["play"], k, "{p}");
        assert!(p["data"]["level_db"].is_number(), "{p}");
        assert!(p["data"]["rms_error_pct"].is_number(), "{p}");
        assert_eq!(p["data"]["outcome"], "judged", "{p}");
        assert!(p["frame"].is_u64(), "{p}");
    }
    assert!(log
        .iter()
        .any(|e| e["kind"] == "state" && e["data"]["to"] == "sysid"));
    let last = log.last().unwrap();
    assert_eq!(last["data"]["outcome"], "completed", "{last}");

    // 8. the data: every enabled input and the drive, one shot each for the
    // identification and every play
    for (name, width) in [("inputs", 2usize), ("drives", 1)] {
        let side = read_json(&folder.join(format!("streams/{name}/stream.json")));
        assert_eq!(side["complete"], true, "{side}");
        assert_eq!(side["channels"].as_array().unwrap().len(), width);
        let shots = side["shots"].as_array().unwrap();
        assert_eq!(shots.len(), plays.len() + 1, "{side}");
        assert_eq!(shots[0]["what"], "identification");
        let mut at = 0;
        for (k, s) in shots.iter().enumerate() {
            assert_eq!(s["first_frame"], at, "{name} shot {k}: {s}");
            at += s["frames"].as_u64().unwrap();
            if k > 0 {
                let p = &plays[k - 1];
                assert_eq!(s["what"], "play");
                assert_eq!(s["play"], k - 1);
                assert_eq!(s["level_db"], p["level_db"], "{s}");
                assert_eq!(s["kind"], p["kind"], "{s}");
                assert_eq!(s["outcome"], "judged", "{s}");
                assert_eq!(s["pre_frames"], 32, "{s}");
                // the shot is the play the core clocked: its record and rest pad
                assert_eq!(s["frames"], p["frames"], "{s} against {p}");
                assert_eq!(
                    s["first_station_frame"], p["start_frame"],
                    "{s} against {p}"
                );
            }
        }
        assert_eq!(side["frames"], at, "{side}");
        let bytes = std::fs::metadata(folder.join(format!("streams/{name}/telemetry.f32")))
            .unwrap()
            .len();
        assert_eq!(bytes, at * width as u64 * 4);
    }
    let drives = read_json(&folder.join("streams/drives/stream.json"));
    assert!(
        drives["pipeline_lag_frames"]["frames"].is_number(),
        "{drives}"
    );
    // ... and the results of every play, the last included, as judged
    let lines = read_lines(&folder.join("results/plays.jsonl"));
    assert_eq!(lines.len(), plays.len(), "{lines:?}");
    for (line, p) in lines.iter().zip(&plays) {
        assert_eq!(line["index"], p["index"]);
        assert_eq!(line["level_db"], p["level_db"]);
        assert_eq!(line["gated"], p["gated"]);
        assert_eq!(line["outcome"], "judged");
    }
    assert_eq!(lines.last().unwrap()["kind"], "playback");
    // ... and every call of the law: the seed, and one after each play that
    // missed its gate
    let calls = read_lines(&folder.join("results/law_calls.jsonl"));
    let missed = plays.iter().filter(|p| p["gated"] == false).count();
    assert_eq!(calls.len(), 1 + missed, "{calls:?}");
    assert_eq!(calls[0]["call"], "seed");
    assert!(calls[1..].iter().all(|c| c["call"] == "iterate"));
    // 10. a unit on every array
    for name in &names {
        let d = read_json(&folder.join(format!("results/{name}.json")));
        for a in d["arrays"].as_array().unwrap() {
            assert!(a["unit"].is_string(), "{name}: {a}");
        }
    }
    assert_eq!(
        read_json(&folder.join("results/target.json"))["kind"],
        "transient_reference"
    );
    let e = read_json(&folder.join("end.json"));
    assert_eq!(e["end"], end["end"]);
    assert_eq!(e["streams"]["inputs"]["drops"], 0);
    // the controller's own capture is gone: nothing beside the run folder
    let root = folder.parent().unwrap();
    for entry in std::fs::read_dir(root).unwrap() {
        let p = entry.unwrap().path();
        assert!(
            !p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("twr_run"),
            "a capture folder {} beside the record",
            p.display()
        );
    }
}

/// The arm gate: a record that cannot be written refuses the arm, naming the path
/// and the reason, and nothing is armed. Red if the arm went ahead without a record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_data_folder_that_cannot_be_written_refuses_the_twr_arm() {
    let mut o = real(0.0);
    let blocked = o.data_root.join("not-a-folder");
    std::fs::create_dir_all(&o.data_root).unwrap();
    std::fs::write(&blocked, b"a file").unwrap();
    o.capture_root = Some(blocked.clone());
    let addr = serve(Service::with_options(o)).await;
    let mut c = Client::connect(addr).await;
    let sha = controller(&mut c, 160).await;
    c.ok(
        "twr.configure",
        json!({ "config": config(&sha, "auto", 1) }),
    )
    .await;
    let e = c.refused("twr.arm", json!({})).await;
    assert_eq!(e["reason"], "record_unwritable", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains(&blocked.display().to_string()),
        "{e}"
    );
    assert_eq!(e["detail"]["path"], blocked.display().to_string());
    let s = c.ok("twr.state", json!({})).await;
    assert_eq!(s["state"], "configured", "{s}");
    assert!(s.get("record").is_none(), "{s}");
    let e = c.refused("twr.start", json!({})).await;
    assert_eq!(e["reason"], "record_unwritable", "{e}");
}

/// `record.streams: false` keeps the record and its results and leaves out the
/// streams, and the manifest and the log say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twr_record_streams_false_writes_the_record_without_streams() {
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    let (end, folder) = run_to_end(&mut c, 160, |sha| {
        let mut cfg = config(sha, "auto", 1);
        cfg["record"] = json!({"streams": false});
        cfg
    })
    .await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    assert_eq!(end["record"]["streams"], "off");
    wait_complete(&folder, LONG);
    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["recording"]["why"], "switched off in the setup");
    assert_eq!(run["streams"], json!([]));
    assert!(!folder.join("streams").exists());
    let rows = c.ok("twr.plays", json!({})).await["next"].as_u64().unwrap();
    assert_eq!(
        read_lines(&folder.join("results/plays.jsonl")).len() as u64,
        rows
    );
    let log = read_lines(&folder.join("log.jsonl"));
    assert!(log.iter().any(|e| e["kind"] == "record"), "{log:?}");
}

/// The play a protection ends the run on is recorded as far as it was acquired, and
/// marked: an abort band tighter than the first play's error cuts that play inside
/// itself (a record long enough that most of it streams after the station starts,
/// so the cut is inside it). Its shot is shorter than a whole play, both streams say
/// it tripped, its
/// results name the trip, and the log's last entry is the end. Red if the cut play
/// were left out (the record before this change) or not marked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_twr_play_cut_by_a_protection_is_recorded_as_far_as_it_went() {
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    let (end, folder) = run_to_end(&mut c, 6144, |sha| {
        let mut cfg = config(sha, "auto", 1);
        cfg["pads"] = json!({"pre": 512, "post": 1024});
        cfg["limits"] = json!({"drive_vpk_ceiling": 5, "envelope_alarm_frac": 0.01,
                               "envelope_abort_frac": 0.02, "envelope_debounce_samples": 2});
        cfg
    })
    .await;
    assert_eq!(end["end"]["outcome"], "aborted", "{end}");
    assert_eq!(end["end"]["trip"]["kind"], "envelope", "{end}");
    assert_eq!(end["end"]["trip"]["in_play"], true, "{end}");
    let tripped = end["end"]["trip"]["play"].as_u64().unwrap();
    wait_complete(&folder, LONG);
    let lines = read_lines(&folder.join("results/plays.jsonl"));
    let last = lines.last().unwrap();
    assert_eq!(last["index"], tripped, "{lines:?}");
    assert_eq!(last["outcome"], "tripped", "{last}");
    assert_eq!(last["trip"]["kind"], "envelope", "{last}");
    for name in ["inputs", "drives"] {
        let side = read_json(&folder.join(format!("streams/{name}/stream.json")));
        let shots = side["shots"].as_array().unwrap();
        let shot = shots.last().unwrap();
        assert_eq!(shot["play"], tripped, "{side}");
        assert_eq!(shot["outcome"], "tripped", "{shot}");
        let frames = shot["frames"].as_u64().unwrap();
        assert!(frames > 0, "{shot}");
        // shorter than its record (8192 samples) and its rest pad
        assert!(frames < 8192, "{shot}");
    }
    let log = read_lines(&folder.join("log.jsonl"));
    let play = log.iter().rfind(|e| e["kind"] == "play").unwrap();
    assert_eq!(play["data"]["outcome"], "tripped", "{play}");
    assert!(log.iter().any(|e| e["kind"] == "trip"), "{log:?}");
    assert_eq!(log.last().unwrap()["data"]["outcome"], "aborted");
}

/// A play a stop cuts short is recorded as far as it was acquired: its shot and its
/// results say it was cancelled and after how many frames, as the run's end does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_twr_play_a_stop_cuts_short_is_recorded_as_cancelled() {
    let addr = serve(Service::with_options(real(1.0))).await;
    let mut c = Client::connect(addr).await;
    // a long reference, so the play is still in flight when the stop arrives
    let sha = controller(&mut c, 6144).await;
    let mut cfg = config(&sha, "manual", 1);
    cfg["pads"] = json!({"pre": 512, "post": 1024});
    c.ok("twr.configure", json!({ "config": cfg })).await;
    c.ok("twr.start", json!({})).await;
    c.wait_for("waiting", LONG, |e| {
        e.topic() == "twr.state" && e.data()["phase"] == "waiting"
    })
    .await;
    c.ok("twr.iterate", json!({})).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    c.ok("twr.stop", json!({})).await;
    let end = wait_finished(&mut c).await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    let done = end["end"]["frames_done"].as_u64().expect("a play was cut");
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    wait_complete(&folder, LONG);
    let lines = read_lines(&folder.join("results/plays.jsonl"));
    let last = lines.last().unwrap();
    assert_eq!(last["outcome"], "cancelled", "{lines:?}");
    assert_eq!(last["frames_done"], done, "{last}");
    assert_eq!(last["index"], end["end"]["play"], "{last}");
    for name in ["inputs", "drives"] {
        let side = read_json(&folder.join(format!("streams/{name}/stream.json")));
        let shot = side["shots"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(shot["outcome"], "cancelled", "{shot}");
        assert!(shot["frames"].as_u64().unwrap() > 0, "{shot}");
    }
}

/// The plays do not depend on the disk: a record whose writer waits two seconds
/// before every write leaves every play's level, start and error and the outcome as
/// they are with no wait. The writer falls behind (the run ends long before its
/// record does) and nothing reaches the play thread. Simulated, paced at real time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_stalled_on_every_write_leaves_the_twr_plays_and_the_outcome_unchanged() {
    async fn timed(delay: Duration) -> (Value, Vec<Value>, PathBuf, f64) {
        let mut o = real(1.0);
        o.record.write_delay = delay;
        let addr = serve(Service::with_options(o)).await;
        let mut c = Client::connect(addr).await;
        let sha = controller(&mut c, 160).await;
        c.ok(
            "twr.configure",
            json!({ "config": config(&sha, "auto", 2) }),
        )
        .await;
        c.ok("twr.arm", json!({})).await;
        let t0 = Instant::now();
        c.ok("twr.start", json!({})).await;
        let end = wait_finished(&mut c).await;
        let wall = t0.elapsed().as_secs_f64();
        let rows = c.ok("twr.plays", json!({})).await["plays"]
            .as_array()
            .unwrap()
            .clone();
        let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
        (end, rows, folder, wall)
    }
    let (free, free_rows, _, free_wall) = timed(Duration::ZERO).await;
    let (stalled, stalled_rows, folder, stalled_wall) = timed(Duration::from_secs(2)).await;
    let starts = |rows: &[Value]| -> Vec<f64> {
        rows.iter()
            .map(|r| r["at_s"].as_f64().unwrap())
            .collect::<Vec<_>>()
    };
    println!(
        "no wait: plays at {:?} s, run {} s, wall {free_wall:.2} s; two seconds a write: plays \
         at {:?} s, run {} s, wall {stalled_wall:.2} s",
        starts(&free_rows),
        free["t_s"],
        starts(&stalled_rows),
        stalled["t_s"]
    );
    assert_eq!(free["end"]["outcome"], "completed", "{free}");
    assert_eq!(
        stalled["end"]["outcome"], free["end"]["outcome"],
        "{stalled}"
    );
    assert_eq!(free_rows.len(), stalled_rows.len());
    for (a, b) in free_rows.iter().zip(&stalled_rows) {
        for k in [
            "level_db",
            "kind",
            "iteration",
            "start_frame",
            "frames",
            "gated",
        ] {
            assert_eq!(a[k], b[k], "{k}: {a} against {b}");
        }
        let (x, y) = (
            a["rms_error_pct"].as_f64().unwrap(),
            b["rms_error_pct"].as_f64().unwrap(),
        );
        assert!((x - y).abs() < 1e-6, "rms error {x} against {y}");
    }
    assert_eq!(free["t_s"], stalled["t_s"]);
    assert!(
        (free_wall - stalled_wall).abs() < 1.0,
        "wall {free_wall} s against {stalled_wall} s"
    );
    // The record is still being written: the run did not wait for it.
    assert!(!folder.join("end.json").exists(), "the writer kept up");
}
