//! The golden Mixed Mode run folder, `schema/golden/runs/mixed/`, against today's
//! code: a short run on the simulator, unpaced, with the simulator's and the
//! identification's fixed seeds, gives the golden folder once it is normalized. The
//! analysis side converts the same folder in `tests/test_run_folder_golden_mixed.py`.
//!
//! Normalized means: the wall times, the run's unique identity and the build fields
//! fixed; each law call's measured duration (`call_us`, a wall-clock figure) set to
//! zero; and each stream cut to its first [`CUT_FRAMES`] frames, its `stream.json`
//! and the end's counts saying so (`cut_from_frames`, the frames the run recorded).
//! The cut is the golden folder's alone: the run's streams are some forty thousand
//! frames, which no folder of 64 KB holds.
//!
//! `scripts/make_golden_runs.sh` rewrites the folder (this test with
//! `HERTZFORGE_WRITE_GOLDEN_RUNS=1`); otherwise the test only compares.
//!
//! The run is SIMULATED; nothing here is evidence about hardware.

mod support;

use hertzforge_station::StationBuildError;
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
const CUT_FRAMES: u64 = 256;

/// The wall time every normalized folder starts at.
const START_UNIX: f64 = 1_794_240_000.25;
const UTC_OFFSET_S: i64 = -25_200;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/golden/runs/mixed")
}

fn row(index: u32, role: &str, description: &str) -> Value {
    let drive = role == "drive";
    json!({
        "index": index, "enabled": true, "description": description, "role": role,
        "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": if drive { "V" } else { "g" },
        "sensitivity_mv_per_eu": if drive { 1000 } else { 100 },
        "range_max": if drive { 10 } else { 100 }, "polarity": "positive",
        "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

/// A short run: a control input, a limited monitor and the drive at 4096 Hz on a
/// coarse grid (16 Hz lines, so the folder stays small), a flat floor across 100 to
/// 500 Hz, a fixed tone and a swept narrowband, at -6 dB and then 0 dB.
fn golden_config() -> Value {
    json!({
        "title": "Golden Mixed Mode",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "the golden run folder",
        "specification": {"document": "Example procedure", "clause": "3.4"},
        "parts": [
            {"id": "floor", "kind": "floor",
             "breakpoints": [{"freq_hz": 100, "psd_g2_hz": 0.0005},
                             {"freq_hz": 500, "slope_db_oct": 0}],
             "alarm_db": [3, 3], "abort_db": [6, 6]},
            {"id": "t1", "name": "Blade", "kind": "tone",
             "schedule": {"kind": "fixed", "freq_hz": 200}, "amplitude_g": 0.5},
            {"id": "b1", "name": "Mesh", "kind": "narrowband",
             "schedule": {"kind": "sweep", "band_hz": [350, 420],
                          "rate": {"law": "linear", "hz_per_s": 20}},
             "level_g2_hz": 0.002, "width_hz": 32}
        ],
        "exclusion_hz": 32,
        "acquisition": {"fs_hz": 4096, "n_pts": 256, "n_avg": 8, "tick_ms": 100},
        "schedule": {"mode": "auto", "levels": [{"db": -6, "dwell_s": 1}, {"db": 0}],
                     "full_level_s": 1},
        "control": {"pretest": {"averages": 2}, "floor_settle_s": 1, "part_ramp_s": 0.5,
                    "level_ramp_s": 0.5, "shutdown_ramp_s": 0.5, "review_after_sysid": false},
        "limits": {"drive_vpk_ceiling": 5, "drive_vpk_abort": 8,
                   "channels": [{"channel": 2, "kind": "response",
                                 "rows": [{"freq_hz": 100, "ceiling": 1},
                                          {"freq_hz": 500, "ceiling": 1}]}]}
    })
}

/// Wait for a `mixed.state` event in `state`.
async fn mixed_state(c: &mut Client, state: &str) -> Value {
    let i = c
        .wait_for(
            &format!("mixed.state {state}"),
            Duration::from_secs(300),
            |e| e.topic() == "mixed.state" && e.data()["state"] == state,
        )
        .await;
    c.events[i].data().clone()
}

/// Run the golden run on a fresh service and return its folder once complete.
async fn golden_run() -> PathBuf {
    let scan = FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )));
    let mut o = options(scan, 0.0, Duration::from_secs(60), Duration::from_secs(5));
    // Unpaced, the simulator outruns any disk: buffers for the whole run, so no
    // frame is dropped and the run is the same every time.
    o.record.stream_buffer_s = 600.0;
    let addr = serve(Service::with_options(o)).await;
    let mut c = Client::connect(addr).await;
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rows = vec![
        row(1, "control", "Control"),
        row(2, "monitor", "Monitor"),
        row(3, "drive", "Drive"),
    ];
    c.ok("channels.set", json!({"revision": 1, "channels": rows}))
        .await;
    c.ok("mixed.configure", json!({"config": golden_config()}))
        .await;
    c.ok("mixed.start", json!({})).await;
    let end = mixed_state(&mut c, "finished").await;
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

/// `call_us` of every record of `control_trace` set to zero.
fn zero_call_times(bin: Vec<u8>, side: &Value) -> Vec<u8> {
    let arrays = side["arrays"].as_array().unwrap();
    let size = |a: &Value| -> usize {
        a["shape"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_u64().unwrap() as usize)
            .product::<usize>()
            * 8
    };
    let record: usize = arrays.iter().map(size).sum();
    let call = arrays.iter().find(|a| a["name"] == "call_us").unwrap();
    let (at, len) = (call["offset"].as_u64().unwrap() as usize, size(call));
    let mut out = bin;
    for r in (0..out.len()).step_by(record.max(1)) {
        out[r + at..r + at + len].fill(0);
    }
    out
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
                set(&mut v, &["run_uid"], json!("golden-mixed"));
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
                set(&mut v, &["run_uid"], json!("golden-mixed"));
                set(&mut v, &["at_unix"], json!(START_UNIX + 60.0));
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
            "results/control_trace.bin" if !golden => {
                let side = read_json(&folder.join("results/control_trace.json"));
                zero_call_times(bytes, &side)
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
async fn todays_code_writes_the_golden_mixed_run_folder() {
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
async fn the_golden_mixed_run_repeats_exactly() {
    let a = normalized(&golden_run().await, false);
    let b = normalized(&golden_run().await, false);
    for (name, bytes) in &a {
        assert_eq!(Some(bytes), b.get(name), "{name} differs between two runs");
    }
    assert_eq!(a.len(), b.len());
}
