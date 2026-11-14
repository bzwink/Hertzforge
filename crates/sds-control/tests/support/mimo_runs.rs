//! A small MIMO Random run on the simulator's 4 by 4 modal plant, for the run
//! record's tests: the channel table, a reference the plant can reach on a short
//! band, the setup, and the waits. Included by the tests that use it
//! (`#[path = "support/mimo_runs.rs"]`).
#![allow(dead_code)]

use super::support::Client;
use hertzforge_hal_sim::plants::StandardPlant;
use hertzforge_sdm::{HermitianStack, C64};
use serde_json::{json, Value};
use std::time::Duration;

/// The grid: 4096 Hz (the plant's modes reach above 1 kHz), 2048 points, 2 Hz lines.
pub const FS: f64 = 4096.0;
pub const NPTS: usize = 2048;
/// The band: four lines, 160 to 166 Hz.
pub const BAND: (f64, f64) = (160.0, 166.0);
pub const LONG: Duration = Duration::from_secs(600);

pub fn row(index: u32, role: &str, description: &str) -> Value {
    let units = if role == "drive" { "V" } else { "g" };
    json!({
        "index": index, "enabled": true, "description": description,
        "role": role, "physical": null, "interface": "single_ended", "iepe_bias_v": null,
        "units": units, "sensitivity_mv_per_eu": if units == "V" { 1000 } else { 100 },
        "range_max": if units == "V" { 10 } else { 5000 }, "polarity": "positive",
        "control_group": null,
        "abort": {"accel_g": null, "velocity_ips": null, "displacement_mil": null}
    })
}

/// Controls 1 to 4, drives 5 to 8: the size of the simulator's 4 by 4 plant.
pub fn four_by_four() -> Value {
    let mut rows: Vec<Value> = (1..=4)
        .map(|i| row(i, "control", &format!("Control {i}")))
        .collect();
    rows.extend((5..=8).map(|k| row(k, "drive", &format!("Shaker {}", k - 4))));
    Value::from(rows)
}

struct Rng(u64);

impl Rng {
    fn sym(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        2.0 * ((self.0 >> 11) as f64 / (1u64 << 53) as f64) - 1.0
    }
}

/// A reference the plant can reach: `H X H^H` with the plant's exact FRF and a
/// partially coherent drive of `vrms` per drive, on the band's grid lines, given as
/// eight points across the band (each between two lines a mix of the two, so still
/// a spectral matrix), as the setup takes an inline reference.
pub fn reachable_reference(vrms: f64) -> Value {
    let plant = StandardPlant::Modal4x4.build();
    let df = FS / NPTS as f64;
    let lines = NPTS / 2 + 1;
    let h = plant.model.analytic_frf(lines, df, FS);
    let (lo, hi) = (
        (BAND.0 / df).round() as usize,
        (BAND.1 / df).round() as usize + 1,
    );
    let bw = (hi - lo) as f64 * df;
    let n = 4;
    let mut rng = Rng(0xD21E ^ 0x9E37_79B9_7F4A_7C15);
    let a: Vec<C64> = (0..n * n).map(|_| C64::new(rng.sym(), rng.sym())).collect();
    let psd = vrms * vrms / bw;
    let x = |i: usize, j: usize| {
        let mut s = C64::new(0.0, 0.0);
        for k in 0..n {
            s += a[i * n + k] * a[j * n + k].conj();
        }
        let mut v = s * (0.4 / n as f64);
        if i == j {
            v += C64::new(0.6, 0.0);
        }
        v * psd
    };
    let on_line = |line: usize| -> Vec<C64> {
        let mut out = Vec::new();
        for r in 0..n {
            for c in r..n {
                let mut acc = C64::new(0.0, 0.0);
                for p in 0..n {
                    for q in 0..n {
                        acc += h.get(line, r, p) * x(p, q) * h.get(line, c, q).conj();
                    }
                }
                out.push(if r == c { C64::new(acc.re, 0.0) } else { acc });
            }
        }
        out
    };
    let grid: Vec<Vec<C64>> = (lo..hi).map(on_line).collect();
    let (mut freqs, mut re, mut im) = (Vec::new(), Vec::new(), Vec::new());
    let points = 8;
    for j in 0..points {
        let f = BAND.0 + (BAND.1 - BAND.0) * j as f64 / (points - 1) as f64;
        let at = (f / df - lo as f64).clamp(0.0, (hi - lo - 1) as f64);
        let k = (at.floor() as usize).min(hi - lo - 1);
        let w = at - k as f64;
        let next = (k + 1).min(hi - lo - 1);
        freqs.push(f);
        for (u, v) in grid[k].iter().zip(&grid[next]) {
            let m = *u * (1.0 - w) + *v * w;
            re.push(m.re);
            im.push(m.im);
        }
    }
    assert_eq!(re.len(), freqs.len() * HermitianStack::packed_len(n));
    json!({"kind": "inline", "freqs_hz": freqs, "n": n, "re": re, "im": im})
}

/// A short run: identification, then straight to full level for `full_level_s`.
pub fn setup(full_level_s: f64) -> Value {
    let mut s = json!({
        "title": "Golden MIMO Random",
        "operator": "Operator",
        "article": "Bracket",
        "notes": "the golden run folder",
        "specification": {"document": "Example procedure", "clause": "4.1"},
        "channels": {
            "controls": (1..=4).map(|c| json!({"channel": c, "alarm_db": [3, 3], "abort_db": [6, 6]})).collect::<Vec<_>>(),
            "drives": (5..=8).map(|d| json!({"channel": d, "ceiling_vrms": 5, "abort_vrms": null})).collect::<Vec<_>>(),
        },
        "space": {"kind": "physical"},
        "reference": reachable_reference(0.5),
        "law": {"id": "pinv_closed", "params": {}},
        "estimation": {"fs_hz": FS, "n_pts": NPTS, "frames_per_loop": 4, "window": "hann",
                        "overlap": 0.5, "memory_loops": 2},
        "identification": {"level_db": -20, "frames": 64, "noise_frames": 16,
                           "coherence_floor": 0.5, "condition_limit": 1000, "review": false},
        "control": {"frf": "hold", "level_ramp_s": 0, "shutdown_ramp_s": 0.5,
                    "max_loops_out": 5, "soft_start_db": 0},
        "schedule": {"mode": "auto", "levels": [{"db": 0}], "full_level_s": full_level_s},
        "backend": {"kind": "sim"},
    });
    // The law's budget is the whole loop in a debug build, as the service's own
    // tests have it.
    if cfg!(debug_assertions) {
        s["control"]["law_budget"] = json!(1.0);
    }
    s
}

pub async fn controller(c: &mut Client) {
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok(
        "channels.set",
        json!({"revision": rev, "channels": four_by_four()}),
    )
    .await;
}

pub async fn wait_mimo(c: &mut Client, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let i = c
        .wait_for(what, LONG, |e| {
            e.topic() == "mimo_random.status" && pred(e.data())
        })
        .await;
    c.events[i].data().clone()
}
