//! A simulated SRS shock engine for wire tests: the protocol's states, verbs,
//! timings and refusals without the controller's service, as the classical shock
//! wire tests run on theirs. A run never uses it unless the service is given it
//! ([`crate::server::ServiceOptions::srs_engine`]).
//!
//! The plant is a wire at [`GAIN_V_PER_G`]: every pulse plays the offline synthesis
//! at the level fired and measures it, its SRS a hair under the target at that
//! level in every band, and passes. Identification takes its burst's time at the
//! given pace; a fire takes its record's. The ladder is the controller's own
//! (`ShockLadderState`), so levels, steps, rungs and completion are the
//! controller's arithmetic.

use super::config::{self, WireSrs};
use super::engine::{
    ArmOutcome, ArmPlan, Armed, EngineContext, EngineEnd, Fired, Identified, LevelMove, LogLine,
    Position, Seeded, SrsEngine, StationFacts,
};
use super::preview::{self, Synthesis};
use super::view::{
    ComponentFigures, FrfFigures, IdentFacts, IdentFigures, LawCall, LawReading, LimitFigures,
    Notch, Prediction, PulseFigures, Row, SrsFigures, StationRef, Trip, TripFacts,
};
use crate::protocol::{num, ErrorCode, MethodError};
use hertzforge_shock_common::{
    pulse_feasibility, FeasibilityTolerances, ShockLadderState, ShockRunMode,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The simulated plant's drive per g of response.
pub const GAIN_V_PER_G: f64 = 0.02;
/// Each pulse measures its target times this.
const MEASURED_RATIO: f64 = 0.99;

/// The simulated engine.
pub struct SimulatedEngine {
    ctx: EngineContext,
    cfg: Option<Arc<WireSrs>>,
    synthesis: Option<Synthesis>,
    ladder: Option<ShockLadderState>,
    vpk_full: Option<f64>,
    identified: bool,
    fired: u64,
    passed: u64,
    armed_at: Option<Instant>,
    log: Vec<LogLine>,
}

impl SimulatedEngine {
    pub fn new(ctx: EngineContext) -> SimulatedEngine {
        SimulatedEngine {
            ctx,
            cfg: None,
            synthesis: None,
            ladder: None,
            vpk_full: None,
            identified: false,
            fired: 0,
            passed: 0,
            armed_at: None,
            log: Vec::new(),
        }
    }

    fn cfg(&self) -> Result<&WireSrs, MethodError> {
        self.cfg
            .as_deref()
            .ok_or_else(|| MethodError::internal("the simulated run is not armed"))
    }

    /// Sleep `s` simulated seconds at the context's pace, in short steps; false when
    /// the run's cancel was raised.
    fn pace(&self, s: f64) -> bool {
        if self.ctx.sim_speed <= 0.0 || s <= 0.0 {
            return !self.ctx.signals.cancelled();
        }
        let end = Instant::now() + Duration::from_secs_f64(s / self.ctx.sim_speed);
        while Instant::now() < end {
            if self.ctx.signals.cancelled() {
                return false;
            }
            std::thread::sleep(
                end.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(10)),
            );
        }
        !self.ctx.signals.cancelled()
    }

    fn record_len(&self) -> usize {
        match (&self.cfg, &self.synthesis) {
            (Some(c), Some(s)) => {
                s.waveform_g.len() + config::ring_out_samples(c.ring_out_s, c.fs_hz())
            }
            _ => 0,
        }
    }

    fn completed(&self) -> Option<EngineEnd> {
        let auto = self
            .cfg
            .as_ref()
            .is_some_and(|c| c.mode == ShockRunMode::Auto);
        (auto && self.ladder.as_ref().is_some_and(|l| l.completed())).then(|| EngineEnd {
            outcome: "completed",
            message: "The schedule finished.".into(),
            trip: None,
        })
    }
}

impl SrsEngine for SimulatedEngine {
    fn arm(&mut self, plan: ArmPlan) -> Result<ArmOutcome, MethodError> {
        let cfg = plan.cfg.clone();
        let syn = preview::synthesize(&cfg.synthesis);
        let feasibility = match &cfg.feasibility {
            None => (
                "not_evaluated",
                "No shaker was given, so the pulse was not checked against shaker ratings."
                    .to_owned(),
            ),
            Some(f) => config::verdict(&pulse_feasibility(
                &f.shaker,
                &f.moving_mass,
                &syn.waveform_g,
                cfg.fs_hz(),
                FeasibilityTolerances {
                    residual_velocity_ips: f.residual_velocity_ips,
                    residual_disp_in: f.residual_disp_in,
                },
            )),
        };
        if feasibility.0 == "exceeded" && plan.ack.is_none() {
            return Ok(ArmOutcome::Blocked(feasibility));
        }
        let ladder = ShockLadderState::new(cfg.schedule())
            .map_err(|e| MethodError::named(ErrorCode::Refused, "arm_refused", e.to_string()))?;
        self.ladder = Some(ladder);
        self.synthesis = Some(syn);
        self.cfg = Some(cfg.clone());
        self.armed_at = Some(Instant::now());
        Ok(ArmOutcome::Armed(Armed {
            fs_hz: cfg.fs_hz(),
            record_len: self.record_len(),
            station: StationRef {
                kind: "sim",
                class: "sim",
                devices: Vec::new(),
            },
            feasibility,
            lock: None,
        }))
    }

    fn identify(&mut self) -> Result<Identified, MethodError> {
        let (secs, cap, floor, review, excitation) = {
            let c = self.cfg()?;
            (
                4.0 * c.sysid_nfft as f64 / c.fs_hz(),
                c.identification_vpk_cap(),
                c.min_mean_coherence,
                c.review,
                config::excitation_name(c.excitation),
            )
        };
        self.pace(secs);
        self.identified = true;
        let peak = self.synthesis.as_ref().map_or(0.0, |s| s.reference.peak_g)
            * GAIN_V_PER_G
            * 10f64.powf(self.cfg()?.sysid_level_db / 20.0);
        Ok(Identified::Done {
            facts: IdentFacts {
                excitation,
                drive_vpk: peak.min(cap),
                max_vpk: cap,
                coherence_mean: 0.998,
                min_mean_coherence: floor,
                passed: true,
            },
            review,
        })
    }

    fn seed(&mut self) -> Result<Seeded, MethodError> {
        let c = self.cfg()?;
        let s = self
            .synthesis
            .as_ref()
            .ok_or_else(|| MethodError::internal("the simulated run has no synthesis"))?;
        let vpk = s.reference.peak_g * GAIN_V_PER_G;
        let (ceiling, abort) = (c.drive_vpk_ceiling, c.drive_vpk_abort);
        let full = c.levels_db.last().copied().unwrap_or(0.0);
        let at_full = 10f64.powf(full / 20.0);
        if c.mode == ShockRunMode::Auto && vpk * at_full > abort {
            return Ok(Seeded::Ended(EngineEnd {
                outcome: "aborted",
                message: format!(
                    "The identified drive needs {} V pk at full level; the drive abort is {} V \
                     pk. Nothing was fired.",
                    num(config::round(vpk * at_full, 3)),
                    num(abort)
                ),
                trip: Some(Trip {
                    facts: TripFacts::DriveVpk {
                        vpk: vpk * at_full,
                        abort_vpk: abort,
                        predicted: true,
                    },
                    pulse: 0,
                    at_level_db: Some(full),
                }),
            }));
        }
        let prediction = Prediction {
            drive_vpk: vpk * at_full,
            drive_vpk_ceiling: ceiling,
            drive_vpk_abort: abort,
            response_peak_g: s.reference.peak_g * at_full,
            velocity_pk_ips: s.reference.velocity_pk_ips * at_full,
            displacement_pk_pk_in: s.reference.displacement_pk_pk_in * at_full,
        };
        self.vpk_full = Some(vpk);
        Ok(Seeded::Ready(prediction))
    }

    fn fire(&mut self) -> Result<Fired, MethodError> {
        let cfg = self
            .cfg
            .clone()
            .ok_or_else(|| MethodError::internal("the simulated run is not armed"))?;
        let vpk_full = self
            .vpk_full
            .ok_or_else(|| MethodError::internal("the simulated law is not seeded"))?;
        let level = self
            .ladder
            .as_ref()
            .ok_or_else(|| MethodError::internal("the simulated run has no ladder"))?
            .current_level_db();
        let scale = 10f64.powf(level / 20.0);
        let wanted = vpk_full * scale;
        let limit_scale = if wanted > cfg.drive_vpk_ceiling {
            cfg.drive_vpk_ceiling / wanted
        } else {
            1.0
        };
        let n = self.record_len() as u64;
        let fs = cfg.fs_hz();
        self.ctx.progress.start(n);
        let blocks = 8u64;
        for b in 1..=blocks {
            // A pulse in flight plays out whole: the committed queue is longer.
            let _ = self.pace(n as f64 / fs / blocks as f64);
            self.ctx.progress.block(n * b / blocks, n);
        }
        self.ctx.progress.end();
        let commanded = scale * limit_scale;
        let s = self
            .synthesis
            .as_ref()
            .ok_or_else(|| MethodError::internal("the simulated run has no synthesis"))?;
        let ring = config::ring_out_samples(cfg.ring_out_s, fs);
        let mut record: Vec<f64> = s.waveform_g.iter().map(|v| v * commanded).collect();
        record.extend(std::iter::repeat_n(0.0, ring));
        let measured: Vec<f64> = record.iter().map(|v| v * MEASURED_RATIO).collect();
        let drive: Vec<f64> = s
            .waveform_g
            .iter()
            .map(|v| v * commanded * GAIN_V_PER_G)
            .collect();
        let target: Vec<f64> = s.target_g.iter().map(|t| t * scale).collect();
        let measured_srs: Vec<f64> = s
            .target_g
            .iter()
            .map(|t| t * commanded * MEASURED_RATIO)
            .collect();
        let env = cfg.envelope_bands(s.bands_hz.len());
        let worst_band = s.bands_hz.len().saturating_sub(1);
        let worst_db = 20.0 * (commanded * MEASURED_RATIO / scale).log10();
        let index = self.fired;
        self.fired += 1;
        self.passed += 1;
        if let Some(l) = self.ladder.as_mut() {
            l.on_pulse_judged(true);
        }
        let limits: Vec<LimitFigures> = cfg
            .limit_channels
            .iter()
            .map(|l| LimitFigures {
                name: format!("ch{}", l.channel),
                bands_hz: s.bands_hz.clone(),
                predicted: target.iter().map(|t| 0.5 * t).collect(),
                bound: match &l.bound {
                    config::WireBound::Flat { accel_g } => vec![*accel_g; s.bands_hz.len()],
                    config::WireBound::PerBand { bounds_g } => bounds_g
                        .iter()
                        .map(|b| b.unwrap_or(f64::INFINITY))
                        .collect(),
                },
                headroom_db: cfg.headroom_db,
            })
            .collect();
        let row = Row {
            index,
            scheduled_db: level,
            commanded_db: level + 20.0 * limit_scale.log10(),
            limit_scale,
            drive_vpk: wanted * limit_scale,
            peak_g: Some(s.reference.peak_g * commanded * MEASURED_RATIO),
            velocity_pk_ips: Some(s.reference.velocity_pk_ips * commanded * MEASURED_RATIO),
            displacement_pk_pk_in: Some(
                s.reference.displacement_pk_pk_in * commanded * MEASURED_RATIO,
            ),
            worst_err_db: Some(worst_db),
            worst_band_hz: s.bands_hz.get(worst_band).copied(),
            alarm_bands: Some(0),
            abort_bands: Some(0),
            passed: Some(true),
            limited_components: (!limits.is_empty()).then_some(0),
            worst_limit_db: (!limits.is_empty()).then_some(-6.0),
            at_s: self.armed_at.map_or(0.0, |t| t.elapsed().as_secs_f64()),
            law: Some(LawCall {
                used: true,
                miss: None,
                call_ms: 0.1,
            }),
        };
        let kin = hertzforge_dsp::shock::pulse_kinematic_series(&measured, fs);
        let figures = PulseFigures {
            fs_hz: fs,
            srs: Some(SrsFigures {
                bands_hz: s.bands_hz.clone(),
                edges: preview::envelope_edges(&target, &env),
                target,
                measured: Some(measured_srs),
            }),
            measured: Some(measured),
            drive,
            velocity: Some(kin.velocity_ips),
            displacement: Some(kin.disp_in),
            components: Some(ComponentFigures {
                freqs_hz: s.components.component_freqs(),
                amplitude_v: s
                    .components
                    .amplitudes()
                    .iter()
                    .map(|a| a * GAIN_V_PER_G)
                    .collect(),
                gains: vec![1.0; s.components.component_freqs().len()],
            }),
            limits,
        };
        self.log.push((
            "pulse",
            format!(
                "pulse {} at {} dB: worst band {} dB, passed",
                index + 1,
                num(config::round(level, 2)),
                num(config::round(worst_db, 2))
            ),
            Some(
                serde_json::json!({"pulse": index, "level_db": level, "measured": true,
                                    "passed": true}),
            ),
        ));
        Ok(Fired {
            rows: vec![row],
            figures: vec![figures],
            end: self.completed(),
        })
    }

    fn set_level(&mut self, to: LevelMove) -> Result<f64, MethodError> {
        let ladder = self
            .ladder
            .as_mut()
            .ok_or_else(|| MethodError::internal("the simulated run has no ladder"))?;
        match to {
            LevelMove::Step(d) if d > 0.0 => ladder.step_up(),
            LevelMove::Step(_) => ladder.step_down(),
            LevelMove::Rung(i) => ladder.jump_to_level(i).map_err(|e| {
                MethodError::named(ErrorCode::BadRequest, "not_a_rung", e.to_string())
            })?,
        }
        Ok(ladder.current_level_db())
    }

    fn predicted_vpk_at(&self, level_db: f64) -> Option<f64> {
        self.vpk_full.map(|v| v * 10f64.powf(level_db / 20.0))
    }

    fn stop(&mut self) {}

    fn abort(&mut self, _reason: &str) {}

    fn position(&self) -> Position {
        let Some(l) = &self.ladder else {
            return Position::default();
        };
        let s = l.schedule();
        let level = l.current_level_db();
        let on = s
            .table_db
            .get(l.level_index())
            .is_some_and(|x| (x - level).abs() < 1e-9);
        Position {
            level_db: level,
            rung: on.then_some(l.level_index()),
            at_level: l.pulses_at_level(),
            of_level: if s.mode == ShockRunMode::Auto {
                s.pulses_for(l.level_index())
            } else {
                0
            },
            passed: self.passed,
        }
    }

    fn identification(&self) -> Option<IdentFigures> {
        let c = self.cfg.as_deref()?;
        if !self.identified {
            return None;
        }
        let b = &c.synthesis.bands;
        let n = 32;
        let freqs: Vec<f64> = (0..n)
            .map(|k| b.f_lo_hz * (b.f_hi_hz / b.f_lo_hz).powf(k as f64 / (n - 1) as f64))
            .collect();
        Some(IdentFigures {
            gain: vec![1.0 / GAIN_V_PER_G; n],
            floor: c.h_floor_frac / GAIN_V_PER_G,
            coherence: vec![0.998; n],
            hold: c.min_mean_coherence,
            unit: "g".into(),
            limit_frf: c
                .limit_channels
                .iter()
                .map(|l| FrfFigures {
                    name: format!("ch{}", l.channel),
                    freqs_hz: l
                        .frf
                        .as_ref()
                        .map_or_else(|| freqs.clone(), |f| f.0.clone()),
                    mag: l
                        .frf
                        .as_ref()
                        .map_or_else(|| vec![1.0 / GAIN_V_PER_G; n], |f| f.1.clone()),
                    measured: l.frf.is_none(),
                })
                .collect(),
            freqs_hz: freqs,
        })
    }

    fn notch(&self) -> Option<Notch> {
        let c = self.cfg.as_deref()?;
        (!c.limit_channels.is_empty()).then_some(Notch {
            notched_bands: 0,
            worst_before_db: Some(-6.0),
            worst_after_db: Some(-6.0),
            converged: true,
        })
    }

    fn law_values(&self) -> Vec<LawReading> {
        if self.vpk_full.is_none() {
            return Vec::new();
        }
        vec![LawReading {
            label: "Worst band error".into(),
            value: 20.0 * MEASURED_RATIO.log10(),
            unit: "dB".into(),
        }]
    }

    fn misses(&self) -> u32 {
        0
    }

    fn take_station_facts(&mut self) -> Option<StationFacts> {
        Some(StationFacts {
            source: "SIM",
            station: serde_json::json!({"kind": "sim", "class": "sim", "label": "simulated engine",
                                        "backend": "none", "devices": [], "clock": "shared"}),
            sim: Some(serde_json::json!({"model": "simulated_engine",
                                         "parameters": {"gain_v_per_g": GAIN_V_PER_G},
                                         "seed": null})),
            build: serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "commit": null,
                                      "dirty": null}),
            output_latency: serde_json::json!({"frames": null, "how": "unknown"}),
            links: None,
        })
    }

    fn take_log(&mut self) -> Vec<LogLine> {
        std::mem::take(&mut self.log)
    }
}
