/**
 * The SRS shock setup's pure rules: every field of a configuration survives the draft and back,
 * and a reload; the shell's defaults; what the editor names before anything is sent; where the
 * service's findings land; what the synthesis preview is asked for; the per-band lists fitted to
 * the service's band grid.
 */
import { describe, expect, it } from 'vitest';
import type { LawEntry } from '$lib/protocol/mimo';
import type { ChannelRow } from '$lib/protocol/types';
import type { SrsConfig } from '$lib/protocol/srs';
import { loadSrsSetup, saveSrsSetup } from './srsStore.svelte';
import { defaultSrsSetup, fitToBands, fromSrsConfig, localErrors, mergeSrsSetup, newLimitChannel, previewParams, srsByKey, srsItemKey, srsSectionOf, toSrsConfig } from './srsSetup';

const LAW: LawEntry = {
	id: 'srs_ratio',
	world: 'shock_srs',
	builtin: true,
	name: 'SRS ratio',
	version: '0.1.0',
	author: 'Hertzforge',
	summary: '',
	sha256: null,
	check: { status: 'built_in', date: null, tool_version: null, cases: [], hardware: true, message: '' },
	params: [{ key: 'max_db_per_pulse', label: 'Largest change per pulse', help: '', kind: 'number', unit: 'dB', min: 0.5, max: 5, step: 0.5, default: 3 }],
	diagnostics: [],
	shapes: null,
	needs: null
};

/** Every field of the configuration, none at its default. */
const FULL: SrsConfig = {
	title: 'SRS 150 g, 80 to 4000 Hz',
	target: { kind: 'breakpoints', points: [[80, 30], [700, 150], [4000, 150]] },
	bands: { f_lo_hz: 80, f_hi_hz: 4000, bands_per_octave: 6, q: 12 },
	synthesis: {
		engine: 'seds',
		components: {
			engine: 'seds',
			rows: [
				{ freq_hz: 100, amplitude_g: 12, zeta: 0.04, delay_s: 0.001, polarity: 'negative' },
				{ freq_hz: 400, amplitude_g: 30, zeta: 0.03, delay_s: 0, polarity: 'positive' }
			]
		},
		max_iters: 80,
		tol_db: 1,
		max_gain_db_per_iter: 2
	},
	envelope: {
		alarm_db: [2, 2.5],
		abort_db: [8, 5],
		per_band: [
			{ alarm_db: [2, 2], abort_db: [6, 4] },
			{ alarm_db: [3, 3], abort_db: [9, 6] }
		]
	},
	acquisition: { fs_hz: 102400, ring_out_s: 0.08, pvss: false },
	schedule: { mode: 'manual', levels_db: [-9, -3, 0], pulses: [1, 2, 4], start_db: -3, advance: 'count', max_pulses_per_level: 7 },
	spacing: { cooling_s: 3, settling_s: 1.5, compute_s: 0.75 },
	identification: { excitation: 'standard_pulse', level_db: -26, max_vpk: 0.04, nfft: 2048, h_floor_frac: 0.1, min_mean_coherence: 0.85, review: false },
	control: { law: { id: 'srs_ratio', params: { max_db_per_pulse: 2 } }, law_budget: 0.3, law_misses: 4, max_pulses: 60, max_gain_rise_db: 5 },
	limits: {
		drive_vpk_ceiling: 0.6,
		drive_vpk_abort: 0.9,
		peak_accel_g: 300,
		abort_factor: 1.5,
		kinematic: { accel_g: 250, velocity_ips: 40, disp_pp_in: 0.5 },
		monitors: [{ channel: 3, unit: 'A', peak_ceiling: 40, kind: 'predictive' }]
	},
	limit_channels: {
		headroom_db: 2,
		channels: [
			{ channel: 4, bound: { kind: 'flat', accel_g: 50 }, frf: { freqs_hz: [100, 500, 2000], mag: [1, 4, 1.2] } },
			{ channel: 5, bound: { kind: 'per_band', bounds_g: [60, null] }, frf: null }
		]
	},
	feasibility: {
		shaker: { name: 'bench', rated_force_lbf: 5000 },
		moving_mass_lb: { armature: 45, slip_table: 0, drive_adapter: 3, fixturing: 10, test_article: 12 },
		residual_velocity_ips: 0.2,
		residual_disp_in: 0.002
	},
	operator: 'A. Tester',
	article: 'Bracket, unit 3',
	notes: 'Second axis.',
	specification: { document: 'Test Method 516', clause: '4.2' },
	record: { streams: false },
	metadata: [{ key: 'run_number', label: 'Run Number', value: '7' }]
};

const ROWS: ChannelRow[] = [
	{ index: 1, enabled: true, role: 'control', units: 'g' },
	{ index: 2, enabled: true, role: 'drive', units: 'V' },
	{ index: 3, enabled: true, role: 'monitor', units: 'A' },
	{ index: 4, enabled: true, role: 'monitor', units: 'g' },
	{ index: 9, enabled: false, role: 'monitor', units: 'g' }
] as unknown as ChannelRow[];

describe('the SRS setup and the protocol', () => {
	it('carries every field of a configuration through the draft and back', () => {
		expect(toSrsConfig(fromSrsConfig(FULL), LAW)).toEqual(FULL);
	});

	it('carries a one-level target, one component per band and no limit channels', () => {
		const c: SrsConfig = {
			...FULL,
			target: { kind: 'flat', accel_g: 80 },
			synthesis: { ...FULL.synthesis, engine: 'wavsyn', components: null },
			envelope: { ...FULL.envelope, per_band: null },
			limit_channels: null,
			feasibility: null
		};
		expect(toSrsConfig(fromSrsConfig(c), LAW)).toEqual(c);
	});

	it('keeps the other engine and target kind in the draft while the wire carries the chosen one', () => {
		const d = fromSrsConfig(FULL);
		d.synthesis.engine = 'wavsyn';
		d.synthesis.wavelets = [{ freq_hz: 200, amplitude_g: 5, half_cycles: 5, delay_s: 0 }];
		expect(toSrsConfig(d, LAW).synthesis?.components).toEqual({ engine: 'wavsyn', rows: [{ freq_hz: 200, amplitude_g: 5, half_cycles: 5, delay_s: 0 }] });
		d.synthesis.engine = 'seds';
		expect(toSrsConfig(d, LAW).synthesis?.components?.rows).toHaveLength(2);
		d.target.kind = 'flat';
		expect(toSrsConfig(d, LAW).target).toEqual({ kind: 'flat', accel_g: 100 });
		d.target.kind = 'breakpoints';
		expect(toSrsConfig(d, LAW).target).toEqual(FULL.target);
	});

	it('sends only the parameters the law declares', () => {
		const d = fromSrsConfig(FULL);
		d.law.params = { max_db_per_pulse: 4, stale: 1 };
		expect(toSrsConfig(d, LAW).control?.law).toEqual({ id: 'srs_ratio', params: { max_db_per_pulse: 4 } });
		expect(toSrsConfig(d, null).control?.law).toEqual({ id: 'srs_ratio', params: {} });
	});

	it('keeps a draft through a reload, emptied fields included', () => {
		const d = fromSrsConfig(FULL);
		d.limits.drive_vpk_ceiling = NaN;
		const back = loadSrsSetup(saveSrsSetup(d));
		expect(Number.isNaN(back.limits.drive_vpk_ceiling)).toBe(true);
		expect(toSrsConfig({ ...back, limits: { ...back.limits, drive_vpk_ceiling: 0.6 } }, LAW)).toEqual(FULL);
		expect(loadSrsSetup('not json')).toEqual(defaultSrsSetup());
		expect(mergeSrsSetup({ title: 'Only a title' } as never).bands).toEqual(defaultSrsSetup().bands);
	});

	it("starts from the shell's defaults: a ladder that advances on passing pulses, no drive limits", () => {
		const d = defaultSrsSetup();
		expect([d.schedule.advance, d.schedule.max_pulses_per_level]).toEqual(['on_pass', 5]);
		expect(d.schedule.levels.map((l) => [l.db, l.pulses])).toEqual([
			[-12, 2],
			[-6, 2],
			[0, 3]
		]);
		expect([d.limits.drive_vpk_ceiling, d.limits.drive_vpk_abort].every(Number.isNaN)).toBe(true);
		expect(d.identification.max_vpk).toBeNull();
		expect(d.law.id).toBe('srs_ratio');
		expect(toSrsConfig(d, LAW).limit_channels).toBeNull();
	});
});

describe('what the editor names before anything is sent', () => {
	it('asks for the drive ceiling and abort, with no default', () => {
		const e = localErrors(defaultSrsSetup(), ROWS);
		expect(Object.keys(e).sort()).toEqual(['limits.drive_vpk_abort', 'limits.drive_vpk_ceiling']);
		const d = fromSrsConfig(FULL);
		d.limits.drive_vpk_abort = 0.5;
		expect(localErrors(d, ROWS)['limits.drive_vpk_abort']).toEqual(['The drive abort is above the drive ceiling.']);
	});

	it('names a target with too few points, a table with no rows and an empty field', () => {
		const d = fromSrsConfig(FULL);
		d.target.points = [{ freq_hz: 100, accel_g: NaN }];
		d.synthesis.sines = [];
		d.envelope.alarm_db[1] = NaN;
		d.limits.drive_vpk_ceiling = 0.6;
		const e = localErrors(d, ROWS);
		expect(e['target.points']).toEqual(['Give at least two points of the target.']);
		expect(e['target.points.0.accel_g']).toEqual(['Enter a number.']);
		expect(e['synthesis.components']?.[0]).toMatch(/at least one component/);
		expect(e['envelope.alarm_db']).toEqual(['Give both: under and over the target.']);
	});

	it('names a limit channel or monitor on a row the table does not enable, and an unfilled per-band bound', () => {
		const d = fromSrsConfig(FULL);
		d.limit_channels.channels.push({ ...newLimitChannel(ROWS[4]) });
		d.limit_channels.channels[1].bound.bounds_g = [];
		d.limits.monitors[0].channel = 2;
		const e = localErrors(d, ROWS);
		expect(e['limit_channels.channels.2.channel']).toEqual(['Channel 9 is not an enabled input of the channel table; enable it, or supply its gain.']);
		// with a supplied gain, any row but the control row
		d.limit_channels.channels[2].frf = { supplied: true, points: [{ freq_hz: 100, mag: 1 }, { freq_hz: 1000, mag: 2 }] };
		expect(localErrors(d, ROWS)['limit_channels.channels.2.channel']).toBeUndefined();
		d.limit_channels.channels[2].channel = 1;
		expect(localErrors(d, ROWS)['limit_channels.channels.2.channel']).toEqual(['Channel 1 is the control channel; a limit channel is another input.']);
		expect(e['limit_channels.channels.2.bound']).toEqual(['Enter a number.']);
		expect(e['limit_channels.channels.1.bound']?.[0]).toMatch(/band grid/);
		expect(e['limits.monitors.0.channel']).toEqual(['Channel 2 is not an enabled input of the channel table.']);
		d.limit_channels.enabled = false;
		expect(Object.keys(localErrors(d, ROWS)).filter((k) => k.startsWith('limit_channels'))).toEqual([]);
	});

	it('asks the preview only for a target and grid that hold numbers', () => {
		const d = defaultSrsSetup();
		expect(previewParams(d)).toEqual({
			target: { kind: 'breakpoints', points: [[100, 40], [1000, 100], [2500, 100]] },
			bands: d.bands,
			synthesis: toSrsConfig(d, LAW).synthesis,
			envelope: { alarm_db: [3, 3], abort_db: [9, 6], per_band: null },
			fs_hz: 51200
		});
		// a per-band envelope is drawn from the dry run: the preview carries the pair for every band
		d.envelope.per_band = true;
		d.envelope.bands = [{ alarm_db: [1, 1], abort_db: [2, 2] }];
		expect(previewParams(d)?.envelope?.per_band).toBeNull();
		d.bands.q = NaN;
		expect(previewParams(d)).toBeNull();
		d.bands.q = 10;
		d.synthesis.table = true;
		expect(previewParams(d)).toBeNull();
	});
});

describe("where the service's findings land", () => {
	it('on the field, the table, the limit channel or the law panel', () => {
		const at = (field: string, reason = 'out_of_range') => srsItemKey({ reason, field });
		expect(at('config.bands.f_hi_hz', 'band_above_grid')).toBe('bands.f_hi_hz');
		expect(at('config.target.points[2]', 'target_not_ascending')).toBe('target.points');
		expect(at('config.synthesis.components.rows[3]', 'component_invalid')).toBe('synthesis.components');
		expect(at('config.envelope.per_band', 'per_band_length_mismatch')).toBe('envelope.per_band');
		expect(at('config.schedule.levels_db[2]', 'levels_not_ascending')).toBe('schedule.levels.2.db');
		expect(at('config.limit_channels.channels[1].bound', 'per_band_length_mismatch')).toBe('limit_channels.channels.1.bound');
		expect(at('config.limit_channels.channels[0]', 'limit_no_frf')).toBe('limit_channels.channels.0.frf');
		expect(at('config.limit_channels.channels[0].channel', 'limit_unknown_channel')).toBe('limit_channels.channels.0.channel');
		expect(at('config.control.law.params.max_db_per_pulse', 'law_param_invalid')).toBe('law.params.max_db_per_pulse');
		expect(at('config.control.law.id', 'law_unknown')).toBe('law.id');
		expect(srsItemKey({ reason: 'no_control_channel' })).toBe('station.channels');
		expect(srsByKey([{ reason: 'out_of_range', field: 'config.target.points[1]', message: 'a' }, { reason: 'out_of_range', field: 'config.target.points[1]', message: 'a' }])).toEqual({ 'target.points': ['a'] });
	});

	it('in the section that edits it', () => {
		const sections = ['bands.q', 'target.points', 'acquisition.fs_hz', 'synthesis.components', 'envelope.per_band', 'spacing.cooling_s', 'identification.max_vpk', 'law.params.x', 'control.max_gain_rise_db', 'limits.kinematic.accel_g', 'feasibility.shaker', 'limit_channels.channels.0.frf', 'metadata', 'specification.document', 'record.streams', 'station.channels'].map(srsSectionOf);
		expect(sections).toEqual(['target', 'target', 'synthesis', 'synthesis', 'envelope', 'schedule', 'identification', 'control', 'control', 'limits', 'limits', 'limit_channels', 'record', 'record', 'record', 'station']);
	});

	it("takes the run's record as Random's: the words trimmed, a specification when either part is given, the switch, and no capture", () => {
		const d = fromSrsConfig(FULL);
		expect(d.record).toEqual({ operator: 'A. Tester', article: 'Bracket, unit 3', notes: 'Second axis.', document: 'Test Method 516', clause: '4.2', streams: false });
		d.record = { operator: '  J. Operator ', article: '', notes: ' ', document: '', clause: '', streams: true };
		const c = toSrsConfig(d, LAW);
		expect([c.operator, c.article, c.notes, c.specification, c.record]).toEqual(['J. Operator', '', '', null, { streams: true }]);
		expect('capture' in c).toBe(false);
		// a clause names its document
		d.record.clause = '4.2';
		expect(localErrors(d)['specification.document']).toEqual(['Name the document this clause is from.']);
	});

	it('reads a draft saved with the old capture as one with the run record, time histories on', () => {
		const old = JSON.stringify({ ...defaultSrsSetup(), record: undefined, capture: { auto_start: true, run_name: 'old' } });
		const d = loadSrsSetup(old);
		expect(d.record).toEqual(defaultSrsSetup().record);
		expect('capture' in d).toBe(false);
	});
});

describe('the per-band lists and the band grid', () => {
	it('keep what they hold and fill the rest, to the count of bands', () => {
		expect(fitToBands([1, 2, 3], 5, () => 0)).toEqual([1, 2, 3, 0, 0]);
		expect(fitToBands([1, 2, 3], 2, () => 0)).toEqual([1, 2]);
		const same = [1, 2];
		expect(fitToBands(same, 2, () => 0)).toBe(same);
	});
});
