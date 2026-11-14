//! Every setup field the screens send reaches the controller's run and changes it.
//!
//! Each test sends a setup over a real loopback WebSocket to the service with the
//! controller's MIMO driver (the wire adapter, `src/mimo/adapter.rs`) on the
//! simulator's modal plants, and checks what the run did with the field: the
//! settings it recorded with the run (the values it held), and a behaviour the field
//! alone changes (a refusal, an abort, a trip count, a held phase, a tolerance line).
//! Before the adapter mapped these fields, each of them was parsed and then dropped,
//! and every assertion on a recorded value or a behaviour here failed.

mod support;

use hertzforge_hal_sim::plants::StandardPlant;
use hertzforge_law_host::{LawIdentity, Param};
use hertzforge_random_laws::{ChoiceError, MimoLawFactory};
use hertzforge_sdm::law::{
    LawError, MimoControlLaw, MimoInput, MimoLoopInputs, MimoLoopOutputs, MimoSetup, MimoShape,
};
use hertzforge_sdm::{HermitianStack, C64};
use hertzforge_station::StationBuildError;
use sds_control::mimo::adapter::{self, AdapterOptions};
use sds_control::server::{Service, ServiceOptions};
use sds_control::station::FixedScanner;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use support::schema::Schema;
use support::{options, serve, Client};

/// Every run here is unpaced and takes seconds; a wait this long means a failure.
const LONG: Duration = Duration::from_secs(90);
const FS: f64 = 4096.0;
const NPTS: usize = 2048;
const BAND: (f64, f64) = (160.0, 1500.0);

fn no_driver() -> FixedScanner {
    FixedScanner(Err(StationBuildError::NiDriverUnavailable(
        "not installed on this test host".into(),
    )))
}

fn real_options() -> ServiceOptions {
    let mut o = options(
        no_driver(),
        0.0,
        Duration::from_secs(600),
        Duration::from_secs(5),
    );
    o.mimo_driver = None;
    o
}

fn row(index: u32, role: &str, description: &str) -> Value {
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

/// `controls` control rows, then `monitors` monitor rows, then `drives` drive rows,
/// indexed from 1 in that order.
fn table(controls: u32, monitors: u32, drives: u32) -> Value {
    let mut rows = Vec::new();
    let mut k = 1;
    for i in 0..controls {
        rows.push(row(k, "control", &format!("Control {}", i + 1)));
        k += 1;
    }
    for i in 0..monitors {
        rows.push(row(k, "monitor", &format!("Monitor {}", i + 1)));
        k += 1;
    }
    for i in 0..drives {
        rows.push(row(k, "drive", &format!("Shaker {}", i + 1)));
        k += 1;
    }
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

/// A reference on the band's lines for the plant's first `n` responses: `H X H^H`
/// with a partially coherent drive of `vrms` per drive (reachable), plus
/// `unreachable` times the mean diagonal as an independent part on each control
/// (which no drive can produce when controls outnumber drives).
fn reference(plant: StandardPlant, n: usize, vrms: f64, unreachable: f64) -> Value {
    let p = plant.build();
    let df = FS / NPTS as f64;
    let lines = NPTS / 2 + 1;
    let h = p.model.analytic_frf(lines, df, FS);
    let nd = h.cols();
    let (lo, hi) = (
        (BAND.0 / df).round() as usize,
        (BAND.1 / df).round() as usize + 1,
    );
    let bw = (hi - lo) as f64 * df;
    let mut rng = Rng(0xD21E ^ 0x9E37_79B9_7F4A_7C15);
    let a: Vec<C64> = (0..nd * nd)
        .map(|_| C64::new(rng.sym(), rng.sym()))
        .collect();
    let psd = vrms * vrms / bw;
    let x = |i: usize, j: usize| {
        let mut s = C64::new(0.0, 0.0);
        for k in 0..nd {
            s += a[i * nd + k] * a[j * nd + k].conj();
        }
        let mut v = s * (0.4 / nd as f64);
        if i == j {
            v += C64::new(0.6, 0.0);
        }
        v * psd
    };
    let (mut freqs, mut re, mut im) = (Vec::new(), Vec::new(), Vec::new());
    for line in lo..hi {
        freqs.push(line as f64 * df);
        let mut g = vec![C64::new(0.0, 0.0); n * n];
        for r in 0..n {
            for c in r..n {
                let mut acc = C64::new(0.0, 0.0);
                for pp in 0..nd {
                    for q in 0..nd {
                        acc += h.get(line, r, pp) * x(pp, q) * h.get(line, c, q).conj();
                    }
                }
                g[r * n + c] = acc;
            }
        }
        let mean = (0..n).map(|i| g[i * n + i].re).sum::<f64>() / n as f64;
        for r in 0..n {
            for c in r..n {
                let mut v = g[r * n + c];
                if r == c {
                    v += C64::new(unreachable * mean, 0.0);
                }
                re.push(v.re);
                im.push(if r == c { 0.0 } else { v.im });
            }
        }
    }
    assert_eq!(re.len(), freqs.len() * HermitianStack::packed_len(n));
    json!({"kind": "inline", "freqs_hz": freqs, "n": n, "re": re, "im": im})
}

/// A 4 by 4 setup on table rows 1 to 4 (controls) and 5 to 8 (drives).
fn setup(review: bool) -> Value {
    let mut s = json!({
        "title": "Four-Axis Random",
        "channels": {
            "controls": (1..=4).map(|c| json!({"channel": c, "alarm_db": [3, 3], "abort_db": [6, 6]})).collect::<Vec<_>>(),
            "drives": (5..=8).map(|d| json!({"channel": d, "ceiling_vrms": 5, "abort_vrms": null})).collect::<Vec<_>>(),
        },
        "space": {"kind": "physical"},
        "reference": reference(StandardPlant::Modal4x4, 4, 0.5, 0.0),
        "law": {"id": "pinv_closed", "params": {}},
        "estimation": {"fs_hz": FS, "n_pts": NPTS, "frames_per_loop": 16, "window": "hann",
                        "overlap": 0.5, "memory_loops": 4},
        "identification": {"level_db": -20, "frames": 64, "noise_frames": 16,
                           "coherence_floor": 0.5, "condition_limit": 1000, "review": review},
        "control": {"frf": "hold", "level_ramp_s": 0, "shutdown_ramp_s": 2, "max_loops_out": 5},
        "schedule": {"mode": "auto", "levels": [{"db": -6, "dwell_s": 600}, {"db": 0}],
                     "full_level_s": 600},
        "backend": {"kind": "sim"},
    });
    // A debug build of the linear algebra is many times slower than release; the
    // law's time budget is the design's in release and the whole loop in a debug
    // test run, as the service's own convergence tests have it (a 12 by 6 seed on
    // a loaded CI runner overran a quarter of the loop). A test of the budget sets
    // its own after this.
    if cfg!(debug_assertions) {
        s["control"]["law_budget"] = json!(1.0);
    }
    s
}

/// A shaker record in the shaker database's form, rated `force_lbf` random.
fn shaker(name: &str, force_lbf: f64) -> Value {
    json!({
        "name": name, "manufacturer": "", "model": "", "notes": "",
        "force_random": {"value": force_lbf, "basis": "Rms"},
        "force_sine": null, "force_shock": null, "max_velocity_ips": null,
        "max_displacement_in_pp": null, "max_accel_bare_g": null, "armature_mass_lb": null,
        "overturning_moment_limit_lbf_in": null, "custom": []
    })
}

/// An array of a binary frame, as numbers.
fn arr(e: &support::Event, r: &Value) -> Vec<f64> {
    use sds_control::codec::ArrayData;
    let d = e.decoded.as_ref().expect("a binary frame");
    match &d.array(r).expect("the array").data {
        ArrayData::F32(v) => v.iter().map(|x| f64::from(*x)).collect(),
        ArrayData::F64(v) => v.clone(),
        _ => panic!("numbers expected"),
    }
}

async fn controller(c: &mut Client, rows: Value) {
    c.ok("session.claim", json!({"name": "Shell"})).await;
    c.ok("station.select", json!({"kind": "sim"})).await;
    let rev = c.ok("channels.get", json!({})).await["revision"].clone();
    c.ok("channels.set", json!({"revision": rev, "channels": rows}))
        .await;
}

async fn wait_mimo(c: &mut Client, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let i = c
        .wait_for(what, LONG, |e| {
            e.topic() == "mimo_random.status" && pred(e.data())
        })
        .await;
    c.events[i].data().clone()
}

/// The settings a run recorded with itself: the controller's own settings in the run
/// folder's `setup.json` (`resolved.controller`) under the records root, the newest
/// first.
fn recorded_config(root: &Path) -> Value {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().is_some_and(|n| n == "setup.json") {
                    out.push(p);
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let path = found.last().expect("the run recorded its settings");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["resolved"]["controller"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_drive_control_and_estimation_field_reaches_the_run() {
    let schema = Schema::load();
    let o = real_options();
    let root = o.data_root.clone();
    let svc = Service::with_options(o);
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    let mut s = setup(true);
    for (k, d) in s["channels"]["drives"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        d["peak_v"] = json!(7.5);
        d["moving_mass_lb"] = json!(10.0 + k as f64);
        d["shaker"] = shaker(&format!("Bench Shaker {k}"), 1.0e6);
    }
    s["channels"]["article_mass_lb"] = json!(24.0);
    s["control"] = json!({"frf": "update", "frf_blend": 0.3, "level_ramp_s": 0,
        "shutdown_ramp_s": 2, "max_loops_out": 5, "cap_up_db": 2.0, "cap_down_db": 4.5,
        "law_budget": 0.9, "law_misses": 7});
    s["estimation"]["window"] = json!("rectangular");
    s["estimation"]["overlap"] = json!(0.25);
    let set = c.ok("mimo_random.setup", json!({"setup": s})).await;
    assert_eq!(set["validation"]["ok"], true, "{set}");
    c.ok("mimo_random.start", json!({})).await;
    let review = wait_mimo(&mut c, "the review hold", |d| {
        d["phase"] == "awaiting_review"
    })
    .await;
    schema.assert_valid(&review, Some("mimoStatus"));
    let cfg = recorded_config(&root);
    for (k, d) in cfg["drives"].as_array().unwrap().iter().enumerate() {
        assert_eq!(d["peak_v"], 7.5, "{cfg}");
        assert_eq!(d["moving_mass_lb"], 10.0 + k as f64, "{cfg}");
        assert_eq!(d["shaker"], format!("Bench Shaker {k}"), "{cfg}");
    }
    assert_eq!(cfg["article_mass_lb"], 24.0, "{cfg}");
    assert_eq!(cfg["frf"], json!({"mode": "update", "blend": 0.3}), "{cfg}");
    assert_eq!(cfg["caps"], json!({"up_db": 2.0, "down_db": 4.5}), "{cfg}");
    assert_eq!(cfg["law_budget"], 0.9, "{cfg}");
    assert_eq!(cfg["law_misses"], 7, "{cfg}");
    assert_eq!(cfg["estimation"]["window"], "Rectangular", "{cfg}");
    assert_eq!(cfg["estimation"]["overlap"], 0.25, "{cfg}");
    // With a shaker record on every drive, the prediction judges each shaker
    // against the acceleration it predicts there.
    let p = &review["prediction"];
    assert_eq!(p["drive_accel"].as_array().unwrap().len(), 4, "{p}");
    assert!(p["drive_accel"][0].as_f64().unwrap() > 0.0, "{p}");
    for v in p["feasibility"].as_array().unwrap() {
        assert_eq!(v["verdict"], "pass", "{p}");
    }
    c.ok("mimo_random.stop", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_article_and_moving_mass_decide_whether_the_shaker_can_carry_the_test() {
    let svc = Service::with_options(real_options());
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    // A 50 lbf shaker under each drive. With no mass on it, the force it needs is
    // nothing and the run starts; with the article on it, it is exceeded.
    let with = |article: f64, moving: f64| {
        let mut s = setup(true);
        for d in s["channels"]["drives"].as_array_mut().unwrap() {
            d["shaker"] = shaker("Small Shaker", 50.0);
            d["moving_mass_lb"] = json!(moving);
        }
        s["channels"]["article_mass_lb"] = json!(article);
        s
    };
    c.ok("mimo_random.setup", json!({"setup": with(4000.0, 0.0)}))
        .await;
    let e = c.refused("mimo_random.start", json!({})).await;
    assert_eq!(e["reason"], "feasibility_exceeded", "{e}");
    let m = e["message"].as_str().unwrap();
    assert!(m.contains("article mass") && m.contains("channel 5"), "{m}");
    // The shaker's own moving mass alone does it too.
    c.ok("mimo_random.setup", json!({"setup": with(0.0, 1000.0)}))
        .await;
    let e = c.refused("mimo_random.start", json!({})).await;
    assert_eq!(e["reason"], "feasibility_exceeded", "{e}");
    // Without either, the same shaker carries it.
    c.ok("mimo_random.setup", json!({"setup": with(0.0, 0.0)}))
        .await;
    c.ok("mimo_random.start", json!({})).await;
    wait_mimo(&mut c, "the review hold", |d| {
        d["phase"] == "awaiting_review"
    })
    .await;
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limit_channel_aborts_the_run_past_its_ceiling() {
    let o = real_options();
    let root = o.data_root.clone();
    let svc = Service::with_options(o);
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    // Three controls, one monitor (the plant's fourth response), four drives.
    controller(&mut c, table(3, 1, 4)).await;
    let mut s = setup(false);
    s["channels"]["controls"] = json!((1..=3).map(|c| json!({"channel": c})).collect::<Vec<_>>());
    s["reference"] = reference(StandardPlant::Modal4x4, 3, 0.5, 0.0);
    s["channels"]["limits"] = json!([{"channel": 4, "ceiling": 1e-14, "abort_margin_db": 3}]);
    let set = c.ok("mimo_random.setup", json!({"setup": s})).await;
    assert_eq!(set["validation"]["ok"], true, "{set}");
    c.ok("mimo_random.start", json!({})).await;
    let done = wait_mimo(&mut c, "the limit abort", |d| {
        d["state"] == "finished" || d["phase"] == "full_level"
    })
    .await;
    assert_ne!(done["end"]["outcome"], "completed", "{done}");
    let trip = &done["end"]["trip"];
    assert_eq!(trip["kind"], "envelope", "{done}");
    assert_eq!(trip["channel"], 4, "{done}");
    let cfg = recorded_config(&root);
    assert_eq!(cfg["limits"][0]["channel"], 4, "{cfg}");
    assert_eq!(cfg["limits"][0]["abort_margin_db"], 3.0, "{cfg}");
}

/// The default law for the seed, then an error on every iteration.
struct Failing(Box<dyn MimoControlLaw + Send>);

impl MimoControlLaw for Failing {
    fn needs(&self) -> &[MimoInput] {
        self.0.needs()
    }
    fn shapes(&self) -> &[MimoShape] {
        self.0.shapes()
    }
    fn seed(&mut self, i: &MimoLoopInputs<'_>) -> Result<MimoLoopOutputs, LawError> {
        self.0.seed(i)
    }
    fn iterate(&mut self, _: &MimoLoopInputs<'_>) -> Result<MimoLoopOutputs, LawError> {
        Err(LawError("no drive this loop".into()))
    }
}

struct FailingFactory(Arc<dyn MimoLawFactory>);

impl MimoLawFactory for FailingFactory {
    fn identity(&self) -> LawIdentity {
        self.0.identity()
    }
    fn params(&self) -> &[Param] {
        self.0.params()
    }
    fn build(
        &self,
        s: &MimoSetup,
        b: Duration,
    ) -> Result<Box<dyn MimoControlLaw + Send>, ChoiceError> {
        Ok(Box::new(Failing(self.0.build(s, b)?)))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_law_miss_count_on_the_wire_is_the_count_that_stops_the_run() {
    let mut o = real_options();
    let inner = hertzforge_mimo_random::law::builtin_factory(
        &hertzforge_random_laws::default_mimo_choice(),
        4,
    )
    .unwrap();
    let mut a = AdapterOptions::local(&o.data_root, o.scanner.clone());
    a.capture_root = o.capture_root.clone();
    a.law_factory = Some(Arc::new(FailingFactory(inner)));
    o.mimo_driver = Some(adapter::factory(a));
    let svc = Service::with_options(o);
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    let mut s = setup(false);
    s["control"]["law_misses"] = json!(1);
    c.ok("mimo_random.setup", json!({"setup": s})).await;
    c.ok("mimo_random.start", json!({})).await;
    let done = wait_mimo(&mut c, "the law fault", |d| d["state"] == "finished").await;
    assert_eq!(done["end"]["trip"]["kind"], "law_fault", "{done}");
    // The default is 3 in a row; the setup asked for 1.
    assert_eq!(done["end"]["trip"]["consecutive"], 1, "{done}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_law_budget_too_small_for_the_law_stops_the_run_before_it_drives() {
    let svc = Service::with_options(real_options());
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    let mut s = setup(true);
    s["control"]["law_budget"] = json!(1e-7);
    c.ok("mimo_random.setup", json!({"setup": s})).await;
    c.ok("mimo_random.start", json!({})).await;
    let done = wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
    let m = done["end"]["message"].as_str().unwrap_or("");
    assert!(m.contains("seed"), "{done}");
    let seen: Vec<String> = c
        .of_topic("mimo_random.status")
        .iter()
        .map(|e| e.data()["phase"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(!seen.iter().any(|p| p == "engaged"), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn virtual_control_takes_its_tolerance_bands_per_degree_of_freedom() {
    let schema = Schema::load();
    let svc = Service::with_options(real_options());
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    let mut s = setup(true);
    // Four degrees of freedom, each a control channel (the identity), so the
    // reference is the same; each its own bands.
    let ident: Vec<f64> = (0..16)
        .map(|k| if k % 5 == 0 { 1.0 } else { 0.0 })
        .collect();
    s["space"] = json!({"kind": "virtual", "dofs": ["A", "B", "C", "D"],
        "response_transform": {"rows": 4, "cols": 4, "values": ident},
        "dof_bands": [
            {"alarm_db": [1, 1], "abort_db": [2, 2]},
            {"alarm_db": [2, 2], "abort_db": [4, 4]},
            {"alarm_db": [3, 3], "abort_db": [6, 6]},
            {"alarm_db": [4, 4], "abort_db": [8, 8]}]});
    let set = c.ok("mimo_random.setup", json!({"setup": s})).await;
    assert_eq!(set["validation"]["ok"], true, "{set}");
    assert_eq!(
        set["state"]["shape"]["bands"][3]["abort_db"],
        json!([8, 8]),
        "{set}"
    );
    c.ok("mimo_random.start", json!({})).await;
    let review = wait_mimo(&mut c, "the review hold", |d| {
        d["phase"] == "awaiting_review"
    })
    .await;
    schema.assert_valid(&review, Some("mimoStatus"));
    // The tolerance lines the run holds are each degree of freedom's own.
    c.ok("mimo_random.proceed", json!({})).await;
    wait_mimo(&mut c, "a loop", |d| d["loop"].as_u64().unwrap_or(0) >= 1).await;
    c.ok(
        "mimo_random.subscribe",
        json!({"rate_hz": 5, "plots": [
            {"id": "a", "quantity": "psd", "index": 0, "points": 64},
            {"id": "d", "quantity": "psd", "index": 3, "points": 64}]}),
    )
    .await;
    let i = c
        .wait_for("a data frame", LONG, |e| e.topic() == "mimo_random.data")
        .await;
    let frame = &c.events[i];
    let ratio = |id: &str, role: &str| -> f64 {
        let plots = frame.msg["data"]["plots"].as_array().unwrap();
        let p = plots.iter().find(|p| p["id"] == id).unwrap();
        let series = p["views"][0]["series"].as_array().unwrap();
        let get = |name: &str| -> Vec<f64> {
            let s = series.iter().find(|s| s["name"] == name).unwrap();
            arr(frame, &s["y"])
        };
        let r = get("Reference");
        let a = get(role);
        let k = r.iter().position(|v| *v > 0.0).unwrap();
        10.0 * (a[k] / r[k]).log10()
    };
    let got = [
        ratio("a", "Abort high"),
        ratio("d", "Abort high"),
        ratio("d", "Alarm high"),
    ];
    for (g, want) in got.iter().zip([2.0, 8.0, 4.0]) {
        assert!((g - want).abs() < 1e-3, "{got:?}");
    }
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prediction_over_a_ceiling_engages_only_on_its_own_acknowledgement() {
    let schema = Schema::load();
    let svc = Service::with_options(real_options());
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    let mut s = setup(true);
    for d in s["channels"]["drives"].as_array_mut().unwrap() {
        d["ceiling_vrms"] = json!(0.3);
        d["peak_v"] = json!(0.9);
    }
    // Identification at -30 dB stays under the identification limit (a tenth of
    // the ceiling) while the test level asks for more than the ceiling.
    s["identification"]["level_db"] = json!(-30);
    c.ok("mimo_random.setup", json!({"setup": s})).await;
    c.ok("mimo_random.start", json!({})).await;
    let held = wait_mimo(&mut c, "the prediction hold", |d| {
        d["state"] == "review" || d["state"] == "finished"
    })
    .await;
    assert_eq!(held["phase"], "prediction_exceeded", "{held}");
    schema.assert_valid(&held, Some("mimoStatus"));
    assert_eq!(held["state"], "review");
    assert_eq!(held["gates"]["passed"], true, "{held}");
    assert_eq!(held["prediction"]["drives_pass"], false, "{held}");
    let e = c.refused("mimo_random.proceed", json!({})).await;
    assert_eq!(e["reason"], "prediction_exceeded");
    // An acknowledgement of something else does not pass this hold.
    let e = c
        .refused(
            "mimo_random.override",
            json!({"ack": "gates reviewed", "reason": "gate_failed"}),
        )
        .await;
    assert_eq!(e["reason"], "override_not_needed");
    assert!(e["message"]
        .as_str()
        .unwrap()
        .contains("prediction_exceeded"));
    let words = "Drives predicted over their ceilings; the ceilings hold them, accepted.";
    let on = c
        .ok(
            "mimo_random.override",
            json!({"ack": words, "reason": "prediction_exceeded"}),
        )
        .await;
    schema.assert_valid(&on, Some("mimoStatus"));
    assert_eq!(on["override"]["reason"], "prediction_exceeded", "{on}");
    assert_eq!(on["override"]["text"], words);
    assert_eq!(on["overrides"].as_array().unwrap().len(), 1);
    wait_mimo(&mut c, "the loop", |d| d["phase"] == "engaged").await;
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn more_controls_than_drives_shows_the_achievable_reference_before_engaging() {
    let svc = Service::with_options(real_options());
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(12, 0, 6)).await;
    let mut s = setup(true);
    s["channels"]["controls"] = json!((1..=12).map(|c| json!({"channel": c})).collect::<Vec<_>>());
    s["channels"]["drives"] = json!((13..=18)
        .map(|d| json!({"channel": d, "ceiling_vrms": 5}))
        .collect::<Vec<_>>());
    // Part of each control is independent of every other: six drives cannot make it.
    s["reference"] = reference(StandardPlant::Modal12x6, 12, 0.5, 0.5);
    s["identification"]["frames"] = json!(96);
    let set = c.ok("mimo_random.setup", json!({"setup": s})).await;
    assert_eq!(set["validation"]["ok"], true, "{set}");
    c.ok("mimo_random.start", json!({})).await;
    let review = wait_mimo(&mut c, "the review hold", |d| d["state"] == "review").await;
    let p = &review["prediction"];
    assert_eq!(p["has_achievable"], true, "{p}");
    let un: Vec<f64> = p["unreachable_db"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert!(un.iter().all(|d| *d < -0.1), "{un:?}");
    c.ok(
        "mimo_random.subscribe",
        json!({"rate_hz": 5, "plots": [
            {"id": "ach", "quantity": "pred_sdm", "block": [0, 1, 2], "points": 64}]}),
    )
    .await;
    let i = c
        .wait_for("the achievable block", LONG, |e| {
            e.topic() == "mimo_random.data"
        })
        .await;
    let frame = &c.events[i];
    let plot = &frame.msg["data"]["plots"][0];
    let v = &plot["views"][0];
    assert_eq!(v["view"], "sdm", "{plot}");
    let m = arr(frame, &v["measured"]["re"]);
    let r = arr(frame, &v["reference"]["re"]);
    assert_eq!(m.len(), r.len());
    // Each control's achievable PSD is below its reference (packed element 0 of the
    // first line is control 0's PSD).
    assert!(m[0] > 0.0 && m[0] < r[0], "{} {}", m[0], r[0]);
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
}

/// Each control's multiple coherence over the band, read at the review hold.
async fn coherences(c: &mut Client, n: usize) -> Vec<f64> {
    c.ok(
        "mimo_random.subscribe",
        json!({"rate_hz": 10, "plots": [{"id": "m", "quantity": "mcoh", "diagonal": true, "points": 64}]}),
    )
    .await;
    let curves = |e: &support::Event| -> Vec<Value> {
        e.data()["plots"][0]["views"]
            .as_array()
            .map(|vs| {
                vs.iter()
                    .flat_map(|v| v["series"].as_array().cloned().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    };
    let i = c
        .wait_for("the coherences", LONG, |e| {
            e.topic() == "mimo_random.data" && curves(e).len() == n
        })
        .await;
    let frame = &c.events[i];
    curves(frame)
        .iter()
        .map(|s| {
            let y = arr(frame, &s["y"]);
            y.iter().sum::<f64>() / y.len() as f64
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_excluded_at_a_failed_gate_is_kept_as_a_monitor_and_the_run_goes_on() {
    let schema = Schema::load();
    let svc = Service::with_options(real_options());
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    // First run: each control's coherence, to set a floor only the weakest misses.
    c.ok("mimo_random.setup", json!({"setup": setup(true)}))
        .await;
    c.ok("mimo_random.start", json!({})).await;
    let first = wait_mimo(&mut c, "the review hold", |d| {
        d["phase"] == "awaiting_review"
    })
    .await;
    // The gate's own coherence per control (the display's curve agrees with it).
    let coh: Vec<f64> = first["gates"]["coherence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(coherences(&mut c, 4).await.len(), 4);
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
    let mut sorted = coh.clone();
    sorted.sort_by(f64::total_cmp);
    let weakest = coh.iter().position(|v| *v == sorted[0]).unwrap();
    assert!(sorted[1] > sorted[0], "{coh:?}");
    let floor = 0.5 * (sorted[0] + sorted[1]);
    // Second run: the floor fails the weakest control only.
    c.events.clear();
    let mut s = setup(true);
    s["identification"]["coherence_floor"] = json!(floor);
    c.ok("mimo_random.setup", json!({"setup": s})).await;
    c.ok("mimo_random.start", json!({})).await;
    let held = wait_mimo(&mut c, "the failed gate", |d| {
        d["state"] == "review" || d["state"] == "finished"
    })
    .await;
    assert_eq!(held["phase"], "gate_failed", "{held}");
    // Not a position the setup has; no reason; a proceed is still refused.
    let e = c
        .refused(
            "mimo_random.exclude",
            json!({"controls": [9], "ack": "why"}),
        )
        .await;
    assert_eq!(e["reason"], "bad_params");
    let e = c
        .refused(
            "mimo_random.exclude",
            json!({"controls": [weakest], "ack": " "}),
        )
        .await;
    assert_eq!(e["reason"], "bad_params");
    let words = "Weakest control sits on a local mode; kept as a monitor.";
    let s1 = c
        .ok(
            "mimo_random.exclude",
            json!({"controls": [weakest], "ack": words}),
        )
        .await;
    schema.assert_valid(&s1, Some("mimoStatus"));
    let review = wait_mimo(&mut c, "the review hold", |d| {
        d["phase"] == "awaiting_review" || d["state"] == "finished"
    })
    .await;
    schema.assert_valid(&review, Some("mimoStatus"));
    assert_eq!(review["phase"], "awaiting_review", "{review}");
    assert_eq!(review["excluded"], json!([weakest]), "{review}");
    assert_eq!(review["gates"]["passed"], true, "{review}");
    let err = review["prediction"]["error_db"].as_array().unwrap();
    assert_eq!(err.len(), 4, "positions keep counting the setup's controls");
    assert!(err[weakest].is_null(), "{review}");
    assert!(err
        .iter()
        .enumerate()
        .all(|(i, v)| i == weakest || v.is_number()));
    c.ok("mimo_random.proceed", json!({})).await;
    let on = wait_mimo(&mut c, "a loop", |d| d["loop"].as_u64().unwrap_or(0) >= 1).await;
    assert!(on["control_error_db"][weakest].is_null(), "{on}");
    // The heatmap keeps a row per setup control; the excluded one is empty.
    c.ok(
        "mimo_random.subscribe",
        json!({"rate_hz": 10, "plots": [{"id": "h", "quantity": "error_db", "rows": "all", "points": 64}]}),
    )
    .await;
    let i = c
        .wait_for("the heatmap", LONG, |e| {
            e.topic() == "mimo_random.data"
                && e.data()["plots"][0]["id"] == "h"
                && e.data()["plots"][0]["views"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
        })
        .await;
    let v = &c.events[i].msg["data"]["plots"][0]["views"][0];
    assert_eq!(v["view"], "heatmap", "{v}");
    assert_eq!(
        c.events[i].msg["data"]["plots"][0]["labels"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_loop_soft_starts_below_the_first_rung_and_climbs_to_it() {
    let svc = Service::with_options(real_options());
    let addr = serve(svc).await;
    let mut c = Client::connect(addr).await;
    controller(&mut c, table(4, 0, 4)).await;
    let first_engaged = |c: &Client| -> Vec<f64> {
        c.of_topic("mimo_random.status")
            .iter()
            .filter(|e| e.data()["phase"] == "engaged")
            .filter_map(|e| e.data()["level_db"].as_f64())
            .collect()
    };
    // With the default soft start (9 dB) the loop takes hold at -15 dB and climbs
    // to the first rung at -6 dB.
    let mut s = setup(false);
    s["control"]["level_ramp_s"] = json!(20);
    c.ok("mimo_random.setup", json!({"setup": s})).await;
    c.ok("mimo_random.start", json!({})).await;
    wait_mimo(&mut c, "the first rung", |d| {
        d["phase"] == "engaged" && d["level_db"].as_f64().is_some_and(|l| l > -6.01)
    })
    .await;
    let levels = first_engaged(&c);
    assert!(levels[0] < -11.0, "engaged at {levels:?}");
    assert!(
        levels.windows(2).all(|w| w[1] >= w[0] - 1e-9),
        "a climb: {levels:?}"
    );
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
    // With the soft start off it engages at the first rung.
    let mut s = setup(false);
    s["control"]["soft_start_db"] = json!(0);
    c.ok("mimo_random.setup", json!({"setup": s})).await;
    c.events.clear();
    let before = 0;
    c.ok("mimo_random.start", json!({})).await;
    wait_mimo(&mut c, "engaged", |d| {
        d["phase"] == "engaged" && d["level_db"].is_number()
    })
    .await;
    let after: Vec<f64> = c.of_topic("mimo_random.status")[before..]
        .iter()
        .filter(|e| e.data()["phase"] == "engaged")
        .filter_map(|e| e.data()["level_db"].as_f64())
        .collect();
    assert!((after[0] + 6.0).abs() < 0.01, "{after:?}");
    c.ok("mimo_random.abort", json!({})).await;
    wait_mimo(&mut c, "the end", |d| d["state"] == "finished").await;
}
