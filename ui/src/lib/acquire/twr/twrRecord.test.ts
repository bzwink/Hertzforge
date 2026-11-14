/**
 * A TWR run's record from the shell's side, as Random's and classical shock's: the run's folder
 * named in the state from arm on, the time histories off when the setup says so, the notice while
 * the record has a problem, the refusal when it cannot be written, the old capture refused, and the
 * finished run's way to its data opening in Transient Analysis. Through the mock control service
 * and the real client.
 */
import { describe, expect, it } from 'vitest';
import { RequestError, ServiceClient } from '$lib/protocol/client.svelte';
import { createControlServer, type ControlServerOptions } from '$lib/protocol/mock/control';
import { mockSocketFactory } from '$lib/protocol/mock/socket';
import { AcquireState } from '../store.svelte';
import { RunOpen, workspaceForRun, type RunOpenHost } from '../runOpen.svelte';
import { folderName, recordProblem } from '../words';
import { TwrStore } from './twrStore.svelte';

const until = async (cond: () => boolean, ms = 8000) => {
	const t0 = Date.now();
	while (!cond()) {
		if (Date.now() - t0 > ms) throw new Error('timed out waiting');
		await new Promise((r) => setTimeout(r, 5));
	}
};

/** A short field-like record as pasted text: a time column and a value. */
function refText(n = 2000, fs = 12800): string {
	const lines = ['time_s, accel_g'];
	for (let i = 0; i < n; i++) {
		const t = i / fs;
		lines.push(`${t.toFixed(10)}, ${(0.2 * Math.exp(-3 * t) * Math.sin(2 * Math.PI * 250 * t)).toFixed(6)}`);
	}
	return lines.join('\n');
}

async function screen(options: ControlServerOptions = {}) {
	const client = new ServiceClient({ service: 'control', url: 'ws://mock/', socketFactory: mockSocketFactory(createControlServer({ speed: 20, dataRoot: '/data/Hertzforge_Data', ...options }), 1) });
	const acq = new AcquireState(client, { session: null, local: null, checkDelayMs: 0 });
	client.connect();
	await client.whenOpen();
	await acq.sync();
	await acq.claim();
	const twr = new TwrStore(acq, { local: null, checkDelayMs: 0 });
	await twr.sync();
	expect(await twr.importReference({ text: refText() })).toBe(true);
	twr.setup.limits.drive_vpk_ceiling = 1;
	twr.setup.identification.review = false;
	return { client, acq, twr };
}

describe("a TWR run's record", () => {
	it('is named in the state from arm on, with its words and the time histories as the setup gives them', async () => {
		const { twr } = await screen();
		twr.setup.record = { operator: 'J. Operator', article: 'Bracket', notes: '', document: 'Test procedure', clause: '4.2', streams: true };
		expect(await twr.apply()).toBe(true);
		expect(twr.served?.config?.operator).toBe('J. Operator');
		expect(twr.served?.config?.specification).toEqual({ document: 'Test procedure', clause: '4.2' });
		expect(twr.runRecord).toBeNull();
		expect(await twr.arm()).toBe(true);
		await until(() => twr.runRecord !== null);
		expect(twr.runRecord?.streams).toBe('on');
		expect(folderName(twr.runRecord!.folder)).toMatch(/^\d{8}-\d{6}_twr_twr-\d{4}$/);
		expect(twr.runRecord!.folder.startsWith('/data/Hertzforge_Data/')).toBe(true);
		// the shared run head carries it, so the shared notices and the Run card read it
		expect(twr.run?.record).toEqual(twr.runRecord);
	});

	it('takes the shared Run Record panel: the channels and the rate its streams would take', async () => {
		const { twr } = await screen();
		// the table's control row and its drive, both enabled
		expect(twr.recordChannels).toBe(twr.rows.filter((r) => r.enabled).length);
		// no rate asked: the reference's own
		expect(twr.setup.acquisition.fs_hz).toBeNull();
		expect(twr.recordRateHz).toBe(12800);
		twr.setup.acquisition.fs_hz = 25600;
		expect(twr.recordRateHz).toBe(25600);
		// a clause with no document is named at its field, in the Run Record section
		twr.setup.record.clause = '4.2';
		expect(twr.fieldErrors['specification.document']).toEqual(['Name the document this clause is from.']);
	});

	it('says the time histories are off when the setup leaves them out', async () => {
		const { twr } = await screen();
		twr.setup.record.streams = false;
		expect(await twr.apply()).toBe(true);
		expect(twr.served?.config?.record).toEqual({ streams: false });
		expect(await twr.arm()).toBe(true);
		await until(() => twr.runRecord !== null);
		expect(twr.runRecord?.streams).toBe('off');
	});

	it('says while the run goes on that recording has a problem, and in the past once it has ended', async () => {
		const { twr } = await screen({ record: 'problem' });
		expect(await twr.apply()).toBe(true);
		expect(await twr.start()).toBe(true);
		await until(() => !!twr.run?.record?.problem, 12000);
		const going = recordProblem(twr.run?.record, false);
		expect(going?.title).toBe('Recording Has a Problem');
		expect(going?.text).toMatch(/^The run goes on\. Recording of the inputs stream stopped/);
		await twr.stop();
		await until(() => twr.status?.state === 'finished', 12000);
		expect(recordProblem(twr.run?.record, true)?.title).toBe('Recording Had a Problem');
	});

	it('is refused at arm, in words, when its folder cannot be written, and nothing arms', async () => {
		const { twr } = await screen({ record: 'unwritable' });
		expect(await twr.apply()).toBe(true);
		expect(await twr.arm()).toBe(false);
		expect(twr.refusal?.reason).toBe('record_unwritable');
		expect(twr.refusal?.title).toBe("The Run's Record Cannot Be Written");
		expect(twr.refusal?.message).toMatch(/could not be written, so the run did not arm/);
		expect(twr.status?.state).toBe('configured');
		expect(twr.runRecord).toBeNull();
	});

	it('has no capture: a configuration that asks for one is refused at the field', async () => {
		const { client, twr } = await screen();
		const config = { ...twr.wire, capture: { auto_start: true, run_name: 'twr_run' } };
		const err = await client.request('twr.configure', { config }).catch((e) => e);
		expect(err).toBeInstanceOf(RequestError);
		expect((err as RequestError).reason).toBe('bad_params');
		expect((err as RequestError).detail).toEqual({ param: 'config.capture' });
		expect('capture' in twr.wire).toBe(false);
	});

	it('opens a finished run in Transient Analysis, through the same opener as Random', async () => {
		const shown: [string, string[]][] = [];
		const host: RunOpenHost = { bring: async () => ['inputs play', 'Specification', 'Run Record'], show: (w, names) => shown.push([w, names]) };
		const opener = new RunOpen(host, 'twr');
		expect(await opener.open('/data/run')).toBe(true);
		expect(shown).toEqual([['transient', ['inputs play', 'Specification', 'Run Record']]]);
		expect(workspaceForRun('twr')).toBe('transient');
		expect(workspaceForRun('classical_shock')).toBe('shock');
		expect(workspaceForRun('recorder')).toBe('signals');
	});
});
