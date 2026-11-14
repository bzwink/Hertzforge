/**
 * Mock time waveform replication (schema/PROTOCOL.md, "Time Waveform Replication" and the `twr`
 * world of "Control Laws"), on the mock control service's run slot and controller seat.
 *
 * A believable closed-loop run, one whole record at a time: a reference imported by path or as
 * pasted text and kept by a hash, its preparation on a padded power-of-two record, identification
 * with band-limited random, the coherence hold, the review hold, the law seeded at proceed, then the
 * ladder: each play plays the whole record (its progress through the record counted in station
 * frames), is judged against the gate, and is corrected by the law until the level gates; once full
 * level gates, playback runs its schedule. On the manual ladder the run waits for Iterate, Play and
 * the level verbs. Freeze holds the law's corrections off; Stop brings the play in flight to rest
 * and ends; Abort ends at once; with nobody in control no new play starts and the run safe-stops
 * after the timeout.
 *
 * It refuses what the service refuses, by the same reasons and on the same parameters: a plot key
 * a quantity does not take (any channel selector; a point count off the record plots), a verb in
 * the wrong ladder mode (`wrong_mode`), playback before the level has gated (`not_converged`),
 * proceeding from the coherence hold (`coherence_hold`), a level move past the drive ceiling
 * (`drive_ceiling`) or off the table (`not_a_rung`), an unreadable reference
 * (`reference_unreadable`), a relative path. It sends `twr.live` to every connection and `twr.data`
 * to each subscribed one, as the service does. Every value is synthetic, made only so the shell has
 * something to draw. Nothing here is the service's preparation, identification or control.
 */

import type { DataSource, LogEntry, LogKind, ReadingRow, RefusalReason, RunRecordRef, Series, SpectrumView, StationRef, TableView, View } from '../types';
import type { LawEntry, LawParam } from '../mimo';
import type {
	TwrConfig,
	TwrDerived,
	TwrEnd,
	TwrFeasibilityCheck,
	TwrIdentificationFacts,
	TwrItemReason,
	TwrLive,
	TwrOutcome,
	TwrPhase,
	TwrPlayKind,
	TwrPlayRow,
	TwrPreviewDerived,
	TwrQuantity,
	TwrReferenceFacts,
	TwrReferenceFormat,
	TwrReportItem,
	TwrRunState,
	TwrState,
	TwrSubscribePlot,
	TwrTrip
} from '../twr';
import { TWR_POINTS_QUANTITIES } from '../twr';
import type { ShockMockHost } from './shock';
import { MockError, type MockConnection, type MockMethod } from './socket';

const G_IPS2 = 386.089;
const TICK_MS = 50;
const LIVE_MS = 200;
/** The simulated station answers about 2 g a volt in its band. */
const G_PER_V = 2;
/** The plays whose records are held for `twr.record`. */
const RECORDS_HELD = 8;
/** Mock seconds between plays on the automatic ladder (the law's call and the next arm). */
const GAP_S = 0.15;
/** Station seconds a stop's taper takes. */
const STOP_S = 0.25;
const MAX_RECORD = 1048576;
const LAG_FRAMES = 4;
/** The rest pad the station clocks behind each record, frames: a play's frames are the record and it. */
const REST_PAD = 1024;
/** The frames the device holds before a play starts, logged after each play. */
const COMMITTED = 256;
/** The fewest ticks a play takes, whatever the speed. */
const MIN_PLAY_TICKS = 3;

const QUANTITIES: TwrQuantity[] = ['target', 'response', 'error', 'drive', 'envelope', 'frf', 'coherence', 'mask', 'spectra', 'convergence', 'law', 'scope', 'readings', 'plays'];
const WITH_POINTS: readonly TwrQuantity[] = TWR_POINTS_QUANTITIES;

type TwrReason = RefusalReason | 'wrong_mode' | 'drive_ceiling' | 'reference_unreadable' | 'not_converged' | 'coherence_hold';

function refuse(code: 'refused' | 'bad_request', reason: TwrReason, message: string, detail?: Record<string, unknown>): never {
	throw new MockError(code, message, reason as RefusalReason, detail);
}

function bad(param: string, message: string): never {
	refuse('bad_request', 'bad_params', message, { param });
}

const round = (v: number, d = 3) => Math.round(v * 10 ** d) / 10 ** d;
const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const absolute = (p: unknown): p is string => typeof p === 'string' && /^(\/|[A-Za-z]:\\)/.test(p);
const f64 = (a: number[]) => Float64Array.from(a);
/** The frames the station clocks for one play: its record and the rest pad. */
const playFrames = (p: { recordLen: number }) => p.recordLen + REST_PAD;
const nextPow2 = (n: number) => 2 ** Math.ceil(Math.log2(Math.max(2, n)));
const linspace = (a: number, b: number, n: number) => Array.from({ length: n }, (_, i) => a + ((b - a) * i) / Math.max(1, n - 1));
const logspace = (a: number, b: number, n: number) => linspace(Math.log10(a), Math.log10(b), n).map((v) => 10 ** v);

function hash64(...parts: (string | number)[]): string {
	let h1 = 0x811c9dc5;
	let h2 = 0x01000193;
	for (const ch of parts.join('|')) {
		h1 = Math.imul(h1 ^ ch.charCodeAt(0), 16777619) >>> 0;
		h2 = Math.imul(h2 + ch.charCodeAt(0), 2246822519) >>> 0;
	}
	let out = '';
	for (let i = 0; out.length < 64; i++) out += (Math.imul(h1 ^ i, 2654435761) >>> 0).toString(16).padStart(8, '0') + (Math.imul(h2 + i, 1597334677) >>> 0).toString(16).padStart(8, '0');
	return out.slice(0, 64);
}

// ---------------------------------------------------------------------------------------------
// The law catalog

const builtIn = { status: 'built_in' as const, date: null, tool_version: null, cases: [], hardware: true, message: 'Built in: cleared for hardware.' };
const unchecked = { status: 'unchecked' as const, date: null, tool_version: null, cases: [], hardware: false, message: 'Not checked: runs on the simulator; check it before it drives hardware.' };

/** The built-in law, as the service lists it (its manifest, word for word). */
const TWR_CORRECT: LawEntry = {
	id: 'twr_correct',
	world: 'twr',
	builtin: true,
	name: 'frequency-domain correction',
	version: '0.1.0',
	author: 'Hertzforge contributors',
	summary: "Starts from the inverse of the identified plant applied to the reference record, then after each correction play corrects the drive spectrum by a fraction of the inverse times the error, on the bins the controller trusts. Each bin's growth is limited per correction. When the controller clipped a play's drive, the correction starts from the drive as played.",
	sha256: null,
	check: builtIn,
	params: [
		{ key: 'beta', label: 'Correction gain', help: "The fraction of each play's error corrected before the next play. 1 corrects the whole error at once; smaller values settle more gently when the plant estimate is uncertain.", kind: 'number', unit: '', min: 0.05, max: 1, step: 0.05, default: 0.5 },
		{ key: 'max_db_per_iter', label: 'Largest rise per correction', help: 'The most any one frequency of the drive may grow from one correction to the next. A frequency may always fall.', kind: 'number', unit: 'dB', min: 1, max: 9, step: 0.5, default: 6 },
		{ key: 'initial_backoff_db', label: 'First play backoff', help: 'How far under the predicted drive the first play starts. The correction brings the level up over the next plays.', kind: 'number', unit: 'dB', min: 0, max: 12, step: 1, default: 0 }
	],
	diagnostics: [
		{ key: 'corrected_bins', label: 'Corrected bins', unit: 'bins', help: 'Bins whose drive the law moved on this call.', layout: null },
		{ key: 'clamped_bins', label: 'Bins at the rise limit', unit: 'bins', help: 'Bins whose rise the law held to its largest rise per correction on this call.', layout: null },
		{ key: 'drive_change_db', label: 'Drive change', unit: 'dB', help: "The drive's peak against the last drive's, in dB (0 at the seed).", layout: null }
	],
	shapes: null,
	needs: null
};

function importedLaw(path: string): LawEntry {
	const sha = hash64('law', path);
	if (/misbehave/i.test(path))
		return {
			id: sha,
			world: 'twr',
			builtin: false,
			name: 'TWR misbehave',
			version: '0.1.0',
			author: 'law author',
			summary: 'A law that answers late on purpose after its seed, for seeing how the core stops a law that misses.',
			sha256: sha,
			check: unchecked,
			params: [],
			diagnostics: [],
			shapes: null,
			needs: null
		};
	return {
		id: sha,
		world: 'twr',
		builtin: false,
		name: 'TWR template',
		version: '0.1.0',
		author: 'law author',
		summary: 'The worked example: a plain correction of the drive by a share of the inverse times the error, with a deliberate model error for trying the check.',
		sha256: sha,
		check: unchecked,
		params: [
			{ key: 'beta', label: 'Correction share', help: 'The share of the correction applied after each play.', kind: 'number', unit: '', min: 0.05, max: 1, step: 0.05, default: 0.5 },
			{ key: 'correct', label: 'Correct after each play', help: 'Off, the seed replays unchanged.', kind: 'toggle', default: true },
			{ key: 'model_error_db', label: 'Model error', help: 'Scales the inverse the law uses, to try the check.', kind: 'number', unit: 'dB', min: -12, max: 12, step: 1, default: 0 }
		],
		diagnostics: [{ key: 'drive_change_db', label: 'Drive change', unit: 'dB', help: "The change of the drive's rms on the last call.", layout: null }],
		shapes: null,
		needs: null
	};
}

const numberParam = (p: LawParam): p is Extract<LawParam, { kind: 'number' | 'integer' }> => p.kind === 'number' || p.kind === 'integer';

// ---------------------------------------------------------------------------------------------
// References

interface StoredRef {
	facts: TwrReferenceFacts;
	/** The record's value at sample `i` (fractional between samples), in its unit. */
	at: (i: number) => number;
}

/** A field-like record: three decaying tones over a multi-sine floor, peaking at `peak`. */
function fieldLike(fs: number, n: number, peak: number, seed: number): (i: number) => number {
	const tones = [
		[250, 1, 0.6],
		[620, 0.55, 1.4],
		[1100, 0.35, 2.2]
	];
	const raw = (t: number) => {
		let v = 0;
		for (const [f, a, d] of tones) v += a * Math.exp(-d * t) * Math.sin(2 * Math.PI * f * t + seed * 0.01);
		v += 0.08 * Math.sin(2 * Math.PI * 150 * t) + 0.06 * Math.sin(2 * Math.PI * 320 * t + 1) + 0.05 * Math.sin(2 * Math.PI * 1500 * t + 2);
		return v;
	};
	let max = 1e-12;
	for (let k = 0; k < 4096; k++) max = Math.max(max, Math.abs(raw((k / 4096) * (n / fs))));
	const s = peak / max;
	return (i) => s * raw(i / fs);
}

function statsOf(at: (i: number) => number, n: number): { peak: number; rms: number } {
	const m = Math.min(n, 8192);
	let peak = 0;
	let sq = 0;
	for (let k = 0; k < m; k++) {
		const v = at((k * (n - 1)) / Math.max(1, m - 1));
		peak = Math.max(peak, Math.abs(v));
		sq += v * v;
	}
	return { peak, rms: Math.sqrt(sq / m) };
}

const TIME_AXIS = { label: 'Time', unit: 's', scale: 'linear' as const };
const FREQ_AXIS = { label: 'Frequency', unit: 'Hz', scale: 'log' as const };

/** A record against time, its y axis named as the service names it ("Acceleration" in the control unit). */
function timeView(title: string, yLabel: string, unit: string | null, series: Series[]): SpectrumView {
	return { view: 'spectrum', title, x: TIME_AXIS, y: { label: yLabel, unit, scale: 'linear' }, series, link: 'time' };
}

const line = (name: string, role: Series['role'], x: number[], y: number[]): Series => ({ name, role, x: f64(x), y: f64(y) });

// ---------------------------------------------------------------------------------------------
// The run

interface Prepared {
	fs: number;
	/** The prepared reference's length, samples at `fs`. */
	len: number;
	start: number;
	sectionLen: number;
	recordLen: number;
	pre: number;
	post: number;
	/** The full-level target at station sample `k` of the record. */
	target: (k: number) => number;
	peak: number;
	rms: number;
	resampled: boolean;
}

interface PlayRecord {
	row: TwrPlayRow;
	err: number;
	level: number;
}

interface Run {
	id: string;
	config: TwrConfig;
	derived: TwrDerived;
	views: View[];
	ref: StoredRef;
	law: LawEntry;
	params: Record<string, unknown>;
	state: TwrRunState;
	phase: TwrPhase;
	station: StationRef;
	source: DataSource;
	mode: 'auto' | 'manual';
	levels: number[];
	rung: number | null;
	level: number | null;
	frozen: boolean;
	alarm: boolean;
	/** Station time: the frames clocked over every play and pad, seconds. */
	t: number;
	armed: boolean;
	/** The present drive's error at full level, percent, as the next play would measure it. */
	err: number;
	atLevel: number;
	playback: number;
	done: number;
	/** The play in flight. */
	play: { index: number; kind: TwrPlayKind; iteration: number | null; t0: number; elapsed: number; done: number; ticks: number } | null;
	sysidT: number;
	gapT: number;
	stopT: number;
	stopTicks: number;
	stopOutcome: TwrOutcome;
	stopBy: string | undefined;
	/** Starting playback on the manual ladder: the schedule then runs to its end. */
	playingBack: boolean;
	/** A freeze asked for during a play, taking effect before the next one. */
	pendingFrozen: boolean | null;
	misses: number;
	/** The law call that made the drive in force, for the next play's row. */
	call: TwrPlayRow['law'];
	lastValues: Record<string, number> | null;
	calls: { index: number; values: Record<string, number> }[];
	identification: TwrIdentificationFacts | null;
	feasibility: TwrState['feasibility'];
	/** The run's record, from arm on. */
	record: RunRecordRef | null;
	records: PlayRecord[];
	rows: TwrPlayRow[];
	end: TwrEnd | null;
	timer: ReturnType<typeof setInterval> | null;
	/** When the run's clock last ticked, ms. */
	lastTick: number;
	/** Bumped whenever a plotted quantity changes: identification, a play, a law call, a level move. */
	version: number;
	/** The reference prepared on the run's record, once. */
	prep: Prepared;
}

export interface TwrMock {
	methods: Record<string, MockMethod>;
	/** The TWR world's laws, built in first. */
	laws(): LawEntry[];
	isLive(): boolean;
	/** The run's id while it is live, for another run's `run_active` refusal. */
	liveId(): string | null;
	detach(conn: MockConnection): void;
	/** Nobody held control for the timeout: the run safe-stops. */
	seatExpired(): void;
}

const LIVE: TwrRunState[] = ['armed', 'running', 'review', 'held', 'stopping'];

/** What the TWR mock needs of the control service around it: classical shock's. */
export type TwrMockHost = ShockMockHost;

type Item = TwrReportItem;
const item = (reason: TwrItemReason, message: string, field?: string): Item => (field ? { reason, message, field } : { reason, message });

export function createTwrMock(host: TwrMockHost): TwrMock {
	let run: Run | null = null;
	let runCount = 0;
	let log: LogEntry[] = [];
	let lastStateKey = '';
	let imported: LawEntry[] = [];
	const refs = new Map<string, StoredRef>();
	const subs = new Map<MockConnection, { rate: number; plots: TwrSubscribePlot[]; acc: number; sent: number }>();
	let liveTimer: ReturnType<typeof setInterval> | null = null;

	const laws = () => [TWR_CORRECT, ...imported];
	const isLive = () => run !== null && LIVE.includes(run.state);
	const controlRow = () => host.rows().find((r) => r.enabled && r.role === 'control') ?? null;
	const driveRow = () => host.rows().find((r) => r.enabled && r.role === 'drive') ?? null;
	const unit = () => controlRow()?.units ?? 'g';

	function addLog(kind: LogKind, text: string) {
		const e: LogEntry = { seq: log.length, at_unix: Date.now() / 1000, kind, text };
		log.push(e);
		if (run) host.broadcast('twr.log', { run_id: run.id, entries: [e] });
	}

	// ---- references ------------------------------------------------------------------------------

	function storeRef(facts: Omit<TwrReferenceFacts, 'peak' | 'rms'>, at: (i: number) => number): StoredRef {
		const { peak, rms } = statsOf(at, facts.samples);
		const stored: StoredRef = { facts: { ...facts, peak: round(peak, 6), rms: round(rms, 6) }, at };
		refs.set(facts.sha256, stored);
		return stored;
	}

	function referenceView(r: StoredRef, points = 2048): SpectrumView {
		const n = r.facts.samples;
		const m = Math.min(points, n);
		const idx = linspace(0, n - 1, m);
		return timeView('Reference', 'Reference', r.facts.units, [line('Reference', 'reference', idx.map((i) => i / r.facts.fs_hz), idx.map(r.at))]);
	}

	function parseText(text: string, fsHz: number | undefined, column: number | undefined): { values: number[]; fs: number } {
		const rows: number[][] = [];
		const lines = text.split(/\r?\n/);
		lines.forEach((raw, k) => {
			const l = raw.trim();
			if (!l || l.startsWith('#')) return;
			const cells = l.split(/\s*[\t,;]\s*|\s+/).filter((c) => c !== '');
			const nums = cells.map(Number);
			if (nums.some((v) => !Number.isFinite(v))) {
				if (rows.length === 0) return; // a header line
				refuse('refused', 'reference_unreadable', `Line ${k + 1} is not a row of numbers: "${l.slice(0, 40)}".`, { path: null, line: k + 1 });
			}
			rows.push(nums);
		});
		if (rows.length < 2) refuse('refused', 'reference_unreadable', 'The text holds fewer than two samples.', { path: null, line: null });
		const width = rows[0].length;
		// Without a rate the first column is time and the data columns follow it; with one, every column is data.
		const timed = fsHz === undefined;
		const col = (column ?? 0) + (timed ? 1 : 0);
		if (timed && width < 2) refuse('refused', 'reference_unreadable', 'The text has no time column; give the rate (fs_hz).', { path: null, line: null });
		if (col >= width) refuse('refused', 'reference_unreadable', `The text has ${timed ? width - 1 : width} data column(s); column ${column ?? 0} is not one of them.`, { path: null, line: null });
		let fs = fsHz;
		if (fs === undefined) {
			const steps = rows.slice(1).map((r, i) => r[0] - rows[i][0]).sort((a, b) => a - b);
			const median = steps[Math.floor(steps.length / 2)];
			if (!(median > 0) || steps.some((d) => Math.abs(d - median) > 0.01 * median)) refuse('refused', 'reference_unreadable', 'The time column is not evenly spaced within 1% of its median step.', { path: null, line: null });
			fs = round((rows.length - 1) / (rows[rows.length - 1][0] - rows[0][0]), 6);
		}
		return { values: rows.map((r) => r[col]), fs };
	}

	// ---- preparation, the grid and the checks ---------------------------------------------------------


	function prepare(r: StoredRef, c: Pick<TwrConfig, 'prep' | 'section' | 'pads' | 'acquisition'>): Prepared {
		const fs = c.acquisition?.fs_hz ?? r.facts.fs_hz;
		const ratio = fs / r.facts.fs_hz;
		const len = Math.round(r.facts.samples * ratio);
		const scale = c.prep?.scale ?? 1;
		const taper = c.prep?.taper_frac === undefined ? 0.02 : c.prep.taper_frac;
		const start = c.section?.start ?? 0;
		const sectionLen = c.section?.len ?? len;
		const pre = c.pads?.pre ?? 512;
		const post = c.pads?.post ?? 1024;
		const recordLen = nextPow2(sectionLen + pre + post);
		const prepped = (j: number) => {
			let w = 1;
			if (taper) {
				const edge = taper * len;
				if (j < edge) w = 0.5 - 0.5 * Math.cos((Math.PI * j) / edge);
				else if (j > len - edge) w = 0.5 - 0.5 * Math.cos((Math.PI * (len - j)) / edge);
			}
			return scale * w * r.at(j / ratio);
		};
		const target = (k: number) => {
			const j = k - pre;
			return j < 0 || j >= sectionLen ? 0 : prepped(start + j);
		};
		const s = statsOf((i) => prepped(start + i), sectionLen);
		return { fs, len, start, sectionLen, recordLen, pre, post, target, peak: s.peak, rms: s.rms, resampled: Math.abs(ratio - 1) > 1e-12 };
	}

	function previewDerived(r: StoredRef, c: Pick<TwrConfig, 'prep' | 'section' | 'pads' | 'acquisition' | 'correction' | 'feasibility'>, p: Prepared): TwrPreviewDerived {
		const hp = c.feasibility?.kinematics_hp_hz ?? 5;
		const fDom = 250;
		const velocity = (p.peak * G_IPS2) / (2 * Math.PI * fDom);
		const disp = (2 * velocity) / (2 * Math.PI * fDom);
		const band = c.correction?.band_hz ?? [20, 2000];
		const floor = c.correction?.ref_floor_db ?? 50;
		const inBand = band[0] <= 150 && band[1] >= 1100 ? 0.9996 : band[1] >= 620 ? 0.93 : 0.62;
		const fraction = round(Math.min(0.9999, inBand - Math.max(0, 40 - floor) * 0.002), 4);
		const checks: TwrFeasibilityCheck[] = [];
		const f = c.feasibility ?? {};
		if (isNum(f.peak_accel)) checks.push({ name: 'peak_accel', value: round(p.peak, 6), limit: f.peak_accel, pass: p.peak <= f.peak_accel });
		if (isNum(f.peak_velocity_ips)) checks.push({ name: 'peak_velocity_ips', value: round(velocity, 6), limit: f.peak_velocity_ips, pass: velocity <= f.peak_velocity_ips });
		if (isNum(f.disp_pkpk_in)) checks.push({ name: 'disp_pkpk_in', value: round(disp, 8), limit: f.disp_pkpk_in, pass: disp <= f.disp_pkpk_in });
		const exceeded = checks.some((x) => !x.pass) || (host.feasibilityExceeds && checks.length > 0);
		return {
			grid: { fs_hz: p.fs, record_len: p.recordLen, df_hz: p.fs / p.recordLen, record_s: p.recordLen / p.fs, section_start: p.pre, section_len: p.sectionLen, actual: false },
			section: { peak: round(p.peak, 6), rms: round(p.rms, 6), duration_s: round(p.sectionLen / p.fs, 6) },
			kinematics: { peak_accel: round(p.peak, 6), peak_velocity_ips: round(velocity, 6), disp_pkpk_in: round(disp, 8), kinematics_hp_hz: hp },
			coverage: { band_hz: [band[0], band[1]], ref_floor_db: floor, fraction, error_floor_pct: round(100 * Math.sqrt(1 - fraction), 3) },
			feasibility: {
				verdict: checks.length === 0 ? 'not_evaluated' : exceeded ? 'exceeded' : 'pass',
				message: checks.length === 0 ? 'No feasibility ceiling was set, so the kinematics were not checked.' : exceeded ? 'The section at full level is over a kinematic ceiling.' : 'The section at full level is within its kinematic ceilings.',
				checks
			}
		};
	}

	function previewViews(p: Prepared, r: StoredRef, c: Pick<TwrConfig, 'correction'>): View[] {
		const m = 1024;
		const ks = linspace(0, p.recordLen - 1, m);
		const ts = ks.map((k) => k / p.fs);
		const section = timeView('Section', 'Reference', r.facts.units, [line('Reference', 'reference', ts, ks.map(p.target))]);
		// The spectrum on the record grid from 0 to half the rate, the reference floor across the correction band.
		const f = linspace(0, p.fs / 2, m);
		const band = c.correction?.band_hz ?? [20, 2000];
		const bump = (x: number, c0: number, w: number) => Math.exp(-(((x - c0) / w) ** 2));
		const spec = f.map((x) => p.rms * (bump(x, 250, 60) + 0.6 * bump(x, 620, 90) + 0.3 * bump(x, 1100, 120) + 1e-4));
		const floor = Math.max(...spec) * 10 ** (-(c.correction?.ref_floor_db ?? 50) / 20);
		const spectrum: SpectrumView = {
			view: 'spectrum',
			title: 'Spectrum',
			x: { label: 'Frequency', unit: 'Hz', scale: 'linear' },
			y: { label: 'Magnitude', unit: r.facts.units, scale: 'log' },
			series: [line('Reference', 'reference', f, spec), line('Reference floor', 'tolerance', [band[0], band[1]], [floor, floor])],
			link: 'frequency'
		};
		const w = 2 * Math.PI * 250;
		const velocity = timeView('Velocity', 'Velocity', 'in/s', [line('Velocity', 'reference', ts, ks.map((k) => (-p.target(k + p.fs / 1000) * G_IPS2) / w))]);
		const displacement = timeView('Displacement', 'Displacement', 'in', [line('Displacement', 'reference', ts, ks.map((k) => (-p.target(k) * G_IPS2) / (w * w)))]);
		return [section, spectrum, velocity, displacement];
	}

	/** What a preparation would be refused, each at its field (the preview's `config_invalid` items). */
	function preparationErrors(r: StoredRef, c: Pick<TwrConfig, 'prep' | 'section' | 'pads' | 'acquisition' | 'correction'>): Item[] {
		const out: Item[] = [];
		const fs = c.acquisition?.fs_hz ?? r.facts.fs_hz;
		if (fs < r.facts.fs_hz - 1e-9 || (Math.abs(fs - r.facts.fs_hz) > 1e-9 && c.prep?.auto_resample === false)) out.push(item('rate_mismatch', `The reference is at ${r.facts.fs_hz} Hz and the station is asked for ${fs} Hz.`, 'acquisition.fs_hz'));
		const taper = c.prep?.taper_frac;
		if (taper !== undefined && taper !== null && !(taper > 0 && taper <= 0.5)) out.push(item('out_of_range', `The taper is ${taper}; it is above 0 and at most 0.5.`, 'prep.taper_frac'));
		const band = c.correction?.band_hz ?? [20, 2000];
		if (band[1] >= fs / 2) out.push(item('band_above_grid', `The correction band reaches ${band[1]} Hz, at or above half the rate.`, 'correction.band_hz'));
		const hp = c.prep?.highpass_hz;
		if (isNum(hp) && hp > band[0]) out.push(item('highpass_above_band', `The high-pass at ${hp} Hz is above the correction band's low edge, ${band[0]} Hz.`, 'prep.highpass_hz'));
		if (out.length) return out;
		const p = prepare(r, c);
		if (p.start + p.sectionLen > p.len || p.start < 0 || p.sectionLen < 1) out.push(item('section_outside_reference', `The section ${p.start} + ${p.sectionLen} leaves the prepared reference of ${p.len} samples.`, 'section'));
		if (p.recordLen > MAX_RECORD) out.push(item('record_too_long', `The record would be ${p.recordLen} samples, over the ${MAX_RECORD} the service plays.`, 'pads'));
		return out;
	}

	function lawOf(c: TwrConfig): LawEntry | null {
		return laws().find((l) => l.id === (c.control?.law?.id ?? 'twr_correct')) ?? null;
	}

	const playsTarget = (c: TwrConfig, p: Prepared) => {
		const r = c.run ?? { schedule: 'once' as const };
		if (r.schedule === 'loop') return r.count;
		if (r.schedule === 'duration') return Math.max(1, Math.ceil(r.duration_s / (p.sectionLen / p.fs)));
		return 1;
	};

	function check(config: TwrConfig): { errors: Item[]; warnings: Item[]; derived: TwrDerived | null; views: View[] } {
		const errors: Item[] = [];
		const warnings: Item[] = [];
		const range = (field: string, v: unknown, lo: number, hi: number, open: '[]' | '(]' | '()' | '[)' = '[]') => {
			if (v === undefined || v === null) return;
			const x = v as number;
			const ok = isNum(x) && (open[0] === '[' ? x >= lo : x > lo) && (open[1] === ']' ? x <= hi : x < hi);
			if (!ok) errors.push(item('out_of_range', `${field} is ${String(v)}; it is ${open[0] === '[' ? 'at least' : 'above'} ${lo} and ${open[1] === ']' ? 'at most' : 'below'} ${hi}.`, `config.${field}`));
		};
		const ref = refs.get(config.reference.sha256);
		if (!ref) errors.push(item('reference_unknown', 'No stored reference has that SHA-256; import the reference again.', 'config.reference.sha256'));
		else if (config.reference.path && /changed/i.test(config.reference.path)) errors.push(item('reference_changed', `The file at ${config.reference.path} no longer has the SHA-256 it was imported with.`, 'config.reference.path'));
		if (ref && ref.facts.units !== unit()) errors.push(item('units_mismatch', `The reference is in ${ref.facts.units}; the control row is in ${unit()}.`, 'config.reference'));
		const ctl = config.control ?? {};
		const lim = config.limits;
		const id = config.identification ?? {};
		const idBand = id.band_hz ?? [20, 2000];
		const corrBand = config.correction?.band_hz ?? idBand;
		range('limits.drive_vpk_ceiling', lim.drive_vpk_ceiling, 0, 10, '(]');
		range('control.law_budget', ctl.law_budget, 0, 1, '(]');
		range('control.law_misses', ctl.law_misses, 1, 100);
		range('control.max_peak_rise_db', ctl.max_peak_rise_db, 0, 40, '(]');
		range('control.max_bin_rise_db', ctl.max_bin_rise_db, 0, 40, '(]');
		range('control.first_drive_ratio', ctl.first_drive_ratio, 1, 10);
		range('control.alignment_frames', ctl.alignment_frames, 0, 16, '(]');
		range('schedule.gate_pct', config.schedule?.gate_pct, 0, 100, '(]');
		range('schedule.max_iterations', config.schedule?.max_iterations, 1, 100);
		range('inverse.tikhonov_floor', config.inverse?.tikhonov_floor, 0, 1, '()');
		range('inverse.coherence_floor', config.inverse?.coherence_floor, 0, 1);
		range('correction.min_band_energy', config.correction?.min_band_energy, 0, 1, '()');
		range('prep.taper_frac', config.prep?.taper_frac, 0, 0.5, '(]');
		range('limits.envelope_debounce_samples', lim.envelope_debounce_samples, 1, 10000);
		range('identification.min_mean_coherence', id.min_mean_coherence, 0, 1);
		if (config.run?.schedule === 'loop') range('run.count', config.run.count, 1, 100000);
		if (config.run?.schedule === 'duration') range('run.duration_s', config.run.duration_s, 0, 86400, '(]');
		const levels = config.schedule?.levels_db ?? [-12, -9, -6, -3, 0];
		if (!levels.length) errors.push(item('levels_empty', 'The level table has no level.', 'config.schedule.levels_db'));
		levels.forEach((l, i) => range(`schedule.levels_db[${i}]`, l, -60, 12));
		if (levels.some((l, i) => i > 0 && l < levels[i - 1])) errors.push(item('levels_not_ascending', 'The levels do not fall from one to the next.', 'config.schedule.levels_db'));
		if ((lim.envelope_alarm_frac ?? 0.25) > (lim.envelope_abort_frac ?? 0.5)) errors.push(item('alarm_wider_than_abort', 'The alarm band is wider than the abort band.', 'config.limits.envelope_alarm_frac'));
		if (corrBand[0] < idBand[0] - 1e-9 || corrBand[1] > idBand[1] + 1e-9) errors.push(item('band_outside_identification', `The correction band ${corrBand[0]} to ${corrBand[1]} Hz leaves the identification band ${idBand[0]} to ${idBand[1]} Hz.`, 'config.correction.band_hz'));
		const hp = config.prep?.highpass_hz;
		if (isNum(hp) && hp > corrBand[0]) errors.push(item('highpass_above_band', `The high-pass at ${hp} Hz is above the correction band's low edge, ${corrBand[0]} Hz.`, 'config.prep.highpass_hz'));
		if (!controlRow()) errors.push(item('no_control_channel', 'The channel table has no enabled control row.'));
		if (!driveRow()) errors.push(item('no_drive_channel', 'The channel table has no enabled drive row.'));
		const law = lawOf(config);
		if (!law) errors.push(item('law_unknown', 'The law is not in the catalog.', 'config.control.law.id'));
		else {
			const params = (config.control?.law?.params ?? {}) as Record<string, unknown>;
			for (const [k, v] of Object.entries(params)) {
				const p = law.params.find((x) => x.key === k);
				if (!p) errors.push(item('law_param_unknown', `The law has no parameter "${k}".`, `config.control.law.params.${k}`));
				else if (numberParam(p) && !(isNum(v) && v >= p.min && v <= p.max)) errors.push(item('law_param_invalid', `${p.label} is from ${p.min} to ${p.max}.`, `config.control.law.params.${k}`));
			}
			if (law.id === 'twr_correct') {
				const clamp = isNum(params.max_db_per_iter) ? params.max_db_per_iter : 6;
				for (const k of ['max_peak_rise_db', 'max_bin_rise_db'] as const) {
					const v = ctl[k] ?? 10;
					if (isNum(v) && v < clamp + 1) errors.push(item('out_of_range', `With this law the cap is at least its growth limit plus 1 dB, ${clamp + 1} dB.`, `config.control.${k}`));
				}
			}
		}
		if (!ref) return { errors, warnings, derived: null, views: [] };
		const fs = config.acquisition?.fs_hz ?? ref.facts.fs_hz;
		if (fs < ref.facts.fs_hz - 1e-9) errors.push(item('rate_mismatch', `The reference is at ${ref.facts.fs_hz} Hz, above the rate asked of the station, ${fs} Hz.`, 'config.acquisition.fs_hz'));
		else if (Math.abs(fs - ref.facts.fs_hz) > 1e-9) {
			if (config.prep?.auto_resample === false) errors.push(item('rate_mismatch', `The reference is at ${ref.facts.fs_hz} Hz and the station at ${fs} Hz; resampling is off.`, 'config.acquisition.fs_hz'));
			else warnings.push(item('reference_resampled', `The reference is resampled from ${ref.facts.fs_hz} Hz to ${fs} Hz.`, 'config.acquisition.fs_hz'));
		}
		if (idBand[1] >= fs / 2) errors.push(item('band_above_grid', `The identification band reaches ${idBand[1]} Hz, at or above half the rate.`, 'config.identification.band_hz'));
		const p = prepare(ref, config);
		if (p.start + p.sectionLen > p.len || p.start < 0 || p.sectionLen < 1) errors.push(item('section_outside_reference', `The section ${p.start} + ${p.sectionLen} leaves the prepared reference of ${p.len} samples.`, 'config.section'));
		if (p.recordLen > MAX_RECORD) errors.push(item('record_too_long', `The record would be ${p.recordLen} samples, over the ${MAX_RECORD} the service plays.`, 'config.pads'));
		if ((id.duration_s ?? 4) * fs < 2 * (id.nfft ?? 2048)) errors.push(item('identification_too_short', 'Identification is shorter than two of its blocks.', 'config.identification.duration_s'));
		const base = previewDerived(ref, config, p);
		const recordS = p.recordLen / p.fs;
		const derived: TwrDerived = {
			...base,
			reference: { sha256: ref.facts.sha256, name: ref.facts.name, fs_hz: ref.facts.fs_hz, units: ref.facts.units, resampled: p.resampled },
			call_budget_s: round((ctl.law_budget ?? 0.25) * Math.max(recordS, 4), 6),
			plays_target: playsTarget(config, p),
			identification_s: id.duration_s ?? 4
		};
		return { errors, warnings, derived, views: previewViews(p, ref, config) };
	}

	// ---- the state ---------------------------------------------------------------------------------

	function emptyState(): TwrState {
		return {
			run_id: null,
			state: 'unconfigured',
			phase: 'idle',
			title: null,
			source: null,
			station: null,
			grid: null,
			clock: null,
			mode: null,
			levels_db: null,
			level_db: null,
			rung: null,
			plays: { done: 0, at_level: 0, of_level: 0, playback: 0, of_playback: 0 },
			frozen: false,
			t_s: null,
			alarm: false,
			identification: null,
			prediction: null,
			last_play: null,
			law: null,
			feasibility: null,
			end: null
		};
	}

	const predictedFull = (r: Run) => (r.derived.section.peak / G_PER_V) * (1 + r.err / 400);
	const driveAt = (r: Run, level: number) => predictedFull(r) * 10 ** (level / 20);
	const ceiling = (r: Run) => r.config.limits.drive_vpk_ceiling;
	const ofLevel = (r: Run) => r.config.schedule?.max_iterations ?? 8;
	const gate = (r: Run) => r.config.schedule?.gate_pct ?? 10;
	const fsOf = (r: Run) => r.derived.grid.fs_hz;
	/** A play's length in station time: its record and the rest pad. */
	const recordS = (r: Run) => (r.derived.grid.record_len + REST_PAD) / fsOf(r);

	function playsOf(r: Run) {
		return { done: r.done, at_level: r.atLevel, of_level: ofLevel(r), playback: r.playback, of_playback: r.derived.plays_target };
	}

	function stateOf(r: Run | null): TwrState {
		if (!r) return emptyState();
		return {
			run_id: r.id,
			state: r.state,
			phase: r.phase,
			title: r.config.title ?? null,
			source: r.source,
			station: r.station,
			grid: r.derived.grid,
			clock: r.armed ? 'shared' : null,
			mode: r.mode,
			levels_db: r.levels,
			level_db: r.level,
			rung: r.rung,
			plays: playsOf(r),
			frozen: r.frozen,
			t_s: r.armed ? round(r.t, 3) : null,
			alarm: r.alarm,
			identification: r.identification,
			prediction: r.identification?.passed && r.level !== null ? { drive_vpk: round(driveAt(r, r.level), 5), drive_vpk_ceiling: ceiling(r), level_db: r.level } : null,
			last_play: r.rows.at(-1) ?? null,
			law: { id: r.law.id, name: r.law.name, version: r.law.version, sha256: r.law.sha256, misses: r.misses },
			feasibility: r.feasibility,
			end: r.end,
			...(r.record ? { record: { ...r.record } } : {})
		};
	}

	function stateChanged(force = false) {
		const s = stateOf(run);
		const key = JSON.stringify([s.run_id, s.state, s.phase, s.mode, s.level_db, s.rung, s.plays, s.frozen, s.alarm, s.last_play?.index ?? null, s.law?.misses ?? null, s.identification, s.end]);
		if (!force && key === lastStateKey) return;
		lastStateKey = key;
		host.broadcast('twr.state', s as unknown as Record<string, unknown>);
	}

	function setPhase(r: Run, state: TwrRunState, phase: TwrPhase) {
		r.state = state;
		r.phase = phase;
		stateChanged();
	}

	// ---- the display ---------------------------------------------------------------------------------

	const ref = (r: Run) => r.prep;

	/** The record's samples at `points` places across it, at the present level. */
	function recordAt(r: Run, points: number, fn: (k: number, target: number) => number) {
		const p = ref(r);
		const ks = linspace(0, p.recordLen - 1, Math.min(points, p.recordLen));
		const s = 10 ** ((r.level ?? 0) / 20);
		return { x: ks.map((k) => k / p.fs), y: ks.map((k) => fn(k, s * p.target(k))), p };
	}

	const ripple = (k: number, fs: number) => Math.sin((2 * Math.PI * 37 * k) / fs + 0.3) + 0.4 * Math.sin((2 * Math.PI * 413 * k) / fs);

	function responseOf(err: number, rmsAtLevel: number) {
		return (k: number, target: number, fs: number) => target + (err / 100) * rmsAtLevel * ripple(k, fs);
	}

	function recordViews(r: Run, rec: PlayRecord, points: number): View[] {
		const p = ref(r);
		const s = 10 ** (rec.level / 20);
		const n = points === 0 ? Math.min(p.recordLen, 65536) : points;
		const ks = linspace(0, p.recordLen - 1, Math.min(n, p.recordLen));
		const t = ks.map((k) => k / p.fs);
		const target = ks.map((k) => s * p.target(k));
		const resp = responseOf(rec.err, s * p.rms);
		const y = ks.map((k, i) => resp(k, target[i], p.fs));
		return [
			timeView('Replication', 'Acceleration', unit(), [line('Response', 'measured', t, y), line('Target', 'reference', t, target)]),
			timeView('Error', 'Error', unit(), [line('Error', 'measured', t, y.map((v, i) => v - target[i]))]),
			timeView('Drive', 'Drive', 'V', [line('Drive', 'drive', t, target.map((v) => v / G_PER_V))])
		];
	}

	function lastRecord(r: Run): PlayRecord | null {
		return r.records.at(-1) ?? null;
	}

	function identificationViews(r: Run): { frf: View[]; coherence: View[]; mask: View[] } {
		const id = r.config.identification ?? {};
		const band = id.band_hz ?? [20, 2000];
		const f = logspace(band[0], band[1], 256);
		const mode = (x: number, fn: number, q: number) => 1 / Math.sqrt((1 - (x / fn) ** 2) ** 2 + (x / (fn * q)) ** 2);
		const gain = f.map((x) => G_PER_V * (0.6 * mode(x, 200, 2.5) + 0.4 * mode(x, 550, 2.5)));
		const phase = f.map((x) => (-180 / Math.PI) * Math.atan2(x / (200 * 2.5), 1 - (x / 200) ** 2));
		const coh = f.map((x) => (r.identification?.coherence_mean ?? 0.5) * (1 - 0.04 * Math.exp(-(((x - 300) / 40) ** 2))));
		const cohFloor = r.config.inverse?.coherence_floor ?? 0.9;
		const minMean = id.min_mean_coherence ?? 0.8;
		const corr = r.config.correction?.band_hz ?? band;
		const fm = logspace(Math.max(1, corr[0] / 2), Math.min(fsOf(r) / 2.2, corr[1] * 2), 256);
		const refSpec = fm.map((x) => Math.exp(-(((Math.log10(x) - Math.log10(250)) / 0.25) ** 2)) + 0.6 * Math.exp(-(((Math.log10(x) - Math.log10(620)) / 0.2) ** 2)) + 0.03);
		const top = Math.max(...refSpec);
		const trusted = fm.map((x, i) => (x >= corr[0] && x <= corr[1] && refSpec[i] / top > 10 ** (-(r.config.correction?.ref_floor_db ?? 50) / 20) ? 1 : 0));
		const spec = (title: string, y: { label: string; unit: string | null; scale: 'linear' | 'log' }, series: Series[]): SpectrumView => ({ view: 'spectrum', title, x: FREQ_AXIS, y, series, link: 'frequency' });
		return {
			frf: [spec('Identification Gain', { label: 'Gain', unit: `${unit()}/V`, scale: 'log' }, [line('Gain', 'measured', f, gain)]), spec('Identification Phase', { label: 'Phase', unit: 'deg', scale: 'linear' }, [line('Phase', 'measured', f, phase)])],
			coherence: [spec('Coherence', { label: 'Coherence', unit: null, scale: 'linear' }, [line('Coherence', 'measured', f, coh), line('Trust floor', 'tolerance', [f[0], f[f.length - 1]], [cohFloor, cohFloor]), line('Hold', 'abort', [f[0], f[f.length - 1]], [minMean, minMean])])],
			mask: [spec('Trust', { label: 'Trusted', unit: null, scale: 'linear' }, [line('Trusted', 'measured', fm, trusted), line('Reference', 'reference', fm, refSpec.map((v) => v / top))])]
		};
	}

	/** The run's numbers, each row only once there is a number for it. */
	function readingsView(r: Run): View {
		const last = r.rows.at(-1);
		const s = stateOf(r);
		const rows: ReadingRow[] = [];
		if (r.level !== null) rows.push({ label: 'Level', value: round(r.level, 2), unit: 'dB', status: 'info' });
		rows.push({ label: 'Plays', value: r.done, unit: null, status: 'info' });
		if (r.derived.plays_target > 0 && r.playback > 0) rows.push({ label: 'Playback', value: `${r.playback} of ${r.derived.plays_target}`, unit: null, status: 'info' });
		if (last) {
			rows.push({ label: 'Last error', value: round(last.rms_error_pct, 3), unit: '%', status: last.gated ? 'pass' : 'info' });
			rows.push({ label: 'Gate', value: gate(r), unit: '%', status: 'info' });
			rows.push({ label: 'Last drive peak', value: round(last.drive_vpk, 4), unit: 'V', status: last.drive_vpk > ceiling(r) ? 'fail' : 'info' });
			rows.push({ label: 'Last response peak', value: round(last.response_peak, 4), unit: unit(), status: 'info' });
		}
		if (s.prediction) rows.push({ label: 'Drive in force', value: round(s.prediction.drive_vpk, 4), unit: 'V', status: s.prediction.drive_vpk > s.prediction.drive_vpk_ceiling ? 'fail' : 'info' });
		if (s.t_s !== null) rows.push({ label: 'Station time', value: round(s.t_s, 3), unit: 's', status: 'info' });
		return { view: 'reading', title: 'Run', rows };
	}

	function lawViews(r: Run): View[] {
		if (!r.calls.length) return [];
		const keys = r.law.diagnostics.map((d) => d.key);
		const series = keys.map((k) => {
			const pts = r.calls.filter((c) => k in c.values);
			return line(r.law.diagnostics.find((d) => d.key === k)?.label ?? k, 'measured', pts.map((c) => c.index + 1), pts.map((c) => c.values[k]));
		});
		// The last used call's values, by their labels, and the unusable calls in a row.
		const values = r.lastValues ?? {};
		const rows: ReadingRow[] = [
			...r.law.diagnostics.filter((d) => values[d.key] !== undefined).map((d): ReadingRow => ({ label: d.label, value: round(values[d.key], 6), unit: d.unit || null, status: 'info' })),
			{ label: 'Unusable calls in a row', value: r.misses, unit: null, status: r.misses ? 'warn' : 'pass' }
		];
		return [{ view: 'spectrum', title: 'Law', x: { label: 'Play', unit: null, scale: 'linear' }, y: { label: 'Value', unit: null, scale: 'linear' }, series }, { view: 'reading', title: 'Law Values', rows }];
	}

	function playsTable(r: Run): TableView {
		return {
			view: 'table',
			title: 'Plays',
			columns: ['Play', 'Kind', 'Level (dB)', 'Iteration', 'Error (%)', 'Peak error (%)', 'Spectral error (dB)', 'Gated', 'Drive peak (V)', 'Clipped samples', 'Clamped bins', 'Lag (frames)'],
			rows: r.rows.map((p) => [
				String(p.index + 1),
				p.kind,
				String(round(p.level_db, 2)),
				p.iteration === null ? '' : String(p.iteration),
				String(round(p.rms_error_pct, 2)),
				String(round(p.peak_error_pct, 2)),
				String(round(p.spectral_error_db, 2)),
				p.gated ? 'yes' : 'no',
				String(round(p.drive_vpk, 4)),
				String(p.clipped_samples),
				String(p.clamped_bins),
				p.lag_frames === null ? '' : String(round(p.lag_frames, 2))
			])
		};
	}

	function scopeView(r: Run, points = 1024): SpectrumView {
		const p = ref(r);
		const empty = timeView('Scope', 'Acceleration', unit(), []);
		if (!r.play) return empty;
		const n = Math.min(points, 1024);
		const end = Math.max(1, Math.round(r.play.done * p.recordLen));
		const startK = Math.max(0, end - Math.round(p.fs * 0.08));
		const ks = linspace(startK, end, n);
		const s = 10 ** ((r.level ?? 0) / 20);
		const resp = responseOf(r.err, s * p.rms);
		return timeView('Scope', 'Acceleration', unit(), [line('Control', 'measured', ks.map((k) => (k - startK) / p.fs), ks.map((k) => resp(k, s * p.target(k), p.fs)))]);
	}

	function plotViews(r: Run, pl: TwrSubscribePlot): View[] {
		const points = pl.points ?? 1024;
		const rec = lastRecord(r);
		const identified = r.identification !== null;
		switch (pl.quantity) {
			case 'target': {
				if (r.level === null) return [];
				const { x, y } = recordAt(r, points, (_k, target) => target);
				return [timeView('Target', 'Acceleration', unit(), [line('Target', 'reference', x, y)])];
			}
			case 'response':
				return rec ? [{ ...(recordViews(r, rec, points)[0] as SpectrumView), title: 'Response' }] : [];
			case 'error':
				return rec ? [recordViews(r, rec, points)[1]] : [];
			case 'drive':
				return rec ? [recordViews(r, rec, points)[2]] : [];
			case 'envelope': {
				if (!rec) return [];
				const v = recordViews(r, rec, points)[0] as SpectrumView;
				const [resp, target] = v.series;
				const pk = Math.max(...Array.from(target.y as Float64Array, Math.abs));
				const alarm = r.config.limits.envelope_alarm_frac ?? 0.25;
				const abort = r.config.limits.envelope_abort_frac ?? 0.5;
				const x = Array.from(target.x as Float64Array);
				const t = Array.from(target.y as Float64Array);
				return [
					timeView('Envelope', 'Acceleration', unit(), [
						{ ...resp, name: 'Response' },
						line('Alarm high', 'tolerance', x, t.map((v) => v + alarm * pk)),
						line('Alarm low', 'tolerance', x, t.map((v) => v - alarm * pk)),
						line('Abort high', 'abort', x, t.map((v) => v + abort * pk)),
						line('Abort low', 'abort', x, t.map((v) => v - abort * pk))
					])
				];
			}
			case 'frf':
				return identified ? identificationViews(r).frf : [];
			case 'coherence':
				return identified ? identificationViews(r).coherence : [];
			case 'mask':
				return identified ? identificationViews(r).mask : [];
			case 'spectra': {
				if (!rec) return [];
				const corr = r.config.correction?.band_hz ?? [20, 2000];
				const f = logspace(corr[0], corr[1], 256);
				const s = 10 ** ((r.level ?? 0) / 20);
				const R = f.map((x) => s * (Math.exp(-(((Math.log10(x) - Math.log10(250)) / 0.25) ** 2)) + 0.6 * Math.exp(-(((Math.log10(x) - Math.log10(620)) / 0.2) ** 2)) + 0.03));
				const Y = R.map((v, i) => v * (1 + (rec.err / 100) * Math.sin(i / 9)));
				const D = R.map((v, i) => v / (G_PER_V * (0.6 + 0.4 * Math.cos(i / 40))));
				return [{ view: 'spectrum', title: 'Spectra', x: FREQ_AXIS, y: { label: 'Magnitude', unit: unit(), scale: 'log' }, series: [line('Target', 'reference', f, R), line('Response', 'measured', f, Y), line('Drive', 'drive', f, D)], link: 'frequency' }];
			}
			case 'convergence': {
				if (!r.rows.length) return [];
				const x = r.rows.map((p) => p.index + 1);
				return [
					{
						view: 'spectrum',
						title: 'Convergence',
						x: { label: 'Play', unit: null, scale: 'linear' },
						y: { label: 'Error', unit: '%', scale: 'log' },
						series: [line('Error', 'measured', x, r.rows.map((p) => p.rms_error_pct)), line('Gate', 'tolerance', [x[0], x[x.length - 1]], [gate(r), gate(r)])]
					}
				];
			}
			case 'law':
				return lawViews(r);
			case 'scope':
				return r.play ? [scopeView(r, points)] : [];
			case 'readings':
				return [readingsView(r)];
			case 'plays':
				return [playsTable(r)];
		}
	}

	function head(r: Run): Omit<TwrLive, 'views'> {
		const last = r.rows.at(-1);
		const p = ref(r);
		return {
			run_id: r.id,
			source: r.source,
			state: r.state,
			phase: r.phase,
			mode: r.mode,
			t_s: round(r.t, 3),
			level_db: r.level,
			rung: r.rung,
			plays: playsOf(r),
			play: r.play ? { index: r.play.index, kind: r.play.kind, iteration: r.play.iteration, frames_done: Math.min(playFrames(p), Math.round(r.play.done * playFrames(p))), frames: playFrames(p) } : null,
			last: last ? { rms_error_pct: last.rms_error_pct, peak_error_pct: last.peak_error_pct, spectral_error_db: last.spectral_error_db, gated: last.gated } : null
		};
	}

	const liveOf = (r: Run): TwrLive => ({ ...head(r), views: [scopeView(r)] });
	const dataFor = (r: Run, plots: TwrSubscribePlot[]) => ({ ...head(r), plots: plots.map((pl) => ({ id: pl.id, quantity: pl.quantity, views: plotViews(r, pl) })) });

	/**
	 * As the service sends them: `twr.live` five times a second from arm until the run has finished
	 * (and once more) to every connection without a TWR subscription of its own; to a subscribed
	 * connection `twr.data` in its place, at its rate while a play is in flight whatever its plots
	 * show, and once when identification ends, once after each play and once when the run has
	 * finished (and at once on subscribe).
	 */
	function publish(r: Run) {
		const final = r.state === 'finished';
		let live: Record<string, unknown> | null = null;
		for (const conn of host.connections()) {
			const sub = subs.get(conn);
			if (!sub) {
				live ??= liveOf(r) as unknown as Record<string, unknown>;
				conn.emit('twr.live', live);
				continue;
			}
			sub.acc += LIVE_MS / 1000;
			const due = sub.sent !== r.version || final || (!!r.play && sub.acc + 1e-9 >= 1 / sub.rate);
			if (due && (sub.sent !== r.version || sub.acc + 1e-9 >= 1 / sub.rate || final)) {
				sub.acc = 0;
				sub.sent = r.version;
				conn.emit('twr.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
			}
		}
	}

	function startLive() {
		if (liveTimer) return;
		liveTimer = setInterval(() => {
			if (!run || !LIVE.includes(run.state)) {
				if (liveTimer) clearInterval(liveTimer);
				liveTimer = null;
				return;
			}
			publish(run);
		}, LIVE_MS);
	}

	// ---- the run's course ----------------------------------------------------------------------------

	function newRun(config: TwrConfig, derived: TwrDerived, views: View[]): Run {
		const st = host.station();
		const law = lawOf(config) ?? TWR_CORRECT;
		const params: Record<string, unknown> = {};
		for (const p of law.params) params[p.key] = (config.control?.law?.params ?? {})[p.key] ?? (p as { default: unknown }).default;
		const levels = config.schedule?.levels_db ?? [-12, -9, -6, -3, 0];
		return {
			id: `twr-${String(++runCount).padStart(4, '0')}`,
			config,
			derived,
			views,
			ref: refs.get(config.reference.sha256)!,
			law,
			params,
			state: 'configured',
			phase: 'idle',
			station: st.ref,
			source: st.source,
			mode: config.schedule?.mode ?? 'auto',
			levels,
			rung: null,
			level: null,
			frozen: false,
			alarm: false,
			t: 0,
			armed: false,
			err: 0,
			atLevel: 0,
			playback: 0,
			done: 0,
			play: null,
			sysidT: 0,
			gapT: 0,
			stopT: 0,
			stopTicks: 0,
			stopOutcome: 'operator_stop',
			stopBy: undefined,
			playingBack: false,
			pendingFrozen: null,
			misses: 0,
			call: null,
			lastValues: null,
			calls: [],
			identification: null,
			feasibility: null,
			record: null,
			records: [],
			rows: [],
			end: null,
			timer: null,
			lastTick: Date.now(),
			version: 0,
			prep: prepare(refs.get(config.reference.sha256)!, config)
		};
	}

	/** Claim the run's folder at arm, or refuse the arm as the service does when it cannot be written. */
	function claimRecord(r: Run) {
		if (r.record) return;
		const folder = host.recordFolder?.(r.id) ?? `~/Hertzforge_Data/${r.id}`;
		if (host.record === 'unwritable') {
			const reason = 'Permission denied';
			refuse('refused', 'record_unwritable', `The run's record could not be written, so the run did not arm: ${folder} (${reason}). Choose a data folder that can be written.`, { path: folder, reason });
		}
		const streams = r.config.record?.streams === false ? 'off' : 'on';
		r.record = { folder, streams, problem: null };
		if (streams === 'off') addLog('record', "the run's streams are not recorded: switched off in the setup");
	}

	function arm(r: Run, ack: string | undefined) {
		const exceeded = r.derived.feasibility.verdict === 'exceeded';
		if (exceeded && !ack) {
			r.feasibility = { verdict: 'exceeded', message: r.derived.feasibility.message };
			setPhase(r, 'blocked', 'feasibility_blocked');
			addLog('feasibility', 'feasibility exceeded; arming needs an acknowledgement');
			const c = r.derived.feasibility.checks.find((x) => !x.pass);
			refuse('refused', 'feasibility_exceeded', "The section's kinematics are over a feasibility ceiling; arm with an acknowledgement to go on.", { verdict: 'exceeded', ...(c ? { check: c.name, value: c.value, limit: c.limit } : {}) });
		}
		if (ack && !exceeded) refuse('refused', 'override_not_needed', 'Nothing blocks arming; arm without an acknowledgement.');
		if (r.source === 'HARDWARE' && !r.law.builtin && !r.law.check.hardware) refuse('refused', 'law_not_checked', `${r.law.name} has no passed check for its exact file; check it before it drives hardware.`, { id: r.law.id });
		claimRecord(r);
		if (ack) addLog('override', `armed past feasibility: "${ack}"`);
		r.feasibility = { verdict: r.derived.feasibility.verdict, message: r.derived.feasibility.message };
		r.derived = { ...r.derived, grid: { ...r.derived.grid, actual: true } };
		r.armed = true;
		setPhase(r, 'armed', 'armed');
		addLog('station', `armed on ${r.station.class}; nothing driven`);
		startLive();
	}

	function start(r: Run) {
		r.sysidT = 0;
		setPhase(r, 'running', 'sysid');
		const id = r.config.identification ?? {};
		addLog('state', `identification: ${id.excitation === 'log_chirp' ? 'a log chirp' : 'band-limited random'} at ${id.drive_vrms ?? 0.05} V rms`);
		if (r.timer) clearInterval(r.timer);
		r.lastTick = Date.now();
		r.timer = setInterval(() => tick(r), TICK_MS);
	}

	function finishIdentification(r: Run) {
		const id = r.config.identification ?? {};
		const vrms = id.drive_vrms ?? 0.05;
		const coh = vrms >= 0.002 ? 0.9999 : 0.42;
		const min = id.min_mean_coherence ?? 0.8;
		const coverage = r.derived.coverage.fraction;
		const passed = coh >= min;
		r.identification = { excitation: id.excitation ?? 'random', drive_vrms: vrms, drive_vpk: round(vrms * 4.6, 4), coherence_mean: coh, min_mean_coherence: min, coverage: passed ? coverage : null, lag_frames: LAG_FRAMES, lag_source: 'measured', passed };
		r.version++;
		addLog('gate', `identification done: mean coherence ${coh}, lag ${LAG_FRAMES} frames measured`);
		if (coh < min) {
			setPhase(r, 'held', 'coherence_hold');
			addLog('gate', `held: the mean coherence ${coh} is under ${min}; only a new run releases the hold`);
			return;
		}
		const need = r.config.correction?.min_band_energy ?? 0.9;
		if (coverage < need) {
			end(r, 'aborted', 'aborted', { kind: 'coverage', play: null, at_level_db: null, coverage, min_band_energy: need, error_floor_pct: r.derived.coverage.error_floor_pct });
			return;
		}
		if (id.review ?? true) {
			setPhase(r, 'review', 'awaiting_review');
			addLog('state', 'holding for review');
		} else proceed(r);
	}

	/** A law call: the next drive, or a miss. */
	function callLaw(r: Run, kind: TwrPlayKind): boolean {
		const misbehave = r.law.name === 'TWR misbehave';
		const budgetMs = r.derived.call_budget_s * 1000;
		if (misbehave && r.calls.length > 0) {
			r.misses++;
			r.call = { used: false, miss: 'over_budget', call_ms: round(budgetMs * 1.4, 3), peak_rise_db: null, bin_rise_db: null, predicted_peak_ratio: null, values: {} };
			addLog('law', `the law's call was late (${round(budgetMs * 1.4, 1)} ms against ${round(budgetMs, 1)} ms); the drive in force plays again`);
			const max = r.config.control?.law_misses ?? 3;
			if (r.misses >= max) {
				end(r, 'law_fault', 'stopped', { kind: 'law_fault', play: r.done > 0 ? r.done - 1 : null, at_level_db: r.level, reason: 'over_budget', consecutive: r.misses });
				return false;
			}
			stateChanged(true);
			return true;
		}
		const beta = typeof r.params.beta === 'number' ? r.params.beta : 0.5;
		const corrects = r.params.correct !== false;
		const before = r.err;
		if (r.calls.length > 0 && corrects) r.err = Math.max(1.4, r.err * (1 - beta * 0.9975 * 10 ** ((typeof r.params.model_error_db === 'number' ? r.params.model_error_db : 0) / 20)));
		const change = before > 0 ? round(20 * Math.log10((100 + before) / (100 + r.err)), 3) : 0;
		const values: Record<string, number> = {};
		for (const d of r.law.diagnostics) values[d.key] = d.key === 'corrected_bins' ? 812 : d.key === 'clamped_bins' ? 0 : change;
		r.misses = 0;
		r.lastValues = values;
		r.calls.push({ index: r.done, values });
		r.call = { used: true, miss: null, call_ms: round(0.8 + (r.calls.length % 5) * 0.3, 3), peak_rise_db: round(Math.abs(change) + 0.2, 3), bin_rise_db: round(Math.abs(change) * 1.6 + 0.4, 3), predicted_peak_ratio: round(1 + r.err / 200, 4), values };
		addLog('law', kind === 'playback' ? 'the law corrected the drive after a playback play' : 'the law corrected the drive');
		return true;
	}

	function proceed(r: Run) {
		// The first level of the table, unless a level move from the review hold set one.
		if (r.level === null) {
			r.level = r.levels[0];
			r.rung = 0;
		}
		r.atLevel = 0;
		const backoff = typeof r.params.initial_backoff_db === 'number' ? r.params.initial_backoff_db : 0;
		r.err = r.params.correct === false ? 50.1 : backoff > 0 ? (1 - 10 ** (-backoff / 20)) * 100 : 18;
		addLog('law', `built ${r.law.name} ${r.law.version} and seeded it from the identification`);
		r.calls = [];
		r.call = null;
		callLaw(r, 'iteration');
		const full = predictedFull(r);
		if (r.mode === 'auto' && full > ceiling(r)) {
			end(r, 'aborted', 'aborted', { kind: 'drive_vpk', play: null, at_level_db: r.levels[r.levels.length - 1], vpk: round(full, 4), ceiling_vpk: ceiling(r), predicted: true });
			return;
		}
		if (r.mode === 'auto') beginPlay(r, 'iteration');
		else setPhase(r, 'running', 'waiting');
	}

	/** A freeze asked for takes effect: logged, and the state says so. */
	function applyFreeze(r: Run) {
		if (r.pendingFrozen === null) return;
		const frozen = r.pendingFrozen;
		r.pendingFrozen = null;
		if (frozen === r.frozen) return;
		r.frozen = frozen;
		addLog('law', r.frozen ? 'corrections frozen: the drive in force replays' : 'corrections resumed');
		stateChanged(true);
	}

	function beginPlay(r: Run, kind: TwrPlayKind) {
		applyFreeze(r);
		const iteration = kind === 'iteration' ? r.atLevel + 1 : null;
		r.play = { index: r.done, kind, iteration, t0: r.t, elapsed: 0, done: 0, ticks: 0 };
		setPhase(r, 'running', kind === 'iteration' ? 'iterating' : 'playing');
	}

	function judgePlay(r: Run) {
		const pl = r.play!;
		const p = ref(r);
		const level = r.level ?? 0;
		const err = round(r.err, 4);
		const gated = err <= gate(r);
		if (err > (r.config.limits.envelope_alarm_frac ?? 0.25) * 100) r.alarm = true;
		const s = 10 ** (level / 20);
		const row: TwrPlayRow = {
			index: pl.index,
			kind: pl.kind,
			level_db: level,
			rung: r.rung,
			iteration: pl.iteration,
			rms_error_pct: err,
			peak_error_pct: round(err * 2.4, 4),
			spectral_error_db: round(err / 14, 4),
			gated,
			clipped_samples: 0,
			clamped_bins: r.call?.values?.clamped_bins ?? 0,
			drive_vpk: round(driveAt(r, level), 5),
			response_peak: round(s * p.peak * (1 + err / 300), 5),
			lag_frames: round(LAG_FRAMES + 0.02, 3),
			drift_frames: 0.02,
			start_frame: Math.round(pl.t0 * p.fs),
			frames: playFrames(p),
			at_s: round(pl.t0, 3),
			law: r.call
		};
		r.call = null;
		r.rows.push(row);
		if (host.record === 'problem' && r.record && r.record.streams === 'on' && r.record.problem === null && r.rows.length >= 2) {
			const problem = 'Recording of the inputs stream stopped: a write failed: No space left on device (os error 28). The run goes on.';
			r.record = { ...r.record, problem };
			addLog('record', problem);
		}
		r.records.push({ row, err, level });
		if (r.records.length > RECORDS_HELD) r.records.shift();
		r.done++;
		if (pl.kind === 'iteration') r.atLevel++;
		else r.playback++;
		r.play = null;
		r.version++;
		addLog('note', `Play ${row.index}: ${COMMITTED} frames committed before start.`);
		afterPlay(r, row);
		// A freeze asked for during the play takes effect once the play's correction is done.
		if (run === r && r.state === 'running') applyFreeze(r);
	}

	function afterPlay(r: Run, row: TwrPlayRow) {
		const full = r.rung === r.levels.length - 1;
		if (row.kind === 'playback') {
			if (r.config.adaptation?.during_playback && !r.frozen && !callLaw(r, 'playback')) return;
			if (r.playback >= r.derived.plays_target) {
				end(r, 'completed', 'stopped', null);
				return;
			}
			queueNext(r, 'playback');
			return;
		}
		if (!row.gated) {
			if (r.atLevel >= ofLevel(r) && r.mode === 'auto') {
				if ((r.config.schedule?.on_gate_fail ?? 'abort') === 'hold') {
					setPhase(r, 'running', 'holding');
					addLog('gate', `the level did not meet its gate in ${r.atLevel} plays; holding`);
					return;
				}
				end(r, 'not_converged', 'aborted', { kind: 'not_converged', play: row.index, at_level_db: row.level_db, iterations: r.atLevel, error_pct: row.rms_error_pct, gate_pct: gate(r) });
				return;
			}
			if (!r.frozen && !callLaw(r, 'iteration')) return;
		}
		if (r.mode === 'manual') {
			setPhase(r, 'running', 'waiting');
			return;
		}
		if (row.gated) {
			if (full) {
				addLog('level', 'full level gated; playback begins');
				queueNext(r, 'playback');
				return;
			}
			r.rung = (r.rung ?? 0) + 1;
			r.level = r.levels[r.rung];
			r.atLevel = 0;
			addLog('level', `level ${r.level} dB`);
		}
		queueNext(r, 'iteration');
	}

	/** Between plays: a short gap on the automatic ladder, longer while nobody holds control. */
	let nextKind: TwrPlayKind = 'iteration';
	function queueNext(r: Run, kind: TwrPlayKind) {
		nextKind = kind;
		r.gapT = 0;
		setPhase(r, 'running', 'waiting');
	}

	function endMessage(outcome: TwrOutcome, trip: TwrTrip | null, by?: string): string {
		switch (outcome) {
			case 'completed':
				return 'The playback schedule finished.';
			case 'operator_stop':
				return `Stopped by ${by ?? 'the operator'}.`;
			case 'operator_abort':
				return `Aborted by ${by ?? 'the operator'}.`;
			case 'safe_stop':
				return `Nobody held control for ${host.timeoutS} s, so the run stopped.`;
			case 'not_converged':
				return trip?.kind === 'not_converged' ? `The level took ${trip.iterations} correction plays without meeting its ${trip.gate_pct}% gate.` : 'A level did not meet its gate.';
			case 'law_fault':
				return trip?.kind === 'law_fault' ? `The control law gave no usable drive ${trip.consecutive} times in a row; the run took its controlled stop.` : 'The control law gave no usable drive.';
			case 'aborted':
				if (trip?.kind === 'drive_vpk' && trip.predicted) return `The full-level drive is predicted at ${trip.vpk} V pk, over the ${trip.ceiling_vpk} V pk ceiling; nothing was played.`;
				if (trip?.kind === 'coverage') return "The trusted bins cover too little of the reference's energy; nothing was played.";
				return trip ? `A protection tripped: ${trip.kind.replaceAll('_', ' ')}.` : 'A protection tripped.';
			default:
				return outcome.replaceAll('_', ' ');
		}
	}

	function end(r: Run, outcome: TwrOutcome, phase: TwrPhase, trip: TwrTrip | null, by?: string) {
		if (r.timer) clearInterval(r.timer);
		r.timer = null;
		const p = ref(r);
		const framesDone = r.play && (outcome === 'operator_stop' || outcome === 'operator_abort' || outcome === 'safe_stop') ? Math.round(r.play.done * p.recordLen) : null;
		const at = r.play?.index ?? (r.done > 0 ? r.done - 1 : null);
		r.end = { outcome, message: endMessage(outcome, trip, by), at_s: r.armed ? round(r.t, 3) : null, level_db: r.level, play: at, frames_done: framesDone, trip };
		r.play = null;
		setPhase(r, 'finished', phase);
		addLog(outcome === 'aborted' ? 'trip' : 'state', `finished: ${outcome.replaceAll('_', ' ')}`);
		publish(r);
	}

	/** Stop: a play in flight comes to rest along the taper, then the run ends; between plays it ends at once. */
	function stopRun(r: Run, outcome: TwrOutcome, by?: string) {
		if (!r.play) {
			end(r, outcome, 'stopped', null, by);
			return;
		}
		r.stopOutcome = outcome;
		r.stopBy = by;
		r.stopT = 0;
		r.stopTicks = 0;
		setPhase(r, 'stopping', 'stopping');
	}

	function tick(r: Run) {
		if (run !== r) return;
		// Time passes on the wall clock, so a tab whose timers the browser slows still plays in time.
		const now = Date.now();
		const dt = Math.min(1, Math.max(0, (now - r.lastTick) / 1000)) * host.speed;
		r.lastTick = now;
		if (r.state === 'stopping') {
			r.stopT += dt;
			r.stopTicks++;
			r.t += Math.min(dt, Math.max(0, STOP_S - (r.stopT - dt)));
			if (r.stopT >= STOP_S && r.stopTicks >= MIN_PLAY_TICKS) end(r, r.stopOutcome, 'stopped', null, r.stopBy);
			return;
		}
		if (r.state !== 'running') return;
		if (r.phase === 'sysid') {
			r.t += dt;
			r.sysidT += dt;
			if (r.sysidT >= (r.config.identification?.duration_s ?? 4)) finishIdentification(r);
			return;
		}
		if (r.play) {
			// A play takes at least a few ticks, so nothing a verb starts ends before the verb is answered,
			// as on the service's socket, where the answer goes out before the events that follow it.
			const was = r.play.elapsed;
			r.play.elapsed = Math.min(recordS(r), was + dt);
			r.t += r.play.elapsed - was;
			r.play.ticks++;
			const whole = r.play.elapsed >= recordS(r);
			r.play.done = whole ? 1 : r.play.elapsed / recordS(r);
			if (whole && r.play.ticks >= MIN_PLAY_TICKS) judgePlay(r);
			return;
		}
		if (r.phase !== 'waiting') return;
		// Between plays: the automatic ladder, or the manual one's playback, plays on while somebody holds control.
		const going = r.mode === 'auto' || r.playingBack;
		if (!going || !host.hasController()) return;
		r.gapT += dt;
		if (r.gapT >= GAP_S * host.speed) beginPlay(r, nextKind);
	}

	// ---- refusals -----------------------------------------------------------------------------------

	function needRun(): Run {
		if (!run) refuse('refused', 'not_configured', 'TWR is not configured; configure a test first.');
		return run;
	}

	function wrongState(r: Run, verb: string): never {
		refuse('refused', 'wrong_state', `${verb} does not apply while the run is ${r.state} (${r.phase.replaceAll('_', ' ')}).`, { state: r.state, phase: r.phase });
	}

	function notWhileOther() {
		const other = host.otherRun();
		if (other) refuse('refused', 'run_active', `Run ${other} holds the station; stop it first.`, { run_id: other });
	}

	function notWhileLive(what: string) {
		if (run && isLive()) refuse('refused', 'run_active', `Run ${run.id} is ${run.state}; stop it before ${what}.`, { run_id: run.id });
	}

	function needManual(r: Run, verb: string) {
		if (r.mode !== 'manual') refuse('refused', 'wrong_mode', `${verb} is for the manual ladder; this run is automatic.`, { mode: r.mode });
	}

	/** At the coherence hold only a new run goes on; the service says why in its own words. */
	function notHeld(r: Run) {
		if (r.state !== 'held') return;
		const id = r.identification;
		refuse('refused', 'coherence_hold', `The run is held at the coherence hold: the identification's mean coherence, ${id?.coherence_mean ?? '-'}, is under ${id?.min_mean_coherence ?? '-'}. Only twr.reset releases it.`, {
			coherence_mean: id?.coherence_mean ?? null,
			min_mean_coherence: id?.min_mean_coherence ?? null
		});
	}

	/** Between plays and before playback (Play also from a level held for its gate). */
	function needBetween(r: Run, verb: string, holding = false) {
		const phaseOk = r.phase === 'waiting' || (holding && r.phase === 'holding');
		if (r.state !== 'running' || !phaseOk || r.play || r.playingBack) wrongState(r, verb);
	}

	const gatedHere = (r: Run) => {
		const last = r.rows.at(-1);
		return !!last && last.kind === 'iteration' && last.gated && r.level !== null && Math.abs(last.level_db - r.level) < 1e-9;
	};

	function parseSubscribe(params: Record<string, unknown>): { rate: number; plots: TwrSubscribePlot[] } | null {
		if (params.default !== undefined) {
			if (params.default !== true) bad('default', '"default" can only be true.');
			if (params.plots !== undefined || params.rate_hz !== undefined) bad('default', '"default" takes no other parameter.');
			return null;
		}
		const rate = params.rate_hz === undefined ? 5 : params.rate_hz;
		if (!(isNum(rate) && rate >= 0.5 && rate <= 10)) bad('rate_hz', '"rate_hz" is a number from 0.5 to 10.');
		if (!Array.isArray(params.plots)) bad('plots', '"plots" is a list of plots.');
		const list = params.plots as unknown[];
		if (list.length > 64) bad('plots', 'At most 64 plots.');
		const seen = new Set<string>();
		const plots = list.map((raw, i): TwrSubscribePlot => {
			const at = `plots[${i}]`;
			if (!isObject(raw)) bad(at, `${at} is an object.`);
			const p = raw as Record<string, unknown>;
			const extra = Object.keys(p).find((k) => !['id', 'quantity', 'points'].includes(k));
			if (extra) bad(`${at}.${extra}`, `A plot has an id, a quantity and points; not "${extra}".`);
			if (typeof p.id !== 'string' || p.id.length < 1 || p.id.length > 64) bad(`${at}.id`, 'A plot id is 1 to 64 characters.');
			if (seen.has(p.id as string)) bad(`${at}.id`, `The plot id "${p.id}" is used twice.`);
			seen.add(p.id as string);
			if (!QUANTITIES.includes(p.quantity as TwrQuantity)) bad(`${at}.quantity`, `"quantity" is one of ${QUANTITIES.join(', ')}.`);
			const quantity = p.quantity as TwrQuantity;
			const out: TwrSubscribePlot = { id: p.id as string, quantity };
			if (WITH_POINTS.includes(quantity)) {
				const pts = p.points ?? 1024;
				if (!(Number.isInteger(pts) && (pts as number) >= 64 && (pts as number) <= 8192)) bad(`${at}.points`, '"points" is a whole number from 64 to 8192.');
				out.points = pts as number;
			} else if (p.points !== undefined && p.points !== null) bad(`${at}.points`, `"points" does not apply to ${quantity}.`);
			return out;
		});
		return { rate: rate as number, plots };
	}

	function shapeOk(config: Record<string, unknown>) {
		if (!isObject(config.reference) || typeof (config.reference as Record<string, unknown>).sha256 !== 'string') bad('config.reference.sha256', 'The configuration names its reference by "sha256".');
		if (!isObject(config.limits)) bad('config.limits', 'The configuration has no "limits".');
		if (!isNum((config.limits as Record<string, unknown>).drive_vpk_ceiling)) bad('config.limits.drive_vpk_ceiling', '"drive_vpk_ceiling" is a number of volts peak; it has no default.');
		if (config.capture != null) bad('config.capture', 'A TWR run is always recorded in its run folder; "capture" is no longer taken. Use "record" to keep or leave out the streams.');
	}

	// ---- the methods -----------------------------------------------------------------------------------

	return {
		laws,
		isLive,
		liveId: () => (run && isLive() ? run.id : null),
		detach: (conn) => {
			subs.delete(conn);
		},
		seatExpired: () => {
			if (run && isLive() && run.state !== 'stopping') stopRun(run, 'safe_stop');
		},
		methods: {
			'laws.import': (params, conn) => {
				host.own(conn);
				const path = params.path;
				if (!absolute(path) || !path.trim()) bad('path', 'A law file is named by its absolute path on the computer that runs the test.');
				if (!/\.wasm$/i.test(path as string)) refuse('refused', 'law_import_failed', 'The file is not a law file (.wasm).');
				const law = importedLaw(path as string);
				if (!imported.some((l) => l.id === law.id)) imported = [...imported, law];
				return { law };
			},
			'twr.reference.import': (params) => {
				const hasText = params.text !== undefined;
				const hasPath = params.path !== undefined;
				if (hasText === hasPath) bad('text', 'Give the reference as "text", or a file by its "path"; one of the two.');
				if (params.fs_hz !== undefined && !(isNum(params.fs_hz) && params.fs_hz > 0)) bad('fs_hz', '"fs_hz" is a rate above 0.');
				if (params.column !== undefined && !(Number.isInteger(params.column) && (params.column as number) >= 0)) bad('column', '"column" is a whole number from 0.');
				const units = typeof params.units === 'string' && params.units.trim() ? params.units.trim() : 'g';
				const fsHz = params.fs_hz as number | undefined;
				let stored: StoredRef;
				if (hasText) {
					if (typeof params.text !== 'string') bad('text', '"text" is the reference as text.');
					const { values, fs } = parseText(params.text as string, fsHz, params.column as number | undefined);
					// The hash is of the record as read (its rate, its unit and its samples), not of the text.
					const sha = hash64('record', fs, units, values.join(','));
					const name = typeof params.name === 'string' && params.name.trim() ? params.name.trim() : 'pasted';
					stored = storeRef({ sha256: sha, path: null, name, format: 'text', fs_hz: round(fs, 6), samples: values.length, duration_s: round(values.length / fs, 6), units }, (i) => {
						const a = Math.max(0, Math.min(values.length - 1, Math.floor(i)));
						const b = Math.min(values.length - 1, a + 1);
						const w = i - a;
						return values[a] * (1 - w) + values[b] * w;
					});
				} else {
					if (!absolute(params.path)) bad('path', 'A reference file is named by its full path on the computer that runs the test.');
					const path = params.path as string;
					if (/missing|unreadable|\.bad$/i.test(path)) refuse('refused', 'reference_unreadable', `Could not read ${path}: no such file.`, { path, line: null });
					const ext = (path.split('.').pop() ?? '').toLowerCase();
					if (!['csv', 'tsv', 'wav', 'json'].includes(ext)) refuse('refused', 'reference_unreadable', `${path} is not a CSV, TSV, WAV or JSON record.`, { path, line: null });
					const fs = fsHz ?? 12800;
					const n = Math.round(fs * 4);
					const name = typeof params.name === 'string' && params.name.trim() ? params.name.trim() : (path.split(/[\\/]/).pop() ?? path).replace(/\.[^.]+$/, '');
					const sha = hash64('path', path, fs, units);
					stored = storeRef({ sha256: sha, path, name, format: ext as TwrReferenceFormat, fs_hz: fs, samples: n, duration_s: n / fs, units }, fieldLike(fs, n, 0.2, sha.charCodeAt(0)));
				}
				return { reference: stored.facts, warnings: [], views: [referenceView(stored)] };
			},
			'twr.reference.preview': (params) => {
				// A wrong shape is a bad parameter, named; what a configuration would be refused is `config_invalid`.
				const sha = isObject(params.reference) ? (params.reference as Record<string, unknown>).sha256 : undefined;
				if (typeof sha !== 'string' || !/^[0-9a-f]{64}$/.test(sha)) bad('reference.sha256', '"reference.sha256" is 64 lowercase hexadecimal digits.');
				for (const k of ['prep', 'section', 'pads', 'acquisition', 'correction', 'feasibility']) {
					const v = params[k];
					if (v !== undefined && v !== null && !isObject(v)) bad(k, `"${k}" is an object.`);
				}
				const c = { ...(params as unknown as Pick<TwrConfig, 'prep' | 'section' | 'pads' | 'acquisition' | 'correction' | 'feasibility'>) };
				// The correction band, when not given, is the default identification's.
				c.correction = { band_hz: [20, 2000], ...(c.correction ?? {}) };
				const r = refs.get(sha as string);
				const errors: Item[] = [];
				if (!r) errors.push(item('reference_unknown', 'No stored reference has that SHA-256; import the reference again.', 'reference.sha256'));
				else errors.push(...preparationErrors(r, c));
				if (errors.length) refuse('bad_request', 'config_invalid', `The preparation has ${errors.length} ${errors.length === 1 ? 'error' : 'errors'}.`, { errors, warnings: [] });
				const p = prepare(r!, c);
				return { derived: previewDerived(r!, c, p), views: previewViews(p, r!, c) };
			},
			'twr.configure': (params, conn) => {
				if (!isObject(params.config)) bad('config', 'Configuring needs a test configuration.');
				shapeOk(params.config as Record<string, unknown>);
				const config = params.config as unknown as TwrConfig;
				const checked = check(config);
				if (params.dry_run === true) return { errors: checked.errors, warnings: checked.warnings, derived: checked.derived, views: checked.views };
				host.own(conn);
				notWhileLive('configuring');
				if (checked.errors.length || !checked.derived) refuse('bad_request', 'config_invalid', `The configuration has ${checked.errors.length} error(s).`, { errors: checked.errors, warnings: checked.warnings });
				if (run?.timer) clearInterval(run.timer);
				run = newRun(config, checked.derived, checked.views);
				log = [];
				addLog('state', `configured "${config.title ?? ''}" on ${run.station.class}`);
				stateChanged(true);
				return { state: stateOf(run), derived: checked.derived, warnings: checked.warnings };
			},
			'twr.config': () => (run ? { run_id: run.id, config: run.config, derived: run.derived, views: run.views } : { run_id: null, config: null, derived: null, views: [] }),
			'twr.arm': (params, conn) => {
				host.own(conn);
				const r = needRun();
				const ack = params.override_ack;
				if (ack !== undefined && (typeof ack !== 'string' || !ack.trim())) bad('override_ack', "An override acknowledgement is the operator's own words; it cannot be empty.");
				if (r.state === 'finished') refuse('refused', 'run_finished', 'This run has finished; start a new run or configure again.', { outcome: r.end?.outcome });
				if (r.state !== 'configured' && r.state !== 'blocked') wrongState(r, 'Arming');
				notWhileOther();
				arm(r, ack as string | undefined);
				return stateOf(r);
			},
			'twr.start': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state === 'finished') refuse('refused', 'run_finished', 'This run has finished; start a new run or configure again.', { outcome: r.end?.outcome });
				if (r.state !== 'configured' && r.state !== 'armed') wrongState(r, 'Starting');
				notWhileOther();
				if (r.state === 'configured') arm(r, undefined);
				start(r);
				return stateOf(r);
			},
			'twr.proceed': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state === 'held') refuse('refused', 'coherence_hold', `The identification's mean coherence, ${r.identification?.coherence_mean ?? '-'}, stayed under its floor; only a new run releases the hold.`, { coherence_mean: r.identification?.coherence_mean, min_mean_coherence: r.identification?.min_mean_coherence });
				if (r.state !== 'review') wrongState(r, 'Proceeding');
				addLog('note', 'the operator reviewed identification and proceeded');
				proceed(r);
				return stateOf(r);
			},
			'twr.iterate': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				needManual(r, 'An iteration');
				notHeld(r);
				needBetween(r, 'An iteration');
				beginPlay(r, 'iteration');
				return stateOf(r);
			},
			'twr.play': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				needManual(r, 'Playback');
				notHeld(r);
				needBetween(r, 'Playback', true);
				if (!gatedHere(r)) refuse('refused', 'not_converged', `Playback begins once the present level has gated; ${r.level} dB has not met the ${gate(r)} % gate.`, { level_db: r.level, gate_pct: gate(r) });
				r.playingBack = true;
				addLog('level', 'playback begins');
				beginPlay(r, 'playback');
				return stateOf(r);
			},
			'twr.level': (params, conn) => {
				host.own(conn);
				const r = needRun();
				const hasDelta = params.delta_db !== undefined;
				const hasRung = params.rung !== undefined;
				if (hasDelta === hasRung) bad('delta_db', 'A level move gives either a step of 1 or -1 dB, or a rung of the table.');
				if (hasDelta && params.delta_db !== 1 && params.delta_db !== -1) bad('delta_db', '"delta_db" is 1 or -1.');
				if (hasRung && !(Number.isInteger(params.rung) && (params.rung as number) >= 0)) bad('rung', '"rung" is a whole number from 0.');
				// The automatic ladder takes a level move only at a level that failed its gate under the hold policy.
				if (r.mode !== 'manual' && r.phase !== 'holding') refuse('refused', 'wrong_mode', 'A level change needs the manual ladder; this run is automatic.', { mode: r.mode });
				const between = (r.state === 'running' && (r.phase === 'waiting' || r.phase === 'holding')) || r.state === 'review';
				if (!between || r.play || r.playingBack) wrongState(r, 'A level change');
				const from = r.level ?? r.levels[0];
				let to: number;
				let rung: number | null;
				if (hasRung) {
					const i = params.rung as number;
					if (i >= r.levels.length) refuse('bad_request', 'not_a_rung', `The table has ${r.levels.length} levels; rung ${i} is not one of them.`, { levels_db: r.levels });
					to = r.levels[i];
					rung = i;
				} else {
					to = from + (params.delta_db as number);
					if (to < r.levels[0] - 1e-9 || to > r.levels[r.levels.length - 1] + 1e-9) refuse('bad_request', 'not_a_rung', `${to} dB is past the table's ${to < r.levels[0] ? 'first' : 'last'} level.`, { levels_db: r.levels });
					const i = r.levels.findIndex((l) => Math.abs(l - to) < 1e-9);
					rung = i >= 0 ? i : null;
				}
				const vpk = driveAt(r, to);
				if (vpk > ceiling(r)) refuse('refused', 'drive_ceiling', `The drive in force would need ${round(vpk, 4)} V pk at ${to} dB; the drive ceiling is ${ceiling(r)} V pk. The level and the drive are unchanged.`, { vpk: round(vpk, 4), ceiling_vpk: ceiling(r), level_db: round(to, 2) });
				r.level = to;
				r.rung = rung;
				r.atLevel = 0;
				addLog('level', `level ${to} dB`);
				if (r.phase === 'holding') {
					// The automatic ladder goes on correcting at the new level.
					queueNext(r, 'iteration');
				} else stateChanged(true);
				return { level_db: to };
			},
			'twr.freeze': (params, conn) => {
				host.own(conn);
				const r = needRun();
				if (typeof params.frozen !== 'boolean') bad('frozen', '"frozen" is true or false.');
				if (r.state !== 'running' && r.state !== 'review') wrongState(r, 'Freezing the law');
				// It takes effect before the next play: a play in flight is corrected after as it would have been.
				r.pendingFrozen = params.frozen as boolean;
				if (!r.play) applyFreeze(r);
				return { frozen: params.frozen };
			},
			'twr.stop': (_params, conn) => {
				if (run && isLive() && run.state !== 'stopping') {
					const by = host.clientName(conn);
					addLog('note', `stop requested by ${by}`);
					stopRun(run, 'operator_stop', by);
				}
				return stateOf(run);
			},
			'twr.abort': (_params, conn) => {
				if (run && isLive()) {
					const by = host.clientName(conn);
					addLog('note', `abort requested by ${by}`);
					end(run, 'operator_abort', 'aborted', null, by);
				}
				return stateOf(run);
			},
			'twr.reset': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				if (isLive() && r.state !== 'held') refuse('refused', 'run_active', `Run ${r.id} is ${r.state}; stop it before a new run.`, { run_id: r.id });
				if (r.timer) clearInterval(r.timer);
				const checked = check(r.config);
				run = newRun(r.config, checked.derived ?? r.derived, checked.views.length ? checked.views : r.views);
				log = [];
				addLog('state', `a new run of "${r.config.title ?? ''}"`);
				stateChanged(true);
				return stateOf(run);
			},
			'twr.clear': (_params, conn) => {
				host.own(conn);
				notWhileLive('clearing');
				run = null;
				log = [];
				stateChanged(true);
				return stateOf(null);
			},
			'twr.state': () => stateOf(run),
			'twr.log': (params) => {
				const since = isNum(params.since) ? Math.max(0, Math.floor(params.since)) : 0;
				return { entries: log.slice(since), next: log.length };
			},
			'twr.plays': (params) => {
				const since = isNum(params.since) ? Math.max(0, Math.floor(params.since)) : 0;
				const rows = run?.rows ?? [];
				return { plays: rows.filter((p) => p.index >= since), next: rows.length };
			},
			'twr.record': (params) => {
				const r = needRun();
				if (!(Number.isInteger(params.index) && (params.index as number) >= 0)) bad('index', '"index" is a play row, from 0.');
				const points = params.points ?? 1024;
				if (!(Number.isInteger(points) && ((points as number) === 0 || ((points as number) >= 64 && (points as number) <= 8192)))) bad('points', '"points" is 0, or a whole number from 64 to 8192.');
				const rec = r.records.find((x) => x.row.index === params.index);
				if (!rec) {
					if (r.rows.some((x) => x.index === params.index)) throw new MockError('not_found', `The records of play ${(params.index as number) + 1} are no longer held; the service keeps the last ${RECORDS_HELD} plays'.`);
					throw new MockError('not_found', `There is no play ${(params.index as number) + 1} in this run.`);
				}
				return { play: rec.row, views: recordViews(r, rec, points as number) };
			},
			'twr.subscribe': (params, conn) => {
				const sub = parseSubscribe(params);
				if (!sub) {
					subs.delete(conn);
					return { default: true, rate_hz: 5, plots: null, quantities: QUANTITIES };
				}
				const entry = { ...sub, acc: 0, sent: -1 };
				subs.set(conn, entry);
				if (run && run.armed) {
					const r = run;
					setTimeout(() => {
						if (subs.get(conn) === entry && run === r) {
							entry.sent = r.version;
							conn.emit('twr.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
						}
					}, 30);
				}
				return { default: false, rate_hz: sub.rate, plots: sub.plots, quantities: QUANTITIES };
			}
		}
	};
}
