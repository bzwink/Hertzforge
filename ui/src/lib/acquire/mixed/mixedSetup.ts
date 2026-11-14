/**
 * The Mixed Mode setup as the operator edits it, and its conversion to and from the protocol's
 * `MixedConfig` (schema/PROTOCOL.md, "Mixed Mode Configuration"). Pure functions only: the service
 * computes every number about the test (each part's mean square, the predicted peak and the
 * kinematics, the lines left out of the floor's average, the closest tones, the grid and the tick),
 * and this module only carries the operator's choices.
 *
 * The parts are edited in three tables: the floor (Random's breakpoint rows), the tones and the
 * narrowbands. On the wire they go in that order: the floor, then the tones, then the narrowbands,
 * each table in its own order. Numbers are held as numbers; a field that holds no number is NaN,
 * which `localErrors` names before anything is sent. The drive ceiling and the drive abort have no
 * default anywhere: the setup asks for both. Where a field is Random's or Sine's, the draft holds
 * it as their editors do, so their editors serve it.
 */

import type { BandDb, ChannelRow, CompositeMode, LadderMode, LimitProfile, ReportItem } from '$lib/protocol/types';
import type { LawEntry } from '$lib/protocol/mimo';
import type {
	MixedBandPart,
	MixedConfig,
	MixedFloorPart,
	MixedPart,
	MixedSchedule,
	MixedTonePart,
	MixedWindow
} from '$lib/protocol/mixed';
import { MIXED_MAX_BANDS, MIXED_MAX_TONES } from '$lib/protocol/mixed';
import type { SineDirection, SineDwellFor } from '$lib/protocol/sine';
import { lawWireParams, type LawDraft } from '../law/lawForm';
import { defaultControl, defaultRecord, newBreakpoint, shakerRecord, type BreakpointDraft, type ControlDraft, type FieldErrors, type LevelDraft, type RecordDraft } from '../setup';
import { newRow, profileDraftOf, profileOf, type SineProfileDraft } from '../sine/sineSetup';

// ---------------------------------------------------------------------------------------------
// The draft

/** How a tone or a narrowband moves. Every kind's settings are kept; the wire carries the chosen one. */
export interface ScheduleDraft {
	kind: 'fixed' | 'sweep' | 'follow';
	/** `fixed`: where it stands, and for how long (`none`: the whole window). */
	freq_hz: number;
	dwell: { by: 'none' | 'seconds' | 'cycles'; seconds: number; cycles: number };
	/** `sweep`: Sine's sweep, bouncing between the band's ends. */
	band_hz: [number, number];
	law: 'log' | 'linear';
	oct_per_min: number;
	hz_per_s: number;
	/** Null: the band's low edge. */
	start_hz: number | null;
	direction: SineDirection;
	/** `none`: until the window closes. */
	end_by: 'none' | 'sweeps' | 'duration' | 'both';
	sweeps: number;
	duration_s: number;
	/** `follow`: another part's id, and the ratio. */
	of: string;
	ratio: number;
}

export interface WindowDraft {
	on_s: number;
	/** Null: to the end of the run. */
	off_s: number | null;
}

export interface MixedToneDraft {
	id: string;
	name: string;
	enabled: boolean;
	window: WindowDraft;
	schedule: ScheduleDraft;
	/** A constant peak amplitude, or a Sine profile read at the tone's frequency. */
	amplitude: { by: 'constant' | 'profile'; g: number; profile: SineProfileDraft };
	alarm_db: BandDb;
	abort_db: BandDb;
}

export interface MixedBandDraft {
	id: string;
	name: string;
	enabled: boolean;
	window: WindowDraft;
	schedule: ScheduleDraft;
	level_g2_hz: number;
	width: { by: 'hz' | 'share'; hz: number; share: number };
	/** Off: the floor's profile bands. */
	own_bands: boolean;
	alarm_db: BandDb;
	abort_db: BandDb;
}

export interface MixedFloorDraft {
	id: string;
	name: string;
	breakpoints: BreakpointDraft[];
	alarm_db: BandDb;
	abort_db: BandDb;
}

/** Random's control fields, and Mixed Mode's own. */
export interface MixedControlDraft extends ControlDraft {
	part_ramp_s: number;
	floor_settle_s: number;
	trust_coherence: number;
	trust_h_floor_frac: number;
	/** The one fixed tracking width, Hz. */
	tracking_hz: number;
}

export interface MixedDraft {
	title: string;
	floor: MixedFloorDraft;
	tones: MixedToneDraft[];
	bands: MixedBandDraft[];
	exclusion_hz: number;
	acquisition: { fs_hz: number; n_pts: number; n_avg: number; tick_ms: number };
	/** Random's ladder, as Random's editor holds it. */
	schedule: { mode: LadderMode; levels: LevelDraft[]; full_level_s: number };
	control: MixedControlDraft;
	lawFloor: LawDraft;
	lawTones: LawDraft;
	guards: {
		floor: { law_budget: number; law_misses: number; max_change_db: number };
		tones: { law_budget: number; law_misses: number; max_step_db: number; max_rise_db_per_s: number; divergence_s: number; fault_debounce_ticks: number };
	};
	limits: {
		/** Volts peak on the summed drive; NaN until the operator gives it. */
		drive_vpk_ceiling: number;
		drive_vpk_abort: number;
		kinematic_monitor: boolean;
		channels: LimitProfile[];
	};
	feasibility: {
		enabled: boolean;
		shaker_text: string;
		moving_mass_lb: { armature: number; slip_table: number; drive_adapter: number; fixturing: number; test_article: number };
		crest_factor: number;
	};
	/** The run's record: the operator's words and whether the time histories are kept (Random's). */
	record: RecordDraft;
	metadata: { key: string; label: string; value: string }[];
}

export const DEFAULT_FLOOR_LAW = 'hinv';
export const DEFAULT_TONE_LAW = 'servo_blend';

/**
 * The built-in tone law's settings in Mixed Mode. It reads each tone through a narrow tracker, so
 * its compression is the one that holds there (the core's own default for the role), not Sine's
 * catalog default: Sine's slow compression lets a sweeping tone fall behind its target.
 */
export const MIXED_TONE_LAW_DEFAULTS: Record<string, string | number> = { compression: 'custom', ki: 0.5, max_rate_db_per_s: 20 };

/** The values a tone law's form starts from: the operator's, over Mixed Mode's for the built-in. */
export function toneLawStart<T>(id: string, previous: Record<string, T> | undefined): Record<string, T | string | number> | undefined {
	if (id !== DEFAULT_TONE_LAW || (previous && 'compression' in previous)) return previous;
	return { ...MIXED_TONE_LAW_DEFAULTS, ...(previous ?? {}) };
}

export function newSchedule(kind: ScheduleDraft['kind'] = 'fixed', freq = 100): ScheduleDraft {
	return {
		kind,
		freq_hz: freq,
		dwell: { by: 'none', seconds: 60, cycles: 6000 },
		band_hz: [freq, freq * 2],
		law: 'log',
		oct_per_min: 1,
		hz_per_s: 2,
		start_hz: null,
		direction: 'up',
		end_by: 'none',
		sweeps: 4,
		duration_s: 300,
		of: '',
		ratio: 2
	};
}

/** An id not yet used by any part: `t1`, `t2` ... or `b1`, `b2` ... */
export function freeId(d: Pick<MixedDraft, 'floor' | 'tones' | 'bands'>, prefix: 't' | 'b'): string {
	const used = new Set([d.floor.id, ...d.tones.map((t) => t.id), ...d.bands.map((b) => b.id)]);
	for (let n = 1; ; n++) if (!used.has(`${prefix}${n}`)) return `${prefix}${n}`;
}

export function newTone(id: string, n: number, freq = 100): MixedToneDraft {
	return {
		id,
		name: `Tone ${n}`,
		enabled: true,
		window: { on_s: 0, off_s: null },
		schedule: newSchedule('fixed', freq),
		amplitude: { by: 'constant', g: 1, profile: { label: '', rows: [newRow(freq, 'accel_g', 1), newRow(freq * 2, 'accel_g', 1)], alarm_db: [1.5, 1.5], abort_db: [3, 3] } },
		alarm_db: [1.5, 1.5],
		abort_db: [3, 3]
	};
}

export function newBand(id: string, n: number, freq = 250): MixedBandDraft {
	return {
		id,
		name: `Narrowband ${n}`,
		enabled: true,
		window: { on_s: 0, off_s: null },
		schedule: newSchedule('fixed', freq),
		level_g2_hz: 0.1,
		width: { by: 'hz', hz: 20, share: 0.05 },
		own_bands: false,
		alarm_db: [3, 3],
		abort_db: [6, 6]
	};
}

/** Random's qualification floor with one fixed tone: a sine-on-random ready to run on the simulator. */
export function defaultMixedSetup(): MixedDraft {
	return {
		title: 'Sine on Random',
		floor: {
			id: 'floor',
			name: 'Floor',
			breakpoints: [newBreakpoint(20, 'psd', 0.01), newBreakpoint(80, 'slope', 3), newBreakpoint(350, 'psd', 0.04), newBreakpoint(2000, 'slope', -3)],
			alarm_db: [3, 3],
			abort_db: [6, 6]
		},
		tones: [newTone('t1', 1, 120)],
		bands: [],
		exclusion_hz: 2,
		acquisition: { fs_hz: 8192, n_pts: 16384, n_avg: 4, tick_ms: 10 },
		schedule: {
			mode: 'auto',
			levels: [
				{ db: -12, dwell_s: 30 },
				{ db: -6, dwell_s: 30 },
				{ db: 0, dwell_s: 0 }
			],
			full_level_s: 300
		},
		control: { ...defaultControl(), part_ramp_s: 3, floor_settle_s: 30, trust_coherence: 0.5, trust_h_floor_frac: 0.05, tracking_hz: 0.5 },
		lawFloor: { id: DEFAULT_FLOOR_LAW, params: {} },
		lawTones: { id: DEFAULT_TONE_LAW, params: {} },
		guards: {
			floor: { law_budget: 0.25, law_misses: 3, max_change_db: 6 },
			tones: { law_budget: 0.25, law_misses: 3, max_step_db: 1, max_rise_db_per_s: 240, divergence_s: 1, fault_debounce_ticks: 50 }
		},
		limits: { drive_vpk_ceiling: NaN, drive_vpk_abort: NaN, kinematic_monitor: true, channels: [] },
		feasibility: { enabled: false, shaker_text: '', moving_mass_lb: { armature: 0, slip_table: 0, drive_adapter: 0, fixturing: 0, test_article: 0 }, crest_factor: 3 },
		record: defaultRecord(),
		metadata: [{ key: 'run_number', label: 'Run Number', value: '' }]
	};
}

const isList = (v: unknown): v is Record<string, unknown>[] => Array.isArray(v) && v.every((r) => r && typeof r === 'object');

function mergeSchedule(s: Partial<ScheduleDraft> | undefined, freq: number): ScheduleDraft {
	const base = newSchedule('fixed', freq);
	const out = { ...base, ...(s ?? {}) } as ScheduleDraft;
	out.dwell = { ...base.dwell, ...(s?.dwell ?? {}) };
	if (!Array.isArray(out.band_hz) || out.band_hz.length !== 2) out.band_hz = base.band_hz;
	return out;
}

/** A draft read back from storage, every section filled from the defaults where it is missing. */
export function mergeMixedSetup(saved: Partial<MixedDraft> | null): MixedDraft {
	const base = defaultMixedSetup();
	if (!saved || typeof saved !== 'object') return base;
	const out = { ...base, ...saved } as MixedDraft;
	out.floor = { ...base.floor, ...(saved.floor ?? {}) };
	if (!isList(out.floor.breakpoints) || !out.floor.breakpoints.length) out.floor.breakpoints = base.floor.breakpoints;
	out.tones = isList(saved.tones)
		? (saved.tones as Partial<MixedToneDraft>[]).map((t, i) => {
				const b = newTone(t.id ?? `t${i + 1}`, i + 1);
				const amp = { ...b.amplitude, ...(t.amplitude ?? {}) };
				amp.profile = { ...b.amplitude.profile, ...(t.amplitude?.profile ?? {}) };
				if (!isList(amp.profile.rows) || !amp.profile.rows.length) amp.profile.rows = b.amplitude.profile.rows;
				return { ...b, ...t, window: { ...b.window, ...(t.window ?? {}) }, schedule: mergeSchedule(t.schedule, 100), amplitude: amp } as MixedToneDraft;
			})
		: base.tones;
	out.bands = isList(saved.bands)
		? (saved.bands as Partial<MixedBandDraft>[]).map((t, i) => {
				const b = newBand(t.id ?? `b${i + 1}`, i + 1);
				return { ...b, ...t, window: { ...b.window, ...(t.window ?? {}) }, schedule: mergeSchedule(t.schedule, 250), width: { ...b.width, ...(t.width ?? {}) } } as MixedBandDraft;
			})
		: base.bands;
	out.acquisition = { ...base.acquisition, ...(saved.acquisition ?? {}) };
	out.schedule = { ...base.schedule, ...(saved.schedule ?? {}) };
	if (!isList(out.schedule.levels) || !out.schedule.levels.length) out.schedule.levels = base.schedule.levels;
	out.control = { ...base.control, ...(saved.control ?? {}) };
	out.control.pretest = { ...base.control.pretest, ...(saved.control?.pretest ?? {}) };
	if (!Array.isArray(out.control.weights)) out.control.weights = [];
	out.lawFloor = { ...base.lawFloor, ...(saved.lawFloor ?? {}) };
	out.lawTones = { ...base.lawTones, ...(saved.lawTones ?? {}) };
	out.guards = { floor: { ...base.guards.floor, ...(saved.guards?.floor ?? {}) }, tones: { ...base.guards.tones, ...(saved.guards?.tones ?? {}) } };
	out.limits = { ...base.limits, ...(saved.limits ?? {}) };
	if (!isList(out.limits.channels)) out.limits.channels = [];
	out.feasibility = { ...base.feasibility, ...(saved.feasibility ?? {}) };
	out.feasibility.moving_mass_lb = { ...base.feasibility.moving_mass_lb, ...(saved.feasibility?.moving_mass_lb ?? {}) };
	out.record = { ...base.record, ...(saved.record ?? {}) };
	delete (out as Partial<MixedDraft> & { capture?: unknown }).capture;
	if (!Array.isArray(out.metadata)) out.metadata = base.metadata;
	return out;
}

// ---------------------------------------------------------------------------------------------
// To and from the protocol

const pair = (v: readonly number[]): BandDb => [v[0], v[1]];
const finiteOrNull = (v: number | null): number | null => (v === null || !Number.isFinite(v) ? null : v);

export function scheduleOf(s: ScheduleDraft): MixedSchedule {
	if (s.kind === 'follow') return { kind: 'follow', of: s.of, ratio: s.ratio };
	if (s.kind === 'fixed') {
		const end: SineDwellFor | null = s.dwell.by === 'seconds' ? { seconds: s.dwell.seconds } : s.dwell.by === 'cycles' ? { cycles: s.dwell.cycles } : null;
		return { kind: 'fixed', freq_hz: s.freq_hz, end };
	}
	const end =
		s.end_by === 'none'
			? null
			: { sweeps: s.end_by === 'duration' ? null : s.sweeps, duration_s: s.end_by === 'sweeps' ? null : s.duration_s };
	return {
		kind: 'sweep',
		band_hz: [s.band_hz[0], s.band_hz[1]],
		rate: s.law === 'log' ? { law: 'log', oct_per_min: s.oct_per_min } : { law: 'linear', hz_per_s: s.hz_per_s },
		start_hz: s.start_hz ?? s.band_hz[0],
		direction: s.direction,
		end
	};
}

/** A schedule from the wire as the editor holds it. A `segments` schedule is not offered: it reads as fixed. */
export function scheduleDraftOf(s: MixedSchedule | undefined, freq: number): ScheduleDraft {
	const out = newSchedule('fixed', freq);
	if (!s) return out;
	if (s.kind === 'fixed') {
		out.freq_hz = s.freq_hz;
		out.band_hz = [s.freq_hz, s.freq_hz * 2];
		const e = s.end;
		if (e && 'seconds' in e) out.dwell = { ...out.dwell, by: 'seconds', seconds: e.seconds };
		else if (e && 'cycles' in e) out.dwell = { ...out.dwell, by: 'cycles', cycles: e.cycles };
	} else if (s.kind === 'sweep') {
		out.kind = 'sweep';
		out.band_hz = [s.band_hz[0], s.band_hz[1]];
		out.freq_hz = s.band_hz[0];
		out.law = s.rate.law;
		if (s.rate.law === 'log') out.oct_per_min = s.rate.oct_per_min;
		else out.hz_per_s = s.rate.hz_per_s;
		out.start_hz = s.start_hz ?? null;
		out.direction = s.direction ?? 'up';
		const sw = s.end?.sweeps ?? null;
		const du = s.end?.duration_s ?? null;
		out.end_by = !s.end || (sw === null && du === null) ? 'none' : sw !== null && du !== null ? 'both' : sw !== null ? 'sweeps' : 'duration';
		if (sw !== null) out.sweeps = sw;
		if (du !== null) out.duration_s = du;
	} else if (s.kind === 'follow') {
		out.kind = 'follow';
		out.of = s.of;
		out.ratio = s.ratio;
	}
	return out;
}

const windowOf = (w: WindowDraft): Required<MixedWindow> => ({ on_s: w.on_s, off_s: w.off_s });

export function floorOf(f: MixedFloorDraft): MixedFloorPart {
	return {
		id: f.id,
		name: f.name.trim() || 'Floor',
		kind: 'floor',
		enabled: true,
		window: { on_s: 0, off_s: null },
		breakpoints: f.breakpoints.map((b) => {
			const row = (b.kind === 'psd' ? { freq_hz: b.freq_hz, psd_g2_hz: b.psd_g2_hz } : { freq_hz: b.freq_hz, slope_db_oct: b.slope_db_oct }) as MixedFloorPart['breakpoints'][number];
			if (b.alarm_db) row.alarm_db = pair(b.alarm_db);
			if (b.abort_db) row.abort_db = pair(b.abort_db);
			return row;
		}),
		alarm_db: pair(f.alarm_db),
		abort_db: pair(f.abort_db)
	};
}

export function toneOf(t: MixedToneDraft): MixedTonePart {
	const base = { id: t.id.trim(), name: t.name.trim(), kind: 'tone' as const, enabled: t.enabled, window: windowOf(t.window), schedule: scheduleOf(t.schedule), alarm_db: pair(t.alarm_db), abort_db: pair(t.abort_db) };
	if (t.amplitude.by === 'constant') return { ...base, amplitude_g: t.amplitude.g };
	// The tone's bands are its row's: the profile carries only its rows (and any row's own bands).
	const { alarm_db: _a, abort_db: _b, ...profile } = profileOf(t.amplitude.profile);
	return { ...base, profile };
}

export function bandOf(b: MixedBandDraft): MixedBandPart {
	const base = {
		id: b.id.trim(),
		name: b.name.trim(),
		kind: 'narrowband' as const,
		enabled: b.enabled,
		window: windowOf(b.window),
		schedule: scheduleOf(b.schedule),
		level_g2_hz: b.level_g2_hz,
		alarm_db: b.own_bands ? pair(b.alarm_db) : null,
		abort_db: b.own_bands ? pair(b.abort_db) : null
	};
	return b.width.by === 'share' ? { ...base, width_share: b.width.share } : { ...base, width_hz: b.width.hz };
}

/** The parts in the order the wire carries them: the floor, the tones, the narrowbands. */
export function partsOf(d: Pick<MixedDraft, 'floor' | 'tones' | 'bands'>): MixedPart[] {
	return [floorOf(d.floor), ...d.tones.map(toneOf), ...d.bands.map(bandOf)];
}

/** Where each draft row sits in `parts`: what a service finding at `parts[<i>]` is about. */
export function partIndex(d: Pick<MixedDraft, 'tones' | 'bands'>, i: number): { table: 'floor' } | { table: 'tones' | 'bands'; row: number } | null {
	if (i === 0) return { table: 'floor' };
	if (i <= d.tones.length) return { table: 'tones', row: i - 1 };
	if (i <= d.tones.length + d.bands.length) return { table: 'bands', row: i - 1 - d.tones.length };
	return null;
}

/**
 * The configuration the draft describes, every field explicit. Call `localErrors` first. With the
 * channel table's rows, a weight is sent only for a row that is an enabled control row.
 */
export function toMixedConfig(d: MixedDraft, laws: { floor: LawEntry | null; tones: LawEntry | null }, rows?: ChannelRow[]): MixedConfig {
	const f = d.feasibility;
	const shaker = f.enabled ? shakerRecord(f.shaker_text) : null;
	const controlRows = rows ? new Set(rows.filter((r) => r.enabled && r.role === 'control').map((r) => r.index)) : null;
	const c = d.control;
	const last = d.schedule.levels.length - 1;
	const r = d.record;
	const [document, clause] = [r.document.trim(), r.clause.trim()];
	return {
		title: d.title.trim(),
		parts: partsOf(d),
		exclusion_hz: d.exclusion_hz,
		acquisition: { fs_hz: d.acquisition.fs_hz, n_pts: d.acquisition.n_pts, n_avg: d.acquisition.n_avg, tick_ms: d.acquisition.tick_ms },
		schedule: {
			mode: d.schedule.mode,
			levels: d.schedule.levels.map((l, i) => (i === last ? { db: l.db } : { db: l.db, dwell_s: l.dwell_s })),
			full_level_s: d.schedule.full_level_s
		},
		control: {
			composite: c.composite,
			weights: c.weights.filter((w) => !controlRows || controlRows.has(w.channel)).map((w) => ({ channel: w.channel, weight: w.weight })),
			control_dof: c.control_dof,
			damping: c.damping,
			max_lines_out: c.max_lines_out,
			max_loops_out: c.max_loops_out,
			max_sigma: finiteOrNull(c.max_sigma),
			drive_ceiling_vrms: c.drive_ceiling_vrms,
			drive_abort_vrms: finiteOrNull(c.drive_abort_vrms),
			coherence_floor: c.coherence_floor,
			review_after_sysid: c.review_after_sysid,
			pretest: { averages: c.pretest.averages, level_db: c.pretest.level_db, max_vrms: finiteOrNull(c.pretest.max_vrms), start_vrms: c.pretest.start_vrms, shaped: c.pretest.shaped },
			soft_start_db: c.soft_start_db,
			soft_start_relax_db: c.soft_start_relax_db,
			level_ramp_s: c.level_ramp_s,
			shutdown_ramp_s: c.shutdown_ramp_s,
			part_ramp_s: c.part_ramp_s,
			floor_settle_s: c.floor_settle_s,
			trust_coherence: c.trust_coherence,
			trust_h_floor_frac: c.trust_h_floor_frac,
			tracking: { kind: 'fixed', hz: c.tracking_hz },
			law: {
				floor: { id: d.lawFloor.id, params: lawWireParams(laws.floor, d.lawFloor.params) },
				tones: { id: d.lawTones.id, params: lawWireParams(laws.tones, d.lawTones.params) }
			},
			law_guards: { floor: { ...d.guards.floor }, tones: { ...d.guards.tones } }
		},
		limits: {
			drive_vpk_ceiling: d.limits.drive_vpk_ceiling,
			drive_vpk_abort: d.limits.drive_vpk_abort,
			kinematic_monitor: d.limits.kinematic_monitor,
			channels: d.limits.channels.map((l) => ({ channel: l.channel, kind: l.kind, rows: l.rows.map((r) => ({ freq_hz: r.freq_hz, ceiling: r.ceiling })) }))
		},
		feasibility: f.enabled && shaker ? { shaker, moving_mass_lb: { ...f.moving_mass_lb }, crest_factor: f.crest_factor } : null,
		operator: r.operator.trim(),
		article: r.article.trim(),
		notes: r.notes.trim(),
		specification: document || clause ? { document, clause } : null,
		record: { streams: r.streams },
		metadata: d.metadata.map((m) => (m.label ? { key: m.key, label: m.label, value: m.value } : { key: m.key, value: m.value }))
	};
}

/**
 * The draft for a configuration the service holds (another screen applied it, or this one before a
 * reload). Fields the configuration leaves out take the wire's defaults. The parts come back sorted
 * into the three tables, each in its order.
 */
export function fromMixedConfig(c: MixedConfig): MixedDraft {
	const base = defaultMixedSetup();
	const floor = c.parts.find((p): p is MixedFloorPart => p.kind === 'floor');
	const tones = c.parts.filter((p): p is MixedTonePart => p.kind === 'tone');
	const bands = c.parts.filter((p): p is MixedBandPart => p.kind === 'narrowband');
	const win = (w: MixedWindow | undefined): WindowDraft => ({ on_s: w?.on_s ?? 0, off_s: w?.off_s ?? null });
	const ctl = c.control ?? {};
	const g = ctl.law_guards ?? {};
	const pre = ctl.pretest ?? {};
	return {
		title: c.title ?? '',
		floor: floor
			? {
					id: floor.id,
					name: floor.name ?? 'Floor',
					breakpoints: floor.breakpoints.map((b) => ({
						freq_hz: b.freq_hz,
						kind: b.psd_g2_hz !== undefined ? 'psd' : 'slope',
						psd_g2_hz: b.psd_g2_hz ?? NaN,
						slope_db_oct: b.slope_db_oct ?? NaN,
						alarm_db: b.alarm_db ? pair(b.alarm_db) : null,
						abort_db: b.abort_db ? pair(b.abort_db) : null
					})),
					alarm_db: floor.alarm_db ? pair(floor.alarm_db) : [3, 3],
					abort_db: floor.abort_db ? pair(floor.abort_db) : [6, 6]
				}
			: base.floor,
		tones: tones.map((t, i) => {
			const b = newTone(t.id, i + 1);
			const freq = t.schedule.kind === 'fixed' ? t.schedule.freq_hz : t.schedule.kind === 'sweep' ? t.schedule.band_hz[0] : 100;
			return {
				id: t.id,
				name: t.name ?? `Tone ${i + 1}`,
				enabled: t.enabled ?? true,
				window: win(t.window),
				schedule: scheduleDraftOf(t.schedule, freq),
				amplitude: t.profile ? { by: 'profile', g: b.amplitude.g, profile: profileDraftOf(t.profile, b.amplitude.profile) } : { by: 'constant', g: t.amplitude_g ?? 1, profile: b.amplitude.profile },
				// a profile's own bands take the tone's place, as the service reads them
				alarm_db: t.profile?.alarm_db ? pair(t.profile.alarm_db) : t.alarm_db ? pair(t.alarm_db) : b.alarm_db,
				abort_db: t.profile?.abort_db ? pair(t.profile.abort_db) : t.abort_db ? pair(t.abort_db) : b.abort_db
			};
		}),
		bands: bands.map((p, i) => {
			const b = newBand(p.id, i + 1);
			const freq = p.schedule.kind === 'fixed' ? p.schedule.freq_hz : p.schedule.kind === 'sweep' ? p.schedule.band_hz[0] : 250;
			const own = !!(p.alarm_db || p.abort_db);
			return {
				id: p.id,
				name: p.name ?? `Narrowband ${i + 1}`,
				enabled: p.enabled ?? true,
				window: win(p.window),
				schedule: scheduleDraftOf(p.schedule, freq),
				level_g2_hz: p.level_g2_hz,
				width: p.width_share !== undefined ? { ...b.width, by: 'share', share: p.width_share } : { ...b.width, by: 'hz', hz: p.width_hz ?? b.width.hz },
				own_bands: own,
				alarm_db: p.alarm_db ? pair(p.alarm_db) : b.alarm_db,
				abort_db: p.abort_db ? pair(p.abort_db) : b.abort_db
			};
		}),
		exclusion_hz: c.exclusion_hz ?? 2,
		acquisition: { fs_hz: c.acquisition.fs_hz, n_pts: c.acquisition.n_pts, n_avg: c.acquisition.n_avg, tick_ms: c.acquisition.tick_ms ?? 10 },
		schedule: { mode: c.schedule.mode, levels: c.schedule.levels.map((l) => ({ db: l.db, dwell_s: l.dwell_s ?? 0 })), full_level_s: c.schedule.full_level_s },
		control: {
			...base.control,
			composite: (ctl.composite ?? 'weighted_avg') as CompositeMode,
			weights: (ctl.weights ?? []).map((w) => ({ ...w })),
			control_dof: ctl.control_dof ?? base.control.control_dof,
			damping: ctl.damping ?? base.control.damping,
			max_lines_out: ctl.max_lines_out ?? base.control.max_lines_out,
			max_loops_out: ctl.max_loops_out ?? base.control.max_loops_out,
			max_sigma: ctl.max_sigma === undefined ? base.control.max_sigma : ctl.max_sigma,
			drive_ceiling_vrms: ctl.drive_ceiling_vrms ?? base.control.drive_ceiling_vrms,
			drive_abort_vrms: ctl.drive_abort_vrms === undefined ? null : ctl.drive_abort_vrms,
			coherence_floor: ctl.coherence_floor ?? base.control.coherence_floor,
			review_after_sysid: ctl.review_after_sysid ?? true,
			pretest: {
				averages: pre.averages ?? base.control.pretest.averages,
				level_db: pre.level_db ?? base.control.pretest.level_db,
				max_vrms: pre.max_vrms === undefined ? base.control.pretest.max_vrms : pre.max_vrms,
				start_vrms: pre.start_vrms ?? base.control.pretest.start_vrms,
				shaped: pre.shaped ?? false
			},
			soft_start_db: ctl.soft_start_db ?? base.control.soft_start_db,
			soft_start_relax_db: ctl.soft_start_relax_db ?? base.control.soft_start_relax_db,
			level_ramp_s: ctl.level_ramp_s ?? base.control.level_ramp_s,
			shutdown_ramp_s: ctl.shutdown_ramp_s ?? base.control.shutdown_ramp_s,
			part_ramp_s: ctl.part_ramp_s ?? 3,
			floor_settle_s: ctl.floor_settle_s ?? 30,
			trust_coherence: ctl.trust_coherence ?? 0.5,
			trust_h_floor_frac: ctl.trust_h_floor_frac ?? 0.05,
			tracking_hz: ctl.tracking?.hz ?? 0.5
		},
		lawFloor: { id: ctl.law?.floor?.id ?? DEFAULT_FLOOR_LAW, params: { ...((ctl.law?.floor?.params ?? {}) as LawDraft['params']) } },
		lawTones: { id: ctl.law?.tones?.id ?? DEFAULT_TONE_LAW, params: { ...((ctl.law?.tones?.params ?? {}) as LawDraft['params']) } },
		guards: { floor: { ...base.guards.floor, ...(g.floor ?? {}) }, tones: { ...base.guards.tones, ...(g.tones ?? {}) } },
		limits: {
			drive_vpk_ceiling: c.limits.drive_vpk_ceiling,
			drive_vpk_abort: c.limits.drive_vpk_abort,
			kinematic_monitor: c.limits.kinematic_monitor ?? true,
			channels: (c.limits.channels ?? []).map((l) => ({ channel: l.channel, kind: l.kind, rows: l.rows.map((r) => ({ ...r })) }))
		},
		feasibility: c.feasibility
			? { enabled: true, shaker_text: JSON.stringify(c.feasibility.shaker, null, 2), moving_mass_lb: { ...c.feasibility.moving_mass_lb }, crest_factor: c.feasibility.crest_factor ?? 3 }
			: base.feasibility,
		record: {
			operator: c.operator ?? '',
			article: c.article ?? '',
			notes: c.notes ?? '',
			document: c.specification?.document ?? '',
			clause: c.specification?.clause ?? '',
			streams: c.record?.streams ?? true
		},
		metadata: (c.metadata ?? []).map((m) => ({ key: m.key, label: m.label ?? '', value: m.value }))
	};
}

// ---------------------------------------------------------------------------------------------
// Where a finding belongs

export type MixedSection = 'station' | 'floor' | 'parts' | 'schedule' | 'control' | 'limits' | 'record';

export const MIXED_SECTIONS: { id: MixedSection; label: string }[] = [
	{ id: 'station', label: 'Station & Channels' },
	{ id: 'floor', label: 'Floor' },
	{ id: 'parts', label: 'Tones & Bands' },
	{ id: 'schedule', label: 'Schedule' },
	{ id: 'control', label: 'Control' },
	{ id: 'limits', label: 'Limits' },
	{ id: 'record', label: 'Run Record' }
];

/** The column of a tone's or a narrowband's row a field path inside the part lands on. */
function partColumn(rest: string): string {
	const head = rest.split('.')[0];
	if (head === 'amplitude_g' || head === 'profile') return 'amplitude';
	if (head === 'width_hz' || head === 'width_share') return 'width';
	if (head === 'level_g2_hz') return 'level';
	if (['id', 'name', 'enabled', 'window', 'schedule', 'alarm_db', 'abort_db'].includes(head)) return head;
	return 'row';
}

const FLOOR_COLUMN: Record<string, string> = {
	first_row_not_anchor: 'kind',
	row_level_ambiguous: 'kind',
	frequency_not_positive: 'freq_hz',
	frequency_not_ascending: 'freq_hz',
	psd_negative: 'value',
	alarm_wider_than_abort: 'bands'
};

/**
 * The editor's key for a service finding, against the draft it was found on. A part's findings
 * go on its row and column: the floor's under Random's profile keys (`profile.breakpoints.<row>.<col>`,
 * `profile.alarm_db`), a tone's under `tones.<row>.<column>` and a narrowband's under
 * `bands.<row>.<column>` (a finding about the whole part, such as one out of the band, under
 * `tones.<row>.row`). The two laws go under `lawFloor.*` and `lawTones.*`, their guards under
 * `guards.floor.*` and `guards.tones.*`; a control or drive row finding under the station.
 */
export function mixedItemKey(d: Pick<MixedDraft, 'tones' | 'bands'>, item: Pick<ReportItem, 'field' | 'channel' | 'row'> & { reason: string }): string {
	if (item.reason === 'no_control_channel' || item.reason === 'no_drive_channel') return 'station.channels';
	if (item.reason === 'too_many_tones') return 'tones';
	if (item.reason === 'too_many_bands') return 'bands';
	if (item.reason === 'no_floor' || item.reason === 'too_many_floors') return 'profile.breakpoints';
	const path = (item.field ?? '').replace(/^config\./, '').replace(/\[(\d+)\]/g, '.$1');
	const m = /^parts\.(\d+)(?:\.(.*))?$/.exec(path);
	if (m) {
		const at = partIndex(d, Number(m[1]));
		const rest = m[2] ?? '';
		if (!at) return 'parts';
		if (at.table === 'floor') {
			if (rest.startsWith('breakpoints')) {
				const row = item.row ?? Number(/^breakpoints\.(\d+)/.exec(rest)?.[1] ?? NaN);
				return Number.isInteger(row) ? `profile.breakpoints.${row}.${FLOOR_COLUMN[item.reason] ?? 'row'}` : 'profile.breakpoints';
			}
			if (rest === 'alarm_db' || rest === 'abort_db') return `profile.${rest}`;
			return 'profile.breakpoints';
		}
		return `${at.table}.${at.row}.${rest ? partColumn(rest) : 'row'}`;
	}
	if (path === 'parts') return 'parts';
	if (item.field) {
		if (/^schedule\.levels\.\d+/.test(path)) return path.split('.').slice(0, 3).join('.');
		const law = /^control\.law\.(floor|tones)(?:\.(id|params)(?:\.(.+))?)?$/.exec(path);
		if (law) return `${law[1] === 'floor' ? 'lawFloor' : 'lawTones'}.${law[2] === 'params' && law[3] ? `params.${law[3]}` : 'id'}`;
		const guard = /^control\.law_guards\.(floor|tones)\.(\w+)/.exec(path);
		if (guard) return `guards.${guard[1]}.${guard[2]}`;
		if (/^control\.tracking(\.|$)/.test(path)) return 'control.tracking_hz';
		if (/^control\.weights(\.|$)/.test(path)) return 'control.weights';
		const lim = /^limits\.channels\.(\d+)/.exec(path);
		if (lim) return `limits.channels.${lim[1]}`;
		if (/^feasibility(\.|$)/.test(path) && !/moving_mass|crest_factor/.test(path)) return 'feasibility.shaker';
		return path;
	}
	if (item.reason === 'limit_unknown_channel' || item.reason === 'limit_on_control_channel') return 'limits.channels';
	if (item.reason === 'weight_unknown_channel') return 'control.weights';
	if (item.channel !== undefined && item.channel !== null) return 'station.channels';
	return 'setup';
}

export function mixedByKey(d: Pick<MixedDraft, 'tones' | 'bands'>, items: { field?: string | null; channel?: number | null; row?: number; reason: string; message: string }[]): FieldErrors {
	const out: FieldErrors = {};
	for (const it of items) {
		const k = mixedItemKey(d, it as Pick<ReportItem, 'field' | 'channel' | 'row'> & { reason: string });
		if (!(out[k] ??= []).includes(it.message)) out[k].push(it.message);
	}
	return out;
}

/** The section a key belongs to. */
export function mixedSectionOf(key: string): MixedSection {
	if (/^(station|table|channel)/.test(key)) return 'station';
	if (/^(title|profile)(\.|$)/.test(key)) return 'floor';
	if (/^(tones|bands|parts|exclusion_hz)(\.|$)/.test(key)) return 'parts';
	if (/^(schedule|control\.(pretest|coherence_floor|review_after_sysid))(\.|$)/.test(key)) return 'schedule';
	if (/^(lawFloor|lawTones|guards|control|acquisition)(\.|$)/.test(key)) return 'control';
	if (/^(limits|feasibility)(\.|$)/.test(key)) return 'limits';
	if (/^(operator|article|notes|specification|record|metadata)(\.|$)/.test(key)) return 'record';
	return 'floor';
}

// ---------------------------------------------------------------------------------------------
// What the editor can say before anything is sent

const NUMBER = 'Enter a number.';
const WHOLE = 'Enter a whole number.';
const ID = /^[A-Za-z0-9_-]{1,32}$/;

function add(map: FieldErrors, key: string, message: string) {
	if (!(map[key] ??= []).includes(message)) map[key].push(message);
}

function need(map: FieldErrors, key: string, v: number, whole = false) {
	if (!Number.isFinite(v)) add(map, key, whole ? WHOLE : NUMBER);
	else if (whole && !Number.isInteger(v)) add(map, key, WHOLE);
}

function needPair(map: FieldErrors, key: string, v: readonly number[]) {
	if (!(Number.isFinite(v[0]) && Number.isFinite(v[1]))) add(map, key, 'Give both: below and above.');
}

function scheduleErrors(out: FieldErrors, s: ScheduleDraft, at: string, id: string, ids: Set<string>) {
	if (s.kind === 'fixed') {
		need(out, at, s.freq_hz);
		if (s.dwell.by === 'seconds') need(out, at, s.dwell.seconds);
		if (s.dwell.by === 'cycles') need(out, at, s.dwell.cycles, true);
	} else if (s.kind === 'sweep') {
		need(out, at, s.band_hz[0]);
		need(out, at, s.band_hz[1]);
		need(out, at, s.law === 'log' ? s.oct_per_min : s.hz_per_s);
		if (s.start_hz !== null) need(out, at, s.start_hz);
		if (s.end_by === 'sweeps' || s.end_by === 'both') need(out, at, s.sweeps, true);
		if (s.end_by === 'duration' || s.end_by === 'both') need(out, at, s.duration_s);
	} else {
		need(out, at, s.ratio);
		if (!s.of) add(out, at, 'Choose the part it follows.');
		else if (s.of === id) add(out, at, 'A part cannot follow itself.');
		else if (!ids.has(s.of)) add(out, at, `There is no tone or narrowband "${s.of}".`);
	}
}

function windowErrors(out: FieldErrors, w: WindowDraft, at: string) {
	need(out, at, w.on_s);
	if (w.off_s !== null) need(out, at, w.off_s);
}

/**
 * Fields that hold no number, a title that is empty, an id that is not one, two parts of one id, a
 * follower with no part to follow, too many tones or bands, a drive ceiling or abort not given, a
 * shaker record that does not read. Everything else (the motion checks, the parts' spacing, the
 * profile's shape) is the service's to judge in the dry run.
 */
export function localErrors(d: MixedDraft): FieldErrors {
	const out: FieldErrors = {};
	if (!d.title.trim()) add(out, 'title', 'Give the test a title.');
	d.floor.breakpoints.forEach((b, i) => {
		const k = `profile.breakpoints.${i}`;
		need(out, `${k}.freq_hz`, b.freq_hz);
		need(out, `${k}.value`, b.kind === 'psd' ? b.psd_g2_hz : b.slope_db_oct);
		for (const band of [b.alarm_db, b.abort_db]) if (band && !(Number.isFinite(band[0]) && Number.isFinite(band[1]))) add(out, `${k}.bands`, 'Give both sides of the band, or use the profile bands.');
	});
	needPair(out, 'profile.alarm_db', d.floor.alarm_db);
	needPair(out, 'profile.abort_db', d.floor.abort_db);
	if (d.tones.length > MIXED_MAX_TONES) add(out, 'tones', `At most ${MIXED_MAX_TONES} tones.`);
	if (d.bands.length > MIXED_MAX_BANDS) add(out, 'bands', `At most ${MIXED_MAX_BANDS} narrowbands.`);
	const all = [d.floor.id, ...d.tones.map((t) => t.id.trim()), ...d.bands.map((b) => b.id.trim())];
	const moving = new Set([...d.tones.map((t) => t.id.trim()), ...d.bands.map((b) => b.id.trim())]);
	const seen = new Map<string, number>();
	for (const id of all) seen.set(id, (seen.get(id) ?? 0) + 1);
	const rowCheck = (table: 'tones' | 'bands', i: number, p: MixedToneDraft | MixedBandDraft) => {
		const id = p.id.trim();
		const at = `${table}.${i}`;
		if (!ID.test(id)) add(out, `${at}.id`, 'An id is 1 to 32 letters, digits, _ or -.');
		else if ((seen.get(id) ?? 0) > 1) add(out, `${at}.id`, `Another part is "${id}" too.`);
		if (!p.name.trim()) add(out, `${at}.name`, 'Give the part a name.');
		windowErrors(out, p.window, `${at}.window`);
		scheduleErrors(out, p.schedule, `${at}.schedule`, id, moving);
		needPair(out, `${at}.alarm_db`, p.alarm_db);
		needPair(out, `${at}.abort_db`, p.abort_db);
	};
	d.tones.forEach((t, i) => {
		rowCheck('tones', i, t);
		if (t.amplitude.by === 'constant') need(out, `tones.${i}.amplitude`, t.amplitude.g);
		else {
			const p = t.amplitude.profile;
			if (p.rows.length < 2) add(out, `tones.${i}.amplitude`, 'Give the amplitude profile at least two rows.');
			for (const r of p.rows) {
				need(out, `tones.${i}.amplitude`, r.freq_hz);
				need(out, `tones.${i}.amplitude`, r.value);
			}
		}
	});
	d.bands.forEach((b, i) => {
		rowCheck('bands', i, b);
		need(out, `bands.${i}.level`, b.level_g2_hz);
		need(out, `bands.${i}.width`, b.width.by === 'hz' ? b.width.hz : b.width.share);
	});
	need(out, 'exclusion_hz', d.exclusion_hz);
	need(out, 'acquisition.fs_hz', d.acquisition.fs_hz);
	need(out, 'acquisition.n_pts', d.acquisition.n_pts, true);
	need(out, 'acquisition.n_avg', d.acquisition.n_avg, true);
	need(out, 'acquisition.tick_ms', d.acquisition.tick_ms);
	const last = d.schedule.levels.length - 1;
	if (!d.schedule.levels.length) add(out, 'schedule.levels', 'Give at least one level.');
	d.schedule.levels.forEach((l, i) => {
		need(out, `schedule.levels.${i}.db`, l.db);
		if (i < last) need(out, `schedule.levels.${i}.dwell_s`, l.dwell_s);
	});
	need(out, 'schedule.full_level_s', d.schedule.full_level_s);
	const c = d.control;
	for (const k of ['control_dof', 'damping', 'drive_ceiling_vrms', 'coherence_floor', 'soft_start_db', 'soft_start_relax_db', 'level_ramp_s', 'shutdown_ramp_s', 'part_ramp_s', 'floor_settle_s', 'trust_coherence', 'trust_h_floor_frac', 'tracking_hz'] as const) need(out, `control.${k}`, c[k]);
	need(out, 'control.max_lines_out', c.max_lines_out, true);
	need(out, 'control.max_loops_out', c.max_loops_out, true);
	if (c.max_sigma !== null) need(out, 'control.max_sigma', c.max_sigma);
	if (c.drive_abort_vrms !== null) need(out, 'control.drive_abort_vrms', c.drive_abort_vrms);
	need(out, 'control.pretest.averages', c.pretest.averages, true);
	need(out, 'control.pretest.level_db', c.pretest.level_db);
	if (c.pretest.max_vrms !== null) need(out, 'control.pretest.max_vrms', c.pretest.max_vrms);
	need(out, 'control.pretest.start_vrms', c.pretest.start_vrms);
	if (c.weights.some((w) => !Number.isFinite(w.weight))) add(out, 'control.weights', 'Give every control channel a weight.');
	for (const [k, v] of Object.entries(d.guards.floor)) need(out, `guards.floor.${k}`, v, k === 'law_misses');
	for (const [k, v] of Object.entries(d.guards.tones)) need(out, `guards.tones.${k}`, v, k === 'law_misses' || k === 'fault_debounce_ticks');
	const lim = d.limits;
	if (!Number.isFinite(lim.drive_vpk_ceiling)) add(out, 'limits.drive_vpk_ceiling', 'Give the drive ceiling in volts peak. There is no default: it is your amplifier input.');
	if (!Number.isFinite(lim.drive_vpk_abort)) add(out, 'limits.drive_vpk_abort', 'Give the drive abort in volts peak, at least the ceiling.');
	else if (Number.isFinite(lim.drive_vpk_ceiling) && lim.drive_vpk_abort < lim.drive_vpk_ceiling) add(out, 'limits.drive_vpk_abort', 'The drive abort is at least the drive ceiling.');
	lim.channels.forEach((l, i) => {
		if (l.rows.length < 1) add(out, `limits.channels.${i}`, 'Give the limit at least one row.');
		for (const r of l.rows) {
			need(out, `limits.channels.${i}`, r.freq_hz);
			need(out, `limits.channels.${i}`, r.ceiling);
		}
	});
	const f = d.feasibility;
	if (f.enabled) {
		if (!f.shaker_text.trim()) add(out, 'feasibility.shaker', 'Load or paste the shaker record, or turn the check off.');
		else if (!shakerRecord(f.shaker_text)) add(out, 'feasibility.shaker', 'The shaker record does not read as a record (JSON object).');
		for (const [k, v] of Object.entries(f.moving_mass_lb)) need(out, `feasibility.moving_mass_lb.${k}`, v);
		need(out, 'feasibility.crest_factor', f.crest_factor);
	}
	if (d.record.clause.trim() && !d.record.document.trim()) add(out, 'specification.document', 'Name the document this clause is from.');
	return out;
}

// ---------------------------------------------------------------------------------------------
// Rows edited together

/** The parts a follower may follow: every other tone and narrowband, by id and name. */
export function followChoices(d: Pick<MixedDraft, 'tones' | 'bands'>, self: string): { id: string; name: string }[] {
	return [...d.tones, ...d.bands].filter((p) => p.id !== self).map((p) => ({ id: p.id, name: p.name || p.id }));
}

/** A plain copy of a draft value (a reactive one included), NaN kept. */
export function deepCopy<T>(v: T): T {
	if (Array.isArray(v)) return v.map(deepCopy) as T;
	if (v !== null && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, deepCopy(x)])) as T;
	return v;
}

/**
 * One field set on several rows at once: the value at `path` of `from` copied into each row of
 * `rows` but `from` (a part's id and name stay its own). Returns the rows changed.
 */
export function copyField<T extends MixedToneDraft | MixedBandDraft>(rows: T[], targets: number[], from: number, path: string): number {
	if (path === 'id' || path === 'name') return 0;
	const src = rows[from];
	if (!src) return 0;
	const keys = path.split('.');
	const read = (o: unknown) => keys.reduce<unknown>((v, k) => (v as Record<string, unknown> | undefined)?.[k], o);
	const value = read(src);
	let n = 0;
	for (const i of targets) {
		if (i === from || !rows[i]) continue;
		let o = rows[i] as unknown as Record<string, unknown>;
		for (const k of keys.slice(0, -1)) o = o[k] as Record<string, unknown>;
		o[keys[keys.length - 1]] = deepCopy(value);
		n++;
	}
	return n;
}
