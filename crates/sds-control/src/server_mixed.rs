//! The Mixed Mode methods of the [`Service`] (schema/PROTOCOL.md, "Mixed Mode"): a
//! configuration and its run beside the other test types, under the same run
//! exclusion and the same controller rules.

use super::{Conn, Feed, Out, Service, DATA_SLACK, DISPLAY_PERIOD, LIVE_TICK};
use crate::codec::{self, Arrays};
use crate::laws;
use crate::mixed::config::{self, CheckContext, Derived, LawFacts, WireBody, WireMixed};
use crate::mixed::run::{self as mrun, ArmContext, Cmd, RunPlan, RunShared};
use crate::mixed::subscribe::{self, Subscription};
use crate::mixed::view::{self, LawState, RoleState, State, StationRef};
use crate::mixed::{refuse, wrong_state, MixedRun};
use crate::protocol::{self, num, MethodError};
use crate::random::run::LevelTarget;
use crate::station::{self, Selection, StationPlan};
use crate::table;
use crate::views::{self, Axis, Role, Scale, Series};
use hertzforge_mixed::{MixedConfig, PartBody, ToneAmplitude};
use hertzforge_random::RunMode;
use hertzforge_random_laws::LawWorld;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One connection's Mixed Mode subscription and when it was last sent a frame.
pub struct MixedDisplay {
    sub: Arc<Subscription>,
    last: Option<Instant>,
}

fn not_configured() -> MethodError {
    refuse(
        "not_configured",
        "Mixed Mode is not configured; configure a test first.",
    )
}

type Checked = (
    WireMixed,
    table::Report,
    Option<Derived>,
    Option<LawFacts>,
    Option<MixedConfig>,
);

impl Service {
    pub(super) fn mixed_displays(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, MixedDisplay>> {
        match self.mixed_displays.lock() {
            Ok(d) => d,
            Err(p) => p.into_inner(),
        }
    }

    fn mixed_station_ref(inner: &super::Inner) -> StationRef {
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
    /// items, its derived values, its laws and the controller's configuration.
    async fn mixed_check(&self, config: &Value) -> Result<Checked, MethodError> {
        let w = config::parse(config)?;
        let (tbl, table_report) = {
            let inner = self.inner.lock().await;
            (
                inner.table.table.clone(),
                table::validate(&inner.table.table, &inner.ctx()),
            )
        };
        let (w, report, derived) = self
            .with_catalog(move |cat| {
                let floor = |i: &str| laws::mixed_lookup(Some(cat), i, LawWorld::Single);
                let tones = |i: &str| laws::mixed_lookup(Some(cat), i, LawWorld::Sine);
                let ctx = CheckContext {
                    table: &tbl,
                    floor_law: &floor,
                    tone_law: &tones,
                };
                let (report, derived) = w.check(&ctx);
                Ok((w, report, derived))
            })
            .await?;
        let mut all = table_report;
        all.extend(report);
        let derived = if all.errors.is_empty() { derived } else { None };
        let core = match &derived {
            Some(_) => {
                let tbl = self.inner.lock().await.table.table.clone();
                w.to_core(&tbl).ok()
            }
            None => None,
        };
        let law = derived.as_ref().map(|d| d.law.clone());
        Ok((w, all, derived, law, core))
    }

    /// The configuration's views: the reference at the start, and the tones.
    fn mixed_views(core: &MixedConfig, d: &impl AsViewFacts, arrays: &mut Arrays) -> Value {
        Value::Array(vec![
            reference_view(core, d, arrays),
            tones_view(core, d, arrays),
        ])
    }

    pub(super) async fn mixed_configure(
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
        let (w, report, derived, facts, core) = self.mixed_check(config).await?;
        if dry_run {
            let mut arrays = Arrays::new();
            let views = match (&derived, &core) {
                (Some(d), Some(c)) => Self::mixed_views(c, d, &mut arrays),
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
            return Err(refuse(
                "run_active",
                "A simulation is running; stop it first.",
            ));
        }
        let (Some(derived), Some(facts), Some(core)) = (derived, facts, core) else {
            return Err(config::config_invalid(&report));
        };
        let run = self.new_mixed_run(&mut inner, w, &derived, facts, core)?;
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

    /// Create a run (replacing the one before).
    fn new_mixed_run(
        &self,
        inner: &mut super::Inner,
        w: WireMixed,
        derived: &Derived,
        facts: LawFacts,
        core: MixedConfig,
    ) -> Result<Arc<MixedRun>, MethodError> {
        inner.mixed_runs += 1;
        let run_id = format!("mixed-{:04}", inner.mixed_runs);
        let station = Self::mixed_station_ref(inner);
        let live = |r: &config::RoleLaw, params: &Map<String, Value>| -> Vec<(String, f64)> {
            r.live
                .iter()
                .map(|k| {
                    let v = params.get(k).and_then(Value::as_f64).or_else(|| {
                        r.params
                            .iter()
                            .find(|p| &p.key == k)
                            .and_then(|p| match &p.kind {
                                hertzforge_law_host::ParamKind::Number(s) => Some(s.default),
                                _ => None,
                            })
                    });
                    (k.clone(), v.unwrap_or(0.0))
                })
                .collect()
        };
        let law = LawState {
            floor: RoleState {
                law: facts.floor.clone(),
                misses: 0,
                capped: 0,
                live: Vec::new(),
            },
            tones: RoleState {
                law: facts.tones.clone(),
                misses: 0,
                capped: 0,
                live: live(&facts.tones, &w.law_tones.params),
            },
            floor_values: Vec::new(),
            tone_values: Vec::new(),
            built: false,
        };
        let reference_rms_g = derived_total_rms(derived);
        let initial = view::configured_view(
            &run_id,
            &w,
            derived.grid,
            reference_rms_g,
            station,
            Some(law),
            Some(derived.feasibility.clone()),
        );
        let raw = w.raw.clone();
        let title = w.title.clone();
        let shared = Arc::new(RunShared::new(initial.clone(), self.mixed_notify.clone()));
        let wire = Arc::new(w);
        let core = Arc::new(core);
        let plan = RunPlan {
            run_id: run_id.clone(),
            wire: wire.clone(),
            core: core.clone(),
            law: facts.clone(),
            sim_speed: self.opts.sim_speed,
            capture_root: Some(self.opts.records_root()),
            timeout_s: self.opts.timeout.as_secs_f64(),
            record: self.opts.record.clone(),
            derived: derived.to_json(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        mrun::spawn(plan, initial.clone(), shared.clone(), rx)
            .map_err(|e| MethodError::internal(format!("could not start the run thread: {e}")))?;
        let run = Arc::new(MixedRun::new(
            run_id.clone(),
            raw,
            derived.to_json(),
            wire,
            core,
            facts,
            shared,
            tx,
        ));
        inner.mixed = Some(run.clone());
        run.add_log(
            "state",
            format!("configured \"{title}\" on {}", initial.station.class),
        );
        let _ = self.mixed_notify.send((run_id, Arc::new(initial)));
        Ok(run)
    }

    pub(super) async fn mixed_config(&self) -> (Value, Arrays) {
        let inner = self.inner.lock().await;
        let mut arrays = Arrays::new();
        let result = match &inner.mixed {
            None => json!({"run_id": null, "config": null, "derived": null, "views": []}),
            Some(run) => {
                let views = match derived_of(&run.derived) {
                    Some(d) => Self::mixed_views(&run.core, &d, &mut arrays),
                    None => json!([]),
                };
                json!({
                    "run_id": run.id,
                    "config": run.config,
                    "derived": run.derived,
                    "views": views,
                })
            }
        };
        (result, arrays)
    }

    async fn controlled_mixed(&self, conn: &Conn) -> Result<Arc<MixedRun>, MethodError> {
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.mixed.clone().ok_or_else(not_configured)
    }

    /// The hardware arming gate of each role (design D15): on a hardware station
    /// an imported law needs a passed check in its own world and a passed mixed
    /// check in its role. The simulator takes any law.
    pub(super) fn mixed_law_gate(facts: &LawFacts, hardware: bool) -> Result<(), MethodError> {
        if !hardware {
            return Ok(());
        }
        for (role, r) in [("floor", &facts.floor), ("tones", &facts.tones)] {
            if !r.hardware {
                return Err(config::law_not_checked(
                    role,
                    &r.name,
                    r.not_cleared.as_deref(),
                ));
            }
        }
        Ok(())
    }

    /// `mixed.arm` and `mixed.start`: the run's gates, the law gate, the laws
    /// resolved, then the controller arms on the selected station.
    pub(super) async fn mixed_arm(
        self: &Arc<Self>,
        conn: &Conn,
        params: &Map<String, Value>,
        start: bool,
    ) -> Result<Value, MethodError> {
        if !start {
            match params.get("override_ack") {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if !s.trim().is_empty() => {}
                Some(_) => {
                    return Err(MethodError::bad_params(
                        "override_ack",
                        "An override acknowledgement is the operator's own words; it cannot be \
                         empty.",
                    ))
                }
            }
        }
        let (run, plan, hardware, tbl, revision, devices) = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.mixed.clone().ok_or_else(not_configured)?;
            let v = run.view();
            if v.state == State::Finished {
                return Err(refuse(
                    "run_finished",
                    "This run has finished; reset or configure again to run.",
                ));
            }
            inner.run_active()?;
            if inner.sim_running() {
                return Err(refuse(
                    "run_active",
                    "A simulation is running; stop it first.",
                ));
            }
            if !(v.state == State::Configured || (v.state == State::Blocked && !start)) {
                return Err(wrong_state(&v, if start { "Starting" } else { "Arming" }));
            }
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
                run,
                plan,
                hardware,
                inner.table.table.clone(),
                inner.table.revision,
                inner.stations.devices().to_vec(),
            )
        };
        Self::mixed_law_gate(&run.law, hardware)?;
        let choice = run.core.law.clone();
        let channels = run.core.control.channels.len();
        let laws = self
            .with_catalog(move |cat| {
                Ok(
                    hertzforge_mixed_laws::resolve_mixed(cat, &choice, channels).map_err(|e| {
                        refuse(
                            "arm_refused",
                            format!("The control law could not be built: {e}."),
                        )
                        .with_detail(json!({"role": match e.role {
                            hertzforge_mixed::LawRole::Floor => "floor",
                            hertzforge_mixed::LawRole::Tones => "tones",
                        }}))
                    }),
                )
            })
            .await?;
        let ctx = ArmContext {
            table: tbl,
            station: plan,
            scanner: self.opts.scanner.clone(),
            data_root: self.opts.data_root.clone(),
            bind: self.opts.bind.clone(),
            laws,
            revision,
            devices,
        };
        if start {
            run.ask(|reply| Cmd::Start { ctx, reply }, ARM_WAIT).await?;
        } else {
            let ack = params
                .get("override_ack")
                .and_then(Value::as_str)
                .map(str::to_owned);
            run.ask(|reply| Cmd::Arm { ack, ctx, reply }, ARM_WAIT)
                .await?;
        }
        self.spawn_mixed_live(run.clone());
        Ok(view::state_json(Some(&run.view())))
    }

    /// The verbs that act on a running test: refused by state until it runs.
    pub(super) async fn mixed_running_verb(
        &self,
        conn: &Conn,
        method: &str,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        // The parameters are checked before the state, as Sine's are.
        match method {
            "mixed.level" => {
                let delta = protocol::named_f64(params, "delta_db")?;
                let to = protocol::named_f64(params, "to_db")?;
                if delta.is_some() == to.is_some() {
                    return Err(MethodError::bad_params(
                        "delta_db",
                        "A level change is a step (\"delta_db\") or a level of the schedule \
                         (\"to_db\"), one of the two.",
                    ));
                }
            }
            "mixed.mode" => match protocol::named_str(params, "mode")? {
                Some("auto") | Some("manual") => {}
                _ => {
                    return Err(MethodError::bad_params(
                        "mode",
                        "\"mode\" is \"auto\" or \"manual\"",
                    ))
                }
            },
            "mixed.part" => {
                protocol::named_str(params, "id")?
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        MethodError::bad_params("id", "Switching a part names it by \"id\".")
                    })?;
                if !matches!(params.get("on"), Some(Value::Bool(_))) {
                    return Err(MethodError::bad_params(
                        "on",
                        "\"on\" is true (switch the part on) or false (switch it off).",
                    ));
                }
            }
            "mixed.adjust" => {
                protocol::named_str(params, "key")?
                    .filter(|k| !k.is_empty())
                    .ok_or_else(|| {
                        MethodError::bad_params(
                            "key",
                            "An adjustment names the parameter's \"key\".",
                        )
                    })?;
                protocol::named_f64(params, "value")?.ok_or_else(|| {
                    MethodError::bad_params(
                        "value",
                        "An adjustment gives the parameter's new \"value\".",
                    )
                })?;
            }
            _ => {}
        }
        let run = self.controlled_mixed(conn).await?;
        let v = run.view();
        if method == "mixed.part" {
            let id = params.get("id").and_then(Value::as_str).unwrap_or("");
            match run.wire.parts.iter().find(|p| p.id == id) {
                None => {
                    return Err(
                        refuse("part_refused", format!("The test has no part \"{id}\"."))
                            .with_detail(json!({"id": id, "why": "unknown_part"})),
                    )
                }
                Some(p)
                    if p.body == WireBody::Floor
                        && params.get("on") == Some(&Value::Bool(false)) =>
                {
                    return Err(refuse(
                        "part_refused",
                        "The floor plays for the whole run; it cannot be switched off.",
                    )
                    .with_detail(json!({"id": id, "why": "floor"})))
                }
                _ => {}
            }
        }
        let w = wait_for(&run);
        let state = || view::state_json(Some(&run.view()));
        match method {
            "mixed.proceed" => {
                if v.state != State::Review {
                    return Err(wrong_state(&v, "Proceeding"));
                }
                run.ask(|reply| Cmd::Proceed { reply }, w).await?;
                Ok(state())
            }
            "mixed.level" => {
                let target = match (
                    protocol::named_f64(params, "delta_db")?,
                    protocol::named_f64(params, "to_db")?,
                ) {
                    (Some(d), _) => LevelTarget::Delta(d),
                    (_, Some(t)) => LevelTarget::To(t),
                    _ => unreachable!("checked above"),
                };
                let level = run.ask(|reply| Cmd::Level { target, reply }, w).await?;
                Ok(json!({"level_db": num(config::round(level, 2))}))
            }
            "mixed.mode" => {
                let mode = match protocol::named_str(params, "mode")? {
                    Some("manual") => RunMode::Manual,
                    _ => RunMode::Auto,
                };
                run.ask(|reply| Cmd::Mode { mode, reply }, w).await?;
                Ok(json!({"mode": crate::random::view::mode_name(mode)}))
            }
            "mixed.hold" | "mixed.resume" => {
                let on = method == "mixed.hold";
                run.ask(|reply| Cmd::Hold { on, reply }, w).await?;
                Ok(state())
            }
            "mixed.part" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let on = params.get("on") == Some(&Value::Bool(true));
                let by = {
                    let inner = self.inner.lock().await;
                    inner.session.client_name(conn.id, &conn.peer)
                };
                let i = id.clone();
                let got = run
                    .ask(
                        |reply| Cmd::Part {
                            id: i,
                            on,
                            by,
                            reply,
                        },
                        w,
                    )
                    .await?;
                Ok(json!({"id": id, "on": got}))
            }
            _ => {
                let key = params
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let value = protocol::named_f64(params, "value")?.unwrap_or(0.0);
                Self::mixed_adjust_check(&run.law.tones, &key, value)?;
                let k = key.clone();
                let got = run
                    .ask(
                        |reply| Cmd::Adjust {
                            key: k,
                            value,
                            reply,
                        },
                        w,
                    )
                    .await?;
                Ok(json!({"key": key, "value": num(got)}))
            }
        }
    }

    /// The refusals of an adjustment the tone law's declaration settles.
    fn mixed_adjust_check(
        facts: &config::RoleLaw,
        key: &str,
        value: f64,
    ) -> Result<(), MethodError> {
        let refused = |why: &'static str, words: String| {
            refuse("adjust_refused", words).with_detail(json!({"key": key, "why": why}))
        };
        let Some(spec) = facts.params.iter().find(|p| p.key == key) else {
            return Err(refused(
                "unknown_key",
                format!("{} declares no parameter \"{key}\".", facts.name),
            ));
        };
        if !facts.live.iter().any(|k| k == key) {
            return Err(refused(
                "not_live",
                format!(
                    "{}'s {} is fixed for the run; only its live parameters change while it runs.",
                    facts.name, spec.label
                ),
            ));
        }
        if let Some(why) = crate::shock::config::param_problem(spec, &Value::from(value)) {
            return Err(refused(
                "out_of_range",
                format!("{}'s {} {why}", facts.name, spec.label),
            ));
        }
        Ok(())
    }

    /// `mixed.stop` and `mixed.abort`: open to every connection.
    pub(super) async fn mixed_end(&self, conn: &Conn, abort: bool) -> Value {
        let (run, by) = {
            let inner = self.inner.lock().await;
            let Some(run) = inner.mixed.clone() else {
                return view::state_json(None);
            };
            (run, inner.session.client_name(conn.id, &conn.peer))
        };
        if run.view().state.is_live() {
            stop_mixed(
                &run,
                if abort { None } else { Some("operator_stop") },
                by,
                true,
            )
            .await;
        }
        view::state_json(Some(&run.view()))
    }

    pub(super) async fn mixed_reset(&self, conn: &Conn) -> Result<Value, MethodError> {
        let raw = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.mixed.clone().ok_or_else(not_configured)?;
            if run.view().state.is_live() {
                inner.run_active()?;
            }
            run.config.clone()
        };
        let (w, report, derived, facts, core) = self.mixed_check(&raw).await?;
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        let (Some(derived), Some(facts), Some(core)) = (derived, facts, core) else {
            return Err(config::config_invalid(&report));
        };
        let run = self.new_mixed_run(&mut inner, w, &derived, facts, core)?;
        let v = run.view();
        Ok(view::state_json(Some(&v)))
    }

    pub(super) async fn mixed_clear(&self, conn: &Conn) -> Result<Value, MethodError> {
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        if inner.mixed_live() {
            inner.run_active()?;
        }
        inner.mixed = None;
        let state = view::state_json(None);
        inner.mixed_state_key = view::state_key(None);
        self.emit("mixed.state", state.clone());
        Ok(state)
    }

    pub(super) async fn mixed_state_now(&self) -> Value {
        let inner = self.inner.lock().await;
        let v = inner.mixed.as_ref().map(|r| r.view());
        view::state_json(v.as_deref())
    }

    pub(super) async fn mixed_log(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let since = match params.get("since") {
            None | Some(Value::Null) => 0,
            Some(v) => v.as_u64().ok_or_else(|| {
                MethodError::bad_params("since", "\"since\" is a whole number from 0")
            })?,
        };
        let inner = self.inner.lock().await;
        let (entries, next) = match &inner.mixed {
            Some(r) => r.log_since(since),
            None => (Vec::new(), 0),
        };
        Ok(json!({
            "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "next": next,
        }))
    }

    /// `mixed.subscribe`: replace `conn`'s subscription, and send it the run's latest
    /// data at once when the run is streaming.
    pub(super) async fn mixed_subscribe(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let sub = subscribe::parse(params)?.map(Arc::new);
        let result = subscribe::result_json(sub.as_deref());
        match &sub {
            None => {
                self.mixed_displays().remove(&conn.id);
            }
            Some(sub) => {
                self.mixed_displays().insert(
                    conn.id,
                    MixedDisplay {
                        sub: sub.clone(),
                        last: None,
                    },
                );
                let run = self.inner.lock().await.mixed.clone();
                if let Some(run) = run.filter(|r| r.view().streaming) {
                    self.emit_mixed_data(conn.id, &run, sub);
                    if let Some(d) = self.mixed_displays().get_mut(&conn.id) {
                        d.last = Some(Instant::now());
                    }
                }
            }
        }
        Ok(result)
    }

    fn emit_mixed_data(&self, conn: u64, run: &MixedRun, sub: &Subscription) {
        let v = run.view();
        let mut arrays = Arrays::new();
        let bands = |id: &str| tone_bands(&run.core, id);
        let limits: Vec<subscribe::LimitRows> = run
            .wire
            .random
            .limits
            .iter()
            .map(|l| (l.channel, l.rows.clone()))
            .collect();
        let data = subscribe::data_json(&v, sub, &bands, &limits, &mut arrays);
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match codec::encode(&protocol::event("mixed.data", seq, data), &arrays) {
            Ok(frame) => {
                let _ = self
                    .events
                    .send(Arc::new(Out::To(conn, Feed::Mixed, frame)));
            }
            Err(e) => eprintln!("sds-control: mixed.data frame not encoded: {e}"),
        }
    }

    /// Each subscribed connection whose frame is due (every one when `last`).
    fn mixed_data_due(&self, run: &MixedRun, last: bool) {
        let now = Instant::now();
        let due: Vec<(u64, Arc<Subscription>)> = {
            let mut displays = self.mixed_displays();
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
            self.emit_mixed_data(conn, run, &sub);
        }
    }

    /// A run published: `mixed.state` when what it shows changed, and its new log
    /// entries. A publish of a run that is no longer the present one is dropped.
    pub(super) async fn mixed_changed(&self, run_id: &str) {
        let mut inner = self.inner.lock().await;
        let Some(run) = inner.mixed.clone() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        let shown = run.view();
        let key = view::state_key(Some(&shown));
        if key != inner.mixed_state_key {
            inner.mixed_state_key = key;
            self.emit("mixed.state", view::state_json(Some(&shown)));
        }
        let since = if inner.mixed_log_sent.0 == run.id {
            inner.mixed_log_sent.1
        } else {
            0
        };
        let (entries, next) = run.log_since(since);
        inner.mixed_log_sent = (run.id.clone(), next);
        if !entries.is_empty() {
            self.emit(
                "mixed.log",
                json!({
                    "run_id": run.id,
                    "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
                }),
            );
        }
    }

    /// Publish `run`'s `mixed.live` five times a second while it streams, and each
    /// subscription's `mixed.data` at its own rate, until it has finished; then once
    /// more with the final state. One task per run.
    pub(super) fn spawn_mixed_live(self: &Arc<Self>, run: Arc<MixedRun>) {
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
                    .mixed
                    .as_ref()
                    .is_some_and(|r| Arc::ptr_eq(r, &run));
                let v = run.view();
                if !v.streaming {
                    if !current || v.state == State::Finished {
                        break;
                    }
                    continue;
                }
                let last = v.state == State::Finished || !current;
                let now = Instant::now();
                if last || last_live.is_none_or(|t| now >= t + DISPLAY_PERIOD - DATA_SLACK) {
                    last_live = Some(now);
                    let mut arrays = Arrays::new();
                    let data = view::live_json(&v, &mut arrays);
                    let seq = svc.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    match codec::encode(&protocol::event("mixed.live", seq, data), &arrays) {
                        Ok(frame) => {
                            let _ = svc
                                .events
                                .send(Arc::new(Out::DefaultLive(Feed::Mixed, frame)));
                        }
                        Err(e) => eprintln!("sds-control: mixed.live frame not encoded: {e}"),
                    }
                }
                svc.mixed_data_due(&run, last);
                if last {
                    break;
                }
            }
        });
    }
}

impl Service {
    /// `laws.check` with `mixed`: the mixed check set with the law in its role and
    /// the built-in in the other. A law file's result is recorded for its role
    /// (design D15); a built-in's is reported only.
    pub(super) async fn mixed_laws_check(
        self: &Arc<Self>,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        use crate::mimo::law_check::{bad_choice, CheckSlot};
        let role = match params.get("mixed").and_then(Value::as_str) {
            Some("floor") => hertzforge_mixed::LawRole::Floor,
            Some("tones") => hertzforge_mixed::LawRole::Tones,
            _ => {
                return Err(MethodError::bad_params(
                    "mixed",
                    "\"mixed\" is the law's role, \"floor\" or \"tones\".",
                ))
            }
        };
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = protocol::named_str(params, "id")?.map(str::to_owned);
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
        let (facts, laws) = self
            .with_catalog(move |cat| {
                let id =
                    match (id, path) {
                        (Some(id), None) => id,
                        (None, Some(p)) => laws::import(cat, &p)?["law"]["id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        _ => return Err(MethodError::bad_params(
                            "id",
                            "Checking a law needs its catalog \"id\" or the \"path\" of its file.",
                        )),
                    };
                let world = match role {
                    hertzforge_mixed::LawRole::Floor => LawWorld::Single,
                    hertzforge_mixed::LawRole::Tones => LawWorld::Sine,
                };
                let facts = laws::mixed_lookup(Some(cat), &id, world)
                    .ok_or_else(|| crate::mimo::law_check::unknown_law(&id))?;
                if facts.world.is_err() {
                    return Err(bad_choice(&id, "a law of another world for this role"));
                }
                let pick = hertzforge_mixed::LawPick {
                    id: id.clone(),
                    params: given,
                };
                let mut choice = hertzforge_mixed::MixedLawChoice::default();
                match role {
                    hertzforge_mixed::LawRole::Floor => choice.floor = pick,
                    hertzforge_mixed::LawRole::Tones => choice.tones = pick,
                }
                let laws = hertzforge_mixed_laws::resolve_mixed(cat, &choice, 1)
                    .map_err(|e| bad_choice(&id, &e.to_string()))?;
                Ok((facts, laws))
            })
            .await?;
        let _slot = CheckSlot::take()?;
        let svc = self.clone();
        let law_id = facts.id.clone();
        let results = tokio::task::spawn_blocking(move || {
            crate::mixed::law_check::run_check(&law_id, role, &laws, &mut |event| {
                svc.emit("laws.check", event);
            })
        })
        .await
        .map_err(|e| MethodError::internal(format!("the law check failed: {e}")))??;
        let passed = !results.is_empty() && results.iter().all(|(_, v)| v.passed);
        let cases: Vec<Value> = results.iter().map(|(j, _)| j.clone()).collect();
        // A law file's result is recorded against its SHA-256 for this role; a
        // built-in is cleared already and its result is reported only. The role's
        // facts are read again so `hardware` states the record just written.
        let (facts, recorded) = if facts.builtin {
            (facts, false)
        } else {
            let id = facts.id.clone();
            let sha = facts.sha256.clone();
            let verdicts: Vec<_> = results.into_iter().map(|(_, v)| v).collect();
            let facts = self
                .with_catalog(move |cat| {
                    let record = hertzforge_mixed_laws::mixed_check_record(
                        &sha,
                        &crate::mimo::law_check::now_rfc3339(),
                        verdicts,
                    );
                    hertzforge_mixed_laws::record_mixed_check(cat, role, &record)
                        .map_err(|e| crate::mimo::law_check::not_recorded(&e))?;
                    laws::mixed_lookup(Some(cat), &id, hertzforge_mixed_laws::role_world(role))
                        .ok_or_else(|| MethodError::internal("the checked law left the catalog"))
                })
                .await?;
            (facts, true)
        };
        Ok(json!({
            "law": facts.to_json(),
            "report": {
                "passed": passed,
                "recorded": recorded,
                "role": crate::mixed::law_check::role_name(role),
                "tool_version": hertzforge_mixed_laws::MIXED_CHECK_VERSION,
                "cases": cases,
            },
        }))
    }
}

/// How long to wait for the run thread to answer: its identification, a margin.
fn wait_for(run: &MixedRun) -> Duration {
    Duration::from_secs_f64(10.0 + 2.0 * run.wire.identification_s())
}

const ARM_WAIT: Duration = Duration::from_secs(120);

/// End `run`: the controlled stop with `outcome` (`None`: the abort), raised on the
/// run's signals first so identification ends at its next block, then queued.
pub(super) async fn stop_mixed(
    run: &MixedRun,
    outcome: Option<&'static str>,
    by: String,
    wait: bool,
) {
    if !run.view().state.is_live() {
        return;
    }
    run.shared.signal_end(outcome.is_none(), &by);
    let w = wait_for(run);
    match outcome {
        None => {
            let _ = run.ask(|reply| Cmd::Abort { by, reply }, w).await;
        }
        Some(outcome) if wait => {
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
        }
        Some(outcome) => {
            let _ = run.send(Cmd::Stop {
                outcome,
                by,
                reply: None,
            });
        }
    }
}

/// How long the shutdown waits for a Mixed Mode run's controlled stop before it
/// aborts: at least ten seconds and twice its stop ramp.
pub(super) fn shutdown_patience(run: &MixedRun) -> Duration {
    let ramp = run.core.shutdown_ramp_s.max(run.core.part_ramp_s);
    Duration::from_secs_f64(10f64.max(2.0 * ramp + 2.0))
}

/// A tone's `(alarm, abort)` bands, dB, by part id.
pub fn tone_bands(core: &MixedConfig, id: &str) -> ((f64, f64), (f64, f64)) {
    core.parts
        .iter()
        .find(|p| p.id == id)
        .and_then(|p| match &p.body {
            PartBody::Tone(t) => Some((t.alarm_db, t.abort_db)),
            _ => None,
        })
        .unwrap_or((
            hertzforge_mixed::parts::DEFAULT_TONE_ALARM_DB,
            hertzforge_mixed::parts::DEFAULT_TONE_ABORT_DB,
        ))
}

fn derived_total_rms(d: &Derived) -> f64 {
    d.parts.iter().map(|p| p.ms_g2).sum::<f64>().sqrt()
}

/// The derived values back from their JSON, for the views `mixed.config` draws:
/// only the grid and the parts' spans are read.
fn derived_of(v: &Value) -> Option<ViewFacts> {
    Some(ViewFacts {
        df_hz: v["grid"]["df_hz"].as_f64()?,
        band_hz: (v["band_hz"][0].as_f64()?, v["band_hz"][1].as_f64()?),
    })
}

/// What the configuration's views need.
pub struct ViewFacts {
    pub df_hz: f64,
    pub band_hz: (f64, f64),
}

impl From<&Derived> for ViewFacts {
    fn from(d: &Derived) -> Self {
        ViewFacts {
            df_hz: d.grid.df_hz,
            band_hz: d.band_hz,
        }
    }
}

trait AsViewFacts {
    fn facts(&self) -> ViewFacts;
}

impl AsViewFacts for Derived {
    fn facts(&self) -> ViewFacts {
        ViewFacts::from(self)
    }
}

impl AsViewFacts for ViewFacts {
    fn facts(&self) -> ViewFacts {
        ViewFacts {
            df_hz: self.df_hz,
            band_hz: self.band_hz,
        }
    }
}

/// **Reference**: the floor plus each narrowband at its starting center, at 0 dB,
/// on the band's lines of the requested grid, with the floor's alarm and abort
/// lines.
fn reference_view(core: &MixedConfig, d: &impl AsViewFacts, arrays: &mut Arrays) -> Value {
    let f = d.facts();
    let mut x = Vec::new();
    let mut y = Vec::new();
    let mut bands = (3.0, 3.0, 6.0, 6.0);
    if let Some(floor) = core.floor() {
        bands = (
            floor.alarm_db.0,
            floor.alarm_db.1,
            floor.abort_db.0,
            floor.abort_db.1,
        );
        if let Ok(ls) = floor.breakpoints.interpolate(f.df_hz, f.band_hz.1) {
            for (k, p) in ls.psd.iter().enumerate() {
                let fk = ls.f0_hz + k as f64 * ls.df_hz;
                if fk >= f.band_hz.0 && fk <= f.band_hz.1 {
                    x.push(fk);
                    y.push(*p);
                }
            }
        }
    }
    let status = hertzforge_mixed::MixedStatus::idle(core);
    for (p, row) in core.parts.iter().zip(&status.parts) {
        if let (PartBody::Narrowband(b), Some(c)) = (&p.body, row.freq_hz) {
            let (lo, hi) = b.width.edges_at(c);
            for (fk, yk) in x.iter().zip(y.iter_mut()) {
                if *fk >= lo && *fk <= hi {
                    *yk += b.level_g2_hz;
                }
            }
        }
    }
    let db = |v: &[f64], d: f64| {
        v.iter()
            .map(|p| p * 10f64.powf(d / 10.0))
            .collect::<Vec<_>>()
    };
    let s = |name: &str, role: Role, v: Vec<f64>| Series::new(name, role, x.clone(), v).stepped();
    let series = if x.is_empty() {
        Vec::new()
    } else {
        vec![
            s("Reference", Role::Reference, y.clone()),
            s("Alarm high", Role::Tolerance, db(&y, bands.1)),
            s("Alarm low", Role::Tolerance, db(&y, -bands.0)),
            s("Abort high", Role::Abort, db(&y, bands.3)),
            s("Abort low", Role::Abort, db(&y, -bands.2)),
        ]
    };
    views::spectrum(
        arrays,
        "Reference",
        &Axis::new("Frequency", Some("Hz"), Scale::Log),
        &Axis::new("PSD", Some("g²/Hz"), Scale::Log),
        series,
        Some("frequency"),
    )
}

/// **Tones**: each tone's amplitude across its motion.
fn tones_view(core: &MixedConfig, _d: &impl AsViewFacts, arrays: &mut Arrays) -> Value {
    let motions = core.motions().unwrap_or_default();
    let mut series = Vec::new();
    let mut count = 0;
    for (p, m) in core.parts.iter().zip(&motions) {
        let (PartBody::Tone(t), Some(m)) = (&p.body, m) else {
            continue;
        };
        count += 1;
        let name = if p.name.is_empty() {
            format!("Tone {count}")
        } else {
            p.name.clone()
        };
        const N: usize = 128;
        let xs: Vec<f64> = if m.hi_hz > m.lo_hz {
            (0..=N)
                .map(|k| m.lo_hz * (m.hi_hz / m.lo_hz).powf(k as f64 / N as f64))
                .collect()
        } else {
            vec![m.lo_hz]
        };
        let ys: Vec<f64> = xs
            .iter()
            .map(|f| match &t.amplitude {
                ToneAmplitude::Constant { g_pk } => *g_pk,
                a => a.g_pk_at(*f).unwrap_or(f64::NAN),
            })
            .collect();
        series.push(Series::new(&name, Role::Reference, xs, ys));
    }
    views::spectrum(
        arrays,
        "Tones",
        &Axis::new("Frequency", Some("Hz"), Scale::Log),
        &Axis::new("Amplitude", Some("g"), Scale::Log),
        series,
        Some("frequency"),
    )
}
