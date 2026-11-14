//! The Explorer methods of the [`Service`] (schema/PROTOCOL.md, "Explorer"): a
//! configuration and its run beside the other modules' slots, under the same run
//! exclusion and controller rules, around the core's run host.

use super::{Conn, Feed, Out, Service, DATA_SLACK, LIVE_TICK};
use crate::codec::{self, Arrays};
use crate::explorer::config::{self, CheckContext, Derived, StationFigures, WireExplorer};
use crate::explorer::run::{wrong_state, ExplorerRun, StartContext};
use crate::explorer::subscribe::{self, DataContext, Subscription};
use crate::explorer::view::{self, ExplorerView, State, StationRef};
use crate::protocol::{self, num, ErrorCode, MethodError};
use crate::pulse::rails::Shared;
use crate::station::{self, Selection, StationPlan};
use crate::table;
use hertzforge_channel_table::ChannelTable;
use hertzforge_explorer_host::host::HostError;
use hertzforge_explorer_host::{FrequencyPart, LevelPart, ServiceError, StopCause, SweepDirection};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a steering verb waits for the core: the pump holds its lock for one batch
/// and one analysis pass, so this is a bound on a stuck station, not on a busy one.
const VERB_WAIT: Duration = Duration::from_secs(5);
/// How often `explorer.live` goes to the connections without a subscription.
const LIVE_PERIOD: Duration = Duration::from_millis(100);

/// One connection's Explorer subscription and when it was last sent a frame.
pub struct ExplorerDisplay {
    sub: Arc<Subscription>,
    last: Option<Instant>,
}

fn not_configured() -> MethodError {
    MethodError::named(
        ErrorCode::Refused,
        "not_configured",
        "Explorer is not configured; configure a test first.",
    )
}

/// How long the shutdown waits for the controlled stop before it aborts: twice the
/// stop at the drive slew, at least ten seconds.
pub(super) fn wait_for(run: &ExplorerRun) -> Duration {
    Duration::from_secs_f64((2.0 * run.derived_figures.stop_s).max(10.0))
}

/// Run `f` on the blocking pool, waiting at most `wait`.
async fn blocking<T: Send + 'static>(
    wait: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, MethodError> {
    match tokio::time::timeout(wait, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(MethodError::internal(format!("the run's verb failed: {e}"))),
        Err(_) => Err(MethodError::internal(
            "the run did not answer in time; the station may be stuck in a read. Abort stays \
             open.",
        )),
    }
}

/// A refusal of the core, in the wire's words.
fn host_refusal(run: &ExplorerRun, verb: &str, e: HostError) -> MethodError {
    match e {
        HostError::Service(ServiceError::LevelZero) => config::level_zero(),
        HostError::Service(ServiceError::BadState(_)) => wrong_state(&run.view(), verb),
        HostError::NotConfigured => wrong_state(&run.view(), verb),
        other => MethodError::internal(format!("{verb}: {other:?}")),
    }
}

fn number(params: &Map<String, Value>, k: &str) -> Result<Option<f64>, MethodError> {
    match params.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite())
            .map(Some)
            .ok_or_else(|| MethodError::bad_params(k, format!("\"{k}\" is a number"))),
    }
}

/// The steer's parts: one frequency part and one level part at most.
fn steer_parts(
    params: &Map<String, Value>,
) -> Result<(Option<FrequencyPart>, Option<LevelPart>), MethodError> {
    const KEYS: [&str; 5] = ["hz", "step_hz", "step_oct", "vpk", "step_db"];
    if let Some(k) = params.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(MethodError::bad_params(
            k,
            format!(
                "a steer takes \"hz\", \"step_hz\" or \"step_oct\", and \"vpk\" or \"step_db\", \
                 not \"{k}\""
            ),
        ));
    }
    let mut freq = None;
    for (k, part) in [
        ("hz", FrequencyPart::Hz as fn(f64) -> FrequencyPart),
        ("step_hz", FrequencyPart::StepHz),
        ("step_oct", FrequencyPart::StepOct),
    ] {
        if let Some(x) = number(params, k)? {
            if freq.is_some() {
                return Err(MethodError::bad_params(
                    k,
                    "a steer has one frequency part: \"hz\", \"step_hz\" or \"step_oct\"",
                ));
            }
            freq = Some(part(x));
        }
    }
    let mut level = None;
    for (k, part) in [
        ("vpk", LevelPart::Vpk as fn(f64) -> LevelPart),
        ("step_db", LevelPart::StepDb),
    ] {
        if let Some(x) = number(params, k)? {
            if level.is_some() {
                return Err(MethodError::bad_params(
                    k,
                    "a steer has one level part: \"vpk\" or \"step_db\"",
                ));
            }
            level = Some(part(x));
        }
    }
    if freq.is_none() && level.is_none() {
        return Err(MethodError::bad_params(
            "hz",
            "a steer has a frequency part, a level part, or one of each",
        ));
    }
    Ok((freq, level))
}

fn since_param(params: &Map<String, Value>) -> Result<Option<u64>, MethodError> {
    match params.get("since") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| MethodError::bad_params("since", "\"since\" is a whole number from 0")),
    }
}

impl Service {
    pub(super) fn explorer_displays(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, ExplorerDisplay>> {
        match self.explorer_displays.lock() {
            Ok(d) => d,
            Err(p) => p.into_inner(),
        }
    }

    fn explorer_station(inner: &super::Inner) -> (StationRef, StationFigures) {
        let selected = inner.stations.selected().clone();
        let figures = if selected == Selection::Sim {
            StationFigures::sim()
        } else {
            StationFigures::Ni
        };
        let r = StationRef {
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
        };
        (r, figures)
    }

    /// Parse and check `config` against the present table and station.
    async fn explorer_check(
        &self,
        config: &Value,
    ) -> Result<
        (
            WireExplorer,
            table::Report,
            Option<Derived>,
            ChannelTable,
            StationRef,
        ),
        MethodError,
    > {
        let w = config::parse(config)?;
        let inner = self.inner.lock().await;
        let tbl = inner.table.table.clone();
        let mut report = table::validate(&tbl, &inner.ctx());
        let (station, figures) = Self::explorer_station(&inner);
        let (r, derived) = w.check(&CheckContext {
            table: &tbl,
            timeout_s: self.opts.timeout.as_secs_f64(),
            station: figures,
        });
        report.errors.extend(r.errors);
        report.warnings.extend(r.warnings);
        let derived = if report.errors.is_empty() {
            derived
        } else {
            None
        };
        Ok((w, report, derived, tbl, station))
    }

    pub(super) async fn explorer_configure(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let config = params
            .get("config")
            .filter(|c| c.is_object())
            .ok_or_else(|| {
                MethodError::bad_params("config", "Configuring needs a test configuration.")
            })?;
        let dry_run = protocol::param_bool(params, "dry_run")?.unwrap_or(false);
        let (w, report, derived, tbl, station) = self.explorer_check(config).await?;
        if dry_run {
            return Ok(json!({
                "errors": report.errors.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                "warnings": report.warnings.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                "derived": derived.as_ref().map_or(Value::Null, Derived::to_json),
            }));
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
        let Some(derived) = derived else {
            return Err(config::config_invalid(&report));
        };
        let run = self.new_explorer_run(&mut inner, w, derived, &tbl, station);
        Ok(json!({
            "state": view::state_json(Some(&run.view())),
            "derived": run.derived,
            "warnings": report.warnings.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        }))
    }

    /// Create a run (replacing the one before).
    fn new_explorer_run(
        &self,
        inner: &mut super::Inner,
        w: WireExplorer,
        derived: Derived,
        tbl: &ChannelTable,
        station: StationRef,
    ) -> Arc<ExplorerRun> {
        inner.explorer_runs += 1;
        let run_id = format!("explorer-{:04}", inner.explorer_runs);
        let initial = view::configured_view(&run_id, &w, &derived, station, tbl);
        let shared = Arc::new(Shared::new(initial.clone(), self.explorer_notify.clone()));
        let title = w.title.clone();
        let run = Arc::new(ExplorerRun::new(
            run_id.clone(),
            shared.clone(),
            Arc::new(w),
            derived,
            initial.clone(),
        ));
        shared.add_log(
            "state",
            format!("configured \"{title}\" on {}", initial.station.class),
        );
        inner.explorer = Some(run.clone());
        let _ = self.explorer_notify.send((run_id, Arc::new(initial)));
        run
    }

    pub(super) async fn explorer_config(&self) -> Value {
        let inner = self.inner.lock().await;
        match &inner.explorer {
            None => json!({"run_id": null, "config": null, "derived": null}),
            Some(run) => json!({"run_id": run.id, "config": run.config, "derived": run.derived}),
        }
    }

    async fn controlled_explorer(&self, conn: &Conn) -> Result<Arc<ExplorerRun>, MethodError> {
        let inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.explorer.clone().ok_or_else(not_configured)
    }

    pub(super) async fn explorer_start(
        self: &Arc<Self>,
        conn: &Conn,
    ) -> Result<Value, MethodError> {
        let (run, ctx) = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.explorer.clone().ok_or_else(not_configured)?;
            let v = run.view();
            if v.state == State::Finished {
                return Err(MethodError::named(
                    ErrorCode::Refused,
                    "run_finished",
                    "This run has finished; reset or configure again to run.",
                )
                .with_detail(json!({"outcome": v.end.as_ref().map(|e| e.outcome)})));
            }
            if v.state.is_live() || run.started() {
                return Err(wrong_state(&v, "explorer.start"));
            }
            if inner.random_live()
                || inner.mimo_live()
                || inner.shock_live()
                || inner.srs_live()
                || inner.sine_live()
                || inner.mixed_live()
                || inner.twr_live()
                || inner.recorder_live()
                || inner.mimo_shock_live()
            {
                inner.run_active()?;
            }
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
            // The table as it is now must still carry the run.
            let (_, figures) = Self::explorer_station(&inner);
            let (r, _) = run.wire.check(&CheckContext {
                table: &inner.table.table,
                timeout_s: self.opts.timeout.as_secs_f64(),
                station: figures,
            });
            if !r.errors.is_empty() {
                return Err(config::config_invalid(&r));
            }
            let fs_hz = run.wire.fs();
            let (revision, devices) = (inner.table.revision, inner.stations.devices().to_vec());
            let plan = match inner.stations.selected() {
                Selection::Sim => StationPlan::sim(fs_hz),
                Selection::Ni { devices } => {
                    let (inputs, outputs) =
                        hertzforge_station::physical_from_table(&inner.table.table);
                    StationPlan::Ni {
                        devices: devices.clone(),
                        inputs,
                        outputs,
                        fs_hz,
                    }
                }
            };
            run.mark_starting();
            let ctx = StartContext {
                table: inner.table.table.clone(),
                station: plan,
                scanner: self.opts.scanner.clone(),
                data_root: self.opts.data_root.clone(),
                records_root: self.opts.records_root(),
                bind: self.opts.bind.clone(),
                revision,
                devices,
                record: self.opts.record.clone(),
            };
            (run, ctx)
        };
        let r = run.clone();
        let started = tokio::task::spawn_blocking(move || r.start(ctx))
            .await
            .map_err(|e| MethodError::internal(format!("the start failed: {e}")))?;
        self.spawn_explorer_live(run.clone());
        started?;
        Ok(view::state_json(Some(&run.view())))
    }

    /// A verb that applies only while the tone runs.
    async fn running_explorer(
        &self,
        conn: &Conn,
        verb: &str,
    ) -> Result<Arc<ExplorerRun>, MethodError> {
        let run = self.controlled_explorer(conn).await?;
        let v = run.view();
        if v.state != State::Running {
            return Err(wrong_state(&v, verb));
        }
        Ok(run)
    }

    pub(super) async fn explorer_steer(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let (freq, level) = steer_parts(params)?;
        let run = self.running_explorer(conn, "explorer.steer").await?;
        let host = run.host.clone();
        let s = blocking(VERB_WAIT, move || host.steer(freq, level))
            .await?
            .map_err(|e| host_refusal(&run, "explorer.steer", e))?;
        Ok(json!({
            "hz": num(s.hz), "vpk": num(s.vpk),
            "clamped": {"hz": s.clamped_hz, "vpk": s.clamped_vpk},
            "frame": s.frame, "input_frame": s.input_frame,
        }))
    }

    pub(super) async fn explorer_sweep(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        if let Some(k) = params
            .keys()
            .find(|k| !matches!(k.as_str(), "direction" | "rate_oct_per_min"))
        {
            return Err(MethodError::bad_params(
                k,
                format!("a sweep takes \"direction\" and \"rate_oct_per_min\", not \"{k}\""),
            ));
        }
        let direction = match params.get("direction").and_then(Value::as_str) {
            Some("up") => SweepDirection::Up,
            Some("down") => SweepDirection::Down,
            _ => {
                return Err(MethodError::bad_params(
                    "direction",
                    "\"direction\" is \"up\" or \"down\"",
                ))
            }
        };
        let rate = number(params, "rate_oct_per_min")?
            .filter(|r| *r > 0.0)
            .ok_or_else(|| {
                MethodError::bad_params(
                    "rate_oct_per_min",
                    "\"rate_oct_per_min\" is a number of octaves per minute above 0",
                )
            })?;
        let run = self.running_explorer(conn, "explorer.sweep").await?;
        let host = run.host.clone();
        let rec = blocking(VERB_WAIT, move || host.sweep(direction, rate))
            .await?
            .map_err(|e| host_refusal(&run, "explorer.sweep", e))?;
        let (rate, clamped) = match rec.kind {
            hertzforge_explorer_host::IntentKind::Sweep {
                rate_oct_per_min, ..
            } => (rate_oct_per_min.adopted, rate_oct_per_min.clamped),
            _ => (rate, false),
        };
        Ok(json!({
            "direction": if direction == SweepDirection::Up { "up" } else { "down" },
            "rate_oct_per_min": num(rate), "clamped": clamped,
            "frame": rec.frame, "input_frame": rec.input_frame,
        }))
    }

    pub(super) async fn explorer_hold(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.running_explorer(conn, "explorer.hold").await?;
        let host = run.host.clone();
        let rec = blocking(VERB_WAIT, move || host.hold())
            .await?
            .map_err(|e| host_refusal(&run, "explorer.hold", e))?;
        let hz = match rec.kind {
            hertzforge_explorer_host::IntentKind::Hold { hz } => hz,
            _ => run.view().target_hz.unwrap_or(0.0),
        };
        Ok(json!({"hz": num(hz), "frame": rec.frame, "input_frame": rec.input_frame}))
    }

    /// `explorer.stop` and `explorer.abort`: open to every connection.
    pub(super) async fn explorer_end(
        &self,
        conn: &Conn,
        abort: bool,
    ) -> Result<Value, MethodError> {
        let (run, by) = {
            let inner = self.inner.lock().await;
            let Some(run) = inner.explorer.clone() else {
                return Ok(view::state_json(None));
            };
            (run, inner.session.client_name(conn.id, &conn.peer))
        };
        if !run.view().state.is_live() {
            return Ok(view::state_json(Some(&run.view())));
        }
        if abort {
            abort_explorer(&run, by).await;
        } else {
            stop_explorer(&run, StopCause::Operator, by);
        }
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn explorer_survey(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let since = since_param(params)?;
        let run = self.inner.lock().await.explorer.clone();
        Ok(match run {
            None => view::survey_result(0, None, None, &[], since),
            Some(run) => {
                let (rev, points) = run.data.survey();
                let control = run.base.control.as_ref().map(|c| c.index);
                view::survey_result(rev, control, Some(&run.base.control_unit), &points, since)
            }
        })
    }

    pub(super) async fn explorer_survey_clear(&self, conn: &Conn) -> Result<Value, MethodError> {
        let run = self.controlled_explorer(conn).await?;
        let v = run.view();
        if !matches!(v.state, State::Running | State::Stopping) {
            return Err(wrong_state(&v, "explorer.survey.clear"));
        }
        let host = run.host.clone();
        let rev = blocking(VERB_WAIT, move || host.clear_survey())
            .await?
            .map_err(|e| host_refusal(&run, "explorer.survey.clear", e))?;
        Ok(json!({"revision": rev}))
    }

    /// `explorer.reset`: configure the present run's configuration again.
    pub(super) async fn explorer_reset(&self, conn: &Conn) -> Result<Value, MethodError> {
        let raw = {
            let inner = self.inner.lock().await;
            inner.session.require_controller(conn.id)?;
            let run = inner.explorer.clone().ok_or_else(not_configured)?;
            inner.run_active()?;
            run.config.clone()
        };
        let (w, report, derived, tbl, station) = self.explorer_check(&raw).await?;
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        inner.run_active()?;
        let Some(derived) = derived else {
            return Err(config::config_invalid(&report));
        };
        let run = self.new_explorer_run(&mut inner, w, derived, &tbl, station);
        Ok(view::state_json(Some(&run.view())))
    }

    pub(super) async fn explorer_clear(&self, conn: &Conn) -> Result<Value, MethodError> {
        let mut inner = self.inner.lock().await;
        inner.session.require_controller(conn.id)?;
        if inner.explorer_live() {
            inner.run_active()?;
        }
        inner.explorer = None;
        let state = view::state_json(None);
        inner.explorer_state_key = view::state_key(None);
        self.emit("explorer.state", state.clone());
        Ok(state)
    }

    pub(super) async fn explorer_state_now(&self) -> Value {
        let inner = self.inner.lock().await;
        let v = inner.explorer.as_ref().map(|r| r.view());
        view::state_json(v.as_deref())
    }

    pub(super) async fn explorer_log(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let since = since_param(params)?.unwrap_or(0);
        let inner = self.inner.lock().await;
        let (entries, next) = match &inner.explorer {
            Some(r) => r.shared.log_since(since),
            None => (Vec::new(), 0),
        };
        Ok(json!({
            "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "next": next,
        }))
    }

    /// `explorer.subscribe`: replace `conn`'s subscription, and send it the run's
    /// frame at once when the run has started.
    pub(super) async fn explorer_subscribe(
        &self,
        conn: &Conn,
        params: &Map<String, Value>,
    ) -> Result<Value, MethodError> {
        let sub = subscribe::parse(params)?.map(Arc::new);
        let result = subscribe::result_json(sub.as_deref());
        match &sub {
            None => {
                self.explorer_displays().remove(&conn.id);
            }
            Some(sub) => {
                self.explorer_displays().insert(
                    conn.id,
                    ExplorerDisplay {
                        sub: sub.clone(),
                        last: None,
                    },
                );
                let run = self.inner.lock().await.explorer.clone();
                if let Some(run) = run.filter(|r| r.started()) {
                    self.emit_explorer_data(conn.id, &run, sub);
                    if let Some(d) = self.explorer_displays().get_mut(&conn.id) {
                        d.last = Some(Instant::now());
                    }
                }
            }
        }
        Ok(result)
    }

    fn emit_explorer_data(&self, conn: u64, run: &ExplorerRun, sub: &Subscription) {
        let v = run.view();
        let (_, survey) = run.data.survey();
        let spectra = run.data.spectra();
        let (first, samples) = run.data.time();
        let ctx = DataContext {
            view: &v,
            survey: &survey,
            spectra: &spectra,
            time: Some((first, v.grid.fs_hz, &samples)),
        };
        let mut arrays = Arrays::new();
        let data = subscribe::data_json(&ctx, sub, &mut arrays);
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match codec::encode(&protocol::event("explorer.data", seq, data), &arrays) {
            Ok(frame) => {
                let _ = self
                    .events
                    .send(Arc::new(Out::To(conn, Feed::Explorer, frame)));
            }
            Err(e) => eprintln!("sds-control: explorer.data frame not encoded: {e}"),
        }
    }

    /// Each subscribed connection whose frame is due (every one when `force`).
    fn explorer_data_due(&self, run: &ExplorerRun, force: bool) {
        let now = Instant::now();
        let due: Vec<(u64, Arc<Subscription>)> = {
            let mut displays = self.explorer_displays();
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
            self.emit_explorer_data(conn, run, &sub);
        }
    }

    /// A run published `v`: `explorer.state` when what it shows changed, and its new
    /// log entries.
    pub(super) async fn explorer_changed(&self, run_id: &str, v: &ExplorerView) {
        let mut inner = self.inner.lock().await;
        let Some(run) = inner.explorer.clone() else {
            return;
        };
        if run.id != run_id {
            return;
        }
        let key = view::state_key(Some(v));
        if key != inner.explorer_state_key {
            inner.explorer_state_key = key;
            self.emit("explorer.state", view::state_json(Some(v)));
        }
        let since = if inner.explorer_log_sent.0 == run.id {
            inner.explorer_log_sent.1
        } else {
            0
        };
        let (entries, next) = run.shared.log_since(since);
        inner.explorer_log_sent = (run.id.clone(), next);
        if !entries.is_empty() {
            self.emit(
                "explorer.log",
                json!({
                    "run_id": run.id,
                    "entries": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
                }),
            );
        }
    }

    /// Publish `run`'s `explorer.live` ten times a second from start until it has
    /// finished, then once more, to the connections without a subscription of their
    /// own; and `explorer.data` to the subscribed ones at their rate. One task per run.
    fn spawn_explorer_live(self: &Arc<Self>, run: Arc<ExplorerRun>) {
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
                    .explorer
                    .as_ref()
                    .is_some_and(|r| Arc::ptr_eq(r, &run));
                let v = run.view();
                let last = v.state == State::Finished || !current;
                let now = Instant::now();
                if last || last_live.is_none_or(|t| now + DATA_SLACK >= t + LIVE_PERIOD) {
                    last_live = Some(now);
                    let (_, survey) = run.data.survey();
                    let drawn = view::draw_survey(&survey, v.band_hz, view::SURVEY_POINTS);
                    let mut arrays = Arrays::new();
                    let data = view::live_json(&v, &drawn, &mut arrays);
                    let seq = svc.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    match codec::encode(&protocol::event("explorer.live", seq, data), &arrays) {
                        Ok(frame) => {
                            let _ = svc
                                .events
                                .send(Arc::new(Out::DefaultLive(Feed::Explorer, frame)));
                        }
                        Err(e) => eprintln!("sds-control: explorer.live frame not encoded: {e}"),
                    }
                }
                if current {
                    svc.explorer_data_due(&run, last);
                }
                if last {
                    break;
                }
            }
        });
    }
}

/// Begin `run`'s controlled stop for `cause`, named for `by`. It answers at once: from
/// `starting` the stop takes effect once the station has opened.
pub(super) fn stop_explorer(run: &ExplorerRun, cause: StopCause, by: String) {
    run.shared.set_ended_by(&by);
    let host = run.host.clone();
    tokio::task::spawn_blocking(move || host.stop_for(cause));
}

/// Abort `run` for `by`, waiting a moment for the core to take it.
pub(super) async fn abort_explorer(run: &ExplorerRun, by: String) {
    run.shared.set_ended_by(&by);
    run.note_operator_abort();
    let host = run.host.clone();
    let _ = blocking(VERB_WAIT, move || host.abort("operator abort")).await;
}
