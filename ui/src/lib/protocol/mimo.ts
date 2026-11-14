/**
 * MIMO Random and the control-law catalog on the wire (schema/PROTOCOL.md, "MIMO Random" and
 * "Control Laws"). Matrices travel packed: one Hermitian matrix per line as its upper triangle,
 * row by row, lines in order, real and imaginary parts as separate arrays.
 */
import type {
	BandDb,
	DataSource,
	Grid,
	LadderMode,
	RandomRunState,
	ReportItem,
	RunEnd,
	RunRecordRef,
	Schedule,
	StationRef,
	View
} from './types';

/** Event topics of MIMO Random. */
export const MIMO_TOPICS = {
	status: 'mimo_random.status',
	validate: 'mimo_random.validate',
	data: 'mimo_random.data',
	report: 'mimo_random.report'
} as const;

// --- setup ---------------------------------------------------------------------------------

export interface MimoTransform {
	rows: number;
	cols: number;
	/** Row-major. */
	values: number[];
}

export interface MimoControlRow {
	channel: number;
	alarm_db?: BandDb;
	abort_db?: BandDb;
}

export interface MimoDriveRow {
	channel: number;
	ceiling_vrms: number;
	abort_vrms?: number | null;
	/** The output's peak, V; the stream is clipped here. Default 3 times the ceiling, at most 10 V. */
	peak_v?: number;
	/** The shaker record (the shaker database's form) for feasibility; null: not evaluated. */
	shaker?: Record<string, unknown> | null;
	/** This shaker's own moving mass (armature, adapter, fixturing), lb. */
	moving_mass_lb?: number;
}

/** A monitored response the core holds under a ceiling (an enabled monitor row). */
export interface MimoLimitRow {
	channel: number;
	/** The PSD ceiling at full level, units squared per hertz. */
	ceiling: number;
	/** Abort this many dB above the ceiling (default 3). */
	abort_margin_db?: number;
}

/** A controlled quantity's alarm and abort bands, `[below, above]` in positive dB. */
export interface MimoBands {
	alarm_db?: BandDb;
	abort_db?: BandDb;
}

export type MimoSpace =
	| { kind: 'physical' }
	| {
			kind: 'virtual';
			dofs: string[];
			response_transform: MimoTransform;
			drive_transform?: MimoTransform | null;
			/** One per degree of freedom, in their order: the tolerances live in the control space. */
			dof_bands?: MimoBands[];
	  };

/** Inline for small references; a native reference file, by path and SHA-256, for large ones. */
export type MimoReference =
	| { kind: 'inline'; freqs_hz: number[]; n: number; re: number[]; im: number[] }
	| { kind: 'file'; path: string; sha256: string };

export interface MimoLawChoice {
	id: string;
	params?: Record<string, unknown>;
}

export interface MimoEstimation {
	fs_hz?: number;
	n_pts?: number;
	frames_per_loop?: number;
	window?: 'hann' | 'rectangular';
	overlap?: number;
	memory_loops?: number;
}

export interface MimoIdentification {
	level_db?: number;
	frames?: number;
	noise_frames?: number;
	coherence_floor?: number;
	condition_limit?: number;
	review?: boolean;
}

export interface MimoControl {
	frf?: 'hold' | 'update';
	/** Under update, the weight of each loop's estimate (above 0, at most 1; default 0.5). */
	frf_blend?: number;
	/** Per-loop rise and fall caps on each drive's PSD, dB (defaults 3.5 and 6). */
	cap_up_db?: number;
	cap_down_db?: number;
	/** The law call's time budget, a fraction of the loop (default 0.25), and the misses in a row that stop the run (default 3). */
	law_budget?: number;
	law_misses?: number;
	level_ramp_s?: number;
	shutdown_ramp_s?: number;
	max_loops_out?: number;
}

export interface MimoSetup {
	title?: string;
	channels: { controls: MimoControlRow[]; drives: MimoDriveRow[]; limits?: MimoLimitRow[]; article_mass_lb?: number };
	space?: MimoSpace;
	reference: MimoReference;
	law?: MimoLawChoice;
	estimation?: MimoEstimation;
	identification?: MimoIdentification;
	control?: MimoControl;
	schedule: Schedule;
	backend?: { kind: 'sim' | 'ni' } | null;
	/** The operator's words for the run's record (its run folder). */
	operator?: string;
	article?: string;
	notes?: string;
	specification?: { document: string; clause: string } | null;
	/** `streams` false: the run's record keeps no time histories. */
	record?: { streams: boolean } | null;
}

export interface MimoSetupParams {
	setup: MimoSetup;
	dry_run?: boolean;
}

// --- validation ----------------------------------------------------------------------------

export type MimoShape = 'square' | 'more_controls_than_drives' | 'fewer_controls_than_drives';

export interface MimoTransformCheck {
	rows: number;
	cols: number;
	rank: number;
	/** Null when the matrix is rank-deficient. */
	condition: number | null;
}

export interface MimoValidation {
	ok: boolean;
	errors: ReportItem[];
	warnings: ReportItem[];
	shape: { controls: number; drives: number; space: 'physical' | 'virtual'; dofs: number | null; shape: MimoShape };
	transforms: { response: MimoTransformCheck | null; drive: MimoTransformCheck | null };
	reference: {
		source: 'inline' | 'file';
		lines: number;
		n: number;
		band_hz: [number, number] | null;
		min_eigenvalue_ratio: number | null;
		rank_min: number | null;
		rank_max: number | null;
		repaired_lines: number | null;
	} | null;
	feasibility: {
		channel: number;
		ceiling_vrms: number;
		verdict: 'pass' | 'advisory' | 'exceeded' | 'not_evaluated';
		predicted_vrms: number | null;
		message: string;
	}[];
	law: { id: string; name: string; version: string; sha256: string | null; admitted: boolean } | null;
}

export interface MimoSetupResult {
	state: MimoStatus;
	validation: MimoValidation;
}

export interface MimoValidateResult {
	validation: MimoValidation | null;
}

// --- running -------------------------------------------------------------------------------

export interface MimoOverrideParams {
	/** The operator's own words; kept with the run. */
	ack: string;
	/** What the words acknowledge; absent: whatever holds the run now. */
	reason?: MimoOverrideReason | null;
}

export interface MimoIterateParams {
	law?: MimoLawChoice | null;
}

export interface MimoLawRef {
	id: string;
	name: string;
	version: string;
	sha256: string | null;
}

export interface MimoPrediction {
	iteration: number;
	law: MimoLawRef;
	drive_rms_v: number[];
	ceiling_vrms: number[];
	error_db: number[];
	drives_pass: boolean;
	/** More controls than drives: only part of the reference is reachable (`pred_sdm`). */
	has_achievable?: boolean;
	/** Per controlled quantity: the achievable reference's rms against the reference's, dB. */
	unreachable_db?: number[];
	/** Per drive: predicted rms acceleration at its shaker at full level, in the controls' unit. */
	drive_accel?: number[];
	/** Per drive: the control position that acceleration is read at. */
	accel_control?: number[];
	/** Per drive: the shaker against that acceleration. */
	feasibility?: { verdict: 'pass' | 'advisory' | 'exceeded' | 'not_evaluated'; message: string }[];
}

/** What an override acknowledges. */
export type MimoOverrideReason = 'gate_failed' | 'prediction_exceeded';

export interface MimoOverride {
	by: string;
	text: string;
	reason?: MimoOverrideReason;
}

export type MimoPhase =
	| 'idle'
	| 'armed'
	| 'sysid_noise'
	| 'sysid'
	| 'awaiting_review'
	| 'gate_failed'
	| 'prediction_exceeded'
	| 'engaged'
	| 'full_level'
	| 'ramping_down'
	| 'stopped'
	| 'aborted';

export interface MimoGates {
	coherence_min: number;
	coherence_floor: number;
	condition_max: number;
	condition_limit: number;
	passed: boolean;
	/** Each controlled quantity's multiple coherence over the band; null for one excluded. */
	coherence?: (number | null)[];
}

export interface MimoShapeInfo {
	controls: number;
	drives: number;
	space: 'physical' | 'virtual';
	dofs: string[] | null;
	shape: MimoShape;
	/** The controlled quantities' names, in order: what a selector's index counts. */
	labels: string[];
	drive_labels: string[];
	/** Each controlled quantity's bands: its control row's, or its degree of freedom's. */
	bands?: { alarm_db: BandDb; abort_db: BandDb }[];
}

/** The `mimo_random.status` result and event. */
export interface MimoStatus {
	run_id: string | null;
	state: RandomRunState;
	phase: MimoPhase;
	title: string | null;
	source: DataSource | null;
	station: StationRef | null;
	grid: Grid | null;
	clock: 'shared' | 'independent' | 'unknown' | null;
	shape: MimoShapeInfo | null;
	mode: LadderMode | null;
	levels_db: number[] | null;
	level_db: number | null;
	elapsed_s: number | null;
	at_level_s: number | null;
	remaining_at_level_s: number | null;
	loop: number;
	alarm: boolean;
	control_error_db: number[];
	gates: MimoGates | null;
	caps: number[];
	drive_rms_v: number[];
	law: MimoLawRef | null;
	faults: { kind: string; message: string }[];
	override: MimoOverride | null;
	/** Every acknowledgement, in order. */
	overrides?: MimoOverride[];
	prediction: MimoPrediction | null;
	/** Control positions taken out of control at the gate (kept as monitors). */
	excluded?: number[];
	end: RunEnd | null;
	/** The run's record from arm on: its folder, whether it keeps the streams, a problem writing it. */
	record?: RunRecordRef | null;
}

export interface MimoReport {
	run_id: string;
	title: string;
	source: DataSource;
	grid: Grid;
	shape: MimoShapeInfo;
	law: MimoLawRef;
	loops: number;
	controls: { label: string; rms: number | null; reference_rms: number | null; error_db: number | null }[];
	drives: { channel: number; label: string; rms_v: number | null; ceiling_vrms: number; min_cap: number | null }[];
	gates: MimoGates | null;
	override: MimoOverride | null;
	overrides?: MimoOverride[];
	end: RunEnd | null;
}

export interface MimoReportResult {
	report: MimoReport | null;
}

// --- the law catalog -----------------------------------------------------------------------

/** Which test a law is for: single-output Random, MIMO Random, classical shock, SRS shock, Sine or TWR. */
export type LawWorld = 'single' | 'mimo' | 'shock_classical' | 'shock_srs' | 'sine' | 'twr' | 'shock_mimo';

export type LawParam = { key: string; label: string; help: string } & (
	| { kind: 'number' | 'per_channel'; unit: string; min: number; max: number; step: number; default: number }
	| { kind: 'integer'; unit: string; min: number; max: number; step: number; default: number }
	| { kind: 'toggle'; default: boolean }
	| { kind: 'choice'; options: string[]; default: string }
	| { kind: 'curve'; unit: string; min: number; max: number; default: [number, number][] }
);

export type LawDiagnosticLayout = 'scalar' | 'per_line' | 'per_control_line' | 'per_drive_line';

export interface LawEntry {
	id: string;
	world: LawWorld;
	builtin: boolean;
	name: string;
	version: string;
	author: string;
	summary: string;
	sha256: string | null;
	check: {
		status: 'built_in' | 'unchecked' | 'passed' | 'failed' | 'unreadable';
		date: string | null;
		tool_version: string | null;
		cases: { name: string; passed: boolean; detail: string }[];
		/** Whether the law may drive hardware; any law runs on the simulator. */
		hardware: boolean;
		message: string;
	};
	params: LawParam[];
	diagnostics: { key: string; label: string; unit: string; help: string; layout: LawDiagnosticLayout | null }[];
	shapes: MimoShape[] | null;
	needs: ('last_drive' | 'response_drive_csd' | 'sysid_response' | 'sysid_drive' | 'sysid_noise')[] | null;
	/** A Sine law's live parameters (the keys `sine.adjust` takes); only a Sine law's entry has it. */
	live?: string[];
}

export interface LawsListParams {
	world?: LawWorld | null;
}

export interface LawsListResult {
	laws: LawEntry[];
	problems: { file: string; reason: string }[];
}

export interface LawsImportParams {
	/** The law file's full path on the computer that runs the test; files are never uploaded. */
	path: string;
}

export interface LawsImportResult {
	law: LawEntry;
}

/** What a law check asks of a case (PROTOCOL.md, "Control Laws", `laws.check`). */
export type LawCheckRequirement = 'converge' | 'settle' | 'survive';

export interface LawsCheckParams {
	/** A catalog id; give this or `path`, not both. */
	id?: string;
	/** A law file's full path on the computer that runs the test; it is imported first. */
	path?: string;
	/** Parameter values by key; the law's defaults fill the rest. */
	params?: Record<string, unknown> | null;
}

/** One simulated test article's result. */
export interface LawCheckCase {
	name: string;
	plant: string;
	requirement: LawCheckRequirement;
	passed: boolean;
	detail: string;
	loops_to_converge: number | null;
	loops_at_full: number;
	final_error_db: number | null;
	worst_line_db: number | null;
	settle_db: number | null;
	trip: string | null;
	misses: number;
	rejected: number;
	repaired_lines: number;
	capped_lines: number;
	calls: number;
	worst_call_ms: number | null;
	budget_ms: number | null;
	gate_passed: boolean;
}

export interface LawsCheckResult {
	/** The law's entry after the check: a law file's `check` now holds the record. */
	law: LawEntry;
	report: { passed: boolean; recorded: boolean; tool_version: string; cases: LawCheckCase[] };
}

/** A `laws.check` event: one as each case starts (`result` null) and one as it ends. */
export interface LawsCheckEvent {
	law_id: string;
	index: number;
	of: number;
	case: string;
	state: 'running' | 'done';
	result: LawCheckCase | null;
}

// --- the display (PROTOCOL.md, "MIMO Random Display") ---------------------------------------

/** What a MIMO plot can show. */
export type MimoQuantity =
	| 'psd'
	| 'coh'
	| 'phase'
	| 'csd_mag'
	| 'error_db'
	| 'coh_error'
	| 'drive_psd'
	| 'drive_coh'
	| 'frf'
	| 'sv'
	| 'cond'
	| 'mcoh'
	| 'cap'
	| 'dof_rms'
	| 'trace'
	| 'pred_drive_rms'
	| 'pred_error_db'
	| 'law_diag'
	| 'sdm'
	| 'readings'
	| 'pred_sdm'
	| 'pred_drive_accel';

/**
 * Which part of a quantity a plot shows. Positions count the setup's controlled quantities (or
 * drives) from 0, in order; the status's `shape.labels` and `shape.drive_labels` name them.
 */
export type MimoSelector =
	| Record<string, never>
	| { index: number }
	| { pair: [number, number] }
	| { rows: 'all' | number[] }
	| { diagonal: true }
	| { block: number[] }
	| { key: string; index?: number };

export type MimoSubscribePlot = { id: string; quantity: MimoQuantity; points?: number } & MimoSelector;

export type MimoSubscribeParams = { rate_hz?: number; plots: MimoSubscribePlot[] } | { default: true };

export interface MimoSubscribeResult {
	default: boolean;
	rate_hz: number;
	plots: (MimoSubscribePlot & { points: number })[] | null;
	quantities: MimoQuantity[];
}

/** One plot's data in a `mimo_random.data` frame. */
export interface MimoDataPlot {
	id: string;
	quantity: MimoQuantity;
	selector: MimoSelector;
	/** What the selector resolved to: the two channels of a pair, the rows of a heatmap. */
	labels: string[];
	views: View[];
}

/** The `mimo_random.data` event: a subscribed connection's own frame. */
export interface MimoData {
	run_id: string;
	source: DataSource;
	state: 'running' | 'review' | 'stopping' | 'finished';
	phase: MimoPhase;
	loop: number;
	elapsed_s: number | null;
	level_db: number | null;
	plots: MimoDataPlot[];
}

/** Where element (r, c) of an n by n Hermitian matrix sits in one packed line (r <= c). */
export function packedIndex(n: number, r: number, c: number): number {
	if (r > c) [r, c] = [c, r];
	return (r * (2 * n - r + 1)) / 2 + (c - r);
}

/**
 * Element (r, c) of line `line` of a packed block, as [re, im]. Below the diagonal it is the
 * conjugate of its mirror.
 */
export function packedElement(re: ArrayLike<number>, im: ArrayLike<number>, n: number, line: number, r: number, c: number): [number, number] {
	const per = (n * (n + 1)) / 2;
	const k = line * per + packedIndex(n, r, c);
	return [re[k], r <= c ? im[k] : -im[k]];
}

/** Coherence of channels r and c on one line of a packed block, `|G_rc|² / (G_rr G_cc)`. */
export function packedCoherence(re: ArrayLike<number>, im: ArrayLike<number>, n: number, line: number, r: number, c: number): number {
	const [a, b] = packedElement(re, im, n, line, r, c);
	const grr = packedElement(re, im, n, line, r, r)[0];
	const gcc = packedElement(re, im, n, line, c, c)[0];
	return grr > 0 && gcc > 0 ? (a * a + b * b) / (grr * gcc) : 0;
}

// --- the reference sub-family (PROTOCOL.md, "MIMO Random Reference") ------------------------

/** The test's frame, which sets the reference's line spacing and lines. */
export interface MimoGridParams {
	fs_hz: number;
	n_pts: number;
}

/** A breakpoint row as single-output Random's profile carries it. */
export interface MimoBreakpoint {
	freq_hz: number;
	psd_g2_hz?: number;
	slope_db_oct?: number;
	alarm_db?: BandDb;
	abort_db?: BandDb;
}

/** A pair value: a constant, or `[frequency, value]` points joined by straight lines. */
export type MimoPairValue = number | [number, number][];

export interface MimoPairSpec {
	row: number;
	col: number;
	/** Squared coherence, 0 to 1. */
	coherence: MimoPairValue;
	/** Phase of `G_row,col`, degrees. */
	phase_deg: MimoPairValue;
}

export interface MimoBuildBreakpointsParams {
	grid: MimoGridParams;
	unit?: string;
	tables: { label: string; rows: MimoBreakpoint[] }[];
	pairs?: MimoPairSpec[];
	unspecified?: 'independent' | 'fully_coherent';
	alarm_db?: BandDb;
	abort_db?: BandDb;
	/** Accept the repair the result describes. */
	repair?: boolean;
	preview_pair?: [number, number];
}

export interface MimoImportParams {
	/** A reference folder, an `.npz` specification or a CSV table, by full path. */
	path: string;
	/** A CSV import's pair table, by full path. */
	pair_path?: string | null;
	grid?: MimoGridParams;
	labels?: string[];
	unit?: string;
	repair?: boolean;
	preview_pair?: [number, number];
}

export interface MimoValidateReferenceParams {
	reference: MimoReference;
	labels?: string[];
	unit?: string;
	preview_pair?: [number, number];
}

export interface MimoTransformOut {
	rows: number;
	cols: number;
	values: number[];
	row_labels: string[];
	col_labels: string[];
	condition: number | null;
}

/** What a reference request made: the reference (for the setup), its checks and its preview. */
export interface MimoReferenceResult {
	ok: boolean;
	/** Null when the reference was not accepted. */
	reference: MimoReference | null;
	labels: string[];
	unit: string;
	df_hz: number;
	band_hz: [number, number] | null;
	lines: number;
	errors: ReportItem[];
	warnings: ReportItem[];
	psd: {
		violations: number;
		/** The most negative eigenvalue relative to its line's largest, as entered (0 when none is). */
		min_eigenvalue_ratio: number | null;
		worst_hz: number | null;
		lines_hz: number[];
		repair: {
			lines: number;
			clamped: number;
			max_change: number;
			pairs: { row: number; col: number; labels: [string, string]; before: number; after: number; freq_hz: number }[];
		} | null;
		repair_accepted: boolean;
	};
	transforms: { response: MimoTransformOut | null; drive: MimoTransformOut | null };
	provenance: {
		method: string;
		source: string;
		tool: string;
		created_unix_secs: number;
		conjugated_on_import: boolean | null;
		assumptions: string[];
	} | null;
	/** Every PSD, the smallest eigenvalue per line, a pair's coherence and its phase. */
	views: View[];
}

export interface MimoRigidSensor {
	label: string;
	position: [number, number, number];
	direction: [number, number, number];
}

export interface MimoRigidParams {
	sensors: MimoRigidSensor[];
	reference_point?: [number, number, number];
	unit?: string;
	length_unit?: string;
}

export interface MimoRigidResult {
	ok: boolean;
	errors: ReportItem[];
	warnings?: ReportItem[];
	dofs: string[];
	dof_units: string[] | null;
	transform: MimoTransformOut | null;
	condition: number | null;
	singular_values: number[];
	/** How much of each sensor's reading the others can check, 0 to 1. */
	sensors: { label: string; check_share: number }[];
}
