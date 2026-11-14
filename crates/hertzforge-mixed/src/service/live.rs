//! The run lifecycle of [`MixedService`]: arm, identification, gate, engage,
//! the chunks and their phase order, the verbs, the ends, and what the run
//! says about itself.

use super::display::{
    BandHistory, LineGrid, MixedDisplay, MixedDisplayChannel, MixedFrf, ToneHistory,
};
use super::run::{Runtime, FLOOR_EQUALIZED_DB};
use super::status::{
    FloorSummary, MixedEventKind, MixedLawStatus, MixedOutcome, MixedPhase, MixedState,
    MixedStatus, MixedTotals, MixedTrip, MixedTripKind, RoleTally,
};
use super::{LawRole, MixedAdjustRefusal, MixedError, MixedReport, MixedService};
use crate::parts::{PartBody, PartSchedule, SweepLaw};
use hertzforge_capture::Mark;
use hertzforge_random::RunMode;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

const TICKS_PER_BATCH_F: f64 = super::run::TICKS_PER_BATCH as f64;

/// The most points a history keeps.
const HISTORY_POINTS: usize = 2400;

/// A verb posted between chunks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verb {
    Level(f64),
    Hold(bool),
    Part(usize, bool),
    Stop,
}

/// The level ladder's place.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Ladder {
    /// The rung the level is on, or the next rung above when it is between.
    pub(crate) rung: usize,
    /// Whether the level is on that rung.
    pub(crate) on_rung: bool,
    /// Seconds at the present level, holds excluded.
    pub(crate) level_s: f64,
    /// Seconds at full level, holds excluded.
    pub(crate) full_s: f64,
    /// Whether the ladder has started (every part is in).
    pub(crate) started: bool,
}

impl Ladder {
    /// The ladder's place for a level of `db` on `table`: the first rung at or
    /// above it (the last when it is above them all).
    pub(crate) fn at(table: &[f64], db: f64) -> Self {
        let i = table
            .iter()
            .position(|r| *r >= db - 1e-6)
            .unwrap_or(table.len().saturating_sub(1));
        Ladder {
            rung: i,
            on_rung: (table[i] - db).abs() < 1e-6,
            ..Ladder::default()
        }
    }
}

/// Each tone's and band's history, a point per chunk.
#[derive(Debug, Clone, Default)]
pub(crate) struct Histories {
    pub(crate) tones: Vec<ToneHistory>,
    pub(crate) bands: Vec<BandHistory>,
}

fn trim<T>(v: &mut Vec<T>) {
    if v.len() > HISTORY_POINTS {
        let cut = v.len() - HISTORY_POINTS;
        v.drain(..cut);
    }
}

fn db(g: f64) -> f64 {
    if g > 0.0 {
        20.0 * g.log10()
    } else {
        f64::NEG_INFINITY
    }
}

impl MixedService {
    fn set_state(&mut self, to: MixedState) {
        if self.state != to {
            let t = self.t_s();
            self.log.push(
                t,
                MixedEventKind::State {
                    from: self.state,
                    to,
                },
            );
            self.state = to;
        }
    }

    fn set_phase(&mut self, to: MixedPhase) {
        if self.phase != to {
            let t = self.t_s();
            self.log.push(t, MixedEventKind::Phase { to });
            self.phase = to;
            self.phase_at = self.frames();
        }
    }

    fn frames(&self) -> u64 {
        self.rt.as_ref().map_or(0, |r| r.step.frames)
    }

    fn t_s(&self) -> f64 {
        self.frames().saturating_sub(self.engaged_at) as f64 / self.sample_rate_hz
    }

    fn start_gain(&self) -> f64 {
        10f64.powf(self.cfg.schedule.start_db / 20.0)
    }

    fn full_db_range(&self) -> (f64, f64) {
        let t = &self.cfg.schedule.table_db;
        let lo = t
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min)
            .min(self.cfg.schedule.start_db);
        (
            lo,
            t.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(0.0),
        )
    }

    // ---- arm, identification, gate, review -----------------------------

    /// Arm: adopt the station's rate (re-validating every part under its
    /// half), open the station through the shared arm gate, build the line
    /// grid, the floor target and envelopes, the band references and the tone
    /// targets. State stays [`MixedState::Idle`] until identification.
    pub fn arm(&mut self) -> Result<(), MixedError> {
        if self.rt.is_some() || self.state != MixedState::Idle {
            return Err(MixedError::BadState("already armed".into()));
        }
        self.sysid_cancel.store(false, Ordering::Release);
        let backend = self.backend.take().ok_or_else(|| {
            MixedError::BadState(
                "the station was consumed by an earlier arm; rebuild the run".into(),
            )
        })?;
        let rt = Runtime::arm(
            &self.cfg,
            &self.table,
            backend,
            self.station,
            self.sample_rate_hz,
            self.block_frames,
            self.clock.clone(),
            self.tone_clock.clone(),
            self.floor_clock.clone(),
        )?;
        self.sample_rate_hz = rt.fs;
        self.rt = Some(Box::new(rt));
        Ok(())
    }

    /// Run Random's broadband pretest at the identification level: `|H|^2`,
    /// coherence and the pipeline lag on the line grid. `pretest_frames` 0
    /// takes the length the grid and the target degrees of freedom need. State
    /// [`MixedState::SysId`] while it runs; cancelled by
    /// [`Self::sysid_cancel_flag`].
    pub fn run_sysid(&mut self, pretest_frames: usize) -> Result<(), MixedError> {
        let _ = pretest_frames;
        if self.rt.is_none() || self.state != MixedState::Idle {
            return Err(MixedError::BadState("identify after arm".into()));
        }
        self.set_state(MixedState::SysId);
        let id = &self.cfg.identification;
        let grms = self
            .cfg
            .floor()
            .and_then(|f| f.breakpoints.grms_of_table().ok())
            .unwrap_or(1.0);
        let mut vrms = grms * 10f64.powf(id.sysid_level_db / 20.0);
        if let Some(m) = id.pretest_max_vrms {
            vrms = vrms.min(m);
        }
        if let Some(m) = id.pretest_min_vrms {
            vrms = vrms.max(m);
        }
        vrms = vrms.min(self.cfg.limits.drive_vpk_ceiling / 5.0);
        let weights: Vec<f64> = self.cfg.control.channels.iter().map(|c| c.weight).collect();
        let trust = (id.trust_coherence, id.trust_h_floor_frac);
        let cancel = self.sysid_cancel.clone();
        let rt = self.rt.as_mut().expect("armed");
        match rt.identify(vrms, &weights, trust, &cancel) {
            Ok(()) => {
                let lag = rt.ident.lag;
                self.log.push(
                    0.0,
                    MixedEventKind::Note {
                        text: format!(
                            "identification at {vrms:.4} V rms: band coherence {:.3}, \
                             lag {lag} frames",
                            rt.ident.band_coherence
                        ),
                    },
                );
                self.set_state(MixedState::CoherenceGate);
                Ok(())
            }
            Err(e) => {
                self.set_state(MixedState::Aborted);
                self.outcome = Some(MixedOutcome::Aborted);
                Err(e)
            }
        }
    }

    /// Judge the identification's band coherence against
    /// `identification.coherence_floor`. `Ok(true)`: passed; the run holds at
    /// [`MixedState::AwaitingReview`] (with `review_hold` off the run thread
    /// proceeds at once). `Ok(false)`: parked at [`MixedState::CoherenceHold`].
    pub fn coherence_gate(&mut self) -> Result<bool, MixedError> {
        if self.state != MixedState::CoherenceGate {
            return Err(MixedError::BadState(
                "the gate follows identification".into(),
            ));
        }
        let c = self.band_coherence();
        if c >= self.cfg.identification.coherence_floor {
            self.set_state(MixedState::AwaitingReview);
            Ok(true)
        } else {
            self.set_state(MixedState::CoherenceHold);
            Ok(false)
        }
    }

    /// Leave the review hold and engage: build the floor law and one tone law
    /// per tone, seed them, and play the floor alone at the schedule's start
    /// level less the soft start ([`MixedPhase::FloorOnly`]); the floor comes
    /// in over the part ramp.
    pub fn proceed_after_review(&mut self) -> Result<(), MixedError> {
        if self.state != MixedState::AwaitingReview {
            return Err(MixedError::BadState("proceed from the review hold".into()));
        }
        let soft = 10f64.powf(-self.cfg.soft_start_db / 20.0);
        let level = self.start_gain() * soft;
        let rt = self.rt.as_mut().expect("armed");
        if let Err(e) = rt.engage(&self.cfg, &self.laws, level) {
            rt.emergency();
            self.set_state(MixedState::Aborted);
            self.outcome = Some(MixedOutcome::LawFault);
            return Err(e);
        }
        rt.relax_db = if self.cfg.soft_start_db > 0.0 {
            self.cfg.soft_start_relax_db
        } else {
            0.0
        };
        self.engaged_at = rt.step.frames;
        self.set_state(MixedState::Engaged);
        self.set_phase(MixedPhase::FloorOnly);
        self.init_histories();
        Ok(())
    }

    fn init_histories(&mut self) {
        let mut h = Histories::default();
        for p in &self.cfg.parts {
            match &p.body {
                PartBody::Tone(_) => h.tones.push(ToneHistory {
                    part: p.id.clone(),
                    ..ToneHistory::default()
                }),
                PartBody::Narrowband(_) => h.bands.push(BandHistory {
                    part: p.id.clone(),
                    ..BandHistory::default()
                }),
                _ => {}
            }
        }
        self.histories = h;
    }

    // ---- running ---------------------------------------------------------

    /// Run about a quarter second of station time: per frame, every part's
    /// clock, oscillator and tracker, and the summed drive's peak check; per
    /// control tick, each settled tone's law; between batches, the floor's
    /// estimate and law, the bands' servo and the next synthesis.
    /// Returns `true` while the run goes on, `false` once it has ended
    /// (stopped, completed or aborted).
    pub fn run_chunk(&mut self) -> Result<bool, MixedError> {
        if !self.state.is_running() {
            return Err(MixedError::BadState(format!(
                "run from {}",
                self.state.name()
            )));
        }
        let n = self.rt.as_ref().expect("running").batches_per_chunk();
        let alive = self.run_batches(n)?;
        self.sample_histories();
        Ok(alive)
    }

    /// Run `batches` station blocks (a test's finer step than a chunk). Same
    /// return as [`Self::run_chunk`].
    pub fn run_batches(&mut self, batches: u32) -> Result<bool, MixedError> {
        if !self.state.is_running() {
            return Ok(false);
        }
        let (mut peak, mut sumsq, mut n) = (0.0f64, 0.0, 0u64);
        let mut clamped = 0u64;
        for _ in 0..batches {
            self.apply_verbs();
            let rt = self.rt.as_mut().expect("running");
            let lead = rt.nfft + 16 * rt.actual.block_frames.max(1);
            rt.top_up(&self.cfg, lead);
            let station_trip = rt.run_batch()?;
            peak = peak.max(rt.step.records.peak);
            clamped += rt.step.records.clamped;
            rt.step.records.clamped = 0;
            sumsq += rt.step.records.sumsq;
            n += rt.step.records.n;
            rt.step.records.peak = 0.0;
            rt.step.records.sumsq = 0.0;
            rt.step.records.n = 0;
            self.rand_acc.0 += rt.step.records.rand_sumsq;
            self.rand_acc.1 += rt.step.records.rand_n;
            rt.step.records.rand_sumsq = 0.0;
            rt.step.records.rand_n = 0;
            let mut vrms_trip = None;
            if self.rand_acc.1 as f64 >= 0.25 * rt.fs {
                let rms = (self.rand_acc.0 / self.rand_acc.1 as f64).sqrt();
                if let Some(lim) = self.cfg.limits.drive_abort_vrms {
                    if rms >= lim {
                        vrms_trip = Some(MixedTripKind::RandomVrms {
                            realized_vrms: rms,
                            limit_vrms: lim,
                        });
                    }
                }
                self.rand_acc = (0.0, 0);
            }
            if let Some(kind) = station_trip {
                rt.emergency();
                self.latch_trip(kind, None);
                self.finish(MixedOutcome::Aborted);
                return Ok(false);
            }
            let dt = (TICKS_PER_BATCH_F * rt.actual.block_frames.max(1) as f64) / rt.fs;
            if self.ladder.started && !rt.step.clock.held() && rt.step.level.settled() {
                self.ladder.level_s += dt;
                if self.at_full() {
                    self.ladder.full_s += dt;
                }
            }
            if let Some(kind) = vrms_trip {
                self.latch_trip(kind, None);
                self.begin_end(MixedOutcome::Aborted, true);
            }
            let rt = self.rt.as_mut().expect("running");
            if let Some(t) = rt.take_step_trip() {
                let part = t.part.map(|i| self.cfg.parts[i].id.clone());
                let law_fault = matches!(t.kind, MixedTripKind::LawFault { .. });
                self.latch_trip(t.kind, part);
                self.begin_end(
                    if law_fault {
                        MixedOutcome::LawFault
                    } else {
                        MixedOutcome::Aborted
                    },
                    t.abort,
                );
            }
            let weights: Vec<f64> = self.cfg.control.channels.iter().map(|c| c.weight).collect();
            let rt = self.rt.as_mut().expect("running");
            rt.tap_records();
            if let Some((kind, part, abort)) = rt.analyse(&self.cfg, &weights) {
                let part = part.map(|i| self.cfg.parts[i].id.clone());
                let law_fault = matches!(kind, MixedTripKind::LawFault { .. });
                self.latch_trip(kind, part);
                self.begin_end(
                    if law_fault {
                        MixedOutcome::LawFault
                    } else {
                        MixedOutcome::Aborted
                    },
                    abort,
                );
            }
            self.advance_phase();
            self.flush_law_records();
            if self.ending.is_some() && self.drive_is_zero() {
                let now = self.frames();
                let lag = self.rt.as_ref().map_or(0, |r| r.step.lag() as u64);
                let at = *self.zero_at.get_or_insert(now);
                // Let the queue play the zeros out before the tasks stop.
                if now
                    >= at + lag + 2 * self.rt.as_ref().map_or(1, |r| r.actual.block_frames as u64)
                {
                    let outcome = self.ending.take().expect("ending");
                    self.finish(outcome);
                    return Ok(false);
                }
            }
        }
        self.chunk_peak = peak;
        self.chunk_clamped = clamped > 0;
        self.chunk_rms = if n > 0 {
            (sumsq / n as f64).sqrt()
        } else {
            0.0
        };
        Ok(self.state.is_running())
    }

    fn latch_trip(&mut self, kind: MixedTripKind, part: Option<String>) {
        if self.trip.is_none() {
            let trip = MixedTrip {
                kind,
                part,
                t_s: self.t_s(),
            };
            self.log
                .push(trip.t_s, MixedEventKind::Trip { trip: trip.clone() });
            self.trip = Some(trip);
        }
    }

    /// Start the end of the run: an abort ramps the drive out at once; a
    /// controlled stop ramps every part out over the shutdown ramp.
    fn begin_end(&mut self, outcome: MixedOutcome, abort: bool) {
        if self.ending.is_some() && !abort {
            return;
        }
        let rt = self.rt.as_mut().expect("running");
        if abort {
            rt.step.begin_abort();
        } else {
            let frames = (self.cfg.shutdown_ramp_s.max(self.cfg.part_ramp_s) * rt.fs).max(1.0);
            rt.step.level.go(0.0, frames);
        }
        self.ending = Some(outcome);
        self.set_phase(MixedPhase::RampingDown);
    }

    fn finish(&mut self, outcome: MixedOutcome) {
        self.flush_law_records();
        if let Some(rt) = self.rt.as_mut() {
            rt.stop_station();
            rt.step.mode = super::step::StepMode::Silent;
        }
        self.outcome = Some(outcome);
        self.set_phase(MixedPhase::Idle);
        self.set_state(match outcome {
            MixedOutcome::Completed | MixedOutcome::Stopped => MixedState::Stopped,
            MixedOutcome::Aborted | MixedOutcome::LawFault => MixedState::Aborted,
        });
    }

    fn apply_verbs(&mut self) {
        let verbs: Vec<Verb> = match self.pending.lock() {
            Ok(mut v) => std::mem::take(&mut *v),
            Err(_) => return,
        };
        for v in verbs {
            let t = self.t_s();
            let rt = self.rt.as_mut().expect("running");
            match v {
                Verb::Level(db_) => {
                    let frames = (self.cfg.level_ramp_s.max(self.cfg.part_ramp_s) * rt.fs).max(1.0);
                    rt.step.level.go(10f64.powf(db_ / 20.0), frames);
                    let started = self.ladder.started;
                    self.ladder = Ladder::at(&self.cfg.schedule.table_db, db_);
                    self.ladder.started = started;
                    self.log.push(t, MixedEventKind::Level { level_db: db_ });
                }
                Verb::Hold(h) => {
                    if h {
                        rt.step.clock.hold();
                    } else {
                        rt.step.clock.resume();
                    }
                    self.log.push(t, MixedEventKind::Hold { held: h });
                }
                Verb::Part(i, on) => {
                    for tc in rt.step.tones.iter_mut().filter(|tc| tc.part == i) {
                        tc.switched_on = on;
                    }
                    for b in rt.bands.iter_mut().filter(|b| b.part == i) {
                        b.switched_on = on;
                    }
                    rt.step.clock.set_enabled(i, on);
                    self.log.push(
                        t,
                        MixedEventKind::Part {
                            part: self.cfg.parts[i].id.clone(),
                            on,
                        },
                    );
                }
                Verb::Stop => self.begin_end(MixedOutcome::Stopped, false),
            }
        }
    }

    /// The start-up order (D9) and the run's own end.
    fn advance_phase(&mut self) {
        if self.ending.is_some() {
            return;
        }
        let start = self.start_gain();
        let fs = self.sample_rate_hz;
        let in_phase_s = self.frames().saturating_sub(self.phase_at) as f64 / fs;
        let has_parts = self.cfg.tones().next().is_some() || self.cfg.bands().next().is_some();
        let rt = self.rt.as_mut().expect("running");
        match self.phase {
            MixedPhase::FloorOnly => {
                // The soft start: once the floor has had its loops at the soft
                // level, it climbs to the start level.
                if rt.step.level.target() < start && rt.loops >= 2 {
                    let frames = (self.cfg.level_ramp_s.max(self.cfg.part_ramp_s) * fs).max(1.0);
                    rt.step.level.go(start, frames);
                }
                let at_start = rt.step.level.settled() && rt.step.level.value() >= start * 0.999;
                if at_start {
                    rt.relax_db = 0.0;
                }
                let equalized = at_start
                    && rt.floor_dof >= self.cfg.control_dof
                    && rt
                        .floor_rms_err_db
                        .is_some_and(|e| e.abs() <= FLOOR_EQUALIZED_DB);
                // The parts wait for the larger of the settle time and the
                // time this grid needs to reach the control degrees of freedom.
                let wait_s = self.cfg.floor_settle_s.max(rt.equalize_s(&self.cfg));
                let timeout = in_phase_s >= wait_s;
                if equalized || timeout {
                    if timeout && !equalized {
                        let t = self.t_s();
                        self.log.push(
                            t,
                            MixedEventKind::Note {
                                text: format!(
                                    "the floor did not equalize in {wait_s:.1} s (the larger \
                                     of its settle time and the {:.1} s this grid needs for \
                                     {} degrees of freedom); the parts come in now",
                                    self.rt.as_ref().map_or(0.0, |r| r.equalize_s(&self.cfg)),
                                    self.cfg.control_dof
                                ),
                            },
                        );
                    }
                    let rt = self.rt.as_mut().expect("running");
                    rt.step
                        .level
                        .go(start, (self.cfg.part_ramp_s * fs).max(1.0));
                    rt.relax_db = 0.0;
                    rt.step.clock_running = true;
                    self.set_phase(if has_parts {
                        MixedPhase::PartsRampingIn
                    } else {
                        MixedPhase::Running
                    });
                }
            }
            MixedPhase::PartsRampingIn => {
                if in_phase_s > 2.0 * self.cfg.part_ramp_s.max(0.1) && rt.parts_settled() {
                    self.set_phase(MixedPhase::Running);
                }
            }
            MixedPhase::Running => {
                if !self.ladder.started {
                    self.ladder =
                        Ladder::at(&self.cfg.schedule.table_db, self.cfg.schedule.start_db);
                    self.ladder.started = true;
                }
                self.climb();
                let rt = self.rt.as_mut().expect("running");
                let done = has_parts && rt.step.clock.all_parts_done() && rt.parts_settled();
                let timed = self
                    .cfg
                    .full_level_duration_s
                    .is_some_and(|d| self.ladder.full_s >= d);
                if done || timed {
                    self.begin_end(MixedOutcome::Completed, false);
                }
            }
            _ => {}
        }
        if self.state == MixedState::Engaged
            && self.phase == MixedPhase::Running
            && self
                .rt
                .as_ref()
                .is_some_and(|r| r.step.level.value() >= 0.999)
        {
            self.set_state(MixedState::FullLevel);
        }
    }

    /// Whether the level in force is the schedule's last rung.
    fn at_full(&self) -> bool {
        let t = &self.cfg.schedule.table_db;
        self.ladder.on_rung && self.ladder.rung + 1 == t.len()
    }

    /// The automatic ladder: after the dwell at the present level, the next
    /// rung (from a level between rungs, the rung above it, dwelling the rung
    /// below's time), every part together, as one ramp.
    fn climb(&mut self) {
        if self.run_mode != RunMode::Auto || self.at_full() {
            return;
        }
        let sch = &self.cfg.schedule;
        let l = self.ladder;
        let dwell = sch.dwell_for(if l.on_rung {
            l.rung
        } else {
            l.rung.saturating_sub(1)
        });
        if l.level_s < dwell {
            return;
        }
        let next = if l.on_rung { l.rung + 1 } else { l.rung };
        let Some(&db_) = sch.table_db.get(next) else {
            return;
        };
        let t = self.t_s();
        let rt = self.rt.as_mut().expect("running");
        let frames = (self.cfg.level_ramp_s.max(self.cfg.part_ramp_s) * rt.fs).max(1.0);
        rt.step.level.go(10f64.powf(db_ / 20.0), frames);
        self.ladder = Ladder {
            rung: next,
            on_rung: true,
            level_s: 0.0,
            full_s: 0.0,
            started: true,
        };
        self.log.push(t, MixedEventKind::Level { level_db: db_ });
    }

    fn sample_histories(&mut self) {
        let t = self.t_s();
        let st = self.live_status();
        if self.summaries.len() != st.parts.len() {
            self.summaries = st
                .parts
                .iter()
                .map(|r| super::status::PartSummary {
                    id: r.id.clone(),
                    kind: r.kind,
                    target: r.target,
                    measured: None,
                    error_db: None,
                    worst_error_db: None,
                    alarm_s: 0.0,
                })
                .collect();
        }
        for (sm, r) in self.summaries.iter_mut().zip(&st.parts) {
            sm.target = r.target;
            if r.measured.is_some() {
                sm.measured = r.measured;
            }
            if let Some(e) = r.error_db {
                sm.error_db = Some(e);
                if sm.worst_error_db.is_none_or(|w| e.abs() > w.abs()) {
                    sm.worst_error_db = Some(e);
                }
            }
            if r.alarm {
                sm.alarm_s += super::run::CHUNK_S;
            }
        }
        let (mut ti, mut bi) = (0, 0);
        for row in &st.parts {
            match row.kind {
                super::status::PartKind::Tone => {
                    let h = &mut self.histories.tones[ti];
                    ti += 1;
                    h.t_s.push(t);
                    h.freq_hz.push(row.freq_hz.unwrap_or(0.0) as f32);
                    h.target_g.push(row.target as f32);
                    h.measured_g
                        .push(row.measured.map_or(f32::NAN, |m| m as f32));
                    h.drive_vpk.push(row.drive as f32);
                    trim(&mut h.t_s);
                    trim(&mut h.freq_hz);
                    trim(&mut h.target_g);
                    trim(&mut h.measured_g);
                    trim(&mut h.drive_vpk);
                }
                super::status::PartKind::Narrowband => {
                    let h = &mut self.histories.bands[bi];
                    bi += 1;
                    h.t_s.push(t);
                    h.center_hz.push(row.freq_hz.unwrap_or(0.0) as f32);
                    h.reference.push(row.target as f32);
                    h.measured.push(row.measured.map_or(f32::NAN, |m| m as f32));
                    h.dof.push(row.dof.unwrap_or(0.0) as f32);
                    trim(&mut h.t_s);
                    trim(&mut h.center_hz);
                    trim(&mut h.reference);
                    trim(&mut h.measured);
                    trim(&mut h.dof);
                }
                _ => {}
            }
        }
    }

    /// Hand the raw control samples since the last call to `on_block`
    /// (`block`, interleaved one sample per control channel per frame, and
    /// `stride` = control channels) and release them: the run thread's tap for
    /// time plots.
    pub fn service_analysis_tapped<F>(&mut self, on_block: F)
    where
        F: FnMut(&[f32], usize),
    {
        let mut on_block = on_block;
        if let Some(rt) = self.rt.as_mut() {
            let stride = rt.tap_stride();
            let tap = std::mem::take(&mut rt.tap);
            if !tap.is_empty() {
                on_block(&tap, stride);
            }
            rt.tap = tap;
            rt.tap.clear();
        }
    }

    // ---- verbs (posted to the step; take effect at its next batch) --------

    fn post(&self, v: Verb) -> Result<(), MixedError> {
        if !self.state.is_running() {
            return Err(MixedError::BadState("the run is not driving".into()));
        }
        self.pending
            .lock()
            .map_err(|_| MixedError::BadState("the verb queue is poisoned".into()))?
            .push(v);
        Ok(())
    }

    fn level_db_now(&self) -> f64 {
        self.rt
            .as_ref()
            .map_or(self.cfg.schedule.start_db, |r| db(r.step.level.target()))
    }

    /// Step the level by `delta_db` (every part moves by the same dB, as a
    /// ramp).
    pub fn step_level(&self, delta_db: f64) -> Result<(), MixedError> {
        self.set_level_db(self.level_db_now() + delta_db)
            .map(|_| ())
    }

    /// Jump to rung `i` of the level table.
    pub fn jump_to_level(&self, i: usize) -> Result<(), MixedError> {
        let db_ = *self
            .cfg
            .schedule
            .table_db
            .get(i)
            .ok_or_else(|| MixedError::BadState(format!("no level rung {i}")))?;
        self.set_level_db(db_).map(|_| ())
    }

    /// Go to `db` against full (clamped to the table's range); returns the
    /// level taken.
    pub fn set_level_db(&self, db_: f64) -> Result<f64, MixedError> {
        if !db_.is_finite() {
            return Err(MixedError::BadState("a level is a finite dB".into()));
        }
        let (lo, hi) = self.full_db_range();
        let taken = db_.clamp(lo, hi);
        self.post(Verb::Level(taken))?;
        Ok(taken)
    }

    /// Set how the schedule advances (automatic or manual). Allowed before
    /// and during the run.
    pub fn set_run_mode(&mut self, mode: RunMode) -> Result<(), MixedError> {
        if self.state.is_terminal() {
            return Err(MixedError::BadState("the run has ended".into()));
        }
        self.run_mode = mode;
        Ok(())
    }

    /// Hold test progress: every part's clock stops (sweeps, dwell timers,
    /// windows); every part keeps playing at its frequency and level, under
    /// control.
    pub fn hold(&self) -> Result<(), MixedError> {
        self.post(Verb::Hold(true))
    }

    /// Resume test progress; every clock restarts together.
    pub fn resume(&self) -> Result<(), MixedError> {
        self.post(Verb::Hold(false))
    }

    /// Switch the part `part` on or off during the run, as a ramp at the part
    /// ramp's rate. The floor cannot be switched off.
    pub fn set_part_enabled(&self, part: &str, on: bool) -> Result<(), MixedError> {
        let i = self
            .cfg
            .index_of(part)
            .ok_or_else(|| MixedError::BadState(format!("no part {part}")))?;
        if matches!(self.cfg.parts[i].body, PartBody::Floor(_)) {
            return Err(MixedError::BadState(
                "the floor cannot be switched off".into(),
            ));
        }
        self.post(Verb::Part(i, on))
    }

    /// Change a live parameter `key` of one law to `value`; returns the value
    /// taken. For [`LawRole::Tones`] it changes every tone's instance.
    pub fn adjust(
        &mut self,
        role: LawRole,
        key: &str,
        value: f64,
    ) -> Result<f64, MixedAdjustRefusal> {
        let Some(rt) = self.rt.as_mut().filter(|r| r.floor_law.is_some()) else {
            return Err(MixedAdjustRefusal::NotEngaged);
        };
        match role {
            LawRole::Floor => Err(MixedAdjustRefusal::NotLive),
            LawRole::Tones => {
                let d = self.laws.tones.describe();
                let Some(live) = d.live.iter().find(|l| l.key == key) else {
                    return Err(if d.params.iter().any(|(k, _)| k == key) {
                        MixedAdjustRefusal::NotLive
                    } else {
                        MixedAdjustRefusal::UnknownKey
                    });
                };
                if !(value >= live.min && value <= live.max) {
                    return Err(MixedAdjustRefusal::OutOfRange {
                        min: live.min,
                        max: live.max,
                    });
                }
                for t in rt.step.tones.iter_mut() {
                    t.law
                        .adjust(key, value)
                        .map_err(|e| MixedAdjustRefusal::LawRefused(e.to_string()))?;
                }
                Ok(value)
            }
        }
    }

    /// Ask for the controlled stop: every part ramps out over
    /// `shutdown_ramp_s` (at least the part ramp), then the run ends
    /// [`MixedOutcome::Stopped`]. The run thread keeps calling
    /// [`Self::run_chunk`] until it returns `false`.
    pub fn request_stop(&self) -> Result<(), MixedError> {
        self.post(Verb::Stop)
    }

    /// Stop now, ramping every part out within this call, and end the run.
    pub fn stop(&mut self) -> Result<(), MixedError> {
        if !self.state.is_running() {
            if !self.state.is_terminal() && self.state != MixedState::Idle {
                if let Some(rt) = self.rt.as_mut() {
                    rt.stop_station();
                }
                self.outcome = Some(MixedOutcome::Stopped);
                self.set_state(MixedState::Stopped);
            }
            return Ok(());
        }
        self.begin_end(MixedOutcome::Stopped, false);
        for _ in 0..100_000 {
            if !self.run_batches(1)? {
                break;
            }
        }
        Ok(())
    }

    /// Abort: the drive ramps to zero over [`super::step::ABORT_RAMP_S`] at
    /// once, the run ends [`MixedOutcome::Aborted`] with `reason` logged.
    /// Always succeeds before engage (there is nothing to take down) and after
    /// the end.
    pub fn abort(&mut self, reason: &str) -> Result<(), MixedError> {
        let t = self.t_s();
        self.log.push(
            t,
            MixedEventKind::Note {
                text: format!("abort: {reason}"),
            },
        );
        if !self.state.is_running() {
            if !self.state.is_terminal() && self.state != MixedState::Idle {
                if let Some(rt) = self.rt.as_mut() {
                    rt.emergency();
                }
                self.outcome = Some(MixedOutcome::Aborted);
                self.set_state(MixedState::Aborted);
            }
            return Ok(());
        }
        self.begin_end(MixedOutcome::Aborted, true);
        for _ in 0..100_000 {
            if !self.run_batches(1)? {
                break;
            }
        }
        Ok(())
    }

    // ---- capture -------------------------------------------------------------

    /// Start a capture named `run_name` under the capture root; returns its
    /// folder. The control channels are captured at the run's rate, and the
    /// law records (`law_ticks.bin`, described by `law.json`) are written
    /// beside them while it runs.
    pub fn start_capture(&mut self, run_name: &str) -> Result<PathBuf, MixedError> {
        let root = self
            .data_root
            .clone()
            .unwrap_or_else(hertzforge_capture::default_data_root);
        let cfg_json =
            serde_json::to_string(&self.cfg).map_err(|e| MixedError::Io(e.to_string()))?;
        let n = self.cfg.control.channels.len();
        let table = self.table.clone();
        let tones: Vec<String> = self.cfg.tones().map(|(p, _)| p.id.clone()).collect();
        let law = serde_json::json!({
            "format": super::law_records::LAW_TICKS_FORMAT,
            "record_bytes": super::law_records::MixedLawRecord::BYTES,
            "fields": super::law_records::LAW_TICK_FIELDS
                .iter()
                .map(|(n, t)| serde_json::json!({"name": n, "type": t}))
                .collect::<Vec<_>>(),
            "roles": ["floor", "tone"],
            "tones": tones,
            "tone_misses": super::step::TONE_MISS_NAMES,
            "floor_miss": "1: the call was not used (its reason is in the run's log)",
            "floor_law": self.cfg.law.floor.id,
            "tone_law": self.laws.tones.describe().id,
        });
        let rt = self
            .rt
            .as_mut()
            .ok_or_else(|| MixedError::BadState("capture after arm".into()))?;
        let sink = hertzforge_capture::CaptureSink::start(
            &root,
            run_name,
            hertzforge_capture::CaptureConfig::at_rate(n, rt.fs),
            table,
            cfg_json,
        )
        .map_err(|e| MixedError::Io(e.to_string()))?;
        let dir = sink.run_dir().to_path_buf();
        std::fs::write(
            dir.join(super::law_records::LAW_JSON_FILE),
            serde_json::to_vec_pretty(&law).map_err(|e| MixedError::Io(e.to_string()))?,
        )
        .map_err(|e| MixedError::Io(e.to_string()))?;
        std::fs::write(dir.join(super::law_records::LAW_TICKS_FILE), [])
            .map_err(|e| MixedError::Io(e.to_string()))?;
        rt.capture = Some(sink);
        self.capture_dir = Some(dir.clone());
        Ok(dir)
    }

    /// Stop the capture: its law records are flushed and the recording is
    /// finished, which is what marks it complete. A recording that is only
    /// dropped reads as one cut short.
    pub fn stop_capture(&mut self) -> Result<(), MixedError> {
        self.flush_law_records();
        let sink = self.rt.as_mut().and_then(|rt| rt.capture.take());
        self.capture_dir = None;
        if let Some(sink) = sink {
            sink.finish().map_err(|e| MixedError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Take the laws' records from the step and the floor loop; append them
    /// to the capture's `law_ticks.bin` while a capture runs.
    fn flush_law_records(&mut self) {
        let Some(rt) = self.rt.as_mut() else { return };
        for r in rt.step.law_log.drain(..).chain(rt.floor_log.drain(..)) {
            if let Some(kept) = self.law_kept.as_mut() {
                kept.push(r);
            }
            if self.capture_dir.is_some() {
                r.write_to(&mut self.law_bytes);
            }
        }
        if let Some(dir) = &self.capture_dir {
            if !self.law_bytes.is_empty() {
                use std::io::Write;
                let path = dir.join(super::law_records::LAW_TICKS_FILE);
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
                    if f.write_all(&self.law_bytes).is_ok() {
                        self.law_bytes.clear();
                    }
                }
            }
        }
    }

    /// Save a snapshot of the spectra and status with `marks`; returns its
    /// path (`snapshots/` under the capture root).
    pub fn save_snapshot(&mut self, marks: Vec<Mark>) -> Result<PathBuf, MixedError> {
        let root = self
            .data_root
            .clone()
            .unwrap_or_else(hertzforge_capture::default_data_root)
            .join("snapshots");
        std::fs::create_dir_all(&root).map_err(|e| MixedError::Io(e.to_string()))?;
        let marks: Vec<serde_json::Value> = marks
            .iter()
            .map(|m| {
                serde_json::json!({"ch": m.ch, "freq_hz": m.freq_hz, "value": m.value,
                    "label": m.label})
            })
            .collect();
        let snap = serde_json::json!({
            "kind": "mixed_snapshot",
            "t_s": self.t_s(),
            "status": self.status(),
            "display": self.display(),
            "marks": marks,
        });
        let path = root.join(format!("mixed_{}_{:.3}.json", self.sysid_seed, self.t_s()));
        std::fs::write(
            &path,
            serde_json::to_vec(&snap).map_err(|e| MixedError::Io(e.to_string()))?,
        )
        .map_err(|e| MixedError::Io(e.to_string()))?;
        Ok(path)
    }

    // ---- reading -------------------------------------------------------------

    /// The full status (every part's row, the floor, the totals, the laws).
    pub fn status(&self) -> MixedStatus {
        self.live_status()
    }

    /// The status as of the last batch, cheap to call each chunk.
    pub fn live_status(&self) -> MixedStatus {
        let mut s = MixedStatus::idle(&self.cfg);
        s.state = self.state;
        s.phase = self.phase;
        s.outcome = self.outcome;
        s.trip = self.trip.clone();
        s.run_mode = self.run_mode;
        s.held = self.is_held();
        let Some(rt) = self.rt.as_ref() else {
            return s;
        };
        s.band_coherence = rt.ident.band_coherence;
        s.overloads = rt.step.overloads.clone();
        s.input_rms = rt.step.input_rms.clone();
        s.capturing = rt.capture.is_some();
        s.trusted_frac = rt.ident.trusted_frac;
        if rt.floor_law.is_none() {
            return s;
        }
        let lev = rt.step.level.value();
        s.t_s = self.t_s();
        s.test_s = rt.step.clock.t_s();
        s.level_db = db(lev);
        s.rung_index = self.ladder.rung;
        s.at_last_level = self.at_full();
        let now = rt.step.clock.now();
        for (i, row) in s.parts.iter_mut().enumerate() {
            let n = now[i];
            row.on = n.on;
            row.done = n.done;
            row.sweeps_done = n.sweeps_done;
            row.direction = n.direction;
            if n.freq_hz.is_some() {
                row.freq_hz = n.freq_hz;
            }
        }
        // The floor.
        let rgain = lev * rt.step.random.value();
        if let Some(fi) = self
            .cfg
            .parts
            .iter()
            .position(|p| matches!(p.body, PartBody::Floor(_)))
        {
            let row = &mut s.parts[fi];
            row.on = true;
            row.scale = rt.step.random.value();
            row.enabled = true;
            row.target *= lev;
            row.measured = rt.floor_rms_g.map(|g| g * lev);
            row.error_db = rt.floor_rms_err_db;
            row.settled = rt.loops > 0;
            row.alarm = rt.alarm_lines > 0;
            row.drive = (rt.floor_drive.iter().sum::<f64>() * rt.df).sqrt() * rgain;
            row.misses = rt.floor_misses;
            row.capped = rt.floor_tally.capped;
            row.limiting = rt.notched.iter().any(|n| *n) || rt.rms_held;
        }
        // Tones.
        for (j, tc) in rt.step.tones.iter().enumerate() {
            let row = &mut s.parts[tc.part];
            let v = rt.step.bank.voice(j);
            let f = tc.last_freq;
            row.freq_hz = Some(f);
            row.target = tc.amp.at(f) * lev;
            row.measured = tc.measured.map(|m| m * lev);
            row.error_db = tc.error_db;
            row.drive = v.amplitude_now() * v.scale() * lev;
            row.settled = tc.usable;
            row.alarm = tc.alarm;
            row.scale = v.scale();
            row.enabled = tc.switched_on;
            row.on = row.on && tc.switched_on;
            row.rate = Some(match self.cfg.parts[tc.part].schedule() {
                Some(PartSchedule::Sweep {
                    law: SweepLaw::LogOctPerMin,
                    ..
                }) if f > 0.0 => tc.rate / (f * std::f64::consts::LN_2) * 60.0,
                _ => tc.rate,
            });
            row.misses = tc.misses_in_row;
            row.capped = tc.tally.capped;
            row.limiting = tc.limiting;
            row.held = tc.limiting;
            if let Some(jd) = tc.judging {
                row.judge_s = Some(jd.judge_s);
                row.judged_alarm_db = Some(jd.alarm_db);
                row.judged_abort_db = Some(jd.abort_db);
                row.tolerance_widened = jd.widened;
            }
            row.judged_error_db = tc.judged_db;
        }
        // Bands.
        for bc in &rt.bands {
            let row = &mut s.parts[bc.part];
            let c = bc.center_now;
            let (lo, hi) = bc.width.edges_at(c);
            row.freq_hz = Some(c);
            row.width_hz = Some(hi - lo);
            row.gain_db = Some(bc.servo.gain_db);
            row.scale = bc.scale.value();
            row.enabled = bc.switched_on;
            let l2 = lev * lev;
            row.target = bc.level.reference().unwrap_or(bc.level_g2) * l2;
            row.measured = bc.level.measured().map(|m| m * l2);
            row.error_db = bc.error_db;
            row.dof = Some(bc.level.dof());
            row.settled = bc.level.frames() > 0.0;
            row.alarm = bc.alarm;
            row.capped = bc.capped;
            row.limiting = bc.limiting;
        }
        s.floor = FloorSummary {
            rms_target_g: s.floor.rms_target_g * lev,
            rms_g: rt.floor_rms_g.map(|g| g * lev),
            error_db_rms: rt.floor_rms_err_db,
            lines_out: rt.lines_out,
            alarm_lines: rt.alarm_lines,
            abort_lines: rt.abort_lines,
            dof: rt.floor_dof_typical,
            lines_in_band: rt.band.1 - rt.band.0,
            rms_judged_lines: rt.rms_judged_lines,
        };
        let control_rms = (!rt.last_composite.is_empty())
            .then(|| (rt.last_composite.iter().sum::<f64>() * rt.df).sqrt());
        s.totals = MixedTotals {
            control_rms_g: control_rms,
            drive_vpk: self.chunk_peak,
            drive_vrms: self.chunk_rms,
            clamped: self.chunk_clamped,
        };
        s.law = self.law_status();
        s
    }

    /// The display: spectra on the line grid and each tone's and band's
    /// history. Empty before the loop engages.
    pub fn display(&self) -> MixedDisplay {
        let Some(rt) = self.rt.as_ref().filter(|r| r.floor_law.is_some()) else {
            return MixedDisplay {
                identification: self.frf(),
                ..MixedDisplay::default()
            };
        };
        let lev = rt.step.level.value();
        let l2 = lev * lev;
        let f32v = |v: &[f64], k: f64| v.iter().map(|x| (x * k) as f32).collect::<Vec<f32>>();
        let mut reference: Vec<f64> = rt.target.clone();
        let mut w = vec![0.0; rt.lines];
        for b in &rt.bands {
            if b.scale.value() > 0.0 {
                let (lo, hi) = b.width.edges_at(b.center_now);
                crate::band::band_weights(0.0, rt.df, lo, hi, &mut w);
                for (r, wk) in reference.iter_mut().zip(&w) {
                    *r += b.level_g2 * wk * b.scale.value().powi(2);
                }
            }
        }
        let floor = self.cfg.floor().expect("validated");
        let around = |d: f64| f32v(&reference, l2 * 10f64.powf(d / 10.0));
        MixedDisplay {
            grid: LineGrid {
                f0_hz: 0.0,
                df_hz: rt.df,
                lines: rt.lines,
            },
            control: f32v(&rt.last_composite, 1.0),
            residual: if rt.last_measured.is_empty() {
                f32v(&rt.last_residual_composite, 1.0)
            } else {
                f32v(&rt.last_measured, l2)
            },
            channels: self
                .cfg
                .control
                .channels
                .iter()
                .enumerate()
                .map(|(c, ch)| MixedDisplayChannel {
                    channel_index: ch.channel_index,
                    control: f32v(&rt.last_control[c], 1.0),
                    residual: f32v(&rt.last_residual[c], 1.0),
                })
                .collect(),
            reference: f32v(&reference, l2),
            alarm_lo: around(-floor.alarm_db.0),
            alarm_hi: around(floor.alarm_db.1),
            abort_lo: around(-floor.abort_db.0),
            abort_hi: around(floor.abort_db.1),
            drive: f32v(&rt.floor_drive, l2),
            excluded: rt.last_excluded.clone(),
            tones: self.histories.tones.clone(),
            bands: self.histories.bands.clone(),
            identification: self.frf(),
        }
    }

    /// The identification on the line grid; `None` before it has run.
    pub fn frf(&self) -> Option<MixedFrf> {
        let rt = self.rt.as_ref()?;
        if rt.ident.h_mag2.is_empty() {
            return None;
        }
        Some(MixedFrf {
            h_mag2: rt.ident.h_mag2.iter().map(|x| *x as f32).collect(),
            coherence: rt.ident.coherence.iter().map(|x| *x as f32).collect(),
            lag_frames: rt.ident.lag,
            band_coherence: rt.ident.band_coherence,
            trusted: rt.ident.trusted.clone(),
            trusted_frac: rt.ident.trusted_frac,
        })
    }

    /// The laws' counters; `None` before the laws are built.
    pub fn law_status(&self) -> Option<MixedLawStatus> {
        let rt = self.rt.as_ref().filter(|r| r.floor_law.is_some())?;
        Some(MixedLawStatus {
            floor_id: self.cfg.law.floor.id.clone(),
            tones_id: self.cfg.law.tones.id.clone(),
            floor: rt.floor_tally,
            tones: rt
                .step
                .tones
                .iter()
                .map(|t| (self.cfg.parts[t.part].id.clone(), t.tally))
                .collect(),
        })
    }

    /// The floor law's counters (zero before engage).
    pub fn floor_law_tally(&self) -> RoleTally {
        self.rt
            .as_ref()
            .map_or_else(RoleTally::default, |r| r.floor_tally)
    }

    /// Test progress, 0 to 1, when the run's end is known (a full-level
    /// time); `None` otherwise.
    pub fn progress(&self) -> Option<f64> {
        let d = self.cfg.full_level_duration_s?;
        (self.phase == MixedPhase::Running).then(|| {
            let s = self.frames().saturating_sub(self.phase_at) as f64 / self.sample_rate_hz;
            (s / d).clamp(0.0, 1.0)
        })
    }

    /// The run's record.
    pub fn report_data(&self) -> MixedReport {
        MixedReport {
            config: self.cfg.clone(),
            sample_rate_hz: self.sample_rate_hz,
            state: self.state,
            outcome: self.outcome,
            trip: self.trip.clone(),
            status: self.status(),
            events: self.log.events().to_vec(),
            parts: self.summaries.clone(),
        }
    }
}
