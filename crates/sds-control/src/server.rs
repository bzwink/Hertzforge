//! The WebSocket server: one [`Service`] shared by every connection.
//!
//! A run belongs to the service, not to a connection. One connection at a time holds
//! control ([`crate::session`]); every connection receives every event. Requests on
//! one connection are answered independently (a slow `random.abort` does not hold up
//! a `random.state`), so responses can arrive out of request order; the `id` pairs
//! them.
//!
//! The service listens on loopback only ([`check_loopback`]): every client is on the
//! computer that runs the test.

use crate::codec::{self, Arrays, Frame};
use crate::mimo::driver::DriverFactory;
use crate::mimo::MimoRun;
use crate::profiles;
use crate::protocol::{self, ErrorCode, MethodError, Request};
use crate::random::backend::{ABORT, STOP};
use crate::random::config as rconfig;
use crate::random::run::{self as rrun, ArmContext, Cmd, LevelTarget, RunPlan, RunShared};
use crate::random::subscribe::{self, Subscription};
use crate::random::view::{self, RunView, State, StationRef};
use crate::random::RandomRun;
use crate::session::{self, ConnId, Session};
use crate::sim::{self, RunRequest, SimRun};
use crate::station::{self, NiScanner, Selection, StationPlan, Stations};
use crate::table::{self, StationContext, Table};
use futures_util::{SinkExt, StreamExt};
use hertzforge_random::RunMode;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;

#[path = "server_mimo.rs"]
mod mimo_methods;

#[path = "server_shock.rs"]
mod shock_methods;

#[path = "server_srs.rs"]
mod srs_methods;

#[path = "server_mimo_shock.rs"]
mod mimo_shock_methods;

#[path = "server_mixed.rs"]
mod mixed_methods;
#[path = "server_sine.rs"]
mod sine_methods;

#[path = "server_explorer.rs"]
mod explorer_methods;
#[path = "server_recorder.rs"]
mod recorder_methods;
#[path = "server_twr.rs"]
mod twr_methods;

/// Display event topic of the `sim.*` runs.
pub const DISPLAY_TOPIC: &str = "random.display";
/// Display event period (5 per second), for `random.display` and `random.live`.
pub const DISPLAY_PERIOD: Duration = Duration::from_millis(200);
/// How often the live task looks for frames due: `random.data` runs up to 10 per
/// second, and `random.live` keeps every second look (5 per second).
const LIVE_TICK: Duration = Duration::from_millis(100);
/// How early a `random.data` frame may go against its period, so a 10 Hz
/// subscription is served on every look despite timer jitter.
const DATA_SLACK: Duration = Duration::from_millis(30);
/// How long `sim.stop` waits for the run to wind down.
const STOP_WAIT: Duration = Duration::from_secs(15);
/// Events buffered per connection before a slow client starts losing them.
const EVENT_BUFFER: usize = 256;
/// How long an arm may take: opening and committing NI tasks takes seconds.
const ARM_WAIT: Duration = Duration::from_secs(120);
/// How often the shutdown looks whether the run it stopped has ended.
const SHUTDOWN_POLL: Duration = Duration::from_millis(50);

/// The methods that could start a drive or raise one. Once shutdown has begun
/// ([`Service::shutdown`]) they are refused; one already under way finishes first.
const DRIVE_METHODS: [&str; 56] = [
    "random.arm",
    "random.start",
    "random.proceed",
    "random.level",
    "random.mode",
    "sim.start",
    "mimo_random.start",
    "mimo_random.proceed",
    "mimo_random.override",
    "mimo_random.exclude",
    "shock.arm",
    "shock.start",
    "shock.proceed",
    "shock.fire",
    "shock.level",
    "shock.train",
    "shock.burst",
    "srs.arm",
    "srs.start",
    "srs.proceed",
    "srs.fire",
    "srs.level",
    "sine.arm",
    "sine.start",
    "sine.proceed",
    "sine.level",
    "sine.mode",
    "sine.resume",
    "sine.reverse",
    "sine.jog",
    "sine.goto",
    "sine.adjust",
    "mixed.arm",
    "mixed.start",
    "mixed.proceed",
    "mixed.level",
    "mixed.mode",
    "mixed.resume",
    "mixed.part",
    "mixed.adjust",
    "twr.arm",
    "twr.start",
    "twr.proceed",
    "twr.iterate",
    "twr.play",
    "twr.level",
    "explorer.start",
    "explorer.steer",
    "explorer.sweep",
    "recorder.start",
    "recorder.arm",
    "recorder.record",
    "mimo_shock.arm",
    "mimo_shock.start",
    "mimo_shock.proceed",
    "mimo_shock.fire",
];

/// The methods this service serves, for the unknown-method message.
const METHODS: &str = "session.state, session.claim, session.release, station.list, \
    station.select, channels.get, channels.set, random.configure, random.arm, random.start, \
    random.proceed, random.level, random.mode, random.stop, random.abort, random.reset, \
    random.clear, random.state, random.config, random.log, random.subscribe, mimo_random.setup, \
    mimo_random.validate, mimo_random.start, mimo_random.proceed, mimo_random.override, \
    mimo_random.exclude, mimo_random.iterate_prediction, mimo_random.stop, mimo_random.abort, mimo_random.status, \
    mimo_random.report, mimo_random.subscribe, mimo_random.identification.save, \
    mimo_random.identification.load, mimo_random.reference.build_breakpoints, \
    mimo_random.reference.build_rigid_body, mimo_random.reference.validate, \
    mimo_random.reference.import, shock.configure, shock.config, shock.preview, shock.arm, \
    shock.start, shock.proceed, shock.fire, shock.level, shock.train, shock.burst, shock.stop, \
    shock.abort, shock.reset, shock.clear, shock.state, shock.log, shock.subscribe, \
    shock.pulses, shock.pulse, srs.configure, srs.config, srs.preview, srs.target.import, \
    srs.table.import, srs.arm, srs.start, srs.proceed, srs.fire, srs.level, srs.stop, srs.abort, \
    srs.reset, srs.clear, srs.state, srs.log, srs.subscribe, srs.pulses, srs.pulse, \
    mimo_shock.configure, mimo_shock.config, mimo_shock.arm, mimo_shock.start, \
    mimo_shock.proceed, mimo_shock.fire, mimo_shock.stop, mimo_shock.abort, mimo_shock.reset, \
    mimo_shock.clear, mimo_shock.state, mimo_shock.log, mimo_shock.subscribe, \
    mimo_shock.pulses, mimo_shock.pulse, \
    sine.configure, sine.config, sine.profile.import, sine.profile.validate, sine.arm, \
    sine.start, sine.proceed, sine.level, sine.mode, sine.hold, sine.resume, sine.reverse, \
    sine.jog, sine.goto, sine.adjust, sine.stop, sine.abort, sine.reset, sine.clear, \
    sine.state, sine.log, sine.subscribe, mixed.configure, mixed.config, mixed.arm, \
    mixed.start, mixed.proceed, mixed.level, mixed.mode, mixed.hold, mixed.resume, mixed.part, \
    mixed.adjust, mixed.stop, mixed.abort, mixed.reset, mixed.clear, mixed.state, mixed.log, \
    mixed.subscribe, twr.configure, twr.config, twr.reference.import, \
    twr.reference.preview, twr.arm, twr.start, twr.proceed, twr.iterate, twr.play, twr.level, \
    twr.freeze, twr.stop, twr.abort, twr.reset, twr.clear, twr.state, twr.log, twr.subscribe, \
    twr.plays, twr.record, explorer.configure, explorer.config, explorer.start, explorer.steer, \
    explorer.sweep, explorer.hold, explorer.stop, explorer.abort, explorer.survey, \
    explorer.survey.clear, explorer.reset, explorer.clear, explorer.state, explorer.log, \
    explorer.subscribe, recorder.configure, recorder.config, recorder.start, recorder.arm, \
    recorder.disarm, recorder.record, recorder.cut, recorder.stop, recorder.abort, \
    recorder.recordings, recorder.marks, recorder.reset, recorder.clear, recorder.state, \
    recorder.log, recorder.subscribe, laws.list, laws.import, laws.check, sim.profiles, \
    sim.start, sim.stop and sim.state";

/// Refuse to serve anywhere but loopback. `localhost` and any loopback address pass.
pub fn check_loopback(host: &str) -> Result<(), String> {
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    let ip: Option<IpAddr> = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok();
    match ip {
        Some(ip) if ip.is_loopback() => Ok(()),
        _ => Err(format!(
            "sds-control serves loopback only, and \"{host}\" is not a loopback address. Every \
             client of the control service runs on the computer that runs the test; use \
             --host 127.0.0.1 (the default), ::1 or localhost."
        )),
    }
}

/// How the service is set up. The defaults are the protocol's; tests shorten the
/// timers, give a fixed NI scan and run the simulator unpaced.
pub struct ServiceOptions {
    pub heartbeat: Duration,
    pub timeout: Duration,
    pub scanner: Arc<dyn NiScanner>,
    /// The local data directory: the NI hardware lock lives here.
    pub data_root: PathBuf,
    /// Where run folders go (`None`: the data root); see [`ServiceOptions::records_root`].
    pub capture_root: Option<PathBuf>,
    /// Simulated seconds per wall second for `random.*` on the simulator.
    pub sim_speed: f64,
    /// What the hardware lock names as this service's address.
    pub bind: String,
    /// Makes the driver of a MIMO run (`None`: the controller's MIMO Random service
    /// through [`crate::mimo::adapter::ServiceDriver`]). Tests that need no real run
    /// give [`simulated_mimo_driver`].
    pub mimo_driver: Option<DriverFactory>,
    /// Makes the engine of a classical shock run (`None`: the controller's
    /// classical shock service, [`crate::shock::engine::ServiceEngine`]). Tests that
    /// need no real run give [`simulated_shock_engine`].
    pub shock_engine: Option<crate::shock::engine::EngineFactory>,
    /// Makes the engine of an SRS shock run (`None`: the controller's SRS shock
    /// service, [`crate::srs::engine::ServiceEngine`]). Tests that need no real run
    /// give [`simulated_srs_engine`].
    pub srs_engine: Option<crate::srs::engine::EngineFactory>,
    /// Makes the engine of a TWR run (`None`: the controller's TWR service,
    /// [`crate::twr::engine::ServiceEngine`]). Tests that need no real run give
    /// [`simulated_twr_engine`].
    pub twr_engine: Option<crate::twr::engine::EngineFactory>,
    /// How a run's record is written (`docs/run-folder.md`).
    pub record: crate::record::RecordOptions,
}

/// The simulated MIMO driver ([`crate::mimo::sim::SimulatedMimoDriver`]) at `speed`
/// simulated seconds per wall second: for wire tests that exercise the protocol
/// without the controller's service. A run never uses it unless given it here.
pub fn simulated_mimo_driver(speed: f64) -> DriverFactory {
    Arc::new(move |_ctx| {
        Box::new(crate::mimo::sim::SimulatedMimoDriver::new(
            crate::mimo::sim::SimClock::wall(speed),
        ))
    })
}

/// The simulated classical shock engine ([`crate::shock::sim::SimulatedEngine`]):
/// for wire tests that exercise the protocol without the controller's service. A run
/// never uses it unless given it here.
pub fn simulated_shock_engine() -> crate::shock::engine::EngineFactory {
    Arc::new(|ctx| Box::new(crate::shock::sim::SimulatedEngine::new(ctx.clone())))
}

/// The simulated SRS shock engine ([`crate::srs::sim::SimulatedEngine`]): for wire
/// tests that exercise the protocol without the controller's service. A run never
/// uses it unless given it here.
pub fn simulated_srs_engine() -> crate::srs::engine::EngineFactory {
    Arc::new(|ctx| Box::new(crate::srs::sim::SimulatedEngine::new(ctx.clone())))
}

/// The simulated TWR engine ([`crate::twr::sim::SimulatedEngine`]): for wire tests
/// that exercise the protocol without the controller's service. A run never uses it
/// unless given it here.
pub fn simulated_twr_engine() -> crate::twr::engine::EngineFactory {
    Arc::new(|ctx| Box::new(crate::twr::sim::SimulatedEngine::new(ctx.clone())))
}

impl ServiceOptions {
    /// Where every module's run folders go: the capture root when one is given,
    /// else the data root. Whether a run records at all is its own capture
    /// setting; this only says where.
    pub fn records_root(&self) -> PathBuf {
        self.capture_root
            .clone()
            .unwrap_or_else(|| self.data_root.clone())
    }
}

impl Default for ServiceOptions {
    fn default() -> Self {
        ServiceOptions {
            heartbeat: session::HEARTBEAT,
            timeout: session::TIMEOUT,
            scanner: Arc::new(station::DriverScanner),
            data_root: hertzforge_capture::default_data_root(),
            capture_root: None,
            sim_speed: 1.0,
            bind: format!("ws://{}:{}/", crate::DEFAULT_HOST, crate::DEFAULT_PORT),
            mimo_driver: None,
            shock_engine: None,
            srs_engine: None,
            twr_engine: None,
            record: crate::record::RecordOptions::default(),
        }
    }
}

/// Which display feed a per-connection frame belongs to: a connection receives a
/// feed's frames only while it holds a subscription to that feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    Random,
    Mimo,
    Shock,
    Srs,
    MimoShock,
    Sine,
    Mixed,
    Twr,
    Explorer,
    Recorder,
}

/// A frame for every connection.
#[derive(Debug, Clone)]
pub enum Out {
    Text(String),
    Binary(Vec<u8>),
    /// A `random.live` frame: for the connections on the default subscription.
    Live(Vec<u8>),
    /// A feed's default frame (`sine.live`): for the connections without a
    /// subscription of their own to that feed.
    DefaultLive(Feed, Vec<u8>),
    /// A `random.data` or `mimo_random.data` frame for one connection only.
    To(ConnId, Feed, Vec<u8>),
    /// `session.changed`, written for each connection with its own role.
    Session {
        seq: u64,
        holder: Option<ConnId>,
        controller: Value,
        orphan_s: Value,
    },
}

impl Out {
    /// The frame as connection `conn` receives it, or `None` when it is not for
    /// `conn`. `subscribed` says whether `conn` has a subscription of its own now,
    /// checked as the frame goes out: once a connection has its answer to
    /// `random.subscribe`, it receives only the frames of its new choice.
    fn to_message(&self, conn: ConnId, subscribed: impl Fn(Feed) -> bool) -> Option<Message> {
        Some(match self {
            Out::Text(t) => Message::Text(t.clone()),
            Out::Binary(b) => Message::Binary(b.clone()),
            Out::Live(b) if !subscribed(Feed::Random) => Message::Binary(b.clone()),
            Out::Live(_) => return None,
            Out::DefaultLive(feed, b) if !subscribed(*feed) => Message::Binary(b.clone()),
            Out::DefaultLive(..) => return None,
            Out::To(to, feed, b) if *to == conn && subscribed(*feed) => Message::Binary(b.clone()),
            Out::To(..) => return None,
            Out::Session {
                seq,
                holder,
                controller,
                orphan_s,
            } => Message::Text(
                protocol::event(
                    "session.changed",
                    *seq,
                    Session::changed_for(conn, *holder, controller, orphan_s),
                )
                .to_string(),
            ),
        })
    }
}

/// A connection, as the service sees it.
#[derive(Debug, Clone)]
pub struct Conn {
    pub id: ConnId,
    pub peer: String,
}

/// One connection's own subscription (schema/PROTOCOL.md, "Live Data
/// Subscriptions") and when it was last sent a frame.
struct Display {
    sub: Arc<Subscription>,
    last: Option<Instant>,
}

#[derive(Default)]
struct SimSlot {
    run: Option<Arc<SimRun>>,
    started: u64,
}

struct Inner {
    session: Session,
    stations: Stations,
    table: Table,
    random: Option<Arc<RandomRun>>,
    runs: u64,
    /// The MIMO setup and its run, beside `random`: one test at a time across both.
    mimo: Option<Arc<MimoRun>>,
    mimo_runs: u64,
    /// The classical shock configuration and its run, beside the others.
    shock: Option<Arc<crate::shock::ShockRun>>,
    shock_runs: u64,
    /// The last `shock.state` sent, how much of the run's log, how many of its
    /// pulses, and whether its identification has been plotted.
    shock_state_key: String,
    shock_log_sent: (String, u64),
    shock_rows_sent: u64,
    shock_ident_sent: bool,
    /// The SRS shock configuration and its run, beside the others, and what of it
    /// has been sent.
    srs: Option<Arc<crate::srs::SrsRun>>,
    srs_runs: u64,
    srs_state_key: String,
    srs_log_sent: (String, u64),
    srs_rows_sent: u64,
    srs_ident_sent: bool,
    /// The MIMO Shock configuration and its run, beside the others, and what of
    /// it has been sent.
    mimo_shock: Option<Arc<crate::mimo_shock::MsRun>>,
    mimo_shock_runs: u64,
    mimo_shock_state_key: String,
    mimo_shock_log_sent: (String, u64),
    mimo_shock_rows_sent: u64,
    mimo_shock_ident_sent: bool,
    /// The Sine configuration and its run, beside the others, and what of it has
    /// been sent.
    sine: Option<Arc<crate::sine::SineRun>>,
    sine_runs: u64,
    sine_state_key: String,
    sine_log_sent: (String, u64),
    /// The Mixed Mode configuration and its run, and what of it has been sent.
    mixed: Option<Arc<crate::mixed::MixedRun>>,
    mixed_runs: u64,
    mixed_state_key: String,
    mixed_log_sent: (String, u64),
    /// The TWR configuration and its run, beside the others, and what of it has
    /// been sent.
    twr: Option<Arc<crate::twr::TwrRun>>,
    twr_runs: u64,
    twr_state_key: String,
    twr_log_sent: (String, u64),
    twr_rows_sent: u64,
    twr_ident_sent: bool,
    /// The Explorer configuration and its run, beside the others, and what of it has
    /// been sent.
    explorer: Option<Arc<crate::explorer::run::ExplorerRun>>,
    explorer_runs: u64,
    explorer_state_key: String,
    explorer_log_sent: (String, u64),
    /// The Recorder configuration and its run, beside the others, and what of it has
    /// been sent.
    recorder: Option<Arc<crate::recorder::run::RecorderRun>>,
    recorder_runs: u64,
    recorder_state_key: String,
    recorder_log_sent: (String, u64),
    sim: SimSlot,
    /// The last `random.state` sent, and how much of the current run's log.
    state_key: String,
    log_sent: (String, u64),
}

impl Inner {
    fn random_live(&self) -> bool {
        self.random
            .as_ref()
            .is_some_and(|r| r.view().state.is_live())
    }

    fn mimo_live(&self) -> bool {
        self.mimo.as_ref().is_some_and(|m| m.is_live())
    }

    fn shock_live(&self) -> bool {
        self.shock
            .as_ref()
            .is_some_and(|s| s.view().state.is_live())
    }

    fn srs_live(&self) -> bool {
        self.srs.as_ref().is_some_and(|s| s.view().state.is_live())
    }

    fn mimo_shock_live(&self) -> bool {
        self.mimo_shock
            .as_ref()
            .is_some_and(|s| s.view().state.is_live())
    }

    fn sine_live(&self) -> bool {
        self.sine.as_ref().is_some_and(|s| s.view().state.is_live())
    }

    fn mixed_live(&self) -> bool {
        self.mixed
            .as_ref()
            .is_some_and(|s| s.view().state.is_live())
    }

    fn twr_live(&self) -> bool {
        self.twr.as_ref().is_some_and(|s| s.view().state.is_live())
    }

    fn explorer_live(&self) -> bool {
        self.explorer
            .as_ref()
            .is_some_and(|s| s.view().state.is_live())
    }

    fn recorder_live(&self) -> bool {
        self.recorder
            .as_ref()
            .is_some_and(|s| s.view().state.is_live())
    }

    /// Any run live, Random, MIMO, classical shock, SRS shock, Sine or TWR: what
    /// the session's orphan clock guards.
    fn any_live(&self) -> bool {
        self.random_live()
            || self.mimo_live()
            || self.shock_live()
            || self.srs_live()
            || self.mimo_shock_live()
            || self.sine_live()
            || self.mixed_live()
            || self.twr_live()
            || self.explorer_live()
            || self.recorder_live()
    }

    fn sim_running(&self) -> bool {
        self.sim.run.as_ref().is_some_and(|r| !r.is_finished())
    }

    fn ctx(&self) -> StationContext<'_> {
        StationContext {
            selection: self.stations.selected(),
            scan: self.stations.devices(),
        }
    }

    fn run_active(&self) -> Result<(), MethodError> {
        if let Some(r) = &self.random {
            let v = r.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!("Run {} is {}; stop it first.", r.id, v.state.name()),
                )
                .with_detail(json!({"run_id": r.id})));
            }
        }
        if let Some(m) = &self.mimo {
            let state = m.with(|d| d.state());
            if state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!("MIMO run {} is {}; stop it first.", m.id, state.name()),
                )
                .with_detail(json!({"run_id": m.id})));
            }
        }
        if let Some(s) = &self.shock {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "Classical shock run {} is {}; stop it first.",
                        s.id,
                        v.state.name()
                    ),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        if let Some(s) = &self.srs {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "SRS shock run {} is {}; stop it first.",
                        s.id,
                        v.state.name()
                    ),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        if let Some(s) = &self.mimo_shock {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "MIMO Shock run {} is {}; stop it first.",
                        s.id,
                        v.state.name()
                    ),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        if let Some(s) = &self.sine {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!("Sine run {} is {}; stop it first.", s.id, v.state.name()),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        if let Some(s) = &self.mixed {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "Mixed Mode run {} is {}; stop it first.",
                        s.id,
                        v.state.name()
                    ),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        if let Some(s) = &self.twr {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!("TWR run {} is {}; stop it first.", s.id, v.state.name()),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        if let Some(s) = &self.explorer {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "Explorer run {} is {}; stop it first.",
                        s.id,
                        v.state.name()
                    ),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        if let Some(s) = &self.recorder {
            let v = s.view();
            if v.state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "Recorder run {} is {}; stop it first.",
                        s.id,
                        v.state.name()
                    ),
                )
                .with_detail(json!({"run_id": s.id})));
            }
        }
        Ok(())
    }

    fn need_run(&self) -> Result<Arc<RandomRun>, MethodError> {
        self.random.clone().ok_or_else(|| {
            MethodError::named(
                ErrorCode::Refused,
                "not_configured",
                "Random is not configured; configure a test first.",
            )
        })
    }
}

/// The control service.
pub struct Service {
    opts: ServiceOptions,
    inner: Mutex<Inner>,
    events: broadcast::Sender<Arc<Out>>,
    seq: AtomicU64,
    conns: AtomicU64,
    notify: mpsc::UnboundedSender<rrun::Notice>,
    notices: std::sync::Mutex<Option<mpsc::UnboundedReceiver<rrun::Notice>>>,
    background: AtomicBool,
    /// The connections with a subscription of their own; every other connection is
    /// on the default subscription (`random.live`).
    displays: std::sync::Mutex<HashMap<ConnId, Display>>,
    /// The connections with a MIMO display subscription.
    mimo_displays: std::sync::Mutex<HashMap<ConnId, mimo_methods::MimoDisplay>>,
    /// The connections with a classical shock display subscription.
    shock_displays: std::sync::Mutex<HashMap<ConnId, shock_methods::ShockDisplay>>,
    /// Classical shock runs' publishes, in order.
    shock_notify: mpsc::UnboundedSender<crate::pulse::rails::Notice<crate::shock::view::ShockView>>,
    shock_notices: std::sync::Mutex<
        Option<mpsc::UnboundedReceiver<crate::pulse::rails::Notice<crate::shock::view::ShockView>>>,
    >,
    /// The connections with an SRS shock display subscription.
    srs_displays: std::sync::Mutex<HashMap<ConnId, srs_methods::SrsDisplay>>,
    /// SRS shock runs' publishes, in order.
    srs_notify: mpsc::UnboundedSender<crate::pulse::rails::Notice<crate::srs::view::SrsView>>,
    srs_notices: std::sync::Mutex<
        Option<mpsc::UnboundedReceiver<crate::pulse::rails::Notice<crate::srs::view::SrsView>>>,
    >,
    /// The connections with a MIMO Shock display subscription.
    mimo_shock_displays: std::sync::Mutex<HashMap<ConnId, mimo_shock_methods::MsDisplay>>,
    /// MIMO Shock runs' publishes, in order.
    mimo_shock_notify:
        mpsc::UnboundedSender<crate::pulse::rails::Notice<crate::mimo_shock::view::MsView>>,
    mimo_shock_notices: std::sync::Mutex<
        Option<
            mpsc::UnboundedReceiver<crate::pulse::rails::Notice<crate::mimo_shock::view::MsView>>,
        >,
    >,
    /// The connections with a Sine display subscription.
    sine_displays: std::sync::Mutex<HashMap<ConnId, sine_methods::SineDisplay>>,
    /// Sine runs' publishes, in order.
    sine_notify: mpsc::UnboundedSender<crate::sine::run::Notice>,
    sine_notices: std::sync::Mutex<Option<mpsc::UnboundedReceiver<crate::sine::run::Notice>>>,
    /// The connections with a Mixed Mode display subscription.
    mixed_displays: std::sync::Mutex<HashMap<ConnId, mixed_methods::MixedDisplay>>,
    /// Mixed Mode runs' publishes, in order.
    mixed_notify: mpsc::UnboundedSender<crate::mixed::Notice>,
    mixed_notices: std::sync::Mutex<Option<mpsc::UnboundedReceiver<crate::mixed::Notice>>>,
    /// The connections with a TWR display subscription.
    twr_displays: std::sync::Mutex<HashMap<ConnId, twr_methods::TwrDisplay>>,
    /// TWR runs' publishes, in order.
    twr_notify: mpsc::UnboundedSender<crate::twr::run::Notice>,
    twr_notices: std::sync::Mutex<Option<mpsc::UnboundedReceiver<crate::twr::run::Notice>>>,
    /// The connections with an Explorer display subscription.
    explorer_displays: std::sync::Mutex<HashMap<ConnId, explorer_methods::ExplorerDisplay>>,
    /// Explorer runs' publishes, in order.
    explorer_notify:
        mpsc::UnboundedSender<crate::pulse::rails::Notice<crate::explorer::view::ExplorerView>>,
    explorer_notices: std::sync::Mutex<
        Option<
            mpsc::UnboundedReceiver<
                crate::pulse::rails::Notice<crate::explorer::view::ExplorerView>,
            >,
        >,
    >,
    /// The connections with a Recorder display subscription.
    recorder_displays: std::sync::Mutex<HashMap<ConnId, recorder_methods::RecorderDisplay>>,
    /// Recorder runs' publishes, in order.
    recorder_notify:
        mpsc::UnboundedSender<crate::pulse::rails::Notice<crate::recorder::view::RecorderView>>,
    recorder_notices: std::sync::Mutex<
        Option<
            mpsc::UnboundedReceiver<
                crate::pulse::rails::Notice<crate::recorder::view::RecorderView>,
            >,
        >,
    >,
    /// The signal a Recorder run on the simulator carries (the stated source unless a
    /// test installs another).
    recorder_signal: recorder_methods::SignalSlot,
    /// The control-law catalog, opened on first use.
    catalog: Arc<std::sync::Mutex<Option<hertzforge_random_laws::LawCatalog>>>,
    /// Set when shutdown begins: [`DRIVE_METHODS`] are refused from then on.
    closing: AtomicBool,
    /// Held shared by each of [`DRIVE_METHODS`] while it runs, and taken whole by the
    /// shutdown, so a method that passed the check before shutdown began has finished
    /// before the shutdown looks for a live run.
    drive_gate: tokio::sync::RwLock<()>,
}

fn unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

impl Service {
    pub fn new() -> Arc<Service> {
        Service::with_options(ServiceOptions::default())
    }

    pub fn with_options(opts: ServiceOptions) -> Arc<Service> {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let (notify, notices) = mpsc::unbounded_channel();
        let (shock_notify, shock_notices) = mpsc::unbounded_channel();
        let (srs_notify, srs_notices) = mpsc::unbounded_channel();
        let (mimo_shock_notify, mimo_shock_notices) = mpsc::unbounded_channel();
        let (sine_notify, sine_notices) = mpsc::unbounded_channel();
        let (mixed_notify, mixed_notices) = mpsc::unbounded_channel();
        let (twr_notify, twr_notices) = mpsc::unbounded_channel();
        let (explorer_notify, explorer_notices) = mpsc::unbounded_channel();
        let (recorder_notify, recorder_notices) = mpsc::unbounded_channel();
        Arc::new(Service {
            inner: Mutex::new(Inner {
                session: Session::new(opts.heartbeat, opts.timeout),
                stations: Stations::new(opts.scanner.clone()),
                table: Table::new(),
                random: None,
                runs: 0,
                mimo: None,
                mimo_runs: 0,
                shock: None,
                shock_runs: 0,
                shock_state_key: String::new(),
                shock_log_sent: (String::new(), 0),
                shock_rows_sent: 0,
                shock_ident_sent: false,
                srs: None,
                srs_runs: 0,
                srs_state_key: String::new(),
                srs_log_sent: (String::new(), 0),
                srs_rows_sent: 0,
                srs_ident_sent: false,
                mimo_shock: None,
                mimo_shock_runs: 0,
                mimo_shock_state_key: String::new(),
                mimo_shock_log_sent: (String::new(), 0),
                mimo_shock_rows_sent: 0,
                mimo_shock_ident_sent: false,
                sine: None,
                sine_runs: 0,
                sine_state_key: String::new(),
                sine_log_sent: (String::new(), 0),
                mixed: None,
                mixed_runs: 0,
                mixed_state_key: String::new(),
                mixed_log_sent: (String::new(), 0),
                twr: None,
                twr_runs: 0,
                twr_state_key: String::new(),
                twr_log_sent: (String::new(), 0),
                twr_rows_sent: 0,
                twr_ident_sent: false,
                explorer: None,
                explorer_runs: 0,
                explorer_state_key: String::new(),
                explorer_log_sent: (String::new(), 0),
                recorder: None,
                recorder_runs: 0,
                recorder_state_key: String::new(),
                recorder_log_sent: (String::new(), 0),
                sim: SimSlot::default(),
                state_key: String::new(),
                log_sent: (String::new(), 0),
            }),
            opts,
            events,
            seq: AtomicU64::new(0),
            conns: AtomicU64::new(0),
            notify,
            notices: std::sync::Mutex::new(Some(notices)),
            background: AtomicBool::new(false),
            displays: std::sync::Mutex::new(HashMap::new()),
            mimo_displays: std::sync::Mutex::new(HashMap::new()),
            shock_displays: std::sync::Mutex::new(HashMap::new()),
            shock_notify,
            shock_notices: std::sync::Mutex::new(Some(shock_notices)),
            srs_displays: std::sync::Mutex::new(HashMap::new()),
            srs_notify,
            srs_notices: std::sync::Mutex::new(Some(srs_notices)),
            mimo_shock_displays: std::sync::Mutex::new(HashMap::new()),
            mimo_shock_notify,
            mimo_shock_notices: std::sync::Mutex::new(Some(mimo_shock_notices)),
            sine_displays: std::sync::Mutex::new(HashMap::new()),
            sine_notify,
            sine_notices: std::sync::Mutex::new(Some(sine_notices)),
            mixed_displays: std::sync::Mutex::new(HashMap::new()),
            mixed_notify,
            mixed_notices: std::sync::Mutex::new(Some(mixed_notices)),
            twr_displays: std::sync::Mutex::new(HashMap::new()),
            twr_notify,
            twr_notices: std::sync::Mutex::new(Some(twr_notices)),
            explorer_displays: std::sync::Mutex::new(HashMap::new()),
            explorer_notify,
            explorer_notices: std::sync::Mutex::new(Some(explorer_notices)),
            recorder_displays: std::sync::Mutex::new(HashMap::new()),
            recorder_notify,
            recorder_notices: std::sync::Mutex::new(Some(recorder_notices)),
            recorder_signal: std::sync::Mutex::new(None),
            catalog: Arc::new(std::sync::Mutex::new(None)),
            closing: AtomicBool::new(false),
            drive_gate: tokio::sync::RwLock::new(()),
        })
    }

    /// Subscribe to event frames.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Out>> {
        self.events.subscribe()
    }

    /// A new connection's identity.
    pub fn connect(&self, peer: String) -> Conn {
        Conn {
            id: self.conns.fetch_add(1, Ordering::Relaxed) + 1,
            peer,
        }
    }

    fn emit(&self, topic: &str, data: Value) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let msg = protocol::event(topic, seq, data);
        let _ = self.events.send(Arc::new(Out::Text(msg.to_string())));
    }

    /// A `random.live` frame, for the connections on the default subscription.
    fn emit_live(&self, data: Value, arrays: &Arrays) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        match codec::encode(&protocol::event("random.live", seq, data), arrays) {
            Ok(frame) => {
                let _ = self.events.send(Arc::new(Out::Live(frame)));
            }
            Err(e) => eprintln!("sds-control: random.live frame not encoded: {e}"),
        }
    }

    fn emit_binary(&self, topic: &str, data: Value, arrays: &Arrays) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        match codec::encode(&protocol::event(topic, seq, data), arrays) {
            // No subscribers is not an error: the run goes on unwatched.
            Ok(frame) => {
                let _ = self.events.send(Arc::new(Out::Binary(frame)));
            }
            Err(e) => eprintln!("sds-control: {topic} frame not encoded: {e}"),
        }
    }

    fn displays(&self) -> std::sync::MutexGuard<'_, HashMap<ConnId, Display>> {
        match self.displays.lock() {
            Ok(d) => d,
            Err(p) => p.into_inner(),
        }
    }

    /// Whether connection `conn` has a subscription of its own to `feed`.
    pub fn subscribed(&self, conn: ConnId, feed: Feed) -> bool {
        match feed {
            Feed::Random => self.displays().contains_key(&conn),
            Feed::Mimo => self.mimo_displays().contains_key(&conn),
            Feed::Shock => self.shock_displays().contains_key(&conn),
            Feed::Srs => self.srs_displays().contains_key(&conn),
            Feed::MimoShock => self.mimo_shock_displays().contains_key(&conn),
            Feed::Sine => self.sine_displays().contains_key(&conn),
            Feed::Mixed => self.mixed_displays().contains_key(&conn),
            Feed::Twr => self.twr_displays().contains_key(&conn),
            Feed::Explorer => self.explorer_displays().contains_key(&conn),
            Feed::Recorder => self.recorder_displays().contains_key(&conn),
        }
    }

    /// One `random.data` frame of `v` for connection `conn`.
    fn emit_data(&self, conn: ConnId, v: &RunView, sub: &Subscription) {
        let mut arrays = Arrays::new();
        let data = subscribe::data_json(v, sub, &mut arrays);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        match codec::encode(&protocol::event("random.data", seq, data), &arrays) {
            Ok(frame) => {
                let _ = self
                    .events
                    .send(Arc::new(Out::To(conn, Feed::Random, frame)));
            }
            Err(e) => eprintln!("sds-control: random.data frame not encoded: {e}"),
        }
    }

    /// `random.subscribe`: replace `conn`'s subscription, and send it the run's
    /// latest data at once when the run has streamed.
    async fn random_subscribe(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let sub = subscribe::parse(params)?.map(Arc::new);
        let result = subscribe::result_json(sub.as_deref());
        match &sub {
            None => {
                self.displays().remove(&conn.id);
            }
            Some(sub) => {
                self.displays().insert(
                    conn.id,
                    Display {
                        sub: sub.clone(),
                        last: None,
                    },
                );
                let run = self.inner.lock().await.random.clone();
                if let Some(run) = run {
                    let v = run.view();
                    if v.streaming {
                        let v = run.live_view(v);
                        self.emit_data(conn.id, &v, sub);
                        if let Some(d) = self.displays().get_mut(&conn.id) {
                            d.last = Some(Instant::now());
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    fn session_changed(&self, inner: &Inner) {
        let live = inner.any_live();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let _ = self.events.send(Arc::new(Out::Session {
            seq,
            holder: inner.session.holder_conn(),
            controller: inner.session.controller_view(),
            orphan_s: protocol::opt_num(inner.session.orphan_s(Instant::now(), live)),
        }));
    }

    fn station_changed(&self, inner: &Inner) {
        self.emit("station.changed", inner.stations.list());
        self.emit("channels.changed", inner.table.get(&inner.ctx()));
    }

    /// Start the background tasks (the run watcher and the orphan clock). Idempotent.
    pub fn start_background(self: &Arc<Self>) {
        if self.background.swap(true, Ordering::SeqCst) {
            return;
        }
        let notices = self.notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, view)) = notices.recv().await {
                    svc.run_changed(&run_id, &view).await;
                }
            });
        }
        let shock_notices = self.shock_notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = shock_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, view)) = notices.recv().await {
                    svc.shock_changed(&run_id, &view).await;
                }
            });
        }
        let srs_notices = self.srs_notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = srs_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, view)) = notices.recv().await {
                    svc.srs_changed(&run_id, &view).await;
                }
            });
        }
        let ms_notices = self
            .mimo_shock_notices
            .lock()
            .ok()
            .and_then(|mut n| n.take());
        if let Some(mut notices) = ms_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, view)) = notices.recv().await {
                    svc.mimo_shock_changed(&run_id, &view).await;
                }
            });
        }
        let sine_notices = self.sine_notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = sine_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, _view)) = notices.recv().await {
                    svc.sine_changed(&run_id).await;
                }
            });
        }
        let mixed_notices = self.mixed_notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = mixed_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, _view)) = notices.recv().await {
                    svc.mixed_changed(&run_id).await;
                }
            });
        }
        let twr_notices = self.twr_notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = twr_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, view)) = notices.recv().await {
                    svc.twr_changed(&run_id, &view).await;
                }
            });
        }
        let explorer_notices = self.explorer_notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = explorer_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, view)) = notices.recv().await {
                    svc.explorer_changed(&run_id, &view).await;
                }
            });
        }
        let recorder_notices = self.recorder_notices.lock().ok().and_then(|mut n| n.take());
        if let Some(mut notices) = recorder_notices {
            let svc = self.clone();
            tokio::spawn(async move {
                while let Some((run_id, view)) = notices.recv().await {
                    svc.recorder_changed(&run_id, &view).await;
                }
            });
        }
        let svc = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                svc.orphan_clock().await;
            }
        });
    }

    /// A run published `v`: send `random.state` if what it shows changed, and the
    /// run's new log entries.
    async fn run_changed(&self, run_id: &str, v: &RunView) {
        let mut inner = self.inner.lock().await;
        let Some(run) = inner.random.clone() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        let key = view::state_key(Some(v));
        if key != inner.state_key {
            inner.state_key = key;
            self.emit("random.state", view::state_json(Some(v)));
        }
        let since = if inner.log_sent.0 == run.id {
            inner.log_sent.1
        } else {
            0
        };
        let (entries, next) = run.log_since(since);
        inner.log_sent = (run.id.clone(), next);
        if !entries.is_empty() {
            self.emit(
                "random.log",
                json!({
                    "run_id": run.id,
                    "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
                }),
            );
        }
    }

    /// Run the orphan clock: give up an absent seat, and safe-stop an unattended run.
    async fn orphan_clock(&self) {
        let mut inner = self.inner.lock().await;
        let live = inner.any_live();
        let e = inner.session.expire(Instant::now(), live);
        if e.changed {
            self.session_changed(&inner);
        }
        if e.safe_stop {
            if let Some(m) = inner.mimo.clone() {
                if m.is_live() {
                    m.with(|d| {
                        d.stop(
                            crate::mimo::driver::StopOutcome::SafeStop,
                            "the service (nobody held control)",
                        )
                    });
                }
            }
            let shock = inner.shock.clone().filter(|s| s.view().state.is_live());
            let srs = inner.srs.clone().filter(|s| s.view().state.is_live());
            let mshock = inner
                .mimo_shock
                .clone()
                .filter(|s| s.view().state.is_live());
            let sine = inner.sine.clone().filter(|s| s.view().state.is_live());
            let mixed = inner.mixed.clone().filter(|s| s.view().state.is_live());
            let twr = inner.twr.clone().filter(|s| s.view().state.is_live());
            let explorer = inner.explorer.clone().filter(|s| s.view().state.is_live());
            let recorder = inner.recorder.clone().filter(|s| s.view().state.is_live());
            let random = inner.random.clone();
            drop(inner);
            if let Some(s) = recorder {
                recorder_methods::stop_recorder(
                    &s,
                    false,
                    "safe_stop",
                    "the service (nobody held control)".into(),
                );
            }
            if let Some(s) = explorer {
                explorer_methods::stop_explorer(
                    &s,
                    hertzforge_explorer_host::StopCause::SafeStop,
                    "the service (nobody held control)".into(),
                );
            }
            if let Some(s) = twr {
                twr_methods::stop_twr(
                    &s,
                    Some("safe_stop"),
                    "the service (nobody held control)".into(),
                    false,
                )
                .await;
            }
            if let Some(s) = mixed {
                mixed_methods::stop_mixed(
                    &s,
                    Some("safe_stop"),
                    "the service (nobody held control)".into(),
                    false,
                )
                .await;
            }
            if let Some(s) = sine {
                sine_methods::stop_sine(
                    &s,
                    Some("safe_stop"),
                    "the service (nobody held control)".into(),
                    false,
                )
                .await;
            }
            if let Some(s) = shock {
                shock_methods::stop_shock(
                    &s,
                    Some("safe_stop"),
                    "the service (nobody held control)".into(),
                    false,
                )
                .await;
            }
            if let Some(s) = srs {
                srs_methods::stop_srs(
                    &s,
                    Some("safe_stop"),
                    "the service (nobody held control)".into(),
                    false,
                )
                .await;
            }
            if let Some(s) = mshock {
                mimo_shock_methods::stop_mimo_shock(
                    &s,
                    Some("safe_stop"),
                    "the service (nobody held control)".into(),
                    false,
                )
                .await;
            }
            if let Some(run) = random {
                stop_run(
                    &run,
                    "safe_stop",
                    "the service (nobody held control)".into(),
                    false,
                )
                .await;
            }
        }
    }

    /// Shut down: the process has been told to stop (a signal, or its parent closing
    /// standard input; `main.rs`). From here on every method that could start or raise
    /// a drive is refused, and one already under way finishes first. A live Random run
    /// then ends with the controlled stop, the path of the safe stop (the level ramps
    /// down over the run's shutdown ramp while the loop drives; never an abort). Should
    /// the run still be live well after its ramp, it is aborted, as `random.abort`
    /// would. This returns only once no run is live, so the process can exit with
    /// nothing driving and every station released; connections are served throughout,
    /// so each screen sees the run end and keeps Stop and Abort. The result says what
    /// was stopped, for the log.
    pub async fn shutdown(&self) -> Option<String> {
        self.closing.store(true, Ordering::SeqCst);
        drop(self.drive_gate.write().await);
        let mixed = self.inner.lock().await.mixed.clone();
        let mixed_said = match mixed.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                mixed_methods::stop_mixed(&s, Some("safe_stop"), rrun::SHUTDOWN_BY.into(), true)
                    .await;
                let asked = Instant::now();
                let patience = mixed_methods::shutdown_patience(&s);
                while s.view().state.is_live() {
                    if asked.elapsed() > patience {
                        mixed_methods::stop_mixed(&s, None, rrun::SHUTDOWN_BY.into(), true).await;
                        break;
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        let (random, sim, shock, srs, sine, twr, explorer, recorder) = {
            let inner = self.inner.lock().await;
            (
                inner.random.clone(),
                inner.sim.run.clone(),
                inner.shock.clone(),
                inner.srs.clone(),
                inner.sine.clone(),
                inner.twr.clone(),
                inner.explorer.clone(),
                inner.recorder.clone(),
            )
        };
        // A Recorder run takes its controlled stop: acquisition ends at the next batch
        // and every recording is finalized. Should it still be live ten seconds later,
        // it is aborted.
        let recorder_said = match recorder.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                recorder_methods::stop_recorder(&s, false, "shutdown", rrun::SHUTDOWN_BY.into());
                let asked = Instant::now();
                let mut aborted = false;
                while s.view().state.is_live() {
                    if !aborted && asked.elapsed() > recorder_methods::SHUTDOWN_PATIENCE {
                        aborted = true;
                        recorder_methods::stop_recorder(
                            &s,
                            true,
                            "shutdown",
                            rrun::SHUTDOWN_BY.into(),
                        );
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        // An Explorer run takes its controlled stop, the ramp at its drive slew.
        // Should it still be live well after (twice its stop, at least ten seconds),
        // it is aborted.
        let explorer_said = match explorer.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                explorer_methods::stop_explorer(
                    &s,
                    hertzforge_explorer_host::StopCause::Shutdown,
                    rrun::SHUTDOWN_BY.into(),
                );
                let asked = Instant::now();
                let patience = explorer_methods::wait_for(&s);
                while s.view().state.is_live() {
                    if asked.elapsed() > patience {
                        let host = s.host.clone();
                        let _ = tokio::task::spawn_blocking(move || host.abort("shutdown")).await;
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        break;
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        // A TWR run takes its controlled stop: nothing more plays, and a play in
        // flight is cancelled at its next block and brought to rest. Should it still
        // be live well after, it is aborted.
        let twr_said = match twr.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                twr_methods::stop_twr(&s, Some("safe_stop"), rrun::SHUTDOWN_BY.into(), true).await;
                let asked = Instant::now();
                let patience = twr_methods::wait_for(&s);
                while s.view().state.is_live() {
                    if asked.elapsed() > patience {
                        twr_methods::stop_twr(&s, None, rrun::SHUTDOWN_BY.into(), true).await;
                        break;
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        // A Sine run takes its controlled stop, the 120 dB/s ramp. Should it still
        // be live well after the ramp (at least ten seconds, three batches and twice
        // its stop), it is aborted.
        let sine_said = match sine.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                sine_methods::stop_sine(&s, Some("safe_stop"), rrun::SHUTDOWN_BY.into(), true)
                    .await;
                let asked = Instant::now();
                let patience = sine_methods::shutdown_patience(&s);
                while s.view().state.is_live() {
                    if asked.elapsed() > patience {
                        let _ = writeln!(
                            std::io::stderr(),
                            "sds-control: run {} is still live {} s after its controlled stop; \
                             aborting it",
                            s.id,
                            patience.as_secs()
                        );
                        sine_methods::stop_sine(&s, None, rrun::SHUTDOWN_BY.into(), true).await;
                        break;
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        // A MIMO Shock run takes its controlled stop the same way.
        let mshock = self.inner.lock().await.mimo_shock.clone();
        let mshock_said = match mshock.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                mimo_shock_methods::stop_mimo_shock(
                    &s,
                    Some("safe_stop"),
                    rrun::SHUTDOWN_BY.into(),
                    true,
                )
                .await;
                let asked = Instant::now();
                while s.view().state.is_live() {
                    if asked.elapsed() > Duration::from_secs(30) {
                        mimo_shock_methods::stop_mimo_shock(
                            &s,
                            None,
                            rrun::SHUTDOWN_BY.into(),
                            true,
                        )
                        .await;
                        break;
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        // An SRS shock run takes its controlled stop the same way.
        let srs_said = match srs.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                srs_methods::stop_srs(&s, Some("safe_stop"), rrun::SHUTDOWN_BY.into(), true).await;
                let asked = Instant::now();
                while s.view().state.is_live() {
                    if asked.elapsed() > Duration::from_secs(30) {
                        srs_methods::stop_srs(&s, None, rrun::SHUTDOWN_BY.into(), true).await;
                        break;
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        // A classical shock run takes its controlled stop: nothing more fires, the
        // pulse in flight finishes. Should it still be live well after, it is aborted.
        let shock_said = match shock.filter(|s| s.view().state.is_live()) {
            None => None,
            Some(s) => {
                shock_methods::stop_shock(&s, Some("safe_stop"), rrun::SHUTDOWN_BY.into(), true)
                    .await;
                let asked = Instant::now();
                while s.view().state.is_live() {
                    if asked.elapsed() > Duration::from_secs(30) {
                        shock_methods::stop_shock(&s, None, rrun::SHUTDOWN_BY.into(), true).await;
                        break;
                    }
                    tokio::time::sleep(SHUTDOWN_POLL).await;
                }
                let v = s.view();
                Some(match &v.end {
                    Some(end) => format!("run {} ended ({}): {}", s.id, end.outcome, end.message),
                    None => format!("run {} ended", s.id),
                })
            }
        };
        // A `sim.*` demonstration run is simulated end to end; it is stopped so the
        // process ends tidily.
        if let Some(sim) = sim.filter(|r| !r.is_finished()) {
            sim.request_stop();
            let _ = tokio::task::spawn_blocking(move || sim.wait(STOP_WAIT)).await;
        }
        let Some(run) = random.filter(|r| r.view().state.is_live()) else {
            return shock_said
                .or(srs_said)
                .or(mshock_said)
                .or(sine_said)
                .or(mixed_said)
                .or(twr_said)
                .or(explorer_said)
                .or(recorder_said);
        };
        let ramp_s = rconfig::parse(&run.config)
            .map_or(rconfig::Control::default().shutdown_ramp_s, |c| {
                c.control.shutdown_ramp_s
            });
        let patience = wait_for(&run) + Duration::from_secs_f64(2.0 * ramp_s.max(0.0));
        stop_run(&run, "safe_stop", rrun::SHUTDOWN_BY.into(), true).await;
        let asked = Instant::now();
        let mut aborted = false;
        while run.view().state.is_live() {
            if !aborted && asked.elapsed() > patience {
                aborted = true;
                // Not `eprintln!`: once a parent has gone, its end of a pipe on
                // standard error may be closed, and that would panic here.
                let _ = writeln!(
                    std::io::stderr(),
                    "sds-control: run {} is still live {} s after its controlled stop; \
                     aborting it",
                    run.id,
                    patience.as_secs()
                );
                run.ended_by(rrun::SHUTDOWN_BY);
                run.signal(ABORT);
                let _ = run
                    .ask(
                        |reply| Cmd::Abort {
                            by: rrun::SHUTDOWN_BY.into(),
                            reply,
                        },
                        wait_for(&run),
                    )
                    .await;
            }
            tokio::time::sleep(SHUTDOWN_POLL).await;
        }
        let v = run.view();
        Some(match &v.end {
            Some(end) => format!("run {} ended ({}): {}", run.id, end.outcome, end.message),
            None => format!("run {} ended", run.id),
        })
    }

    /// The connection went: with a goodbye (a clean close) control is free at once;
    /// without one, the seat is kept for the timeout.
    pub async fn disconnected(&self, conn: &Conn, goodbye: bool) {
        self.displays().remove(&conn.id);
        self.mimo_displays().remove(&conn.id);
        self.shock_displays().remove(&conn.id);
        self.srs_displays().remove(&conn.id);
        self.mimo_shock_displays().remove(&conn.id);
        self.sine_displays().remove(&conn.id);
        self.mixed_displays().remove(&conn.id);
        self.twr_displays().remove(&conn.id);
        self.explorer_displays().remove(&conn.id);
        self.recorder_displays().remove(&conn.id);
        let mut inner = self.inner.lock().await;
        if inner.session.holder_conn() == Some(conn.id) {
            // Nobody holds control now: a TWR run starts no new play until control
            // is claimed again (or the orphan clock stops it).
            if let Some(s) = &inner.twr {
                if s.view().state.is_live() {
                    s.hold(true);
                    let name = inner.session.client_name(conn.id, &conn.peer);
                    s.shared.add_log_data(
                        "note",
                        format!("{name} left control; new plays are held"),
                        Some(json!({"action": "hold", "by": name})),
                    );
                }
            }
            if let Some(s) = &inner.srs {
                if s.view().state.is_live() {
                    s.hold(true);
                    let name = inner.session.client_name(conn.id, &conn.peer);
                    s.shared.add_log_data(
                        "note",
                        format!("{name} left control; firing is held"),
                        Some(json!({"action": "hold", "by": name})),
                    );
                }
            }
            if let Some(s) = &inner.mimo_shock {
                if s.view().state.is_live() {
                    s.hold(true);
                    let name = inner.session.client_name(conn.id, &conn.peer);
                    s.shared
                        .add_log("note", format!("{name} left control; firing is held"));
                }
            }
            // Nobody holds control now: a classical shock run fires nothing more
            // until control is claimed again (or the orphan clock stops it).
            if let Some(s) = &inner.shock {
                if s.view().state.is_live() {
                    s.hold(true);
                    let name = inner.session.client_name(conn.id, &conn.peer);
                    s.shared.add_log_data(
                        "note",
                        format!("{name} left control; firing is held"),
                        Some(json!({"action": "hold", "by": name})),
                    );
                }
            }
            if let Some(m) = &inner.mimo {
                let name = inner.session.client_name(conn.id, &conn.peer);
                let how = if goodbye {
                    "closed its connection"
                } else {
                    "went without a goodbye"
                };
                m.with(|d| d.controller_lost(&format!("{name} {how}")));
            }
        }
        let now = Instant::now();
        let live = inner.any_live();
        let changed = if goodbye {
            inner.session.release(conn.id, now, live)
        } else {
            inner.session.dropped(conn.id, now)
        };
        if changed {
            self.session_changed(&inner);
        }
    }

    /// The `random.state` object now.
    async fn state_now(&self) -> Value {
        let inner = self.inner.lock().await;
        let v = inner.random.as_ref().map(|r| r.view());
        view::state_json(v.as_deref())
    }

    /// Answer one request from `conn`: the result, and the arrays it references (a
    /// result with arrays travels as a binary frame).
    pub async fn call(
        self: &Arc<Self>,
        conn: &Conn,
        method: &str,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        let _gate = if DRIVE_METHODS.contains(&method) {
            let gate = self.drive_gate.read().await;
            if self.closing.load(Ordering::SeqCst) {
                return Err(shutting_down());
            }
            Some(gate)
        } else {
            None
        };
        match method {
            "random.configure" => self.random_configure(conn, params).await,
            "random.config" => Ok(self.random_config().await),
            "mimo_random.reference.build_breakpoints"
            | "mimo_random.reference.validate"
            | "mimo_random.reference.import" => self.mimo_reference(method, params).await,
            "shock.configure" => self.shock_configure(conn, params).await,
            "shock.config" => Ok(self.shock_config().await),
            "shock.preview" => self.shock_preview(params).await,
            "shock.pulse" => self.shock_pulse(params).await,
            "srs.configure" => self.srs_configure(conn, params).await,
            "srs.config" => Ok(self.srs_config().await),
            "srs.preview" => self.srs_preview(params).await,
            "srs.pulse" => self.srs_pulse(params).await,
            "mimo_shock.configure" => self.mimo_shock_configure(conn, params).await,
            "mimo_shock.config" => Ok(self.mimo_shock_config().await),
            "mimo_shock.pulse" => self.mimo_shock_pulse(params).await,
            "sine.configure" => self.sine_configure(conn, params).await,
            "sine.config" => Ok(self.sine_config().await),
            "sine.profile.validate" => self.sine_profile_validate(params).await,
            "mixed.configure" => self.mixed_configure(conn, params).await,
            "mixed.config" => Ok(self.mixed_config().await),
            "twr.configure" => self.twr_configure(conn, params).await,
            "twr.config" => Ok(self.twr_config().await),
            "twr.reference.import" => self.twr_reference_import(params).await,
            "twr.reference.preview" => self.twr_reference_preview(params).await,
            "twr.record" => self.twr_record(params).await,
            _ => self
                .call_plain(conn, method, params)
                .await
                .map(|v| (v, Arrays::new())),
        }
    }

    /// The methods whose results carry no arrays.
    async fn call_plain(
        self: &Arc<Self>,
        conn: &Conn,
        method: &str,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        match method {
            "sim.profiles" => Ok(json!({
                "profiles": profiles::all().iter().map(|p| p.describe()).collect::<Vec<_>>()
            })),
            "sim.start" => self.sim_start(params).await,
            "sim.stop" => self.sim_stop().await,
            "sim.state" => Ok(self.sim_state().await),

            "session.state" => {
                let inner = self.inner.lock().await;
                Ok(inner
                    .session
                    .state(conn.id, Instant::now(), inner.any_live()))
            }
            "session.claim" => self.session_claim(conn, params).await,
            "session.release" => {
                let mut inner = self.inner.lock().await;
                let now = Instant::now();
                let live = inner.any_live();
                let held = inner.session.holder_conn() == Some(conn.id);
                if inner.session.release(conn.id, now, live) {
                    self.session_changed(&inner);
                }
                if let Some(s) = inner.shock.as_ref().filter(|_| held) {
                    if s.view().state.is_live() {
                        s.hold(true);
                    }
                }
                if let Some(s) = inner.srs.as_ref().filter(|_| held) {
                    if s.view().state.is_live() {
                        s.hold(true);
                    }
                }
                if let Some(s) = inner.mimo_shock.as_ref().filter(|_| held) {
                    if s.view().state.is_live() {
                        s.hold(true);
                    }
                }
                if let Some(s) = inner.twr.as_ref().filter(|_| held) {
                    if s.view().state.is_live() {
                        s.hold(true);
                    }
                }
                Ok(json!({"role": "observer", "orphan_s": inner.session.orphan_s(now, live)}))
            }

            "station.list" => self.station_list(params).await,
            "station.select" => self.station_select(conn, params).await,
            "channels.get" => {
                let inner = self.inner.lock().await;
                Ok(inner.table.get(&inner.ctx()))
            }
            "channels.set" => self.channels_set(conn, params).await,

            "random.arm" => self.random_arm(conn, params, false).await,
            "random.start" => self.random_arm(conn, params, true).await,
            "random.proceed" => self.random_proceed(conn).await,
            "random.level" => self.random_level(conn, params).await,
            "random.mode" => self.random_mode(conn, params).await,
            "random.stop" => self.random_end(conn, false).await,
            "random.abort" => self.random_end(conn, true).await,
            "random.reset" => self.random_reset(conn).await,
            "random.clear" => self.random_clear(conn).await,
            "random.state" => Ok(self.state_now().await),
            "random.log" => self.random_log(params).await,
            "random.subscribe" => self.random_subscribe(conn, params).await,

            "mimo_random.setup" => self.mimo_setup(conn, params).await,
            "mimo_random.validate" => self.mimo_validate(params).await,
            "mimo_random.start" => self.mimo_start(conn).await,
            "mimo_random.proceed" => self.mimo_proceed(conn).await,
            "mimo_random.override" => self.mimo_override(conn, params).await,
            "mimo_random.exclude" => self.mimo_exclude(conn, params).await,
            "mimo_random.iterate_prediction" => self.mimo_iterate(conn, params).await,
            "mimo_random.stop" => self.mimo_end(conn, false).await,
            "mimo_random.abort" => self.mimo_end(conn, true).await,
            "mimo_random.status" => Ok(self.mimo_status_now().await),
            "mimo_random.report" => Ok(self.mimo_report().await),
            "mimo_random.subscribe" => self.mimo_subscribe(conn, params).await,
            "mimo_random.reference.build_rigid_body" => {
                crate::mimo::reference::build_rigid_body(params)
            }
            "mimo_random.identification.save" => {
                self.mimo_identification(conn, params, false).await
            }
            "mimo_random.identification.load" => self.mimo_identification(conn, params, true).await,
            "shock.arm" => self.shock_arm(conn, params, false).await,
            "shock.start" => self.shock_arm(conn, params, true).await,
            "shock.proceed" => self.shock_proceed(conn).await,
            "shock.fire" => self.shock_fire(conn).await,
            "shock.level" => self.shock_level(conn, params).await,
            "shock.train" => self.shock_train(conn, params).await,
            "shock.burst" => self.shock_burst(conn).await,
            "shock.stop" => self.shock_end(conn, false).await,
            "shock.abort" => self.shock_end(conn, true).await,
            "shock.reset" => self.shock_reset(conn).await,
            "shock.clear" => self.shock_clear(conn).await,
            "shock.state" => Ok(self.shock_state_now().await),
            "shock.log" => self.shock_log(params).await,
            "shock.subscribe" => self.shock_subscribe(conn, params).await,
            "shock.pulses" => self.shock_pulses(params).await,
            "srs.target.import" => self.srs_target_import(params).await,
            "srs.table.import" => self.srs_table_import(params).await,
            "srs.arm" => self.srs_arm(conn, params, false).await,
            "srs.start" => self.srs_arm(conn, params, true).await,
            "srs.proceed" => self.srs_proceed(conn).await,
            "srs.fire" => self.srs_fire(conn).await,
            "srs.level" => self.srs_level(conn, params).await,
            "srs.stop" => self.srs_end(conn, false).await,
            "srs.abort" => self.srs_end(conn, true).await,
            "srs.reset" => self.srs_reset(conn).await,
            "srs.clear" => self.srs_clear(conn).await,
            "srs.state" => Ok(self.srs_state_now().await),
            "srs.log" => self.srs_log(params).await,
            "srs.subscribe" => self.srs_subscribe(conn, params).await,
            "srs.pulses" => self.srs_pulses(params).await,
            "mimo_shock.arm" => self.mimo_shock_arm(conn, false).await,
            "mimo_shock.start" => self.mimo_shock_arm(conn, true).await,
            "mimo_shock.proceed" => self.mimo_shock_proceed(conn, params).await,
            "mimo_shock.fire" => self.mimo_shock_fire(conn).await,
            "mimo_shock.stop" => self.mimo_shock_end(conn, false).await,
            "mimo_shock.abort" => self.mimo_shock_end(conn, true).await,
            "mimo_shock.reset" => self.mimo_shock_reset(conn).await,
            "mimo_shock.clear" => self.mimo_shock_clear(conn).await,
            "mimo_shock.state" => Ok(self.mimo_shock_state_now().await),
            "mimo_shock.log" => self.mimo_shock_log(params).await,
            "mimo_shock.subscribe" => self.mimo_shock_subscribe(conn, params).await,
            "mimo_shock.pulses" => self.mimo_shock_pulses(params).await,
            "sine.profile.import" => self.sine_profile_import(params).await,
            "sine.arm" => self.sine_arm(conn, params, false).await,
            "sine.start" => self.sine_arm(conn, params, true).await,
            "sine.proceed" => self.sine_proceed(conn).await,
            "sine.level" => self.sine_level(conn, params).await,
            "sine.mode" => self.sine_mode(conn, params).await,
            "sine.hold" => self.sine_hold(conn, true).await,
            "sine.resume" => self.sine_hold(conn, false).await,
            "sine.reverse" => self.sine_reverse(conn).await,
            "sine.jog" => self.sine_jog(conn, params).await,
            "sine.goto" => self.sine_goto(conn, params).await,
            "sine.adjust" => self.sine_adjust(conn, params).await,
            "sine.stop" => self.sine_end(conn, false).await,
            "sine.abort" => self.sine_end(conn, true).await,
            "sine.reset" => self.sine_reset(conn).await,
            "sine.clear" => self.sine_clear(conn).await,
            "sine.state" => Ok(self.sine_state_now().await),
            "sine.log" => self.sine_log(params).await,
            "sine.subscribe" => self.sine_subscribe(conn, params).await,
            "mixed.arm" => self.mixed_arm(conn, params, false).await,
            "mixed.start" => self.mixed_arm(conn, params, true).await,
            "mixed.proceed" | "mixed.level" | "mixed.mode" | "mixed.hold" | "mixed.resume"
            | "mixed.part" | "mixed.adjust" => self.mixed_running_verb(conn, method, params).await,
            "mixed.stop" => Ok(self.mixed_end(conn, false).await),
            "mixed.abort" => Ok(self.mixed_end(conn, true).await),
            "mixed.reset" => self.mixed_reset(conn).await,
            "mixed.clear" => self.mixed_clear(conn).await,
            "mixed.state" => Ok(self.mixed_state_now().await),
            "mixed.log" => self.mixed_log(params).await,
            "mixed.subscribe" => self.mixed_subscribe(conn, params).await,
            "twr.arm" => self.twr_arm(conn, params, false).await,
            "twr.start" => self.twr_arm(conn, params, true).await,
            "twr.proceed" => self.twr_proceed(conn).await,
            "twr.iterate" => self.twr_iterate(conn).await,
            "twr.play" => self.twr_play(conn).await,
            "twr.level" => self.twr_level(conn, params).await,
            "twr.freeze" => self.twr_freeze(conn, params).await,
            "explorer.configure" => self.explorer_configure(conn, params).await,
            "explorer.config" => Ok(self.explorer_config().await),
            "explorer.start" => self.explorer_start(conn).await,
            "explorer.steer" => self.explorer_steer(conn, params).await,
            "explorer.sweep" => self.explorer_sweep(conn, params).await,
            "explorer.hold" => self.explorer_hold(conn).await,
            "explorer.stop" => self.explorer_end(conn, false).await,
            "explorer.abort" => self.explorer_end(conn, true).await,
            "explorer.survey" => self.explorer_survey(params).await,
            "explorer.survey.clear" => self.explorer_survey_clear(conn).await,
            "explorer.reset" => self.explorer_reset(conn).await,
            "explorer.clear" => self.explorer_clear(conn).await,
            "explorer.state" => Ok(self.explorer_state_now().await),
            "explorer.log" => self.explorer_log(params).await,
            "explorer.subscribe" => self.explorer_subscribe(conn, params).await,
            "recorder.configure" => self.recorder_configure(conn, params).await,
            "recorder.config" => Ok(self.recorder_config().await),
            "recorder.start" => self.recorder_start(conn, params).await,
            "recorder.arm" => self.recorder_arm(conn, params).await,
            "recorder.disarm" => self.recorder_disarm(conn, params).await,
            "recorder.record" => self.recorder_record(conn, params).await,
            "recorder.cut" => self.recorder_cut(conn, params).await,
            "recorder.stop" => self.recorder_end(conn, false).await,
            "recorder.abort" => self.recorder_end(conn, true).await,
            "recorder.recordings" => self.recorder_recordings(params).await,
            "recorder.marks" => self.recorder_marks(params).await,
            "recorder.reset" => self.recorder_reset(conn).await,
            "recorder.clear" => self.recorder_clear(conn).await,
            "recorder.state" => Ok(self.recorder_state_now().await),
            "recorder.log" => self.recorder_log(params).await,
            "recorder.subscribe" => self.recorder_subscribe(conn, params).await,
            "twr.stop" => self.twr_end(conn, false).await,
            "twr.abort" => self.twr_end(conn, true).await,
            "twr.reset" => self.twr_reset(conn).await,
            "twr.clear" => self.twr_clear(conn).await,
            "twr.state" => Ok(self.twr_state_now().await),
            "twr.log" => self.twr_log(params).await,
            "twr.subscribe" => self.twr_subscribe(conn, params).await,
            "twr.plays" => self.twr_plays(params).await,
            "laws.list" => self.laws_list(params).await,
            "laws.import" => self.laws_import(conn, params).await,
            "laws.check" if params.get("mixed").is_some_and(|m| !m.is_null()) => {
                self.mixed_laws_check(conn, params).await
            }
            "laws.check" if self.mimo_shock_law_id(conn, params).await.is_some() => {
                let id = self
                    .mimo_shock_law_id(conn, params)
                    .await
                    .unwrap_or_default();
                let mut p = params.clone();
                p.remove("path");
                p.insert("id".into(), Value::from(id));
                self.mimo_shock_laws_check(conn, &p).await
            }
            "laws.check" => match self.classical_law_id(conn, params).await {
                Some(id) => {
                    let mut p = params.clone();
                    p.remove("path");
                    p.insert("id".into(), Value::from(id));
                    self.shock_laws_check(conn, &p).await
                }
                None => match self.srs_law_id(conn, params).await {
                    Some(id) => {
                        let mut p = params.clone();
                        p.remove("path");
                        p.insert("id".into(), Value::from(id));
                        self.srs_laws_check(conn, &p).await
                    }
                    None => match self.sine_law_id(conn, params).await {
                        Some(id) => {
                            let mut p = params.clone();
                            p.remove("path");
                            p.insert("id".into(), Value::from(id));
                            self.sine_laws_check(conn, &p).await
                        }
                        None => match self.twr_law_id(conn, params).await {
                            Some(id) => {
                                let mut p = params.clone();
                                p.remove("path");
                                p.insert("id".into(), Value::from(id));
                                self.twr_laws_check(conn, &p).await
                            }
                            None => self.laws_check(conn, params).await,
                        },
                    },
                },
            },
            other => Err(MethodError::new(
                ErrorCode::UnknownMethod,
                format!("the control service has no method \"{other}\"; it serves {METHODS}"),
            )),
        }
    }

    // ---- session ------------------------------------------------------------------

    async fn session_claim(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let name = protocol::named_str(params, "name")?;
        if name.is_some_and(str::is_empty) {
            return Err(MethodError::bad_params(
                "name",
                "\"name\" must not be empty",
            ));
        }
        let lease = protocol::named_str(params, "lease")?;
        let mut inner = self.inner.lock().await;
        let c = inner
            .session
            .claim(conn.id, name, lease, &conn.peer, Instant::now())?;
        if c.resumed {
            if let Some(m) = &inner.mimo {
                let who = inner.session.client_name(conn.id, &conn.peer);
                m.with(|d| d.controller_back(&who));
            }
        }
        if let Some(s) = &inner.shock {
            if s.shared.signals.held() {
                s.hold(false);
                let who = inner.session.client_name(conn.id, &conn.peer);
                s.shared.add_log_data(
                    "note",
                    format!("{who} holds control; firing resumes"),
                    Some(json!({"action": "resume", "by": who})),
                );
            }
        }
        if let Some(s) = &inner.srs {
            if s.shared.signals.held() {
                s.hold(false);
                let who = inner.session.client_name(conn.id, &conn.peer);
                s.shared.add_log_data(
                    "note",
                    format!("{who} holds control; firing resumes"),
                    Some(json!({"action": "resume", "by": who})),
                );
            }
        }
        if let Some(s) = &inner.mimo_shock {
            if s.shared.signals.held() {
                s.hold(false);
                let who = inner.session.client_name(conn.id, &conn.peer);
                s.shared
                    .add_log("note", format!("{who} holds control; firing resumes"));
            }
        }
        if let Some(s) = &inner.twr {
            if s.shared.signals.held() {
                s.hold(false);
                let who = inner.session.client_name(conn.id, &conn.peer);
                s.shared.add_log_data(
                    "note",
                    format!("{who} holds control; plays resume"),
                    Some(json!({"action": "resume", "by": who})),
                );
            }
        }
        self.session_changed(&inner);
        Ok(json!({
            "role": "controller", "lease": c.lease, "resumed": c.resumed,
            "heartbeat_s": inner.session.heartbeat().as_secs_f64(),
            "timeout_s": inner.session.timeout().as_secs_f64(),
        }))
    }

    // ---- stations and the table ---------------------------------------------------

    async fn scan(&self) -> station::Scan {
        let scanner = self.opts.scanner.clone();
        tokio::task::spawn_blocking(move || station::take_scan(&*scanner))
            .await
            .unwrap_or_else(|e| station::Scan {
                at_unix: unix_s(),
                result: Err(station::ScanFailure {
                    reason: "scan_failed",
                    message: format!("The scan did not complete: {e}"),
                }),
            })
    }

    async fn station_list(&self, params: &Map<String, Value>) -> Result<Value, MethodError> {
        let rescan = protocol::param_bool(params, "rescan")?.unwrap_or(false);
        let never = self.inner.lock().await.stations.last_scan().is_none();
        if rescan || never {
            let scan = self.scan().await;
            let mut inner = self.inner.lock().await;
            inner.stations.record_scan(scan);
            self.station_changed(&inner);
            return Ok(inner.stations.list());
        }
        Ok(self.inner.lock().await.stations.list())
    }

    async fn station_select(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            inner.run_active()?;
        }
        match protocol::named_str(params, "kind")? {
            Some("sim") => {
                let mut inner = self.inner.lock().await;
                inner.session.require_controller(conn.id)?;
                inner.run_active()?;
                inner.stations.select_sim();
                self.station_changed(&inner);
                Ok(json!({"selected": inner.stations.selected_json()}))
            }
            Some("ni") => {
                let devices: Vec<String> = match params.get("devices") {
                    Some(Value::Array(a))
                        if !a.is_empty()
                            && a.iter().all(|d| d.as_str().is_some_and(|s| !s.is_empty())) =>
                    {
                        a.iter()
                            .filter_map(|d| d.as_str().map(str::to_owned))
                            .collect()
                    }
                    _ => {
                        return Err(MethodError::bad_params(
                            "devices",
                            "Selecting NI hardware needs at least one device.",
                        ))
                    }
                };
                let scan = self.scan().await;
                let busy = station::hardware_holder(&self.opts.data_root);
                let mut inner = self.inner.lock().await;
                inner.session.require_controller(conn.id)?;
                inner.run_active()?;
                let r = inner.stations.select_ni(devices, scan, busy);
                // The scan changed whether or not the selection was accepted.
                self.station_changed(&inner);
                r?;
                Ok(json!({"selected": inner.stations.selected_json()}))
            }
            other => Err(MethodError::bad_params(
                "kind",
                format!(
                    "\"kind\" is \"sim\" or \"ni\", not {}",
                    other.map_or("absent".to_owned(), |k| format!("\"{k}\""))
                ),
            )),
        }
    }

    async fn channels_set(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let revision = params
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                MethodError::bad_params(
                    "revision",
                    "A table edit names the revision it was made from.",
                )
            })?;
        let channels = params
            .get("channels")
            .ok_or_else(|| MethodError::bad_params("channels", "A table edit carries its rows."))?;
        let table = table::parse_rows(channels)?;
        let dry_run = protocol::param_bool(params, "dry_run")?.unwrap_or(false);
        let mut inner = self.inner.lock().await;
        let report = table::validate(&table, &inner.ctx());
        if dry_run {
            return Ok(json!({"revision": inner.table.revision, "report": report.to_json()}));
        }
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        if revision != inner.table.revision {
            let current = inner.table.revision;
            return Err(MethodError::named(
                ErrorCode::Refused,
                "stale_revision",
                format!(
                    "The table changed since revision {revision} (it is now {current}); read it \
                     again and review the change before sending."
                ),
            )
            .with_detail(json!({"current": current, "provided": revision})));
        }
        if !report.errors.is_empty() {
            return Err(table_invalid(&report, "so it was not written"));
        }
        inner.table.table = table;
        inner.table.revision += 1;
        self.emit("channels.changed", inner.table.get(&inner.ctx()));
        Ok(json!({"revision": inner.table.revision, "report": report.to_json()}))
    }

    // ---- Random -------------------------------------------------------------------

    async fn random_configure(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        let config = params
            .get("config")
            .filter(|c| c.is_object())
            .ok_or_else(|| {
                MethodError::bad_params("config", "Configuring needs a test configuration.")
            })?;
        let cfg = rconfig::parse(config)?;
        let dry_run = protocol::param_bool(params, "dry_run")?.unwrap_or(false);
        let mut inner = self.inner.lock().await;
        let table_report = table::validate(&inner.table.table, &inner.ctx());
        let (report, derived) = cfg.check(&inner.table.table);
        if dry_run {
            let mut all = table_report.clone();
            all.extend(report.clone());
            let derived = if all.errors.is_empty() { derived } else { None };
            let mut arrays = Arrays::new();
            let views = match &derived {
                Some(_) => vec![view::reference_view(
                    &view::lines_on(&cfg.loop_config(), &cfg),
                    &mut arrays,
                )],
                None => Vec::new(),
            };
            return Ok((
                json!({
                    "errors": all.errors.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                    "warnings": all.warnings.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                    "derived": derived.map_or(Value::Null, |d| d.to_json()),
                    "views": views,
                }),
                arrays,
            ));
        }
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        if !table_report.errors.is_empty() {
            return Err(table_invalid(&table_report, "on this station"));
        }
        let Some(derived) = derived else {
            return Err(rrun::config_invalid(&report));
        };
        let run = self.new_run(&mut inner, cfg, &derived)?;
        let v = run.view();
        Ok((
            json!({
                "state": view::state_json(Some(&v)),
                "derived": derived.to_json(),
                "warnings": report.warnings.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
            }),
            Arrays::new(),
        ))
    }

    /// `random.config`: what the present run was configured with.
    async fn random_config(&self) -> (Value, Arrays) {
        let inner = self.inner.lock().await;
        let mut arrays = Arrays::new();
        let result = match &inner.random {
            None => json!({"run_id": null, "config": null, "derived": null, "views": []}),
            Some(run) => json!({
                "run_id": run.id,
                "config": run.config,
                "derived": run.derived,
                "views": [view::reference_view(&run.reference, &mut arrays)],
            }),
        };
        (result, arrays)
    }

    /// Create a run (replacing the one before) and start its thread.
    fn new_run(
        &self,
        inner: &mut Inner,
        cfg: rconfig::WireConfig,
        derived: &rconfig::Derived,
    ) -> Result<Arc<RandomRun>, MethodError> {
        inner.runs += 1;
        let run_id = format!("r-{:04}", inner.runs);
        let class = inner.stations.class();
        let selected = inner.stations.selected().clone();
        let devices: Vec<(String, String)> = match &selected {
            Selection::Sim => Vec::new(),
            Selection::Ni { devices } => devices
                .iter()
                .map(|id| {
                    let sn = inner
                        .stations
                        .devices()
                        .iter()
                        .find(|d| &d.id == id)
                        .map(|d| d.serial.clone())
                        .unwrap_or_default();
                    (id.clone(), sn)
                })
                .collect(),
        };
        let station_ref = StationRef {
            kind: if selected == Selection::Sim {
                "sim"
            } else {
                "ni"
            },
            class: station::class_name(class),
            devices,
        };
        let initial = RunView {
            run_id: run_id.clone(),
            title: cfg.title.clone(),
            state: State::Configured,
            phase: "idle",
            mode: cfg.mode,
            levels_db: cfg.levels.iter().map(|l| l.0).collect(),
            full_level_s: cfg.full_level_s,
            station: station_ref.clone(),
            grid: derived.grid.clone(),
            clock: None,
            level_db: None,
            live_level_db: None,
            drive_now_db: None,
            drive_level_db: None,
            elapsed_s: None,
            at_level_s: None,
            remaining_at_level_s: None,
            alarm: false,
            feasibility: None,
            end: None,
            loops: 0,
            control_rms_g: None,
            reference_rms_g: None,
            drive_rms_v: None,
            lines_alarm: 0,
            lines_abort: 0,
            channels: rrun::live_channels(&inner.table.table),
            alarms: Vec::new(),
            lines: None,
            control_psd: None,
            drive_psd: None,
            inputs: None,
            coherence: None,
            limits: Arc::new(Vec::new()),
            level_s: None,
            streaming: false,
            record: None,
        };
        let shared = Arc::new(RunShared::new(initial.clone(), self.notify.clone()));
        let raw = cfg.raw.clone();
        let reference = Arc::new(view::lines_on(&cfg.loop_config(), &cfg));
        let title = cfg.title.clone();
        let plan = RunPlan {
            run_id: run_id.clone(),
            config: cfg,
            sim_speed: self.opts.sim_speed,
            capture_root: Some(self.opts.records_root()),
            timeout_s: self.opts.timeout.as_secs_f64(),
            record: self.opts.record.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        rrun::spawn(plan, initial.clone(), shared.clone(), rx)
            .map_err(|e| MethodError::internal(format!("could not start the run thread: {e}")))?;
        let run = Arc::new(RandomRun::new(
            run_id,
            shared.clone(),
            tx,
            raw,
            derived.to_json(),
            reference,
        ));
        inner.random = Some(run.clone());
        // The log starts at configure.
        shared.add_log(
            "state",
            format!("configured \"{title}\" on {}", initial.station.class),
        );
        let _ = self.notify.send((run.id.clone(), Arc::new(initial)));
        Ok(run)
    }

    /// What a run arms on now: the current table and the selected station.
    fn arm_context(&self, inner: &Inner, run: &RandomRun) -> Result<ArmContext, MethodError> {
        let report = table::validate(&inner.table.table, &inner.ctx());
        if !report.errors.is_empty() {
            return Err(table_invalid(
                &report,
                "on this station, so the run cannot arm",
            ));
        }
        let fs_hz = run.view().grid.fs_hz;
        let plan = match inner.stations.selected() {
            Selection::Sim => StationPlan::sim(fs_hz),
            Selection::Ni { devices } => {
                let (inputs, outputs) = hertzforge_station::physical_from_table(&inner.table.table);
                StationPlan::Ni {
                    devices: devices.clone(),
                    inputs,
                    outputs,
                    fs_hz,
                }
            }
        };
        Ok(ArmContext {
            table: inner.table.table.clone(),
            station: plan,
            scanner: self.opts.scanner.clone(),
            data_root: self.opts.data_root.clone(),
            bind: self.opts.bind.clone(),
            revision: inner.table.revision,
            devices: inner.stations.devices().to_vec(),
        })
    }

    async fn random_arm(
        self: &Arc<Self>,
        conn: &Conn,
        params: &Map<String, Value>,
        start: bool,
    ) -> Result<Value, MethodError> {
        let ack =
            if start {
                None
            } else {
                match params.get("override_ack") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
                    Some(_) => return Err(MethodError::bad_params(
                        "override_ack",
                        "An override acknowledgement is the operator's own words; it cannot be \
                         empty.",
                    )),
                }
            };
        let (run, ctx) = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.need_run()?;
            let v = run.view();
            if v.state == State::Finished {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_finished",
                    "This run has finished; reset or configure again to run.",
                )
                .with_detail(json!({"outcome": v.end.as_ref().map(|e| e.outcome)})));
            }
            if v.state.is_busy() || (!start && v.state == State::Armed) {
                inner.run_active()?;
            }
            if inner.mimo_live()
                || inner.shock_live()
                || inner.srs_live()
                || inner.sine_live()
                || inner.mixed_live()
                || inner.twr_live()
                || inner.explorer_live()
                || inner.recorder_live()
                || inner.mimo_shock_live()
            {
                inner.run_active()?;
            }
            if inner.sim_running() {
                let id = inner.sim.run.as_ref().map(|r| r.run_id().to_owned());
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "Simulation {} is running; stop it first.",
                        id.as_deref().unwrap_or("")
                    ),
                )
                .with_detail(json!({ "run_id": id })));
            }
            let ctx = self.arm_context(&inner, &run)?;
            (run, ctx)
        };
        if start {
            run.ask(|reply| Cmd::Start { ctx, reply }, ARM_WAIT).await?;
            self.spawn_live(run.clone());
        } else {
            run.ask(|reply| Cmd::Arm { ack, ctx, reply }, ARM_WAIT)
                .await?;
        }
        Ok(view::state_json(Some(&run.view())))
    }

    /// Publish `run`'s live data until it has finished, then once more with the
    /// final state: `random.live` five times a second for the connections on the
    /// default subscription, and `random.data` at each subscribed connection's own
    /// rate (schema/PROTOCOL.md, "Live Display" and "Live Data Subscriptions").
    fn spawn_live(self: &Arc<Self>, run: Arc<RandomRun>) {
        let svc = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(LIVE_TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_live: Option<Instant> = None;
            loop {
                tick.tick().await;
                let current = svc
                    .inner
                    .lock()
                    .await
                    .random
                    .as_ref()
                    .is_some_and(|r| r.id == run.id);
                let v = run.view();
                if !v.streaming {
                    if !current {
                        break;
                    }
                    continue;
                }
                let v = run.live_view(v);
                let last = v.state == State::Finished || !current;
                let now = Instant::now();
                if last || last_live.is_none_or(|t| now >= t + DISPLAY_PERIOD - DATA_SLACK) {
                    last_live = Some(now);
                    let mut arrays = Arrays::new();
                    let data = view::live_json(&v, &mut arrays);
                    svc.emit_live(data, &arrays);
                }
                let due: Vec<(ConnId, Arc<Subscription>)> = {
                    let mut displays = svc.displays();
                    displays
                        .iter_mut()
                        .filter(|(_, d)| {
                            last || d
                                .last
                                .is_none_or(|t| now + DATA_SLACK >= t + d.sub.period())
                        })
                        .map(|(id, d)| {
                            d.last = Some(now);
                            (*id, d.sub.clone())
                        })
                        .collect()
                };
                for (conn, sub) in due {
                    svc.emit_data(conn, &v, &sub);
                }
                if last {
                    break;
                }
            }
        });
    }

    async fn controlled_run(&self, conn: &Conn) -> Result<Arc<RandomRun>, MethodError> {
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.need_run()
    }

    async fn random_proceed(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_run(conn).await?;
        let v = run.view();
        if v.state != State::Review {
            return Err(wrong_state(&v, "Proceeding"));
        }
        run.ask(|reply| Cmd::Proceed { reply }, wait_for(&run))
            .await?;
        Ok(view::state_json(Some(&run.view())))
    }

    async fn random_level(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let delta = protocol::named_f64(params, "delta_db")?;
        let to = protocol::named_f64(params, "to_db")?;
        let target = match (delta, to) {
            (Some(d), None) => LevelTarget::Delta(d),
            (None, Some(t)) => LevelTarget::To(t),
            _ => {
                return Err(MethodError::bad_params(
                    "delta_db",
                    "A level change gives either a step in dB or a level of the schedule.",
                ))
            }
        };
        let run = self.controlled_run(conn).await?;
        let v = run.view();
        if v.mode != RunMode::Manual {
            return Err(MethodError::named(
                ErrorCode::Refused,
                "manual_mode_required",
                "Level changes need the manual ladder; switch the ladder to manual first.",
            )
            .with_detail(json!({"mode": view::mode_name(v.mode)})));
        }
        if !(v.state == State::Running && matches!(v.phase, "engaged" | "full_level")) {
            return Err(wrong_state(&v, "A level change"));
        }
        let level = run
            .ask(|reply| Cmd::Level { target, reply }, wait_for(&run))
            .await?;
        Ok(json!({"level_db": protocol::num(rconfig::round(level, 2))}))
    }

    async fn random_mode(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let mode = match protocol::named_str(params, "mode")? {
            Some("auto") => RunMode::Auto,
            Some("manual") => RunMode::Manual,
            _ => {
                return Err(MethodError::bad_params(
                    "mode",
                    "\"mode\" is \"auto\" or \"manual\".",
                ))
            }
        };
        let run = self.controlled_run(conn).await?;
        let v = run.view();
        if v.state == State::Finished {
            return Err(wrong_state(&v, "Changing the ladder mode"));
        }
        if v.mode != mode {
            run.ask(|reply| Cmd::Mode { mode, reply }, wait_for(&run))
                .await?;
        }
        Ok(json!({"mode": view::mode_name(mode)}))
    }

    /// `random.stop` and `random.abort`: open to every connection.
    async fn random_end(&self, conn: &Conn, abort: bool) -> Result<Value, MethodError> {
        let (run, by) = {
            let inner = self.inner.lock().await;
            let Some(run) = inner.random.clone() else {
                return Ok(view::state_json(None));
            };
            (run, inner.session.client_name(conn.id, &conn.peer))
        };
        if !run.view().state.is_live() {
            return Ok(view::state_json(Some(&run.view())));
        }
        run.ended_by(&by);
        if abort {
            run.signal(ABORT);
            let wait = wait_for(&run);
            let _ = run.ask(|reply| Cmd::Abort { by, reply }, wait).await;
        } else {
            stop_run(&run, "operator_stop", by, true).await;
        }
        Ok(view::state_json(Some(&run.view())))
    }

    async fn random_reset(&self, conn: &Conn) -> Result<Value, MethodError> {
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        let run = inner.need_run()?;
        if run.view().state.is_busy() {
            inner.run_active()?;
        }
        let cfg = rconfig::parse(&run.config)?;
        let table_report = table::validate(&inner.table.table, &inner.ctx());
        if !table_report.errors.is_empty() {
            return Err(table_invalid(&table_report, "on this station"));
        }
        let (report, derived) = cfg.check(&inner.table.table);
        let Some(derived) = derived else {
            return Err(rrun::config_invalid(&report));
        };
        let run = self.new_run(&mut inner, cfg, &derived)?;
        let v = run.view();
        Ok(view::state_json(Some(&v)))
    }

    async fn random_clear(&self, conn: &Conn) -> Result<Value, MethodError> {
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        if let Some(run) = &inner.random {
            if run.view().state.is_busy() {
                inner.run_active()?;
            }
        }
        inner.random = None;
        let state = view::state_json(None);
        inner.state_key = view::state_key(None);
        self.emit("random.state", state.clone());
        Ok(state)
    }

    async fn random_log(&self, params: &Map<String, Value>) -> Result<Value, MethodError> {
        let since = match params.get("since") {
            None | Some(Value::Null) => 0,
            Some(v) => v.as_u64().ok_or_else(|| {
                MethodError::bad_params("since", "\"since\" is a whole number from 0")
            })?,
        };
        let inner = self.inner.lock().await;
        let (entries, next) = match &inner.random {
            Some(r) => r.log_since(since),
            None => (Vec::new(), 0),
        };
        Ok(json!({
            "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "next": next,
        }))
    }

    // ---- sim.* (superseded for running tests; the Simulate workspace's demos) ------

    async fn sim_start(
        self: &Arc<Self>,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let profile_id = protocol::param_str(params, "profile")?.ok_or_else(|| {
            MethodError::bad_request(format!(
                "sim.start needs \"profile\"; sim.profiles lists them ({})",
                profiles::ids().join(", ")
            ))
        })?;
        let profile = profiles::find(profile_id).ok_or_else(|| {
            MethodError::not_found(format!(
                "no profile named \"{profile_id}\"; sim.profiles lists the ones that exist ({})",
                profiles::ids().join(", ")
            ))
        })?;
        let request = RunRequest {
            profile,
            level_db: protocol::param_f64(params, "level_db")?.unwrap_or(0.0),
            speed: protocol::param_f64(params, "speed")?.unwrap_or(1.0),
            duration_s: protocol::param_f64(params, "duration_s")?
                .unwrap_or(sim::DEFAULT_DURATION_S),
        };
        request.validate().map_err(MethodError::bad_request)?;

        let mut inner = self.inner.lock().await;
        if inner.mimo_live()
            || inner.shock_live()
            || inner.srs_live()
            || inner.sine_live()
            || inner.mixed_live()
            || inner.twr_live()
            || inner.explorer_live()
            || inner.recorder_live()
            || inner.mimo_shock_live()
        {
            inner.run_active()?;
        }
        if let Some(r) = &inner.random {
            if r.view().state.is_live() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!(
                        "Run {} holds the station; stop it before starting a simulation.",
                        r.id
                    ),
                )
                .with_detail(json!({"run_id": r.id})));
            }
        }
        if let Some(run) = &inner.sim.run {
            if !run.is_finished() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    format!("run {} is still running; stop it first", run.run_id()),
                )
                .with_detail(json!({"run_id": run.run_id()})));
            }
        }
        inner.sim.started += 1;
        let run_id = format!("sim-{}-{}", unix_ms(), inner.sim.started);
        let echo = json!({
            "profile": request.profile.id,
            "level_db": request.level_db,
            "speed": request.speed,
            "duration_s": request.duration_s,
        });
        let run = tokio::task::spawn_blocking(move || SimRun::start(run_id, request))
            .await
            .map_err(|e| MethodError::internal(format!("the run start task failed: {e}")))?
            .map_err(MethodError::refused)?;
        let run = Arc::new(run);
        inner.sim.run = Some(run.clone());
        drop(inner);
        self.spawn_display(run.clone());

        let mut result = json!({"run_id": run.run_id(), "source": "SIM"});
        if let (Value::Object(r), Value::Object(e)) = (&mut result, echo) {
            r.extend(e);
        }
        Ok(result)
    }

    async fn sim_stop(&self) -> Result<Value, MethodError> {
        let run = self.inner.lock().await.sim.run.clone();
        let Some(run) = run else {
            return Ok(json!({"state": "idle", "run_id": Value::Null}));
        };
        run.request_stop();
        let waiter = run.clone();
        let finished = tokio::task::spawn_blocking(move || waiter.wait(STOP_WAIT))
            .await
            .unwrap_or(false);
        if !finished {
            return Err(MethodError::internal(format!(
                "run {} did not stop within {} s; the stop request stands",
                run.run_id(),
                STOP_WAIT.as_secs()
            )));
        }
        Ok(json!({"state": run.protocol_state(), "run_id": run.run_id()}))
    }

    async fn sim_state(&self) -> Value {
        let run = self.inner.lock().await.sim.run.clone();
        match run {
            None => json!({"state": "idle", "run_id": Value::Null}),
            Some(run) => {
                let snap = run.snapshot();
                let finish = snap.finish.as_ref();
                json!({
                    "state": run.protocol_state(),
                    "run_id": run.run_id(),
                    "source": "SIM",
                    "profile": run.request().profile.id,
                    "phase": sim::phase_name(snap.phase),
                    "elapsed_s": crate::views::round_to(run.elapsed_s(), 3),
                    "level_db": crate::views::round_to(snap.level_db, 2),
                    "outcome": finish.map(|f| f.outcome.as_str()),
                    "reason": finish.map(|f| f.reason.clone()),
                })
            }
        }
    }

    /// Publish `random.display` for a `sim.*` run every [`DISPLAY_PERIOD`] until it
    /// has finished, then once more with the final state and the reason it ended.
    fn spawn_display(self: &Arc<Self>, run: Arc<SimRun>) {
        let svc = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(DISPLAY_PERIOD);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let finished = run.is_finished();
                let mut arrays = Arrays::new();
                let data = run.display_data(&mut arrays);
                svc.emit_binary(DISPLAY_TOPIC, data, &arrays);
                if finished {
                    break;
                }
            }
        });
    }
}

/// How long to wait for a run to answer a request: three of its control loops and
/// a margin.
fn wait_for(run: &RandomRun) -> Duration {
    let loop_s = run.view().grid.loop_s.max(0.05);
    Duration::from_secs_f64(loop_s * 3.0 + 10.0)
}

/// The controlled stop (`random.stop` and the safe stop). During system
/// identification and the gate the station stops at the next block.
async fn stop_run(run: &RandomRun, outcome: &'static str, by: String, wait: bool) {
    let v = run.view();
    if !v.state.is_live() {
        return;
    }
    run.note_request_level();
    if v.state == State::Running && matches!(v.phase, "sysid" | "coherence_gate") {
        run.signal(STOP);
    }
    // While the loop drives, the drive starts down at the next output block, not
    // at the end of the control loop in progress (2026-11-01, bench finding 2).
    if v.state == State::Running && matches!(v.phase, "engaged" | "full_level") {
        run.begin_stop(outcome, &by);
    }
    if wait {
        let w = wait_for(run);
        let _ = run
            .ask(
                |reply| Cmd::Stop {
                    outcome,
                    by,
                    reply: Some(reply),
                },
                w,
            )
            .await;
    } else {
        let _ = run.send(Cmd::Stop {
            outcome,
            by,
            reply: None,
        });
    }
}

/// The refusal of a method that could start or raise a drive once shutdown has begun.
fn shutting_down() -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "shutting_down",
        "The control service is shutting down: it is stopping any run and starts none.",
    )
}

fn wrong_state(v: &RunView, verb: &str) -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "wrong_state",
        format!(
            "{verb} does not apply while the run is {} ({}).",
            v.state.name(),
            v.phase.replace('_', " ")
        ),
    )
    .with_detail(json!({"state": v.state.name(), "phase": v.phase}))
}

fn table_invalid(report: &table::Report, what: &str) -> MethodError {
    let n = report.errors.len();
    MethodError::named(
        ErrorCode::BadRequest,
        "table_invalid",
        format!(
            "The channel table has {n} error{}, {what}.",
            if n == 1 { "" } else { "s" }
        ),
    )
    .with_detail(report.to_json())
}

/// Accept connections until the listener fails.
pub async fn serve(listener: TcpListener, service: Arc<Service>) -> std::io::Result<()> {
    service.start_background();
    loop {
        let (stream, peer) = listener.accept().await?;
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(stream, peer.to_string(), service).await {
                eprintln!("sds-control: connection ended: {e}");
            }
        });
    }
}

async fn connection(
    stream: TcpStream,
    peer: String,
    service: Arc<Service>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut source) = ws.split();
    sink.send(Message::Text(protocol::hello().to_string()))
        .await?;
    let conn = service.connect(peer);

    // The writer owns the sink: responses and pings arrive on `out`, events on
    // `events`.
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(64);
    let mut events = service.subscribe();
    let me = conn.id;
    let mine = service.clone();
    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                m = out_rx.recv() => match m {
                    Some(m) => m,
                    None => break,
                },
                e = events.recv() => match e {
                    Ok(frame) => match frame.to_message(me, |feed| mine.subscribed(me, feed)) {
                        Some(m) => m,
                        None => continue,
                    },
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // The heartbeat: a ping every `heartbeat`; three with nothing inbound and the
    // connection is treated as gone.
    let heartbeat = service.opts.heartbeat;
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_inbound = Instant::now();
    let mut goodbye = false;
    loop {
        let msg = tokio::select! {
            m = source.next() => m,
            _ = ping.tick() => {
                if last_inbound.elapsed() > heartbeat * session::MISSED_HEARTBEATS {
                    break;
                }
                if out_tx.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
                continue;
            }
        };
        let Some(Ok(msg)) = msg else { break };
        last_inbound = Instant::now();
        let message = match msg {
            Message::Text(text) => codec::decode_text(&text),
            Message::Binary(bytes) => codec::decode(&bytes).map(|d| d.message),
            Message::Close(_) => {
                goodbye = true;
                break;
            }
            _ => continue,
        };
        let service = service.clone();
        let out = out_tx.clone();
        let conn = conn.clone();
        tokio::spawn(async move {
            let (reply, arrays) = match message {
                Err(e) => (
                    protocol::error_response(
                        protocol::UNREADABLE_ID,
                        &MethodError::bad_request(format!("the frame could not be decoded: {e}")),
                    ),
                    Arrays::new(),
                ),
                Ok(message) => match protocol::parse_request(&message) {
                    Err((id, e)) => (protocol::error_response(id, &e), Arrays::new()),
                    Ok(Request { id, method, params }) => {
                        match service.call(&conn, &method, &params).await {
                            Ok((result, arrays)) => (protocol::response(id, result), arrays),
                            Err(e) => (protocol::error_response(id, &e), Arrays::new()),
                        }
                    }
                },
            };
            let frame = match codec::encode_frame(&reply, &arrays) {
                Ok(Frame::Text(t)) => Message::Text(t),
                Ok(Frame::Binary(b)) => Message::Binary(b),
                Err(e) => Message::Text(
                    protocol::error_response(
                        protocol::UNREADABLE_ID,
                        &MethodError::internal(format!("the reply could not be encoded: {e}")),
                    )
                    .to_string(),
                ),
            };
            let _ = out.send(frame).await;
        });
    }
    service.disconnected(&conn, goodbye).await;
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Who receives which live frame: `random.live` only a connection on the
    /// default subscription, `random.data` only its own connection while that
    /// connection is subscribed (so nothing built for a subscription it has just
    /// given up reaches it after the answer).
    #[test]
    fn live_frames_reach_only_the_connections_they_are_for() {
        let live = Out::Live(vec![1]);
        assert!(live.to_message(7, |_| false).is_some());
        assert!(live.to_message(7, |_| true).is_none());
        let data = Out::To(7, Feed::Random, vec![2]);
        assert!(data.to_message(7, |_| true).is_some());
        assert!(
            data.to_message(8, |_| true).is_none(),
            "another connection's data"
        );
        assert!(
            data.to_message(7, |_| false).is_none(),
            "data for a connection back on the default"
        );
        let text = Out::Text("x".into());
        assert!(text.to_message(7, |_| true).is_some());
        assert!(text.to_message(7, |_| false).is_some());
        // A MIMO frame reaches its connection only while it holds a MIMO subscription,
        // whatever its Random subscription.
        let mimo = Out::To(7, Feed::Mimo, vec![3]);
        assert!(mimo.to_message(7, |f| f == Feed::Mimo).is_some());
        assert!(mimo.to_message(7, |f| f == Feed::Random).is_none());
        assert!(
            live.to_message(7, |f| f == Feed::Mimo).is_some(),
            "a MIMO subscription leaves random.live alone"
        );
    }
}
