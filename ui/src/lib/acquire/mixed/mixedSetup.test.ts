/**
 * The Mixed Mode draft against the wire: PROTOCOL.md's own combined configuration read into the
 * draft and written back field for field (every part kind, every schedule kind, both amplitude and
 * both width forms, the laws and their guards, limits, the run record); every wire field the draft writes
 * is one the configuration names; the editor's own findings; where a service finding lands; and an
 * edit made to several rows at once.
 */
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { MixedConfig } from '$lib/protocol/mixed';
import {
	copyField,
	defaultMixedSetup,
	freeId,
	fromMixedConfig,
	localErrors,
	mergeMixedSetup,
	mixedByKey,
	mixedItemKey,
	mixedSectionOf,
	newTone,
	partsOf,
	scheduleDraftOf,
	scheduleOf,
	toMixedConfig
} from './mixedSetup';

// This suite's environment has no module URL to resolve from: the protocol is found from the folder the tests run in.
const PROTOCOL = readFileSync([resolve(process.cwd(), '../schema/PROTOCOL.md'), resolve(process.cwd(), 'schema/PROTOCOL.md')].find((p) => existsSync(p))!, 'utf8').replace(/\r\n/g, '\n');

/** PROTOCOL's combined configuration, its placeholders taken out. */
function protocolConfig(): MixedConfig {
	const at = PROTOCOL.indexOf('### Mixed Mode Configuration');
	const from = PROTOCOL.indexOf('```\n', at) + 4;
	const text = PROTOCOL.slice(from, PROTOCOL.indexOf('```', from))
		.replace(/,\s*\.\.\.\s*\}/g, '}');
	return JSON.parse(text) as MixedConfig;
}

const laws = { floor: null, tones: null };

/** Keys sorted, so key order on the wire does not matter. */
function sorted(v: unknown): unknown {
	if (Array.isArray(v)) return v.map(sorted);
	if (v && typeof v === 'object') return Object.fromEntries(Object.keys(v as object).sort().map((k) => [k, sorted((v as Record<string, unknown>)[k])]));
	return v;
}

describe('the draft and the wire', () => {
	it("reads PROTOCOL's combined configuration and writes every part back as it came", () => {
		const c = protocolConfig();
		const d = fromMixedConfig(c);
		expect(d.tones.map((t) => t.id)).toEqual(['t1', 't2', 't3']);
		expect(d.bands.map((b) => b.id)).toEqual(['b1', 'b2']);
		expect(d.tones[1].amplitude.by).toBe('profile');
		expect(d.tones[2].schedule).toMatchObject({ kind: 'follow', of: 't2', ratio: 2 });
		expect(d.bands[1].width).toMatchObject({ by: 'share', share: 0.05 });
		const back = toMixedConfig(d, laws);
		// the floor's and every part's fields, field for field
		back.parts.forEach((p, i) => expect(sorted(p), p.id).toEqual(sorted(c.parts[i])));
		expect(back.exclusion_hz).toBe(c.exclusion_hz);
		expect(back.acquisition).toEqual(c.acquisition);
		expect(sorted(back.schedule)).toEqual(sorted(c.schedule));
		expect(back.limits.drive_vpk_ceiling).toBe(4);
		expect(back.limits.channels).toEqual(c.limits.channels);
		expect([back.operator, back.article, back.notes, back.specification, back.record]).toEqual([c.operator, c.article, c.notes, c.specification, c.record]);
		expect(back.metadata).toEqual(c.metadata);
	});

	it('writes every control field of the configuration, with the values it came with', () => {
		const c = protocolConfig();
		const back = toMixedConfig(fromMixedConfig(c), laws);
		const want = c.control!;
		const got = back.control!;
		expect(Object.keys(got).sort()).toEqual(Object.keys(want).sort());
		for (const k of Object.keys(want) as (keyof typeof want)[]) {
			if (k === 'law') continue;
			expect(sorted(got[k]), k).toEqual(sorted(want[k]));
		}
		expect(got.law?.floor?.id).toBe('hinv');
		expect(got.law?.tones?.id).toBe('servo_blend');
	});

	it('the default draft is a sine-on-random with lines no wider than the tracking width', () => {
		const d = defaultMixedSetup();
		const c = toMixedConfig(d, laws);
		expect(c.parts.map((p) => p.kind)).toEqual(['floor', 'tone']);
		expect(c.acquisition.fs_hz / c.acquisition.n_pts).toBeLessThanOrEqual(c.control!.tracking!.hz);
		// no drive ceiling is made up: the setup asks for it
		expect(localErrors(d)['limits.drive_vpk_ceiling']).toBeTruthy();
	});

	it('writes each schedule kind in its wire shape, and reads each back', () => {
		const s = scheduleDraftOf({ kind: 'sweep', band_hz: [300, 600], rate: { law: 'log', oct_per_min: 1 }, start_hz: 300, direction: 'up', end: { sweeps: 4, duration_s: null } }, 300);
		expect(scheduleOf(s)).toEqual({ kind: 'sweep', band_hz: [300, 600], rate: { law: 'log', oct_per_min: 1 }, start_hz: 300, direction: 'up', end: { sweeps: 4, duration_s: null } });
		s.end_by = 'none';
		expect((scheduleOf(s) as { end: unknown }).end).toBeNull();
		const f = scheduleDraftOf({ kind: 'fixed', freq_hz: 120, end: { cycles: 600 } }, 120);
		expect(scheduleOf(f)).toEqual({ kind: 'fixed', freq_hz: 120, end: { cycles: 600 } });
		expect(scheduleOf(scheduleDraftOf({ kind: 'follow', of: 't2', ratio: 3 }, 0))).toEqual({ kind: 'follow', of: 't2', ratio: 3 });
	});

	it('puts the parts on the wire floor first, then the tones, then the narrowbands', () => {
		const d = defaultMixedSetup();
		d.bands = [fromMixedConfig(protocolConfig()).bands[0]];
		expect(partsOf(d).map((p) => p.kind)).toEqual(['floor', 'tone', 'narrowband']);
	});

	it('keeps a draft through storage, filling what a stored draft lacks', () => {
		const d = mergeMixedSetup({ title: 'Kept', tones: [{ id: 't9' } as never] });
		expect(d.title).toBe('Kept');
		expect(d.tones[0].id).toBe('t9');
		expect(d.tones[0].schedule.kind).toBe('fixed');
		expect(d.floor.breakpoints.length).toBe(4);
	});
});

describe('what the editor says before anything is sent', () => {
	it('names an id that is not one, an id used twice, a follower of nothing and of itself', () => {
		const d = defaultMixedSetup();
		d.tones.push(newTone('t1', 2), newTone('bad id!', 3), newTone('t4', 4), newTone('t5', 5));
		d.tones[3].schedule.kind = 'follow';
		d.tones[3].schedule.of = 'nope';
		d.tones[4].schedule.kind = 'follow';
		d.tones[4].schedule.of = 't5';
		const e = localErrors(d);
		expect(e['tones.0.id']).toEqual(['Another part is "t1" too.']);
		expect(e['tones.2.id']?.[0]).toMatch(/1 to 32/);
		expect(e['tones.3.schedule']?.[0]).toMatch(/no tone or narrowband "nope"/);
		expect(e['tones.4.schedule']).toEqual(['A part cannot follow itself.']);
	});

	it('gives a free id for a new part', () => {
		const d = defaultMixedSetup();
		expect(freeId(d, 't')).toBe('t2');
		expect(freeId(d, 'b')).toBe('b1');
	});
});

describe('where a service finding lands', () => {
	const d = fromMixedConfig(protocolConfig());
	it('a part finding goes on its table, row and column', () => {
		expect(mixedItemKey(d, { reason: 'follow_loop', field: 'parts[3].schedule.of' })).toBe('tones.2.schedule');
		expect(mixedItemKey(d, { reason: 'part_out_of_band', field: 'parts[5]' })).toBe('bands.1.row');
		expect(mixedItemKey(d, { reason: 'out_of_range', field: 'parts[2].profile.rows', row: 1 })).toBe('tones.1.amplitude');
		expect(mixedItemKey(d, { reason: 'out_of_range', field: 'config.parts[4].width_hz' })).toBe('bands.0.width');
		expect(mixedItemKey(d, { reason: 'frequency_not_ascending', field: 'parts[0].breakpoints', row: 2 })).toBe('profile.breakpoints.2.freq_hz');
	});
	it('the laws, their guards, the tracking and the limits go on their fields', () => {
		expect(mixedItemKey(d, { reason: 'law_wrong_world', field: 'control.law.floor' })).toBe('lawFloor.id');
		expect(mixedItemKey(d, { reason: 'law_param_invalid', field: 'control.law.tones.params.kp' })).toBe('lawTones.params.kp');
		expect(mixedItemKey(d, { reason: 'out_of_range', field: 'control.law_guards.tones.max_step_db' })).toBe('guards.tones.max_step_db');
		expect(mixedItemKey(d, { reason: 'out_of_range', field: 'control.tracking.hz' })).toBe('control.tracking_hz');
		expect(mixedItemKey(d, { reason: 'limit_on_control_channel', field: 'limits.channels[0].channel' })).toBe('limits.channels.0');
		expect(mixedItemKey(d, { reason: 'no_drive_channel' })).toBe('station.channels');
	});
	it('every key is in a section of the screen', () => {
		const keys = Object.keys(mixedByKey(d, [{ reason: 'parts_too_close', field: 'parts[2]', message: 'a' }, { reason: 'out_of_range', field: 'exclusion_hz', message: 'b' }, { reason: 'out_of_range', field: 'control.pretest.averages', message: 'c' }]));
		expect(keys.map(mixedSectionOf)).toEqual(['parts', 'parts', 'schedule']);
		expect(mixedSectionOf('lawTones.params.kp')).toBe('control');
		expect(mixedSectionOf('limits.drive_vpk_ceiling')).toBe('limits');
	});
});

describe('several rows at once', () => {
	it('an edit at one picked row is made to every picked row; ids and names stay their own', () => {
		const d = defaultMixedSetup();
		d.tones.push(newTone('t2', 2), newTone('t3', 3));
		d.tones[0].window.on_s = 12;
		expect(copyField(d.tones, [0, 2], 0, 'window.on_s')).toBe(1);
		expect(d.tones.map((t) => t.window.on_s)).toEqual([12, 0, 12]);
		d.tones[1].amplitude.g = NaN;
		copyField(d.tones, [0, 1, 2], 1, 'amplitude.g');
		expect(d.tones.every((t) => Number.isNaN(t.amplitude.g))).toBe(true);
		expect(copyField(d.tones, [0, 1], 0, 'id')).toBe(0);
		expect(d.tones.map((t) => t.id)).toEqual(['t1', 't2', 't3']);
	});
});
