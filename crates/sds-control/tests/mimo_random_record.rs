//! Every MIMO Random run leaves a run folder (docs/run-folder.md): the record
//! written at arm, the log as it is made, the streams, the results and the end, on
//! the simulator through the protocol, checked against "What a MIMO Random Run
//! Saves".
//!
//! Every run here is SIMULATED (`hertzforge-hal-sim` through the station factory);
//! nothing in this file is evidence about hardware.

#[path = "support/mimo_runs.rs"]
mod mimo_runs;
mod support;

use hertzforge_station::StationBuildError;
use mimo_runs::{controller, setup, wait_mimo};
use sds_control::server::{Service, ServiceOptions};
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use support::schema::Schema;
use support::{options, serve, Client};

fn no_driver() -> FixedScanner {
    FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )))
}

/// The service with the controller's MIMO driver, the simulator at `speed`.
fn real(speed: f64) -> ServiceOptions {
    let mut o = options(
        no_driver(),
        speed,
        Duration::from_secs(600),
        Duration::from_secs(5),
    );
    o.mimo_driver = None;
    // Unpaced, the simulator outruns any disk: buffers for the whole run.
    o.record.stream_buffer_s = 600.0;
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

fn wait_complete(folder: &Path) {
    let t0 = Instant::now();
    while !(folder.join("end.json").is_file()
        && read_json(&folder.join("run.json"))["complete"] == true)
    {
        assert!(
            t0.elapsed() < Duration::from_secs(120),
            "the record of {} did not complete",
            folder.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Set up and run to the end; the final status and the folder.
async fn run_to_end(c: &mut Client, s: Value) -> (Value, PathBuf) {
    controller(c).await;
    c.ok("mimo_random.setup", json!({ "setup": s })).await;
    c.ok("mimo_random.start", json!({})).await;
    let end = wait_mimo(c, "the end", |d| {
        d["state"] == "finished" || d["state"] == "review"
    })
    .await;
    let folder = PathBuf::from(end["record"]["folder"].as_str().expect("the folder"));
    (end, folder)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mimo_random_run_leaves_a_complete_self_describing_folder() {
    let schema = Schema::load();
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    let (end, folder) = run_to_end(&mut c, setup(2.5)).await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    assert_eq!(end["record"]["streams"], "on");
    assert!(end["record"]["problem"].is_null(), "{end}");
    for e in c.of_topic("mimo_random.status") {
        schema.assert_valid(e.data(), Some("mimoStatus"));
    }
    wait_complete(&folder);

    // The manifest: the test, the operator's words, the source, never guessed.
    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["schema"], "hertzforge-run/1");
    assert_eq!(run["test"], "mimo_random");
    assert_eq!(run["run_id"], end["run_id"]);
    assert_eq!(run["source"], "SIM");
    assert_eq!(run["operator"], "Operator");
    assert_eq!(run["article"], "Bracket");
    assert_eq!(
        run["specification"],
        json!({"document": "Example procedure", "clause": "4.1"})
    );
    assert_eq!(run["station"]["kind"], "sim");
    assert_eq!(run["complete"], true);
    let declared: Vec<&str> = run["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    for want in [
        "reference",
        "identification_frf",
        "identification_coherence",
        "identification_response",
        "identification_noise",
        "identification_drive",
        "identification_cross",
        "identification",
        "control_spectra",
        "drive_spectra",
        "control_sdm",
        "drive_sdm",
        "law_calls",
    ] {
        assert!(declared.contains(&want), "{want} not in {declared:?}");
        assert!(
            folder.join(format!("results/{want}.json")).is_file(),
            "{want}"
        );
    }

    // The table and the setup as accepted and resolved.
    let table = read_json(&folder.join("table.json"));
    assert_eq!(table["rows"].as_array().unwrap().len(), 8);
    let s = read_json(&folder.join("setup.json"));
    assert_eq!(s["config"]["title"], "Golden MIMO Random");
    let r = &s["resolved"];
    assert_eq!(r["reference"]["n"], 4);
    assert_eq!(r["reference"]["band"], json!([80, 84]));
    assert_eq!(r["channels"]["drives"].as_array().unwrap().len(), 4);
    assert_eq!(r["law"]["id"], "pinv_closed");
    assert_eq!(r["schedule"]["levels"][0]["db"], 0.0);
    assert!(r["setup_hash"].as_str().unwrap().len() > 8);
    assert_eq!(s["derived"]["grid"]["lines"], 1025);
    let law = read_json(&folder.join("law/law.json"));
    assert_eq!(law["id"], "pinv_closed");
    assert!(law["values"].is_array());

    // The streams: every enabled input and every drive, the identification in them.
    let inputs = read_json(&folder.join("streams/inputs/stream.json"));
    let drives = read_json(&folder.join("streams/drives/stream.json"));
    assert_eq!(inputs["channels"].as_array().unwrap().len(), 4);
    assert_eq!(drives["channels"].as_array().unwrap().len(), 4);
    assert_eq!(drives["channels"][0]["index"], 5);
    let frames = inputs["frames"].as_u64().unwrap();
    let ended = read_json(&folder.join("end.json"));
    // The identification alone is 64 frames of 2048 points.
    assert!(frames > 64 * 2048, "{inputs}");
    assert!(drives["frames"].as_u64().unwrap() > 64 * 2048, "{drives}");
    assert_eq!(ended["streams"]["inputs"]["frames"], frames);
    assert_eq!(ended["streams"]["inputs"]["drops"], 0);
    assert_eq!(
        std::fs::metadata(folder.join("streams/inputs/telemetry.f32"))
            .unwrap()
            .len(),
        frames * 4 * 4
    );

    // The log, in order, the controller's own events in it.
    let log = read_lines(&folder.join("log.jsonl"));
    for (k, e) in log.iter().enumerate() {
        assert_eq!(e["seq"], k as u64, "{e}");
    }
    assert!(log
        .iter()
        .any(|e| e["kind"] == "state" && e["text"] == "noise floor to sys id"));
    assert!(log.iter().any(|e| e["kind"] == "operator"
        && e["text"]
            .as_str()
            .unwrap()
            .starts_with("finished: completed")));

    // The end, and the loops it counts.
    assert_eq!(ended["end"]["outcome"], "completed");
    let loops = read_lines(&folder.join("results/drive_spectra.jsonl"));
    assert_eq!(
        loops.len() as u64,
        ended["summary"]["loops"].as_u64().unwrap()
    );
    let calls = read_lines(&folder.join("results/law_calls.jsonl"));
    assert_eq!(calls[0]["call"], "seed");
    assert_eq!(calls[0]["law"]["id"], "pinv_closed");
    assert_eq!(
        calls.len() as u64,
        ended["summary"]["law_calls"].as_u64().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_data_folder_that_cannot_be_written_refuses_the_mimo_random_arm() {
    let mut o = real(0.0);
    // The data folder is a file: no folder can be made in it.
    let blocked = o.data_root.join("not-a-folder");
    std::fs::create_dir_all(&o.data_root).unwrap();
    std::fs::write(&blocked, b"a file").unwrap();
    o.capture_root = Some(blocked.clone());
    let addr = serve(Service::with_options(o)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    c.ok("mimo_random.setup", json!({"setup": setup(2.5)}))
        .await;
    let e = c.refused("mimo_random.start", json!({})).await;
    assert_eq!(e["reason"], "record_unwritable", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains(&blocked.display().to_string()),
        "{e}"
    );
    assert_eq!(e["detail"]["path"], blocked.display().to_string());
    let s = c.ok("mimo_random.status", json!({})).await;
    assert!(s["record"].is_null(), "{s}");
    assert_ne!(s["state"], "running", "{s}");
    assert!(
        s["drive_rms_v"]
            .as_array()
            .is_none_or(|v| v.iter().all(|x| x.as_f64() == Some(0.0))),
        "{s}"
    );
}

/// `record.streams: false` keeps the record and leaves out the streams, and the
/// manifest and the log say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mimo_random_record_streams_false_writes_the_record_without_streams() {
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    let mut s = setup(2.5);
    s["record"] = json!({"streams": false});
    let (end, folder) = run_to_end(&mut c, s).await;
    assert_eq!(end["record"]["streams"], "off");
    wait_complete(&folder);
    assert!(!folder.join("streams").exists());
    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["recording"]["streams"], "off");
    assert_eq!(run["streams"], json!([]));
    let log = read_lines(&folder.join("log.jsonl"));
    assert!(log.iter().any(|e| e["kind"] == "record"), "{log:?}");
    assert!(folder.join("results/control_spectra.bin").is_file());
}

/// The log is on the disk as the run goes: held at the review after
/// identification, the folder already holds the run's events, the reference and the
/// identification, and says the run has not ended; an abort then ends it with the
/// abort in the end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mimo_random_log_is_on_disk_before_the_run_ends() {
    let addr = serve(Service::with_options(real(0.0))).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    let mut s = setup(2.5);
    s["identification"]["review"] = json!(true);
    c.ok("mimo_random.setup", json!({ "setup": s })).await;
    c.ok("mimo_random.start", json!({})).await;
    let held = wait_mimo(&mut c, "the review hold", |d| {
        d["phase"] == "awaiting_review"
    })
    .await;
    let folder = PathBuf::from(held["record"]["folder"].as_str().unwrap());
    let t0 = Instant::now();
    let log = loop {
        let log = read_lines(&folder.join("log.jsonl"));
        if log.iter().any(|e| e["text"] == "sys id to gate")
            && folder.join("results/identification_frf.json").is_file()
        {
            break log;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "{log:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(log.iter().any(|e| e["text"] == "idle to noise floor"));
    assert!(!folder.join("end.json").exists());
    assert_eq!(read_json(&folder.join("run.json"))["complete"], false);
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the abort", |d| d["state"] == "finished").await;
    wait_complete(&folder);
    let ended = read_json(&folder.join("end.json"));
    assert_eq!(ended["end"]["outcome"], "operator_abort", "{ended}");
    let log = read_lines(&folder.join("log.jsonl"));
    assert!(log
        .iter()
        .any(|e| e["text"].as_str().unwrap().starts_with("operator_abort by")));
}

/// The drive does not wait on the disk: a record whose writer waits two seconds
/// before every write leaves the run's outcome, its loops and every number the
/// drive was made from as they are with no wait, and its wall time as it is.
/// Unpaced, the run makes its loops as fast as it can: a run thread that waited on
/// the writer even once a loop would take seconds longer here. The writer falls
/// behind (the run ends before its record does), so the streams lose blocks and
/// say so; nothing reaches the drive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_stalled_on_every_write_leaves_the_mimo_random_loop_and_the_outcome_unchanged() {
    async fn timed(delay: Duration) -> (Value, PathBuf, f64, Value) {
        let mut o = real(0.0);
        o.record.write_delay = delay;
        o.record.stream_buffer_s = 0.5;
        let addr = serve(Service::with_options(o)).await;
        let mut c = Client::connect(addr).await;
        controller(&mut c).await;
        c.ok("mimo_random.setup", json!({"setup": setup(2.5)}))
            .await;
        let t0 = Instant::now();
        c.ok("mimo_random.start", json!({})).await;
        let end = wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
        let wall = t0.elapsed().as_secs_f64();
        let report = c.ok("mimo_random.report", json!({})).await;
        let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
        (end, folder, wall, report)
    }
    let (free, _, free_wall, free_report) = timed(Duration::ZERO).await;
    let (stalled, folder, stalled_wall, stalled_report) = timed(Duration::from_secs(2)).await;
    println!(
        "no wait: {} loops, wall {free_wall:.2} s; two seconds a write: {} loops, wall \
         {stalled_wall:.2} s",
        free["loop"], stalled["loop"]
    );
    assert_eq!(free["end"]["outcome"], "completed", "{free}");
    assert_eq!(stalled["end"]["outcome"], free["end"]["outcome"]);
    assert_eq!(stalled["loop"], free["loop"]);
    // Every number the run made its drive from, the same.
    let r = |v: &Value, k: &str| v["report"][k].clone();
    for k in ["controls", "drives", "loops", "gates"] {
        assert_eq!(r(&stalled_report, k), r(&free_report, k), "{k}");
    }
    assert!(
        (free_wall - stalled_wall).abs() < 1.5,
        "wall {free_wall} s against {stalled_wall} s"
    );
    // The stalled writer was still behind when the run ended.
    assert!(
        !folder.join("end.json").is_file(),
        "the stalled writer kept up, so nothing was shown"
    );
}
