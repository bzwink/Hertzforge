/**
 * Mock Explorer (schema/PROTOCOL.md, "Explorer"), on the mock control service's run slot and
 * controller seat.
 *
 * One sine tone the operator steers by hand, on a plant like the control service's simulated
 * station (two modes, 200 and 550 Hz, Q 2.5, with a feedthrough). It starts quiet at exactly 0 V;
 * a level steps once to 1% of the target and then ramps at the drive slew; down to 0 V it ramps to
 * 1% of where it was and then steps to 0. A frequency lands in one frame and halts a sweep; a sweep
 * moves the tone at its rate, clamped to 60 times the frequency slew, and parks at its edge; a hold
 * leaves the tone where it is. Stop ramps the drive down at the drive slew; abort ends at once; with
 * nobody in control the run goes on and safe-stops after the timeout. An input row's abort limits
 * are checked on its response (trip `watchdog`). The survey line keeps the response level at each
 * frequency visited by the core's rules: a point once the tone has stood for the pipeline, the
 * guard and three cycles, a revisit within the tolerance replacing it, the tone passing through a
 * span removing the older points inside it.
 *
 * It refuses what the service refuses, by the same reasons and on the same parameters: a steering
 * verb outside `running` (`wrong_state` with the state and phase), a dB step from 0 V
 * (`level_zero`), two parts of one kind or none (`bad_params`), a configuration key the wire does
 * not have, configuration content by item (`config_invalid`), a plot key a quantity does not take.
 * It sends `explorer.live` to every connection without an Explorer subscription and `explorer.data`
 * in its place to a subscribed one. Every value is synthetic, made only so the shell has something
 * to draw. Nothing here is the service's analysis or its arithmetic beyond what PROTOCOL states.
 */

import type { ChannelRow, DataSource, LogEntry, LogKind, ReadingRow, RefusalReason, RunRecordRef, Series, SpectrumView, StationRef, View } from '../types';
import type {
	ExplorerConfig,
	ExplorerDerived,
	ExplorerEnd,
	ExplorerHealth,
	ExplorerItemReason,
	ExplorerLiveChannel,
	ExplorerLiveHead,
	ExplorerLiveQuantity,
	ExplorerOutcome,
	ExplorerPhase,
	ExplorerReportItem,
	ExplorerRunState,
	ExplorerState,
	ExplorerStopCause,
	ExplorerSubscribePlot,
	ExplorerSurveyPeak,
	ExplorerSweep,
	ExplorerSweepDirection,
	ExplorerTrip
} from '../explorer';
import type { ShockMockHost } from './shock';
import { MockError, type MockConnection, type MockMethod } from './socket';

const G_IPS2 = 386.089;
/** One batch of the mock's loop, wall-clock ms (times the host's speed in station time). */
const TICK_MS = 40;
/** `explorer.live` ten times a second. */
const LIVE_MS = 100;
const FS_DEFAULT = 8192;
const STOP_WAIT_S = 30;
/** The 40 dB span a rise and a stop ramp over, and the step at its end (1%). */
const SPAN_DB = 40;
const LIFT = 0.01;
const SURVEY_GRID = 256;
const NOISE = 1e-4;

const QUANTITIES: ExplorerLiveQuantity[] = ['survey', 'spectrum', 'time', 'readings'];
const LIVE: ExplorerRunState[] = ['starting', 'running', 'stopping'];
const CONFIG_KEYS = ['title', 'acquisition', 'band_hz', 'start_hz', 'max_drive_vpk', 'slew', 'display', 'capture', 'metadata', 'operator', 'article', 'notes', 'specification', 'record'];

function refuse(code: 'refused' | 'bad_request', reason: RefusalReason | 'level_zero', message: string, detail?: Record<string, unknown>): never {
	throw new MockError(code, message, reason as RefusalReason, detail);
}

function bad(param: string, message: string): never {
	refuse('bad_request', 'bad_params', message, { param });
}

const round = (v: number, d = 3) => Math.round(v * 10 ** d) / 10 ** d;
const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const f64 = (a: number[]) => Float64Array.from(a);
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** The power of two nearest 20 ms of the rate: the station's block. */
export function blockFramesAt(fs: number): number {
	const want = fs * 0.02;
	let p = 1;
	while (p * 2 <= want) p *= 2;
	return want - p <= p * 2 - want ? p : p * 2;
}

/** The plant of the simulated station: two modes and a feedthrough, magnitude in g per volt. */
export function explorerPlantGain(f: number): number {
	let re = 0.35;
	let im = 0;
	for (const fn of [200, 550]) {
		const x = f / fn;
		const a = 1 - x * x;
		const b = x / 2.5;
		const d = a * a + b * b;
		// j b / (a + j b) = (b^2 + j a b) / (a^2 + b^2)
		re += (b * b) / d;
		im += (a * b) / d;
	}
	return Math.hypot(re, im);
}

type Item = ExplorerReportItem;
const item = (reason: ExplorerItemReason, message: string, field?: string): Item => (field ? { reason, message, field } : { reason, message });

interface Run {
	id: string;
	config: ExplorerConfig;
	derived: ExplorerDerived;
	state: ExplorerRunState;
	phase: ExplorerPhase;
	station: StationRef;
	source: DataSource;
	/** The armed station's figures. */
	fs: number;
	block: number;
	lookahead: number;
	groupDelay: number;
	pipeline: number;
	band: [number, number];
	/** Input frames since start: station time is `frames / fs`. */
	frames: number;
	freq: number;
	targetHz: number;
	drive: number;
	targetVpk: number;
	/** Where a fall to 0 V steps off: 1% of the level the fall started from. */
	floorVpk: number;
	sweep: ExplorerSweep | null;
	/** Station time the tone last moved by hand (a click, a go-to): the survey and the meter wait from it. */
	settledAt: number;
	stop: ExplorerStopCause | null;
	stopBy: string | undefined;
	/** The drive when the stop's ramp began, and the ramp's station time so far. */
	rampFrom: number;
	rampT: number;
	end: ExplorerEnd | null;
	survey: [number, number][];
	surveyRevision: number;
	/** The last frequency a point was taken at, for a pass through a span. */
	lastPointHz: number | null;
	health: ExplorerHealth;
	/** The run's record, from start on. */
	record: RunRecordRef | null;
	/** Batches run: the gap between two is known from the second. */
	batches: number;
	timer: ReturnType<typeof setInterval> | null;
}

export interface ExplorerMock {
	methods: Record<string, MockMethod>;
	isLive(): boolean;
	/** The run's id while it is live, for another run's `run_active` refusal. */
	liveId(): string | null;
	detach(conn: MockConnection): void;
	/** Nobody held control for the timeout: the run safe-stops. */
	seatExpired(): void;
}

/**
 * What the Explorer mock needs of the control service around it: classical shock's, and how a
 * run's record behaves (refused at start, or a stream failing as the run goes).
 */
export type ExplorerMockHost = ShockMockHost;

/** Where an Explorer run's folder is made: its run name under the local data folder. */
const DATA_ROOT = '~/Hertzforge_Data';

export function createExplorerMock(host: ExplorerMockHost): ExplorerMock {
	let run: Run | null = null;
	let runCount = 0;
	let log: LogEntry[] = [];
	let lastStateKey = '';
	const subs = new Map<MockConnection, { rate: number; plots: ExplorerSubscribePlot[]; acc: number }>();
	let liveTimer: ReturnType<typeof setInterval> | null = null;

	const isLive = () => run !== null && LIVE.includes(run.state);
	const enabled = () => host.rows().filter((r) => r.enabled);
	const inputs = () => enabled().filter((r) => r.role !== 'drive');
	/** The first enabled control row, else the first enabled input. */
	const controlRow = (): ChannelRow | null => inputs().find((r) => r.role === 'control') ?? inputs()[0] ?? null;
	const tSec = (r: Run) => r.frames / r.fs;

	function addLog(kind: LogKind, text: string) {
		const e: LogEntry = { seq: log.length, at_unix: Date.now() / 1000, kind, text };
		log.push(e);
		if (run) host.broadcast('explorer.log', { run_id: run.id, entries: [e] });
	}

	// ---- the station's figures and the derived numbers ------------------------------------------

	function stationFigures(fs: number) {
		const st = host.station();
		const block = blockFramesAt(fs);
		const ni = st.ref.kind !== 'sim';
		// The simulator states a 4-frame lookahead and no group delay; NI two blocks and its filter's.
		const lookahead = ni ? 2 * block : 4;
		const groupDelay = ni ? Math.ceil(38.4 + 3.2e-6 * fs) : 0;
		return { block, lookahead, groupDelay, pipeline: 2 * lookahead + groupDelay };
	}

	function derivedOf(c: Required<Pick<ExplorerConfig, 'band_hz'>> & ExplorerConfig): ExplorerDerived {
		const fs = c.acquisition?.fs_hz ?? FS_DEFAULT;
		const fig = stationFigures(fs);
		const slew = c.slew?.drive_db_per_s ?? 6;
		const nfft = c.display?.nfft ?? 4096;
		const df = fs / nfft;
		const pipeS = fig.pipeline / fs;
		const first = (f: number) => round(pipeS + Math.max((2 * fig.block) / fs, 3 / f) + 3 / f, 3);
		return {
			grid: { fs_hz: fs, block_frames: fig.block, lookahead_frames: fig.lookahead, actual: false },
			band_hz: [c.band_hz[0], c.band_hz[1]],
			pipeline_frames: fig.pipeline,
			pipeline_s: round(pipeS, 5),
			rise_s: round(SPAN_DB / slew, 3),
			stop_s: round(SPAN_DB / slew, 3),
			stop_wait_s: STOP_WAIT_S,
			sweep_max_oct_per_min: round(60 * (c.slew?.freq_oct_per_s ?? 1), 6),
			display: { df_hz: round(df, 6), window_s: round(nfft / fs, 6) },
			survey: { tolerance_hz: round(df / 2, 6), first_point_s: [first(c.band_hz[0]), first(c.band_hz[1])] }
		};
	}

	// ---- checking a configuration ---------------------------------------------------------------

	/** Shape first, as the service reads the request: a key the wire does not have is refused by name. */
	function shapeOk(c: Record<string, unknown>) {
		for (const k of Object.keys(c)) if (!CONFIG_KEYS.includes(k)) bad(`config.${k}`, `The configuration has no "${k}"; a level is set in volts peak at the drive only.`);
		const nested: Record<string, string[]> = { acquisition: ['fs_hz'], slew: ['freq_oct_per_s', 'drive_db_per_s'], display: ['nfft'], capture: ['run_name'], specification: ['document', 'clause'], record: ['streams'] };
		for (const [k, keys] of Object.entries(nested)) {
			if (c[k] === undefined || ((k === 'specification' || k === 'record') && c[k] === null)) continue;
			if (!isObject(c[k])) bad(`config.${k}`, `"${k}" is an object.`);
			for (const kk of Object.keys(c[k] as object)) if (!keys.includes(kk)) bad(`config.${k}.${kk}`, `"${k}" has no "${kk}".`);
		}
		if (!isNum(c.max_drive_vpk)) bad('config.max_drive_vpk', '"max_drive_vpk" is a number of volts peak; it has no default.');
		if (c.title !== undefined && typeof c.title !== 'string') bad('config.title', '"title" is text.');
		if (c.band_hz !== undefined && !(Array.isArray(c.band_hz) && c.band_hz.length === 2 && c.band_hz.every(isNum))) bad('config.band_hz', '"band_hz" is two numbers, [lo, hi] Hz.');
		if (c.start_hz !== undefined && !isNum(c.start_hz)) bad('config.start_hz', '"start_hz" is a number of hertz.');
		const acq = c.acquisition as Record<string, unknown> | undefined;
		if (acq?.fs_hz !== undefined && acq.fs_hz !== null && !isNum(acq.fs_hz)) bad('config.acquisition.fs_hz', '"fs_hz" is a number of hertz, or null.');
		const slew = c.slew as Record<string, unknown> | undefined;
		for (const k of ['freq_oct_per_s', 'drive_db_per_s']) if (slew?.[k] !== undefined && !isNum(slew[k])) bad(`config.slew.${k}`, `"${k}" is a number.`);
		const disp = c.display as Record<string, unknown> | undefined;
		if (disp?.nfft !== undefined && !(isNum(disp.nfft) && Number.isInteger(disp.nfft))) bad('config.display.nfft', '"nfft" is a whole number.');
		const cap = c.capture as Record<string, unknown> | undefined;
		if (cap?.run_name !== undefined && typeof cap.run_name !== 'string') bad('config.capture.run_name', '"run_name" is text.');
		if (c.metadata !== undefined && !(Array.isArray(c.metadata) && c.metadata.every((m) => isObject(m) && typeof m.key === 'string' && typeof m.value === 'string'))) bad('config.metadata', '"metadata" is a list of {key, label, value}.');
	}

	/** The table's three, at configure and again at start. */
	function tableItems(): Item[] {
		const drives = enabled().filter((r) => r.role === 'drive');
		const out: Item[] = [];
		if (!drives.length) out.push(item('no_drive_channel', 'The channel table has no enabled drive row.'));
		if (drives.length > 1) out.push(item('extra_drive_channels', `The channel table has ${drives.length} enabled drive rows; Explorer drives one, and the others would stay silent.`));
		if (!inputs().length) out.push(item('no_input_channel', 'The channel table has no enabled input: nothing would measure the tone.'));
		return out;
	}

	function check(c: ExplorerConfig): { errors: Item[]; warnings: Item[]; derived: ExplorerDerived | null } {
		const errors: Item[] = [];
		const fs = c.acquisition?.fs_hz ?? FS_DEFAULT;
		if (c.acquisition?.fs_hz != null && !(fs >= 1000 && fs <= 204800)) errors.push(item('out_of_range', 'The rate is from 1000 to 204800 Hz.', 'acquisition.fs_hz'));
		const band: [number, number] = c.band_hz ?? [10, 2000];
		if (!(band[0] > 0 && band[1] > band[0])) errors.push(item('out_of_range', 'The band is above 0 Hz and ascending, [lo, hi].', 'band_hz'));
		else if (!(band[1] < 0.45 * fs)) errors.push(item('band_above_grid', `The band ends under 0.45 times the rate, ${round(0.45 * fs, 2)} Hz.`, 'band_hz'));
		const start = c.start_hz ?? 100;
		if (!(start >= band[0] && start <= band[1])) errors.push(item('out_of_range', `The tone starts inside the band, ${band[0]} to ${band[1]} Hz.`, 'start_hz'));
		const ceil = c.max_drive_vpk;
		const drive = enabled().find((r) => r.role === 'drive');
		if (!(ceil > 0 && ceil <= 10)) errors.push(item('out_of_range', 'The drive ceiling is above 0 and at most 10 V pk.', 'max_drive_vpk'));
		else if (drive && ceil > drive.range_max) errors.push(item('ceiling_over_range', `The drive ceiling is over the drive row's range, ${drive.range_max} V.`, 'max_drive_vpk'));
		const fslew = c.slew?.freq_oct_per_s ?? 1;
		if (!(fslew > 0 && fslew <= 10)) errors.push(item('out_of_range', 'The frequency slew is above 0 and at most 10 oct/s.', 'slew.freq_oct_per_s'));
		const dslew = c.slew?.drive_db_per_s ?? 6;
		if (!(dslew > 0 && dslew <= 1000)) errors.push(item('out_of_range', 'The drive slew is above 0 and at most 1000 dB/s.', 'slew.drive_db_per_s'));
		else if (SPAN_DB / dslew > STOP_WAIT_S) errors.push(item('safe_stop_budget', `At ${dslew} dB/s the stop takes ${round(SPAN_DB / dslew, 3)} s, longer than the service's ${STOP_WAIT_S} s stop wait; the slowest drive slew is ${round(SPAN_DB / STOP_WAIT_S, 3)} dB/s.`, 'slew.drive_db_per_s'));
		const nfft = c.display?.nfft ?? 4096;
		if (!(nfft >= 1024 && nfft <= 16384 && (nfft & (nfft - 1)) === 0)) errors.push(item('out_of_range', 'The FFT size is a power of two from 1024 to 16384.', 'display.nfft'));
		const name = c.capture?.run_name ?? 'explorer';
		if (!name.trim() || /[\\/]/.test(name) || name.includes('..')) errors.push(item('out_of_range', 'The run folder is a plain name, not a path.', 'capture.run_name'));
		errors.push(...tableItems());
		if (errors.length) return { errors, warnings: [], derived: null };
		return { errors, warnings: [], derived: derivedOf({ ...c, band_hz: band }) };
	}

	// ---- the state --------------------------------------------------------------------------------

	function emptyState(): ExplorerState {
		return {
			run_id: null,
			state: 'unconfigured',
			phase: 'idle',
			title: null,
			source: null,
			station: null,
			grid: null,
			clock: null,
			band_hz: null,
			max_drive_vpk: null,
			slew: null,
			pipeline_frames: null,
			t_s: null,
			frequency_hz: null,
			target_hz: null,
			drive_vpk: null,
			target_vpk: null,
			sweep: null,
			health: null,
			stall: null,
			stop: null,
			end: null
		};
	}

	const started = (r: Run) => r.state !== 'configured';
	const armed = (r: Run) => started(r) && r.state !== 'starting' && r.end?.outcome !== 'arm_refused';

	function stateOf(r: Run | null): ExplorerState {
		if (!r) return emptyState();
		const on = armed(r);
		const c = r.config;
		return {
			run_id: r.id,
			state: r.state,
			phase: r.phase,
			title: c.title ?? '',
			source: r.source,
			station: r.station,
			grid: { requested_fs_hz: c.acquisition?.fs_hz ?? FS_DEFAULT, fs_hz: r.fs, block_frames: r.block, lookahead_frames: r.lookahead, actual: on },
			clock: on ? 'shared' : null,
			band_hz: [...r.band],
			max_drive_vpk: c.max_drive_vpk,
			slew: { freq_oct_per_s: c.slew?.freq_oct_per_s ?? 1, drive_db_per_s: c.slew?.drive_db_per_s ?? 6 },
			pipeline_frames: r.pipeline,
			t_s: on ? round(tSec(r), 3) : null,
			frequency_hz: on ? round(r.freq, 6) : null,
			target_hz: on ? round(r.targetHz, 6) : null,
			drive_vpk: on ? round(r.drive, 6) : null,
			target_vpk: on ? round(r.targetVpk, 6) : null,
			sweep: r.sweep ? { ...r.sweep } : null,
			health: on ? { ...r.health, pass_ms: { ...r.health.pass_ms } } : null,
			stall: null,
			stop: r.stop,
			end: r.end,
			record: r.record ? { ...r.record } : null
		};
	}

	function stateChanged(force = false) {
		const s = stateOf(run);
		const key = JSON.stringify([s.run_id, s.state, s.phase, s.sweep, s.stop, s.stall, s.end, s.record]);
		if (!force && key === lastStateKey) return;
		lastStateKey = key;
		host.broadcast('explorer.state', s as unknown as Record<string, unknown>);
	}

	/** One phase at a time, in PROTOCOL's order. */
	function phaseOf(r: Run): ExplorerPhase {
		if (r.state === 'stopping') return 'ramping_down';
		if (r.targetVpk === 0 && r.drive === 0) return 'quiet';
		if (Math.abs(r.drive - r.targetVpk) > 1e-12) return 'ramping';
		if (r.sweep && !r.sweep.halted) return 'sweeping';
		if (r.sweep && r.sweep.halted && atEdge(r)) return 'at_edge';
		return 'steady';
	}

	const atEdge = (r: Run) => !!r.sweep && (r.sweep.direction === 'up' ? r.freq >= r.band[1] - 1e-9 : r.freq <= r.band[0] + 1e-9);

	function rephase(r: Run) {
		if (r.state !== 'running' && r.state !== 'stopping') return;
		const p = phaseOf(r);
		if (p !== r.phase) {
			r.phase = p;
			stateChanged();
		}
	}

	// ---- running ----------------------------------------------------------------------------------

	function newRun(config: ExplorerConfig, derived: ExplorerDerived): Run {
		const st = host.station();
		const fs = config.acquisition?.fs_hz ?? FS_DEFAULT;
		const fig = stationFigures(fs);
		const start = config.start_hz ?? 100;
		return {
			id: `explorer-${String(++runCount).padStart(4, '0')}`,
			config: structuredClone(config),
			derived,
			state: 'configured',
			phase: 'idle',
			station: st.ref,
			source: st.source,
			fs,
			...fig,
			band: [...derived.band_hz] as [number, number],
			frames: 0,
			freq: start,
			targetHz: start,
			drive: 0,
			targetVpk: 0,
			floorVpk: 0,
			sweep: null,
			settledAt: 0,
			stop: null,
			stopBy: undefined,
			rampFrom: 0,
			rampT: 0,
			end: null,
			survey: [],
			surveyRevision: 0,
			lastPointHz: null,
			health: { engine_ticks: 0, worst_tick_ms: 0.4, budget_ms: round((1000 * fig.block) / fs, 3), health_faults: 0, ring_overruns: 0, blocks_dropped: 0, driver_warnings: 0, pass_ms: { last: null, worst: null }, batch_gap_ms: null },
			record: null,
			batches: 0,
			timer: null
		};
	}

	/**
	 * The run's record, at start before the station is opened: its folder, or the start refused
	 * `record_unwritable` (the run ends `arm_refused`, nothing driven), as the service does.
	 */
	function claimRecord(r: Run) {
		const folder = `${DATA_ROOT}/${r.config.capture?.run_name ?? 'explorer'}`;
		if (host.record === 'unwritable') {
			const reason = 'Permission denied';
			const message = `The run's record could not be written, so the run did not start: ${folder} (${reason}). Choose a data folder that can be written.`;
			r.end = { outcome: 'arm_refused', message, at_s: null, at_hz: null, vpk: null, ramp_s: null, trip: null };
			r.state = 'finished';
			r.phase = 'faulted';
			addLog('state', `start refused: ${message}`);
			stateChanged();
			refuse('refused', 'record_unwritable', message, { path: folder, reason });
		}
		const streams = r.config.record?.streams === false ? 'off' : 'on';
		r.record = { folder, streams, problem: null };
		if (streams === 'off') addLog('record', "the run's streams are not recorded: switched off in the setup");
		addLog('note', `records in ${folder}`);
	}

	/** The generated frame a change made now first appears in: the lead-in keeps two lookaheads ahead. */
	const genFrame = (r: Run) => r.frames + 2 * r.lookahead;

	function setTargetVpk(r: Run, v: number) {
		r.targetVpk = v;
		if (v === 0 && r.drive > 0) r.floorVpk = r.drive * LIFT;
	}

	/** The drive moves to its target at the slew: off zero one step to 1%, down to 1% then to 0. */
	function slewDrive(r: Run, dt: number) {
		const slew = r.config.slew?.drive_db_per_s ?? 6;
		const k = 10 ** ((slew * dt) / 20);
		const t = r.targetVpk;
		if (r.drive === t) return;
		if (r.drive === 0) {
			r.drive = t * LIFT;
			return;
		}
		if (t === 0) {
			const next = r.drive / k;
			r.drive = next <= r.floorVpk * (1 + 1e-9) ? 0 : next;
			return;
		}
		r.drive = t > r.drive ? Math.min(t, r.drive * k) : Math.max(t, r.drive / k);
	}

	function moveSweep(r: Run, dt: number) {
		const s = r.sweep;
		if (!s || s.halted) return;
		const sign = s.direction === 'up' ? 1 : -1;
		const edge = s.direction === 'up' ? r.band[1] : r.band[0];
		const next = r.freq * 2 ** ((sign * s.rate_oct_per_min * dt) / 60);
		if ((sign > 0 && next >= edge) || (sign < 0 && next <= edge)) {
			r.freq = edge;
			r.targetHz = edge;
			s.halted = true;
			stateChanged(true);
		} else {
			r.freq = next;
			r.targetHz = next;
		}
	}

	/** The control row's amplitude at the tone, peak, in its unit. */
	const levelAt = (r: Run, row: ChannelRow | null) => {
		if (!row) return 0;
		const k = row.role === 'control' ? 1 : 0.6 + 0.4 * Math.sin(row.index);
		return r.drive * explorerPlantGain(r.freq) * k;
	};

	function survey(r: Run) {
		if (r.drive <= 0) return;
		const t = tSec(r);
		const moving = !!r.sweep && !r.sweep.halted;
		const wait = r.pipeline / r.fs + Math.max((2 * r.block) / r.fs, 3 / r.freq) + 3 / r.freq;
		if (!moving && t - r.settledAt < wait) return;
		const tol = r.derived.display.df_hz / 2;
		const f = r.freq;
		const level = levelAt(r, controlRow());
		const last = r.lastPointHz;
		const lo = last === null ? f : Math.min(last, f);
		const hi = last === null ? f : Math.max(last, f);
		// a revisit within the tolerance replaces a point; a pass through a span removes the older points inside it
		r.survey = r.survey.filter(([hz]) => Math.abs(hz - f) > tol && !(hz > lo && hz < hi));
		r.survey.push([round(f, 4), round(level, 6)]);
		r.survey.sort((a, b) => a[0] - b[0]);
		r.lastPointHz = f;
		r.surveyRevision += 1;
	}

	function peakOf(r: Run): ExplorerSurveyPeak | null {
		let best: [number, number] | null = null;
		for (const p of r.survey) if (!best || p[1] > best[1]) best = p;
		return best ? { hz: best[0], level: best[1] } : null;
	}

	function watchdog(r: Run): ExplorerTrip | null {
		for (const row of inputs()) {
			if (row.units !== 'g') continue;
			const a = levelAt(r, row);
			const w = 2 * Math.PI * r.freq;
			const checks: ['accel' | 'velocity' | 'displacement', number, number | null][] = [
				['accel', a, row.abort.accel_g],
				['velocity', (a * G_IPS2) / w, row.abort.velocity_ips],
				['displacement', ((a * G_IPS2) / (w * w)) * 1000, row.abort.displacement_mil]
			];
			for (const [axis, value, limit] of checks) if (limit !== null && value > limit) return { kind: 'watchdog', at_hz: round(r.freq, 3), channel: row.index, axis, value: round(value, 5), limit };
		}
		return null;
	}

	function endMessage(outcome: ExplorerOutcome, by?: string, trip?: ExplorerTrip | null): string {
		switch (outcome) {
			case 'operator_stop':
				return `Stopped by ${by ?? 'the operator'}.`;
			case 'operator_abort':
				return `Aborted by ${by ?? 'the operator'}.`;
			case 'safe_stop':
				return `Nobody held control for ${host.timeoutS} s, so the run stopped with its ramp.`;
			case 'limit_stop':
				return 'The derived motion passed its limit, so the run stopped with its ramp.';
			case 'aborted':
				return trip?.kind === 'watchdog' ? `Channel ${trip.channel}'s ${trip.axis} abort limit tripped.` : 'A protection tripped.';
			case 'arm_refused':
				return 'The station refused to arm; nothing was driven.';
			default:
				return 'The station or the service loop failed.';
		}
	}

	function end(r: Run, outcome: ExplorerOutcome, phase: ExplorerPhase, trip: ExplorerTrip | null, by?: string, ramp_s: number | null = null) {
		if (r.timer) clearInterval(r.timer);
		r.timer = null;
		const on = armed(r);
		r.end = { outcome, message: endMessage(outcome, by, trip), at_s: on ? round(tSec(r), 3) : null, at_hz: on ? round(r.freq, 3) : null, vpk: on ? round(outcome === 'operator_stop' || outcome === 'safe_stop' ? r.rampFrom : r.drive, 6) : null, ramp_s, trip };
		r.drive = 0;
		r.targetVpk = 0;
		r.sweep = null;
		r.state = 'finished';
		r.phase = phase;
		stateChanged();
		addLog(outcome === 'aborted' ? 'trip' : 'state', `finished: ${outcome.replaceAll('_', ' ')}`);
		publish(r);
	}

	/** The controlled stop: any sweep ends, the drive ramps to 0 V at the drive slew, then the run ends. */
	function beginStop(r: Run, cause: ExplorerStopCause, by?: string) {
		r.stop = cause;
		r.stopBy = by;
		addLog('note', cause === 'operator' ? `stop requested by ${by ?? 'the operator'}` : cause === 'safe_stop' ? 'nobody held control: the controlled stop' : 'the controlled stop');
		// From `starting`, the stop takes effect once the station has opened.
		if (r.state === 'starting') return;
		rampDown(r);
	}

	function rampDown(r: Run) {
		r.rampFrom = r.drive;
		r.rampT = 0;
		r.sweep = null;
		r.state = 'stopping';
		r.phase = 'ramping_down';
		setTargetVpk(r, 0);
		stateChanged();
		if (r.drive === 0) end(r, stopOutcome(r), 'stopped', null, r.stopBy, 0);
	}

	const stopOutcome = (r: Run): ExplorerOutcome => (r.stop === 'operator' ? 'operator_stop' : r.stop === 'limit' ? 'limit_stop' : 'safe_stop');

	function tick(r: Run) {
		if (run !== r || r.state === 'finished') return;
		const dt = Math.min(1, (TICK_MS / 1000) * host.speed);
		const was = r.drive;
		r.frames += Math.round(dt * r.fs);
		r.health.engine_ticks += Math.max(1, Math.round((dt * r.fs) / r.block));
		const pass = round(0.5 + 0.3 * Math.abs(Math.sin(r.frames)), 2);
		r.health.pass_ms = { last: pass, worst: Math.max(r.health.pass_ms.worst ?? 0, pass) };
		if (++r.batches >= 2) r.health.batch_gap_ms = Math.max(r.health.batch_gap_ms ?? 0, round(TICK_MS * 0.1, 2));
		slewDrive(r, dt);
		if (r.state === 'stopping') {
			r.rampT += dt;
			if (r.drive === 0) end(r, stopOutcome(r), 'stopped', null, r.stopBy, round(was > 0 ? r.rampT : 0, 3));
			return;
		}
		moveSweep(r, dt);
		const trip = watchdog(r);
		if (trip) {
			end(r, 'aborted', 'aborted', trip);
			return;
		}
		survey(r);
		rephase(r);
		if (host.record === 'problem' && r.record && r.record.streams === 'on' && r.record.problem === null && tSec(r) >= 2) {
			const problem = 'Recording of the inputs stream stopped: a write failed: no space left on the disk. The run goes on.';
			r.record = { ...r.record, problem };
			addLog('record', problem);
			stateChanged(true);
		}
	}

	// ---- display ----------------------------------------------------------------------------------

	const unitOfControl = () => controlRow()?.units ?? 'g';
	const titleOf = (row: ChannelRow) => `Channel ${row.index}: ${row.description.trim() || row.role}`;
	const freqAxis = { label: 'Frequency', unit: 'Hz', scale: 'log' as const };

	/** The survey line on `n` log-spaced points across the band, nothing outside the span visited. */
	function surveyOn(r: Run, n: number | null): { x: number[]; y: number[] } {
		const pts = r.survey;
		if (!pts.length || n === null) return { x: pts.map((p) => p[0]), y: pts.map((p) => p[1]) };
		const [lo, hi] = r.band;
		const x: number[] = [];
		const y: number[] = [];
		const from = pts[0][0];
		const to = pts[pts.length - 1][0];
		for (let i = 0; i < n; i++) {
			const f = lo * (hi / lo) ** (i / (n - 1));
			if (f < from - 1e-9 || f > to + 1e-9) continue;
			let j = 0;
			while (j < pts.length - 2 && pts[j + 1][0] < f) j++;
			const [f0, y0] = pts[j];
			const [f1, y1] = pts[Math.min(j + 1, pts.length - 1)];
			const u = f1 > f0 ? (Math.log(f) - Math.log(f0)) / (Math.log(f1) - Math.log(f0)) : 0;
			x.push(f);
			y.push(y0 + clamp(u, 0, 1) * (y1 - y0));
		}
		// A span narrower than the grid's spacing (a single point) is drawn on the survey's own points.
		if (x.length < 2) return { x: pts.map((p) => p[0]), y: pts.map((p) => p[1]) };
		return { x, y };
	}

	function surveyView(r: Run, n: number | null): SpectrumView {
		const s = surveyOn(r, n);
		return { view: 'spectrum', title: 'Survey', x: freqAxis, y: { label: 'Level', unit: unitOfControl(), scale: 'log' }, series: s.x.length ? [{ name: 'Survey', role: 'measured', x: f64(s.x), y: f64(s.y) }] : [], link: 'frequency' };
	}

	function motion(r: Run) {
		const a = levelAt(r, controlRow());
		const w = 2 * Math.PI * r.freq;
		return { v: (a * G_IPS2) / w, d: ((a * G_IPS2) / (w * w)) * 1000 };
	}

	function sweepWord(s: ExplorerSweep | null): string {
		if (!s) return 'none';
		if (s.halted) return 'parked';
		return `${s.direction} at ${round(s.rate_oct_per_min, 3)} oct/min`;
	}

	function measuredHz(r: Run): number | null {
		if (r.state !== 'running' || r.drive <= 0) return null;
		if (r.sweep && !r.sweep.halted) return null;
		if (tSec(r) - r.settledAt < r.pipeline / r.fs + 0.5 + TICK_MS / 1000) return null;
		return round(r.freq * (1 + 2e-6 * Math.sin(r.frames)), 4);
	}

	function readings(r: Run): View {
		const row = controlRow();
		const m = row?.units === 'g' ? motion(r) : null;
		const peak = peakOf(r);
		const meas = measuredHz(r);
		// A number the run does not have yet reads `none`, as the service writes it.
		const num = (v: number | null, d: number) => (v === null ? 'none' : round(v, d));
		const rows: ReadingRow[] = [
			{ label: 'Frequency', value: round(r.freq, 3), unit: 'Hz', status: 'info' },
			{ label: 'Target', value: round(r.targetHz, 3), unit: 'Hz', status: 'info' },
			{ label: 'Measured', value: num(meas, 3), unit: 'Hz', status: 'info' },
			{ label: 'Drive', value: round(r.drive, 5), unit: 'V', status: 'info' },
			{ label: 'Target drive', value: round(r.targetVpk, 5), unit: 'V', status: 'info' },
			{ label: 'Ceiling', value: r.config.max_drive_vpk, unit: 'V', status: 'info' },
			{ label: 'Sweep', value: sweepWord(r.sweep), unit: null, status: 'info' },
			{ label: 'Control rms', value: round(levelAt(r, row) / Math.SQRT2 + NOISE, 5), unit: row?.units ?? null, status: 'info' },
			{ label: 'Velocity', value: m ? round(m.v, 5) : 'none', unit: 'in/s', status: 'info' },
			{ label: 'Displacement', value: m ? round(m.d, 5) : 'none', unit: 'mil', status: 'info' },
			{ label: 'Peak', value: peak ? round(peak.hz, 3) : 'none', unit: 'Hz', status: 'info' },
			{ label: 'Pipeline', value: r.pipeline, unit: 'frames', status: 'info' }
		];
		return { view: 'reading', title: 'Run', rows };
	}

	function resolve(sel: ExplorerSubscribePlot['channels']): ChannelRow[] {
		const ins = inputs();
		if (Array.isArray(sel)) return ins.filter((x) => sel.includes(x.index));
		if (sel === 'control') {
			const c = controlRow();
			return c ? [c] : [];
		}
		return ins;
	}

	function spectrumViews(r: Run, rows: ChannelRow[]): View[] {
		const nfft = r.config.display?.nfft ?? 4096;
		const df = r.fs / nfft;
		const n = nfft / 2 + 1;
		const bin = Math.round(r.freq / df);
		return rows.map((row) => {
			const amp = levelAt(r, row);
			const x = Array.from({ length: n }, (_, i) => i * df);
			const y = x.map((_, i) => (i === bin ? amp : Math.abs(i - bin) === 1 ? amp * 0.5 : NOISE * (1 + 0.3 * Math.sin(i * 1.3 + row.index))));
			return { view: 'spectrum', title: titleOf(row), x: { label: 'Frequency', unit: 'Hz', scale: 'linear' }, y: { label: 'Amplitude', unit: row.units, scale: 'linear' }, series: [{ name: `Channel ${row.index}`, role: 'measured', x: f64(x), y: f64(y) }], link: 'frequency' } as SpectrumView;
		});
	}

	function timeView(r: Run, points: number): { view: SpectrumView; first: number } | null {
		const row = controlRow();
		if (!row) return null;
		const amp = levelAt(r, row);
		const first = Math.max(0, r.frames - points);
		const t = Array.from({ length: points }, (_, i) => (first + i) / r.fs);
		const series: Series[] = [{ name: `Channel ${row.index}`, role: 'measured', x: f64(t), y: f64(t.map((s) => amp * Math.sin(2 * Math.PI * r.freq * s) + NOISE * Math.sin(s * 7919))) }];
		return { view: { view: 'spectrum', title: 'Time', x: { label: 'Time', unit: 's', scale: 'linear' }, y: { label: 'Amplitude', unit: row.units, scale: 'linear' }, series }, first };
	}

	function head(r: Run): ExplorerLiveHead {
		const s = stateOf(r);
		const row = controlRow();
		const m = row?.units === 'g' ? motion(r) : null;
		const channels: ExplorerLiveChannel[] = inputs().map((x) => ({ index: x.index, description: x.description, role: x.role as ExplorerLiveChannel['role'], rms: round(levelAt(r, x) / Math.SQRT2 + NOISE, 6), unit: x.units }));
		return {
			run_id: r.id,
			source: r.source,
			class: r.station.class,
			state: r.state as ExplorerLiveHead['state'],
			phase: r.phase,
			t_s: s.t_s,
			frequency_hz: s.frequency_hz,
			target_hz: s.target_hz,
			measured_hz: measuredHz(r),
			drive_vpk: s.drive_vpk,
			target_vpk: s.target_vpk,
			max_drive_vpk: r.config.max_drive_vpk,
			sweep: s.sweep,
			channels,
			control: row ? { index: row.index, velocity_ips: m ? round(m.v, 6) : null, displacement_mil: m ? round(m.d, 6) : null, settling: tSec(r) < 0.16 } : null,
			pipeline_frames: r.pipeline,
			peak: peakOf(r),
			blocks_dropped: r.health.blocks_dropped,
			pass_ms: { ...r.health.pass_ms }
		};
	}

	function liveOf(r: Run) {
		return { ...head(r), views: [surveyView(r, SURVEY_GRID), readings(r)] };
	}

	function dataFor(r: Run, plots: ExplorerSubscribePlot[]) {
		const passed = r.frames > 0;
		return {
			...head(r),
			plots: plots.map((pl) => {
				const base = { id: pl.id, quantity: pl.quantity, channels: null as number[] | null, first_frame: null as number | null, views: [] as View[] };
				switch (pl.quantity) {
					case 'survey':
						if (r.survey.length) base.views = [surveyView(r, pl.points ?? null)];
						break;
					case 'spectrum': {
						const rows = resolve(pl.channels ?? 'all');
						base.channels = rows.map((x) => x.index);
						if (passed) base.views = spectrumViews(r, rows);
						break;
					}
					case 'time': {
						const tv = passed ? timeView(r, pl.points ?? 1024) : null;
						if (tv) {
							base.views = [tv.view];
							base.first_frame = tv.first;
						}
						break;
					}
					case 'readings':
						if (passed) base.views = [readings(r)];
						break;
				}
				return base;
			})
		};
	}

	/** As the service sends them: `explorer.live` to every connection without a subscription, `explorer.data` to each subscribed one at its rate. */
	function publish(r: Run) {
		const final = r.state === 'finished';
		let frame: Record<string, unknown> | null = null;
		for (const conn of host.connections()) {
			const sub = subs.get(conn);
			if (!sub) {
				frame ??= liveOf(r) as unknown as Record<string, unknown>;
				conn.emit('explorer.live', frame);
				continue;
			}
			sub.acc += LIVE_MS / 1000;
			if (final || sub.acc + 1e-9 >= 1 / sub.rate) {
				sub.acc = 0;
				conn.emit('explorer.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
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
			if (run.state === 'starting') return;
			publish(run);
		}, LIVE_MS);
	}

	// ---- refusals -----------------------------------------------------------------------------------

	function needRun(): Run {
		if (!run) refuse('refused', 'not_configured', 'Explorer is not configured; configure a test first.');
		return run;
	}

	function wrongState(r: Run, verb: string): never {
		const why = r.state === 'stopping' ? ' Nothing raises or moves the drive once a stop has begun.' : '';
		refuse('refused', 'wrong_state', `${verb} does not apply while the run is ${r.state} (${r.phase.replaceAll('_', ' ')}).${why}`, { state: r.state, phase: r.phase });
	}

	function needRunning(r: Run, verb: string) {
		if (r.state !== 'running') wrongState(r, verb);
	}

	function notWhileOther() {
		const other = host.otherRun();
		if (other) refuse('refused', 'run_active', `Run ${other} holds the station; stop it first.`, { run_id: other });
	}

	function notWhileLive(what: string) {
		if (run && isLive()) refuse('refused', 'run_active', `Run ${run.id} is ${run.state}; stop it before ${what}.`, { run_id: run.id });
	}

	function noOtherKeys(params: Record<string, unknown>, keys: string[]) {
		const extra = Object.keys(params).find((k) => !keys.includes(k));
		if (extra) bad(extra, `"${extra}" is not a parameter here; it takes ${keys.join(', ') || 'none'}.`);
	}

	function parseSubscribe(params: Record<string, unknown>): { rate: number; plots: ExplorerSubscribePlot[] } | null {
		if (params.default !== undefined) {
			if (params.default !== true) bad('default', '"default" can only be true.');
			if (params.plots !== undefined || params.rate_hz !== undefined) bad('default', '"default" takes no other parameter.');
			return null;
		}
		noOtherKeys(params, ['rate_hz', 'plots']);
		const rate = params.rate_hz === undefined ? 10 : params.rate_hz;
		if (!(isNum(rate) && rate >= 0.5 && rate <= 10)) bad('rate_hz', '"rate_hz" is a number from 0.5 to 10.');
		if (!Array.isArray(params.plots)) bad('plots', '"plots" is a list of plots.');
		const list = params.plots as unknown[];
		if (list.length > 64) bad('plots', 'At most 64 plots.');
		const seen = new Set<string>();
		const plots = list.map((raw, i): ExplorerSubscribePlot => {
			const at = `plots[${i}]`;
			if (!isObject(raw)) bad(at, `${at} is an object.`);
			const p = raw as Record<string, unknown>;
			const extra = Object.keys(p).find((k) => !['id', 'quantity', 'channels', 'points'].includes(k));
			if (extra) bad(`${at}.${extra}`, `A plot has an id, a quantity, channels and points; not "${extra}".`);
			if (typeof p.id !== 'string' || p.id.length < 1 || p.id.length > 64) bad(`${at}.id`, 'A plot id is 1 to 64 characters.');
			if (seen.has(p.id as string)) bad(`${at}.id`, `The plot id "${p.id}" is used twice.`);
			seen.add(p.id as string);
			if (!QUANTITIES.includes(p.quantity as ExplorerLiveQuantity)) bad(`${at}.quantity`, `"quantity" is one of ${QUANTITIES.join(', ')}.`);
			const quantity = p.quantity as ExplorerLiveQuantity;
			const out: ExplorerSubscribePlot = { id: p.id as string, quantity };
			if (quantity === 'spectrum') {
				const ch = p.channels ?? 'all';
				const ok = ch === 'all' || ch === 'control' || (Array.isArray(ch) && ch.every((x) => Number.isInteger(x) && x >= 0) && new Set(ch).size === ch.length);
				if (!ok) bad(`${at}.channels`, '"channels" is a list of channel indexes, or "all" or "control".');
				out.channels = ch as ExplorerSubscribePlot['channels'];
			} else if (p.channels !== undefined) bad(`${at}.channels`, `"channels" does not apply to ${quantity}.`);
			if (quantity === 'survey') {
				const pts = p.points ?? null;
				if (pts !== null && !(Number.isInteger(pts) && (pts as number) >= 64 && (pts as number) <= 8192)) bad(`${at}.points`, '"points" is null (the survey\'s own points) or a whole number from 64 to 8192.');
				out.points = pts as number | null;
			} else if (quantity === 'time') {
				const pts = p.points ?? 1024;
				if (!(Number.isInteger(pts) && (pts as number) >= 64 && (pts as number) <= 8192)) bad(`${at}.points`, '"points" is a whole number from 64 to 8192.');
				out.points = pts as number;
			} else if (p.points !== undefined) bad(`${at}.points`, `"points" does not apply to ${quantity}.`);
			return out;
		});
		return { rate: rate as number, plots };
	}

	/** A steer's parts: one of a kind at most, at least one in all. */
	function parseSteer(params: Record<string, unknown>) {
		noOtherKeys(params, ['hz', 'step_hz', 'step_oct', 'vpk', 'step_db']);
		const freq = (['hz', 'step_hz', 'step_oct'] as const).filter((k) => params[k] !== undefined);
		const level = (['vpk', 'step_db'] as const).filter((k) => params[k] !== undefined);
		if (freq.length > 1) bad(freq[1], `A steer has one frequency part; it gave ${freq.join(' and ')}.`);
		if (level.length > 1) bad(level[1], `A steer has one level part; it gave ${level.join(' and ')}.`);
		if (!freq.length && !level.length) bad('hz', 'A steer has a frequency part (hz, step_hz or step_oct), a level part (vpk or step_db), or one of each.');
		for (const k of [...freq, ...level]) if (!isNum(params[k])) bad(k, `"${k}" is a number.`);
		return { freq: freq[0] ?? null, level: level[0] ?? null };
	}

	// ---- the methods --------------------------------------------------------------------------------

	return {
		isLive,
		liveId: () => (run && isLive() ? run.id : null),
		detach: (conn) => {
			subs.delete(conn);
		},
		seatExpired: () => {
			if (run && (run.state === 'running' || run.state === 'starting')) beginStop(run, 'safe_stop');
		},
		methods: {
			'explorer.configure': (params, conn) => {
				noOtherKeys(params, ['config', 'dry_run']);
				if (!isObject(params.config)) bad('config', 'Configuring needs a test configuration.');
				shapeOk(params.config as Record<string, unknown>);
				const config = params.config as unknown as ExplorerConfig;
				const checked = check(config);
				if (params.dry_run === true) return { errors: checked.errors, warnings: checked.warnings, derived: checked.derived };
				host.own(conn);
				notWhileLive('configuring');
				if (checked.errors.length || !checked.derived) refuse('bad_request', 'config_invalid', `The Explorer configuration has ${checked.errors.length} error(s).`, { errors: checked.errors, warnings: checked.warnings });
				if (run?.timer) clearInterval(run.timer);
				run = newRun(config, checked.derived);
				log = [];
				addLog('state', `configured "${config.title ?? ''}" on ${run.station.class}`);
				stateChanged(true);
				return { state: stateOf(run), derived: checked.derived, warnings: checked.warnings };
			},
			'explorer.config': () => (run ? { run_id: run.id, config: run.config, derived: run.derived } : { run_id: null, config: null, derived: null }),
			'explorer.start': (params, conn) => {
				noOtherKeys(params, []);
				host.own(conn);
				const r = needRun();
				if (r.state === 'finished') refuse('refused', 'run_finished', 'This run has finished; reset or configure again to run.', { outcome: r.end?.outcome });
				if (r.state !== 'configured') wrongState(r, 'Starting');
				notWhileOther();
				const table = tableItems();
				if (table.length) refuse('bad_request', 'config_invalid', `The channel table no longer carries the run: ${table.length} error(s).`, { errors: table, warnings: [] });
				r.state = 'starting';
				r.phase = 'priming';
				stateChanged();
				claimRecord(r);
				// The simulated station opens at once and programs the rate asked.
				addLog('station', `rate ${r.fs} Hz adopted (asked ${r.config.acquisition?.fs_hz ?? FS_DEFAULT} Hz); band ${r.band[0]} to ${r.band[1]} Hz`);
				r.derived = { ...r.derived, grid: { ...r.derived.grid, actual: true } };
				r.state = 'running';
				r.phase = 'quiet';
				r.frames = 0;
				r.settledAt = 0;
				r.timer = setInterval(() => tick(r), TICK_MS);
				stateChanged();
				startLive();
				// A stop asked while the station opened takes effect once it has.
				if (r.stop) rampDown(r);
				return stateOf(r);
			},
			'explorer.steer': (params, conn) => {
				host.own(conn);
				const r = needRun();
				const parts = parseSteer(params);
				needRunning(r, 'Steering');
				if (parts.level === 'step_db' && r.targetVpk === 0) refuse('refused', 'level_zero', 'The level is 0 V; a step in dB needs a level to step from. Set a level in volts first.', { target_vpk: 0 });
				const input = r.frames;
				const frame = genFrame(r);
				const clamped = { hz: false, vpk: false };
				if (parts.freq) {
					const v = params[parts.freq] as number;
					const want = parts.freq === 'hz' ? v : parts.freq === 'step_hz' ? r.targetHz + v : r.targetHz * 2 ** v;
					const hz = clamp(Number.isFinite(want) ? want : r.band[0], r.band[0], r.band[1]);
					clamped.hz = Math.abs(hz - want) > 1e-9;
					r.freq = hz;
					r.targetHz = hz;
					r.settledAt = r.frames / r.fs;
					if (r.sweep && !r.sweep.halted) {
						r.sweep.halted = true;
						stateChanged(true);
					}
				}
				if (parts.level) {
					const v = params[parts.level] as number;
					const want = parts.level === 'vpk' ? v : r.targetVpk * 10 ** (v / 20);
					const vpk = clamp(want, 0, r.config.max_drive_vpk);
					clamped.vpk = Math.abs(vpk - want) > 1e-12;
					setTargetVpk(r, vpk);
				}
				rephase(r);
				return { hz: round(r.targetHz, 6), vpk: round(r.targetVpk, 6), clamped, frame, input_frame: input };
			},
			'explorer.sweep': (params, conn) => {
				host.own(conn);
				const r = needRun();
				noOtherKeys(params, ['direction', 'rate_oct_per_min']);
				if (params.direction !== 'up' && params.direction !== 'down') bad('direction', '"direction" is "up" or "down".');
				if (!(isNum(params.rate_oct_per_min) && params.rate_oct_per_min > 0)) bad('rate_oct_per_min', '"rate_oct_per_min" is a number above 0.');
				needRunning(r, 'A sweep');
				const max = r.derived.sweep_max_oct_per_min;
				const asked = params.rate_oct_per_min as number;
				const rate = Math.min(asked, max);
				const direction = params.direction as ExplorerSweepDirection;
				r.sweep = { direction, rate_oct_per_min: rate, halted: false };
				r.targetHz = r.freq;
				if (atEdge(r)) r.sweep.halted = true;
				stateChanged(true);
				rephase(r);
				return { direction, rate_oct_per_min: rate, clamped: rate < asked, frame: genFrame(r), input_frame: r.frames };
			},
			'explorer.hold': (params, conn) => {
				noOtherKeys(params, []);
				host.own(conn);
				const r = needRun();
				needRunning(r, 'Hold');
				if (r.sweep) {
					r.sweep = null;
					r.targetHz = r.freq;
					r.settledAt = r.frames / r.fs;
					stateChanged(true);
					rephase(r);
				}
				return { hz: round(r.freq, 6), frame: genFrame(r), input_frame: r.frames };
			},
			'explorer.stop': (_params, conn) => {
				if (run && isLive() && run.state !== 'stopping' && !run.stop) {
					beginStop(run, 'operator', host.clientName(conn));
				}
				return stateOf(run);
			},
			'explorer.abort': (_params, conn) => {
				if (run && isLive()) {
					const by = host.clientName(conn);
					addLog('note', `abort requested by ${by}`);
					end(run, 'operator_abort', 'aborted', null, by);
				}
				return stateOf(run);
			},
			'explorer.survey': (params) => {
				noOtherKeys(params, ['since']);
				if (params.since !== undefined && !(Number.isInteger(params.since) && (params.since as number) >= 0)) bad('since', '"since" is a revision, a whole number from 0.');
				const r = run;
				const row = controlRow();
				if (!r) return { revision: 0, channel: row?.index ?? null, unit: row?.units ?? null, points: [], peak: null };
				const same = params.since !== undefined && params.since === r.surveyRevision;
				return { revision: r.surveyRevision, channel: row?.index ?? null, unit: row?.units ?? null, points: same ? null : r.survey.map((p) => [...p]), peak: peakOf(r) };
			},
			'explorer.survey.clear': (params, conn) => {
				noOtherKeys(params, []);
				host.own(conn);
				const r = needRun();
				if (r.state !== 'running' && r.state !== 'stopping') wrongState(r, 'Clearing the survey');
				r.survey = [];
				r.lastPointHz = null;
				r.surveyRevision += 1;
				return { revision: r.surveyRevision };
			},
			'explorer.reset': (_params, conn) => {
				host.own(conn);
				const r = needRun();
				notWhileLive('resetting');
				const checked = check(r.config);
				if (checked.errors.length) refuse('bad_request', 'config_invalid', `The configuration no longer checks on this table and station: ${checked.errors.length} error(s).`, { errors: checked.errors, warnings: [] });
				run = newRun(r.config, checked.derived ?? r.derived);
				log = [];
				addLog('state', `reset; configured "${r.config.title ?? ''}" again`);
				stateChanged(true);
				return stateOf(run);
			},
			'explorer.clear': (_params, conn) => {
				host.own(conn);
				notWhileLive('clearing');
				run = null;
				log = [];
				stateChanged(true);
				return stateOf(null);
			},
			'explorer.state': () => stateOf(run),
			'explorer.log': (params) => {
				const since = isNum(params.since) ? Math.max(0, Math.floor(params.since)) : 0;
				return { entries: log.slice(since), next: log.length };
			},
			'explorer.subscribe': (params, conn) => {
				const sub = parseSubscribe(params);
				if (!sub) {
					subs.delete(conn);
					return { default: true, rate_hz: 10, plots: null, quantities: QUANTITIES };
				}
				const entry = { ...sub, acc: 0 };
				subs.set(conn, entry);
				if (run && armed(run)) {
					const r = run;
					setTimeout(() => {
						if (subs.get(conn) === entry && run === r) conn.emit('explorer.data', dataFor(r, sub.plots) as unknown as Record<string, unknown>);
					}, 30);
				}
				return { default: false, rate_hz: sub.rate, plots: sub.plots, quantities: QUANTITIES };
			}
		}
	};
}
