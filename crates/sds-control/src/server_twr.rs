//! The TWR methods of the [`Service`] (schema/PROTOCOL.md, "Time Waveform
//! Replication"): the references, a configuration and its run beside the other
//! modules' slots, under the same run exclusion and the same controller rules, on
//! the transient-run rails.

use super::{Conn, Feed, Out, Service, DATA_SLACK, DISPLAY_PERIOD, LIVE_TICK};
use crate::codec::{self, Arrays};
use crate::laws;
use crate::protocol::{self, num, ErrorCode, MethodError};
use crate::pulse::rails::{Jobs, Shared};
use crate::shock::config::LawFacts;
use crate::station::{self, Selection, StationPlan};
use crate::table;
use crate::twr::config::{self, CheckContext, WireTwr};
use crate::twr::engine::{self, LevelMove};
use crate::twr::reference::{self, Stored};
use crate::twr::run::{self as trun, ArmContext, Cmd, RunPlan};
use crate::twr::subscribe::{self, DataContext, Subscription};
use crate::twr::view::{self, LawValueSpec, State, StationRef, TwrView};
use crate::twr::{RunData, TwrRun};
use hertzforge_random_laws::{may_arm_on_hardware, LawCatalog, LawWorld};
use hertzforge_twr::schedule::TwrRunMode;
use hertzforge_twr::TwrLawFactory;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One connection's TWR subscription and when it was last sent a frame.
pub struct TwrDisplay {
    sub: Arc<Subscription>,
    last: Option<Instant>,
}

/// How long to wait for the run thread to answer: its identification, two records
/// and a margin.
pub(super) fn wait_for(run: &TwrRun) -> Duration {
    let w = &run.wire;
    let fs = run.prepared.grid.fs_hz.max(1.0);
    let record_s = run.prepared.grid.n as f64 / fs;
    Duration::from_secs_f64(10.0 + w.sysid_duration_s.max(0.0) + 2.0 * record_s)
}

const ARM_WAIT: Duration = Duration::from_secs(120);

fn not_configured() -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "not_configured",
        "TWR is not configured; configure a test first.",
    )
}

fn law_specs(cat: &LawCatalog, id: &str) -> Vec<LawValueSpec> {
    cat.entry(id)
        .map(|e| {
            e.manifest
                .diagnostics
                .iter()
                .map(|d| LawValueSpec {
                    key: d.key.clone(),
                    label: d.label.clone(),
                    unit: d.unit.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Service {
    pub(super) fn twr_displays(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, TwrDisplay>> {
        match self.twr_displays.lock() {
            Ok(d) => d,
            Err(p) => p.into_inner(),
        }
    }

    fn twr_station_ref(inner: &super::Inner) -> StationRef {
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

    /// Parse and check `config` against the present table and the stored
    /// references: the configuration, its items, its derived values, its record,
    /// its stored reference and its law.
    #[allow(clippy::type_complexity)]
    async fn twr_check(
        &self,
        config: &Value,
    ) -> Result<
        (
            WireTwr,
            table::Report,
            Option<config::Derived>,
            Option<(Arc<Stored>, reference::Prepared)>,
            Option<LawFacts>,
            Vec<LawValueSpec>,
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
        let root = self.opts.data_root.clone();
        let id = w.law.id.clone();
        let (w, report, derived, prepared, facts, specs) = self
            .with_catalog(move |cat| {
                let lookup = |i: &str| laws::twr_lookup(Some(cat), i);
                let refs = |sha: &str| reference::load(&root, sha).map(Arc::new);
                let file = |s: &Stored, p: &str| reference::still_matches(s, p);
                let ctx = CheckContext {
                    table: &tbl,
                    law: &lookup,
                    reference: &refs,
                    file_check: &file,
                };
                let (report, derived, stored) = w.check(&ctx);
                let prepared = match (&derived, stored) {
                    (Some(_), Some(s)) => {
                        let fs = w.shape.fs_for(s.fs_hz);
                        let band = w.shape.band(w.sysid_band_hz);
                        reference::prepare(&s, &w.shape, fs, band)
                            .ok()
                            .map(|p| (s, p))
                    }
                    _ => None,
                };
                let facts = laws::twr_lookup(Some(cat), &id);
                let specs = law_specs(cat, &id);
                Ok((w, report, derived, prepared, facts, specs))
            })
            .await?;
        let mut all = table_report;
        all.extend(report);
        let derived = if all.errors.is_empty() { derived } else { None };
        Ok((w, all, derived, prepared, facts, specs))
    }

    pub(super) async fn twr_configure(
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
        let (w, report, derived, prepared, facts, specs) = self.twr_check(config).await?;
        if dry_run {
            let mut arrays = Arrays::new();
            let views = match (&derived, &prepared) {
                (Some(_), Some((_, p))) => {
                    Value::Array(p.views(subscribe::DEFAULT_POINTS, &mut arrays))
                }
                _ => json!([]),
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
        let (Some(derived), Some((stored, prepared)), Some(_)) = (derived, prepared, facts) else {
            return Err(config::config_invalid(&report));
        };
        let run = self.new_twr_run(&mut inner, w, &derived, stored, prepared, specs)?;
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
    fn new_twr_run(
        &self,
        inner: &mut super::Inner,
        w: WireTwr,
        derived: &config::Derived,
        stored: Arc<Stored>,
        prepared: reference::Prepared,
        specs: Vec<LawValueSpec>,
    ) -> Result<Arc<TwrRun>, MethodError> {
        inner.twr_runs += 1;
        let run_id = format!("twr-{:04}", inner.twr_runs);
        let station = Self::twr_station_ref(inner);
        let units = stored.units.clone();
        let initial = view::configured_view(&run_id, &w, derived, station, &units);
        let shared = Arc::new(Shared::new(initial.clone(), self.twr_notify.clone()));
        let data = Arc::new(RunData::new(specs));
        let raw = w.raw.clone();
        let wire = Arc::new(w);
        let plan = RunPlan {
            run_id: run_id.clone(),
            cfg: wire.clone(),
            engine: self
                .opts
                .twr_engine
                .clone()
                .unwrap_or_else(engine::service_engine_factory),
            sim_speed: self.opts.sim_speed,
            timeout_s: self.opts.timeout.as_secs_f64(),
            derived: derived.to_json(),
            record: self.opts.record.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        trun::spawn(plan, initial.clone(), shared.clone(), data.clone(), rx)
            .map_err(|e| MethodError::internal(format!("could not start the run thread: {e}")))?;
        let run = Arc::new(TwrRun {
            id: run_id.clone(),
            shared: shared.clone(),
            data,
            config: raw,
            derived: derived.to_json(),
            prepared: Arc::new(prepared),
            wire: wire.clone(),
            stored,
            jobs: Jobs::new(&run_id, tx),
        });
        inner.twr = Some(run.clone());
        inner.twr_rows_sent = 0;
        inner.twr_ident_sent = false;
        shared.add_log(
            "state",
            format!("configured \"{}\" on {}", wire.title, initial.station.class),
        );
        let _ = self.twr_notify.send((run_id, Arc::new(initial)));
        Ok(run)
    }

    pub(super) async fn twr_config(&self) -> (Value, Arrays) {
        let inner = self.inner.lock().await;
        let mut arrays = Arrays::new();
        let result = match &inner.twr {
            None => json!({"run_id": null, "config": null, "derived": null, "views": []}),
            Some(run) => json!({
                "run_id": run.id,
                "config": run.config,
                "derived": run.derived,
                "views": run.prepared.views(subscribe::DEFAULT_POINTS, &mut arrays),
            }),
        };
        (result, arrays)
    }

    pub(super) async fn twr_reference_import(
        &self,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        let req = reference::parse_import(params)?;
        let root = self.opts.data_root.clone();
        let stored = tokio::task::spawn_blocking(move || reference::import(&root, &req))
            .await
            .map_err(|e| MethodError::internal(format!("the import failed: {e}")))??;
        let mut arrays = Arrays::new();
        let result = reference::import_json(&stored, &mut arrays);
        Ok((result, arrays))
    }

    pub(super) async fn twr_reference_preview(
        &self,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        let shape = config::parse_preview(params)?;
        let root = self.opts.data_root.clone();
        let out = tokio::task::spawn_blocking(move || {
            let mut r = table::Report::default();
            let refs = |sha: &str| reference::load(&root, sha).map(Arc::new);
            let file = |s: &Stored, p: &str| reference::still_matches(s, p);
            let sysid = hertzforge_twr::SysIdConfig::default();
            let got = shape.check(&mut r, &refs, &file, (sysid.f_lo_hz, sysid.f_hi_hz));
            match got {
                Some((_, prepared, derived)) if r.errors.is_empty() => {
                    let mut arrays = Arrays::new();
                    let views = prepared.views(subscribe::DEFAULT_POINTS, &mut arrays);
                    Ok((
                        json!({"derived": derived.to_json(), "views": views}),
                        arrays,
                    ))
                }
                _ => Err(config::preview_invalid(&r)),
            }
        })
        .await
        .map_err(|e| MethodError::internal(format!("the preview failed: {e}")))??;
        Ok(out)
    }

    async fn controlled_twr(&self, conn: &Conn) -> Result<Arc<TwrRun>, MethodError> {
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.twr.clone().ok_or_else(not_configured)
    }

    /// What a run arms on now: the table, the selected station and its law.
    async fn twr_arm_context(&self, run: &TwrRun) -> Result<ArmContext, MethodError> {
        let (tbl, station_plan, hardware, revision, devices) = {
            let inner = self.inner.lock().await;
            let report = table::validate(&inner.table.table, &inner.ctx());
            if !report.errors.is_empty() {
                return Err(super::table_invalid(
                    &report,
                    "on this station, so the run cannot arm",
                ));
            }
            let fs_hz = run.prepared.grid.fs_hz;
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
        let (facts, law) = self
            .with_catalog(move |cat| {
                if let Some(entry) = cat.entry(&id) {
                    if hardware {
                        match may_arm_on_hardware(&entry) {
                            Ok(()) => {}
                            // Passed, but at a check the current one has replaced.
                            Err(hertzforge_random_laws::ArmRefusal::CheckOutdated {
                                tool_version,
                                current,
                            }) => {
                                return Err(MethodError::named(
                                    ErrorCode::Refused,
                                    "law_not_checked",
                                    format!(
                                        "The control law \"{}\" passed {tool_version}, which \
                                         {current} has replaced; run laws.check on it again before \
                                         it drives hardware.",
                                        entry.identity.name
                                    ),
                                )
                                .with_detail(json!({
                                    "tool_version": tool_version,
                                    "current": current,
                                })));
                            }
                            Err(_) => {
                                return Err(MethodError::named(
                                    ErrorCode::Refused,
                                    "law_not_checked",
                                    format!(
                                        "The control law \"{}\" has no passed check for this \
                                         file; run laws.check on it before it drives hardware.",
                                        entry.identity.name
                                    ),
                                ));
                            }
                        }
                    }
                }
                let lf = laws::twr_lookup(Some(cat), &id).ok_or_else(|| {
                    MethodError::named(
                        ErrorCode::NotFound,
                        "law_unknown",
                        format!("No control law \"{id}\" is in the catalog."),
                    )
                })?;
                let choice = wire.law_choice(&lf);
                let law: Result<Arc<dyn TwrLawFactory>, MethodError> =
                    hertzforge_twr_laws::resolve_twr(cat, &choice).map_err(|e| {
                        MethodError::named(
                            ErrorCode::Refused,
                            "arm_refused",
                            format!("The control law could not be built: {e}."),
                        )
                    });
                Ok((lf, law))
            })
            .await?;
        Ok(ArmContext {
            table: tbl,
            station: station_plan,
            scanner: self.opts.scanner.clone(),
            data_root: self.opts.data_root.clone(),
            records_root: self.opts.records_root(),
            bind: self.opts.bind.clone(),
            stored: run.stored.clone(),
            facts,
            law,
            revision,
            devices,
        })
    }

    pub(super) async fn twr_arm(
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
            let run = inner.twr.clone().ok_or_else(not_configured)?;
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
                || inner.srs_live()
                || inner.sine_live()
                || inner.mixed_live()
                || inner.explorer_live()
                || inner.recorder_live()
                || inner.mimo_shock_live()
            {
                inner.run_active()?;
            }
            if v.state.is_busy() || v.state == State::Held || (!start && v.state == State::Armed) {
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
        let ctx = self.twr_arm_context(&run).await?;
        if start {
            run.jobs
                .ask(|reply| Cmd::Start { ctx, reply }, ARM_WAIT)
                .await?;
        } else {
            run.jobs
                .ask(|reply| Cmd::Arm { ack, ctx, reply }, ARM_WAIT)
                .await?;
        }
        self.spawn_twr_live(run.clone());
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn twr_proceed(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_twr(conn).await?;
        run.jobs
            .ask(|reply| Cmd::Proceed { reply }, wait_for(&run))
            .await?;
        Ok(view::state_json(Some(&run.view())))
    }

    /// The refusals a verb gets without asking the run thread: the automatic
    /// ladder, and a play in flight.
    fn pre_verb(run: &TwrRun, verb: &str, level: bool) -> Result<(), MethodError> {
        let v = run.view();
        // In `auto` only a level verb, and only at the hold a failed gate leaves.
        if run.wire.mode == TwrRunMode::Auto && !(level && v.phase == "holding") {
            return Err(trun::wrong_mode(verb));
        }
        if run.shared.progress.now().is_some() || matches!(v.phase, "iterating" | "playing") {
            return Err(trun::wrong_state(&v, verb));
        }
        Ok(())
    }

    pub(super) async fn twr_iterate(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_twr(conn).await?;
        Self::pre_verb(&run, "An iteration", false)?;
        run.jobs
            .ask(|reply| Cmd::Iterate { reply }, wait_for(&run))
            .await?;
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn twr_play(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_twr(conn).await?;
        Self::pre_verb(&run, "Playback", false)?;
        run.jobs
            .ask(|reply| Cmd::Play { reply }, wait_for(&run))
            .await?;
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn twr_level(
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
        let run = self.controlled_twr(conn).await?;
        Self::pre_verb(&run, "A level change", true)?;
        let level = run
            .jobs
            .ask(|reply| Cmd::Level { to, reply }, wait_for(&run))
            .await?;
        Ok(json!({"level_db": num(crate::shock::config::round(level, 2))}))
    }

    pub(super) async fn twr_freeze(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let frozen = protocol::param_bool(params, "frozen")?
            .ok_or_else(|| MethodError::bad_params("frozen", "\"frozen\" is true or false"))?;
        let run = self.controlled_twr(conn).await?;
        let v = run.view();
        if !matches!(v.state, State::Running | State::Review) {
            return Err(trun::wrong_state(&v, "Freezing the law"));
        }
        run.jobs.send(Cmd::Freeze { frozen })?;
        Ok(json!({"frozen": frozen}))
    }

    /// `twr.stop` and `twr.abort`: open to every connection.
    pub(super) async fn twr_end(&self, conn: &Conn, abort: bool) -> Result<Value, MethodError> {
        let (run, by) = {
            let inner = self.inner.lock().await;
            let Some(run) = inner.twr.clone() else {
                return Ok(view::state_json(None));
            };
            (run, inner.session.client_name(conn.id, &conn.peer))
        };
        if !run.view().state.is_live() {
            return Ok(view::state_json(Some(&run.view())));
        }
        stop_twr(
            &run,
            if abort { None } else { Some("operator_stop") },
            by,
            true,
        )
        .await;
        Ok(view::state_json(Some(&run.view())))
    }

    /// `twr.reset`: configure the present run's configuration again. From the
    /// coherence hold it ends the held run first.
    pub(super) async fn twr_reset(&self, conn: &Conn) -> Result<Value, MethodError> {
        let (run, raw) = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.twr.clone().ok_or_else(not_configured)?;
            let raw = run.config.clone();
            (run, raw)
        };
        let v = run.view();
        if v.state == State::Held {
            let by = {
                let inner = self.inner.lock().await;
                inner.session.client_name(conn.id, &conn.peer)
            };
            run.shared.add_log(
                "note",
                format!("{by} reset the run from the coherence hold"),
            );
            stop_twr(&run, Some("operator_stop"), by, true).await;
        } else if v.state.is_live() {
            self.inner.lock().await.run_active()?;
        }
        let (w, report, derived, prepared, facts, specs) = self.twr_check(&raw).await?;
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        let (Some(derived), Some((stored, prepared)), Some(_)) = (derived, prepared, facts) else {
            return Err(config::config_invalid(&report));
        };
        let run = self.new_twr_run(&mut inner, w, &derived, stored, prepared, specs)?;
        let v = run.view();
        Ok(view::state_json(Some(&v)))
    }

    pub(super) async fn twr_clear(&self, conn: &Conn) -> Result<Value, MethodError> {
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        if inner.twr_live() {
            inner.run_active()?;
        }
        inner.twr = None;
        let state = view::state_json(None);
        inner.twr_state_key = view::state_key(None);
        self.emit("twr.state", state.clone());
        Ok(state)
    }

    pub(super) async fn twr_state_now(&self) -> Value {
        let inner = self.inner.lock().await;
        let v = inner.twr.as_ref().map(|r| r.view());
        view::state_json(v.as_deref())
    }

    pub(super) async fn twr_log(&self, params: &Map<String, Value>) -> Result<Value, MethodError> {
        let since = since_param(params)?;
        let inner = self.inner.lock().await;
        let (entries, next) = match &inner.twr {
            Some(r) => r.log_since(since),
            None => (Vec::new(), 0),
        };
        Ok(json!({
            "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "next": next,
        }))
    }

    pub(super) async fn twr_plays(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let since = since_param(params)?;
        let run = self.inner.lock().await.twr.clone();
        let (rows, next) = match &run {
            Some(r) => r.data.rows_since(since),
            None => (Vec::new(), 0),
        };
        Ok(json!({"plays": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(), "next": next}))
    }

    pub(super) async fn twr_record(
        &self,
        params: &Map<String, Value>,
    ) -> Result<(Value, Arrays), MethodError> {
        let index = params
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| MethodError::bad_params("index", "\"index\" is a play, from 0"))?;
        let points = subscribe::parse_record_points(params.get("points"))?;
        let run = self
            .inner
            .lock()
            .await
            .twr
            .clone()
            .ok_or_else(not_configured)?;
        let row = run.data.row(index).ok_or_else(|| {
            MethodError::not_found(format!(
                "No play {index} has been judged in run {} ({} so far).",
                run.id,
                run.data.row_count()
            ))
        })?;
        let rec = run.data.records(index).ok_or_else(|| {
            MethodError::not_found(format!(
                "The records of play {index} are no longer held; the last {} plays' are. Its \
                 row is in twr.plays.",
                crate::twr::RECORDS_HELD
            ))
        })?;
        let units = run.stored.units.clone();
        let mut arrays = Arrays::new();
        let views = vec![
            view::response_view("Replication", &rec, &units, points, &mut arrays),
            view::error_view(&rec, &units, points, &mut arrays),
            view::drive_view(&rec, points, &mut arrays),
        ];
        Ok((json!({"play": row.to_json(), "views": views}), arrays))
    }

    /// `twr.subscribe`: replace `conn`'s subscription, and send it the run's
    /// latest data at once when the run has streamed.
    pub(super) async fn twr_subscribe(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let sub = subscribe::parse(params)?.map(Arc::new);
        let result = subscribe::result_json(sub.as_deref());
        match &sub {
            None => {
                self.twr_displays().remove(&conn.id);
            }
            Some(sub) => {
                self.twr_displays().insert(
                    conn.id,
                    TwrDisplay {
                        sub: sub.clone(),
                        last: None,
                    },
                );
                let run = self.inner.lock().await.twr.clone();
                if let Some(run) = run.filter(|r| r.view().streaming) {
                    self.emit_twr_data(conn.id, &run, sub);
                    if let Some(d) = self.twr_displays().get_mut(&conn.id) {
                        d.last = Some(Instant::now());
                    }
                }
            }
        }
        Ok(result)
    }

    fn emit_twr_data(&self, conn: u64, run: &TwrRun, sub: &Subscription) {
        let v = run.view();
        let last = run.data.last_records();
        let target = run.data.target();
        let ident = run.data.identification();
        let mask = run.data.mask();
        let (rows, _) = run.data.rows_since(0);
        let scope = if sub.has_scope() {
            run.scope()
        } else {
            view::Scope::default()
        };
        let ctx = DataContext {
            view: &v,
            last: last.as_deref(),
            target: target.as_deref(),
            identification: ident.as_deref(),
            mask: mask.as_deref(),
            rows: &rows,
            law_specs: &run.data.law_specs,
            scope: &scope,
        };
        let mut arrays = Arrays::new();
        let data = subscribe::data_json(&ctx, sub, &mut arrays);
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match codec::encode(&protocol::event("twr.data", seq, data), &arrays) {
            Ok(frame) => {
                let _ = self.events.send(Arc::new(Out::To(conn, Feed::Twr, frame)));
            }
            Err(e) => eprintln!("sds-control: twr.data frame not encoded: {e}"),
        }
    }

    /// Each subscribed connection whose frame is due (every one when `force`).
    fn twr_data_due(&self, run: &TwrRun, force: bool) {
        let now = Instant::now();
        let due: Vec<(u64, Arc<Subscription>)> = {
            let mut displays = self.twr_displays();
            displays
                .iter_mut()
                .filter(|(_, d)| {
                    force
                        || d.last
                            .is_none_or(|t| now + DATA_SLACK >= t + d.sub.period())
                })
                .map(|(id, d)| {
                    d.last = Some(now);
                    (*id, d.sub.clone())
                })
                .collect()
        };
        for (conn, sub) in due {
            self.emit_twr_data(conn, run, &sub);
        }
    }

    /// A run published `v`: `twr.state` when what it shows changed, its new log
    /// entries, and `twr.data` when there is something new to plot.
    pub(super) async fn twr_changed(&self, run_id: &str, v: &TwrView) {
        let mut inner = self.inner.lock().await;
        let Some(run) = inner.twr.clone() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        // The view as published, so every judged play has its own state event
        // even when publishes arrive faster than they are sent.
        let shown = run.as_of_now(Arc::new(v.clone()));
        let key = view::state_key(Some(&shown));
        if key != inner.twr_state_key {
            inner.twr_state_key = key;
            self.emit("twr.state", view::state_json(Some(&shown)));
        }
        let since = if inner.twr_log_sent.0 == run.id {
            inner.twr_log_sent.1
        } else {
            0
        };
        let (entries, next) = run.log_since(since);
        inner.twr_log_sent = (run.id.clone(), next);
        if !entries.is_empty() {
            self.emit(
                "twr.log",
                json!({
                    "run_id": run.id,
                    "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
                }),
            );
        }
        let rows_now = run.data.row_count();
        let first_new = inner.twr_rows_sent;
        inner.twr_rows_sent = rows_now;
        let ident_new = v.identification.is_some() && !inner.twr_ident_sent;
        inner.twr_ident_sent = v.identification.is_some();
        drop(inner);
        let finished = v.state == State::Finished;
        if rows_now > first_new || ident_new || finished {
            self.twr_data_due(&run, true);
        }
    }

    /// Publish `run`'s `twr.live` five times a second until it has finished, then
    /// once more with the final state, to the connections without a subscription of
    /// their own; and `twr.data` to the subscribed ones at their rate while a play is
    /// in flight. One task per run.
    fn spawn_twr_live(self: &Arc<Self>, run: Arc<TwrRun>) {
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
                    .twr
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
                    let scope = run.scope();
                    let mut arrays = Arrays::new();
                    let data = view::live_json(&v, &scope, &mut arrays);
                    let seq = svc.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // For the connections without a TWR subscription of their own; a
                    // subscribed one has its `twr.data` instead.
                    match codec::encode(&protocol::event("twr.live", seq, data), &arrays) {
                        Ok(frame) => {
                            let _ = svc
                                .events
                                .send(Arc::new(Out::DefaultLive(Feed::Twr, frame)));
                        }
                        Err(e) => eprintln!("sds-control: twr.live frame not encoded: {e}"),
                    }
                }
                // While a play is in flight a subscribed screen gets its frame at its
                // rate, with the play's progress, whatever its plots show.
                if v.play.is_some() && current {
                    svc.twr_data_due(&run, false);
                }
                if last {
                    break;
                }
            }
        });
    }
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
/// run's signals first so a play in flight ends at its next block, then queued.
pub(super) async fn stop_twr(run: &TwrRun, outcome: Option<&'static str>, by: String, wait: bool) {
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
    /// The catalog id of the TWR law `laws.check` names, by its `id` or the `path`
    /// of its file (imported first, for the controller only), or `None` when it
    /// names no TWR law.
    pub(super) async fn twr_law_id(
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
            if id == laws::TWR_BUILTIN_ID {
                return Ok(Some(id));
            }
            Ok(cat
                .entry(&id)
                .filter(|e| e.world == LawWorld::Twr)
                .map(|e| e.id))
        })
        .await
        .ok()
        .flatten()
    }

    /// `laws.check` of a TWR law: every case of the TWR check on its simulated
    /// station, one `laws.check` event as each starts and as it ends.
    pub(super) async fn twr_laws_check(
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
                let facts = laws::twr_lookup(Some(cat), &law_id)
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
                let factory = hertzforge_twr_laws::resolve_twr(cat, &choice).map_err(|e| {
                    MethodError::named(
                        ErrorCode::Refused,
                        "law_check_unsupported",
                        format!("The TWR check cannot run \"{law_id}\": {e}."),
                    )
                })?;
                Ok((facts, factory))
            })
            .await?;
        let _slot = CheckSlot::take()?;
        let svc = self.clone();
        let check_id = facts.id.clone();
        let results = tokio::task::spawn_blocking(move || {
            crate::twr::law_check::run_check(&check_id, factory, &mut |event| {
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
                        .ok_or_else(|| MethodError::internal("the built-in law is not listed"))
                })
                .await?;
            (law, false)
        } else {
            let sha = facts.sha256.clone();
            let verdicts: Vec<_> = results.into_iter().map(|(_, v)| v).collect();
            let law = self
                .with_catalog(move |cat| {
                    let record = crate::twr::law_check::check_record(sha, now_rfc3339(), verdicts);
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
            "report": crate::twr::law_check::report_json(&cases, passed, recorded),
        }))
    }
}
