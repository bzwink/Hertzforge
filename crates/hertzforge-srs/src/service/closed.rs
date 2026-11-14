//! The law seam of an SRS run and its closed loop (`SrsTestConfig::control`
//! set).
//!
//! # The seam, on every run
//!
//! The run calls its law through [`SrsShockService`]'s core, with or without
//! `control`: once after identification, before the arm-time notch (the
//! seed), and once after each fired, measured and judged pulse. The law
//! returns one gain per component; the core caps each by what the limit
//! channels' predictions leave (`min(law gain, cap)`, the cap damped up and
//! cut down as [`crate::law::predictive_cap`] says) and applies it to the
//! table. Without `control` the built-in is built from the configuration's
//! `synthesis.max_gain_db_per_iter`, with no time budget and no gain cap, so
//! the run is the one it was before the seam.
//!
//! # The closed loop
//!
//! With `control` the drive is in volts. Identification plays the synthesis
//! at its level with its peak held at the volts cap; a mean in-band coherence
//! under the floor holds the run. The core always holds for review after
//! identification; the caller proceeds. At proceed the law is seeded from the
//! identified gain at each component, the notch is planned, and a predicted
//! drive over the abort at the highest level the run is scheduled to reach
//! ends the run before any pulse. Before each pulse the core waits out the
//! time between pulses, then checks the drive in volts (scaled to the
//! ceiling, refused over the abort) and the predicted kinematics. Each law
//! call is timed against its budget and contained; a late, failed, panicked or
//! refused call is a miss and the next pulse plays the table in force; misses
//! in a row up to the limit end the run `law_fault`. The ladder advances on
//! the schedule's rule and the iteration limits end the run `not_converged`.

use super::pacer::PulsePacer;
use super::report::{
    ConvergenceLimit, DrivePrediction, EventKind, HeldReason, LawReport, LawStatus, PulseLawCall,
    RunState, TripKind, TripRecord,
};
use super::{ServiceError, SrsShockService};
use crate::control::{SrsControl, SrsInputs, SrsLaw, SrsLawFactory, SrsLawSetup, SrsOutputs};
use crate::law::LawLimitStatus;
use crate::limit::predict_response_waveform;
use hertzforge_shock_common::law_guard::{
    LawCall, LawCallRecord, LawClock, LawDescriptor, LawFault, LawRecordRing, LawValue,
    MissCounter, OutputRule, MAX_LAW_VALUES,
};
use hertzforge_shock_common::predict_peak_accel_g;
use hertzforge_shock_common::{Prepared, ShotHooks};
use serde::{Deserialize, Serialize};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How often the wait between pulses reads the cancel flag, seconds.
const CANCEL_POLL_S: f64 = 0.05;

/// One SRS law call's record, as `law_pulses` stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SrsCallPayload {
    /// The target the law saw, EU, on the band grid.
    pub target: Vec<f32>,
    /// The measured SRS the law saw, EU (zeros at the seed).
    pub measured: Vec<f32>,
    /// The scale the last pulse was commanded at.
    pub commanded_scale: f64,
    /// The gains the law returned (empty when the call gave none).
    pub gains: Vec<f64>,
    /// The gains the core applied after its caps (empty on a miss).
    pub applied: Vec<f64>,
    /// The values the law reported.
    pub values: Vec<LawValue>,
    /// Components the limit cap held back on this call's gains.
    pub limited_components: usize,
}

/// One SRS law call's record.
pub type SrsCallRecord = LawCallRecord<SrsCallPayload>;

/// The law's counts over a run.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct LawStats {
    pub calls: u64,
    pub misses: u64,
    pub refused: u64,
    pub limited_components: u64,
    pub worst_call_s: f64,
}

/// The law and its bookkeeping for one run.
pub(crate) struct LawRun {
    pub law: Box<dyn SrsLaw + Send>,
    pub descriptor: LawDescriptor,
    /// One call's budget, seconds (infinite without `control`).
    pub budget_s: f64,
    /// The core's per-pulse gain cap, dB (`None` without `control`).
    pub gain_rise_db: Option<f64>,
    pub misses: MissCounter,
    pub stats: LawStats,
    pub records: LawRecordRing<SrsCallRecord>,
    pub last_values: Vec<LawValue>,
    /// The call behind the gains the next pulse plays.
    pub pending: Option<PulseLawCall>,
    /// `law.json` has been written into the run folder.
    pub descriptor_written: bool,
}

/// What a law call came to.
pub(crate) enum LawOutcome {
    /// The gains to apply, already capped by the core.
    Used(Vec<f64>),
    /// A miss under the limit: the table in force stays.
    Missed,
    /// The call ended the run: a seed miss, or the misses in a row reached
    /// the limit.
    Ended(LawFault, u32),
}

/// Build the law with `factory`, contained: a panic is a crash, not the
/// service's.
pub(crate) fn build_law(
    factory: &dyn SrsLawFactory,
    setup: &SrsLawSetup<'_>,
) -> Result<Box<dyn SrsLaw + Send>, LawFault> {
    match catch_unwind(AssertUnwindSafe(|| factory.build(setup))) {
        Ok(r) => r,
        Err(p) => Err(LawFault::Crashed(panic_text(p))),
    }
}

/// Call the law once, timed on `clock` and contained. A call that returns
/// gains after its budget is over budget; a call that fails keeps its reason.
pub(crate) fn guarded_call(
    law: &mut (dyn SrsLaw + Send),
    which: LawCall,
    inputs: &SrsInputs<'_>,
    clock: &dyn LawClock,
    budget_s: f64,
) -> (Result<SrsOutputs, LawFault>, f64) {
    let t0 = clock.now_s();
    let r = catch_unwind(AssertUnwindSafe(|| match which {
        LawCall::Seed => law.seed(inputs),
        LawCall::Iterate => law.iterate(inputs),
    }));
    let took_s = (clock.now_s() - t0).max(0.0);
    let r = match r {
        Ok(Ok(_)) if took_s > budget_s => Err(LawFault::OverBudget { took_s, budget_s }),
        Ok(r) => r,
        Err(p) => Err(LawFault::Crashed(panic_text(p))),
    };
    (r, took_s)
}

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic with no message".into()
    }
}

/// Check a law's output: one gain per component, every number finite, every
/// gain above zero, at most [`MAX_LAW_VALUES`] values, and (after a pulse,
/// with `control`) no gain rising more than `gain_rise_db`.
pub(crate) fn check_output(
    out: &SrsOutputs,
    components: usize,
    gain_rise_db: Option<f64>,
) -> Result<(), LawFault> {
    if out.gains.len() != components {
        return Err(LawFault::BadOutput(OutputRule::WrongLength));
    }
    if out.gains.iter().any(|g| !g.is_finite()) || out.values.iter().any(|v| !v.value.is_finite()) {
        return Err(LawFault::BadOutput(OutputRule::NonFinite));
    }
    if out.gains.iter().any(|&g| g <= 0.0) {
        return Err(LawFault::BadOutput(OutputRule::NotPositive));
    }
    if out.values.len() > MAX_LAW_VALUES {
        return Err(LawFault::BadOutput(OutputRule::TooManyValues));
    }
    if let Some(db) = gain_rise_db {
        let most = 10f64.powf(db / 20.0);
        if out.gains.iter().any(|&g| g > most) {
            return Err(LawFault::BadOutput(OutputRule::GainRise));
        }
    }
    Ok(())
}

/// The largest absolute value of `x`.
pub(crate) fn peak(x: &[f64]) -> f64 {
    x.iter().fold(0.0f64, |m, v| m.max(v.abs()))
}

/// `10^(db / 20)`.
pub(crate) fn db_scale(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// What the identification view shows: the control channel's gain as the
/// core believes it, its coherence, and the floor the seed divides by.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentificationView {
    /// The estimate's frequencies, Hz.
    pub freqs_hz: Vec<f64>,
    /// `|H|`, EU per volt (scaled by any identification error the run asks
    /// for).
    pub h_mag: Vec<f64>,
    /// The complex estimate `(re, im)`, drive to control response, whose
    /// modulus is `h_mag`: what the run's record keeps. Nothing in the control
    /// reads it.
    pub h: Vec<(f64, f64)>,
    /// Coherence, 0 to 1.
    pub coherence: Vec<f64>,
    /// The floor `|H|` is held at before the seed divides by it, EU per volt.
    pub h_floor: f64,
    /// The band the mean coherence is taken over, Hz.
    pub band_hz: (f64, f64),
}

impl SrsShockService {
    /// True when this run closes the loop (`SrsTestConfig::control` set).
    pub fn is_closed_loop(&self) -> bool {
        self.cfg.control.is_some()
    }

    /// Scale the identified control gain by `db` before the seed (the law
    /// check's identification-error case; 0 dB otherwise). The core's
    /// predictions use the scaled gain too: it is the gain the core believes.
    pub fn with_identification_scale_db(mut self, db: f64) -> Self {
        self.identification_scale_db = db;
        self
    }

    /// Install the clock the time between pulses is kept on (a
    /// [`super::pacer::VirtualPacer`] in tests).
    pub fn set_pacer(&mut self, pacer: Box<dyn PulsePacer>) {
        self.pacer = std::sync::Mutex::new(pacer);
    }

    /// Install an observer the wait between pulses calls with the seconds
    /// left, each time it reads the clock.
    pub fn set_wait_observer(&mut self, f: Box<dyn FnMut(f64) + Send>) {
        self.wait_observer = Some(f);
    }

    /// Seconds until the next pulse may start: the rest of the time between
    /// pulses since the last pulse started. `None` without `control` or before
    /// the first pulse (it does not wait).
    pub fn next_pulse_in_s(&self) -> Option<f64> {
        let control = self.cfg.control.as_ref()?;
        let prev = self.last_pulse_start_s?;
        if !matches!(self.state, RunState::Armed | RunState::Firing) {
            return None;
        }
        let now = self.pacer.lock().unwrap_or_else(|e| e.into_inner()).now_s();
        Some((prev + control.min_spacing_s() - now).max(0.0))
    }

    /// The last pulse's bring-up, timed: the drive's checks, the arm, the
    /// commit, the wait for the time between pulses and the start call. `None`
    /// without `control` or before the first pulse.
    pub fn last_bring_up(&self) -> Option<hertzforge_shock_common::BringUp> {
        self.pulse_rows.last().and_then(|r| r.bring_up)
    }

    /// Seconds since the last pulse started, on the pacer: since the device's
    /// clock ran for it (`PulsePlant::fire_timed`). `None` without `control` or
    /// before the first pulse. A host that keeps its own countdown counts it
    /// from here, so its display and the core's wait agree.
    pub fn since_last_pulse_start_s(&self) -> Option<f64> {
        self.cfg.control.as_ref()?;
        let prev = self.last_pulse_start_s?;
        let now = self.pacer.lock().unwrap_or_else(|e| e.into_inner()).now_s();
        Some((now - prev).max(0.0))
    }

    /// The drive's peak, volts, if the table in force fired at `level_db` (with
    /// the between-pulse monitors' trim). `None` without `control` or before
    /// the law is seeded.
    pub fn predicted_drive_vpk_at(&self, level_db: f64) -> Option<f64> {
        self.cfg.control.as_ref()?;
        self.law_run.as_ref()?;
        let engine = self.engine.as_ref()?;
        Some(peak(&engine.synthesize(self.fs_hz)) * db_scale(level_db) * self.monitor_trim)
    }

    /// Refuse a level move whose drive would pass the drive abort.
    pub(super) fn check_level_move(&self, level_db: f64) -> Result<(), ServiceError> {
        let (Some(vpk), Some(control)) = (self.predicted_drive_vpk_at(level_db), &self.cfg.control)
        else {
            return Ok(());
        };
        if vpk > control.drive_vpk_abort {
            return Err(ServiceError::DriveCeiling {
                level_db,
                vpk,
                abort_vpk: control.drive_vpk_abort,
            });
        }
        Ok(())
    }

    /// The identification for the review view: `None` until identification
    /// is done.
    pub fn identification_view(&self) -> Option<IdentificationView> {
        let frf = self.control_frf.as_ref()?;
        let spec = self.cfg.sysid.as_ref()?;
        Some(IdentificationView {
            freqs_hz: frf.freqs_hz.clone(),
            h_mag: frf.mag.clone(),
            h: frf.h.clone(),
            coherence: frf.coherence.clone(),
            h_floor: self.h_floor(spec.h_floor_frac),
            band_hz: (self.cfg.f_lo_hz, self.cfg.f_hi_hz),
        })
    }

    /// The gains the core last applied to the table (after its caps), table
    /// order; empty before the seed.
    pub fn last_law_gains(&self) -> &[f64] {
        &self.last_gains
    }

    /// Take the law-call records the run has not written: every call since
    /// the last drain, oldest first. With capture running the run writes them
    /// into the run folder itself after each call, and this returns what is
    /// left (nothing, normally).
    pub fn drain_law_records(&mut self) -> Vec<SrsCallRecord> {
        self.law_run
            .as_mut()
            .map(|r| r.records.drain())
            .unwrap_or_default()
    }

    /// The floor the seed holds `|H|` at: `h_floor_frac` of the in-band peak.
    pub(super) fn h_floor(&self, h_floor_frac: f64) -> f64 {
        let Some(frf) = &self.control_frf else {
            return f64::MIN_POSITIVE;
        };
        let mut h_max = 0.0f64;
        for (k, &f) in frf.freqs_hz.iter().enumerate() {
            if f >= self.cfg.f_lo_hz && f <= self.cfg.f_hi_hz {
                h_max = h_max.max(frf.mag[k]);
            }
        }
        (h_floor_frac * h_max).max(f64::MIN_POSITIVE)
    }

    /// `|H|` at each of `component_hz`, floored: the identified gain the seed
    /// divides by. `None` when nothing was identified.
    pub(super) fn component_gains(&self, component_hz: &[f64]) -> Option<Vec<f64>> {
        let (frf, spec) = (self.control_frf.as_ref()?, self.cfg.sysid.as_ref()?);
        let floor = self.h_floor(spec.h_floor_frac);
        Some(
            component_hz
                .iter()
                .map(|&fc| super::interp_clamped(&frf.freqs_hz, &frf.mag, fc).max(floor))
                .collect(),
        )
    }

    /// Build the run's law and take its seed through the core's checks. The
    /// seed's gains, or the end of the run when they are not usable.
    pub(super) fn build_and_seed(
        &mut self,
        factory: Arc<dyn SrsLawFactory>,
        component_hz: &[f64],
        start_scale: f64,
    ) -> LawOutcome {
        let control = self.cfg.control.clone();
        let budget_s = control
            .as_ref()
            .map_or(f64::INFINITY, |c| c.guards.call_budget_s(c.min_spacing_s()));
        let component_gain = self.component_gains(component_hz);
        let built = {
            let setup = SrsLawSetup {
                band_hz: &self.band_freqs,
                component_hz,
                target: &self.target_full,
                q: self.cfg.q,
                component_gain: component_gain.as_deref(),
                call_budget_s: budget_s,
            };
            build_law(factory.as_ref(), &setup)
        };
        let descriptor = factory.describe();
        let law = match built {
            Ok(l) => l,
            Err(fault) => {
                self.log.push(EventKind::LawCalled {
                    pulse: 0,
                    call: LawCall::Seed,
                    used: false,
                    reason: Some(fault.reason().into()),
                    detail: Some(fault.to_string()),
                    call_ms: 0.0,
                });
                return LawOutcome::Ended(fault, 1);
            }
        };
        if control.is_some() {
            self.log.push(EventKind::Note {
                text: format!("control law: {}", LawReport::line_for(&descriptor)),
            });
        }
        self.law_run = Some(LawRun {
            law,
            descriptor,
            budget_s,
            gain_rise_db: control.as_ref().map(|c| c.max_gain_rise_db),
            misses: MissCounter::new(control.as_ref().map_or(u32::MAX, |c| c.guards.max_misses)),
            stats: LawStats::default(),
            records: LawRecordRing::default(),
            last_values: Vec::new(),
            pending: None,
            descriptor_written: false,
        });
        let target: Vec<f64> = self.target_full.iter().map(|&t| t * start_scale).collect();
        let zeros = vec![0.0f64; self.band_freqs.len()];
        let inputs = SrsInputs {
            pulse_index: 0,
            target: &target,
            measured: &zeros,
            commanded_scale: start_scale,
        };
        self.call_law(LawCall::Seed, &inputs, component_hz, 1.0)
    }

    /// What the limit cap multiplies the last pulse's predictions by, with
    /// `control`: the highest level the run is scheduled to reach over the
    /// level the pulse fired at (at least 1). The cap then holds the table to
    /// what the limit channels can take at that level, so the gains do not
    /// climb back over the notch at the lower levels and pass the bound when
    /// the ladder reaches the top. Without `control` it is 1: the cap reads the
    /// prediction at the level fired, as it always has.
    pub(super) fn limit_reference_scale(&self, level_scale: f64) -> f64 {
        if self.cfg.control.is_none() || level_scale <= 0.0 {
            return 1.0;
        }
        (db_scale(self.top_level_db()) / level_scale).max(1.0)
    }

    /// The bands the limit channels hold down on purpose: every band the
    /// arm-time notch cut, and every band within the limit cap's window
    /// ([`crate::law::LIMIT_CAP_WINDOW_OCTAVES`]) of one, where the cap holds
    /// the components that reach the notched band. Empty without limit
    /// channels.
    pub fn limit_held_bands(&self) -> Vec<usize> {
        let Some(report) = &self.arm_notch_report else {
            return Vec::new();
        };
        let window = std::f64::consts::LN_2 * crate::law::LIMIT_CAP_WINDOW_OCTAVES;
        let notched: Vec<f64> = report
            .notched_bands
            .iter()
            .map(|b| b.band_freq_hz)
            .collect();
        self.band_freqs
            .iter()
            .enumerate()
            .filter(|(_, &f)| {
                notched
                    .iter()
                    .any(|&n| n > 0.0 && f > 0.0 && (f / n).ln().abs() <= window + 1e-12)
            })
            .map(|(b, _)| b)
            .collect()
    }

    /// The pulse's pass, for the ladder and its row: no band outside the alarm
    /// band. With `control`, a band the limit channels hold down
    /// ([`Self::limit_held_bands`]) that reads low is not held against the
    /// pulse: the limit holds it there on purpose. A band reading high, or
    /// outside the abort band, still fails it.
    pub(super) fn judged_pass(
        &self,
        check: &super::config::EnvelopeCheck,
        measured: &[f64],
        level_scale: f64,
    ) -> bool {
        if check.aborted() {
            return false;
        }
        if self.cfg.control.is_none() {
            return !check.alarmed();
        }
        let held = self.limit_held_bands();
        check.alarm_bands.iter().all(|&b| {
            let t = self.target_full.get(b).copied().unwrap_or(0.0) * level_scale;
            let m = measured.get(b).copied().unwrap_or(0.0);
            held.contains(&b) && m < t
        })
    }

    /// Call the law once and take its output through the core's checks and
    /// caps. The seed is not capped (its gains are the starting gains); an
    /// iteration's gains are capped by the per-pulse rise limit (a refusal)
    /// and by the limit channels' predictions (`min(law gain, cap)`), each
    /// prediction multiplied by `limit_ref_scale` first (1 without `control`;
    /// see [`Self::limit_reference_scale`]).
    pub(super) fn call_law(
        &mut self,
        which: LawCall,
        inputs: &SrsInputs<'_>,
        component_hz: &[f64],
        limit_ref_scale: f64,
    ) -> LawOutcome {
        let clock = Arc::clone(&self.law_clock);
        let preds: Vec<Vec<f64>> = self
            .last_predicted_limit_srs
            .iter()
            .map(|(_, p)| {
                if limit_ref_scale == 1.0 {
                    p.clone()
                } else {
                    p.iter().map(|v| v * limit_ref_scale).collect()
                }
            })
            .collect();
        let Some(run) = self.law_run.as_mut() else {
            return LawOutcome::Ended(LawFault::Failed("the law was not built".into()), 1);
        };
        let (result, took_s) = guarded_call(
            run.law.as_mut(),
            which,
            inputs,
            clock.as_ref(),
            run.budget_s,
        );
        run.stats.calls += 1;
        run.stats.worst_call_s = run.stats.worst_call_s.max(took_s);
        let rise = if which == LawCall::Seed {
            None
        } else {
            run.gain_rise_db
        };
        let checked =
            result.and_then(|out| check_output(&out, component_hz.len(), rise).map(|()| out));
        let call_ms = took_s * 1e3;
        let mut status = LawLimitStatus::default();
        let (outcome, gains_out, values, applied) = match checked {
            Ok(out) => {
                let mut applied = out.gains.clone();
                if which == LawCall::Iterate {
                    if let Some(limits) = &self.law_limits {
                        let p = (!preds.is_empty()).then_some(preds.as_slice());
                        for (j, g) in applied.iter_mut().enumerate() {
                            let fc = component_hz.get(j).copied().unwrap_or(0.0);
                            let (cap, held) =
                                crate::law::predictive_cap(limits, p, &self.band_freqs, fc);
                            status.held_no_prediction |= held;
                            let (capped, bound) = crate::law::apply_cap(*g, cap);
                            *g = capped;
                            status.limited_components += usize::from(bound);
                        }
                        if let Some(p) = p {
                            status.worst_predicted_exceedance_db =
                                crate::law::worst_predicted_exceedance_db(limits, p);
                        }
                    }
                }
                (Ok(()), out.gains, out.values, applied)
            }
            Err(f) => (Err(f), Vec::new(), Vec::new(), Vec::new()),
        };
        let miss = outcome.as_ref().err().cloned();
        run.records.push(LawCallRecord {
            pulse_index: inputs.pulse_index,
            call: which,
            miss: miss.clone(),
            call_us: (took_s * 1e6).round() as u64,
            payload: SrsCallPayload {
                target: inputs.target.iter().map(|&v| v as f32).collect(),
                measured: inputs.measured.iter().map(|&v| v as f32).collect(),
                commanded_scale: inputs.commanded_scale,
                gains: gains_out,
                applied: applied.clone(),
                values: values.clone(),
                limited_components: status.limited_components,
            },
        });
        let result = match outcome {
            Ok(()) => {
                run.misses.used();
                run.last_values = values;
                run.stats.limited_components += status.limited_components as u64;
                run.pending = Some(PulseLawCall {
                    used: true,
                    miss: None,
                    call_ms,
                });
                LawOutcome::Used(applied)
            }
            Err(fault) => {
                run.stats.misses += 1;
                if matches!(fault, LawFault::BadOutput(_)) {
                    run.stats.refused += 1;
                }
                run.pending = Some(PulseLawCall {
                    used: false,
                    miss: Some(fault.clone()),
                    call_ms,
                });
                let tripped = run.misses.miss();
                if which == LawCall::Seed || tripped {
                    LawOutcome::Ended(fault, run.misses.in_a_row())
                } else {
                    LawOutcome::Missed
                }
            }
        };
        if which == LawCall::Iterate {
            self.last_limit_status = status;
        }
        self.log.push(EventKind::LawCalled {
            pulse: inputs.pulse_index,
            call: which,
            used: miss.is_none(),
            reason: miss.as_ref().map(|f| f.reason().to_string()),
            detail: miss.as_ref().map(|f| f.to_string()),
            call_ms,
        });
        self.persist_law_records();
        result
    }

    /// Write the law's identity once, and every call record since the last
    /// write, into the run folder when capture is running. Between pulses,
    /// after the pulse is judged; never during a fire.
    pub(super) fn persist_law_records(&mut self) {
        let Some(dir) = self.capture.as_ref().map(|s| s.run_dir().to_path_buf()) else {
            return;
        };
        let control = self.cfg.control.clone();
        let Some(run) = self.law_run.as_mut() else {
            return;
        };
        if !run.descriptor_written {
            let doc = serde_json::json!({
                "law": run.descriptor,
                "guards": control.as_ref().map(|c| &c.guards),
                "call_budget_s": run.budget_s.is_finite().then_some(run.budget_s),
                "max_gain_rise_db": run.gain_rise_db,
            });
            let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
            run.descriptor_written = std::fs::write(dir.join("law.json"), text).is_ok();
        }
        for r in run.records.drain() {
            let mut index = serde_json::Map::new();
            index.insert("pulse_index".into(), r.pulse_index.into());
            index.insert(
                "call".into(),
                serde_json::to_value(r.call).unwrap_or_default(),
            );
            index.insert("used".into(), r.miss.is_none().into());
            index.insert("miss".into(), r.miss.as_ref().map(|f| f.reason()).into());
            index.insert("call_us".into(), r.call_us.into());
            index.insert("commanded_scale".into(), r.payload.commanded_scale.into());
            index.insert(
                "limited_components".into(),
                r.payload.limited_components.into(),
            );
            index.insert(
                "values".into(),
                serde_json::to_value(&r.payload.values).unwrap_or_default(),
            );
            index.insert("target_len".into(), r.payload.target.len().into());
            index.insert("measured_len".into(), r.payload.measured.len().into());
            index.insert("gains_len".into(), r.payload.gains.len().into());
            index.insert("applied_len".into(), r.payload.applied.len().into());
            let mut bytes = Vec::with_capacity(
                4 * (r.payload.target.len() + r.payload.measured.len())
                    + 8 * (r.payload.gains.len() + r.payload.applied.len()),
            );
            for v in r.payload.target.iter().chain(&r.payload.measured) {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            for v in r.payload.gains.iter().chain(&r.payload.applied) {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            if hertzforge_capture::append_indexed_record(&dir, "law_pulses", &index, &bytes)
                .is_err()
            {
                // Keep it for the host rather than lose it.
                run.records.push(r);
                break;
            }
        }
    }

    /// End the run with `kind` and land in `to`: the controlled stop, nothing
    /// more fires.
    pub(super) fn end_run(&mut self, kind: TripKind, to: RunState) {
        let record = TripRecord {
            kind,
            at_level_db: self.current_level_db(),
            at_pulse: self.store.len() as u64,
        };
        self.log.push(EventKind::Trip {
            record: record.clone(),
        });
        self.trip = Some(record);
        self.park_plant();
        if self.cfg.capture.snapshot_on_stop {
            let _ = self.save_snapshot(Vec::new());
        }
        let _ = self.stop_capture_inner();
        self.transition(to);
    }

    /// Identification held the run: nothing fires.
    pub(super) fn hold(&mut self, reason: HeldReason) {
        self.log.push(EventKind::Held {
            reason: reason.clone(),
        });
        self.held = Some(reason);
        self.park_plant();
        let _ = self.stop_capture_inner();
        self.transition(RunState::Held);
    }

    /// The highest level the run is scheduled to reach: the top of an AUTO
    /// table, the present MANUAL level.
    pub(super) fn top_level_db(&self) -> f64 {
        if self.cfg.schedule.mode == hertzforge_shock_common::ShockRunMode::Auto {
            self.cfg.schedule.full_db()
        } else {
            self.current_level_db()
        }
    }

    /// Predict the drive and the control response for the table in force at
    /// the highest level the run is scheduled to reach.
    pub(super) fn refresh_prediction(&mut self) {
        let (Some(control), Some(engine), Some(frf)) =
            (&self.cfg.control, &self.engine, &self.control_frf)
        else {
            self.prediction = None;
            return;
        };
        let level_db = self.top_level_db();
        let s = db_scale(level_db) * self.monitor_trim;
        let drive: Vec<f64> = engine
            .synthesize(self.fs_hz)
            .iter()
            .map(|v| v * s)
            .collect();
        let response = predict_response_waveform(&drive, self.fs_hz, &frf.freqs_hz, &frf.mag);
        let k = hertzforge_dsp::shock::pulse_kinematics(&response, self.fs_hz);
        self.prediction = Some(DrivePrediction {
            level_db,
            drive_vpk: peak(&drive),
            drive_vpk_ceiling: control.drive_vpk_ceiling,
            drive_vpk_abort: control.drive_vpk_abort,
            response_peak_g: predict_peak_accel_g(&drive, self.fs_hz, &frf.freqs_hz, &frf.mag),
            velocity_pk_ips: k.peak_velocity_ips,
            displacement_pk_pk_in: k.pk_pk_disp_in,
        });
    }

    /// The predicted kinematics of the control response to `drive` through
    /// the identified gain, against the run's kinematic aborts and the
    /// shaker's ratings: the breach, if any, as `(axis, value, limit)`.
    pub(super) fn predicted_kinematic_breach(&self, drive: &[f64]) -> Option<(String, f64, f64)> {
        let frf = self.control_frf.as_ref()?;
        let response = predict_response_waveform(drive, self.fs_hz, &frf.freqs_hz, &frf.mag);
        let k = hertzforge_dsp::shock::pulse_kinematics(&response, self.fs_hz);
        let ka = &self.cfg.kinematic_abort;
        let shaker_v = self
            .cfg
            .shaker
            .as_ref()
            .and_then(|s| s.max_velocity_ips.as_ref())
            .map(|r| r.value);
        let shaker_d = self
            .cfg
            .shaker
            .as_ref()
            .and_then(|s| s.max_displacement_in_pp.as_ref())
            .map(|r| r.value);
        let checks = [
            ("accel", k.peak_accel_g, ka.accel_g),
            ("velocity", k.peak_velocity_ips, ka.velocity_ips),
            ("velocity", k.peak_velocity_ips, shaker_v),
            ("displacement", k.pk_pk_disp_in, ka.disp_pp_in),
            ("displacement", k.pk_pk_disp_in, shaker_d),
        ];
        checks
            .into_iter()
            .find_map(|(axis, value, limit)| match limit {
                Some(lim) if value > lim => Some((axis.to_string(), value, lim)),
                _ => None,
            })
    }

    pub(super) fn law_status(&self) -> Option<LawStatus> {
        let run = self.law_run.as_ref()?;
        Some(LawStatus {
            descriptor: run.descriptor.clone(),
            misses_in_a_row: run.misses.in_a_row(),
            misses: run.stats.misses,
            calls: run.stats.calls,
            values: run.last_values.clone(),
        })
    }

    pub(super) fn law_report(&self) -> Option<LawReport> {
        self.cfg.control.as_ref()?;
        let run = self.law_run.as_ref()?;
        Some(LawReport {
            line: LawReport::line_for(&run.descriptor),
            descriptor: run.descriptor.clone(),
            calls: run.stats.calls,
            misses: run.stats.misses,
            refused: run.stats.refused,
            limited_components: run.stats.limited_components,
            worst_call_ms: run.stats.worst_call_s * 1e3,
            budget_ms: run.budget_s * 1e3,
        })
    }

    // --- the core's guards ---------------------------------------------------------

    /// Hold the identification burst's peak at `cap`, volts.
    pub(super) fn hold_burst_at_cap(&mut self, burst: &mut [f64], cap: f64) {
        let pk = peak(burst);
        if pk > cap && pk > 0.0 {
            let s = cap / pk;
            for v in burst.iter_mut() {
                *v *= s;
            }
            self.log.push(EventKind::Note {
                text: format!(
                    "the identification burst's {pk:.4} V pk is held at its {cap:.4} V pk cap"
                ),
            });
        }
    }

    /// The coherence hold: `true`, with the run held, when the mean in-band
    /// coherence is under the floor.
    pub(super) fn coherence_holds(&mut self, mean: f64, floor: f64) -> bool {
        if mean < floor {
            self.hold(HeldReason::LowCoherence { mean, floor });
            true
        } else {
            false
        }
    }

    /// At proceed: the predicted drive at the highest level the run is
    /// scheduled to reach over the drive abort ends the run before any pulse.
    /// `true` when it did.
    pub(super) fn predicted_drive_abort(&mut self) -> bool {
        let (Some(c), Some(p)) = (&self.cfg.control, &self.prediction) else {
            return false;
        };
        if p.drive_vpk > c.drive_vpk_abort {
            let kind = TripKind::DriveVpk {
                vpk: p.drive_vpk,
                abort_vpk: c.drive_vpk_abort,
                predicted: true,
            };
            self.end_run(kind, RunState::Aborted);
            return true;
        }
        false
    }

    /// The run's pulse cap: `true`, with the run ended `not_converged`, when
    /// `fired` pulses have reached it.
    pub(super) fn run_pulse_cap_reached(&mut self, max_pulses: u64, fired: u64) -> bool {
        if fired >= max_pulses {
            self.end_not_converged(ConvergenceLimit::Run, fired);
            return true;
        }
        false
    }

    /// Before a pulse plays: the drive in volts over the abort is refused
    /// (nothing plays) and over the ceiling is scaled to it (a logged action);
    /// then the predicted response's kinematics against the run's kinematic
    /// aborts and the shaker's ratings. `false` when the run ended.
    pub(super) fn check_drive_before_fire(
        &mut self,
        c: &SrsControl,
        full: &[f64],
        fire_scale: &mut f64,
        level_db: f64,
        pulse_seq: u64,
    ) -> bool {
        let vpk = peak(full) * *fire_scale;
        if vpk > c.drive_vpk_abort {
            let kind = TripKind::DriveVpk {
                vpk,
                abort_vpk: c.drive_vpk_abort,
                predicted: false,
            };
            self.abort(kind, level_db, pulse_seq);
            return false;
        }
        if vpk > c.drive_vpk_ceiling {
            let s = c.drive_vpk_ceiling / vpk;
            *fire_scale *= s;
            self.log.push(EventKind::DriveScaled {
                scale: s,
                reason: format!(
                    "the drive's {vpk:.4} V pk is over the {:.4} V pk drive ceiling; scaled to it",
                    c.drive_vpk_ceiling
                ),
            });
        }
        let drive: Vec<f64> = full.iter().map(|v| v * *fire_scale).collect();
        if let Some((axis, value, limit)) = self.predicted_kinematic_breach(&drive) {
            let kind = TripKind::Kinematic {
                axis,
                value,
                limit,
                predicted: true,
            };
            self.abort(kind, level_db, pulse_seq);
            return false;
        }
        true
    }

    /// The iteration limit at one level ended the run: `true` when it did.
    pub(super) fn iteration_limit_ends_run(&mut self, pulses: u32) -> bool {
        self.end_not_converged(ConvergenceLimit::PerLevel, u64::from(pulses));
        true
    }

    /// End the run when the misses reached the limit (the controlled stop).
    pub(super) fn end_law_fault(&mut self, fault: LawFault, consecutive: u32) {
        self.end_run(TripKind::LawFault { fault, consecutive }, RunState::Aborted);
    }

    /// The iteration limit ended the run.
    pub(super) fn end_not_converged(&mut self, limit: ConvergenceLimit, pulses: u64) {
        self.end_run(
            TripKind::NotConverged { limit, pulses },
            RunState::NotConverged,
        );
    }
}

/// How the wait before a pulse ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Waited {
    /// The pulse may start.
    Due,
    /// A stop was raised: nothing more fires.
    Cancelled,
    /// The host's `should_continue` was lowered (nobody holds control): this
    /// pulse does not start, the run goes on.
    Held,
}

/// Wait until `deadline` on `pacer`, reading the cancel flag and the host's
/// `should_continue` every [`CANCEL_POLL_S`] and telling `observer` the seconds
/// left at each read. `None` checks the flags once and does not wait (the first
/// pulse has no pulse before it).
pub(super) fn wait_out(
    pacer: &mut Box<dyn PulsePacer>,
    cancel: &AtomicBool,
    should_continue: Option<&AtomicBool>,
    observer: &mut Option<Box<dyn FnMut(f64) + Send>>,
    deadline: Option<f64>,
) -> Waited {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Waited::Cancelled;
        }
        if should_continue.is_some_and(|g| !g.load(Ordering::Relaxed)) {
            return Waited::Held;
        }
        let Some(deadline) = deadline else {
            return Waited::Due;
        };
        let now = pacer.now_s();
        if let Some(obs) = observer.as_mut() {
            obs((deadline - now).max(0.0));
        }
        if now >= deadline {
            return Waited::Due;
        }
        // The last chunk targets the deadline itself, not `now + remainder`: the
        // two differ by a float ulp, and on the wrong side of it the test above
        // spins on ever smaller steps.
        if deadline - now <= CANCEL_POLL_S {
            pacer.sleep_until(deadline);
        } else {
            pacer.sleep_until(now + CANCEL_POLL_S);
        }
    }
}

/// One shot's pacing, as the plant's hooks (`hertzforge_shock_common::shot`):
/// the wait for the time between pulses inside `ready`, once the shot is
/// brought up, so only the start call lands on the deadline; and the start
/// stamp inside `started`, once the device's clock is running.
pub(super) struct Pace<'a> {
    pub pacer: &'a mut Box<dyn PulsePacer>,
    pub cancel: &'a AtomicBool,
    pub should_continue: Option<&'a AtomicBool>,
    pub observer: &'a mut Option<Box<dyn FnMut(f64) + Send>>,
    /// When the pulse may start, on the pacer; `None` when no pulse is before it.
    pub deadline: Option<f64>,
    /// Whether the run is paced (the closed loop). The open loop neither waits
    /// nor stamps.
    pub paced: bool,
    pub prepared: Prepared,
    pub waited: Waited,
    /// The wait, seconds on the pacer.
    pub wait_s: f64,
    /// The start call, seconds.
    pub start_s: f64,
    /// The pulse's start, on the pacer.
    pub started_at: Option<f64>,
}

impl ShotHooks for Pace<'_> {
    fn ready(&mut self, prepared: Prepared) -> bool {
        self.prepared = prepared;
        if !self.paced {
            return true;
        }
        let t0 = self.pacer.now_s();
        self.waited = wait_out(
            self.pacer,
            self.cancel,
            self.should_continue,
            self.observer,
            self.deadline,
        );
        self.wait_s = (self.pacer.now_s() - t0).max(0.0);
        self.waited == Waited::Due
    }

    fn started(&mut self, start_s: f64) {
        self.start_s = start_s;
        if self.paced && self.started_at.is_none() {
            self.started_at = Some(self.pacer.pulse_start_s());
        }
    }
}
