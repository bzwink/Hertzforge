/**
 * The Explorer store over the real client and codec, against the mock control service: the dry
 * run of the draft and its findings on their sections, the setup applied, a run from the quiet
 * start through the level, the frequency, a sweep and a hold to the controlled stop, each steering
 * answer as the service adopted it, every refusal a steering verb meets (the wrong state with its
 * state named, a dB step from 0 V, two parts of one kind, an observer), the run slot shared with
 * the other test types, a window's subscription, and the safe stop with nobody in control. The
 * mock refuses as the service does; these tests hold it to that.
 */
import { describe, expect, it } from 'vitest';
import { RequestError, ServiceClient } from '$lib/protocol/client.svelte';
import { createControlServer, type ControlServerOptions } from '$lib/protocol/mock/control';
import { mockSocketFactory } from '$lib/protocol/mock/socket';
import type { ExplorerData, ExplorerSteerResult } from '$lib/protocol/explorer';
import { RunOpen, workspaceForRun } from '../runOpen.svelte';
import { AcquireState } from '../store.svelte';
import { recordProblem } from '../words';
import { SineStore } from '../sine/sineStore.svelte';
import { ExplorerStore } from './explorerStore.svelte';
import { ExplorerViewsState } from './explorerViews.svelte';
import { explorerLevelStatus } from './explorerWords';

const until = async (cond: () => boolean, ms = 8000) => {
	const t0 = Date.now();
	while (!cond()) {
		if (Date.now() - t0 > ms) throw new Error('timed out waiting');
		await new Promise((r) => setTimeout(r, 5));
	}
};

async function screen(options: ControlServerOptions = {}) {
	const server = createControlServer({ speed: 20, ...options });
	const client = new ServiceClient({ service: 'control', url: 'ws://mock/', socketFactory: mockSocketFactory(server, 1) });
	const acq = new AcquireState(client, { session: null, local: null, checkDelayMs: 0 });
	client.connect();
	await client.whenOpen();
	await acq.sync();
	await acq.claim();
	const explorer = new ExplorerStore(acq, { local: null, checkDelayMs: 0 });
	await explorer.sync();
	/** A second screen on the same service, observing. */
	const observer = async () => {
		const c = new ServiceClient({ service: 'control', url: 'ws://mock/', socketFactory: mockSocketFactory(server, 1) });
		c.connect();
		await c.whenOpen();
		return c;
	};
	return { client, acq, explorer, observer };
}

/** The bench's setup: 20 to 2000 Hz, starting at 250 Hz, a 0.5 V ceiling. */
function ready(explorer: ExplorerStore) {
	const d = explorer.setup;
	d.band_hz = [20, 2000];
	d.start_hz = 250;
	d.max_drive_vpk = 0.5;
	d.acquisition.fs_hz = 12800;
}

const refusal = async (p: Promise<unknown>) => (await p.catch((e: unknown) => e)) as RequestError;

async function running(explorer: ExplorerStore) {
	if (explorer.setupStatus !== 'applied') expect(await explorer.apply()).toBe(true);
	expect(await explorer.start()).toBe(true);
	await until(() => explorer.status?.state === 'running');
}

describe('the Explorer store', () => {
	it('asks for the drive ceiling before the Explorer setup can be applied', async () => {
		const { explorer } = await screen();
		expect(explorer.actions.apply.reason).toBe('Fix the error in the setup first.');
		expect(explorer.fieldErrors.max_drive_vpk?.[0]).toMatch(/no default/);
		expect(explorer.counts.test.errors).toBe(1);
		ready(explorer);
		expect(explorer.actions.apply.enabled).toBe(true);
	});

	it("shows the service's derived numbers for the Explorer draft from the dry run, never its own", async () => {
		const { explorer } = await screen();
		ready(explorer);
		await explorer.checkSetup();
		const d = explorer.derived!;
		expect(d.grid).toEqual({ fs_hz: 12800, block_frames: 256, lookahead_frames: 4, actual: false });
		expect([d.pipeline_frames, d.rise_s, d.stop_s, d.stop_wait_s, d.sweep_max_oct_per_min]).toEqual([8, 6.667, 6.667, 30, 60]);
		expect(d.display).toEqual({ df_hz: 3.125, window_s: 0.32 });
		expect(d.survey.tolerance_hz).toBe(1.5625);
		explorer.setup.slew.drive_db_per_s = 30;
		expect(explorer.derived?.rise_s).not.toBe(1.333);
		await explorer.checkSetup();
		expect(explorer.derived?.rise_s).toBe(1.333);
	});

	it('lands each Explorer configuration finding on its field and section, by the names the service uses', async () => {
		const { explorer, acq } = await screen();
		ready(explorer);
		explorer.setup.band_hz = [20, 6000];
		explorer.setup.start_hz = 10;
		explorer.setup.slew.drive_db_per_s = 1;
		explorer.setup.display.nfft = 3000;
		const res = await explorer.checkSetup();
		expect(res?.errors.map((e) => [e.reason, e.field])).toEqual([
			['band_above_grid', 'band_hz'],
			['out_of_range', 'start_hz'],
			['safe_stop_budget', 'slew.drive_db_per_s'],
			['out_of_range', 'display.nfft']
		]);
		expect(explorer.fieldErrors.band_hz?.[0]).toMatch(/5760 Hz/);
		expect(explorer.fieldErrors['slew.drive_db_per_s']?.[0]).toMatch(/40 s/);
		expect(explorer.counts.test.errors).toBe(4);
		// the ceiling against the drive row's range, and the table's own three on the station's section
		Object.assign(explorer.setup, { band_hz: [20, 2000], start_hz: 250 });
		explorer.setup.slew.drive_db_per_s = 6;
		explorer.setup.display.nfft = 4096;
		const rows = acq.rows.map((r) => ({ ...r }));
		rows.find((r) => r.role === 'drive')!.range_max = 0.4;
		rows.push({ ...rows.find((r) => r.role === 'drive')!, index: 3, description: 'Drive 2' });
		acq.rows = rows;
		expect(await acq.applyTable()).toBe(true);
		const again = await explorer.checkSetup();
		expect(again?.errors.map((e) => e.reason)).toEqual(['ceiling_over_range', 'extra_drive_channels']);
		expect(explorer.counts.station.errors).toBe(1);
		expect(explorer.fieldErrors['station.channels']?.[0]).toMatch(/2 enabled drive rows/);
	});

	it('refuses an Explorer configuration key the wire does not have by name, so no level in another unit can be asked for', async () => {
		const { client } = await screen();
		const e = await refusal(client.request('explorer.configure', { config: { max_drive_vpk: 0.5, level_g: 1 }, dry_run: true }));
		expect([e.reason, e.detail?.param]).toEqual(['bad_params', 'config.level_g']);
		const f = await refusal(client.request('explorer.configure', { config: { band_hz: [20, 2000] }, dry_run: true }));
		expect([f.reason, f.detail?.param]).toEqual(['bad_params', 'config.max_drive_vpk']);
	});

	it('starts an Explorer run quiet at 0 V and raises the level only when asked, on the service answer', async () => {
		const { explorer } = await screen();
		ready(explorer);
		await running(explorer);
		expect([explorer.status?.phase, explorer.status?.drive_vpk, explorer.status?.target_vpk, explorer.status?.frequency_hz]).toEqual(['quiet', 0, 0, 250]);
		expect(explorerLevelStatus(explorer.now).text).toBe('Quiet: the tone is at 0 V');
		expect(explorer.status?.grid?.actual).toBe(true);
		await until(() => explorer.log.some((e) => /rate 12800 Hz adopted/.test(e.text)));
		const res = (await explorer.setVpk(0.2)) as ExplorerSteerResult;
		expect([res.hz, res.vpk, res.clamped]).toEqual([250, 0.2, { hz: false, vpk: false }]);
		expect(res.frame - res.input_frame).toBe(8);
		expect(explorer.answer?.words).toBe('Answered 0.200 V pk');
		await until(() => explorer.now?.phase === 'ramping');
		expect(explorerLevelStatus(explorer.now).text).toBe('Rising to 0.200 V pk');
		await until(() => explorer.now?.phase === 'steady', 10000);
		expect(explorerLevelStatus(explorer.now).text).toBe('Holding 250.0 Hz at 0.200 V pk');
		await explorer.abort();
	}, 15000);

	it('answers an Explorer steer with what the service adopted: clamped to the band and to the ceiling, both parts in one frame', async () => {
		const { explorer } = await screen();
		ready(explorer);
		await running(explorer);
		let r = (await explorer.goToHz(5000))!;
		expect([r.hz, r.clamped.hz]).toEqual([2000, true]);
		expect(explorer.answer?.words).toBe('Answered 2000.0 Hz, clamped to the band');
		r = (await explorer.goToHz(10))!;
		expect([r.hz, r.clamped.hz]).toEqual([20, true]);
		r = (await explorer.setVpk(0.9))!;
		expect([r.vpk, r.clamped.vpk]).toEqual([0.5, true]);
		expect(explorer.answer?.words).toBe('Answered 0.500 V pk, clamped to the ceiling');
		r = (await explorer.steer({ hz: 1000, step_db: -6 }))!;
		expect([r.hz, Math.round(r.vpk * 1e4) / 1e4, r.clamped]).toEqual([1000, 0.2506, { hz: false, vpk: false }]);
		r = (await explorer.stepFrequency(-1))!;
		expect(r.hz).toBe(500);
		await explorer.abort();
	}, 15000);

	it('sends each Explorer steering verb as its own part and shows the service answer, never the request', async () => {
		const { explorer, client } = await screen();
		ready(explorer);
		await running(explorer);
		const sent: [string, unknown][] = [];
		const send = client.request.bind(client);
		(client as unknown as { request: typeof client.request }).request = ((method: string, params?: Record<string, unknown>) => {
			if (method.startsWith('explorer.')) sent.push([method, params]);
			return send(method, params);
		}) as typeof client.request;
		await explorer.setVpk(0.9);
		expect(explorer.answer?.result).toMatchObject({ vpk: 0.5, clamped: { hz: false, vpk: true } });
		await explorer.stepLevel(-6);
		await explorer.stepFrequency(1 / 6);
		await explorer.goToHz(5000);
		await explorer.sweep('down', 4);
		await explorer.hold();
		expect(sent).toEqual([
			['explorer.steer', { vpk: 0.9 }],
			['explorer.steer', { step_db: -6 }],
			['explorer.steer', { step_oct: 1 / 6 }],
			['explorer.steer', { hz: 5000 }],
			['explorer.sweep', { direction: 'down', rate_oct_per_min: 4 }],
			['explorer.hold', {}]
		]);
		expect(explorer.answer?.verb).toBe('hold');
		await explorer.abort();
	}, 15000);

	it('refuses an Explorer dB step from 0 V by name, and two parts of one kind', async () => {
		const { explorer, client } = await screen();
		ready(explorer);
		await running(explorer);
		expect(explorer.verbs.levelStep.reason).toMatch(/set a level in volts first/);
		expect(await explorer.stepLevel(3)).toBeNull();
		expect([explorer.refusal?.reason, explorer.refusal?.title]).toEqual(['level_zero', 'The Level Is 0 V']);
		const e = await refusal(client.request('explorer.steer', { hz: 100, step_hz: 10 }));
		expect([e.reason, e.detail?.param]).toEqual(['bad_params', 'step_hz']);
		const f = await refusal(client.request('explorer.steer', {}));
		expect(f.reason).toBe('bad_params');
		await explorer.abort();
	}, 15000);

	it('sweeps an Explorer tone at the rate the service answers, clamped to 60 times the slew, parks at the edge, and holds where it is', async () => {
		const { explorer } = await screen();
		ready(explorer);
		await running(explorer);
		await explorer.setVpk(0.2);
		const s = (await explorer.sweep('up', 600))!;
		expect([s.rate_oct_per_min, s.clamped]).toEqual([60, true]);
		expect(explorer.answer?.words).toBe('Sweeping up at 60 oct/min, clamped to the fastest sweep');
		await until(() => explorer.now?.phase === 'at_edge', 10000);
		expect(explorer.now?.frequency_hz).toBe(2000);
		expect(explorerLevelStatus(explorer.now).text).toBe('Parked at the top of the band, 2000 Hz');
		await explorer.sweep('down', 30);
		await until(() => explorer.now?.phase === 'sweeping');
		expect(explorerLevelStatus(explorer.now).text).toBe('Sweeping down at 30 oct/min');
		await until(() => (explorer.now?.frequency_hz ?? 2000) < 1500);
		const h = (await explorer.hold())!;
		await until(() => explorer.status?.sweep === null);
		const held = explorer.status!.frequency_hz!;
		expect(Math.abs(held - h.hz)).toBeLessThan(1e-6);
		await new Promise((r) => setTimeout(r, 200));
		expect(explorer.now?.frequency_hz).toBe(held);
		await explorer.abort();
	}, 20000);

	it('refuses every Explorer steering verb once the stop has begun, naming the state, and ramps down to the end', async () => {
		const { explorer, client } = await screen({ speed: 4 });
		ready(explorer);
		await running(explorer);
		await explorer.setVpk(0.2);
		await until(() => explorer.now?.phase === 'steady', 10000);
		expect(await explorer.stop()).toBe(true);
		expect([explorer.status?.state, explorer.status?.phase, explorer.status?.stop]).toEqual(['stopping', 'ramping_down', 'operator']);
		expect(explorer.verbs.levelVolts.reason).toMatch(/nothing raises or moves the drive/);
		expect(explorerLevelStatus(explorer.now).text).toBe('Stopping: the drive is ramping down');
		const e = await refusal(client.request('explorer.steer', { vpk: 0.3 }));
		expect([e.reason, e.detail?.state, e.detail?.phase]).toEqual(['wrong_state', 'stopping', 'ramping_down']);
		expect(await explorer.setVpk(0.3)).toBeNull();
		expect(explorer.refusal?.facts).toEqual(['State: Stopping, Ramping down']);
		for (const [m, p] of [['explorer.sweep', { direction: 'up', rate_oct_per_min: 4 }], ['explorer.hold', {}]] as const) expect((await refusal(client.request(m, p))).reason).toBe('wrong_state');
		await until(() => explorer.status?.state === 'finished', 15000);
		const end = explorer.status!.end!;
		expect([end.outcome, end.vpk]).toEqual(['operator_stop', 0.2]);
		expect(end.ramp_s).toBeGreaterThan(6);
		expect(end.ramp_s).toBeLessThan(7.5);
		expect(explorer.actions.reset.enabled).toBe(true);
	}, 30000);

	it('lets an Explorer observer stop and abort but not steer', async () => {
		const { explorer, observer } = await screen();
		ready(explorer);
		await running(explorer);
		const o = await observer();
		const e = await refusal(o.request('explorer.steer', { vpk: 0.1 }));
		expect(e.reason).toBe('not_controller');
		expect((await refusal(o.request('explorer.survey.clear', {}))).reason).toBe('not_controller');
		const survey = await o.request<{ revision: number }>('explorer.survey', {});
		expect(typeof survey.revision).toBe('number');
		await o.request('explorer.abort', {});
		await until(() => explorer.status?.state === 'finished');
		expect([explorer.status?.end?.outcome, explorer.status?.phase]).toEqual(['operator_abort', 'aborted']);
		expect(explorer.status?.end?.ramp_s).toBeNull();
	}, 15000);

	it('fills the Explorer survey line as the tone visits frequencies, with its peak, and clears it on asking', async () => {
		const { explorer, client } = await screen();
		ready(explorer);
		await running(explorer);
		await explorer.setVpk(0.2);
		await until(() => explorer.now?.phase === 'steady', 10000);
		for (const f of [150, 200, 300]) {
			await explorer.goToHz(f);
			await new Promise((r) => setTimeout(r, 120));
		}
		await until(() => (explorer.frameOfRun?.peak?.hz ?? 0) > 0);
		const s = await client.request<{ revision: number; points: [number, number][]; peak: { hz: number; level: number }; unit: string; channel: number }>('explorer.survey', {});
		expect(s.points.map((p) => p[0])).toEqual([...s.points.map((p) => p[0])].sort((a, b) => a - b));
		expect([s.unit, s.channel]).toEqual(['g', 1]);
		expect(s.peak.level).toBe(Math.max(...s.points.map((p) => p[1])));
		const again = await client.request<{ points: null; revision: number }>('explorer.survey', { since: s.revision });
		if (again.revision === s.revision) expect(again.points).toBeNull();
		expect(await explorer.clearSurvey()).toBeGreaterThan(s.revision);
		const cleared = await client.request<{ points: unknown[] }>('explorer.survey', {});
		expect(cleared.points.length).toBeLessThanOrEqual(1);
		await explorer.abort();
	}, 15000);

	it('trips an Explorer run on an input row abort limit as a watchdog, with its facts', async () => {
		const { explorer, acq } = await screen();
		const rows = acq.rows.map((r) => ({ ...r, abort: { ...r.abort } }));
		rows.find((r) => r.role === 'control')!.abort.accel_g = 0.15;
		acq.rows = rows;
		expect(await acq.applyTable()).toBe(true);
		ready(explorer);
		await running(explorer);
		await explorer.setVpk(0.5);
		await until(() => explorer.status?.state === 'finished', 10000);
		const end = explorer.status!.end!;
		expect(end.outcome).toBe('aborted');
		expect(end.trip).toMatchObject({ kind: 'watchdog', channel: 1, axis: 'accel', limit: 0.15 });
		expect((end.trip as { value: number }).value).toBeGreaterThan(0.15);
	}, 15000);

	it('holds the Explorer run slot against another test type, and is held off by one', async () => {
		const { explorer, acq } = await screen();
		ready(explorer);
		await running(explorer);
		const sine = new SineStore(acq, { local: null, checkDelayMs: 0 });
		await sine.sync();
		sine.setup.limits.drive_vpk_ceiling = 0.5;
		sine.setup.limits.drive_vpk_abort = 0.8;
		expect(await sine.apply()).toBe(true);
		expect(await sine.arm()).toBe(false);
		expect(sine.refusal?.reason).toBe('run_active');
		await explorer.abort();
		await until(() => explorer.status?.state === 'finished');
		expect(await sine.arm()).toBe(true);
		expect(await explorer.reset()).toBe(true);
		expect(await explorer.start()).toBe(false);
		expect(explorer.refusal?.reason).toBe('run_active');
		await sine.abort();
	}, 15000);

	it('reads the Explorer run numbers from its own data frames once it has subscribed to plots, as the service sends them', async () => {
		const { explorer, client } = await screen();
		ready(explorer);
		expect(await explorer.apply()).toBe(true);
		const lives: number[] = [];
		client.on('explorer.live', () => lives.push(1));
		const datas: ExplorerData[] = [];
		client.on('explorer.data', (d) => datas.push(d as unknown as ExplorerData));
		const sub = await client.request<{ plots: unknown[] }>('explorer.subscribe', { rate_hz: 10, plots: [{ id: 'a', quantity: 'survey' }, { id: 'b', quantity: 'spectrum' }, { id: 'c', quantity: 'time' }, { id: 'd', quantity: 'readings' }] });
		expect(sub.plots).toEqual([
			{ id: 'a', quantity: 'survey', points: null },
			{ id: 'b', quantity: 'spectrum', channels: 'all' },
			{ id: 'c', quantity: 'time', points: 1024 },
			{ id: 'd', quantity: 'readings' }
		]);
		expect(await explorer.start()).toBe(true);
		await explorer.setVpk(0.2);
		await until(() => datas.some((d) => d.plots.every((p) => p.quantity === 'survey' || p.views.length > 0)));
		const d = datas.at(-1)!;
		const time = d.plots.find((p) => p.quantity === 'time')!;
		expect(typeof time.first_frame).toBe('number');
		expect(d.plots.find((p) => p.quantity === 'spectrum')!.channels).toEqual([1]);
		expect(d.plots.find((p) => p.quantity === 'readings')!.first_frame).toBeNull();
		expect(typeof explorer.frameOfRun?.drive_vpk).toBe('number');
		expect(lives).toHaveLength(0);
		await explorer.abort();
	}, 15000);

	it("refuses an Explorer plot's key the quantity does not take, and every page the shell offers is accepted", async () => {
		const { client } = await screen();
		for (const [plot, param] of [
			[{ id: 'a', quantity: 'survey', channels: 'all' }, 'plots[0].channels'],
			[{ id: 'a', quantity: 'spectrum', points: 512 }, 'plots[0].points'],
			[{ id: 'a', quantity: 'readings', points: 512 }, 'plots[0].points'],
			[{ id: 'a', quantity: 'time', points: 10 }, 'plots[0].points'],
			[{ id: 'a', quantity: 'phase' }, 'plots[0].quantity']
		] as const) {
			const e = await refusal(client.request('explorer.subscribe', { plots: [plot] }));
			expect([e.reason, e.detail?.param]).toEqual(['bad_params', param]);
		}
		const views = new ExplorerViewsState(client, { setupTitle: () => 'x', showing: () => true, storage: null, session: null, events: null, subscribeDelayMs: 0 });
		for (const page of views.pages) {
			views.selectPage(page.id);
			const params = views.hooks.feed!.params(views.wanted, views.layout.rate);
			const res = await client.request<{ default: boolean }>('explorer.subscribe', params);
			expect(res.default).toBe(false);
		}
		// a spectrum asked for the limited channels, which the service does not take, is sent as every channel
		const spectrum = views.addPlot('spectrum') as string;
		views.updatePlot(spectrum, { channels: 'limited' });
		const sent = views.hooks.feed!.params(views.wanted, views.layout.rate) as { plots: { quantity: string; channels?: unknown }[] };
		expect(sent.plots.find((p) => p.quantity === 'spectrum')?.channels).toBe('all');
		await expect(client.request('explorer.subscribe', sent)).resolves.toMatchObject({ default: false });
	});

	it('safe-stops an Explorer run with its ramp once nobody has held control for the timeout', async () => {
		const { explorer, acq } = await screen({ timeoutS: 0.5 });
		ready(explorer);
		await running(explorer);
		await explorer.setVpk(0.2);
		await acq.release();
		await until(() => explorer.status?.state === 'stopping' || explorer.status?.state === 'finished', 5000);
		expect(explorer.status?.stop).toBe('safe_stop');
		await until(() => explorer.status?.state === 'finished', 10000);
		expect(explorer.status?.end?.outcome).toBe('safe_stop');
	}, 20000);

	it("names the Explorer run's folder from start, and says when its record has a problem while the run goes on", async () => {
		const { explorer } = await screen({ speed: 4, record: 'problem' });
		ready(explorer);
		explorer.setup.capture.run_name = 'fixture_a';
		explorer.setup.record.operator = 'A. Operator';
		await running(explorer);
		expect(explorer.status?.record).toEqual({ folder: '~/Hertzforge_Data/fixture_a', streams: 'on', problem: null });
		expect(explorer.run?.record?.folder).toBe('~/Hertzforge_Data/fixture_a');
		expect(explorer.served?.config?.operator).toBe('A. Operator');
		expect(explorer.recordChannels).toBe(explorer.acq.rows.filter((r) => r.enabled).length);
		expect(explorer.recordRateHz).toBe(12800);
		await until(() => !!explorer.status?.record?.problem, 10000);
		expect(explorer.status?.state).toBe('running');
		// the notice the shared Notices draws from the run head while the run goes on
		const words = recordProblem(explorer.run?.record, false);
		expect(words?.title).toBe('Recording Has a Problem');
		expect(words?.text).toMatch(/^The run goes on\. Recording of the inputs stream stopped/);
		await until(() => explorer.log.some((e) => e.kind === 'record'));
		expect(await explorer.stop()).toBe(true);
		await until(() => explorer.status?.state === 'finished', 15000);
		expect(explorer.status?.end?.outcome).toBe('operator_stop');
		expect(explorer.run?.record?.folder).toBe('~/Hertzforge_Data/fixture_a');
	}, 30000);

	it('opens a finished Explorer run in the Signals workspace, where its time histories go', async () => {
		expect(workspaceForRun('explorer')).toBe('signals');
		const shown: string[] = [];
		const open = new RunOpen({ bring: async () => ['Channel Table', 'inputs', 'drives', 'Run Record'], show: (w) => shown.push(w) }, 'explorer');
		expect(await open.open('/data/hf/explorer')).toBe(true);
		expect(shown).toEqual(['signals']);
		expect(open.added).toContain('inputs');
	});

	it("refuses an Explorer start whose record cannot be written by name, before anything is driven", async () => {
		const { explorer } = await screen({ record: 'unwritable' });
		ready(explorer);
		expect(await explorer.apply()).toBe(true);
		expect(await explorer.start()).toBe(false);
		expect(explorer.refusal?.reason).toBe('record_unwritable');
		expect(explorer.refusal?.title).toBe("The Run's Record Cannot Be Written");
		expect(explorer.refusal?.facts).toContain('Path: ~/Hertzforge_Data/explorer');
		await until(() => explorer.status?.state === 'finished');
		expect([explorer.status?.end?.outcome, explorer.status?.t_s, explorer.status?.record]).toEqual(['arm_refused', null, null]);
	});

	it('round-trips every Explorer setup field through what the service holds', async () => {
		const { explorer } = await screen();
		ready(explorer);
		explorer.setup.title = 'Fixture A';
		explorer.setup.slew = { freq_oct_per_s: 2, drive_db_per_s: 12 };
		explorer.setup.display.nfft = 8192;
		explorer.setup.capture.run_name = 'fixture_a';
		explorer.setup.metadata = [{ key: 'run_number', label: 'Run Number', value: '3' }];
		explorer.setup.steps.fine_per_oct = 48;
		expect(await explorer.apply()).toBe(true);
		await until(() => explorer.served?.config?.title === 'Fixture A');
		const wire = structuredClone(explorer.wire);
		explorer.setup = { ...explorer.setup, title: 'changed' };
		expect(explorer.setupStatus).toBe('changed');
		expect(explorer.adoptServed()).toBe(true);
		expect(explorer.wire).toEqual(wire);
		expect(explorer.setup.steps.fine_per_oct).toBe(48);
		expect(explorer.setupStatus).toBe('applied');
	});
});
