/**
 * Explorer on the wire (schema/PROTOCOL.md, "Explorer"): one sine tone the operator steers by
 * hand, its frequency, its level in volts peak at the drive output and a sweep toward either band
 * edge, with no control loop. The service remembers the response level at each frequency visited
 * (the survey line). Where a part of the wire is Random's, the type is theirs.
 */
import type {
	ChannelRole,
	ClockDomain,
	DataSource,
	ItemReason,
	LogEntry,
	ReadingView,
	RefusalReason,
	ReportItem,
	RunRecordRef,
	SpectrumView,
	StationClass,
	StationRef,
	View
} from './types';

/** Event topics of Explorer. */
export const EXPLORER_TOPICS = {
	state: 'explorer.state',
	log: 'explorer.log',
	live: 'explorer.live',
	data: 'explorer.data'
} as const;

/** Refusals of the `explorer.*` methods: the shared ones and Explorer's own. */
export type ExplorerRefusalReason = RefusalReason | 'level_zero';

// --- configuration -------------------------------------------------------------------------

export interface ExplorerAcquisition {
	/** The rate asked of the station, 1000 to 204800 Hz; null asks for 8192 Hz. */
	fs_hz?: number | null;
}

/** Every field optional; the defaults are in PROTOCOL.md. */
export interface ExplorerSlew {
	/** Octaves per second, default 1: the fastest a sweep moves the tone. */
	freq_oct_per_s?: number;
	/** dB per second, default 6: every level change and the stop ramp at it. */
	drive_db_per_s?: number;
}

export interface ExplorerDisplay {
	/** A power of two from 1024 to 16384; default 4096. */
	nfft?: number;
}

export interface ExplorerCapture {
	/** The run folder's name under the local data root; default `explorer`. */
	run_name?: string;
}

export interface ExplorerConfig {
	title?: string;
	acquisition?: ExplorerAcquisition;
	/** `[lo, hi]` Hz; default `[10, 2000]`. */
	band_hz?: [number, number];
	/** Inside the band; default 100. */
	start_hz?: number;
	/** The drive ceiling, volts peak; required (the wire has no default). */
	max_drive_vpk: number;
	slew?: ExplorerSlew;
	display?: ExplorerDisplay;
	capture?: ExplorerCapture;
	/** Random's fields; `label` is the key when absent. */
	metadata?: { key: string; label?: string; value: string }[];
	/** Random's words for the run's record: who ran it, on what, and notes. */
	operator?: string;
	article?: string;
	notes?: string;
	/** The specification the run was made to. */
	specification?: { document: string; clause: string } | null;
	/** Whether the run's record keeps its time histories; default true. */
	record?: { streams?: boolean } | null;
}

export interface ExplorerConfigureParams {
	config: ExplorerConfig;
	dry_run?: boolean;
}

export type ExplorerItemReason =
	| ItemReason
	| 'ceiling_over_range'
	| 'safe_stop_budget'
	| 'extra_drive_channels'
	| 'no_input_channel';

export type ExplorerReportItem = Omit<ReportItem, 'reason'> & { reason: ExplorerItemReason };

/** The station's working figures at the requested rate; the station's own once armed. */
export interface ExplorerDerivedGrid {
	fs_hz: number;
	block_frames: number;
	lookahead_frames: number;
	/** True once the run has armed and the figures are the station's. */
	actual: boolean;
}

export interface ExplorerDerived {
	grid: ExplorerDerivedGrid;
	band_hz: [number, number];
	/** Input frames that still carry the tone from before a change when it is made. */
	pipeline_frames: number;
	pipeline_s: number;
	/** A rise from 0 V and the stop, seconds at the drive slew. */
	rise_s: number;
	stop_s: number;
	/** How long the service waits for a stop. */
	stop_wait_s: number;
	/** 60 times the frequency slew. */
	sweep_max_oct_per_min: number;
	display: { df_hz: number; window_s: number };
	survey: {
		tolerance_hz: number;
		/** At the band's low and high edges, seconds after a change of frequency. */
		first_point_s: [number, number];
	};
}

export interface ExplorerConfigureResult {
	state: ExplorerState;
	derived: ExplorerDerived;
	warnings: ExplorerReportItem[];
}

export interface ExplorerDryRunResult {
	errors: ExplorerReportItem[];
	warnings: ExplorerReportItem[];
	/** Null with errors. */
	derived: ExplorerDerived | null;
}

/** `explorer.config`: what the present run was configured with; all null when unconfigured. */
export interface ExplorerConfigResult {
	run_id: string | null;
	config: ExplorerConfig | null;
	derived: ExplorerDerived | null;
}

// --- steering ------------------------------------------------------------------------------

export type ExplorerSweepDirection = 'up' | 'down';

/** Absolute, added to the target frequency, or the target times 2 to that power. */
export type ExplorerFrequencyPart = { hz: number } | { step_hz: number } | { step_oct: number };

/** Absolute volts peak, or dB from the target level (refused `level_zero` from 0 V). */
export type ExplorerLevelPart = { vpk: number } | { step_db: number };

/** One part, or one of each; both parts land in the same generated frame. */
export type ExplorerSteerParams =
	| ExplorerFrequencyPart
	| ExplorerLevelPart
	| (ExplorerFrequencyPart & ExplorerLevelPart);

/** What the service adopted: the targets in force after the steer. */
export interface ExplorerSteerResult {
	hz: number;
	vpk: number;
	/** True where a part was clamped; false for a part not sent. */
	clamped: { hz: boolean; vpk: boolean };
	/** The generated frame the change first appears in. */
	frame: number;
	/** The input frames acquired when the service took the change. */
	input_frame: number;
}

export interface ExplorerSweepParams {
	direction: ExplorerSweepDirection;
	/** Above 0; clamped to `derived.sweep_max_oct_per_min`. */
	rate_oct_per_min: number;
}

export interface ExplorerSweepResult {
	direction: ExplorerSweepDirection;
	/** The rate used. */
	rate_oct_per_min: number;
	clamped: boolean;
	frame: number;
	input_frame: number;
}

/** `explorer.hold`: where the tone now holds. */
export interface ExplorerHoldResult {
	hz: number;
	frame: number;
	input_frame: number;
}

// --- run state -----------------------------------------------------------------------------

export type ExplorerRunState = 'unconfigured' | 'configured' | 'starting' | 'running' | 'stopping' | 'finished';

export type ExplorerPhase =
	| 'idle'
	| 'priming'
	| 'quiet'
	| 'ramping'
	| 'sweeping'
	| 'at_edge'
	| 'steady'
	| 'ramping_down'
	| 'stopped'
	| 'aborted'
	| 'faulted';

export type ExplorerOutcome =
	| 'operator_stop'
	| 'safe_stop'
	| 'limit_stop'
	| 'operator_abort'
	| 'aborted'
	| 'fault'
	| 'arm_refused';

/** Why a stop began. */
export type ExplorerStopCause = 'operator' | 'safe_stop' | 'shutdown' | 'limit';

export type ExplorerTrip = { at_hz: number } & (
	| {
			kind: 'watchdog';
			channel: number;
			axis: 'accel' | 'velocity' | 'displacement';
			value: number;
			limit: number;
	  }
	| { kind: 'drive_vpk'; vpk: number; ceiling_vpk: number }
	| { kind: 'limit'; channel: number; axis: 'velocity' | 'displacement'; value: number; limit: number }
);

export interface ExplorerEnd {
	outcome: ExplorerOutcome;
	message: string;
	/** Station time from start. */
	at_s: number | null;
	/** The tone when the end began. */
	at_hz: number | null;
	vpk: number | null;
	/** The controlled stop's length on the station's clock; null for an abort and a fault. */
	ramp_s: number | null;
	/** Present when `outcome` is `aborted` or `limit_stop`. */
	trip: ExplorerTrip | null;
}

export interface ExplorerSweep {
	direction: ExplorerSweepDirection;
	rate_oct_per_min: number;
	/** Parked at its edge, or a frequency was set by hand. */
	halted: boolean;
}

export interface ExplorerStateGrid {
	requested_fs_hz: number;
	/** The rate the station programmed once armed; the request until then. */
	fs_hz: number;
	block_frames: number;
	lookahead_frames: number;
	actual: boolean;
}

export interface ExplorerPassMs {
	last: number | null;
	worst: number | null;
}

export interface ExplorerHealth {
	engine_ticks: number;
	worst_tick_ms: number | null;
	budget_ms: number | null;
	health_faults: number;
	ring_overruns: number;
	blocks_dropped: number;
	driver_warnings: number;
	pass_ms: ExplorerPassMs;
	/** The longest gap between two batches. */
	batch_gap_ms: number | null;
}

/** The service's loop has missed three of its periods; the run may still be driving. */
export interface ExplorerStall {
	stale_s: number;
	period_s: number;
	message: string;
}

/** The `explorer.state` result and event. */
export interface ExplorerState {
	run_id: string | null;
	state: ExplorerRunState;
	phase: ExplorerPhase;
	title: string | null;
	source: DataSource | null;
	station: StationRef | null;
	grid: ExplorerStateGrid | null;
	clock: ClockDomain | null;
	band_hz: [number, number] | null;
	max_drive_vpk: number | null;
	slew: { freq_oct_per_s: number; drive_db_per_s: number } | null;
	pipeline_frames: number | null;
	/** Station time since start. */
	t_s: number | null;
	/** The tone now, as generated. */
	frequency_hz: number | null;
	target_hz: number | null;
	drive_vpk: number | null;
	target_vpk: number | null;
	sweep: ExplorerSweep | null;
	health: ExplorerHealth | null;
	stall: ExplorerStall | null;
	stop: ExplorerStopCause | null;
	end: ExplorerEnd | null;
	/** The run's folder from start on, whether it keeps the streams, and the latest problem writing it. */
	record?: RunRecordRef | null;
}

export interface ExplorerLogResult {
	entries: LogEntry[];
	next: number;
}

export interface ExplorerLogEvent {
	run_id: string;
	entries: LogEntry[];
}

// --- the survey line -----------------------------------------------------------------------

export interface ExplorerSurveyParams {
	since?: number;
}

/** `[hz, level]`: the control row's amplitude at the tone, peak, in its unit. */
export type ExplorerSurveyPoint = [number, number];

export interface ExplorerSurveyPeak {
	hz: number;
	level: number;
}

export interface ExplorerSurveyResult {
	revision: number;
	/** The control row's table index and unit. */
	channel: number | null;
	unit: string | null;
	/** Sorted by frequency; null when `since` equals the present revision. */
	points: ExplorerSurveyPoint[] | null;
	peak: ExplorerSurveyPeak | null;
}

export interface ExplorerSurveyClearResult {
	revision: number;
}

// --- display -------------------------------------------------------------------------------

export type ExplorerLiveQuantity = 'survey' | 'spectrum' | 'time' | 'readings';

/** Which channels a `spectrum` plot shows: indexes, or a set the service resolves. */
export type ExplorerChannelSelector = number[] | 'all' | 'control';

export interface ExplorerSubscribePlot {
	id: string;
	quantity: ExplorerLiveQuantity;
	/** `spectrum` only. */
	channels?: ExplorerChannelSelector;
	/** `survey` (null: the survey's own points) and `time` only. */
	points?: number | null;
}

export type ExplorerSubscribeParams = { rate_hz?: number; plots: ExplorerSubscribePlot[] } | { default: true };

export interface ExplorerSubscribeResult {
	default: boolean;
	rate_hz: number;
	plots: ExplorerSubscribePlot[] | null;
	quantities: ExplorerLiveQuantity[];
}

export interface ExplorerLiveChannel {
	index: number;
	description: string;
	role: Exclude<ChannelRole, 'drive'>;
	/** Over the last analysis pass, DC included, in the row's unit. */
	rms: number | null;
	unit: string;
}

/** The control row's derived motion, each the peak over the last analysis pass. */
export interface ExplorerControlMotion {
	index: number;
	/** Null when the control row's unit is not `g`. */
	velocity_ips: number | null;
	displacement_mil: number | null;
	/** True while the integrators' start-up transient lasts. */
	settling: boolean;
}

/** The numbers every live frame carries, `explorer.live` and `explorer.data` alike. */
export interface ExplorerLiveHead {
	run_id: string;
	source: DataSource;
	class: StationClass;
	state: 'running' | 'stopping' | 'finished';
	phase: ExplorerPhase;
	/** Station time since start. */
	t_s: number | null;
	frequency_hz: number | null;
	target_hz: number | null;
	/** Measured on the control row; null until a tone above 0 V has held past the pipeline. */
	measured_hz: number | null;
	drive_vpk: number | null;
	target_vpk: number | null;
	max_drive_vpk: number;
	sweep: ExplorerSweep | null;
	channels: ExplorerLiveChannel[];
	control: ExplorerControlMotion | null;
	pipeline_frames: number | null;
	peak: ExplorerSurveyPeak | null;
	blocks_dropped: number;
	pass_ms: ExplorerPassMs;
}

/** The `explorer.live` event: the survey and run views plus the numbers behind them. */
export interface ExplorerLive extends ExplorerLiveHead {
	/** Survey, run readings, in that order. */
	views: [SpectrumView, ReadingView];
}

/** One plot's data in an `explorer.data` frame. */
export interface ExplorerDataPlot {
	id: string;
	quantity: ExplorerLiveQuantity;
	/** The resolved channels of a `spectrum` plot, in table order; null for the rest. */
	channels: number[] | null;
	/** The input frame of a `time` plot's first sample; null for the rest. */
	first_frame: number | null;
	views: View[];
}

/** The `explorer.data` event: a subscribed connection's own frame. */
export interface ExplorerData extends ExplorerLiveHead {
	plots: ExplorerDataPlot[];
}
