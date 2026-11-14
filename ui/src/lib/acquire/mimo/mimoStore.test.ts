/**
 * The MIMO Random store over the real client and codec: the station and table from the mock
 * control service, and the `mimo_random.*` and `laws.*` answers given here. What it shows comes
 * from those answers: the reference the service built (refused until its repair is accepted),
 * the dry run's findings on the field and channel they name, and the law's parameters.
 */
import { describe, expect, it } from 'vitest';
import { ServiceClient } from '$lib/protocol/client.svelte';
import { createControlServer } from '$lib/protocol/mock/control';
import { mockSocketFactory } from '$lib/protocol/mock/socket';
import type { ChannelRow } from '$lib/protocol/types';
import type { LawEntry, MimoReferenceResult, MimoValidation } from '$lib/protocol/mimo';
import { RequestError } from '$lib/protocol/client.svelte';
import type { MimoStatus } from '$lib/protocol/mimo';
import { RunOpen, workspaceForRun } from '../runOpen.svelte';
import { AcquireState } from '../store.svelte';
import { recordProblem } from '../words';
import { ensureSettings } from './mimoSetup';
import { MimoState } from './mimoStore.svelte';

function row(index: number, role: ChannelRow['role'], description: string): ChannelRow {
	const drive = role === 'drive';
	return {
		index,
		enabled: true,
		description,
		role,
		physical: null,
		interface: 'single_ended',
		iepe_bias_v: null,
		units: drive ? 'V' : 'g',
		sensitivity_mv_per_eu: drive ? 1000 : 100,
		range_max: drive ? 10 : 100,
		polarity: 'positive',
		control_group: null,
		abort: { accel_g: null, velocity_ips: null, displacement_mil: null }
	};
}

const LAW: LawEntry = {
	id: 'pinv_closed',
	world: 'mimo',
	builtin: true,
	name: 'pseudo-inverse, closed loop',
	version: '1.0.0',
	author: 'Hertzforge',
	summary: 'The default law.',
	sha256: null,
	check: { status: 'built_in', date: null, tool_version: null, cases: [], hardware: true, message: 'Built in.' },
	params: [{ key: 'convergence', label: 'Convergence', help: '', kind: 'number', unit: '', min: 0, max: 1, step: 0.05, default: 0.5 }],
	diagnostics: [],
	shapes: ['square'],
	needs: []
};

function refResult(ok: boolean): MimoReferenceResult {
	return {
		ok,
		reference: ok ? { kind: 'file', path: '/data/references/abc', sha256: 'd'.repeat(64) } : null,
		labels: ['Front', 'Rear'],
		unit: 'g',
		df_hz: 2,
		band_hz: [20, 1000],
		lines: 491,
		errors: ok ? [] : [{ reason: 'reference_not_psd', field: 'reference', message: 'On 4 lines the coherences and phases cannot all hold at once.' }],
		warnings: ok ? [{ reason: 'reference_repaired', field: 'reference', message: '4 lines were repaired.' }] : [],
		psd: { violations: 4, min_eigenvalue_ratio: -0.4, worst_hz: 24, lines_hz: [24], repair: { lines: 4, clamped: 4, max_change: 0.3, pairs: [] }, repair_accepted: ok },
		transforms: { response: null, drive: null },
		provenance: null,
		views: []
	};
}

const VALIDATION: MimoValidation = {
	ok: false,
	errors: [{ reason: 'out_of_range', field: 'setup.channels.drives[1].peak_v', message: "A drive's peak is at least its rms ceiling and at most 10 V." }],
	warnings: [],
	shape: { controls: 2, drives: 2, space: 'physical', dofs: null, shape: 'square' },
	transforms: { response: null, drive: null },
	reference: null,
	feasibility: [],
	law: { id: 'pinv_closed', name: 'pseudo-inverse, closed loop', version: '1.0.0', sha256: null, admitted: true }
};

const until = async (cond: () => boolean, ms = 4000) => {
	const t0 = Date.now();
	while (!cond()) {
		if (Date.now() - t0 > ms) throw new Error('timed out waiting');
		await new Promise((r) => setTimeout(r, 5));
	}
};

async function screen() {
	const client = new ServiceClient({ service: 'control', url: 'ws://mock/', socketFactory: mockSocketFactory(createControlServer(), 1) });
	const acq = new AcquireState(client, { session: null, local: null, checkDelayMs: 0 });
	client.connect();
	await client.whenOpen();
	await acq.sync();
	const sent: { method: string; params: Record<string, unknown> }[] = [];
	let repairAccepted = false;
	let startRefused = false;
	const real = client.request.bind(client);
	// The MIMO methods answer from here; everything else goes to the mock service.
	client.request = (async (method: string, params: Record<string, unknown> = {}) => {
		sent.push({ method, params });
		if (method === 'laws.list') return { laws: [LAW], problems: [] };
		if (method === 'mimo_random.status') return { run_id: null, state: 'unconfigured', phase: 'idle' };
		if (method === 'mimo_random.report') return { report: null };
		if (method === 'mimo_random.reference.build_breakpoints') return refResult(repairAccepted || params.repair === true);
		if (method === 'mimo_random.setup') return { validation: VALIDATION };
		if (method === 'mimo_random.start' && startRefused)
			throw new RequestError('refused', "The run's record could not be written, so the run did not arm: /data/x (Read-only file system). Choose a data folder that can be written.", 'mimo_random.start', 'record_unwritable', { path: '/data/x', reason: 'Read-only file system' });
		return real(method, params);
	}) as typeof client.request;
	const mimo = new MimoState(acq, { local: null, checkDelayMs: 0 });
	acq.rows = [row(1, 'control', 'Front'), row(2, 'control', 'Rear'), row(3, 'drive', 'Shaker A'), row(4, 'drive', 'Shaker B')];
	// What the store's watch does as rows change.
	ensureSettings(acq.rows, mimo.setup);
	await mimo.sync();
	return { client, acq, mimo, sent, accept: () => (repairAccepted = true), refuseStart: () => (startRefused = true) };
}

describe('the MIMO Random store', () => {
	it('reads the law catalog and starts the law at its defaults', async () => {
		const { mimo } = await screen();
		expect(mimo.law?.id).toBe('pinv_closed');
		expect(mimo.setup.law.params.convergence).toBe(0.5);
		mimo.setup.law.params.convergence = 2;
		expect(mimo.fieldErrors['law.params.convergence']).toEqual(['From 0 to 1.']);
	});

	it('shows the service\'s refusal of the reference until the repair is accepted', async () => {
		const { mimo, sent } = await screen();
		for (const l of mimo.labels) mimo.setup.reference.tables[l] ??= [];
		mimo.setup.reference.tables.Front = [{ freq_hz: 20, kind: 'psd', psd_g2_hz: 0.01, slope_db_oct: NaN, alarm_db: null, abort_db: null }];
		mimo.setup.reference.tables.Rear = [{ freq_hz: 20, kind: 'psd', psd_g2_hz: 0.01, slope_db_oct: NaN, alarm_db: null, abort_db: null }];
		await mimo.buildReference();
		const build = sent.filter((s) => s.method === 'mimo_random.reference.build_breakpoints').at(-1)!;
		expect((build.params.tables as { label: string }[]).map((t) => t.label)).toEqual(['Front', 'Rear']);
		expect(build.params.repair).toBe(false);
		expect(mimo.reference).toBeNull();
		expect(mimo.wire).toBeNull();
		expect(mimo.fieldErrors.reference).toEqual(['On 4 lines the coherences and phases cannot all hold at once.']);
		expect(mimo.counts.reference.errors).toBe(1);
		expect(mimo.pending).toBe('The setup needs its reference first.');

		mimo.setup.reference.acceptRepair = true;
		await mimo.buildReference();
		expect(mimo.reference).toEqual({ kind: 'file', path: '/data/references/abc', sha256: 'd'.repeat(64) });
		expect(mimo.fieldErrors.reference).toBeUndefined();
		expect(mimo.fieldWarnings.reference).toEqual(['4 lines were repaired.']);
		expect(mimo.wire?.reference.kind).toBe('file');
	});

	it('puts the dry run\'s finding on the drive it names, by channel', async () => {
		const { mimo, sent } = await screen();
		mimo.setup.reference.acceptRepair = true;
		await mimo.buildReference();
		await mimo.checkSetup();
		const dry = sent.filter((s) => s.method === 'mimo_random.setup').at(-1)!;
		expect(dry.params.dry_run).toBe(true);
		expect(mimo.fieldErrors['drive.4.peak_v']).toEqual(["A drive's peak is at least its rms ceiling and at most 10 V."]);
		expect(mimo.counts.channels.errors).toBe(1);
		// An edit makes the dry run stale: its finding is no longer shown.
		mimo.setup.drives['4'].peak_v = 5;
		await until(() => mimo.fieldErrors['drive.4.peak_v'] === undefined);
	});

	it('offers Apply first, and Start only once the setup is held', async () => {
		const { mimo } = await screen();
		expect(mimo.actions.apply.visible).toBe(true);
		expect(mimo.actions.start.visible).toBe(false);
		expect(mimo.actions.arm.visible).toBe(false);
		expect(mimo.primary).toBe('apply');
		expect(mimo.actions.apply.enabled).toBe(false);
		expect(mimo.actions.apply.reason).toBe('Take control to apply the setup.');
	});

	it("sends the run record's words with the setup, and sizes an hour of its streams", async () => {
		const { mimo, sent } = await screen();
		mimo.setup.record = { operator: 'A. Operator', article: 'Bracket', notes: '', document: '', clause: '4.2', streams: true };
		expect(mimo.fieldErrors['specification.document']).toEqual(['Name the document this clause is from.']);
		expect(mimo.counts.record.errors).toBe(1);
		mimo.setup.record.document = 'Test procedure 12';
		expect(mimo.counts.record.errors).toBe(0);
		mimo.setup.reference.acceptRepair = true;
		await mimo.buildReference();
		await mimo.checkSetup();
		const dry = sent.filter((s) => s.method === 'mimo_random.setup').at(-1)!;
		const setup = dry.params.setup as Record<string, unknown>;
		expect([setup.operator, setup.article, setup.specification, setup.record]).toEqual(['A. Operator', 'Bracket', { document: 'Test procedure 12', clause: '4.2' }, { streams: true }]);
		// four enabled rows at the setup's rate
		expect([mimo.recordChannels, mimo.recordRateHz]).toEqual([4, 8192]);
	});

	it('says in words when the run\'s record cannot be written, and the run does not start', async () => {
		const { mimo, refuseStart } = await screen();
		refuseStart();
		expect(await mimo.start()).toBe(false);
		expect(mimo.refusal?.reason).toBe('record_unwritable');
		expect(mimo.refusal?.title).toBe("The Run's Record Cannot Be Written");
		expect(mimo.refusal?.facts).toEqual(['Path: /data/x', 'Reason: Read-only file system']);
	});

	it('shows the folder from arm on, a problem as a notice, and opens a finished run in the Random workspace', async () => {
		const { mimo } = await screen();
		const record = { folder: '/data/runs/20261113-140000_mimo_random_m-0003', streams: 'on' as const, problem: null };
		mimo.status = { run_id: 'm-0003', state: 'running', phase: 'engaged', record } as unknown as MimoStatus;
		expect(mimo.run?.record?.folder).toBe(record.folder);
		expect(recordProblem(mimo.run?.record, false)).toBeNull();
		mimo.status = { ...mimo.status, record: { ...record, problem: 'the inputs stream stopped: the disk is full' } } as MimoStatus;
		expect(recordProblem(mimo.run?.record, false)?.text).toContain('The inputs stream stopped');
		expect(workspaceForRun('mimo_random')).toBe('random');
		const shown: string[] = [];
		const opener = new RunOpen({ bring: async () => ['Specification', 'identification_frf'], show: (w) => shown.push(w) }, 'mimo_random');
		expect(await opener.open(record.folder)).toBe(true);
		expect(opener.at(record.folder)).toBe('done');
		expect(shown).toEqual(['random']);
	});
});
