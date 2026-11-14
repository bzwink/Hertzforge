//! The run service's own records (design 11, Q38).
//!
//! The control service does not use them: it turns them off and records every MIMO
//! Random run in the shared run folder (`docs/run-folder.md`, "What a MIMO Random Run
//! Saves"), taking each loop and law call from
//! [`crate::MimoRandomService::keep_for_record`] and writing nothing on the loop's
//! thread. They stay for a run made with the service directly (its own tests, and the
//! [`crate::replay`] of a recorded law), and for the identification set's own files,
//! which the control service writes and reads to save and load a set
//! ([`write_identification`], [`read_identification`]).
//!
//! On by default for such a run. Its folder under the local data root holds:
//!
//! | File | What | Cadence |
//! |---|---|---|
//! | `telemetry.f32`, `capture.json` | the raw input stream (`hertzforge_capture::CaptureSink`) | every segment |
//! | `identification.bin` / `.json` | the identification set with its setup hash | once |
//! | `loops_diagonal.bin` / `.json` | per loop: every control's PSD and error, every drive's PSD | every loop |
//! | `loops_summary.jsonl` | per loop: level, state, rms errors, drive rms, guards, timing | every loop |
//! | `sdm_<loop>.bin` / `.json` | the full control response and drive spectral matrices | every `snapshot_every_s`, plus the first and last loop at full level |
//! | `reference.bin` / `.json` | the reference in the control space, its tolerances, spacing, and band (`band` as half-open line indexes, `band_hz` as the first and last band line in Hz) | once |
//! | `law.json`, `law_file.wasm` | the law: id, name, version, hash, parameter values, and the law file when it is one | at each seed |
//! | `law_calls.bin` / `.jsonl` | every law call: its declared inputs packed, the loop index and the law's output (unchanged inputs carried forward) | every call |
//! | `report.json` | the report data | at the end |
//!
//! Binary payloads are little-endian `f64`, complex values as `re, im` pairs,
//! Hermitian stacks packed (upper triangle, row-major, line-major), matrix stacks
//! row-major per line; each sidecar names the arrays, their shapes and their byte
//! offsets. These records are not converted to a project file; a control service
//! run's folder is.

use crate::report::{GuardSummary, LawLine, LoopTiming, MimoReport, MimoState};
use crate::service::IdentificationSet;
use hertzforge_capture::{
    append_binary_record, read_binary_record, write_atomic, write_binary_record, CaptureConfig,
    CaptureSink,
};
use hertzforge_channel_table::ChannelTable;
use hertzforge_random::service::engine::SegmentResult;
use hertzforge_sdm::{HermitianStack, MatrixStack, C64};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The identification record's name.
pub const IDENTIFICATION: &str = "identification";
/// The per-loop diagonal record's name.
pub const LOOPS_DIAGONAL: &str = "loops_diagonal";
/// The per-loop summary file.
pub const LOOPS_SUMMARY: &str = "loops_summary.jsonl";

/// One loop's summary line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopSummary {
    pub loop_index: u64,
    pub level_db: f64,
    pub state: MimoState,
    pub error_db: Vec<f64>,
    pub drive_rms: Vec<f64>,
    pub guards: GuardSummary,
    pub timing: LoopTiming,
    pub law_misses: u32,
}

/// One array in a binary payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayInfo {
    pub name: String,
    /// `f64` or `c64` (two `f64` per value).
    pub kind: String,
    /// Logical shape; a packed Hermitian stack is `[lines, n, n]` with
    /// `packed: true`.
    pub shape: Vec<usize>,
    pub packed: bool,
    pub offset_bytes: usize,
    pub len_bytes: usize,
}

/// A payload being assembled.
#[derive(Default)]
struct Payload {
    bytes: Vec<u8>,
    arrays: Vec<ArrayInfo>,
}

impl Payload {
    fn f64s(&mut self, name: &str, shape: Vec<usize>, v: &[f64]) {
        let off = self.bytes.len();
        for x in v {
            self.bytes.extend_from_slice(&x.to_le_bytes());
        }
        self.arrays.push(ArrayInfo {
            name: name.into(),
            kind: "f64".into(),
            shape,
            packed: false,
            offset_bytes: off,
            len_bytes: self.bytes.len() - off,
        });
    }

    fn c64s(&mut self, name: &str, shape: Vec<usize>, packed: bool, v: &[C64]) {
        let off = self.bytes.len();
        for x in v {
            self.bytes.extend_from_slice(&x.re.to_le_bytes());
            self.bytes.extend_from_slice(&x.im.to_le_bytes());
        }
        self.arrays.push(ArrayInfo {
            name: name.into(),
            kind: "c64".into(),
            shape,
            packed,
            offset_bytes: off,
            len_bytes: self.bytes.len() - off,
        });
    }

    fn herm(&mut self, name: &str, g: &HermitianStack) {
        self.c64s(name, vec![g.lines(), g.n(), g.n()], true, g.data());
    }

    fn mat(&mut self, name: &str, m: &MatrixStack) {
        let (l, r, c) = m.dims();
        self.c64s(name, vec![l, r, c], false, m.data());
    }
}

/// The identification record's sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentificationSidecar {
    pub format: String,
    pub setup_hash: String,
    pub drive_vrms: Vec<f64>,
    pub level_db: f64,
    pub frames: usize,
    pub steps: usize,
    pub lag_frames: usize,
    pub arrays: Vec<ArrayInfo>,
}

/// The format tag of an identification record.
pub const IDENT_FORMAT: &str = "hertzforge-mimo-identification/1";

fn read_f64(b: &[u8], a: &ArrayInfo) -> Result<Vec<f64>, String> {
    let s = b
        .get(a.offset_bytes..a.offset_bytes + a.len_bytes)
        .ok_or_else(|| format!("array {} runs past the payload", a.name))?;
    Ok(s.chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().expect("8 bytes")))
        .collect())
}

fn read_c64(b: &[u8], a: &ArrayInfo) -> Result<Vec<C64>, String> {
    let v = read_f64(b, a)?;
    Ok(v.chunks_exact(2).map(|p| C64::new(p[0], p[1])).collect())
}

fn find<'a>(arrays: &'a [ArrayInfo], name: &str) -> Result<&'a ArrayInfo, String> {
    arrays
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("the record has no array `{name}`"))
}

fn herm_of(b: &[u8], arrays: &[ArrayInfo], name: &str) -> Result<HermitianStack, String> {
    let a = find(arrays, name)?;
    let (l, n) = (a.shape[0], a.shape[1]);
    HermitianStack::from_packed(l, n, read_c64(b, a)?).map_err(|e| e.0)
}

fn mat_of(b: &[u8], arrays: &[ArrayInfo], name: &str) -> Result<MatrixStack, String> {
    let a = find(arrays, name)?;
    MatrixStack::from_vec(a.shape[0], a.shape[1], a.shape[2], read_c64(b, a)?).map_err(|e| e.0)
}

/// Write an identification set as a binary record in `dir`.
pub fn write_identification(dir: &Path, set: &IdentificationSet) -> Result<PathBuf, String> {
    let mut p = Payload::default();
    p.mat("frf", &set.frf);
    p.herm("g_cc", &set.g_cc);
    p.mat("g_rd", &set.g_rd);
    p.herm("g_dd", &set.g_dd);
    p.herm("noise", &set.noise);
    let lines = set.frf.lines();
    p.f64s(
        "coherence",
        vec![lines, set.coherence.len() / lines.max(1)],
        &set.coherence,
    );
    p.f64s("condition", vec![lines], &set.condition);
    let side = IdentificationSidecar {
        format: IDENT_FORMAT.into(),
        setup_hash: set.setup_hash.clone(),
        drive_vrms: set.drive_vrms.clone(),
        level_db: set.level_db,
        frames: set.frames,
        steps: set.steps,
        lag_frames: set.lag_frames,
        arrays: p.arrays,
    };
    write_binary_record(dir, IDENTIFICATION, &side, &p.bytes).map_err(|e| e.to_string())
}

/// Read an identification set back from a run folder, for
/// `MimoRandomService::load_identification` (which checks the setup hash).
pub fn read_identification(dir: &Path) -> Result<IdentificationSet, String> {
    let (side, bytes) = read_binary_record(dir, IDENTIFICATION)
        .map_err(|e| format!("no identification record in {}: {e}", dir.display()))?;
    let side: IdentificationSidecar = serde_json::from_value(side)
        .map_err(|e| format!("the identification sidecar is not readable: {e}"))?;
    if side.format != IDENT_FORMAT {
        return Err(format!(
            "the identification record is format `{}`, this program reads `{IDENT_FORMAT}`",
            side.format
        ));
    }
    let a = &side.arrays;
    Ok(IdentificationSet {
        setup_hash: side.setup_hash.clone(),
        frf: mat_of(&bytes, a, "frf")?,
        g_cc: herm_of(&bytes, a, "g_cc")?,
        g_rd: mat_of(&bytes, a, "g_rd")?,
        g_dd: herm_of(&bytes, a, "g_dd")?,
        noise: herm_of(&bytes, a, "noise")?,
        coherence: read_f64(&bytes, find(a, "coherence")?)?,
        condition: read_f64(&bytes, find(a, "condition")?)?,
        drive_vrms: side.drive_vrms,
        level_db: side.level_db,
        frames: side.frames,
        steps: side.steps,
        lag_frames: side.lag_frames,
    })
}

/// The per-loop diagonal record's sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagonalSidecar {
    pub lines: usize,
    pub controls: usize,
    pub drives: usize,
    /// Loops in the payload; each is `lines * (2 * controls + drives)` `f64`:
    /// the control PSDs at full level, the control errors in dB (NaN out of band),
    /// the drive PSDs at full level, each line-major.
    pub loops: u64,
    pub loop_index: Vec<u64>,
}

/// The snapshot sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotSidecar {
    pub loop_index: u64,
    pub why: String,
    pub arrays: Vec<ArrayInfo>,
}

/// Writes one run's records.
pub struct RunRecorder {
    dir: PathBuf,
    sink: Option<CaptureSink>,
    diag: Option<DiagonalSidecar>,
    snapshot_every_s: f64,
    since_snapshot_s: f64,
    first_full_written: bool,
    last_full: Option<(u64, HermitianStack, HermitianStack)>,
    loop_index: u64,
    law_calls: LawCallRecorder,
    law_record: Option<LawRecord>,
}

impl RunRecorder {
    /// Start the run's records under `data_root/name` (or the first free
    /// `name-<n>`; [`Self::run_dir`] says which) with the raw stream of `n_ch`
    /// inputs at `fs_hz`.
    pub fn start(
        data_root: &Path,
        name: &str,
        n_ch: usize,
        fs_hz: f64,
        table: ChannelTable,
        settings_json: &str,
    ) -> Result<Self, String> {
        let sink = CaptureSink::start(
            data_root,
            name,
            CaptureConfig::at_rate(n_ch, fs_hz),
            table,
            settings_json.to_string(),
        )
        .map_err(|e| e.to_string())?;
        Ok(RunRecorder {
            dir: sink.run_dir().to_path_buf(),
            sink: Some(sink),
            diag: None,
            snapshot_every_s: crate::config::DEFAULT_SNAPSHOT_EVERY_S,
            since_snapshot_s: 0.0,
            first_full_written: false,
            last_full: None,
            loop_index: 0,
            law_calls: LawCallRecorder::default(),
            law_record: None,
        })
    }

    /// Set the snapshot cadence, seconds.
    pub fn set_snapshot_every_s(&mut self, s: f64) {
        self.snapshot_every_s = s;
    }

    /// The run folder.
    pub fn run_dir(&self) -> &Path {
        &self.dir
    }

    /// Feed one segment's raw interleaved inputs (never blocks; drops on a full
    /// ring, counted by the sink).
    pub fn feed_raw(&mut self, seg: &SegmentResult) {
        if let Some(s) = self.sink.as_ref() {
            let frames = seg.interleaved.len() / seg.n_ch.max(1);
            let samples: Vec<f32> = seg.interleaved.iter().map(|&v| v as f32).collect();
            s.push(&samples, frames);
        }
    }

    /// Record the reference the run controls to.
    pub fn write_reference(
        &self,
        reference: &HermitianStack,
        tol: &hertzforge_sdm::law::Tolerances,
        band: (usize, usize),
        df_hz: f64,
        controls: &[u32],
        space: &str,
    ) -> Result<(), String> {
        write_reference(&self.dir, reference, tol, band, df_hz, controls, space)
    }

    /// Record one law call: its declared inputs, its loop index and its outputs.
    pub fn write_law_call(
        &mut self,
        seed: bool,
        inputs: &hertzforge_sdm::law::MimoLoopInputs<'_>,
        outcome: Result<&hertzforge_sdm::law::MimoLoopOutputs, &str>,
        law: Option<(&str, &[serde_json::Value])>,
    ) -> Result<(), String> {
        let dir = self.dir.clone();
        self.law_calls.write(&dir, seed, inputs, outcome, law)
    }

    /// Record the law: its identity and parameter values, and the law file itself
    /// (copied into the run folder as `law_file.wasm`) when it is one.
    pub fn write_law_record(
        &mut self,
        mut record: LawRecord,
        law_file: Option<&Path>,
    ) -> Result<(), String> {
        if let Some(f) = law_file {
            let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
            write_atomic(&self.dir.join("law_file.wasm"), &bytes).map_err(|e| e.to_string())?;
            record.file = Some("law_file.wasm".into());
            record.catalog_path = Some(f.display().to_string());
        }
        let json = serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?;
        write_atomic(&self.dir.join(LAW_RECORD), &json).map_err(|e| e.to_string())?;
        self.law_record = Some(record);
        Ok(())
    }

    /// Record the identification set.
    pub fn write_identification(&mut self, set: &IdentificationSet) -> Result<String, String> {
        write_identification(&self.dir, set).map(|p| p.display().to_string())
    }

    fn snapshot(
        &self,
        loop_index: u64,
        why: &str,
        resp: &HermitianStack,
        drive: &HermitianStack,
    ) -> Result<(), String> {
        let mut p = Payload::default();
        p.herm("response_full_level", resp);
        p.herm("drive_full_level", drive);
        let side = SnapshotSidecar {
            loop_index,
            why: why.into(),
            arrays: p.arrays,
        };
        write_binary_record(&self.dir, &format!("sdm_{loop_index:06}"), &side, &p.bytes)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Record one loop: the summary, the diagonals, and a full snapshot when one
    /// is due.
    pub fn write_loop(
        &mut self,
        summary: &LoopSummary,
        resp_full: Option<&HermitianStack>,
        drive_full: &HermitianStack,
        error_lines: &[f64],
        at_full_level: bool,
        segment_s: f64,
    ) -> Result<(), String> {
        use std::io::Write;
        self.loop_index = summary.loop_index;
        let line = serde_json::to_string(summary).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(LOOPS_SUMMARY))
            .map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
        let Some(resp) = resp_full else {
            return Ok(());
        };
        let (lines, nc, nd) = (resp.lines(), resp.n(), drive_full.n());
        let mut chunk = Vec::with_capacity(lines * (2 * nc + nd) * 8);
        for line in 0..lines {
            for i in 0..nc {
                chunk.extend_from_slice(&resp.get(line, i, i).re.to_le_bytes());
            }
        }
        for line in 0..lines {
            for i in 0..nc {
                let e = error_lines.get(line * nc + i).copied().unwrap_or(f64::NAN);
                chunk.extend_from_slice(&e.to_le_bytes());
            }
        }
        for line in 0..lines {
            for k in 0..nd {
                chunk.extend_from_slice(&drive_full.get(line, k, k).re.to_le_bytes());
            }
        }
        let side = self.diag.get_or_insert(DiagonalSidecar {
            lines,
            controls: nc,
            drives: nd,
            loops: 0,
            loop_index: Vec::new(),
        });
        side.loops += 1;
        side.loop_index.push(summary.loop_index);
        append_binary_record(&self.dir, LOOPS_DIAGONAL, side, &chunk).map_err(|e| e.to_string())?;
        self.since_snapshot_s += segment_s;
        if at_full_level && !self.first_full_written {
            self.first_full_written = true;
            self.since_snapshot_s = 0.0;
            self.snapshot(
                summary.loop_index,
                "first loop at full level",
                resp,
                drive_full,
            )?;
        } else if self.since_snapshot_s >= self.snapshot_every_s {
            self.since_snapshot_s = 0.0;
            self.snapshot(summary.loop_index, "cadence", resp, drive_full)?;
        }
        if at_full_level {
            self.last_full = Some((summary.loop_index, resp.clone(), drive_full.clone()));
        }
        Ok(())
    }

    /// Finish: the last full-level snapshot, the law record, the report, the raw
    /// stream's sidecar.
    pub fn finish(&mut self, law: Option<&LawLine>, report: &MimoReport) -> Result<(), String> {
        if let Some((idx, r, d)) = self.last_full.take() {
            let name = format!("sdm_{idx:06}");
            if !self.dir.join(format!("{name}.bin")).exists() {
                self.snapshot(idx, "last loop at full level", &r, &d)?;
            }
        }
        if self.law_record.is_none() {
            if let Some(l) = law {
                let json = serde_json::to_vec_pretty(l).map_err(|e| e.to_string())?;
                write_atomic(&self.dir.join(LAW_RECORD), &json).map_err(|e| e.to_string())?;
            }
        }
        let json = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
        write_atomic(&self.dir.join("report.json"), &json).map_err(|e| e.to_string())?;
        if let Some(s) = self.sink.take() {
            s.finish().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Read a full spectral-matrix snapshot back: the response and drive at full
/// level.
pub fn read_snapshot(
    dir: &Path,
    loop_index: u64,
) -> Result<(HermitianStack, HermitianStack), String> {
    let (side, bytes) =
        read_binary_record(dir, &format!("sdm_{loop_index:06}")).map_err(|e| e.to_string())?;
    let side: SnapshotSidecar = serde_json::from_value(side).map_err(|e| e.to_string())?;
    Ok((
        herm_of(&bytes, &side.arrays, "response_full_level")?,
        herm_of(&bytes, &side.arrays, "drive_full_level")?,
    ))
}

// ---- the reference, the law and the law's inputs (design 11 and A11) ------------------

/// The reference record's name.
pub const REFERENCE: &str = "reference";
/// The law-input record's name: one entry per law call.
pub const LAW_CALLS: &str = "law_calls";
/// The law record's file.
pub const LAW_RECORD: &str = "law.json";
/// The format tag of the law-input record.
pub const LAW_CALLS_FORMAT: &str = "hertzforge-mimo-law-calls/1";

/// The reference record's sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceSidecar {
    /// The line spacing, Hz.
    pub df_hz: f64,
    /// The controlled band as line indexes, half-open: lines `band[0]` up to
    /// but not including `band[1]`.
    pub band: [usize; 2],
    /// The same band in hertz: the frequencies of its first and last
    /// controlled lines, both included (`band[0] * df_hz` and
    /// `(band[1] - 1) * df_hz`). Absent from records written before it was
    /// added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band_hz: Option<[f64; 2]>,
    /// The control channels (table indexes) of a physical reference; for a
    /// virtual one, the channels the response transformation reads.
    pub controls: Vec<u32>,
    /// Physical or virtual.
    pub space: String,
    pub arrays: Vec<ArrayInfo>,
}

/// Write the reference the run controls to: the spectral matrix in the control
/// space at full level, its tolerances, band and line spacing.
pub fn write_reference(
    dir: &Path,
    reference: &HermitianStack,
    tol: &hertzforge_sdm::law::Tolerances,
    band: (usize, usize),
    df_hz: f64,
    controls: &[u32],
    space: &str,
) -> Result<(), String> {
    let mut p = Payload::default();
    p.herm("sdm", reference);
    let shape = vec![reference.lines(), reference.n()];
    p.f64s("alarm_upper", shape.clone(), &tol.alarm_upper);
    p.f64s("alarm_lower", shape.clone(), &tol.alarm_lower);
    p.f64s("abort_upper", shape.clone(), &tol.abort_upper);
    p.f64s("abort_lower", shape, &tol.abort_lower);
    let side = ReferenceSidecar {
        df_hz,
        band: [band.0, band.1],
        band_hz: Some(band_hz(band, df_hz)),
        controls: controls.to_vec(),
        space: space.into(),
        arrays: p.arrays,
    };
    write_binary_record(dir, REFERENCE, &side, &p.bytes)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The band's first and last controlled line in hertz, both included, from
/// the half-open line range `band` at spacing `df_hz`.
pub fn band_hz(band: (usize, usize), df_hz: f64) -> [f64; 2] {
    let last = band.1.saturating_sub(1).max(band.0);
    [band.0 as f64 * df_hz, last as f64 * df_hz]
}

/// Read the reference record: the spectral matrix, tolerances, band and spacing.
pub fn read_reference(
    dir: &Path,
) -> Result<
    (
        HermitianStack,
        hertzforge_sdm::law::Tolerances,
        (usize, usize),
        f64,
    ),
    String,
> {
    let (side, bytes) = read_binary_record(dir, REFERENCE).map_err(|e| e.to_string())?;
    let side: ReferenceSidecar = serde_json::from_value(side).map_err(|e| e.to_string())?;
    let a = &side.arrays;
    let tol = hertzforge_sdm::law::Tolerances {
        alarm_upper: read_f64(&bytes, find(a, "alarm_upper")?)?,
        alarm_lower: read_f64(&bytes, find(a, "alarm_lower")?)?,
        abort_upper: read_f64(&bytes, find(a, "abort_upper")?)?,
        abort_lower: read_f64(&bytes, find(a, "abort_lower")?)?,
    };
    Ok((
        herm_of(&bytes, a, "sdm")?,
        tol,
        (side.band[0], side.band[1]),
        side.df_hz,
    ))
}

/// One parameter value as the law record keeps it: `{"key", "kind", "value"}`.
pub fn param_json(p: &hertzforge_law_host::Param) -> serde_json::Value {
    use hertzforge_law_host::ParamValue as V;
    let (kind, value) = match &p.value {
        V::Number(x) => ("number", serde_json::json!(x)),
        V::Integer(x) => ("integer", serde_json::json!(x)),
        V::Toggle(x) => ("toggle", serde_json::json!(x)),
        V::Choice(x) => ("choice", serde_json::json!(x)),
        V::PerChannel(x) => ("per_channel", serde_json::json!(x)),
        V::Curve(points) => (
            "curve",
            serde_json::Value::from(
                points
                    .iter()
                    .map(|b| serde_json::json!([b.frequency_hz, b.value]))
                    .collect::<Vec<_>>(),
            ),
        ),
    };
    serde_json::json!({"key": p.key, "kind": kind, "value": value})
}

/// A parameter value back from its record form.
pub fn param_from_json(v: &serde_json::Value) -> Result<hertzforge_law_host::Param, String> {
    use hertzforge_law_host::{Breakpoint, Param, ParamValue as V};
    let key = v["key"]
        .as_str()
        .ok_or("a recorded parameter has no key")?
        .to_owned();
    let x = &v["value"];
    let bad = || format!("the recorded parameter `{key}` is not readable");
    let value = match v["kind"].as_str() {
        Some("number") => V::Number(x.as_f64().ok_or_else(bad)?),
        Some("integer") => V::Integer(x.as_i64().ok_or_else(bad)?),
        Some("toggle") => V::Toggle(x.as_bool().ok_or_else(bad)?),
        Some("choice") => V::Choice(x.as_str().ok_or_else(bad)?.to_owned()),
        Some("per_channel") => V::PerChannel(
            x.as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|e| e.as_f64().ok_or_else(bad))
                .collect::<Result<_, _>>()?,
        ),
        Some("curve") => V::Curve(
            x.as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|e| {
                    Ok(Breakpoint {
                        frequency_hz: e[0].as_f64().ok_or_else(bad)?,
                        value: e[1].as_f64().ok_or_else(bad)?,
                    })
                })
                .collect::<Result<_, String>>()?,
        ),
        _ => return Err(bad()),
    };
    Ok(Param::new(key, value))
}

/// The law record (design 11): which law ran, with which parameter values, and
/// where its code is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LawRecord {
    /// The catalog id: a built-in law's name or an imported file's SHA-256.
    pub id: String,
    pub name: String,
    pub version: String,
    /// The law file's SHA-256; empty for a built-in law.
    pub sha256: String,
    pub builtin: bool,
    /// Every parameter value, in manifest order (`param_json`).
    pub params: Vec<serde_json::Value>,
    /// The law file copied into the run folder (its name there), when the law is
    /// a file.
    pub file: Option<String>,
    /// Where the law file was in the law catalog when the run used it.
    pub catalog_path: Option<String>,
    /// The parameter values as the report prints them.
    pub parameters: Vec<String>,
}

/// One law call as recorded.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    pub seq: u64,
    /// `true` for the seed (a freshly built law), `false` for an iteration.
    pub seed: bool,
    pub loop_index: u64,
    pub inputs: OwnedInputs,
    /// The law's output as it returned it (before the core's checks), or the
    /// words of a call that did not return one.
    pub outcome: Result<hertzforge_sdm::law::MimoLoopOutputs, String>,
    /// For a seed: the law that was built (`id` and parameter values).
    pub law: Option<(String, Vec<serde_json::Value>)>,
}

/// A law's inputs, owned (what [`hertzforge_sdm::law::MimoLoopInputs`] borrows).
#[derive(Debug, Clone)]
pub struct OwnedInputs {
    pub reference: HermitianStack,
    pub tolerances: hertzforge_sdm::law::Tolerances,
    pub response: HermitianStack,
    pub response_dof: f64,
    pub frf: MatrixStack,
    pub multiple_coherence: Vec<f64>,
    pub df_hz: f64,
    pub band: (usize, usize),
    pub loop_index: u64,
    pub last_drive: Option<HermitianStack>,
    pub response_drive_csd: Option<MatrixStack>,
    pub sysid_response: Option<HermitianStack>,
    pub sysid_drive: Option<HermitianStack>,
    pub sysid_noise: Option<HermitianStack>,
}

impl OwnedInputs {
    /// The borrowed form a law takes.
    pub fn as_inputs(&self) -> hertzforge_sdm::law::MimoLoopInputs<'_> {
        hertzforge_sdm::law::MimoLoopInputs {
            reference: &self.reference,
            tolerances: &self.tolerances,
            response: &self.response,
            response_dof: self.response_dof,
            frf: &self.frf,
            multiple_coherence: &self.multiple_coherence,
            df_hz: self.df_hz,
            band: self.band,
            loop_index: self.loop_index,
            last_drive: self.last_drive.as_ref(),
            response_drive_csd: self.response_drive_csd.as_ref(),
            sysid_response: self.sysid_response.as_ref(),
            sysid_drive: self.sysid_drive.as_ref(),
            sysid_noise: self.sysid_noise.as_ref(),
        }
    }

    /// The law setup these inputs were made for.
    pub fn setup(&self) -> hertzforge_sdm::law::MimoSetup {
        hertzforge_sdm::law::MimoSetup {
            controls: self.reference.n(),
            drives: self.frf.cols(),
            lines: self.reference.lines(),
            df_hz: self.df_hz,
            band: self.band,
            reference: self.reference.clone(),
            tolerances: self.tolerances.clone(),
        }
    }
}

/// Writes the law-input record: each call's inputs packed, with the loop index and
/// the law's outputs. Inputs that do not change between calls (the reference, its
/// tolerances, the FRF while it is held, the identification set) are written when
/// they change and carried forward by the reader: an entry lists them under
/// `carried`.
#[derive(Default)]
pub struct LawCallRecorder {
    last: std::collections::HashMap<&'static str, Vec<u8>>,
    seq: u64,
}

/// Names of the inputs carried forward when unchanged.
const CARRIED: [&str; 10] = [
    "reference",
    "alarm_upper",
    "alarm_lower",
    "abort_upper",
    "abort_lower",
    "frf",
    "multiple_coherence",
    "sysid_response",
    "sysid_drive",
    "sysid_noise",
];

impl LawCallRecorder {
    /// Record one call. `law` names the law built for a seed.
    pub fn write(
        &mut self,
        dir: &Path,
        seed: bool,
        inputs: &hertzforge_sdm::law::MimoLoopInputs<'_>,
        outcome: Result<&hertzforge_sdm::law::MimoLoopOutputs, &str>,
        law: Option<(&str, &[serde_json::Value])>,
    ) -> Result<(), String> {
        // Each array encoded alone, so a carried one is compared by its bytes.
        let mut parts: Vec<(&'static str, Payload)> = Vec::new();
        let one = |f: &dyn Fn(&mut Payload)| {
            let mut p = Payload::default();
            f(&mut p);
            p
        };
        let (lines, n) = (inputs.reference.lines(), inputs.reference.n());
        let tshape = vec![lines, n];
        let t = inputs.tolerances;
        parts.push(("reference", one(&|p| p.herm("reference", inputs.reference))));
        parts.push((
            "alarm_upper",
            one(&|p| p.f64s("alarm_upper", tshape.clone(), &t.alarm_upper)),
        ));
        parts.push((
            "alarm_lower",
            one(&|p| p.f64s("alarm_lower", tshape.clone(), &t.alarm_lower)),
        ));
        parts.push((
            "abort_upper",
            one(&|p| p.f64s("abort_upper", tshape.clone(), &t.abort_upper)),
        ));
        parts.push((
            "abort_lower",
            one(&|p| p.f64s("abort_lower", tshape.clone(), &t.abort_lower)),
        ));
        parts.push(("response", one(&|p| p.herm("response", inputs.response))));
        parts.push(("frf", one(&|p| p.mat("frf", inputs.frf))));
        parts.push((
            "multiple_coherence",
            one(&|p| {
                p.f64s(
                    "multiple_coherence",
                    vec![lines, inputs.multiple_coherence.len() / lines.max(1)],
                    inputs.multiple_coherence,
                )
            }),
        ));
        if let Some(g) = inputs.last_drive {
            parts.push(("last_drive", one(&|p| p.herm("last_drive", g))));
        }
        if let Some(m) = inputs.response_drive_csd {
            parts.push((
                "response_drive_csd",
                one(&|p| p.mat("response_drive_csd", m)),
            ));
        }
        for (name, g) in [
            ("sysid_response", inputs.sysid_response),
            ("sysid_drive", inputs.sysid_drive),
            ("sysid_noise", inputs.sysid_noise),
        ] {
            if let Some(g) = g {
                parts.push((name, one(&|p| p.herm(name, g))));
            }
        }
        if let Ok(out) = outcome {
            parts.push(("out_drive", one(&|p| p.herm("out_drive", &out.drive))));
        }
        let mut chunk = Vec::new();
        let mut arrays = Vec::new();
        let mut carried = Vec::new();
        let mut present = Vec::new();
        for (name, p) in parts {
            present.push(name);
            if CARRIED.contains(&name) && self.last.get(name) == Some(&p.bytes) {
                carried.push(name);
                continue;
            }
            for mut a in p.arrays {
                a.offset_bytes += chunk.len();
                arrays.push(a);
            }
            chunk.extend_from_slice(&p.bytes);
            if CARRIED.contains(&name) {
                self.last.insert(name, p.bytes);
            }
        }
        let mut index = serde_json::Map::new();
        index.insert("format".into(), LAW_CALLS_FORMAT.into());
        index.insert("seq".into(), self.seq.into());
        index.insert("call".into(), if seed { "seed" } else { "iterate" }.into());
        index.insert("loop_index".into(), inputs.loop_index.into());
        index.insert("response_dof".into(), inputs.response_dof.into());
        index.insert("df_hz".into(), inputs.df_hz.into());
        index.insert(
            "band".into(),
            serde_json::json!([inputs.band.0, inputs.band.1]),
        );
        index.insert("present".into(), serde_json::json!(present));
        index.insert("carried".into(), serde_json::json!(carried));
        index.insert(
            "arrays".into(),
            serde_json::to_value(&arrays).map_err(|e| e.to_string())?,
        );
        match outcome {
            Ok(out) => {
                index.insert("outcome".into(), "ok".into());
                index.insert(
                    "diagnostics".into(),
                    serde_json::to_value(&out.diagnostics).map_err(|e| e.to_string())?,
                );
            }
            Err(words) => {
                index.insert("outcome".into(), "miss".into());
                index.insert("miss".into(), words.into());
            }
        }
        if let Some((id, params)) = law {
            index.insert(
                "law".into(),
                serde_json::json!({"id": id, "params": params}),
            );
        }
        hertzforge_capture::append_indexed_record(dir, LAW_CALLS, &index, &chunk)
            .map_err(|e| e.to_string())?;
        self.seq += 1;
        Ok(())
    }
}

/// Read every recorded law call of a run folder, in order, carried inputs filled in.
pub fn read_law_calls(dir: &Path) -> Result<Vec<RecordedCall>, String> {
    let entries = hertzforge_capture::read_indexed_record(dir, LAW_CALLS)
        .map_err(|e| format!("no law-input record in {}: {e}", dir.display()))?;
    let mut carry: std::collections::HashMap<String, (ArrayInfo, Vec<u8>)> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    for (index, chunk) in entries {
        if index.get("format").and_then(|v| v.as_str()) != Some(LAW_CALLS_FORMAT) {
            return Err(format!(
                "the law-input record is not format `{LAW_CALLS_FORMAT}`; read it with the \
                 version that wrote it"
            ));
        }
        let arrays: Vec<ArrayInfo> =
            serde_json::from_value(index.get("arrays").cloned().unwrap_or_default())
                .map_err(|e| format!("the law-input index is not readable: {e}"))?;
        let mut here: std::collections::HashMap<String, (ArrayInfo, Vec<u8>)> =
            std::collections::HashMap::new();
        for a in arrays {
            let b = chunk
                .get(a.offset_bytes..a.offset_bytes + a.len_bytes)
                .ok_or_else(|| format!("array {} runs past its entry", a.name))?
                .to_vec();
            let mut a2 = a.clone();
            a2.offset_bytes = 0;
            if CARRIED.contains(&a.name.as_str()) {
                carry.insert(a.name.clone(), (a2.clone(), b.clone()));
            }
            here.insert(a.name.clone(), (a2, b));
        }
        let present: Vec<String> =
            serde_json::from_value(index.get("present").cloned().unwrap_or_default())
                .unwrap_or_default();
        let get = |name: &str| -> Option<(ArrayInfo, Vec<u8>)> {
            if !present.iter().any(|p| p == name) && name != "out_drive" {
                return None;
            }
            here.get(name).cloned().or_else(|| carry.get(name).cloned())
        };
        let herm = |name: &str| -> Result<Option<HermitianStack>, String> {
            match get(name) {
                None => Ok(None),
                Some((a, b)) => herm_of(&b, std::slice::from_ref(&a), name).map(Some),
            }
        };
        let mat = |name: &str| -> Result<Option<MatrixStack>, String> {
            match get(name) {
                None => Ok(None),
                Some((a, b)) => mat_of(&b, std::slice::from_ref(&a), name).map(Some),
            }
        };
        let f64v = |name: &str| -> Result<Vec<f64>, String> {
            let (a, b) = get(name).ok_or_else(|| format!("the entry has no `{name}`"))?;
            read_f64(&b, &a)
        };
        let need = |v: Option<HermitianStack>, name: &str| {
            v.ok_or_else(|| format!("the entry has no `{name}`"))
        };
        let band = index
            .get("band")
            .and_then(|b| b.as_array())
            .map(|b| {
                (
                    b[0].as_u64().unwrap_or(0) as usize,
                    b[1].as_u64().unwrap_or(0) as usize,
                )
            })
            .unwrap_or((0, 0));
        let inputs = OwnedInputs {
            reference: need(herm("reference")?, "reference")?,
            tolerances: hertzforge_sdm::law::Tolerances {
                alarm_upper: f64v("alarm_upper")?,
                alarm_lower: f64v("alarm_lower")?,
                abort_upper: f64v("abort_upper")?,
                abort_lower: f64v("abort_lower")?,
            },
            response: need(herm("response")?, "response")?,
            response_dof: index
                .get("response_dof")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            frf: mat("frf")?.ok_or("the entry has no `frf`")?,
            multiple_coherence: f64v("multiple_coherence")?,
            df_hz: index.get("df_hz").and_then(|v| v.as_f64()).unwrap_or(0.0),
            band,
            loop_index: index
                .get("loop_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            last_drive: herm("last_drive")?,
            response_drive_csd: mat("response_drive_csd")?,
            sysid_response: herm("sysid_response")?,
            sysid_drive: herm("sysid_drive")?,
            sysid_noise: herm("sysid_noise")?,
        };
        let outcome = match index.get("outcome").and_then(|v| v.as_str()) {
            Some("ok") => {
                let drive = here
                    .get("out_drive")
                    .ok_or("an ok entry has no output drive")?;
                Ok(hertzforge_sdm::law::MimoLoopOutputs {
                    drive: herm_of(&drive.1, std::slice::from_ref(&drive.0), "out_drive")?,
                    diagnostics: serde_json::from_value(
                        index.get("diagnostics").cloned().unwrap_or_default(),
                    )
                    .map_err(|e| format!("recorded diagnostics are not readable: {e}"))?,
                })
            }
            _ => Err(index
                .get("miss")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned()),
        };
        let law = index.get("law").map(|l| {
            (
                l["id"].as_str().unwrap_or("").to_owned(),
                l["params"].as_array().cloned().unwrap_or_default(),
            )
        });
        out.push(RecordedCall {
            seq: index.get("seq").and_then(|v| v.as_u64()).unwrap_or(0),
            seed: index.get("call").and_then(|v| v.as_str()) == Some("seed"),
            loop_index: inputs.loop_index,
            inputs,
            outcome,
            law,
        });
    }
    Ok(out)
}

/// Read a run's law record.
pub fn read_law_record(dir: &Path) -> Result<LawRecord, String> {
    let text = std::fs::read(dir.join(LAW_RECORD))
        .map_err(|e| format!("no law record in {}: {e}", dir.display()))?;
    serde_json::from_slice(&text).map_err(|e| format!("the law record is not readable: {e}"))
}

/// Copy a run's identification set (its record pair) into `dest`, a folder.
pub fn copy_identification(run_dir: &Path, dest: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    for ext in ["bin", "json"] {
        let from = run_dir.join(format!("{IDENTIFICATION}.{ext}"));
        let bytes = std::fs::read(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        write_atomic(&dest.join(format!("{IDENTIFICATION}.{ext}")), &bytes)
            .map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(dest.to_path_buf())
}
