/**
 * The Explorer setup as the operator edits it, and its conversion to and from the protocol's
 * `ExplorerConfig` (schema/PROTOCOL.md, "Explorer Configuration"). Pure functions only: the service
 * computes every number about the run (the station's block and lookahead, the pipeline, the rise
 * and stop lengths, the fastest sweep, the display's line spacing, the survey's tolerance and its
 * first-point delays), and this module only carries the operator's choices.
 *
 * Numbers are held as numbers; a field that holds no number is NaN, which `localErrors` names
 * before anything is sent. The rate is null when the usual one (8192 Hz) is wanted. The drive
 * ceiling has no default anywhere: the setup asks for it. The steering steps are this screen's own
 * (how far one press moves the tone); they are not sent, since the service steers by what each
 * verb asks.
 */

import type { ChannelRow, ReportItem } from '$lib/protocol/types';
import type { ExplorerConfig } from '$lib/protocol/explorer';
import { defaultRecord, type FieldErrors, type RecordDraft } from '../setup';

// ---------------------------------------------------------------------------------------------
// The draft

/** How far one press moves the tone: frequency in steps an octave (96 is 1/96 octave), level in dB. */
export interface ExplorerSteps {
	fine_per_oct: number;
	coarse_per_oct: number;
	fine_db: number;
	coarse_db: number;
	/** The sweep rate the steering panel starts from, oct/min. */
	sweep_oct_per_min: number;
}

/** A press's frequency step in octaves. */
export const fineOct = (s: ExplorerSteps) => 1 / s.fine_per_oct;
export const coarseOct = (s: ExplorerSteps) => 1 / s.coarse_per_oct;

export interface ExplorerDraft {
	title: string;
	acquisition: { fs_hz: number | null };
	band_hz: [number, number];
	start_hz: number;
	/** Volts peak; NaN until the operator gives it. */
	max_drive_vpk: number;
	slew: { freq_oct_per_s: number; drive_db_per_s: number };
	display: { nfft: number };
	capture: { run_name: string };
	metadata: { key: string; label: string; value: string }[];
	/** The run's record: the operator's words and whether the time histories are kept (Random's). */
	record: RecordDraft;
	steps: ExplorerSteps;
}

/** The FFT sizes the service takes: powers of two from 1024 to 16384. */
export const NFFT_CHOICES = [1024, 2048, 4096, 8192, 16384];

/** One press: 1/96 and 1/6 octave, 0.5 and 3 dB, until the operator changes them. */
export const DEFAULT_STEPS: ExplorerSteps = { fine_per_oct: 96, coarse_per_oct: 6, fine_db: 0.5, coarse_db: 3, sweep_oct_per_min: 4 };

export function defaultExplorerSetup(): ExplorerDraft {
	return {
		title: 'Explorer survey',
		acquisition: { fs_hz: null },
		band_hz: [10, 2000],
		start_hz: 100,
		max_drive_vpk: NaN,
		slew: { freq_oct_per_s: 1, drive_db_per_s: 6 },
		display: { nfft: 4096 },
		capture: { run_name: 'explorer' },
		metadata: [{ key: 'run_number', label: 'Run Number', value: '' }],
		record: defaultRecord(),
		steps: { ...DEFAULT_STEPS }
	};
}

const isPair = (v: unknown): v is [number, number] => Array.isArray(v) && v.length === 2 && v.every((x) => typeof x === 'number');

/** A draft read back from storage, every part filled from the defaults where it is missing. */
export function mergeExplorerSetup(saved: Partial<ExplorerDraft> | null): ExplorerDraft {
	const base = defaultExplorerSetup();
	if (!saved || typeof saved !== 'object') return base;
	const out = { ...base, ...saved } as ExplorerDraft;
	if (typeof out.title !== 'string') out.title = base.title;
	out.acquisition = { ...base.acquisition, ...(saved.acquisition ?? {}) };
	if (!isPair(out.band_hz)) out.band_hz = base.band_hz;
	if (typeof out.start_hz !== 'number') out.start_hz = base.start_hz;
	if (typeof out.max_drive_vpk !== 'number') out.max_drive_vpk = base.max_drive_vpk;
	out.slew = { ...base.slew, ...(saved.slew ?? {}) };
	out.display = { ...base.display, ...(saved.display ?? {}) };
	out.capture = { ...base.capture, ...(saved.capture ?? {}) };
	if (!Array.isArray(out.metadata)) out.metadata = base.metadata;
	out.record = { ...base.record, ...(saved.record ?? {}) };
	out.steps = { ...base.steps, ...(saved.steps ?? {}) };
	return out;
}

// ---------------------------------------------------------------------------------------------
// To and from the protocol

/** The configuration the wire takes: every field explicit, the steering steps left out. */
export function toExplorerConfig(d: ExplorerDraft): ExplorerConfig {
	const r = d.record;
	const [document, clause] = [r.document.trim(), r.clause.trim()];
	return {
		title: d.title.trim(),
		acquisition: { fs_hz: d.acquisition.fs_hz },
		band_hz: [d.band_hz[0], d.band_hz[1]],
		start_hz: d.start_hz,
		max_drive_vpk: d.max_drive_vpk,
		slew: { freq_oct_per_s: d.slew.freq_oct_per_s, drive_db_per_s: d.slew.drive_db_per_s },
		display: { nfft: d.display.nfft },
		capture: { run_name: d.capture.run_name.trim() },
		metadata: d.metadata.map((m) => (m.label ? { key: m.key, label: m.label, value: m.value } : { key: m.key, value: m.value })),
		operator: r.operator.trim(),
		article: r.article.trim(),
		notes: r.notes.trim(),
		specification: document || clause ? { document, clause } : null,
		record: { streams: r.streams }
	};
}

/**
 * The draft for a configuration the service holds (another screen applied it, or this one before a
 * reload). Fields the configuration leaves out take the wire's defaults; the steering steps stay
 * this screen's.
 */
export function fromExplorerConfig(c: ExplorerConfig, steps: ExplorerSteps = DEFAULT_STEPS): ExplorerDraft {
	return {
		title: c.title ?? '',
		acquisition: { fs_hz: c.acquisition?.fs_hz ?? null },
		band_hz: c.band_hz ? [c.band_hz[0], c.band_hz[1]] : [10, 2000],
		start_hz: c.start_hz ?? 100,
		max_drive_vpk: c.max_drive_vpk,
		slew: { freq_oct_per_s: c.slew?.freq_oct_per_s ?? 1, drive_db_per_s: c.slew?.drive_db_per_s ?? 6 },
		display: { nfft: c.display?.nfft ?? 4096 },
		capture: { run_name: c.capture?.run_name ?? 'explorer' },
		metadata: (c.metadata ?? []).map((m) => ({ key: m.key, label: m.label ?? '', value: m.value })),
		record: {
			operator: c.operator ?? '',
			article: c.article ?? '',
			notes: c.notes ?? '',
			document: c.specification?.document ?? '',
			clause: c.specification?.clause ?? '',
			streams: c.record?.streams ?? true
		},
		steps: { ...steps }
	};
}

// ---------------------------------------------------------------------------------------------
// The run's rows

/**
 * The rows the run uses, as PROTOCOL names them: the one enabled drive row, and the control row
 * (the first enabled control row, else the first enabled input), which the survey line, the
 * measured frequency and the derived motion are read from.
 */
export function explorerChannels(rows: ChannelRow[]): { control: ChannelRow | null; drive: ChannelRow | null; drives: number; inputs: number } {
	const on = rows.filter((r) => r.enabled);
	const inputs = on.filter((r) => r.role !== 'drive');
	const drives = on.filter((r) => r.role === 'drive');
	return { control: inputs.find((r) => r.role === 'control') ?? inputs[0] ?? null, drive: drives[0] ?? null, drives: drives.length, inputs: inputs.length };
}

// ---------------------------------------------------------------------------------------------
// Where a finding belongs

/** The setup's sections, in the screen's order. */
export type ExplorerSection = 'station' | 'test' | 'capture';

export const EXPLORER_SECTIONS: { id: ExplorerSection; label: string }[] = [
	{ id: 'station', label: 'Station & Channels' },
	{ id: 'test', label: 'Tone & Drive' },
	{ id: 'capture', label: 'Run Record' }
];

/** The table's findings, located on the station's section. */
const TABLE_REASONS = ['no_drive_channel', 'extra_drive_channels', 'no_input_channel'];

/**
 * The editor's key for a service finding. A configuration item's `field` is already the path
 * inside the configuration; a refusal's `param` (a shape error) carries the request's `config.`
 * head, which goes. The band's two ends share the band's key.
 */
export function explorerItemKey(item: Pick<ReportItem, 'field' | 'channel'> & { reason: string }): string {
	if (TABLE_REASONS.includes(item.reason)) return 'station.channels';
	if (item.field) {
		const path = item.field.replace(/^config\./, '').replace(/\[(\d+)\]/g, '.$1');
		if (/^band_hz(\.|$)/.test(path)) return 'band_hz';
		if (/^metadata(\.|$)/.test(path)) return 'metadata';
		return path;
	}
	if (item.channel !== undefined && item.channel !== null) return 'station.channels';
	return 'setup';
}

export function explorerByKey(items: { field?: string | null; channel?: number | null; reason: string; message: string }[]): FieldErrors {
	const out: FieldErrors = {};
	for (const it of items) {
		const k = explorerItemKey(it as Pick<ReportItem, 'field' | 'channel'> & { reason: string });
		if (!(out[k] ??= []).includes(it.message)) out[k].push(it.message);
	}
	return out;
}

/** The section a key belongs to. */
export function explorerSectionOf(key: string): ExplorerSection {
	if (/^station|^table|^channel/.test(key)) return 'station';
	if (/^(capture|metadata|operator|article|notes|specification|record)(\.|$)/.test(key)) return 'capture';
	return 'test';
}

// ---------------------------------------------------------------------------------------------
// What the editor can say before anything is sent

const NUMBER = 'Enter a number.';

function add(map: FieldErrors, key: string, message: string) {
	(map[key] ??= []).push(message);
}

function need(map: FieldErrors, key: string, v: number) {
	if (!Number.isFinite(v)) add(map, key, NUMBER);
}

/**
 * Fields that hold no number, a drive ceiling not given, a run folder not named, a steering step
 * that does not move. The ranges, the band against the rate, the ceiling against the drive row, the
 * stop against the service's wait and the table's one drive row and its inputs are the service's
 * to judge in the dry run.
 */
export function localErrors(d: ExplorerDraft): FieldErrors {
	const out: FieldErrors = {};
	if (d.acquisition.fs_hz !== null) need(out, 'acquisition.fs_hz', d.acquisition.fs_hz);
	if (!(Number.isFinite(d.band_hz[0]) && Number.isFinite(d.band_hz[1]))) add(out, 'band_hz', 'Give both ends of the band.');
	need(out, 'start_hz', d.start_hz);
	if (!Number.isFinite(d.max_drive_vpk)) add(out, 'max_drive_vpk', 'Give the drive ceiling in volts peak. There is no default: it is your amplifier input.');
	need(out, 'slew.freq_oct_per_s', d.slew.freq_oct_per_s);
	need(out, 'slew.drive_db_per_s', d.slew.drive_db_per_s);
	need(out, 'display.nfft', d.display.nfft);
	if (!d.capture.run_name.trim()) add(out, 'capture.run_name', 'Name the run folder.');
	if (d.record.clause.trim() && !d.record.document.trim()) add(out, 'specification.document', 'Name the document this clause is from.');
	const s = d.steps;
	for (const k of ['fine_per_oct', 'coarse_per_oct', 'fine_db', 'coarse_db', 'sweep_oct_per_min'] as const) {
		if (!Number.isFinite(s[k])) add(out, `steps.${k}`, NUMBER);
		else if (!(s[k] > 0)) add(out, `steps.${k}`, 'A step is above 0.');
	}
	if (Number.isFinite(s.fine_per_oct) && Number.isFinite(s.coarse_per_oct) && s.coarse_per_oct > s.fine_per_oct) add(out, 'steps.coarse_per_oct', 'The coarse step is at least the fine one: no more steps an octave.');
	if (Number.isFinite(s.fine_db) && Number.isFinite(s.coarse_db) && s.coarse_db < s.fine_db) add(out, 'steps.coarse_db', 'The coarse step is at least the fine one.');
	return out;
}

/** The steering steps are this screen's only; the wire does not carry them. */
export const isSteeringKey = (key: string): boolean => key.startsWith('steps.');
