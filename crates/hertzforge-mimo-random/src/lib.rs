//! `hertzforge-mimo-random`: the multiple-output (MIMO) Random run service.
//!
//! A test that drives `n_d` shakers at once so that `n_c` control responses, or
//! `n_v` virtual degrees of freedom derived from them, match a reference
//! spectral density matrix on every line of the control band: each control's
//! PSD and the coherence and phase between every pair.
//!
//! - [`config`]: the test configuration and its validation.
//! - [`service`]: [`MimoRandomService`], the state machine, identification,
//!   gates, prediction and the closed loop.
//! - [`guards`]: the core's checks and limits on every drive a law proposes.
//! - [`estimate`]: Welch spectral-matrix estimates in the `E[X X^H]` convention.
//! - [`quantities`]: the display quantities, served in process.
//! - [`records`]: the run's records.
//! - [`replay`]: a recorded run's law calls replayed through the recorded law.
//! - [`pool`]: the per-line linear algebra split over threads.
//!
//! Conventions (fixed; design 7.0): `G = E[X X^H]` per line, one-sided Welch
//! scaling, Hermitian stacks packed. The law works at full level; the core owns
//! the level schedule and every protection.
#![forbid(unsafe_code)]
// Index loops mirror the matrix algebra they implement.
#![allow(clippy::needless_range_loop)]

pub mod config;
pub mod estimate;
pub mod guards;
pub mod law;
pub mod pool;
pub mod quantities;
pub mod records;
pub mod replay;
pub mod report;
pub mod service;

pub use config::{
    Caps, ConfigError, ControlSpace, DriveSpec, Estimation, FrfMode, Gates, Identification,
    LimitSpec, MimoConfig, Protections, Records, ReferenceSpec,
};
pub use quantities::{MimoQuantity, Series, Trace};
pub use report::{
    GateReport, LawLine, MimoEvent, MimoEventKind, MimoFaultReason, MimoReport, MimoState,
    MimoStatus, MimoTripKind, MimoTripRecord, Prediction,
};
pub use service::{
    setup_hash_of, IdentificationSet, Kept, KeptLawCall, KeptLoop, MimoError, MimoRandomService,
};
