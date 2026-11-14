//! A simulated TWR engine for wire tests that exercise the protocol without the
//! controller's service: it keeps the order of things a real run keeps
//! (identification, the review, a ladder of correction plays, playback, the
//! coherence hold, the ceiling refusals) and plays each record as a loop of blocks
//! that reads the run's cancel flag at every block, so stop, abort and the pause
//! while nobody holds control behave as they do on a station. None of its numbers
//! is control math: each correction play halves the error, a level gates under
//! the gate, the response is the target scaled by the error.
//!
//! A run never uses it unless the service is given it
//! ([`crate::server::simulated_twr_engine`]).

use super::config::{excitation_name, WireTwr};
use super::engine::{
    level_words, ArmOutcome, ArmPlan, Armed, EngineContext, EngineEnd, Identified, LevelMove,
    Played, Position, Proceeded, TwrEngine,
};
use super::reference::{self, Prepared};
use super::view::{
    IdentFacts, IdentFigures, MaskFigures, PlayRecords, PlayRow, Prediction, StationRef, Trip,
    TripFacts,
};
use crate::protocol::{num, ErrorCode, MethodError};
use hertzforge_twr::schedule::{GateFailPolicy, TwrRunMode};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Frames a simulated play advances per block.
pub const BLOCK: u64 = 256;
/// The rest pad behind each simulated record, frames.
pub const PAD: u64 = 1024;
/// A simulated identification at a drive under this, volts rms, does not reach
/// its coherence.
pub const HOLD_BELOW_VRMS: f64 = 1e-3;
/// The error of the first correction play at a level, percent.
pub const FIRST_ERROR_PCT: f64 = 25.0;
/// The error of a playback play, percent.
pub const PLAYBACK_ERROR_PCT: f64 = 2.0;

/// The simulated engine.
pub struct SimulatedEngine {
    ctx: EngineContext,
    cfg: Option<Arc<WireTwr>>,
    prepared: Option<Prepared>,
    fs: f64,
    level_db: f64,
    rung: Option<usize>,
    at_level: u32,
    gated: bool,
    error_pct: f64,
    playing: bool,
    holding: bool,
    frozen: bool,
    playback: u32,
    plays: u64,
    clocked: u64,
    log: Vec<(&'static str, String)>,
}

impl SimulatedEngine {
    pub fn new(ctx: EngineContext) -> SimulatedEngine {
        SimulatedEngine {
            ctx,
            cfg: None,
            prepared: None,
            fs: 1.0,
            level_db: 0.0,
            rung: None,
            at_level: 0,
            gated: false,
            error_pct: FIRST_ERROR_PCT,
            playing: false,
            holding: false,
            frozen: false,
            playback: 0,
            plays: 0,
            clocked: 0,
            log: Vec::new(),
        }
    }

    fn cfg(&self) -> Result<&WireTwr, MethodError> {
        self.cfg
            .as_deref()
            .ok_or_else(|| MethodError::internal("the simulated run is not armed"))
    }

    fn record_len(&self) -> u64 {
        self.prepared.as_ref().map_or(0, |p| p.grid.n as u64)
    }

    fn scale(level_db: f64) -> f64 {
        10f64.powf(level_db / 20.0)
    }

    /// The full-level section's peak: on the simulated wire, 1 V a unit.
    fn full_vpk(&self) -> f64 {
        self.prepared.as_ref().map_or(0.0, |p| {
            p.target.iter().fold(0.0f64, |m, &v| m.max(v.abs()))
        })
    }

    /// Play `frames` frames and the rest pad in blocks, reading the cancel flag at
    /// every block, at the simulator's pace. `Err(done)` when cancelled.
    fn run_frames(&mut self, frames: u64) -> Result<(), u64> {
        let total = frames + PAD;
        let t0 = Instant::now();
        self.ctx.progress.start(total);
        let mut done = 0;
        while done < total {
            if self.ctx.signals.cancelled() {
                self.ctx.progress.end();
                self.clocked += done;
                return Err(done);
            }
            done = (done + BLOCK).min(total);
            self.ctx.progress.block(done, total);
            if self.ctx.sim_speed > 0.0 && self.fs > 0.0 {
                let due = done as f64 / self.fs / self.ctx.sim_speed;
                let ahead = due - t0.elapsed().as_secs_f64();
                if ahead > 0.0 {
                    std::thread::sleep(Duration::from_secs_f64(ahead.min(0.05)));
                }
            }
        }
        self.ctx.progress.end();
        self.clocked += total;
        Ok(())
    }

    fn levels(&self) -> Vec<f64> {
        self.cfg
            .as_ref()
            .map_or_else(Vec::new, |c| c.levels_db.clone())
    }

    fn records(&self, level_db: f64, err: f64) -> Option<PlayRecords> {
        let p = self.prepared.as_ref()?;
        let s = Self::scale(level_db);
        let target: Vec<f64> = p.target.iter().map(|v| v * s).collect();
        let response: Vec<f64> = target.iter().map(|v| v * (1.0 + err / 100.0)).collect();
        let cfg = self.cfg.as_ref()?;
        Some(PlayRecords {
            fs_hz: self.fs,
            drive: target.clone(),
            target,
            response,
            alarm_frac: cfg.envelope_alarm_frac,
            abort_frac: cfg.envelope_abort_frac,
            spectra: None,
        })
    }

    fn not_converged(&self, cfg: &WireTwr, level: f64, err: f64) -> EngineEnd {
        EngineEnd {
            outcome: "not_converged",
            message: format!(
                "{} did not gate within {} correction plays.",
                level_words(level),
                cfg.max_iterations
            ),
            trip: Some(Trip {
                facts: TripFacts::NotConverged {
                    iterations: u64::from(self.at_level),
                    error_pct: err,
                    gate_pct: cfg.gate_pct,
                },
                play: Some(self.plays - 1),
                at_level_db: Some(level),
            }),
        }
    }
}

impl TwrEngine for SimulatedEngine {
    fn arm(&mut self, plan: ArmPlan) -> Result<ArmOutcome, MethodError> {
        // No law runs here: the simulated plays follow their own rule.
        let _ = plan.law;
        let cfg = plan.cfg.clone();
        let fs = cfg.shape.fs_for(plan.stored.fs_hz);
        let band = cfg.shape.band(cfg.sysid_band_hz);
        let prepared = reference::prepare(&plan.stored, &cfg.shape, fs, band)
            .map_err(|i| MethodError::named(ErrorCode::Refused, "arm_refused", i.message))?;
        let feas = prepared.feasibility.clone();
        if feas.blocked() && plan.ack.is_none() {
            return Ok(ArmOutcome::Blocked((feas.verdict, feas.message)));
        }
        self.fs = fs;
        self.level_db = cfg.levels_db.first().copied().unwrap_or(0.0);
        self.rung = Some(0);
        let record_len = prepared.grid.n;
        let (section_start, section_len) = (prepared.grid.pad_pre, prepared.grid.section_len);
        self.prepared = Some(prepared);
        self.cfg = Some(cfg);
        self.log
            .push(("note", "the simulated station is armed".into()));
        Ok(ArmOutcome::Armed(Box::new(Armed {
            fs_hz: fs,
            record_len,
            section_start,
            section_len,
            station: StationRef {
                kind: "sim",
                class: "sim",
                devices: Vec::new(),
            },
            feasibility: (feas.verdict, feas.message),
            lock: None,
            tap: None,
            units: plan.stored.units.clone(),
        })))
    }

    fn identify(&mut self) -> Result<Identified, MethodError> {
        let cfg = self.cfg()?.clone();
        let frames = (cfg.sysid_duration_s * self.fs).round() as u64;
        if self.run_frames(frames).is_err() {
            return Err(MethodError::internal("identification was stopped"));
        }
        let held = cfg.sysid_drive_vrms < HOLD_BELOW_VRMS;
        let facts = IdentFacts {
            excitation: excitation_name(cfg.excitation),
            drive_vrms: cfg.sysid_drive_vrms,
            drive_vpk: 4.5 * cfg.sysid_drive_vrms,
            coherence_mean: if held { 0.5 } else { 0.9999 },
            min_mean_coherence: cfg.min_mean_coherence,
            coverage: (!held).then_some(0.999),
            lag_frames: Some(4.0),
            lag_source: "measured",
            passed: !held,
        };
        if held {
            return Ok(Identified::Held {
                facts: Some(facts),
                message: format!(
                    "Identification did not reach its coherence ({} in-band mean); raise its \
                     drive or its length, or check the response path, then reset.",
                    num(cfg.min_mean_coherence)
                ),
            });
        }
        Ok(Identified::Done {
            facts,
            review: cfg.review,
        })
    }

    fn proceed(&mut self) -> Result<Proceeded, MethodError> {
        let cfg = self.cfg()?.clone();
        let full = self.full_vpk();
        if cfg.mode == TwrRunMode::Auto && full > cfg.drive_vpk_ceiling {
            return Ok(Proceeded::Ended(EngineEnd {
                outcome: "aborted",
                message: format!(
                    "The full-level drive needs {} V pk; the drive ceiling is {} V pk. Nothing \
                     was played for it.",
                    num(crate::shock::view::sig(full, 4)),
                    num(cfg.drive_vpk_ceiling)
                ),
                trip: Some(Trip {
                    facts: TripFacts::DriveVpk {
                        vpk: full,
                        ceiling_vpk: cfg.drive_vpk_ceiling,
                        predicted: true,
                    },
                    play: None,
                    at_level_db: Some(cfg.levels_db.last().copied().unwrap_or(0.0)),
                }),
            }));
        }
        Ok(Proceeded::Ready(Some(Prediction {
            drive_vpk: full * Self::scale(self.level_db),
            drive_vpk_ceiling: cfg.drive_vpk_ceiling,
            level_db: self.level_db,
        })))
    }

    fn play(&mut self) -> Result<Played, MethodError> {
        let cfg = self.cfg()?.clone();
        let start = self.clocked;
        let frames = self.record_len();
        if let Err(done) = self.run_frames(frames) {
            return Ok(Played {
                cancelled_at: Some(done),
                ..Played::default()
            });
        }
        let level = self.level_db;
        let playback = self.playing;
        let err = if playback {
            PLAYBACK_ERROR_PCT
        } else {
            self.at_level += 1;
            self.error_pct
        };
        let gated = err <= cfg.gate_pct;
        let vpk = self.full_vpk() * Self::scale(level);
        let row = PlayRow {
            index: self.plays,
            playback,
            level_db: level,
            rung: self.rung,
            iteration: (!playback).then_some(self.at_level),
            rms_error_pct: err,
            peak_error_pct: 2.0 * err,
            spectral_error_db: err / 20.0,
            gated,
            clipped_samples: 0,
            clamped_bins: 0,
            drive_vpk: vpk,
            response_peak: vpk * (1.0 + err / 100.0),
            lag_frames: None,
            drift_frames: None,
            start_frame: start,
            frames: frames + PAD,
            at_s: start as f64 / self.fs,
            law: None,
        };
        // As the core's note: the simulated device takes a block before start.
        self.log.push((
            "note",
            format!(
                "Play {}: {BLOCK} frames committed before start.",
                self.plays
            ),
        ));
        self.plays += 1;
        let records = self.records(level, err);
        let mut end = None;
        if playback {
            self.playback += 1;
            if self.playback >= self.position().of_playback {
                end = Some(EngineEnd {
                    outcome: "completed",
                    message: format!(
                        "The playback schedule finished: {} play{} at {}.",
                        self.playback,
                        if self.playback == 1 { "" } else { "s" },
                        level_words(level)
                    ),
                    trip: None,
                });
            }
        } else if gated {
            self.gated = true;
            if cfg.mode == TwrRunMode::Auto {
                let levels = self.levels();
                let next = self.rung.map_or(levels.len(), |r| r + 1);
                if next < levels.len() {
                    self.level_db = levels[next];
                    self.rung = Some(next);
                    self.at_level = 0;
                    self.gated = false;
                    self.error_pct = FIRST_ERROR_PCT;
                } else {
                    self.playing = true;
                    self.playback = 0;
                }
            }
        } else {
            if !self.frozen {
                self.error_pct /= 2.0;
            }
            if u64::from(self.at_level) >= cfg.max_iterations {
                match cfg.on_gate_fail {
                    GateFailPolicy::Abort => end = Some(self.not_converged(&cfg, level, err)),
                    GateFailPolicy::Hold => self.holding = true,
                }
            }
        }
        Ok(Played {
            row: Some(row),
            records,
            end,
            cancelled_at: None,
        })
    }

    fn begin_playback(&mut self) -> Result<(), MethodError> {
        if !self.gated {
            return Err(MethodError::named(
                ErrorCode::Refused,
                "not_converged",
                format!(
                    "Playback begins once the present level has gated; {} has not.",
                    level_words(self.level_db)
                ),
            ));
        }
        self.playing = true;
        self.playback = 0;
        Ok(())
    }

    fn set_level(&mut self, to: LevelMove) -> Result<f64, MethodError> {
        let levels = self.levels();
        let target = match to {
            LevelMove::Step(d) => self.level_db + d,
            LevelMove::Rung(i) => levels.get(i).copied().unwrap_or(self.level_db),
        };
        let ceiling = self.cfg()?.drive_vpk_ceiling;
        let vpk = self.full_vpk() * Self::scale(target);
        if vpk > ceiling {
            return Err(MethodError::named(
                ErrorCode::Refused,
                "drive_ceiling",
                format!(
                    "The drive in force would need {} V pk at {}; the drive ceiling is {} V pk. \
                     The level and the drive are unchanged.",
                    num(crate::shock::view::sig(vpk, 4)),
                    level_words(target),
                    num(ceiling)
                ),
            )
            .with_detail(json!({
                "vpk": num(crate::shock::view::sig(vpk, 4)),
                "ceiling_vpk": num(ceiling),
                "level_db": num(crate::shock::config::round(target, 2)),
            })));
        }
        self.level_db = target;
        self.rung = levels.iter().position(|l| (l - target).abs() < 1e-9);
        self.at_level = 0;
        self.gated = false;
        self.holding = false;
        self.error_pct = FIRST_ERROR_PCT;
        Ok(target)
    }

    fn predicted_vpk_at(&self, level_db: f64) -> Option<f64> {
        self.prepared.as_ref()?;
        Some(self.full_vpk() * Self::scale(level_db))
    }

    fn freeze(&mut self, frozen: bool) {
        self.frozen = frozen;
    }

    fn stop(&mut self) {}

    fn abort(&mut self, _reason: &str) {}

    fn position(&self) -> Position {
        let (of_level, of_playback) = match &self.cfg {
            Some(c) => {
                let section_s = self
                    .prepared
                    .as_ref()
                    .map_or(1.0, |p| p.section.len() as f64 / self.fs);
                (
                    c.max_iterations.min(u64::from(u32::MAX)) as u32,
                    c.run.core().plays_target(section_s),
                )
            }
            None => (0, 0),
        };
        Position {
            level_db: self.level_db,
            rung: self.rung,
            at_level: self.at_level,
            of_level,
            playback: self.playback,
            of_playback,
            gated: self.gated,
            playing: self.playing,
            holding: self.holding,
            frozen: self.frozen,
            alarm: false,
        }
    }

    fn identification(&self) -> Option<IdentFigures> {
        let cfg = self.cfg.as_ref()?;
        let (lo, hi) = cfg.sysid_band_hz;
        let freqs: Vec<f64> = (0..8).map(|i| lo + (hi - lo) * i as f64 / 7.0).collect();
        Some(IdentFigures {
            gain: vec![1.0; freqs.len()],
            phase_deg: vec![0.0; freqs.len()],
            coherence: vec![0.9999; freqs.len()],
            freqs_hz: freqs,
            coherence_floor: cfg.coherence_floor,
            min_mean_coherence: cfg.min_mean_coherence,
            unit: "g/V".into(),
        })
    }

    fn mask(&self) -> Option<MaskFigures> {
        let p = self.prepared.as_ref()?;
        let peak = p
            .spectrum
            .iter()
            .copied()
            .fold(0.0f64, f64::max)
            .max(f64::MIN_POSITIVE);
        let (lo, hi) = p.band_hz;
        let keep: Vec<usize> = (0..p.spectrum.len())
            .filter(|&k| {
                let f = p.grid.freq(k);
                f >= lo && f <= hi
            })
            .step_by(64)
            .collect();
        Some(MaskFigures {
            freqs_hz: keep.iter().map(|&k| p.grid.freq(k)).collect(),
            trusted: vec![1.0; keep.len()],
            reference: keep.iter().map(|&k| p.spectrum[k] / peak).collect(),
        })
    }

    fn level_target(&self) -> Option<PlayRecords> {
        self.records(self.level_db, 0.0).map(|mut r| {
            r.response.clear();
            r.drive.clear();
            r
        })
    }

    fn misses(&self) -> u32 {
        0
    }

    fn frames_clocked(&self) -> u64 {
        self.clocked
    }

    fn take_log(&mut self) -> Vec<(&'static str, String)> {
        std::mem::take(&mut self.log)
    }

    fn take_station_facts(&mut self) -> Option<crate::shock::record::StationFacts> {
        Some(crate::shock::record::StationFacts {
            source: "SIM",
            station: serde_json::json!({"kind": "sim", "class": "sim", "label": "simulated engine",
                                        "backend": "none", "devices": [], "clock": "shared"}),
            sim: Some(
                serde_json::json!({"model": "simulated_engine", "parameters": {},
                                         "seed": null}),
            ),
            build: serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "commit": null,
                                      "dirty": null}),
            output_latency: serde_json::json!({"frames": null, "how": "unknown"}),
            links: None,
        })
    }

    fn full_level_target(&self) -> Option<Vec<f64>> {
        let p = self.prepared.as_ref()?;
        Some(p.target.clone())
    }
}
