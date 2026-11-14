//! The golden Explorer run folder, `schema/golden/runs/explorer/`, against today's
//! code: a short Explorer run on the simulator, with the simulator's fixed seed and its
//! stated source, gives the golden folder once what depends on the wall clock is
//! normalized. The tone stands at 200 Hz at 0 V, is raised to 0.1 V pk, holds until the
//! survey has its point, and the run is stopped. The analysis side converts the same
//! folder in `tests/test_run_folder_golden_explorer.py`.
//!
//! Explorer is steered by hand, so when each verb lands follows the wall clock. What
//! depends on it is normalized: the wall times, the run's unique identity, the build,
//! the folder's path, the station frames (of the log's entries, of the intents, of the
//! end and of the streams' lengths), the run's health, what the run measured at the end
//! (each channel's rms, the measured frequency, the derived motion), and the values of
//! the results (the survey's level and the spectra), whose shapes, channels and units
//! are kept. Each stream is cut to its first [`CUT_FRAMES`] frames, before the first
//! steer can land, so the samples kept are the same every run; `cut_from_frames` says
//! so. The folder holds no gap: a gap needs a disk that falls behind, which a test
//! cannot make the same every time; gaps are tested on the record's writer and on the
//! reader.
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
use support::{options, serve, Client};

/// The most the golden folder may hold, bytes.
const MOST_BYTES: usize = 64 * 1024;
/// The frames of each stream the golden folder keeps: an eighth of a second at the
/// golden rate, before the first steer (which is sent half a second after the start).
const CUT_FRAMES: u64 = 256;
/// The rate the golden run asks for.
const FS: f64 = 2048.0;

/// The wall time every normalized folder starts at.
const START_UNIX: f64 = 1_794_240_000.25;
const UTC_OFFSET_S: i64 = -25_200;
/// The station frames, as normalized.
const STATION_FRAMES: u64 = 8192;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/golden/runs/explorer")
}

fn row(index: u32, role: &str, description: &str) -> Value {
    let drive = role == "drive";
    json!({
        "index": index, "enabled": true, "description": description, "role": role,
        "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": if drive { "V" } else { "g" },
        "sensitivity_mv_per_eu": if drive { 1000 } else { 100 },
        "range_max": if drive { 10 } else { 50 },
        "polarity": "positive", "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

fn golden_config() -> Value {
    json!({
        "title": "Golden Explorer",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "the golden run folder",
        "specification": {"document": "Example procedure", "clause": "4.2"},
        "acquisition": {"fs_hz": FS},
        "band_hz": [20, 800], "start_hz": 200, "max_drive_vpk": 0.5,
        "slew": {"freq_oct_per_s": 1, "drive_db_per_s": 200},
        "display": {"nfft": 1024},
        "capture": {"run_name": "golden_explorer"},
        "metadata": [{"key": "run_number", "label": "Run Number", "value": "7"}]
    })
}

/// Run the golden run on a fresh service and return its folder once complete.
async fn golden_run() -> PathBuf {
    let scan = FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )));
    let o = options(scan, 1.0, Duration::from_secs(60), Duration::from_secs(5));
    let addr = serve(Service::with_options(o)).await;
    let mut c = Client::connect(addr).await;
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rows = vec![
        row(1, "control", "Control"),
        row(2, "drive", "Drive"),
        row(3, "monitor", "Fixture"),
    ];
    c.ok("channels.set", json!({"revision": 1, "channels": rows}))
        .await;
    c.ok("explorer.configure", json!({"config": golden_config()}))
        .await;
    c.ok("explorer.start", json!({})).await;
    // The first frames, which the golden folder keeps, are at 0 V.
    c.pump(Duration::from_millis(500)).await;
    let s = c.ok("explorer.steer", json!({"vpk": 0.1})).await;
    assert!(s["frame"].as_u64().unwrap() > CUT_FRAMES, "{s}");
    let t0 = Instant::now();
    loop {
        let sv = c.ok("explorer.survey", json!({})).await;
        let st = c.ok("explorer.state", json!({})).await;
        if sv["points"].as_array().is_some_and(|p| !p.is_empty()) && st["phase"] == "steady" {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "{sv} {st}");
        c.pump(Duration::from_millis(20)).await;
    }
    c.ok("explorer.stop", json!({})).await;
    let t0 = Instant::now();
    let end = loop {
        let s = c.ok("explorer.state", json!({})).await;
        if s["state"] == "finished" {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "{s}");
        c.pump(Duration::from_millis(20)).await;
    };
    assert_eq!(end["end"]["outcome"], "operator_stop", "{end}");
    let folder = PathBuf::from(end["record"]["folder"].as_str().unwrap());
    // The run does not wait for its record: the end follows.
    let t0 = Instant::now();
    while !(folder.join("end.json").is_file()
        && read_json(&folder.join("run.json"))["complete"] == true)
    {
        assert!(t0.elapsed() < Duration::from_secs(30), "{folder:?}");
        c.pump(Duration::from_millis(20)).await;
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

/// The station's time as normalized.
fn at_s() -> Value {
    json!(STATION_FRAMES as f64 / FS)
}

/// The run's state at the end, normalized: its station time, its health and the
/// folder's path.
fn normalize_state(v: &mut Value) {
    set(v, &["t_s"], at_s());
    set(v, &["end", "at_s"], at_s());
    set(v, &["health"], Value::Null);
    set(
        v,
        &["record", "folder"],
        json!("Hertzforge_Data/golden_explorer"),
    );
}

/// A folder's files with what depends on the wall clock, the host and the build
/// replaced by fixed values, and each stream cut to its first frames. `golden` is true
/// for the golden folder itself, whose streams are cut already.
fn normalized(folder: &Path, golden: bool) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut widths = BTreeMap::new();
    for name in ["inputs", "drives"] {
        let side = folder.join("streams").join(name).join("stream.json");
        if side.is_file() {
            let n = read_json(&side)["channels"].as_array().unwrap().len();
            widths.insert(name.to_owned(), n);
        }
    }
    for rel in files(folder) {
        let name = rel.to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(folder.join(&rel)).unwrap();
        let fixed = match name.as_str() {
            "run.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                set(&mut v, &["run_uid"], json!("golden-explorer"));
                for k in ["armed", "start"] {
                    set(&mut v, &[k, "unix"], json!(START_UNIX));
                    set(&mut v, &[k, "utc_offset_s"], json!(UTC_OFFSET_S));
                }
                set(
                    &mut v,
                    &["build"],
                    json!({"version": "0.0.0", "commit": "0".repeat(40), "dirty": false}),
                );
                pretty(&v)
            }
            "end.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                set(&mut v, &["run_uid"], json!("golden-explorer"));
                set(&mut v, &["at_unix"], json!(START_UNIX + 60.0));
                set(&mut v, &["end", "at_s"], at_s());
                set(&mut v, &["station_frames"], json!(STATION_FRAMES));
                for s in ["inputs", "drives"] {
                    set(&mut v, &["streams", s, "frames"], json!(CUT_FRAMES));
                }
                if let Some(m) = v.get_mut("measured") {
                    set(m, &["measured_hz"], Value::Null);
                    set(m, &["control", "velocity_ips"], json!(0.0));
                    set(m, &["control", "displacement_mil"], json!(0.0));
                    set(m, &["control", "settling"], json!(false));
                    set(m, &["peak", "level"], json!(0.0));
                    if let Some(rows) = m["channels"].as_array_mut() {
                        for r in rows {
                            set(r, &["rms"], json!(0.0));
                        }
                    }
                }
                if let Some(st) = v.get_mut("state") {
                    normalize_state(st);
                }
                pretty(&v)
            }
            "log.jsonl" => {
                let mut text = Vec::new();
                for (k, line) in String::from_utf8(bytes).unwrap().lines().enumerate() {
                    let mut v: Value = serde_json::from_str(line).unwrap();
                    set(&mut v, &["at_unix"], json!(START_UNIX + k as f64));
                    if !v["frame"].is_null() {
                        set(&mut v, &["frame"], json!(STATION_FRAMES));
                    }
                    if let Some(d) = v.get_mut("data") {
                        if d.get("verb").is_some() {
                            set(d, &["frame"], json!(STATION_FRAMES));
                            set(d, &["input_frame"], json!(STATION_FRAMES));
                            set(d, &["t_s"], at_s());
                        }
                    }
                    if let Some(t) = v["text"].as_str() {
                        if t.starts_with("records in ") {
                            set(&mut v, &["text"], json!("records in golden_explorer"));
                        }
                    }
                    text.extend(serde_json::to_vec(&v).unwrap());
                    text.push(b'\n');
                }
                text
            }
            n if n.ends_with("/stream.json") => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                v["frames"] = json!(CUT_FRAMES);
                v["cut_from_frames"] = json!(STATION_FRAMES);
                pretty(&v)
            }
            n if n.ends_with("/telemetry.f32") && !golden => {
                let stream = n
                    .trim_start_matches("streams/")
                    .trim_end_matches("/telemetry.f32");
                let keep = CUT_FRAMES as usize * widths[stream] * 4;
                assert!(bytes.len() >= keep, "{n} holds {} bytes", bytes.len());
                bytes[..keep].to_vec()
            }
            "results/survey.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                set(&mut v, &["peak", "level"], json!(0.0));
                set(&mut v, &["revision"], json!(1));
                pretty(&v)
            }
            // The values follow when the stop arrived; the layout does not.
            "results/survey.bin" => {
                let n = bytes.len() / 16;
                let mut b = bytes[..n * 8].to_vec();
                b.extend(std::iter::repeat_n(0u8, n * 8));
                b
            }
            "results/spectra.bin" => vec![0u8; bytes.len()],
            _ => bytes,
        };
        out.insert(name, fixed);
    }
    out
}

/// Numbers within a part in a million (or 1e-9 near zero).
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

fn same_samples(name: &str, got: &[u8], want: &[u8], item: usize) {
    assert_eq!(
        got.len(),
        want.len(),
        "{name}: {} bytes against the golden {}",
        got.len(),
        want.len()
    );
    for (k, (a, b)) in got.chunks(item).zip(want.chunks(item)).enumerate() {
        let (x, y) = if item == 4 {
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
        assert!(
            x == y || (x - y).abs() <= 1e-5 * x.abs().max(y.abs()) + 1e-9,
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
            assert_eq!(x.len(), y.len(), "{name}: lines differ\n{x:#?}\n{y:#?}");
            for (k, (u, v)) in x.iter().zip(&y).enumerate() {
                same_json(
                    &serde_json::from_str(u).unwrap(),
                    &serde_json::from_str(v).unwrap(),
                    &format!("{name}:{}", k + 1),
                );
            }
        } else if name.ends_with(".f32") {
            same_samples(name, bytes, gold, 4);
        } else if name.ends_with(".bin") {
            same_samples(name, bytes, gold, 8);
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
async fn todays_code_writes_the_golden_explorer_run_folder() {
    let folder = golden_run().await;
    // The run itself, before any comparison: a point at the tone's frequency, the
    // drive raised to 0.1 V pk and back to 0 V.
    let survey = read_json(&folder.join("results/survey.json"));
    assert_eq!(survey["peak"]["hz"], 200.0, "{survey}");
    let drive: Vec<f32> = std::fs::read(folder.join("streams/drives/telemetry.f32"))
        .unwrap()
        .chunks(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let top = drive.iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!((top - 0.1).abs() < 1e-4, "the drive's peak is {top}");
    assert!(drive[..CUT_FRAMES as usize].iter().all(|x| *x == 0.0));
    assert_eq!(*drive.last().unwrap(), 0.0);
    let got = normalized(&folder, false);
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
    let want = normalized(&golden_dir(), true);
    compare(&got, &want);
}

/// The run is the same run every time: two runs of the golden setup give the same
/// folder, so the golden folder is a fact about the code, not about one run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_golden_explorer_run_repeats_exactly() {
    let a = normalized(&golden_run().await, false);
    let b = normalized(&golden_run().await, false);
    for (name, bytes) in &a {
        assert_eq!(Some(bytes), b.get(name), "{name} differs between two runs");
    }
    assert_eq!(a.len(), b.len());
}
