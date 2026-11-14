//! The golden TWR run folder, `schema/golden/runs/twr/`, against today's code: a
//! short automatic run on the simulator, unpaced, with the simulator's fixed seed,
//! gives the golden folder once its wall times, build fields and each law call's
//! measured duration are normalized, and each shot of its streams cut to its first
//! [`CUT_FRAMES`] frames, its `stream.json` and the end's counts saying so
//! (`cut_from_frames`, the frames the run recorded, on the stream and on each shot).
//! The cut is the golden folder's alone: every shot a play leaves is its record and
//! a rest pad of the station's, 1024 frames on the simulator, so whole shots would
//! not fit. The analysis side converts the same folder in
//! `tests/test_twr_run_folder_golden.py`.
//!
//! `scripts/make_golden_runs.sh` rewrites the folder (this test with
//! `HERTZFORGE_WRITE_GOLDEN_RUNS=1`); otherwise the test only compares, within the
//! tolerances the Random golden run uses.
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
use support::{options, serve, Client};

/// The most the golden folder may hold, bytes.
const MOST_BYTES: usize = 64 * 1024;

/// The frames each shot keeps in the golden folder.
const CUT_FRAMES: u64 = 128;

/// The wall time every normalized folder starts at.
const START_UNIX: f64 = 1_794_240_000.25;
const UTC_OFFSET_S: i64 = -25_200;

/// The reference's rate, Hz.
pub const FS: f64 = 2048.0;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/golden/runs/twr")
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

/// A short field-like reference: two decaying tones, 160 samples at 2048 Hz.
pub fn reference_text() -> String {
    let mut text = String::new();
    for i in 0..160 {
        let t = i as f64 / FS;
        let tone = |f: f64, tau: f64, a: f64| {
            a * (-t / tau).exp() * (2.0 * std::f64::consts::PI * f * t).sin()
        };
        let v = tone(150.0, 0.03, 0.4) + tone(330.0, 0.02, 0.3);
        text.push_str(&format!("{v}\n"));
    }
    text
}

/// A short run: a control input, a monitor and the drive at 2048 Hz, the reference
/// between short pads (a record of 256 samples), identification for half a second,
/// two levels gated at 5.5 % (the first play at each misses it, so the law is called),
/// one playback play.
pub fn golden_config(sha: &Value) -> Value {
    json!({
        "title": "Golden TWR",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "the golden run folder",
        "specification": {"document": "Example procedure", "clause": "4.2"},
        "reference": {"sha256": sha},
        "pads": {"pre": 32, "post": 64},
        "identification": {"drive_vrms": 0.1, "duration_s": 0.5, "nfft": 256,
                           "band_hz": [20, 800], "review": false},
        "schedule": {"mode": "auto", "levels_db": [-6, 0], "gate_pct": 5.5},
        "run": {"schedule": "once"},
        "limits": {"drive_vpk_ceiling": 5}
    })
}

/// Run the golden run on a fresh service and return its folder once complete.
async fn golden_run() -> PathBuf {
    let scan = FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )));
    let mut o = options(scan, 0.0, Duration::from_secs(60), Duration::from_secs(5));
    o.twr_engine = None;
    o.mimo_driver = None;
    // Unpaced, the simulator outruns any disk: buffers for the whole run, so no
    // frame is dropped and the run is the same every time.
    o.record.stream_buffer_s = 120.0;
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
    let sha = c
        .ok(
            "twr.reference.import",
            json!({"text": reference_text(), "fs_hz": FS, "name": "field"}),
        )
        .await["reference"]["sha256"]
        .clone();
    c.ok("twr.configure", json!({"config": golden_config(&sha)}))
        .await;
    c.ok("twr.start", json!({})).await;
    let i = c
        .wait_for("the run's end", Duration::from_secs(120), |e| {
            e.topic() == "twr.state" && e.data()["state"] == "finished"
        })
        .await;
    let end = c.events[i].data().clone();
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

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if !p.file_name().unwrap().to_string_lossy().starts_with('.') {
                out.push(p.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    out.sort();
    out
}

fn set(v: &mut Value, path: &[&str], to: Value) {
    let mut at = v;
    for k in &path[..path.len() - 1] {
        match at.get_mut(*k) {
            Some(next) => at = next,
            None => return,
        }
    }
    if let Some(m) = at.as_object_mut() {
        if m.contains_key(path[path.len() - 1]) {
            m.insert(path[path.len() - 1].to_owned(), to);
        }
    }
}

fn pretty(v: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec_pretty(v).unwrap();
    out.push(b'\n');
    out
}

/// A folder's files with what depends on the wall clock, the host and the build
/// replaced by fixed values: times, the run's unique identity, the build, the
/// times the run measured from the wall clock.
fn normalized(folder: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for rel in files(folder) {
        let name = rel.to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(folder.join(&rel)).unwrap();
        let fixed = match name.as_str() {
            "run.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                set(&mut v, &["run_uid"], json!("golden-twr"));
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
                set(&mut v, &["run_uid"], json!("golden-twr"));
                set(&mut v, &["at_unix"], json!(START_UNIX + 60.0));
                for s in ["inputs", "drives"] {
                    if let Some(f) = v["streams"][s]["frames"].as_u64() {
                        if v["streams"][s].get("cut_from_frames").is_none() {
                            let side = read_json(&folder.join(format!("streams/{s}/stream.json")));
                            let kept: u64 = side["shots"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|shot| shot["frames"].as_u64().unwrap().min(CUT_FRAMES))
                                .sum();
                            v["streams"][s]["frames"] = json!(kept);
                            v["streams"][s]["cut_from_frames"] = json!(f);
                        }
                    }
                }
                pretty(&v)
            }
            "log.jsonl" | "results/plays.jsonl" | "results/law_calls.jsonl" => {
                let mut text = Vec::new();
                for (k, line) in String::from_utf8(bytes).unwrap().lines().enumerate() {
                    let mut v: Value = serde_json::from_str(line).unwrap();
                    set(&mut v, &["at_unix"], json!(START_UNIX + k as f64));
                    text.extend(serde_json::to_vec(&v).unwrap());
                    text.push(b'\n');
                }
                text
            }
            // each law call's measured duration is the host's, not the code's
            "results/law_calls.bin" => {
                let d = read_json(&folder.join("results/law_calls.json"));
                let mut b = bytes;
                if let Some(a) = d["arrays"]
                    .as_array()
                    .and_then(|a| a.iter().find(|a| a["name"] == "call_ms"))
                {
                    let at = a["offset"].as_u64().unwrap() as usize;
                    let per: usize = d["arrays"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|a| {
                            a["shape"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|n| n.as_u64().unwrap() as usize)
                                .product::<usize>()
                                * 8
                        })
                        .sum();
                    let mut k = at;
                    while k + 8 <= b.len() {
                        b[k..k + 8].copy_from_slice(&1.0f64.to_le_bytes());
                        k += per;
                    }
                }
                b
            }
            "streams/inputs/telemetry.f32" | "streams/drives/telemetry.f32" => {
                let side = read_json(&folder.join(name.replace("telemetry.f32", "stream.json")));
                if side.get("cut_from_frames").is_some() {
                    bytes
                } else {
                    let width = side["channels"].as_array().map_or(1, Vec::len) * 4;
                    let mut out = Vec::new();
                    for shot in side["shots"].as_array().unwrap() {
                        let at = shot["first_frame"].as_u64().unwrap() as usize * width;
                        let n = shot["frames"].as_u64().unwrap().min(CUT_FRAMES) as usize * width;
                        out.extend_from_slice(
                            &bytes[at.min(bytes.len())..(at + n).min(bytes.len())],
                        );
                    }
                    out
                }
            }
            "streams/inputs/stream.json" | "streams/drives/stream.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                if v.get("cut_from_frames").is_none() {
                    let frames = v["frames"].as_u64().unwrap();
                    let mut at = 0;
                    for shot in v["shots"].as_array_mut().unwrap() {
                        let n = shot["frames"].as_u64().unwrap();
                        shot["cut_from_frames"] = json!(n);
                        shot["frames"] = json!(n.min(CUT_FRAMES));
                        shot["first_frame"] = json!(at);
                        at += n.min(CUT_FRAMES);
                    }
                    v["frames"] = json!(at);
                    v["cut_from_frames"] = json!(frames);
                }
                pretty(&v)
            }
            _ => bytes,
        };
        out.insert(name, fixed);
    }
    out
}

/// Numbers within a part in a million (or 1e-9 near zero): the platform's maths
/// library may differ in its last bits.
fn close(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= 1e-6 * a.abs().max(b.abs()) + 1e-9 || (a.is_nan() && b.is_nan())
}

fn same_json(a: &Value, b: &Value, path: &str) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => assert!(
            close(x.as_f64().unwrap(), y.as_f64().unwrap()),
            "{path}: {x} against the golden {y}"
        ),
        (Value::Object(x), Value::Object(y)) => {
            let mut kx: Vec<&String> = x.keys().collect();
            let mut ky: Vec<&String> = y.keys().collect();
            kx.sort();
            ky.sort();
            assert_eq!(kx, ky, "{path}: members differ");
            for (k, v) in x {
                same_json(v, &y[k], &format!("{path}.{k}"));
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            assert_eq!(x.len(), y.len(), "{path}: lengths differ");
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                same_json(u, v, &format!("{path}[{i}]"));
            }
        }
        _ => assert_eq!(a, b, "{path}: differs"),
    }
}

fn same_numbers(name: &str, got: &[u8], want: &[u8], width: usize) {
    assert_eq!(
        got.len(),
        want.len(),
        "{name}: {} bytes against the golden {}",
        got.len(),
        want.len()
    );
    for (k, (a, b)) in got.chunks(width).zip(want.chunks(width)).enumerate() {
        let (x, y) = if width == 4 {
            (
                f64::from(f32::from_le_bytes(a.try_into().unwrap())),
                f64::from(f32::from_le_bytes(b.try_into().unwrap())),
            )
        } else {
            (
                f64::from_le_bytes(a.try_into().unwrap()),
                f64::from_le_bytes(b.try_into().unwrap()),
            )
        };
        let tol = if width == 4 { 1e-5 } else { 1e-6 };
        assert!(
            x == y
                || (x - y).abs() <= tol * x.abs().max(y.abs()) + 1e-9
                || (x.is_nan() && y.is_nan()),
            "{name}: value {k} is {x}, the golden {y}"
        );
    }
}

fn compare(got: &BTreeMap<String, Vec<u8>>, want: &BTreeMap<String, Vec<u8>>) {
    let a: Vec<&String> = got.keys().collect();
    let b: Vec<&String> = want.keys().collect();
    assert_eq!(a, b, "the folder's files differ from the golden folder's");
    for (name, bytes) in got {
        let gold = &want[name];
        if name.ends_with(".json") {
            same_json(
                &serde_json::from_slice(bytes).unwrap(),
                &serde_json::from_slice(gold).unwrap(),
                name,
            );
        } else if name.ends_with(".jsonl") {
            let (x, y) = (
                String::from_utf8(bytes.clone()).unwrap(),
                String::from_utf8(gold.clone()).unwrap(),
            );
            let (x, y): (Vec<&str>, Vec<&str>) = (x.lines().collect(), y.lines().collect());
            assert_eq!(x.len(), y.len(), "{name}: lines differ");
            for (k, (u, v)) in x.iter().zip(&y).enumerate() {
                same_json(
                    &serde_json::from_str(u).unwrap(),
                    &serde_json::from_str(v).unwrap(),
                    &format!("{name}:{}", k + 1),
                );
            }
        } else if name.ends_with(".f32") {
            same_numbers(name, bytes, gold, 4);
        } else if name.ends_with(".bin") {
            same_numbers(name, bytes, gold, 8);
        } else {
            assert_eq!(bytes, gold, "{name} differs");
        }
    }
}

fn write_golden(files: &BTreeMap<String, Vec<u8>>) {
    let dir = golden_dir();
    let _ = std::fs::remove_dir_all(&dir);
    for (name, bytes) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn todays_code_writes_the_golden_twr_run_folder() {
    let folder = golden_run().await;
    let got = normalized(&folder);
    let total: usize = got.values().map(Vec::len).sum();
    assert!(
        total <= MOST_BYTES,
        "the golden folder holds {total} bytes, over {MOST_BYTES}"
    );
    if std::env::var_os("HERTZFORGE_WRITE_GOLDEN_RUNS").is_some() {
        write_golden(&got);
        println!("wrote {} ({total} bytes)", golden_dir().display());
        return;
    }
    let want = normalized(&golden_dir());
    compare(&got, &want);
}

/// The run is the same run every time: two runs of the golden setup give the same
/// folder, so the golden folder is a fact about the code, not about one run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_golden_twr_run_repeats_exactly() {
    let a = normalized(&golden_run().await);
    let b = normalized(&golden_run().await);
    for (name, bytes) in &a {
        assert_eq!(Some(bytes), b.get(name), "{name} differs between two runs");
    }
    assert_eq!(a.len(), b.len());
}
