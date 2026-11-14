//! An Explorer run's record (`docs/run-folder.md`, "What an Explorer Run Saves"), on
//! the control service's simulated station: every run leaves a complete,
//! self-describing run folder through the shared record; a data folder that cannot be
//! written refuses the start before anything is driven; `record.streams: false` and a
//! disk under the free-space floor keep the record without the streams; a write that
//! fails while the run goes is a `record` entry and the state's `record.problem`, and
//! the run goes on; and a writer that stalls on every write changes nothing that
//! reaches the output, nor when.
//!
//! The runs are SIMULATED; nothing here is evidence about hardware.

mod support;

use hertzforge_station::StationBuildError;
use sds_control::record::RecordOptions;
use sds_control::server::Service;
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
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

fn config(fs_hz: f64, run_name: &str) -> Value {
    json!({
        "title": "Recorded Explorer",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "an Explorer record",
        "specification": {"document": "Example procedure", "clause": "4.2"},
        "acquisition": {"fs_hz": fs_hz},
        "band_hz": [20, 800], "start_hz": 200, "max_drive_vpk": 0.5,
        "slew": {"freq_oct_per_s": 4, "drive_db_per_s": 200},
        "display": {"nfft": 1024},
        "capture": {"run_name": run_name},
    })
}

async fn client(o: sds_control::server::ServiceOptions) -> Client {
    let svc = Service::with_options(o);
    svc.start_background();
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": [
            row(1, "control", "Control"), row(2, "drive", "Drive"), row(3, "monitor", "Fixture")
        ]}),
    )
    .await;
    c
}

fn opts() -> sds_control::server::ServiceOptions {
    options(no_driver(), 1.0, LONG, Duration::from_millis(300))
}

async fn state_until(c: &mut Client, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let t0 = Instant::now();
    loop {
        let s = c.ok("explorer.state", json!({})).await;
        if pred(&s) {
            return s;
        }
        assert!(t0.elapsed() < LONG, "{what}: {s}");
        c.pump(Duration::from_millis(20)).await;
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_else(|e| panic!("{path:?}: {e}")))
        .unwrap()
}

/// Wait for the record to have written its end (the run does not wait for it).
async fn complete(c: &mut Client, folder: &Path) {
    let t0 = Instant::now();
    while !(folder.join("end.json").is_file()
        && read_json(&folder.join("run.json"))["complete"] == true)
    {
        assert!(t0.elapsed() < LONG, "{folder:?} did not complete");
        c.pump(Duration::from_millis(20)).await;
    }
}

fn log_of(folder: &Path) -> Vec<Value> {
    std::fs::read_to_string(folder.join("log.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn samples(folder: &Path, stream: &str) -> Vec<f32> {
    std::fs::read(folder.join("streams").join(stream).join("telemetry.f32"))
        .unwrap()
        .chunks(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

/// A short run: the tone raised at 200 Hz, a survey point, a sweep, a hold, the stop.
async fn steered_run(c: &mut Client) -> (Value, PathBuf) {
    c.ok("explorer.start", json!({})).await;
    let s = c.ok("explorer.state", json!({})).await;
    let folder = PathBuf::from(
        s["record"]["folder"]
            .as_str()
            .expect("the state names the folder"),
    );
    c.ok("explorer.steer", json!({"vpk": 0.2})).await;
    let t0 = Instant::now();
    loop {
        let sv = c.ok("explorer.survey", json!({})).await;
        if sv["points"].as_array().is_some_and(|p| !p.is_empty())
            && c.ok("explorer.state", json!({})).await["phase"] == "steady"
        {
            break;
        }
        assert!(t0.elapsed() < LONG, "no survey point: {sv}");
        c.pump(Duration::from_millis(20)).await;
    }
    c.ok(
        "explorer.sweep",
        json!({"direction": "up", "rate_oct_per_min": 60}),
    )
    .await;
    c.pump(Duration::from_millis(300)).await;
    c.ok("explorer.hold", json!({})).await;
    c.pump(Duration::from_millis(300)).await;
    c.ok("explorer.stop", json!({})).await;
    let end = state_until(c, "the end", |s| s["state"] == "finished").await;
    (end, folder)
}

/// A run leaves a complete, self-describing folder: the manifest with the operator's
/// words and SIM from the station, the table and the setup as accepted, the log with
/// every intent in order and its facts, both streams with the drive as played, the
/// survey and the spectra, and the end. Red if any of it were missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explorer_run_leaves_a_complete_self_describing_folder() {
    let mut c = client(opts()).await;
    let cfg = config(4096.0, "explorer_record");
    c.ok("explorer.configure", json!({"config": cfg})).await;
    let (end, folder) = steered_run(&mut c).await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    assert_eq!(end["record"]["streams"], "on", "{end}");
    assert_eq!(end["record"]["problem"], Value::Null, "{end}");
    complete(&mut c, &folder).await;
    assert_eq!(folder.file_name().unwrap(), "explorer_record");

    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["schema"], "hertzforge-run/1");
    assert_eq!(run["test"], "explorer");
    assert_eq!(run["source"], "SIM");
    assert_eq!(run["station"]["kind"], "sim");
    assert_eq!(run["operator"], "Operator");
    assert_eq!(run["article"], "Bracket");
    assert_eq!(run["notes"], "an Explorer record");
    assert_eq!(run["specification"]["clause"], "4.2");
    assert_eq!(run["grid"]["actual"], true, "{run}");
    assert_eq!(run["grid"]["fs_hz"], 4096.0);
    assert_eq!(run["complete"], true);
    let names = |k: &str| -> Vec<String> {
        run[k]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names("streams"), ["inputs", "drives"]);
    assert_eq!(names("results"), ["survey", "spectra"]);
    assert!(run["run_uid"].as_str().unwrap().len() > 16);

    let setup = read_json(&folder.join("setup.json"));
    assert_eq!(setup["config"], cfg);
    assert_eq!(setup["resolved"]["drive"]["max_drive_vpk"], 0.5);
    assert_eq!(setup["resolved"]["survey"]["channel"], 1);
    let table = read_json(&folder.join("table.json"));
    assert_eq!(table["rows"].as_array().unwrap().len(), 3);

    // Every intent is a log entry with its facts, in the order taken.
    let log = log_of(&folder);
    let seqs: Vec<u64> = log.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (0..log.len() as u64).collect::<Vec<_>>());
    let verbs: Vec<&str> = log
        .iter()
        .filter(|e| e["kind"] == "intent")
        .map(|e| e["data"]["verb"].as_str().unwrap())
        .collect();
    assert_eq!(verbs, ["steer", "sweep", "hold", "stop"], "{log:?}");
    let steer = log.iter().find(|e| e["kind"] == "intent").unwrap();
    assert_eq!(steer["data"]["vpk"]["adopted"], 0.2);
    assert!(steer["text"].as_str().unwrap().starts_with("steer: "));
    assert!(log.iter().any(|e| e["text"]
        .as_str()
        .unwrap()
        .starts_with("ended operator_stop")));

    // The streams: every input, and the drive as played, up to 0.2 V pk.
    let inputs = read_json(&folder.join("streams/inputs/stream.json"));
    assert_eq!(inputs["kind"], "inputs");
    assert_eq!(inputs["channels"].as_array().unwrap().len(), 2);
    assert_eq!(inputs["complete"], true);
    let drives = read_json(&folder.join("streams/drives/stream.json"));
    assert_eq!(drives["channels"][0]["index"], 2);
    assert_eq!(drives["channels"][0]["unit"], "V");
    let drive = samples(&folder, "drives");
    assert_eq!(drive.len() as u64, drives["frames"].as_u64().unwrap());
    let top = drive.iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!((top - 0.2).abs() < 1e-3, "the drive's peak is {top}");
    assert_eq!(*drive.last().unwrap(), 0.0, "the stop ramped to 0 V");
    assert_eq!(
        samples(&folder, "inputs").len() as u64,
        2 * inputs["frames"].as_u64().unwrap()
    );
    // The stream holds the run to its end: no frame was left in a tap.
    let end_json = read_json(&folder.join("end.json"));
    assert_eq!(end_json["streams"]["drives"]["drops"], 0, "{end_json}");
    assert_eq!(end_json["streams"]["inputs"]["drops"], 0, "{end_json}");
    assert_eq!(
        inputs["frames"], end_json["station_frames"],
        "the input stream holds every frame the station read: {end_json}"
    );

    // The results: the survey and each channel's last spectrum.
    let survey = read_json(&folder.join("results/survey.json"));
    assert_eq!(survey["kind"], "survey");
    assert_eq!(survey["channels"]["responses"], json!([1]));
    assert_eq!(survey["arrays"][1]["unit"], "g");
    let spectra = read_json(&folder.join("results/spectra.json"));
    assert_eq!(spectra["kind"], "amplitude_spectrum");
    assert_eq!(spectra["channels"]["responses"], json!([1, 3]));
    assert_eq!(spectra["arrays"][0]["shape"], json!([2, 513]));

    assert_eq!(end_json["end"]["outcome"], "operator_stop");
    assert_eq!(
        end_json["measured"]["channels"].as_array().unwrap().len(),
        2
    );
    let _ = std::fs::remove_dir_all(folder.parent().unwrap());
}

/// A data folder that cannot be written refuses the start `record_unwritable`, naming
/// the path and the reason, before the station is opened. Red if the run had started.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explorer_a_data_folder_that_cannot_be_written_refuses_the_start() {
    let mut o = opts();
    let blocked = o.data_root.join("not-a-folder");
    std::fs::create_dir_all(&o.data_root).unwrap();
    std::fs::write(&blocked, b"a file").unwrap();
    o.capture_root = Some(blocked.clone());
    let mut c = client(o).await;
    c.ok(
        "explorer.configure",
        json!({"config": config(4096.0, "blocked")}),
    )
    .await;
    let e = c.refused("explorer.start", json!({})).await;
    assert_eq!(e["reason"], "record_unwritable", "{e}");
    let message = e["message"].as_str().unwrap();
    assert!(
        message.contains(&blocked.display().to_string()),
        "{message}"
    );
    assert_eq!(e["detail"]["path"], blocked.display().to_string());
    let s = c.ok("explorer.state", json!({})).await;
    assert_eq!(s["state"], "finished", "{s}");
    assert_eq!(s["end"]["outcome"], "arm_refused", "{s}");
    assert_eq!(s["t_s"], Value::Null, "nothing ran: {s}");
    assert_eq!(s["record"], Value::Null, "{s}");
}

/// `record.streams: false` keeps the record without its streams, and says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explorer_record_streams_false_writes_the_record_without_streams() {
    let mut c = client(opts()).await;
    let mut cfg = config(4096.0, "no_streams");
    cfg["record"] = json!({"streams": false});
    c.ok("explorer.configure", json!({"config": cfg})).await;
    c.ok("explorer.start", json!({})).await;
    c.ok("explorer.stop", json!({})).await;
    let end = state_until(&mut c, "the end", |s| s["state"] == "finished").await;
    assert_eq!(end["record"]["streams"], "off", "{end}");
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    complete(&mut c, &folder).await;
    let run = read_json(&folder.join("run.json"));
    assert_eq!(run["recording"]["streams"], "off");
    assert_eq!(run["recording"]["why"], "switched off in the setup");
    assert_eq!(run["streams"], json!([]));
    assert!(!folder.join("streams").exists());
    assert!(log_of(&folder)
        .iter()
        .any(|e| e["kind"] == "record" && e["data"]["stream"].is_null()));
}

/// Under the free-space floor at start the run goes on without its streams.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explorer_below_the_free_space_floor_the_run_goes_on_without_streams() {
    let mut o = opts();
    o.record = RecordOptions {
        free_space_floor_bytes: u64::MAX,
        ..RecordOptions::default()
    };
    let mut c = client(o).await;
    c.ok(
        "explorer.configure",
        json!({"config": config(4096.0, "floor")}),
    )
    .await;
    c.ok("explorer.start", json!({})).await;
    let s = c.ok("explorer.state", json!({})).await;
    assert_eq!(s["state"], "running", "{s}");
    assert_eq!(s["record"]["streams"], "off", "{s}");
    c.ok("explorer.stop", json!({})).await;
    let end = state_until(&mut c, "the end", |s| s["state"] == "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_stop");
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    complete(&mut c, &folder).await;
    let run = read_json(&folder.join("run.json"));
    assert!(
        run["recording"]["why"]
            .as_str()
            .unwrap()
            .contains("MB free"),
        "{run}"
    );
}

/// A write that fails while the run goes (here the streams crossing the free-space
/// floor) is a `record` entry in the log and the state's `record.problem`, in words;
/// the run goes on and the tone is still steered. Red if the problem were swallowed
/// or if it ended the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explorer_a_failed_write_is_a_problem_and_the_run_goes_on() {
    let mut o = opts();
    std::fs::create_dir_all(&o.data_root).unwrap();
    let free = hertzforge_capture::free_space_bytes(&o.data_root).unwrap();
    o.record = RecordOptions {
        flush_interval: Duration::from_millis(100),
        // crossed once the streams hold a few megabytes
        free_space_floor_bytes: free.saturating_sub(3_000_000),
        ..RecordOptions::default()
    };
    let mut c = client(o).await;
    c.ok(
        "explorer.configure",
        json!({"config": config(51_200.0, "problem")}),
    )
    .await;
    c.ok("explorer.start", json!({})).await;
    c.ok("explorer.steer", json!({"vpk": 0.1})).await;
    let s = state_until(&mut c, "a record problem", |s| {
        s["record"]["problem"].is_string()
    })
    .await;
    assert_eq!(s["state"], "running", "{s}");
    let problem = s["record"]["problem"].as_str().unwrap().to_owned();
    assert!(problem.contains("free space"), "{problem}");
    let log = c.ok("explorer.log", json!({})).await;
    assert!(
        log["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "record" && e["text"] == problem.as_str()),
        "{log}"
    );
    // The run goes on: the tone still answers.
    let st = c.ok("explorer.steer", json!({"hz": 300})).await;
    assert_eq!(st["hz"], 300.0);
    c.ok("explorer.stop", json!({})).await;
    let end = state_until(&mut c, "the end", |s| s["state"] == "finished").await;
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    assert_eq!(end["record"]["problem"], problem.as_str());
}

/// A writer that stalls two seconds on every write leaves the output as it was: the
/// steering answers come as fast, each change takes effect as close behind the frame
/// it was taken at, the station's loop turns as often (no wider gap between batches,
/// no fault, no underrun), the stop ramps for the same station time and the run ends
/// the same way at the same level. The run never waits for the record: the stalled
/// writer is still behind when the run has finished. Red if the run's thread or the
/// station's waited on a write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explorer_a_stalled_writer_leaves_the_output_unchanged() {
    struct Took {
        end: Value,
        answers_ms: Vec<f64>,
        leads: Vec<u64>,
        wall_s: f64,
        folder: PathBuf,
    }
    async fn timed(delay: Duration) -> Took {
        let mut o = opts();
        o.record.write_delay = delay;
        let mut c = client(o).await;
        c.ok(
            "explorer.configure",
            json!({"config": config(12_800.0, "stalled")}),
        )
        .await;
        let t0 = Instant::now();
        c.ok("explorer.start", json!({})).await;
        let mut answers_ms = Vec::new();
        let mut leads = Vec::new();
        for (k, p) in [
            json!({"vpk": 0.2}),
            json!({"hz": 300}),
            json!({"hz": 400, "vpk": 0.1}),
            json!({"hz": 250}),
            json!({"vpk": 0.3}),
        ]
        .into_iter()
        .enumerate()
        {
            c.pump(Duration::from_millis(if k == 0 { 100 } else { 400 }))
                .await;
            let asked = Instant::now();
            let s = c.ok("explorer.steer", p).await;
            answers_ms.push(asked.elapsed().as_secs_f64() * 1e3);
            // generated frame of the change against the input frame it was taken at:
            // how far ahead of the clock the output runs
            leads.push(
                s["frame"]
                    .as_u64()
                    .unwrap()
                    .saturating_sub(s["input_frame"].as_u64().unwrap()),
            );
        }
        c.pump(Duration::from_millis(400)).await;
        c.ok("explorer.stop", json!({})).await;
        let end = state_until(&mut c, "the end", |s| s["state"] == "finished").await;
        let wall_s = t0.elapsed().as_secs_f64();
        let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
        Took {
            end,
            answers_ms,
            leads,
            wall_s,
            folder,
        }
    }
    let free = timed(Duration::ZERO).await;
    let stalled = timed(Duration::from_secs(2)).await;
    let worst = |v: &[f64]| v.iter().cloned().fold(0.0, f64::max);
    let gap = |t: &Took| t.end["health"]["batch_gap_ms"].as_f64().unwrap_or(0.0);
    println!(
        "no wait: steer answers worst {:.1} ms, leads {:?} frames, batch gap worst {} ms, \
         ramp {} s, ended at {} s, wall {:.2} s; two seconds a write: steer answers worst \
         {:.1} ms, leads {:?} frames, batch gap worst {} ms, ramp {} s, ended at {} s, wall \
         {:.2} s",
        worst(&free.answers_ms),
        free.leads,
        gap(&free),
        free.end["end"]["ramp_s"],
        free.end["end"]["at_s"],
        free.wall_s,
        worst(&stalled.answers_ms),
        stalled.leads,
        gap(&stalled),
        stalled.end["end"]["ramp_s"],
        stalled.end["end"]["at_s"],
        stalled.wall_s,
    );
    for t in [&free, &stalled] {
        assert_eq!(t.end["end"]["outcome"], "operator_stop", "{}", t.end);
        assert_eq!(t.end["health"]["health_faults"], 0, "{}", t.end);
    }
    assert_eq!(stalled.end["end"]["vpk"], free.end["end"]["vpk"]);
    assert_eq!(stalled.end["end"]["at_hz"], free.end["end"]["at_hz"]);
    assert_eq!(
        stalled.end["end"]["ramp_s"], free.end["end"]["ramp_s"],
        "the stop ramped for another length of station time"
    );
    // The output runs as far ahead of the clock as without the stall.
    for (a, b) in free.leads.iter().zip(&stalled.leads) {
        assert!(
            a.abs_diff(*b) <= 512,
            "leads {:?} against {:?}",
            free.leads,
            stalled.leads
        );
    }
    // A steer that waited on a two-second write would answer seconds late.
    assert!(
        worst(&stalled.answers_ms) < 250.0,
        "steer answers {:?} ms",
        stalled.answers_ms
    );
    // A run thread that waited a tenth of the writer's stall at each publish would be
    // seconds slower here.
    assert!(
        (free.wall_s - stalled.wall_s).abs() < 1.0,
        "wall {} s against {} s",
        free.wall_s,
        stalled.wall_s
    );
    assert!(
        gap(&stalled) < gap(&free).max(5.0) * 4.0 + 20.0,
        "batch gap {} ms against {} ms",
        gap(&stalled),
        gap(&free)
    );
    // The stalled writer was still behind when the run ended.
    assert!(
        !stalled.folder.join("end.json").is_file(),
        "the stalled writer kept up, so nothing was shown"
    );
}
