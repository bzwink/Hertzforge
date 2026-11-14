/**
 * The time waveform replication (TWR) setup as the operator edits it, and its conversion to and
 * from the protocol's `TwrConfig` (schema/PROTOCOL.md, "TWR Configuration"). Pure functions only:
 * the service prepares the reference, lays out the record, computes its kinematics and every other
 * number, and this module only carries the operator's choices.
 *
 * Numbers are held as numbers; a field that holds no number is NaN, which `localErrors` names
 * before anything is sent. An optional limit or step that is off is null. The drive ceiling has no
 * default anywhere: the setup asks for it. The reference is chosen by the SHA-256 the service gave
 * it on import; the import's facts are kept beside the choice to show it, and never sent.
 */

import type { ChannelRow, LadderMode, ReportItem } from '$lib/protocol/types';
import type { LawEntry } from '$lib/protocol/mimo';
import type {
	TwrConfig,
	TwrDetrend,
	TwrExcitation,
	TwrGateFailPolicy,
	TwrReferenceFacts,
	TwrReferencePreviewParams,
	TwrRun
} from '$lib/protocol/twr';
import { lawWireParams, type LawDraft } from '../law/lawForm';
import { defaultRecord, type FieldErrors, type RecordDraft } from '../setup';

// ---------------------------------------------------------------------------------------------
// The draft

export interface TwrDraft {
	title: string;
	/** The stored reference by its SHA-256 (empty until one is imported), and the file it came from. */
	reference: { sha256: string; path: string | null };
	/** What the import said of the chosen reference, to show it. Not sent. */
	facts: TwrReferenceFacts | null;
	prep: {
		auto_resample: boolean;
		detrend: TwrDetrend;
		/** Hz; null for none. */
		highpass_hz: number | null;
		scale: number;
		/** A fraction of the reference at each end; null for none. */
		taper_frac: number | null;
	};
	/** The samples of the prepared reference to replicate; the whole of it when `whole`. */
	section: { whole: boolean; start: number; len: number };
	pads: { pre: number; post: number };
	/** Null: the reference's own rate. */
	acquisition: { fs_hz: number | null };
	identification: {
		excitation: TwrExcitation;
		drive_vrms: number;
		duration_s: number;
		nfft: number;
		band_hz: [number, number];
		min_mean_coherence: number;
		review: boolean;
	};
	inverse: { tikhonov_floor: number; coherence_floor: number };
	correction: { band_hz: [number, number]; ref_floor_db: number; min_band_energy: number };
	law: LawDraft;
	control: {
		law_budget: number;
		law_misses: number;
		max_peak_rise_db: number;
		max_bin_rise_db: number;
		first_drive_ratio: number;
		alignment_frames: number;
	};
	schedule: {
		mode: LadderMode;
		levels_db: number[];
		gate_pct: number;
		max_iterations: number;
		on_gate_fail: TwrGateFailPolicy;
	};
	/** The playback once full level has gated; `count` and `duration_s` kept for the other choices. */
	run: { schedule: TwrRun['schedule']; count: number; duration_s: number };
	adaptation: { during_playback: boolean };
	limits: {
		/** Volts peak; NaN until the operator gives it. */
		drive_vpk_ceiling: number;
		drive_clip_vpk: number | null;
		drive_vrms_limit: number | null;
		response_rms_limit: number | null;
		envelope_alarm_frac: number;
		envelope_abort_frac: number;
		envelope_debounce_samples: number;
		divergence_plays: number;
		divergence_rise_frac: number;
	};
	feasibility: {
		peak_accel: number | null;
		peak_velocity_ips: number | null;
		disp_pkpk_in: number | null;
		kinematics_hp_hz: number;
	};
	/** The run's record: the operator's words and whether the folder keeps the time histories (Random's). */
	record: RecordDraft;
	metadata: { key: string; label: string; value: string }[];
}

export const DEFAULT_TWR_LAW = 'twr_correct';

export const DETRENDS: { id: TwrDetrend; label: string }[] = [
	{ id: 'none', label: 'None' },
	{ id: 'mean', label: 'Remove the mean' },
	{ id: 'linear', label: 'Remove a straight line' }
];

export const EXCITATIONS: { id: TwrExcitation; label: string }[] = [
	{ id: 'random', label: 'Band-limited random' },
	{ id: 'log_chirp', label: 'Log chirp' }
];

export const RUN_SCHEDULES: { id: TwrRun['schedule']; label: string }[] = [
	{ id: 'once', label: 'Once' },
	{ id: 'loop', label: 'A number of times' },
	{ id: 'duration', label: 'For a time' }
];

export const NFFT_OPTIONS = [256, 512, 1024, 2048, 4096, 8192, 16384];

/** The service's limits on the record (PROTOCOL.md "TWR Configuration"), for the editor's words. */
export const MAX_RECORD_LEN = 1048576;

/** The shell's defaults: the wire's, with no reference and no drive ceiling. */
export function defaultTwrSetup(): TwrDraft {
	return {
		title: 'Field Transient',
		reference: { sha256: '', path: null },
		facts: null,
		prep: { auto_resample: true, detrend: 'mean', highpass_hz: 5, scale: 1, taper_frac: 0.02 },
		section: { whole: true, start: 0, len: NaN },
		pads: { pre: 512, post: 1024 },
		acquisition: { fs_hz: null },
		identification: { excitation: 'random', drive_vrms: 0.05, duration_s: 4, nfft: 2048, band_hz: [20, 2000], min_mean_coherence: 0.8, review: true },
		inverse: { tikhonov_floor: 0.05, coherence_floor: 0.9 },
		correction: { band_hz: [20, 2000], ref_floor_db: 50, min_band_energy: 0.9 },
		law: { id: DEFAULT_TWR_LAW, params: {} },
		control: { law_budget: 0.25, law_misses: 3, max_peak_rise_db: 10, max_bin_rise_db: 10, first_drive_ratio: 2, alignment_frames: 1 },
		schedule: { mode: 'auto', levels_db: [-12, -9, -6, -3, 0], gate_pct: 10, max_iterations: 8, on_gate_fail: 'abort' },
		run: { schedule: 'once', count: 10, duration_s: 60 },
		adaptation: { during_playback: false },
		limits: {
			drive_vpk_ceiling: NaN,
			drive_clip_vpk: null,
			drive_vrms_limit: null,
			response_rms_limit: null,
			envelope_alarm_frac: 0.25,
			envelope_abort_frac: 0.5,
			envelope_debounce_samples: 8,
			divergence_plays: 3,
			divergence_rise_frac: 0.1
		},
		feasibility: { peak_accel: null, peak_velocity_ips: null, disp_pkpk_in: null, kinematics_hp_hz: 5 },
		record: defaultRecord(),
		metadata: [{ key: 'run_number', label: 'Run Number', value: '' }]
	};
}

const pair = (v: unknown, base: [number, number]): [number, number] => (Array.isArray(v) && v.length === 2 ? [Number(v[0]), Number(v[1])] : [base[0], base[1]]);

/** A draft read back from storage, every section filled from the defaults where it is missing. */
export function mergeTwrSetup(saved: Partial<TwrDraft> | null): TwrDraft {
	const base = defaultTwrSetup();
	if (!saved || typeof saved !== 'object') return base;
	const out = { ...base, ...saved } as TwrDraft;
	out.reference = { ...base.reference, ...(saved.reference ?? {}) };
	out.facts = saved.facts && typeof saved.facts === 'object' && typeof saved.facts.sha256 === 'string' ? saved.facts : null;
	out.prep = { ...base.prep, ...(saved.prep ?? {}) };
	out.section = { ...base.section, ...(saved.section ?? {}) };
	out.pads = { ...base.pads, ...(saved.pads ?? {}) };
	out.acquisition = { ...base.acquisition, ...(saved.acquisition ?? {}) };
	out.identification = { ...base.identification, ...(saved.identification ?? {}) };
	out.identification.band_hz = pair(out.identification.band_hz, base.identification.band_hz);
	out.inverse = { ...base.inverse, ...(saved.inverse ?? {}) };
	out.correction = { ...base.correction, ...(saved.correction ?? {}) };
	out.correction.band_hz = pair(out.correction.band_hz, base.correction.band_hz);
	out.law = { ...base.law, ...(saved.law ?? {}) };
	out.control = { ...base.control, ...(saved.control ?? {}) };
	out.schedule = { ...base.schedule, ...(saved.schedule ?? {}) };
	if (!Array.isArray(out.schedule.levels_db) || !out.schedule.levels_db.length) out.schedule.levels_db = base.schedule.levels_db;
	out.run = { ...base.run, ...(saved.run ?? {}) };
	out.adaptation = { ...base.adaptation, ...(saved.adaptation ?? {}) };
	out.limits = { ...base.limits, ...(saved.limits ?? {}) };
	out.feasibility = { ...base.feasibility, ...(saved.feasibility ?? {}) };
	// A draft saved before the run's record took the capture's place has a capture and no record.
	delete (out as Partial<TwrDraft> & { capture?: unknown }).capture;
	out.record = { ...base.record, ...(saved.record ?? {}) };
	if (!Array.isArray(out.metadata)) out.metadata = base.metadata;
	return out;
}

// ---------------------------------------------------------------------------------------------
// To and from the protocol

function runOf(d: TwrDraft): TwrRun {
	switch (d.run.schedule) {
		case 'loop':
			return { schedule: 'loop', count: d.run.count };
		case 'duration':
			return { schedule: 'duration', duration_s: d.run.duration_s };
		default:
			return { schedule: 'once' };
	}
}

/** The configuration the draft describes, every field explicit. Call `localErrors` first. */
export function toTwrConfig(d: TwrDraft, law: LawEntry | null): TwrConfig {
	return {
		title: d.title.trim(),
		reference: { sha256: d.reference.sha256, path: d.reference.path },
		prep: { ...d.prep },
		section: d.section.whole ? null : { start: d.section.start, len: d.section.len },
		pads: { ...d.pads },
		acquisition: { fs_hz: d.acquisition.fs_hz },
		identification: { ...d.identification, band_hz: [d.identification.band_hz[0], d.identification.band_hz[1]] },
		inverse: { ...d.inverse },
		correction: { ...d.correction, band_hz: [d.correction.band_hz[0], d.correction.band_hz[1]] },
		control: { law: { id: d.law.id, params: lawWireParams(law, d.law.params) }, ...d.control },
		schedule: { ...d.schedule, levels_db: [...d.schedule.levels_db] },
		run: runOf(d),
		adaptation: { ...d.adaptation },
		limits: { ...d.limits },
		feasibility: { ...d.feasibility },
		operator: d.record.operator.trim(),
		article: d.record.article.trim(),
		notes: d.record.notes.trim(),
		specification: d.record.document.trim() || d.record.clause.trim() ? { document: d.record.document.trim(), clause: d.record.clause.trim() } : null,
		record: { streams: d.record.streams },
		metadata: d.metadata.map((m) => ({ ...m }))
	};
}

/**
 * The draft for a configuration the service holds (another screen applied it, or this one before
 * a reload). Fields the configuration leaves out take the wire's defaults; a correction band left
 * out is the identification's band, as the service takes it.
 */
export function fromTwrConfig(c: TwrConfig, facts: TwrReferenceFacts | null = null): TwrDraft {
	const base = defaultTwrSetup();
	const id = { ...base.identification, ...(c.identification ?? {}) };
	id.band_hz = pair(c.identification?.band_hz, base.identification.band_hz);
	const run = c.run ?? { schedule: 'once' as const };
	const { law: _law, ...control } = c.control ?? {};
	void _law;
	return {
		title: c.title ?? '',
		reference: { sha256: c.reference.sha256, path: c.reference.path ?? null },
		facts: facts && facts.sha256 === c.reference.sha256 ? facts : null,
		prep: { ...base.prep, ...(c.prep ?? {}) },
		section: c.section ? { whole: false, start: c.section.start, len: c.section.len } : { ...base.section },
		pads: { ...base.pads, ...(c.pads ?? {}) },
		acquisition: { fs_hz: c.acquisition?.fs_hz ?? null },
		identification: id,
		inverse: { ...base.inverse, ...(c.inverse ?? {}) },
		correction: {
			...base.correction,
			...(c.correction ?? {}),
			band_hz: pair(c.correction?.band_hz, id.band_hz)
		},
		law: { id: c.control?.law?.id ?? DEFAULT_TWR_LAW, params: { ...((c.control?.law?.params ?? {}) as LawDraft['params']) } },
		control: { ...base.control, ...control },
		schedule: { ...base.schedule, ...(c.schedule ?? {}), levels_db: [...(c.schedule?.levels_db ?? base.schedule.levels_db)] },
		run: {
			schedule: run.schedule,
			count: run.schedule === 'loop' ? run.count : base.run.count,
			duration_s: run.schedule === 'duration' ? run.duration_s : base.run.duration_s
		},
		adaptation: { ...base.adaptation, ...(c.adaptation ?? {}) },
		limits: { ...base.limits, ...c.limits },
		feasibility: { ...base.feasibility, ...(c.feasibility ?? {}) },
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

const finite = (...v: (number | null)[]) => v.every((x) => x === null || Number.isFinite(x));

/** What `twr.reference.preview` is asked for the draft's reference, or null while it cannot be asked. */
export function previewParams(d: TwrDraft): TwrReferencePreviewParams | null {
	if (!d.reference.sha256) return null;
	const p = d.prep;
	if (!finite(p.highpass_hz, p.scale, p.taper_frac, d.pads.pre, d.pads.post, d.acquisition.fs_hz, d.correction.band_hz[0], d.correction.band_hz[1], d.correction.ref_floor_db, d.feasibility.kinematics_hp_hz)) return null;
	if (!finite(d.feasibility.peak_accel, d.feasibility.peak_velocity_ips, d.feasibility.disp_pkpk_in)) return null;
	if (!d.section.whole && !finite(d.section.start, d.section.len)) return null;
	return {
		reference: { sha256: d.reference.sha256 },
		prep: { ...p },
		section: d.section.whole ? null : { start: d.section.start, len: d.section.len },
		pads: { ...d.pads },
		acquisition: { fs_hz: d.acquisition.fs_hz },
		correction: { ...d.correction, band_hz: [d.correction.band_hz[0], d.correction.band_hz[1]] },
		feasibility: { ...d.feasibility }
	};
}

// ---------------------------------------------------------------------------------------------
// Where a finding belongs

/** The setup's sections, in the screen's order. */
export type TwrSetupSection = 'station' | 'reference' | 'identification' | 'control' | 'schedule' | 'limits' | 'record';

export const TWR_SECTIONS: { id: TwrSetupSection; label: string }[] = [
	{ id: 'station', label: 'Station & Channels' },
	{ id: 'reference', label: 'Reference' },
	{ id: 'identification', label: 'Identification' },
	{ id: 'control', label: 'Control' },
	{ id: 'schedule', label: 'Schedule' },
	{ id: 'limits', label: 'Limits' },
	{ id: 'record', label: 'Run Record' }
];

/** Findings the service names by reason alone, by the editor's key. */
const BY_REASON: Record<string, string> = {
	no_control_channel: 'station.channels',
	no_drive_channel: 'station.channels',
	reference_unknown: 'reference',
	reference_changed: 'reference',
	units_mismatch: 'reference',
	rate_mismatch: 'acquisition.fs_hz',
	section_outside_reference: 'section',
	record_too_long: 'pads',
	levels_empty: 'schedule.levels_db',
	levels_not_ascending: 'schedule.levels_db',
	alarm_wider_than_abort: 'limits.envelope_alarm_frac',
	identification_too_short: 'identification.duration_s',
	band_outside_identification: 'correction.band_hz',
	highpass_above_band: 'prep.highpass_hz',
	law_unknown: 'law.id',
	law_wrong_world: 'law.id'
};

/**
 * The editor's key for a service finding. A field path loses its `config.` head and its brackets
 * (`schedule.levels_db[2]` is `schedule.levels_db.2`); the reference's two fields are the reference
 * the setup chose (`reference`); a band's edges are its band; the law's path is the law panel's
 * (`control.law.params.beta` is `law.params.beta`). A finding about the channel table's control or
 * drive row is the station's.
 */
export function twrItemKey(item: Pick<ReportItem, 'field' | 'channel'> & { reason: string }): string {
	if (item.reason === 'no_control_channel' || item.reason === 'no_drive_channel') return 'station.channels';
	if (item.field) {
		const path = item.field.replace(/^config\./, '').replace(/\[(\d+)\]/g, '.$1');
		if (/^reference(\.|$)/.test(path)) return 'reference';
		if (/^section(\.|$)/.test(path) && !/^section\.(start|len)$/.test(path)) return 'section';
		const band = /^(identification|correction)\.band_hz(\.\d+)?$/.exec(path);
		if (band) return `${band[1]}.band_hz`;
		if (path.startsWith('control.law')) return path.replace(/^control\.law(\.id)?/, (_, id) => (id ? 'law.id' : 'law')).replace(/^law$/, 'law.id');
		return path;
	}
	if (BY_REASON[item.reason]) return BY_REASON[item.reason];
	if (item.channel !== undefined && item.channel !== null) return 'station.channels';
	return 'title';
}

export function twrByKey(items: { field?: string | null; channel?: number | null; reason: string; message: string }[]): FieldErrors {
	const out: FieldErrors = {};
	for (const it of items) {
		const k = twrItemKey(it as Pick<ReportItem, 'field' | 'channel'> & { reason: string });
		if (!(out[k] ??= []).includes(it.message)) out[k].push(it.message);
	}
	return out;
}

/** The section a key belongs to. */
export function twrSectionOf(key: string): TwrSetupSection {
	if (/^station|^table|^channel/.test(key)) return 'station';
	if (/^(title|reference|prep|section|pads|acquisition)(\.|$)/.test(key)) return 'reference';
	if (/^identification(\.|$)/.test(key)) return 'identification';
	if (/^(law|control|inverse|correction)(\.|$)/.test(key)) return 'control';
	if (/^(schedule|run|adaptation)(\.|$)/.test(key)) return 'schedule';
	if (/^(limits|feasibility)(\.|$)/.test(key)) return 'limits';
	if (/^(metadata|operator|article|notes|specification|record)(\.|$)/.test(key)) return 'record';
	return 'reference';
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

function needOptional(map: FieldErrors, key: string, v: number | null, whole = false) {
	if (v !== null) need(map, key, v, whole);
}

function needBand(map: FieldErrors, key: string, band: [number, number]) {
	if (!(Number.isFinite(band[0]) && Number.isFinite(band[1]))) add(map, key, 'Give both edges of the band.');
	else if (band[0] >= band[1]) add(map, key, 'The band runs from its low edge up to its high edge.');
}

/**
 * Fields that hold no number, a title that is empty, no reference imported, no drive ceiling, an
 * alarm band wider than the abort band. Everything else, the record's layout and the reference's
 * kinematics included, is the service's to judge in the dry run.
 */
export function localErrors(d: TwrDraft): FieldErrors {
	const out: FieldErrors = {};
	if (!d.title.trim()) add(out, 'title', 'Give the test a title.');
	if (!d.reference.sha256) add(out, 'reference', 'Import the reference to replicate: a file by its path, or pasted text.');
	const p = d.prep;
	needOptional(out, 'prep.highpass_hz', p.highpass_hz);
	need(out, 'prep.scale', p.scale);
	if (Number.isFinite(p.scale) && p.scale === 0) add(out, 'prep.scale', 'The scale multiplies the reference; it is not 0.');
	needOptional(out, 'prep.taper_frac', p.taper_frac);
	if (!d.section.whole) {
		need(out, 'section.start', d.section.start, true);
		need(out, 'section.len', d.section.len, true);
	}
	need(out, 'pads.pre', d.pads.pre, true);
	need(out, 'pads.post', d.pads.post, true);
	needOptional(out, 'acquisition.fs_hz', d.acquisition.fs_hz);
	const id = d.identification;
	for (const k of ['drive_vrms', 'duration_s', 'min_mean_coherence'] as const) need(out, `identification.${k}`, id[k]);
	need(out, 'identification.nfft', id.nfft, true);
	needBand(out, 'identification.band_hz', id.band_hz);
	need(out, 'inverse.tikhonov_floor', d.inverse.tikhonov_floor);
	need(out, 'inverse.coherence_floor', d.inverse.coherence_floor);
	needBand(out, 'correction.band_hz', d.correction.band_hz);
	need(out, 'correction.ref_floor_db', d.correction.ref_floor_db);
	need(out, 'correction.min_band_energy', d.correction.min_band_energy);
	const c = d.control;
	for (const k of ['law_budget', 'max_peak_rise_db', 'max_bin_rise_db', 'first_drive_ratio', 'alignment_frames'] as const) need(out, `control.${k}`, c[k]);
	need(out, 'control.law_misses', c.law_misses, true);
	const s = d.schedule;
	if (!s.levels_db.length) add(out, 'schedule.levels_db', 'Give at least one level; the last is full level.');
	s.levels_db.forEach((l, i) => need(out, `schedule.levels_db.${i}`, l));
	need(out, 'schedule.gate_pct', s.gate_pct);
	need(out, 'schedule.max_iterations', s.max_iterations, true);
	if (d.run.schedule === 'loop') need(out, 'run.count', d.run.count, true);
	if (d.run.schedule === 'duration') need(out, 'run.duration_s', d.run.duration_s);
	const lim = d.limits;
	if (!Number.isFinite(lim.drive_vpk_ceiling)) add(out, 'limits.drive_vpk_ceiling', 'Give the drive ceiling in volts peak. There is no default: it is your amplifier input.');
	needOptional(out, 'limits.drive_clip_vpk', lim.drive_clip_vpk);
	needOptional(out, 'limits.drive_vrms_limit', lim.drive_vrms_limit);
	needOptional(out, 'limits.response_rms_limit', lim.response_rms_limit);
	need(out, 'limits.envelope_alarm_frac', lim.envelope_alarm_frac);
	need(out, 'limits.envelope_abort_frac', lim.envelope_abort_frac);
	if (Number.isFinite(lim.envelope_alarm_frac) && Number.isFinite(lim.envelope_abort_frac) && lim.envelope_alarm_frac > lim.envelope_abort_frac) add(out, 'limits.envelope_alarm_frac', 'The alarm band is at most the abort band.');
	need(out, 'limits.envelope_debounce_samples', lim.envelope_debounce_samples, true);
	need(out, 'limits.divergence_plays', lim.divergence_plays, true);
	need(out, 'limits.divergence_rise_frac', lim.divergence_rise_frac);
	const f = d.feasibility;
	needOptional(out, 'feasibility.peak_accel', f.peak_accel);
	needOptional(out, 'feasibility.peak_velocity_ips', f.peak_velocity_ips);
	needOptional(out, 'feasibility.disp_pkpk_in', f.disp_pkpk_in);
	need(out, 'feasibility.kinematics_hp_hz', f.kinematics_hp_hz);
	if (d.record.clause.trim() && !d.record.document.trim()) add(out, 'specification.document', 'Name the document this clause is from.');
	return out;
}

/** The control and drive rows the run uses: the table's first enabled control and drive rows. */
export function runChannels(rows: ChannelRow[]): { control: ChannelRow | null; drive: ChannelRow | null } {
	return {
		control: rows.find((r) => r.enabled && r.role === 'control') ?? null,
		drive: rows.find((r) => r.enabled && r.role === 'drive') ?? null
	};
}

/** The unit the reference must be in: the control row's (g when the table has none yet). */
export function controlUnit(rows: ChannelRow[]): string {
	return runChannels(rows).control?.units ?? 'g';
}
