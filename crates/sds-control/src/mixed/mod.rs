//! Mixed Mode through the protocol (schema/PROTOCOL.md, "Mixed Mode"): the
//! configuration and its checks, the state and live frames encoded from the
//! controller's status and display, the subscriptions, and the laws' facts for
//! the floor's role and the tones'.

pub mod config;
pub mod law_check;
pub mod record;
pub mod run;
pub mod subscribe;
pub mod view;

use crate::protocol::{ErrorCode, MethodError};
use crate::random::run::LogEntry;
use run::{Cmd, RunShared};
use serde_json::{json, Value};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use view::MixedView;

pub use run::Notice;

/// The service's handle on one configured run. Dropping it ends the run thread,
/// which releases anything the run holds; the service drops a run only when it is
/// not live.
pub struct MixedRun {
    pub id: String,
    /// The configuration as accepted, for `mixed.reset` and `mixed.config`.
    pub config: Value,
    /// Its derived values, as configure returned them.
    pub derived: Value,
    /// The configuration as parsed.
    pub wire: Arc<config::WireMixed>,
    /// The controller's configuration, as the configure checks built it.
    pub core: Arc<hertzforge_mixed::MixedConfig>,
    /// Both roles' laws as the checks saw them.
    pub law: config::LawFacts,
    pub shared: Arc<RunShared>,
    tx: Mutex<mpsc::Sender<Cmd>>,
}

impl MixedRun {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        config: Value,
        derived: Value,
        wire: Arc<config::WireMixed>,
        core: Arc<hertzforge_mixed::MixedConfig>,
        law: config::LawFacts,
        shared: Arc<RunShared>,
        tx: mpsc::Sender<Cmd>,
    ) -> MixedRun {
        MixedRun {
            id,
            config,
            derived,
            wire,
            core,
            law,
            shared,
            tx: Mutex::new(tx),
        }
    }

    pub fn view(&self) -> Arc<MixedView> {
        self.shared.view()
    }

    pub fn log_since(&self, since: u64) -> (Vec<LogEntry>, u64) {
        self.shared.log_since(since)
    }

    pub fn add_log(&self, kind: &'static str, text: impl Into<String>) {
        self.shared.add_log(kind, text)
    }

    /// Queue a request and wait for its answer, at most `wait`.
    pub async fn ask<T: Send + 'static>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, MethodError>>) -> Cmd,
        wait: Duration,
    ) -> Result<T, MethodError> {
        let (tx, rx) = oneshot::channel();
        self.send(make(tx))?;
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(MethodError::internal(format!(
                "run {} ended before it answered",
                self.id
            ))),
            Err(_) => Err(MethodError::new(
                ErrorCode::Internal,
                format!(
                    "run {} did not answer within {:.0} s; the request stands",
                    self.id,
                    wait.as_secs_f64()
                ),
            )),
        }
    }

    pub fn send(&self, cmd: Cmd) -> Result<(), MethodError> {
        let tx = match self.tx.lock() {
            Ok(t) => t,
            Err(p) => p.into_inner(),
        };
        tx.send(cmd)
            .map_err(|_| MethodError::internal(format!("run {} is no longer running", self.id)))
    }
}

/// A refusal by name.
pub fn refuse(reason: &'static str, message: impl Into<String>) -> MethodError {
    MethodError::named(ErrorCode::Refused, reason, message)
}

/// A verb asked in a state it does not apply in.
pub fn wrong_state(v: &MixedView, verb: &str) -> MethodError {
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
