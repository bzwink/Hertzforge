//! One configured SRS shock run, on its own thread, on the transient-run rails
//! ([`crate::pulse::rails`]).
//!
//! The thread owns the run's engine ([`super::engine`]) outright and drives it in
//! the order the protocol states: arm, identification, the review hold, the seed
//! and the notch, then pulses (in `auto` at the time between pulses, in `manual` on
//! `srs.fire`), and the end. Nothing here is control math.
//! Requests arrive as [`Cmd`]s the thread takes between pulses; while it waits for
//! the next pulse it keeps taking them. The thread publishes a view after every
//! step and puts each pulse's row and figures where the service reads them without
//! the thread.
//!
//! Stop and Abort reach a fire in flight through the run's cancel flag, raised by
//! the service before it queues the request. An absent controller raises the hold:
//! no pulse starts while it stands. Every engine call runs under `catch_unwind`; a
//! panic ends the run in the emergency stop. The station is held only while the
//! run is live.

use super::config::{Grid, WireSrs};
use super::engine::{
    ArmOutcome, ArmPlan, EngineContext, EngineEnd, EngineFactory, Fired, Identified, LevelMove,
    Seeded, SrsEngine,
};
use super::preview::Synthesis;
use super::record::{self as srec, Records, Shot};
use super::view::{End, RecordRef, SrsView, State, Trip};
use super::RunData;
use crate::protocol::{num, ErrorCode, MethodError};
use crate::pulse::rails::{self, guarded, now_unix, Shared, Waited};
use crate::random::record as rec;
use crate::random::run::NO_FRAME;
use crate::record::{ProblemFn, RecordStatus, RunRecord};
use crate::station::{NiScanner, StationPlan};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use hertzforge_shock_common::ShockRunMode;
use hertzforge_srs::SrsLawFactory;
use serde_json::{json, Map, Value};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Who stops a run when the service shuts down.
pub const SHUTDOWN_BY: &str = crate::random::run::SHUTDOWN_BY;

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
    pub law: Result<Arc<dyn SrsLawFactory>, MethodError>,
    /// The values the law reports, as its manifest declares them.
    pub law_values: Vec<hertzforge_law_host::DiagnosticSpec>,
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
    Fire {
        reply: Reply<()>,
    },
    Level {
        to: LevelMove,
        reply: Reply<f64>,
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
    pub cfg: Arc<WireSrs>,
    pub engine: EngineFactory,
    pub sim_speed: f64,
    /// The orphan timeout, for the safe stop's message.
    pub timeout_s: f64,
    /// The derived values as configure gave them, for the record's setup.
    pub derived: Value,
    /// The offline synthesis the configuration was accepted with.
    pub synthesis: Arc<Synthesis>,
    /// How the run's record is written.
    pub record: crate::record::RecordOptions,
}

fn refuse(reason: &'static str, message: impl Into<String>) -> MethodError {
    MethodError::named(ErrorCode::Refused, reason, message)
}

pub fn wrong_state(v: &SrsView, verb: &str) -> MethodError {
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

/// The refusal of a pulse before the time between pulses has passed.
pub fn not_yet(seconds: f64) -> MethodError {
    refuse(
        "not_yet",
        format!(
            "The next pulse may fire in {} s, when the time between pulses has passed.",
            num(super::config::round(seconds.max(0.0), 2))
        ),
    )
    .with_detail(json!({"seconds": num(super::config::round(seconds.max(0.0), 3))}))
}

/// The run thread's state.
struct Actor {
    plan: RunPlan,
    shared: Arc<Shared<SrsView>>,
    data: Arc<RunData>,
    engine: Box<dyn SrsEngine>,
    view: SrsView,
    lock: Option<hertzforge_station::lock::LockGuard>,
    started: Option<Instant>,
    ended_at: Option<f64>,
    /// When the last pulse started, as the engine stamped it once the device's
    /// clock ran (this thread's own moment of asking when the engine keeps none):
    /// the next is due a spacing later.
    last_start: Option<Instant>,
    seeded: bool,
    stop_outcome: &'static str,
    stopped_by: String,
    /// The run's record, from arm until the run ends.
    record: Option<RunRecord>,
    record_status: Option<Arc<RecordStatus>>,
    run_uid: String,
    utc_offset_s: Option<i64>,
    /// The streams the record holds, by their place in it: the inputs and the drive.
    shot_streams: Vec<usize>,
    /// The shots the station wrapper has begun, and which of them are named.
    shots: Arc<AtomicU64>,
    named: Vec<bool>,
    /// The control channel and its unit, and the drive channel.
    control: (u32, String, Option<u32>),
    /// The next record of each repeating result.
    records: Records,
    /// The table the run armed with, for the record's channels.
    record_table: Option<ChannelTable>,
}

/// Start the run thread. Returns once the thread exists; the run is `configured`.
pub fn spawn(
    plan: RunPlan,
    initial: SrsView,
    shared: Arc<Shared<SrsView>>,
    data: Arc<RunData>,
    rx: Receiver<Cmd>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let name = format!("srs-{}", plan.run_id);
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
            started: None,
            ended_at: None,
            last_start: None,
            seeded: false,
            stop_outcome: "operator_stop",
            stopped_by: "the operator".into(),
            record: None,
            record_status: None,
            run_uid: String::new(),
            utc_offset_s: None,
            shot_streams: Vec::new(),
            shots: Arc::new(AtomicU64::new(0)),
            named: Vec::new(),
            control: (0, "g".into(), None),
            records: Records::default(),
            record_table: None,
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
                    if !self.between_pulses(&rx) {
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

    fn publish(&mut self) {
        if let (Some(status), Some(r)) = (self.record_status.as_ref(), self.view.record.as_mut()) {
            r.problem = status.problem();
        }
        if let Some(t0) = self.started {
            self.view.elapsed_s = Some(self.ended_at.unwrap_or_else(|| t0.elapsed().as_secs_f64()));
        }
        self.view.misses = self.engine.misses();
        // A finished run keeps the counts of its last pulse.
        if self.armed() || self.view.state == State::Finished {
            let p = self.engine.position();
            self.view.level_db = Some(p.level_db);
            self.view.rung = p.rung;
            self.view.pulses.at_level = p.at_level;
            self.view.pulses.of_level = p.of_level;
            self.view.pulses.passed = p.passed;
        }
        self.forward_log();
        self.shared.publish(&self.plan.run_id, self.view.clone());
    }

    fn armed(&self) -> bool {
        matches!(
            self.view.state,
            State::Armed | State::Running | State::Review | State::Stopping
        )
    }

    fn forward_log(&mut self) {
        for (kind, text, data) in self.engine.take_log() {
            self.shared.add_log_data(kind, text, data);
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
    fn call<T>(&mut self, what: &str, f: impl FnOnce(&mut dyn SrsEngine) -> T) -> Option<T> {
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
            Cmd::Proceed { reply } => {
                let ok = self.view.state == State::Review;
                if !ok {
                    let _ = reply.send(Err(wrong_state(&self.view, "Proceeding")));
                    return;
                }
                self.shared.add_log_data(
                    "note",
                    "the operator reviewed identification and proceeded",
                    Some(json!({"action": "proceed"})),
                );
                let _ = reply.send(Ok(()));
                self.seed();
            }
            Cmd::Fire { reply } => match self.may_fire("A fire") {
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
                Ok(()) => {
                    let _ = reply.send(Ok(()));
                    self.shared.add_log_data(
                        "note",
                        "the operator fired a pulse",
                        Some(json!({"action": "fire"})),
                    );
                    self.fire(|e| e.fire());
                }
            },
            Cmd::Level { to, reply } => {
                let r = self.level_cmd(to);
                self.publish();
                let _ = reply.send(r);
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
        self.started = Some(Instant::now());
        self.set(State::Running, "sysid");
        self.shared.add_log_data(
            "note",
            format!(
                "started; identification at {} dB, at most {} V pk",
                num(self.plan.cfg.sysid_level_db),
                num(super::config::round(
                    self.plan.cfg.identification_vpk_cap(),
                    4
                ))
            ),
            Some(
                json!({"action": "start", "level_db": self.plan.cfg.sysid_level_db,
                        "vpk_cap": self.plan.cfg.identification_vpk_cap()}),
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
        let cfg = self.plan.cfg.clone();
        let root = ctx.records_root.clone();
        // The streams are recorded unless the setup leaves them out or the disk is
        // already under its free-space floor.
        let streams_off = if !cfg.record_streams {
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
        let plan = ArmPlan {
            cfg,
            table: ctx.table,
            station: ctx.station,
            scanner: ctx.scanner,
            data_root: ctx.data_root,
            bind: ctx.bind,
            law: ctx.law,
            law_values: ctx.law_values,
            ack: ack.clone(),
            devices: ctx.devices,
            streams: streams_off.is_none(),
            stream_buffer_s: self.plan.record.stream_buffer_s,
            station_frame: self.shared.station_frame.clone(),
            shots: self.shots.clone(),
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
                    "Feasibility exceeded: the pulse asks more of the shaker than its ratings. \
                     Review it, or arm with an acknowledgement.",
                )
                .with_detail(json!({
                    "feasibility": {"verdict": feasibility.0, "message": feasibility.1}
                })))
            }
            ArmOutcome::Armed(a) => {
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
                if let Err(e) = self.open_record(&a, &table, ctx.revision, root, streams_off) {
                    self.engine.release();
                    return Err(e);
                }
                self.view.grid = Grid {
                    fs_hz: a.fs_hz,
                    record_len: a.record_len,
                    actual: true,
                };
                self.view.clock = Some("shared");
                self.view.station = a.station;
                self.view.streaming = true;
                self.lock = a.lock;
                self.set(State::Armed, "armed");
                self.shared.add_log_data(
                    "station",
                    format!(
                        "armed on {} at {} Hz, records of {} samples",
                        self.view.station.class,
                        num(super::config::round(a.fs_hz, 3)),
                        a.record_len
                    ),
                    Some(json!({"class": self.view.station.class, "fs_hz": a.fs_hz,
                                "record_len": a.record_len})),
                );
                Ok(())
            }
        }
    }

    // ---- identification, review, seed --------------------------------------------

    fn identify(&mut self) {
        self.publish();
        let Some(r) = self.call("identification", |e| e.identify()) else {
            return;
        };
        // The controller's own lines first, so the log is in the order things
        // happened.
        self.forward_log();
        // The identification is the streams' first shot, whatever came of it.
        if let Some(k) = self.shots_begun().checked_sub(1) {
            let level_db = self.plan.cfg.sysid_level_db;
            self.name_shot(k, Shot::Identification { level_db });
        }
        self.record_identification();
        if self.interrupted() {
            return;
        }
        match r {
            Err(e) => self.end("fault", None, Some(e.message)),
            Ok(Identified::Held { facts, message }) => {
                self.view.identification = facts;
                self.end("held", None, Some(message));
            }
            Ok(Identified::Done { facts, review }) => {
                self.view.identification = Some(facts);
                self.data.set_identification(self.engine.identification());
                if review {
                    self.set(State::Review, "awaiting_review");
                    self.publish();
                } else {
                    self.seed();
                }
            }
        }
    }

    fn seed(&mut self) {
        let Some(r) = self.call("the law's seed", |e| e.seed()) else {
            return;
        };
        self.forward_log();
        if self.interrupted() {
            return;
        }
        match r {
            Err(e) => self.end("fault", None, Some(e.message)),
            Ok(Seeded::Ended(end)) => self.end_engine(end),
            Ok(Seeded::Ready(prediction)) => {
                self.view.prediction = Some(prediction);
                self.view.notch = self.engine.notch();
                self.seeded = true;
                self.data.set_law(self.engine.law_values());
                self.set(State::Running, "waiting");
                self.shared.add_log_data(
                    "note",
                    format!(
                        "seeded; the predicted full-level drive is {} V pk",
                        num(super::config::round(prediction.drive_vpk, 4))
                    ),
                    Some(json!({"seeded": true, "drive_vpk": prediction.drive_vpk,
                                "drive_vpk_ceiling": prediction.drive_vpk_ceiling,
                                "drive_vpk_abort": prediction.drive_vpk_abort,
                                "notch": self.view.notch.map(|n| json!({
                                    "notched_bands": n.notched_bands,
                                    "converged": n.converged}))})),
                );
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

    // ---- between pulses and firing -----------------------------------------------

    fn spacing(&self) -> Duration {
        Duration::from_secs_f64(self.plan.cfg.spacing_s())
    }

    fn next_due(&self) -> Instant {
        self.last_start
            .map_or_else(Instant::now, |t| t + self.spacing())
    }

    /// Wait between pulses: take requests, and in `auto` fire when the next pulse is
    /// due. False when the service has dropped the run.
    fn between_pulses(&mut self, rx: &Receiver<Cmd>) -> bool {
        let auto = self.plan.cfg.mode == ShockRunMode::Auto;
        if !auto || !self.seeded {
            self.data.set_next_due(self.seeded.then(|| self.next_due()));
            match rx.recv_timeout(rails::POLL) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
            if self.view.state.is_live() && self.interrupted() {
                return true;
            }
            return true;
        }
        let due = self.next_due();
        self.data.set_next_due(Some(due));
        // Ask the engine its lead ahead of the due time: the controller brings
        // the shot up and waits out the rest itself, so only the start lands on
        // the due time (`rails::lead_for`). The countdown still shows the due time.
        let lead = self.engine.lead().min(self.spacing());
        let ask = due.checked_sub(lead).unwrap_or(due);
        let signals = self.shared.signals.clone();
        let mut gone = false;
        let waited = rails::wait_until(ask, &signals, || loop {
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
            Waited::Due => self.fire(|e| e.fire()),
        }
        true
    }

    /// Whether a pulse may fire now on the operator's word.
    fn may_fire(&self, verb: &str) -> Result<(), MethodError> {
        if self.plan.cfg.mode == ShockRunMode::Auto {
            return Err(wrong_mode(verb));
        }
        if !(self.view.state == State::Running && self.view.phase == "waiting" && self.seeded) {
            return Err(wrong_state(&self.view, verb));
        }
        let left = self.next_due().saturating_duration_since(Instant::now());
        if !left.is_zero() {
            return Err(not_yet(left.as_secs_f64()));
        }
        Ok(())
    }

    fn fire(&mut self, f: impl FnOnce(&mut dyn SrsEngine) -> Result<Fired, MethodError>) {
        self.last_start = Some(Instant::now());
        self.data.set_next_due(None);
        self.set(State::Running, "firing");
        self.publish();
        let Some(r) = self.call("a pulse", f) else {
            return;
        };
        self.forward_log();
        // The controller's own start time for the pulse: the next is due the time
        // between pulses after the device started, not after this thread asked.
        if let Some(t) = self.engine.last_pulse_start() {
            self.last_start = Some(t);
        }
        // Each pulse's bring-up, one line each in the service log (standard
        // error; not `eprintln!`, which panics once a parent's pipe is closed).
        for (index, b) in self.engine.take_bring_ups() {
            let _ = writeln!(
                std::io::stderr(),
                "sds-control: srs run {} pulse {index} bring-up: {}",
                self.plan.run_id,
                b.line()
            );
        }
        let fired = match r {
            Ok(f) => f,
            Err(e) => {
                self.end("fault", None, Some(e.message));
                return;
            }
        };
        self.view.pulses.fired += fired.rows.len() as u64;
        if let Some(last) = fired.rows.last() {
            self.view.alarm = last.alarm_bands.is_some_and(|n| n > 0);
            self.view.last_pulse = Some(last.clone());
        }
        // Each pulse into the record as it is judged, the last included.
        self.record_pulses(&fired);
        self.data.add_pulses(fired.rows, fired.figures);
        self.data.set_law(self.engine.law_values());
        // The law moved the table: the prediction is the table in force's.
        if let Some(p) = self.engine.prediction() {
            self.view.prediction = Some(p);
        }
        if let Some(end) = fired.end {
            self.end_engine(end);
            return;
        }
        if self.interrupted() {
            return;
        }
        self.set(State::Running, "waiting");
        self.publish();
    }

    fn level_cmd(&mut self, to: LevelMove) -> Result<f64, MethodError> {
        if self.plan.cfg.mode == ShockRunMode::Auto {
            return Err(wrong_mode("A level change"));
        }
        let live = matches!(
            (self.view.state, self.view.phase),
            (State::Running, "waiting") | (State::Review, _)
        );
        if !live {
            return Err(wrong_state(&self.view, "A level change"));
        }
        let levels = &self.plan.cfg.levels_db;
        let (lo, hi) = (
            levels.first().copied().unwrap_or(0.0),
            levels.last().copied().unwrap_or(0.0),
        );
        let target = match to {
            LevelMove::Step(d) => self.engine.position().level_db + d,
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
                    num(super::config::round(target, 2)),
                    num(lo),
                    num(hi)
                ),
            )
            .with_detail(
                json!({"levels_db": levels.iter().map(|l| num(*l)).collect::<Vec<_>>()}),
            ));
        }
        if let Some(vpk) = self.engine.predicted_vpk_at(target) {
            let abort = self.plan.cfg.drive_vpk_abort;
            if vpk > abort {
                return Err(refuse(
                    "drive_ceiling",
                    format!(
                        "The identified drive needs {} V pk at {} dB; the drive abort is {} V pk.",
                        num(super::config::round(vpk, 3)),
                        num(super::config::round(target, 2)),
                        num(abort)
                    ),
                )
                .with_detail(json!({
                    "vpk": num(super::config::round(vpk, 4)),
                    "abort_vpk": num(abort),
                    "level_db": num(super::config::round(target, 2)),
                })));
            }
        }
        let Some(r) = self.call("a level change", |e| e.set_level(to)) else {
            return Err(MethodError::internal("the run failed changing level"));
        };
        let level = r?;
        self.shared.add_log_data(
            "level",
            format!("level {} dB", num(super::config::round(level, 2))),
            Some(json!({"level_db": level, "action": match to {
                LevelMove::Step(d) => json!({"step_db": d}),
                LevelMove::Rung(i) => json!({"rung": i}),
            }})),
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
        self.forward_log();
        let phase = match outcome {
            "aborted" | "operator_abort" | "fault" | "law_fault" => "aborted",
            "held" => "held",
            _ => "stopped",
        };
        self.ended_at = self.started.map(|t| t.elapsed().as_secs_f64());
        let message = message.unwrap_or_else(|| outcome.replace('_', " "));
        let pulse = (self.view.pulses.fired > 0).then(|| self.view.pulses.fired - 1);
        self.view.end = Some(End {
            outcome,
            message: message.clone(),
            at_s: self.ended_at,
            level_db: self.view.level_db,
            pulse,
            trip,
        });
        self.data.set_next_due(None);
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

    // ---- the record -----------------------------------------------------------------

    /// Open the run's record at arm: the folder claimed, `run.json`, `table.json`,
    /// `setup.json` and the law written before any sample, the log attached, and
    /// the target with its envelope and the offline synthesis. Refused
    /// `record_unwritable` when it cannot be written.
    fn open_record(
        &mut self,
        a: &super::engine::Armed,
        table: &ChannelTable,
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
        let (index, unit) = table
            .channels
            .iter()
            .find(|c| c.enabled && c.role == ChannelRole::Control)
            .map_or((0, "g".to_owned()), |c| {
                (c.index, crate::table::unit_name(&c.units))
            });
        let drive = rec::drive_rows(table).first().map(|c| c.index);
        self.control = (index, unit, drive);
        let law = self.view.law.as_ref().map_or(Value::Null, |l| {
            json!({"id": l.id, "name": l.name, "version": l.version,
                   "sha256": if l.sha256.is_empty() { Value::Null } else { Value::from(l.sha256.as_str()) },
                   "builtin": l.sha256.is_empty(), "params": cfg.law.params,
                   "guards": {"law_budget": cfg.law_budget, "law_misses": cfg.law_misses,
                              "max_pulses": cfg.max_pulses,
                              "max_gain_rise_db": cfg.max_gain_rise_db}})
        });
        let synthesis = self.plan.synthesis.clone();
        let facts = srec::ArmFacts {
            run_id: &self.plan.run_id,
            run_uid: &self.run_uid,
            cfg: &cfg,
            synthesis: &synthesis,
            derived: &self.plan.derived,
            table,
            revision,
            station: &station,
            fs_hz: a.fs_hz,
            record_len: a.record_len,
            feasibility: &a.feasibility,
            law,
            armed_unix,
            utc_offset_s: self.utc_offset_s,
            streams_off: streams_off.clone(),
        };
        let begin = srec::begin(&facts, root);
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
        let n = synthesis.bands_hz.len();
        let edges = super::preview::envelope_edges(&synthesis.target_g, &cfg.envelope_bands(n));
        let mut results = srec::reference_results(
            &synthesis.bands_hz,
            &synthesis.target_g,
            &edges,
            cfg.synthesis.bands.q,
            self.control.0,
            &self.control.1,
        );
        results.extend(srec::synthesis_results(&synthesis, self.control.0));
        for (name, d, arrays) in results {
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

    fn shots_begun(&self) -> usize {
        usize::try_from(self.shots.load(Ordering::Relaxed)).unwrap_or(usize::MAX)
    }

    /// Name the streams' shot `ordinal`.
    fn name_shot(&mut self, ordinal: usize, shot: Shot<'_>) {
        let Some(r) = self.record.as_ref() else {
            return;
        };
        if self.named.len() <= ordinal {
            self.named.resize(ordinal + 1, false);
        }
        self.named[ordinal] = true;
        let facts = srec::shot_label(shot);
        for &k in &self.shot_streams {
            r.label_shot(k, ordinal, facts.clone());
        }
    }

    /// The identification's transfer function and coherence, once it is done.
    fn record_identification(&self) {
        let (Some(r), Some(table)) = (self.record.as_ref(), self.record_table.as_ref()) else {
            return;
        };
        let Some(id) = self.engine.ident_record() else {
            return;
        };
        for (name, d, arrays) in srec::identification_results(&id, table) {
            r.put_result(&name, d, arrays, None);
        }
    }

    /// Each pulse of a fire into the record: its shot named (a fire's pulses are
    /// its last shots), its SRS, drive table, kinematics, verdict and level.
    fn record_pulses(&mut self, fired: &Fired) {
        if self.record.is_none() {
            return;
        }
        let frame = self.frame();
        let first = self.shots_begun().saturating_sub(fired.rows.len());
        for (k, (row, fig)) in fired.rows.iter().zip(&fired.figures).enumerate() {
            self.name_shot(first + k, Shot::Pulse(row));
            let results = srec::pulse_results(
                row,
                fig,
                &mut self.records,
                frame,
                (self.control.0, self.control.2),
                &self.control.1,
            );
            let Some(r) = self.record.as_ref() else {
                return;
            };
            for ((name, d, arrays), line) in results {
                r.put_result(&name, d, arrays, Some(line));
            }
        }
    }

    /// End the record: every shot no pulse was judged on named so, `end.json`
    /// with the run's end as its state reports it, then `complete`. Stop
    /// appending the log first, so the last entry is the end.
    fn finish_record(&mut self) {
        for k in 0..self.shots_begun() {
            if !self.named.get(k).copied().unwrap_or(false) {
                self.name_shot(k, Shot::Unjudged);
            }
        }
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
            json!({"pulses_fired": self.view.pulses.fired, "pulses_passed": self.view.pulses.passed,
                   "elapsed_s": self.ended_at, "level_db": self.view.level_db}),
        );
        r.finish(end);
    }
}
