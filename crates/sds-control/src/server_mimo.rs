//! The MIMO Random and control-law methods of the [`Service`] (schema/PROTOCOL.md,
//! "MIMO Random" and "Control Laws"): a MIMO setup and its run beside the Random
//! slot, under the same run exclusion and the same controller rules.

use super::{Conn, Feed, Out, Service, DATA_SLACK, LIVE_TICK};
use crate::codec::{self, Arrays};
use crate::laws;
use crate::mimo::display;
use crate::mimo::driver::{LawRef, OverrideReason, StartContext, StopOutcome};
use crate::mimo::setup::{self, SetupContext};
use crate::mimo::{view as mview, MimoRun};
use crate::protocol::{self, ErrorCode, MethodError};
use crate::random::config::Grid;
use crate::random::view::{State, StationRef};
use crate::station::{self, Selection};
use crate::table::{self, Report};
use hertzforge_random_laws::LawCatalog;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// One connection's MIMO display subscription and when it was last sent a frame.
pub struct MimoDisplay {
    sub: Arc<display::MimoSubscription>,
    last: Option<std::time::Instant>,
}

impl Service {
    pub(super) fn mimo_displays(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, MimoDisplay>> {
        match self.mimo_displays.lock() {
            Ok(d) => d,
            Err(p) => p.into_inner(),
        }
    }

    /// Run `f` on the law catalog, opening it on first use.
    pub(super) async fn with_catalog<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut LawCatalog) -> Result<T, MethodError> + Send + 'static,
    ) -> Result<T, MethodError> {
        let cell = self.catalog.clone();
        let root = self.opts.data_root.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = match cell.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if guard.is_none() {
                *guard = Some(LawCatalog::open(&root).map_err(|e| laws::catalog_unavailable(&e))?);
            }
            match guard.as_mut() {
                Some(c) => f(c),
                None => Err(MethodError::internal("the law catalog did not open")),
            }
        })
        .await
        .map_err(|e| MethodError::internal(format!("the law catalog task failed: {e}")))?
    }

    fn new_driver(&self, ctx: &StartContext) -> Box<dyn crate::mimo::driver::MimoRunDriver> {
        match &self.opts.mimo_driver {
            Some(f) => f(ctx),
            None => Box::new(crate::mimo::adapter::ServiceDriver::new(
                crate::mimo::adapter::AdapterOptions {
                    data_root: self.opts.data_root.clone(),
                    capture_root: Some(self.opts.records_root()),
                    sim_speed: self.opts.sim_speed,
                    scanner: self.opts.scanner.clone(),
                    bind: self.opts.bind.clone(),
                    catalog: Some(self.catalog.clone()),
                    law_factory: None,
                    record: self.opts.record.clone(),
                },
            )),
        }
    }

    /// Parse `setup` and validate it with a fresh driver: the setup, its driver
    /// (configured) and the validation report.
    async fn mimo_check(
        &self,
        setup: &Value,
    ) -> Result<
        (
            Arc<setup::MimoSetup>,
            Box<dyn crate::mimo::driver::MimoRunDriver>,
            crate::mimo::check::Validation,
            StartContext,
        ),
        MethodError,
    > {
        let (table, kind, class, table_report, ni_devices) = {
            let inner = self.inner.lock().await;
            let kind = match inner.stations.selected() {
                Selection::Sim => "sim",
                Selection::Ni { .. } => "ni",
            };
            (
                inner.table.table.clone(),
                kind,
                station::class_name(inner.stations.class()),
                table::validate(&inner.table.table, &inner.ctx()),
                ni_devices(inner.stations.selected()),
            )
        };
        let raw = setup.clone();
        let table_now = table.clone();
        let (parsed, items, law) = self
            .with_catalog(move |cat| {
                let lookup = |id: &str| cat.entry(id).map(|e| laws::law_info(&e));
                let ctx = SetupContext {
                    table: &table,
                    selected_kind: kind,
                    law: &lookup,
                };
                let (s, items) = setup::parse(&raw, &ctx)?;
                let law = lookup(&s.law.id)
                    .filter(|l| l.mimo.is_some())
                    .map(|l| laws::law_ref(&l));
                Ok((s, items, law))
            })
            .await?;
        let mut all = Report::default();
        all.extend(table_report);
        all.extend(items);
        let ctx = StartContext {
            class,
            kind,
            table: Some(table_now),
            ni_devices,
            run_id: String::new(),
            revision: 0,
            devices: Vec::new(),
        };
        let parsed = Arc::new(parsed);
        let mut driver = self.new_driver(&ctx);
        let v = driver.configure(parsed.clone(), law, all);
        Ok((parsed, driver, v, ctx))
    }

    pub(super) async fn mimo_setup(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let setup = params
            .get("setup")
            .filter(|s| s.is_object())
            .ok_or_else(|| {
                MethodError::bad_params("setup", "Setting up MIMO Random needs a setup.")
            })?;
        let dry_run = protocol::param_bool(params, "dry_run")?.unwrap_or(false);
        let (parsed, driver, v, _) = self.mimo_check(setup).await?;
        let validation = v.to_json();
        if dry_run {
            return Ok(json!({ "validation": validation }));
        }
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        if inner.sim_running() {
            return Err(MethodError::named(
                ErrorCode::Refused,
                "run_active",
                "A simulation is running; stop it first.",
            ));
        }
        if !v.ok() {
            return Err(setup::setup_invalid(&validation, v.report.errors.len()));
        }
        inner.mimo_runs += 1;
        let id = format!("m-{:04}", inner.mimo_runs);
        let station = StationRef {
            kind: match inner.stations.selected() {
                Selection::Sim => "sim",
                Selection::Ni { .. } => "ni",
            },
            class: station::class_name(inner.stations.class()),
            devices: match inner.stations.selected() {
                Selection::Sim => Vec::new(),
                Selection::Ni { devices } => devices
                    .iter()
                    .map(|d| {
                        let sn = inner
                            .stations
                            .devices()
                            .iter()
                            .find(|x| &x.id == d)
                            .map(|x| x.serial.clone())
                            .unwrap_or_default();
                        (d.clone(), sn)
                    })
                    .collect(),
            },
        };
        let grid = Grid {
            fs_hz: parsed.estimation.fs_hz,
            n_pts: parsed.estimation.n_pts,
            df_hz: parsed.df_hz(),
            lines: parsed.lines_hz().len(),
            loop_s: parsed.loop_s(),
            actual: false,
        };
        let run = Arc::new(MimoRun::new(
            id.clone(),
            parsed,
            station,
            grid,
            validation.clone(),
            driver,
        ));
        inner.mimo = Some(run.clone());
        drop(inner);
        let (_, state) = run.status();
        self.emit("mimo_random.status", state.clone());
        self.emit(
            "mimo_random.validate",
            json!({"run_id": id, "validation": validation}),
        );
        Ok(json!({"state": state, "validation": validation}))
    }

    pub(super) async fn mimo_validate(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        match params.get("setup") {
            None | Some(Value::Null) => {
                let inner = self.inner.lock().await;
                Ok(json!({
                    "validation": inner.mimo.as_ref().map_or(Value::Null, |m| m.validation.clone())
                }))
            }
            Some(s) if s.is_object() => {
                let (_, _, v, _) = self.mimo_check(s).await?;
                Ok(json!({"validation": v.to_json()}))
            }
            Some(_) => Err(MethodError::bad_params(
                "setup",
                "\"setup\" is a MIMO setup, or absent for the present one's report.",
            )),
        }
    }

    async fn controlled_mimo(&self, conn: &Conn) -> Result<Arc<MimoRun>, MethodError> {
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.mimo.clone().ok_or_else(|| {
            MethodError::named(
                ErrorCode::Refused,
                "not_configured",
                "MIMO Random is not set up; send a setup first.",
            )
        })
    }

    pub(super) async fn mimo_start(self: &Arc<Self>, conn: &Conn) -> Result<Value, MethodError> {
        let (run, ctx) = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.mimo.clone().ok_or_else(|| {
                MethodError::named(
                    ErrorCode::Refused,
                    "not_configured",
                    "MIMO Random is not set up; send a setup first.",
                )
            })?;
            let state = run.with(|d| d.state());
            if state == State::Finished {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_finished",
                    "This MIMO run has finished; send the setup again to run.",
                ));
            }
            inner.run_active()?;
            if inner.sim_running() {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_active",
                    "A simulation is running; stop it first.",
                ));
            }
            let report = table::validate(&inner.table.table, &inner.ctx());
            if !report.errors.is_empty() {
                return Err(super::table_invalid(
                    &report,
                    "on this station, so the run cannot start",
                ));
            }
            let ctx = StartContext {
                class: station::class_name(inner.stations.class()),
                kind: match inner.stations.selected() {
                    Selection::Sim => "sim",
                    Selection::Ni { .. } => "ni",
                },
                table: Some(inner.table.table.clone()),
                ni_devices: ni_devices(inner.stations.selected()),
                run_id: run.id.clone(),
                revision: inner.table.revision,
                devices: inner.stations.devices().to_vec(),
            };
            (run, ctx)
        };
        run.with(|d| d.start(&ctx))?;
        self.spawn_mimo_watch(run.clone());
        Ok(run.status().1)
    }

    pub(super) async fn mimo_proceed(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_mimo(conn).await?;
        run.with(|d| d.proceed())?;
        Ok(run.status().1)
    }

    pub(super) async fn mimo_override(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let ack = match params.get("ack") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
            _ => {
                return Err(MethodError::bad_params(
                    "ack",
                    "An override is the operator's own words on why the run should engage; it cannot be empty.",
                ))
            }
        };
        let reason = match params.get("reason") {
            None | Some(Value::Null) => None,
            Some(Value::String(r)) => Some(OverrideReason::from_name(r).ok_or_else(|| {
                MethodError::bad_params(
                    "reason",
                    "\"reason\" is \"gate_failed\" or \"prediction_exceeded\": what the override acknowledges",
                )
            })?),
            Some(_) => {
                return Err(MethodError::bad_params(
                    "reason",
                    "\"reason\" is \"gate_failed\" or \"prediction_exceeded\": what the override acknowledges",
                ))
            }
        };
        let run = self.controlled_mimo(conn).await?;
        let by = {
            let inner = self.inner.lock().await;
            inner.session.client_name(conn.id, &conn.peer)
        };
        run.with(|d| d.override_gate(&ack, &by, reason))?;
        Ok(run.status().1)
    }

    /// Take controls out of control at a failed gate, kept as monitors, with the
    /// operator's reason.
    pub(super) async fn mimo_exclude(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let ack = match params.get("ack") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
            _ => {
                return Err(MethodError::bad_params(
                    "ack",
                    "Excluding a control is the operator's decision in their own words; the reason cannot be empty.",
                ))
            }
        };
        let positions: Vec<usize> = match params.get("controls").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => a
                .iter()
                .map(|v| v.as_u64().map(|x| x as usize))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    MethodError::bad_params(
                        "controls",
                        "\"controls\" lists control positions, counted from 0",
                    )
                })?,
            _ => {
                return Err(MethodError::bad_params(
                    "controls",
                    "\"controls\" lists the control positions to exclude, counted from 0",
                ))
            }
        };
        let run = self.controlled_mimo(conn).await?;
        let by = {
            let inner = self.inner.lock().await;
            inner.session.client_name(conn.id, &conn.peer)
        };
        run.with(|d| d.exclude_controls(&positions, &ack, &by))?;
        Ok(run.status().1)
    }

    pub(super) async fn mimo_iterate(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let run = self.controlled_mimo(conn).await?;
        let law = match params.get("law") {
            None | Some(Value::Null) => None,
            Some(Value::Object(m)) => {
                let id = m
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| MethodError::bad_params("law.id", "A law is named by its id."))?
                    .to_owned();
                let p = match m.get("params") {
                    None | Some(Value::Null) => Map::new(),
                    Some(Value::Object(p)) => p.clone(),
                    Some(_) => {
                        return Err(MethodError::bad_params(
                            "law.params",
                            "\"params\" is an object of parameter values.",
                        ))
                    }
                };
                let shape = run.setup.shape();
                let n = run.setup.controlled();
                let (lid, lp) = (id.clone(), p.clone());
                let (info, report) = self
                    .with_catalog(move |cat| {
                        let info = cat.entry(&lid).map(|e| laws::law_info(&e));
                        let mut r = Report::default();
                        setup::check_law(
                            &setup::LawSetup {
                                id: lid,
                                params: lp,
                            },
                            info.clone(),
                            shape,
                            n,
                            &mut r,
                        );
                        Ok((info, r))
                    })
                    .await?;
                if let Some(first) = report.errors.first() {
                    return Err(MethodError::bad_params("law", first.message.clone()));
                }
                let info = info.ok_or_else(|| MethodError::bad_params("law.id", "No such law."))?;
                let law: LawRef = laws::law_ref(&info);
                Some((law, p))
            }
            Some(_) => {
                return Err(MethodError::bad_params(
                    "law",
                    "\"law\" is {\"id\", \"params\"}, or absent for the run's own.",
                ))
            }
        };
        let p = run.with(|d| d.iterate_prediction(law))?;
        Ok(json!({"prediction": mview::prediction_json(&p)}))
    }

    /// `mimo_random.stop` and `mimo_random.abort`: open to every connection.
    pub(super) async fn mimo_end(&self, conn: &Conn, abort: bool) -> Result<Value, MethodError> {
        let (run, by) = {
            let inner = self.inner.lock().await;
            let Some(run) = inner.mimo.clone() else {
                return Ok(mview::status_json(None));
            };
            (run, inner.session.client_name(conn.id, &conn.peer))
        };
        if run.is_live() {
            if abort {
                run.with(|d| d.abort(&by));
            } else {
                run.with(|d| d.stop(StopOutcome::OperatorStop, &by));
            }
        }
        Ok(run.status().1)
    }

    pub(super) async fn mimo_status_now(&self) -> Value {
        let run = self.inner.lock().await.mimo.clone();
        match run {
            Some(r) => r.status().1,
            None => mview::status_json(None),
        }
    }

    pub(super) async fn mimo_report(&self) -> Value {
        let run = self.inner.lock().await.mimo.clone();
        json!({"report": run.and_then(|r| r.report()).unwrap_or(Value::Null)})
    }

    /// `mimo_random.reference.build_breakpoints`, `validate` and `import`: computed
    /// on a blocking thread (a 72-control reference is a few thousand
    /// eigendecompositions), open to every connection. A large reference is written
    /// into the data folder by its content hash.
    pub(super) async fn mimo_reference(
        &self,
        method: &str,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        use crate::mimo::reference;
        let root = self.opts.data_root.clone();
        let params = params.clone();
        let method = method.to_owned();
        tokio::task::spawn_blocking(move || match method.as_str() {
            "mimo_random.reference.build_breakpoints" => {
                reference::build_breakpoints(&params, &root)
            }
            "mimo_random.reference.validate" => reference::validate(&params, &root),
            _ => reference::import(&params, &root),
        })
        .await
        .map_err(|e| MethodError::internal(format!("the reference task failed: {e}")))?
    }

    /// `mimo_random.identification.save` and `.load`: controller only (they write
    /// files, or change what the next start does).
    pub(super) async fn mimo_identification(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
        load: bool,
    ) -> Result<Value, MethodError> {
        let path = protocol::named_str(params, "path")?
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| {
                MethodError::bad_params(
                    "path",
                    "Name the folder of the identification set with \"path\".",
                )
            })?
            .to_owned();
        let (run, table) = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.mimo.clone().ok_or_else(|| {
                MethodError::named(
                    ErrorCode::Refused,
                    "not_configured",
                    "MIMO Random is not set up; send a setup first.",
                )
            })?;
            (run, inner.table.table.clone())
        };
        let info = tokio::task::spawn_blocking(move || {
            let p = std::path::PathBuf::from(&path);
            if load {
                run.with(|d| d.load_identification(&p, &table))
            } else {
                run.with(|d| d.save_identification(&p))
            }
        })
        .await
        .map_err(|e| MethodError::internal(format!("the identification task failed: {e}")))??;
        Ok(json!({"identification": info.to_json()}))
    }

    pub(super) async fn laws_list(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let world = laws::parse_world(params)?;
        self.with_catalog(move |cat| {
            cat.rescan().map_err(|e| laws::catalog_unavailable(&e))?;
            Ok(laws::list_json(cat, world))
        })
        .await
    }

    pub(super) async fn laws_import(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let path = protocol::named_str(params, "path")?
            .ok_or_else(|| {
                MethodError::bad_params("path", "Importing a law needs the path of its file.")
            })?
            .to_owned();
        self.inner
            .lock()
            .await
            .session
            .require_controller(conn.id)?;
        self.with_catalog(move |cat| laws::import(cat, &path)).await
    }

    /// `laws.check`: run a multiple-output law on every simulated test article
    /// (`mimo::law_check`), one `laws.check` event per case as it starts and as it
    /// ends, and, for a law file, record the result against its hash.
    pub(super) async fn laws_check(
        self: &Arc<Self>,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        use crate::mimo::law_check;
        let id = protocol::named_str(params, "id")?.map(str::to_owned);
        let path = protocol::named_str(params, "path")?.map(str::to_owned);
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
        if id.is_some() == path.is_some() {
            return Err(MethodError::bad_params(
                "id",
                "Checking a law needs either its catalog \"id\" or the \"path\" of its file.",
            ));
        }
        {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            inner.run_active()?;
        }
        // The law, its parameters and a factory for each plant size, resolved under
        // the catalog's lock; the runs themselves hold no lock.
        let sizes = law_check::control_counts();
        let (entry, factories) = self
            .with_catalog(move |cat| {
                let id = match (id, path) {
                    (Some(id), _) => id,
                    (None, Some(p)) => laws::import(cat, &p)?["law"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    (None, None) => String::new(),
                };
                let entry = cat.entry(&id).ok_or_else(|| law_check::unknown_law(&id))?;
                if entry.world != hertzforge_random_laws::LawWorld::Mimo {
                    return Err(law_check::single_output_refusal(&id));
                }
                let choice = hertzforge_random_laws::LawChoice::new(
                    id.clone(),
                    crate::mimo::adapter::params_of(&entry.manifest.params, &given),
                );
                let mut factories = Vec::new();
                for n in sizes {
                    let f = cat
                        .resolve_mimo(&choice, n)
                        .map_err(|e| law_check::bad_choice(&id, &e.to_string()))?;
                    factories.push((n, f));
                }
                Ok((entry, factories))
            })
            .await?;
        let _slot = law_check::CheckSlot::take()?;
        let svc = self.clone();
        let law_id = entry.id.clone();
        let cases = tokio::task::spawn_blocking(move || {
            let law = |n: usize| {
                factories
                    .iter()
                    .find(|(k, _)| *k == n)
                    .map(|(_, f)| f.clone())
                    .ok_or_else(|| format!("no law was resolved for {n} controls"))
            };
            law_check::run_check(&law, &law_check::CheckOptions::default(), &mut |i, c, r| {
                svc.emit("laws.check", law_check::progress_json(&law_id, i, c, r));
            })
        })
        .await
        .map_err(|e| MethodError::internal(format!("the law check failed: {e}")))?;
        let law = if entry.builtin {
            laws::entry_json(&entry)
        } else {
            let sha = entry.identity.sha256.clone();
            let verdicts: Vec<_> = cases.iter().map(|c| c.verdict.clone()).collect();
            self.with_catalog(move |cat| {
                let record = hertzforge_random_laws::mimo_check_record(
                    sha,
                    law_check::now_rfc3339(),
                    verdicts,
                );
                cat.record_check(&record)
                    .map_err(|e| law_check::not_recorded(&e.to_string()))?;
                let e = cat
                    .entry(&record.law_sha256)
                    .ok_or_else(|| MethodError::internal("the checked law left the catalog"))?;
                Ok(laws::entry_json(&e))
            })
            .await?
        };
        Ok(json!({"law": law, "report": law_check::report_json(&cases, !entry.builtin)}))
    }

    /// Follow `run` until it has finished: `mimo_random.status` on every change, and
    /// the report once at the end.
    fn spawn_mimo_watch(self: &Arc<Self>, run: Arc<MimoRun>) {
        let svc = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(LIVE_TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut key = String::new();
            loop {
                tick.tick().await;
                let current = svc
                    .inner
                    .lock()
                    .await
                    .mimo
                    .as_ref()
                    .is_some_and(|m| Arc::ptr_eq(m, &run));
                // The driver and the frames are computation: off the socket's threads.
                let (svc2, run2) = (svc.clone(), run.clone());
                let Ok((s, state)) = tokio::task::spawn_blocking(move || {
                    let (s, state) = run2.status();
                    (s, state)
                })
                .await
                else {
                    break;
                };
                let k = mview::status_key(Some(&s));
                if k != key {
                    key = k;
                    svc.emit("mimo_random.status", state);
                }
                let last = s.state == State::Finished || !current;
                let s2 = s.clone();
                let run3 = run.clone();
                if tokio::task::spawn_blocking(move || svc2.mimo_data_due(&run3, &s2, last))
                    .await
                    .is_err()
                {
                    break;
                }
                if s.state == State::Finished {
                    if let Some(report) = run.report() {
                        svc.emit("mimo_random.report", json!({"report": report}));
                    }
                    break;
                }
                if !current {
                    break;
                }
            }
        });
    }

    /// `mimo_random.subscribe`: replace `conn`'s MIMO subscription, and send it the
    /// run's latest data at once when the run has streamed.
    pub(super) async fn mimo_subscribe(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let sub = display::parse(params)?.map(Arc::new);
        let result = display::result_json(sub.as_deref());
        match &sub {
            None => {
                self.mimo_displays().remove(&conn.id);
            }
            Some(sub) => {
                self.mimo_displays().insert(
                    conn.id,
                    MimoDisplay {
                        sub: sub.clone(),
                        last: None,
                    },
                );
                let run = self.inner.lock().await.mimo.clone();
                if let Some(run) = run {
                    let (s, _) = run.status();
                    if s.streaming {
                        self.emit_mimo_data(conn.id, &run, &s, sub);
                        if let Some(d) = self.mimo_displays().get_mut(&conn.id) {
                            d.last = Some(std::time::Instant::now());
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    /// Send each subscribed connection whose frame is due its `mimo_random.data`
    /// (every one of them when `last`).
    fn mimo_data_due(&self, run: &Arc<MimoRun>, s: &crate::mimo::driver::MimoStatus, last: bool) {
        if !s.streaming {
            return;
        }
        let now = std::time::Instant::now();
        let due: Vec<(u64, Arc<display::MimoSubscription>)> = {
            let mut displays = self.mimo_displays();
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
            self.emit_mimo_data(conn, run, s, &sub);
        }
    }

    /// One `mimo_random.data` frame of `run` for connection `conn`: only the
    /// subscribed plots, each computed by the driver for its own selector and
    /// decimated to its width.
    fn emit_mimo_data(
        &self,
        conn: u64,
        run: &Arc<MimoRun>,
        s: &crate::mimo::driver::MimoStatus,
        sub: &display::MimoSubscription,
    ) {
        let data = mimo_data_json(run, s, sub);
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (msg, arrays) = data;
        match codec::encode(&protocol::event("mimo_random.data", seq, msg), &arrays) {
            Ok(frame) => {
                let _ = self.events.send(Arc::new(Out::To(conn, Feed::Mimo, frame)));
            }
            Err(e) => eprintln!("sds-control: mimo_random.data frame not encoded: {e}"),
        }
    }
}

/// The NI devices a station selection names.
fn ni_devices(sel: &Selection) -> Vec<String> {
    match sel {
        Selection::Sim => Vec::new(),
        Selection::Ni { devices } => devices.clone(),
    }
}

/// The `mimo_random.data` message for `sub`, and its arrays.
pub fn mimo_data_json(
    run: &MimoRun,
    s: &crate::mimo::driver::MimoStatus,
    sub: &display::MimoSubscription,
) -> (Value, Arrays) {
    let display::SetupDisplay {
        labels,
        drive_labels,
        unit,
        bands,
    } = display::context(&run.setup);
    let ctx = display::PlotContext {
        labels: &labels,
        drive_labels: &drive_labels,
        unit: &unit,
        bands,
        df_hz: run.setup.df_hz(),
    };
    let (n, nd) = (labels.len(), drive_labels.len());
    let mut arrays = Arrays::new();
    let plots: Vec<Value> = sub
        .plots
        .iter()
        .map(|p| {
            let sel = display::resolve(p, n, nd);
            let data: Vec<_> = sel.iter().map(|q| run.with(|d| d.quantity(q))).collect();
            let views = display::plot_views(p, &ctx, s, &data, &mut arrays);
            display::plot_json(p, display::plot_labels(p, &ctx), views)
        })
        .collect();
    let msg = json!({
        "run_id": run.id,
        "source": run.station.source(),
        "state": s.state.name(),
        "phase": s.phase,
        "loop": s.loop_index,
        "elapsed_s": s.elapsed_s.map(|x| protocol::num(crate::random::config::round(x, 2))),
        "level_db": protocol::opt_num(s.level_db.map(|x| crate::random::config::round(x, 2))),
        "plots": plots,
    });
    (msg, arrays)
}
