/**
 * The MIMO Random setup's pure rules: roles and their conflicts, the transformation grids
 * (paste, CSV, shape), the pair table and its defaults, where the service's findings land, the
 * law's parameter form from its manifest, the setup on the wire, and the summary's missing list.
 */
import { describe, expect, it } from 'vitest';
import type { ChannelRow } from '$lib/protocol/types';
import type { LawEntry, LawParam, MimoValidation } from '$lib/protocol/mimo';
import {
	assignRole,
	breakpointParams,
	channelErrors,
	controlledLabels,
	defaultMimoSetup,
	ensureSettings,
	itemKey,
	lawParamDescriptor,
	lawParamError,
	lawValues,
	livePairs,
	matrixFromCsv,
	parseGrid,
	pasteInto,
	emptyMatrix,
	resize,
	roleOf,
	sectionOf,
	settingErrors,
	spaceErrors,
	testChannels,
	toWireSetup,
	ensureDofBands,
	type EffectiveSpace,
	type MimoDraft
} from './mimoSetup';
import { missingList, summarize, type SummaryInputs } from './summary';

function row(index: number, role: ChannelRow['role'], description = ''): ChannelRow {
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

/** Two controls (1, 2), two drives (3, 4), a monitor (5). */
function bench(): { rows: ChannelRow[]; d: MimoDraft } {
	const rows = [row(1, 'control', 'Front'), row(2, 'control', 'Rear'), row(3, 'drive', 'Shaker A'), row(4, 'drive', 'Shaker B'), row(5, 'monitor', 'Lid')];
	const d = defaultMimoSetup();
	ensureSettings(rows, d);
	return { rows, d };
}

const PHYSICAL: EffectiveSpace = { kind: 'physical', dofs: [], response: null, drive: null, from: 'operator' };

describe('roles', () => {
	it('reads the four roles from the table and the draft', () => {
		const { rows, d } = bench();
		d.limits['5'] = { ceiling: 0.1, abort_margin_db: 3 };
		expect(rows.map((r) => roleOf(r, d))).toEqual(['control', 'control', 'drive', 'drive', 'limit']);
		const t = testChannels(rows, d);
		expect(t.limits.map((r) => r.index)).toEqual([5]);
		expect(t.monitors).toEqual([]);
	});

	it('a limit is a monitor row with a limit; a drive becomes an output in volts', () => {
		const { rows, d } = bench();
		rows[4].physical = { device: 'Dev1', channel: 'ai4' };
		assignRole(rows[4], 'limit', d);
		expect(rows[4].role).toBe('monitor');
		expect(d.limits['5']).toBeDefined();
		assignRole(rows[4], 'drive', d);
		expect(rows[4].role).toBe('drive');
		expect(rows[4].units).toBe('V');
		expect(rows[4].physical).toBeNull();
		expect(d.limits['5']).toBeUndefined();
		expect(d.drives['5']).toBeDefined();
		assignRole(rows[4], 'control', d);
		expect(rows[4].units).toBe('g');
		expect(d.controls['5']).toBeDefined();
	});

	it('names every conflict a run cannot start with', () => {
		const { rows, d } = bench();
		expect(channelErrors(rows, d)).toEqual({});
		rows.push(row(6, 'reference', 'Old ref'));
		d.drives['3'].peak_v = 1;
		d.drives['4'].abort_vrms = 1;
		d.drives['4'].shaker_text = '{not json';
		d.limits['5'] = { ceiling: 0, abort_margin_db: 3 };
		const e = channelErrors(rows, d);
		expect(e['channel.6'][0]).toContain('reference row');
		expect(e['drive.3.peak_v']).toEqual(['At least the rms ceiling and at most 10 V.']);
		expect(e['drive.4.abort_vrms']).toEqual(['Above the ceiling, or off.']);
		expect(e['drive.4.shaker'][0]).toContain('JSON');
		expect(e['limit.5.ceiling']).toEqual(['Above 0.']);
	});

	it('needs a control and a drive, and at most 72 controls and 24 drives', () => {
		const d = defaultMimoSetup();
		expect(channelErrors([], d).channels).toHaveLength(2);
		const many = [...Array.from({ length: 73 }, (_, i) => row(i + 1, 'control')), ...Array.from({ length: 25 }, (_, i) => row(100 + i, 'drive'))];
		ensureSettings(many, d);
		const e = channelErrors(many, d).channels;
		expect(e.some((m) => m.includes('at most 72'))).toBe(true);
		expect(e.some((m) => m.includes('at most 24'))).toBe(true);
	});
});

describe('transformation grids', () => {
	it('reads a spreadsheet block, tabs or commas, and names cells that are not numbers', () => {
		const tsv = parseGrid('1\t0\t0.5\r\n0\t1\tx\n\n');
		expect(tsv.cells).toEqual([
			[1, 0, 0.5],
			[0, 1, NaN]
		]);
		expect(tsv.bad).toEqual([{ row: 1, col: 2, text: 'x' }]);
		expect(parseGrid('1,2;3').cells).toEqual([[1, 2, 3]]);
	});

	it('pastes from the cell it lands on and counts what falls outside', () => {
		const m = emptyMatrix(2, 3);
		const { matrix, outside } = pasteInto(m, [
			[1, 2],
			[3, 4],
			[5, 6]
		], 0, 2);
		expect(matrix.values).toEqual([0, 0, 1, 0, 0, 3]);
		expect(outside).toBe(4);
		expect(m.values).toEqual([0, 0, 0, 0, 0, 0]);
	});

	it('reads a CSV with labels in the first row and column', () => {
		const r = matrixFromCsv('dof,Front,Rear\nX,0.5,0.5\nRZ,1,-1\n');
		expect(r.error).toBeNull();
		expect(r.values).toEqual([
			[0.5, 0.5],
			[1, -1]
		]);
		expect(r.rowLabels).toEqual(['X', 'RZ']);
		expect(matrixFromCsv('1,2\n3\n').error).toContain('same number');
	});

	it('keeps the values that still fit when the channels change', () => {
		const m = { rows: 2, cols: 2, values: [1, 2, 3, 4], source: 'Pasted' };
		expect(resize(m, 2, 3).values).toEqual([1, 2, 0, 3, 4, 0]);
		expect(resize(m, 1, 1).values).toEqual([1]);
	});

	it('virtual control needs names and numbers, and no more degrees of freedom than controls', () => {
		const { rows, d } = bench();
		d.space.kind = 'virtual';
		d.space.dofs = ['X', 'X', 'Y'];
		d.space.response = { ...emptyMatrix(3, 2), values: [1, 0, 0, 1, NaN, 0] };
		const space: EffectiveSpace = { kind: 'virtual', dofs: d.space.dofs, response: d.space.response, drive: null, from: 'operator' };
		const e = spaceErrors(rows, d, space);
		expect(e['space.dofs'].some((m) => m.includes('its own name'))).toBe(true);
		expect(e['space.dofs'].some((m) => m.includes('3 degrees of freedom and 2 control channels'))).toBe(true);
		expect(e['space.response_transform']).toEqual(['Every cell holds a number.']);
		expect(controlledLabels(rows, d, space)).toEqual(['X', 'X', 'Y']);
		expect(controlledLabels(rows, d, PHYSICAL)).toEqual(['Front', 'Rear']);
	});
});

describe('the pair table', () => {
	it('leaves out a pair named twice, a pair on itself, and one past the quantities', () => {
		const c = { kind: 'constant' as const, value: 0.5 };
		const pairs = [
			{ row: 0, col: 1, coherence: c, phase: c },
			{ row: 1, col: 0, coherence: c, phase: c },
			{ row: 2, col: 2, coherence: c, phase: c },
			{ row: 0, col: 3, coherence: c, phase: c }
		];
		expect(livePairs(pairs, 3)).toEqual([pairs[0]]);
	});

	it('sends constants and tables, the default for pairs not given, and a preview pair that exists', () => {
		const { d } = bench();
		d.reference.pairs = [{ row: 0, col: 1, coherence: { kind: 'table', points: [{ freq_hz: 20, value: 0.2 }, { freq_hz: 200, value: 0.8 }] }, phase: { kind: 'constant', value: 90 } }];
		d.reference.unspecified = 'fully_coherent';
		d.reference.previewPair = [4, 5];
		const p = breakpointParams(d, ['Front', 'Rear']);
		expect(p.pairs).toEqual([{ row: 0, col: 1, coherence: [[20, 0.2], [200, 0.8]], phase_deg: 90 }]);
		expect(p.unspecified).toBe('fully_coherent');
		expect(p.preview_pair).toEqual([0, 1]);
		expect(p.tables.map((t) => t.label)).toEqual(['Front', 'Rear']);
		expect(p.tables[0].rows[0]).toEqual({ freq_hz: 20, psd_g2_hz: 0.001 });
		expect(p.tables[0].rows[1]).toEqual({ freq_hz: 80, slope_db_oct: 3 });
		expect(p.grid).toEqual({ fs_hz: 8192, n_pts: 4096 });
	});
});

describe('where findings land', () => {
	it('puts a drive finding on the drive channel and a channel finding on its row', () => {
		const { rows, d } = bench();
		const setup = toWireSetup(d, { rows, space: PHYSICAL, reference: { kind: 'file', path: '/r', sha256: 'a'.repeat(64) }, law: null, backend: 'sim' });
		expect(itemKey({ reason: 'out_of_range', field: 'setup.channels.drives[1].peak_v', message: 'x' }, setup)).toBe('drive.4.peak_v');
		expect(itemKey({ reason: 'out_of_range', field: 'setup.channels.controls[0].alarm_db', message: 'x' }, setup)).toBe('control.1.alarm_db');
		expect(itemKey({ reason: 'out_of_range', field: 'setup.schedule.levels[2].dwell_s', message: 'x' }, setup)).toBe('schedule.levels.2.dwell_s');
		expect(itemKey({ reason: 'drive_unknown_channel', channel: 9, message: 'x' }, setup)).toBe('channel.9');
		expect(itemKey({ reason: 'reference_not_psd', message: 'x' }, setup)).toBe('setup');
	});
});

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
	params: [
		{ key: 'regularization', label: 'Regularization', help: '', kind: 'choice', options: ['truncated', 'tikhonov'], default: 'truncated' },
		{ key: 'convergence', label: 'Convergence', help: '', kind: 'number', unit: '', min: 0, max: 1, step: 0.05, default: 0.5 },
		{ key: 'iterations', label: 'Iterations', help: '', kind: 'integer', unit: '', min: 1, max: 10, step: 1, default: 2 },
		{ key: 'notch', label: 'Notch', help: '', kind: 'toggle', default: false },
		{ key: 'weight', label: 'Weight', help: '', kind: 'per_channel', unit: '', min: 0, max: 10, step: 0.1, default: 1 },
		{ key: 'shape', label: 'Shape', help: '', kind: 'curve', unit: 'dB', min: -20, max: 20, default: [[20, 0]] }
	] as LawParam[],
	diagnostics: [],
	shapes: ['square', 'more_controls_than_drives'],
	needs: []
};

describe('the law form from its manifest', () => {
	it('draws numbers, whole numbers, toggles and choices; leaves per-control and curves to the screen', () => {
		const forms = LAW.params.map(lawParamDescriptor);
		expect(forms.map((f) => f?.type ?? null)).toEqual(['enum', 'number', 'integer', 'boolean', null, null]);
		expect(forms[0]?.options?.map((o) => o.label)).toEqual(['Truncated', 'Tikhonov']);
		expect(forms[1]).toMatchObject({ min: 0, max: 1, step: 0.05, default: 0.5 });
	});

	it('starts from the defaults, keeps valid values, one per control', () => {
		const v = lawValues(LAW, { convergence: 0.8, regularization: 'none', weight: [2] }, 3);
		expect(v.convergence).toBe(0.8);
		expect(v.regularization).toBe('truncated');
		expect(v.weight).toEqual([2, 1, 1]);
		expect(v.shape).toEqual([[20, 0]]);
		expect(v.notch).toBe(false);
	});

	it('says what is out of the manifest range', () => {
		expect(lawParamError(LAW.params[1], 1.5)).toBe('From 0 to 1.');
		expect(lawParamError(LAW.params[2], 2.5)).toBe('Enter a whole number.');
		expect(lawParamError(LAW.params[4], [1, 11])).toBe('From 0 to 10.');
		expect(lawParamError(LAW.params[5], [[100, 0], [50, 0]])).toContain('rise');
		expect(lawParamError(LAW.params[1], 0.5)).toBeNull();
	});
});

describe('the setup on the wire', () => {
	it('carries each role\'s settings, the space, and only the parameters the law declares', () => {
		const { rows, d } = bench();
		d.limits['5'] = { ceiling: 0.2, abort_margin_db: 4 };
		d.drives['3'].shaker_text = '{"name":"S1"}';
		d.law.params = { convergence: 0.6, stray: 1 };
		const s = toWireSetup(d, { rows, space: PHYSICAL, reference: { kind: 'file', path: '/r', sha256: 'b'.repeat(64) }, law: LAW, backend: 'sim' })!;
		expect(s.channels.controls.map((c) => c.channel)).toEqual([1, 2]);
		expect(s.channels.drives[0]).toEqual({ channel: 3, ceiling_vrms: 2, abort_vrms: null, peak_v: 6, shaker: { name: 'S1' }, moving_mass_lb: 0 });
		expect(s.channels.limits).toEqual([{ channel: 5, ceiling: 0.2, abort_margin_db: 4 }]);
		expect(s.space).toEqual({ kind: 'physical' });
		expect(s.law).toEqual({ id: 'pinv_closed', params: { convergence: 0.6 } });
		expect(s.schedule.levels.at(-1)).toEqual({ db: 0 });
		expect(s.control?.cap_up_db).toBe(3.5);
		expect(toWireSetup(d, { rows, space: PHYSICAL, reference: null, law: LAW, backend: null })).toBeNull();
	});

	it('under virtual control sends each degree of freedom its own bands, in their order', () => {
		const { rows, d } = bench();
		const virt: EffectiveSpace = { kind: 'virtual', dofs: ['X', 'Y'], response: { rows: 2, cols: 2, values: [1, 0, 0, 1], source: null }, drive: null, from: 'operator' };
		ensureDofBands(d, virt.dofs);
		d.space.dofBands['Y'] = { alarm_db: [1, 2], abort_db: [3, 4] };
		const s = toWireSetup(d, { rows, space: virt, reference: { kind: 'file', path: '/r', sha256: 'c'.repeat(64) }, law: LAW, backend: 'sim' })!;
		if (s.space?.kind !== 'virtual') throw new Error('virtual expected');
		expect(s.space.dof_bands).toEqual([
			{ alarm_db: [3, 3], abort_db: [6, 6] },
			{ alarm_db: [1, 2], abort_db: [3, 4] }
		]);
	});
});

describe('the setup summary', () => {
	function inputs(over: Partial<SummaryInputs> = {}): SummaryInputs {
		const { rows, d } = bench();
		const zero = { errors: 0, warnings: 0 };
		return {
			draft: d,
			channels: testChannels(rows, d),
			space: PHYSICAL,
			labels: ['Front', 'Rear'],
			ref: null,
			refBusy: false,
			law: LAW,
			lawsLoaded: true,
			check: null,
			counts: { channels: zero, reference: zero, control: zero, schedule: zero, record: zero, backend: zero, predict: zero },
			station: { kind: 'sim', dirty: false },
			tableErrors: 0,
			setupStatus: 'none',
			...over
		};
	}

	const built = { ok: true, reference: { kind: 'file', path: '/r', sha256: 'c'.repeat(64) }, labels: ['Front', 'Rear'], unit: 'g', df_hz: 2, band_hz: [20, 1000], lines: 491, errors: [], warnings: [], psd: { violations: 0, min_eigenvalue_ratio: 0, worst_hz: null, lines_hz: [], repair: null, repair_accepted: false }, transforms: { response: null, drive: null }, provenance: null, views: [] } as const;

	it('lists nothing once every section is complete', () => {
		const s = summarize(inputs({ ref: structuredClone(built) as never }));
		expect(missingList(s)).toEqual([]);
		expect(s.find((x) => x.section === 'reference')?.state).toBe('Breakpoint tables: 2 by 2 on 491 lines');
	});

	it('lists what is missing, section by section', () => {
		const d = defaultMimoSetup();
		d.reference.method = 'import';
		const bad: MimoValidation['law'] = { id: 'x', name: 'x', version: '1', sha256: null, admitted: false };
		const s = summarize(
			inputs({
				draft: d,
				channels: { drives: [], controls: [], limits: [], monitors: [] },
				law: null,
				check: { law: bad } as MimoValidation,
				station: { kind: 'ni', dirty: true },
				tableErrors: 2
			})
		);
		const m = missingList(s).map((x) => `${x.section}: ${x.text}`);
		expect(m).toEqual([
			'channels: Give a channel the Control role.',
			'channels: Give an output the Drive role.',
			'reference: Give the path of the reference to import.',
			'control: Choose a control law.',
			'control: The chosen law does not admit this test; choose another, or change its parameters.',
			'backend: Use the chosen station, or put the choice back.',
			'backend: Fix the 2 errors in the channel wiring.'
		]);
	});

	it('asks for the repair when the service refused lines it can repair', () => {
		const refused = { ...structuredClone(built), ok: false, reference: null, psd: { ...built.psd, violations: 4, repair: { lines: 4, clamped: 4, max_change: 0.3, pairs: [] }, repair_accepted: false } };
		const s = summarize(inputs({ ref: refused as never }));
		expect(missingList(s).map((x) => x.text)).toEqual(['Accept the repair, or change the pairs that cannot hold.']);
	});
});

describe("the run record's words", () => {
	it('go with the setup, trimmed, the specification only when one is named, the streams on by default', () => {
		const { rows, d } = bench();
		expect(d.record).toEqual({ operator: '', article: '', notes: '', document: '', clause: '', streams: true });
		const at = (draft: MimoDraft) => toWireSetup(draft, { rows, space: PHYSICAL, reference: { kind: 'file', path: '/r', sha256: 'e'.repeat(64) }, law: LAW, backend: 'sim' })!;
		const none = at(d);
		expect([none.operator, none.article, none.notes, none.specification, none.record]).toEqual(['', '', '', null, { streams: true }]);
		d.record = { operator: ' A. Operator ', article: 'Bracket, serial 7', notes: ' first article ', document: 'Test procedure 12', clause: '4.2', streams: false };
		const s = at(d);
		expect([s.operator, s.article, s.notes, s.specification, s.record]).toEqual(['A. Operator', 'Bracket, serial 7', 'first article', { document: 'Test procedure 12', clause: '4.2' }, { streams: false }]);
	});

	it('are a section of their own, where a clause without its document is named', () => {
		for (const key of ['operator', 'article', 'notes', 'specification.document', 'record.streams']) expect(sectionOf(key)).toBe('record');
		expect(sectionOf('schedule.levels.0.db')).toBe('schedule');
		const d = defaultMimoSetup();
		d.record.clause = '4.2';
		expect(settingErrors(d)['specification.document']).toEqual(['Name the document this clause is from.']);
		d.record.document = 'Test procedure 12';
		expect(settingErrors(d)['specification.document']).toBeUndefined();
	});
});

