/**
 * SRS shock on the wire (schema/PROTOCOL.md, "SRS Shock"): each pulse synthesized to match a
 * target shock response spectrum, closed loop between pulses, the drive in volts. The run's law
 * is chosen from the control-law catalog's `shock_srs` world (PROTOCOL.md, "Control Laws").
 * Where a part of the wire is the same as classical shock's, the type is classical shock's.
 */
import type {
	ClockDomain,
	DataSource,
	FeasibilityVerdict,
	ItemReason,
	LadderMode,
	ReportItem,
	RunRecordRef,
	StationRef,
	View
} from './types';
import type { LawEntry } from './mimo';
import type {
	ShockFeasibility,
	ShockGrid,
	ShockLawChoice,
	ShockLawRef,
	ShockMonitor,
	ShockOutcome,
	ShockPhase,
	ShockPrediction,
	ShockRefusalReason,
	ShockRunState,
	ShockSchedule,
	ShockSpacing,
	ShockSubscribeResult
} from './shock';

/** Event topics of SRS shock. */
export const SRS_TOPICS = {
	state: 'srs.state',
	log: 'srs.log',
	live: 'srs.live',
	pulse: 'srs.pulse',
	data: 'srs.data'
} as const;

/** Refusals of the `srs.*` methods: the same set as the `shock.*` methods'. */
export type SrsRefusalReason = ShockRefusalReason;

// --- configuration -------------------------------------------------------------------------

/** A target SRS, g. Breakpoint rows are `[frequency_hz, accel_g]`, joined on log-log axes. */
export type SrsTarget =
	| { kind: 'flat'; accel_g: number }
	| { kind: 'breakpoints'; points: [number, number][] };

export interface SrsBands {
	f_lo_hz: number;
	f_hi_hz: number;
	/** Default 3. */
	bands_per_octave?: number;
	/** The quality factor every SRS is computed at; default 10. */
	q?: number;
}

export type SrsEngine = 'wavsyn' | 'seds';

/** One odd-half-cycle wavelet of a `wavsyn` table. */
export interface SrsWaveletRow {
	freq_hz: number;
	amplitude_g: number;
	/** Odd, at least 3. */
	half_cycles: number;
	delay_s: number;
}

/** One damped sine of a `seds` table. */
export interface SrsDampedSineRow {
	freq_hz: number;
	amplitude_g: number;
	zeta: number;
	delay_s: number;
	polarity: 'positive' | 'negative';
}

/** A component table to start from; its `engine` is the configuration's. */
export type SrsComponents =
	| { engine: 'wavsyn'; rows: SrsWaveletRow[] }
	| { engine: 'seds'; rows: SrsDampedSineRow[] };

export interface SrsSynthesis {
	/** Default `wavsyn`. */
	engine?: SrsEngine;
	/** Null: one component per band. */
	components?: SrsComponents | null;
	max_iters?: number;
	tol_db?: number;
	max_gain_db_per_iter?: number;
}

/** `[low, high]` dB: how far under the target, then how far over it. */
export type SrsBandDb = [number, number];

export interface SrsEnvelope {
	/** Default `[3, 3]`: 3 dB under, 3 dB over. */
	alarm_db?: SrsBandDb;
	/** Default `[9, 6]`: 9 dB under, 6 dB over. */
	abort_db?: SrsBandDb;
	/** One per band, in place of the two above; null when not used. */
	per_band?: { alarm_db: SrsBandDb; abort_db: SrsBandDb }[] | null;
}

export interface SrsAcquisition {
	fs_hz?: number;
	ring_out_s?: number;
	pvss?: boolean;
}

export type SrsExcitation = 'shaped' | 'standard_pulse';

export interface SrsIdentification {
	excitation?: SrsExcitation;
	level_db?: number;
	/** Volts peak; null: a tenth of the drive ceiling. */
	max_vpk?: number | null;
	nfft?: number;
	h_floor_frac?: number;
	min_mean_coherence?: number;
	review?: boolean;
}

/** Every field optional; the defaults are in PROTOCOL.md. */
export interface SrsControl {
	law?: ShockLawChoice;
	law_budget?: number;
	law_misses?: number;
	max_pulses?: number;
	max_gain_rise_db?: number;
}

/** A ceiling per axis; null is no ceiling. */
export interface SrsKinematicLimits {
	accel_g?: number | null;
	velocity_ips?: number | null;
	disp_pp_in?: number | null;
}

export interface SrsLimits {
	drive_vpk_ceiling: number;
	drive_vpk_abort: number;
	peak_accel_g?: number | null;
	abort_factor?: number;
	kinematic?: SrsKinematicLimits;
	monitors?: ShockMonitor[];
}

/** A limit channel's bound, g; a per-band null is no bound at that band. */
export type SrsLimitBound =
	| { kind: 'flat'; accel_g: number }
	| { kind: 'per_band'; bounds_g: (number | null)[] };

/** A drive-to-channel gain, g per volt, on an ascending frequency grid. */
export interface SrsFrf {
	freqs_hz: number[];
	mag: number[];
}

export interface SrsLimitChannel {
	/** The table row's `index`. */
	channel: number;
	bound: SrsLimitBound;
	/** Null: identification measures it. */
	frf?: SrsFrf | null;
}

export interface SrsLimitChannels {
	headroom_db: number;
	channels: SrsLimitChannel[];
}

export interface SrsConfig {
	title: string;
	target: SrsTarget;
	bands: SrsBands;
	synthesis?: SrsSynthesis;
	envelope?: SrsEnvelope;
	acquisition?: SrsAcquisition;
	/** As classical shock's; with `on_pass` a pulse passes when every band is in the alarm band. */
	schedule: ShockSchedule;
	spacing?: ShockSpacing;
	identification?: SrsIdentification;
	control?: SrsControl;
	limits: SrsLimits;
	limit_channels?: SrsLimitChannels | null;
	feasibility?: ShockFeasibility | null;
	/** The run's record, as Random's: the operator's words, what the test is run to, and whether the folder keeps the time histories. */
	operator?: string;
	article?: string;
	notes?: string;
	specification?: { document: string; clause: string } | null;
	record?: { streams?: boolean } | null;
	metadata?: { key: string; label: string; value: string }[];
}

export interface SrsConfigureParams {
	config: SrsConfig;
	dry_run?: boolean;
}

export type SrsItemReason =
	| ItemReason
	| 'pulses_length_mismatch'
	| 'record_too_long'
	| 'bands_empty'
	| 'target_not_covering'
	| 'target_not_ascending'
	| 'per_band_length_mismatch'
	| 'components_engine_mismatch'
	| 'component_invalid'
	| 'limit_no_frf'
	| 'target_file'
	| 'table_file'
	| 'synthesis_not_converged';

export type SrsReportItem = Omit<ReportItem, 'reason'> & { reason: SrsItemReason };

/** The offline fit of the component table to the target. */
export interface SrsSynthesisFacts {
	engine: SrsEngine;
	components: number;
	iters: number;
	converged: boolean;
	worst_err_db: number | null;
	duration_s: number;
}

/** The synthesized pulse at full level, g. */
export interface SrsReferenceFacts {
	peak_g: number;
	velocity_pk_ips: number;
	displacement_pk_pk_in: number;
	residual_velocity_ips: number;
	residual_disp_in: number;
}

export interface SrsDerived {
	grid: ShockGrid;
	bands_hz: number[];
	/** The full-level target on the band grid, g. */
	target_g: number[];
	synthesis: SrsSynthesisFacts;
	reference: SrsReferenceFacts;
	/** The least time between pulses, seconds. */
	spacing_s: number;
	/** One law call's time budget, seconds. */
	call_budget_s: number;
	feasibility: FeasibilityVerdict;
}

export interface SrsConfigureResult {
	state: SrsState;
	derived: SrsDerived;
	warnings: SrsReportItem[];
}

export interface SrsDryRunResult {
	errors: SrsReportItem[];
	warnings: SrsReportItem[];
	derived: SrsDerived | null;
	views: View[];
}

/** `srs.config`: what the present run was configured with; all null when unconfigured. */
export interface SrsConfigResult {
	run_id: string | null;
	config: SrsConfig | null;
	derived: SrsDerived | null;
	views: View[];
}

export interface SrsPreviewParams {
	target: SrsTarget;
	bands: SrsBands;
	synthesis?: SrsSynthesis;
	/** The alarm and abort bands the Target view draws. */
	envelope?: SrsEnvelope;
	fs_hz?: number;
}

export interface SrsPreviewResult {
	derived: Pick<SrsDerived, 'bands_hz' | 'target_g' | 'synthesis' | 'reference'>;
	/** Target, Synthesis, Pulse, Velocity, Displacement. A computation, never a measurement. */
	views: View[];
}

export interface SrsTargetImportParams {
	/** A full path on the computer that runs the test. */
	path: string;
}

export interface SrsTargetImportResult {
	target: SrsTarget | null;
	errors: SrsReportItem[];
	warnings: SrsReportItem[];
}

export interface SrsTableImportParams {
	/** A full path on the computer that runs the test. */
	path: string;
	engine: SrsEngine;
}

export interface SrsTableImportResult {
	components: SrsComponents | null;
	errors: SrsReportItem[];
	warnings: SrsReportItem[];
}

// --- running -------------------------------------------------------------------------------

/** `srs.arm`, `srs.level` and their results are classical shock's. */
export type { ShockArmParams as SrsArmParams } from './shock';
export type { ShockLevelParams as SrsLevelParams } from './shock';
export type { ShockLevelResult as SrsLevelResult } from './shock';

export type SrsRunState = ShockRunState;
export type SrsPhase = ShockPhase;
/** Classical shock's outcomes: SRS shock ends the same ways. */
export type SrsOutcome = ShockOutcome;

/** Why an SRS law call was not used. */
export type SrsLawFaultReason =
	| 'error'
	| 'panicked'
	| 'over_budget'
	| 'over_memory'
	| 'wrong_length'
	| 'non_finite'
	| 'not_positive'
	| 'undeclared_value'
	| 'too_many_values'
	| 'gain_rise';

export type SrsTrip = { pulse: number; at_level_db: number | null } & (
	| { kind: 'envelope'; abort_bands: number; worst_band_hz: number; worst_err_db: number }
	| {
			kind: 'kinematic';
			axis: 'accel' | 'velocity' | 'displacement';
			value: number;
			limit: number;
			predicted: boolean;
	  }
	| { kind: 'drive_vpk'; vpk: number; abort_vpk: number; predicted: boolean }
	| { kind: 'accel'; predicted_g: number; limit_g: number }
	| { kind: 'monitor'; channel: number; value: number; limit: number }
	| { kind: 'not_converged'; limit: 'per_level' | 'run'; pulses: number }
	| { kind: 'law_fault'; reason: SrsLawFaultReason; consecutive: number }
);

export interface SrsEnd {
	outcome: SrsOutcome;
	message: string;
	at_s: number | null;
	level_db: number | null;
	/** The pulse the end came at; null before any pulse. */
	pulse: number | null;
	/** Present when `outcome` is `aborted`, `not_converged` or `law_fault`. */
	trip: SrsTrip | null;
}

export interface SrsIdentificationFacts {
	excitation: SrsExcitation;
	/** The burst's peak as played, volts. */
	drive_vpk: number;
	/** The cap it was held under, volts. */
	max_vpk: number;
	coherence_mean: number;
	min_mean_coherence: number;
	passed: boolean;
}

/** The arm-time limit notch. */
export interface SrsNotch {
	notched_bands: number;
	/** The worst predicted exceedance before and after it, dB over the allowed level. */
	worst_before_db: number | null;
	worst_after_db: number | null;
	converged: boolean;
}

/** One fired pulse (`srs.pulses`, `srs.pulse`, the `srs.pulse` event). */
export interface SrsPulseRow {
	index: number;
	origin: 'ladder';
	scheduled_db: number;
	commanded_db: number;
	/** The drive-limit scale applied; 1 when none. */
	limit_scale: number;
	drive_vpk: number;
	peak_g: number | null;
	velocity_pk_ips: number | null;
	displacement_pk_pk_in: number | null;
	worst_err_db: number | null;
	worst_band_hz: number | null;
	alarm_bands: number | null;
	abort_bands: number | null;
	/** No band outside the alarm band. */
	passed: boolean | null;
	/** Null without limit channels. */
	limited_components: number | null;
	worst_limit_db: number | null;
	at_s: number;
	/** The call that produced the gains this pulse played. */
	law: { used: boolean; miss: SrsLawFaultReason | null; call_ms: number } | null;
}

/** The `srs.state` result and event. */
export interface SrsState {
	run_id: string | null;
	state: SrsRunState;
	phase: SrsPhase;
	title: string | null;
	source: DataSource | null;
	station: StationRef | null;
	grid: ShockGrid | null;
	clock: ClockDomain | null;
	mode: LadderMode | null;
	levels_db: number[] | null;
	/** The commanded level; null when nothing is scheduled. */
	level_db: number | null;
	rung: number | null;
	pulses: { fired: number; passed: number; at_level: number; of_level: number };
	/** Seconds to the next pulse; null when none is due. */
	next_pulse_s: number | null;
	elapsed_s: number | null;
	alarm: boolean;
	identification: SrsIdentificationFacts | null;
	prediction: ShockPrediction | null;
	/** Null without limit channels, or before the notch is planned. */
	notch: SrsNotch | null;
	last_pulse: SrsPulseRow | null;
	law: ShockLawRef | null;
	feasibility: FeasibilityVerdict | null;
	end: SrsEnd | null;
	/** The run's record, from arm on: its folder, whether it keeps the time histories, the latest problem. */
	record?: RunRecordRef;
}

/** `srs.log`, its event, `srs.pulses` and `srs.pulse`'s params are classical shock's. */
export type {
	ShockLogResult as SrsLogResult,
	ShockLogEvent as SrsLogEvent,
	ShockPulsesParams as SrsPulsesParams,
	ShockPulseParams as SrsPulseParams
} from './shock';

export interface SrsPulsesResult {
	pulses: SrsPulseRow[];
	next: number;
}

export interface SrsPulseResult {
	pulse: SrsPulseRow;
	/** SRS, Pulse, Drive, Velocity, Displacement. */
	views: View[];
}

// --- display -------------------------------------------------------------------------------

export type SrsQuantity =
	| 'srs'
	| 'pulse'
	| 'velocity'
	| 'displacement'
	| 'identification'
	| 'components'
	| 'limit_channels'
	| 'limit_frf'
	| 'law'
	| 'readings'
	| 'pulses';

export interface SrsSubscribePlot {
	id: string;
	quantity: SrsQuantity;
	/** The time histories only. */
	points?: number;
}

export type SrsSubscribeParams = { rate_hz?: number; plots: SrsSubscribePlot[] } | { default: true };

export interface SrsSubscribeResult extends Omit<ShockSubscribeResult, 'plots' | 'quantities'> {
	plots: SrsSubscribePlot[] | null;
	quantities: SrsQuantity[];
}

/** The `srs.live` event, 5 times a second while the run is armed through finished. */
export interface SrsLive {
	run_id: string;
	source: DataSource;
	state: SrsRunState;
	phase: SrsPhase;
	mode: LadderMode | null;
	level_db: number | null;
	rung: number | null;
	pulses: { fired: number; passed: number; at_level: number; of_level: number };
	next_pulse_s: number | null;
	/** The fire in flight: frames delivered of expected; null between pulses. */
	fire: { done: number; of: number } | null;
}

/** The `srs.pulse` event: to every connection, once per pulse. */
export interface SrsPulseEvent {
	run_id: string;
	pulse: SrsPulseRow;
	/** SRS, Pulse, Drive at 1024 points. */
	views: View[];
}

/** One plot's data in an `srs.data` frame. */
export interface SrsDataPlot {
	id: string;
	quantity: SrsQuantity;
	views: View[];
}

/** The `srs.data` event: a subscribed connection's own frame. */
export interface SrsData extends SrsLive {
	plots: SrsDataPlot[];
}

// --- the law catalog and check (PROTOCOL.md, "Control Laws") ------------------------------

export type SrsCheckRequirement = 'within_by' | 'each_level_within' | 'limit_held';

/** One simulated test article's result in an SRS law check. */
export interface SrsLawCheckCase {
	name: string;
	plant: string;
	requirement: SrsCheckRequirement;
	passed: boolean;
	detail: string;
	end: SrsOutcome;
	trip: string | null;
	pulses: number;
	full_level_pulses: number;
	first_within_pulse: number | null;
	final_worst_band_db: number | null;
	worst_limit_over_db: number | null;
	misses: number;
	refused: number;
	limited_components: number;
	calls: number;
	worst_call_ms: number | null;
	budget_ms: number;
	max_drive_vpk: number;
}

export interface SrsLawsCheckResult {
	/** The law's entry after the check: a law file's `check` now holds the record. */
	law: LawEntry;
	report: { passed: boolean; recorded: boolean; tool_version: string; cases: SrsLawCheckCase[] };
}
