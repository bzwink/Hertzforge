/**
 * The Explorer setup draft against the wire: every field explicit on the way out and read back
 * unchanged, the steering steps kept off the wire, the draft read back from storage, what the
 * editor says before anything is sent, and where each of the service's findings lands.
 */
import { describe, expect, it } from 'vitest';
import type { ExplorerConfig } from '$lib/protocol/explorer';
import { DEFAULT_STEPS, defaultExplorerSetup, EXPLORER_SECTIONS, explorerByKey, explorerChannels, explorerItemKey, explorerSectionOf, fromExplorerConfig, localErrors, mergeExplorerSetup, toExplorerConfig } from './explorerSetup';
import { loadExplorerSetup, saveExplorerSetup } from './explorerStore.svelte';

/** PROTOCOL's example, every field given. */
const EXAMPLE: ExplorerConfig = {
	title: 'Survey, fixture A',
	acquisition: { fs_hz: 12800 },
	band_hz: [20, 2000],
	start_hz: 250,
	max_drive_vpk: 0.5,
	slew: { freq_oct_per_s: 1, drive_db_per_s: 6 },
	display: { nfft: 4096 },
	capture: { run_name: 'explorer' },
	metadata: [{ key: 'run_number', label: 'Run Number', value: '3' }],
	operator: 'A. Operator',
	article: 'Bracket, SN 0042',
	notes: '',
	specification: { document: 'Test procedure', clause: '4.2' },
	record: { streams: true }
};

describe('the Explorer setup draft', () => {
	it("carries the Explorer run record's words and its streams switch to the wire and back", () => {
		const d = fromExplorerConfig(EXAMPLE);
		expect(d.record).toEqual({ operator: 'A. Operator', article: 'Bracket, SN 0042', notes: '', document: 'Test procedure', clause: '4.2', streams: true });
		d.record.streams = false;
		d.record.document = '';
		d.record.clause = '';
		const c = toExplorerConfig(d);
		expect([c.record, c.specification]).toEqual([{ streams: false }, null]);
		// a configuration without them reads as the wire's defaults: streams on, no words
		const bare = fromExplorerConfig({ max_drive_vpk: 0.5 });
		expect(bare.record).toEqual({ operator: '', article: '', notes: '', document: '', clause: '', streams: true });
		// a clause with no document is said before anything is sent, on the record's section
		bare.record.clause = '4.2';
		expect(localErrors(bare)['specification.document']).toEqual(['Name the document this clause is from.']);
		expect(['operator', 'article', 'notes', 'specification.document', 'record.streams'].map(explorerSectionOf)).toEqual(['capture', 'capture', 'capture', 'capture', 'capture']);
		expect(EXPLORER_SECTIONS.find((s) => s.id === 'capture')?.label).toBe('Run Record');
		// a draft saved before the record existed reads back with it
		const old = { ...defaultExplorerSetup() } as Partial<ReturnType<typeof defaultExplorerSetup>>;
		delete old.record;
		expect(mergeExplorerSetup(old).record.streams).toBe(true);
	});

	it('sends every Explorer wire field and reads each back unchanged', () => {
		expect(toExplorerConfig(fromExplorerConfig(EXAMPLE))).toEqual(EXAMPLE);
		const keys = Object.keys(toExplorerConfig({ ...defaultExplorerSetup(), max_drive_vpk: 0.5 })).sort();
		expect(keys).toEqual(Object.keys(EXAMPLE).sort());
		// each field reaches the wire: change one and only that one changes
		const base = toExplorerConfig(fromExplorerConfig(EXAMPLE));
		const d = fromExplorerConfig(EXAMPLE);
		d.slew.drive_db_per_s = 30;
		expect(toExplorerConfig(d)).toEqual({ ...base, slew: { ...base.slew, drive_db_per_s: 30 } });
		d.acquisition.fs_hz = null;
		expect(toExplorerConfig(d).acquisition).toEqual({ fs_hz: null });
	});

	it('keeps the Explorer steering steps on the screen, never on the wire', () => {
		const d = fromExplorerConfig(EXAMPLE, { ...DEFAULT_STEPS, fine_per_oct: 48 });
		expect(d.steps.fine_per_oct).toBe(48);
		expect(JSON.stringify(toExplorerConfig(d))).not.toMatch(/per_oct|fine|coarse|steps/);
		expect(DEFAULT_STEPS).toEqual({ fine_per_oct: 96, coarse_per_oct: 6, fine_db: 0.5, coarse_db: 3, sweep_oct_per_min: 4 });
	});

	it('fills an Explorer configuration that leaves fields out with the wire defaults', () => {
		const d = fromExplorerConfig({ max_drive_vpk: 0.4 });
		expect(toExplorerConfig(d)).toEqual({ title: '', acquisition: { fs_hz: null }, band_hz: [10, 2000], start_hz: 100, max_drive_vpk: 0.4, slew: { freq_oct_per_s: 1, drive_db_per_s: 6 }, display: { nfft: 4096 }, capture: { run_name: 'explorer' }, metadata: [], operator: '', article: '', notes: '', specification: null, record: { streams: true } });
	});

	it('reads an Explorer draft back from storage, an emptied field kept empty, a missing part from the defaults', () => {
		const d = defaultExplorerSetup();
		d.start_hz = NaN;
		const back = loadExplorerSetup(saveExplorerSetup(d));
		expect(Number.isNaN(back.start_hz)).toBe(true);
		expect(Number.isNaN(back.max_drive_vpk)).toBe(true);
		expect(mergeExplorerSetup({ title: 'x', steps: { fine_db: 1 } as never }).steps).toEqual({ ...DEFAULT_STEPS, fine_db: 1 });
		expect(loadExplorerSetup('not json')).toEqual(defaultExplorerSetup());
	});

	it('names what the Explorer editor can see before sending: the ceiling, an empty field, a step that does not move', () => {
		const d = defaultExplorerSetup();
		expect(Object.keys(localErrors(d))).toEqual(['max_drive_vpk']);
		d.max_drive_vpk = 0.5;
		expect(localErrors(d)).toEqual({});
		d.band_hz = [20, NaN];
		d.steps.coarse_per_oct = 200;
		d.steps.fine_db = 0;
		d.capture.run_name = ' ';
		expect(localErrors(d)).toEqual({
			band_hz: ['Give both ends of the band.'],
			'capture.run_name': ['Name the run folder.'],
			'steps.fine_db': ['A step is above 0.'],
			'steps.coarse_per_oct': ['The coarse step is at least the fine one: no more steps an octave.']
		});
	});

	it("lands each Explorer finding on its field and section: an item's field, a refusal's param, the table's three", () => {
		expect(explorerItemKey({ reason: 'band_above_grid', field: 'band_hz' })).toBe('band_hz');
		expect(explorerItemKey({ reason: 'out_of_range', field: 'band_hz[1]' })).toBe('band_hz');
		expect(explorerItemKey({ reason: 'out_of_range', field: 'config.slew.drive_db_per_s' })).toBe('slew.drive_db_per_s');
		expect(explorerItemKey({ reason: 'out_of_range', field: 'metadata[0].key' })).toBe('metadata');
		for (const reason of ['no_drive_channel', 'extra_drive_channels', 'no_input_channel']) expect(explorerItemKey({ reason })).toBe('station.channels');
		expect(explorerSectionOf('station.channels')).toBe('station');
		expect(explorerSectionOf('capture.run_name')).toBe('capture');
		expect(explorerSectionOf('metadata')).toBe('capture');
		for (const k of ['band_hz', 'start_hz', 'max_drive_vpk', 'slew.drive_db_per_s', 'display.nfft', 'acquisition.fs_hz', 'steps.fine_db', 'title']) expect(explorerSectionOf(k)).toBe('test');
		expect(EXPLORER_SECTIONS.map((s) => s.id)).toEqual(['station', 'test', 'capture']);
		expect(explorerByKey([{ reason: 'safe_stop_budget', field: 'slew.drive_db_per_s', message: 'm' }, { reason: 'safe_stop_budget', field: 'slew.drive_db_per_s', message: 'm' }])).toEqual({ 'slew.drive_db_per_s': ['m'] });
	});

	it('names the Explorer control row as PROTOCOL does: the first control row, else the first input', () => {
		const row = (index: number, role: 'control' | 'monitor' | 'drive', enabled = true) => ({ index, role, enabled }) as never;
		expect(explorerChannels([row(1, 'monitor'), row(2, 'control'), row(3, 'drive')])).toMatchObject({ control: { index: 2 }, drive: { index: 3 }, drives: 1, inputs: 2 });
		expect(explorerChannels([row(1, 'monitor'), row(2, 'control', false), row(3, 'drive'), row(4, 'drive')])).toMatchObject({ control: { index: 1 }, drives: 2, inputs: 1 });
	});
});
