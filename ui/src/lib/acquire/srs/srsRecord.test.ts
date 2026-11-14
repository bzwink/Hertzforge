/**
 * An SRS shock run's record from the shell's side, as Random's: the run's folder named in the
 * state from arm on, the time histories off when the setup says so, the notice while the record
 * has a problem, the refusal when it cannot be written, the old capture refused, and the finished
 * run's way to its data opening in the Shock workspace. Through the mock control service and the
 * real client.
 */
import { describe, expect, it } from 'vitest';
import { RequestError, ServiceClient } from '$lib/protocol/client.svelte';
import { createControlServer, type ControlServerOptions } from '$lib/protocol/mock/control';
import { mockSocketFactory } from '$lib/protocol/mock/socket';
import { AcquireState } from '../store.svelte';
import { RunOpen, workspaceForRun, type RunOpenHost } from '../runOpen.svelte';
import { folderName, recordProblem } from '../words';
import { SRS_SECTIONS } from './srsSetup';
import { SrsStore } from './srsStore.svelte';

const until = async (cond: () => boolean, ms = 6000) => {
	const t0 = Date.now();
	while (!cond()) {
		if (Date.now() - t0 > ms) throw new Error('timed out waiting');
		await new Promise((r) => setTimeout(r, 5));
	}
};

async function screen(options: ControlServerOptions = {}) {
	const client = new ServiceClient({ service: 'control', url: 'ws://mock/', socketFactory: mockSocketFactory(createControlServer({ speed: 20, dataRoot: '/data/Hertzforge_Data', ...options }), 1) });
	const acq = new AcquireState(client, { session: null, local: null, checkDelayMs: 0 });
	client.connect();
	await client.whenOpen();
	await acq.sync();
	await acq.claim();
	const srs = new SrsStore(acq, { local: null, checkDelayMs: 0 });
	await srs.sync();
	srs.setup.limits.drive_vpk_ceiling = 0.5;
	srs.setup.limits.drive_vpk_abort = 0.8;
	srs.setup.identification.review = false;
	return { client, acq, srs };
}

describe("an SRS run's record", () => {
	it('is named in the state from arm on, with its words and the time histories as the setup gives them', async () => {
		const { srs } = await screen();
		srs.setup.record = { operator: 'J. Operator', article: 'Bracket', notes: '', document: 'Test Method 516', clause: '4.2', streams: true };
		expect(await srs.apply()).toBe(true);
		expect(srs.served?.config?.operator).toBe('J. Operator');
		expect(srs.served?.config?.specification).toEqual({ document: 'Test Method 516', clause: '4.2' });
		expect(srs.record).toBeNull();
		expect(await srs.arm()).toBe(true);
		await until(() => srs.record !== null);
		expect(srs.record?.streams).toBe('on');
		expect(folderName(srs.record!.folder)).toMatch(/^\d{8}-\d{6}_srs_srs-\d{4}$/);
		expect(srs.record!.folder.startsWith('/data/Hertzforge_Data/')).toBe(true);
		// the shared run head carries it, so the shared notices and the Run card read it
		expect(srs.run?.record).toEqual(srs.record);
	});

	it('says the time histories are off when the setup leaves them out', async () => {
		const { srs } = await screen();
		srs.setup.record.streams = false;
		expect(await srs.apply()).toBe(true);
		expect(await srs.arm()).toBe(true);
		await until(() => srs.record !== null);
		expect(srs.record?.streams).toBe('off');
	});

	it('says while the run goes on that recording has a problem, and in the past once it has ended', async () => {
		const { srs } = await screen({ record: 'problem' });
		expect(await srs.apply()).toBe(true);
		expect(await srs.start()).toBe(true);
		await until(() => !!srs.run?.record?.problem, 8000);
		const going = recordProblem(srs.run?.record, false);
		expect(going?.title).toBe('Recording Has a Problem');
		expect(going?.text).toMatch(/^The run goes on\. Recording of the inputs stream stopped/);
		await until(() => srs.status?.state === 'finished', 15000);
		expect(recordProblem(srs.run?.record, true)?.title).toBe('Recording Had a Problem');
	});

	it('is refused at arm, in words, when its folder cannot be written, and nothing arms', async () => {
		const { srs } = await screen({ record: 'unwritable' });
		expect(await srs.apply()).toBe(true);
		expect(await srs.arm()).toBe(false);
		expect(srs.refusal?.reason).toBe('record_unwritable');
		expect(srs.refusal?.title).toBe("The Run's Record Cannot Be Written");
		expect(srs.refusal?.message).toMatch(/could not be written, so the run did not arm/);
		expect(srs.status?.state).toBe('configured');
		expect(srs.record).toBeNull();
	});

	it('has no capture: a configuration that asks for one is refused at the field, and the setup edits the Run Record', async () => {
		const { client, srs } = await screen();
		const config = { ...srs.wire, capture: { auto_start: true, run_name: 'old' } };
		const err = await client.request('srs.configure', { config }).catch((e) => e);
		expect(err).toBeInstanceOf(RequestError);
		expect((err as RequestError).reason).toBe('bad_params');
		expect((err as RequestError).detail).toEqual({ param: 'config.capture' });
		expect('capture' in srs.wire).toBe(false);
		expect(SRS_SECTIONS.at(-1)).toEqual({ id: 'record', label: 'Run Record' });
		expect(srs.recordChannels).toBe(srs.rows.filter((r) => r.enabled).length);
		expect(srs.recordRateHz).toBe(srs.setup.acquisition.fs_hz);
	});

	it('opens a finished run in the Shock workspace, through the same opener as Random', async () => {
		const shown: [string, string[]][] = [];
		const host: RunOpenHost = { bring: async () => ['inputs pulse', 'pulse_srs', 'srs_reference', 'Run Record'], show: (w, names) => shown.push([w, names]) };
		const opener = new RunOpen(host, 'srs');
		expect(await opener.open('/data/run')).toBe(true);
		expect(shown).toEqual([['shock', ['inputs pulse', 'pulse_srs', 'srs_reference', 'Run Record']]]);
		expect(workspaceForRun('srs')).toBe('shock');
		expect(workspaceForRun('classical_shock')).toBe('shock');
		expect(workspaceForRun('recorder')).toBe('signals');
	});
});
