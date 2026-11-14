/**
 * The SRS shock setup as the operator edits it, and its conversion to and from the protocol's
 * `SrsConfig` (schema/PROTOCOL.md, "SRS Shock Configuration"). Pure functions only: the service
 * computes every number about the test (the band grid, the target on it, the synthesis and its
 * kinematics, the grid, the time between pulses), and this module only carries the operator's
 * choices.
 *
 * Numbers are held as numbers; a field that holds no number is NaN, which `localErrors` names
 * before anything is sent. A choice the wire leaves to the service's default (the identification
 * burst's volts cap, the predicted peak ceiling, a kinematic ceiling) is null. The drive ceiling
 * and the drive abort have no default anywhere: the setup asks for both.
 *
 * The shell's own defaults differ from the wire's on purpose: a ladder that advances on a passing
 * pulse with at most five pulses a level. A per-band envelope and a per-band limit bound have one
 * entry per band of the grid the service derives; the editor offers them once the service has
 * said what the bands are.
 */

import type { ChannelRow, LadderMode, ReportItem } from '$lib/protocol/types';
import type { LawEntry } from '$lib/protocol/mimo';
import type { ShockAdvance, ShockMonitor } from '$lib/protocol/shock';
import type { SrsBandDb, SrsConfig, SrsDampedSineRow, SrsEngine, SrsExcitation, SrsPreviewParams, SrsSynthesis, SrsTarget, SrsWaveletRow } from '$lib/protocol/srs';
import { lawWireParams, type LawDraft } from '../law/lawForm';
import { defaultRecord, shakerRecord, type FieldErrors, type RecordDraft } from '../setup';
import { newMonitor, runChannels, type ShockDraft } from '../shock/shockSetup';

export { newMonitor, runChannels };

// ---------------------------------------------------------------------------------------------
// The draft

export interface SrsPointDraft {
	freq_hz: number;
	accel_g: number;
}

export interface SrsFrfPointDraft {
	freq_hz: number;
	mag: number;
}

export interface SrsLimitChannelDraft {
	/** The channel table row's `index`. */
	channel: number;
	bound: {
		kind: 'flat' | 'per_band';
		accel_g: number;
		/** One per band; null is no bound at that band. */
		bounds_g: (number | null)[];
	};
	/** The drive-to-channel gain the operator supplies; off, identification measures it. */
	frf: { supplied: boolean; points: SrsFrfPointDraft[] };
}

export interface SrsEnvelopeBandDraft {
	alarm_db: SrsBandDb;
	abort_db: SrsBandDb;
}

export interface SrsDraft {
	title: string;
	/** Both forms are kept, so a change of kind loses nothing; the wire carries the chosen one. */
	target: { kind: 'flat' | 'breakpoints'; accel_g: number; points: SrsPointDraft[] };
	bands: { f_lo_hz: number; f_hi_hz: number; bands_per_octave: number; q: number };
	synthesis: {
		engine: SrsEngine;
		/** Start from the component table below; off, one component per band. */
		table: boolean;
		/** Each engine keeps its own rows; the wire carries the chosen engine's. */
		wavelets: SrsWaveletRow[];
		sines: SrsDampedSineRow[];
		max_iters: number;
		tol_db: number;
		max_gain_db_per_iter: number;
	};
	envelope: {
		/** One alarm and abort band per band of the grid, in place of the two below. */
		per_band: boolean;
		/** `[low, high]` dB: how far under and over the target a band may be. */
		alarm_db: SrsBandDb;
		abort_db: SrsBandDb;
		bands: SrsEnvelopeBandDraft[];
	};
	acquisition: { fs_hz: number; ring_out_s: number; pvss: boolean };
	schedule: {
		mode: LadderMode;
		levels: { db: number; pulses: number }[];
		start_db: number;
		advance: ShockAdvance;
		max_pulses_per_level: number;
	};
	spacing: { cooling_s: number; settling_s: number; compute_s: number };
	identification: {
		excitation: SrsExcitation;
		level_db: number;
		/** Volts peak; null: a tenth of the drive ceiling. */
		max_vpk: number | null;
		nfft: number;
		h_floor_frac: number;
		min_mean_coherence: number;
		review: boolean;
	};
	law: LawDraft;
	control: { law_budget: number; law_misses: number; max_pulses: number; max_gain_rise_db: number };
	limits: {
		/** Volts peak; NaN until the operator gives it. */
		drive_vpk_ceiling: number;
		drive_vpk_abort: number;
		/** A predicted response peak over it scales the pulse down; null: none. */
		peak_accel_g: number | null;
		abort_factor: number;
		kinematic: { accel_g: number | null; velocity_ips: number | null; disp_pp_in: number | null };
		monitors: ShockMonitor[];
	};
	limit_channels: { enabled: boolean; headroom_db: number; channels: SrsLimitChannelDraft[] };
	feasibility: ShockDraft['feasibility'];
	/** The run's record: the operator's words and whether the folder keeps the time histories (Random's). */
	record: RecordDraft;
	metadata: { key: string; label: string; value: string }[];
}

export const DEFAULT_SRS_LAW = 'srs_ratio';

export const ENGINES: { id: SrsEngine; label: string; help: string }[] = [
	{ id: 'wavsyn', label: 'Wavelets', help: 'Odd-half-cycle wavelets, one per band: at rest at both ends by construction.' },
	{ id: 'seds', label: 'Damped sines', help: 'Damped sines with the compensating terms that bring the pulse to rest.' }
];

export const EXCITATIONS: { id: SrsExcitation; label: string; help: string }[] = [
	{ id: 'shaped', label: 'The synthesized pulse', help: 'A quiet copy of the test pulse: the energy goes where the test drives.' },
	{ id: 'standard_pulse', label: 'A short broadband pulse', help: 'A short pulse that excites every band evenly.' }
];

/** Bands an octave the editor offers; the wire takes any whole number from 1 to 24. */
export const BANDS_PER_OCTAVE = [1, 3, 6, 12];

export const NFFTS = [256, 512, 1024, 2048, 4096, 8192];

export function newWavelet(freq_hz = 100): SrsWaveletRow {
	return { freq_hz, amplitude_g: 1, half_cycles: 5, delay_s: 0 };
}

export function newDampedSine(freq_hz = 100): SrsDampedSineRow {
	return { freq_hz, amplitude_g: 1, zeta: 0.05, delay_s: 0, polarity: 'positive' };
}

export function newLimitChannel(row: ChannelRow | undefined): SrsLimitChannelDraft {
	return { channel: row?.index ?? 0, bound: { kind: 'flat', accel_g: NaN, bounds_g: [] }, frf: { supplied: false, points: [] } };
}

export function defaultSrsSetup(): SrsDraft {
	return {
		title: 'SRS 100 g, 100 to 2000 Hz',
		target: {
			kind: 'breakpoints',
			accel_g: 100,
			points: [
				{ freq_hz: 100, accel_g: 40 },
				{ freq_hz: 1000, accel_g: 100 },
				// past the grid's last band, which sits at or just above its top (2016 Hz here)
				{ freq_hz: 2500, accel_g: 100 }
			]
		},
		bands: { f_lo_hz: 100, f_hi_hz: 2000, bands_per_octave: 3, q: 10 },
		synthesis: { engine: 'wavsyn', table: false, wavelets: [], sines: [], max_iters: 50, tol_db: 1.5, max_gain_db_per_iter: 3 },
		envelope: { per_band: false, alarm_db: [3, 3], abort_db: [9, 6], bands: [] },
		acquisition: { fs_hz: 51200, ring_out_s: 0.05, pvss: true },
		schedule: {
			mode: 'auto',
			levels: [
				{ db: -12, pulses: 2 },
				{ db: -6, pulses: 2 },
				{ db: 0, pulses: 3 }
			],
			start_db: -12,
			advance: 'on_pass',
			max_pulses_per_level: 5
		},
		spacing: { cooling_s: 1, settling_s: 0.5, compute_s: 0.25 },
		identification: { excitation: 'shaped', level_db: -20, max_vpk: null, nfft: 1024, h_floor_frac: 0.05, min_mean_coherence: 0.8, review: true },
		law: { id: DEFAULT_SRS_LAW, params: {} },
		control: { law_budget: 0.25, law_misses: 3, max_pulses: 100, max_gain_rise_db: 6 },
		limits: {
			drive_vpk_ceiling: NaN,
			drive_vpk_abort: NaN,
			peak_accel_g: null,
			abort_factor: 2,
			kinematic: { accel_g: null, velocity_ips: null, disp_pp_in: null },
			monitors: []
		},
		limit_channels: { enabled: false, headroom_db: 1, channels: [] },
		feasibility: {
			enabled: false,
			shaker_text: '',
			moving_mass_lb: { armature: 0, slip_table: 0, drive_adapter: 0, fixturing: 0, test_article: 0 },
			residual_velocity_ips: 0.1,
			residual_disp_in: 0.001
		},
		record: defaultRecord(),
		metadata: [{ key: 'run_number', label: 'Run Number', value: '' }]
	};
}

/** A draft read back from storage, every section filled from the defaults where it is missing. */
export function mergeSrsSetup(saved: Partial<SrsDraft> | null): SrsDraft {
	const base = defaultSrsSetup();
	if (!saved || typeof saved !== 'object') return base;
	const out = { ...base, ...saved } as SrsDraft;
	out.target = { ...base.target, ...(saved.target ?? {}) };
	if (!Array.isArray(out.target.points)) out.target.points = base.target.points;
	out.bands = { ...base.bands, ...(saved.bands ?? {}) };
	out.synthesis = { ...base.synthesis, ...(saved.synthesis ?? {}) };
	if (!Array.isArray(out.synthesis.wavelets)) out.synthesis.wavelets = [];
	if (!Array.isArray(out.synthesis.sines)) out.synthesis.sines = [];
	out.envelope = { ...base.envelope, ...(saved.envelope ?? {}) };
	if (!Array.isArray(out.envelope.bands)) out.envelope.bands = [];
	out.acquisition = { ...base.acquisition, ...(saved.acquisition ?? {}) };
	out.schedule = { ...base.schedule, ...(saved.schedule ?? {}) };
	if (!Array.isArray(out.schedule.levels) || !out.schedule.levels.length) out.schedule.levels = base.schedule.levels;
	out.spacing = { ...base.spacing, ...(saved.spacing ?? {}) };
	out.identification = { ...base.identification, ...(saved.identification ?? {}) };
	out.law = { ...base.law, ...(saved.law ?? {}) };
	out.control = { ...base.control, ...(saved.control ?? {}) };
	out.limits = { ...base.limits, ...(saved.limits ?? {}) };
	out.limits.kinematic = { ...base.limits.kinematic, ...(saved.limits?.kinematic ?? {}) };
	if (!Array.isArray(out.limits.monitors)) out.limits.monitors = [];
	out.limit_channels = { ...base.limit_channels, ...(saved.limit_channels ?? {}) };
	if (!Array.isArray(out.limit_channels.channels)) out.limit_channels.channels = [];
	out.feasibility = { ...base.feasibility, ...(saved.feasibility ?? {}) };
	out.feasibility.moving_mass_lb = { ...base.feasibility.moving_mass_lb, ...(saved.feasibility?.moving_mass_lb ?? {}) };
	// A draft saved before the run's record took the capture's place has a capture and no record.
	delete (out as Partial<SrsDraft> & { capture?: unknown }).capture;
	out.record = { ...base.record, ...(saved.record ?? {}) };
	if (!Array.isArray(out.metadata)) out.metadata = base.metadata;
	return out;
}

// ---------------------------------------------------------------------------------------------
// To and from the protocol

const pair = (v: readonly number[]): SrsBandDb => [v[0], v[1]];

function targetOf(d: SrsDraft): SrsTarget {
	return d.target.kind === 'flat' ? { kind: 'flat', accel_g: d.target.accel_g } : { kind: 'breakpoints', points: d.target.points.map((p) => [p.freq_hz, p.accel_g] as [number, number]) };
}

function synthesisOf(d: SrsDraft): SrsSynthesis {
	const s = d.synthesis;
	const components = !s.table ? null : s.engine === 'wavsyn' ? { engine: 'wavsyn' as const, rows: s.wavelets.map((r) => ({ ...r })) } : { engine: 'seds' as const, rows: s.sines.map((r) => ({ ...r })) };
	return { engine: s.engine, components, max_iters: s.max_iters, tol_db: s.tol_db, max_gain_db_per_iter: s.max_gain_db_per_iter };
}

function bandsOf(d: SrsDraft) {
	return { f_lo_hz: d.bands.f_lo_hz, f_hi_hz: d.bands.f_hi_hz, bands_per_octave: d.bands.bands_per_octave, q: d.bands.q };
}

/** The configuration the draft describes, every field explicit. Call `localErrors` first. */
export function toSrsConfig(d: SrsDraft, law: LawEntry | null): SrsConfig {
	const f = d.feasibility;
	const shaker = f.enabled ? shakerRecord(f.shaker_text) : null;
	const lc = d.limit_channels;
	return {
		title: d.title.trim(),
		target: targetOf(d),
		bands: bandsOf(d),
		synthesis: synthesisOf(d),
		envelope: {
			alarm_db: pair(d.envelope.alarm_db),
			abort_db: pair(d.envelope.abort_db),
			per_band: d.envelope.per_band ? d.envelope.bands.map((b) => ({ alarm_db: pair(b.alarm_db), abort_db: pair(b.abort_db) })) : null
		},
		acquisition: { ...d.acquisition },
		schedule: {
			mode: d.schedule.mode,
			levels_db: d.schedule.levels.map((l) => l.db),
			pulses: d.schedule.levels.map((l) => l.pulses),
			start_db: d.schedule.start_db,
			advance: d.schedule.advance,
			max_pulses_per_level: d.schedule.max_pulses_per_level
		},
		spacing: { ...d.spacing },
		identification: { ...d.identification },
		control: { law: { id: d.law.id, params: lawWireParams(law, d.law.params) }, ...d.control },
		limits: {
			drive_vpk_ceiling: d.limits.drive_vpk_ceiling,
			drive_vpk_abort: d.limits.drive_vpk_abort,
			peak_accel_g: d.limits.peak_accel_g,
			abort_factor: d.limits.abort_factor,
			kinematic: { ...d.limits.kinematic },
			monitors: d.limits.monitors.map((m) => ({ ...m }))
		},
		limit_channels: lc.enabled
			? {
					headroom_db: lc.headroom_db,
					channels: lc.channels.map((c) => ({
						channel: c.channel,
						bound: c.bound.kind === 'flat' ? { kind: 'flat', accel_g: c.bound.accel_g } : { kind: 'per_band', bounds_g: [...c.bound.bounds_g] },
						frf: c.frf.supplied ? { freqs_hz: c.frf.points.map((p) => p.freq_hz), mag: c.frf.points.map((p) => p.mag) } : null
					}))
				}
			: null,
		feasibility:
			f.enabled && shaker
				? { shaker, moving_mass_lb: { ...f.moving_mass_lb }, residual_velocity_ips: f.residual_velocity_ips, residual_disp_in: f.residual_disp_in }
				: null,
		operator: d.record.operator.trim(),
		article: d.record.article.trim(),
		notes: d.record.notes.trim(),
		specification: d.record.document.trim() || d.record.clause.trim() ? { document: d.record.document.trim(), clause: d.record.clause.trim() } : null,
		record: { streams: d.record.streams },
		metadata: d.metadata.map((m) => ({ ...m }))
	};
}

/**
 * The draft for a configuration the service holds (another screen applied it, or this one before a
 * reload). Fields the configuration leaves out take the wire's defaults where the wire has one, and
 * the shell's where it does not; one pulse count for every level is spread over the levels.
 */
export function fromSrsConfig(c: SrsConfig): SrsDraft {
	const base = defaultSrsSetup();
	const levels = c.schedule.levels_db.map((db, i) => ({ db, pulses: c.schedule.pulses.length === 1 ? c.schedule.pulses[0] : (c.schedule.pulses[i] ?? NaN) }));
	const t = c.target;
	const s = c.synthesis ?? {};
	const comp = s.components ?? null;
	const env = c.envelope ?? {};
	const lc = c.limit_channels ?? null;
	const { law, ...control } = c.control ?? {};
	return {
		title: c.title,
		target:
			t.kind === 'flat'
				? { kind: 'flat', accel_g: t.accel_g, points: base.target.points }
				: { kind: 'breakpoints', accel_g: base.target.accel_g, points: t.points.map(([freq_hz, accel_g]) => ({ freq_hz, accel_g })) },
		bands: {
			f_lo_hz: c.bands.f_lo_hz,
			f_hi_hz: c.bands.f_hi_hz,
			bands_per_octave: c.bands.bands_per_octave ?? base.bands.bands_per_octave,
			q: c.bands.q ?? base.bands.q
		},
		synthesis: {
			engine: s.engine ?? 'wavsyn',
			table: comp !== null,
			wavelets: comp?.engine === 'wavsyn' ? comp.rows.map((r) => ({ ...r })) : [],
			sines: comp?.engine === 'seds' ? comp.rows.map((r) => ({ ...r })) : [],
			max_iters: s.max_iters ?? base.synthesis.max_iters,
			tol_db: s.tol_db ?? base.synthesis.tol_db,
			max_gain_db_per_iter: s.max_gain_db_per_iter ?? base.synthesis.max_gain_db_per_iter
		},
		envelope: {
			per_band: Array.isArray(env.per_band),
			alarm_db: env.alarm_db ? pair(env.alarm_db) : base.envelope.alarm_db,
			abort_db: env.abort_db ? pair(env.abort_db) : base.envelope.abort_db,
			bands: (env.per_band ?? []).map((b) => ({ alarm_db: pair(b.alarm_db), abort_db: pair(b.abort_db) }))
		},
		acquisition: { ...base.acquisition, ...(c.acquisition ?? {}) },
		schedule: {
			mode: c.schedule.mode,
			levels,
			start_db: c.schedule.start_db ?? levels[0]?.db ?? base.schedule.start_db,
			advance: c.schedule.advance ?? 'count',
			max_pulses_per_level: c.schedule.max_pulses_per_level ?? base.schedule.max_pulses_per_level
		},
		spacing: { ...base.spacing, ...(c.spacing ?? {}) },
		identification: { ...base.identification, ...(c.identification ?? {}) },
		law: { id: law?.id ?? DEFAULT_SRS_LAW, params: { ...((law?.params ?? {}) as LawDraft['params']) } },
		control: { ...base.control, ...control },
		limits: {
			drive_vpk_ceiling: c.limits.drive_vpk_ceiling,
			drive_vpk_abort: c.limits.drive_vpk_abort,
			peak_accel_g: c.limits.peak_accel_g ?? null,
			abort_factor: c.limits.abort_factor ?? base.limits.abort_factor,
			kinematic: { ...base.limits.kinematic, ...(c.limits.kinematic ?? {}) },
			monitors: (c.limits.monitors ?? []).map((m) => ({ ...m }))
		},
		limit_channels: lc
			? {
					enabled: true,
					headroom_db: lc.headroom_db,
					channels: lc.channels.map((ch) => ({
						channel: ch.channel,
						bound: ch.bound.kind === 'flat' ? { kind: 'flat', accel_g: ch.bound.accel_g, bounds_g: [] } : { kind: 'per_band', accel_g: NaN, bounds_g: [...ch.bound.bounds_g] },
						frf: ch.frf ? { supplied: true, points: ch.frf.freqs_hz.map((freq_hz, i) => ({ freq_hz, mag: ch.frf!.mag[i] ?? NaN })) } : { supplied: false, points: [] }
					}))
				}
			: { ...base.limit_channels },
		feasibility: c.feasibility
			? {
					enabled: true,
					shaker_text: JSON.stringify(c.feasibility.shaker, null, 2),
					moving_mass_lb: { ...c.feasibility.moving_mass_lb },
					residual_velocity_ips: c.feasibility.residual_velocity_ips ?? base.feasibility.residual_velocity_ips,
					residual_disp_in: c.feasibility.residual_disp_in ?? base.feasibility.residual_disp_in
				}
			: base.feasibility,
		record: {
			operator: c.operator ?? '',
			article: c.article ?? '',
			notes: c.notes ?? '',
			document: c.specification?.document ?? '',
			clause: c.specification?.clause ?? '',
			streams: c.record?.streams ?? true
		},
		metadata: (c.metadata ?? []).map((m) => ({ ...m }))
	};
}

/**
 * What `srs.preview` is asked for the draft's target, or null while it holds no numbers. The
 * envelope it draws is the pair for every band: a per-band envelope is fitted to the grid the
 * preview itself gives, so it is drawn from the dry run of the whole setup instead.
 */
export function previewParams(d: SrsDraft): SrsPreviewParams | null {
	const nums = [d.bands.f_lo_hz, d.bands.f_hi_hz, d.bands.bands_per_octave, d.bands.q, d.acquisition.fs_hz, d.synthesis.max_iters, d.synthesis.tol_db, d.synthesis.max_gain_db_per_iter];
	if (d.target.kind === 'flat') nums.push(d.target.accel_g);
	else {
		if (d.target.points.length < 2) return null;
		for (const p of d.target.points) nums.push(p.freq_hz, p.accel_g);
	}
	if (d.synthesis.table) {
		const rows: Record<string, unknown>[] = d.synthesis.engine === 'wavsyn' ? (d.synthesis.wavelets as unknown as Record<string, unknown>[]) : (d.synthesis.sines as unknown as Record<string, unknown>[]);
		if (!rows.length) return null;
		for (const r of rows) for (const v of Object.values(r)) if (typeof v === 'number') nums.push(v);
	}
	const env = d.envelope;
	nums.push(env.alarm_db[0], env.alarm_db[1], env.abort_db[0], env.abort_db[1]);
	if (!nums.every(Number.isFinite)) return null;
	return { target: targetOf(d), bands: bandsOf(d), synthesis: synthesisOf(d), envelope: { alarm_db: pair(env.alarm_db), abort_db: pair(env.abort_db), per_band: null }, fs_hz: d.acquisition.fs_hz };
}

// ---------------------------------------------------------------------------------------------
// Where a finding belongs

/** The setup's sections, in the screen's order. */
export type SrsSection = 'station' | 'target' | 'synthesis' | 'envelope' | 'schedule' | 'identification' | 'control' | 'limits' | 'limit_channels' | 'record';

export const SRS_SECTIONS: { id: SrsSection; label: string }[] = [
	{ id: 'station', label: 'Station & Channels' },
	{ id: 'target', label: 'Target' },
	{ id: 'synthesis', label: 'Synthesis' },
	{ id: 'envelope', label: 'Envelope' },
	{ id: 'schedule', label: 'Schedule' },
	{ id: 'identification', label: 'Identification' },
	{ id: 'control', label: 'Control' },
	{ id: 'limits', label: 'Limits' },
	{ id: 'limit_channels', label: 'Limit Channels' },
	{ id: 'record', label: 'Run Record' }
];

/**
 * The editor's key for a service finding. A field path loses its `config.` head and its brackets.
 * The tables' findings go under the table (`target.points`, `synthesis.components`,
 * `envelope.per_band`), their message naming the row; the level table's lists name the level
 * (`schedule.levels_db[2]` is `schedule.levels.2.db`); a limit channel's findings go on its
 * channel, its bound or its gain; the law's path is the law panel's (`control.law.params.x` is
 * `law.params.x`). A finding about the channel table's control or drive row is the station's.
 */
export function srsItemKey(item: Pick<ReportItem, 'field' | 'channel'> & { reason: string }): string {
	if (item.reason === 'no_control_channel' || item.reason === 'no_drive_channel') return 'station.channels';
	if (item.field) {
		const path = item.field.replace(/^config\./, '').replace(/\[(\d+)\]/g, '.$1');
		let m = /^schedule\.levels_db\.(\d+)$/.exec(path);
		if (m) return `schedule.levels.${m[1]}.db`;
		m = /^schedule\.pulses\.(\d+)$/.exec(path);
		if (m) return `schedule.levels.${m[1]}.pulses`;
		if (path === 'schedule.levels_db') return 'schedule.levels';
		if (/^target\.points(\.|$)/.test(path)) return 'target.points';
		if (/^synthesis\.components(\.|$)/.test(path)) return 'synthesis.components';
		if (/^envelope\.per_band(\.|$)/.test(path)) return 'envelope.per_band';
		m = /^limit_channels\.channels\.(\d+)(?:\.(channel|bound|frf))?/.exec(path);
		if (m) return `limit_channels.channels.${m[1]}.${m[2] ?? (item.reason === 'limit_no_frf' ? 'frf' : 'channel')}`;
		if (path.startsWith('control.law')) return path.replace(/^control\.law(\.id)?/, (_, id) => (id ? 'law.id' : 'law')).replace(/^law$/, 'law.id');
		return path;
	}
	if (item.reason === 'limit_unknown_channel') return 'limits.monitors';
	if (item.channel !== undefined && item.channel !== null) return 'station.channels';
	return 'setup';
}

export function srsByKey(items: { field?: string | null; channel?: number | null; reason: string; message: string }[]): FieldErrors {
	const out: FieldErrors = {};
	for (const it of items) {
		const k = srsItemKey(it as Pick<ReportItem, 'field' | 'channel'> & { reason: string });
		if (!(out[k] ??= []).includes(it.message)) out[k].push(it.message);
	}
	return out;
}

/** The section a key belongs to. */
export function srsSectionOf(key: string): SrsSection {
	if (/^station|^table|^channel/.test(key)) return 'station';
	if (/^(title|target|bands)(\.|$)/.test(key)) return 'target';
	if (/^(synthesis|acquisition)(\.|$)/.test(key)) return 'synthesis';
	if (/^envelope(\.|$)/.test(key)) return 'envelope';
	if (/^(schedule|spacing)(\.|$)/.test(key)) return 'schedule';
	if (/^identification(\.|$)/.test(key)) return 'identification';
	if (/^(law|control)(\.|$)/.test(key)) return 'control';
	if (/^(limits|feasibility)(\.|$)/.test(key)) return 'limits';
	if (/^limit_channels(\.|$)/.test(key)) return 'limit_channels';
	if (/^(metadata|operator|article|notes|specification|record)(\.|$)/.test(key)) return 'record';
	return 'target';
}

// ---------------------------------------------------------------------------------------------
// What the editor can say before anything is sent

const NUMBER = 'Enter a number.';
const WHOLE = 'Enter a whole number.';

function add(map: FieldErrors, key: string, message: string) {
	(map[key] ??= []).push(message);
}

function need(map: FieldErrors, key: string, v: number, whole = false) {
	if (!Number.isFinite(v)) add(map, key, whole ? WHOLE : NUMBER);
	else if (whole && !Number.isInteger(v)) add(map, key, WHOLE);
}

function needOptional(map: FieldErrors, key: string, v: number | null) {
	if (v !== null && !Number.isFinite(v)) add(map, key, NUMBER);
}

function needPair(map: FieldErrors, key: string, v: readonly number[]) {
	if (!(Number.isFinite(v[0]) && Number.isFinite(v[1]))) add(map, key, 'Give both: under and over the target.');
}

/**
 * Fields that hold no number, a title that is empty, a table with too few rows, a drive ceiling or
 * abort not given (or the abort not above the ceiling), a limit channel or monitor on a row the
 * table does not enable, a shaker record that does not read. Everything else, the synthesis and
 * the band grid included, is the service's to judge in the dry run.
 */
export function localErrors(d: SrsDraft, rows: ChannelRow[] = []): FieldErrors {
	const out: FieldErrors = {};
	if (!d.title.trim()) add(out, 'title', 'Give the test a title.');
	if (d.target.kind === 'flat') need(out, 'target.accel_g', d.target.accel_g);
	else {
		if (d.target.points.length < 2) add(out, 'target.points', 'Give at least two points of the target.');
		d.target.points.forEach((p, i) => {
			need(out, `target.points.${i}.freq_hz`, p.freq_hz);
			need(out, `target.points.${i}.accel_g`, p.accel_g);
		});
	}
	for (const k of ['f_lo_hz', 'f_hi_hz', 'q'] as const) need(out, `bands.${k}`, d.bands[k]);
	need(out, 'bands.bands_per_octave', d.bands.bands_per_octave, true);
	const s = d.synthesis;
	need(out, 'synthesis.max_iters', s.max_iters, true);
	need(out, 'synthesis.tol_db', s.tol_db);
	need(out, 'synthesis.max_gain_db_per_iter', s.max_gain_db_per_iter);
	if (s.table) {
		const list = s.engine === 'wavsyn' ? s.wavelets : s.sines;
		if (!list.length) add(out, 'synthesis.components', 'Give at least one component, import a table, or start from one per band.');
		if (s.engine === 'wavsyn')
			s.wavelets.forEach((r, i) => {
				for (const k of ['freq_hz', 'amplitude_g', 'delay_s'] as const) need(out, `synthesis.components.${i}.${k}`, r[k]);
				need(out, `synthesis.components.${i}.half_cycles`, r.half_cycles, true);
			});
		else
			s.sines.forEach((r, i) => {
				for (const k of ['freq_hz', 'amplitude_g', 'zeta', 'delay_s'] as const) need(out, `synthesis.components.${i}.${k}`, r[k]);
			});
	}
	needPair(out, 'envelope.alarm_db', d.envelope.alarm_db);
	needPair(out, 'envelope.abort_db', d.envelope.abort_db);
	if (d.envelope.per_band) {
		if (!d.envelope.bands.length) add(out, 'envelope.per_band', 'The per-band envelope is filled once the control service has given the band grid.');
		d.envelope.bands.forEach((b, i) => {
			needPair(out, `envelope.per_band.${i}.alarm_db`, b.alarm_db);
			needPair(out, `envelope.per_band.${i}.abort_db`, b.abort_db);
		});
	}
	need(out, 'acquisition.fs_hz', d.acquisition.fs_hz);
	need(out, 'acquisition.ring_out_s', d.acquisition.ring_out_s);
	const sch = d.schedule;
	if (!sch.levels.length) add(out, 'schedule.levels', 'Give at least one level.');
	sch.levels.forEach((l, i) => {
		need(out, `schedule.levels.${i}.db`, l.db);
		need(out, `schedule.levels.${i}.pulses`, l.pulses, true);
	});
	if (sch.mode === 'manual') need(out, 'schedule.start_db', sch.start_db);
	if (sch.advance === 'on_pass') need(out, 'schedule.max_pulses_per_level', sch.max_pulses_per_level, true);
	for (const k of ['cooling_s', 'settling_s', 'compute_s'] as const) need(out, `spacing.${k}`, d.spacing[k]);
	const id = d.identification;
	for (const k of ['level_db', 'h_floor_frac', 'min_mean_coherence'] as const) need(out, `identification.${k}`, id[k]);
	need(out, 'identification.nfft', id.nfft, true);
	needOptional(out, 'identification.max_vpk', id.max_vpk);
	const c = d.control;
	need(out, 'control.law_budget', c.law_budget);
	need(out, 'control.max_gain_rise_db', c.max_gain_rise_db);
	for (const k of ['law_misses', 'max_pulses'] as const) need(out, `control.${k}`, c[k], true);
	const lim = d.limits;
	if (!Number.isFinite(lim.drive_vpk_ceiling)) add(out, 'limits.drive_vpk_ceiling', 'Give the drive ceiling in volts peak. There is no default: it is your amplifier input.');
	if (!Number.isFinite(lim.drive_vpk_abort)) add(out, 'limits.drive_vpk_abort', 'Give the drive abort in volts peak, above the ceiling.');
	else if (Number.isFinite(lim.drive_vpk_ceiling) && lim.drive_vpk_abort <= lim.drive_vpk_ceiling) add(out, 'limits.drive_vpk_abort', 'The drive abort is above the drive ceiling.');
	needOptional(out, 'limits.peak_accel_g', lim.peak_accel_g);
	need(out, 'limits.abort_factor', lim.abort_factor);
	for (const k of ['accel_g', 'velocity_ips', 'disp_pp_in'] as const) needOptional(out, `limits.kinematic.${k}`, lim.kinematic[k]);
	const enabledInput = (index: number) => !rows.length || rows.some((r) => r.index === index && r.enabled && r.role !== 'drive');
	const control = runChannels(rows).control?.index ?? null;
	const anyRow = (index: number) => !rows.length || rows.some((r) => r.index === index);
	lim.monitors.forEach((m, i) => {
		need(out, `limits.monitors.${i}.peak_ceiling`, m.peak_ceiling);
		if (!enabledInput(m.channel)) add(out, `limits.monitors.${i}.channel`, `Channel ${m.channel} is not an enabled input of the channel table.`);
	});
	const lc = d.limit_channels;
	if (lc.enabled) {
		need(out, 'limit_channels.headroom_db', lc.headroom_db);
		if (!lc.channels.length) add(out, 'limit_channels.channels', 'Add a limit channel, or turn limit channels off.');
		lc.channels.forEach((ch, i) => {
			// any row but the control row; one whose gain is measured, an enabled input
			if (ch.channel === control) add(out, `limit_channels.channels.${i}.channel`, `Channel ${ch.channel} is the control channel; a limit channel is another input.`);
			else if (ch.frf.supplied ? !anyRow(ch.channel) : !enabledInput(ch.channel))
				add(out, `limit_channels.channels.${i}.channel`, ch.frf.supplied ? `Channel ${ch.channel} is not a row of the channel table.` : `Channel ${ch.channel} is not an enabled input of the channel table; enable it, or supply its gain.`);
			if (ch.bound.kind === 'flat') need(out, `limit_channels.channels.${i}.bound`, ch.bound.accel_g);
			else if (!ch.bound.bounds_g.length) add(out, `limit_channels.channels.${i}.bound`, 'The per-band bound is filled once the control service has given the band grid.');
			else if (ch.bound.bounds_g.some((b) => b !== null && !Number.isFinite(b))) add(out, `limit_channels.channels.${i}.bound`, 'Enter a number for each band, or no bound.');
			if (ch.frf.supplied) {
				if (ch.frf.points.length < 2) add(out, `limit_channels.channels.${i}.frf`, 'Give at least two points of the gain, or let identification measure it.');
				else if (ch.frf.points.some((p) => !Number.isFinite(p.freq_hz) || !Number.isFinite(p.mag))) add(out, `limit_channels.channels.${i}.frf`, 'Enter a frequency and a gain in every row.');
			}
		});
	}
	const f = d.feasibility;
	if (f.enabled) {
		if (!f.shaker_text.trim()) add(out, 'feasibility.shaker', 'Load or paste the shaker record, or turn the check off.');
		else if (!shakerRecord(f.shaker_text)) add(out, 'feasibility.shaker', 'The shaker record does not read as a record (JSON object).');
		for (const [k, v] of Object.entries(f.moving_mass_lb)) need(out, `feasibility.moving_mass_lb.${k}`, v);
		need(out, 'feasibility.residual_velocity_ips', f.residual_velocity_ips);
		need(out, 'feasibility.residual_disp_in', f.residual_disp_in);
	}
	if (d.record.clause.trim() && !d.record.document.trim()) add(out, 'specification.document', 'Name the document this clause is from.');
	return out;
}

/**
 * The per-band lists a draft holds, fitted to the band grid the service derived: a per-band
 * envelope or bound kept where it has a value for a band, the uniform envelope (or no bound)
 * where it has none. Returns the draft's lists unchanged when they already fit.
 */
export function fitToBands<T>(list: readonly T[], bands: number, fill: () => T): T[] {
	if (list.length === bands) return list as T[];
	return Array.from({ length: bands }, (_, i) => (i < list.length ? list[i] : fill()));
}
