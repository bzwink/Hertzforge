//! The golden MIMO Random run folder, `schema/golden/runs/mimo_random/`, against
//! today's code: a short run on the simulator's 4 by 4 plant, unpaced, with the
//! simulator's and the run's fixed seeds, gives the golden folder once it is
//! normalized. The analysis side converts the same folder in
//! `tests/test_run_folder_golden_mimo_random.py`.
//!
//! Normalized means: the wall times, the run's unique identity and the build fields
//! fixed; each loop's measured durations (`timing`, wall-clock figures) set to zero;
//! and each stream cut to its first [`CUT_FRAMES`] frames, its `stream.json` and the
//! end's counts saying so (`cut_from_frames`, the frames the run recorded). The cut
//! is the golden folder's alone.
//!
//! `scripts/make_golden_runs.sh` rewrites the folder (this test with
//! `HERTZFORGE_WRITE_GOLDEN_RUNS=1`); otherwise the test only compares.
//!
//! The run is SIMULATED; nothing here is evidence about hardware.

#[path = "support/mimo_runs.rs"]
mod mimo_runs;
mod support;

use hertzforge_station::StationBuildError;
use mimo_runs::{controller, setup, wait_mimo};
use sds_control::server::Service;
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use support::golden_runs::{compare, files, lines_at, pretty, read_json, set, write_golden};
use support::{options, serve, Client};

/// The most the golden folder may hold, bytes.
const MOST_BYTES: usize = 64 * 1024;
/// The frames of each stream the golden folder keeps.
const CUT_FRAMES: u64 = 128;

/// The wall time every normalized folder starts at.
const START_UNIX: f64 = 1_794_240_000.25;
const UTC_OFFSET_S: i64 = -25_200;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/golden/runs/mimo_random")
}

/// Run the golden run on a fresh service and return its folder once complete.
async fn golden_run() -> PathBuf {
    let scan = FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )));
    let mut o = options(scan, 0.0, Duration::from_secs(600), Duration::from_secs(5));
    o.mimo_driver = None;
    // Unpaced, the simulator outruns any disk: buffers for the whole run, so no
    // frame is dropped and the run is the same every time.
    o.record.stream_buffer_s = 600.0;
    let addr = serve(Service::with_options(o)).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c).await;
    c.ok("mimo_random.setup", json!({"setup": setup(2.5)}))
        .await;
    c.ok("mimo_random.start", json!({})).await;
    let end = wait_mimo(&mut c, "the end", |d| {
        d["state"] == "finished" || d["state"] == "review"
    })
    .await;
    assert_eq!(end["end"]["outcome"], "completed", "{end}");
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    let t0 = Instant::now();
    while !(folder.join("end.json").is_file()
        && read_json(&folder.join("run.json"))["complete"] == true)
    {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the record did not complete"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    folder
}

/// Each line of a loop result's record lines with its wall-clock durations zeroed.
fn zero_timing(bytes: Vec<u8>) -> Vec<u8> {
    let mut text = Vec::new();
    for line in String::from_utf8(bytes).unwrap().lines() {
        let mut v: Value = serde_json::from_str(line).unwrap();
        if let Some(t) = v.get_mut("timing").and_then(Value::as_object_mut) {
            for x in t.values_mut() {
                *x = json!(0.0);
            }
        }
        text.extend(serde_json::to_vec(&v).unwrap());
        text.push(b'\n');
    }
    text
}

/// A folder's files normalized (the module's own docs say how).
fn normalized(folder: &Path, golden: bool) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut widths = BTreeMap::new();
    for rel in files(folder) {
        let name = rel.to_string_lossy().replace('\\', "/");
        if let Some(stream) = name
            .strip_prefix("streams/")
            .and_then(|s| s.strip_suffix("/stream.json"))
        {
            let v = read_json(&folder.join(&rel));
            widths.insert(
                stream.to_owned(),
                v["channels"].as_array().map_or(0, Vec::len),
            );
        }
    }
    for rel in files(folder) {
        let name = rel.to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(folder.join(&rel)).unwrap();
        let fixed = match name.as_str() {
            "run.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                set(&mut v, &["run_uid"], json!("golden-mimo-random"));
                set(&mut v, &["armed", "unix"], json!(START_UNIX));
                set(&mut v, &["armed", "utc_offset_s"], json!(UTC_OFFSET_S));
                set(&mut v, &["start", "unix"], json!(START_UNIX));
                set(&mut v, &["start", "utc_offset_s"], json!(UTC_OFFSET_S));
                set(
                    &mut v,
                    &["build"],
                    json!({"version": "0.0.0", "commit": "0".repeat(40), "dirty": false}),
                );
                pretty(&v)
            }
            "end.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                set(&mut v, &["run_uid"], json!("golden-mimo-random"));
                set(&mut v, &["at_unix"], json!(START_UNIX + 60.0));
                set(&mut v, &["end", "at_s"], json!(30.0));
                if !golden {
                    if let Some(streams) = v["streams"].as_object_mut() {
                        for s in streams.values_mut() {
                            let n = s["frames"].as_u64().unwrap_or(0);
                            s["frames"] = json!(n.min(CUT_FRAMES));
                        }
                    }
                }
                pretty(&v)
            }
            "log.jsonl" => lines_at(bytes, START_UNIX),
            "results/control_spectra.jsonl" | "results/drive_spectra.jsonl" if !golden => {
                zero_timing(bytes)
            }
            n if n.ends_with("/stream.json") && !golden => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                let frames = v["frames"].as_u64().unwrap();
                v["frames"] = json!(frames.min(CUT_FRAMES));
                v["cut_from_frames"] = json!(frames);
                pretty(&v)
            }
            n if n.ends_with("/telemetry.f32") && !golden => {
                let stream = n
                    .trim_start_matches("streams/")
                    .trim_end_matches("/telemetry.f32");
                let keep = CUT_FRAMES as usize * widths[stream] * 4;
                bytes[..keep.min(bytes.len())].to_vec()
            }
            _ => bytes,
        };
        out.insert(name, fixed);
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn todays_code_writes_the_golden_mimo_random_run_folder() {
    let folder = golden_run().await;
    let got = normalized(&folder, false);
    let total: usize = got.values().map(Vec::len).sum();
    if std::env::var_os("HERTZFORGE_SHOW_GOLDEN_SIZES").is_some() {
        for (name, bytes) in &got {
            println!("{name}: {}", bytes.len());
        }
    }
    assert!(
        total <= MOST_BYTES,
        "the golden folder holds {total} bytes, over {MOST_BYTES}"
    );
    if std::env::var_os("HERTZFORGE_WRITE_GOLDEN_RUNS").is_some() {
        write_golden(&golden_dir(), &got);
        println!("wrote {} ({total} bytes)", golden_dir().display());
        return;
    }
    let want = normalized(&golden_dir(), true);
    compare(&got, &want);
}

/// The run is the same run every time: two runs of the golden setup give the same
/// folder, so the golden folder is a fact about the code, not about one run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_golden_mimo_random_run_repeats_exactly() {
    let a = normalized(&golden_run().await, false);
    let b = normalized(&golden_run().await, false);
    for (name, bytes) in &a {
        assert_eq!(Some(bytes), b.get(name), "{name} differs between two runs");
    }
    assert_eq!(a.len(), b.len());
}
