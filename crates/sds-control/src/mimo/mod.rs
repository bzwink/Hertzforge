//! MIMO Random through the protocol (schema/PROTOCOL.md, "MIMO Random"): the setup
//! and its validation, the driver trait the service runs a MIMO test through, the
//! status and report objects, and the display quantities.
//!
//! - [`setup`]: the setup on the wire, parsed and checked against the channel table
//!   and the law catalog.
//! - [`check`]: the validation report (ranks, positive semi-definiteness,
//!   feasibility per drive, law shape admission).
//! - [`driver`]: [`driver::MimoRunDriver`], what the service needs from a run.
//! - [`adapter`]: [`adapter::ServiceDriver`], the controller's MIMO Random service
//!   behind the trait, on its own run thread: what a run uses.
//! - [`sim`]: [`sim::SimulatedMimoDriver`], a deterministic stand-in for wire tests
//!   that do not need the controller's service.
//! - [`view`]: the status, prediction and report objects.
//! - [`display`]: the display quantities, their selectors and decimation.
//! - [`record`]: what a run puts in its run folder (`docs/run-folder.md`).
//! - [`law_check`]: the check of a MIMO law on the simulated test articles before it
//!   drives hardware (`laws.check`).
//! - [`reference`]: the reference builders, readers and checks on the wire
//!   (`mimo_random.reference.*`).

pub mod adapter;
pub mod check;
pub mod display;
pub mod driver;
pub mod law_check;
pub mod record;
pub mod reference;
pub mod setup;
pub mod sim;
pub mod view;

use crate::random::config::Grid;
use crate::random::view::StationRef;
use driver::{MimoRunDriver, MimoStatus};
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// The service's handle on one MIMO setup and its run.
pub struct MimoRun {
    pub id: String,
    pub setup: Arc<setup::MimoSetup>,
    pub station: StationRef,
    pub grid: Grid,
    /// The setup's validation report, as `mimo_random.setup` returned it.
    pub validation: Value,
    driver: Mutex<Box<dyn MimoRunDriver>>,
}

impl MimoRun {
    pub fn new(
        id: String,
        setup: Arc<setup::MimoSetup>,
        station: StationRef,
        grid: Grid,
        validation: Value,
        driver: Box<dyn MimoRunDriver>,
    ) -> MimoRun {
        MimoRun {
            id,
            setup,
            station,
            grid,
            validation,
            driver: Mutex::new(driver),
        }
    }

    /// Call the driver. Its methods return promptly, so the lock is short.
    pub fn with<T>(&self, f: impl FnOnce(&mut dyn MimoRunDriver) -> T) -> T {
        let mut d = match self.driver.lock() {
            Ok(d) => d,
            Err(p) => p.into_inner(),
        };
        f(d.as_mut())
    }

    pub fn facts(&self) -> view::RunFacts<'_> {
        view::RunFacts {
            run_id: &self.id,
            setup: &self.setup,
            station: &self.station,
            grid: &self.grid,
            clock: None,
        }
    }

    /// The driver's status, and the `mimo_random.status` object of it.
    pub fn status(&self) -> (MimoStatus, Value) {
        let s = self.with(|d| d.status());
        let v = view::status_json(Some((&self.facts(), &s)));
        (s, v)
    }

    /// Armed, running, holding for review or stopping.
    pub fn is_live(&self) -> bool {
        self.with(|d| d.state()).is_live()
    }

    /// The report object, once the run has finished.
    pub fn report(&self) -> Option<Value> {
        self.with(|d| d.report())
            .map(|r| view::report_json(&self.facts(), &r))
    }
}
