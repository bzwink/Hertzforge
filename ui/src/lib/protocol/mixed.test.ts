/**
 * Mixed Mode's wire types against PROTOCOL.md's own examples: the configuration, the derived
 * values and the run state there carry every field the types name, and no other, so the text and
 * the types cannot drift apart. The goldens come with the service.
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
	MIXED_CHECK_CASES,
	MIXED_LAW_WORLDS,
	MIXED_MAX_BANDS,
	MIXED_MAX_TONES,
	MIXED_TOPICS,
	type MixedConfig,
	type MixedControl,
	type MixedDerived,
	type MixedDerivedPart,
	type MixedLawGuards,
	type MixedLimits,
	type MixedPartRow,
	type MixedState,
	type MixedSweepSchedule
} from './mixed';

// A Windows checkout may hold the file with CRLF line ends; the examples are found by their LF fences.
const PROTOCOL = readFileSync(fileURLToPath(new URL('../../../../schema/PROTOCOL.md', import.meta.url)), 'utf8').replace(/\r\n/g, '\n');

/** The first example block after `heading`, with the text's `...` placeholders taken out. */
function example(heading: string, after = 0): unknown {
	const at = PROTOCOL.indexOf(heading);
	expect(at, heading).toBeGreaterThan(0);
	let from = at;
	for (let i = 0; i <= after; i++) from = PROTOCOL.indexOf('```\n', from) + 4;
	const text = PROTOCOL.slice(from, PROTOCOL.indexOf('```', from))
		.replace(/,\s*\.\.\.\s*\]/g, ']')
		.replace(/,\s*\.\.\.\s*\}/g, '}');
	return JSON.parse(text);
}

/** Every key of `T`, listed once: the compiler holds the list to the type both ways. */
const keysOf = <T>(keys: Record<keyof T, true>) => Object.keys(keys).sort();

const CONFIG_KEYS = keysOf<MixedConfig>({ title: true, parts: true, exclusion_hz: true, acquisition: true, schedule: true, control: true, limits: true, feasibility: true, operator: true, article: true, notes: true, specification: true, record: true, metadata: true });
const CONTROL_KEYS = keysOf<MixedControl>({
	composite: true,
	weights: true,
	control_dof: true,
	damping: true,
	max_lines_out: true,
	max_loops_out: true,
	max_sigma: true,
	drive_ceiling_vrms: true,
	drive_abort_vrms: true,
	coherence_floor: true,
	review_after_sysid: true,
	pretest: true,
	soft_start_db: true,
	soft_start_relax_db: true,
	level_ramp_s: true,
	shutdown_ramp_s: true,
	part_ramp_s: true,
	floor_settle_s: true,
	trust_coherence: true,
	trust_h_floor_frac: true,
	tracking: true,
	law: true,
	law_guards: true
});
const GUARD_KEYS = {
	floor: keysOf<NonNullable<MixedLawGuards['floor']>>({ law_budget: true, law_misses: true, max_change_db: true }),
	tones: keysOf<NonNullable<MixedLawGuards['tones']>>({ law_budget: true, law_misses: true, max_step_db: true, max_rise_db_per_s: true, divergence_s: true, fault_debounce_ticks: true })
};
const LIMIT_KEYS = keysOf<MixedLimits>({ drive_vpk_ceiling: true, drive_vpk_abort: true, kinematic_monitor: true, channels: true });
const SWEEP_KEYS = keysOf<MixedSweepSchedule>({ kind: true, band_hz: true, rate: true, start_hz: true, direction: true, end: true });
const DERIVED_KEYS = keysOf<MixedDerived>({
	band_hz: true,
	grid: true,
	control_channels: true,
	parts: true,
	reference: true,
	kinematics: true,
	exclusion: true,
	closest_tones: true,
	tracking: true, tone_judging: true,
	test_s: true,
	floor_equalize_s: true,
	identification_s: true,
	call_budget_s: true,
	law: true,
	feasibility: true
});
const DERIVED_PART_KEYS = keysOf<MixedDerivedPart>({ id: true, kind: true, name: true, freq_hz: true, width_hz: true, ms_g2: true, peak_g: true });
const STATE_KEYS = keysOf<MixedState>({
	run_id: true,
	state: true,
	phase: true,
	title: true,
	source: true,
	station: true,
	grid: true,
	clock: true,
	mode: true,
	levels_db: true,
	level_db: true,
	full_level_s: true,
	t_s: true,
	elapsed_s: true,
	parts_s: true,
	at_level_s: true,
	remaining_at_level_s: true,
	held: true,
	alarm: true,
	limiting: true,
	parts: true,
	total: true,
	identification: true,
	law: true,
	feasibility: true,
	end: true,
	record: true
});
const ROW_KEYS = keysOf<MixedPartRow>({
	follows: true,
	judge_s: true,
	judged_error_db: true,
	judged_alarm_db: true,
	judged_abort_db: true,
	tolerance_widened: true,
	id: true,
	kind: true,
	name: true,
	enabled: true,
	on: true,
	scale: true,
	freq_hz: true,
	direction: true,
	sweeps: true,
	rate_oct_per_min: true,
	rate_hz_per_s: true,
	width_hz: true,
	unit: true,
	target: true,
	measured: true,
	error_db: true,
	alarm: true,
	drive_vpk: true,
	drive_vrms: true,
	gain_db: true,
	settled: true,
	limiting: true,
	dof: true,
	lines: true,
	law: true
});

const sorted = (o: object) => Object.keys(o).sort();

describe('Mixed Mode on the wire', () => {
	const config = example('### Mixed Mode Configuration') as MixedConfig;
	const derived = example('**Result.** `state` is the `mixed.state` object') as MixedDerived;
	const state = example('### Mixed Mode Run State') as MixedState;

	it("states a configuration with every field: a floor, a fixed, a swept and a following tone, a fixed and a swept narrowband", () => {
		expect(sorted(config)).toEqual(CONFIG_KEYS);
		expect(sorted(config.control!)).toEqual(CONTROL_KEYS);
		expect(sorted(config.control!.law_guards!.floor!)).toEqual(GUARD_KEYS.floor);
		expect(sorted(config.control!.law_guards!.tones!)).toEqual(GUARD_KEYS.tones);
		expect(sorted(config.limits)).toEqual(LIMIT_KEYS);
		const kinds = config.parts.map((p) => `${p.kind}:${'schedule' in p ? p.schedule.kind : '-'}`);
		expect(kinds).toEqual(['floor:-', 'tone:fixed', 'tone:sweep', 'tone:follow', 'narrowband:fixed', 'narrowband:sweep']);
		for (const p of config.parts) {
			expect(sorted(p)).toEqual(expect.arrayContaining(['enabled', 'id', 'kind', 'name', 'window']));
			if (p.kind !== 'floor' && p.schedule.kind === 'sweep') expect(sorted(p.schedule)).toEqual(SWEEP_KEYS);
		}
		// Each tone has exactly one of the two amplitudes; each band exactly one of the two widths.
		for (const p of config.parts) {
			if (p.kind === 'tone') expect(('amplitude_g' in p ? 1 : 0) + ('profile' in p ? 1 : 0)).toBe(1);
			if (p.kind === 'narrowband') expect(('width_hz' in p ? 1 : 0) + ('width_share' in p ? 1 : 0)).toBe(1);
		}
		// The laws are asked for by role, from the existing worlds.
		expect(config.control!.law).toEqual({ floor: { id: 'hinv', params: {} }, tones: expect.objectContaining({ id: 'servo_blend' }) });
		expect(MIXED_LAW_WORLDS).toEqual({ floor: 'single', tones: 'sine' });
	});

	it('states the derived values with one row per part, in the parts order, and the totals they sum to', () => {
		expect(sorted(derived)).toEqual(DERIVED_KEYS);
		expect(derived.parts.map((p) => p.id)).toEqual(config.parts.map((p) => p.id));
		for (const p of derived.parts) expect(sorted(p)).toEqual(DERIVED_PART_KEYS);
		const r = derived.reference;
		const sum = (k: string) => derived.parts.filter((p) => p.kind === k).reduce((a, p) => a + p.ms_g2, 0);
		expect(r.floor_ms_g2).toBeCloseTo(sum('floor'), 6);
		expect(r.tones_ms_g2).toBeCloseTo(sum('tone'), 6);
		expect(r.bands_ms_g2).toBeCloseTo(sum('narrowband'), 6);
		expect(r.total_ms_g2).toBeCloseTo(r.floor_ms_g2 + r.tones_ms_g2 + r.bands_ms_g2, 6);
		expect(r.total_rms_g).toBeCloseTo(Math.sqrt(r.total_ms_g2), 3);
		expect(r.random_rms_g).toBeCloseTo(Math.sqrt(r.floor_ms_g2 + r.bands_ms_g2), 2);
		// A tone's mean square is half its largest amplitude squared.
		for (const p of derived.parts.filter((p) => p.kind === 'tone')) expect(p.ms_g2).toBeCloseTo(p.peak_g! ** 2 / 2, 6);
		expect(derived.tracking.map((t) => t.part)).toEqual(config.parts.filter((p) => p.kind === 'tone').map((p) => p.id));
	});

	it('states the run state with every field, and its part rows with every field and the part id', () => {
		expect(sorted(state)).toEqual(STATE_KEYS);
		expect(state.parts.length).toBeGreaterThan(1);
		for (const row of state.parts) {
			expect(sorted(row)).toEqual(ROW_KEYS);
			expect(config.parts.map((p) => p.id)).toContain(row.id);
		}
		// Rows keep the parts' order.
		const order = state.parts.map((r) => config.parts.findIndex((p) => p.id === r.id));
		expect(order).toEqual([...order].sort((a, b) => a - b));
	});

	it('names its topics, limits and check cases', () => {
		expect(Object.values(MIXED_TOPICS)).toEqual(['mixed.state', 'mixed.log', 'mixed.live', 'mixed.data']);
		expect([MIXED_MAX_TONES, MIXED_MAX_BANDS]).toEqual([12, 12]);
		expect(MIXED_CHECK_CASES).toHaveLength(8);
		for (const c of MIXED_CHECK_CASES) expect(PROTOCOL).toContain(`\`${c}\``);
	});
});
