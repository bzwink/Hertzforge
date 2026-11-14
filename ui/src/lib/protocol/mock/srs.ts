/**
 * Mock SRS shock (schema/PROTOCOL.md, "SRS Shock" and the SRS world of "Control Laws"), on the
 * mock control service's run slot and controller seat.
 *
 * A believable closed-loop run: identification with a quiet burst of the synthesis, the review
 * hold, the law's seed, then pulse after pulse with the time between pulses, each pulse's SRS
 * judged band by band against the target at the level fired and corrected by the law. The first
 * pulse comes in a few dB low in the upper bands and the correction closes most of the error each
 * pulse, so an automatic ladder that advances on a pass shows a failed pulse, a correction and a
 * pass. The manual ladder fires and steps on the operator's word. Stop finishes the pulse in
 * flight; abort ends at once; with nobody in control firing pauses, and the run safe-stops after
 * the timeout.
 *
 * It refuses what the service refuses, by the same reasons and on the same parameters: a point
 * count on a plot that is not a time history, a level step other than one decibel, a fire before
 * the time between pulses, a relative path. Every value is synthetic, made only so the shell has
 * something to draw. Nothing here is the service's synthesis or SRS math.
 */

import type { LogEntry, LogKind, ReadingRow, RefusalReason, RunRecordRef, Series, SpectrumView, StationRef, DataSource, TableView, View } from '../types';
import type { LawEntry } from '../mimo';
import type { ShockLevelParams } from '../shock';
import type {
	SrsBands,
	SrsComponents,
	SrsConfig,
	SrsDerived,
	SrsEnd,
	SrsLive,
	SrsOutcome,
	SrsPhase,
	SrsPulseRow,
	SrsQuantity,
	SrsReportItem,
	SrsRunState,
	SrsState,
	SrsSubscribePlot,
	SrsSynthesis,
	SrsTarget,
	SrsTrip
} from '../srs';
import type { ShockMockHost } from './shock';
import { MockError, type MockConnection, type MockMethod } from './socket';

const G_IPS2 = 386.089;
const TICK_MS = 50;
const LIVE_MS = 200;
const SYSID_S = 1.5;
const FIRE_S = 0.25;
/** The views of the most recent pulses are held; an older pulse's row stays. */
const HELD_VIEWS = 32;

const QUANTITIES: SrsQuantity[] = ['srs', 'pulse', 'velocity', 'displacement', 'identification', 'components', 'limit_channels', 'limit_frf', 'law', 'readings', 'pulses'];
const TIME_QUANTITIES: SrsQuantity[] = ['pulse', 'velocity', 'displacement'];

function refuse(code: 'refused' | 'bad_request', reason: RefusalReason | 'not_yet' | 'wrong_mode' | 'drive_ceiling', message: string, detail?: Record<string, unknown>): never {
	throw new MockError(code, message, reason as RefusalReason, detail);
}

function bad(param: string, message: string): never {
	refuse('bad_request', 'bad_params', message, { param });
}
const round = (v: number, d = 3) => Math.round(v * 10 ** d) / 10 ** d;
const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const absolute = (p: unknown): p is string => typeof p === 'string' && /^(\/|[A-Za-z]:\\)/.test(p);

// ---------------------------------------------------------------------------------------------
// The law catalog

const SRS_RATIO: LawEntry = {
	id: 'srs_ratio',
	world: 'shock_srs',
	builtin: true,
	name: 'SRS ratio',
	version: '0.1.0',
	author: 'Hertzforge',
	summary: "Moves each component by its band's target-to-measured ratio, limited each pulse.",
	sha256: null,
	check: { status: 'built_in', date: null, tool_version: null, cases: [], hardware: true, message: 'Built in: cleared for hardware.' },
	params: [{ key: 'max_db_per_pulse', label: 'Largest change per pulse', help: 'The most any component may move from one pulse to the next.', kind: 'number', unit: 'dB', min: 0.5, max: 5, step: 0.5, default: 3 }],
	diagnostics: [
		{ key: 'worst_band_error_db', label: 'Worst band error', unit: 'dB', help: 'The band furthest from the target on the last pulse.', layout: null },
		{ key: 'worst_band_hz', label: 'Worst band', unit: 'Hz', help: "That band's centre.", layout: null },
		{ key: 'clamped_components', label: 'Components held back', unit: '', help: 'Components whose move was held to the largest change per pulse.', layout: null }
	],
	shapes: null,
	needs: null
};

function importedLaw(path: string): LawEntry {
	let h = 0;
	for (const ch of path) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
	const sha = (h.toString(16).padStart(8, '0') + '5e5a11').repeat(5).slice(0, 64);
	return {
		id: sha,
		world: 'shock_srs',
		builtin: false,
		name: 'SRS template',
		version: '0.1.0',
		author: 'law author',
		summary: 'The SRS template: the built-in ratio with a switch and an error on its own seed.',
		sha256: sha,
		check: { status: 'unchecked', date: null, tool_version: null, cases: [], hardware: false, message: 'Not checked: runs on the simulator; check it before it drives hardware.' },
		params: [
			{ key: 'correct', label: 'Correct', help: 'Off keeps the seed for every pulse.', kind: 'toggle', default: true },
			{ key: 'seed_error_db', label: 'Seed error', help: "An error put on the law's own seed on purpose, for seeing how the core's guards respond.", kind: 'number', unit: 'dB', min: -12, max: 12, step: 1, default: 0 }
		],
		diagnostics: [{ key: 'worst_band_error_db', label: 'Worst band error', unit: 'dB', help: 'The band furthest from the target on the last pulse.', layout: null }],
		shapes: null,
		needs: null
	};
}

// ---------------------------------------------------------------------------------------------
// The band grid, the target and the pulse, made up for the screen

function bandCentres(b: SrsBands): number[] {
	const n = b.bands_per_octave ?? 3;
	const out: number[] = [];
	for (let k = 0; k < 400; k++) {
		const f = b.f_lo_hz * 2 ** (k / n);
		out.push(round(f, 2));
		if (f >= b.f_hi_hz - 1e-9) break;
	}
	return out;
}

function targetAt(t: SrsTarget, f: number): number {
	if (t.kind === 'flat') return t.accel_g;
	const p = t.points;
	if (f <= p[0][0]) return p[0][1];
	for (let i = 1; i < p.length; i++) {
		if (f <= p[i][0]) {
			const [f0, a0] = p[i - 1];
			const [f1, a1] = p[i];
			const x = Math.log(f / f0) / Math.log(f1 / f0);
			return Math.exp(Math.log(a0) + x * (Math.log(a1) - Math.log(a0)));
		}
	}
	return p[p.length - 1][1];
}

const freqAxis = { label: 'Frequency', unit: 'Hz', scale: 'log' as const };
const srsAxis = { label: 'Peak acceleration', unit: 'g', scale: 'log' as const };
const timeAxis = { label: 'Time', unit: 'ms', scale: 'linear' as const };
const f64 = (a: number[]) => Float64Array.from(a);

/** The target with its alarm and abort bands, at `scale`, and a measured SRS when given. */
function srsView(title: string, bands: number[], target: number[], alarm: [number, number], abort: [number, number], scale: number, measured?: number[], measuredName = 'Measured'): SpectrumView {
	const x = f64(bands);
	const t = target.map((v) => v * scale);
	const at = (db: number, sign: number) => f64(t.map((v) => v * 10 ** ((sign * db) / 20)));
	const series: Series[] = [
		{ name: 'Target', role: 'reference', x, y: f64(t) },
		{ name: 'Alarm high', role: 'tolerance', x, y: at(alarm[1], 1) },
		{ name: 'Alarm low', role: 'tolerance', x, y: at(alarm[0], -1) },
		{ name: 'Abort high', role: 'abort', x, y: at(abort[1], 1) },
		{ name: 'Abort low', role: 'abort', x, y: at(abort[0], -1) }
	];
	if (measured) series.push({ name: measuredName, role: 'measured', x, y: f64(measured) });
	return { view: 'spectrum', title, x: freqAxis, y: srsAxis, series, link: 'srs' };
}

interface Wave {
	t_ms: Float64Array;
	a: Float64Array;
	duration_s: number;
}

/** A pulse made of one odd-half-cycle wavelet per band, each sized to its band's target. */
function wave(bands: number[], target: number[], scale: number, q: number, wobble = 0): Wave {
	const halfCycles = 7;
	const duration = Math.max(...bands.map((f) => halfCycles / (2 * f))) * 1.2;
	const n = 2048;
	const t = new Float64Array(n);
	const a = new Float64Array(n);
	for (let i = 0; i < n; i++) {
		const s = (i / (n - 1)) * duration;
		t[i] = s * 1000;
		let v = 0;
		bands.forEach((f, k) => {
			const len = halfCycles / (2 * f);
			if (s > len) return;
			const amp = (target[k] * scale) / (q * 0.6);
			v += amp * Math.sin((Math.PI * s) / len) * Math.sin(2 * Math.PI * f * s) * (1 + wobble * Math.sin(k));
		});
		a[i] = v;
	}
	return { t_ms: t, a, duration_s: duration };
}

function integrate(t_ms: Float64Array, y: Float64Array, k: number): Float64Array {
	const out = new Float64Array(y.length);
	for (let i = 1; i < y.length; i++) out[i] = out[i - 1] + ((y[i] + y[i - 1]) / 2) * ((t_ms[i] - t_ms[i - 1]) / 1000) * k;
	return out;
}

const peak = (y: Float64Array) => y.reduce((m, v) => Math.max(m, Math.abs(v)), 0);
const span = (y: Float64Array) => y.reduce((m, v) => Math.max(m, v), -Infinity) - y.reduce((m, v) => Math.min(m, v), Infinity);

function timeViews(w: Wave, title = 'Pulse', name = 'Synthesized'): SpectrumView[] {
	const v = integrate(w.t_ms, w.a, G_IPS2);
	const d = integrate(w.t_ms, v, 1);
	const role = name === 'Measured' ? ('measured' as const) : ('reference' as const);
	return [
		{ view: 'spectrum', title, x: timeAxis, y: { label: 'Acceleration', unit: 'g', scale: 'linear' }, series: [{ name, role, x: w.t_ms, y: w.a }], link: 'time' },
		{ view: 'spectrum', title: 'Velocity', x: timeAxis, y: { label: 'Velocity', unit: 'in/s', scale: 'linear' }, series: [{ name, role, x: w.t_ms, y: v }], link: 'time' },
		{ view: 'spectrum', title: 'Displacement', x: timeAxis, y: { label: 'Displacement', unit: 'in', scale: 'linear' }, series: [{ name, role, x: w.t_ms, y: d }], link: 'time' }
	];
}

/** A view decimated to at most `points` points, as the service sends a long time history. */
function decimate(v: SpectrumView, points: number): SpectrumView {
	const n = v.series[0]?.x.length ?? 0;
	if (n <= points) return v;
	const step = n / points;
	return {
		...v,
		series: v.series.map((s) => {
			const x = new Float64Array(points);
			const y = new Float64Array(points);
			const yMin = new Float64Array(points);
			for (let i = 0; i < points; i++) {
				const a = Math.floor(i * step);
				const b = Math.max(a + 1, Math.floor((i + 1) * step));
				let hi = -Infinity;
				let lo = Infinity;
				for (let j = a; j < b; j++) {
					hi = Math.max(hi, (s.y as Float64Array)[j]);
					lo = Math.min(lo, (s.y as Float64Array)[j]);
				}
				x[i] = (s.x as Float64Array)[a];
				y[i] = hi;
				yMin[i] = lo;
			}
			return { ...s, x, y, y_min: yMin };
		})
	};
}

interface Synth {
	bands: number[];
	target: number[];
	wave: Wave;
	derived: Pick<SrsDerived, 'bands_hz' | 'target_g' | 'synthesis' | 'reference'>;
	views: View[];
}

function synthesize(target: SrsTarget, bands: SrsBands, synthesis: SrsSynthesis | undefined, alarm: [number, number], abort: [number, number]): Synth {
	const centres = bandCentres(bands);
	const tg = centres.map((f) => round(targetAt(target, f), 4));
	const q = bands.q ?? 10;
	const w = wave(centres, tg, 1, q);
	const v = integrate(w.t_ms, w.a, G_IPS2);
	const d = integrate(w.t_ms, v, 1);
	const tol = synthesis?.tol_db ?? 1.5;
	const converged = tol >= 0.5;
	const engine = synthesis?.engine ?? 'wavsyn';
	const comps = synthesis?.components?.rows.length ?? centres.length;
	const fit = tg.map((x, k) => x * 10 ** ((Math.min(tol, 1.2) * Math.sin(k * 1.3) * 0.8) / 20));
	return {
		bands: centres,
		target: tg,
		wave: w,
		derived: {
			bands_hz: centres,
			target_g: tg,
			synthesis: { engine, components: comps, iters: converged ? 6 : (synthesis?.max_iters ?? 50), converged, worst_err_db: round(Math.min(tol, 1.2) * 0.8, 2), duration_s: round(w.duration_s, 4) },
			reference: { peak_g: round(peak(w.a), 3), velocity_pk_ips: round(peak(v), 3), displacement_pk_pk_in: round(span(d), 4), residual_velocity_ips: 0, residual_disp_in: 0 }
		},
		views: [
			srsView('Target', centres, tg, alarm, abort, 1),
			{
				view: 'spectrum',
				title: 'Synthesis',
				x: freqAxis,
				y: srsAxis,
				series: [
					{ name: 'Target', role: 'reference', x: f64(centres), y: f64(tg) },
					{ name: 'Synthesis', role: 'other', x: f64(centres), y: f64(fit) }
				],
				link: 'srs'
			},
			...timeViews(w)
		]
	};
}

function envelopeOf(c: SrsConfig): { alarm: [number, number]; abort: [number, number] } {
	return { alarm: (c.envelope?.alarm_db ?? [3, 3]) as [number, number], abort: (c.envelope?.abort_db ?? [9, 6]) as [number, number] };
}

// ---------------------------------------------------------------------------------------------
// The run

interface Run {
	id: string;
	config: SrsConfig;
	derived: SrsDerived;
	synth: Synth;
	state: SrsRunState;
	phase: SrsPhase;
	station: StationRef;
	source: DataSource;
	mode: 'auto' | 'manual';
	levels: number[];
	pulsesPer: number[];
	rung: number;
	levelDb: number;
	firedAtLevel: number;
	passedAtLevel: number;
	failsInRow: number;
	/** The remaining error per band on the next pulse, dB. */
	error: number[];
	/** The gains the law last returned, one per component. */
	gains: number[];
	rows: SrsPulseRow[];
	lastViews: View[];
	t: number;
	phaseT: number;
	nextAt: number | null;
	stopAfterPulse: boolean;
	stopOutcome: SrsOutcome;
	stopBy: string | undefined;
	pending: { limitScale: number } | null;
	identification: SrsState['identification'];
	prediction: SrsState['prediction'];
	notch: SrsState['notch'];
	feasibility: SrsState['feasibility'];
	end: SrsEnd | null;
	alarm: boolean;
	timer: ReturnType<typeof setInterval> | null;
	law: LawEntry;
	/** The run's record, from arm on. */
	record: RunRecordRef | null;
}

export interface SrsMock {
	methods: Record<string, MockMethod>;
	/** The SRS world's laws, built in first. */
	laws(): LawEntry[];
	isLive(): boolean;
	/** The run's id while it is live, for another run's `run_active` refusal. */
	liveId(): string | null;
	detach(conn: MockConnection): void;
	/** Nobody held control for the timeout: the run safe-stops. */
	seatExpired(): void;
}

const LIVE: SrsRunState[] = ['armed', 'running', 'review', 'stopping'];

/** What the SRS mock needs of the control service around it: classical shock's, and whether another run holds the slot. */
export type SrsMockHost = ShockMockHost;

export function createSrsMock(host: SrsMockHost): SrsMock {
	let run: Run | null = null;
	let runCount = 0;
	let log: LogEntry[] = [];
	let lastStateKey = '';
	let imported: LawEntry[] = [];
	const subs = new Map<MockConnection, { rate: number; plots: SrsSubscribePlot[] }>();
	let liveTimer: ReturnType<typeof setInterval> | null = null;
	const pulseViews = new Map<number, View[]>();

	const laws = () => [SRS_RATIO, ...imported];
	const isLive = () => run !== null && LIVE.includes(run.state);

	function addLog(kind: LogKind, text: string) {
		const e: LogEntry = { seq: log.length, at_unix: Date.now() / 1000, kind, text };
		log.push(e);
		if (run) host.broadcast('srs.log', { run_id: run.id, entries: [e] });
	}

	const rungOf = (r: Run) => {
		const i = r.levels.findIndex((l) => Math.abs(l - r.levelDb) < 1e-9);
		return i >= 0 ? i : null;
	};

	function stateOf(r: Run | null): SrsState {
		if (!r) {
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
				pulses: { fired: 0, passed: 0, at_level: 0, of_level: 0 },
				next_pulse_s: null,
				elapsed_s: null,
				alarm: false,
				identification: null,
				prediction: null,
				notch: null,
				last_pulse: null,
				law: null,
				feasibility: null,
				end: null
			};
		}
		const started = r.state !== 'configured' && r.state !== 'blocked';
		return {
			run_id: r.id,
			state: r.state,
			phase: r.phase,
			title: r.config.title,
			source: r.source,
			station: r.station,
			grid: r.derived.grid,
			clock: started ? 'shared' : null,
			mode: r.mode,
			levels_db: r.levels,
			level_db: started ? r.levelDb : null,
			rung: started ? rungOf(r) : null,
			pulses: { fired: r.rows.length, passed: r.rows.filter((x) => x.passed).length, at_level: r.firedAtLevel, of_level: r.mode === 'auto' ? (r.pulsesPer[r.rung] ?? 1) : 0 },
			next_pulse_s: r.phase !== 'waiting' ? null : r.nextAt === null ? (r.mode === 'manual' ? 0 : null) : round(Math.max(0, r.nextAt - r.t), 2),
			elapsed_s: started ? round(r.t, 2) : null,
			alarm: r.alarm,
			identification: r.identification,
			prediction: r.prediction,
			notch: r.notch,
			last_pulse: r.rows.at(-1) ?? null,
			law: { id: r.law.id, name: r.law.name, version: r.law.version, sha256: r.law.sha256, misses: 0 },
			feasibility: r.feasibility,
			end: r.end,
			...(r.record ? { record: { ...r.record } } : {})
		};
	}

	function stateChanged(force = false) {
		const s = stateOf(run);
		const key = JSON.stringify({ ...s, next_pulse_s: null, elapsed_s: null });
		if (!force && key === lastStateKey) return;
		lastStateKey = key;
		host.broadcast('srs.state', s as unknown as Record<string, unknown>);
	}

	function liveOf(r: Run): SrsLive {
		const s = stateOf(r);
		return {
			run_id: r.id,
			source: r.source,
			state: r.state,
			phase: r.phase,
			mode: r.mode,
			level_db: s.level_db,
			rung: s.rung,
			pulses: s.pulses,
			next_pulse_s: s.next_pulse_s,
			fire: r.phase === 'firing' ? { done: Math.min(10, Math.round((r.phaseT / FIRE_S) * 10)), of: 10 } : null
		};
	}

	// ---- checking a configuration ---------------------------------------------------------------

	function checkTarget(t: SrsTarget, bands: SrsBands, at: string, item: (reason: SrsReportItem['reason'], message: string, field: string) => SrsReportItem): SrsReportItem[] {
		const errors: SrsReportItem[] = [];
		if (t.kind === 'flat') {
			if (!(t.accel_g > 0)) errors.push(item('out_of_range', 'A flat target is above 0 g.', `${at}.accel_g`));
			return errors;
		}
		const p = t.points;
		if (!Array.isArray(p) || p.length < 2) {
			errors.push(item('out_of_range', 'A breakpoint target has at least two points.', `${at}.points`));
			return errors;
		}
		p.forEach(([f, a], i) => {
			if (!(f > 0) || !(a > 0)) errors.push(item('out_of_range', `Point ${i + 1}: the frequency and the SRS are above 0.`, `${at}.points[${i}]`));
			if (i > 0 && !(f > p[i - 1][0])) errors.push(item('target_not_ascending', `Point ${i + 1}: frequencies rise from point to point.`, `${at}.points[${i}]`));
		});
		const centres = bandCentres(bands);
		if (!errors.length && (p[0][0] > centres[0] + 1e-6 || p[p.length - 1][0] < centres[centres.length - 1] - 1e-6))
			errors.push(item('target_not_covering', `The target covers ${p[0][0]} to ${p[p.length - 1][0]} Hz; the band grid runs from ${centres[0]} to ${centres[centres.length - 1]} Hz.`, `${at}.points`));
		return errors;
	}

	function checkBands(b: SrsBands, fs: number, at: string, item: (reason: SrsReportItem['reason'], message: string, field: string) => SrsReportItem): SrsReportItem[] {
		const errors: SrsReportItem[] = [];
		if (!(b.f_lo_hz > 0)) errors.push(item('out_of_range', 'The band grid starts above 0 Hz.', `${at}.f_lo_hz`));
		if (!(b.f_hi_hz > b.f_lo_hz)) errors.push(item('bands_empty', 'The band grid ends above where it starts.', `${at}.f_hi_hz`));
		else if (b.f_hi_hz >= fs / 2) errors.push(item('band_above_grid', `The band grid ends below half the rate, ${fs / 2} Hz.`, `${at}.f_hi_hz`));
		const n = b.bands_per_octave ?? 3;
		if (!(Number.isInteger(n) && n >= 1 && n <= 24)) errors.push(item('out_of_range', 'Bands an octave is a whole number from 1 to 24.', `${at}.bands_per_octave`));
		const q = b.q ?? 10;
		if (!(q > 0.5 && q <= 100)) errors.push(item('out_of_range', 'The quality factor is above 0.5 and at most 100.', `${at}.q`));
		return errors;
	}

	function checkComponents(c: SrsComponents | null | undefined, engine: string, item: (reason: SrsReportItem['reason'], message: string, field: string) => SrsReportItem): SrsReportItem[] {
		if (!c) return [];
		const errors: SrsReportItem[] = [];
		if (c.engine !== engine) errors.push(item('components_engine_mismatch', `The component table is for ${c.engine}; the synthesis engine is ${engine}.`, 'config.synthesis.components.engine'));
		c.rows.forEach((r, i) => {
			const ok = r.freq_hz > 0 && Number.isFinite(r.amplitude_g) && r.delay_s >= 0 && ('half_cycles' in r ? Number.isInteger(r.half_cycles) && r.half_cycles >= 3 && r.half_cycles % 2 === 1 : r.zeta > 0);
			if (!ok) errors.push({ ...item('component_invalid', `Component ${i + 1} is not a valid ${c.engine === 'wavsyn' ? 'wavelet (an odd count of at least 3 half cycles)' : 'damped sine'}.`, `config.synthesis.components.rows[${i}]`), row: i });
		});
		return errors;
	}

	function check(config: SrsConfig): { errors: SrsReportItem[]; warnings: SrsReportItem[]; derived: SrsDerived | null; views: View[]; synth: Synth | null } {
		const errors: SrsReportItem[] = [];
		const warnings: SrsReportItem[] = [];
		const item = (reason: SrsReportItem['reason'], message: string, field: string): SrsReportItem => ({ reason, message, field });
		const fs = config.acquisition?.fs_hz ?? 51200;
		if (!(fs >= 1000 && fs <= 204800)) errors.push(item('out_of_range', 'The rate is from 1000 to 204800 Hz.', 'config.acquisition.fs_hz'));
		const ring = config.acquisition?.ring_out_s ?? 0.05;
		if (!(ring >= 0 && ring <= 10)) errors.push(item('out_of_range', 'The ring-out is from 0 to 10 s.', 'config.acquisition.ring_out_s'));
		errors.push(...checkBands(config.bands, fs, 'config.bands', item));
		if (!errors.some((e) => e.field?.startsWith('config.bands'))) errors.push(...checkTarget(config.target, config.bands, 'config.target', item));
		const syn = config.synthesis ?? {};
		const engine = syn.engine ?? 'wavsyn';
		errors.push(...checkComponents(syn.components, engine, item));
		if (syn.max_iters !== undefined && !(Number.isInteger(syn.max_iters) && syn.max_iters >= 1 && syn.max_iters <= 1000)) errors.push(item('out_of_range', 'The synthesis passes are a whole number from 1 to 1000.', 'config.synthesis.max_iters'));
		if (syn.tol_db !== undefined && !(syn.tol_db > 0 && syn.tol_db <= 20)) errors.push(item('out_of_range', 'The synthesis tolerance is above 0 and at most 20 dB.', 'config.synthesis.tol_db'));
		if (syn.max_gain_db_per_iter !== undefined && !(syn.max_gain_db_per_iter > 0 && syn.max_gain_db_per_iter <= 20)) errors.push(item('out_of_range', "A pass's largest move is above 0 and at most 20 dB.", 'config.synthesis.max_gain_db_per_iter'));
		const { alarm, abort } = envelopeOf(config);
		for (const [name, v] of [['alarm_db', alarm], ['abort_db', abort]] as const) if (!(v[0] > 0 && v[1] > 0 && v[0] <= 60 && v[1] <= 60)) errors.push(item('out_of_range', 'Each side of the envelope is above 0 and at most 60 dB.', `config.envelope.${name}`));
		if (alarm[0] > abort[0] || alarm[1] > abort[1]) errors.push(item('alarm_wider_than_abort', 'The abort band is at least as wide as the alarm band on each side: a band would abort before it alarms.', 'config.envelope.abort_db'));
		const centres = errors.some((e) => e.field?.startsWith('config.bands')) ? [] : bandCentres(config.bands);
		const perBand = config.envelope?.per_band;
		if (Array.isArray(perBand) && centres.length && perBand.length !== centres.length) errors.push(item('per_band_length_mismatch', `The per-band envelope has ${perBand.length} entries; the grid has ${centres.length} bands.`, 'config.envelope.per_band'));
		const s = config.schedule;
		if (!s.levels_db.length) errors.push(item('levels_empty', 'The schedule has no levels.', 'config.schedule.levels_db'));
		for (let i = 1; i < s.levels_db.length; i++) if (s.levels_db[i] < s.levels_db[i - 1]) errors.push(item('levels_not_ascending', 'Levels rise, or stay level, from one to the next.', `config.schedule.levels_db[${i}]`));
		if (s.pulses.length !== 1 && s.pulses.length !== s.levels_db.length) errors.push(item('pulses_length_mismatch', 'Give one pulse count for every level, or one per level.', 'config.schedule.pulses'));
		const id = config.identification ?? {};
		if (id.level_db !== undefined && !(id.level_db < 0 && id.level_db >= -60)) errors.push(item('out_of_range', 'The burst is below full level, from -60 to below 0 dB.', 'config.identification.level_db'));
		if (id.nfft !== undefined && !(Number.isInteger(Math.log2(id.nfft)) && id.nfft >= 64 && id.nfft <= 65536)) errors.push(item('out_of_range', 'The block is a power of two from 64 to 65536.', 'config.identification.nfft'));
		const lim = config.limits;
		if (!(lim.drive_vpk_ceiling > 0 && lim.drive_vpk_ceiling <= 10)) errors.push(item('out_of_range', 'The drive ceiling is above 0 and at most 10 V pk.', 'config.limits.drive_vpk_ceiling'));
		if (!(lim.drive_vpk_abort > lim.drive_vpk_ceiling && lim.drive_vpk_abort <= 10)) errors.push(item('out_of_range', 'The drive abort is above the drive ceiling and at most 10 V pk.', 'config.limits.drive_vpk_abort'));
		if (id.max_vpk != null && !(id.max_vpk > 0 && id.max_vpk <= lim.drive_vpk_ceiling)) errors.push(item('out_of_range', "The burst's cap is above 0 and at most the drive ceiling.", 'config.identification.max_vpk'));
		if (lim.abort_factor !== undefined && !(lim.abort_factor >= 1 && lim.abort_factor <= 100)) errors.push(item('out_of_range', 'The abort factor is from 1 to 100.', 'config.limits.abort_factor'));
		for (const [k, v] of Object.entries(lim.kinematic ?? {})) if (v !== null && v !== undefined && !(v > 0)) errors.push(item('out_of_range', 'A kinematic ceiling is above 0, or none.', `config.limits.kinematic.${k}`));
		const headroom = config.limit_channels?.headroom_db;
		if (headroom !== undefined && !(headroom >= 0 && headroom <= 40)) errors.push(item('out_of_range', 'The headroom is from 0 to 40 dB.', 'config.limit_channels.headroom_db'));
		const rise = config.control?.max_gain_rise_db;
		if (rise !== undefined && !(rise > 0 && rise <= 20)) errors.push(item('out_of_range', "A gain's rise cap is above 0 and at most 20 dB.", 'config.control.max_gain_rise_db'));
		const rows = host.rows();
		const input = (index: number) => rows.some((r) => r.enabled && r.index === index && r.role !== 'drive');
		if (!rows.some((r) => r.enabled && r.role === 'control')) errors.push({ reason: 'no_control_channel', message: 'The channel table has no enabled control row.' });
		if (!rows.some((r) => r.enabled && r.role === 'drive')) errors.push({ reason: 'no_drive_channel', message: 'The channel table has no enabled drive row.' });
		(lim.monitors ?? []).forEach((m, i) => {
			if (!input(m.channel)) errors.push(item('limit_unknown_channel', `Channel ${m.channel} is not an enabled input of the channel table.`, `config.limits.monitors[${i}].channel`));
		});
		const control = rows.find((r) => r.enabled && r.role === 'control')?.index ?? null;
		(config.limit_channels?.channels ?? []).forEach((c, i) => {
			// any row but the control row; one with no supplied gain, an enabled input to measure it from
			if (c.channel === control || !rows.some((r) => r.index === c.channel)) errors.push(item('limit_unknown_channel', `Channel ${c.channel} is not a row of the channel table other than the control row.`, `config.limit_channels.channels[${i}].channel`));
			else if (!c.frf && !input(c.channel)) errors.push(item('limit_no_frf', `Channel ${c.channel} has no supplied gain and is not an enabled input to measure one from.`, `config.limit_channels.channels[${i}]`));
			if (c.bound.kind === 'per_band' && centres.length && c.bound.bounds_g.length !== centres.length) errors.push(item('per_band_length_mismatch', `The bound has ${c.bound.bounds_g.length} entries; the grid has ${centres.length} bands.`, `config.limit_channels.channels[${i}].bound`));
			if (c.frf && (c.frf.freqs_hz.length < 2 || c.frf.freqs_hz.length !== c.frf.mag.length)) errors.push(item('out_of_range', 'A supplied gain has at least two points, one magnitude for each frequency.', `config.limit_channels.channels[${i}].frf`));
		});
		const lawId = config.control?.law?.id ?? 'srs_ratio';
		const law = laws().find((l) => l.id === lawId);
		if (!law) errors.push(item('law_unknown', `No control law "${lawId}" in the catalog.`, 'config.control.law.id'));
		else {
			for (const [k, v] of Object.entries(config.control?.law?.params ?? {})) {
				const decl = law.params.find((x) => x.key === k);
				if (!decl) errors.push(item('law_param_unknown', `The law declares no parameter "${k}".`, `config.control.law.params.${k}`));
				else if ((decl.kind === 'number' || decl.kind === 'integer') && (typeof v !== 'number' || v < decl.min || v > decl.max)) errors.push(item('law_param_invalid', `${decl.label} is from ${decl.min} to ${decl.max}.`, `config.control.law.params.${k}`));
			}
		}
		if (errors.length) return { errors, warnings, derived: null, views: [], synth: null };
		const synth = synthesize(config.target, config.bands, config.synthesis, alarm, abort);
		if (!synth.derived.synthesis.converged) warnings.push(item('synthesis_not_converged', `The offline synthesis did not reach ${syn.tol_db ?? 1.5} dB in every band; the run's law refines from there.`, 'config.synthesis.tol_db'));
		const spacing = Math.max(config.spacing?.cooling_s ?? 1, config.spacing?.settling_s ?? 0.5, config.spacing?.compute_s ?? 0.25);
		const len = Math.ceil((synth.wave.duration_s + ring) * fs);
		const derived: SrsDerived = {
			grid: { fs_hz: fs, record_len: len, actual: false },
			...synth.derived,
			spacing_s: spacing,
			call_budget_s: round((config.control?.law_budget ?? 0.25) * spacing, 3),
			feasibility: config.feasibility
				? host.feasibilityExceeds
					? { verdict: 'exceeded', message: "The pulse's displacement exceeds the shaker's stroke." }
					: { verdict: 'pass', message: "Within the shaker's ratings." }
				: { verdict: 'not_evaluated', message: 'No shaker was given, so feasibility was not evaluated.' }
		};
		return { errors, warnings, derived, views: synth.views, synth };
	}

	// ---- running ----------------------------------------------------------------------------

	function newRun(config: SrsConfig, derived: SrsDerived, synth: Synth): Run {
		const st = host.station();
		const law = laws().find((l) => l.id === (config.control?.law?.id ?? 'srs_ratio')) ?? SRS_RATIO;
		const levels = [...config.schedule.levels_db];
		const pulsesPer = config.schedule.pulses.length === 1 ? levels.map(() => config.schedule.pulses[0]) : [...config.schedule.pulses];
		const startDb = config.schedule.mode === 'manual' ? (config.schedule.start_db ?? levels[0]) : levels[0];
		const n = synth.bands.length;
		return {
			id: `srs-${String(++runCount).padStart(4, '0')}`,
			config: structuredClone(config),
			derived,
			synth,
			state: 'configured',
			phase: 'idle',
			station: st.ref,
			source: st.source,
			mode: config.schedule.mode,
			levels,
			pulsesPer,
			rung: Math.max(0, levels.findIndex((l) => Math.abs(l - startDb) < 1e-9)),
			levelDb: startDb,
			firedAtLevel: 0,
			passedAtLevel: 0,
			failsInRow: 0,
			// the upper bands come in low on the first pulse
			error: synth.bands.map((_, k) => -4.5 * (k / Math.max(1, n - 1)) + 0.4 * Math.sin(k)),
			gains: synth.bands.map(() => 1),
			rows: [],
			lastViews: [],
			t: 0,
			phaseT: 0,
			nextAt: null,
			stopAfterPulse: false,
			stopOutcome: 'operator_stop',
			stopBy: undefined,
			pending: null,
			identification: null,
			prediction: null,
			notch: null,
			feasibility: null,
			end: null,
			alarm: false,
			timer: null,
			law,
			record: null
		};
	}

	function setPhase(r: Run, state: SrsRunState, phase: SrsPhase) {
		r.state = state;
		r.phase = phase;
		r.phaseT = 0;
		stateChanged();
	}

	function startLive() {
		if (liveTimer) return;
		liveTimer = setInterval(() => {
			if (!run || !LIVE.includes(run.state)) {
				if (liveTimer) clearInterval(liveTimer);
				liveTimer = null;
				return;
			}
			host.broadcast('srs.live', liveOf(run) as unknown as Record<string, unknown>);
		}, LIVE_MS);
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
		if (r.config.feasibility && host.feasibilityExceeds && !ack) {
			r.feasibility = { verdict: 'exceeded', message: "The pulse's displacement exceeds the shaker's stroke." };
			setPhase(r, 'blocked', 'feasibility_blocked');
			addLog('feasibility', 'feasibility exceeded; arming needs an acknowledgement');
			refuse('refused', 'feasibility_exceeded', "The pulse's displacement exceeds the shaker's stroke; arm with an acknowledgement to go on.", { verdict: 'exceeded' });
		}
		claimRecord(r);
		if (ack) addLog('override', `armed past feasibility: "${ack}"`);
		r.feasibility = r.derived.feasibility;
		r.derived = { ...r.derived, grid: { ...r.derived.grid, actual: true } };
		setPhase(r, 'armed', 'armed');
		addLog('station', `armed on ${r.station.class}; nothing driven`);
		startLive();
	}

	const capVpk = (r: Run) => r.config.identification?.max_vpk ?? r.config.limits.drive_vpk_ceiling / 10;

	function start(r: Run) {
		setPhase(r, 'running', 'sysid');
		const id = r.config.identification ?? {};
		addLog('state', `identification: a burst at ${id.level_db ?? -20} dB, held at ${round(capVpk(r), 4)} V pk`);
		if (r.timer) clearInterval(r.timer);
		r.timer = setInterval(() => tick(r), TICK_MS);
	}

	function finishIdentification(r: Run) {
		const id = r.config.identification ?? {};
		const min = id.min_mean_coherence ?? 0.8;
		const cap = capVpk(r);
		r.identification = { excitation: id.excitation ?? 'shaped', drive_vpk: round(cap * 0.92, 4), max_vpk: round(cap, 4), coherence_mean: 0.97, min_mean_coherence: min, passed: true };
		const fullVpk = round(0.004 * r.derived.reference.peak_g, 4);
		r.prediction = {
			drive_vpk: fullVpk,
			drive_vpk_ceiling: r.config.limits.drive_vpk_ceiling,
			drive_vpk_abort: r.config.limits.drive_vpk_abort,
			response_peak_g: round(r.derived.reference.peak_g * 1.01, 2),
			velocity_pk_ips: r.derived.reference.velocity_pk_ips,
			displacement_pk_pk_in: r.derived.reference.displacement_pk_pk_in
		};
		addLog('gate', 'identification done: mean coherence 0.97 in the band');
		if (r.config.identification?.review ?? true) {
			setPhase(r, 'review', 'awaiting_review');
			addLog('state', 'holding for review');
		} else proceed(r);
	}

	function proceed(r: Run) {
		const p = r.prediction;
		if (p && p.drive_vpk > p.drive_vpk_abort) {
			end(r, 'aborted', 'aborted', { kind: 'drive_vpk', pulse: 0, at_level_db: 0, vpk: p.drive_vpk, abort_vpk: p.drive_vpk_abort, predicted: true });
			return;
		}
		addLog('law', `seeded ${r.law.name} ${r.law.version}`);
		if (r.config.limit_channels?.channels.length) {
			r.notch = { notched_bands: 2, worst_before_db: 3.1, worst_after_db: -0.6, converged: true };
			addLog('note', 'limit notch planned at full level: 2 bands notched');
		}
		r.nextAt = r.mode === 'auto' ? r.t + r.derived.spacing_s : null;
		setPhase(r, 'running', 'waiting');
	}

	const vpkAt = (r: Run, levelDb: number) => (r.prediction?.drive_vpk ?? 0.4) * 10 ** (levelDb / 20);

	function fire(r: Run) {
		const v = vpkAt(r, r.levelDb);
		if (v > r.config.limits.drive_vpk_abort) {
			end(r, 'aborted', 'aborted', { kind: 'drive_vpk', pulse: r.rows.length + 1, at_level_db: r.levelDb, vpk: round(v), abort_vpk: r.config.limits.drive_vpk_abort, predicted: true });
			return;
		}
		const ceiling = r.config.limits.drive_vpk_ceiling;
		r.pending = { limitScale: v > ceiling ? ceiling / v : 1 };
		setPhase(r, 'running', 'firing');
	}

	function judge(r: Run) {
		const pend = r.pending ?? { limitScale: 1 };
		r.pending = null;
		const { alarm, abort } = envelopeOf(r.config);
		// judged against the target at the level fired: a drive scaled down to the ceiling fires lower
		const scale = 10 ** (r.levelDb / 20) * pend.limitScale;
		const scaleDb = 20 * Math.log10(pend.limitScale);
		const err = r.error;
		const bands = r.synth.bands;
		let worst = 0;
		err.forEach((e, k) => {
			if (Math.abs(e) > Math.abs(err[worst])) worst = k;
		});
		const alarmBands = err.filter((e) => e < -alarm[0] || e > alarm[1]).length;
		const abortBands = err.filter((e) => e < -abort[0] || e > abort[1]).length;
		const passed = alarmBands === 0;
		const limited = r.config.limit_channels?.channels.length ? 2 : null;
		const measuredWave = wave(bands, r.synth.target.map((t, k) => t * 10 ** (err[k] / 20)), scale, r.config.bands.q ?? 10, 0.05);
		const vel = integrate(measuredWave.t_ms, measuredWave.a, G_IPS2);
		const disp = integrate(measuredWave.t_ms, vel, 1);
		const row: SrsPulseRow = {
			index: r.rows.length,
			origin: 'ladder',
			scheduled_db: r.levels[r.rung] ?? r.levelDb,
			commanded_db: round(r.levelDb + scaleDb, 2),
			limit_scale: round(pend.limitScale, 4),
			drive_vpk: round(vpkAt(r, r.levelDb) * pend.limitScale, 4),
			peak_g: round(peak(measuredWave.a), 3),
			velocity_pk_ips: round(peak(vel), 3),
			displacement_pk_pk_in: round(span(disp), 4),
			worst_err_db: round(err[worst], 2),
			worst_band_hz: bands[worst],
			alarm_bands: alarmBands,
			abort_bands: abortBands,
			passed,
			limited_components: limited,
			worst_limit_db: limited === null ? null : -0.6,
			at_s: round(r.t, 2),
			law: { used: true, miss: null, call_ms: 0.2 }
		};
		r.rows.push(row);
		if (host.record === 'problem' && r.record && r.record.streams === 'on' && r.record.problem === null && r.rows.length >= 2) {
			const problem = `Recording of the inputs stream stopped: a write failed: No space left on device (os error 28). The run goes on.`;
			r.record = { ...r.record, problem };
			addLog('record', problem);
		}
		r.firedAtLevel += 1;
		if (passed) r.passedAtLevel += 1;
		r.failsInRow = passed ? 0 : r.failsInRow + 1;
		const measured = r.synth.target.map((t, k) => t * scale * 10 ** (r.error[k] / 20));
		const srs = srsView('SRS', bands, r.synth.target, alarm, abort, scale, measured);
		const [pulseV, velV, dispV] = timeViews(measuredWave, 'Pulse', 'Measured');
		const drive: SpectrumView = {
			view: 'spectrum',
			title: 'Drive',
			x: timeAxis,
			y: { label: 'Drive', unit: 'V', scale: 'linear' },
			series: [{ name: 'Drive played', role: 'drive', x: measuredWave.t_ms, y: Float64Array.from(measuredWave.a, (v) => (v / (r.derived.reference.peak_g || 1)) * row.drive_vpk) }],
			link: 'time'
		};
		r.lastViews = [srs, pulseV, drive, velV, dispV];
		pulseViews.set(row.index, r.lastViews);
		for (const k of [...pulseViews.keys()]) if (k <= row.index - HELD_VIEWS) pulseViews.delete(k);
		addLog('pulse', `pulse ${row.index + 1} at ${r.levelDb} dB: worst band ${row.worst_band_hz} Hz, ${row.worst_err_db} dB; ${passed ? 'pass' : `${alarmBands} bands outside the alarm band`}`);
		const views: View[] = [decimate(srs, 1024), decimate(pulseV, 1024), decimate(drive, 1024)];
		for (const c of host.connections()) c.emit('srs.pulse', { run_id: r.id, pulse: row, views } as unknown as Record<string, unknown>);
		// the law closes most of the error each pulse
		const correct = r.law.builtin || (r.config.control?.law?.params as Record<string, unknown> | undefined)?.correct !== false;
		if (correct) {
			r.gains = r.error.map((e) => 10 ** (-e * 0.6 / 20));
			r.error = r.error.map((e) => e * 0.4);
		}
		publishData(r);
		if (abortBands > 0) {
			end(r, 'aborted', 'aborted', { kind: 'envelope', pulse: row.index + 1, at_level_db: r.levelDb, abort_bands: abortBands, worst_band_hz: bands[worst], worst_err_db: round(err[worst], 2) });
			return;
		}
		if (!passed) r.alarm = true;
		const maxPer = r.config.schedule.max_pulses_per_level ?? 5;
		const onPass = r.config.schedule.advance === 'on_pass';
		if (r.stopAfterPulse) {
			end(r, r.stopOutcome, 'stopped', null, r.stopBy);
			return;
		}
		if (r.mode === 'auto') {
			const need = r.pulsesPer[r.rung] ?? 1;
			const done = onPass ? r.passedAtLevel >= need : r.firedAtLevel >= need;
			if (onPass && !done && r.firedAtLevel >= maxPer) {
				end(r, 'not_converged', 'stopped', { kind: 'not_converged', pulse: row.index + 1, at_level_db: r.levelDb, limit: 'per_level', pulses: r.firedAtLevel });
				return;
			}
			if (done) {
				if (r.rung >= r.levels.length - 1) {
					end(r, 'completed', 'stopped', null);
					return;
				}
				r.rung += 1;
				r.levelDb = r.levels[r.rung];
				r.firedAtLevel = 0;
				r.passedAtLevel = 0;
				addLog('level', `level ${r.levelDb} dB`);
			}
		} else if (onPass && r.failsInRow >= maxPer) {
			end(r, 'not_converged', 'stopped', { kind: 'not_converged', pulse: row.index + 1, at_level_db: r.levelDb, limit: 'per_level', pulses: r.failsInRow });
			return;
		}
		r.nextAt = r.t + r.derived.spacing_s;
		setPhase(r, 'running', 'waiting');
	}

	function stopAfterPulse(r: Run, outcome: SrsOutcome, by?: string) {
		r.stopAfterPulse = true;
		r.stopOutcome = outcome;
		r.stopBy = by;
		r.state = 'stopping';
		r.phase = 'stopping';
		stateChanged();
	}

	function endMessage(outcome: SrsOutcome, trip: SrsTrip | null, by?: string): string {
		switch (outcome) {
			case 'completed':
				return 'The schedule finished.';
			case 'operator_stop':
				return `Stopped by ${by ?? 'the operator'}; the pulse in flight finished.`;
			case 'operator_abort':
				return `Aborted by ${by ?? 'the operator'}.`;
			case 'safe_stop':
				return `Nobody held control for ${host.timeoutS} s, so the run stopped.`;
			case 'not_converged':
				return trip?.kind === 'not_converged' ? `${trip.pulses} pulses at ${trip.at_level_db} dB without a pass.` : 'The iteration limit ended the run.';
			case 'aborted':
				return trip ? `A protection tripped: ${trip.kind.replaceAll('_', ' ')}.` : 'A protection tripped.';
			default:
				return outcome.replaceAll('_', ' ');
		}
	}

	function end(r: Run, outcome: SrsOutcome, phase: SrsPhase, trip: SrsTrip | null, by?: string) {
		if (r.timer) clearInterval(r.timer);
		r.timer = null;
		r.end = { outcome, message: endMessage(outcome, trip, by), at_s: round(r.t, 2), level_db: r.levelDb, pulse: r.rows.length ? r.rows.length : null, trip };
		r.nextAt = null;
		setPhase(r, 'finished', phase);
		addLog(outcome === 'aborted' ? 'trip' : 'state', `finished: ${outcome.replaceAll('_', ' ')}`);
		host.broadcast('srs.live', liveOf(r) as unknown as Record<string, unknown>);
	}

	function tick(r: Run) {
		if (run !== r || r.state === 'finished') return;
		const dt = (TICK_MS / 1000) * host.speed;
		// Nobody holds control: firing pauses; the clock of the safe stop is the seat's.
		if (!host.hasController() && r.phase === 'waiting') return;
		r.t += dt;
		r.phaseT += dt;
		switch (r.phase) {
			case 'sysid':
				if (r.phaseT >= SYSID_S) finishIdentification(r);
				break;
			case 'waiting':
				if (r.nextAt !== null && r.t >= r.nextAt && r.mode === 'auto') fire(r);
				break;
			case 'firing':
			case 'stopping':
				if (r.pending && r.phaseT >= FIRE_S) judge(r);
				break;
		}
	}

	// ---- display ------------------------------------------------------------------------------

	function pulsesTable(r: Run): TableView {
		return {
			view: 'table',
			title: 'Pulses',
			columns: ['Pulse', 'Level', 'Worst band', 'Error', 'Result'],
			rows: r.rows.map((x) => [String(x.index + 1), `${x.commanded_db} dB`, `${x.worst_band_hz ?? '-'} Hz`, `${x.worst_err_db ?? '-'} dB`, x.passed ? 'Pass' : 'Fail'])
		};
	}

	function readings(r: Run): View {
		const rows: ReadingRow[] = [
			{ label: 'Pulses fired', value: r.rows.length, status: 'info' },
			{ label: 'Pulses passed', value: r.rows.filter((x) => x.passed).length, status: 'info' },
			{ label: 'Level', value: r.levelDb, unit: 'dB', status: 'info' },
			{ label: 'Predicted drive at full level', value: r.prediction?.drive_vpk ?? '-', unit: 'V pk', status: 'info' },
			{ label: 'Time between pulses', value: r.derived.spacing_s, unit: 's', status: 'info' }
		];
		return { view: 'reading', title: 'Run', rows };
	}

	function lawReading(r: Run): View {
		const last = r.rows.at(-1);
		return {
			view: 'reading',
			title: 'Control Law',
			rows: [
				{ label: 'Worst band error', value: last?.worst_err_db ?? '-', unit: 'dB', status: 'info' },
				{ label: 'Worst band', value: last?.worst_band_hz ?? '-', unit: 'Hz', status: 'info' },
				{ label: 'Components held back', value: last ? 0 : '-', status: 'info' }
			]
		};
	}

	function identificationViews(r: Run): View[] {
		const n = 160;
		const f = new Float64Array(n);
		const mag = new Float64Array(n);
		const floor = new Float64Array(n);
		const coh = new Float64Array(n);
		const hold = new Float64Array(n).fill(r.identification?.min_mean_coherence ?? 0.8);
		const frac = r.config.identification?.h_floor_frac ?? 0.05;
		const mode = (x: number, fn: number, q: number) => 1 / Math.sqrt((1 - (x / fn) ** 2) ** 2 + (x / (fn * q)) ** 2);
		for (let i = 0; i < n; i++) {
			const fr = 50 * (5000 / 50) ** (i / (n - 1));
			f[i] = fr;
			mag[i] = 40 * (0.6 * mode(fr, 200, 2.5) + 0.4 * mode(fr, 550, 2.5));
			coh[i] = Math.min(0.999, 0.96 + 0.03 * Math.sin(i / 9));
		}
		const top = mag.reduce((m, v) => Math.max(m, v), 0);
		floor.fill(top * frac);
		return [
			{ view: 'spectrum', title: 'Identification Gain', x: freqAxis, y: { label: '|H|', unit: 'g/V', scale: 'log' }, series: [{ name: 'Gain', role: 'measured', x: f, y: mag }, { name: 'Floor', role: 'tolerance', x: f, y: floor }], link: 'frequency' },
			{ view: 'spectrum', title: 'Coherence', x: freqAxis, y: { label: 'Coherence', unit: null, scale: 'linear' }, series: [{ name: 'Coherence', role: 'measured', x: f, y: coh }, { name: 'Hold', role: 'tolerance', x: f, y: hold }], link: 'frequency' }
		];
	}

	/** The table in force, and the law's last gains once it has been called. */
	function componentsViews(r: Run): View[] {
		const x = f64(r.synth.bands);
		const full = r.prediction?.drive_vpk ?? 0.4;
		const amp = f64(r.synth.target.map((t, k) => (full * t * r.gains[k]) / Math.max(1, r.synth.target.reduce((s, v) => s + v, 0))));
		const table: View = { view: 'spectrum', title: 'Components', x: freqAxis, y: { label: 'Amplitude', unit: 'V', scale: 'log' }, series: [{ name: 'Drive amplitude', role: 'drive', x, y: amp }], link: 'srs' };
		if (!r.rows.length) return [table];
		return [table, { view: 'spectrum', title: 'Gains', x: freqAxis, y: { label: 'Gain', unit: null, scale: 'log' }, series: [{ name: 'Last gains', role: 'other', x, y: f64(r.gains) }], link: 'srs' }];
	}

	function limitViews(r: Run, q: 'limit_channels' | 'limit_frf'): View[] {
		const chans = r.config.limit_channels?.channels ?? [];
		const headroom = r.config.limit_channels?.headroom_db ?? 1;
		const x = f64(r.synth.bands);
		// a limit channel is named by its row's description, or ch<index> without one
		const nameOf = (index: number) => host.rows().find((row) => row.index === index)?.description.trim() || `ch${index}`;
		return chans.map((c) => {
			if (q === 'limit_frf') {
				const freqs = c.frf ? f64(c.frf.freqs_hz) : x;
				const mag = c.frf ? f64(c.frf.mag) : f64(r.synth.bands.map((f) => 1 + 3 / (1 + ((f - 500) / 60) ** 2)));
				return { view: 'spectrum', title: `Gain ${nameOf(c.channel)}`, x: freqAxis, y: { label: 'Gain', unit: 'g/V', scale: 'log' }, series: [{ name: c.frf ? 'Supplied' : 'Measured', role: c.frf ? 'reference' : 'measured', x: freqs, y: mag }], link: 'frequency' } as SpectrumView;
			}
			const bound = r.synth.bands.map((_, k) => (c.bound.kind === 'flat' ? c.bound.accel_g : (c.bound.bounds_g[k] ?? NaN)));
			const pred = r.synth.bands.map((f, k) => (Number.isFinite(bound[k]) ? bound[k] * 10 ** ((-headroom - 0.6 - 2 * Math.abs(Math.log2(f / 500))) / 20) : NaN));
			return {
				view: 'spectrum',
				title: `Limit ${nameOf(c.channel)}`,
				x: freqAxis,
				y: srsAxis,
				series: [
					{ name: 'Predicted', role: 'other', x, y: f64(pred) },
					{ name: 'Bound', role: 'limit', x, y: f64(bound) },
					{ name: 'Bound less headroom', role: 'tolerance', x, y: f64(bound.map((b) => b * 10 ** (-headroom / 20))) }
				],
				link: 'srs'
			} as SpectrumView;
		});
	}

	function plotViews(r: Run, pl: SrsSubscribePlot): View[] {
		const pts = pl.points ?? 1024;
		const last = (title: string) => r.lastViews.filter((v) => v.view === 'spectrum' && v.title === title).map((v) => (TIME_QUANTITIES.includes(pl.quantity) ? decimate(v as SpectrumView, pts) : v));
		const fired = r.rows.length > 0;
		switch (pl.quantity) {
			case 'srs':
				return fired ? last('SRS') : [];
			case 'pulse':
				return fired ? [...last('Pulse'), ...last('Drive')] : [];
			case 'velocity':
				return fired ? last('Velocity') : [];
			case 'displacement':
				return fired ? last('Displacement') : [];
			case 'identification':
				return r.identification ? identificationViews(r) : [];
			case 'components':
				return componentsViews(r);
			case 'limit_channels':
				return fired ? limitViews(r, 'limit_channels') : [];
			case 'limit_frf':
				return r.identification ? limitViews(r, 'limit_frf') : [];
			case 'law':
				return [lawReading(r)];
			case 'readings':
				return [readings(r)];
			case 'pulses':
				return [pulsesTable(r)];
		}
	}

	function dataFor(r: Run, plots: SrsSubscribePlot[]) {
		return { ...liveOf(r), plots: plots.map((pl) => ({ id: pl.id, quantity: pl.quantity, views: plotViews(r, pl) })) };
	}

	function publishData(r: Run) {
		for (const [conn, sub] of subs) conn.emit('srs.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
	}

	// ---- refusals -----------------------------------------------------------------------------

	function needRun(): Run {
		if (!run) refuse('refused', 'not_configured', 'SRS shock is not configured; configure a test first.');
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

	/** `points` for a plot: a whole number from 64 to 8192 on a time history, refused on any other. */
	function pointsOf(quantity: SrsQuantity | null, v: unknown, param: string): number | undefined {
		const time = quantity === null || TIME_QUANTITIES.includes(quantity);
		if (v === undefined || v === null) return time ? 1024 : undefined;
		if (!time) bad(param, '"points" applies to the pulse, velocity and displacement plots');
		if (!(Number.isInteger(v) && (v as number) >= 64 && (v as number) <= 8192)) bad(param, '"points" is a whole number from 64 to 8192');
		return v as number;
	}

	function parseSubscribe(params: Record<string, unknown>): { rate: number; plots: SrsSubscribePlot[] } | null {
		const def = params.default;
		if (def !== undefined && def !== null && typeof def !== 'boolean') bad('default', '"default" is true or false');
		if (def === true) {
			if (params.plots !== undefined && params.plots !== null) bad('plots', 'the default subscription takes no plots; send either "default": true or a list of plots');
			return null;
		}
		let rate = 5;
		if (params.rate_hz !== undefined && params.rate_hz !== null) {
			const x = params.rate_hz;
			if (!(typeof x === 'number' && Number.isFinite(x) && x >= 0.5 && x <= 10)) bad('rate_hz', '"rate_hz" is a number of frames per second from 0.5 to 10');
			rate = x as number;
		}
		if (params.plots === undefined || params.plots === null) bad('plots', 'a subscription lists its plots ("plots", possibly empty), or asks for the default with "default": true');
		if (!Array.isArray(params.plots)) bad('plots', '"plots" is a list');
		const list = params.plots as unknown[];
		if (list.length > 64) bad('plots', `a subscription holds at most 64 plots; ${list.length} were sent`);
		const plots: SrsSubscribePlot[] = [];
		list.forEach((p, i) => {
			const at = (f: string) => `plots[${i}].${f}`;
			if (!isObject(p)) bad(`plots[${i}]`, 'each plot is an object');
			const o = p as Record<string, unknown>;
			const extra = Object.keys(o).find((k) => !['id', 'quantity', 'points'].includes(k));
			if (extra) bad(at(extra), `a plot has an id, a quantity and, for a time history, points; not "${extra}"`);
			const id = o.id;
			if (!(typeof id === 'string' && id.length >= 1 && [...id].length <= 64)) bad(at('id'), 'a plot\'s "id" is text of 1 to 64 characters');
			if (plots.some((q) => q.id === id)) bad(at('id'), `the plot id "${id as string}" is used twice; each plot's id is its own`);
			const quantity = typeof o.quantity === 'string' && (QUANTITIES as string[]).includes(o.quantity) ? (o.quantity as SrsQuantity) : null;
			if (!quantity) bad(at('quantity'), `a plot's "quantity" is one of ${QUANTITIES.join(', ')}`);
			const points = pointsOf(quantity, o.points, at('points'));
			plots.push(points === undefined ? { id: id as string, quantity: quantity! } : { id: id as string, quantity: quantity!, points });
		});
		return { rate, plots };
	}

	const shapeOk = (config: Record<string, unknown>) => {
		for (const key of ['target', 'bands', 'schedule', 'limits'] as const) if (!isObject(config[key])) bad(`config.${key}`, `the configuration has no "${key}".`);
		const t = config.target as Record<string, unknown>;
		if (t.kind !== 'flat' && t.kind !== 'breakpoints') bad('config.target.kind', 'a target is "flat" or "breakpoints"');
	};

	return {
		laws,
		isLive,
		liveId: () => (run && isLive() ? run.id : null),
		detach: (conn) => {
			subs.delete(conn);
		},
		seatExpired: () => {
			if (run && isLive()) {
				if (run.phase === 'firing') stopAfterPulse(run, 'safe_stop');
				else end(run, 'safe_stop', 'stopped', null);
			}
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
			'srs.configure': (params, conn) => {
				if (!isObject(params.config)) bad('config', 'Configuring needs a test configuration.');
				shapeOk(params.config as Record<string, unknown>);
				if ((params.config as Record<string, unknown>).capture != null) bad('config.capture', 'An SRS shock run is always recorded in its run folder; "capture" is no longer taken. Use "record" to keep or leave out the streams.');
				const config = params.config as unknown as SrsConfig;
				const checked = check(config);
				if (params.dry_run === true) return { errors: checked.errors, warnings: checked.warnings, derived: checked.derived, views: checked.views };
				host.own(conn);
				notWhileLive('configuring');
				if (checked.errors.length || !checked.derived || !checked.synth) refuse('bad_request', 'config_invalid', `The configuration has ${checked.errors.length} error(s).`, { errors: checked.errors, warnings: checked.warnings });
				if (run?.timer) clearInterval(run.timer);
				run = newRun(config, checked.derived, checked.synth);
				pulseViews.clear();
				log = [];
				addLog('state', `configured "${config.title}" on ${run.station.class}`);
				stateChanged(true);
				return { state: stateOf(run), derived: checked.derived, warnings: checked.warnings };
			},
			'srs.config': () => (run ? { run_id: run.id, config: run.config, derived: run.derived, views: run.synth.views } : { run_id: null, config: null, derived: null, views: [] }),
			'srs.preview': (params) => {
				if (!isObject(params.target)) bad('target', 'A preview needs a target.');
				if (!isObject(params.bands)) bad('bands', 'A preview needs the band grid.');
				const target = params.target as unknown as SrsTarget;
				const bands = params.bands as unknown as SrsBands;
				const synthesis = (isObject(params.synthesis) ? params.synthesis : undefined) as SrsSynthesis | undefined;
				const fs = typeof params.fs_hz === 'number' ? params.fs_hz : 51200;
				const item = (reason: SrsReportItem['reason'], message: string, field: string): SrsReportItem => ({ reason, message, field });
				const env = (isObject(params.envelope) ? params.envelope : {}) as { alarm_db?: [number, number]; abort_db?: [number, number] };
				const errors = [...checkBands(bands, fs, 'bands', item)];
				if (!errors.length) errors.push(...checkTarget(target, bands, 'target', item));
				errors.push(...checkComponents(synthesis?.components, synthesis?.engine ?? 'wavsyn', item));
				if (errors.length) bad((errors[0].field ?? 'target').replace(/^config\./, ''), errors[0].message);
				const s = synthesize(target, bands, synthesis, env.alarm_db ?? [3, 3], env.abort_db ?? [9, 6]);
				return { derived: s.derived, views: s.views };
			},
			'srs.target.import': (params) => {
				if (!absolute(params.path)) bad('path', 'A target file is named by its full path on the computer that runs the test.');
				const path = params.path as string;
				if (/missing|\.bad$/i.test(path)) return { target: null, errors: [{ reason: 'target_file', message: `Row 3 of ${path}: the SRS is not a number.`, row: 2 }], warnings: [] };
				const target: SrsTarget = { kind: 'breakpoints', points: [[100, 30], [800, 120], [2000, 120]] };
				return { target, errors: [], warnings: [] };
			},
			'srs.table.import': (params) => {
				if (!absolute(params.path)) bad('path', 'A component table is named by its full path on the computer that runs the test.');
				if (params.engine !== 'wavsyn' && params.engine !== 'seds') bad('engine', '"engine" is wavsyn or seds');
				const path = params.path as string;
				if (/missing|\.bad$/i.test(path)) return { components: null, errors: [{ reason: 'table_file', message: `Row 2 of ${path}: "half_cycles" is not a whole number.`, row: 1 }], warnings: [] };
				const freqs = [100, 200, 400, 800, 1600];
				const components: SrsComponents =
					params.engine === 'wavsyn'
						? { engine: 'wavsyn', rows: freqs.map((f, i) => ({ freq_hz: f, amplitude_g: 10 + 5 * i, half_cycles: 7, delay_s: 0 })) }
						: { engine: 'seds', rows: freqs.map((f, i) => ({ freq_hz: f, amplitude_g: 10 + 5 * i, zeta: 0.05, delay_s: 0, polarity: 'positive' as const })) };
				return { components, errors: [], warnings: [] };
			},
			'srs.arm': (params, conn) => {
				host.own(conn);
				const r = needRun();
				const ack = params.override_ack;
				if (ack !== undefined && (typeof ack !== 'string' || !ack.trim())) bad('override_ack', "An override acknowledgement is the operator's own words; it cannot be empty.");
				if (r.state === 'finished') refuse('refused', 'run_finished', 'This run has finished; reset or configure again to run.', { outcome: r.end?.outcome });
				if (r.state !== 'configured' && r.state !== 'blocked') wrongState(r, 'Arming');
				notWhileOther();
				arm(r, ack as string | undefined);
				stateChanged();
				return stateOf(r);
			},
			'srs.start': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state === 'finished') refuse('refused', 'run_finished', 'This run has finished; reset or configure again to run.', { outcome: r.end?.outcome });
				if (r.state !== 'configured' && r.state !== 'armed') wrongState(r, 'Starting');
				notWhileOther();
				if (r.state === 'configured') arm(r, undefined);
				start(r);
				return stateOf(r);
			},
			'srs.proceed': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state !== 'review') wrongState(r, 'Proceeding');
				addLog('note', 'the operator reviewed identification and proceeded');
				proceed(r);
				return stateOf(r);
			},
			'srs.fire': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.mode !== 'manual') refuse('refused', 'wrong_mode', "Fire is for the manual ladder; this run's schedule fires itself.", { mode: r.mode });
				if (r.state !== 'running') wrongState(r, 'Fire');
				if (r.phase === 'firing') refuse('refused', 'not_yet', 'A pulse is playing.', { seconds: round(FIRE_S - r.phaseT, 2) });
				if (r.phase !== 'waiting') wrongState(r, 'Fire');
				if (r.nextAt !== null && r.t < r.nextAt) refuse('refused', 'not_yet', `The time between pulses has not passed; the next pulse may fire in ${round(r.nextAt - r.t, 1)} s.`, { seconds: round(r.nextAt - r.t, 2) });
				fire(r);
				return stateOf(r);
			},
			'srs.level': (params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.mode !== 'manual') refuse('refused', 'wrong_mode', 'Level moves are for the manual ladder.', { mode: r.mode });
				if (r.state !== 'running' && r.state !== 'review') wrongState(r, 'A level move');
				const p = params as unknown as ShockLevelParams;
				const hasDelta = 'delta_db' in params && params.delta_db !== undefined;
				const hasRung = 'rung' in params && params.rung !== undefined;
				if (hasDelta === hasRung) bad('delta_db', 'A level move gives either a step of 1 or -1 dB or a level of the table.');
				let target: number;
				if (hasRung) {
					const i = (p as { rung: number }).rung;
					if (!Number.isInteger(i)) bad('rung', '"rung" is a level of the table, from 0');
					if (i < 0 || i >= r.levels.length) refuse('bad_request', 'not_a_rung', `Level ${i} is not in the table (0 to ${r.levels.length - 1}).`, { levels_db: r.levels });
					target = r.levels[i];
				} else {
					const d = (p as { delta_db: number }).delta_db;
					if (d !== 1 && d !== -1) bad('delta_db', '"delta_db" is 1 or -1');
					target = r.levelDb + d;
					if (target > r.levels[r.levels.length - 1] + 1e-9 || target < r.levels[0] - 1e-9) refuse('bad_request', 'not_a_rung', `A step to ${target} dB leaves the table (${r.levels[0]} to ${r.levels[r.levels.length - 1]} dB).`, { levels_db: r.levels });
				}
				const vpk = vpkAt(r, target);
				if (vpk > r.config.limits.drive_vpk_abort) refuse('refused', 'drive_ceiling', `At ${target} dB the identified drive would be ${round(vpk)} V pk, over the drive abort of ${r.config.limits.drive_vpk_abort} V pk.`, { vpk: round(vpk), abort_vpk: r.config.limits.drive_vpk_abort, level_db: target });
				r.levelDb = round(target, 2);
				const i = rungOf(r);
				if (i !== null) r.rung = i;
				r.firedAtLevel = 0;
				r.passedAtLevel = 0;
				r.failsInRow = 0;
				addLog('level', `level ${r.levelDb} dB`);
				stateChanged(true);
				return { level_db: r.levelDb };
			},
			'srs.stop': (_params, conn) => {
				if (run && isLive()) {
					const by = host.clientName(conn);
					addLog('note', `stop requested by ${by}`);
					if (run.phase === 'firing') stopAfterPulse(run, 'operator_stop', by);
					else end(run, 'operator_stop', 'stopped', null, by);
				}
				return stateOf(run);
			},
			'srs.abort': (_params, conn) => {
				if (run && isLive()) {
					const by = host.clientName(conn);
					addLog('note', `abort requested by ${by}`);
					end(run, 'operator_abort', 'aborted', null, by);
				}
				return stateOf(run);
			},
			'srs.reset': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				notWhileLive('resetting');
				const checked = check(r.config);
				run = newRun(r.config, checked.derived ?? r.derived, checked.synth ?? r.synth);
				pulseViews.clear();
				log = [];
				addLog('state', `reset; configured "${r.config.title}" again`);
				stateChanged(true);
				return stateOf(run);
			},
			'srs.clear': (_params, conn) => {
				host.own(conn);
				notWhileLive('clearing');
				run = null;
				pulseViews.clear();
				log = [];
				stateChanged(true);
				return stateOf(null);
			},
			'srs.state': () => stateOf(run),
			'srs.log': (params) => {
				const since = typeof params.since === 'number' ? Math.max(0, Math.floor(params.since)) : 0;
				return { entries: log.slice(since), next: log.length };
			},
			'srs.pulses': (params) => {
				const since = typeof params.since === 'number' ? Math.max(0, Math.floor(params.since)) : 0;
				const rows = run?.rows ?? [];
				return { pulses: rows.slice(since), next: rows.length };
			},
			'srs.pulse': (params) => {
				const i = params.index;
				if (!(Number.isInteger(i) && (i as number) >= 0)) bad('index', '"index" is a pulse, from 0');
				const points = pointsOf(null, params.points, 'points') ?? 1024;
				const r = needRun();
				const row = r.rows[i as number];
				if (!row) throw new MockError('not_found', `No pulse ${String(i)} has fired in run ${r.id} (${r.rows.length} so far).`);
				const views = pulseViews.get(i as number);
				if (!views) throw new MockError('not_found', `The views of pulse ${String(i)} are no longer held; the last ${HELD_VIEWS} pulses' are. Its row is in srs.pulses.`);
				return { pulse: row, views: views.map((v) => (v.view === 'spectrum' && v.x.unit === 'ms' ? decimate(v, points) : v)) };
			},
			'srs.subscribe': (params, conn) => {
				const sub = parseSubscribe(params);
				if (!sub) {
					subs.delete(conn);
					return { default: true, rate_hz: 5, plots: null, quantities: QUANTITIES };
				}
				subs.set(conn, sub);
				if (run && run.state !== 'configured' && run.state !== 'blocked') {
					const r = run;
					setTimeout(() => {
						if (subs.get(conn) === sub && run === r) conn.emit('srs.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
					}, 30);
				}
				return { default: false, rate_hz: sub.rate, plots: sub.plots, quantities: QUANTITIES };
			}
		}
	};
}
