/**
 * Mixed Mode on the wire (schema/PROTOCOL.md, "Mixed Mode"): a list of parts played together on
 * one drive and controlled together. One floor (broadband random), up to 12 tones and up to 12
 * narrowbands; a tone or a narrowband stands still, sweeps, or follows another part at a ratio.
 * The floor's law is chosen from the `single` world, the tones' from the `sine` world
 * (PROTOCOL.md, "Mixed Mode Laws"). Where a part of the wire is Random's or Sine's, the type is
 * theirs.
 */
import type {
	BandDb,
	Breakpoint,
	ChannelSelector,
	ClockDomain,
	CompositeMode,
	DataSource,
	FeasibilityInputs,
	FeasibilityVerdict,
	ItemReason,
	LadderMode,
	LawFaultReason,
	LimitProfile,
	LiveChannel,
	LogEntry,
	LogKind,
	PretestParams,
	RandomPhase,
	RandomRunState,
	ReadingView,
	RefusalReason,
	ReportItem,
	Schedule,
	SpectrumView,
	StationClass,
	RunRecordRef,
	StationRef,
	View
} from './types';
import type { LawEntry } from './mimo';
import type { ShockLawChoice } from './shock';
import type {
	SineAdjustRefusal,
	SineDirection,
	SineDwellFor,
	SineIdentificationFacts,
	SineItemReason,
	SineKinematics,
	SineLawFaultReason,
	SineProfile,
	SineRate,
	SineTrackingVerdict
} from './sine';

/** Event topics of Mixed Mode. */
export const MIXED_TOPICS = {
	state: 'mixed.state',
	log: 'mixed.log',
	live: 'mixed.live',
	data: 'mixed.data'
} as const;

/** The most tones and narrowbands a run may have. */
export const MIXED_MAX_TONES = 12;
export const MIXED_MAX_BANDS = 12;

/** Refusals of the `mixed.*` methods: Random's, Sine's `adjust_refused`, and `part_refused`. */
export type MixedRefusalReason = RefusalReason | 'adjust_refused' | 'part_refused';

/** `detail.why` of an `adjust_refused` refusal: Sine's. */
export type MixedAdjustRefusal = SineAdjustRefusal;

/** `detail.why` of a `part_refused` refusal. */
export type MixedPartRefusal = 'unknown_part' | 'floor' | 'window_closed' | 'schedule_ended';

// --- parts ---------------------------------------------------------------------------------

export type MixedPartKind = 'floor' | 'tone' | 'narrowband';

/** When a part plays, in parts time. */
export interface MixedWindow {
	/** Default 0. */
	on_s?: number;
	/** Above `on_s`; null (the default) to the end of the run. */
	off_s?: number | null;
}

/** A part that stands still. `end` is Sine's dwell end; null (the default) for the whole window. */
export interface MixedFixedSchedule {
	kind: 'fixed';
	freq_hz: number;
	end?: SineDwellFor | null;
}

/** Sine's sweep: bounces between the band's ends. */
export interface MixedSweepSchedule {
	kind: 'sweep';
	band_hz: [number, number];
	rate: SineRate;
	/** Default the band's low edge. */
	start_hz?: number;
	/** The first leg; default `up`. */
	direction?: SineDirection;
	/** One-way sweeps, parts seconds, or whichever first; null (the default) sweeps until the window closes. */
	end?: { sweeps?: number | null; duration_s?: number | null } | null;
}

/** Another part's frequency times `ratio`. */
export interface MixedFollowSchedule {
	kind: 'follow';
	/** Another tone's or narrowband's `id`. */
	of: string;
	/** Above 0. */
	ratio: number;
}

/** Reserved (sweep, dwell, continue in one part); refused `schedule_unsupported`. */
export interface MixedSegmentsSchedule {
	kind: 'segments';
	[key: string]: unknown;
}

export type MixedSchedule = MixedFixedSchedule | MixedSweepSchedule | MixedFollowSchedule | MixedSegmentsSchedule;

export type MixedScheduleKind = MixedSchedule['kind'];

/** The fields every part carries. */
export interface MixedPartBase {
	/** 1 to 32 letters, digits, `_` and `-`; unique in the list. */
	id: string;
	/** Default the kind and its count: `Floor`, `Tone 2`, `Narrowband 1`. */
	name?: string;
	/** Default true; the floor is always enabled. */
	enabled?: boolean;
	/** Default `{on_s: 0, off_s: null}`; the floor's is always that. */
	window?: MixedWindow;
}

export interface MixedFloorPart extends MixedPartBase {
	kind: 'floor';
	/** Random's profile rows, g²/Hz. */
	breakpoints: Breakpoint[];
	/** Default `[3, 3]`. */
	alarm_db?: BandDb;
	/** Default `[6, 6]`. */
	abort_db?: BandDb;
}

/** A tone's amplitude: a constant in g peak, or a Sine profile read at its frequency (exactly one). */
export type MixedToneAmplitude = { amplitude_g: number; profile?: never } | { profile: SineProfile; amplitude_g?: never };

export type MixedTonePart = MixedPartBase & {
	kind: 'tone';
	schedule: MixedSchedule;
	/** Default `[1.5, 1.5]`; a profile's own bands take their place. */
	alarm_db?: BandDb;
	/** Default `[3, 3]`. */
	abort_db?: BandDb;
} & MixedToneAmplitude;

/** A narrowband's full width: Hz, or a share of its center (exactly one). */
export type MixedBandWidth = { width_hz: number; width_share?: never } | { width_share: number; width_hz?: never };

export type MixedBandPart = MixedPartBase & {
	kind: 'narrowband';
	schedule: MixedSchedule;
	/** Density added over the floor between the edges, g²/Hz. */
	level_g2_hz: number;
	/** Null (the default): the floor's profile bands; widened to the band's own scatter either way. */
	alarm_db?: BandDb | null;
	abort_db?: BandDb | null;
} & MixedBandWidth;

export type MixedPart = MixedFloorPart | MixedTonePart | MixedBandPart;

// --- configuration -------------------------------------------------------------------------

export interface MixedAcquisition {
	fs_hz: number;
	/** A power of two; lines no wider than the tracking width. */
	n_pts: number;
	n_avg: number;
	/** The control tick; default 10. */
	tick_ms?: number;
}

/** Mixed Mode tracks every tone at one fixed width. */
export interface MixedTracking {
	kind: 'fixed';
	/** Hz; default 0.5. */
	hz: number;
}

/** The two laws: the floor's from world `single`, the tones' from world `sine`. */
export interface MixedLawChoice {
	/** Default `{id: 'hinv'}`. */
	floor?: ShockLawChoice;
	/** Default `{id: 'servo_blend'}`. */
	tones?: ShockLawChoice;
}

/** Every field optional; the defaults are in PROTOCOL.md. */
export interface MixedFloorGuards {
	/** Fraction of one loop's acquisition time; default 0.25. */
	law_budget?: number;
	/** Default 3. */
	law_misses?: number;
	/** Default 6. */
	max_change_db?: number;
}

/** Sine's guards, for each tone; the defaults are Sine's. */
export interface MixedToneGuards {
	law_budget?: number;
	law_misses?: number;
	max_step_db?: number;
	max_rise_db_per_s?: number;
	divergence_s?: number;
	fault_debounce_ticks?: number;
}

export interface MixedLawGuards {
	floor?: MixedFloorGuards;
	tones?: MixedToneGuards;
}

/** Every field optional; the defaults are in PROTOCOL.md. Random's fields act on the floor. */
export interface MixedControl {
	composite?: CompositeMode;
	weights?: { channel: number; weight: number }[];
	control_dof?: number;
	damping?: number;
	max_lines_out?: number;
	max_loops_out?: number;
	/** On the random part alone; null for no clip. */
	max_sigma?: number | null;
	/** On the random part's synthesis. */
	drive_ceiling_vrms?: number;
	drive_abort_vrms?: number | null;
	coherence_floor?: number;
	review_after_sysid?: boolean;
	pretest?: PretestParams;
	soft_start_db?: number;
	soft_start_relax_db?: number;
	level_ramp_s?: number;
	shutdown_ramp_s?: number;
	/** One part's ramp in or out; default 3. */
	part_ramp_s?: number;
	/** The most the floor alone is given to equalize; default 30. */
	floor_settle_s?: number;
	trust_coherence?: number;
	trust_h_floor_frac?: number;
	tracking?: MixedTracking;
	law?: MixedLawChoice;
	law_guards?: MixedLawGuards;
}

export interface MixedLimits {
	/** Volts peak on the summed drive; required. */
	drive_vpk_ceiling: number;
	/** Volts peak, at least the ceiling; required. */
	drive_vpk_abort: number;
	/** Default true; needs `feasibility`. */
	kinematic_monitor?: boolean;
	/** Random's rows; they lower the random part, never a tone. */
	channels?: LimitProfile[];
}

export interface MixedConfig {
	title?: string;
	/** Exactly one floor, at most 12 tones and 12 narrowbands, in the order the run reports them. */
	parts: MixedPart[];
	/** Hz each side of a tone that stands still; default 2. */
	exclusion_hz?: number;
	acquisition: MixedAcquisition;
	/** Random's ladder; every part moves by the same dB. */
	schedule: Schedule;
	control?: MixedControl;
	limits: MixedLimits;
	/** Random's, with its crest factor (default 3). */
	feasibility?: FeasibilityInputs | null;
	/** The operator's words, kept in the run's record (Random's). */
	operator?: string;
	article?: string;
	notes?: string;
	/** The document and clause the test is run to. */
	specification?: { document: string; clause: string } | null;
	/** `streams: false` keeps the record without the inputs and drive streams. */
	record?: { streams?: boolean } | null;
	metadata?: { key: string; label?: string; value: string }[];
}

export interface MixedConfigureParams {
	config: MixedConfig;
	dry_run?: boolean;
}

export type MixedItemReason =
	| ItemReason
	| SineItemReason
	| 'no_floor'
	| 'too_many_floors'
	| 'unknown_part_kind'
	| 'duplicate_part_id'
	| 'part_out_of_band'
	| 'parts_too_close'
	| 'too_many_tones'
	| 'too_many_bands'
	| 'follow_unknown_part'
	| 'follow_self'
	| 'follow_loop'
	| 'schedule_unsupported';

export type MixedReportItem = Omit<ReportItem, 'reason'> & { reason: MixedItemReason };

/** Random's grid with Sine's tick. */
export interface MixedGrid {
	fs_hz: number;
	n_pts: number;
	df_hz: number;
	/** Lines in the band. */
	lines: number;
	loop_s: number;
	tick_frames: number;
	tick_s: number;
	/** True once the run has armed and every number is the station's. */
	actual: boolean;
}

export interface MixedDerivedPart {
	id: string;
	kind: MixedPartKind;
	name: string;
	/** Lowest and highest frequency or center of its whole motion; null for the floor. */
	freq_hz: [number, number] | null;
	/** A narrowband's narrowest and widest width; null otherwise. */
	width_hz: [number, number] | null;
	/** Reference mean square, g². */
	ms_g2: number;
	/** A tone's largest amplitude, g peak; null otherwise. */
	peak_g: number | null;
}

export interface MixedReference {
	floor_ms_g2: number;
	tones_ms_g2: number;
	bands_ms_g2: number;
	total_ms_g2: number;
	total_rms_g: number;
	/** Floor plus narrowbands. */
	random_rms_g: number;
}

/** A span left out of the floor's average for the whole run. */
export interface MixedExclusion {
	part: string;
	lo_hz: number;
	hi_hz: number;
	lines: number;
}

export interface MixedClosestTones {
	parts: [string, string];
	spacing_hz: number;
	/** Two tracking widths. */
	min_hz: number;
}

export type MixedTrackingVerdict = SineTrackingVerdict & { part: string };

export interface MixedLawFacts {
	id: string;
	name: string;
	version: string;
	sha256: string | null;
	/** The keys of the law's live parameters (a `single` law has none). */
	live: string[];
	/** Whether it may drive hardware in this role. */
	hardware: boolean;
}

export interface MixedDerived {
	/** The floor's band. */
	band_hz: [number, number];
	grid: MixedGrid;
	control_channels: number[];
	/** In the parts' order. */
	parts: MixedDerivedPart[];
	reference: MixedReference;
	kinematics: SineKinematics;
	exclusion: MixedExclusion[];
	/** Null with fewer than two tones. */
	closest_tones: MixedClosestTones | null;
	/** One per tone, in the parts' order. */
	tracking: MixedTrackingVerdict[];
	/** How each tone will be judged, in the parts' order. */
	tone_judging: MixedToneJudging[];
	/** Parts time of the ladder; null in manual. */
	test_s: number | null;
	/** Station time the floor needs on this grid to reach its control degrees of freedom; the parts wait for the larger of this and `control.floor_settle_s`. */
	floor_equalize_s: number;
	identification_s: number;
	call_budget_s: { floor: number; tones: number };
	law: { floor: MixedLawFacts; tones: MixedLawFacts };
	feasibility: FeasibilityVerdict;
}

export interface MixedConfigureResult {
	state: MixedState;
	derived: MixedDerived;
	warnings: MixedReportItem[];
}

export interface MixedDryRunResult {
	errors: MixedReportItem[];
	warnings: MixedReportItem[];
	derived: MixedDerived | null;
	/** Reference and Tones; empty with errors. */
	views: SpectrumView[];
}

/** `mixed.config`: what the present run was configured with; all null when unconfigured. */
export interface MixedConfigResult {
	run_id: string | null;
	config: MixedConfig | null;
	derived: MixedDerived | null;
	views: SpectrumView[];
}

// --- running -------------------------------------------------------------------------------

export interface MixedArmParams {
	/** The operator's acknowledgement to arm past a feasibility exceedance; logged verbatim. */
	override_ack?: string;
}

export type MixedLevelParams = { delta_db: number } | { to_db: number };

export interface MixedLevelResult {
	level_db: number;
}

export interface MixedModeParams {
	mode: LadderMode;
}

export interface MixedModeResult {
	mode: LadderMode;
}

/** Switch a part on or off live, as a ramp. */
export interface MixedPartParams {
	id: string;
	on: boolean;
}

export interface MixedPartResult {
	id: string;
	on: boolean;
}

/** A live parameter of the tone law, for every tone. */
export interface MixedAdjustParams {
	key: string;
	value: number;
}

export interface MixedAdjustResult {
	key: string;
	value: number;
}

export type MixedRunState = RandomRunState;

/** Sine's phases with `floor_only`: the floor alone at the first level before the parts come in. */
export type MixedPhase =
	| Extract<
			RandomPhase,
			| 'idle'
			| 'feasibility_blocked'
			| 'armed'
			| 'sysid'
			| 'coherence_gate'
			| 'engaged'
			| 'full_level'
			| 'awaiting_review'
			| 'ramping_down'
			| 'stopped'
			| 'aborted'
			| 'coherence_hold'
	  >
	| 'floor_only';

export type MixedOutcome =
	| 'completed'
	| 'operator_stop'
	| 'operator_abort'
	| 'safe_stop'
	| 'aborted'
	| 'held'
	| 'sysid_level_unreachable'
	| 'law_fault'
	| 'fault';

/** Which law: the floor's, or the tones'. */
export type MixedLawRole = 'floor' | 'tones';

/** Random's reasons for the floor, Sine's for a tone. */
export type MixedLawFaultReason = LawFaultReason | SineLawFaultReason;

/** Facts every trip carries. `part` is null for a trip of the summed drive or a channel. */
export interface MixedTripWhere {
	part: string | null;
	at_level_db: number;
	at_s: number;
	at_loop: number;
	at_tick: number;
	/** The part's frequency then; null for the floor and for a trip of no part. */
	at_hz: number | null;
}

export type MixedTrip = MixedTripWhere &
	(
		| { kind: 'envelope'; abort_lines: number; worst_db: number | null }
		/** `error_db` the part's error then; `measured_g` and `limit_g` null until the core reports them. */
		| { kind: 'abort_band'; side: 'high' | 'low'; error_db?: number | null; measured_g: number | null; limit_g: number | null }
		| { kind: 'band_abort'; side: 'high' | 'low'; error_db?: number | null; measured_g2_hz: number | null; limit_g2_hz: number | null; dof: number | null }
		| { kind: 'servo_fault'; consecutive_ticks: number }
		/** The floor: `consecutive_loops` and `divergent_bins`; a tone: `capped_s`. */
		| { kind: 'divergence'; consecutive_loops?: number; divergent_bins?: number; capped_s?: number }
		| { kind: 'law_fault'; role: MixedLawRole; reason: MixedLawFaultReason; consecutive: number }
		| { kind: 'oscillator_range'; requested_hz: number; fs_hz: number }
		| { kind: 'kinematic'; channel: number; axis: 'accel' | 'velocity' | 'displacement'; value: number; limit: number }
		| { kind: 'drive_vpk'; vpk: number; abort_vpk: number }
		| { kind: 'drive_vrms'; realized_vrms: number; limit_vrms: number }
		| {
				kind: 'watchdog';
				channel: number;
				axis: string;
				value: number;
				limit: number;
				measured: number;
				drive_vpk: number;
		  }
	);

export interface MixedEnd {
	outcome: MixedOutcome;
	/** Names the part by its name. */
	message: string;
	/** Station time from the floor's engage. */
	at_s: number | null;
	level_db: number | null;
	/** The controlled stop's length on the station's clock; null for an abort. */
	ramp_s: number | null;
	/** Present when `outcome` is `aborted` or `law_fault`. */
	trip: MixedTrip | null;
}

/** The floor's lines in the last loop. */
export interface MixedFloorLines {
	in_band: number;
	/** Read in the last loop: not left out. */
	clean: number;
	alarm: number;
	abort: number;
	/** The lines the floor's rms abort is judged on; it arms once three quarters of the in-band lines qualify. */
	rms_judged: number;
}

/** The prediction of how a tone's alarm and abort judge it. */
export interface MixedToneJudging {
	part: string;
	/** The averaging time, s (at most 8). */
	judge_s: number;
	/** The single reading's predicted scatter, dB. */
	sigma_db: number;
	alarm_db: BandDb;
	abort_db: BandDb;
	/** Judged against tolerances widened to that scatter. */
	widened: boolean;
}

/** One part's row in the state and in every frame: every part, in the parts' order. */
export interface MixedPartRow {
	id: string;
	kind: MixedPartKind;
	name: string;
	/** As configured, or as `mixed.part` last set it. */
	enabled: boolean;
	/** Playing now, ramps included. */
	on: boolean;
	/** Its ramp now, 0 to 1. */
	scale: number;
	/** A tone's frequency or a narrowband's center now; null for the floor. */
	freq_hz: number | null;
	/** Null when it stands still; a follower carries its leader's. */
	direction: SineDirection | null;
	/** Null unless the part sweeps. */
	sweeps: { done: number; of: number | null } | null;
	/** Signed, from the part's own schedule (a log follower its leader's oct/min, a linear one its
	 * leader's Hz/s times the ratio); one is a number, the other null; both null when it stands still;
	 * 0 while the parts' clock is stopped. */
	rate_oct_per_min: number | null;
	rate_hz_per_s: number | null;
	/** A narrowband's width now; null otherwise. */
	width_hz: number | null;
	/** `g` (the floor rms, a tone peak) or `g²/Hz` (a narrowband). */
	unit: string;
	target: number | null;
	measured: number | null;
	error_db: number | null;
	alarm: boolean;
	/** A tone's amplitude in the drive, V peak; null otherwise. */
	drive_vpk: number | null;
	/** The floor's share of the random part, V rms; null for a tone and a narrowband. */
	drive_vrms: number | null;
	/** A narrowband's servo gain; null otherwise. */
	gain_db: number | null;
	settled: boolean;
	limiting: boolean;
	/** The floor's typical (median) clean line, or a narrowband's level estimate; null for a tone. */
	dof: number | null;
	/** The floor only. */
	lines: MixedFloorLines | null;
	/** The part's law instance; null for a narrowband. `capped` counts ticks (a tone) or loops (the floor). */
	law: { misses: number; capped: number } | null;
	/** A follower's leader and ratio; null for a part that does not follow. */
	follows: { of: string; ratio: number } | null;
	/** A tone's judging time, s: how long its reading is averaged before its alarm and abort judge it; null until set, and on the floor and narrowbands. */
	judge_s: number | null;
	/** A tone's judged error, dB (the averaged reading the alarm and abort use); `error_db` stays the single reading. */
	judged_error_db: number | null;
	/** A tone's alarm and abort bands as judged, `[below, above]` positive dB. */
	judged_alarm_db: BandDb | null;
	judged_abort_db: BandDb | null;
	/** A tone judged against tolerances widened to its reading's scatter; null on the floor and narrowbands. */
	tolerance_widened: boolean | null;
}

/** The composite: shown, not judged. */
export interface MixedTotal {
	rms_g: number | null;
	reference_rms_g: number | null;
	drive_vpk: number | null;
	drive_vrms: number | null;
}

export interface MixedLawRef {
	id: string;
	name: string;
	version: string;
	sha256: string | null;
	/** Unusable calls in a row now (the worst instance's, for the tones). */
	misses: number;
	/** Ticks or loops a cap has bound this run. */
	capped: number;
	/** The live parameters' values in force. */
	live: Record<string, number>;
}

/** The `mixed.state` result and event. */
export interface MixedState {
	run_id: string | null;
	state: MixedRunState;
	phase: MixedPhase;
	title: string | null;
	source: DataSource | null;
	station: StationRef | null;
	grid: MixedGrid | null;
	clock: ClockDomain | null;
	mode: LadderMode | null;
	levels_db: number[] | null;
	/** The commanded level; null when nothing drives. */
	level_db: number | null;
	full_level_s: number | null;
	/** Station time since the floor engaged, holds included. */
	t_s: number | null;
	/** The same, holds excluded. */
	elapsed_s: number | null;
	/** Parts time; null before the parts come in. */
	parts_s: number | null;
	at_level_s: number | null;
	remaining_at_level_s: number | null;
	held: boolean;
	alarm: boolean;
	limiting: boolean;
	/** Every part, in the parts' order; empty when unconfigured. */
	parts: MixedPartRow[];
	total: MixedTotal | null;
	identification: SineIdentificationFacts | null;
	law: { floor: MixedLawRef; tones: MixedLawRef } | null;
	feasibility: FeasibilityVerdict | null;
	end: MixedEnd | null;
	/** The run's record, from arm on (Random's). */
	record?: RunRecordRef;
}

/** Random's kinds, and `part`: a part switched on or off. */
export type MixedLogKind = LogKind | 'part';

export type MixedLogEntry = Omit<LogEntry, 'kind'> & { kind: MixedLogKind };

export interface MixedLogResult {
	entries: MixedLogEntry[];
	next: number;
}

export interface MixedLogEvent {
	run_id: string;
	entries: MixedLogEntry[];
}

// --- display -------------------------------------------------------------------------------

export type MixedLiveQuantity =
	| 'control'
	| 'residual'
	| 'drive'
	| 'response'
	| 'time'
	| 'coherence'
	| 'identification'
	| 'amplitude'
	| 'frequency'
	| 'level'
	| 'law'
	| 'readings';

/** Which parts an `amplitude`, `frequency` or `level` plot shows: ids, or a set the service resolves. */
export type MixedPartSelector = string[] | 'all' | 'tones' | 'bands';

export interface MixedSubscribePlot {
	id: string;
	quantity: MixedLiveQuantity;
	/** `response` and `time` only. */
	channels?: ChannelSelector;
	/** `amplitude`, `frequency` and `level` only; default `all`. */
	parts?: MixedPartSelector;
	/** `time` only. */
	points?: number;
}

export type MixedSubscribeParams = { rate_hz?: number; plots: MixedSubscribePlot[] } | { default: true };

export interface MixedSubscribeResult {
	default: boolean;
	rate_hz: number;
	plots: MixedSubscribePlot[] | null;
	quantities: MixedLiveQuantity[];
}

export type MixedAlarm =
	| { kind: 'envelope'; part: string; lines: number; worst_db: number | null }
	| { kind: 'band'; part: string; side: 'high' | 'low'; error_db: number }
	| { kind: 'overload'; channel: number };

export interface MixedLawLive {
	values: Record<string, number>;
	misses: number;
	capped: number;
}

export interface MixedLawsLive {
	floor: MixedLawLive;
	/** One per tone with a law instance, in the parts' order. */
	tones: (MixedLawLive & { part: string })[];
}

/** The numbers every live frame carries, `mixed.live` and `mixed.data` alike. */
export interface MixedLiveHead {
	run_id: string;
	source: DataSource;
	class: StationClass;
	state: 'running' | 'review' | 'stopping' | 'finished';
	phase: MixedPhase;
	mode: LadderMode;
	t_s: number | null;
	elapsed_s: number | null;
	parts_s: number | null;
	/** The level the drive is at now. */
	level_db: number | null;
	/** The ladder's level. */
	commanded_db: number | null;
	at_level_s: number | null;
	remaining_at_level_s: number | null;
	level_s: number | null;
	loop: number;
	tick: number;
	held: boolean;
	alarm: boolean;
	limiting: boolean;
	/** Every part, in the parts' order. */
	parts: MixedPartRow[];
	total: MixedTotal;
	channels: LiveChannel[];
	alarms: MixedAlarm[];
	/** Null until the laws are built. */
	law: MixedLawsLive | null;
}

/** The `mixed.live` event: control, residual, drive and run views plus the numbers behind them. */
export interface MixedLive extends MixedLiveHead {
	/** Control, residual, drive, run readings, in that order. */
	views: [SpectrumView, SpectrumView, SpectrumView, ReadingView];
}

/** One plot's data in a `mixed.data` frame. */
export interface MixedDataPlot {
	id: string;
	quantity: MixedLiveQuantity;
	/** The resolved channels, in table order, for `response` and `time`; null otherwise. */
	channels: number[] | null;
	/** The resolved part ids, in the parts' order, for `amplitude`, `frequency` and `level`; null otherwise. */
	parts: string[] | null;
	views: View[];
}

/** The `mixed.data` event: a subscribed connection's own frame. */
export interface MixedData extends MixedLiveHead {
	plots: MixedDataPlot[];
}

// --- the laws (PROTOCOL.md, "Mixed Mode Laws") ---------------------------------------------

/** Which worlds `laws.list` is asked for, by role. */
export const MIXED_LAW_WORLDS = { floor: 'single', tones: 'sine' } as const;

/** The mixed check set's cases. */
export type MixedCheckCaseId =
	| 'floor_only'
	| 'fixed_tone_on_floor'
	| 'swept_tone_through_mode'
	| 'two_tones_close'
	| 'narrowband_fixed'
	| 'narrowband_swept'
	| 'level_ladder'
	| 'transport_delay';

export const MIXED_CHECK_CASES: readonly MixedCheckCaseId[] = [
	'floor_only',
	'fixed_tone_on_floor',
	'swept_tone_through_mode',
	'two_tones_close',
	'narrowband_fixed',
	'narrowband_swept',
	'level_ladder',
	'transport_delay'
];

/** `laws.check` with a role: the mixed check set, the law in that role and the built-in in the other. */
export interface MixedLawsCheckParams {
	id?: string;
	path?: string;
	params?: Record<string, unknown> | null;
	mixed: MixedLawRole;
}

export interface MixedLawCheckCase {
	name: string;
	case: MixedCheckCaseId;
	role: MixedLawRole;
	plant: string;
	/** The case's pass band, in words. */
	requirement: string;
	passed: boolean;
	detail: string;
	end: MixedOutcome;
	trip: string | null;
	floor_worst_db: number | null;
	tone_worst_db: number | null;
	band_worst_db: number | null;
	calls: number;
	misses: number;
	refused: number;
	capped: number;
	worst_call_ms: number | null;
	budget_ms: number;
	max_drive_vpk: number;
}

export interface MixedLawsCheckResult {
	/** The law's entry, unchanged by a mixed check. */
	law: LawEntry;
	report: { passed: boolean; recorded: boolean; tool_version: string; cases: MixedLawCheckCase[] };
}
