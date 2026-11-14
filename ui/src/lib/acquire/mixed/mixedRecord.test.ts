/**
 * Mixed Mode's run record in the shell, over the real client and codec against the mock control
 * service: the record's fields reach the service and come back from the configuration it holds;
 * from arm the state names the folder, the Run card's line is drawn from it, and a problem is a
 * notice while the run goes on; a record that cannot be written refuses the arm in words; the
 * service's own capture is gone from the setup; a finished run's Open in Analysis lands in the
 * Sine workspace, where the analysis side's mixed-mode data sheet is. The shared pieces are
 * Random's (RunRecordEditor, RunRecordLine, RunOpenActions, runOpen); these tests hold Mixed Mode
 * to them.
 */
import { describe, expect, it, vi } from 'vitest';
import { ServiceClient } from '$lib/protocol/client.svelte';
import { createControlServer, runFolderName, type ControlServerOptions } from '$lib/protocol/mock/control';
import { mockSocketFactory } from '$lib/protocol/mock/socket';
import { RunOpen, workspaceForRun } from '../runOpen.svelte';
import { recordBytesPerHour } from '../setup';
import { AcquireState } from '../store.svelte';
import { recordProblem } from '../words';
import { fromMixedConfig, localErrors, MIXED_SECTIONS, mixedSectionOf, toMixedConfig } from './mixedSetup';
import { MixedStore } from './mixedStore.svelte';

vi.setConfig({ testTimeout: 40000 });

const until = async (cond: () => boolean, ms = 15000) => {
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
	const mixed = new MixedStore(acq, { local: null, checkDelayMs: 0 });
	await mixed.sync();
	return { client, acq, mixed };
}

/** A short automatic run with no review hold, and the operator's words. */
function short(mixed: MixedStore) {
	const d = mixed.setup;
	d.limits.drive_vpk_ceiling = 4;
	d.limits.drive_vpk_abort = 6;
	d.schedule.mode = 'auto';
	d.schedule.levels = [{ db: -6, dwell_s: 1 }, { db: 0, dwell_s: 0 }];
	d.schedule.full_level_s = 2;
	d.control.review_after_sysid = false;
	d.control.part_ramp_s = 0.5;
	d.control.floor_settle_s = 1;
	d.record = { operator: 'A. Operator', article: 'Bracket, serial 7', notes: ' first article ', document: 'Test procedure 12', clause: '4.3', streams: true };
}

describe("Mixed Mode's run record", () => {
	it('is a section of the setup, named as Random names its panel, with no capture section', () => {
		expect(MIXED_SECTIONS.map((s) => s.id)).toContain('record');
		expect(MIXED_SECTIONS.find((s) => s.id === 'record')?.label).toBe('Run Record');
		expect(MIXED_SECTIONS.map((s) => s.id)).not.toContain('capture');
		expect(mixedSectionOf('specification.document')).toBe('record');
		expect(mixedSectionOf('notes')).toBe('record');
	});

	it("sends the operator's words and the streams switch, and reads them back", async () => {
		const { mixed } = await screen();
		short(mixed);
		const c = toMixedConfig(mixed.setup, { floor: null, tones: null });
		expect('capture' in c).toBe(false);
		expect([c.operator, c.article, c.notes, c.specification, c.record]).toEqual(['A. Operator', 'Bracket, serial 7', 'first article', { document: 'Test procedure 12', clause: '4.3' }, { streams: true }]);
		expect(fromMixedConfig(c).record).toEqual({ ...mixed.setup.record, notes: 'first article' });
		expect(await mixed.apply()).toBe(true);
		expect(mixed.runConfig?.operator).toBe('A. Operator');
		// the size of an hour of streams at the rate the run asks for
		expect(mixed.recordRateHz).toBe(mixed.setup.acquisition.fs_hz);
		expect(recordBytesPerHour(mixed.recordChannels, mixed.recordRateHz)).toBe(mixed.recordChannels * mixed.setup.acquisition.fs_hz * 4 * 3600);
	});

	it('names a clause without its document before anything is sent', async () => {
		const { mixed } = await screen();
		short(mixed);
		mixed.setup.record.document = '';
		expect(localErrors(mixed.setup)['specification.document']?.[0]).toMatch(/document/);
	});

	it('names the folder from arm to the end, the time histories off when the setup says so', async () => {
		const { mixed } = await screen({ dataRoot: '/data/hf' });
		short(mixed);
		mixed.setup.record.streams = false;
		expect(await mixed.apply()).toBe(true);
		expect(mixed.status?.record).toBeUndefined();
		expect(await mixed.start()).toBe(true);
		await until(() => !!mixed.status?.record);
		const record = mixed.status!.record!;
		expect(record.folder).toMatch(/^\/data\/hf\/\d{8}-\d{6}_mixed_mixed-\d{4}$/);
		expect(record.streams).toBe('off');
		expect(mixed.run?.record?.folder).toBe(record.folder);
		await until(() => mixed.status?.state === 'finished', 30000);
		expect(mixed.status?.record?.folder).toBe(record.folder);
		expect(runFolderName(new Date(Date.UTC(2026, 10, 13, 18, 4, 5)), 'mixed-0001', 'mixed')).toBe('20261113-180405_mixed_mixed-0001');
	});

	it('says a recording problem while the run goes on, and the run still ends as it would', async () => {
		const { mixed } = await screen({ record: 'problem' });
		short(mixed);
		expect(await mixed.apply()).toBe(true);
		expect(await mixed.start()).toBe(true);
		await until(() => !!mixed.run?.record?.problem, 30000);
		const words = recordProblem(mixed.run?.record, false);
		expect(words?.title).toBe('Recording Has a Problem');
		expect(words?.text).toMatch(/^The run goes on\. Recording of the inputs stream stopped/);
		await until(() => mixed.status?.state === 'finished', 30000);
		expect(mixed.status?.end?.outcome).toBe('completed');
	});

	it('refuses the arm in words when the record cannot be written, and nothing is armed', async () => {
		const { mixed } = await screen({ record: 'unwritable' });
		short(mixed);
		expect(await mixed.apply()).toBe(true);
		expect(await mixed.arm()).toBe(false);
		expect(mixed.refusal?.reason).toBe('record_unwritable');
		expect(mixed.refusal?.title).toBe("The Run's Record Cannot Be Written");
		expect(mixed.refusal?.facts.some((f) => f.startsWith('Path: '))).toBe(true);
		expect(mixed.status?.state).toBe('configured');
	});

	it('refuses a configuration that still gives a capture, by name', async () => {
		const { client, mixed } = await screen();
		short(mixed);
		const config = { ...toMixedConfig(mixed.setup, { floor: null, tones: null }), capture: { rule: 'entire_test' } };
		await expect(client.request('mixed.configure', { config })).rejects.toMatchObject({ reason: 'bad_params' });
	});

	it('opens a finished run in the Sine workspace, where its data sheet is', async () => {
		expect(workspaceForRun('mixed')).toBe('sine');
		const shown: string[] = [];
		const open = new RunOpen({ bring: async () => ['Channel Table', 'Specification', 'Tones'], show: (w) => shown.push(w) }, 'mixed');
		expect(await open.open('/data/hf/20261113-180405_mixed_mixed-0001')).toBe(true);
		expect(shown).toEqual(['sine']);
		expect(open.added).toContain('Tones');
	});
});
