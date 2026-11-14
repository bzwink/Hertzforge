/**
 * The TWR goldens (schema/golden/) through the shell's codec, types, setup rules and words. The
 * files are read from the folder itself, so this suite, the Rust suite and the Python suite all
 * read the same frames: a golden added there is checked here without a list to update. Each check
 * that names a golden runs when that golden is in the folder, as a text frame (`.json`) or a binary
 * one (`.bin` with its `.expected.json`). The last part holds the mock control service's frames to
 * the service's form.
 */
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { decodeFrame, encodeFrame, withShape } from './codec';
import type { Dtype, Event, ReadingView, Request, ResponseErr, ResponseOk, SpectrumView, View } from './types';
import type { LawsListResult } from './mimo';
import {
	TWR_POINTS_QUANTITIES,
	TWR_TOPICS,
	type TwrCheckRequirement,
	type TwrConfigureParams,
	type TwrConfigureResult,
	type TwrData,
	type TwrLawCheckCase,
	type TwrLawsCheckResult,
	type TwrLive,
	type TwrPlayRow,
	type TwrPlaysResult,
	type TwrQuantity,
	type TwrReferenceImportResult,
	type TwrReferencePreviewResult,
	type TwrState,
	type TwrSubscribeParams,
	type TwrSubscribeResult
} from './twr';
import { ServiceClient } from './client.svelte';
import { createControlServer } from './mock/control';
import { mockSocketFactory } from './mock/socket';
import { lawsOfWorld } from '$lib/acquire/law/lawForm';
import { AcquireState } from '$lib/acquire/store.svelte';
import { defaultPulseScreen, pulseSubscription } from '$lib/acquire/pulse/pulseScreen';
import type { PlotPanel } from '$lib/acquire/screen/screen.svelte';
import { TWR_PLOT_TYPES, TWR_QUANTITIES, TWR_SCREEN } from '$lib/acquire/twr/twrLayout';
import { defaultTwrSetup, fromTwrConfig, toTwrConfig, twrByKey, twrSectionOf } from '$lib/acquire/twr/twrSetup';
import { TwrStore } from '$lib/acquire/twr/twrStore.svelte';
import { playProgress, twrLevelStatus } from '$lib/acquire/twr/twrWords';

const DIR = fileURLToPath(new URL('../../../../schema/golden/', import.meta.url));
const golden = (name: string) => `${DIR}${name}`;
const has = (name: string) => existsSync(golden(name));
const readJson = (name: string) => JSON.parse(readFileSync(golden(name), 'utf-8'));
const readBin = (name: string) => new Uint8Array(readFileSync(golden(name)));
/** A golden by its name without the extension, as a text frame or a binary one. */
const hasFrame = (base: string) => has(`${base}.json`) || has(`${base}.bin`);
const frame = <T>(base: string): T => (has(`${base}.json`) ? readJson(`${base}.json`) : decodeFrame(readBin(`${base}.bin`))) as T;

/** Every TWR golden in the folder: its text frames and its binary frames. */
const TWR = readdirSync(DIR).filter((f) => /(^|_)twr(_|\.)/.test(f));
const TEXT = TWR.filter((f) => f.endsWith('.json') && !f.endsWith('.expected.json')).sort();
const BIN = TWR.filter((f) => f.endsWith('.bin')).sort();
const QUANTITIES: readonly TwrQuantity[] = TWR_QUANTITIES;
const REQUIREMENTS: TwrCheckRequirement[] = ['first_play', 'every_rung', 'first_rung_by', 'full_level', 'survive'];
const ROW_FIELDS: (keyof TwrPlayRow)[] = ['index', 'kind', 'level_db', 'rung', 'iteration', 'rms_error_pct', 'peak_error_pct', 'spectral_error_db', 'gated', 'clipped_samples', 'clamped_bins', 'drive_vpk', 'response_peak', 'lag_frames', 'drift_frames', 'start_frame', 'frames', 'at_s', 'law'];

type Expected = { dtype: Dtype; shape: number[]; values: number[] };
const isExpected = (v: unknown): v is Expected => typeof v === 'object' && v !== null && 'dtype' in v && 'values' in v;
function toTyped(node: unknown): unknown {
	if (isExpected(node)) {
		const ctor = { f32: Float32Array, f64: Float64Array, i32: Int32Array, u32: Uint32Array, u8: Uint8Array }[node.dtype];
		return withShape(ctor.from(node.values), node.shape);
	}
	if (Array.isArray(node)) return node.map(toTyped);
	if (node !== null && typeof node === 'object') return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, toTyped(v)]));
	return node;
}

/**
 * A plot's point count only where the quantity takes it, never channels; `filled` (the service's
 * answer, every default filled in) also has the count on every plot that takes one.
 */
function keysAsTheServiceTakes(plots: { quantity: TwrQuantity; points?: number; channels?: unknown }[], filled = false) {
	for (const p of plots) {
		const has = 'points' in p && p.points !== undefined;
		if (filled || has) expect(has).toBe(TWR_POINTS_QUANTITIES.includes(p.quantity));
		expect('channels' in p).toBe(false);
	}
}

/** The form of a view the shell compares with the service's: titles, axes, series names and roles. */
function shapeOf(v: View): unknown {
	if (v.view === 'spectrum') return { view: v.view, title: v.title, x: v.x, y: v.y, series: v.series.map((s) => [s.name, s.role]), link: v.link ?? null };
	if (v.view === 'reading') return { view: v.view, title: v.title, rows: (v as ReadingView).rows.map((r) => r.label) };
	return { view: v.view, title: (v as { title?: string }).title ?? null };
}

const testConfig = () => (has('request_twr_configure.json') ? (readJson('request_twr_configure.json') as Request<TwrConfigureParams>).params.config : null);
const serviceLaws = () => (has('response_laws_list_twr.json') ? lawsOfWorld((readJson('response_laws_list_twr.json') as ResponseOk<LawsListResult>).result.laws, 'twr') : null);

describe('TWR goldens in the folder', () => {
	it('are text frames or binary frames with their expected form', () => {
		for (const f of BIN) expect(TWR).toContain(f.replace(/\.bin$/, '.expected.json'));
		for (const f of TWR) expect(f).toMatch(/\.(json|bin)$/);
	});

	for (const name of TEXT) {
		it(`${name} decodes and re-encodes as the same compact text frame`, () => {
			const text = readFileSync(golden(name), 'utf-8');
			const message = decodeFrame(text);
			expect(message).toEqual(JSON.parse(text));
			expect(encodeFrame(message)).toBe(JSON.stringify(JSON.parse(text)));
		});
	}

	for (const name of BIN) {
		it(`${name} is a binary frame that decodes to its expected form`, () => {
			const bin = readBin(name);
			expect(new TextDecoder().decode(bin.slice(0, 4))).toBe('SDS1');
			const expected = toTyped(readJson(name.replace(/\.bin$/, '.expected.json'))) as Record<string, unknown>;
			expect(decodeFrame(bin)).toEqual(expected);
		});
	}
});

describe('TWR goldens through the shell', () => {
	it.skipIf(!has('request_twr_configure.json'))('a configuration request survives the setup draft and back, every field', () => {
		const config = testConfig()!;
		const law = serviceLaws()?.find((l) => l.id === config.control?.law?.id) ?? null;
		const back = toTwrConfig(fromTwrConfig(config), law);
		if (law) expect(back).toEqual(config);
		else expect({ ...back, control: { ...back.control, law: config.control?.law } }).toEqual(config);
	});

	it.skipIf(!has('request_twr_configure.json'))("the shell's own default setup asks for the same kinds of fields the service's example has", () => {
		const config = testConfig()!;
		const d = defaultTwrSetup();
		d.reference.sha256 = config.reference.sha256;
		d.limits.drive_vpk_ceiling = 0.5;
		const mine = toTwrConfig(d, null);
		expect(Object.keys(mine).sort()).toEqual(Object.keys(config).sort());
		for (const k of ['reference', 'prep', 'pads', 'acquisition', 'identification', 'inverse', 'correction', 'control', 'schedule', 'adaptation', 'limits', 'feasibility', 'record'] as const) {
			expect(Object.keys(mine[k] ?? {}).sort()).toEqual(Object.keys(config[k] ?? {}).sort());
		}
	});

	it.skipIf(!has('response_laws_list_twr.json'))('the TWR world lists its built-in law first, with a form the shell draws', () => {
		const list = (readJson('response_laws_list_twr.json') as ResponseOk<LawsListResult>).result;
		const laws = lawsOfWorld(list.laws, 'twr');
		expect(laws.length).toBe(list.laws.length);
		expect([laws[0].id, laws[0].builtin, laws[0].check.hardware]).toEqual(['twr_correct', true, true]);
		for (const l of laws) {
			for (const p of l.params) expect(['number', 'integer', 'toggle', 'choice']).toContain(p.kind);
			for (const d of l.diagnostics) expect(d.layout).toBeNull();
			expect(l.diagnostics.length).toBeLessThanOrEqual(8);
		}
	});

	it.skipIf(!has('response_twr_reference_import.json') && !has('response_twr_reference_import.bin'))('a reference read is one the setup names by its SHA-256, drawn as one Reference view', () => {
		const r = frame<ResponseOk<TwrReferenceImportResult>>('response_twr_reference_import').result;
		expect(r.reference.sha256).toMatch(/^[0-9a-f]{64}$/);
		expect(Object.keys(r.reference).sort()).toEqual(['duration_s', 'format', 'fs_hz', 'name', 'path', 'peak', 'rms', 'samples', 'sha256', 'units']);
		expect(r.views.map((v) => v.title)).toEqual(['Reference']);
		const d = defaultTwrSetup();
		d.reference = { sha256: r.reference.sha256, path: r.reference.path };
		expect(toTwrConfig(d, null).reference).toEqual({ sha256: r.reference.sha256, path: r.reference.path });
	});

	it.skipIf(!hasFrame('response_twr_reference_preview'))('the preparation carries the grid, section, kinematics, coverage and feasibility, and four views', () => {
		const r = frame<ResponseOk<TwrReferencePreviewResult>>('response_twr_reference_preview').result;
		expect(Object.keys(r.derived).sort()).toEqual(['coverage', 'feasibility', 'grid', 'kinematics', 'section']);
		expect(Math.log2(r.derived.grid.record_len) % 1).toBe(0);
		expect(r.views.map((v) => v.title)).toEqual(['Section', 'Spectrum', 'Velocity', 'Displacement']);
	});

	it.skipIf(!has('response_twr_configure.json'))('the configure result is a configured state with what the service derived', () => {
		const res = (readJson('response_twr_configure.json') as ResponseOk<TwrConfigureResult>).result;
		expect([res.state.state, res.state.phase]).toEqual(['configured', 'idle']);
		expect(twrLevelStatus({ ...res.state, play: null }).text).toBe('Set up, ready to arm');
		expect(res.derived.grid.actual).toBe(false);
		for (const k of ['reference', 'call_budget_s', 'plays_target', 'identification_s'] as const) expect(res.derived).toHaveProperty(k);
	});

	it.skipIf(!has('response_error_twr_config_invalid.json'))("an invalid configuration's findings land on the fields and sections they name", () => {
		const err = readJson('response_error_twr_config_invalid.json') as ResponseErr;
		expect([err.error.code, err.error.reason]).toEqual(['bad_request', 'config_invalid']);
		const items = (err.error.detail as { errors: { reason: string; field?: string | null; message: string }[] }).errors;
		const keys = Object.keys(twrByKey(items));
		for (const k of keys) expect(k).not.toMatch(/^config\.|\[/);
		for (const k of keys) expect(['station', 'reference', 'identification', 'control', 'schedule', 'limits', 'capture']).toContain(twrSectionOf(k));
	});

	it.skipIf(!has('event_twr_state.json'))('the state event reads in the level card words', () => {
		const ev = readJson('event_twr_state.json') as Event<TwrState>;
		expect(ev.topic).toBe(TWR_TOPICS.state);
		const s = ev.data;
		const words = twrLevelStatus({ ...s, play: null }).text;
		if (s.state === 'running' && s.phase === 'playing') expect(words).toMatch(/^Playing \d+ of \d+ at /);
		if (s.state === 'running' && s.phase === 'iterating') expect(words).toMatch(/^Correcting at .+, iteration \d+ of \d+$/);
		if (s.last_play) expect(Object.keys(s.last_play).sort()).toEqual([...ROW_FIELDS].sort());
	});

	it.skipIf(!hasFrame('event_twr_live'))('the live frame carries the play in flight, the last metrics and the Scope', () => {
		const ev = frame<Event<TwrLive>>('event_twr_live');
		expect(ev.topic).toBe(TWR_TOPICS.live);
		const l = ev.data;
		expect(l.views.map((v) => v.title)).toEqual(['Scope']);
		if (l.play) {
			expect(Object.keys(l.play).sort()).toEqual(['frames', 'frames_done', 'index', 'iteration', 'kind']);
			expect(playProgress(l.play)).toBeGreaterThanOrEqual(0);
		}
		if (l.last) expect(Object.keys(l.last).sort()).toEqual(['gated', 'peak_error_pct', 'rms_error_pct', 'spectral_error_db']);
	});

	it.skipIf(!hasFrame('event_twr_data'))("a data frame's plots are quantities the shell draws", () => {
		const ev = frame<Event<TwrData>>('event_twr_data');
		expect(ev.topic).toBe(TWR_TOPICS.data);
		expect('views' in ev.data).toBe(false);
		for (const p of ev.data.plots) {
			expect(QUANTITIES).toContain(p.quantity);
			for (const v of p.views) expect(['spectrum', 'reading', 'table']).toContain(v.view);
		}
	});

	it.skipIf(!has('request_twr_subscribe.json'))('a subscription asks only for quantities the shell knows, a point count only where it applies', () => {
		const p = (readJson('request_twr_subscribe.json') as Request<TwrSubscribeParams>).params;
		if ('plots' in p) {
			for (const plot of p.plots) expect(QUANTITIES).toContain(plot.quantity);
			keysAsTheServiceTakes(p.plots);
		}
	});

	it('every plot type the shell offers subscribes with a point count on the record plots only', () => {
		const first = defaultPulseScreen(TWR_SCREEN, TWR_PLOT_TYPES).pages.flatMap((pg) => pg.panels.filter((x): x is PlotPanel => x.kind === 'plot'));
		const one = TWR_PLOT_TYPES.map((t, i): PlotPanel => ({ ...first[0], id: `pl-${i}`, plot: { ...first[0].plot, quantity: t.id } }));
		for (const plots of [first, one]) {
			const sent = pulseSubscription(TWR_PLOT_TYPES, plots, 5) as { plots: { quantity: TwrQuantity }[] };
			keysAsTheServiceTakes(sent.plots, true);
		}
		expect(TWR_PLOT_TYPES.map((t) => t.id).sort()).toEqual(TWR_QUANTITIES.filter((q) => q !== 'plays').sort());
	});

	it.skipIf(!has('response_twr_subscribe.json'))('the subscription is answered with every default filled in', () => {
		const r = (readJson('response_twr_subscribe.json') as ResponseOk<TwrSubscribeResult>).result;
		expect([...r.quantities].sort()).toEqual([...QUANTITIES].sort());
		if (r.plots) keysAsTheServiceTakes(r.plots, true);
	});

	it.skipIf(!has('response_twr_plays.json'))('a plays row has every field the plays table reads', () => {
		const r = (readJson('response_twr_plays.json') as ResponseOk<TwrPlaysResult>).result;
		for (const row of r.plays) expect(Object.keys(row).sort()).toEqual([...ROW_FIELDS].sort());
		expect(r.next).toBeGreaterThanOrEqual(r.plays.length);
	});

	it.skipIf(!has('response_laws_check_twr.json'))("a law check's cases name a requirement the shell knows, with every field the shell reads", () => {
		const r = (readJson('response_laws_check_twr.json') as ResponseOk<TwrLawsCheckResult>).result;
		expect(r.law.world).toBe('twr');
		const fields: (keyof TwrLawCheckCase)[] = ['name', 'plant', 'requirement', 'passed', 'detail', 'end', 'trip', 'plays', 'rungs', 'full_level_error_pct', 'lag_frames', 'lag_measured', 'calls', 'misses', 'refused', 'clamped_bins', 'worst_peak_rise_db', 'worst_bin_rise_db', 'worst_call_ms', 'budget_ms', 'max_drive_vpk'];
		for (const c of r.report.cases) {
			expect(REQUIREMENTS).toContain(c.requirement);
			expect(Object.keys(c).sort()).toEqual([...fields].sort());
		}
	});
});

describe("the mock control service's TWR frames have the service's form", () => {
	const until = async (cond: () => boolean, ms = 8000) => {
		const t0 = Date.now();
		while (!cond()) {
			if (Date.now() - t0 > ms) throw new Error('timed out waiting');
			await new Promise((r) => setTimeout(r, 5));
		}
	};

	async function mockRun() {
		const server = createControlServer({ speed: 20 });
		const client = new ServiceClient({ service: 'control', url: 'ws://mock/', socketFactory: mockSocketFactory(server, 1) });
		const acq = new AcquireState(client, { session: null, local: null, checkDelayMs: 0 });
		client.connect();
		await client.whenOpen();
		await acq.sync();
		await acq.claim();
		const twr = new TwrStore(acq, { local: null, checkDelayMs: 0 });
		await twr.sync();
		/** A second screen on the default subscription. */
		const observer = async () => {
			const c = new ServiceClient({ service: 'control', url: 'ws://mock/', socketFactory: mockSocketFactory(server, 1) });
			c.connect();
			await c.whenOpen();
			return c;
		};
		return { client, twr, observer };
	}

	it.skipIf(!has('response_laws_list_twr.json'))("the mock's built-in law is the service's", async () => {
		const { twr } = await mockRun();
		expect(twr.law).toEqual(serviceLaws()![0]);
	});

	it.skipIf(!has('event_twr_state.json'))("the mock's state has the service's fields", async () => {
		const { client } = await mockRun();
		const mine = await client.request<TwrState>('twr.state');
		const theirs = (readJson('event_twr_state.json') as Event<TwrState>).data;
		expect(Object.keys(mine).sort()).toEqual(Object.keys(theirs).sort());
	});

	it.skipIf(!hasFrame('response_twr_reference_import') || !hasFrame('response_twr_reference_preview'))("the mock's reference and its preparation, view for view", async () => {
		const { client } = await mockRun();
		const text = ['time_s, accel_g', ...Array.from({ length: 2000 }, (_, i) => `${(i / 12800).toFixed(10)}, ${(0.2 * Math.sin((2 * Math.PI * 250 * i) / 12800)).toFixed(6)}`)].join('\n');
		const mine = await client.request<TwrReferenceImportResult>('twr.reference.import', { text });
		const theirs = frame<ResponseOk<TwrReferenceImportResult>>('response_twr_reference_import').result;
		expect(Object.keys(mine.reference).sort()).toEqual(Object.keys(theirs.reference).sort());
		expect(mine.views.map(shapeOf)).toEqual(theirs.views.map(shapeOf));
		const prep = await client.request<TwrReferencePreviewResult>('twr.reference.preview', { reference: { sha256: mine.reference.sha256 }, feasibility: { disp_pkpk_in: 1 } });
		const theirPrep = frame<ResponseOk<TwrReferencePreviewResult>>('response_twr_reference_preview').result;
		expect(prep.views.map(shapeOf)).toEqual(theirPrep.views.map(shapeOf));
		for (const k of ['grid', 'section', 'kinematics', 'coverage', 'feasibility'] as const) expect(Object.keys(prep.derived[k]).sort()).toEqual(Object.keys(theirPrep.derived[k]).sort());
		expect(prep.derived.feasibility.checks.map((c) => c.name)).toEqual(theirPrep.derived.feasibility.checks.map((c) => c.name));
	});

	it.skipIf(!hasFrame('event_twr_data') || !has('request_twr_subscribe.json'))("a subscribed screen's data frame and the live frame, view for view", async () => {
		const { client, twr, observer } = await mockRun();
		expect(await twr.importReference({ path: '/data/references/r1.csv' })).toBe(true);
		twr.setup.limits.drive_vpk_ceiling = 1;
		twr.setup.identification.review = false;
		const want = (readJson('request_twr_subscribe.json') as Request<TwrSubscribeParams>).params;
		let data: TwrData | null = null;
		let live: TwrLive | null = null;
		let liveToSubscribed = 0;
		client.on(TWR_TOPICS.data, (d) => (data = d as unknown as TwrData));
		// The subscribed screen gets its data frame in place of the live frame; the other screen gets the live frame.
		client.on(TWR_TOPICS.live, () => liveToSubscribed++);
		(await observer()).on(TWR_TOPICS.live, (d) => (live = d as unknown as TwrLive));
		await client.request('twr.subscribe', want as unknown as Record<string, unknown>);
		expect(await twr.apply()).toBe(true);
		expect(await twr.start()).toBe(true);
		await until(() => twr.status?.state === 'finished', 20000);
		await until(() => (data as TwrData | null)?.state === 'finished');
		const theirs = frame<Event<TwrData>>('event_twr_data').data;
		const mine = data as unknown as TwrData;
		expect(Object.keys(mine).sort()).toEqual(Object.keys(theirs).sort());
		for (const p of theirs.plots) {
			const m = mine.plots.find((x) => x.id === p.id)!;
			expect(m.quantity).toBe(p.quantity);
			// The scope is empty between plays, and the run has ended; every other plot is drawn.
			if (p.quantity !== 'scope') expect(m.views.length).toBeGreaterThan(0);
			if (p.views.length && m.views.length) expect(m.views.map(shapeOf)).toEqual(p.views.map(shapeOf));
		}
		if (has('response_twr_plays.json')) {
			const rows = (await client.request<TwrPlaysResult>('twr.plays', { since: 0 })).plays;
			const their = (readJson('response_twr_plays.json') as ResponseOk<TwrPlaysResult>).result.plays[0];
			for (const row of rows) expect(Object.keys(row).sort()).toEqual(Object.keys(their).sort());
			for (const row of rows) if (row.law) expect(Object.keys(row.law).sort()).toEqual(Object.keys(their.law!).sort());
		}
		expect(liveToSubscribed).toBe(0);
		if (hasFrame('event_twr_live')) {
			const l = frame<Event<TwrLive>>('event_twr_live').data;
			expect(Object.keys(live as unknown as TwrLive).sort()).toEqual(Object.keys(l).sort());
			const head = (v: View) => (v.view === 'spectrum' ? { view: v.view, title: v.title, x: (v as SpectrumView).x, y: (v as SpectrumView).y } : { view: v.view });
			expect((live as unknown as TwrLive).views.map(head)).toEqual(l.views.map(head));
		}
	}, 30000);
});
