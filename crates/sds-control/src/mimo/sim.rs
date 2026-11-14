//! A simulated MIMO run behind [`MimoRunDriver`]: deterministic, no station, no
//! threads. It walks the states a real run walks (identification, its gates, the
//! review hold, the ladder, full level, the stop) on a clock, and answers every
//! quantity from a closed form, computing only the element asked for, so a
//! 72-control run costs nothing until a screen subscribes to something.
//!
//! It exists so every wire test runs without the controller's MIMO service. Its
//! numbers are plausible, not physical: a reference spectral matrix (the setup's
//! own, or a positive definite one built when the reference is a file), a measured
//! matrix that converges on it loop by loop, a modal FRF. Everything it reports is
//! `SIM`; nothing it makes is ever presented as a measurement.

use super::check::{self, Validation};
use super::driver::{
    Curve, Fault, Gates, LawRef, MimoReport, MimoRunDriver, MimoStatus, OverrideAck,
    OverrideReason, PackedBlock, Prediction, QuantitySelector, Series, StartContext, StopOutcome,
};
use super::setup::{MimoSetup, Reference};
use crate::protocol::{ErrorCode, MethodError};
use crate::random::view::{End, State};
use crate::table::Report;
use crate::views::Role;
use hertzforge_random::RunMode;
use hertzforge_sdm::HermitianStack;
use serde_json::{Map, Value};
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Loops of identification: one with the drives off (the noise floor), two driven.
const SYSID_LOOPS: f64 = 3.0;
/// The error left after each loop, as a fraction of the loop before.
const CONVERGENCE: f64 = 0.8;
/// Simulated seconds per wall second when the service runs unpaced.
const UNPACED_SPEED: f64 = 200.0;

/// The simulated run's clock: wall time scaled, or a time a test sets.
#[derive(Clone)]
pub enum SimClock {
    Wall { t0: Instant, speed: f64 },
    Manual(Arc<Mutex<f64>>),
}

impl SimClock {
    /// Wall time at `speed` times real time (0: as fast as useful).
    pub fn wall(speed: f64) -> SimClock {
        SimClock::Wall {
            t0: Instant::now(),
            speed: if speed > 0.0 { speed } else { UNPACED_SPEED },
        }
    }

    fn now(&self) -> f64 {
        match self {
            SimClock::Wall { t0, speed } => t0.elapsed().as_secs_f64() * speed,
            SimClock::Manual(t) => t.lock().map_or(0.0, |t| *t),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    Configured,
    /// Identifying since `t`.
    Identifying {
        t: f64,
    },
    /// Holding for review since `t`; `failed` when a gate failed.
    Review {
        t: f64,
        failed: bool,
    },
    /// Driving the schedule since `t`.
    Engaged {
        t: f64,
    },
    /// Ramping down since `t`, from level `from_db`.
    Stopping {
        t: f64,
        from_db: Option<f64>,
        outcome: StopOutcome,
    },
    Finished,
}

/// The simulated driver.
pub struct SimulatedMimoDriver {
    clock: SimClock,
    setup: Option<Arc<MimoSetup>>,
    law: LawRef,
    freqs: Arc<Vec<f64>>,
    reference: Option<HermitianStack>,
    step: Step,
    started_at: Option<f64>,
    gates: Option<Gates>,
    prediction: Option<Prediction>,
    override_ack: Vec<OverrideAck>,
    end: Option<End>,
    faults: Vec<Fault>,
    /// Loops run, frozen at the end.
    loops_at_end: u64,
    /// Per line, the mean of the reference's PSDs; per drive, its rms at 0 dB; and the
    /// per-control error of the last engaged loop asked for. Cached: a status is
    /// asked for many times a second.
    ref_mean: Vec<f64>,
    drive_rms0: Vec<f64>,
    error_cache: Option<(u64, Vec<f64>)>,
    log: Vec<String>,
}

impl SimulatedMimoDriver {
    pub fn new(clock: SimClock) -> SimulatedMimoDriver {
        SimulatedMimoDriver {
            clock,
            setup: None,
            law: LawRef {
                id: String::new(),
                name: String::new(),
                version: String::new(),
                sha256: String::new(),
            },
            freqs: Arc::new(Vec::new()),
            reference: None,
            step: Step::Configured,
            started_at: None,
            gates: None,
            prediction: None,
            override_ack: Vec::new(),
            end: None,
            faults: Vec::new(),
            loops_at_end: 0,
            ref_mean: Vec::new(),
            drive_rms0: Vec::new(),
            error_cache: None,
            log: Vec::new(),
        }
    }

    /// What the run has logged (the controller's lost and found notes among them).
    pub fn log(&self) -> &[String] {
        &self.log
    }

    fn setup(&self) -> &MimoSetup {
        self.setup.as_deref().expect("configured")
    }

    fn loop_s(&self) -> f64 {
        self.setup().loop_s().max(1e-3)
    }

    fn n(&self) -> usize {
        self.setup().controlled()
    }

    fn nd(&self) -> usize {
        self.setup().drives.len()
    }

    // ---- the reference, element by element ---------------------------------------

    /// One reference PSD on every line.
    fn ref_psd(&self, i: usize) -> Vec<f64> {
        (0..self.freqs.len())
            .map(|l| self.ref_element(l, i, i).0)
            .collect()
    }

    /// The reference's element `(i, j)` on line `l`, as `(re, im)`.
    fn ref_element(&self, l: usize, i: usize, j: usize) -> (f64, f64) {
        match &self.reference {
            Some(g) => {
                let v = g.get(l, i, j);
                (v.re, v.im)
            }
            None => {
                // A positive definite matrix from a file's stand-in: each PSD 0.01 on
                // a gentle slope, an AR(1) correlation 0.5^|i-j|, and a phase that
                // turns with frequency (a unitary diagonal congruence keeps it
                // positive definite).
                let f = self.freqs[l];
                let p = |k: usize| 0.01 * (1.0 + 0.1 * k as f64) * (100.0 / f.max(1.0)).powf(0.1);
                let mag = (p(i) * p(j)).sqrt() * 0.5f64.powi((i as i32 - j as i32).abs());
                let th = (i as f64 - j as f64) * 2.0 * PI * f / 20_000.0;
                (mag * th.cos(), mag * th.sin())
            }
        }
    }

    // ---- the run's progress ---------------------------------------------------------

    /// The level (dB) and the time at it, while engaged `dt` seconds.
    fn level_at(&self, dt: f64) -> (f64, f64, Option<f64>, bool) {
        let s = &self.setup().schedule;
        let mut t = 0.0;
        let last = s.levels.len().saturating_sub(1);
        for (k, (db, dwell)) in s.levels.iter().enumerate() {
            let hold = if k == last {
                s.full_level_s
            } else {
                dwell.unwrap_or(0.0)
            };
            if s.mode == RunMode::Manual {
                return (*db, dt, None, false);
            }
            if dt < t + hold {
                return (*db, dt - t, Some(t + hold - dt), k == last);
            }
            t += hold;
        }
        let db = s.levels.last().map_or(0.0, |l| l.0);
        (db, dt - t, Some(0.0), true)
    }

    fn schedule_s(&self) -> f64 {
        let s = &self.setup().schedule;
        let n = s.levels.len();
        s.levels
            .iter()
            .take(n.saturating_sub(1))
            .map(|l| l.1.unwrap_or(0.0))
            .sum::<f64>()
            + s.full_level_s
    }

    /// Move the run along to now: identification ends, the schedule completes, a
    /// ramp-down arrives.
    fn advance(&mut self) {
        let now = self.clock.now();
        loop {
            match self.step {
                Step::Identifying { t } if now >= t + SYSID_LOOPS * self.loop_s() => {
                    let at = t + SYSID_LOOPS * self.loop_s();
                    self.identified();
                    let failed = !self.gates.as_ref().is_some_and(Gates::passed);
                    self.step = if failed || self.setup().identification.review {
                        Step::Review { t: at, failed }
                    } else {
                        Step::Engaged { t: at }
                    };
                }
                Step::Engaged { t }
                    if self.setup().schedule.mode == RunMode::Auto
                        && now >= t + self.schedule_s() =>
                {
                    let at = t + self.schedule_s();
                    self.finish(at, "completed", "The schedule finished.".into(), Some(0.0));
                }
                Step::Stopping {
                    t,
                    from_db,
                    outcome,
                } if now >= t + self.setup().control.shutdown_ramp_s => {
                    let at = t + self.setup().control.shutdown_ramp_s;
                    let words = match outcome {
                        StopOutcome::OperatorStop => "Stopped by the operator.".to_owned(),
                        StopOutcome::SafeStop => {
                            "Nobody held control, so the service stopped the run.".to_owned()
                        }
                    };
                    self.finish(at, outcome.name(), words, from_db);
                }
                _ => return,
            }
        }
    }

    fn finish(&mut self, at: f64, outcome: &'static str, message: String, level_db: Option<f64>) {
        self.loops_at_end = self.loops_until(at);
        self.end = Some(End {
            outcome,
            message,
            at_s: self.started_at.map(|s| (at - s).max(0.0)),
            level_db,
            trip: None,
        });
        self.step = Step::Finished;
    }

    fn loops_until(&self, at: f64) -> u64 {
        self.started_at
            .map_or(0, |s| ((at - s) / self.loop_s()).max(0.0).floor() as u64)
    }

    /// Identification is done: measure the gates and make the first prediction.
    fn identified(&mut self) {
        let (n, nd) = (self.n() as f64, self.nd() as f64);
        let id = &self.setup().identification;
        self.gates = Some(Gates {
            coherence_min: 0.962,
            coherence_floor: id.coherence_floor,
            condition_max: 4.0 + 8.5 * (n / nd).max(nd / n),
            condition_limit: id.condition_limit,
            coherence: Vec::new(),
        });
        self.prediction = Some(self.predict(1, self.law.clone(), &Map::new()));
    }

    fn predict(&self, iteration: u32, law: LawRef, params: &Map<String, Value>) -> Prediction {
        // A law that corrects less leaves more error.
        let gain = params
            .get("convergence")
            .and_then(Value::as_f64)
            .unwrap_or(0.5);
        let drive_rms_v = (0..self.nd())
            .map(|k| self.drive_rms_at(k, 0.0))
            .collect::<Vec<_>>();
        Prediction {
            iteration,
            law,
            ceiling_vrms: self.setup().drives.iter().map(|d| d.ceiling_vrms).collect(),
            drive_rms_v,
            error_db: (0..self.n())
                .map(|i| 0.25 / gain.max(0.1) * ((i + 1) as f64).sin())
                .collect(),
            has_achievable: false,
            unreachable_db: vec![0.0; self.n()],
            drive_accel: Vec::new(),
            accel_control: Vec::new(),
            feasibility: Vec::new(),
        }
    }

    /// Engaged loops (0 until engaged).
    fn engaged_loops(&self, now: f64) -> f64 {
        match self.step {
            Step::Engaged { t } => ((now - t) / self.loop_s()).max(0.0),
            Step::Stopping { .. } | Step::Finished => 30.0,
            _ => 0.0,
        }
    }

    /// The level the loop runs at (dB), and whether anything drives.
    fn level_now(&self, now: f64) -> Option<f64> {
        match self.step {
            Step::Identifying { .. } => Some(self.setup().identification.level_db),
            Step::Engaged { t } => Some(self.level_at(now - t).0),
            Step::Stopping { t, from_db, .. } => {
                let ramp = self.setup().control.shutdown_ramp_s.max(1e-6);
                from_db.map(|db| db - 40.0 * ((now - t) / ramp).min(1.0))
            }
            _ => None,
        }
    }

    /// Control `i`'s error on line `l`, dB, after `k` engaged loops.
    fn error_db(&self, i: usize, l: usize, k: f64) -> f64 {
        let f = self.freqs[l];
        let amp = 2.0 * CONVERGENCE.powf(k) + 0.05;
        amp * (2.0 * PI * f / 400.0 + i as f64 * 0.9).sin()
    }

    /// The measured element `(i, j)` on line `l`, at level `db`, after `k` loops.
    fn measured(&self, l: usize, i: usize, j: usize, db: f64, k: f64) -> (f64, f64) {
        let (re, im) = self.ref_element(l, i, j);
        let lin = 10f64.powf(db / 10.0);
        let gi = 10f64.powf(self.error_db(i, l, k) / 10.0);
        let gj = 10f64.powf(self.error_db(j, l, k) / 10.0);
        let s = lin * (gi * gj).sqrt();
        if i == j {
            return (re * s, 0.0);
        }
        // Coherence a little short of the reference's, and a little phase error,
        // both shrinking as the loop converges.
        let keep = 1.0 - (0.05 * CONVERGENCE.powf(k) + 0.01);
        let ph = (2.0 * CONVERGENCE.powf(k)).to_radians() * ((i + j) as f64).sin();
        let (c, sn) = (ph.cos(), ph.sin());
        (s * keep * (re * c - im * sn), s * keep * (re * sn + im * c))
    }

    fn drive_psd(&self, k: usize, db: f64) -> Vec<f64> {
        let lin = 10f64.powf(db / 10.0);
        (0..self.freqs.len())
            .map(|l| {
                let mean = self.ref_mean.get(l).copied().unwrap_or(0.0);
                let w = 0.5 * (1.0 + 0.5 * (2.0 * PI * self.freqs[l] / 700.0 + k as f64).sin());
                lin * mean * w * 4.0
            })
            .collect()
    }

    fn drive_rms_at(&self, k: usize, db: f64) -> f64 {
        self.drive_rms0.get(k).copied().unwrap_or(0.0) * 10f64.powf(db / 20.0)
    }

    /// The modal FRF from drive `k` to control `i` on line `l`.
    fn frf(&self, l: usize, i: usize, k: usize) -> (f64, f64) {
        let f = self.freqs[l];
        let mut re = 0.0;
        let mut im = 0.0;
        for (m, (fm, zeta)) in [(150.0f64, 0.03f64), (420.0, 0.02), (900.0, 0.025)]
            .iter()
            .enumerate()
        {
            let r = (i as f64 * 0.7 + k as f64 * 1.3 + m as f64).cos() * fm * fm;
            let a = fm * fm - f * f;
            let b = 2.0 * zeta * fm * f;
            let d = a * a + b * b;
            re += r * a / d;
            im -= r * b / d;
        }
        (re, im)
    }

    fn singular_values(&self, l: usize) -> Vec<f64> {
        let top = (0..self.n())
            .map(|i| {
                let (re, im) = self.frf(l, i, 0);
                re.hypot(im)
            })
            .fold(0.0, f64::max)
            .max(1e-6);
        let cond = self.gates.as_ref().map_or(10.0, |g| g.condition_max);
        let k = self.n().min(self.nd());
        (0..k)
            .map(|j| top * cond.powf(-(j as f64) / (k.max(2) - 1) as f64))
            .collect()
    }

    fn streaming(&self) -> bool {
        !matches!(self.step, Step::Configured)
    }

    fn require(&self, ok: bool, verb: &str, state: &str) -> Result<(), MethodError> {
        if ok {
            return Ok(());
        }
        Err(MethodError::named(
            ErrorCode::Refused,
            "wrong_state",
            format!("{verb} does not apply while the run is {state}."),
        ))
    }

    fn state_words(&self) -> &'static str {
        match self.step {
            Step::Configured => "configured",
            Step::Identifying { .. } => "identifying the system",
            Step::Review { .. } => "holding for review",
            Step::Engaged { .. } => "running",
            Step::Stopping { .. } => "stopping",
            Step::Finished => "finished",
        }
    }
}

impl MimoRunDriver for SimulatedMimoDriver {
    fn configure(
        &mut self,
        setup: Arc<MimoSetup>,
        law: Option<LawRef>,
        items: Report,
    ) -> Validation {
        let v = check::numeric(&setup, items, law.clone());
        self.freqs = Arc::new(setup.lines_hz());
        self.reference = match &setup.reference {
            Reference::Inline {
                freqs_hz,
                n,
                re,
                im,
            } if v.ok() => HermitianStack::from_packed(
                freqs_hz.len(),
                *n,
                re.iter()
                    .zip(im)
                    .map(|(r, i)| hertzforge_sdm::Complex::new(*r, *i))
                    .collect(),
            )
            .ok(),
            _ => None,
        };
        if let Some(l) = law {
            self.law = l;
        }
        self.setup = Some(setup);
        let n = self.n().max(1) as f64;
        self.ref_mean = (0..self.freqs.len())
            .map(|l| {
                (0..self.n())
                    .map(|i| self.ref_element(l, i, i).0)
                    .sum::<f64>()
                    / n
            })
            .collect();
        let df = self.setup().df_hz();
        self.drive_rms0 = (0..self.nd())
            .map(|k| (self.drive_psd(k, 0.0).iter().sum::<f64>() * df).sqrt())
            .collect();
        v
    }

    fn start(&mut self, _ctx: &StartContext) -> Result<(), MethodError> {
        self.advance();
        let state = self.state_words();
        self.require(matches!(self.step, Step::Configured), "Starting", state)?;
        let now = self.clock.now();
        self.started_at = Some(now);
        self.step = Step::Identifying { t: now };
        self.log.push("started: identification".into());
        Ok(())
    }

    fn proceed(&mut self) -> Result<(), MethodError> {
        self.advance();
        match self.step {
            Step::Review { failed: false, .. } => {
                self.step = Step::Engaged {
                    t: self.clock.now(),
                };
                Ok(())
            }
            Step::Review { failed: true, .. } => Err(MethodError::named(
                ErrorCode::Refused,
                "gate_failed",
                "Identification failed a gate, so the run holds; override with your reason to engage anyway, or stop and correct the setup.",
            )
            .with_detail(serde_json::json!({"gates": self.gates.as_ref().map(|g| g.passed())}))),
            _ => self.require(false, "Proceeding", self.state_words()),
        }
    }

    fn override_gate(
        &mut self,
        ack: &str,
        by: &str,
        reason: Option<OverrideReason>,
    ) -> Result<(), MethodError> {
        self.advance();
        if reason == Some(OverrideReason::Prediction) {
            return Err(MethodError::named(
                ErrorCode::Refused,
                "override_not_needed",
                "The prediction is within every ceiling; proceed instead.",
            ));
        }
        match self.step {
            Step::Review { failed: true, .. } => {
                self.override_ack.push(OverrideAck {
                    by: by.to_owned(),
                    text: ack.to_owned(),
                    reason: OverrideReason::Gate,
                });
                self.log.push(format!("gate overridden by {by}: {ack}"));
                self.step = Step::Engaged {
                    t: self.clock.now(),
                };
                Ok(())
            }
            Step::Review { failed: false, .. } => Err(MethodError::named(
                ErrorCode::Refused,
                "override_not_needed",
                "Identification passed its gates; proceed instead.",
            )),
            _ => self.require(false, "Overriding the gate", self.state_words()),
        }
    }

    fn iterate_prediction(
        &mut self,
        law: Option<(LawRef, Map<String, Value>)>,
    ) -> Result<Prediction, MethodError> {
        self.advance();
        self.require(
            matches!(self.step, Step::Review { .. }),
            "A new prediction",
            self.state_words(),
        )?;
        let iteration = self.prediction.as_ref().map_or(1, |p| p.iteration + 1);
        let (law, params) = match law {
            Some((l, p)) => {
                self.law = l.clone();
                (l, p)
            }
            None => (self.law.clone(), self.setup().law.params.clone()),
        };
        let p = self.predict(iteration, law, &params);
        self.prediction = Some(p.clone());
        Ok(p)
    }

    fn stop(&mut self, outcome: StopOutcome, by: &str) {
        self.advance();
        let now = self.clock.now();
        self.log.push(format!("{} by {by}", outcome.name()));
        match self.step {
            Step::Engaged { .. } => {
                self.step = Step::Stopping {
                    t: now,
                    from_db: self.level_now(now),
                    outcome,
                };
            }
            Step::Identifying { .. } | Step::Review { .. } => {
                let words = format!("Stopped by {by} before the loop engaged.");
                self.finish(now, outcome.name(), words, None);
            }
            _ => {}
        }
    }

    fn abort(&mut self, by: &str) {
        self.advance();
        let now = self.clock.now();
        if matches!(
            self.step,
            Step::Identifying { .. }
                | Step::Review { .. }
                | Step::Engaged { .. }
                | Step::Stopping { .. }
        ) {
            let level = self.level_now(now);
            self.finish(now, "operator_abort", format!("Aborted by {by}."), level);
        }
    }

    fn status(&mut self) -> MimoStatus {
        self.advance();
        let now = self.clock.now();
        let (state, phase) = match self.step {
            Step::Configured => (State::Configured, "idle"),
            Step::Identifying { t } => (
                State::Running,
                if now - t < self.loop_s() {
                    "sysid_noise"
                } else {
                    "sysid"
                },
            ),
            Step::Review { failed, .. } => (
                State::Review,
                if failed {
                    "gate_failed"
                } else {
                    "awaiting_review"
                },
            ),
            Step::Engaged { t } => (
                State::Running,
                if self.level_at(now - t).3 {
                    "full_level"
                } else {
                    "engaged"
                },
            ),
            Step::Stopping { .. } => (State::Stopping, "ramping_down"),
            Step::Finished => (
                State::Finished,
                if self
                    .end
                    .as_ref()
                    .is_some_and(|e| e.outcome == "operator_abort")
                {
                    "aborted"
                } else {
                    "stopped"
                },
            ),
        };
        let loop_index = match self.step {
            Step::Finished => self.loops_at_end,
            _ => self.loops_until(now),
        };
        let k = self.engaged_loops(now);
        let driving = matches!(self.step, Step::Engaged { .. } | Step::Stopping { .. });
        let level = self.level_now(now);
        let (at_level_s, remaining) = match self.step {
            Step::Engaged { t } => {
                let (_, at, rem, _) = self.level_at(now - t);
                (Some(at), rem)
            }
            _ => (None, None),
        };
        let lines = self.freqs.len();
        let kk = k.floor();
        let cached = self
            .error_cache
            .as_ref()
            .filter(|(at, _)| *at == kk as u64)
            .map(|(_, e)| e.clone());
        let control_error_db =
            if let Some(e) = cached.filter(|_| driving || matches!(self.step, Step::Finished)) {
                e
            } else if driving || matches!(self.step, Step::Finished) && self.gates.is_some() {
                let k = kk;
                (0..self.n())
                    .map(|i| {
                        let p = self.ref_psd(i);
                        let meas: f64 = (0..lines)
                            .map(|l| p[l] * 10f64.powf(self.error_db(i, l, k) / 10.0))
                            .sum();
                        let r: f64 = p.iter().sum();
                        if r > 0.0 {
                            10.0 * (meas / r).log10()
                        } else {
                            0.0
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
        if !control_error_db.is_empty() {
            self.error_cache = Some((kk as u64, control_error_db.clone()));
        }
        let (caps, drive_rms_v) = match (driving, level) {
            (true, Some(db)) => (0..self.nd())
                .map(|d| {
                    let want = self.drive_rms_at(d, db);
                    let ceiling = self.setup().drives[d].ceiling_vrms;
                    if want > ceiling {
                        (ceiling / want, ceiling)
                    } else {
                        (1.0, want)
                    }
                })
                .unzip(),
            _ => (Vec::new(), Vec::new()),
        };
        let alarm = control_error_db.iter().enumerate().any(|(i, e)| {
            let c = self.setup().controls.get(i);
            c.is_some_and(|c| *e > c.alarm_db[1] || -*e > c.alarm_db[0])
        });
        MimoStatus {
            state,
            phase,
            elapsed_s: self.started_at.map(|s| match (&self.step, &self.end) {
                (Step::Finished, Some(e)) => e.at_s.unwrap_or(0.0),
                _ => (now - s).max(0.0),
            }),
            level_db: if matches!(self.step, Step::Stopping { .. }) {
                level.map(|l| l.max(-40.0))
            } else {
                level
            },
            at_level_s,
            remaining_at_level_s: remaining,
            loop_index,
            alarm,
            control_error_db,
            gates: self.gates.clone(),
            caps,
            drive_rms_v,
            law: self.law.clone(),
            faults: self.faults.clone(),
            override_ack: self.override_ack.clone(),
            prediction: self.prediction.clone(),
            excluded: Vec::new(),
            end: self.end.clone(),
            streaming: self.streaming(),
            record: None,
        }
    }

    fn state(&mut self) -> State {
        self.advance();
        match self.step {
            Step::Configured => State::Configured,
            Step::Identifying { .. } | Step::Engaged { .. } => State::Running,
            Step::Review { .. } => State::Review,
            Step::Stopping { .. } => State::Stopping,
            Step::Finished => State::Finished,
        }
    }

    fn quantity(&mut self, q: &QuantitySelector) -> Option<Series> {
        self.advance();
        self.setup.as_ref()?;
        let now = self.clock.now();
        let (n, nd, lines) = (self.n(), self.nd(), self.freqs.len());
        let measured = self.gates.is_some()
            || matches!(self.step, Step::Identifying { t } if now - t >= self.loop_s());
        let db = self.level_now(now).unwrap_or(0.0);
        let k = self.engaged_loops(now);
        let freqs = self.freqs.clone();
        let lin = 10f64.powf(db / 10.0);
        let line = |curves: Vec<Curve>| {
            Some(Series::Lines {
                freqs: freqs.clone(),
                curves,
            })
        };
        let curve = |name: &str, role: Role, y: Vec<f64>| Curve {
            name: name.to_owned(),
            role,
            y,
        };
        let pair_ok = |i: usize, j: usize, m: usize| i < m && j < m;
        match q {
            QuantitySelector::Psd(i) if *i < n => {
                let r: Vec<f64> = self.ref_psd(*i).iter().map(|x| x * lin).collect();
                let c = &self.setup().controls.get(*i);
                let (al, ab) = c.map_or(([3.0, 3.0], [6.0, 6.0]), |c| (c.alarm_db, c.abort_db));
                let band = |db: f64| {
                    r.iter()
                        .map(|x| x * 10f64.powf(db / 10.0))
                        .collect::<Vec<_>>()
                };
                let mut curves = Vec::new();
                if measured {
                    curves.push(curve(
                        "Control",
                        Role::Measured,
                        (0..lines)
                            .map(|l| self.measured(l, *i, *i, db, k).0)
                            .collect(),
                    ));
                }
                curves.push(curve("Reference", Role::Reference, r.clone()));
                curves.push(curve("Alarm high", Role::Tolerance, band(al[1])));
                curves.push(curve("Alarm low", Role::Tolerance, band(-al[0])));
                curves.push(curve("Abort high", Role::Abort, band(ab[1])));
                curves.push(curve("Abort low", Role::Abort, band(-ab[0])));
                line(curves)
            }
            QuantitySelector::Coh(i, j)
            | QuantitySelector::Phase(i, j)
            | QuantitySelector::CsdMag(i, j)
                if pair_ok(*i, *j, n) =>
            {
                let pick = |(re, im): (f64, f64), di: f64, dj: f64| match q {
                    QuantitySelector::Coh(..) => {
                        if di * dj > 0.0 {
                            (re * re + im * im) / (di * dj)
                        } else {
                            0.0
                        }
                    }
                    QuantitySelector::Phase(..) => im.atan2(re).to_degrees(),
                    _ => re.hypot(im),
                };
                let reference: Vec<f64> = (0..lines)
                    .map(|l| {
                        pick(
                            self.ref_element(l, *i, *j),
                            self.ref_element(l, *i, *i).0,
                            self.ref_element(l, *j, *j).0,
                        ) * if matches!(q, QuantitySelector::CsdMag(..)) {
                            lin
                        } else {
                            1.0
                        }
                    })
                    .collect();
                let mut curves = Vec::new();
                if measured {
                    curves.push(curve(
                        "Measured",
                        Role::Measured,
                        (0..lines)
                            .map(|l| {
                                pick(
                                    self.measured(l, *i, *j, db, k),
                                    self.measured(l, *i, *i, db, k).0,
                                    self.measured(l, *j, *j, db, k).0,
                                )
                            })
                            .collect(),
                    ));
                }
                curves.push(curve("Reference", Role::Reference, reference));
                line(curves)
            }
            QuantitySelector::ErrorDb(rows) if measured => {
                if rows.iter().any(|i| *i >= n) {
                    return None;
                }
                Some(Series::Rows {
                    freqs,
                    rows: rows
                        .iter()
                        .map(|i| (0..lines).map(|l| self.error_db(*i, l, k)).collect())
                        .collect(),
                })
            }
            QuantitySelector::DrivePsd(d) if *d < nd && measured => {
                line(vec![curve("Drive", Role::Drive, self.drive_psd(*d, db))])
            }
            QuantitySelector::DriveCoh(a, b) if pair_ok(*a, *b, nd) && measured => {
                let c = if a == b {
                    1.0
                } else {
                    0.3f64.powi((*a as i32 - *b as i32).abs())
                };
                line(vec![curve(
                    "Coherence",
                    Role::Measured,
                    (0..lines)
                        .map(|l| c * (0.9 + 0.1 * (self.freqs[l] / 300.0).sin().abs()))
                        .collect(),
                )])
            }
            QuantitySelector::Frf(i, d) if *i < n && *d < nd && measured => {
                let h: Vec<(f64, f64)> = (0..lines).map(|l| self.frf(l, *i, *d)).collect();
                line(vec![
                    curve(
                        "Magnitude",
                        Role::Measured,
                        h.iter().map(|(r, m)| r.hypot(*m)).collect(),
                    ),
                    curve(
                        "Phase",
                        Role::Measured,
                        h.iter().map(|(r, m)| m.atan2(*r).to_degrees()).collect(),
                    ),
                ])
            }
            QuantitySelector::Sv if measured => {
                let sv: Vec<Vec<f64>> = (0..lines).map(|l| self.singular_values(l)).collect();
                let kk = n.min(nd);
                line(
                    (0..kk)
                        .map(|j| {
                            curve(
                                &format!("Singular value {}", j + 1),
                                Role::Measured,
                                sv.iter().map(|s| s[j]).collect(),
                            )
                        })
                        .collect(),
                )
            }
            QuantitySelector::Cond if measured => {
                let limit = self.setup().identification.condition_limit;
                line(vec![
                    curve(
                        "Condition number",
                        Role::Measured,
                        (0..lines)
                            .map(|l| {
                                let s = self.singular_values(l);
                                s[0] / s[s.len() - 1].max(1e-300)
                            })
                            .collect(),
                    ),
                    curve("Gate", Role::Limit, vec![limit; lines]),
                ])
            }
            QuantitySelector::Mcoh(i) if *i < n && measured => line(vec![curve(
                "Multiple coherence",
                Role::Measured,
                (0..lines)
                    .map(|l| 0.97 - 0.02 * (self.freqs[l] / 150.0 + *i as f64).sin().abs())
                    .collect(),
            )]),
            QuantitySelector::Trace => {
                let reference: Vec<f64> = (0..lines)
                    .map(|l| (0..n).map(|i| self.ref_element(l, i, i).0).sum::<f64>() * lin)
                    .collect();
                let mut curves = Vec::new();
                if measured {
                    curves.push(curve(
                        "Control",
                        Role::Measured,
                        (0..lines)
                            .map(|l| (0..n).map(|i| self.measured(l, i, i, db, k).0).sum())
                            .collect(),
                    ));
                }
                curves.push(curve("Reference", Role::Reference, reference));
                line(curves)
            }
            QuantitySelector::SdmBlock(idx) if idx.iter().all(|i| *i < n) && !idx.is_empty() => {
                let m = idx.len();
                let per = HermitianStack::packed_len(m);
                let mut meas = PackedBlock {
                    n: m,
                    re: Vec::with_capacity(lines * per),
                    im: Vec::with_capacity(lines * per),
                };
                let mut refb = PackedBlock {
                    n: m,
                    re: Vec::with_capacity(lines * per),
                    im: Vec::with_capacity(lines * per),
                };
                for l in 0..lines {
                    for (r, i) in idx.iter().enumerate() {
                        for j in &idx[r..] {
                            let (a, b) = self.measured(l, *i, *j, db, k);
                            meas.re.push(a);
                            meas.im.push(b);
                            let (a, b) = self.ref_element(l, *i, *j);
                            refb.re.push(a * lin);
                            refb.im.push(b * lin);
                        }
                    }
                }
                if !measured {
                    return Some(Series::Packed {
                        freqs,
                        measured: PackedBlock {
                            n: m,
                            re: Vec::new(),
                            im: Vec::new(),
                        },
                        reference: Some(refb),
                    });
                }
                Some(Series::Packed {
                    freqs,
                    measured: meas,
                    reference: Some(refb),
                })
            }
            QuantitySelector::PredErrorDb(i) if *i < n => {
                let p = self.prediction.as_ref()?;
                let e = p.error_db.get(*i).copied().unwrap_or(0.0);
                line(vec![curve(
                    "Predicted error",
                    Role::Measured,
                    (0..lines)
                        .map(|l| e * (1.0 + 0.5 * (self.freqs[l] / 250.0).sin()))
                        .collect(),
                )])
            }
            QuantitySelector::LawDiag(key, _) if measured && key == "condition" => {
                self.quantity(&QuantitySelector::Cond)
            }
            _ => None,
        }
    }

    fn report(&mut self) -> Option<MimoReport> {
        self.advance();
        if !matches!(self.step, Step::Finished) {
            return None;
        }
        let s = self.status();
        let df = self.setup().df_hz();
        let reference_rms: Vec<f64> = (0..self.n())
            .map(|i| (self.ref_psd(i).iter().sum::<f64>() * df).sqrt())
            .collect();
        let control_rms = reference_rms
            .iter()
            .zip(&s.control_error_db)
            .map(|(r, e)| r * 10f64.powf(e / 20.0))
            .collect();
        Some(MimoReport {
            law: self.law.clone(),
            end: self.end.clone(),
            loops: self.loops_at_end,
            control_rms,
            reference_rms,
            control_error_db: s.control_error_db.clone(),
            drive_rms_v: (0..self.nd()).map(|k| self.drive_rms_at(k, 0.0)).collect(),
            min_cap: self
                .setup()
                .drives
                .iter()
                .enumerate()
                .map(|(k, d)| (d.ceiling_vrms / self.drive_rms_at(k, 0.0)).min(1.0))
                .collect(),
            gates: self.gates.clone(),
            override_ack: self.override_ack.clone(),
        })
    }

    fn controller_lost(&mut self, name: &str) {
        self.log
            .push(format!("{name} went without a goodbye; the run goes on"));
    }

    fn controller_back(&mut self, name: &str) {
        self.log.push(format!("{name} is back in control"));
    }
}
