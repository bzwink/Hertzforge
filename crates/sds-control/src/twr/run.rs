//! One configured TWR run, on its own thread, on the transient-run rails
//! ([`crate::pulse::rails`]).
//!
//! The thread owns the run's engine ([`super::engine`]) outright and drives it in
//! the order the protocol states: arm, identification, the review hold (or the
//! coherence hold), proceed, then plays (in `auto` one after another, in `manual`
//! on `twr.iterate`, and through the playback once `twr.play` has begun it), and
//! the end. Nothing here is control math. Requests arrive as [`Cmd`]s the thread
//! takes between plays; while it waits it keeps taking them. The thread publishes
//! a view after every step and puts each play's row and records where the service
//! reads them without the thread.
//!
//! The run's record ([`super::record`], `docs/run-folder.md`) is opened at arm,
//! before anything is driven, and the arm is refused `record_unwritable` when it
//! cannot be written. Each shot of the streams is named after its play, and each
//! play's results and the law's calls go to the record as the play is judged,
//! the play that ends the run included. Every hand-off to the record is a send
//! that never waits; the record's own thread does the file work.
//!
//! Stop and Abort reach a play in flight through the run's cancel flag, raised by
//! the service before it queues the request. An absent controller raises the hold:
//! no play starts while it stands, and the play in flight finishes. Every engine
//! call runs under `catch_unwind`; a panic ends the run in the emergency stop. The
//! station is held only while the run is live.

use super::config::{Grid, WireTwr};
use super::engine::{
    level_words, ArmOutcome, ArmPlan, EngineContext, EngineEnd, EngineFactory, Identified,
    LevelMove, Proceeded, TwrEngine,
};
use super::record::{self as trec, PlayFacts, PlayOutcome};
use super::reference::Stored;
use super::view::{End, RecordRef, State, Trip, TwrView};
use super::{RunData, Starting};
use crate::protocol::{num, ErrorCode, MethodError};
use crate::pulse::rails::{self, guarded, now_unix, Shared, Waited};
use crate::random::record as rec;
use crate::random::run::NO_FRAME;
use crate::record::{ProblemFn, RecordStatus, RunRecord};
use crate::shock::config::LawFacts;
use crate::station::{NiScanner, StationPlan};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_twr::schedule::TwrRunMode;
use hertzforge_twr::TwrLawFactory;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::oneshot;

/// Who stops a run when the service shuts down.
pub const SHUTDOWN_BY: &str = crate::random::run::SHUTDOWN_BY;

/// A publish, as the service hears it.
pub type Notice = rails::Notice<TwrView>;

type Reply<T> = oneshot::Sender<Result<T, MethodError>>;

/// What a run arms on, taken from the service when the request arrives.
pub struct ArmContext {
    pub table: ChannelTable,
    pub station: StationPlan,
    pub scanner: Arc<dyn NiScanner>,
    pub data_root: PathBuf,
    /// Where run folders go.
    pub records_root: PathBuf,
    pub bind: String,
    pub stored: Arc<Stored>,
    pub facts: LawFacts,
    pub law: Result<Arc<dyn TwrLawFactory>, MethodError>,
    /// The table's revision, for the run's record.
    pub revision: u64,
    /// The devices of the last scan, for the record's models and serial numbers.
    pub devices: Vec<hertzforge_hal::DiscoveredDevice>,
}

/// A request to the run thread.
pub enum Cmd {
    Arm {
        ack: Option<String>,
        ctx: ArmContext,
        reply: Reply<()>,
    },
    Start {
        ctx: ArmContext,
        reply: Reply<()>,
    },
    Proceed {
        reply: Reply<()>,
    },
    Iterate {
        reply: Reply<()>,
    },
    Play {
        reply: Reply<()>,
    },
    Level {
        to: LevelMove,
        reply: Reply<f64>,
    },
    Freeze {
        frozen: bool,
    },
    Stop {
        outcome: &'static str,
        by: String,
        reply: Option<Reply<()>>,
    },
    Abort {
        by: String,
        reply: Reply<()>,
    },
}

/// The fixed facts of one run.
pub struct RunPlan {
    pub run_id: String,
    pub cfg: Arc<WireTwr>,
    pub engine: EngineFactory,
    pub sim_speed: f64,
    /// The orphan timeout, for the safe stop's message.
    pub timeout_s: f64,
    /// The derived values as configure gave them, for the record's setup.
    pub derived: Value,
    /// How the run's record is written.
    pub record: crate::record::RecordOptions,
}

fn refuse(reason: &'static str, message: impl Into<String>) -> MethodError {
    MethodError::named(ErrorCode::Refused, reason, message)
}

pub fn wrong_state(v: &TwrView, verb: &str) -> MethodError {
    refuse(
        "wrong_state",
        format!(
            "{verb} does not apply while the run is {} ({}).",
            v.state.name(),
            v.phase.replace('_', " ")
        ),
    )
    .with_detail(json!({"state": v.state.name(), "phase": v.phase}))
}

pub fn wrong_mode(verb: &str) -> MethodError {
    refuse(
        "wrong_mode",
        format!("{verb} needs the manual ladder; this run is automatic."),
    )
    .with_detail(json!({"mode": "auto"}))
}

/// The refusal at the coherence hold, in the service's own words.
pub fn coherence_hold(message: &str, ident: Option<&super::view::IdentFacts>) -> MethodError {
    refuse(
        "coherence_hold",
        format!(
            "The run is held at the coherence hold: {} Only twr.reset releases it.",
            message
        ),
    )
    .with_detail(json!({
        "coherence_mean": ident.map_or(serde_json::Value::Null, |i| {
            crate::protocol::opt_num(i.coherence_mean.is_finite().then_some(i.coherence_mean))
        }),
        "min_mean_coherence": ident.map_or(serde_json::Value::Null, |i| num(i.min_mean_coherence)),
    }))
}

/// The refusal of playback before the present level has gated.
pub fn not_converged(level_db: f64, gate_pct: f64) -> MethodError {
    refuse(
        "not_converged",
        format!(
            "Playback begins once the present level has gated; {} has not met the {} % gate.",
            level_words(level_db),
            num(gate_pct)
        ),
    )
    .with_detail(json!({
        "level_db": num(crate::shock::config::round(level_db, 2)),
        "gate_pct": num(gate_pct),
    }))
}

/// The run thread's state.
struct Actor {
    plan: RunPlan,
    shared: Arc<Shared<TwrView>>,
    data: Arc<RunData>,
    engine: Box<dyn TwrEngine>,
    view: TwrView,
    lock: Option<hertzforge_station::lock::LockGuard>,
    seeded: bool,
    /// A level verb has moved the level (from review, before proceed).
    level_moved: bool,
    hold_message: String,
    /// The play a stop or an abort cancelled, and the frames it had played.
    cancelled: Option<(u64, u64)>,
    stop_outcome: &'static str,
    stopped_by: String,
    /// The run's record, from arm until the run ends.
    record: Option<RunRecord>,
    record_status: Option<Arc<RecordStatus>>,
    run_uid: String,
    utc_offset_s: Option<i64>,
    /// The streams the record holds, by their place in it: the inputs and the drive.
    shot_streams: Vec<usize>,
    /// The control channel and its unit.
    control: (u32, String),
    /// The record's grid: the rate, its length and the section's first sample.
    record_grid: (f64, usize, usize),
    /// The table the run armed with, for the record's channels.
    record_table: Option<ChannelTable>,
    /// The station's starts already named as shots.
    shots_named: u64,
    /// Law calls written to the record so far.
    law_calls: usize,
    /// The response alarm has been put in the log.
    alarm_logged: bool,
}

/// Start the run thread. Returns once the thread exists; the run is `configured`.
pub fn spawn(
    plan: RunPlan,
    initial: TwrView,
    shared: Arc<Shared<TwrView>>,
    data: Arc<RunData>,
    rx: Receiver<Cmd>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let name = format!("twr-{}", plan.run_id);
    std::thread::Builder::new().name(name).spawn(move || {
        let ctx = EngineContext {
            signals: shared.signals.clone(),
            progress: shared.progress.clone(),
            sim_speed: plan.sim_speed,
        };
        let engine = (plan.engine)(&ctx);
        let mut a = Actor {
            plan,
            shared,
            data,
            engine,
            view: initial,
            lock: None,
            seeded: false,
            level_moved: false,
            hold_message: String::new(),
            cancelled: None,
            stop_outcome: "operator_stop",
            stopped_by: "the operator".into(),
            record: None,
            record_status: None,
            run_uid: String::new(),
            utc_offset_s: None,
            shot_streams: Vec::new(),
            control: (0, "g".into()),
            record_grid: (0.0, 0, 0),
            record_table: None,
            shots_named: 0,
            law_calls: 0,
            alarm_logged: false,
        };
        a.run(rx);
    })
}

impl Actor {
    fn run(&mut self, rx: Receiver<Cmd>) {
        loop {
            match (self.view.state, self.view.phase) {
                (State::Running, "sysid") => self.identify(),
                (State::Running, _) => {
                    if !self.between_plays(&rx) {
                        self.abandon();
                        return;
                    }
                }
                _ => match rx.recv() {
                    Ok(cmd) => self.handle(cmd),
                    Err(_) => {
                        self.abandon();
                        return;
                    }
                },
            }
        }
    }

    /// The service dropped this run: release anything held, the drive first.
    fn abandon(&mut self) {
        if self.view.state.is_live() {
            self.shared.signals.raise(true);
            let _ = guarded(|| self.engine.abort("the service dropped the run"));
        }
        self.lock = None;
        // The record keeps what it has, `complete: false`: the run did not end.
        self.shared.detach_record();
        self.record = None;
    }

    fn armed(&self) -> bool {
        self.view.state.is_live() || self.view.state == State::Finished
    }

    fn publish(&mut self) {
        if let (Some(status), Some(r)) = (self.record_status.as_ref(), self.view.record.as_mut()) {
            r.problem = status.problem();
        }
        if self.armed() && self.view.grid.actual {
            let p = self.engine.position();
            if self.seeded || self.level_moved {
                self.view.level_db = Some(p.level_db);
                self.view.rung = p.rung;
            }
            self.view.plays.at_level = p.at_level;
            self.view.plays.of_level = p.of_level;
            self.view.plays.playback = p.playback;
            self.view.plays.of_playback = p.of_playback;
            self.view.frozen = p.frozen;
            self.view.alarm = self.view.alarm || p.alarm;
            if self.view.alarm && !self.alarm_logged {
                self.alarm_logged = true;
                self.shared.add_log_data(
                    "alarm",
                    format!(
                        "the response entered its alarm band at {}",
                        level_words(p.level_db)
                    ),
                    Some(
                        json!({"level_db": num(crate::shock::config::round(p.level_db, 2)),
                                "play": self.data.row_count().checked_sub(1),
                                "alarm_frac": num(self.plan.cfg.envelope_alarm_frac)}),
                    ),
                );
            }
            let fs = self.view.grid.fs_hz;
            if fs > 0.0 {
                self.view.t_s = Some(self.engine.frames_clocked() as f64 / fs);
            }
            if let Some(l) = self.view.law.as_mut() {
                l.misses = self.engine.misses();
            }
            if self.seeded && self.view.state.is_live() {
                if let Some(vpk) = self.engine.predicted_vpk_at(p.level_db) {
                    self.view.prediction = Some(super::view::Prediction {
                        drive_vpk: vpk,
                        drive_vpk_ceiling: self.plan.cfg.drive_vpk_ceiling,
                        level_db: p.level_db,
                    });
                }
            }
        }
        self.forward_log();
        self.shared.publish(&self.plan.run_id, self.view.clone());
    }

    fn forward_log(&mut self) {
        for (kind, text) in self.engine.take_log() {
            self.shared.add_log(kind, text);
        }
    }

    fn set(&mut self, state: State, phase: &'static str) {
        if self.view.phase != phase {
            self.shared.add_log_data(
                "state",
                format!(
                    "{} to {}",
                    self.view.phase.replace('_', " "),
                    phase.replace('_', " ")
                ),
                Some(json!({"from": self.view.phase, "to": phase, "state": state.name()})),
            );
        }
        self.view.state = state;
        self.view.phase = phase;
    }

    /// Run `f` on the engine; a panic ends the run in the emergency stop.
    fn call<T>(&mut self, what: &str, f: impl FnOnce(&mut dyn TwrEngine) -> T) -> Option<T> {
        let engine = &mut self.engine;
        match guarded(|| f(engine.as_mut())) {
            Ok(t) => Some(t),
            Err(words) => {
                self.shared.signals.raise(true);
                let engine = &mut self.engine;
                let _ = guarded(|| engine.abort("a panic in the run"));
                self.end(
                    "fault",
                    None,
                    Some(format!("The run stopped on a fault in {what}: {words}")),
                );
                None
            }
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Arm { ack, ctx, reply } => {
                let r = self.arm_cmd(ack, ctx);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Start { ctx, reply } => {
                let r = self.start_cmd(ctx);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Proceed { reply } => match self.view.state {
                State::Review => {
                    self.shared.add_log_data(
                        "note",
                        "the operator reviewed identification and proceeded",
                        Some(json!({"action": "proceed"})),
                    );
                    let _ = reply.send(Ok(()));
                    self.proceed();
                }
                State::Held => {
                    let _ = reply.send(Err(coherence_hold(
                        &self.hold_message,
                        self.view.identification.as_ref(),
                    )));
                }
                _ => {
                    let _ = reply.send(Err(wrong_state(&self.view, "Proceeding")));
                }
            },
            Cmd::Iterate { reply } => match self.may_iterate() {
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
                // The answer goes once the play shows as in flight, so a second
                // verb sent on it is refused rather than queued behind it.
                Ok(()) => self.play_once(Some(reply)),
            },
            Cmd::Play { reply } => {
                let r = self.play_cmd();
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Level { to, reply } => {
                let r = self.level_cmd(to);
                self.publish();
                let _ = reply.send(r);
            }
            Cmd::Freeze { frozen } => {
                if matches!(self.view.state, State::Running | State::Review) {
                    let _ = self.call("freezing", |e| e.freeze(frozen));
                    self.view.frozen = frozen;
                    self.shared.add_log_data(
                        "note",
                        if frozen {
                            "the law's corrections are frozen; the drive in force replays"
                        } else {
                            "the law's corrections resume"
                        },
                        Some(json!({"action": if frozen { "freeze" } else { "resume" }})),
                    );
                    self.publish();
                }
            }
            Cmd::Stop { outcome, by, reply } => {
                if self.view.state.is_live() {
                    self.stop_outcome = outcome;
                    self.stopped_by = by;
                    self.end_on_stop();
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            Cmd::Abort { by, reply } => {
                if self.view.state.is_live() {
                    self.stopped_by = by;
                    self.end_on_abort();
                }
                let _ = reply.send(Ok(()));
            }
        }
    }

    // ---- arm and start ---------------------------------------------------------

    fn arm_cmd(&mut self, ack: Option<String>, ctx: ArmContext) -> Result<(), MethodError> {
        match self.view.state {
            State::Configured | State::Blocked => {}
            State::Finished => return Err(self.finished_refusal()),
            _ => return Err(self.run_active()),
        }
        if ack.is_some() && self.view.state != State::Blocked {
            return Err(refuse(
                "override_not_needed",
                "The run is not blocked by feasibility; arm without an acknowledgement.",
            ));
        }
        self.arm(ack, ctx)
    }

    fn start_cmd(&mut self, ctx: ArmContext) -> Result<(), MethodError> {
        match self.view.state {
            State::Configured | State::Blocked => self.arm(None, ctx)?,
            State::Armed => {}
            State::Finished => return Err(self.finished_refusal()),
            _ => return Err(self.run_active()),
        }
        self.set(State::Running, "sysid");
        self.shared.add_log_data(
            "note",
            format!(
                "started; identification at {} V rms for {} s",
                num(self.plan.cfg.sysid_drive_vrms),
                num(self.plan.cfg.sysid_duration_s)
            ),
            Some(
                json!({"action": "start", "drive_vrms": num(self.plan.cfg.sysid_drive_vrms),
                        "duration_s": num(self.plan.cfg.sysid_duration_s)}),
            ),
        );
        if let Some(r) = self.record.as_ref() {
            let frame = self.shared.station_frame.load(Ordering::Relaxed);
            let mut patch = Map::new();
            patch.insert(
                "start".into(),
                rec::start_json(
                    now_unix(),
                    self.utc_offset_s,
                    if frame == NO_FRAME { 0 } else { frame },
                ),
            );
            r.patch_manifest(patch);
        }
        Ok(())
    }

    fn finished_refusal(&self) -> MethodError {
        refuse(
            "run_finished",
            "This run has finished; reset or configure again to run.",
        )
        .with_detail(json!({"outcome": self.view.end.as_ref().map(|e| e.outcome)}))
    }

    fn run_active(&self) -> MethodError {
        refuse(
            "run_active",
            format!(
                "Run {} is {}; stop it first.",
                self.view.run_id,
                self.view.state.name()
            ),
        )
        .with_detail(json!({"run_id": self.view.run_id}))
    }

    fn arm(&mut self, ack: Option<String>, ctx: ArmContext) -> Result<(), MethodError> {
        let root = ctx.records_root.clone();
        // The streams are recorded unless the setup leaves them out or the disk is
        // already under its free-space floor.
        let streams_off = if !self.plan.cfg.record_streams {
            Some("switched off in the setup".to_owned())
        } else {
            match hertzforge_capture::free_space_bytes_for(&root) {
                Ok(free) if free < self.plan.record.free_space_floor_bytes => Some(format!(
                    "the disk had {} MB free, under the {} MB kept free",
                    free / 1_000_000,
                    self.plan.record.free_space_floor_bytes / 1_000_000
                )),
                _ => None,
            }
        };
        let table = ctx.table.clone();
        let stored = ctx.stored.clone();
        let revision = ctx.revision;
        let plan = ArmPlan {
            cfg: self.plan.cfg.clone(),
            stored: ctx.stored,
            facts: ctx.facts,
            table: ctx.table,
            station: ctx.station,
            scanner: ctx.scanner,
            data_root: ctx.data_root,
            bind: ctx.bind,
            law: ctx.law,
            ack: ack.clone(),
            devices: ctx.devices,
            streams: streams_off.is_none(),
            stream_buffer_s: self.plan.record.stream_buffer_s,
            station_frame: self.shared.station_frame.clone(),
        };
        let Some(r) = self.call("arm", |e| e.arm(plan)) else {
            return Err(MethodError::internal("the run failed while arming"));
        };
        match r? {
            ArmOutcome::Blocked(feasibility) => {
                self.view.feasibility = Some(feasibility.clone());
                self.set(State::Blocked, "feasibility_blocked");
                Err(refuse(
                    "feasibility_exceeded",
                    "Feasibility exceeded: the section at full level is over its kinematic \
                     ceilings. Review it, or arm with an acknowledgement.",
                )
                .with_detail(json!({
                    "feasibility": {"verdict": feasibility.0, "message": feasibility.1}
                })))
            }
            ArmOutcome::Armed(a) => {
                let a = *a;
                if let Some(ack) = &ack {
                    self.shared.add_log_data(
                        "override",
                        format!("operator acknowledgement: {ack}"),
                        Some(json!({"what": "feasibility_exceeded", "acknowledgement": ack})),
                    );
                }
                self.view.feasibility = Some(a.feasibility.clone());
                // The run's record, before any sample: refused here, the arm is
                // refused and the station released (nothing has been driven).
                if let Err(e) = self.open_record(&a, &table, &stored, revision, root, streams_off) {
                    self.engine.release();
                    return Err(e);
                }
                // The armed grid is the engine's whole: its rate beside the
                // configure-time section counts is the grid bench T0 was shown.
                self.view.grid = Grid {
                    fs_hz: a.fs_hz,
                    record_len: a.record_len,
                    section_start: a.section_start,
                    section_len: a.section_len,
                    actual: true,
                };
                self.view.clock = Some("shared");
                self.view.station = a.station;
                self.view.streaming = true;
                self.view.units = a.units;
                self.data.set_station(a.fs_hz, a.tap);
                self.lock = a.lock;
                self.set(State::Armed, "armed");
                self.shared.add_log_data(
                    "station",
                    format!(
                        "armed on {} at {} Hz, records of {} samples",
                        self.view.station.class,
                        num(crate::shock::view::sig(a.fs_hz, 9)),
                        a.record_len
                    ),
                    Some(json!({"class": self.view.station.class,
                                "fs_hz": num(crate::shock::view::sig(a.fs_hz, 9)),
                                "record_len": a.record_len, "section_start": a.section_start,
                                "section_len": a.section_len})),
                );
                Ok(())
            }
        }
    }

    // ---- identification, review, proceed --------------------------------------------

    fn identify(&mut self) {
        self.publish();
        let Some(r) = self.call("identification", |e| e.identify()) else {
            return;
        };
        // The controller's own lines first, so the log is in the order things
        // happened.
        self.forward_log();
        // The identification is the streams' first shot, whatever came of it.
        self.name_shot(None);
        self.record_identification();
        if self.interrupted() {
            return;
        }
        match r {
            Err(e) => self.end("fault", None, Some(e.message)),
            Ok(Identified::Held { facts, message }) => {
                self.view.identification = facts;
                self.data.set_identification(self.engine.identification());
                self.hold_message = message.clone();
                self.set(State::Held, "coherence_hold");
                let ident = self.view.identification.as_ref();
                self.shared.add_log_data(
                    "trip",
                    message,
                    Some(json!({"hold": "coherence",
                                "coherence_mean": ident.map_or(Value::Null, |i| {
                                    crate::protocol::opt_num(
                                        i.coherence_mean.is_finite().then_some(i.coherence_mean))
                                }),
                                "min_mean_coherence": num(self.plan.cfg.min_mean_coherence)})),
                );
                self.publish();
            }
            Ok(Identified::Ended { facts, end }) => {
                self.view.identification = facts;
                self.end_engine(end);
            }
            Ok(Identified::Done { facts, review }) => {
                self.view.identification = Some(facts);
                self.data.set_identification(self.engine.identification());
                self.data.set_mask(self.engine.mask());
                self.data.set_target(self.engine.level_target());
                if review {
                    self.set(State::Review, "awaiting_review");
                    self.publish();
                } else {
                    self.proceed();
                }
            }
        }
    }

    fn proceed(&mut self) {
        let Some(r) = self.call("the law's seed", |e| e.proceed()) else {
            return;
        };
        self.forward_log();
        // The seed's call, before any play.
        self.record_law_calls();
        if self.interrupted() {
            return;
        }
        match r {
            Err(e) => self.end("fault", None, Some(e.message)),
            Ok(Proceeded::Ended(end)) => self.end_engine(end),
            Ok(Proceeded::Ready(prediction)) => {
                self.view.prediction = prediction;
                self.seeded = true;
                self.set(State::Running, "waiting");
                match prediction {
                    Some(p) => self.shared.add_log_data(
                        "note",
                        format!(
                            "seeded; the drive in force peaks at {} V at {}",
                            num(crate::shock::view::sig(p.drive_vpk, 4)),
                            level_words(p.level_db)
                        ),
                        Some(json!({"seeded": true,
                                    "drive_vpk": num(crate::shock::view::sig(p.drive_vpk, 4)),
                                    "level_db": num(crate::shock::config::round(p.level_db, 2))})),
                    ),
                    None => {
                        self.shared
                            .add_log_data("note", "seeded", Some(json!({"seeded": true})))
                    }
                }
                self.publish();
            }
        }
    }

    /// Ended by the operator's signal while a long call ran.
    fn interrupted(&mut self) -> bool {
        if self.shared.signals.aborting() {
            self.end_on_abort();
            true
        } else if self.shared.signals.cancelled() {
            self.end_on_stop();
            true
        } else {
            false
        }
    }

    // ---- between plays and playing -----------------------------------------------

    /// Wait between plays: take requests, and when a play is due (in `auto`, or
    /// through a playback `twr.play` began) play it. False when the service has
    /// dropped the run.
    fn between_plays(&mut self, rx: &Receiver<Cmd>) -> bool {
        let pos = self.engine.position();
        let due =
            self.seeded && !pos.holding && (self.plan.cfg.mode == TwrRunMode::Auto || pos.playing);
        if !due {
            match rx.recv_timeout(rails::POLL) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
            if self.view.state.is_live() {
                let _ = self.interrupted();
            }
            return true;
        }
        let signals = self.shared.signals.clone();
        let mut gone = false;
        let waited = rails::wait_until(Instant::now(), &signals, || loop {
            match rx.try_recv() {
                Ok(cmd) => {
                    self.handle(cmd);
                    if self.view.state != State::Running {
                        return false;
                    }
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    return false;
                }
            }
        });
        if gone {
            return false;
        }
        match waited {
            Waited::Cancelled => {
                let _ = self.interrupted();
            }
            Waited::Interrupted => {}
            Waited::Due => {
                if self.view.state == State::Running {
                    self.play_once(None);
                }
            }
        }
        true
    }

    fn may_iterate(&self) -> Result<(), MethodError> {
        if self.plan.cfg.mode == TwrRunMode::Auto {
            return Err(wrong_mode("An iteration"));
        }
        if self.view.state == State::Held {
            return Err(coherence_hold(
                &self.hold_message,
                self.view.identification.as_ref(),
            ));
        }
        let pos = self.engine.position();
        if !(self.view.state == State::Running
            && self.view.phase == "waiting"
            && self.seeded
            && !pos.playing)
        {
            return Err(wrong_state(&self.view, "An iteration"));
        }
        Ok(())
    }

    fn play_cmd(&mut self) -> Result<(), MethodError> {
        if self.plan.cfg.mode == TwrRunMode::Auto {
            return Err(wrong_mode("Playback"));
        }
        if self.view.state == State::Held {
            return Err(coherence_hold(
                &self.hold_message,
                self.view.identification.as_ref(),
            ));
        }
        let pos = self.engine.position();
        if !(self.view.state == State::Running
            && matches!(self.view.phase, "waiting" | "holding")
            && self.seeded
            && !pos.playing)
        {
            return Err(wrong_state(&self.view, "Playback"));
        }
        if !pos.gated {
            return Err(not_converged(pos.level_db, self.plan.cfg.gate_pct));
        }
        let Some(r) = self.call("beginning playback", |e| e.begin_playback()) else {
            return Err(MethodError::internal("the run failed beginning playback"));
        };
        r?;
        self.shared.add_log_data(
            "note",
            format!("playback begins at {}", level_words(pos.level_db)),
            Some(json!({"action": "play",
                        "level_db": num(crate::shock::config::round(pos.level_db, 2))})),
        );
        Ok(())
    }

    fn play_once(&mut self, reply: Option<Reply<()>>) {
        let pos = self.engine.position();
        let index = self.data.row_count();
        let starting = Starting {
            index,
            playback: pos.playing,
            iteration: (!pos.playing).then_some(pos.at_level + 1),
        };
        self.data.start_play(starting, self.engine.frames_clocked());
        self.set(
            State::Running,
            if pos.playing { "playing" } else { "iterating" },
        );
        self.publish();
        if let Some(reply) = reply {
            let _ = reply.send(Ok(()));
        }
        let r = self.call("a play", |e| e.play());
        self.data.end_play(self.engine.frames_clocked());
        let Some(r) = r else {
            return;
        };
        self.forward_log();
        let played = match r {
            Ok(p) => p,
            Err(e) => {
                self.end("fault", None, Some(e.message));
                return;
            }
        };
        // The play into the record as it is judged: the play the run ended on and
        // a play cut short included.
        let (outcome, trip) = match (&played.cancelled_at, &played.end) {
            (Some(_), _) => (PlayOutcome::Cancelled, None),
            (None, Some(end)) => match &end.trip {
                Some(t) if t.play.is_some() => (PlayOutcome::Tripped, Some(t)),
                _ => (PlayOutcome::Judged, None),
            },
            (None, None) => (PlayOutcome::Judged, None),
        };
        let row = played.row.as_ref();
        let index = row
            .map(|r| r.index)
            .or(trip.and_then(|t| t.play))
            .unwrap_or(index);
        let facts = PlayFacts {
            index,
            level_db: row
                .map(|r| r.level_db)
                .or(trip.and_then(|t| t.at_level_db))
                .unwrap_or(pos.level_db),
            playback: row.map_or(starting.playback, |r| r.playback),
            iteration: row.map_or(starting.iteration, |r| r.iteration),
            row,
            outcome,
            trip,
            frames_done: played.cancelled_at,
        };
        let shot = self.name_shot(Some(&facts));
        let reached = row.is_some() || shot.is_some();
        if reached {
            self.record_play(&facts, shot);
        }
        self.record_law_calls();
        if let Some(done) = played.cancelled_at {
            self.cancelled = Some((index, done));
            if !self.interrupted() {
                self.end(
                    "fault",
                    None,
                    Some("A play was cancelled with no stop asked for.".into()),
                );
            }
            return;
        }
        if let Some(row) = played.row {
            self.view.plays.done += 1;
            self.view.last_play = Some(row.clone());
            self.data.add_play(row, played.records);
            self.data.set_target(self.engine.level_target());
        }
        if let Some(end) = played.end {
            self.end_engine(end);
            return;
        }
        if self.interrupted() {
            return;
        }
        let holding = self.engine.position().holding;
        self.set(State::Running, if holding { "holding" } else { "waiting" });
        self.publish();
    }

    // ---- the record -----------------------------------------------------------------

    /// Open the run's record at arm: the folder claimed, `run.json`, `table.json`,
    /// `setup.json` and the law written before any sample, the log attached, and
    /// the reference and its target at full level. Refused `record_unwritable`
    /// when it cannot be written.
    fn open_record(
        &mut self,
        a: &super::engine::Armed,
        table: &ChannelTable,
        stored: &Stored,
        revision: u64,
        root: PathBuf,
        streams_off: Option<String>,
    ) -> Result<(), MethodError> {
        let cfg = self.plan.cfg.clone();
        let armed_unix = now_unix();
        self.utc_offset_s = rec::utc_offset_s();
        self.run_uid = rec::run_uid(armed_unix, &self.plan.run_id);
        let station = self.engine.take_station_facts().ok_or_else(|| {
            MethodError::internal("the run's engine said nothing of its station at arm")
        })?;
        let streams_off = streams_off.or_else(|| {
            station
                .links
                .is_none()
                .then(|| "the run has no station whose blocks the record can take".to_owned())
        });
        self.control = table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Control)
            .map_or((0, "g".to_owned()), |c| {
                (c.index, crate::table::unit_name(&c.units))
            });
        self.record_grid = (a.fs_hz, a.record_len, a.section_start);
        let law = self.view.law.as_ref().map_or(Value::Null, |l| {
            json!({"id": l.id, "name": l.name, "version": l.version,
                   "sha256": if l.sha256.is_empty() { Value::Null } else { Value::from(l.sha256.as_str()) },
                   "builtin": l.sha256.is_empty(), "params": cfg.law.params,
                   "guards": {"law_budget": cfg.law_budget, "law_misses": cfg.law_misses,
                              "max_peak_rise_db": cfg.max_peak_rise_db,
                              "max_bin_rise_db": cfg.max_bin_rise_db,
                              "first_drive_ratio": cfg.first_drive_ratio,
                              "alignment_frames": cfg.alignment_frames,
                              "rest_taper": true}})
        });
        let facts = trec::ArmFacts {
            run_id: &self.plan.run_id,
            run_uid: &self.run_uid,
            cfg: &cfg,
            derived: &self.plan.derived,
            table,
            revision,
            station: &station,
            stored,
            fs_hz: a.fs_hz,
            record_len: a.record_len,
            section_start: a.section_start,
            section_len: a.section_len,
            feasibility: &a.feasibility,
            law,
            armed_unix,
            utc_offset_s: self.utc_offset_s,
            streams_off: streams_off.clone(),
        };
        let begin = trec::begin(&facts, root);
        self.shot_streams = (0..begin.streams.len()).collect();
        let shared = self.shared.clone();
        let run_id = self.plan.run_id.clone();
        let problem: ProblemFn = Box::new(move |words: String, data: Value| {
            shared.add_log_data("record", words.clone(), Some(data));
            let mut v = (*shared.view()).clone();
            if let Some(r) = v.record.as_mut() {
                r.problem = Some(words);
                shared.publish(&run_id, v);
            }
        });
        let record = RunRecord::open(begin, &self.plan.record, problem).map_err(|e| {
            refuse(
                "record_unwritable",
                format!(
                    "The run's record could not be written, so the run did not arm: {} ({}). \
                     Choose a data folder that can be written.",
                    e.path.display(),
                    e.reason
                ),
            )
            .with_detail(json!({"path": e.path.display().to_string(), "reason": e.reason}))
        })?;
        self.shared.attach_record(record.log());
        if let Some(why) = &streams_off {
            self.shared.add_log_data(
                "record",
                format!("the run's streams are not recorded: {why}"),
                Some(json!({"stream": null, "failed": why})),
            );
        }
        self.record_table = Some(table.clone());
        self.shots_named = 0;
        let target = self.engine.full_level_target();
        for (name, d, arrays) in trec::reference_results(
            stored,
            target
                .as_deref()
                .map(|t| (t, a.fs_hz, a.section_start, a.section_len)),
            cfg.envelope_alarm_frac,
            cfg.envelope_abort_frac,
            self.control.0,
        ) {
            record.put_result(&name, d, arrays, None);
        }
        self.view.record = Some(RecordRef {
            folder: record.folder().display().to_string(),
            streams: streams_off.is_none(),
            problem: None,
        });
        self.record_status = Some(record.status());
        self.record = Some(record);
        Ok(())
    }

    fn frame(&self) -> Option<u64> {
        let f = self.shared.station_frame.load(Ordering::Relaxed);
        (f != NO_FRAME).then_some(f)
    }

    /// Name the shot the station's last start began, when it began one since the
    /// last named: the identification (`None`) or a play. The shot's ordinal, or
    /// `None` when nothing was played.
    fn name_shot(&mut self, play: Option<&PlayFacts<'_>>) -> Option<usize> {
        let starts = self.engine.shots();
        if starts <= self.shots_named {
            return None;
        }
        self.shots_named = starts;
        let ordinal = (starts - 1) as usize;
        if let Some(r) = self.record.as_ref() {
            let label = trec::shot_label(play, self.record_grid.2);
            for &k in &self.shot_streams {
                r.label_shot(k, ordinal, label.clone());
            }
        }
        Some(ordinal)
    }

    /// The identification's transfer function and coherence, and the lag every
    /// play is aligned at, once it is done.
    fn record_identification(&self) {
        let (Some(r), Some(table)) = (self.record.as_ref(), self.record_table.as_ref()) else {
            return;
        };
        if let Some(id) = self.engine.ident_record() {
            for (name, d, arrays) in crate::shock::record::identification_results(&id, table) {
                r.put_result(&name, d, arrays, None);
            }
        }
        if let (Some((lag, how)), Some(&drives)) = (self.engine.lag(), self.shot_streams.get(1)) {
            let mut patch = Map::new();
            patch.insert(
                "pipeline_lag_frames".into(),
                json!({"frames": num(lag), "how": how}),
            );
            r.patch_stream(drives, patch);
        }
    }

    /// One play into the record and the log: its results, and an entry with its
    /// level, error and how it ended.
    fn record_play(&self, p: &PlayFacts<'_>, shot: Option<usize>) {
        let frame = self.frame();
        let (described, line) = trec::play_result(p, shot, frame, self.control.0, &self.control.1);
        if let Some(r) = self.record.as_ref() {
            let (name, d, arrays) = described;
            r.put_result(&name, d, arrays, Some(line));
        }
        let kind = if p.playback { "playback" } else { "iteration" };
        let said = match (p.row, p.outcome) {
            (Some(row), _) => format!(
                "play {} ({kind} at {}): {} % rms error, {}{}",
                p.index,
                level_words(p.level_db),
                num(crate::shock::view::sig(row.rms_error_pct, 4)),
                if row.gated {
                    "met the gate"
                } else {
                    "above the gate"
                },
                if p.outcome == PlayOutcome::Tripped {
                    "; the run ended on it"
                } else {
                    ""
                }
            ),
            (None, PlayOutcome::Cancelled) => format!(
                "play {} ({kind} at {}) was cut short after {} frames",
                p.index,
                level_words(p.level_db),
                p.frames_done.unwrap_or(0)
            ),
            (None, _) => format!(
                "play {} ({kind} at {}) was cut short by a protection",
                p.index,
                level_words(p.level_db)
            ),
        };
        self.shared.add_log_data(
            "play",
            said,
            Some(json!({
                "play": p.index, "kind": kind,
                "level_db": num(crate::shock::config::round(p.level_db, 2)),
                "iteration": p.iteration,
                "rms_error_pct": p.row.map(|r| num(crate::shock::view::sig(r.rms_error_pct, 6))),
                "peak_error_pct": p.row.map(|r| num(crate::shock::view::sig(r.peak_error_pct, 6))),
                "spectral_error_db": p.row.map(|r| num(crate::shock::view::sig(r.spectral_error_db, 6))),
                "gated": p.row.map(|r| r.gated),
                "outcome": p.outcome.name(),
                "shot": shot,
            })),
        );
    }

    /// The law's calls since the last, into the record.
    fn record_law_calls(&mut self) {
        let calls = match self.call("the law's records", |e| e.take_law_calls()) {
            Some(c) => c,
            None => return,
        };
        let frame = self.frame();
        for call in &calls {
            let (described, line) =
                trec::law_call_result(call, self.law_calls, self.record_grid, frame);
            self.law_calls += 1;
            if let Some(r) = self.record.as_ref() {
                let (name, d, arrays) = described;
                r.put_result(&name, d, arrays, Some(line));
            }
        }
    }

    /// End the record: `end.json` with the run's end as its state reports it,
    /// then `complete`. Stop appending the log first, so the last entry is the end.
    fn finish_record(&mut self) {
        self.shared.detach_record();
        let Some(r) = self.record.take() else {
            return;
        };
        let mut end = Map::new();
        end.insert("run_uid".into(), Value::from(self.run_uid.as_str()));
        end.insert(
            "end".into(),
            self.view.end.as_ref().map_or(Value::Null, End::to_json),
        );
        end.insert("at_unix".into(), Value::from(now_unix()));
        end.insert(
            "station_frames".into(),
            Value::from(self.frame().unwrap_or(0)),
        );
        end.insert(
            "summary".into(),
            json!({"plays": self.data.row_count(), "playback_plays": self.view.plays.playback,
                   "law_calls": self.law_calls, "level_db": self.view.level_db,
                   "t_s": self.view.t_s}),
        );
        r.finish(end);
    }

    fn level_cmd(&mut self, to: LevelMove) -> Result<f64, MethodError> {
        // In `auto` a level moves only from the hold a failed gate leaves under the
        // `hold` policy.
        if self.plan.cfg.mode == TwrRunMode::Auto && self.view.phase != "holding" {
            return Err(wrong_mode("A level change"));
        }
        let pos = self.engine.position();
        let live = matches!(
            (self.view.state, self.view.phase),
            (State::Running, "waiting") | (State::Running, "holding") | (State::Review, _)
        ) && !pos.playing;
        if !live {
            return Err(wrong_state(&self.view, "A level change"));
        }
        let levels = &self.plan.cfg.levels_db;
        let (lo, hi) = (
            levels.first().copied().unwrap_or(0.0),
            levels.last().copied().unwrap_or(0.0),
        );
        let target = match to {
            LevelMove::Step(d) => pos.level_db + d,
            LevelMove::Rung(i) => *levels.get(i).ok_or_else(|| {
                MethodError::named(
                    ErrorCode::BadRequest,
                    "not_a_rung",
                    format!(
                        "The table has {} levels; rung {i} is not one.",
                        levels.len()
                    ),
                )
                .with_detail(
                    json!({"levels_db": levels.iter().map(|l| num(*l)).collect::<Vec<_>>()}),
                )
            })?,
        };
        if target < lo - 1e-9 || target > hi + 1e-9 {
            return Err(MethodError::named(
                ErrorCode::BadRequest,
                "not_a_rung",
                format!(
                    "{} dB is outside the table's levels ({} to {} dB).",
                    num(crate::shock::config::round(target, 2)),
                    num(lo),
                    num(hi)
                ),
            )
            .with_detail(
                json!({"levels_db": levels.iter().map(|l| num(*l)).collect::<Vec<_>>()}),
            ));
        }
        if let Some(vpk) = self.engine.predicted_vpk_at(target) {
            let ceiling = self.plan.cfg.drive_vpk_ceiling;
            if vpk > ceiling {
                return Err(refuse(
                    "drive_ceiling",
                    format!(
                        "The drive in force would need {} V pk at {}; the drive ceiling is {} V \
                         pk. The level and the drive are unchanged.",
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
        }
        let Some(r) = self.call("a level change", |e| e.set_level(to)) else {
            return Err(MethodError::internal("the run failed changing level"));
        };
        let level = r?;
        self.level_moved = true;
        self.data.set_target(self.engine.level_target());
        if self.view.phase == "holding" {
            self.set(State::Running, "waiting");
        }
        self.shared.add_log_data(
            "level",
            format!("level {}", level_words(level)),
            Some(json!({"action": "level",
                        "level_db": num(crate::shock::config::round(level, 2))})),
        );
        Ok(level)
    }

    // ---- the end --------------------------------------------------------------------

    fn end_on_stop(&mut self) {
        let by = self
            .shared
            .ended_by()
            .unwrap_or_else(|| self.stopped_by.clone());
        let outcome = self.shared.stop_outcome().unwrap_or(self.stop_outcome);
        let message = match outcome {
            "safe_stop" if by == SHUTDOWN_BY => {
                "The service was shut down; the run took its controlled stop.".to_owned()
            }
            "safe_stop" => format!(
                "Nobody held control for {} s; the run took its controlled stop.",
                num(self.plan.timeout_s.round())
            ),
            _ => format!("Stopped by {by}."),
        };
        let _ = guarded(|| self.engine.stop());
        self.end(outcome, None, Some(message));
    }

    fn end_on_abort(&mut self) {
        let by = self
            .shared
            .ended_by()
            .unwrap_or_else(|| self.stopped_by.clone());
        let engine = &mut self.engine;
        let _ = guarded(|| engine.abort("the operator aborted"));
        self.end("operator_abort", None, Some(format!("Aborted by {by}.")));
    }

    fn end_engine(&mut self, end: EngineEnd) {
        if end.outcome != "completed" {
            self.shared.add_log_data(
                "trip",
                end.message.clone(),
                Some(json!({"outcome": end.outcome,
                            "trip": end.trip.as_ref().map_or(Value::Null, Trip::to_json)})),
            );
        }
        let _ = guarded(|| self.engine.stop());
        self.end(end.outcome, end.trip, Some(end.message));
    }

    fn end(&mut self, outcome: &'static str, trip: Option<Trip>, message: Option<String>) {
        if self.view.state == State::Finished {
            return;
        }
        let phase = match outcome {
            "completed" | "operator_stop" | "safe_stop" => "stopped",
            _ => "aborted",
        };
        let message = message.unwrap_or_else(|| outcome.replace('_', " "));
        // The controller's last lines before the end, so the end is the log's last.
        self.forward_log();
        let fs = self.view.grid.fs_hz;
        let at_s =
            (self.view.grid.actual && fs > 0.0).then(|| self.engine.frames_clocked() as f64 / fs);
        let (play, frames_done) = match self.cancelled {
            Some((i, done)) => (Some(i), Some(done)),
            None => (
                (self.view.plays.done > 0).then(|| self.view.plays.done - 1),
                None,
            ),
        };
        self.view.end = Some(End {
            outcome,
            message: message.clone(),
            at_s,
            level_db: self.view.level_db,
            play,
            frames_done,
            trip,
        });
        self.set(State::Finished, phase);
        let by = matches!(outcome, "operator_stop" | "operator_abort" | "safe_stop").then(|| {
            self.shared
                .ended_by()
                .unwrap_or_else(|| self.stopped_by.clone())
        });
        self.shared.add_log_data(
            "state",
            format!("ended ({outcome}): {message}"),
            Some(json!({"outcome": outcome, "by": by,
                        "end": self.view.end.as_ref().map_or(Value::Null, End::to_json)})),
        );
        self.publish();
        self.finish_record();
        self.lock = None;
    }
}
