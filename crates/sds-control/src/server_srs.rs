//! The SRS shock methods of the [`Service`] (schema/PROTOCOL.md, "SRS Shock"): a
//! configuration and its run beside the Random, MIMO and classical shock slots,
//! under the same run exclusion and the same controller rules, on the
//! transient-run rails.

use super::{Conn, Feed, Out, Service, DATA_SLACK, DISPLAY_PERIOD, LIVE_TICK};
use crate::codec::{self, Arrays};
use crate::laws;
use crate::protocol::{self, num, ErrorCode, MethodError};
use crate::pulse::rails::{Jobs, Shared};
use crate::srs::config::{self, CheckContext, LawFacts, WireSrs};
use crate::srs::engine::{self, LevelMove};
use crate::srs::preview;
use crate::srs::run::{self as srun, ArmContext, Cmd, RunPlan};
use crate::srs::subscribe::{self, DataContext, Subscription};
use crate::srs::view::{self, LawRef, SrsView, State, StationRef};
use crate::srs::{RunData, SrsRun};
use crate::station::{self, Selection, StationPlan};
use crate::table;
use hertzforge_random_laws::{may_arm_on_hardware, LawCatalog, LawWorld};
use hertzforge_shock_common::ShockRunMode;
use hertzforge_srs::SrsLawFactory;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One connection's SRS shock subscription and when it was last sent a frame.
pub struct SrsDisplay {
    sub: Arc<Subscription>,
    last: Option<Instant>,
}

/// How long to wait for the run thread to answer: its identification, a record,
/// and a margin.
fn wait_for(run: &SrsRun) -> Duration {
    let w = &run.wire;
    let fs = w.fs_hz().max(1.0);
    let record_s = record_len(run) as f64 / fs;
    let sysid_s = 4.0 * w.sysid_nfft as f64 / fs + 2.0 * record_s;
    Duration::from_secs_f64(10.0 + sysid_s + 2.0 * record_s)
}

/// One pulse's record at the requested rate, samples.
fn record_len(run: &SrsRun) -> usize {
    run.synthesis.waveform_g.len() + config::ring_out_samples(run.wire.ring_out_s, run.wire.fs_hz())
}

const ARM_WAIT: Duration = Duration::from_secs(120);

fn not_configured() -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "not_configured",
        "SRS shock is not configured; configure a test first.",
    )
}

/// The law a configuration's checks look up by id: the catalog's entry.
fn law_facts(cat: &LawCatalog, id: &str) -> Option<LawFacts> {
    cat.entry(id).map(|e| laws::srs_facts(&e))
}

fn law_ref(f: &LawFacts) -> LawRef {
    LawRef {
        id: f.id.clone(),
        name: f.name.clone(),
        version: f.version.clone(),
        sha256: f.sha256.clone(),
    }
}

impl Service {
    pub(super) fn srs_displays(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, SrsDisplay>> {
        match self.srs_displays.lock() {
            Ok(d) => d,
            Err(p) => p.into_inner(),
        }
    }

    fn srs_station_ref(inner: &super::Inner) -> StationRef {
        let selected = inner.stations.selected().clone();
        StationRef {
            kind: if selected == Selection::Sim {
                "sim"
            } else {
                "ni"
            },
            class: station::class_name(inner.stations.class()),
            devices: match &selected {
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
            },
        }
    }

    /// Parse and check `config` against the present table: the configuration, its
    /// items, its derived values and its law.
    async fn srs_check(
        &self,
        config: &Value,
    ) -> Result<
        (
            WireSrs,
            table::Report,
            Option<config::Derived>,
            Option<LawFacts>,
        ),
        MethodError,
    > {
        let w = config::parse(config)?;
        let (tbl, table_report) = {
            let inner = self.inner.lock().await;
            (
                inner.table.table.clone(),
                table::validate(&inner.table.table, &inner.ctx()),
            )
        };
        let timeout_s = self.opts.timeout.as_secs_f64();
        let id = w.law.id.clone();
        let (w, report, derived, facts) = self
            .with_catalog(move |cat| {
                let lookup = |i: &str| law_facts(cat, i);
                let ctx = CheckContext {
                    table: &tbl,
                    law: &lookup,
                    timeout_s,
                };
                let (report, derived) = w.check(&ctx);
                let facts = law_facts(cat, &id);
                Ok((w, report, derived, facts))
            })
            .await?;
        let mut all = table_report;
        all.extend(report);
        let derived = if all.errors.is_empty() { derived } else { None };
        Ok((w, all, derived, facts))
    }

    pub(super) async fn srs_configure(
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
        let dry_run = protocol::param_bool(params, "dry_run")?.unwrap_or(false);
        let (w, report, derived, facts) = self.srs_check(config).await?;
        if dry_run {
            let mut arrays = Arrays::new();
            let views = match &derived {
                Some(_) => {
                    let syn = synthesize(&w).await?;
                    let env = w.envelope_bands(syn.bands_hz.len());
                    Value::Array(preview::synthesis_views(
                        &syn.figures(&env),
                        Some(subscribe::DEFAULT_POINTS),
                        &mut arrays,
                    ))
                }
                None => json!([]),
            };
            return Ok((
                json!({
                    "errors": report.errors.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                    "warnings": report.warnings.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                    "derived": derived.as_ref().map_or(Value::Null, |d| d.to_json()),
                    "views": views,
                }),
                arrays,
            ));
        }
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        if inner.sim_running() {
            return Err(MethodError::named(
                ErrorCode::Refused,
                "run_active",
                "A simulation is running; stop it first.",
            ));
        }
        let (Some(derived), Some(facts)) = (derived, facts) else {
            return Err(config::config_invalid(&report));
        };
        drop(inner);
        let syn = synthesize(&w).await?;
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        let run = self.new_srs_run(&mut inner, w, syn, &derived, &facts)?;
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

    /// Create a run (replacing the one before) and start its thread.
    fn new_srs_run(
        &self,
        inner: &mut super::Inner,
        w: WireSrs,
        synthesis: preview::Synthesis,
        derived: &config::Derived,
        facts: &LawFacts,
    ) -> Result<Arc<SrsRun>, MethodError> {
        inner.srs_runs += 1;
        let run_id = format!("srs-{:04}", inner.srs_runs);
        let station = Self::srs_station_ref(inner);
        let initial = view::configured_view(&run_id, &w, derived, station, law_ref(facts));
        let shared = Arc::new(Shared::new(initial.clone(), self.srs_notify.clone()));
        let data = Arc::new(RunData::default());
        let synthesis = Arc::new(synthesis);
        let raw = w.raw.clone();
        let wire = Arc::new(w);
        let plan = RunPlan {
            run_id: run_id.clone(),
            cfg: wire.clone(),
            engine: self
                .opts
                .srs_engine
                .clone()
                .unwrap_or_else(engine::service_engine_factory),
            sim_speed: self.opts.sim_speed,
            timeout_s: self.opts.timeout.as_secs_f64(),
            derived: derived.to_json(),
            synthesis: synthesis.clone(),
            record: self.opts.record.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        srun::spawn(plan, initial.clone(), shared.clone(), data.clone(), rx)
            .map_err(|e| MethodError::internal(format!("could not start the run thread: {e}")))?;
        let run = Arc::new(SrsRun {
            id: run_id.clone(),
            shared: shared.clone(),
            data,
            config: raw,
            derived: derived.to_json(),
            synthesis,
            wire: wire.clone(),
            jobs: Jobs::new(&run_id, tx),
        });
        inner.srs = Some(run.clone());
        inner.srs_rows_sent = 0;
        shared.add_log(
            "state",
            format!("configured \"{}\" on {}", wire.title, initial.station.class),
        );
        let _ = self.srs_notify.send((run_id, Arc::new(initial)));
        Ok(run)
    }

    pub(super) async fn srs_config(&self) -> (Value, Arrays) {
        let inner = self.inner.lock().await;
        let mut arrays = Arrays::new();
        let result = match &inner.srs {
            None => json!({"run_id": null, "config": null, "derived": null, "views": []}),
            Some(run) => {
                let env = run.wire.envelope_bands(run.synthesis.bands_hz.len());
                json!({
                    "run_id": run.id,
                    "config": run.config,
                    "derived": run.derived,
                    "views": preview::synthesis_views(
                        &run.synthesis.figures(&env),
                        Some(subscribe::DEFAULT_POINTS),
                        &mut arrays,
                    ),
                })
            }
        };
        (result, arrays)
    }

    pub(super) async fn srs_preview(
        &self,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        let w = config::parse_preview(params)?;
        let env = match params.get("envelope") {
            None | Some(Value::Null) => None,
            Some(e) => Some(e.clone()),
        };
        // The envelope's checks are a configuration's.
        let probe = json!({
            "target": params["target"],
            "bands": params["bands"],
            "envelope": env,
            "schedule": {"mode": "manual", "levels_db": [0], "pulses": [1]},
            "limits": {"drive_vpk_ceiling": 1, "drive_vpk_abort": 2},
        });
        let probe = config::parse(&probe)?;
        let mut r = table::Report::default();
        config::check_synthesis(&mut r, &w, "");
        let (er, _) = probe.check(&CheckContext {
            table: &preview_table(),
            law: &|id: &str| (id == "srs_ratio").then(laws::builtin_srs_facts),
            timeout_s: f64::INFINITY,
        });
        r.errors.extend(
            er.errors.into_iter().filter(
                |i| matches!(&i.at, crate::table::At::Field(f) if f.starts_with("envelope")),
            ),
        );
        if let Some(first) = r.errors.first() {
            let field = match &first.at {
                crate::table::At::Field(f) if f == "acquisition.fs_hz" => "fs_hz".to_owned(),
                crate::table::At::Field(f) => f.clone(),
                _ => "target".into(),
            };
            return Err(MethodError::bad_params(&field, first.message.clone()));
        }
        let syn = tokio::task::spawn_blocking(move || preview::synthesize(&w))
            .await
            .map_err(|e| MethodError::internal(format!("the preview failed: {e}")))?;
        let env = probe.envelope_bands(syn.bands_hz.len());
        let mut arrays = Arrays::new();
        let result =
            preview::preview_json(&syn, &env, Some(subscribe::DEFAULT_POINTS), &mut arrays);
        Ok((result, arrays))
    }

    pub(super) async fn srs_target_import(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let path = protocol::named_str(params, "path")?
            .ok_or_else(|| {
                MethodError::bad_params("path", "An import needs the path of its file.")
            })?
            .to_owned();
        tokio::task::spawn_blocking(move || crate::srs::import::target_import(&path))
            .await
            .map_err(|e| MethodError::internal(format!("the import failed: {e}")))?
    }

    pub(super) async fn srs_table_import(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let path = protocol::named_str(params, "path")?
            .ok_or_else(|| {
                MethodError::bad_params("path", "An import needs the path of its file.")
            })?
            .to_owned();
        let engine = crate::srs::import::parse_engine(params)?;
        tokio::task::spawn_blocking(move || crate::srs::import::table_import(&path, engine))
            .await
            .map_err(|e| MethodError::internal(format!("the import failed: {e}")))?
    }

    async fn controlled_srs(&self, conn: &Conn) -> Result<Arc<SrsRun>, MethodError> {
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.srs.clone().ok_or_else(not_configured)
    }

    /// What a run arms on now: the table, the selected station and its law.
    async fn srs_arm_context(&self, run: &SrsRun) -> Result<ArmContext, MethodError> {
        let (tbl, station_plan, hardware, revision, devices) = {
            let inner = self.inner.lock().await;
            let report = table::validate(&inner.table.table, &inner.ctx());
            if !report.errors.is_empty() {
                return Err(super::table_invalid(
                    &report,
                    "on this station, so the run cannot arm",
                ));
            }
            let fs_hz = run.wire.fs_hz();
            let (plan, hardware) = match inner.stations.selected() {
                Selection::Sim => (StationPlan::sim(fs_hz), false),
                Selection::Ni { devices } => {
                    let (inputs, outputs) =
                        hertzforge_station::physical_from_table(&inner.table.table);
                    (
                        StationPlan::Ni {
                            devices: devices.clone(),
                            inputs,
                            outputs,
                            fs_hz,
                        },
                        station::class_name(inner.stations.class()) == "ni_real",
                    )
                }
            };
            (
                inner.table.table.clone(),
                plan,
                hardware,
                inner.table.revision,
                inner.stations.devices().to_vec(),
            )
        };
        let id = run.wire.law.id.clone();
        let wire = run.wire.clone();
        let values_of = id.clone();
        let law_values = self
            .with_catalog(move |cat| {
                Ok(cat
                    .entry(&values_of)
                    .map(|e| e.manifest.diagnostics)
                    .unwrap_or_default())
            })
            .await
            .unwrap_or_default();
        let law: Result<Arc<dyn SrsLawFactory>, MethodError> = self
            .with_catalog(move |cat| {
                if let Some(entry) = cat.entry(&id) {
                    if hardware && may_arm_on_hardware(&entry).is_err() {
                        return Err(MethodError::named(
                            ErrorCode::Refused,
                            "law_not_checked",
                            format!(
                                "The control law \"{}\" has no passed check for this file; run \
                                 laws.check on it before it drives hardware.",
                                entry.identity.name
                            ),
                        ));
                    }
                }
                let lf = law_facts(cat, &id).ok_or_else(|| {
                    MethodError::named(
                        ErrorCode::NotFound,
                        "law_unknown",
                        format!("No control law \"{id}\" is in the catalog."),
                    )
                })?;
                let choice = wire.law_choice(&lf);
                Ok(
                    hertzforge_shock_laws::resolve_srs(cat, &choice).map_err(|e| {
                        MethodError::named(
                            ErrorCode::Refused,
                            "arm_refused",
                            format!("The control law could not be built: {e}."),
                        )
                    }),
                )
            })
            .await?;
        Ok(ArmContext {
            table: tbl,
            station: station_plan,
            scanner: self.opts.scanner.clone(),
            data_root: self.opts.data_root.clone(),
            records_root: self.opts.records_root(),
            bind: self.opts.bind.clone(),
            law,
            law_values,
            revision,
            devices,
        })
    }

    pub(super) async fn srs_arm(
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
        let run = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.srs.clone().ok_or_else(not_configured)?;
            let v = run.view();
            if v.state == State::Finished {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_finished",
                    "This run has finished; reset or configure again to run.",
                )
                .with_detail(json!({"outcome": v.end.as_ref().map(|e| e.outcome)})));
            }
            if inner.random_live()
                || inner.mimo_live()
                || inner.shock_live()
                || inner.sine_live()
                || inner.mixed_live()
                || inner.twr_live()
                || inner.explorer_live()
                || inner.recorder_live()
                || inner.mimo_shock_live()
            {
                inner.run_active()?;
            }
            if v.state.is_busy() || (!start && v.state == State::Armed) {
                inner.run_active()?;
            }
            if inner.sim_running() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    "A simulation is running; stop it first.",
                ));
            }
            run
        };
        let ctx = self.srs_arm_context(&run).await?;
        if start {
            run.jobs
                .ask(|reply| Cmd::Start { ctx, reply }, ARM_WAIT)
                .await?;
        } else {
            run.jobs
                .ask(|reply| Cmd::Arm { ack, ctx, reply }, ARM_WAIT)
                .await?;
        }
        self.spawn_srs_live(run.clone());
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn srs_proceed(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_srs(conn).await?;
        let v = run.view();
        if v.state != State::Review {
            return Err(srun::wrong_state(&v, "Proceeding"));
        }
        run.jobs
            .ask(|reply| Cmd::Proceed { reply }, wait_for(&run))
            .await?;
        Ok(view::state_json(Some(&run.view())))
    }

    /// The refusals a fire, train or burst gets without asking the run thread.
    fn srs_pre_fire(run: &SrsRun, verb: &str) -> Result<(), MethodError> {
        if run.wire.mode == ShockRunMode::Auto {
            return Err(srun::wrong_mode(verb));
        }
        let v = run.view();
        if run.shared.progress.now().is_some() || v.phase == "firing" {
            return Err(srun::not_yet(
                record_len(run) as f64 / run.wire.fs_hz().max(1.0),
            ));
        }
        if !(v.state == State::Running && v.phase == "waiting") {
            return Err(srun::wrong_state(&v, verb));
        }
        Ok(())
    }

    pub(super) async fn srs_fire(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_srs(conn).await?;
        Self::srs_pre_fire(&run, "A fire")?;
        run.jobs
            .ask(|reply| Cmd::Fire { reply }, wait_for(&run))
            .await?;
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn srs_level(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let delta = protocol::named_f64(params, "delta_db")?;
        let rung = match params.get("rung") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| {
                MethodError::bad_params("rung", "\"rung\" is a level of the table, from 0")
            })? as usize),
        };
        let to = match (delta, rung) {
            (Some(d), None) if d == 1.0 || d == -1.0 => LevelMove::Step(d),
            (None, Some(i)) => LevelMove::Rung(i),
            _ => {
                return Err(MethodError::bad_params(
                    "delta_db",
                    "A level change is a step of 1 or -1 dB (\"delta_db\"), or a level of the \
                     table (\"rung\").",
                ))
            }
        };
        let run = self.controlled_srs(conn).await?;
        if run.wire.mode == ShockRunMode::Auto {
            return Err(srun::wrong_mode("A level change"));
        }
        let level = run
            .jobs
            .ask(|reply| Cmd::Level { to, reply }, wait_for(&run))
            .await?;
        Ok(json!({"level_db": num(config::round(level, 2))}))
    }

    /// `srs.stop` and `srs.abort`: open to every connection.
    pub(super) async fn srs_end(&self, conn: &Conn, abort: bool) -> Result<Value, MethodError> {
        let (run, by) = {
            let inner = self.inner.lock().await;
            let Some(run) = inner.srs.clone() else {
                return Ok(view::state_json(None));
            };
            (run, inner.session.client_name(conn.id, &conn.peer))
        };
        if !run.view().state.is_live() {
            return Ok(view::state_json(Some(&run.view())));
        }
        stop_srs(
            &run,
            if abort { None } else { Some("operator_stop") },
            by,
            true,
        )
        .await;
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn srs_reset(&self, conn: &Conn) -> Result<Value, MethodError> {
        let (raw, busy) = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.srs.clone().ok_or_else(not_configured)?;
            (run.config.clone(), run.view().state.is_live())
        };
        if busy {
            self.inner.lock().await.run_active()?;
        }
        let (w, report, derived, facts) = self.srs_check(&raw).await?;
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        let (Some(derived), Some(facts)) = (derived, facts) else {
            return Err(config::config_invalid(&report));
        };
        drop(inner);
        let syn = synthesize(&w).await?;
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        let run = self.new_srs_run(&mut inner, w, syn, &derived, &facts)?;
        let v = run.view();
        Ok(view::state_json(Some(&v)))
    }

    pub(super) async fn srs_clear(&self, conn: &Conn) -> Result<Value, MethodError> {
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        if inner.srs_live() {
            inner.run_active()?;
        }
        inner.srs = None;
        let state = view::state_json(None);
        inner.srs_state_key = view::state_key(None);
        self.emit("srs.state", state.clone());
        Ok(state)
    }

    pub(super) async fn srs_state_now(&self) -> Value {
        let inner = self.inner.lock().await;
        let v = inner.srs.as_ref().map(|r| r.view());
        view::state_json(v.as_deref())
    }

    pub(super) async fn srs_log(&self, params: &Map<String, Value>) -> Result<Value, MethodError> {
        let since = since_param(params)?;
        let inner = self.inner.lock().await;
        let (entries, next) = match &inner.srs {
            Some(r) => r.log_since(since),
            None => (Vec::new(), 0),
        };
        Ok(json!({
            "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "next": next,
        }))
    }

    pub(super) async fn srs_pulses(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let since = since_param(params)?;
        let run = self.inner.lock().await.srs.clone();
        let (rows, next) = match &run {
            Some(r) => r.data.rows_since(since),
            None => (Vec::new(), 0),
        };
        Ok(json!({"pulses": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(), "next": next}))
    }

    pub(super) async fn srs_pulse(
        &self,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        let index = params
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| MethodError::bad_params("index", "\"index\" is a pulse, from 0"))?;
        let points = subscribe::parse_points(true, params.get("points"), "points")?;
        let run = self
            .inner
            .lock()
            .await
            .srs
            .clone()
            .ok_or_else(not_configured)?;
        let row = run.data.row(index).ok_or_else(|| {
            MethodError::not_found(format!(
                "No pulse {index} has fired in run {} ({} so far).",
                run.id,
                run.data.row_count()
            ))
        })?;
        let fig = run.data.figures(index).ok_or_else(|| {
            MethodError::not_found(format!(
                "The views of pulse {index} are no longer held; the last {} pulses' are. Its \
                 row is in srs.pulses.",
                crate::pulse::ring::DEFAULT_CAPACITY
            ))
        })?;
        let mut arrays = Arrays::new();
        let views = view::all_pulse_views(&fig, points, &mut arrays);
        Ok((json!({"pulse": row.to_json(), "views": views}), arrays))
    }

    /// `srs.subscribe`: replace `conn`'s subscription, and send it the run's
    /// latest data at once when the run has any.
    pub(super) async fn srs_subscribe(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let sub = subscribe::parse(params)?.map(Arc::new);
        let result = subscribe::result_json(sub.as_deref());
        match &sub {
            None => {
                self.srs_displays().remove(&conn.id);
            }
            Some(sub) => {
                self.srs_displays().insert(
                    conn.id,
                    SrsDisplay {
                        sub: sub.clone(),
                        last: None,
                    },
                );
                let run = self.inner.lock().await.srs.clone();
                if let Some(run) = run.filter(|r| r.view().streaming) {
                    self.emit_srs_data(conn.id, &run, sub);
                    if let Some(d) = self.srs_displays().get_mut(&conn.id) {
                        d.last = Some(Instant::now());
                    }
                }
            }
        }
        Ok(result)
    }

    fn emit_srs_data(&self, conn: u64, run: &SrsRun, sub: &Subscription) {
        let v = run.view();
        let last = run.data.last_figures();
        let ident = run.data.identification();
        let law = run.data.law();
        let (rows, _) = run.data.rows_since(0);
        let ctx = DataContext {
            view: &v,
            last: last.as_deref(),
            identification: ident.as_deref(),
            law: &law,
            rows: &rows,
        };
        let mut arrays = Arrays::new();
        let data = subscribe::data_json(&ctx, sub, &mut arrays);
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match codec::encode(&protocol::event("srs.data", seq, data), &arrays) {
            Ok(frame) => {
                let _ = self.events.send(Arc::new(Out::To(conn, Feed::Srs, frame)));
            }
            Err(e) => eprintln!("sds-control: srs.data frame not encoded: {e}"),
        }
    }

    /// Each subscribed connection whose frame is due (every one when `last`).
    fn srs_data_due(&self, run: &SrsRun, last: bool) {
        let now = Instant::now();
        let due: Vec<(u64, Arc<Subscription>)> = {
            let mut displays = self.srs_displays();
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
            self.emit_srs_data(conn, run, &sub);
        }
    }

    /// A run published `v`: `srs.state` when what it shows changed, its new log
    /// entries, an `srs.pulse` for each new pulse, and `srs.data` when there is
    /// something new to plot.
    pub(super) async fn srs_changed(&self, run_id: &str, v: &SrsView) {
        let mut inner = self.inner.lock().await;
        let Some(run) = inner.srs.clone() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        let shown = run.view();
        let key = view::state_key(Some(&shown));
        if key != inner.srs_state_key {
            inner.srs_state_key = key;
            self.emit("srs.state", view::state_json(Some(&shown)));
        }
        let since = if inner.srs_log_sent.0 == run.id {
            inner.srs_log_sent.1
        } else {
            0
        };
        let (entries, next) = run.log_since(since);
        inner.srs_log_sent = (run.id.clone(), next);
        if !entries.is_empty() {
            self.emit(
                "srs.log",
                json!({
                    "run_id": run.id,
                    "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
                }),
            );
        }
        let rows_now = run.data.row_count();
        let first_new = inner.srs_rows_sent;
        inner.srs_rows_sent = rows_now;
        let ident_new = v.identification.is_some() && !inner.srs_ident_sent;
        inner.srs_ident_sent = v.identification.is_some();
        drop(inner);
        for index in first_new..rows_now {
            let (Some(row), fig) = (run.data.row(index), run.data.figures(index)) else {
                continue;
            };
            let mut arrays = Arrays::new();
            let views = fig.map_or_else(Vec::new, |f| {
                view::default_pulse_views(&f, Some(subscribe::DEFAULT_POINTS), &mut arrays)
            });
            self.emit_binary(
                "srs.pulse",
                json!({"run_id": run.id, "pulse": row.to_json(), "views": views}),
                &arrays,
            );
        }
        let finished = v.state == State::Finished;
        if rows_now > first_new || ident_new || finished {
            self.srs_data_due(&run, finished);
        }
    }

    /// Publish `run`'s `srs.live` five times a second until it has finished, then
    /// once more with the final state. One task per run.
    fn spawn_srs_live(self: &Arc<Self>, run: Arc<SrsRun>) {
        if run
            .shared
            .live_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
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
                    .srs
                    .as_ref()
                    .is_some_and(|r| Arc::ptr_eq(r, &run));
                let v = run.view();
                if !v.streaming {
                    if !current {
                        break;
                    }
                    continue;
                }
                let last = v.state == State::Finished || !current;
                let now = Instant::now();
                if last || last_live.is_none_or(|t| now >= t + DISPLAY_PERIOD - DATA_SLACK) {
                    last_live = Some(now);
                    svc.emit_binary("srs.live", view::live_json(&v), &Arrays::new());
                }
                if last {
                    break;
                }
            }
        });
    }
}

/// The offline synthesis of `w`, off the async threads.
async fn synthesize(w: &WireSrs) -> Result<preview::Synthesis, MethodError> {
    let s = w.synthesis.clone();
    tokio::task::spawn_blocking(move || preview::synthesize(&s))
        .await
        .map_err(|e| MethodError::internal(format!("the synthesis failed: {e}")))
}

/// The table a preview's checks run against: a control row and a drive row.
fn preview_table() -> hertzforge_channel_table::ChannelTable {
    let mut t = table::Table::new().table;
    let mut c = table::new_row(1);
    c.role = hertzforge_channel_table::ChannelRole::Control;
    let mut d = table::new_row(2);
    d.role = hertzforge_channel_table::ChannelRole::Drive;
    t.channels = vec![c, d];
    t
}

fn since_param(params: &Map<String, Value>) -> Result<u64, MethodError> {
    match params.get("since") {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v
            .as_u64()
            .ok_or_else(|| MethodError::bad_params("since", "\"since\" is a whole number from 0")),
    }
}

/// End `run`: the controlled stop with `outcome` (`None`: the abort), raised on the
/// run's signals first so a fire in flight ends at its next block, then queued.
pub(super) async fn stop_srs(run: &SrsRun, outcome: Option<&'static str>, by: String, wait: bool) {
    if !run.view().state.is_live() {
        return;
    }
    run.signal(outcome, &by);
    let w = wait_for(run);
    match outcome {
        None => {
            let _ = run.jobs.ask(|reply| Cmd::Abort { by, reply }, w).await;
        }
        Some(outcome) if wait => {
            let _ = run
                .jobs
                .ask(
                    |reply| Cmd::Stop {
                        outcome,
                        by,
                        reply: Some(reply),
                    },
                    w,
                )
                .await;
        }
        Some(outcome) => {
            let _ = run.jobs.send(Cmd::Stop {
                outcome,
                by,
                reply: None,
            });
        }
    }
}

impl Service {
    /// The catalog id of the SRS shock law `laws.check` names, by its `id` or the
    /// `path` of its file (imported first, for the controller only, as the check
    /// itself would), or `None` when it names no SRS law.
    pub(super) async fn srs_law_id(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Option<String> {
        let id = params.get("id").and_then(Value::as_str).map(str::to_owned);
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if path.is_some() {
            let inner = self.inner.lock().await;
            if id.is_some() || inner.session.require_controller(conn.id).is_err() {
                return None;
            }
        }
        self.with_catalog(move |cat| {
            let id = match (id, path) {
                (Some(id), None) => id,
                (None, Some(p)) => laws::import(cat, &p)?["law"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                _ => return Ok(None),
            };
            Ok(cat
                .entry(&id)
                .filter(|e| e.world == LawWorld::ShockSrs)
                .map(|e| e.id))
        })
        .await
        .ok()
        .flatten()
    }

    /// `laws.check` of an SRS shock law: every case of the SRS check on its
    /// simulated station, one `laws.check` event as each starts and as it ends.
    pub(super) async fn srs_laws_check(
        self: &Arc<Self>,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        use crate::mimo::law_check::{bad_choice, not_recorded, now_rfc3339, CheckSlot};
        let id = protocol::named_str(params, "id")?
            .map(str::to_owned)
            .ok_or_else(|| {
                MethodError::bad_params("id", "Checking a law needs its catalog \"id\".")
            })?;
        let given = match params.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                return Err(MethodError::bad_params(
                    "params",
                    "\"params\" is an object of parameter values by key.",
                ))
            }
        };
        {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            inner.run_active()?;
        }
        let law_id = id.clone();
        let (facts, factory) = self
            .with_catalog(move |cat| {
                let facts = law_facts(cat, &law_id)
                    .ok_or_else(|| crate::mimo::law_check::unknown_law(&law_id))?;
                for (k, v) in &given {
                    match facts.params.iter().find(|p| &p.key == k) {
                        None => return Err(bad_choice(&law_id, &format!("no parameter {k}"))),
                        Some(spec) => {
                            if let Some(why) = crate::shock::config::param_problem(spec, v) {
                                return Err(bad_choice(&law_id, &format!("{} {why}", spec.label)));
                            }
                        }
                    }
                }
                let choice = hertzforge_random_laws::LawChoice::new(
                    law_id.clone(),
                    crate::mimo::adapter::params_of(&facts.params, &given),
                );
                let factory = hertzforge_shock_laws::resolve_srs(cat, &choice).map_err(|e| {
                    MethodError::named(
                        ErrorCode::Refused,
                        "law_check_unsupported",
                        format!("The SRS shock check cannot run \"{law_id}\": {e}."),
                    )
                })?;
                Ok((facts, factory))
            })
            .await?;
        let _slot = CheckSlot::take()?;
        let svc = self.clone();
        let check_id = facts.id.clone();
        let results = tokio::task::spawn_blocking(move || {
            crate::srs::law_check::run_check(&check_id, factory, &mut |event| {
                svc.emit("laws.check", event);
            })
        })
        .await
        .map_err(|e| MethodError::internal(format!("the law check failed: {e}")))??;
        let passed = !results.is_empty() && results.iter().all(|(_, v)| v.passed);
        let cases: Vec<Value> = results.iter().map(|(j, _)| j.clone()).collect();
        let (law, recorded) = if facts.builtin {
            let id = facts.id.clone();
            let law = self
                .with_catalog(move |cat| {
                    cat.entry(&id)
                        .map(|e| laws::entry_json(&e))
                        .ok_or_else(|| MethodError::internal("the built-in law left the catalog"))
                })
                .await?;
            (law, false)
        } else {
            let sha = facts.sha256.clone();
            let verdicts: Vec<_> = results.into_iter().map(|(_, v)| v).collect();
            let law = self
                .with_catalog(move |cat| {
                    let record = crate::srs::law_check::check_record(sha, now_rfc3339(), verdicts);
                    cat.record_check(&record)
                        .map_err(|e| not_recorded(&e.to_string()))?;
                    let e = cat
                        .entry(&record.law_sha256)
                        .ok_or_else(|| MethodError::internal("the checked law left the catalog"))?;
                    Ok(laws::entry_json(&e))
                })
                .await?;
            (law, true)
        };
        Ok(json!({
            "law": law,
            "report": crate::srs::law_check::report_json(&cases, passed, recorded),
        }))
    }
}
