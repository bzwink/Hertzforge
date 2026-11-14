//! SRS shock through the protocol (schema/PROTOCOL.md, "SRS Shock"): the
//! configuration, the offline synthesis and its preview, the two imports, the run
//! on the transient-run rails and its views, the display subscriptions, and the
//! law check.

pub mod config;
pub mod engine;
pub mod import;
pub mod law_check;
pub mod preview;
pub mod record;
pub mod run;
pub mod sim;
pub mod subscribe;
pub mod view;

use crate::pulse::rails::{Jobs, Shared};
use crate::pulse::ring::PulseRing;
use crate::random::run::LogEntry;
use crate::random::view::State;
use run::Cmd;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use view::{IdentFigures, LawReading, PulseFigures, Row, SrsView};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// What the service reads of a run without its thread: every row, the latest
/// pulses' figures, the identification, the law's last values and when the next
/// pulse is due.
#[derive(Default)]
pub struct RunData {
    rows: Mutex<Vec<Row>>,
    figures: Mutex<PulseRing<Arc<PulseFigures>>>,
    identification: Mutex<Option<Arc<IdentFigures>>>,
    law: Mutex<Vec<LawReading>>,
    next_due: Mutex<Option<Instant>>,
}

impl RunData {
    pub fn add_pulses(&self, rows: Vec<Row>, figures: Vec<PulseFigures>) {
        let mut ring = lock(&self.figures);
        for (row, fig) in rows.iter().zip(figures) {
            ring.push(row.index, Arc::new(fig));
        }
        lock(&self.rows).extend(rows);
    }

    /// The rows from index `since`, and the index after the last.
    pub fn rows_since(&self, since: u64) -> (Vec<Row>, u64) {
        let rows = lock(&self.rows);
        let next = rows.len() as u64;
        let from = (since as usize).min(rows.len());
        (rows[from..].to_vec(), next)
    }

    pub fn row(&self, index: u64) -> Option<Row> {
        lock(&self.rows).get(index as usize).cloned()
    }

    pub fn row_count(&self) -> u64 {
        lock(&self.rows).len() as u64
    }

    pub fn figures(&self, index: u64) -> Option<Arc<PulseFigures>> {
        lock(&self.figures).get(index).cloned()
    }

    pub fn last_figures(&self) -> Option<Arc<PulseFigures>> {
        lock(&self.figures).last().cloned()
    }

    pub fn set_identification(&self, f: Option<IdentFigures>) {
        *lock(&self.identification) = f.map(Arc::new);
    }

    pub fn identification(&self) -> Option<Arc<IdentFigures>> {
        lock(&self.identification).clone()
    }

    pub fn set_law(&self, values: Vec<LawReading>) {
        *lock(&self.law) = values;
    }

    pub fn law(&self) -> Vec<LawReading> {
        lock(&self.law).clone()
    }

    pub fn set_next_due(&self, at: Option<Instant>) {
        *lock(&self.next_due) = at;
    }

    /// Seconds until the next pulse is due, 0 once it is; `None` when none is.
    pub fn next_pulse_s(&self) -> Option<f64> {
        lock(&self.next_due).map(|t| t.saturating_duration_since(Instant::now()).as_secs_f64())
    }
}

/// The service's handle on one configured run. Dropping it ends the run thread,
/// which releases anything the run holds; the service drops a run only when it is
/// not live.
pub struct SrsRun {
    pub id: String,
    pub shared: Arc<Shared<SrsView>>,
    pub data: Arc<RunData>,
    /// The configuration as accepted, for `srs.reset` and `srs.config`.
    pub config: serde_json::Value,
    /// Its derived values, as configure returned them.
    pub derived: serde_json::Value,
    /// The offline synthesis at the requested rate, for the configuration's views.
    pub synthesis: Arc<preview::Synthesis>,
    pub wire: Arc<config::WireSrs>,
    pub jobs: Jobs<Cmd>,
}

impl SrsRun {
    /// The view as published, with the clocks and the fire's progress as of now: a
    /// stop asked for shows as `stopping` until the run has ended.
    pub fn view(&self) -> Arc<SrsView> {
        let v = self.shared.view();
        let next = self.data.next_pulse_s();
        let fire = self.shared.progress.now();
        let stopping =
            self.shared.signals.cancelled() && matches!(v.state, State::Running | State::Review);
        let running = matches!(v.state, State::Running | State::Stopping);
        if !stopping && !running {
            return v;
        }
        let mut v = (*v).clone();
        if running {
            v.next_pulse_s = next.filter(|_| v.phase == "waiting");
            v.fire = fire;
        }
        if stopping {
            v.state = State::Stopping;
            v.phase = "stopping";
            v.next_pulse_s = None;
        }
        Arc::new(v)
    }

    pub fn log_since(&self, since: u64) -> (Vec<LogEntry>, u64) {
        self.shared.log_since(since)
    }

    /// Raise the run's stop (or abort) before the request is queued, so a fire in
    /// flight ends at its next block and nothing more fires.
    pub fn signal(&self, stop_outcome: Option<&'static str>, by: &str) {
        self.shared.set_ended_by(by);
        if let Some(o) = stop_outcome {
            self.shared.set_stop_outcome(o);
        }
        self.shared.signals.raise(stop_outcome.is_none());
    }

    /// Hold firing while nobody holds control, or release it.
    pub fn hold(&self, hold: bool) {
        self.shared.signals.set_hold(hold);
    }
}
