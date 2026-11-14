/**
 * Mock Mixed Mode (schema/PROTOCOL.md, "Mixed Mode"), on the mock control service's run slot and
 * controller seat.
 *
 * A believable closed-loop run for every kind of test the parts list makes (sine-on-random,
 * random-on-random and both): identification with a noise burst, the coherence check, the review
 * hold, then the floor alone at the first level until it has equalized, then every tone and
 * narrowband ramping in over the part ramp. A fixed part stands still (for its dwell, when it has
 * one), a swept part bounces between its band's ends at its rate and counts its one-way sweeps, a
 * follower reads its leader's frequency times its ratio. Parts time stops on a hold; a part's
 * window opens and closes on it; a part switched off live ramps out and holds its clock. The
 * automatic ladder moves every part together and the run ends complete with the controlled stop.
 * Stop ramps; abort ends at once; with nobody in control the run goes on and safe-stops after the
 * timeout. A title holding "abort tone" trips the first tone's abort a few seconds after the parts
 * come in, so the run-end naming a part can be seen.
 *
 * It refuses what the service refuses, by the same reasons and on the same paths: a configuration
 * with no floor or two, a part kind or a schedule kind it does not know, a part id used twice, a
 * follower of nothing, of itself or in a loop, a part outside the floor's band anywhere in its
 * motion, two tones closer than two tracking widths, a law of the wrong world, an amplitude or a
 * width given both ways or neither, a selector on a plot that does not take it, a part switch the
 * run cannot take. Every value is synthetic, made only so the shell has something to draw. Nothing
 * here is the service's control.
 */

import type { ChannelRow, DataSource, LogEntry, ReadingRow, RefusalReason, RunRecordRef, Series, SpectrumView, StationRef, View } from '../types';
import type { LawEntry } from '../mimo';
import type {
	MixedAlarm,
	MixedConfig,
	MixedDerived,
	MixedDerivedPart,
	MixedEnd,
	MixedFloorLines,
	MixedToneJudging,
	MixedLiveHead,
	MixedLiveQuantity,
	MixedLogKind,
	MixedOutcome,
	MixedPart,
	MixedPartRow,
	MixedPhase,
	MixedReportItem,
	MixedRunState,
	MixedState,
	MixedSubscribePlot,
	MixedTrip
} from '../mixed';
import type { SineProfile } from '../sine';
import { binEdges } from './bins';
import type { ShockMockHost } from './shock';
import { MockError, type MockConnection, type MockMethod } from './socket';

const TICK_MS = 50;
const LIVE_MS = 200;
const SYSID_S = 1.5;
const GATE_S = 0.3;
const FLOOR_ONLY_S = 3;
const HISTORY_S = 60;
const HISTORY_POINTS = 1200;
const MAX_LINES = 320;
const G_IPS2 = 386.089;

const QUANTITIES: MixedLiveQuantity[] = ['control', 'residual', 'drive', 'response', 'time', 'coherence', 'identification', 'amplitude', 'frequency', 'level', 'law', 'readings'];
const WITH_CHANNELS: MixedLiveQuantity[] = ['response', 'time'];
const WITH_PARTS: MixedLiveQuantity[] = ['amplitude', 'frequency', 'level'];

function refuse(code: 'refused' | 'bad_request', reason: RefusalReason | 'part_refused' | 'adjust_refused', message: string, detail?: Record<string, unknown>): never {
	throw new MockError(code, message, reason as RefusalReason, detail);
}

function bad(param: string, message: string): never {
	refuse('bad_request', 'bad_params', message, { param });
}

const round = (v: number, d = 3) => Math.round(v * 10 ** d) / 10 ** d;
const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const f64 = (a: number[]) => Float64Array.from(a);

const builtIn = { status: 'built_in' as const, date: null, tool_version: null, cases: [], hardware: true, message: 'Built in: cleared for hardware.' };

/** The floor's built-in law, as the service lists it in the `single` world. */
const HINV: LawEntry = {
	id: 'hinv',
	world: 'single',
	builtin: true,
	name: 'Inverse plant',
	version: '0.1.0',
	author: 'Hertzforge contributors',
	summary: 'Shapes the drive spectrum by the inverse of the identified plant, correcting each line by its error in the last loop.',
	sha256: null,
	check: builtIn,
	params: [],
	diagnostics: [{ key: 'mean_correction_db', label: 'Mean correction', unit: 'dB', help: 'The mean change the last call made to the drive lines.', layout: null }],
	shapes: null,
	needs: null,
	live: []
};

// ---------------------------------------------------------------------------------------------
// The floor and the plant

/** The floor's PSD at each breakpoint (anchors, and slopes from the row before), g²/Hz. */
function floorPoints(c: MixedConfig): [number, number][] {
	const floor = c.parts.find((p) => p.kind === 'floor');
	if (!floor || floor.kind !== 'floor') return [];
	const out: [number, number][] = [];
	for (const b of floor.breakpoints) {
		if (b.psd_g2_hz !== undefined && b.psd_g2_hz !== null) out.push([b.freq_hz, b.psd_g2_hz]);
		else if (out.length) {
			const [f0, p0] = out[out.length - 1];
			out.push([b.freq_hz, p0 * 10 ** (((b.slope_db_oct ?? 0) * Math.log2(b.freq_hz / f0)) / 10)]);
		}
	}
	return out;
}

function floorAt(pts: [number, number][], f: number): number {
	if (!pts.length) return 0;
	if (f <= pts[0][0]) return pts[0][1];
	for (let i = 1; i < pts.length; i++) {
		const [f0, p0] = pts[i - 1];
		const [f1, p1] = pts[i];
		if (f <= f1) return p0 * (p1 / p0) ** (Math.log(f / f0) / Math.log(f1 / f0));
	}
	return pts[pts.length - 1][1];
}

const mode = (f: number, fn: number, q: number) => 1 / Math.sqrt((1 - (f / fn) ** 2) ** 2 + (f / (fn * q)) ** 2);
/** The simulated plant: two modes, 200 and 550 Hz, Q 2.5; g per volt. */
const plantGain = (f: number) => 40 * (0.6 * mode(f, 200, 2.5) + 0.4 * mode(f, 550, 2.5));

function profileAt(p: SineProfile, f: number): number {
	const rows = p.rows;
	const accel = (r: (typeof rows)[number]) => (r.quantity === 'accel_g' ? r.value : r.quantity === 'velocity_ips' ? (r.value * 2 * Math.PI * f) / G_IPS2 : (r.value / 2) * (2 * Math.PI * f) ** 2 / G_IPS2);
	if (f <= rows[0].freq_hz) return accel(rows[0]);
	for (let i = 1; i < rows.length; i++) if (f <= rows[i].freq_hz) return accel(rows[i - 1]) + ((accel(rows[i]) - accel(rows[i - 1])) * (f - rows[i - 1].freq_hz)) / (rows[i].freq_hz - rows[i - 1].freq_hz);
	return accel(rows[rows.length - 1]);
}

// ---------------------------------------------------------------------------------------------
// Checking a configuration

type Item = MixedReportItem;
const item = (reason: Item['reason'], message: string, field?: string, row?: number): Item => ({ reason, message, ...(field !== undefined ? { field } : {}), ...(row !== undefined ? { row } : {}) });

type Moving = Exclude<MixedPart, { kind: 'floor' }>;

/** The lowest and highest frequency of a part's whole motion, through its follow ratios. */
function rangeOf(parts: MixedPart[], p: Moving, seen = new Set<string>()): [number, number] | null {
	const s = p.schedule;
	if (s.kind === 'fixed') return [s.freq_hz, s.freq_hz];
	if (s.kind === 'sweep') return [s.band_hz[0], s.band_hz[1]];
	if (s.kind === 'follow') {
		if (seen.has(p.id)) return null;
		seen.add(p.id);
		const lead = parts.find((x): x is Moving => x.id === s.of && x.kind !== 'floor');
		const r = lead ? rangeOf(parts, lead, seen) : null;
		return r ? [r[0] * s.ratio, r[1] * s.ratio] : null;
	}
	return null;
}

/** The pipeline's lag the service assumes for the simulator before the station arms, frames. */
const MOCK_LAG_FRAMES = 128;

/** The part a follower's chain starts from, and the product of the ratios along it. */
function rootOf(parts: MixedPart[], p: Moving): { id: string; factor: number } {
	let at: Moving = p;
	let factor = 1;
	for (let n = 0; n < 32 && at.schedule.kind === 'follow'; n++) {
		const s = at.schedule;
		const lead = parts.find((x): x is Moving => x.id === s.of && x.kind !== 'floor');
		if (!lead) break;
		factor *= s.ratio;
		at = lead;
	}
	return { id: at.id, factor };
}

/**
 * The closest two tones come over their whole motion. Two parts moved by the same leader are
 * compared at the same moment (their ratios apart, at the leader's lowest frequency); any others by
 * the gap between their ranges.
 */
function spacing(parts: MixedPart[], a: Moving, b: Moving, ra: [number, number], rb: [number, number]): number {
	const x = rootOf(parts, a);
	const y = rootOf(parts, b);
	if (x.id === y.id) {
		const root = parts.find((q): q is Moving => q.id === x.id && q.kind !== 'floor');
		const lo = root ? (rangeOf(parts, root)?.[0] ?? 0) : 0;
		return Math.abs(x.factor - y.factor) * lo;
	}
	return Math.max(0, Math.max(ra[0], rb[0]) - Math.min(ra[1], rb[1]));
}

function widthRange(p: Extract<MixedPart, { kind: 'narrowband' }>, r: [number, number]): [number, number] {
	return p.width_share !== undefined ? [p.width_share * r[0], p.width_share * r[1]] : [p.width_hz ?? 0, p.width_hz ?? 0];
}

/** Content errors and warnings, by name, at the path inside the configuration. */
function checkConfig(c: MixedConfig, rows: ChannelRow[], toneLaws: LawEntry[]): { errors: Item[]; warnings: Item[] } {
	const errors: Item[] = [];
	const warnings: Item[] = [];
	const floors = c.parts.filter((p) => p.kind === 'floor');
	if (!floors.length) errors.push(item('no_floor', 'A test has a floor: one broadband random part.', 'parts'));
	if (floors.length > 1) errors.push(item('too_many_floors', 'A test has exactly one floor.', `parts[${c.parts.indexOf(floors[1])}]`));
	const tones = c.parts.filter((p) => p.kind === 'tone');
	const bands = c.parts.filter((p) => p.kind === 'narrowband');
	if (tones.length > 12) errors.push(item('too_many_tones', 'At most 12 tones.', 'parts'));
	if (bands.length > 12) errors.push(item('too_many_bands', 'At most 12 narrowbands.', 'parts'));
	const ids = new Set<string>();
	c.parts.forEach((p, i) => {
		if (ids.has(p.id)) errors.push(item('duplicate_part_id', `Another part is "${p.id}" too.`, `parts[${i}].id`));
		ids.add(p.id);
	});
	const pts = floorPoints(c);
	const fb = floors[0]?.kind === 'floor' ? floors[0].breakpoints : [];
	const band: [number, number] = fb.length ? [fb[0].freq_hz, fb[fb.length - 1].freq_hz] : [0, 0];
	fb.forEach((b, r) => {
		if (b.freq_hz <= 0) errors.push(item('frequency_not_positive', `Row ${r + 1}: the frequency is above 0.`, `parts[${c.parts.indexOf(floors[0])}].breakpoints`, r));
		if (r > 0 && b.freq_hz <= fb[r - 1].freq_hz) errors.push(item('frequency_not_ascending', `Row ${r + 1}: the frequencies rise from row to row.`, `parts[${c.parts.indexOf(floors[0])}].breakpoints`, r));
	});
	if (fb.length && fb[0].psd_g2_hz === undefined) errors.push(item('first_row_not_anchor', 'Row 1: the first row is a PSD, not a slope.', `parts[${c.parts.indexOf(floors[0])}].breakpoints`, 0));
	const moving = c.parts.filter((p): p is Moving => p.kind !== 'floor');
	c.parts.forEach((p, i) => {
		if (p.kind === 'floor') return;
		const at = `parts[${i}]`;
		const s = p.schedule;
		if (s.kind === 'segments') return errors.push(item('schedule_unsupported', 'A schedule that sweeps, dwells and sweeps on is not built yet.', `${at}.schedule.kind`));
		if (s.kind === 'follow') {
			if (s.of === p.id) return errors.push(item('follow_self', `${p.name ?? p.id} cannot follow itself.`, `${at}.schedule.of`));
			if (!moving.some((x) => x.id === s.of)) return errors.push(item('follow_unknown_part', `There is no tone or narrowband "${s.of}" to follow.`, `${at}.schedule.of`));
		}
		const r = rangeOf(c.parts, p);
		if (!r) return errors.push(item('follow_loop', `${p.name ?? p.id} follows a part that follows it back.`, `${at}.schedule.of`));
		if (s.kind === 'sweep' && !(s.band_hz[0] > 0 && s.band_hz[0] < s.band_hz[1])) return errors.push(item('out_of_range', 'The sweep band is low below high, both above 0.', `${at}.schedule.band_hz`));
		const half = p.kind === 'narrowband' ? widthRange(p, r)[1] / 2 : 0;
		if (pts.length && (r[0] - half < band[0] || r[1] + half > band[1])) errors.push(item('part_out_of_band', `${p.name ?? p.id} goes outside the floor's band, ${band[0]} to ${band[1]} Hz, in its motion (${round(r[0] - half, 1)} to ${round(r[1] + half, 1)} Hz).`, at));
		if (p.kind === 'tone' && p.profile) {
			const rows = p.profile.rows;
			if (rows.length && (rows[0].freq_hz > r[0] || rows[rows.length - 1].freq_hz < r[1])) errors.push(item('profile_not_covering' as Item['reason'], `${p.name ?? p.id}'s amplitude profile does not cover its motion, ${r[0]} to ${r[1]} Hz.`, `${at}.profile`));
		}
		if (!(p.window?.on_s === undefined || p.window.on_s >= 0) || (p.window?.off_s !== undefined && p.window.off_s !== null && p.window.off_s <= (p.window.on_s ?? 0))) errors.push(item('out_of_range', 'A window opens at 0 s or later and closes after it opens.', `${at}.window`));
	});
	const track = c.control?.tracking?.hz ?? 0.5;
	if (c.control?.tracking && (c.control.tracking as { kind: string }).kind !== 'fixed') errors.push(item('out_of_range', 'Mixed Mode tracks every tone at one fixed width.', 'control.tracking'));
	if (!(track > 0 && track <= 10)) errors.push(item('out_of_range', 'The tracking width is above 0 and at most 10 Hz.', 'control.tracking.hz'));
	const toneParts = c.parts.map((p, i) => ({ p, i })).filter((x): x is { p: Extract<MixedPart, { kind: 'tone' }>; i: number } => x.p.kind === 'tone');
	// A moving tone's reading lags its reference by its rate times the pipeline's lag (the
	// simulator's, before the station arms): over half the tracking width it is refused, over a
	// fifth it is warned, at the top of its motion, as the service does.
	for (const { p, i } of toneParts) {
		const root = rootOf(c.parts, p);
		const lead = c.parts.find((x) => x.id === root.id);
		const r = rangeOf(c.parts, p);
		if (!lead || lead.kind === 'floor' || lead.schedule.kind !== 'sweep' || !r) continue;
		const rate = lead.schedule.rate as { law: string; oct_per_min?: number; hz_per_s?: number };
		const hzPerS = rate.law === 'log' ? (r[1] * Math.LN2 * (rate.oct_per_min ?? 0)) / 60 : root.factor * (rate.hz_per_s ?? 0);
		const ratio = (hzPerS * MOCK_LAG_FRAMES) / c.acquisition.fs_hz / track;
		const words = `${p.name ?? p.id} moves ${round(hzPerS, 2)} Hz/s at ${round(r[1], 1)} Hz: its reading lags by ${round(ratio, 2)} of the tracking width.`;
		if (ratio > 0.5) errors.push(item('tracking_lag' as Item['reason'], `${words} Slow the sweep or widen the tracking.`, `parts[${i}].schedule`));
		else if (ratio > 0.2) warnings.push(item('tracking_lag' as Item['reason'], words, `parts[${i}].schedule`));
	}
	for (let a = 0; a < toneParts.length; a++)
		for (let b = a + 1; b < toneParts.length; b++) {
			const A = toneParts[a].p;
			const B = toneParts[b].p;
			const ra = rangeOf(c.parts, A);
			const rb = rangeOf(c.parts, B);
			if (!ra || !rb) continue;
			const gap = spacing(c.parts, A, B, ra, rb);
			if (gap < 2 * track) errors.push(item('parts_too_close', `${B.name ?? B.id} comes within ${round(gap, 2)} Hz of ${A.name ?? A.id}; tones stay at least ${round(2 * track, 2)} Hz apart.`, `parts[${toneParts[b].i}]`));
		}
	const df = c.acquisition.fs_hz / c.acquisition.n_pts;
	if (df > track) errors.push(item('out_of_range', `The lines are ${round(df, 3)} Hz apart; they are no wider than the tracking width, ${track} Hz.`, 'acquisition.n_pts'));
	const ex = c.exclusion_hz ?? 2;
	if (!(ex > 0 && ex <= 50)) errors.push(item('out_of_range', 'The exclusion width is above 0 and at most 50 Hz.', 'exclusion_hz'));
	const ceiling = c.limits.drive_vpk_ceiling;
	if (!(ceiling > 0 && ceiling <= 10)) errors.push(item('out_of_range', 'The drive ceiling is above 0 and at most 10 V peak.', 'limits.drive_vpk_ceiling'));
	if (c.limits.drive_vpk_abort < ceiling) errors.push(item('abort_below_ceiling' as Item['reason'], 'The drive abort is at least the drive ceiling.', 'limits.drive_vpk_abort'));
	const floorLaw = c.control?.law?.floor?.id ?? 'hinv';
	if (floorLaw !== 'hinv') errors.push(toneLaws.some((l) => l.id === floorLaw) ? item('law_wrong_world' as Item['reason'], 'That is a Sine law; the floor takes a law of the single-output Random world.', 'control.law.floor') : item('law_unknown' as Item['reason'], `No floor law "${floorLaw}" in the catalog.`, 'control.law.floor'));
	const toneLaw = c.control?.law?.tones?.id ?? 'servo_blend';
	if (toneLaw === 'hinv') errors.push(item('law_wrong_world' as Item['reason'], 'That is a Random law; the tones take a law of the Sine world.', 'control.law.tones'));
	else if (!toneLaws.some((l) => l.id === toneLaw)) errors.push(item('law_unknown' as Item['reason'], `No tone law "${toneLaw}" in the catalog.`, 'control.law.tones'));
	if (!rows.some((r) => r.enabled && r.role === 'control')) errors.push(item('no_control_channel' as Item['reason'], 'The channel table has no enabled control row.'));
	if (!rows.some((r) => r.enabled && r.role === 'drive')) errors.push(item('no_drive_channel' as Item['reason'], 'The channel table has no enabled drive row.'));
	(c.limits.channels ?? []).forEach((l, i) => {
		const row = rows.find((r) => r.index === l.channel && r.enabled);
		if (!row) errors.push(item('limit_unknown_channel' as Item['reason'], `Channel ${l.channel} is not an enabled input of the channel table.`, `limits.channels[${i}].channel`));
		else if (row.role === 'control') errors.push(item('limit_on_control_channel' as Item['reason'], `Channel ${l.channel} is a control channel; a limit channel is another input.`, `limits.channels[${i}].channel`));
	});
	c.parts.forEach((p, i) => {
		if (p.kind === 'floor') return;
		const al = p.alarm_db;
		const ab = p.abort_db;
		if (al && ab && (al[0] > ab[0] || al[1] > ab[1])) warnings.push(item('alarm_wider_than_abort' as Item['reason'], `${p.name ?? p.id}: the alarm band is wider than the abort band.`, `parts[${i}].alarm_db`));
	});
	return { errors, warnings };
}

/** The shape of a configuration: `bad_params` at the request's own path for anything malformed. */
function shapeCheck(config: unknown) {
	if (!isObject(config)) bad('config', '"config" is a Mixed Mode configuration.');
	if (!Array.isArray(config.parts)) bad('config.parts', '"parts" is a list of parts.');
	(config.parts as unknown[]).forEach((raw, i) => {
		const at = `config.parts[${i}]`;
		if (!isObject(raw)) bad(at, `${at} is a part.`);
		if (typeof raw.id !== 'string' || !/^[A-Za-z0-9_-]{1,32}$/.test(raw.id)) bad(`${at}.id`, 'A part id is 1 to 32 letters, digits, _ or -.');
		if (raw.kind === 'tone' && (raw.amplitude_g === undefined) === (raw.profile === undefined)) bad(`${at}.amplitude_g`, 'A tone has amplitude_g or a profile: exactly one.');
		if (raw.kind === 'narrowband' && (raw.width_hz === undefined) === (raw.width_share === undefined)) bad(`${at}.width_hz`, 'A narrowband has width_hz or width_share: exactly one.');
		if (raw.kind === 'tone' || raw.kind === 'narrowband') {
			const s = raw.schedule;
			if (!isObject(s) || typeof s.kind !== 'string') bad(`${at}.schedule`, 'A part has a schedule.');
		}
	});
	if (!isObject(config.acquisition) || !isNum(config.acquisition.fs_hz) || !isNum(config.acquisition.n_pts) || !isNum(config.acquisition.n_avg)) bad('config.acquisition', '"acquisition" has fs_hz, n_pts and n_avg.');
	if (!isObject(config.schedule) || !Array.isArray(config.schedule.levels)) bad('config.schedule', '"schedule" is Random\'s ladder.');
	if (!isObject(config.limits) || !isNum(config.limits.drive_vpk_ceiling) || !isNum(config.limits.drive_vpk_abort)) bad('config.limits', '"limits" has drive_vpk_ceiling and drive_vpk_abort.');
}

// ---------------------------------------------------------------------------------------------
// What the service computes at configure

interface Grid {
	/** The lines drawn: the band's grid lines, fewer when there are many. */
	lines: number[];
	edges: Float64Array;
	df: number;
}

function gridOf(band: [number, number], fs: number, nPts: number): Grid {
	const df = fs / nPts;
	const step = Math.max(df, (band[1] - band[0]) / MAX_LINES);
	const lines: number[] = [];
	for (let f = band[0]; f <= band[1] + 1e-9; f += step) lines.push(round(f, 4));
	return { lines, edges: binEdges(lines), df: step };
}

const nameOf = (p: MixedPart, n: number) => p.name ?? (p.kind === 'floor' ? 'Floor' : p.kind === 'tone' ? `Tone ${n}` : `Narrowband ${n}`);

function namesOf(c: MixedConfig): Map<string, string> {
	const out = new Map<string, string>();
	let t = 0;
	let b = 0;
	for (const p of c.parts) out.set(p.id, nameOf(p, p.kind === 'tone' ? ++t : p.kind === 'narrowband' ? ++b : 1));
	return out;
}

function toneAmp(p: Extract<MixedPart, { kind: 'tone' }>, f: number): number {
	return p.profile ? profileAt(p.profile, f) : (p.amplitude_g ?? 1);
}

/** The judging time a tone's single reading needs, as the service predicts it, modelled: the floor in
 * the tracking width against the tone gives a reading's scatter; it is averaged until the scatter is a
 * third of the alarm band, up to 8 s, past which the tolerances are widened to the scatter at 8 s. */
function toneJudging(c: MixedConfig, pts: [number, number][], t: Extract<MixedPart, { kind: 'tone' }>, trackHz: number): MixedToneJudging {
	const r = rangeOf(c.parts, t) ?? [0, 0];
	const worst = Math.max(...[r[0], r[1]].map((f) => Math.sqrt(floorAt(pts, f) * trackHz) / (toneAmp(t, f) / Math.SQRT2)));
	const sigma = 8.686 * worst;
	const alarm = (t.alarm_db ?? [1.5, 1.5]) as [number, number];
	const abort = (t.abort_db ?? [3, 3]) as [number, number];
	const need = 0.5 * ((3 * sigma) / Math.min(...alarm)) ** 2;
	const widened = need > 8;
	const at8 = 3 * sigma * Math.sqrt(0.5 / 8);
	const alarmJ: [number, number] = widened ? [round(Math.max(alarm[0], at8), 2), round(Math.max(alarm[1], at8), 2)] : alarm;
	return { part: t.id, judge_s: round(Math.min(8, Math.max(0.5, need)), 2), sigma_db: round(sigma, 2), alarm_db: alarmJ, abort_db: abort, widened };
}

function derive(c: MixedConfig, rows: ChannelRow[], toneLaws: LawEntry[], actual: boolean, fsActual?: number): MixedDerived {
	const pts = floorPoints(c);
	const band: [number, number] = [pts[0]?.[0] ?? 20, pts[pts.length - 1]?.[0] ?? 2000];
	const fs = fsActual ?? c.acquisition.fs_hz;
	const df = fs / c.acquisition.n_pts;
	const names = namesOf(c);
	const grid = gridOf(band, fs, c.acquisition.n_pts);
	let floorMs = 0;
	for (let i = 0; i < grid.lines.length; i++) floorMs += floorAt(pts, grid.lines[i]) * (grid.edges[i + 1] - grid.edges[i]);
	const parts: MixedDerivedPart[] = c.parts.map((p) => {
		const name = names.get(p.id) ?? p.id;
		if (p.kind === 'floor') return { id: p.id, kind: 'floor', name, freq_hz: null, width_hz: null, ms_g2: round(floorMs, 4), peak_g: null };
		const r = rangeOf(c.parts, p) ?? [0, 0];
		if (p.kind === 'tone') {
			const peak = Math.max(toneAmp(p, r[0]), toneAmp(p, r[1]));
			return { id: p.id, kind: 'tone', name, freq_hz: r, width_hz: null, ms_g2: round((peak * peak) / 2, 4), peak_g: round(peak, 4) };
		}
		const w = widthRange(p, r);
		return { id: p.id, kind: 'narrowband', name, freq_hz: r, width_hz: [round(w[0], 3), round(w[1], 3)], ms_g2: round(p.level_g2_hz * w[1], 4), peak_g: null };
	});
	const sum = (k: MixedDerivedPart['kind']) => parts.filter((p) => p.kind === k).reduce((n, p) => n + p.ms_g2, 0);
	const tonesMs = sum('tone');
	const bandsMs = sum('narrowband');
	const total = floorMs + tonesMs + bandsMs;
	const randomRms = Math.sqrt(floorMs + bandsMs);
	const crest = c.feasibility?.crest_factor ?? 3;
	const tonePeaks = parts.filter((p) => p.kind === 'tone').reduce((n, p) => n + (p.peak_g ?? 0), 0);
	let vel = 0;
	let disp = 0;
	for (const p of parts) {
		if (p.kind !== 'tone' || !p.freq_hz) continue;
		const w = 2 * Math.PI * p.freq_hz[0];
		vel += ((p.peak_g ?? 0) * G_IPS2) / w;
		disp += (2 * (p.peak_g ?? 0) * G_IPS2) / (w * w);
	}
	const wr = 2 * Math.PI * band[0] * 2;
	vel += (crest * randomRms * G_IPS2) / wr / 4;
	disp += (2 * crest * randomRms * G_IPS2) / (wr * wr) / 4;
	const tracking = track(c);
	const exclusion = c.parts.flatMap((p) => {
		if (p.kind === 'floor') return [];
		const r = rangeOf(c.parts, p);
		if (!r || r[0] !== r[1]) return [];
		const half = p.kind === 'tone' ? (c.exclusion_hz ?? 2) : widthRange(p, r)[0] / 2 + 2 * df;
		return [{ part: p.id, lo_hz: round(r[0] - half, 3), hi_hz: round(r[0] + half, 3), lines: Math.round((2 * half) / df) + 1 }];
	});
	const tones = c.parts.filter((p): p is Extract<MixedPart, { kind: 'tone' }> => p.kind === 'tone');
	let closest: MixedDerived['closest_tones'] = null;
	for (let a = 0; a < tones.length; a++)
		for (let b = a + 1; b < tones.length; b++) {
			const ra = rangeOf(c.parts, tones[a]);
			const rb = rangeOf(c.parts, tones[b]);
			if (!ra || !rb) continue;
			const gap = spacing(c.parts, tones[a], tones[b], ra, rb);
			if (!closest || gap < closest.spacing_hz) closest = { parts: [tones[a].id, tones[b].id], spacing_hz: round(gap, 3), min_hz: round(2 * tracking, 3) };
		}
	const levels = c.schedule.levels;
	const loopS = (c.acquisition.n_pts / fs) * ((c.acquisition.n_avg + 1) / 2);
	const tickFrames = Math.round(((c.acquisition.tick_ms ?? 10) * fs) / 1000);
	const tickS = tickFrames / fs;
	const toneLaw = toneLaws.find((l) => l.id === (c.control?.law?.tones?.id ?? 'servo_blend')) ?? toneLaws[0];
	const feasibility = c.feasibility ? { verdict: 'pass' as const, message: 'Within the shaker ratings given.' } : { verdict: 'not_evaluated' as const, message: 'No shaker record was given.' };
	return {
		band_hz: band,
		grid: { fs_hz: fs, n_pts: c.acquisition.n_pts, df_hz: round(df, 6), lines: Math.round((band[1] - band[0]) / df) + 1, loop_s: round(loopS, 4), tick_frames: tickFrames, tick_s: round(tickS, 7), actual },
		control_channels: rows.filter((r) => r.enabled && r.role === 'control').map((r) => r.index),
		parts,
		reference: { floor_ms_g2: round(floorMs, 4), tones_ms_g2: round(tonesMs, 4), bands_ms_g2: round(bandsMs, 4), total_ms_g2: round(total, 4), total_rms_g: round(Math.sqrt(total), 4), random_rms_g: round(randomRms, 4) },
		kinematics: { peak_accel_g: Number((tonePeaks + crest * randomRms).toPrecision(4)), peak_velocity_ips: Number(vel.toPrecision(4)), peak_disp_pp_in: Number(disp.toPrecision(4)), ...feasibility },
		exclusion,
		closest_tones: tones.length >= 2 ? closest : null,
		tracking: tones.map((t) => ({ part: t.id, verdict: 'ok' as const, ratio: t.schedule.kind === 'fixed' ? 0 : 0.002, at_hz: null, message: null })),
		tone_judging: tones.map((t) => toneJudging(c, pts, t, track(c))),
		test_s: c.schedule.mode === 'auto' ? round(levels.slice(0, -1).reduce((n, l) => n + (l.dwell_s ?? 0), 0) + c.schedule.full_level_s, 3) : null,
		floor_equalize_s: round(((c.control?.control_dof ?? 120) / 2 + 1) * (loopS / Math.max(1, c.acquisition?.n_avg ?? 4)) + loopS, 3),
		identification_s: round((c.control?.pretest?.averages ?? 4) * loopS, 3),
		call_budget_s: { floor: round((c.control?.law_guards?.floor?.law_budget ?? 0.25) * loopS, 7), tones: round((c.control?.law_guards?.tones?.law_budget ?? 0.25) * tickS, 7) },
		law: {
			floor: { id: HINV.id, name: HINV.name, version: HINV.version, sha256: null, live: [], hardware: true },
			tones: { id: toneLaw.id, name: toneLaw.name, version: toneLaw.version, sha256: toneLaw.sha256, live: toneLaw.live ?? [], hardware: toneLaw.check.hardware }
		},
		feasibility
	};
}

function track(c: MixedConfig): number {
	return c.control?.tracking?.hz ?? 0.5;
}

const freqAxis = { label: 'Frequency', unit: 'Hz', scale: 'log' as const };
const psdAxis = { label: 'PSD', unit: 'g²/Hz', scale: 'log' as const };

function stepped(name: string, role: Series['role'], g: Grid, y: number[]): Series {
	return { name, role, x: f64(g.lines), y: f64(y), shape: 'steps', edges: g.edges };
}

/** The reference (the floor and each narrowband at `centers`) on the drawn lines, at `db`, with its bands. */
function referenceSeries(c: MixedConfig, g: Grid, centers: Map<string, number>, db: number): Series[] {
	const pts = floorPoints(c);
	const floor = c.parts.find((p) => p.kind === 'floor');
	const al = (floor?.kind === 'floor' && floor.alarm_db) || [3, 3];
	const ab = (floor?.kind === 'floor' && floor.abort_db) || [6, 6];
	const scale = 10 ** (db / 10);
	const ref = g.lines.map((f) => {
		let v = floorAt(pts, f);
		for (const p of c.parts) {
			if (p.kind !== 'narrowband') continue;
			const at = centers.get(p.id);
			if (at === undefined) continue;
			const w = p.width_share !== undefined ? p.width_share * at : (p.width_hz ?? 0);
			if (Math.abs(f - at) <= w / 2) v += p.level_g2_hz;
		}
		return v * scale;
	});
	const by = (d: number) => ref.map((v) => v * 10 ** (d / 10));
	return [stepped('Reference', 'reference', g, ref), stepped('Alarm high', 'tolerance', g, by(al[1])), stepped('Alarm low', 'tolerance', g, by(-al[0])), stepped('Abort high', 'abort', g, by(ab[1])), stepped('Abort low', 'abort', g, by(-ab[0]))];
}

function startCenters(c: MixedConfig): Map<string, number> {
	const out = new Map<string, number>();
	for (const p of c.parts) {
		if (p.kind === 'floor') continue;
		const r = rangeOf(c.parts, p);
		if (!r) continue;
		const s = p.schedule;
		out.set(p.id, s.kind === 'sweep' ? (s.start_hz ?? s.band_hz[0]) : r[0]);
	}
	return out;
}

function configureViews(c: MixedConfig): SpectrumView[] {
	const pts = floorPoints(c);
	const band: [number, number] = [pts[0][0], pts[pts.length - 1][0]];
	const g = gridOf(band, c.acquisition.fs_hz, c.acquisition.n_pts);
	const names = namesOf(c);
	const tones = c.parts.filter((p): p is Extract<MixedPart, { kind: 'tone' }> => p.kind === 'tone');
	return [
		{ view: 'spectrum', title: 'Reference', x: freqAxis, y: psdAxis, link: 'frequency', series: referenceSeries(c, g, startCenters(c), 0) },
		{
			view: 'spectrum',
			title: 'Tones',
			x: freqAxis,
			y: { label: 'Amplitude', unit: 'g', scale: 'log' },
			series: tones.map((t) => {
				const r = rangeOf(c.parts, t) ?? [0, 0];
				const xs = r[0] === r[1] ? [r[0]] : Array.from({ length: 24 }, (_, i) => r[0] * (r[1] / r[0]) ** (i / 23));
				return { name: names.get(t.id) ?? t.id, role: 'reference' as const, x: f64(xs), y: f64(xs.map((f) => toneAmp(t, f))), shape: 'line' as const };
			})
		}
	];
}

// ---------------------------------------------------------------------------------------------
// The run

interface PartRun {
	part: Exclude<MixedPart, { kind: 'floor' }>;
	enabled: boolean;
	scale: number;
	clock: number;
	freq: number;
	dir: 'up' | 'down';
	sweeps: number;
	ended: boolean;
	measured: number | null;
	target: number | null;
	settledT: number;
	history: { t: number[]; measured: (number | null)[]; target: number[]; freq: number[]; level: { t: number[]; measured: number[]; target: number[]; dof: number[] } };
}

interface Run {
	id: string;
	config: MixedConfig;
	derived: MixedDerived;
	views: SpectrumView[];
	grid: Grid;
	names: Map<string, string>;
	state: MixedRunState;
	phase: MixedPhase;
	station: StationRef;
	source: DataSource;
	mode: 'auto' | 'manual';
	levels: number[];
	rung: number;
	commanded: number;
	level: number;
	held: boolean;
	t: number;
	elapsed: number;
	partsT: number | null;
	atLevelT: number;
	phaseT: number;
	loop: number;
	tick: number;
	loopAcc: number;
	parts: PartRun[];
	floorMeasured: number | null;
	toneValues: Record<string, number>;
	stopOutcome: MixedOutcome;
	pendingTrip: MixedTrip | null;
	alarmFlags: Set<string>;
	identification: MixedState['identification'];
	end: MixedEnd | null;
	timer: ReturnType<typeof setInterval> | null;
	lawLive: Record<string, number>;
	/** The run's record, from arm on. */
	record: RunRecordRef | null;
}

export interface MixedMock {
	methods: Record<string, MockMethod>;
	/** The floor's laws (the `single` world), built in first. */
	floorLaws(): LawEntry[];
	isLive(): boolean;
	liveId(): string | null;
	detach(conn: MockConnection): void;
	seatExpired(): void;
}

const LIVE: MixedRunState[] = ['armed', 'running', 'review', 'stopping'];

export interface MixedMockHost extends ShockMockHost {
	/** The tones' laws: the Sine world's, as the Sine mock lists them. */
	toneLaws(): LawEntry[];
	/** Where a run's folder is made (default `~/Hertzforge_Data`). */
	dataRoot?: string;
	/** A run's record: refused at arm (`unwritable`), or a stream stops while it runs (`problem`). */
	record?: 'unwritable' | 'problem';
	/** A run folder's name: the time it armed, the test and the run's id. */
	folderName?: (at: Date, runId: string, test: string) => string;
}

export function createMixedMock(host: MixedMockHost): MixedMock {
	let run: Run | null = null;
	let runCount = 0;
	let log: LogEntry[] = [];
	let lastStateKey = '';
	const subs = new Map<MockConnection, { rate: number; plots: MixedSubscribePlot[]; acc: number }>();
	let liveTimer: ReturnType<typeof setInterval> | null = null;
	let configured: { config: MixedConfig; derived: MixedDerived; views: SpectrumView[] } | null = null;

	const isLive = () => run !== null && LIVE.includes(run.state);

	function addLog(kind: MixedLogKind, text: string) {
		const e = { seq: log.length, at_unix: Date.now() / 1000, kind, text } as LogEntry;
		log.push(e);
		if (run) host.broadcast('mixed.log', { run_id: run.id, entries: [e] });
	}

	function rowOf(r: Run, p: PartRun | null): MixedPartRow {
		if (!p) {
			const floor = r.config.parts.find((x) => x.kind === 'floor')!;
			const target = Math.sqrt(r.derived.reference.floor_ms_g2) * 10 ** (r.level / 20);
			const driving = ['floor_only', 'engaged', 'full_level', 'ramping_down'].includes(r.phase);
			const m = driving ? r.floorMeasured : null;
			const err = m !== null && target > 0 ? round(20 * Math.log10(m / target), 2) : null;
			return {
				id: floor.id,
				kind: 'floor',
				name: r.names.get(floor.id) ?? 'Floor',
				enabled: true,
				on: driving,
				scale: driving ? 1 : 0,
				freq_hz: null,
				direction: null,
				follows: null,
				judge_s: null,
				judged_error_db: null,
				judged_alarm_db: null,
				judged_abort_db: null,
				tolerance_widened: null,
				sweeps: null,
				rate_oct_per_min: null,
				rate_hz_per_s: null,
				width_hz: null,
				unit: 'g',
				target: driving ? round(target, 4) : null,
				measured: m === null ? null : round(m, 4),
				error_db: err,
				alarm: false,
				drive_vpk: null,
				drive_vrms: driving ? round(target / 12, 4) : null,
				gain_db: null,
				settled: driving && r.phaseT > 1.5,
				limiting: false,
				dof: driving ? Math.min(120, 8 * r.loop + 8) : null,
				lines: driving ? floorLines(r) : null,
				law: r.phase === 'sysid' || r.phase === 'coherence_gate' || r.state === 'review' ? null : { misses: 0, capped: 0 }
			};
		}
		const part = p.part;
		const s = part.schedule;
		const lead = s.kind === 'follow' ? r.parts.find((x) => x.part.id === s.of) : null;
		const sweep = s.kind === 'sweep' ? s : lead?.part.schedule.kind === 'sweep' ? lead.part.schedule : null;
		const moving = !!sweep && !r.held && p.scale > 0;
		// As the service sends it: a follower carries its leader's direction.
		const dir = s.kind === 'sweep' ? p.dir : sweep && lead ? lead.dir : null;
		const signed = (v: number) => (dir === 'down' ? -v : v);
		const rate = sweep?.rate;
		const tone = part.kind === 'tone';
		const err = p.measured !== null && p.target !== null && p.target > 0 ? round((tone ? 20 : 10) * Math.log10(p.measured / p.target), 2) : null;
		const al = part.alarm_db ?? (tone ? [1.5, 1.5] : [3, 3]);
		const width = part.kind === 'narrowband' ? (part.width_share !== undefined ? part.width_share * p.freq : (part.width_hz ?? 0)) : null;
		return {
			id: part.id,
			kind: part.kind,
			name: r.names.get(part.id) ?? part.id,
			enabled: p.enabled,
			on: p.scale > 0,
			scale: round(p.scale, 3),
			freq_hz: round(p.freq, 2),
			direction: dir,
			follows: s.kind === 'follow' ? { of: s.of, ratio: s.ratio } : null,
			...judged(r, part.id, tone, err, p.settledT),
			sweeps: s.kind === 'sweep' ? { done: p.sweeps, of: s.end?.sweeps ?? null } : null,
			rate_oct_per_min: rate?.law === 'log' ? (moving ? signed(rate.oct_per_min) : 0) : null,
			rate_hz_per_s: rate?.law === 'linear' ? (moving ? signed(rate.hz_per_s * (s.kind === 'follow' ? s.ratio : 1)) : 0) : null,
			width_hz: width === null ? null : round(width, 3),
			unit: tone ? 'g' : 'g²/Hz',
			target: p.target === null ? null : round(p.target, 5),
			measured: p.measured === null ? null : round(p.measured, 5),
			error_db: err,
			alarm: err !== null && p.settledT > 1 && (err > al[1] || err < -al[0]),
			drive_vpk: tone && p.target !== null ? round((p.target * p.scale) / plantGain(p.freq), 4) : null,
			// The core reports no drive for a narrowband.
			drive_vrms: null,
			gain_db: tone ? null : p.scale > 0 ? round(0.4 * Math.sin(r.t / 7), 2) : null,
			settled: p.settledT > 1,
			limiting: false,
			dof: tone ? null : p.scale > 0 ? Math.min(400, Math.round(16 + p.settledT * 12)) : null,
			lines: null,
			law: tone ? (r.phase === 'floor_only' || p.scale > 0 || p.settledT > 0 ? { misses: 0, capped: 0 } : null) : null
		};
	}

	/** A tone's judged reading: the derived judging once it has settled for its judging time; null on the floor and narrowbands. */
	function judged(r: Run, id: string, tone: boolean, err: number | null, settledT: number) {
		if (!tone) return { judge_s: null, judged_error_db: null, judged_alarm_db: null, judged_abort_db: null, tolerance_widened: null };
		const j = r.derived.tone_judging.find((x) => x.part === id);
		const ready = !!j && err !== null && settledT >= j.judge_s;
		return {
			judge_s: j && settledT > 0 ? j.judge_s : null,
			// the averaged reading scatters less than the single one
			judged_error_db: ready ? round(err! * 0.4, 2) : null,
			judged_alarm_db: j && settledT > 0 ? j.alarm_db : null,
			judged_abort_db: j && settledT > 0 ? ([Math.max(j.abort_db[0], j.alarm_db[0]), Math.max(j.abort_db[1], j.alarm_db[1])] as [number, number]) : null,
			tolerance_widened: j?.widened ?? false
		};
	}

	/** The floor's lines: the rms abort is judged on the lines that qualify, all of them after a few loops. */
	function floorLines(r: Run): MixedFloorLines {
		const clean = r.derived.grid.lines - r.derived.exclusion.reduce((n, e) => n + e.lines, 0);
		return { in_band: r.derived.grid.lines, clean, alarm: 0, abort: 0, rms_judged: r.loop >= 3 ? clean : Math.round((clean * r.loop) / 3) };
	}

	function rows(r: Run): MixedPartRow[] {
		const out: MixedPartRow[] = [];
		for (const part of r.config.parts) out.push(part.kind === 'floor' ? rowOf(r, null) : rowOf(r, r.parts.find((p) => p.part.id === part.id)!));
		return out;
	}

	function stateOf(r: Run | null): MixedState {
		if (!r)
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
				full_level_s: null,
				t_s: null,
				elapsed_s: null,
				parts_s: null,
				at_level_s: null,
				remaining_at_level_s: null,
				held: false,
				alarm: false,
				limiting: false,
				parts: [],
				total: null,
				identification: null,
				law: null,
				feasibility: null,
				end: null
			};
		const prows = rows(r);
		const driving = r.state === 'running' || r.state === 'stopping' || r.state === 'finished';
		const tl = host.toneLaws().find((l) => l.id === (r.config.control?.law?.tones?.id ?? 'servo_blend')) ?? host.toneLaws()[0];
		return {
			run_id: r.id,
			state: r.state,
			phase: r.phase,
			title: r.config.title ?? '',
			source: r.state === 'unconfigured' || r.state === 'configured' ? null : r.source,
			station: r.state === 'configured' ? null : r.station,
			grid: r.derived.grid,
			clock: r.state === 'configured' ? null : 'shared',
			mode: r.mode,
			levels_db: r.levels,
			level_db: driving && r.state !== 'finished' ? round(r.commanded, 2) : r.state === 'finished' ? round(r.commanded, 2) : null,
			full_level_s: r.config.schedule.full_level_s,
			t_s: driving ? round(r.t, 2) : null,
			elapsed_s: driving ? round(r.elapsed, 2) : null,
			parts_s: r.partsT === null ? null : round(r.partsT, 2),
			at_level_s: driving ? round(r.atLevelT, 2) : null,
			remaining_at_level_s: driving && r.mode === 'auto' ? round(Math.max(0, levelDwell(r) - r.atLevelT), 2) : null,
			held: r.held,
			alarm: prows.some((p) => p.alarm),
			limiting: false,
			parts: prows,
			total: totalOf(r, prows),
			identification: r.identification,
			law:
				r.state === 'running' && !['sysid', 'coherence_gate'].includes(r.phase)
					? {
							floor: { id: HINV.id, name: HINV.name, version: HINV.version, sha256: null, misses: 0, capped: 0, live: {} },
							tones: { id: tl.id, name: tl.name, version: tl.version, sha256: tl.sha256, misses: 0, capped: 0, live: { ...r.lawLive } }
						}
					: null,
			feasibility: r.derived.feasibility,
			end: r.end,
			...(r.record ? { record: { ...r.record } } : {})
		};
	}

	function totalOf(r: Run, prows: MixedPartRow[]): MixedState['total'] {
		const on = prows.filter((p) => p.on && p.measured !== null);
		if (!on.length) return { rms_g: null, reference_rms_g: null, drive_vpk: null, drive_vrms: null };
		const ms = (p: MixedPartRow, v: number) => (p.kind === 'floor' ? v * v : p.kind === 'tone' ? (v * v) / 2 : v * (p.width_hz ?? 0));
		const meas = on.reduce((n, p) => n + ms(p, p.measured!), 0);
		const ref = on.reduce((n, p) => n + ms(p, p.target ?? 0), 0);
		const random = Math.sqrt(on.filter((p) => p.kind !== 'tone').reduce((n, p) => n + (p.drive_vrms ?? 0) ** 2, 0));
		const tones = on.reduce((n, p) => n + (p.drive_vpk ?? 0), 0);
		return { rms_g: round(Math.sqrt(meas), 4), reference_rms_g: round(Math.sqrt(ref), 4), drive_vpk: round(tones + 3 * random, 4), drive_vrms: round(Math.sqrt(random ** 2 + tones ** 2 / 2), 4) };
	}

	function stateChanged() {
		if (!run) return;
		const s = stateOf(run);
		const key = JSON.stringify([s.state, s.phase, s.mode, s.level_db, s.held, s.alarm, s.limiting, s.end?.outcome ?? null, s.parts.map((p) => [p.on, p.alarm, p.settled]), s.record ?? null]);
		if (key === lastStateKey) return;
		lastStateKey = key;
		host.broadcast('mixed.state', s as unknown as Record<string, unknown>);
	}

	function levelDwell(r: Run): number {
		const last = r.levels.length - 1;
		if (r.rung >= last) return r.config.schedule.full_level_s;
		return r.config.schedule.levels[r.rung]?.dwell_s ?? 30;
	}

	function newRun(config: MixedConfig, derived: MixedDerived, views: SpectrumView[]): Run {
		const { source, ref } = host.station();
		const pts = floorPoints(config);
		const band: [number, number] = [pts[0][0], pts[pts.length - 1][0]];
		const centers = startCenters(config);
		return {
			id: `mixed-${String(++runCount).padStart(4, '0')}`,
			config,
			derived,
			views,
			grid: gridOf(band, config.acquisition.fs_hz, config.acquisition.n_pts),
			names: namesOf(config),
			state: 'configured',
			phase: 'idle',
			station: ref,
			source,
			mode: config.schedule.mode,
			levels: config.schedule.levels.map((l) => l.db),
			rung: 0,
			commanded: config.schedule.levels[0]?.db ?? 0,
			level: -60,
			held: false,
			t: 0,
			elapsed: 0,
			partsT: null,
			atLevelT: 0,
			phaseT: 0,
			loop: 0,
			tick: 0,
			loopAcc: 0,
			parts: config.parts
				.filter((p): p is Exclude<MixedPart, { kind: 'floor' }> => p.kind !== 'floor')
				.map((part) => ({
					part,
					enabled: part.enabled ?? true,
					scale: 0,
					clock: 0,
					freq: centers.get(part.id) ?? 100,
					dir: part.schedule.kind === 'sweep' ? (part.schedule.direction ?? 'up') : 'up',
					sweeps: 0,
					ended: false,
					measured: null,
					target: null,
					settledT: 0,
					history: { t: [], measured: [], target: [], freq: [], level: { t: [], measured: [], target: [], dof: [] } }
				})),
			floorMeasured: null,
			toneValues: {},
			stopOutcome: 'operator_stop',
			pendingTrip: null,
			alarmFlags: new Set(),
			identification: null,
			end: null,
			timer: null,
			lawLive: Object.fromEntries((host.toneLaws().find((l) => l.id === (config.control?.law?.tones?.id ?? 'servo_blend'))?.params ?? []).filter((p) => p.kind === 'number').map((p) => [p.key, ((config.control?.law?.tones?.params ?? {}) as Record<string, number>)[p.key] ?? (p as { default: number }).default])),
			record: null
		};
	}

	function setPhase(r: Run, state: MixedRunState, phase: MixedPhase) {
		r.state = state;
		r.phase = phase;
		r.phaseT = 0;
		stateChanged();
	}

	function finish(r: Run, outcome: MixedOutcome, message: string, trip: MixedTrip | null = null) {
		if (r.timer) clearInterval(r.timer);
		r.timer = null;
		r.state = 'finished';
		r.phase = outcome === 'held' ? 'coherence_hold' : outcome === 'completed' || outcome === 'operator_stop' || outcome === 'safe_stop' || outcome === 'law_fault' ? 'stopped' : 'aborted';
		for (const p of r.parts) p.scale = 0;
		r.end = { outcome, message, at_s: round(r.t, 2), level_db: round(r.commanded, 2), ramp_s: outcome === 'operator_abort' || outcome === 'aborted' ? null : (r.config.control?.shutdown_ramp_s ?? 3), trip };
		addLog('state', `Run ended: ${message}`);
		stateChanged();
		publish(r);
	}

	/** The part's frequency at its own clock: its schedule, or its leader's times the ratio. */
	function advancePart(r: Run, p: PartRun, dt: number) {
		const s = p.part.schedule;
		if (s.kind === 'follow') {
			const lead = r.parts.find((x) => x.part.id === s.of);
			if (lead) {
				p.freq = lead.freq * s.ratio;
				p.dir = lead.dir;
			}
			return;
		}
		if (s.kind === 'fixed') {
			p.freq = s.freq_hz;
			const e = s.end;
			if (e && (('seconds' in e && p.clock >= e.seconds) || ('cycles' in e && p.clock * s.freq_hz >= e.cycles))) p.ended = true;
			return;
		}
		if (s.kind !== 'sweep') return;
		const [lo, hi] = s.band_hz;
		let f = p.freq;
		if (s.rate.law === 'log') f *= 2 ** (((p.dir === 'up' ? 1 : -1) * s.rate.oct_per_min * dt) / 60);
		else f += (p.dir === 'up' ? 1 : -1) * s.rate.hz_per_s * dt;
		if (f >= hi) {
			f = hi;
			p.dir = 'down';
			p.sweeps++;
		} else if (f <= lo) {
			f = lo;
			p.dir = 'up';
			p.sweeps++;
		}
		p.freq = f;
		const e = s.end;
		if (e && ((e.sweeps !== null && e.sweeps !== undefined && p.sweeps >= e.sweeps) || (e.duration_s !== null && e.duration_s !== undefined && p.clock >= e.duration_s))) p.ended = true;
	}

	function tick(r: Run) {
		const dt = (TICK_MS / 1000) * host.speed;
		r.phaseT += dt;
		const c = r.config;
		if (r.state === 'running' && r.phase === 'sysid') {
			if (r.phaseT >= SYSID_S) {
				setPhase(r, 'running', 'coherence_gate');
				addLog('gate', 'Identification done; checking the coherence.');
			}
			publish(r);
			return;
		}
		if (r.state === 'running' && r.phase === 'coherence_gate') {
			if (r.phaseT >= GATE_S) {
				r.identification = { level_db: c.control?.pretest?.level_db ?? -20, band_coherence: 0.97, gate_coherence: c.control?.coherence_floor ?? 0.5, lag_frames: 0, trusted_frac: 0.96, passed: true };
				addLog('gate', 'Coherence 0.97 against 0.50: passed.');
				if (c.control?.review_after_sysid === false) return engage(r);
				r.state = 'review';
				r.phase = 'awaiting_review';
				r.phaseT = 0;
				stateChanged();
			}
			publish(r);
			return;
		}
		if (r.state !== 'running' && r.state !== 'stopping') return;
		if (host.record === 'problem' && r.record && r.record.streams === 'on' && r.record.problem === null && r.t >= 2) {
			const problem = 'Recording of the inputs stream stopped: a write failed: no space left on the disk. The run goes on.';
			r.record = { ...r.record, problem };
			addLog('record', problem);
			lastStateKey = '';
			stateChanged();
		}
		r.t += dt;
		if (!r.held) r.elapsed += dt;
		const ramp = r.config.control?.part_ramp_s ?? 3;
		if (r.state === 'stopping') {
			r.level -= (40 / (r.config.control?.shutdown_ramp_s ?? 3)) * dt;
			if (r.level <= r.commanded - 40) {
				const trip = r.pendingTrip;
				if (trip) finish(r, trip.kind === 'law_fault' ? 'law_fault' : 'aborted', endMessage(r, trip), trip);
				else finish(r, r.stopOutcome, r.stopOutcome === 'completed' ? 'The schedule finished; every part ramped down.' : r.stopOutcome === 'safe_stop' ? 'Nobody held control for the timeout; every part ramped down.' : 'Stopped by the operator; every part ramped down.');
				return;
			}
		} else {
			const step = (6 * dt) / Math.max(0.1, (r.config.control?.level_ramp_s ?? 3) / 3);
			r.level = r.level < r.commanded ? Math.min(r.commanded, r.level + step) : Math.max(r.commanded, r.level - step);
		}
		r.loopAcc += dt;
		const loopS = r.derived.grid.loop_s;
		while (r.loopAcc >= loopS) {
			r.loopAcc -= loopS;
			r.loop++;
		}
		r.tick += Math.round((dt * 1000) / (c.acquisition.tick_ms ?? 10));
		const noise = (k: number) => 1 + 0.02 * Math.sin(r.t * 3.1 + k * 1.7) + 0.01 * Math.sin(r.t * 11.3 + k);
		const lvlA = 10 ** (r.level / 20);
		r.floorMeasured = Math.sqrt(r.derived.reference.floor_ms_g2) * lvlA * noise(0);
		if (r.phase === 'floor_only' && r.state === 'running' && r.phaseT >= Math.min(FLOOR_ONLY_S, c.control?.floor_settle_s ?? 30)) {
			r.partsT = 0;
			setPhase(r, 'running', r.rung >= r.levels.length - 1 ? 'full_level' : 'engaged');
			addLog('state', 'The floor has equalized; the tones and narrowbands come in.');
		}
		const partsOn = r.partsT !== null;
		if (partsOn && !r.held && r.state === 'running') {
			r.partsT! += dt;
			r.atLevelT += dt;
			if (r.mode === 'auto' && r.atLevelT >= levelDwell(r)) {
				if (r.rung >= r.levels.length - 1) {
					r.stopOutcome = 'completed';
					r.state = 'stopping';
					r.phase = 'ramping_down';
					stateChanged();
				} else {
					r.rung++;
					r.commanded = r.levels[r.rung];
					r.atLevelT = 0;
					if (r.rung >= r.levels.length - 1) r.phase = 'full_level';
					addLog('level', `Level ${r.commanded.toFixed(1)} dB.`);
					stateChanged();
				}
			}
		}
		const pts = floorPoints(c);
		for (const p of r.parts) {
			const w = p.part.window ?? {};
			const open = partsOn && r.partsT! >= (w.on_s ?? 0) && (w.off_s === null || w.off_s === undefined || r.partsT! < w.off_s);
			const want = open && p.enabled && !p.ended && r.state === 'running';
			const was = p.scale > 0;
			if (r.state === 'stopping') p.scale = Math.max(0, p.scale);
			else p.scale = want ? Math.min(1, p.scale + dt / ramp) : Math.max(0, p.scale - dt / ramp);
			if (!was && p.scale > 0) addLog('part', `${r.names.get(p.part.id)} on.`);
			if (was && p.scale === 0) addLog('part', `${r.names.get(p.part.id)} off${p.ended ? ': its schedule ended' : ''}.`);
			if (want && !r.held) p.clock += dt;
		}
		// leaders first, then followers
		for (const p of r.parts) if (p.part.schedule.kind !== 'follow' && p.scale > 0 && !r.held && r.state === 'running') advancePart(r, p, dt);
		for (const p of r.parts) if (p.part.schedule.kind === 'follow') advancePart(r, p, dt);
		for (const [k, p] of r.parts.entries()) {
			if (p.part.kind === 'tone') {
				p.target = toneAmp(p.part, p.freq) * lvlA;
				p.measured = p.scale > 0 ? p.target * p.scale * noise(k + 1) : null;
			} else {
				const lvl = 10 ** (r.level / 10);
				p.target = (floorAt(pts, p.freq) + p.part.level_g2_hz) * lvl;
				p.measured = p.scale > 0 ? p.target * p.scale * noise(k + 1) ** 2 : null;
			}
			p.settledT = p.scale >= 0.999 ? p.settledT + dt : 0;
			const h = p.history;
			if (p.scale > 0 || h.t.length) {
				h.t.push(round(r.t, 3));
				h.measured.push(p.scale >= 0.999 ? p.measured : null);
				h.target.push(p.target);
				h.freq.push(p.freq);
				while (h.t.length > HISTORY_POINTS || (h.t.length && h.t[0] < r.t - HISTORY_S)) {
					h.t.shift();
					h.measured.shift();
					h.target.shift();
					h.freq.shift();
				}
				if (p.part.kind === 'narrowband' && p.scale >= 0.999 && (!h.level.t.length || r.t - h.level.t[h.level.t.length - 1] >= Math.min(loopS, 0.5))) {
					h.level.t.push(round(r.t, 3));
					h.level.measured.push(p.measured ?? 0);
					h.level.target.push(p.target);
					h.level.dof.push(Math.min(400, Math.round(16 + p.settledT * 12)));
					while (h.level.t.length && h.level.t[0] < r.t - HISTORY_S) for (const a of [h.level.t, h.level.measured, h.level.target, h.level.dof]) a.shift();
				}
			}
		}
		const values = r.lawLive;
		const totalStep = round(0.05 * Math.sin(r.t), 3);
		r.toneValues = { servo_step_db: totalStep, prediction_step_db: round(totalStep / 2, 3), total_step_db: totalStep, servo_level_db: round(-6 + (values.blend_w ?? 0.5), 2) };
		// a part's abort: the first tone, a few seconds after it settled, when the title asks for it
		if (r.state === 'running' && /abort tone/i.test(c.title ?? '')) {
			const t = r.parts.find((p) => p.part.kind === 'tone');
			if (t && t.settledT > 2.5) {
				const limit = (t.target ?? 0) * 10 ** ((t.part.abort_db?.[1] ?? 3) / 20);
				const trip: MixedTrip = { kind: 'abort_band', part: t.part.id, side: 'high', error_db: round((t.part.abort_db?.[1] ?? 3) + 0.4, 2), measured_g: round(limit * 1.05, 4), limit_g: round(limit, 4), at_level_db: round(r.level, 2), at_s: round(r.t, 2), at_loop: r.loop, at_tick: r.tick, at_hz: round(t.freq, 1) };
				addLog('trip', endMessage(r, trip));
				finish(r, 'aborted', endMessage(r, trip), trip);
				return;
			}
		}
		stateChanged();
	}

	function endMessage(r: Run, trip: MixedTrip): string {
		const name = trip.part ? (r.names.get(trip.part) ?? trip.part) : 'The drive';
		if (trip.kind === 'abort_band') return `${name} abort: ${(trip.error_db ?? 0).toFixed(1)} dB over its target at ${trip.at_hz?.toFixed(1)} Hz, at ${trip.at_level_db.toFixed(1)} dB.`;
		return `${name}: ${trip.kind.replaceAll('_', ' ')}.`;
	}

	function engage(r: Run) {
		r.level = r.commanded - (r.config.control?.soft_start_db ?? 9);
		r.t = 0;
		r.elapsed = 0;
		setPhase(r, 'running', 'floor_only');
		addLog('state', `Floor engaged alone at ${r.commanded.toFixed(1)} dB.`);
	}

	// ---- frames ----------------------------------------------------------------------------

	function centersNow(r: Run): Map<string, number> {
		const out = new Map<string, number>();
		for (const p of r.parts) if (p.part.kind === 'narrowband' && p.scale > 0) out.set(p.part.id, p.freq);
		return out;
	}

	function controlView(r: Run, title: 'Control' | 'Residual'): SpectrumView {
		const g = r.grid;
		const refs = referenceSeries(r.config, g, centersNow(r), r.level);
		const ref = refs[0].y as Float64Array;
		const y = Array.from(ref, (v, i) => v * (0.82 + 0.36 * Math.abs(Math.sin(i * 12.9898 + r.loop * 0.7))));
		if (title === 'Control')
			for (const p of r.parts) {
				if (p.part.kind !== 'tone' || p.measured === null) continue;
				const i = g.lines.findIndex((f, k) => p.freq >= g.edges[k] && p.freq < g.edges[k + 1]);
				if (i >= 0) y[i] += (p.measured * p.measured) / 2 / (g.edges[i + 1] - g.edges[i]);
			}
		const regions = r.derived.exclusion
			.filter((e) => (r.parts.find((p) => p.part.id === e.part)?.scale ?? 0) > 0)
			.map((e) => ({ x0: e.lo_hz, x1: e.hi_hz, kind: 'cut' as const, label: `Left out: ${r.names.get(e.part) ?? e.part}` }));
		const started = r.loop > 0 || r.phaseT > 0.5;
		return { view: 'spectrum', title, x: freqAxis, y: psdAxis, link: 'frequency', series: [...(started ? [stepped(title, 'measured', g, y)] : []), ...refs], regions };
	}

	function driveView(r: Run): SpectrumView {
		const g = r.grid;
		const ref = referenceSeries(r.config, g, centersNow(r), r.level)[0].y as Float64Array;
		return { view: 'spectrum', title: 'Drive', x: freqAxis, y: { label: 'PSD', unit: 'V²/Hz', scale: 'log' }, link: 'frequency', series: [stepped('Drive', 'drive', g, Array.from(ref, (v, i) => v / plantGain(g.lines[i]) ** 2))] };
	}

	function readingsView(r: Run): View {
		const prows = rows(r);
		const total = totalOf(r, prows);
		const out: ReadingRow[] = [
			{ label: 'Level', value: round(r.level, 1), unit: 'dB', status: 'info' },
			{ label: 'Elapsed', value: round(r.elapsed, 1), unit: 's', status: 'info' },
			...(r.mode === 'auto' ? [{ label: 'Remaining', value: round(Math.max(0, levelDwell(r) - r.atLevelT), 1), unit: 's', status: 'info' as const }] : []),
			{ label: 'Total rms', value: total?.rms_g ?? '-', unit: 'g', status: 'info' },
			{ label: 'Reference rms', value: total?.reference_rms_g ?? '-', unit: 'g', status: 'info' },
			{ label: 'Drive peak', value: total?.drive_vpk ?? '-', unit: 'V', status: 'info' }
		];
		for (const p of prows) {
			if (!p.on) continue;
			const st = p.alarm ? 'warn' : 'pass';
			if (p.freq_hz !== null) out.push({ label: `${p.name}: frequency`, value: p.freq_hz, unit: 'Hz', status: 'info' });
			out.push({ label: `${p.name}: measured`, value: p.measured ?? '-', unit: p.unit, status: st });
			out.push({ label: `${p.name}: target`, value: p.target ?? '-', unit: p.unit, status: 'info' });
			out.push({ label: `${p.name}: error`, value: p.error_db ?? '-', unit: 'dB', status: st });
		}
		return { view: 'reading', title: 'Run', rows: out };
	}

	const timeAxis = { label: 'Time', unit: 's', scale: 'linear' as const };

	function historyView(r: Run, p: PartRun, q: 'amplitude' | 'frequency' | 'level'): SpectrumView {
		const name = r.names.get(p.part.id) ?? p.part.id;
		const h = p.history;
		if (q === 'frequency') return { view: 'spectrum', title: name, x: timeAxis, y: { label: 'Frequency', unit: 'Hz', scale: 'linear' }, link: 'time', series: [{ name: 'Frequency', role: 'reference', x: f64(h.t), y: f64(h.freq), shape: 'line' }] };
		if (q === 'level') {
			const L = h.level;
			const ab = p.part.abort_db ?? [6, 6];
			const al = p.part.alarm_db ?? [3, 3];
			const widen = (dof: number) => 10 * Math.log10(1 + 1.645 * Math.sqrt(2 / Math.max(2, dof)));
			const band = (d: number, sign: 1 | -1) => f64(L.target.map((v, i) => v * 10 ** ((sign * (d + widen(L.dof[i]))) / 10)));
			return {
				view: 'spectrum',
				title: name,
				x: timeAxis,
				y: { label: 'Level', unit: 'g²/Hz', scale: 'log' },
				link: 'time',
				series: [
					{ name: 'Measured', role: 'measured', x: f64(L.t), y: f64(L.measured), shape: 'line' },
					{ name: 'Reference', role: 'reference', x: f64(L.t), y: f64(L.target), shape: 'line' },
					{ name: 'Alarm high', role: 'tolerance', x: f64(L.t), y: band(al[1], 1), shape: 'line' },
					{ name: 'Alarm low', role: 'tolerance', x: f64(L.t), y: band(al[0], -1), shape: 'line' },
					{ name: 'Abort high', role: 'abort', x: f64(L.t), y: band(ab[1], 1), shape: 'line' },
					{ name: 'Abort low', role: 'abort', x: f64(L.t), y: band(ab[0], -1), shape: 'line' }
				]
			};
		}
		const al = p.part.alarm_db ?? [1.5, 1.5];
		const ab = p.part.abort_db ?? [3, 3];
		const by = (d: number) => f64(h.target.map((v) => v * 10 ** (d / 20)));
		const kept = h.t.map((t, i) => [t, h.measured[i]] as const).filter(([, v]) => v !== null) as [number, number][];
		return {
			view: 'spectrum',
			title: name,
			x: timeAxis,
			y: { label: 'Amplitude', unit: 'g', scale: 'log' },
			link: 'time',
			series: [
				{ name: 'Measured', role: 'measured', x: f64(kept.map((k) => k[0])), y: f64(kept.map((k) => k[1])), shape: 'line' },
				{ name: 'Target', role: 'reference', x: f64(h.t), y: f64(h.target), shape: 'line' },
				{ name: 'Alarm high', role: 'tolerance', x: f64(h.t), y: by(al[1]), shape: 'line' },
				{ name: 'Alarm low', role: 'tolerance', x: f64(h.t), y: by(-al[0]), shape: 'line' },
				{ name: 'Abort high', role: 'abort', x: f64(h.t), y: by(ab[1]), shape: 'line' },
				{ name: 'Abort low', role: 'abort', x: f64(h.t), y: by(-ab[0]), shape: 'line' }
			]
		};
	}

	function resolveParts(r: Run, sel: MixedSubscribePlot['parts'], q: 'amplitude' | 'frequency' | 'level'): PartRun[] {
		const takes = (p: PartRun) => (q === 'amplitude' ? p.part.kind === 'tone' : q === 'level' ? p.part.kind === 'narrowband' : true);
		const s = sel ?? 'all';
		const list = r.parts.filter((p) => takes(p) && (s === 'all' || (s === 'tones' && p.part.kind === 'tone') || (s === 'bands' && p.part.kind === 'narrowband') || (Array.isArray(s) && s.includes(p.part.id))));
		return list.filter((p) => p.history.t.length > 0);
	}

	function resolveChannels(sel: MixedSubscribePlot['channels']): number[] {
		const inputs = host.rows().filter((r) => r.enabled && r.role !== 'drive');
		const limited = new Set((run?.config.limits.channels ?? []).map((l) => l.channel));
		const s = sel ?? 'all';
		if (s === 'all') return inputs.map((r) => r.index);
		if (s === 'control') return inputs.filter((r) => r.role === 'control').map((r) => r.index);
		if (s === 'limited') return inputs.filter((r) => limited.has(r.index)).map((r) => r.index);
		return inputs.filter((r) => s.includes(r.index)).map((r) => r.index);
	}

	function plotViews(r: Run, p: MixedSubscribePlot): { channels: number[] | null; parts: string[] | null; views: View[] } {
		const engaged = ['floor_only', 'engaged', 'full_level', 'ramping_down', 'stopped', 'aborted'].includes(r.phase) && r.t > 0;
		const gated = r.identification !== null;
		switch (p.quantity) {
			case 'control':
			case 'residual':
				return { channels: null, parts: null, views: engaged ? [controlView(r, p.quantity === 'control' ? 'Control' : 'Residual')] : [] };
			case 'drive':
				return { channels: null, parts: null, views: engaged ? [driveView(r)] : [] };
			case 'response':
			case 'time': {
				const chs = resolveChannels(p.channels);
				if (!engaged) return { channels: chs, parts: null, views: [] };
				const base = controlView(r, 'Control');
				const views = chs.map((ch, k): SpectrumView => {
					if (p.quantity === 'response') return { ...base, title: `Channel ${ch}`, regions: [], series: [{ ...base.series[0], name: 'Response', y: f64(Array.from(base.series[0]?.y ?? [], (v) => v * (1 + 0.3 * k))) }] };
					const n = p.points ?? 1024;
					const fs = r.derived.grid.fs_hz;
					const xs = Array.from({ length: n }, (_, i) => round(r.t + i / fs, 6));
					const ys = xs.map((t, i) => r.parts.reduce((s, q) => s + (q.part.kind === 'tone' && q.measured !== null ? q.measured * Math.sin(2 * Math.PI * q.freq * t) : 0), 0) + 2 * Math.sin(i * 12.9898 + k) * Math.sin(i * 0.37));
					return { view: 'spectrum', title: `Channel ${ch}`, x: timeAxis, y: { label: 'Acceleration', unit: 'g', scale: 'linear' }, link: 'time', series: [{ name: 'Samples', role: 'measured', x: f64(xs), y: f64(ys), shape: 'line' }] };
				});
				return { channels: chs, parts: null, views };
			}
			case 'coherence':
				return {
					channels: null,
					parts: null,
					views: gated ? [{ view: 'spectrum', title: 'Coherence', x: freqAxis, y: { label: 'Coherence', unit: null, scale: 'linear' }, series: [stepped('Coherence', 'measured', r.grid, r.grid.lines.map((f, i) => 0.93 + 0.06 * Math.abs(Math.sin(i * 0.31)))), stepped('Gate', 'tolerance', r.grid, r.grid.lines.map(() => r.config.control?.coherence_floor ?? 0.5))] }] : []
				};
			case 'identification': {
				if (!gated) return { channels: null, parts: null, views: [] };
				const gain = r.grid.lines.map(plantGain);
				const peak = Math.max(...gain);
				return { channels: null, parts: null, views: [{ view: 'spectrum', title: 'Identification Gain', x: freqAxis, y: { label: 'Gain', unit: 'g/V', scale: 'log' }, series: [stepped('Gain', 'measured', r.grid, gain), stepped('Trusted', 'reference', r.grid, gain.map((v) => (v >= 0.05 * peak ? v : NaN)))] }] };
			}
			case 'amplitude':
			case 'frequency':
			case 'level': {
				const list = resolveParts(r, p.parts, p.quantity);
				return { channels: null, parts: list.map((x) => x.part.id), views: list.map((x) => historyView(r, x, p.quantity as 'amplitude' | 'frequency' | 'level')) };
			}
			case 'law': {
				if (r.phase === 'sysid' || r.phase === 'coherence_gate' || r.state === 'review' || !engaged) return { channels: null, parts: null, views: [] };
				const views: View[] = [{ view: 'reading', title: 'Floor law', rows: [{ label: 'Mean correction', value: round(0.1 * Math.sin(r.t / 3), 3), unit: 'dB', status: 'info' }, { label: 'Unusable calls in a row', value: 0, unit: null, status: 'pass' }, { label: 'Caps bound', value: 0, unit: null, status: 'info' }] }];
				for (const p2 of r.parts)
					if (p2.part.kind === 'tone' && (p2.scale > 0 || p2.settledT > 0))
						views.push({ view: 'reading', title: `Tone law: ${r.names.get(p2.part.id)}`, rows: [...Object.entries(r.toneValues).map(([k, v]) => ({ label: k.replaceAll('_', ' '), value: v, unit: 'dB', status: 'info' as const })), { label: 'Unusable calls in a row', value: 0, unit: null, status: 'pass' }, { label: 'Caps bound', value: 0, unit: null, status: 'info' }] });
				return { channels: null, parts: null, views };
			}
			case 'readings':
				return { channels: null, parts: null, views: engaged ? [readingsView(r)] : [] };
		}
	}

	function headOf(r: Run): MixedLiveHead {
		const s = stateOf(r);
		const prows = s.parts;
		const alarms: MixedAlarm[] = prows.filter((p) => p.alarm && p.kind !== 'floor').map((p) => ({ kind: 'band', part: p.id, side: (p.error_db ?? 0) > 0 ? 'high' : 'low', error_db: p.error_db ?? 0 }));
		return {
			run_id: r.id,
			source: r.source,
			class: r.station.class,
			state: r.state === 'armed' || r.state === 'configured' || r.state === 'blocked' || r.state === 'unconfigured' ? 'running' : r.state,
			phase: r.phase,
			mode: r.mode,
			t_s: s.t_s,
			elapsed_s: s.elapsed_s,
			parts_s: s.parts_s,
			level_db: round(r.level, 2),
			commanded_db: round(r.commanded, 2),
			at_level_s: s.at_level_s,
			remaining_at_level_s: s.remaining_at_level_s,
			level_s: s.at_level_s,
			loop: r.loop,
			tick: r.tick,
			held: r.held,
			alarm: s.alarm,
			limiting: false,
			parts: prows,
			total: s.total ?? { rms_g: null, reference_rms_g: null, drive_vpk: null, drive_vrms: null },
			channels: host
				.rows()
				.filter((c) => c.enabled && c.role !== 'drive')
				.map((c, k) => ({ index: c.index, description: c.description, role: c.role as never, rms: s.total?.rms_g !== null && s.total?.rms_g !== undefined ? round(s.total.rms_g * (1 + 0.3 * k), 3) : null, unit: 'g', overload: false })),
			alarms,
			law: s.law ? { floor: { values: { mean_correction_db: round(0.1 * Math.sin(r.t / 3), 3) }, misses: 0, capped: 0 }, tones: r.parts.filter((p) => p.part.kind === 'tone' && (p.scale > 0 || p.settledT > 0)).map((p) => ({ part: p.part.id, values: { ...r.toneValues }, misses: 0, capped: 0 })) } : null
		};
	}

	function liveOf(r: Run) {
		return { ...headOf(r), views: [plotViews(r, { id: 'c', quantity: 'control' }).views[0] ?? emptySpectrum('Control'), plotViews(r, { id: 'r', quantity: 'residual' }).views[0] ?? emptySpectrum('Residual'), plotViews(r, { id: 'd', quantity: 'drive' }).views[0] ?? emptySpectrum('Drive'), readingsView(r)] };
	}

	const emptySpectrum = (title: string): SpectrumView => ({ view: 'spectrum', title, x: freqAxis, y: psdAxis, series: [] });

	function dataFor(r: Run, plots: MixedSubscribePlot[]) {
		return { ...headOf(r), plots: plots.map((p) => ({ id: p.id, quantity: p.quantity, ...plotViews(r, p) })) };
	}

	function publish(r: Run) {
		const final = r.state === 'finished';
		let frame: Record<string, unknown> | null = null;
		for (const conn of host.connections()) {
			const sub = subs.get(conn);
			if (!sub) {
				frame ??= liveOf(r) as unknown as Record<string, unknown>;
				conn.emit('mixed.live', frame);
				continue;
			}
			sub.acc += LIVE_MS / 1000;
			if (final || sub.acc + 1e-9 >= 1 / sub.rate) {
				sub.acc = 0;
				conn.emit('mixed.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
			}
		}
	}

	function startTimers(r: Run) {
		r.timer = setInterval(() => run === r && tick(r), TICK_MS);
		if (liveTimer) return;
		liveTimer = setInterval(() => {
			if (!run || !LIVE.includes(run.state) || run.state === 'armed') {
				if (!run || !LIVE.includes(run.state)) {
					if (liveTimer) clearInterval(liveTimer);
					liveTimer = null;
				}
				return;
			}
			publish(run);
		}, LIVE_MS);
	}

	// ---- verbs -----------------------------------------------------------------------------

	function needRun(): Run {
		if (!run) refuse('refused', 'not_configured', 'Mixed Mode is not configured; configure a test first.');
		return run;
	}

	function wrongState(r: Run, verb: string): never {
		refuse('refused', 'wrong_state', `${verb} does not apply while the run is ${r.state} (${r.phase.replaceAll('_', ' ')}).`, { state: r.state, phase: r.phase });
	}

	function notWhileOther(what: string) {
		const other = host.otherRun();
		if (other) refuse('refused', 'run_active', `Run ${other} is live; stop it before ${what}.`, { run_id: other });
	}

	function parseSubscribe(params: Record<string, unknown>): { rate: number; plots: MixedSubscribePlot[] } | null {
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
		const plots = list.map((raw, i): MixedSubscribePlot => {
			const at = `plots[${i}]`;
			if (!isObject(raw)) bad(at, `${at} is an object.`);
			const extra = Object.keys(raw).find((k) => !['id', 'quantity', 'channels', 'parts', 'points'].includes(k));
			if (extra) bad(`${at}.${extra}`, `A plot has an id, a quantity, channels, parts and points; not "${extra}".`);
			if (typeof raw.id !== 'string' || raw.id.length < 1 || raw.id.length > 64) bad(`${at}.id`, 'A plot id is 1 to 64 characters.');
			if (seen.has(raw.id as string)) bad(`${at}.id`, `The plot id "${raw.id}" is used twice.`);
			seen.add(raw.id as string);
			if (!QUANTITIES.includes(raw.quantity as MixedLiveQuantity)) bad(`${at}.quantity`, `"quantity" is one of ${QUANTITIES.join(', ')}.`);
			const quantity = raw.quantity as MixedLiveQuantity;
			const out: MixedSubscribePlot = { id: raw.id as string, quantity };
			if (WITH_CHANNELS.includes(quantity)) {
				const ch = raw.channels ?? 'all';
				const ok = ch === 'all' || ch === 'control' || ch === 'limited' || (Array.isArray(ch) && ch.every((x) => Number.isInteger(x) && x >= 0));
				if (!ok) bad(`${at}.channels`, '"channels" is a list of channel indexes, or "all", "control" or "limited".');
				out.channels = ch as MixedSubscribePlot['channels'];
			} else if (raw.channels !== undefined && raw.channels !== null) bad(`${at}.channels`, `"channels" does not apply to ${quantity}.`);
			if (WITH_PARTS.includes(quantity)) {
				const ps = raw.parts ?? 'all';
				const ok = ps === 'all' || ps === 'tones' || ps === 'bands' || (Array.isArray(ps) && ps.every((x) => typeof x === 'string'));
				if (!ok) bad(`${at}.parts`, '"parts" is a list of part ids, or "all", "tones" or "bands".');
				out.parts = ps as MixedSubscribePlot['parts'];
			} else if (raw.parts !== undefined && raw.parts !== null) bad(`${at}.parts`, `"parts" does not apply to ${quantity}.`);
			if (quantity === 'time') {
				const pts = raw.points ?? 1024;
				if (!(Number.isInteger(pts) && (pts as number) >= 64 && (pts as number) <= 8192)) bad(`${at}.points`, '"points" is a whole number from 64 to 8192.');
				out.points = pts as number;
			} else if (raw.points !== undefined && raw.points !== null) bad(`${at}.points`, `"points" does not apply to ${quantity}.`);
			return out;
		});
		return { rate: rate as number, plots };
	}

	/** Claim the run's folder at arm, or refuse the arm as the service does when it cannot be written. */
	function claimRecord(r: Run) {
		if (r.record) return;
		const root = (host.dataRoot ?? '~/Hertzforge_Data').replace(/[\\/]+$/, '');
		const name = host.folderName ? host.folderName(new Date(), r.id, 'mixed') : `mixed_${r.id}`;
		const folder = `${root}/${name}`;
		if (host.record === 'unwritable') {
			const reason = 'Permission denied';
			refuse('refused', 'record_unwritable' as RefusalReason, `The run's record could not be written, so the run did not arm: ${folder} (${reason}). Choose a data folder that can be written.`, { path: folder, reason });
		}
		const streams = r.config.record?.streams === false ? 'off' : 'on';
		r.record = { folder, streams, problem: null };
		if (streams === 'off') addLog('record', "the run's streams are not recorded: switched off in the setup");
	}

	function configure(params: Record<string, unknown>, conn: MockConnection) {
		const dry = params.dry_run === true;
		if (!dry) host.own(conn);
		shapeCheck(params.config);
		if ((params.config as Record<string, unknown>).capture != null)
			bad('config.capture', 'Mixed Mode takes no "capture": every run is recorded in its run folder as it goes, and "record.streams" says whether the record keeps its time histories.');
		const config = structuredClone(params.config) as MixedConfig;
		const { errors, warnings } = checkConfig(config, host.rows(), host.toneLaws());
		if (dry) {
			if (errors.length) return { errors, warnings, derived: null, views: [] };
			return { errors, warnings, derived: derive(config, host.rows(), host.toneLaws(), false), views: configureViews(config) };
		}
		if (isLive()) refuse('refused', 'run_active', `Run ${run!.id} is ${run!.state}; stop it before configuring.`, { run_id: run!.id });
		notWhileOther('configuring Mixed Mode');
		if (errors.length) refuse('bad_request', 'config_invalid', `The configuration has ${errors.length} ${errors.length === 1 ? 'error' : 'errors'}.`, { errors, warnings });
		const derived = derive(config, host.rows(), host.toneLaws(), false);
		const views = configureViews(config);
		if (run?.timer) clearInterval(run.timer);
		configured = { config, derived, views };
		run = newRun(config, derived, views);
		log = [];
		lastStateKey = '';
		addLog('state', `Configured: ${config.title ?? 'Mixed Mode'}.`);
		stateChanged();
		return { state: stateOf(run), derived, warnings };
	}

	return {
		floorLaws: () => [HINV],
		isLive,
		liveId: () => (run && isLive() ? run.id : null),
		detach: (conn) => {
			subs.delete(conn);
		},
		seatExpired: () => {
			if (run && isLive() && run.state === 'running') {
				run.stopOutcome = 'safe_stop';
				run.state = 'stopping';
				run.phase = 'ramping_down';
				stateChanged();
			}
		},
		methods: {
			'mixed.configure': (params, conn) => configure(params, conn),
			'mixed.config': () => (run && configured ? { run_id: run.id, config: configured.config, derived: run.derived, views: configured.views } : { run_id: null, config: null, derived: null, views: [] }),
			'mixed.state': () => stateOf(run),
			'mixed.log': (params) => {
				const since = isNum(params.since) ? params.since : 0;
				return { entries: log.filter((e) => e.seq >= since), next: log.length };
			},
			'mixed.arm': (params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state !== 'configured' && r.state !== 'blocked') wrongState(r, 'Arming');
				notWhileOther('arming');
				if (r.state === 'configured' && host.feasibilityExceeds && r.config.feasibility) {
					r.derived = { ...r.derived, feasibility: { verdict: 'exceeded', message: "The predicted peak exceeds the shaker's ratings." } };
					setPhase(r, 'blocked', 'feasibility_blocked');
					return stateOf(r);
				}
				if (r.state === 'blocked' && typeof params.override_ack !== 'string') refuse('refused', 'feasibility_exceeded' as RefusalReason, 'Arming past the ratings needs an acknowledgement.');
				claimRecord(r);
				r.derived = derive(r.config, host.rows(), host.toneLaws(), true, r.config.acquisition.fs_hz);
				addLog('station', 'Station open and gated; nothing driven.');
				setPhase(r, 'armed', 'armed');
				return stateOf(r);
			},
			'mixed.start': (_p, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state === 'configured') {
					notWhileOther('starting');
					claimRecord(r);
					r.derived = derive(r.config, host.rows(), host.toneLaws(), true, r.config.acquisition.fs_hz);
					setPhase(r, 'armed', 'armed');
				}
				if (r.state !== 'armed') wrongState(r, 'Start');
				addLog('state', 'Identifying the system.');
				setPhase(r, 'running', 'sysid');
				startTimers(r);
				return stateOf(r);
			},
			'mixed.proceed': (_p, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state !== 'review') wrongState(r, 'Proceed');
				engage(r);
				return stateOf(r);
			},
			'mixed.mode': (params, conn) => {
				host.own(conn);
				const r = needRun();
				if (params.mode !== 'auto' && params.mode !== 'manual') bad('mode', '"mode" is auto or manual.');
				if (r.state === 'finished') wrongState(r, 'A change of ladder mode');
				r.mode = params.mode;
				addLog('level', `Ladder ${r.mode}.`);
				stateChanged();
				return { mode: r.mode };
			},
			'mixed.level': (params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state !== 'running' || !['engaged', 'full_level', 'floor_only'].includes(r.phase)) wrongState(r, 'A level change');
				if (r.mode !== 'manual') refuse('refused', 'manual_mode_required', 'Switch the ladder to manual to step the level.', { mode: r.mode });
				let to: number;
				if (isNum(params.delta_db)) to = r.commanded + params.delta_db;
				else if (isNum(params.to_db)) {
					if (!r.levels.includes(params.to_db)) refuse('refused', 'not_a_rung', `${params.to_db} dB is not a level of the schedule.`, { levels_db: r.levels });
					to = params.to_db;
				} else bad('delta_db', 'Give delta_db or to_db.');
				to = Math.min(r.levels[r.levels.length - 1], Math.max(r.levels[0], to));
				r.commanded = round(to, 2);
				r.rung = r.levels.reduce((best, l, i) => (l <= r.commanded + 1e-9 ? i : best), 0);
				r.phase = r.partsT === null ? 'floor_only' : r.commanded >= r.levels[r.levels.length - 1] ? 'full_level' : 'engaged';
				r.atLevelT = 0;
				addLog('level', `Level ${r.commanded.toFixed(1)} dB.`);
				stateChanged();
				return { level_db: r.commanded };
			},
			'mixed.hold': (_p, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state !== 'running') wrongState(r, 'Hold');
				r.held = true;
				addLog('state', 'Held: every part at its frequency and level.');
				stateChanged();
				return stateOf(r);
			},
			'mixed.resume': (_p, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state !== 'running' || !r.held) wrongState(r, 'Resume');
				r.held = false;
				addLog('state', 'Resumed.');
				stateChanged();
				return stateOf(r);
			},
			'mixed.part': (params, conn) => {
				host.own(conn);
				const r = needRun();
				if (typeof params.id !== 'string') bad('id', '"id" is a part id.');
				if (typeof params.on !== 'boolean') bad('on', '"on" is true or false.');
				if (r.state !== 'running' || (r.phase !== 'engaged' && r.phase !== 'full_level')) wrongState(r, 'Switching a part');
				const id = params.id as string;
				const on = params.on as boolean;
				const floor = r.config.parts.find((p) => p.kind === 'floor');
				if (floor?.id === id) refuse('refused', 'part_refused', 'The floor plays for the whole run.', { id, why: 'floor' });
				const p = r.parts.find((x) => x.part.id === id);
				if (!p) refuse('refused', 'part_refused', `The run has no part "${id}".`, { id, why: 'unknown_part' });
				if (p.enabled === on) return { id, on };
				const w = p.part.window ?? {};
				if (on && w.off_s !== null && w.off_s !== undefined && (r.partsT ?? 0) >= w.off_s) refuse('refused', 'part_refused', `${r.names.get(id)}'s window closed at ${w.off_s} s.`, { id, why: 'window_closed' });
				if (on && p.ended) refuse('refused', 'part_refused', `${r.names.get(id)}'s schedule has ended.`, { id, why: 'schedule_ended' });
				p.enabled = on;
				addLog('part', `${r.names.get(id)} switched ${on ? 'on' : 'off'} by ${host.clientName(conn)}.`);
				stateChanged();
				return { id, on };
			},
			'mixed.adjust': (params, conn) => {
				host.own(conn);
				const r = needRun();
				if (r.state !== 'running' || !['floor_only', 'engaged', 'full_level'].includes(r.phase)) wrongState(r, 'A live parameter change');
				const key = params.key;
				const value = params.value;
				const law = host.toneLaws().find((l) => l.id === (r.config.control?.law?.tones?.id ?? 'servo_blend'));
				const p = law?.params.find((x) => x.key === key);
				if (!p) refuse('refused', 'adjust_refused', `The tone law has no parameter "${String(key)}".`, { key, why: 'unknown_key' });
				if (!(law?.live ?? []).includes(p.key)) refuse('refused', 'adjust_refused', `"${p.key}" is fixed for the run.`, { key, why: 'not_live' });
				if (!isNum(value) || p.kind !== 'number' || value < p.min || value > p.max) refuse('refused', 'adjust_refused', `"${p.key}" is from ${(p as { min: number }).min} to ${(p as { max: number }).max}.`, { key, why: 'out_of_range' });
				r.lawLive[p.key] = value;
				addLog('law', `${p.label} set to ${value} for every tone.`);
				stateChanged();
				return { key: p.key, value };
			},
			'mixed.stop': () => {
				const r = needRun();
				if (r.state === 'finished') return stateOf(r);
				if (r.state === 'armed' || r.state === 'review' || r.state === 'configured' || r.state === 'blocked') {
					finish(r, 'operator_stop', 'Stopped by the operator before anything played.');
					return stateOf(r);
				}
				if (r.state === 'running') {
					r.stopOutcome = 'operator_stop';
					r.state = 'stopping';
					r.phase = 'ramping_down';
					stateChanged();
				}
				return stateOf(r);
			},
			'mixed.abort': () => {
				const r = needRun();
				if (r.state !== 'finished') finish(r, 'operator_abort', 'Aborted by the operator; the drive was cut.');
				return stateOf(r);
			},
			'mixed.reset': (_p, conn) => {
				host.own(conn);
				const r = needRun();
				if (isLive()) wrongState(r, 'A new run');
				if (!configured) refuse('refused', 'not_configured', 'Nothing to run again.');
				run = newRun(configured.config, configured.derived, configured.views);
				log = [];
				lastStateKey = '';
				stateChanged();
				return stateOf(run);
			},
			'mixed.clear': (_p, conn) => {
				host.own(conn);
				if (isLive()) wrongState(run!, 'Clearing');
				run = null;
				configured = null;
				log = [];
				const s = stateOf(null);
				host.broadcast('mixed.state', s as unknown as Record<string, unknown>);
				return s;
			},
			'mixed.subscribe': (params, conn) => {
				const sub = parseSubscribe(params);
				if (!sub) {
					subs.delete(conn);
					return { default: true, rate_hz: 5, plots: null, quantities: QUANTITIES };
				}
				const entry = { ...sub, acc: 0 };
				subs.set(conn, entry);
				if (run && run.state !== 'configured' && run.state !== 'blocked') {
					const r = run;
					setTimeout(() => {
						if (subs.get(conn) === entry && run === r) conn.emit('mixed.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
					}, 30);
				}
				return { default: false, rate_hz: sub.rate, plots: sub.plots, quantities: QUANTITIES };
			}
		}
	};
}

