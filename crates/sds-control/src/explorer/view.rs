//! The Explorer run's state and live frames (schema/PROTOCOL.md, "Explorer Run State",
//! "Explorer Display"), built from one [`ExplorerView`] of the run. The run rounds every
//! figure as it composes the view; these builders lay it out in contract order.

use super::config::{Derived, WireExplorer};
use crate::codec::Arrays;
use crate::protocol::{num, opt_num};
use crate::table::unit_name;
use crate::views::{self, Axis, Role, Row, Scale, Series, Status};
use hertzforge_channel_table::{ChannelRole, ChannelTable};
use serde_json::{json, Map, Value};

pub use crate::random::view::RecordRef;
pub use crate::sine::view::StationRef;

/// Points the live frame's survey line is drawn on across the band.
pub const SURVEY_POINTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unconfigured,
    Configured,
    Starting,
    Running,
    Stopping,
    Finished,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Unconfigured => "unconfigured",
            State::Configured => "configured",
            State::Starting => "starting",
            State::Running => "running",
            State::Stopping => "stopping",
            State::Finished => "finished",
        }
    }

    /// Starting, running or stopping: the station is reserved.
    pub fn is_live(self) -> bool {
        matches!(self, State::Starting | State::Running | State::Stopping)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub requested_fs_hz: f64,
    pub fs_hz: f64,
    pub block_frames: usize,
    pub lookahead_frames: u32,
    pub actual: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sweep {
    pub direction: &'static str,
    pub rate_oct_per_min: f64,
    pub halted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    pub engine_ticks: u64,
    pub worst_tick_ms: Option<f64>,
    pub budget_ms: Option<f64>,
    pub health_faults: u64,
    pub ring_overruns: u64,
    pub blocks_dropped: u64,
    pub driver_warnings: u64,
    pub pass_last_ms: Option<f64>,
    pub pass_worst_ms: Option<f64>,
    pub batch_gap_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stall {
    pub stale_s: f64,
    pub period_s: f64,
    pub message: String,
}

/// What a protection saw: `watchdog` (`channel`, `axis`, `value`, `limit`), `drive_vpk`
/// (`vpk`, `ceiling_vpk`) or `limit` (`channel`, `axis`, `value`, `limit`).
#[derive(Debug, Clone, PartialEq)]
pub enum Trip {
    Watchdog {
        channel: u32,
        axis: &'static str,
        value: f64,
        limit: f64,
    },
    DriveVpk {
        vpk: f64,
        ceiling_vpk: f64,
    },
    Limit {
        channel: u32,
        axis: &'static str,
        value: f64,
        limit: f64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct End {
    pub outcome: &'static str,
    pub message: String,
    pub at_s: Option<f64>,
    pub at_hz: Option<f64>,
    pub vpk: Option<f64>,
    pub ramp_s: Option<f64>,
    pub trip: Option<Trip>,
}

impl End {
    pub fn phase(&self) -> &'static str {
        match self.outcome {
            "operator_stop" | "safe_stop" | "limit_stop" => "stopped",
            "operator_abort" | "aborted" => "aborted",
            _ => "faulted",
        }
    }

    pub fn to_json(&self) -> Value {
        let trip = self.trip.as_ref().map_or(Value::Null, |t| {
            let mut m = Map::new();
            let mut facts = |kind: &str, channel: u32, axis: &str, value: f64, limit: f64| {
                m.insert("kind".into(), Value::from(kind));
                m.insert("at_hz".into(), opt_num(self.at_hz));
                m.insert("channel".into(), Value::from(channel));
                m.insert("axis".into(), Value::from(axis));
                m.insert("value".into(), num(value));
                m.insert("limit".into(), num(limit));
            };
            match t {
                Trip::Watchdog {
                    channel,
                    axis,
                    value,
                    limit,
                } => facts("watchdog", *channel, axis, *value, *limit),
                Trip::Limit {
                    channel,
                    axis,
                    value,
                    limit,
                } => facts("limit", *channel, axis, *value, *limit),
                Trip::DriveVpk { vpk, ceiling_vpk } => {
                    m.insert("kind".into(), Value::from("drive_vpk"));
                    m.insert("at_hz".into(), opt_num(self.at_hz));
                    m.insert("vpk".into(), num(*vpk));
                    m.insert("ceiling_vpk".into(), num(*ceiling_vpk));
                }
            }
            Value::Object(m)
        });
        json!({
            "outcome": self.outcome,
            "message": self.message,
            "at_s": opt_num(self.at_s),
            "at_hz": opt_num(self.at_hz),
            "vpk": opt_num(self.vpk),
            "ramp_s": opt_num(self.ramp_s),
            "trip": trip,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveChannel {
    pub index: u32,
    pub description: String,
    pub role: &'static str,
    pub rms: Option<f64>,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControlMotion {
    pub index: u32,
    pub velocity_ips: Option<f64>,
    pub displacement_mil: Option<f64>,
    pub settling: bool,
}

/// One moment of an Explorer run, everything the state and the live frames say.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplorerView {
    pub run_id: String,
    pub state: State,
    pub phase: &'static str,
    pub title: String,
    pub station: StationRef,
    pub grid: Grid,
    pub clock: Option<&'static str>,
    pub band_hz: (f64, f64),
    pub max_drive_vpk: f64,
    pub slew: (f64, f64),
    pub pipeline_frames: Option<u64>,
    pub t_s: Option<f64>,
    pub frequency_hz: Option<f64>,
    pub target_hz: Option<f64>,
    pub drive_vpk: Option<f64>,
    pub target_vpk: Option<f64>,
    pub sweep: Option<Sweep>,
    pub health: Option<Health>,
    pub stall: Option<Stall>,
    pub stop: Option<&'static str>,
    pub end: Option<End>,
    // The live frame's own.
    pub measured_hz: Option<f64>,
    pub channels: Vec<LiveChannel>,
    pub control: Option<ControlMotion>,
    /// The survey's highest point, `(hz, level)`.
    pub peak: Option<(f64, f64)>,
    /// The control row's unit.
    pub control_unit: String,
    /// The run's folder from start on, whether it keeps the streams, and the latest
    /// problem writing it.
    pub record: Option<RecordRef>,
}

fn role_name(r: ChannelRole) -> &'static str {
    match r {
        ChannelRole::Control => "control",
        ChannelRole::Reference => "reference",
        _ => "monitor",
    }
}

/// The acquired channels of `table` in table order, rms unknown.
pub fn table_channels(table: &ChannelTable) -> Vec<LiveChannel> {
    table
        .channels
        .iter()
        .filter(|c| c.enabled && c.role != ChannelRole::Drive)
        .map(|c| LiveChannel {
            index: c.index,
            description: c.description.clone(),
            role: role_name(c.role),
            rms: None,
            unit: unit_name(&c.units),
        })
        .collect()
}

/// The view of a configured run, nothing opened.
pub fn configured_view(
    run_id: &str,
    w: &WireExplorer,
    d: &Derived,
    station: StationRef,
    table: &ChannelTable,
) -> ExplorerView {
    let control = WireExplorer::control_row(table);
    ExplorerView {
        run_id: run_id.to_owned(),
        state: State::Configured,
        phase: "idle",
        title: w.title.clone(),
        station,
        grid: Grid {
            requested_fs_hz: d.fs_hz,
            fs_hz: d.fs_hz,
            block_frames: d.block_frames,
            lookahead_frames: d.lookahead_frames,
            actual: false,
        },
        clock: None,
        band_hz: w.band_hz,
        max_drive_vpk: w.max_drive_vpk,
        slew: (w.freq_oct_per_s, w.drive_db_per_s),
        pipeline_frames: Some(d.pipeline_frames),
        t_s: None,
        frequency_hz: None,
        target_hz: None,
        drive_vpk: None,
        target_vpk: None,
        sweep: None,
        health: None,
        stall: None,
        stop: None,
        end: None,
        measured_hz: None,
        channels: table_channels(table),
        control: control.map(|c| ControlMotion {
            index: c.index,
            velocity_ips: None,
            displacement_mil: None,
            settling: true,
        }),
        peak: None,
        control_unit: control.map_or_else(|| "g".into(), |c| unit_name(&c.units)),
        record: None,
    }
}

fn sweep_json(s: &Option<Sweep>) -> Value {
    s.as_ref().map_or(Value::Null, |s| {
        json!({"direction": s.direction, "rate_oct_per_min": num(s.rate_oct_per_min),
               "halted": s.halted})
    })
}

fn pass_json(last: Option<f64>, worst: Option<f64>) -> Value {
    json!({"last": opt_num(last), "worst": opt_num(worst)})
}

/// The `explorer.state` object; unconfigured for `None`.
pub fn state_json(v: Option<&ExplorerView>) -> Value {
    let Some(v) = v else {
        return json!({
            "run_id": null, "state": "unconfigured", "phase": "idle", "title": null,
            "source": null, "station": null, "grid": null, "clock": null, "band_hz": null,
            "max_drive_vpk": null, "slew": null, "pipeline_frames": null, "t_s": null,
            "frequency_hz": null, "target_hz": null, "drive_vpk": null, "target_vpk": null,
            "sweep": null, "health": null, "stall": null, "stop": null, "end": null,
            "record": null,
        });
    };
    let health = v.health.as_ref().map_or(Value::Null, |h| {
        json!({
            "engine_ticks": h.engine_ticks,
            "worst_tick_ms": opt_num(h.worst_tick_ms),
            "budget_ms": opt_num(h.budget_ms),
            "health_faults": h.health_faults,
            "ring_overruns": h.ring_overruns,
            "blocks_dropped": h.blocks_dropped,
            "driver_warnings": h.driver_warnings,
            "pass_ms": pass_json(h.pass_last_ms, h.pass_worst_ms),
            "batch_gap_ms": opt_num(h.batch_gap_ms),
        })
    });
    let stall = v.stall.as_ref().map_or(
        Value::Null,
        |s| json!({"stale_s": num(s.stale_s), "period_s": num(s.period_s), "message": s.message}),
    );
    json!({
        "run_id": v.run_id,
        "state": v.state.name(),
        "phase": v.phase,
        "title": v.title,
        "source": v.station.source(),
        "station": v.station.to_json(),
        "grid": {
            "requested_fs_hz": num(v.grid.requested_fs_hz),
            "fs_hz": num(v.grid.fs_hz),
            "block_frames": v.grid.block_frames,
            "lookahead_frames": v.grid.lookahead_frames,
            "actual": v.grid.actual,
        },
        "clock": v.clock,
        "band_hz": [num(v.band_hz.0), num(v.band_hz.1)],
        "max_drive_vpk": num(v.max_drive_vpk),
        "slew": {"freq_oct_per_s": num(v.slew.0), "drive_db_per_s": num(v.slew.1)},
        "pipeline_frames": v.pipeline_frames,
        "t_s": opt_num(v.t_s),
        "frequency_hz": opt_num(v.frequency_hz),
        "target_hz": opt_num(v.target_hz),
        "drive_vpk": opt_num(v.drive_vpk),
        "target_vpk": opt_num(v.target_vpk),
        "sweep": sweep_json(&v.sweep),
        "health": health,
        "stall": stall,
        "stop": v.stop,
        "end": v.end.as_ref().map_or(Value::Null, End::to_json),
        "record": v.record.as_ref().map_or(Value::Null, RecordRef::to_json),
    })
}

/// What an `explorer.state` event is sent on a change of: state, phase, sweep, stop,
/// stall, end and record.
pub fn state_key(v: Option<&ExplorerView>) -> String {
    match v {
        None => "unconfigured".into(),
        Some(v) => format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            v.run_id,
            v.state.name(),
            v.phase,
            sweep_json(&v.sweep),
            v.stop.unwrap_or(""),
            v.stall.is_some(),
            v.end.as_ref().map_or(Value::Null, End::to_json),
            v.record.as_ref().map_or(Value::Null, RecordRef::to_json)
        ),
    }
}

/// The numbers every live frame carries, `explorer.live` and `explorer.data` alike.
pub fn live_head(v: &ExplorerView) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("run_id".into(), Value::from(v.run_id.as_str()));
    m.insert("source".into(), Value::from(v.station.source()));
    m.insert("class".into(), Value::from(v.station.class));
    let state = match v.state {
        State::Stopping => "stopping",
        State::Finished => "finished",
        _ => "running",
    };
    m.insert("state".into(), Value::from(state));
    m.insert("phase".into(), Value::from(v.phase));
    m.insert("t_s".into(), opt_num(v.t_s));
    m.insert("frequency_hz".into(), opt_num(v.frequency_hz));
    m.insert("target_hz".into(), opt_num(v.target_hz));
    m.insert("measured_hz".into(), opt_num(v.measured_hz));
    m.insert("drive_vpk".into(), opt_num(v.drive_vpk));
    m.insert("target_vpk".into(), opt_num(v.target_vpk));
    m.insert("max_drive_vpk".into(), num(v.max_drive_vpk));
    m.insert("sweep".into(), sweep_json(&v.sweep));
    m.insert(
        "channels".into(),
        Value::Array(
            v.channels
                .iter()
                .map(|c| {
                    json!({"index": c.index, "description": c.description, "role": c.role,
                           "rms": opt_num(c.rms), "unit": c.unit})
                })
                .collect(),
        ),
    );
    m.insert(
        "control".into(),
        v.control.as_ref().map_or(Value::Null, |c| {
            json!({"index": c.index, "velocity_ips": opt_num(c.velocity_ips),
                   "displacement_mil": opt_num(c.displacement_mil), "settling": c.settling})
        }),
    );
    m.insert("pipeline_frames".into(), json!(v.pipeline_frames));
    m.insert(
        "peak".into(),
        v.peak.map_or(
            Value::Null,
            |(hz, level)| json!({"hz": num(hz), "level": num(level)}),
        ),
    );
    m.insert(
        "blocks_dropped".into(),
        Value::from(v.health.as_ref().map_or(0, |h| h.blocks_dropped)),
    );
    let (last, worst) = v
        .health
        .as_ref()
        .map_or((None, None), |h| (h.pass_last_ms, h.pass_worst_ms));
    m.insert("pass_ms".into(), pass_json(last, worst));
    m
}

/// The **Survey** view: the survey line on `points` (its own, or drawn).
pub fn survey_view(points: &[(f64, f64)], unit: &str, arrays: &mut Arrays) -> Value {
    views::spectrum(
        arrays,
        "Survey",
        &Axis::new("Frequency", Some("Hz"), Scale::Log),
        &Axis::new("Level", Some(unit), Scale::Log),
        vec![Series::new(
            "Survey",
            Role::Measured,
            points.iter().map(|p| p.0).collect(),
            points.iter().map(|p| p.1).collect(),
        )],
        Some("frequency"),
    )
}

/// The **Run** reading.
pub fn run_reading(v: &ExplorerView) -> Value {
    let info = Status::Info;
    let n = |label: &str, x: Option<f64>, unit: &str| match x {
        Some(x) => Row::number(label, x, Some(unit), info),
        None => Row::text(label, "none", info),
    };
    let sweep = match &v.sweep {
        None => "none".to_owned(),
        Some(s) if s.halted => "parked".to_owned(),
        Some(s) => format!(
            "{} at {} oct/min",
            s.direction,
            crate::protocol::num(s.rate_oct_per_min)
        ),
    };
    let control_rms = v
        .control
        .as_ref()
        .and_then(|c| v.channels.iter().find(|ch| ch.index == c.index))
        .and_then(|ch| ch.rms);
    let motion = v.control.as_ref();
    reading(
        "Run",
        vec![
            n("Frequency", v.frequency_hz, "Hz"),
            n("Target", v.target_hz, "Hz"),
            n("Measured", v.measured_hz, "Hz"),
            n("Drive", v.drive_vpk, "V"),
            n("Target drive", v.target_vpk, "V"),
            Row::number("Ceiling", v.max_drive_vpk, Some("V"), info),
            Row::text("Sweep", &sweep, info),
            n("Control rms", control_rms, &v.control_unit),
            n("Velocity", motion.and_then(|m| m.velocity_ips), "in/s"),
            n(
                "Displacement",
                motion.and_then(|m| m.displacement_mil),
                "mil",
            ),
            n("Peak", v.peak.map(|p| p.0), "Hz"),
            n("Pipeline", v.pipeline_frames.map(|p| p as f64), "frames"),
        ],
    )
}

/// A `reading` view with its numbers as the binary header writes them: a whole number
/// as an integer, so every language's writer gives the same bytes.
fn reading(title: &str, rows: Vec<Row>) -> Value {
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let mut e = Map::new();
            e.insert("label".into(), Value::from(r.label));
            e.insert(
                "value".into(),
                match r.value {
                    views::RowValue::Number(x) => num(x),
                    views::RowValue::Text(s) => Value::from(s),
                },
            );
            e.insert("unit".into(), r.unit.map_or(Value::Null, Value::from));
            e.insert("status".into(), Value::from(r.status.as_str()));
            Value::Object(e)
        })
        .collect();
    json!({"view": "reading", "title": title, "rows": rows})
}

/// The `explorer.live` frame: the head, then the survey drawn and the run reading.
pub fn live_json(v: &ExplorerView, drawn: &[(f64, f64)], arrays: &mut Arrays) -> Value {
    let mut m = live_head(v);
    let views = vec![survey_view(drawn, &v.control_unit, arrays), run_reading(v)];
    m.insert("views".into(), Value::Array(views));
    Value::Object(m)
}

/// One acquired channel's latest display window: `Channel <i>: <description>`.
pub fn channel_view(
    index: u32,
    description: &str,
    unit: &str,
    df_hz: f64,
    mags: &[f64],
    arrays: &mut Arrays,
) -> Value {
    let x: Vec<f64> = (0..mags.len()).map(|k| k as f64 * df_hz).collect();
    views::spectrum(
        arrays,
        &format!("Channel {index}: {description}"),
        &Axis::new("Frequency", Some("Hz"), Scale::Linear),
        &Axis::new("Amplitude", Some(unit), Scale::Linear),
        vec![Series::new(
            &format!("Channel {index}"),
            Role::Measured,
            x,
            mags.to_vec(),
        )],
        Some("frequency"),
    )
}

/// The **Time** view: the control row's samples from `first_frame`, x station time.
pub fn time_view(
    index: u32,
    unit: &str,
    fs_hz: f64,
    first_frame: u64,
    samples: &[f64],
    arrays: &mut Arrays,
) -> Value {
    let x: Vec<f64> = (0..samples.len())
        .map(|i| (first_frame + i as u64) as f64 / fs_hz)
        .collect();
    views::spectrum(
        arrays,
        "Time",
        &Axis::new("Time", Some("s"), Scale::Linear),
        &Axis::new("Amplitude", Some(unit), Scale::Linear),
        vec![Series::new(
            &format!("Channel {index}"),
            Role::Measured,
            x,
            samples.to_vec(),
        )],
        None,
    )
}

/// The survey line drawn on `n` log-spaced points across `band`, joined in log
/// frequency between its points, and nothing outside the span visited. A single point
/// is carried as itself.
pub fn draw_survey(points: &[(f64, f64)], band: (f64, f64), n: usize) -> Vec<(f64, f64)> {
    if points.len() < 2 || n < 2 || !(band.0 > 0.0 && band.1 > band.0) {
        return points.to_vec();
    }
    let (lo, hi) = (band.0.ln(), band.1.ln());
    let (first, last) = (points[0].0, points[points.len() - 1].0);
    let mut out = Vec::new();
    let mut seg = 0usize;
    for i in 0..n {
        let f = (lo + (hi - lo) * i as f64 / (n - 1) as f64).exp();
        if f < first || f > last {
            continue;
        }
        while seg + 2 < points.len() && points[seg + 1].0 < f {
            seg += 1;
        }
        let (fa, va) = points[seg];
        let (fb, vb) = points[seg + 1];
        let t = if fb > fa {
            (f.ln() - fa.ln()) / (fb.ln() - fa.ln())
        } else {
            0.0
        };
        out.push((f, va + (vb - va) * t));
    }
    out
}

/// The `explorer.survey` result: the points (null when `since` is the revision) and the
/// peak. Levels to six significant figures, frequencies to the millihertz.
pub fn survey_result(
    revision: u64,
    channel: Option<u32>,
    unit: Option<&str>,
    points: &[(f64, f64)],
    since: Option<u64>,
) -> Value {
    use crate::sine::profile::sig;
    let peak = points
        .iter()
        .copied()
        .fold(None, |best: Option<(f64, f64)>, p| match best {
            Some(b) if b.1 >= p.1 => Some(b),
            _ => Some(p),
        });
    let pts = if since == Some(revision) {
        Value::Null
    } else {
        Value::Array(
            points
                .iter()
                .map(|(f, l)| json!([num(crate::explorer::config::round(*f, 3)), num(sig(*l, 6))]))
                .collect(),
        )
    };
    json!({
        "revision": revision,
        "channel": channel,
        "unit": unit,
        "points": pts,
        "peak": peak.map_or(Value::Null, |(f, l)| json!({
            "hz": num(crate::explorer::config::round(f, 3)),
            "level": num(sig(l, 6)),
        })),
    })
}
