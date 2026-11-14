//! The run's machinery behind [`super::MixedService`]: the station and its
//! engine, the step, the identification, and the between-chunk loop (the
//! floor's estimate and law, the bands' servo, the next synthesis).
//!
//! Between chunks, off the step's thread (design section 3, steps 3 to 7):
//! the residual the step recorded is cut into Hann frames at half overlap;
//! each frame, normalized by the random part's gain that played it, adds to
//! the floor's per-line average on its clean lines and to each band's
//! moving-frame level; every `blocks_per_window` frames the floor law is
//! called under Random's guards and each band's servo moves; the random part
//! of the drive (floor plus each band at the center it will have when the
//! samples play) is synthesized into the step's ring ahead of play.

use super::status::{MixedTripKind, RoleTally};
use super::step::{AmpModel, MixedProcessStep, PlantGrid, StepMode, StepTrip, ToneCtl};
use super::{LawRole, MixedError, MixedLawFactories};
use crate::band::{
    add_band_drive, band_weights, inner_lines, widened_db, BandLevel, BandServo, BAND_GUARD_LINES,
};
use crate::floor::{dof_per_frame, exclusion_mask, hann, periodogram, FloorEstimator};
use crate::parts::{BandWidth, MixedConfig, PartBody};
use crate::tone::{Ramp, ToneBank};
use hertzforge_dsp::cola::ColaSynth;
use hertzforge_engine::{Engine, EngineConfig, StopReason, VecCaptureSink};
use hertzforge_hal::{Backend, ConfiguredActual};
use hertzforge_random::service::guard::{cap_change, guarded_call, Budget};
use hertzforge_random::{ControlLaw, LawClock, LoopInputs};
use hertzforge_sine::law::{Identification, LawValues, SineLawSetup};
use std::collections::VecDeque;
use std::sync::Arc;

/// Engine ticks per bounded batch.
pub(crate) const TICKS_PER_BATCH: u64 = 4;
/// Station time per chunk, seconds.
pub(crate) const CHUNK_S: f64 = 0.25;
/// The identification's random part ramps in and out over this, seconds.
const SYSID_RAMP_S: f64 = 0.25;
/// Analysis frames the identification averages.
const SYSID_FRAMES: usize = 48;
/// Longest pipeline lag the identification searches, seconds.
const MAX_LAG_S: f64 = 0.25;
/// A floor line is judged against its envelope at this many degrees of freedom.
pub(crate) const MIN_ENVELOPE_DOF: f64 = 16.0;
/// The share of the floor's in-band lines that must each hold
/// [`MIN_ENVELOPE_DOF`] before the floor's rms abort is armed. Three
/// quarters: an rms over most of the band is the band's rms (tones' and
/// bands' exclusions cover a few percent of it), and one starved line (a line
/// a part has just freed) cannot disarm the guard.
pub(crate) const RMS_ABORT_SHARE: f64 = 0.75;
/// The floor is equalized (tones and bands may come in) when its rms is
/// within this of the target, dB.
pub(crate) const FLOOR_EQUALIZED_DB: f64 = 1.0;
/// A frame whose random gain varies more than this share across it is not
/// averaged (it played during a ramp).
const GAIN_STEADY: f64 = 0.01;

/// One band's control state.
pub(crate) struct BandCtl {
    pub(crate) part: usize,
    pub(crate) level_g2: f64,
    pub(crate) width: BandWidth,
    pub(crate) servo: BandServo,
    /// The average since the last loop (the servo's input).
    pub(crate) loop_level: BandLevel,
    /// The running average shown and judged.
    pub(crate) level: BandLevel,
    pub(crate) alarm_db: (f64, f64),
    pub(crate) abort_db: (f64, f64),
    /// The band's ramp, stepped per synthesized hop.
    pub(crate) scale: Ramp,
    pub(crate) switched_on: bool,
    pub(crate) alarm: bool,
    pub(crate) error_db: Option<f64>,
    pub(crate) center_now: f64,
    pub(crate) capped: u64,
    /// Whether the random part's rms ceiling or a limit channel held the band
    /// down in its last loop (its servo did not raise its gain).
    pub(crate) limiting: bool,
}

/// What one synthesized stretch of the ring carries.
#[derive(Debug, Clone)]
struct SegMeta {
    /// The drive frame its first sample plays at.
    start: u64,
    len: usize,
    /// Each band's center and its ramp scale there.
    bands: Vec<(f64, f64)>,
}

/// The identification.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ident {
    pub(crate) h_mag2: Vec<f64>,
    pub(crate) gain: Vec<f64>,
    pub(crate) coherence: Vec<f64>,
    pub(crate) trusted: Vec<bool>,
    pub(crate) lag: usize,
    pub(crate) band_coherence: f64,
    pub(crate) trusted_frac: f64,
    /// Per control channel, the complex transfer function, drive to response
    /// (the response taken `lag` frames later), in the file's convention:
    /// `conj(X) Y / |X|^2` with the standard (negative-exponent) transform, so
    /// a delay lags. Kept for a host's record; nothing in the run reads it.
    pub(crate) h: Vec<Vec<(f64, f64)>>,
    /// Per control channel, the ordinary coherence (the composite is
    /// [`Self::coherence`]).
    pub(crate) coherence_by: Vec<Vec<f64>>,
}

/// Everything a run holds once it is armed.
pub(crate) struct Runtime {
    pub(crate) backend: Box<dyn Backend>,
    engine: Option<Engine>,
    /// The table the engine is built against (kept to rebuild it at engage).
    table: hertzforge_channel_table::ChannelTable,
    sim: bool,
    n_in: usize,
    pub(crate) actual: ConfiguredActual,
    pub(crate) step: MixedProcessStep,
    pub(crate) fs: f64,
    pub(crate) nfft: usize,
    pub(crate) df: f64,
    pub(crate) lines: usize,
    /// The floor's band, `[lo, hi)` lines.
    pub(crate) band: (usize, usize),
    pub(crate) target: Vec<f64>,
    pub(crate) win: Vec<f64>,
    scratch: Vec<(f64, f64)>,
    synth: ColaSynth,
    hop: usize,
    // Analysis.
    res_buf: Vec<VecDeque<f64>>,
    ctl_buf: Vec<VecDeque<f64>>,
    gain_buf: VecDeque<f64>,
    /// The response frame index of `res_buf[.][0]`.
    resp_at: u64,
    pub(crate) est: FloorEstimator,
    frames_since_loop: usize,
    pub(crate) frames_total: u64,
    pub(crate) floor_law: Option<Box<dyn ControlLaw + Send>>,
    pub(crate) floor_drive: Vec<f64>,
    pub(crate) floor_tally: RoleTally,
    pub(crate) floor_misses: u32,
    divergent_loops: u32,
    floor_clock: Arc<dyn LawClock>,
    pub(crate) bands: Vec<BandCtl>,
    segs: VecDeque<SegMeta>,
    /// Drive frames the ring's tail will play at.
    pub(crate) ring_end: u64,
    pub(crate) ident: Ident,
    // Display.
    pub(crate) last_control: Vec<Vec<f64>>,
    pub(crate) last_residual: Vec<Vec<f64>>,
    pub(crate) last_composite: Vec<f64>,
    pub(crate) last_residual_composite: Vec<f64>,
    pub(crate) last_measured: Vec<f64>,
    pub(crate) last_excluded: Vec<bool>,
    pub(crate) floor_rms_err_db: Option<f64>,
    pub(crate) floor_rms_g: Option<f64>,
    pub(crate) alarm_lines: usize,
    pub(crate) abort_lines: usize,
    pub(crate) lines_out: usize,
    pub(crate) floor_dof: f64,
    /// The typical (median) degrees of freedom over the floor's clean lines,
    /// as the floor's status reports them.
    pub(crate) floor_dof_typical: f64,
    /// The lines the floor's rms abort was judged on in the last loop (each
    /// holding [`MIN_ENVELOPE_DOF`]); 0 while it is not armed.
    pub(crate) rms_judged_lines: usize,
    pub(crate) loops: u64,
    pub(crate) relax_db: f64,
    /// Raw control samples since the run thread last took them, interleaved.
    pub(crate) tap: Vec<f32>,
    /// Limit channels: index, response ceiling on the grid, `|H|^2` to it.
    pub(crate) limits: Vec<LimitCtl>,
    /// Lines of the random part the limit channels held down on the last
    /// synthesis.
    pub(crate) notched: Vec<bool>,
    /// Whether the random part's rms ceiling scaled the last hop synthesized.
    pub(crate) rms_held: bool,
    /// Each floor law call since the service last drained them.
    pub(crate) floor_log: Vec<super::law_records::MixedLawRecord>,
    /// The capture, while one runs.
    pub(crate) capture: Option<hertzforge_capture::CaptureSink>,
    floor_drive_in_last: f64,
    floor_took_last: f64,
    floor_missed_last: bool,
}

/// One limit channel.
pub(crate) struct LimitCtl {
    pub(crate) channel_index: u32,
    pub(crate) ceiling: Vec<f64>,
    pub(crate) h_mag2: Vec<f64>,
    buf: VecDeque<f64>,
    sum: Vec<f64>,
    frames: usize,
}

/// The line grid's frame length for `cfg` at `fs`.
pub(crate) fn frame_len(cfg: &MixedConfig, fs: f64) -> usize {
    cfg.nfft.unwrap_or_else(|| {
        let n = (fs / cfg.df_hz).ceil().max(16.0) as usize;
        n.next_power_of_two()
    })
}

impl Runtime {
    /// Open, configure, gate and start the station; build the grid and the
    /// step. The step is silent.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn arm(
        cfg: &MixedConfig,
        table: &hertzforge_channel_table::ChannelTable,
        mut backend: Box<dyn Backend>,
        station: hertzforge_module_abi::StationClass,
        requested_rate: f64,
        block_frames: Option<usize>,
        clock: crate::schedule::PartClock,
        tone_clock: Arc<dyn hertzforge_sine::law::LawClock>,
        floor_clock: Arc<dyn LawClock>,
    ) -> Result<Self, MixedError> {
        let plant = |e: hertzforge_hal::HalError| MixedError::Plant(e.to_string());
        backend.open().map_err(plant)?;
        let block = block_frames.unwrap_or_else(|| {
            if station.is_hardware() {
                hertzforge_engine::default_block_frames(requested_rate)
            } else {
                hertzforge_engine::SIM_NATURAL_BLOCK_FRAMES
            }
        });
        let acq = hertzforge_hal::AcquisitionConfig::new(requested_rate, block);
        let actual = backend.configure(table, &acq).map_err(plant)?;
        let fs = actual.actual_rate_hz;
        cfg.validate(Some(fs))?;
        hertzforge_module_abi::arm_gate(
            &backend.caps(),
            station,
            hertzforge_module_abi::Resolution::host_closed_loop(),
            None,
            fs,
        )
        .map_err(|reason| MixedError::BadState(format!("arm refused: {reason}")))?;
        backend.arm().map_err(plant)?;
        backend.start().map_err(plant)?;

        let nfft = frame_len(cfg, fs);
        let df = fs / nfft as f64;
        // The exclusion must cover at least two lines either side of a tone
        // (the tracker reads in time and needs no line rule of its own).
        if cfg.tones().next().is_some() && df > cfg.exclusion_hz / 2.0 {
            backend.emergency_stop().ok();
            return Err(MixedError::Config(crate::parts::ConfigError::BadConfig {
                field: "exclusion_hz",
                why: format!(
                    "the exclusion covers at least two lines: lines of {df:.3} Hz need \
                     exclusion_hz of at least {:.3} Hz",
                    2.0 * df
                ),
            }));
        }
        let lines = nfft / 2 + 1;
        let floor = cfg
            .floor()
            .ok_or(MixedError::Config(crate::parts::ConfigError::NoFloor))?;
        let (flo, fhi) = floor.band().unwrap_or((0.0, 0.0));
        let band = (
            ((flo / df).ceil() as usize).min(lines),
            (((fhi / df).floor() as usize) + 1).min(lines),
        );
        let spec = floor
            .breakpoints
            .interpolate(df, (lines - 1) as f64 * df)
            .map_err(|e| MixedError::BadState(format!("the floor's profile: {e}")))?;
        let mut target = vec![0.0; lines];
        for (k, t) in target.iter_mut().enumerate().take(band.1).skip(band.0) {
            let f = k as f64 * df;
            let j = ((f - spec.f0_hz) / spec.df_hz).round();
            if j >= 0.0 {
                *t = spec.psd.get(j as usize).copied().unwrap_or(0.0);
            }
        }
        // Input frame layout: enabled non-drive rows in table order.
        let mut slots = Vec::new();
        let mut n_in = 0usize;
        let mut slot_of = std::collections::HashMap::new();
        for ch in &table.channels {
            if !ch.enabled || ch.role == hertzforge_channel_table::ChannelRole::Drive {
                continue;
            }
            slot_of.insert(ch.index, n_in);
            n_in += 1;
        }
        for c in &cfg.control.channels {
            slots.push(*slot_of.get(&c.channel_index).ok_or_else(|| {
                MixedError::BadState(format!(
                    "control channel {} is not an input",
                    c.channel_index
                ))
            })?);
        }
        let chunk_frames = (CHUNK_S * fs) as usize;
        let mut step = MixedProcessStep::new(
            cfg,
            fs,
            n_in.max(1),
            1,
            slots,
            clock,
            tone_clock,
            chunk_frames * 2 + 4096,
        );
        // The raw-sample guards.
        if cfg.limits.kinematic_monitor {
            let kin = hertzforge_random::KinematicMonitor::new(table, fs, 5.0, 1, 5.0);
            step.kin = kin.is_active().then_some(kin);
        }
        for ch in &table.channels {
            if !ch.enabled || ch.role == hertzforge_channel_table::ChannelRole::Drive {
                continue;
            }
            let at = if ch.range_max > 0.0 {
                cfg.limits.overload_fraction * ch.range_max
            } else {
                f64::INFINITY
            };
            let steers = cfg
                .control
                .channels
                .iter()
                .any(|c| c.channel_index == ch.index)
                || cfg
                    .limits
                    .limit_profiles
                    .iter()
                    .any(|l| l.channel_index == ch.index);
            step.overload.push((ch.index, at, steers));
        }
        let mut limits = Vec::new();
        for lp in &cfg.limits.limit_profiles {
            let slot = *slot_of.get(&lp.channel_index).ok_or_else(|| {
                MixedError::BadState(format!(
                    "limit channel {} is not an input",
                    lp.channel_index
                ))
            })?;
            step.lim_slots.push(slot);
            step.records
                .limits
                .push(Vec::with_capacity(chunk_frames * 2 + 4096));
            let mut ceiling = vec![f64::INFINITY; lines];
            for (c, v) in ceiling.iter_mut().zip(&lp.resp_ceiling) {
                *c = *v;
            }
            limits.push(LimitCtl {
                channel_index: lp.channel_index,
                ceiling,
                h_mag2: vec![0.0; lines],
                buf: VecDeque::new(),
                sum: vec![0.0; lines],
                frames: 0,
            });
        }
        let n_ctrl = cfg.control.channels.len();
        let depth = (cfg.control_dof / dof_per_frame()).ceil() as usize;
        let mut ecfg = EngineConfig::with_block(n_in.max(1), 1, &actual);
        if !station.is_hardware() {
            ecfg.jitter_frac = hertzforge_engine::TEST_DISABLE_JITTER_GUARD;
            ecfg.compute_budget = Some(hertzforge_engine::TEST_RELAX_COMPUTE_BUDGET);
        }
        let engine = Engine::new(ecfg, table, backend.as_ref());
        Ok(Runtime {
            backend,
            engine: Some(engine),
            table: table.clone(),
            sim: !station.is_hardware(),
            n_in: n_in.max(1),
            actual,
            step,
            fs,
            nfft,
            df,
            lines,
            band,
            target,
            win: hann(nfft),
            scratch: Vec::with_capacity(nfft),
            synth: ColaSynth::with_seed(nfft, 0.5, fs, 0x4D49_5845_445F_0001),
            hop: nfft / 2,
            res_buf: vec![VecDeque::new(); n_ctrl],
            ctl_buf: vec![VecDeque::new(); n_ctrl],
            gain_buf: VecDeque::new(),
            resp_at: 0,
            est: FloorEstimator::new(lines, depth.max(cfg.blocks_per_window)),
            frames_since_loop: 0,
            frames_total: 0,
            floor_law: None,
            floor_drive: vec![0.0; lines],
            floor_tally: RoleTally::default(),
            floor_misses: 0,
            divergent_loops: 0,
            floor_clock,
            bands: Vec::new(),
            segs: VecDeque::new(),
            ring_end: 0,
            ident: Ident::default(),
            last_control: vec![Vec::new(); n_ctrl],
            last_residual: vec![Vec::new(); n_ctrl],
            last_composite: Vec::new(),
            last_residual_composite: Vec::new(),
            last_measured: Vec::new(),
            last_excluded: vec![false; lines],
            floor_rms_err_db: None,
            floor_rms_g: None,
            alarm_lines: 0,
            abort_lines: 0,
            lines_out: 0,
            floor_dof: 0.0,
            floor_dof_typical: 0.0,
            rms_judged_lines: 0,
            loops: 0,
            relax_db: 0.0,
            tap: Vec::new(),
            limits,
            notched: vec![false; lines],
            rms_held: false,
            floor_log: Vec::new(),
            capture: None,
            floor_drive_in_last: 0.0,
            floor_took_last: 0.0,
            floor_missed_last: false,
        })
    }

    /// Frames per bounded batch.
    fn batch_frames(&self) -> usize {
        TICKS_PER_BATCH as usize * self.actual.block_frames.max(1)
    }

    /// Batches in one chunk.
    pub(crate) fn batches_per_chunk(&self) -> u32 {
        ((CHUNK_S * self.fs / self.batch_frames() as f64).ceil() as u32).max(1)
    }

    /// One bounded batch. `Ok(None)` while the station runs; a station stop
    /// becomes a trip.
    pub(crate) fn run_batch(&mut self) -> Result<Option<MixedTripKind>, MixedError> {
        let Some(engine) = self.engine.as_mut() else {
            return Err(MixedError::BadState("the station is closed".into()));
        };
        let mut sink = VecCaptureSink::new();
        let backend = &mut self.backend;
        let step = &mut self.step;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.run_ticks(backend.as_mut(), step, &mut sink, TICKS_PER_BATCH)
        }));
        let report = match caught {
            Ok(r) => r,
            Err(_) => {
                let _ = self.backend.emergency_stop();
                return Ok(Some(MixedTripKind::Station {
                    message: "the run's step panicked; the station was stopped".into(),
                }));
            }
        };
        Ok(match report.reason {
            StopReason::InputEnded => None,
            StopReason::AbortTripped(t) => Some(MixedTripKind::Kinematic {
                channel_index: t.channel_index,
                axis: format!("{:?}", t.axis).to_lowercase(),
                value: t.value,
                limit: t.limit,
            }),
            other => Some(MixedTripKind::Station {
                message: format!("{other:?}"),
            }),
        })
    }

    /// Park the station after the identification: the drive has ramped to
    /// zero; the controlled stop takes the device off drive, so the gate and
    /// the review hold wait with nothing playing and nothing unwatched (Sine's
    /// park). [`Self::restart`] brings it back at engage.
    pub(crate) fn park(&mut self) -> Result<(), MixedError> {
        self.engine = None;
        self.backend
            .stop()
            .map_err(|e| MixedError::Plant(format!("parking the station: {e}")))
    }

    /// Re-arm and restart a parked station with a fresh engine.
    pub(crate) fn restart(&mut self) -> Result<(), MixedError> {
        if self.engine.is_some() {
            return Ok(());
        }
        let plant = |e: hertzforge_hal::HalError| MixedError::Plant(e.to_string());
        self.backend.arm().map_err(plant)?;
        self.backend.start().map_err(plant)?;
        let mut ecfg = EngineConfig::with_block(self.n_in, 1, &self.actual);
        if self.sim {
            ecfg.jitter_frac = hertzforge_engine::TEST_DISABLE_JITTER_GUARD;
            ecfg.compute_budget = Some(hertzforge_engine::TEST_RELAX_COMPUTE_BUDGET);
        }
        self.engine = Some(Engine::new(ecfg, &self.table, self.backend.as_ref()));
        Ok(())
    }

    /// Stop the station's tasks (the drive has been ramped to zero).
    pub(crate) fn stop_station(&mut self) {
        let _ = self.backend.stop();
        self.engine = None;
    }

    /// Stop the station at once.
    pub(crate) fn emergency(&mut self) {
        let _ = self.backend.emergency_stop();
        self.engine = None;
    }

    // ---- identification ---------------------------------------------------

    /// Broadband identification on the line grid: flat in the floor's band at
    /// `vrms`, ramped in and out. Fills [`Self::ident`].
    pub(crate) fn identify(
        &mut self,
        vrms: f64,
        weights: &[f64],
        trust: (f64, f64),
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), MixedError> {
        let (lo, hi) = self.band;
        let width = (hi - lo) as f64 * self.df;
        let mut psd = vec![0.0; self.lines];
        for p in psd.iter_mut().take(hi).skip(lo) {
            *p = vrms * vrms / width;
        }
        let max_lag = (MAX_LAG_S * self.fs) as usize;
        let need = SYSID_FRAMES * self.hop + self.nfft + max_lag;
        let ramp = SYSID_RAMP_S * self.fs;
        self.step.mode = StepMode::Identify;
        self.step.level = Ramp::at(1.0);
        self.step.random = Ramp::at(0.0);
        self.step.random.go(1.0, ramp);
        let lead = (ramp as usize) + self.batch_frames();
        let mut drive = Vec::with_capacity(need + lead);
        let mut resp: Vec<Vec<f64>> = vec![Vec::with_capacity(need + lead); weights.len()];
        let mut lresp: Vec<Vec<f64>> = vec![Vec::new(); self.limits.len()];
        let mut recorded = 0usize;
        let skip = ramp as usize + self.batch_frames();
        let mut stopping = false;
        loop {
            while self.step.ring.len() < 4 * self.batch_frames() + self.hop {
                let x = self.synth.next_frame(&psd);
                self.step.ring.extend(x);
            }
            if let Some(t) = self.run_batch()? {
                self.emergency();
                return Err(MixedError::Plant(format!("identification stopped: {t:?}")));
            }
            let r = &mut self.step.records;
            if !stopping {
                for (k, x) in r.drive.iter().enumerate() {
                    if recorded + k >= skip {
                        drive.push(*x);
                        for (rc, ctl) in resp.iter_mut().zip(&r.control) {
                            rc.push(ctl[k]);
                        }
                        for (lc, lim) in lresp.iter_mut().zip(&r.limits) {
                            lc.push(lim[k]);
                        }
                    }
                }
                recorded += r.drive.len();
            }
            clear(r);
            if cancel.load(std::sync::atomic::Ordering::Acquire) && !stopping {
                self.step.random.go(0.0, ramp);
                stopping = true;
            }
            if !stopping && drive.len() >= need {
                self.step.random.go(0.0, ramp);
                stopping = true;
            }
            if stopping && self.step.random.value() == 0.0 {
                break;
            }
        }
        self.step.ring.clear();
        self.step.mode = StepMode::Silent;
        self.park()?;
        if cancel.load(std::sync::atomic::Ordering::Acquire) && drive.len() < need {
            return Err(MixedError::BadState(
                "the identification was cancelled".into(),
            ));
        }
        self.ident = estimate_plant(
            &drive, &resp, weights, self.fs, self.nfft, self.band, max_lag, trust,
        );
        for (l, y) in self.limits.iter_mut().zip(lresp) {
            l.h_mag2 = estimate_plant(
                &drive,
                &[y],
                &[1.0],
                self.fs,
                self.nfft,
                self.band,
                max_lag,
                trust,
            )
            .h_mag2;
        }
        Ok(())
    }

    // ---- engage --------------------------------------------------------------

    /// Build the laws, the tones and the bands, seed the floor's drive, and
    /// start the floor alone at `level` (a gain) ramping in over `ramp_frames`.
    pub(crate) fn engage(
        &mut self,
        cfg: &MixedConfig,
        laws: &MixedLawFactories,
        level: f64,
    ) -> Result<(), MixedError> {
        self.restart()?;
        let guards = cfg.law_guards.clone().unwrap_or_default();
        let loop_s = cfg.blocks_per_window as f64 * self.hop as f64 / self.fs;
        let budget_s = guards.floor.budget_fraction * loop_s;
        let setup = hertzforge_random::LawSetup {
            target_psd: &self.target,
            band: self.band,
            df_hz: self.df,
            hinv: &laws.floor_hinv,
            control_channels: cfg.control.channels.len(),
            call_budget_s: budget_s,
        };
        let mut law = laws
            .floor
            .build(&setup)
            .map_err(|e| MixedError::Law(e.to_string()))?;
        let zeros = vec![0.0; self.lines];
        let seed = LoopInputs {
            target_psd: &self.target,
            measured_psd: &zeros,
            h_mag2: &self.ident.h_mag2,
            coherence: &self.ident.coherence,
            current_level_db: 0.0,
            df_hz: self.df,
            notched_bins: &[],
            band: self.band,
            dof: 0.0,
        };
        let budget = Budget {
            clock: self.floor_clock.as_ref(),
            seconds: budget_s,
        };
        let out = guarded_call(law.as_mut(), budget, self.lines, self.band, |l| {
            l.init(&seed)
        })
        .map_err(|m| MixedError::Law(m.words()))?;
        self.floor_tally.calls += 1;
        self.floor_drive = out.drive_psd;
        self.floor_law = Some(law);

        // Tones.
        // The grid a tone law is given starts at the first line above 0 Hz, as
        // the Sine core gives its own (a law host refuses a grid that is not
        // positive and ascending, so a 0 Hz line refused every law file).
        let first = 1.min(self.lines);
        let freqs: Vec<f64> = (first..self.lines).map(|k| k as f64 * self.df).collect();
        let mut f0 = Vec::new();
        let mut tones = Vec::new();
        let now = self.step.clock.now().to_vec();
        for (i, p) in cfg.parts.iter().enumerate() {
            let PartBody::Tone(t) = &p.body else { continue };
            let f = now[i].freq_hz.unwrap_or(0.0);
            f0.push(f);
            let amp = AmpModel::from(&t.amplitude);
            let tgt: Vec<f64> = freqs.iter().map(|&f| amp.at(f)).collect();
            let id = Identification {
                frequency_hz: &freqs,
                gain: &self.ident.gain[first.min(self.ident.gain.len())..],
                coherence: &self.ident.coherence[first.min(self.ident.coherence.len())..],
                trusted: &self.ident.trusted[first.min(self.ident.trusted.len())..],
                target: &tgt,
            };
            let tick_s = cfg
                .control_tick_s
                .unwrap_or(hertzforge_sine::law::DEFAULT_TICK_S);
            let setup = SineLawSetup {
                control_channels: cfg.control.channels.len(),
                tick_s,
                band_hz: cfg.floor().and_then(|f| f.band()).unwrap_or((0.0, 0.0)),
                drive_ceiling: cfg.limits.drive_vpk_ceiling,
                identification: id,
                call_budget_s: guards.tones.call_budget_s(tick_s),
            };
            let law = laws
                .tones
                .build(&setup)
                .map_err(|e| MixedError::Law(format!("tone {}: {e}", p.id)))?;
            let declared = laws.tones.describe().diagnostics.len();
            tones.push(ToneCtl {
                part: i,
                amp,
                law,
                values: LawValues::new(declared),
                engaged: false,
                ticks: 0,
                misses_in_row: 0,
                tally: RoleTally::default(),
                cap_bound_s: 0.0,
                last_freq: f,
                rate: 0.0,
                quiet_frames: 0,
                measured: None,
                error_db: None,
                usable: false,
                read_once: false,
                shown_db: 0.0,
                limiting: false,
                judging: None,
                avg_ms: 0.0,
                avg_s: 0.0,
                judged_db: None,
                alarm: false,
                abort_s: 0.0,
                alarm_db: t.alarm_db,
                abort_db: t.abort_db,
                switched_on: p.enabled,
            });
        }
        let bank = ToneBank::new(
            self.fs,
            &f0,
            cfg.control.channels.len(),
            cfg.tracking_hz,
            self.ident.lag,
        );
        self.step.set_tones(bank, tones);
        self.step.plant = PlantGrid {
            f0: 0.0,
            df: self.df,
            gain: self.ident.gain.clone(),
            trusted: self.ident.trusted.clone(),
            floor: self.target.clone(),
        };
        // Bands.
        self.bands = cfg
            .parts
            .iter()
            .enumerate()
            .filter_map(|(i, p)| match &p.body {
                PartBody::Narrowband(b) => Some(BandCtl {
                    part: i,
                    level_g2: b.level_g2_hz,
                    width: b.width,
                    servo: BandServo::default(),
                    loop_level: BandLevel::new(0),
                    level: BandLevel::new(self.est_depth()),
                    alarm_db: b.alarm_db,
                    abort_db: b.abort_db,
                    scale: Ramp::at(0.0),
                    switched_on: p.enabled,
                    alarm: false,
                    error_db: None,
                    center_now: now[i].freq_hz.unwrap_or(0.0),
                    capped: 0,
                    limiting: false,
                }),
                _ => None,
            })
            .collect();
        // Start: a fresh analysis, the floor alone ramping in.
        for r in self.res_buf.iter_mut().chain(self.ctl_buf.iter_mut()) {
            r.clear();
        }
        self.gain_buf.clear();
        clear(&mut self.step.records);
        self.step.ring.clear();
        self.segs.clear();
        self.ring_end = self.step.frames;
        self.resp_at = self.step.frames;
        self.step.mode = StepMode::Run;
        self.step.engaged_frame = self.step.frames;
        self.step.level = Ramp::at(level);
        self.step.random = Ramp::at(0.0);
        self.step.random.go(1.0, cfg.part_ramp_s * self.fs);
        Ok(())
    }

    fn est_depth(&self) -> usize {
        64
    }

    // ---- between chunks ---------------------------------------------------------

    /// Keep the ring `frames` ahead of play.
    pub(crate) fn top_up(&mut self, cfg: &MixedConfig, frames: usize) {
        let ramp_hops = (cfg.part_ramp_s * self.fs / self.hop as f64).max(1.0);
        let mut psd = vec![0.0; self.lines];
        let mut w = vec![0.0; self.lines];
        while self.step.ring.len() < frames {
            // Where each band is when this hop plays.
            let ahead = (self.step.ring.len() + self.hop / 2) as f64 / self.fs;
            let mut clock = self.step.clock.clone();
            if self.step.clock_running {
                clock.tick(ahead);
            }
            psd.copy_from_slice(&self.floor_drive);
            let mut meta = Vec::with_capacity(self.bands.len());
            for b in self.bands.iter_mut() {
                let now = clock.now()[b.part];
                let want = self.step.clock_running && now.on && b.switched_on;
                let target = if want { 1.0 } else { 0.0 };
                if b.scale.target() != target {
                    b.scale.go(target, ramp_hops);
                }
                let s = b.scale.next();
                let c = now.freq_hz.unwrap_or(b.center_now);
                if s > 0.0 {
                    let (lo, hi) = b.width.edges_at(c);
                    band_weights(0.0, self.df, lo, hi, &mut w);
                    add_band_drive(
                        &w,
                        b.level_g2,
                        b.servo.gain() * s * s,
                        &self.ident.h_mag2,
                        &mut psd,
                    );
                }
                meta.push((c, s));
            }
            // Limit channels lower the random part only (a tone is held by
            // the channel's abort, the kinematic monitor and the drive
            // ceiling): the tightest predicted drive ceiling per line, at the
            // level the hop will play at.
            let lev = self.step.level.value().max(self.step.level.target());
            if !self.limits.is_empty() && lev > 0.0 {
                #[allow(clippy::needless_range_loop)]
                for k in 0..self.lines {
                    let mut ceil = f64::INFINITY;
                    for l in &self.limits {
                        if l.h_mag2[k] > 1e-12 && l.ceiling[k].is_finite() {
                            ceil = ceil.min(l.ceiling[k] / l.h_mag2[k] / (lev * lev));
                        }
                    }
                    self.notched[k] = psd[k] > ceil;
                    if self.notched[k] {
                        psd[k] = ceil;
                    }
                }
            }
            // The random part's rms ceiling.
            self.rms_held = false;
            if let Some(vmax) = cfg.limits.drive_ceiling_vrms {
                let rms = (psd.iter().sum::<f64>() * self.df).sqrt() * lev;
                if rms > vmax && rms > 0.0 {
                    self.rms_held = true;
                    let k = (vmax / rms).powi(2);
                    psd.iter_mut().for_each(|p| *p *= k);
                }
            }
            if let Some(clip) = cfg.limits.drive_clip_sigma {
                let ms: f64 = psd.iter().sum::<f64>() * self.df;
                let lim = clip * ms.sqrt();
                let x = self.synth.next_frame(&psd);
                self.step.ring.extend(x.iter().map(|v| v.clamp(-lim, lim)));
            } else {
                let x = self.synth.next_frame(&psd);
                self.step.ring.extend(x);
            }
            self.segs.push_back(SegMeta {
                start: self.ring_end,
                len: self.hop,
                bands: meta,
            });
            self.ring_end += self.hop as u64;
        }
    }

    /// The bands at drive frame `d`: `(center, scale)` each.
    fn bands_at(&self, d: u64) -> Option<&[(f64, f64)]> {
        self.segs
            .iter()
            .find(|s| d >= s.start && d < s.start + s.len as u64)
            .map(|s| s.bands.as_slice())
    }

    /// Take the step's records and analyse every whole frame; run the floor
    /// loop when its frames are in. Returns a trip, if one is due.
    pub(crate) fn analyse(
        &mut self,
        cfg: &MixedConfig,
        weights: &[f64],
    ) -> Option<(MixedTripKind, Option<usize>, bool)> {
        let r = &mut self.step.records;
        for c in 0..self.res_buf.len() {
            self.res_buf[c].extend(r.residual[c].drain(..));
            self.ctl_buf[c].extend(r.control[c].drain(..));
        }
        self.gain_buf.extend(r.gain.drain(..));
        for (l, rec) in self.limits.iter_mut().zip(r.limits.iter_mut()) {
            l.buf.extend(rec.drain(..));
        }
        let mut lp = vec![0.0; self.lines];
        let mut lframe = vec![0.0; self.nfft];
        for l in self.limits.iter_mut() {
            while l.buf.len() >= self.nfft {
                for (k, v) in l.buf.iter().take(self.nfft).enumerate() {
                    lframe[k] = *v;
                }
                periodogram(&lframe, &self.win, self.fs, &mut self.scratch, &mut lp);
                for (s, p) in l.sum.iter_mut().zip(&lp) {
                    *s += p;
                }
                l.frames += 1;
                l.buf.drain(..self.hop);
            }
        }
        let lag = self.step.lag() as u64;
        let mut trip = None;
        let n_ctrl = self.res_buf.len();
        let mut frame = vec![0.0; self.nfft];
        let mut per_res: Vec<Vec<f64>> = vec![vec![0.0; self.lines]; n_ctrl];
        let mut per_ctl: Vec<Vec<f64>> = vec![vec![0.0; self.lines]; n_ctrl];
        let mut mask = vec![false; self.lines];
        let mut reference = vec![0.0; self.lines];
        while self.gain_buf.len() >= self.nfft {
            // Frame statistics of the gain that played it.
            let (mut g2, mut gmin, mut gmax) = (0.0, f64::INFINITY, 0.0f64);
            for &g in self.gain_buf.iter().take(self.nfft) {
                g2 += g * g;
                gmin = gmin.min(g);
                gmax = gmax.max(g);
            }
            g2 /= self.nfft as f64;
            for c in 0..n_ctrl {
                for (k, v) in self.res_buf[c].iter().take(self.nfft).enumerate() {
                    frame[k] = *v;
                }
                periodogram(
                    &frame,
                    &self.win,
                    self.fs,
                    &mut self.scratch,
                    &mut per_res[c],
                );
                for (k, v) in self.ctl_buf[c].iter().take(self.nfft).enumerate() {
                    frame[k] = *v;
                }
                periodogram(
                    &frame,
                    &self.win,
                    self.fs,
                    &mut self.scratch,
                    &mut per_ctl[c],
                );
            }
            let comp_res = composite(&per_res, weights);
            self.last_control = per_ctl.clone();
            self.last_residual = per_res.clone();
            self.last_composite = composite(&per_ctl, weights);
            self.last_residual_composite = comp_res.clone();
            let mid = self.resp_at + (self.nfft / 2) as u64;
            let steady = gmax > 0.0 && (gmax - gmin) <= GAIN_STEADY * gmax;
            if steady {
                let norm: Vec<f64> = comp_res.iter().map(|p| p / g2).collect();
                // The mask: every tone that has any drive (or whose tracker
                // has not let it go), over the span it can have covered while
                // this frame played, and bands with their guard.
                let tones = self.tone_spans(cfg, weights);
                let bands_now = self.bands_at(mid.saturating_sub(lag)).map(|b| b.to_vec());
                let edges: Vec<(f64, f64)> = match &bands_now {
                    Some(b) => self
                        .bands
                        .iter()
                        .zip(b)
                        .filter(|(_, (_, s))| *s > 0.0)
                        .map(|(bc, (c, _))| bc.width.edges_at(*c))
                        .collect(),
                    None => Vec::new(),
                };
                exclusion_mask(self.df, &[], 0.0, &edges, BAND_GUARD_LINES, &mut mask);
                for &(lo, hi) in &tones {
                    crate::floor::mark_span(self.df, lo, hi, &mut mask);
                }
                for (k, m) in mask.iter_mut().enumerate() {
                    if k < self.band.0 || k >= self.band.1 {
                        *m = true;
                    }
                }
                self.est.add_frame(&norm, &mask);
                self.last_excluded.copy_from_slice(&mask);
                if let Some(b) = &bands_now {
                    for (bc, &(c, s)) in self.bands.iter_mut().zip(b.iter()) {
                        bc.center_now = c;
                        if s < 0.999 {
                            continue;
                        }
                        let w = bc.width.edges_at(c);
                        let lines = inner_lines(0.0, self.df, c, w.1 - w.0, self.lines);
                        for (r, t) in reference[lines.0..lines.1]
                            .iter_mut()
                            .zip(&self.target[lines.0..lines.1])
                        {
                            *r = t + bc.level_g2;
                        }
                        bc.loop_level.add_frame(&norm, &reference, lines);
                        bc.level.add_frame(&norm, &reference, lines);
                    }
                }
                self.frames_since_loop += 1;
                self.frames_total += 1;
            }
            // Next frame: drop a hop.
            for c in 0..n_ctrl {
                self.res_buf[c].drain(..self.hop);
                self.ctl_buf[c].drain(..self.hop);
            }
            self.gain_buf.drain(..self.hop);
            self.resp_at += self.hop as u64;
            while self
                .segs
                .front()
                .is_some_and(|s| s.start + (s.len as u64) + lag + (self.nfft as u64) < self.resp_at)
            {
                self.segs.pop_front();
            }
            if self.frames_since_loop >= cfg.blocks_per_window {
                self.frames_since_loop = 0;
                if let Some(t) = self.floor_loop(cfg) {
                    trip = trip.or(Some(t));
                }
            }
        }
        trip
    }

    /// One floor loop: the law, the bands' servo, the judgements.
    fn floor_loop(&mut self, cfg: &MixedConfig) -> Option<(MixedTripKind, Option<usize>, bool)> {
        self.loops += 1;
        let mut measured = vec![0.0; self.lines];
        let clean = self.est.estimate(&self.target, self.band, &mut measured);
        self.floor_dof = self.est.min_dof(self.band);
        self.floor_dof_typical = self.est.typical_dof(self.band);
        self.lines_out = (self.band.1 - self.band.0).saturating_sub(clean);
        let guards = cfg.law_guards.clone().unwrap_or_default().floor;
        let loop_s = cfg.blocks_per_window as f64 * self.hop as f64 / self.fs;
        let inputs = LoopInputs {
            target_psd: &self.target,
            measured_psd: &measured,
            h_mag2: &self.ident.h_mag2,
            coherence: &self.ident.coherence,
            current_level_db: 0.0,
            df_hz: self.df,
            notched_bins: &self.notched,
            band: self.band,
            dof: self.floor_dof,
        };
        let mut trip = None;
        if let Some(law) = self.floor_law.as_mut() {
            let budget = Budget {
                clock: self.floor_clock.as_ref(),
                seconds: guards.budget_fraction * loop_s,
            };
            self.floor_tally.calls += 1;
            let rms = |d: &[f64]| (d.iter().sum::<f64>() * self.df).sqrt();
            let drive_in = rms(&self.floor_drive);
            let t0 = self.floor_clock.now_s();
            let result = guarded_call(law.as_mut(), budget, self.lines, self.band, |l| {
                l.iterate(&inputs)
            });
            let took = (self.floor_clock.now_s() - t0).max(0.0);
            self.floor_tally.worst_call_us = self.floor_tally.worst_call_us.max(took * 1e6);
            self.floor_drive_in_last = drive_in;
            self.floor_took_last = took * 1e6;
            self.floor_missed_last = result.is_err();
            match result {
                Ok(mut out) => {
                    self.floor_misses = 0;
                    let rep = cap_change(
                        &mut out.drive_psd,
                        &self.floor_drive,
                        self.band,
                        guards.max_change_db,
                    );
                    if !rep.bound.is_empty() {
                        self.floor_tally.capped += 1;
                    }
                    // While the random part's rms ceiling holds the drive,
                    // the floor reads low and the law's asks against the
                    // hold are not a diverging loop (the floor row says it
                    // is limiting; its rms alarm and abort judge it).
                    if out.diagnostics.divergent_bins.is_empty() || self.rms_held {
                        self.divergent_loops = 0;
                    } else {
                        self.divergent_loops += 1;
                        if self.divergent_loops >= cfg.limits.divergence_loops {
                            trip = Some((MixedTripKind::Divergence, None, true));
                        }
                    }
                    self.floor_drive = out.drive_psd;
                }
                Err(miss) => {
                    self.floor_tally.misses += 1;
                    self.floor_misses += 1;
                    if self.floor_misses >= guards.max_misses {
                        trip = Some((
                            MixedTripKind::LawFault {
                                role: LawRole::Floor,
                                reason: miss.reason.as_str().to_owned(),
                            },
                            None,
                            false,
                        ));
                    }
                }
            }
        }
        if self.floor_log.len() < 4096 {
            let rms = (self.floor_drive.iter().sum::<f64>() * self.df).sqrt();
            self.floor_log.push(super::law_records::MixedLawRecord {
                t_s: self.step.frames.saturating_sub(self.step.engaged_frame) as f64 / self.fs,
                drive_in: self.floor_drive_in_last,
                drive_out: rms,
                took_us: self.floor_took_last as f32,
                role: super::law_records::RecordRole::Floor,
                tone: 0,
                miss: u8::from(self.floor_missed_last),
                flags: 0,
            });
        }
        // The floor's judgement on clean lines.
        let floor = cfg.floor().expect("validated");
        let (mut ms_m, mut ms_t, mut alarm, mut abort) = (0.0, 0.0, 0usize, 0usize);
        // The rms abort's own sums: lines that each hold the gate's DOF.
        let (mut qm, mut qt, mut qn) = (0.0, 0.0, 0usize);
        for k in self.band.0..self.band.1 {
            let Some(m) = self.est.line(k) else { continue };
            ms_m += m;
            ms_t += self.target[k];
            if self.est.dof(k) < MIN_ENVELOPE_DOF || self.target[k] <= 0.0 || m <= 0.0 {
                continue;
            }
            qm += m;
            qt += self.target[k];
            qn += 1;
            let e = 10.0 * (m / self.target[k]).log10();
            let r = self.relax_db;
            if e < -(floor.alarm_db.0 + r) || e > floor.alarm_db.1 + r {
                alarm += 1;
            }
            if e < -(floor.abort_db.0 + r) || e > floor.abort_db.1 + r {
                abort += 1;
            }
        }
        self.measured_store(measured);
        self.alarm_lines = alarm;
        self.abort_lines = abort;
        if ms_t > 0.0 && ms_m > 0.0 {
            self.floor_rms_err_db = Some(10.0 * (ms_m / ms_t).log10());
            self.floor_rms_g = Some((ms_m * self.df).sqrt());
        }
        // The floor's rms against the tighter side of its abort band, judged
        // on the lines that each hold the gate's DOF, once most of the band
        // does (one starved line cannot disarm it).
        let in_band = self.band.1.saturating_sub(self.band.0);
        let armed = in_band > 0 && qn as f64 >= RMS_ABORT_SHARE * in_band as f64;
        self.rms_judged_lines = if armed { qn } else { 0 };
        let judged_db = (armed && qm > 0.0 && qt > 0.0).then(|| 10.0 * (qm / qt).log10());
        if trip.is_none()
            && self.relax_db == 0.0
            && judged_db.is_some_and(|e| e.abs() > floor.abort_db.0.min(floor.abort_db.1))
        {
            trip = Some((
                MixedTripKind::FloorRmsAbort {
                    error_db: judged_db.unwrap_or(0.0),
                },
                None,
                true,
            ));
        }
        // Each limit channel against its ceiling plus the floor's abort margin.
        let margin = 10f64.powf(floor.abort_db.1 / 10.0);
        for l in self.limits.iter_mut() {
            if l.frames == 0 {
                continue;
            }
            let (mut over, mut worst) = (0usize, (0.0, 0.0, 0.0f64));
            for k in 0..self.lines {
                let m = l.sum[k] / l.frames as f64;
                if l.ceiling[k].is_finite() && m > l.ceiling[k] * margin {
                    over += 1;
                    if m / l.ceiling[k] > worst.2 {
                        worst = (m, l.ceiling[k], m / l.ceiling[k]);
                    }
                }
            }
            l.sum.iter_mut().for_each(|s| *s = 0.0);
            l.frames = 0;
            if over > cfg.limits.max_lines_out && trip.is_none() {
                trip = Some((
                    MixedTripKind::LimitChannel {
                        channel_index: l.channel_index,
                        measured: worst.0,
                        limit: worst.1,
                    },
                    None,
                    true,
                ));
            }
        }
        if abort > cfg.limits.max_lines_out && trip.is_none() {
            trip = Some((MixedTripKind::FloorAbort { lines_out: abort }, None, true));
        }
        // Bands.
        for bc in self.bands.iter_mut() {
            // A band the random part's rms ceiling or a limit channel holds
            // reads low; its servo may lower its gain but does not raise it
            // against the hold (it would wind up and step the band past its
            // level when the hold lets go).
            let (lo, hi) = bc.width.edges_at(bc.center_now);
            let (k0, k1) = (
                ((lo / self.df).floor().max(0.0) as usize).min(self.lines),
                ((hi / self.df).ceil().max(0.0) as usize + 1).min(self.lines),
            );
            bc.limiting = self.rms_held || self.notched[k0..k1].iter().any(|n| *n);
            if let Some(m) = bc.loop_level.measured() {
                let floor_ref = bc.loop_level.reference().unwrap_or(0.0) - bc.level_g2;
                let before = bc.servo.gain_db;
                if bc.servo.update(m - floor_ref, bc.level_g2) && !bc.limiting {
                    bc.capped += 1;
                }
                if bc.limiting && bc.servo.gain_db > before {
                    bc.servo.gain_db = before;
                }
            }
            bc.loop_level.reset();
            bc.error_db = bc.level.error_db();
            if let Some(e) = bc.error_db {
                let dof = bc.level.dof();
                let al = widened_db(bc.alarm_db, dof);
                let ab = widened_db(bc.abort_db, dof);
                bc.alarm = e < -al.0 || e > al.1;
                if (e < -ab.0 || e > ab.1) && trip.is_none() && dof >= MIN_ENVELOPE_DOF {
                    trip = Some((
                        MixedTripKind::BandAbort { error_db: e },
                        Some(bc.part),
                        true,
                    ));
                }
            }
        }
        trip
    }

    fn measured_store(&mut self, m: Vec<f64>) {
        self.last_measured = m;
    }

    /// The step's trip, taken.
    pub(crate) fn take_step_trip(&mut self) -> Option<StepTrip> {
        self.step.trip.take()
    }

    /// The frequency spans the floor leaves out this frame for the tones,
    /// `(lo, hi)` Hz.
    ///
    /// Every tone that is on, dwelling or sweeping, settled or not, and every
    /// tone switched off whose tracker still reads it (its passed waveform
    /// decays over the settle time). The span is where the tone can have been
    /// while the frame played: its frequency now less its rate times the
    /// frame, the lag and a batch either side. Its half-width is the larger of
    /// `exclusion_hz` and the lines the whole tone's Hann leakage needs to
    /// fall under a tenth of the floor (the brief's 3.2 inequality,
    /// `x (x^2 - 1) >= sqrt(A^2 / (3 eps G df)) / pi`), at the tone's
    /// full-level target over the floor's full-level target (the ratio does
    /// not move with the level). At the usual ratios (A^2 / G of 100 Hz at
    /// 1 Hz lines) that is `exclusion_hz` itself.
    fn tone_spans(&self, cfg: &MixedConfig, weights: &[f64]) -> Vec<(f64, f64)> {
        let behind = (2.0 * self.nfft as f64 + self.step.lag() as f64 + self.batch_frames() as f64)
            / self.fs;
        let mut out = Vec::new();
        for (j, t) in self.step.tones.iter().enumerate() {
            let target = t.amp.at(t.last_freq);
            let (a, _) = self.step.bank.composite(j, weights);
            let voice = self.step.bank.voice(j);
            if voice.scale() <= 0.0 && a <= 0.01 * target {
                continue;
            }
            // Never narrower than the whole tone's Hann leakage needs: the
            // subtraction is not credited (measured at A^2 / G = 3e4 Hz, a
            // settled dwelling tone still read +3 to +5 dB three and four lines
            // out with only `exclusion_hz` = 2 lines left out).
            let g = self.step.plant.floor_at(t.last_freq).max(1e-300);
            let half = cfg
                .exclusion_hz
                .max(leak_lines(target, g, self.df) * self.df);
            let reach = t.rate.abs() * behind;
            out.push((t.last_freq - reach - half, t.last_freq + reach + half));
        }
        out
    }

    /// The longest the floor can need to reach its degrees of freedom on
    /// this grid, seconds: the frames `control_dof` needs, a frame, and a loop.
    pub(crate) fn equalize_s(&self, cfg: &MixedConfig) -> f64 {
        let frames = (cfg.control_dof / dof_per_frame()).ceil();
        let hop = self.hop as f64 / self.fs;
        frames * hop + self.nfft as f64 / self.fs + cfg.blocks_per_window as f64 * hop
    }

    /// The engine's health counters.
    pub(crate) fn engine_health(&self) -> Option<hertzforge_engine::EngineHealth> {
        self.engine.as_ref().map(Engine::health)
    }

    /// Control channels per tapped frame.
    pub(crate) fn tap_stride(&self) -> usize {
        self.res_buf.len().max(1)
    }

    /// Copy the step's new control samples into the tap (bounded to ten
    /// seconds; older samples are dropped).
    pub(crate) fn tap_records(&mut self) {
        let r = &self.step.records;
        let n = r.control.first().map_or(0, Vec::len);
        let from = self.tap.len();
        for k in 0..n {
            for c in &r.control {
                self.tap.push(c[k] as f32);
            }
        }
        if let Some(cap) = self.capture.as_ref() {
            cap.push(&self.tap[from..], n);
        }
        let cap = (10.0 * self.fs) as usize * self.tap_stride();
        if self.tap.len() > cap {
            let cut = self.tap.len() - cap;
            self.tap.drain(..cut);
        }
    }

    /// The phase the parts are in, from the ramps.
    pub(crate) fn parts_settled(&self) -> bool {
        let tones = (0..self.step.tones.len()).all(|j| self.step.bank.voice(j).ramp_settled());
        let bands = self.bands.iter().all(|b| b.scale.settled());
        tones && bands && self.step.random.settled()
    }
}

/// The Hann leakage of a tone of peak `a` stays under a tenth of a floor of
/// density `g` beyond this many lines of `df` (the brief's 3.2 inequality).
pub(crate) fn leak_lines(a: f64, g: f64, df: f64) -> f64 {
    let rhs = (a * a / (3.0 * 0.1 * g * df)).sqrt() / std::f64::consts::PI;
    let mut x: f64 = 2.0;
    while x * (x * x - 1.0) < rhs && x < 1000.0 {
        x += 0.25;
    }
    x
}

/// Weighted average of per-channel spectra.
fn composite(per: &[Vec<f64>], weights: &[f64]) -> Vec<f64> {
    let n = per.first().map_or(0, Vec::len);
    let den: f64 = weights.iter().sum::<f64>().max(1e-300);
    (0..n)
        .map(|k| per.iter().zip(weights).map(|(p, w)| p[k] * w).sum::<f64>() / den)
        .collect()
}

fn clear(r: &mut super::step::Records) {
    r.rand_sumsq = 0.0;
    r.rand_n = 0;
    for v in r
        .control
        .iter_mut()
        .chain(r.residual.iter_mut())
        .chain(r.limits.iter_mut())
    {
        v.clear();
    }
    r.gain.clear();
    r.drive.clear();
    r.peak = 0.0;
    r.sumsq = 0.0;
    r.n = 0;
}

/// The plant from the identification: the lag by cross-correlation, then
/// per channel `H1 = Sxy / Sxx` and coherence over Hann frames at half
/// overlap; the composite `|H|^2`, `|H|` and coherence weighted as the
/// control channels are.
#[allow(clippy::too_many_arguments)]
fn estimate_plant(
    drive: &[f64],
    resp: &[Vec<f64>],
    weights: &[f64],
    fs: f64,
    nfft: usize,
    band: (usize, usize),
    max_lag: usize,
    trust: (f64, f64),
) -> Ident {
    let lines = nfft / 2 + 1;
    // The lag: the largest cross-correlation of the drive with the first
    // control channel, over a second of record.
    let n = drive.len().saturating_sub(max_lag).min(fs as usize);
    let mut best = (0usize, f64::MIN);
    for l in 0..max_lag.min(drive.len()) {
        let mut s = 0.0;
        for i in 0..n {
            if i + l < resp[0].len() {
                s += drive[i] * resp[0][i + l];
            }
        }
        if s > best.1 {
            best = (l, s);
        }
    }
    let lag = best.0;
    let win = hann(nfft);
    let hop = nfft / 2;
    let mut per_h2 = vec![vec![0.0; lines]; resp.len()];
    let mut per_g = vec![vec![0.0; lines]; resp.len()];
    let mut per_coh = vec![vec![0.0; lines]; resp.len()];
    let mut per_h = vec![vec![(0.0, 0.0); lines]; resp.len()];
    for (c, y) in resp.iter().enumerate() {
        let (mut sxx, mut syy) = (vec![0.0; lines], vec![0.0; lines]);
        let mut sxy = vec![(0.0, 0.0); lines];
        let mut start = 0;
        let mut fx = vec![(0.0, 0.0); nfft];
        let mut fy = vec![(0.0, 0.0); nfft];
        while start + nfft + lag <= y.len().min(drive.len() + lag) && start + nfft <= drive.len() {
            for i in 0..nfft {
                fx[i] = (drive[start + i] * win[i], 0.0);
                fy[i] = (y[start + lag + i] * win[i], 0.0);
            }
            hertzforge_dsp::fft::fft_in_place(&mut fx);
            hertzforge_dsp::fft::fft_in_place(&mut fy);
            for k in 0..lines {
                let (a, b) = fx[k];
                let (c2, d) = fy[k];
                sxx[k] += a * a + b * b;
                syy[k] += c2 * c2 + d * d;
                // conj(X) Y
                sxy[k].0 += a * c2 + b * d;
                sxy[k].1 += a * d - b * c2;
            }
            start += hop;
        }
        for k in 0..lines {
            let m2 = sxy[k].0 * sxy[k].0 + sxy[k].1 * sxy[k].1;
            if sxx[k] > 0.0 {
                per_h2[c][k] = m2 / (sxx[k] * sxx[k]);
                per_g[c][k] = per_h2[c][k].sqrt();
                // The transform above has the positive exponent, so its
                // conj(X) Y is the conjugate of the standard one: conjugated
                // once here, the file's convention (a delay lags).
                per_h[c][k] = (sxy[k].0 / sxx[k], -sxy[k].1 / sxx[k]);
            }
            if sxx[k] > 0.0 && syy[k] > 0.0 {
                per_coh[c][k] = (m2 / (sxx[k] * syy[k])).clamp(0.0, 1.0);
            }
        }
    }
    let h_mag2 = composite(&per_h2, weights);
    let gain = composite(&per_g, weights);
    let coherence = composite(&per_coh, weights);
    let (lo, hi) = band;
    let peak = gain[lo..hi.max(lo)].iter().copied().fold(0.0, f64::max);
    let trusted: Vec<bool> = (0..lines)
        .map(|k| k >= lo && k < hi && coherence[k] >= trust.0 && gain[k] >= trust.1 * peak)
        .collect();
    let in_band = (hi - lo).max(1) as f64;
    let band_coherence = coherence[lo..hi].iter().sum::<f64>() / in_band;
    let trusted_frac = trusted[lo..hi].iter().filter(|t| **t).count() as f64 / in_band;
    Ident {
        h_mag2,
        gain,
        coherence,
        trusted,
        lag,
        band_coherence,
        trusted_frac,
        h: per_h,
        coherence_by: per_coh,
    }
}

#[cfg(test)]
mod mixed_frf_convention_tests {
    use super::estimate_plant;

    /// The identification's complex transfer function is kept in the file's
    /// convention, `conj(X) Y / |X|^2` with the standard (negative-exponent)
    /// transform, so it is `Y/X` and a delay lags. A resonance behind a pure delay
    /// of three frames is identified with the lag search held to zero (so the
    /// delay stays in the record) and compared with the plant line by line. Red if
    /// the conjugation in `estimate_plant` is removed (every phase reversed).
    #[test]
    fn a_delay_lags_in_the_mixed_identification() {
        use std::f64::consts::PI;
        let fs = 1024.0;
        let nfft = 256;
        let delay = 3usize;
        let (r, w0) = (0.97f64, 2.0 * PI * 100.0 / fs);
        let (a1, a2) = (-2.0 * r * w0.cos(), r * r);
        let b0 = 1.0 + a1 + a2;
        let n = 1 << 16;
        let mut seed = 0x1234_5678_u64;
        let x: Vec<f64> = (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            })
            .collect();
        let mut y = vec![0.0; n];
        for k in 0..n {
            let xd = if k >= delay { x[k - delay] } else { 0.0 };
            let y1 = if k >= 1 { y[k - 1] } else { 0.0 };
            let y2 = if k >= 2 { y[k - 2] } else { 0.0 };
            y[k] = b0 * xd - a1 * y1 - a2 * y2;
        }
        let ident = estimate_plant(&x, &[y], &[1.0], fs, nfft, (5, 100), 1, (0.5, 0.05));
        assert_eq!(ident.lag, 0);
        let (h, coh) = (&ident.h[0], &ident.coherence_by[0]);
        let mut checked = 0;
        for k in 5..100 {
            let w = 2.0 * PI * k as f64 / nfft as f64;
            let (dr, di) = (
                1.0 + a1 * w.cos() + a2 * (2.0 * w).cos(),
                -a1 * w.sin() - a2 * (2.0 * w).sin(),
            );
            let (nr, ni) = (
                b0 * (w * delay as f64).cos(),
                -b0 * (w * delay as f64).sin(),
            );
            let den = dr * dr + di * di;
            let (tr, ti) = ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den);
            if coh[k] < 0.95 {
                continue;
            }
            let (er, ei) = h[k];
            let phase = (ei * tr - er * ti).atan2(er * tr + ei * ti);
            assert!(phase.abs() < 0.15, "line {k}: phase off by {phase:.3} rad");
            let ratio = er.hypot(ei) / tr.hypot(ti);
            assert!(
                (ratio - 1.0).abs() < 0.15,
                "line {k}: magnitude ratio {ratio:.3}"
            );
            checked += 1;
        }
        assert!(checked > 60, "only {checked} lines were coherent");
    }
}
