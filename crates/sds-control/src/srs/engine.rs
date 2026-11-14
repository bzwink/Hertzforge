//! The seam between the run thread and whatever runs the pulses: [`SrsEngine`].
//!
//! The run thread ([`super::run`]) owns the order of things on the wire (arm,
//! start, review, the time between pulses, the hold while nobody holds control,
//! stop and abort, every publish); an engine owns the pulses themselves. The engine
//! a run uses in service is [`ServiceEngine`], the controller's `SrsShockService`
//! over the station's backend: identification, the seed, the law, the caps, the
//! guards, the ladder and every judgement are the controller's. Wire tests that
//! exercise the protocol without a controller run give the service
//! [`super::sim::SimulatedEngine`] instead.

use super::config::{self, WireSrs};
use super::record::{IdentRecord, ShotRecording};
use super::view::{
    ComponentFigures, FrfFigures, IdentFacts, IdentFigures, LawCall, LawReading, LimitFigures,
    Notch, Prediction, PulseFigures, Row, SrsFigures, Trip, TripFacts,
};
use crate::protocol::{num, ErrorCode, MethodError};
use crate::pulse::observe::{Observed, Watch};
use crate::station::{self, class_name, NiScanner, StationPlan};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_law_host::DiagnosticSpec;
use hertzforge_shock_common::ShockRunMode;
use hertzforge_srs::service::plant::{channel_display_name, FirePath, HalPulsePlant};
use hertzforge_srs::service::report::{
    ConvergenceLimit, EventKind, PulseRow, RunState, TripKind, TripRecord,
};
use hertzforge_srs::{SrsLawFactory, SrsShockService};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub use crate::shock::engine::{ArmOutcome, Armed, EngineContext, LevelMove, LogLine, Position};
pub use crate::shock::record::StationFacts;

/// What a run arms on, taken from the service when the request arrives.
pub struct ArmPlan {
    pub cfg: Arc<WireSrs>,
    pub table: ChannelTable,
    pub station: StationPlan,
    pub scanner: Arc<dyn NiScanner>,
    /// The service's local data directory: the hardware lock lives here.
    pub data_root: PathBuf,
    pub bind: String,
    /// The run's law, resolved by the service from the catalog.
    pub law: Result<Arc<dyn SrsLawFactory>, MethodError>,
    /// The values the law reports, as its manifest declares them.
    pub law_values: Vec<DiagnosticSpec>,
    /// The operator's acknowledgement to arm past a feasibility exceedance.
    pub ack: Option<String>,
    /// The devices of the last scan, for the record's models and serial numbers.
    pub devices: Vec<hertzforge_hal::DiscoveredDevice>,
    /// Whether the record keeps the streams, and how many seconds of samples
    /// their buffers hold before blocks are dropped.
    pub streams: bool,
    pub stream_buffer_s: f64,
    /// The station frame, set by the station wrapper after every read.
    pub station_frame: Arc<AtomicU64>,
    /// The shots the station wrapper has begun.
    pub shots: Arc<AtomicU64>,
}

/// How identification ended.
pub enum Identified {
    /// Done: hold for review, or go on.
    Done { facts: IdentFacts, review: bool },
    /// The run is held before any pulse.
    Held {
        facts: Option<IdentFacts>,
        message: String,
    },
}

/// The engine ended the run: its outcome, words and trip.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineEnd {
    pub outcome: &'static str,
    pub message: String,
    pub trip: Option<Trip>,
}

/// How seeding the law ended.
pub enum Seeded {
    Ready(Prediction),
    Ended(EngineEnd),
}

/// What one fire did.
#[derive(Default)]
pub struct Fired {
    pub rows: Vec<Row>,
    pub figures: Vec<PulseFigures>,
    /// The engine ended the run on it.
    pub end: Option<EngineEnd>,
}

/// What a run's engine does. Every call is made on the run thread; a fire blocks it
/// for the whole record.
pub trait SrsEngine: Send {
    /// Feasibility, the station, the arm gate and the grid at the rate the station
    /// programmed. Nothing is driven.
    fn arm(&mut self, plan: ArmPlan) -> Result<ArmOutcome, MethodError>;
    /// Identification: one cancellable fire.
    fn identify(&mut self) -> Result<Identified, MethodError>;
    /// Seed the law, plan the notch and predict the full-level drive.
    fn seed(&mut self) -> Result<Seeded, MethodError>;
    /// Fire one pulse at the present level.
    fn fire(&mut self) -> Result<Fired, MethodError>;
    /// Move the level; the new level, dB.
    fn set_level(&mut self, to: LevelMove) -> Result<f64, MethodError>;
    /// The identified drive's peak at `level_db`, volts, once seeded.
    fn predicted_vpk_at(&self, level_db: f64) -> Option<f64>;
    /// The controlled stop: park, nothing more fires.
    fn stop(&mut self);
    /// The emergency stop.
    fn abort(&mut self, reason: &str);
    fn position(&self) -> Position;
    fn identification(&self) -> Option<IdentFigures>;
    /// The predicted full-level drive and response for the table in force, once
    /// seeded; `None` leaves the state's prediction as the seed made it.
    fn prediction(&self) -> Option<Prediction> {
        None
    }
    fn notch(&self) -> Option<Notch>;
    fn law_values(&self) -> Vec<LawReading>;
    /// Unusable law calls in a row now.
    fn misses(&self) -> u32;
    /// Log lines since the last call, each with its facts for the record.
    fn take_log(&mut self) -> Vec<LogLine>;
    /// Let the station go after an arm the run refused (its record could not be
    /// written): nothing has been driven.
    fn release(&mut self) {}
    /// What the run's record says of the station armed, and its streams' links;
    /// taken once, after an arm.
    fn take_station_facts(&mut self) -> Option<StationFacts> {
        None
    }
    /// The identification as the record keeps it, once done.
    fn ident_record(&self) -> Option<IdentRecord> {
        None
    }
    /// When the device started the last pulse, as the controller stamped it once
    /// the station's clock ran: the moment the time between pulses is held from.
    /// `None` leaves the run thread its own moment of asking for the pulse.
    fn last_pulse_start(&self) -> Option<Instant> {
        None
    }
    /// How far ahead of a pulse's due time the run thread asks for it in `auto`:
    /// the controller brings the shot up (arms the station, commits the record)
    /// and waits out the rest of the time between pulses itself, so only the
    /// start lands on the due time. Zero asks at the due time.
    fn lead(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
    /// Each pulse's bring-up since the last call, timed, by pulse index, for the
    /// service log. An engine that times nothing has none.
    fn take_bring_ups(&mut self) -> Vec<(u64, hertzforge_shock_common::BringUp)> {
        Vec::new()
    }
}

/// Makes a run's engine.
pub type EngineFactory = Arc<dyn Fn(&EngineContext) -> Box<dyn SrsEngine> + Send + Sync>;

/// The engine a run uses when the service is given none: the controller's.
pub fn service_engine_factory() -> EngineFactory {
    Arc::new(|ctx| Box::new(ServiceEngine::new(ctx.clone())))
}

fn arm_refused(words: impl std::fmt::Display) -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "arm_refused",
        format!("The station refused to arm: {words}"),
    )
}

fn fault(words: impl std::fmt::Display) -> MethodError {
    MethodError::internal(format!("the SRS shock service failed: {words}"))
}

/// A level in words: "full level" at 0 dB, else its dB.
fn level_words(db: f64) -> String {
    if db.abs() < 1e-9 {
        "full level".to_owned()
    } else {
        format!("{} dB", num(config::round(db, 2)))
    }
}

fn sig3(x: f64) -> serde_json::Value {
    num(super::view::sig(x, 3))
}

/// A controller pulse row on the wire.
pub fn row_of(p: &PulseRow) -> Row {
    let measured = p.passed.is_some();
    Row {
        index: p.seq,
        scheduled_db: p.scheduled_db,
        commanded_db: p.commanded_db,
        limit_scale: p.limit_scale,
        drive_vpk: p.drive_vpk.unwrap_or(f64::NAN),
        peak_g: measured.then_some(p.peak_accel_g),
        velocity_pk_ips: measured.then_some(p.peak_velocity_ips),
        displacement_pk_pk_in: measured.then_some(p.pk_pk_disp_in),
        worst_err_db: p.worst_err_db,
        worst_band_hz: p.worst_band_hz,
        alarm_bands: measured.then_some(p.alarm_bands),
        abort_bands: measured.then_some(p.abort_bands),
        passed: p.passed,
        limited_components: p.worst_limit_db.map(|_| p.limited_components),
        worst_limit_db: p.worst_limit_db,
        at_s: p.at_s.unwrap_or(0.0),
        law: p.law.as_ref().map(|l| LawCall {
            used: l.used,
            miss: l.miss.as_ref().map(|f| f.reason()),
            call_ms: l.call_ms,
        }),
    }
}

/// A controller prediction on the wire.
fn prediction_of(p: &hertzforge_srs::service::report::DrivePrediction) -> Prediction {
    Prediction {
        drive_vpk: p.drive_vpk,
        drive_vpk_ceiling: p.drive_vpk_ceiling,
        drive_vpk_abort: p.drive_vpk_abort,
        response_peak_g: p.response_peak_g,
        velocity_pk_ips: p.velocity_pk_ips,
        displacement_pk_pk_in: p.displacement_pk_pk_in,
    }
}

/// The table row a controller channel name is, by description or `ch<index>`.
fn row_index(table: &ChannelTable, name: &str) -> u32 {
    table
        .channels
        .iter()
        .find(|c| channel_display_name(c.index, &c.description) == name)
        .map_or(0, |c| c.index)
}

/// A controller trip on the wire: the end it makes, its words and its trip.
/// `predicted_db` is the level a drive found over the abort at the seed names (the
/// top of an automatic schedule, the present manual level).
pub fn end_of_trip(t: &TripRecord, predicted_db: f64, table: &ChannelTable) -> EngineEnd {
    let at = t.at_level_db;
    let trip = |facts, pulse: u64, at: f64| {
        Some(Trip {
            facts,
            pulse,
            at_level_db: Some(at),
        })
    };
    let aborted = |message: String, trip| EngineEnd {
        outcome: "aborted",
        message,
        trip,
    };
    let p = t.at_pulse;
    match &t.kind {
        TripKind::Envelope {
            abort_bands,
            worst_band_freq_hz,
            worst_err_db,
            ..
        } => aborted(
            format!(
                "Pulse {} left the abort band at {} in {abort_bands} band{}; the worst was {} Hz, \
                 {} dB off the target.",
                p + 1,
                level_words(at),
                if *abort_bands == 1 { "" } else { "s" },
                sig3(*worst_band_freq_hz),
                num(config::round(*worst_err_db, 2))
            ),
            trip(
                TripFacts::Envelope {
                    abort_bands: *abort_bands,
                    worst_band_hz: *worst_band_freq_hz,
                    worst_err_db: *worst_err_db,
                },
                p,
                at,
            ),
        ),
        TripKind::Kinematic {
            axis,
            value,
            limit,
            predicted,
        } => {
            let axis: &'static str = match axis.as_str() {
                "accel" => "accel",
                "velocity" => "velocity",
                _ => "displacement",
            };
            aborted(
                if *predicted {
                    format!(
                        "Pulse {} at {} would pass the {axis} ceiling: {} predicted against {}. \
                         Nothing was fired for it.",
                        p + 1,
                        level_words(at),
                        sig3(*value),
                        sig3(*limit)
                    )
                } else {
                    format!(
                        "Pulse {} passed the {axis} ceiling: {} against {}.",
                        p + 1,
                        sig3(*value),
                        sig3(*limit)
                    )
                },
                trip(
                    TripFacts::Kinematic {
                        axis,
                        value: *value,
                        limit: *limit,
                        predicted: *predicted,
                    },
                    p,
                    at,
                ),
            )
        }
        TripKind::DriveBreach {
            channel,
            peak,
            ceiling,
            predicted: true,
        } => {
            let _ = channel;
            aborted(
                format!(
                    "Pulse {}'s predicted peak is {} g, at or over the abort factor times the {} g \
                     ceiling. Nothing was fired for it.",
                    p + 1,
                    sig3(*peak),
                    sig3(*ceiling)
                ),
                trip(
                    TripFacts::Accel {
                        predicted_g: *peak,
                        limit_g: *ceiling,
                    },
                    p,
                    at,
                ),
            )
        }
        TripKind::DriveBreach {
            channel,
            peak,
            ceiling,
            predicted: false,
        } => aborted(
            format!(
                "Pulse {} drove {channel} to {}, at or over the abort factor times its ceiling of \
                 {}.",
                p + 1,
                sig3(*peak),
                sig3(*ceiling)
            ),
            trip(
                TripFacts::Monitor {
                    channel: row_index(table, channel),
                    value: *peak,
                    limit: *ceiling,
                },
                p,
                at,
            ),
        ),
        TripKind::Plant { message } => EngineEnd {
            outcome: "fault",
            message: format!("The station failed: {message}"),
            trip: None,
        },
        TripKind::DriveVpk {
            vpk,
            abort_vpk,
            predicted,
        } => {
            let level = if *predicted { predicted_db } else { at };
            let v = num(config::round(*vpk, 3));
            let a = num(config::round(*abort_vpk, 3));
            aborted(
                if *predicted {
                    format!(
                        "The identified drive needs {v} V pk at {}; the drive abort is {a} V pk. \
                         Nothing was fired.",
                        level_words(level)
                    )
                } else {
                    format!(
                        "Pulse {}'s drive would have been {v} V pk at {}, over the {a} V pk drive \
                         abort; it was not played.",
                        p + 1,
                        level_words(level)
                    )
                },
                trip(
                    TripFacts::DriveVpk {
                        vpk: *vpk,
                        abort_vpk: *abort_vpk,
                        predicted: *predicted,
                    },
                    p,
                    level,
                ),
            )
        }
        TripKind::NotConverged { limit, pulses } => EngineEnd {
            outcome: "not_converged",
            message: match limit {
                ConvergenceLimit::PerLevel => format!(
                    "{pulses} pulses at {} without every band in the alarm band; the run did not \
                     converge.",
                    level_words(at)
                ),
                ConvergenceLimit::Run => {
                    format!("The run's limit of {pulses} pulses came before the schedule finished.")
                }
            },
            trip: trip(
                TripFacts::NotConverged {
                    limit: limit.as_str(),
                    pulses: *pulses,
                },
                p.saturating_sub(1),
                at,
            ),
        },
        TripKind::LawFault { fault, consecutive } => {
            EngineEnd {
                outcome: "law_fault",
                message: if p == 0 {
                    format!("The control law's first gains were not usable: {fault}. Nothing was fired.")
                } else {
                    format!(
                    "The control law gave no usable gains {consecutive} times in a row: {fault}. \
                     Nothing more was fired."
                )
                },
                trip: trip(
                    TripFacts::LawFault {
                        reason: fault.reason(),
                        consecutive: *consecutive,
                    },
                    p,
                    at,
                ),
            }
        }
    }
}

/// The controller's events since `seen`, as log lines with their facts for the
/// record. `rows` are the controller's pulse rows, for each fired pulse's verdict.
fn forward_log(seen: &mut usize, log: &mut Vec<LogLine>, svc: &SrsShockService, rows: &[PulseRow]) {
    let level = |db: f64| num(config::round(db, 2));
    let events = svc.log().events();
    for e in events.iter().skip(*seen) {
        let line: Option<LogLine> = match &e.kind {
            EventKind::Note { text } => Some(("note", text.clone(), None)),
            EventKind::PulseFired {
                seq,
                level_db,
                peak_accel_g,
                worst_err_db,
            } => {
                let row = rows.iter().find(|r| r.seq == *seq);
                Some((
                    "pulse",
                    format!(
                        "pulse {} at {} dB: {} g{}",
                        seq + 1,
                        level(*level_db),
                        sig3(*peak_accel_g),
                        worst_err_db.map_or_else(String::new, |w| format!(
                            ", worst band {} dB",
                            num(config::round(w, 2))
                        ))
                    ),
                    Some(json!({
                        "pulse": seq, "level_db": level_db,
                        "scheduled_db": row.map(|r| r.scheduled_db),
                        "peak_g": peak_accel_g, "worst_err_db": worst_err_db,
                        "alarm_bands": row.map(|r| r.alarm_bands),
                        "abort_bands": row.map(|r| r.abort_bands),
                        "measured": row.map(|r| r.passed.is_some()),
                        "passed": row.and_then(|r| r.passed),
                    })),
                ))
            }
            EventKind::SysId {
                band_coherence,
                level_db,
                channels,
            } => Some((
                "note",
                format!(
                    "identified at {} dB: mean coherence {:.3}",
                    level(*level_db),
                    band_coherence
                ),
                Some(json!({"identified": true, "level_db": level_db,
                            "coherence_mean": band_coherence, "channels": channels})),
            )),
            EventKind::Held { reason } => Some((
                "note",
                format!(
                    "identification held the run: {}",
                    reason.as_str().replace('_', " ")
                ),
                Some(json!({"held": reason.as_str()})),
            )),
            EventKind::NotchPlanned {
                notched_bands,
                worst_before_db,
                worst_after_db,
                converged,
            } => Some((
                "note",
                format!(
                    "the limit notch cuts {notched_bands} band{}{}",
                    if *notched_bands == 1 { "" } else { "s" },
                    if *converged {
                        ""
                    } else {
                        "; its fit did not converge"
                    }
                ),
                Some(json!({"notch": {"notched_bands": notched_bands,
                                      "worst_before_db": worst_before_db,
                                      "worst_after_db": worst_after_db,
                                      "converged": converged}})),
            )),
            EventKind::EnvelopeAlarm {
                bands,
                worst_err_db,
            } => Some((
                "alarm",
                format!(
                    "{bands} band{} outside the alarm band; the worst {} dB",
                    if *bands == 1 { "" } else { "s" },
                    num(config::round(*worst_err_db, 2))
                ),
                Some(json!({"bands": bands, "worst_err_db": worst_err_db})),
            )),
            EventKind::DriveScaled { scale, reason } => Some((
                "note",
                format!("drive limit: scale {scale:.3}, {reason}"),
                Some(json!({"drive_limit": "scaled", "scale": scale, "reason": reason})),
            )),
            EventKind::MonitorScaled { scale, channel } => Some((
                "note",
                format!("monitor {channel}: the next pulse is scaled by {scale:.3}"),
                Some(json!({"monitor": channel, "scale": scale})),
            )),
            EventKind::LevelChange {
                from_db,
                to_db,
                auto,
            } => Some((
                "level",
                format!("level {} to {} dB", level(*from_db), level(*to_db)),
                Some(json!({"from_db": from_db, "to_db": to_db, "auto": auto})),
            )),
            EventKind::LawCalled {
                pulse,
                used: false,
                reason,
                detail,
                ..
            } => Some((
                "note",
                format!(
                    "the law's call after {pulse} pulse{} was not used{}",
                    if *pulse == 1 { "" } else { "s" },
                    detail
                        .as_deref()
                        .map_or_else(String::new, |d| format!(": {d}"))
                ),
                Some(
                    json!({"after_pulses": pulse, "used": false, "reason": reason,
                            "detail": detail}),
                ),
            )),
            _ => None,
        };
        if let Some(l) = line {
            log.push(l);
        }
    }
    *seen = events.len();
}

/// The controller's SRS shock service over the station's backend, the backend
/// seen through [`Observed`] so the run's stop reaches a fire at its next block.
pub struct ServiceEngine {
    ctx: EngineContext,
    svc: Option<SrsShockService>,
    cfg: Option<Arc<WireSrs>>,
    table: ChannelTable,
    expected: Arc<AtomicU64>,
    law_values: Vec<DiagnosticSpec>,
    unit: String,
    rows_seen: usize,
    log_seen: usize,
    log: Vec<LogLine>,
    /// Each pulse's bring-up not yet in the service log.
    bring_ups: Vec<(u64, hertzforge_shock_common::BringUp)>,
    /// What the record says of the station armed, until the run takes it.
    station_facts: Option<StationFacts>,
}

impl ServiceEngine {
    pub fn new(ctx: EngineContext) -> ServiceEngine {
        ServiceEngine {
            ctx,
            svc: None,
            cfg: None,
            table: ChannelTable::default(),
            expected: Arc::new(AtomicU64::new(0)),
            law_values: Vec::new(),
            unit: "g".into(),
            rows_seen: 0,
            log_seen: 0,
            log: Vec::new(),
            bring_ups: Vec::new(),
            station_facts: None,
        }
    }

    fn svc(&mut self) -> Result<&mut SrsShockService, MethodError> {
        self.svc
            .as_mut()
            .ok_or_else(|| fault("the run lost its station"))
    }

    fn predicted_db(&self) -> f64 {
        match &self.svc {
            Some(s) if s.config().schedule.mode == ShockRunMode::Auto => {
                s.config().schedule.full_db()
            }
            Some(s) => s.current_level_db(),
            None => 0.0,
        }
    }

    /// How the controller ended the run, when it has.
    fn ended(&self) -> Option<EngineEnd> {
        let svc = self.svc.as_ref()?;
        match svc.state() {
            RunState::Aborted | RunState::NotConverged => Some(
                svc.trip()
                    .map(|t| end_of_trip(t, self.predicted_db(), &self.table))
                    .unwrap_or_else(|| EngineEnd {
                        outcome: "fault",
                        message: "The controller ended the run without a trip record.".into(),
                        trip: None,
                    }),
            ),
            RunState::Stopped if svc.status().ladder_completed => Some(EngineEnd {
                outcome: "completed",
                message: "The schedule finished.".into(),
                trip: None,
            }),
            _ => None,
        }
    }

    /// The frames the next pulse plays: the table in force and its ring-out.
    fn set_expected_pulse(&self) {
        if let Some(svc) = &self.svc {
            let n = svc
                .drive_components()
                .map_or(0, |e| e.synthesize(svc.fs_hz()).len());
            self.expected
                .store((n + svc.ring_out_samples()) as u64, Ordering::Relaxed);
        }
    }

    /// The rows and figures fired since the last look, and the end the controller's
    /// state says.
    fn collect(&mut self) -> Fired {
        self.ctx.progress.end();
        let Some(svc) = self.svc.as_mut() else {
            return Fired::default();
        };
        // The controller's own capture never runs here (the run's record is the
        // one way an SRS shock run records), so its law-call records have nowhere
        // to go and are let go rather than held for the whole run.
        let _ = svc.drain_law_records();
        let Some(svc) = self.svc.as_ref() else {
            return Fired::default();
        };
        let report = svc.report_data();
        let mut out = Fired::default();
        for p in report.pulses.iter().skip(self.rows_seen) {
            out.rows.push(row_of(p));
            out.figures.push(self.figures_of(svc, p));
            if let Some(b) = p.bring_up {
                self.bring_ups.push((p.seq, b));
            }
        }
        self.rows_seen = report.pulses.len();
        forward_log(&mut self.log_seen, &mut self.log, svc, &report.pulses);
        out.end = self.ended();
        out
    }

    /// One pulse's figures, from the controller's view of it.
    fn figures_of(&self, svc: &SrsShockService, p: &PulseRow) -> PulseFigures {
        let fs = svc.fs_hz();
        let components = svc.drive_components().map(|e| ComponentFigures {
            freqs_hz: e.component_freqs(),
            amplitude_v: e.amplitudes(),
            gains: svc.last_law_gains().to_vec(),
        });
        let limits = self.limit_figures(svc);
        let Some(v) = svc.pulse_view(p.seq) else {
            return PulseFigures {
                fs_hz: fs,
                srs: None,
                measured: None,
                drive: Vec::new(),
                velocity: None,
                displacement: None,
                components,
                limits,
            };
        };
        let (alarm_lo, alarm_hi): (Vec<f64>, Vec<f64>) = v.alarm.iter().copied().unzip();
        let (abort_lo, abort_hi): (Vec<f64>, Vec<f64>) = v.abort.iter().copied().unzip();
        PulseFigures {
            fs_hz: v.fs_hz,
            srs: Some(SrsFigures {
                bands_hz: v.band_hz.clone(),
                target: v.target_srs.clone(),
                edges: super::preview::Edges {
                    alarm_hi,
                    alarm_lo,
                    abort_hi,
                    abort_lo,
                },
                measured: Some(v.measured_srs.clone()),
            }),
            measured: Some(v.measured.clone()),
            drive: v.drive.clone(),
            velocity: Some(v.velocity_ips.clone()),
            displacement: Some(v.displacement_in.clone()),
            components,
            limits,
        }
    }

    fn limit_figures(&self, svc: &SrsShockService) -> Vec<LimitFigures> {
        let Some(cfg) = self.cfg.as_deref() else {
            return Vec::new();
        };
        let Some(set) = cfg.limit_set(&self.table) else {
            return Vec::new();
        };
        let bands = svc.band_freqs().to_vec();
        set.channels
            .iter()
            .filter_map(|ch| {
                let predicted = svc
                    .last_predicted_limit_srs()
                    .iter()
                    .find(|(n, _)| *n == ch.name)
                    .map(|(_, p)| p.clone())?;
                let bound = match &ch.limit {
                    hertzforge_srs::service::config::LimitTarget::Flat { accel_g } => {
                        vec![*accel_g; bands.len()]
                    }
                    hertzforge_srs::service::config::LimitTarget::PerBand { bounds_g } => {
                        bounds_g.clone()
                    }
                };
                Some(LimitFigures {
                    name: ch.name.clone(),
                    bands_hz: bands.clone(),
                    predicted,
                    bound,
                    headroom_db: set.headroom_db,
                })
            })
            .collect()
    }
}

/// A fire the operator or the service stopped is not a fault: the run thread ends
/// the run on its signal.
fn fired_or_cancelled<T>(r: Result<T, hertzforge_srs::ServiceError>) -> Result<(), MethodError> {
    match r {
        Ok(_) | Err(hertzforge_srs::ServiceError::Cancelled(_)) => Ok(()),
        Err(e) => Err(fault(e)),
    }
}

impl SrsEngine for ServiceEngine {
    fn arm(&mut self, plan: ArmPlan) -> Result<ArmOutcome, MethodError> {
        let law = plan.law?;
        // The configuration keeps the controller's own capture off: the run's
        // record is the one way an SRS shock run records.
        let mut cfg = plan.cfg.build(&plan.table);
        // The run thread holds for review itself (the protocol's hold) and calls the
        // seed when it is over, so the controller always stops there.
        cfg.review_after_sysid = true;
        let built = station::build(&plan.station, &*plan.scanner)?;
        let hardware = built.station.is_hardware();
        let lock = if hardware {
            Some(
                hertzforge_station::lock::acquire(
                    &plan.data_root,
                    &plan.bind,
                    &built.provenance.station_label,
                )
                .map_err(|e| match e {
                    hertzforge_station::lock::LockError::Held(h) => MethodError::named(
                        ErrorCode::Refused,
                        "hardware_busy",
                        format!("Another program holds the NI hardware: {}", h.advice()),
                    )
                    .with_detail(json!({"holder": h.advice()})),
                    hertzforge_station::lock::LockError::Io(m) => arm_refused(m),
                })?,
            )
        } else {
            None
        };
        let class = built.station;
        let provenance = built.provenance.clone();
        // The streams are taken where the station's blocks pass: every input as
        // read and the drive as the device accepted it, a shot per start. Their
        // buffers are made here, before anything runs.
        let caps = built.backend.caps();
        let buffers = if plan.streams {
            crate::random::record::tap_buffers(plan.cfg.fs_hz(), plan.stream_buffer_s)
        } else {
            1
        };
        let (inputs_tap, inputs_link) = crate::record::tap::pair(
            crate::random::record::input_rows(&plan.table).len(),
            crate::random::record::TAP_BLOCK_FRAMES,
            buffers,
        );
        let (drives_tap, drives_link) = crate::record::tap::pair(
            crate::random::record::drive_rows(&plan.table).len(),
            crate::random::record::TAP_BLOCK_FRAMES,
            buffers,
        );
        let recorded = ShotRecording::wrap(
            built.backend,
            inputs_tap,
            drives_tap,
            plan.station_frame.clone(),
            plan.shots.clone(),
        );
        self.station_facts = Some(StationFacts {
            source: station::source_of(class),
            station: crate::random::record::station_member(
                matches!(&plan.station, StationPlan::Sim { .. }),
                class,
                &provenance,
                &plan.devices,
                &caps,
            ),
            sim: crate::random::record::sim_value(&plan.station, class),
            build: crate::random::record::build_value(provenance.core_build),
            output_latency: match &caps.output_buffer {
                Some(b) => json!({"frames": b.min_lookahead_frames, "how": "declared"}),
                None => json!({"frames": null, "how": "unknown"}),
            },
            links: Some((inputs_link, drives_link)),
        });
        self.unit = plan
            .table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Control)
            .map_or_else(|| "g".to_owned(), |c| crate::table::unit_name(&c.units));
        let own = Arc::new(AtomicBool::new(false));
        let watch = Watch {
            asked: self.ctx.signals.cancel.clone(),
            own: own.clone(),
            progress: self.ctx.progress.clone(),
            expected: self.expected.clone(),
            speed: if hardware { 0.0 } else { self.ctx.sim_speed },
            abort: None,
        };
        let wait = watch.clone();
        let backend: Box<dyn hertzforge_hal::Backend> = Box::new(Observed::new(recorded, watch));
        let plant = HalPulsePlant::new(backend, plan.table.clone(), class).map_err(arm_refused)?;
        let mut svc = SrsShockService::new(
            cfg,
            plan.table.clone(),
            FirePath::Host(Box::new(plant)),
            None,
        )
        .map_err(arm_refused)?
        .with_law_factory(law);
        svc.set_cancel_handle(own);
        // The run's stop reaches the controller's flag at every poll of its wait
        // between pulses, as it does at every block of a fire.
        svc.set_wait_observer(Box::new(move |_left| wait.bridge()));
        let feas = svc.assess_feasibility().map_err(arm_refused)?;
        let verdict = config::verdict(&feas);
        if svc.state() == RunState::FeasibilityBlocked {
            match &plan.ack {
                Some(a) => svc.arm_with_override(a).map_err(arm_refused)?,
                None => return Ok(ArmOutcome::Blocked(verdict)),
            }
        } else {
            svc.arm().map_err(arm_refused)?;
        }
        let adopted = svc.fs_hz();
        let record_len = svc
            .config()
            .seed_table()
            .map(|t| t.synthesize(adopted).len())
            .unwrap_or(0)
            + svc.ring_out_samples();
        self.cfg = Some(plan.cfg.clone());
        self.table = plan.table;
        self.law_values = plan.law_values;
        forward_log(&mut self.log_seen, &mut self.log, &svc, &[]);
        self.svc = Some(svc);
        Ok(ArmOutcome::Armed(Armed {
            fs_hz: adopted,
            record_len,
            station: super::view::StationRef {
                kind: if matches!(plan.station, StationPlan::Sim { .. }) {
                    "sim"
                } else {
                    "ni"
                },
                class: class_name(class),
                devices: provenance
                    .device_ids
                    .iter()
                    .cloned()
                    .zip(provenance.serials.iter().cloned())
                    .collect(),
            },
            feasibility: verdict,
            lock,
        }))
    }

    fn identify(&mut self) -> Result<Identified, MethodError> {
        let review = self
            .cfg
            .as_deref()
            .ok_or_else(|| fault("the run is not armed"))?
            .review;
        self.expected.store(0, Ordering::Relaxed);
        let r = self.svc()?.run_sysid();
        self.ctx.progress.end();
        let svc = self
            .svc
            .as_ref()
            .ok_or_else(|| fault("the run lost its station"))?;
        forward_log(&mut self.log_seen, &mut self.log, svc, &[]);
        let status = svc.status();
        let facts = status.identification.as_ref().map(|i| IdentFacts {
            excitation: config::excitation_name(i.excitation),
            drive_vpk: i.drive_vpk,
            max_vpk: i.max_vpk,
            coherence_mean: i.coherence_mean,
            min_mean_coherence: i.min_mean_coherence,
            passed: i.passed,
        });
        match (r, svc.state()) {
            (Err(hertzforge_srs::ServiceError::Cancelled(_)), _) => {
                Err(fault("identification was stopped"))
            }
            (Err(e), _) => Err(fault(e)),
            (Ok(()), RunState::AwaitingReview) => Ok(Identified::Done {
                facts: facts.ok_or_else(|| fault("identification left no summary"))?,
                review,
            }),
            (Ok(()), RunState::Held) => Ok(Identified::Held {
                facts,
                message: status.held.map_or_else(
                    || "Identification held the run.".to_owned(),
                    |h| {
                        let w = h.to_string();
                        let mut c = w.chars();
                        match c.next() {
                            Some(f) => format!("{}{}.", f.to_uppercase(), c.as_str()),
                            None => w,
                        }
                    },
                ),
            }),
            (Ok(()), other) => Err(fault(format!("identification ended in {other:?}"))),
        }
    }

    fn seed(&mut self) -> Result<Seeded, MethodError> {
        self.svc()?.proceed_after_review().map_err(fault)?;
        let svc = self
            .svc
            .as_ref()
            .ok_or_else(|| fault("the run lost its station"))?;
        forward_log(&mut self.log_seen, &mut self.log, svc, &[]);
        if let Some(end) = self.ended() {
            return Ok(Seeded::Ended(end));
        }
        let state = svc.state();
        if !matches!(state, RunState::Armed | RunState::Firing) {
            return Err(fault(format!("the law's seed left the run in {state:?}")));
        }
        let p = svc
            .status()
            .prediction
            .ok_or_else(|| fault("the law's seed made no prediction"))?;
        Ok(Seeded::Ready(prediction_of(&p)))
    }

    fn fire(&mut self) -> Result<Fired, MethodError> {
        self.set_expected_pulse();
        // `go` lowers when nobody holds control: a pulse whose wait it finds
        // lowered is parked unplayed, and the run thread asks again once it rises.
        let go = self.ctx.signals.go.clone();
        fired_or_cancelled(self.svc()?.fire_pulse_cancellable(Some(&go)))?;
        let mut fired = self.collect();
        if fired.end.is_none() {
            let completed = self.svc.as_ref().is_some_and(|s| {
                s.config().schedule.mode == ShockRunMode::Auto && s.status().ladder_completed
            });
            if completed {
                let _ = self.svc()?.stop();
                fired.end = Some(EngineEnd {
                    outcome: "completed",
                    message: "The schedule finished.".into(),
                    trip: None,
                });
            }
        }
        Ok(fired)
    }

    fn set_level(&mut self, to: LevelMove) -> Result<f64, MethodError> {
        let svc = self.svc()?;
        let r = match to {
            LevelMove::Step(d) if d > 0.0 => svc.step_up(),
            LevelMove::Step(_) => svc.step_down(),
            LevelMove::Rung(i) => svc.jump_to_level(i),
        };
        r.map_err(|e| MethodError::named(ErrorCode::Refused, "wrong_state", e.to_string()))
    }

    fn predicted_vpk_at(&self, level_db: f64) -> Option<f64> {
        self.svc.as_ref()?.predicted_drive_vpk_at(level_db)
    }

    fn stop(&mut self) {
        if let Some(svc) = self.svc.as_mut() {
            let _ = svc.stop();
        }
    }

    fn abort(&mut self, reason: &str) {
        if let Some(svc) = self.svc.as_mut() {
            if !svc.state().is_terminal() {
                svc.abort_now(reason);
            }
        }
    }

    fn position(&self) -> Position {
        let Some(svc) = self.svc.as_ref() else {
            return Position::default();
        };
        let st = svc.status();
        let schedule = &svc.config().schedule;
        let on_table = schedule
            .table_db
            .get(st.level_index)
            .is_some_and(|l| (l - st.current_level_db).abs() < 1e-9);
        Position {
            level_db: st.current_level_db,
            rung: on_table.then_some(st.level_index),
            at_level: st.pulses_at_level,
            of_level: if schedule.mode == ShockRunMode::Auto {
                schedule.pulses_for(st.level_index)
            } else {
                0
            },
            passed: st.pulses_passed,
        }
    }

    fn identification(&self) -> Option<IdentFigures> {
        let svc = self.svc.as_ref()?;
        let st = svc.status();
        let ident = st.identification?;
        let v = svc.identification_view()?;
        let (lo, hi) = v.band_hz;
        let mut freqs = Vec::new();
        let mut gain = Vec::new();
        let mut coherence = Vec::new();
        for (k, (&f, &m)) in v.freqs_hz.iter().zip(&v.h_mag).enumerate() {
            if f >= lo && f <= hi {
                freqs.push(f);
                gain.push(m);
                coherence.push(v.coherence.get(k).copied().unwrap_or(0.0));
            }
        }
        let limit_frf = svc
            .report_data()
            .frf_summary
            .iter()
            .filter(|f| f.channel != "control")
            .map(|f| FrfFigures {
                name: f.channel.clone(),
                freqs_hz: f.freqs_hz.clone(),
                mag: f.mag.clone(),
                measured: f.band_coherence.is_some(),
            })
            .collect();
        Some(IdentFigures {
            coherence,
            freqs_hz: freqs,
            gain,
            floor: v.h_floor,
            hold: ident.min_mean_coherence,
            unit: self.unit.clone(),
            limit_frf,
        })
    }

    fn prediction(&self) -> Option<Prediction> {
        self.svc
            .as_ref()?
            .status()
            .prediction
            .as_ref()
            .map(prediction_of)
    }

    fn notch(&self) -> Option<Notch> {
        let n = self.svc.as_ref()?.status().notch?;
        Some(Notch {
            notched_bands: n.notched_bands,
            worst_before_db: n.worst_before_db,
            worst_after_db: n.worst_after_db,
            converged: n.converged,
        })
    }

    fn law_values(&self) -> Vec<LawReading> {
        let Some(law) = self.svc.as_ref().and_then(|s| s.status().law) else {
            return Vec::new();
        };
        law.values
            .iter()
            .map(|v| {
                let d = self.law_values.iter().find(|d| d.key == v.key);
                LawReading {
                    label: d.map_or_else(|| v.key.clone(), |d| d.label.clone()),
                    value: v.value,
                    unit: d.map_or_else(String::new, |d| d.unit.clone()),
                }
            })
            .collect()
    }

    fn misses(&self) -> u32 {
        self.svc
            .as_ref()
            .and_then(|s| s.status().law)
            .map_or(0, |l| l.misses_in_a_row)
    }

    fn take_log(&mut self) -> Vec<LogLine> {
        std::mem::take(&mut self.log)
    }

    fn release(&mut self) {
        if let Some(mut svc) = self.svc.take() {
            let _ = svc.stop();
        }
    }

    fn take_station_facts(&mut self) -> Option<StationFacts> {
        self.station_facts.take()
    }

    fn ident_record(&self) -> Option<IdentRecord> {
        let v = self.svc.as_ref()?.identification_view()?;
        let (lo, hi) = v.band_hz;
        let bins: Vec<usize> = (0..v.freqs_hz.len().min(v.h.len()))
            .filter(|&k| (lo..=hi).contains(&v.freqs_hz[k]))
            .collect();
        let first = *bins.first()?;
        let df = match v.freqs_hz.get(1) {
            Some(f1) => f1 - v.freqs_hz[0],
            None => return None,
        };
        Some(IdentRecord {
            first_hz: v.freqs_hz[first],
            df_hz: df,
            h: bins.iter().map(|&k| v.h[k]).collect(),
            coherence: bins
                .iter()
                .map(|&k| v.coherence.get(k).copied().unwrap_or(0.0))
                .collect(),
        })
    }

    fn last_pulse_start(&self) -> Option<Instant> {
        // The age first, then the instant: the instant read after the age can
        // only make the start later, never earlier, so a countdown from it is
        // never short.
        let age = self.svc.as_ref()?.since_last_pulse_start_s()?;
        Instant::now().checked_sub(std::time::Duration::from_secs_f64(age))
    }

    fn lead(&self) -> std::time::Duration {
        crate::pulse::rails::lead_for(self.svc.as_ref().and_then(|s| s.last_bring_up()))
    }

    fn take_bring_ups(&mut self) -> Vec<(u64, hertzforge_shock_common::BringUp)> {
        std::mem::take(&mut self.bring_ups)
    }
}
