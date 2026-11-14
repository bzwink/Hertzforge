//! What a MIMO Random run puts in its record (`docs/run-folder.md`, "What a MIMO
//! Random Run Saves"): the manifest, the table and the setup at arm, the two streams
//! (every enabled input as read and every drive as the device accepted it, the noise
//! floor and the identification included), and the results: the reference spectral
//! density matrix and its tolerances; the identification's transfer function matrix,
//! multiple coherence, spectral matrices and conditioning; each loop's control and
//! drive spectra with the error against the reference; the full control and drive
//! spectral matrices at the first and last loop at full level and on the setup's
//! cadence; and every control-law call with the inputs that change from call to call
//! and what the law returned.
//!
//! The writing is the record's ([`crate::record`]); this module only says what is
//! written, in the reader's form (`hertzforge.io.run_folder`). Spectral results are
//! on the band's lines (outside the band nothing is driven or controlled), each
//! complex matrix in the file's convention (`conj(X) Y`: the controller's `E[X X^H]`
//! conjugated), values in the channels' own units as the table names them: the
//! analysis side converts.

use super::setup::{ControlSpace, MimoSetup, Reference};
use crate::protocol::num;
use crate::random::record::{self as rrec, Described};
use crate::record::{tap::TapLink, ArrayValues, Begin, ResultArray, StreamSpec};
use crate::station::source_of;
use crate::table::unit_name;
use hertzforge_channel_table::ChannelTable;
use hertzforge_hal::{Caps, DiscoveredDevice};
use hertzforge_mimo_random::{IdentificationSet, KeptLawCall, KeptLoop, MimoEvent, MimoEventKind};
use hertzforge_module_abi::StationClass;
use hertzforge_sdm::{HermitianStack, MatrixStack};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Snapshots of the full spectral matrices are taken at the first loop at full level,
/// then every this many seconds of loops, and at the last loop at full level.
pub use hertzforge_mimo_random::config::DEFAULT_SNAPSHOT_EVERY_S as SNAPSHOT_EVERY_S;

/// The facts of an armed run its record begins with.
pub struct ArmFacts<'a> {
    pub run_id: &'a str,
    pub run_uid: &'a str,
    pub setup: &'a MimoSetup,
    pub table: &'a ChannelTable,
    pub revision: u64,
    pub class: StationClass,
    /// The simulator's plant, when the run is on the simulator.
    pub sim: Option<Value>,
    pub provenance: &'a hertzforge_station::ProvenanceBasis,
    pub devices: &'a [DiscoveredDevice],
    pub caps: &'a Caps,
    /// The reference as the run holds it, in the control space.
    pub reference: &'a HermitianStack,
    pub tolerances: &'a hertzforge_sdm::law::Tolerances,
    pub band: (usize, usize),
    pub df_hz: f64,
    /// Where the reference came from, in words.
    pub reference_source: &'a str,
    /// The controller's own settings as it armed (`MimoRandomService::config_json`).
    pub controller: Value,
    pub setup_hash: String,
    pub seed: u64,
    pub fs_hz: f64,
    pub n_pts: usize,
    pub segment_s: f64,
    pub block_frames: usize,
    pub law: Value,
    pub armed_unix: f64,
    pub utc_offset_s: Option<i64>,
    /// `None` when the streams are recorded; otherwise why not.
    pub streams_off: Option<String>,
}

fn pair(p: [f64; 2]) -> Value {
    json!([num(p[0]), num(p[1])])
}

fn matrix_json(m: &super::setup::RealMatrix) -> Value {
    json!({"rows": m.rows, "cols": m.cols, "values": m.values.iter().map(|v| num(*v)).collect::<Vec<_>>()})
}

fn unit_of(table: &ChannelTable, ch: u32) -> String {
    table
        .channels
        .iter()
        .find(|c| c.index == ch)
        .map_or_else(|| "g".to_owned(), |c| unit_name(&c.units).to_string())
}

/// The band's first and last controlled line in hertz, both included.
fn band_hz(band: (usize, usize), df: f64) -> Value {
    let last = band.1.saturating_sub(1).max(band.0);
    json!([num(band.0 as f64 * df), num(last as f64 * df)])
}

fn setup_json(f: &ArmFacts<'_>) -> Value {
    let s = f.setup;
    let controls: Vec<Value> = s
        .controls
        .iter()
        .map(|c| {
            json!({"channel": c.channel, "label": c.label, "unit": c.unit,
                   "alarm_db": pair(c.alarm_db), "abort_db": pair(c.abort_db)})
        })
        .collect();
    let drives: Vec<Value> = s
        .drives
        .iter()
        .map(|d| {
            json!({"channel": d.channel, "label": d.label, "ceiling_vrms": num(d.ceiling_vrms),
                   "abort_vrms": d.abort_vrms.map(num), "peak_v": num(d.peak_v),
                   "shaker": d.shaker, "moving_mass_lb": num(d.moving_mass_lb)})
        })
        .collect();
    let limits: Vec<Value> = s
        .limits
        .iter()
        .map(|l| {
            json!({"channel": l.channel, "label": l.label, "unit": unit_of(f.table, l.channel),
                   "ceiling": num(l.ceiling), "abort_margin_db": num(l.abort_margin_db)})
        })
        .collect();
    let space = match &s.space {
        ControlSpace::Physical => json!({"kind": "physical"}),
        ControlSpace::Virtual {
            dofs,
            response,
            drive,
        } => json!({"kind": "virtual", "dofs": dofs, "response_transform": matrix_json(response),
                    "drive_transform": drive.as_ref().map(matrix_json),
                    "dof_bands": s.dof_bands.iter().map(|(a, b)| json!({"alarm_db": pair(*a), "abort_db": pair(*b)})).collect::<Vec<_>>()}),
    };
    let reference = match &s.reference {
        Reference::Inline { freqs_hz, n, .. } => json!({
            "kind": "inline", "lines_given": freqs_hz.len(), "n": n,
            "freqs_hz": [freqs_hz.first().copied().map(num), freqs_hz.last().copied().map(num)],
        }),
        Reference::File { path, sha256 } => json!({"kind": "file", "path": path, "sha256": sha256}),
    };
    let e = &s.estimation;
    let i = &s.identification;
    let c = &s.control;
    let lines = e.n_pts / 2 + 1;
    let loop_s = s.loop_s();
    json!({
        "config": s.raw,
        "resolved": {
            "title": s.title,
            "reference": {
                "given": reference,
                "source": f.reference_source,
                "n": f.reference.n(),
                "labels": s.controlled_labels(),
                "space": match s.space { ControlSpace::Physical => "physical", ControlSpace::Virtual { .. } => "virtual" },
                "band": [f.band.0, f.band.1],
                "band_hz": band_hz(f.band, f.df_hz),
                "df_hz": num(f.df_hz),
                "lines": f.reference.lines(),
                "result": "reference",
            },
            "channels": {"controls": controls, "drives": drives, "limits": limits,
                         "article_mass_lb": num(s.article_mass_lb)},
            "space": space,
            "law": {"id": s.law.id, "params": s.law.params},
            "estimation": {"fs_hz": num(e.fs_hz), "n_pts": e.n_pts,
                           "frames_per_loop": e.frames_per_loop, "window": e.window,
                           "overlap": num(e.overlap), "memory_loops": e.memory_loops},
            "identification": {"level_db": num(i.level_db), "frames": i.frames,
                               "noise_frames": i.noise_frames,
                               "coherence_floor": num(i.coherence_floor),
                               "condition_limit": num(i.condition_limit), "review": i.review},
            "control": {"frf": if c.frf_update { "update" } else { "hold" },
                        "frf_blend": num(c.frf_blend), "level_ramp_s": num(c.level_ramp_s),
                        "shutdown_ramp_s": num(c.shutdown_ramp_s),
                        "max_loops_out": c.max_loops_out, "cap_up_db": num(c.cap_up_db),
                        "cap_down_db": num(c.cap_down_db), "law_budget": num(c.law_budget),
                        "law_misses": c.law_misses, "soft_start_db": num(c.soft_start_db)},
            "schedule": {"mode": crate::random::view::mode_name(s.schedule.mode),
                         "levels": s.schedule.levels.iter().map(|(db, dwell)| json!({"db": num(*db), "dwell_s": dwell.map(num)})).collect::<Vec<_>>(),
                         "full_level_s": num(s.schedule.full_level_s)},
            "controller": f.controller,
            "setup_hash": f.setup_hash,
            "seed": f.seed,
        },
        "derived": {
            "grid": {"fs_hz": num(f.fs_hz), "n_pts": f.n_pts, "df_hz": num(f.df_hz),
                     "lines": lines, "loop_s": num(loop_s), "segment_s": num(f.segment_s),
                     "block_frames": f.block_frames, "actual": true},
            "band": {"lines": [f.band.0, f.band.1], "hz": band_hz(f.band, f.df_hz)},
            "controlled": s.controlled(),
            "drives": s.drives.len(),
            "snapshot_every_s": num(SNAPSHOT_EVERY_S),
        },
    })
}

/// The record a MIMO Random run begins with, at arm.
pub fn begin(f: &ArmFacts<'_>, root: PathBuf, links: (TapLink, TapLink), log: Vec<Value>) -> Begin {
    let s = f.setup;
    let mut manifest = Map::new();
    manifest.insert("test".into(), Value::from("mimo_random"));
    manifest.insert("run_uid".into(), Value::from(f.run_uid));
    manifest.insert("run_id".into(), Value::from(f.run_id));
    manifest.insert("title".into(), Value::from(s.title.as_str()));
    manifest.insert("operator".into(), Value::from(s.record.operator.as_str()));
    manifest.insert("article".into(), Value::from(s.record.article.as_str()));
    manifest.insert("notes".into(), Value::from(s.record.notes.as_str()));
    if let Some((document, clause)) = &s.record.specification {
        manifest.insert(
            "specification".into(),
            json!({"document": document, "clause": clause}),
        );
    }
    manifest.insert("source".into(), Value::from(source_of(f.class)));
    if let Some(sim) = &f.sim {
        manifest.insert("sim".into(), sim.clone());
    }
    manifest.insert(
        "station".into(),
        rrec::station_member(f.sim.is_some(), f.class, f.provenance, f.devices, f.caps),
    );
    manifest.insert(
        "grid".into(),
        json!({"fs_hz": num(f.fs_hz), "n_pts": f.n_pts, "df_hz": num(f.df_hz),
               "lines": f.n_pts / 2 + 1, "loop_s": num(s.loop_s())}),
    );
    manifest.insert("build".into(), rrec::build_value(f.provenance.core_build));
    manifest.insert(
        "armed".into(),
        json!({"unix": f.armed_unix, "utc_offset_s": f.utc_offset_s}),
    );
    manifest.insert(
        "start".into(),
        rrec::start_json(f.armed_unix, f.utc_offset_s, 0),
    );
    manifest.insert(
        "recording".into(),
        match &f.streams_off {
            None => json!({"streams": "on"}),
            Some(why) => json!({"streams": "off", "why": why}),
        },
    );
    let mut streams = Vec::new();
    if f.streams_off.is_none() {
        let mut inputs = Map::new();
        inputs.insert("kind".into(), Value::from("inputs"));
        inputs.insert("fs_hz".into(), num(f.fs_hz));
        inputs.insert(
            "channels".into(),
            Value::Array(
                rrec::input_rows(f.table)
                    .into_iter()
                    .map(rrec::stream_channel_value)
                    .collect(),
            ),
        );
        inputs.insert("first_station_frame".into(), Value::from(0));
        streams.push(StreamSpec {
            name: "inputs".into(),
            description: inputs,
            link: links.0,
        });
        let drives = rrec::drive_rows(f.table);
        if !drives.is_empty() {
            let mut d = Map::new();
            d.insert("kind".into(), Value::from("drives"));
            d.insert("fs_hz".into(), num(f.fs_hz));
            d.insert(
                "channels".into(),
                Value::Array(drives.into_iter().map(rrec::stream_channel_value).collect()),
            );
            d.insert("first_station_frame".into(), Value::from(0));
            d.insert(
                "output_latency_frames".into(),
                match &f.caps.output_buffer {
                    Some(b) => json!({"frames": b.min_lookahead_frames, "how": "declared"}),
                    None => json!({"frames": null, "how": "unknown"}),
                },
            );
            streams.push(StreamSpec {
                name: "drives".into(),
                description: d,
                link: links.1,
            });
        }
    }
    let name = format!("{}_mimo_random_{}", rrec::utc_stamp(f.armed_unix), f.run_id);
    Begin {
        root,
        name,
        manifest,
        table: rrec::table_json(f.table, f.revision),
        setup: setup_json(f),
        law: Some((f.law.clone(), None)),
        log,
        streams,
    }
}

// ---- the results --------------------------------------------------------------------

/// What the results name: the controlled quantities (control channels, or degrees of
/// freedom under virtual control), the response channels of the identification
/// (controls then limit channels), the drives and the units.
#[derive(Debug, Clone)]
pub struct Names {
    /// Table indexes of the controlled quantities under physical control; empty under
    /// virtual control.
    pub controls: Vec<u32>,
    /// The controlled quantities' labels (degrees of freedom under virtual control).
    pub labels: Vec<String>,
    pub virtual_space: bool,
    pub responses: Vec<u32>,
    pub drives: Vec<u32>,
    pub control_unit: String,
    pub response_unit: String,
    pub drive_unit: String,
    /// The physical control channels and their unit (identification's response SDM).
    pub physical_controls: Vec<u32>,
}

impl Names {
    /// The names of `setup`'s run on `table`, with only the setup positions `kept`
    /// still controlled (`None`: every one).
    pub fn of(setup: &MimoSetup, table: &ChannelTable, kept: Option<&[usize]>) -> Names {
        let physical: Vec<u32> = setup.controls.iter().map(|c| c.channel).collect();
        let pick = |all: Vec<String>| -> Vec<String> {
            match kept {
                Some(k) => k.iter().filter_map(|&i| all.get(i).cloned()).collect(),
                None => all,
            }
        };
        let virtual_space = matches!(setup.space, ControlSpace::Virtual { .. });
        let controls = if virtual_space {
            Vec::new()
        } else {
            match kept {
                Some(k) => k.iter().filter_map(|&i| physical.get(i).copied()).collect(),
                None => physical.clone(),
            }
        };
        let first = physical.first().copied().unwrap_or(0);
        let control_unit = unit_of(table, first);
        let drives: Vec<u32> = setup.drives.iter().map(|d| d.channel).collect();
        let drive_unit = drives
            .first()
            .map_or_else(|| "V".to_owned(), |d| unit_of(table, *d));
        let mut responses = physical.clone();
        responses.extend(setup.limits.iter().map(|l| l.channel));
        Names {
            controls,
            labels: pick(setup.controlled_labels()),
            virtual_space,
            response_unit: control_unit.clone(),
            control_unit,
            responses,
            drives,
            drive_unit,
            physical_controls: physical,
        }
    }

    /// The controlled quantities as a result names them.
    fn controlled(&self) -> Value {
        if self.virtual_space {
            json!({"dofs": self.labels})
        } else {
            json!({"responses": self.controls, "labels": self.labels})
        }
    }

    fn controlled_pair(&self) -> Value {
        if self.virtual_space {
            json!({"dofs": self.labels})
        } else {
            json!({"responses": self.controls, "references": self.controls,
                   "labels": self.labels})
        }
    }
}

fn f64s(name: &str, shape: Vec<usize>, unit: &str, values: Vec<f64>) -> ResultArray {
    ResultArray {
        name: name.into(),
        shape,
        unit: unit.into(),
        values: ArrayValues::F64(values),
    }
}

fn c16s(name: &str, shape: Vec<usize>, unit: &str, values: Vec<(f64, f64)>) -> ResultArray {
    ResultArray {
        name: name.into(),
        shape,
        unit: unit.into(),
        values: ArrayValues::C128(values),
    }
}

fn described(kind: &str, band: (usize, usize), df: f64, channels: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), Value::from(kind));
    m.insert(
        "abscissa".into(),
        json!({"kind": "frequency", "unit": "Hz", "start": band.0 as f64 * df, "step": df}),
    );
    m.insert("channels".into(), channels);
    m.insert("band".into(), json!([band.0, band.1]));
    m
}

/// A Hermitian stack on the band as `[n, n, lines]` in the file's convention
/// (`conj(G)` of the controller's `G = E[X X^H]`).
fn herm_file(g: &HermitianStack, band: (usize, usize)) -> Vec<(f64, f64)> {
    let n = g.n();
    let mut out = Vec::with_capacity(n * n * (band.1 - band.0));
    for r in 0..n {
        for c in 0..n {
            for line in band.0..band.1 {
                let v = g.get(line, r, c);
                out.push((v.re, -v.im));
            }
        }
    }
    out
}

/// A Hermitian stack on the band packed as `[n (n + 1) / 2, lines]`: its upper
/// triangle row by row, in the file's convention.
fn herm_packed(g: &HermitianStack, band: (usize, usize)) -> Vec<(f64, f64)> {
    let n = g.n();
    let mut out = Vec::with_capacity(n * (n + 1) / 2 * (band.1 - band.0));
    for r in 0..n {
        for c in r..n {
            for line in band.0..band.1 {
                let v = g.get(line, r, c);
                out.push((v.re, -v.im));
            }
        }
    }
    out
}

/// A matrix stack on the band as `[rows, cols, lines]`, unchanged (a transfer
/// function `Y = H X` is the same in either convention).
fn mat_band(m: &MatrixStack, band: (usize, usize)) -> Vec<(f64, f64)> {
    let (_, rows, cols) = m.dims();
    let mut out = Vec::with_capacity(rows * cols * (band.1 - band.0));
    for r in 0..rows {
        for c in 0..cols {
            for line in band.0..band.1 {
                let v = m.get(line, r, c);
                out.push((v.re, v.im));
            }
        }
    }
    out
}

/// A cross spectrum between responses and drives on the band as `[rows, cols,
/// lines]` in the file's convention.
fn cross_file(m: &MatrixStack, band: (usize, usize)) -> Vec<(f64, f64)> {
    mat_band(m, band)
        .into_iter()
        .map(|(re, im)| (re, -im))
        .collect()
}

/// Line-major values (`lines * n`) on the band as `[n, lines]`.
fn per_line(v: &[f64], n: usize, band: (usize, usize)) -> Vec<f64> {
    let mut out = Vec::with_capacity(n * (band.1 - band.0));
    for k in 0..n {
        for line in band.0..band.1 {
            out.push(v.get(line * n + k).copied().unwrap_or(f64::NAN));
        }
    }
    out
}

fn diag(g: &HermitianStack, band: (usize, usize)) -> Vec<f64> {
    let n = g.n();
    let mut out = Vec::with_capacity(n * (band.1 - band.0));
    for k in 0..n {
        for line in band.0..band.1 {
            out.push(g.get(line, k, k).re);
        }
    }
    out
}

/// The reference spectral density matrix the run controls to, at full level, with
/// its alarm and abort lines per controlled quantity.
pub fn reference_result(
    reference: &HermitianStack,
    tol: &hertzforge_sdm::law::Tolerances,
    band: (usize, usize),
    df: f64,
    names: &Names,
    source: &str,
) -> Described {
    let n = reference.n();
    let lb = band.1 - band.0;
    let unit = rrec::psd_unit(&names.control_unit);
    let mut d = described("sdm_reference", band, df, names.controlled_pair());
    d.insert("convention".into(), Value::from("file"));
    d.insert("source".into(), Value::from(source));
    d.insert(
        "description".into(),
        Value::from(
            "the reference spectral density matrix at full level, in the control space, and \
             each controlled quantity's alarm and abort lines as spectral densities (NaN where \
             there is none)",
        ),
    );
    let lines = |v: &[f64]| per_line(v, n, band);
    (
        "reference".into(),
        d,
        vec![
            c16s("values", vec![n, n, lb], &unit, herm_file(reference, band)),
            f64s("alarm_upper", vec![n, lb], &unit, lines(&tol.alarm_upper)),
            f64s("alarm_lower", vec![n, lb], &unit, lines(&tol.alarm_lower)),
            f64s("abort_upper", vec![n, lb], &unit, lines(&tol.abort_upper)),
            f64s("abort_lower", vec![n, lb], &unit, lines(&tol.abort_lower)),
        ],
    )
}

/// The identification set: the transfer function matrix (drives to the control and
/// limit channels), each controlled quantity's multiple coherence, the response,
/// noise-floor and drive spectral matrices and the response-to-drive cross spectrum,
/// and the conditioning, on the band.
pub fn identification_results(
    set: &IdentificationSet,
    loaded: bool,
    band: (usize, usize),
    df: f64,
    names: &Names,
) -> Vec<Described> {
    let lb = band.1 - band.0;
    let (_, n_resp, n_d) = set.frf.dims();
    let n_c = set.g_cc.n();
    let words = if loaded {
        "the identification set loaded for this run"
    } else {
        "the identification measured in this run"
    };
    let responses = json!({"responses": names.responses, "references": names.drives});
    let lag = set.lag_frames;
    let mut frf = described("frf", band, df, responses.clone());
    frf.insert("convention".into(), Value::from("file"));
    frf.insert(
        "description".into(),
        Value::from(format!(
            "{words}: the transfer function matrix from each drive to each control and limit \
             channel, the response taken {lag} frames later (the lag the identification \
             measured)"
        )),
    );
    frf.insert("lag_frames".into(), Value::from(lag));
    let n_r = set.coherence.len() / set.frf.lines().max(1);
    let mut coh = described("coherence", band, df, names.controlled());
    coh.insert(
        "description".into(),
        Value::from(format!(
            "{words}: each controlled quantity's multiple coherence with the drives"
        )),
    );
    let pair = |chs: &[u32]| json!({"responses": chs, "references": chs});
    let mut g_cc = described("cpsd", band, df, pair(&names.physical_controls));
    g_cc.insert("convention".into(), Value::from("file"));
    g_cc.insert(
        "description".into(),
        Value::from(format!(
            "{words}: the control channels' spectral matrix while the drives played"
        )),
    );
    let mut noise = described("cpsd", band, df, pair(&names.physical_controls));
    noise.insert("convention".into(), Value::from("file"));
    noise.insert(
        "description".into(),
        Value::from(format!(
            "{words}: the control channels' spectral matrix with the drives off (the noise floor)"
        )),
    );
    let mut g_dd = described("cpsd", band, df, pair(&names.drives));
    g_dd.insert("convention".into(), Value::from("file"));
    g_dd.insert(
        "description".into(),
        Value::from(format!("{words}: the drives' spectral matrix")),
    );
    let mut g_rd = described("cpsd", band, df, responses);
    g_rd.insert("convention".into(), Value::from("file"));
    g_rd.insert(
        "description".into(),
        Value::from(format!(
            "{words}: the cross spectrum of each control and limit channel with each drive"
        )),
    );
    let mut facts = described(
        "mimo_identification",
        band,
        df,
        json!({"controls": names.physical_controls, "drives": names.drives}),
    );
    facts.insert(
        "description".into(),
        Value::from(format!(
            "{words}: the transfer function's condition number in the control space per line, \
             and the rms each drive played"
        )),
    );
    facts.insert("setup_hash".into(), Value::from(set.setup_hash.as_str()));
    facts.insert("level_db".into(), num(set.level_db));
    facts.insert("frames".into(), Value::from(set.frames));
    facts.insert("steps".into(), Value::from(set.steps));
    facts.insert("lag_frames".into(), Value::from(lag));
    facts.insert("loaded".into(), Value::from(loaded));
    let (cu, du) = (&names.response_unit, &names.drive_unit);
    let psd = rrec::psd_unit;
    vec![
        (
            "identification_frf".into(),
            frf,
            vec![c16s(
                "values",
                vec![n_resp, n_d, lb],
                &format!("{cu}/{du}"),
                mat_band(&set.frf, band),
            )],
        ),
        (
            "identification_coherence".into(),
            coh,
            vec![f64s(
                "values",
                vec![n_r, lb],
                "",
                per_line(&set.coherence, n_r, band),
            )],
        ),
        (
            "identification_response".into(),
            g_cc,
            vec![c16s(
                "values",
                vec![n_c, n_c, lb],
                &psd(cu),
                herm_file(&set.g_cc, band),
            )],
        ),
        (
            "identification_noise".into(),
            noise,
            vec![c16s(
                "values",
                vec![n_c, n_c, lb],
                &psd(cu),
                herm_file(&set.noise, band),
            )],
        ),
        (
            "identification_drive".into(),
            g_dd,
            vec![c16s(
                "values",
                vec![n_d, n_d, lb],
                &psd(du),
                herm_file(&set.g_dd, band),
            )],
        ),
        (
            "identification_cross".into(),
            g_rd,
            vec![c16s(
                "values",
                vec![n_resp, n_d, lb],
                &format!("{cu}*{du}/Hz"),
                cross_file(&set.g_rd, band),
            )],
        ),
        (
            "identification".into(),
            facts,
            vec![
                f64s(
                    "condition",
                    vec![lb],
                    "",
                    (band.0..band.1)
                        .map(|l| set.condition.get(l).copied().unwrap_or(f64::NAN))
                        .collect(),
                ),
                f64s("drive_vrms", vec![n_d], du, set.drive_vrms.clone()),
            ],
        ),
    ]
}

/// The loop record line: the loop's summary as the controller keeps it.
fn loop_line(l: &KeptLoop) -> Value {
    let mut v = serde_json::to_value(&l.summary).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("at_full_level".into(), Value::from(l.at_full_level));
        m.insert("segment_s".into(), num(l.segment_s));
        if let Some(Value::Number(x)) = m.get("level_db") {
            if x.as_f64().is_none_or(|x| !x.is_finite()) {
                m.insert("level_db".into(), Value::Null);
            }
        }
    }
    v
}

/// One loop's results, each a record of a result that repeats: every drive's spectra
/// in force at full level, whose record line is the loop's summary (`drive_spectra`,
/// every loop), and, once the loop has judged the response, the controlled
/// quantities' spectra at full level with their error against the reference
/// (`control_spectra`).
pub fn loop_results(
    l: &KeptLoop,
    band: (usize, usize),
    df: f64,
    names: &Names,
) -> Vec<(Described, Value)> {
    let lb = band.1 - band.0;
    let n_d = l.drive_full.n();
    let line = loop_line(l);
    let mut d = described("psd", band, df, json!({"responses": names.drives}));
    d.insert(
        "description".into(),
        Value::from(
            "each loop: every drive's spectral density in force, referred to full level; the \
             record line is the loop's summary (its level, state, errors, drive rms, what the \
             guards did, its timing and the law's misses in a row)",
        ),
    );
    d.insert(
        "records".into(),
        json!({"index": "loop_index", "level": "level_db"}),
    );
    let mut out = vec![(
        (
            "drive_spectra".into(),
            d,
            vec![f64s(
                "values",
                vec![n_d, lb],
                &rrec::psd_unit(&names.drive_unit),
                diag(&l.drive_full, band),
            )],
        ),
        line.clone(),
    )];
    if let Some(g) = &l.response_full {
        let n_r = g.n();
        let mut c = described("psd", band, df, names.controlled());
        c.insert(
            "description".into(),
            Value::from(
                "each loop that judged the response: every controlled quantity's spectral \
                 density referred to full level and its error against the reference, dB",
            ),
        );
        c.insert(
            "records".into(),
            json!({"index": "loop_index", "level": "level_db"}),
        );
        out.push((
            (
                "control_spectra".into(),
                c,
                vec![
                    f64s(
                        "values",
                        vec![n_r, lb],
                        &rrec::psd_unit(&names.control_unit),
                        diag(g, band),
                    ),
                    f64s(
                        "error_db",
                        vec![n_r, lb],
                        "dB",
                        per_line(&l.error_lines, n_r, band),
                    ),
                ],
            ),
            json!({"loop_index": l.summary.loop_index, "level_db": line["level_db"].clone()}),
        ));
    }
    out
}

/// The full spectral matrices of one loop at full level, each a record of a result
/// that repeats (`control_sdm`, `drive_sdm`); `why` says why this loop was taken.
pub fn snapshot_results(
    loop_index: u64,
    why: &str,
    response: &HermitianStack,
    drive: &HermitianStack,
    band: (usize, usize),
    df: f64,
    names: &Names,
) -> Vec<(Described, Value)> {
    let lb = band.1 - band.0;
    let (n_r, n_d) = (response.n(), drive.n());
    let line = json!({"loop_index": loop_index, "why": why});
    let mut c = described("cpsd", band, df, names.controlled_pair());
    c.insert("convention".into(), Value::from("file"));
    c.insert(
        "description".into(),
        Value::from(
            "the controlled quantities' full spectral matrix referred to full level, at the \
             first loop at full level, on the cadence the setup derives and at the last loop at \
             full level",
        ),
    );
    c.insert("records".into(), json!({"index": "loop_index"}));
    let mut d = described(
        "cpsd",
        band,
        df,
        json!({"responses": names.drives, "references": names.drives}),
    );
    d.insert("convention".into(), Value::from("file"));
    d.insert(
        "description".into(),
        Value::from("the drives' full spectral matrix in force at full level, at the same loops"),
    );
    d.insert("records".into(), json!({"index": "loop_index"}));
    vec![
        (
            (
                "control_sdm".into(),
                c,
                vec![c16s(
                    "values",
                    vec![n_r, n_r, lb],
                    &rrec::psd_unit(&names.control_unit),
                    herm_file(response, band),
                )],
            ),
            line.clone(),
        ),
        (
            (
                "drive_sdm".into(),
                d,
                vec![c16s(
                    "values",
                    vec![n_d, n_d, lb],
                    &rrec::psd_unit(&names.drive_unit),
                    herm_file(drive, band),
                )],
            ),
            line,
        ),
    ]
}

/// One control-law call as a record of `law_calls`: the inputs that change from call
/// to call (the response, the transfer function and the multiple coherence it was
/// given, the drive in force, the response-to-drive cross spectrum when the law takes
/// it) and the drive it returned, all in the law's own space at full level; NaN where
/// a call returned none or an input was not given. The reference, its tolerances and
/// the identification are their own results.
pub fn law_call_result(
    seq: u64,
    c: &KeptLawCall,
    band: (usize, usize),
    df: f64,
    names: &Names,
    with_csd: bool,
    law: Option<Value>,
) -> (Described, Value) {
    let lb = band.1 - band.0;
    let n_r = c.response.n();
    let n_u = c.frf.cols();
    let nan = |n: usize| vec![(f64::NAN, f64::NAN); n];
    let tri = |n: usize| n * (n + 1) / 2;
    let herm_or = |g: Option<&HermitianStack>, n: usize| {
        g.filter(|g| g.n() == n)
            .map_or_else(|| nan(tri(n) * lb), |g| herm_packed(g, band))
    };
    let (drive, outcome) = match &c.outcome {
        Ok(o) => (herm_or(Some(&o.drive), n_u), json!("ok")),
        Err(_) => (nan(tri(n_u) * lb), json!("miss")),
    };
    let cu = &names.control_unit;
    let du = &names.drive_unit;
    let mut arrays = vec![
        c16s(
            "response",
            vec![tri(n_r), lb],
            &rrec::psd_unit(cu),
            herm_packed(&c.response, band),
        ),
        c16s(
            "frf",
            vec![n_r, n_u, lb],
            &format!("{cu}/{du}"),
            mat_band(&c.frf, band),
        ),
        f64s(
            "multiple_coherence",
            vec![n_r, lb],
            "",
            per_line(&c.multiple_coherence, n_r, band),
        ),
        c16s(
            "last_drive",
            vec![tri(n_u), lb],
            &rrec::psd_unit(du),
            herm_or(c.last_drive.as_ref(), n_u),
        ),
        c16s("drive", vec![tri(n_u), lb], &rrec::psd_unit(du), drive),
    ];
    if with_csd {
        arrays.push(c16s(
            "response_drive_csd",
            vec![n_r, n_u, lb],
            &format!("{cu}*{du}/Hz"),
            c.response_drive_csd
                .as_ref()
                .filter(|m| m.dims().1 == n_r && m.dims().2 == n_u)
                .map_or_else(|| nan(n_r * n_u * lb), |m| cross_file(m, band)),
        ));
    }
    let mut d = described("mimo_law_calls", band, df, names.controlled());
    d.insert("convention".into(), Value::from("file"));
    d.insert(
        "description".into(),
        Value::from(
            "every call of the control law, in its own space at full level: the inputs that \
             change from call to call and the drive it returned (NaN where it returned none, or \
             where an input was not given); the record line says which call it was, how it \
             ended and the law's declared values on the band",
        ),
    );
    d.insert(
        "packed".into(),
        json!({"arrays": ["response", "last_drive", "drive"],
               "how": "each line's upper triangle, row by row: (0,0), (0,1), ..., (1,1), ..."}),
    );
    d.insert("records".into(), json!({"index": "seq"}));
    let mut line = Map::new();
    line.insert("seq".into(), Value::from(seq));
    if let Some(l) = law {
        line.insert("law".into(), l);
    }
    line.insert(
        "call".into(),
        Value::from(if c.seed { "seed" } else { "iterate" }),
    );
    line.insert("loop_index".into(), Value::from(c.loop_index));
    line.insert("response_dof".into(), num(c.response_dof));
    line.insert("outcome".into(), outcome);
    line.insert(
        "last_drive_given".into(),
        Value::from(c.last_drive.is_some()),
    );
    line.insert(
        "response_drive_csd_given".into(),
        Value::from(c.response_drive_csd.is_some()),
    );
    match &c.outcome {
        Ok(o) => {
            line.insert(
                "diagnostics".into(),
                Value::Array(
                    o.diagnostics
                        .iter()
                        .map(|d| diagnostic_json(d, band, n_r, n_u))
                        .collect(),
                ),
            );
        }
        Err(words) => {
            line.insert("miss".into(), Value::from(words.as_str()));
        }
    }
    (("law_calls".into(), d, arrays), Value::Object(line))
}

/// A law's declared value on the band: a number for the loop, or per line (per
/// control or per drive) on the band's lines, `[n, lines]`.
fn diagnostic_json(
    d: &hertzforge_sdm::law::DiagnosticValue,
    band: (usize, usize),
    n_r: usize,
    n_u: usize,
) -> Value {
    use hertzforge_sdm::law::DiagnosticData as D;
    let (kind, values) = match &d.data {
        D::Scalar(x) => ("scalar", json!(num(*x))),
        D::PerLine(v) => (
            "per_line",
            json!(per_line(v, 1, band)
                .into_iter()
                .map(num)
                .collect::<Vec<_>>()),
        ),
        D::PerControlLine(v) => (
            "per_control_line",
            json!(chunks(per_line(v, n_r, band), band)),
        ),
        D::PerDriveLine(v) => (
            "per_drive_line",
            json!(chunks(per_line(v, n_u, band), band)),
        ),
    };
    json!({"key": d.key, "kind": kind, "values": values})
}

fn chunks(v: Vec<f64>, band: (usize, usize)) -> Vec<Vec<Value>> {
    v.chunks((band.1 - band.0).max(1))
        .map(|c| c.iter().map(|x| num(*x)).collect())
        .collect()
}

// ---- the log ------------------------------------------------------------------------

/// A controller event as the record's log holds it: its kind, its words and its
/// facts.
pub fn event_entry(e: &MimoEvent) -> (&'static str, String, Value) {
    let facts = match serde_json::to_value(&e.kind) {
        Ok(Value::Object(m)) => m.into_iter().next().map_or(Value::Null, |(_, v)| v),
        _ => Value::Null,
    };
    let state = |s: hertzforge_mimo_random::MimoState| {
        serde_json::to_value(s)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
            .replace('_', " ")
    };
    let (kind, text) = match &e.kind {
        MimoEventKind::StateChange { from, to } => {
            ("state", format!("{} to {}", state(*from), state(*to)))
        }
        MimoEventKind::LevelChange {
            from_db,
            to_db,
            auto,
        } => (
            "level",
            format!(
                "level {} dB to {} dB{}",
                fmt_db(*from_db),
                fmt_db(*to_db),
                if *auto { " (schedule)" } else { "" }
            ),
        ),
        MimoEventKind::Override { what, ack } => ("override", format!("override of {what}: {ack}")),
        MimoEventKind::EnvelopeAlarm {
            control,
            lines,
            worst_db,
        } => (
            "alarm",
            format!(
                "control {control} outside its alarm band on {lines} lines, worst {:.2} dB",
                worst_db
            ),
        ),
        MimoEventKind::OverloadLatch { channel_index } => {
            ("overload", format!("channel {channel_index} overloaded"))
        }
        MimoEventKind::LawMiss { reason, detail } => (
            "law_miss",
            format!("the law's call was not used: {} ({detail})", reason.words()),
        ),
        MimoEventKind::Trip { record } => ("trip", record.kind.words()),
        MimoEventKind::Record { path } => ("record", format!("recording to {path}")),
        MimoEventKind::Note { text } => return ("note", text.clone(), Value::Null),
        MimoEventKind::ControlExcluded { channel_index, ack } => (
            "override",
            format!("channel {channel_index} taken out of control: {ack}"),
        ),
        #[allow(unreachable_patterns)]
        _ => ("note", String::new()),
    };
    (kind, text, facts)
}

fn fmt_db(x: f64) -> String {
    if x.is_finite() {
        format!("{x}")
    } else {
        "off".into()
    }
}

/// The input rows the station reads, for a stream's width.
pub fn input_width(table: &ChannelTable) -> usize {
    rrec::input_rows(table).len()
}

/// The drive rows the station plays.
pub fn drive_width(table: &ChannelTable) -> usize {
    rrec::drive_rows(table).len()
}
