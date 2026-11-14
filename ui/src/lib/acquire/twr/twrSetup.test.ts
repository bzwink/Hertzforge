/**
 * The TWR setup draft and its wire form: every field of a configuration through the draft and back,
 * the shell's own defaults, what the editor says before anything is sent, and where each of the
 * service's findings lands.
 */
import { describe, expect, it } from 'vitest';
import type { LawEntry } from '$lib/protocol/mimo';
import type { TwrConfig } from '$lib/protocol/twr';
import { saveTwrSetup, loadTwrSetup } from './twrStore.svelte';
import { defaultTwrSetup, fromTwrConfig, localErrors, mergeTwrSetup, previewParams, toTwrConfig, twrByKey, twrItemKey, twrSectionOf } from './twrSetup';

const LAW: LawEntry = {
	id: 'twr_correct',
	world: 'twr',
	builtin: true,
	name: 'frequency-domain correction',
	version: '0.1.0',
	author: '',
	summary: '',
	sha256: null,
	check: { status: 'built_in', date: null, tool_version: null, cases: [], hardware: true, message: '' },
	params: [
		{ key: 'beta', label: 'Correction share', help: '', kind: 'number', unit: '', min: 0.05, max: 1, step: 0.05, default: 0.5 },
		{ key: 'max_db_per_iter', label: 'Growth limit', help: '', kind: 'number', unit: 'dB', min: 1, max: 9, step: 0.5, default: 6 },
		{ key: 'initial_backoff_db', label: 'Backoff', help: '', kind: 'number', unit: 'dB', min: 0, max: 12, step: 1, default: 0 }
	],
	diagnostics: [],
	shapes: null,
	needs: null
};

/** Every field of the configuration, none at its default, as PROTOCOL.md's example lays them out. */
function everyField(): TwrConfig {
	return {
		title: 'Field Transient R1',
		reference: { sha256: '9f2c'.repeat(16), path: '/data/references/r1.csv' },
		prep: { auto_resample: false, detrend: 'linear', highpass_hz: 4, scale: 1.5, taper_frac: 0.05 },
		section: { start: 100, len: 51000 },
		pads: { pre: 256, post: 2048 },
		acquisition: { fs_hz: 25600 },
		identification: { excitation: 'log_chirp', drive_vrms: 0.03, duration_s: 6, nfft: 4096, band_hz: [10, 3000], min_mean_coherence: 0.7, review: false },
		inverse: { tikhonov_floor: 0.04, coherence_floor: 0.85 },
		correction: { band_hz: [15, 2500], ref_floor_db: 45, min_band_energy: 0.8 },
		control: {
			law: { id: 'twr_correct', params: { beta: 0.4, max_db_per_iter: 5, initial_backoff_db: 3 } },
			law_budget: 0.3,
			law_misses: 4,
			max_peak_rise_db: 9,
			max_bin_rise_db: 8,
			first_drive_ratio: 2.5,
			alignment_frames: 1.5
		},
		schedule: { mode: 'manual', levels_db: [-15, -10, -5, 0], gate_pct: 8, max_iterations: 6, on_gate_fail: 'hold' },
		run: { schedule: 'duration', duration_s: 120 },
		adaptation: { during_playback: true },
		limits: {
			drive_vpk_ceiling: 0.5,
			drive_clip_vpk: 0.8,
			drive_vrms_limit: 0.3,
			response_rms_limit: 2,
			envelope_alarm_frac: 0.2,
			envelope_abort_frac: 0.4,
			envelope_debounce_samples: 12,
			divergence_plays: 4,
			divergence_rise_frac: 0.15
		},
		feasibility: { peak_accel: 5, peak_velocity_ips: 20, disp_pkpk_in: 0.9, kinematics_hp_hz: 3 },
		operator: 'J. Operator',
		article: 'Bracket',
		notes: 'bolted at four corners',
		specification: { document: 'Test procedure', clause: '4.2' },
		record: { streams: false },
		metadata: [{ key: 'run_number', label: 'Run Number', value: '7' }]
	};
}

describe('the TWR setup on the wire', () => {
	it('carries every field of a configuration through the draft and back', () => {
		const c = everyField();
		expect(toTwrConfig(fromTwrConfig(c), LAW)).toEqual(c);
	});

	it('carries each playback schedule, and the whole reference as no section', () => {
		for (const run of [{ schedule: 'once' as const }, { schedule: 'loop' as const, count: 25 }, { schedule: 'duration' as const, duration_s: 600 }]) {
			const c = { ...everyField(), run, section: null };
			expect(toTwrConfig(fromTwrConfig(c), LAW)).toEqual(c);
		}
	});

	it('a field the draft dropped fails the round trip', () => {
		const c = everyField();
		const d = fromTwrConfig(c);
		const back = toTwrConfig(d, LAW) as unknown as Record<string, Record<string, unknown>>;
		delete back.limits.envelope_debounce_samples;
		expect(back).not.toEqual(c);
	});

	it("a configuration that leaves fields out takes the wire's defaults; the correction band is the identification's", () => {
		const d = fromTwrConfig({ title: 'T', reference: { sha256: 'a'.repeat(64) }, identification: { band_hz: [30, 1500] }, limits: { drive_vpk_ceiling: 1 } });
		expect(d.correction.band_hz).toEqual([30, 1500]);
		expect(d.schedule.levels_db).toEqual([-12, -9, -6, -3, 0]);
		expect(d.pads).toEqual({ pre: 512, post: 1024 });
		expect(d.section.whole).toBe(true);
		expect(d.acquisition.fs_hz).toBeNull();
		expect(d.law.id).toBe('twr_correct');
		expect(d.run.schedule).toBe('once');
	});

	it('sends only the parameters the law declares', () => {
		const d = defaultTwrSetup();
		d.law.params = { beta: 0.3, stray: 1 };
		expect(toTwrConfig(d, LAW).control?.law?.params).toEqual({ beta: 0.3 });
	});

	it('asks the service to prepare a reference only once one is chosen and its numbers read', () => {
		const d = defaultTwrSetup();
		expect(previewParams(d)).toBeNull();
		d.reference.sha256 = 'b'.repeat(64);
		const p = previewParams(d)!;
		expect(p.reference).toEqual({ sha256: 'b'.repeat(64) });
		expect(p.section).toBeNull();
		expect(Object.keys(p).sort()).toEqual(['acquisition', 'correction', 'feasibility', 'pads', 'prep', 'reference', 'section']);
		d.pads.pre = NaN;
		expect(previewParams(d)).toBeNull();
	});

	it('keeps a draft across a reload, an emptied field included', () => {
		const d = defaultTwrSetup();
		d.reference = { sha256: 'c'.repeat(64), path: '/r.csv' };
		d.prep.scale = NaN;
		d.schedule.levels_db = [-6, 0];
		const back = loadTwrSetup(saveTwrSetup(d));
		expect(back.reference).toEqual(d.reference);
		expect(Number.isNaN(back.prep.scale)).toBe(true);
		expect(back.schedule.levels_db).toEqual([-6, 0]);
		expect(mergeTwrSetup({ schedule: { levels_db: [] } as never }).schedule.levels_db).toEqual([-12, -9, -6, -3, 0]);
	});
});

describe('what the TWR editor says before anything is sent', () => {
	it('asks for a reference and the drive ceiling, which have no default', () => {
		const e = localErrors(defaultTwrSetup());
		expect(Object.keys(e).sort()).toEqual(['limits.drive_vpk_ceiling', 'reference']);
		expect(e['limits.drive_vpk_ceiling'][0]).toMatch(/no default/);
	});

	it('names an emptied field, a whole number that is not, an alarm band wider than the abort band', () => {
		const d = defaultTwrSetup();
		d.reference.sha256 = 'd'.repeat(64);
		d.limits.drive_vpk_ceiling = 0.5;
		d.pads.pre = 1.5;
		d.identification.drive_vrms = NaN;
		d.limits.envelope_alarm_frac = 0.6;
		d.identification.band_hz = [2000, 20];
		d.run.schedule = 'loop';
		d.run.count = NaN;
		d.schedule.levels_db = [];
		const e = localErrors(d);
		expect(e['pads.pre']).toEqual(['Enter a whole number.']);
		expect(e['identification.drive_vrms']).toEqual(['Enter a number.']);
		expect(e['limits.envelope_alarm_frac'][0]).toMatch(/at most the abort band/);
		expect(e['identification.band_hz'][0]).toMatch(/low edge up to its high edge/);
		expect(e['run.count']).toEqual(['Enter a whole number.']);
		expect(e['schedule.levels_db'][0]).toMatch(/at least one level/);
	});

	it('checks a section only when one is given', () => {
		const d = defaultTwrSetup();
		expect(localErrors(d)['section.len']).toBeUndefined();
		d.section.whole = false;
		expect(localErrors(d)['section.len']).toEqual(['Enter a whole number.']);
	});
});

describe("where the service's TWR findings land", () => {
	it('by field: the head and brackets go, the reference is the one the setup chose, the law is the panel', () => {
		const at = (field: string, reason = 'out_of_range') => twrItemKey({ field, reason });
		expect(at('config.schedule.levels_db[2]')).toBe('schedule.levels_db.2');
		expect(at('config.reference.sha256', 'reference_unknown')).toBe('reference');
		expect(at('config.reference.path', 'reference_changed')).toBe('reference');
		expect(at('config.identification.band_hz[1]')).toBe('identification.band_hz');
		expect(at('config.control.law.params.beta', 'law_param_invalid')).toBe('law.params.beta');
		expect(at('config.control.law.id', 'law_unknown')).toBe('law.id');
		expect(at('config.limits.drive_vpk_ceiling')).toBe('limits.drive_vpk_ceiling');
		expect(at('config.section', 'section_outside_reference')).toBe('section');
		expect(at('config.section.len')).toBe('section.len');
	});

	it('by reason when the service names no field', () => {
		const at = (reason: string) => twrItemKey({ reason });
		expect(at('no_control_channel')).toBe('station.channels');
		expect(at('units_mismatch')).toBe('reference');
		expect(at('record_too_long')).toBe('pads');
		expect(at('rate_mismatch')).toBe('acquisition.fs_hz');
	});

	it('in the section that edits it', () => {
		const sections = Object.fromEntries(
			['title', 'reference', 'prep.scale', 'section.len', 'pads.pre', 'acquisition.fs_hz', 'identification.nfft', 'law.params.beta', 'control.law_budget', 'inverse.tikhonov_floor', 'correction.band_hz', 'schedule.gate_pct', 'run.count', 'adaptation.during_playback', 'limits.drive_vpk_ceiling', 'feasibility.disp_pkpk_in', 'operator', 'specification.document', 'record.streams', 'metadata', 'station.channels'].map((k) => [k, twrSectionOf(k)])
		);
		expect(sections).toEqual({
			title: 'reference',
			reference: 'reference',
			'prep.scale': 'reference',
			'section.len': 'reference',
			'pads.pre': 'reference',
			'acquisition.fs_hz': 'reference',
			'identification.nfft': 'identification',
			'law.params.beta': 'control',
			'control.law_budget': 'control',
			'inverse.tikhonov_floor': 'control',
			'correction.band_hz': 'control',
			'schedule.gate_pct': 'schedule',
			'run.count': 'schedule',
			'adaptation.during_playback': 'schedule',
			'limits.drive_vpk_ceiling': 'limits',
			'feasibility.disp_pkpk_in': 'limits',
			operator: 'record',
			'specification.document': 'record',
			'record.streams': 'record',
			metadata: 'record',
			'station.channels': 'station'
		});
	});

	it('one message once per key', () => {
		const e = twrByKey([
			{ reason: 'out_of_range', field: 'config.schedule.gate_pct', message: 'm' },
			{ reason: 'out_of_range', field: 'schedule.gate_pct', message: 'm' }
		]);
		expect(e).toEqual({ 'schedule.gate_pct': ['m'] });
	});
});
