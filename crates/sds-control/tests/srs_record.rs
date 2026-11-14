//! Every SRS shock run leaves a run folder (docs/run-folder.md): the record written
//! at arm, the log as it is made, the streams of shots, each pulse's results as it
//! is judged, and the end, on the simulator through the protocol, on the
//! controller's own SRS shock service.
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

fn no_driver() -> FixedScanner {
    FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )))
}

/// The service on the controller's own SRS shock service, at `speed` times real
/// time (0 unpaced).
fn real(speed: f64) -> ServiceOptions {
    let mut o = options(
        no_driver(),
        speed,
        Duration::from_secs(60),
        Duration::from_secs(5),
    );
    o.srs_engine = None;
    o.mimo_driver = None;
    o
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
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
    let units = if role == "drive" { "V" } else { "g" };
    json!({
        "index": index, "enabled": true, "description": role,
        "role": role, "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": units, "sensitivity_mv_per_eu": 1000,
        "range_max": 10, "polarity": "positive", "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

/// A flat 2 g target, 100 to 1000 Hz at 1/3 octave, Q 10, wavelets, at 4096 Hz;
/// three levels with a pulse each, advancing by count; the default law.
fn config(cooling_s: f64) -> Value {
    json!({
        "title": "Recorded SRS",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "three levels",
        "specification": {"document": "Example procedure", "clause": "4.2"},
        "target": {"kind": "flat", "accel_g": 2},
        "bands": {"f_lo_hz": 100, "f_hi_hz": 1000, "bands_per_octave": 3, "q": 10},
        "acquisition": {"fs_hz": 4096},
        "schedule": {"mode": "auto", "levels_db": [-12, -6, 0], "pulses": [1, 1, 1],
                     "advance": "count"},
        "spacing": {"cooling_s": cooling_s, "settling_s": 0, "compute_s": 0},
        "identification": {"nfft": 1024, "review": false},
        "limits": {"drive_vpk_ceiling": 5, "drive_vpk_abort": 8}
    })
}

async fn controller(c: &mut Client) {
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": [row(1, "control"), row(2, "monitor"),
                                             row(3, "drive")]}),
    )
    .await;
}

async fn run_to_end(c: &mut Client, config: Value) -> (Value, PathBuf) {
    controller(c).await;
    c.ok("srs.configure", json!({ "config": config })).await;
    c.ok("srs.start", json!({})).await;
    let i = c
        .wait_for("the run's end", LONG, |e| {
            e.topic() == "srs.state" && e.data()["state"] == "finished"
        })
        .await;
    let end = c.events[i].data().clone();
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    (end, folder)
}

/// The standard, item by item, for an SRS shock run on the simulator: the table,
/// the station, the setup as accepted and resolved, the limits, the law, the
/// timeline with its data, every enabled input and the drive as shots (the
/// identification and every pulse), every pulse's results the last included, the
/// provenance and a unit on every array.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_srs_run_leaves_a_complete_self_describing_folder() {
    let schema = Schema::load();
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    let (end, folder) = run_to_end(&mut c, config(0.05)).await;
    schema.assert_valid(&end, Some("srsState"));
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    assert_eq!(end["record"]["streams"], "on");
    wait_complete(&folder, LONG);

    // 9. provenance; 2. the station
    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["schema"], "hertzforge-run/1");
    assert_eq!(run["test"], "srs");
    assert_eq!(run["source"], "SIM");
    assert_eq!(run["operator"], "Operator");
    assert_eq!(run["article"], "Bracket");
    assert_eq!(run["specification"]["clause"], "4.2");
    assert!(run["sim"]["seed"].is_u64(), "{run}");
    assert_eq!(run["station"]["class"], "sim");
    assert!(run["build"]["commit"].is_string());
    assert_eq!(run["complete"], true);
    let names: Vec<&str> = run["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    for want in [
        "srs_reference",
        "synthesis",
        "identification_frf",
        "identification_coherence",
        "pulse_srs",
        "drive_tables",
        "pulses",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }

    // 1. the table, every column
    let table = read_json(&folder.join("table.json"));
    assert_eq!(table["rows"].as_array().unwrap().len(), 3);
    assert_eq!(table["sha256"].as_str().unwrap().len(), 64);

    // 3-6. the setup as accepted and resolved, the limits, the control settings
    let setup = read_json(&folder.join("setup.json"));
    assert_eq!(setup["config"]["title"], "Recorded SRS");
    let r = &setup["resolved"];
    assert_eq!(r["target"]["kind"], "flat");
    assert_eq!(r["bands"]["q"], 10);
    assert!(
        r["bands"]["centres_hz"].as_array().unwrap().len() > 5,
        "{r}"
    );
    assert_eq!(r["envelope"]["alarm_db"], json!([3, 3]), "{r}");
    assert_eq!(r["synthesis"]["engine"], "wavsyn");
    assert!(r["synthesis"]["table"]["rows"].as_array().unwrap().len() > 5);
    assert!(r["synthesis"]["pulse"]["peak_g"].is_number(), "{r}");
    assert_eq!(r["schedule"]["pulses"], json!([1, 1, 1]));
    assert_eq!(r["limits"]["drive_vpk_abort"], 8);
    assert_eq!(r["control"]["law"]["id"], "srs_ratio");
    assert_eq!(r["acquisition"]["fs_hz"], 4096);
    assert_eq!(r["control_channel"], 1);
    assert_eq!(r["drive_channel"], 3);
    assert!(setup["derived"]["spacing_s"].is_number(), "{setup}");
    assert_eq!(setup["feasibility"]["verdict"], "not_evaluated");
    let law = read_json(&folder.join("law/law.json"));
    assert_eq!(law["id"], "srs_ratio");
    assert_eq!(law["builtin"], true);

    // 7. the timeline: states, every pulse with its level and verdict, the end
    let log = read_lines(&folder.join("log.jsonl"));
    for (k, e) in log.iter().enumerate() {
        assert_eq!(e["seq"], k, "{e}");
    }
    let pulses: Vec<&Value> = log.iter().filter(|e| e["kind"] == "pulse").collect();
    assert_eq!(pulses.len(), 3, "{log:?}");
    for (k, p) in pulses.iter().enumerate() {
        assert_eq!(p["data"]["pulse"], k, "{p}");
        assert!(p["data"]["level_db"].is_number(), "{p}");
        assert!(p["data"]["passed"].is_boolean(), "{p}");
        assert!(p["frame"].is_u64(), "{p}");
    }
    assert!(log
        .iter()
        .any(|e| e["kind"] == "state" && e["data"]["to"] == "sysid"));
    assert!(
        log.iter().any(|e| e["data"]["identified"] == true),
        "{log:?}"
    );
    let last = log.last().unwrap();
    assert_eq!(last["data"]["outcome"], "completed", "{last}");

    // 8. the data: every enabled input and the drive, one shot each for the
    // identification and every pulse
    let record_len = end["grid"]["record_len"].as_u64().unwrap();
    for (name, width) in [("inputs", 2usize), ("drives", 1)] {
        let side = read_json(&folder.join(format!("streams/{name}/stream.json")));
        assert_eq!(side["complete"], true, "{side}");
        assert_eq!(side["channels"].as_array().unwrap().len(), width);
        let shots = side["shots"].as_array().unwrap();
        assert_eq!(shots.len(), 4, "{side}");
        assert_eq!(shots[0]["what"], "identification");
        assert!(shots[0]["frames"].as_u64().unwrap() > 0, "{side}");
        let mut at = 0;
        for (k, s) in shots.iter().enumerate() {
            assert_eq!(s["first_frame"], at, "{name} shot {k}: {s}");
            at += s["frames"].as_u64().unwrap();
            if k > 0 {
                assert_eq!(s["what"], "pulse");
                assert_eq!(s["pulse"], k - 1);
                assert_eq!(s["frames"], record_len, "{s}");
                assert!(s["level_db"].is_number());
                assert_eq!(s["station_frame"], s["first_station_frame"], "{s}");
            }
        }
        assert_eq!(side["frames"], at, "{side}");
        let bytes = std::fs::metadata(folder.join(format!("streams/{name}/telemetry.f32")))
            .unwrap()
            .len();
        assert_eq!(bytes, at * width as u64 * 4);
    }
    // ... and the results of every pulse, the last included, as judged
    let lines = read_lines(&folder.join("results/pulses.jsonl"));
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2]["index"], 2);
    assert!(lines[2]["passed"].is_boolean(), "{lines:?}");
    assert_eq!(read_lines(&folder.join("results/pulse_srs.jsonl")).len(), 3);
    assert_eq!(
        read_lines(&folder.join("results/drive_tables.jsonl")).len(),
        3
    );
    // 10. a unit on every array
    for name in &names {
        let d = read_json(&folder.join(format!("results/{name}.json")));
        for a in d["arrays"].as_array().unwrap() {
            assert!(a["unit"].is_string(), "{name}: {a}");
        }
    }
    let srs = read_json(&folder.join("results/pulse_srs.json"));
    assert_eq!(srs["kind"], "srs");
    let reference = read_json(&folder.join("results/srs_reference.json"));
    assert_eq!(reference["kind"], "srs_reference");
    assert_eq!(reference["q"], 10);
    let e = read_json(&folder.join("end.json"));
    assert_eq!(e["end"], end["end"]);
    assert_eq!(e["streams"]["inputs"]["drops"], 0);
}

/// The arm gate: a record that cannot be written refuses the arm, naming the path
/// and the reason, and nothing is armed. Red if the arm went ahead without a record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_data_folder_that_cannot_be_written_refuses_the_srs_arm() {
    let mut o = real(0.0);
    let blocked = o.data_root.join("not-a-folder");
    std::fs::create_dir_all(&o.data_root).unwrap();
    std::fs::write(&blocked, b"a file").unwrap();
    o.capture_root = Some(blocked.clone());
    let addr = serve(Service::with_options(o)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    c.ok("srs.configure", json!({ "config": config(0.05) }))
        .await;
    let e = c.refused("srs.arm", json!({})).await;
    assert_eq!(e["reason"], "record_unwritable", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains(&blocked.display().to_string()),
        "{e}"
    );
    assert_eq!(e["detail"]["path"], blocked.display().to_string());
    let s = c.ok("srs.state", json!({})).await;
    assert_eq!(s["state"], "configured", "{s}");
    assert!(s.get("record").is_none(), "{s}");
    let e = c.refused("srs.start", json!({})).await;
    assert_eq!(e["reason"], "record_unwritable", "{e}");
}

/// `record.streams: false` keeps the record and its results and leaves out the
/// streams, and the manifest and the log say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn srs_record_streams_false_writes_the_record_without_streams() {
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    let mut cfg = config(0.05);
    cfg["record"] = json!({"streams": false});
    let (end, folder) = run_to_end(&mut c, cfg).await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    assert_eq!(end["record"]["streams"], "off");
    wait_complete(&folder, LONG);
    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["recording"]["why"], "switched off in the setup");
    assert_eq!(run["streams"], json!([]));
    assert!(!folder.join("streams").exists());
    assert_eq!(read_lines(&folder.join("results/pulses.jsonl")).len(), 3);
    let log = read_lines(&folder.join("log.jsonl"));
    assert!(log.iter().any(|e| e["kind"] == "record"), "{log:?}");
}

/// The firing does not depend on the disk: a record whose writer waits two seconds
/// before every write leaves the pulses' starts, the levels fired and the outcome
/// as they are with no wait. The writer falls behind (the run ends long before its
/// record does) and nothing reaches the firing thread. Simulated, paced at real
/// time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_stalled_on_every_write_leaves_the_srs_firing_and_the_outcome_unchanged() {
    async fn timed(delay: Duration) -> (Value, Vec<Value>, PathBuf, f64) {
        let mut o = real(1.0);
        o.record.write_delay = delay;
        let addr = serve(Service::with_options(o)).await;
        let mut c = Client::connect(addr).await;
        controller(&mut c).await;
        c.ok("srs.configure", json!({ "config": config(0.5) }))
            .await;
        c.ok("srs.arm", json!({})).await;
        let t0 = Instant::now();
        c.ok("srs.start", json!({})).await;
        let i = c
            .wait_for("the run's end", Duration::from_secs(300), |e| {
                e.topic() == "srs.state" && e.data()["state"] == "finished"
            })
            .await;
        let wall = t0.elapsed().as_secs_f64();
        let end = c.events[i].data().clone();
        let rows = c.ok("srs.pulses", json!({})).await["pulses"]
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
        "no wait: pulses at {:?} s, elapsed {} s, wall {free_wall:.2} s; two seconds a write: \
         pulses at {:?} s, elapsed {} s, wall {stalled_wall:.2} s",
        starts(&free_rows),
        free["elapsed_s"],
        starts(&stalled_rows),
        stalled["elapsed_s"]
    );
    assert_eq!(free["end"]["outcome"], "completed", "{free}");
    assert_eq!(
        stalled["end"]["outcome"], free["end"]["outcome"],
        "{stalled}"
    );
    assert_eq!(free_rows.len(), stalled_rows.len());
    for (a, b) in free_rows.iter().zip(&stalled_rows) {
        assert_eq!(a["commanded_db"], b["commanded_db"], "{a} against {b}");
        let (x, y) = (a["at_s"].as_f64().unwrap(), b["at_s"].as_f64().unwrap());
        assert!((x - y).abs() < 0.25, "a pulse at {x} s against {y} s");
    }
    let (a, b) = (
        free["elapsed_s"].as_f64().unwrap(),
        stalled["elapsed_s"].as_f64().unwrap(),
    );
    assert!((a - b).abs() < 0.5, "elapsed {a} s against {b} s");
    assert!(
        (free_wall - stalled_wall).abs() < 1.0,
        "wall {free_wall} s against {stalled_wall} s"
    );
    // The record is still being written: the run did not wait for it.
    assert!(!folder.join("end.json").exists(), "the writer kept up");
}
