/**
 * Time waveform replication on the wire (schema/PROTOCOL.md, "Time Waveform Replication"): one
 * whole record at a time, the loop closed between records, the drive in volts. The run's law is
 * chosen from the control-law catalog's `twr` world (PROTOCOL.md, "Control Laws"). Where a part
 * of the wire is Random's or classical shock's, the type is theirs.
 */
import type {
	ClockDomain,
	DataSource,
	FeasibilityVerdict,
	ItemReason,
	LadderMode,
	LogEntry,
	RefusalReason,
	ReportItem,
	RunRecordRef,
	StationRef,
	View
} from './types';
import type { LawEntry } from './mimo';
import type { ShockLawChoice } from './shock';

/** Event topics of TWR. */
export const TWR_TOPICS = {
	state: 'twr.state',
	log: 'twr.log',
	live: 'twr.live',
	data: 'twr.data'
} as const;

/** Refusals of the `twr.*` methods: the shared ones and TWR's own. */
export type TwrRefusalReason =
	| RefusalReason
	| 'wrong_mode'
	| 'drive_ceiling'
	| 'reference_unreadable'
	| 'not_converged'
	| 'coherence_hold';

// --- references ----------------------------------------------------------------------------

/** `twr.reference.import`: a file by its full path, or pasted text. */
export type TwrReferenceImportParams = ({ path: string } | { text: string }) & {
	/** Required for a delimited record without a time column. */
	fs_hz?: number;
	/** The data column of a delimited file, or the channel of a WAV file, from 0. */
	column?: number;
	/** The record's unit; default `g`. It must be the control row's. */
	units?: string;
	/** Default: the file's name, or `pasted`. */
	name?: string;
};

export type TwrReferenceFormat = 'csv' | 'tsv' | 'wav' | 'json' | 'text';

/** A stored reference, kept by its SHA-256. */
export interface TwrReferenceFacts {
	sha256: string;
	/** Null for pasted text. */
	path: string | null;
	name: string;
	format: TwrReferenceFormat;
	fs_hz: number;
	samples: number;
	duration_s: number;
	units: string;
	peak: number;
	rms: number;
}

export interface TwrReferenceImportResult {
	reference: TwrReferenceFacts;
	warnings: TwrReportItem[];
	/** Reference (time). */
	views: View[];
}

/** `twr.reference.preview`: a stored reference prepared as a configuration would. */
export interface TwrReferencePreviewParams {
	reference: { sha256: string };
	prep?: TwrPrep;
	section?: TwrSection | null;
	pads?: TwrPads;
	acquisition?: TwrAcquisition;
	correction?: TwrCorrection;
	feasibility?: TwrFeasibility;
}

export interface TwrReferencePreviewResult {
	derived: TwrPreviewDerived;
	/** Section, Spectrum, Velocity, Displacement. */
	views: View[];
}

// --- configuration -------------------------------------------------------------------------

export interface TwrReferenceChoice {
	sha256: string;
	/** The file it was imported from; it must still have the SHA-256 when given. */
	path?: string | null;
}

export type TwrDetrend = 'none' | 'mean' | 'linear';

export interface TwrPrep {
	auto_resample?: boolean;
	/** Default `mean`. */
	detrend?: TwrDetrend;
	/** Hz; null for none. Default 5. */
	highpass_hz?: number | null;
	scale?: number;
	/** A fraction of the reference per end; null for none. Default 0.02. */
	taper_frac?: number | null;
}

/** Samples of the prepared reference to replicate. */
export interface TwrSection {
	start: number;
	len: number;
}

export interface TwrPads {
	pre?: number;
	post?: number;
}

export interface TwrAcquisition {
	/** Default: the reference's own rate. */
	fs_hz?: number | null;
}

export type TwrExcitation = 'random' | 'log_chirp';

export interface TwrIdentification {
	excitation?: TwrExcitation;
	drive_vrms?: number;
	duration_s?: number;
	nfft?: number;
	band_hz?: [number, number];
	min_mean_coherence?: number;
	review?: boolean;
}

export interface TwrInverse {
	tikhonov_floor?: number;
	coherence_floor?: number;
}

export interface TwrCorrection {
	/** Default: the identification's band; inside it. */
	band_hz?: [number, number];
	ref_floor_db?: number;
	min_band_energy?: number;
}

/** Every field optional; the defaults are in PROTOCOL.md. */
export interface TwrControl {
	law?: ShockLawChoice;
	law_budget?: number;
	law_misses?: number;
	max_peak_rise_db?: number;
	max_bin_rise_db?: number;
	first_drive_ratio?: number;
	alignment_frames?: number;
}

export type TwrGateFailPolicy = 'abort' | 'hold';

export interface TwrSchedule {
	mode?: LadderMode;
	levels_db?: number[];
	gate_pct?: number;
	max_iterations?: number;
	on_gate_fail?: TwrGateFailPolicy;
}

/** The playback once full level has gated. */
export type TwrRun =
	| { schedule: 'once' }
	| { schedule: 'loop'; count: number }
	| { schedule: 'duration'; duration_s: number };

export interface TwrAdaptation {
	during_playback?: boolean;
}

export interface TwrLimits {
	drive_vpk_ceiling: number;
	drive_clip_vpk?: number | null;
	drive_vrms_limit?: number | null;
	response_rms_limit?: number | null;
	envelope_alarm_frac?: number;
	envelope_abort_frac?: number;
	envelope_debounce_samples?: number;
	divergence_plays?: number;
	divergence_rise_frac?: number;
}

export interface TwrFeasibility {
	peak_accel?: number | null;
	peak_velocity_ips?: number | null;
	disp_pkpk_in?: number | null;
	kinematics_hp_hz?: number;
}

export interface TwrConfig {
	title: string;
	reference: TwrReferenceChoice;
	prep?: TwrPrep;
	section?: TwrSection | null;
	pads?: TwrPads;
	acquisition?: TwrAcquisition;
	identification?: TwrIdentification;
	inverse?: TwrInverse;
	correction?: TwrCorrection;
	control?: TwrControl;
	schedule?: TwrSchedule;
	run?: TwrRun;
	adaptation?: TwrAdaptation;
	limits: TwrLimits;
	feasibility?: TwrFeasibility;
	/** The run's record, as Random's: the operator's words, what the test is run to, and whether the folder keeps the time histories. */
	operator?: string;
	article?: string;
	notes?: string;
	specification?: { document: string; clause: string } | null;
	record?: { streams?: boolean } | null;
	metadata?: { key: string; label: string; value: string }[];
}

export interface TwrConfigureParams {
	config: TwrConfig;
	dry_run?: boolean;
}

export type TwrItemReason =
	| ItemReason
	| 'reference_unknown'
	| 'reference_changed'
	| 'units_mismatch'
	| 'rate_mismatch'
	| 'section_outside_reference'
	| 'record_too_long'
	| 'band_outside_identification'
	| 'highpass_above_band'
	| 'identification_too_short'
	| 'reference_resampled';

export type TwrReportItem = Omit<ReportItem, 'reason'> & { reason: TwrItemReason };

/** The record every play is played and measured as. */
export interface TwrGrid {
	fs_hz: number;
	record_len: number;
	df_hz: number;
	record_s: number;
	section_start: number;
	section_len: number;
	/** True once the run has armed and the rate is the one the station runs at. */
	actual: boolean;
}

export interface TwrSectionFacts {
	peak: number;
	rms: number;
	duration_s: number;
}

export interface TwrKinematics {
	peak_accel: number;
	peak_velocity_ips: number;
	disp_pkpk_in: number;
	kinematics_hp_hz: number;
}

/** The section's spectral energy the trusted bins can cover, before identification. */
export interface TwrCoverage {
	band_hz: [number, number];
	ref_floor_db: number;
	fraction: number;
	/** `100 sqrt(1 - fraction)`, percent. */
	error_floor_pct: number;
}

export interface TwrFeasibilityCheck {
	name: string;
	value: number;
	limit: number;
	pass: boolean;
}

export interface TwrFeasibilityFacts extends FeasibilityVerdict {
	checks: TwrFeasibilityCheck[];
}

export interface TwrPreviewDerived {
	grid: TwrGrid;
	section: TwrSectionFacts;
	kinematics: TwrKinematics;
	coverage: TwrCoverage;
	feasibility: TwrFeasibilityFacts;
}

export interface TwrDerived extends TwrPreviewDerived {
	reference: { sha256: string; name: string; fs_hz: number; units: string; resampled: boolean };
	/** One law call's time budget, seconds. */
	call_budget_s: number;
	/** The playback plays the run schedule asks for. */
	plays_target: number;
	identification_s: number;
}

export interface TwrConfigureResult {
	state: TwrState;
	derived: TwrDerived;
	warnings: TwrReportItem[];
}

export interface TwrDryRunResult {
	errors: TwrReportItem[];
	warnings: TwrReportItem[];
	derived: TwrDerived | null;
	views: View[];
}

/** `twr.config`: what the present run was configured with; all null when unconfigured. */
export interface TwrConfigResult {
	run_id: string | null;
	config: TwrConfig | null;
	derived: TwrDerived | null;
	views: View[];
}

// --- running -------------------------------------------------------------------------------

export interface TwrArmParams {
	/** The operator's acknowledgement to arm past a feasibility exceedance; logged verbatim. */
	override_ack?: string;
}

export type TwrLevelParams = { delta_db: number } | { rung: number };

export interface TwrLevelResult {
	level_db: number;
}

export interface TwrFreezeParams {
	frozen: boolean;
}

export interface TwrFreezeResult {
	frozen: boolean;
}

export type TwrRunState =
	| 'unconfigured'
	| 'configured'
	| 'blocked'
	| 'armed'
	| 'running'
	| 'review'
	| 'held'
	| 'stopping'
	| 'finished';

export type TwrPhase =
	| 'idle'
	| 'feasibility_blocked'
	| 'armed'
	| 'sysid'
	| 'iterating'
	| 'playing'
	| 'waiting'
	| 'holding'
	| 'awaiting_review'
	| 'coherence_hold'
	| 'stopping'
	| 'stopped'
	| 'aborted';

export type TwrOutcome =
	| 'completed'
	| 'operator_stop'
	| 'operator_abort'
	| 'safe_stop'
	| 'aborted'
	| 'not_converged'
	| 'law_fault'
	| 'fault';

/** Why a TWR law call was not used. */
export type TwrLawFaultReason =
	| 'error'
	| 'panicked'
	| 'over_budget'
	| 'over_memory'
	| 'wrong_length'
	| 'non_finite'
	| 'undeclared_value'
	| 'too_many_values'
	| 'peak_rise'
	| 'bin_rise'
	| 'predicted_peak'
	| 'first_drive';

export type TwrTrip = { play: number | null; at_level_db: number | null } & (
	| { kind: 'envelope'; in_play: boolean; worst_frac: number; abort_frac: number; samples: number; at_sample: number }
	| { kind: 'drive_vpk'; vpk: number; ceiling_vpk: number; predicted: boolean }
	| { kind: 'drive_rms'; vrms: number; limit_vrms: number }
	| { kind: 'response_rms'; rms: number; limit: number }
	| {
			kind: 'alignment';
			lag_frames: number;
			identification_lag_frames: number;
			drift_frames: number;
			limit_frames: number;
	  }
	| { kind: 'coverage'; coverage: number; min_band_energy: number; error_floor_pct: number }
	| { kind: 'divergence'; plays: number; error_pct: number }
	| { kind: 'not_converged'; iterations: number; error_pct: number; gate_pct: number }
	| { kind: 'law_fault'; reason: TwrLawFaultReason; consecutive: number }
);

export interface TwrEnd {
	outcome: TwrOutcome;
	message: string;
	/** Station time. */
	at_s: number | null;
	level_db: number | null;
	/** The play the end came at; null before any play. */
	play: number | null;
	/** Frames of that play the station had taken when a stop or an abort cancelled it. */
	frames_done: number | null;
	/** Present when `outcome` is `aborted`, `not_converged` or `law_fault`. */
	trip: TwrTrip | null;
}

export interface TwrPlaysCount {
	/** Plays judged over the run, identification not counted. */
	done: number;
	at_level: number;
	/** The most correction plays at this level (`max_iterations`). */
	of_level: number;
	playback: number;
	of_playback: number;
}

export interface TwrIdentificationFacts {
	excitation: TwrExcitation;
	drive_vrms: number;
	drive_vpk: number;
	/** The mean in-band coherence measured; null at a hold for the lag (no coherence measured). */
	coherence_mean: number | null;
	min_mean_coherence: number;
	/** The trusted bins' coverage of the reference's energy; null at the coherence hold. */
	coverage: number | null;
	/** Null when no lag was found. */
	lag_frames: number | null;
	lag_source: 'measured' | 'derived';
	passed: boolean;
}

/** The drive in force at the present level against the ceiling. */
export interface TwrPrediction {
	drive_vpk: number;
	drive_vpk_ceiling: number;
	level_db: number;
}

export interface TwrLawRef {
	id: string;
	name: string;
	version: string;
	sha256: string | null;
	/** Unusable calls in a row now. */
	misses: number;
}

export type TwrPlayKind = 'iteration' | 'playback';

/** One play (`twr.plays`, `twr.record`, `last_play`). */
export interface TwrPlayRow {
	index: number;
	kind: TwrPlayKind;
	level_db: number;
	rung: number | null;
	/** From 1 at its level; null for a playback play. */
	iteration: number | null;
	rms_error_pct: number;
	peak_error_pct: number;
	spectral_error_db: number;
	gated: boolean;
	clipped_samples: number;
	clamped_bins: number;
	drive_vpk: number;
	response_peak: number;
	/** The play's fitted lag, frames; null until the run's alignment check has measured it. */
	lag_frames: number | null;
	/** Its drift from the identification's lag, frames; null until measured, as `lag_frames`. */
	drift_frames: number | null;
	start_frame: number;
	frames: number;
	/** The play's start, station time. */
	at_s: number;
	/** The call that produced this play's drive; null when no call was behind it. */
	law: {
		used: boolean;
		miss: TwrLawFaultReason | null;
		call_ms: number;
		peak_rise_db: number | null;
		bin_rise_db: number | null;
		predicted_peak_ratio: number | null;
		values: Record<string, number>;
	} | null;
}

/** The `twr.state` result and event. */
export interface TwrState {
	run_id: string | null;
	state: TwrRunState;
	phase: TwrPhase;
	title: string | null;
	source: DataSource | null;
	station: StationRef | null;
	grid: TwrGrid | null;
	clock: ClockDomain | null;
	mode: LadderMode | null;
	levels_db: number[] | null;
	/** The commanded level; null before the run starts. */
	level_db: number | null;
	rung: number | null;
	plays: TwrPlaysCount;
	frozen: boolean;
	/** Station time since the run armed. */
	t_s: number | null;
	alarm: boolean;
	identification: TwrIdentificationFacts | null;
	prediction: TwrPrediction | null;
	last_play: TwrPlayRow | null;
	law: TwrLawRef | null;
	feasibility: FeasibilityVerdict | null;
	end: TwrEnd | null;
	/** The run's record, from arm on: its folder, whether it keeps the time histories, the latest problem. */
	record?: RunRecordRef;
}

export interface TwrLogResult {
	entries: LogEntry[];
	next: number;
}

export interface TwrLogEvent {
	run_id: string;
	entries: LogEntry[];
}

export interface TwrPlaysParams {
	since?: number;
}

export interface TwrPlaysResult {
	plays: TwrPlayRow[];
	next: number;
}

export interface TwrRecordParams {
	index: number;
	/** The width the records are decimated to (64 to 8192); 0 sends every sample. */
	points?: number;
}

export interface TwrRecordResult {
	play: TwrPlayRow;
	/** Replication, Error, Drive. */
	views: View[];
}

// --- display -------------------------------------------------------------------------------

export type TwrQuantity =
	| 'target'
	| 'response'
	| 'error'
	| 'drive'
	| 'envelope'
	| 'frf'
	| 'coherence'
	| 'mask'
	| 'spectra'
	| 'convergence'
	| 'law'
	| 'scope'
	| 'readings'
	| 'plays';

/** The quantities that take `points`. */
export const TWR_POINTS_QUANTITIES: readonly TwrQuantity[] = [
	'target',
	'response',
	'error',
	'drive',
	'envelope',
	'scope'
];

export interface TwrSubscribePlot {
	id: string;
	quantity: TwrQuantity;
	points?: number;
}

export type TwrSubscribeParams = { rate_hz?: number; plots: TwrSubscribePlot[] } | { default: true };

export interface TwrSubscribeResult {
	default: boolean;
	rate_hz: number;
	plots: TwrSubscribePlot[] | null;
	quantities: TwrQuantity[];
}

/** The play in flight. */
export interface TwrPlayInFlight {
	index: number;
	kind: TwrPlayKind;
	iteration: number | null;
	frames_done: number;
	frames: number;
}

export interface TwrLastMetrics {
	rms_error_pct: number;
	peak_error_pct: number;
	spectral_error_db: number;
	gated: boolean;
}

/** The `twr.live` event, 5 times a second while the run is armed through finished. */
export interface TwrLive {
	run_id: string;
	source: DataSource;
	state: TwrRunState;
	phase: TwrPhase;
	mode: LadderMode | null;
	t_s: number;
	level_db: number | null;
	rung: number | null;
	plays: TwrPlaysCount;
	/** Null between plays. */
	play: TwrPlayInFlight | null;
	/** Null before any play. */
	last: TwrLastMetrics | null;
	/** Scope. */
	views: View[];
}

/** One plot's data in a `twr.data` frame. */
export interface TwrDataPlot {
	id: string;
	quantity: TwrQuantity;
	views: View[];
}

/** The `twr.data` event: a subscribed connection's own frame. */
export interface TwrData extends Omit<TwrLive, 'views'> {
	plots: TwrDataPlot[];
}

// --- the law catalog and check (PROTOCOL.md, "Control Laws") ------------------------------

export type TwrCheckRequirement =
	| 'first_play'
	| 'every_rung'
	| 'first_rung_by'
	| 'full_level'
	| 'survive';

/** How a check case's run ended. */
export type TwrCheckEnd =
	| 'completed'
	| 'not_converged'
	| 'aborted'
	| 'law_fault'
	| 'held'
	| 'fault';

/** One simulated test article's result in a TWR law check. */
export interface TwrLawCheckCase {
	name: string;
	plant: string;
	requirement: TwrCheckRequirement;
	passed: boolean;
	detail: string;
	end: TwrCheckEnd;
	/** The trip's kind, or the law fault's reason. */
	trip: string | null;
	plays: number;
	rungs: { level_db: number; plays_to_gate: number | null }[];
	full_level_error_pct: number | null;
	lag_frames: number | null;
	lag_measured: boolean;
	calls: number;
	misses: number;
	refused: number;
	clamped_bins: number;
	worst_peak_rise_db: number | null;
	worst_bin_rise_db: number | null;
	worst_call_ms: number | null;
	budget_ms: number;
	max_drive_vpk: number;
}

export interface TwrLawsCheckResult {
	/** The law's entry after the check: a law file's `check` now holds the record. */
	law: LawEntry;
	report: { passed: boolean; recorded: boolean; tool_version: string; cases: TwrLawCheckCase[] };
}
