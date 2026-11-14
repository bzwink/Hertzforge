//! The golden SRS shock run folder, `schema/golden/runs/srs/`, against today's
//! code: a short automatic run on the simulator, unpaced, with the simulator's
//! fixed seed, gives the golden folder once its wall times and build fields are
//! normalized. The analysis side converts the same folder in
//! `tests/test_srs_run_folder_golden.py`.
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

/// The wall time every normalized folder starts at.
const START_UNIX: f64 = 1_794_240_000.25;
const UTC_OFFSET_S: i64 = -25_200;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/golden/runs/srs")
}

fn row(index: u32, role: &str, description: &str) -> Value {
    let drive = role == "drive";
    json!({
        "index": index, "enabled": true, "description": description, "role": role,
        "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": if drive { "V" } else { "g" },
        "sensitivity_mv_per_eu": 1000,
        "range_max": 10, "polarity": "positive",
        "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

/// A short run: a control input, a monitor and the drive at 2048 Hz, a flat 2 g
/// target from 200 to 800 Hz at 1/3 octave, two levels with a pulse each.
pub fn golden_config() -> Value {
    json!({
        "title": "Golden SRS",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "the golden run folder",
        "specification": {"document": "Example procedure", "clause": "4.2"},
        "target": {"kind": "flat", "accel_g": 2},
        "bands": {"f_lo_hz": 200, "f_hi_hz": 800, "bands_per_octave": 3, "q": 10},
        "acquisition": {"fs_hz": 2048, "ring_out_s": 0.02},
        "schedule": {"mode": "auto", "levels_db": [-6, 0], "pulses": [1, 1],
                     "advance": "count"},
        "spacing": {"cooling_s": 0.05, "settling_s": 0, "compute_s": 0},
        "identification": {"nfft": 256, "review": false},
        "limits": {"drive_vpk_ceiling": 5, "drive_vpk_abort": 8}
    })
}

/// Run the golden run on a fresh service and return its folder once complete.
async fn golden_run() -> PathBuf {
    let scan = FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )));
    let mut o = options(scan, 0.0, Duration::from_secs(60), Duration::from_secs(5));
    o.srs_engine = None;
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
    c.ok("srs.configure", json!({"config": golden_config()}))
        .await;
    c.ok("srs.start", json!({})).await;
    let i = c
        .wait_for("the run's end", Duration::from_secs(120), |e| {
            e.topic() == "srs.state" && e.data()["state"] == "finished"
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
/// times the run measured from the wall clock (a law call's time included).
fn normalized(folder: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for rel in files(folder) {
        let name = rel.to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(folder.join(&rel)).unwrap();
        let fixed = match name.as_str() {
            "run.json" => {
                let mut v: Value = serde_json::from_slice(&bytes).unwrap();
                set(&mut v, &["run_uid"], json!("golden-srs"));
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
                set(&mut v, &["run_uid"], json!("golden-srs"));
                set(&mut v, &["at_unix"], json!(START_UNIX + 60.0));
                set(&mut v, &["end", "at_s"], json!(30.0));
                set(&mut v, &["summary", "elapsed_s"], json!(30.0));
                pretty(&v)
            }
            "log.jsonl"
            | "results/pulses.jsonl"
            | "results/pulse_srs.jsonl"
            | "results/drive_tables.jsonl" => {
                let mut text = Vec::new();
                for (k, line) in String::from_utf8(bytes).unwrap().lines().enumerate() {
                    let mut v: Value = serde_json::from_str(line).unwrap();
                    set(&mut v, &["at_unix"], json!(START_UNIX + k as f64));
                    set(&mut v, &["at_s"], json!(k as f64));
                    set(&mut v, &["data", "end", "at_s"], json!(30.0));
                    // a law call's time is the wall clock's
                    set(&mut v, &["law", "call_ms"], json!(0.0));
                    text.extend(serde_json::to_vec(&v).unwrap());
                    text.push(b'\n');
                }
                text
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
async fn todays_code_writes_the_golden_srs_run_folder() {
    let folder = golden_run().await;
    let got = normalized(&folder);
    let total: usize = got.values().map(Vec::len).sum();
    assert!(
        total <= MOST_BYTES,
        "the golden folder holds {total} bytes, over {MOST_BYTES}: {:?}",
        got.iter().map(|(k, v)| (k, v.len())).collect::<Vec<_>>()
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
async fn the_golden_srs_run_repeats_exactly() {
    let a = normalized(&golden_run().await);
    let b = normalized(&golden_run().await);
    for (name, bytes) in &a {
        assert_eq!(Some(bytes), b.get(name), "{name} differs between two runs");
    }
    assert_eq!(a.len(), b.len());
}
