/**
 * The MIMO Random setup as the operator edits it, and its conversion to the protocol's
 * `MimoSetup` (schema/PROTOCOL.md, "MIMO Random Setup"). Pure functions only.
 *
 * The channels are the channel table's rows: a drive row is a drive, a control row a control, a
 * monitor row a monitor, or a limit when it carries a limit. The draft keeps each channel's own
 * settings (a drive's ceilings and shaker, a control's bands, a limit's ceiling) by its table
 * index, so a row keeps them when the table is reordered.
 *
 * Numbers are held as numbers; a field that holds no number is NaN, which `localErrors` names
 * before anything is sent. The service judges everything else, by the dry run of the setup, and
 * `itemKey` puts each of its findings on the field or channel it names.
 */

import type { BandDb, ChannelRow, LadderMode, ReportItem } from '$lib/protocol/types';
import type {
	LawEntry,
	MimoBreakpoint,
	MimoBuildBreakpointsParams,
	MimoPairSpec,
	MimoPairValue,
	MimoReference,
	MimoRigidParams,
	MimoSetup,
	MimoTransform
} from '$lib/protocol/mimo';
import { defaultRecord, newBreakpoint, type BreakpointDraft, type FieldErrors, type LevelDraft, type RecordDraft } from '../setup';
import { lawWireParams, type LawValue } from '../law/lawForm';

// ---------------------------------------------------------------------------------------------
// The draft

export type MimoRole = 'drive' | 'control' | 'limit' | 'monitor';

export const MIMO_ROLES: { id: MimoRole; label: string }[] = [
	{ id: 'drive', label: 'Drive' },
	{ id: 'control', label: 'Control' },
	{ id: 'limit', label: 'Limit' },
	{ id: 'monitor', label: 'Monitor' }
];

export const MAX_CONTROLS = 72;
export const MAX_DRIVES = 24;
/** The most values an inline reference carries (the service's limit). */
export const MAX_INLINE_VALUES = 262_144;

export interface DriveSettings {
	ceiling_vrms: number;
	peak_v: number;
	/** null: no drive abort beyond the ceiling's protection. */
	abort_vrms: number | null;
	/** The shaker record as text (JSON, the shaker database's form); empty: not checked. */
	shaker_text: string;
	moving_mass_lb: number;
}

export interface ControlSettings {
	alarm_db: BandDb;
	abort_db: BandDb;
}

export interface LimitSettings {
	/** PSD ceiling at full level, units squared per hertz. */
	ceiling: number;
	abort_margin_db: number;
}

/** A real matrix as the operator enters it, row-major. */
export interface MatrixDraft {
	rows: number;
	cols: number;
	values: number[];
	/** Where the values came from, in words ("Pasted", a file name), or null. */
	source: string | null;
}

export type PairValueDraft = { kind: 'constant'; value: number } | { kind: 'table'; points: { freq_hz: number; value: number }[] };

export interface PairDraft {
	row: number;
	col: number;
	coherence: PairValueDraft;
	phase: PairValueDraft;
}

export type ReferenceMethod = 'import' | 'breakpoints' | 'rigid' | 'field';

export interface SensorGeometry {
	position: [number, number, number];
	direction: [number, number, number];
}

export interface MimoDraft {
	title: string;
	drives: Record<string, DriveSettings>;
	controls: Record<string, ControlSettings>;
	/** A monitor row with an entry here is a limit. */
	limits: Record<string, LimitSettings>;
	article_mass_lb: number;
	space: {
		kind: 'physical' | 'virtual';
		dofs: string[];
		response: MatrixDraft;
		/** The drives are transformed too (ganged or differential shakers). */
		driveOn: boolean;
		drive: MatrixDraft;
		/** Under virtual control, each degree of freedom's bands, by its name. */
		dofBands: Record<string, ControlSettings>;
	};
	reference: {
		method: ReferenceMethod;
		importPath: string;
		importPairPath: string;
		unit: string;
		/** One PSD breakpoint table per controlled quantity, by its label. */
		tables: Record<string, BreakpointDraft[]>;
		alarm_db: BandDb;
		abort_db: BandDb;
		pairs: PairDraft[];
		unspecified: 'independent' | 'fully_coherent';
		/** The operator accepted the repair the service described. */
		acceptRepair: boolean;
		previewPair: [number, number];
		rigid: { sensors: Record<string, SensorGeometry>; reference_point: [number, number, number]; length_unit: string };
	};
	law: { id: string; params: Record<string, LawValue> };
	frf: 'hold' | 'update';
	frf_blend: number;
	estimation: { fs_hz: number; n_pts: number; frames_per_loop: number; window: 'hann' | 'rectangular'; overlap: number; memory_loops: number };
	identification: { level_db: number; frames: number; noise_frames: number; coherence_floor: number; condition_limit: number; review: boolean };
	control: { level_ramp_s: number; shutdown_ramp_s: number; max_loops_out: number; cap_up_db: number; cap_down_db: number; law_budget: number; law_misses: number };
	schedule: { mode: LadderMode; levels: LevelDraft[]; full_level_s: number };
	/** The run's record: the operator's words and whether the time histories are kept (Random's). */
	record: RecordDraft;
}

export type { LawValue };

export const DEFAULT_LAW = 'pinv_closed';
export const RIGID_DOFS = ['X', 'Y', 'Z', 'RX', 'RY', 'RZ'];

export function emptyMatrix(rows: number, cols: number): MatrixDraft {
	return { rows, cols, values: new Array(rows * cols).fill(0), source: null };
}

/** The identity where it fits, zero elsewhere: a starting point the operator edits. */
export function identityMatrix(rows: number, cols: number): MatrixDraft {
	const m = emptyMatrix(rows, cols);
	for (let i = 0; i < Math.min(rows, cols); i++) m.values[i * cols + i] = 1;
	return m;
}

/** The service's defaults (PROTOCOL.md, "MIMO Random Setup"). */
export function defaultMimoSetup(): MimoDraft {
	return {
		title: 'MIMO Random',
		drives: {},
		controls: {},
		limits: {},
		article_mass_lb: 0,
		space: { kind: 'physical', dofs: [], response: emptyMatrix(0, 0), driveOn: false, drive: emptyMatrix(0, 0), dofBands: {} },
		reference: {
			method: 'breakpoints',
			importPath: '',
			importPairPath: '',
			unit: 'g',
			tables: {},
			alarm_db: [3, 3],
			abort_db: [6, 6],
			pairs: [],
			unspecified: 'independent',
			acceptRepair: false,
			previewPair: [0, 1],
			rigid: { sensors: {}, reference_point: [0, 0, 0], length_unit: 'm' }
		},
		law: { id: DEFAULT_LAW, params: {} },
		frf: 'hold',
		frf_blend: 0.5,
		estimation: { fs_hz: 8192, n_pts: 4096, frames_per_loop: 8, window: 'hann', overlap: 0.5, memory_loops: 4 },
		identification: { level_db: -20, frames: 32, noise_frames: 16, coherence_floor: 0.5, condition_limit: 1000, review: true },
		control: { level_ramp_s: 3, shutdown_ramp_s: 3, max_loops_out: 5, cap_up_db: 3.5, cap_down_db: 6, law_budget: 0.25, law_misses: 3 },
		schedule: {
			mode: 'auto',
			levels: [
				{ db: -12, dwell_s: 10 },
				{ db: -6, dwell_s: 10 },
				{ db: 0, dwell_s: 0 }
			],
			full_level_s: 60
		},
		record: defaultRecord()
	};
}

export function defaultDrive(): DriveSettings {
	return { ceiling_vrms: 2, peak_v: 6, abort_vrms: null, shaker_text: '', moving_mass_lb: 0 };
}

export function defaultControl(d: MimoDraft): ControlSettings {
	return { alarm_db: [...d.reference.alarm_db] as BandDb, abort_db: [...d.reference.abort_db] as BandDb };
}

export function defaultLimit(): LimitSettings {
	return { ceiling: 0.1, abort_margin_db: 3 };
}

/** A flat profile across the usual band: where a new table starts. */
export function defaultTable(): BreakpointDraft[] {
	return [newBreakpoint(20, 'psd', 0.001), newBreakpoint(80, 'slope', 3), newBreakpoint(500, 'psd', 0.004), newBreakpoint(1000, 'slope', -3)];
}

// ---------------------------------------------------------------------------------------------
// Channels and roles

export const rowName = (r: ChannelRow): string => (r.description.trim() ? r.description.trim() : `Channel ${r.index}`);

/** A row's role in the test, or 'reference' for a row MIMO Random does not use. */
export function roleOf(r: ChannelRow, d: MimoDraft): MimoRole | 'reference' {
	if (r.role === 'drive') return 'drive';
	if (r.role === 'control') return 'control';
	if (r.role === 'reference') return 'reference';
	return String(r.index) in d.limits ? 'limit' : 'monitor';
}

export interface TestChannels {
	drives: ChannelRow[];
	controls: ChannelRow[];
	limits: ChannelRow[];
	monitors: ChannelRow[];
}

/** The enabled rows by their role in the test, in table order. */
export function testChannels(rows: ChannelRow[], d: MimoDraft): TestChannels {
	const out: TestChannels = { drives: [], controls: [], limits: [], monitors: [] };
	for (const r of rows) {
		if (!r.enabled) continue;
		const role = roleOf(r, d);
		if (role === 'drive') out.drives.push(r);
		else if (role === 'control') out.controls.push(r);
		else if (role === 'limit') out.limits.push(r);
		else if (role === 'monitor') out.monitors.push(r);
	}
	return out;
}

/**
 * Give a row a role: the table row's own role, and for a limit the draft's limit entry. Returns
 * the table role the row needs.
 */
export function assignRole(r: ChannelRow, role: MimoRole, d: MimoDraft): void {
	const key = String(r.index);
	const wasDrive = r.role === 'drive';
	r.role = role === 'limit' ? 'monitor' : role;
	if (role === 'limit') d.limits[key] ??= defaultLimit();
	else delete d.limits[key];
	if (role === 'drive') {
		d.drives[key] ??= defaultDrive();
		r.units = 'V';
		r.sensitivity_mv_per_eu = 1000;
		r.range_max = 10;
		r.interface = 'single_ended';
		r.iepe_bias_v = null;
	} else if (wasDrive) {
		r.units = 'g';
		r.sensitivity_mv_per_eu = 100;
		r.range_max = 100;
	}
	if (wasDrive !== (role === 'drive')) r.physical = null;
	if (role === 'control') d.controls[key] ??= defaultControl(d);
}

export const driveOf = (d: MimoDraft, ch: number): DriveSettings => d.drives[String(ch)] ?? defaultDrive();
export const controlOf = (d: MimoDraft, ch: number): ControlSettings => d.controls[String(ch)] ?? defaultControl(d);
export const limitOf = (d: MimoDraft, ch: number): LimitSettings => d.limits[String(ch)] ?? defaultLimit();
/** A degree of freedom's bands under virtual control: its own, or the reference's defaults. */
export const dofBandsOf = (d: MimoDraft, name: string): ControlSettings => d.space.dofBands?.[name] ?? defaultControl(d);

/** Give a degree of freedom its own bands to edit, starting from the defaults. */
export function ensureDofBands(d: MimoDraft, names: string[]): void {
	if (!d.space.dofBands) d.space.dofBands = {};
	for (const n of names) if (!d.space.dofBands[n]) d.space.dofBands[n] = defaultControl(d);
}

/** Make sure every drive, control and limit row has its settings in the draft. */
export function ensureSettings(rows: ChannelRow[], d: MimoDraft): void {
	const t = testChannels(rows, d);
	for (const r of t.drives) d.drives[String(r.index)] ??= defaultDrive();
	for (const r of t.controls) d.controls[String(r.index)] ??= defaultControl(d);
}

// ---------------------------------------------------------------------------------------------
// The control space

export interface EffectiveSpace {
	kind: 'physical' | 'virtual';
	dofs: string[];
	response: MatrixDraft | null;
	drive: MatrixDraft | null;
	/** Who set it: the operator, the rigid-body fit, or the imported reference. */
	from: 'operator' | 'rigid' | 'import';
}

/** The labels of what the test controls: the control channels, or the degrees of freedom. */
export function controlledLabels(rows: ChannelRow[], d: MimoDraft, space: EffectiveSpace): string[] {
	if (space.kind === 'virtual') return space.dofs;
	return testChannels(rows, d).controls.map(rowName);
}

/** Bring the operator's matrices to the shape the channels and degrees of freedom need. */
export function shapeSpace(d: MimoDraft, controls: number, drives: number): void {
	const s = d.space;
	const n = s.dofs.length;
	if (s.response.rows !== n || s.response.cols !== controls) s.response = resize(s.response, n, controls);
	const u = s.drive.cols || drives;
	if (s.drive.rows !== drives) s.drive = resize(s.drive, drives, u);
}

/** The matrix at a new shape, keeping the values that still fit. */
export function resize(m: MatrixDraft, rows: number, cols: number): MatrixDraft {
	const out = emptyMatrix(rows, cols);
	for (let r = 0; r < Math.min(rows, m.rows); r++) for (let c = 0; c < Math.min(cols, m.cols); c++) out.values[r * cols + c] = m.values[r * m.cols + c];
	out.source = m.source;
	return out;
}

/**
 * Cells of a table as a spreadsheet copies them: rows on lines, cells split by tabs (or by
 * commas or semicolons when there is no tab). Empty trailing lines are dropped; a cell that does
 * not read as a number is NaN and listed.
 */
export function parseGrid(text: string): { cells: number[][]; bad: { row: number; col: number; text: string }[] } {
	const lines = text.replace(/\r\n?/g, '\n').split('\n');
	while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
	const bad: { row: number; col: number; text: string }[] = [];
	const cells = lines.map((line, r) => {
		const parts = line.includes('\t') ? line.split('\t') : line.split(/[,;]/);
		return parts.map((raw, c) => {
			const t = raw.trim();
			const v = t === '' ? NaN : Number(t);
			if (!Number.isFinite(v)) bad.push({ row: r, col: c, text: t });
			return v;
		});
	});
	return { cells, bad };
}

/**
 * Paste a grid into a matrix with its top-left cell at (row, col). Cells beyond the matrix are
 * left out and counted; the matrix keeps its shape.
 */
export function pasteInto(m: MatrixDraft, cells: number[][], row = 0, col = 0): { matrix: MatrixDraft; outside: number } {
	const out: MatrixDraft = { ...m, values: [...m.values] };
	let outside = 0;
	cells.forEach((line, i) =>
		line.forEach((v, j) => {
			const r = row + i;
			const c = col + j;
			if (r < m.rows && c < m.cols) out.values[r * m.cols + c] = v;
			else outside++;
		})
	);
	return { matrix: out, outside };
}

/**
 * A CSV file's matrix. A first row or first column of names (cells that are not numbers) is
 * read as labels and left out of the values.
 */
export function matrixFromCsv(text: string): { values: number[][]; rowLabels: string[] | null; colLabels: string[] | null; error: string | null } {
	const lines = text
		.replace(/\r\n?/g, '\n')
		.split('\n')
		.filter((l) => l.trim() && !l.trim().startsWith('#'));
	const split = (l: string) => (l.includes('\t') ? l.split('\t') : l.split(/[,;]/)).map((c) => c.trim());
	let rows = lines.map(split);
	if (!rows.length) return { values: [], rowLabels: null, colLabels: null, error: 'The file holds no rows.' };
	const num = (c: string) => c !== '' && Number.isFinite(Number(c));
	let colLabels: string[] | null = null;
	let rowLabels: string[] | null = null;
	if (rows[0].slice(1).some((c) => !num(c))) {
		colLabels = rows[0].slice(1);
		rows = rows.slice(1);
	}
	if (rows.length && rows.every((r) => !num(r[0]))) {
		rowLabels = rows.map((r) => r[0]);
		rows = rows.map((r) => r.slice(1));
		if (!colLabels) colLabels = null;
	} else if (colLabels) {
		// The header named every column, the first included.
		colLabels = [lines.length ? split(lines[0])[0] : '', ...colLabels];
	}
	const values = rows.map((r) => r.map((c) => (num(c) ? Number(c) : NaN)));
	const width = values[0]?.length ?? 0;
	if (values.some((r) => r.length !== width)) return { values, rowLabels, colLabels, error: 'Every row of the file has the same number of values.' };
	const badRow = values.findIndex((r) => r.some((v) => !Number.isFinite(v)));
	if (badRow >= 0) return { values, rowLabels, colLabels, error: `Row ${badRow + 1} of the file holds a value that is not a number.` };
	return { values, rowLabels, colLabels, error: null };
}

export function toTransform(m: MatrixDraft): MimoTransform {
	return { rows: m.rows, cols: m.cols, values: [...m.values] };
}

// ---------------------------------------------------------------------------------------------
// The reference

export function breakpointsWire(rows: BreakpointDraft[]): MimoBreakpoint[] {
	return rows.map((b) => {
		const row: MimoBreakpoint = b.kind === 'psd' ? { freq_hz: b.freq_hz, psd_g2_hz: b.psd_g2_hz } : { freq_hz: b.freq_hz, slope_db_oct: b.slope_db_oct };
		if (b.alarm_db) row.alarm_db = [b.alarm_db[0], b.alarm_db[1]];
		if (b.abort_db) row.abort_db = [b.abort_db[0], b.abort_db[1]];
		return row;
	});
}

function pairValueWire(v: PairValueDraft): MimoPairValue {
	return v.kind === 'constant' ? v.value : v.points.map((p) => [p.freq_hz, p.value] as [number, number]);
}

/** Pairs that name two of the present quantities, each pair once. */
export function livePairs(pairs: PairDraft[], n: number): PairDraft[] {
	const seen = new Set<string>();
	return pairs.filter((p) => {
		const key = `${Math.min(p.row, p.col)}-${Math.max(p.row, p.col)}`;
		if (p.row === p.col || p.row >= n || p.col >= n || seen.has(key)) return false;
		seen.add(key);
		return true;
	});
}

/** The table for a label, made on first use. */
export function tableFor(d: MimoDraft, label: string): BreakpointDraft[] {
	return (d.reference.tables[label] ??= defaultTable());
}

/** What the breakpoint builder is asked: every quantity's table, the pairs and the bands. */
export function breakpointParams(d: MimoDraft, labels: string[]): MimoBuildBreakpointsParams {
	const r = d.reference;
	const n = labels.length;
	const pair = r.previewPair[0] < n && r.previewPair[1] < n && r.previewPair[0] !== r.previewPair[1] ? r.previewPair : ([0, 1] as [number, number]);
	return {
		grid: { fs_hz: d.estimation.fs_hz, n_pts: d.estimation.n_pts },
		unit: r.unit,
		tables: labels.map((label) => ({ label, rows: breakpointsWire(r.tables[label] ?? defaultTable()) })),
		pairs: livePairs(r.pairs, n).map((p): MimoPairSpec => ({ row: p.row, col: p.col, coherence: pairValueWire(p.coherence), phase_deg: pairValueWire(p.phase) })),
		unspecified: r.unspecified,
		alarm_db: [r.alarm_db[0], r.alarm_db[1]],
		abort_db: [r.abort_db[0], r.abort_db[1]],
		repair: r.acceptRepair,
		preview_pair: pair
	};
}

/** The rigid-body builder's input: each control channel's geometry, in channel order. */
export function rigidParams(rows: ChannelRow[], d: MimoDraft): MimoRigidParams {
	const g = d.reference.rigid;
	return {
		sensors: testChannels(rows, d).controls.map((r) => {
			const s = g.sensors[String(r.index)] ?? { position: [0, 0, 0], direction: [0, 0, 1] };
			return { label: rowName(r), position: [...s.position] as [number, number, number], direction: [...s.direction] as [number, number, number] };
		}),
		reference_point: [...g.reference_point] as [number, number, number],
		unit: d.reference.unit,
		length_unit: g.length_unit
	};
}

// ---------------------------------------------------------------------------------------------
// The law (shared with every test type that offers a law choice: `../law/lawForm`)

export { choiceWord, lawParamDescriptor, lawParamError, lawValues } from '../law/lawForm';

// ---------------------------------------------------------------------------------------------
// To the protocol

export interface WireInputs {
	rows: ChannelRow[];
	space: EffectiveSpace;
	/** The reference the service built or read, or null while there is none. */
	reference: MimoReference | null;
	law: LawEntry | null;
	backend: 'sim' | 'ni' | null;
}

const finiteOrNull = (v: number | null): number | null => (v === null || !Number.isFinite(v) ? null : v);

export function shakerRecord(text: string): Record<string, unknown> | null {
	try {
		const parsed: unknown = JSON.parse(text);
		return typeof parsed === 'object' && parsed !== null && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : null;
	} catch {
		return null;
	}
}

/** The setup the draft describes, or null while it has no reference. Call `localErrors` first. */
export function toWireSetup(d: MimoDraft, x: WireInputs): MimoSetup | null {
	if (!x.reference) return null;
	const t = testChannels(x.rows, d);
	const last = d.schedule.levels.length - 1;
	const params = lawWireParams(x.law, d.law.params);
	const r = d.record;
	const [document, clause] = [r.document.trim(), r.clause.trim()];
	const setup: MimoSetup = {
		title: d.title.trim(),
		operator: r.operator.trim(),
		article: r.article.trim(),
		notes: r.notes.trim(),
		specification: document || clause ? { document, clause } : null,
		record: { streams: r.streams },
		channels: {
			controls: t.controls.map((r) => {
				const c = controlOf(d, r.index);
				return { channel: r.index, alarm_db: [c.alarm_db[0], c.alarm_db[1]], abort_db: [c.abort_db[0], c.abort_db[1]] };
			}),
			drives: t.drives.map((r) => {
				const s = driveOf(d, r.index);
				const shaker = s.shaker_text.trim() ? shakerRecord(s.shaker_text) : null;
				return { channel: r.index, ceiling_vrms: s.ceiling_vrms, abort_vrms: finiteOrNull(s.abort_vrms), peak_v: s.peak_v, shaker, moving_mass_lb: s.moving_mass_lb };
			}),
			limits: t.limits.map((r) => {
				const l = limitOf(d, r.index);
				return { channel: r.index, ceiling: l.ceiling, abort_margin_db: l.abort_margin_db };
			}),
			article_mass_lb: d.article_mass_lb
		},
		space:
			x.space.kind === 'virtual' && x.space.response
				? {
						kind: 'virtual',
						dofs: [...x.space.dofs],
						response_transform: toTransform(x.space.response),
						drive_transform: x.space.drive ? toTransform(x.space.drive) : null,
						dof_bands: x.space.dofs.map((n) => {
							const b = dofBandsOf(d, n);
							return { alarm_db: [b.alarm_db[0], b.alarm_db[1]] as BandDb, abort_db: [b.abort_db[0], b.abort_db[1]] as BandDb };
						})
					}
				: { kind: 'physical' },
		reference: x.reference,
		law: { id: d.law.id, params },
		estimation: { ...d.estimation },
		identification: { ...d.identification },
		control: {
			frf: d.frf,
			frf_blend: d.frf_blend,
			level_ramp_s: d.control.level_ramp_s,
			shutdown_ramp_s: d.control.shutdown_ramp_s,
			max_loops_out: d.control.max_loops_out,
			cap_up_db: d.control.cap_up_db,
			cap_down_db: d.control.cap_down_db,
			law_budget: d.control.law_budget,
			law_misses: d.control.law_misses
		},
		schedule: {
			mode: d.schedule.mode,
			levels: d.schedule.levels.map((l, i) => (i === last ? { db: l.db } : { db: l.db, dwell_s: l.dwell_s })),
			full_level_s: d.schedule.full_level_s
		},
		backend: x.backend ? { kind: x.backend } : null
	};
	return setup;
}

/** JSON with every object's keys sorted, so the same setup always gives the same text. */
export function canonical(v: unknown): string {
	if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
	if (v !== null && typeof v === 'object') {
		const o = v as Record<string, unknown>;
		const keys = Object.keys(o)
			.filter((k) => o[k] !== undefined)
			.sort();
		return `{${keys.map((k) => `${JSON.stringify(k)}:${canonical(o[k])}`).join(',')}}`;
	}
	return JSON.stringify(v) ?? 'null';
}

// ---------------------------------------------------------------------------------------------
// Where a finding belongs

/**
 * The editor's key for a service finding. Field paths lose their `setup.` head and their
 * brackets; a path into a channel list names the channel instead of its position
 * (`setup.channels.drives[1].peak_v` is `drive.4.peak_v` when the second drive is channel 4);
 * a finding about a channel is `channel.<index>`.
 */
export function itemKey(item: ReportItem, setup: MimoSetup | null): string {
	if (item.field) {
		const path = item.field.replace(/^setup\./, '').replace(/\[(\d+)\]/g, '.$1');
		const m = /^channels\.(controls|drives|limits)\.(\d+)(.*)$/.exec(path);
		if (m && setup) {
			const list = setup.channels[m[1] as 'controls' | 'drives' | 'limits'] ?? [];
			const ch = list[Number(m[2])]?.channel;
			const kind = m[1] === 'controls' ? 'control' : m[1] === 'drives' ? 'drive' : 'limit';
			if (ch !== undefined) return `${kind}.${ch}${m[3]}`;
		}
		return path;
	}
	if (item.channel !== undefined && item.channel !== null) return `channel.${item.channel}`;
	return 'setup';
}

export function byKey(items: ReportItem[], setup: MimoSetup | null): FieldErrors {
	const out: FieldErrors = {};
	for (const it of items) (out[itemKey(it, setup)] ??= []).push(it.message);
	return out;
}

/** The section a key belongs to. */
export type MimoSection = 'channels' | 'reference' | 'control' | 'schedule' | 'record' | 'backend' | 'predict';

export function sectionOf(key: string): MimoSection {
	if (/^(operator|article|notes|specification|record)(\.|$)/.test(key)) return 'record';
	if (/^(channel|drive|control|limit)\.|^channels|^space|^title/.test(key)) return 'channels';
	if (/^reference/.test(key)) return 'reference';
	if (/^schedule/.test(key)) return 'schedule';
	if (/^backend|^table/.test(key)) return 'backend';
	return 'control';
}

// ---------------------------------------------------------------------------------------------
// What the editor can say before anything is sent

const NUMBER = 'Enter a number.';
const WHOLE = 'Enter a whole number.';

function add(map: FieldErrors, key: string, message: string) {
	(map[key] ??= []).push(message);
}

function need(map: FieldErrors, key: string, v: number, whole = false) {
	if (!Number.isFinite(v)) add(map, key, NUMBER);
	else if (whole && !Number.isInteger(v)) add(map, key, WHOLE);
}

/** Role conflicts and channel settings the editor can judge on its own. */
export function channelErrors(rows: ChannelRow[], d: MimoDraft): FieldErrors {
	const out: FieldErrors = {};
	const t = testChannels(rows, d);
	for (const r of rows) {
		if (r.enabled && r.role === 'reference') add(out, `channel.${r.index}`, `${rowName(r)} is a reference row, which MIMO Random does not use. Make it a control, a limit or a monitor.`);
	}
	if (!t.controls.length) add(out, 'channels', 'A MIMO test needs at least one control channel. Give a channel the Control role.');
	if (!t.drives.length) add(out, 'channels', 'A MIMO test needs at least one drive. Give an output the Drive role.');
	if (t.controls.length > MAX_CONTROLS) add(out, 'channels', `A MIMO test has at most ${MAX_CONTROLS} control channels; ${t.controls.length} have the Control role.`);
	if (t.drives.length > MAX_DRIVES) add(out, 'channels', `A MIMO test has at most ${MAX_DRIVES} drives; ${t.drives.length} have the Drive role.`);
	for (const r of t.drives) {
		const s = driveOf(d, r.index);
		const k = `drive.${r.index}`;
		need(out, `${k}.ceiling_vrms`, s.ceiling_vrms);
		need(out, `${k}.peak_v`, s.peak_v);
		need(out, `${k}.moving_mass_lb`, s.moving_mass_lb);
		if (s.abort_vrms !== null) need(out, `${k}.abort_vrms`, s.abort_vrms);
		if (Number.isFinite(s.ceiling_vrms) && (s.ceiling_vrms <= 0 || s.ceiling_vrms > 10)) add(out, `${k}.ceiling_vrms`, 'Above 0 and at most 10 V rms.');
		if (Number.isFinite(s.peak_v) && Number.isFinite(s.ceiling_vrms) && (s.peak_v < s.ceiling_vrms || s.peak_v > 10)) add(out, `${k}.peak_v`, 'At least the rms ceiling and at most 10 V.');
		if (s.abort_vrms !== null && Number.isFinite(s.abort_vrms) && s.abort_vrms <= s.ceiling_vrms) add(out, `${k}.abort_vrms`, 'Above the ceiling, or off.');
		if (s.shaker_text.trim() && !shakerRecord(s.shaker_text)) add(out, `${k}.shaker`, 'The shaker record does not read as a record (JSON object). Load the file again or clear it.');
	}
	const seen = new Map<string, number>();
	for (const r of t.controls) {
		const name = rowName(r);
		const first = seen.get(name);
		if (first !== undefined) add(out, `channel.${r.index}`, `Channel ${first} is also named "${name}". Give each control its own name; the reference is written by name.`);
		else seen.set(name, r.index);
	}
	for (const r of t.controls) {
		const c = controlOf(d, r.index);
		const k = `control.${r.index}`;
		for (const [band, name] of [
			[c.alarm_db, 'alarm_db'],
			[c.abort_db, 'abort_db']
		] as const) {
			if (!(Number.isFinite(band[0]) && Number.isFinite(band[1]))) add(out, `${k}.${name}`, 'Give both sides of the band.');
			else if (band[0] <= 0 || band[1] <= 0) add(out, `${k}.${name}`, 'Both sides are above 0 dB.');
		}
	}
	for (const r of t.limits) {
		const l = limitOf(d, r.index);
		const k = `limit.${r.index}`;
		need(out, `${k}.ceiling`, l.ceiling);
		need(out, `${k}.abort_margin_db`, l.abort_margin_db);
		if (Number.isFinite(l.ceiling) && l.ceiling <= 0) add(out, `${k}.ceiling`, 'Above 0.');
	}
	need(out, 'channels.article_mass_lb', d.article_mass_lb);
	return out;
}

/** The control space: names, and every matrix cell a number. */
export function spaceErrors(rows: ChannelRow[], d: MimoDraft, space: EffectiveSpace): FieldErrors {
	const out: FieldErrors = {};
	if (space.kind !== 'virtual' || space.from !== 'operator') return out;
	const t = testChannels(rows, d);
	const dofs = d.space.dofs;
	if (!dofs.length) add(out, 'space.dofs', 'Name at least one degree of freedom.');
	if (dofs.some((n) => !n.trim())) add(out, 'space.dofs', 'Every degree of freedom has a name.');
	if (new Set(dofs.map((n) => n.trim())).size !== dofs.length) add(out, 'space.dofs', 'Each degree of freedom has its own name.');
	if (dofs.length > t.controls.length) add(out, 'space.dofs', `There are ${dofs.length} degrees of freedom and ${t.controls.length} control channels; the response transformation needs at least as many controls as degrees of freedom.`);
	if (d.space.response.values.some((v) => !Number.isFinite(v))) add(out, 'space.response_transform', 'Every cell holds a number.');
	if (d.space.driveOn) {
		if (d.space.drive.values.some((v) => !Number.isFinite(v))) add(out, 'space.drive_transform', 'Every cell holds a number.');
		if (d.space.drive.cols > d.space.drive.rows) add(out, 'space.drive_transform', 'There are more virtual drives than drives; the drive transformation needs at least as many drives as virtual drives.');
	}
	return out;
}

/** Everything else the editor can judge: numbers that are not numbers, an empty title. */
export function settingErrors(d: MimoDraft): FieldErrors {
	const out: FieldErrors = {};
	if (!d.title.trim()) add(out, 'title', 'Give the test a title.');
	if (d.record.clause.trim() && !d.record.document.trim()) add(out, 'specification.document', 'Name the document this clause is from.');
	const e = d.estimation;
	need(out, 'estimation.fs_hz', e.fs_hz);
	need(out, 'estimation.n_pts', e.n_pts, true);
	need(out, 'estimation.frames_per_loop', e.frames_per_loop, true);
	need(out, 'estimation.overlap', e.overlap);
	need(out, 'estimation.memory_loops', e.memory_loops, true);
	const i = d.identification;
	need(out, 'identification.level_db', i.level_db);
	need(out, 'identification.frames', i.frames, true);
	need(out, 'identification.noise_frames', i.noise_frames, true);
	need(out, 'identification.coherence_floor', i.coherence_floor);
	need(out, 'identification.condition_limit', i.condition_limit);
	const c = d.control;
	for (const k of ['level_ramp_s', 'shutdown_ramp_s', 'cap_up_db', 'cap_down_db', 'law_budget'] as const) need(out, `control.${k}`, c[k]);
	need(out, 'control.max_loops_out', c.max_loops_out, true);
	need(out, 'control.law_misses', c.law_misses, true);
	if (d.frf === 'update') need(out, 'control.frf_blend', d.frf_blend);
	const last = d.schedule.levels.length - 1;
	d.schedule.levels.forEach((l, k) => {
		need(out, `schedule.levels.${k}.db`, l.db);
		if (k < last) need(out, `schedule.levels.${k}.dwell_s`, l.dwell_s);
	});
	need(out, 'schedule.full_level_s', d.schedule.full_level_s);
	return out;
}

/** The breakpoint tables and pairs: numbers in every cell, coherence from 0 to 1. */
export function referenceErrors(d: MimoDraft, labels: string[]): FieldErrors {
	const out: FieldErrors = {};
	const r = d.reference;
	if (r.method === 'breakpoints' || r.method === 'rigid') {
		labels.forEach((label, q) => {
			(r.tables[label] ?? []).forEach((b, i) => {
				const k = `reference.tables.${q}.profile.breakpoints.${i}`;
				need(out, `${k}.freq_hz`, b.freq_hz);
				need(out, `${k}.value`, b.kind === 'psd' ? b.psd_g2_hz : b.slope_db_oct);
			});
		});
		livePairs(r.pairs, labels.length).forEach((p, i) => {
			const k = `reference.pairs.${i}`;
			for (const [v, what] of [
				[p.coherence, 'coherence'],
				[p.phase, 'phase']
			] as const) {
				const values = v.kind === 'constant' ? [v.value] : v.points.flatMap((pt) => [pt.freq_hz, pt.value]);
				if (values.some((x) => !Number.isFinite(x))) add(out, `${k}.${what}`, NUMBER);
				const cohs = v.kind === 'constant' ? [v.value] : v.points.map((pt) => pt.value);
				if (what === 'coherence' && cohs.some((x) => Number.isFinite(x) && (x < 0 || x > 1))) add(out, `${k}.coherence`, 'Coherence is from 0 to 1.');
			}
		});
		for (const band of [r.alarm_db, r.abort_db]) if (!(Number.isFinite(band[0]) && Number.isFinite(band[1]))) add(out, 'reference.bands', 'Give both sides of each band.');
	}
	if (r.method === 'import' && !r.importPath.trim()) add(out, 'reference.import', 'Give the path of the reference to import.');
	return out;
}

// ---------------------------------------------------------------------------------------------
// The setup summary

export interface SectionState {
	section: MimoSection;
	title: string;
	/** One line on where it stands. */
	state: string;
	/** What must happen before a run can start, in words. Empty: nothing. */
	missing: string[];
	errors: number;
	warnings: number;
}
