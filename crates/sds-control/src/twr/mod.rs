//! Time waveform replication through the protocol (schema/PROTOCOL.md, "Time
//! Waveform Replication"): the references and their preview, the configuration,
//! the run on the transient-run rails and its views, the display subscriptions,
//! and the law check.

pub mod config;
pub mod engine;
pub mod law_check;
pub mod record;
pub mod reference;
pub mod run;
pub mod sim;
pub mod subscribe;
pub mod view;

use crate::pulse::rails::{Jobs, Shared};
use crate::pulse::ring::PulseRing;
use crate::random::run::LogEntry;
use hertzforge_twr::player::SampleRing;
use run::Cmd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use view::{
    IdentFigures, InFlight, LawValueSpec, MaskFigures, PlayRecords, PlayRow, Scope, State, TwrView,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// How many plays' records a run holds.
pub const RECORDS_HELD: usize = 8;

/// The control channel's samples a Scope keeps.
pub const SCOPE_KEEP: usize = 4096;

/// The play in flight, as the run thread set it before the play.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Starting {
    pub index: u64,
    pub playback: bool,
    pub iteration: Option<u32>,
}

/// What the service reads of a run without its thread: every row, the latest
/// plays' records, the identification, the trusted bins, the play in flight and
/// its scope.
pub struct RunData {
    rows: Mutex<Vec<PlayRow>>,
    records: Mutex<PulseRing<Arc<PlayRecords>>>,
    identification: Mutex<Option<Arc<IdentFigures>>>,
    mask: Mutex<Option<Arc<MaskFigures>>>,
    target: Mutex<Option<Arc<PlayRecords>>>,
    starting: Mutex<Option<Starting>>,
    clocked_before: AtomicU64,
    fs_hz: Mutex<f64>,
    tap: Mutex<Option<Arc<Mutex<SampleRing>>>>,
    scope: Mutex<Vec<f64>>,
    /// The values the run's law reports, as its manifest declares them.
    pub law_specs: Vec<LawValueSpec>,
}

impl RunData {
    pub fn new(law_specs: Vec<LawValueSpec>) -> RunData {
        RunData {
            rows: Mutex::new(Vec::new()),
            records: Mutex::new(PulseRing::new(RECORDS_HELD)),
            identification: Mutex::new(None),
            mask: Mutex::new(None),
            target: Mutex::new(None),
            starting: Mutex::new(None),
            clocked_before: AtomicU64::new(0),
            fs_hz: Mutex::new(1.0),
            tap: Mutex::new(None),
            scope: Mutex::new(Vec::new()),
            law_specs,
        }
    }

    pub fn add_play(&self, row: PlayRow, records: Option<PlayRecords>) {
        if let Some(r) = records {
            lock(&self.records).push(row.index, Arc::new(r));
        }
        lock(&self.rows).push(row);
    }

    /// The rows from index `since`, and the index after the last.
    pub fn rows_since(&self, since: u64) -> (Vec<PlayRow>, u64) {
        let rows = lock(&self.rows);
        let next = rows.len() as u64;
        let from = (since as usize).min(rows.len());
        (rows[from..].to_vec(), next)
    }

    pub fn row(&self, index: u64) -> Option<PlayRow> {
        lock(&self.rows).get(index as usize).cloned()
    }

    pub fn row_count(&self) -> u64 {
        lock(&self.rows).len() as u64
    }

    pub fn records(&self, index: u64) -> Option<Arc<PlayRecords>> {
        lock(&self.records).get(index).cloned()
    }

    pub fn last_records(&self) -> Option<Arc<PlayRecords>> {
        lock(&self.records).last().cloned()
    }

    pub fn set_identification(&self, f: Option<IdentFigures>) {
        *lock(&self.identification) = f.map(Arc::new);
    }

    pub fn identification(&self) -> Option<Arc<IdentFigures>> {
        lock(&self.identification).clone()
    }

    pub fn set_mask(&self, f: Option<MaskFigures>) {
        *lock(&self.mask) = f.map(Arc::new);
    }

    pub fn mask(&self) -> Option<Arc<MaskFigures>> {
        lock(&self.mask).clone()
    }

    pub fn set_target(&self, f: Option<PlayRecords>) {
        *lock(&self.target) = f.map(Arc::new);
    }

    pub fn target(&self) -> Option<Arc<PlayRecords>> {
        lock(&self.target).clone()
    }

    pub fn set_station(&self, fs_hz: f64, tap: Option<Arc<Mutex<SampleRing>>>) {
        *lock(&self.fs_hz) = fs_hz;
        *lock(&self.tap) = tap;
    }

    pub fn fs_hz(&self) -> f64 {
        *lock(&self.fs_hz)
    }

    /// A play is about to start at station frame `clocked`.
    pub fn start_play(&self, s: Starting, clocked: u64) {
        self.clocked_before.store(clocked, Ordering::SeqCst);
        if let Some(t) = lock(&self.tap).as_ref() {
            if let Ok(mut ring) = t.lock() {
                let _ = ring.drain();
            }
        }
        lock(&self.scope).clear();
        *lock(&self.starting) = Some(s);
    }

    /// The play has ended; `clocked` frames over every play now.
    pub fn end_play(&self, clocked: u64) {
        *lock(&self.starting) = None;
        self.clocked_before.store(clocked, Ordering::SeqCst);
    }

    pub fn starting(&self) -> Option<Starting> {
        *lock(&self.starting)
    }

    pub fn clocked_before(&self) -> u64 {
        self.clocked_before.load(Ordering::SeqCst)
    }

    /// The control channel's most recent samples of the play in flight, `done`
    /// frames into it; empty between plays.
    pub fn scope_now(&self, done: Option<u64>) -> Scope {
        let fs = self.fs_hz();
        let Some(done) = done else {
            return Scope {
                t0_s: 0.0,
                fs_hz: fs,
                samples: Vec::new(),
            };
        };
        let mut buf = lock(&self.scope);
        if let Some(t) = lock(&self.tap).as_ref() {
            if let Ok(mut ring) = t.lock() {
                buf.extend(ring.drain().samples);
            }
        }
        if buf.len() > SCOPE_KEEP {
            let cut = buf.len() - SCOPE_KEEP;
            buf.drain(..cut);
        }
        let first = (self.clocked_before() + done).saturating_sub(buf.len() as u64);
        Scope {
            t0_s: first as f64 / fs,
            fs_hz: fs,
            samples: buf.clone(),
        }
    }
}

/// The service's handle on one configured run. Dropping it ends the run thread,
/// which releases anything the run holds; the service drops a run only when it is
/// not live.
pub struct TwrRun {
    pub id: String,
    pub shared: Arc<Shared<TwrView>>,
    pub data: Arc<RunData>,
    /// The configuration as accepted, for `twr.reset` and `twr.config`.
    pub config: serde_json::Value,
    /// Its derived values, as configure returned them.
    pub derived: serde_json::Value,
    /// The prepared record, for the configuration's views.
    pub prepared: Arc<reference::Prepared>,
    pub wire: Arc<config::WireTwr>,
    pub stored: Arc<reference::Stored>,
    pub jobs: Jobs<Cmd>,
}

impl TwrRun {
    /// The view as published, with the station time and the play in flight as of
    /// now: a stop asked for shows as `stopping` until the run has ended.
    pub fn view(&self) -> Arc<TwrView> {
        self.as_of_now(self.shared.view())
    }

    /// `v`, a view the run published, with the station time and the play in flight
    /// as of now, and a stop asked for shown as `stopping`.
    pub fn as_of_now(&self, v: Arc<TwrView>) -> Arc<TwrView> {
        let stopping = self.shared.signals.cancelled()
            && matches!(v.state, State::Running | State::Review | State::Held);
        let running = matches!(v.state, State::Running | State::Stopping);
        if !stopping && !running {
            return v;
        }
        let mut v = (*v).clone();
        if running {
            if let (Some(s), Some((done, of))) = (self.data.starting(), self.shared.progress.now())
            {
                v.play = Some(InFlight {
                    index: s.index,
                    playback: s.playback,
                    iteration: s.iteration,
                    frames_done: done,
                    frames: of,
                });
                let fs = self.data.fs_hz();
                if fs > 0.0 {
                    v.t_s = Some((self.data.clocked_before() + done) as f64 / fs);
                }
            }
        }
        if stopping {
            v.state = State::Stopping;
            v.phase = "stopping";
        }
        Arc::new(v)
    }

    /// The Scope of the play in flight.
    pub fn scope(&self) -> Scope {
        let done = self
            .data
            .starting()
            .and_then(|_| self.shared.progress.now())
            .map(|(d, _)| d);
        self.data.scope_now(done)
    }

    pub fn log_since(&self, since: u64) -> (Vec<LogEntry>, u64) {
        self.shared.log_since(since)
    }

    /// Raise the run's stop (or abort) before the request is queued, so a play in
    /// flight ends at its next block and nothing more plays.
    pub fn signal(&self, stop_outcome: Option<&'static str>, by: &str) {
        self.shared.set_ended_by(by);
        if let Some(o) = stop_outcome {
            self.shared.set_stop_outcome(o);
        }
        self.shared.signals.raise(stop_outcome.is_none());
    }

    /// Pause new plays while nobody holds control, or release them.
    pub fn hold(&self, hold: bool) {
        self.shared.signals.set_hold(hold);
    }
}
