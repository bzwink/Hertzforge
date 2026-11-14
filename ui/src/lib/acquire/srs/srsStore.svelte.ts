/**
 * SRS shock's working state, against the control service's `srs.*` and `laws.*` methods
 * (schema/PROTOCOL.md, "SRS Shock", "Control Laws").
 *
 * The station, the channel table and the controller seat are the service's and shared by every
 * test type: they stay in the Acquire store, and this one reads them from there. What is SRS
 * shock's own is here: the setup draft (kept across reloads), the service's offline synthesis of
 * the target (`srs.preview`), the dry run of the setup, the target and component tables read from
 * files, the law catalog of the SRS world, the run's state and its pulse rows, the pulses picked to
 * lay over the plots, and the verbs. As everywhere in Acquire, the service holds the truth and
 * computes every number; this store keeps the operator's edits beside it and sends them when the
 * operator says so.
 */

import { regresses } from '../runOrder';
import { untrack } from 'svelte';
import type { ServiceClient } from '$lib/protocol/client.svelte';
import type { ChannelRow, LogEntry, RunRecordRef, View } from '$lib/protocol/types';
import type { LawEntry, LawsListResult } from '$lib/protocol/mimo';
import {
	SRS_TOPICS,
	type SrsConfig,
	type SrsConfigResult,
	type SrsConfigureResult,
	type SrsDryRunResult,
	type SrsLevelResult,
	type SrsLive,
	type SrsLogEvent,
	type SrsLogResult,
	type SrsPreviewResult,
	type SrsPulseEvent,
	type SrsPulseResult,
	type SrsPulseRow,
	type SrsPulsesResult,
	type SrsReportItem,
	type SrsState,
	type SrsTableImportResult,
	type SrsTargetImportResult
} from '$lib/protocol/srs';
import type { ActionId, ActionState, RunHead, SetupStatus } from '../actions';
import { lawParamErrors, lawsOfWorld, lawValues } from '../law/lawForm';
import { canonical } from '../mimo/mimoSetup';
import { errorCount, mergeErrors, type FieldErrors } from '../setup';
import type { AcquireState } from '../store.svelte';
import { toRefusal, type Refusal } from '../words';
import type { ShockVerbId } from '../shock/shockVerbs';
import { MAX_OVERLAY, toggleOverlay } from './srsLayout';
import {
	defaultSrsSetup,
	fitToBands,
	fromSrsConfig,
	localErrors,
	mergeSrsSetup,
	previewParams,
	SRS_SECTIONS,
	srsByKey,
	srsSectionOf,
	toSrsConfig,
	type SrsDraft,
	type SrsSection
} from './srsSetup';
import { srsBarActions, srsIsLive, srsModuleVerbs, srsPrimary, type SrsVerbContext } from './srsVerbs';
import { plannedPulses, SRS_OUTCOME_WORDS, SRS_PHASE_WORDS } from './srsWords';

const SETUP_KEY = 'hertzforge.acquire.srs.setup.v1';
/** The setup this screen applied, by run: `{runId, key}`. */
const HELD_KEY = 'hertzforge.acquire.srs.held.v1';

function storageOf(name: 'localStorage'): Storage | null {
	try {
		return typeof window === 'undefined' ? null : window[name];
	} catch {
		return null;
	}
}

function read(storage: Storage | null, key: string): string | null {
	try {
		return storage?.getItem(key) ?? null;
	} catch {
		return null;
	}
}

function write(storage: Storage | null, key: string, value: string | null) {
	try {
		if (!storage) return;
		if (value === null) storage.removeItem(key);
		else storage.setItem(key, value);
	} catch {
		/* storage refused; the draft just does not persist */
	}
}

/** JSON that keeps NaN (a field the operator has emptied) through a reload. */
const nanOut = (_k: string, v: unknown) => (typeof v === 'number' && Number.isNaN(v) ? '__NaN__' : v);
const nanIn = (_k: string, v: unknown) => (v === '__NaN__' ? NaN : v);

export function saveSrsSetup(d: SrsDraft): string {
	return JSON.stringify(d, nanOut);
}

export function loadSrsSetup(raw: string | null): SrsDraft {
	if (!raw) return defaultSrsSetup();
	try {
		return mergeSrsSetup(JSON.parse(raw, nanIn) as Partial<SrsDraft>);
	} catch {
		return defaultSrsSetup();
	}
}

export interface SrsOptions {
	local?: Storage | null;
	/** Delay before a dry run or a preview after an edit, ms. */
	checkDelayMs?: number;
	/** A display window observes: it keeps no draft and checks nothing. */
	drafts?: boolean;
}

/** What reading a table from a file said, for the field it was read into. */
export interface ImportNote {
	what: 'target' | 'table';
	path: string;
	errors: SrsReportItem[];
	warnings: SrsReportItem[];
}

export class SrsStore {
	readonly control: ServiceClient;
	private readonly localStore: Storage | null;
	private readonly checkDelayMs: number;
	private readonly drafts: boolean;

	// ---- the operator's draft --------------------------------------------------------------
	setup = $state<SrsDraft>(defaultSrsSetup());
	section = $state<SrsSection>('target');
	/** Where the operator is: the setup, or the run. */
	step = $state<'setup' | 'run'>('setup');

	// ---- from the service ------------------------------------------------------------------
	status = $state.raw<SrsState | null>(null);
	live = $state.raw<SrsLive | null>(null);
	/** What the service holds: the configuration of the present run. */
	served = $state.raw<SrsConfigResult | null>(null);
	/** The dry run of the draft's setup, and the key of the setup it checked. */
	check = $state.raw<SrsDryRunResult | null>(null);
	checkedKey = $state('');
	checking = $state(false);
	/** The service's offline synthesis of the draft's target, and the key of what it synthesized. */
	preview = $state.raw<SrsPreviewResult | null>(null);
	previewKey = $state('');
	previewBusy = $state(false);
	/** Why the service would not synthesize the draft as it stands, in its words. */
	previewError = $state<string | null>(null);
	laws = $state.raw<LawEntry[] | null>(null);
	lawProblems = $state.raw<{ file: string; reason: string }[]>([]);
	/** The last table read from a file, and what the service said about it. */
	imported = $state.raw<ImportNote | null>(null);
	/** The run's pulses, in order, and the views of the last one (from the `srs.pulse` event). */
	pulses = $state.raw<SrsPulseRow[]>([]);
	lastViews = $state.raw<View[]>([]);
	log = $state.raw<LogEntry[]>([]);
	/** The pulses picked in the table to lay over the SRS and Pulse pages, by row index. */
	overlay = $state.raw<number[]>([]);
	/** Each picked pulse's views, as `srs.pulse` gave them. */
	overlayViews = $state.raw<Record<number, View[]>>({});
	/** The setup this screen applied: the run it made and the setup's key. */
	held = $state.raw<{ runId: string | null; key: string } | null>(null);

	// ---- feedback --------------------------------------------------------------------------
	busy = $state<string | null>(null);
	#refusal = $state.raw<Refusal | null>(null);
	syncError = $state<string | null>(null);

	private checkSeq = 0;
	private previewSeq = 0;
	private pulsesRun: string | null = null;
	private checkTimer: ReturnType<typeof setTimeout> | null = null;
	private previewTimer: ReturnType<typeof setTimeout> | null = null;

	constructor(
		readonly acq: AcquireState,
		options: SrsOptions = {}
	) {
		this.control = acq.control;
		this.localStore = options.local === undefined ? storageOf('localStorage') : options.local;
		this.checkDelayMs = options.checkDelayMs ?? 300;
		this.drafts = options.drafts ?? true;
		this.setup = loadSrsSetup(read(this.localStore, SETUP_KEY));
		const held = read(this.localStore, HELD_KEY);
		if (held) {
			try {
				this.held = JSON.parse(held);
			} catch {
				this.held = null;
			}
		}
		this.control.on(SRS_TOPICS.state, (d) => this.onStatus(d as unknown as SrsState));
		this.control.on(SRS_TOPICS.live, (d) => (this.live = d as unknown as SrsLive));
		this.control.on(SRS_TOPICS.pulse, (d) => this.onPulse(d as unknown as SrsPulseEvent));
		this.control.on(SRS_TOPICS.log, (d) => {
			const e = d as unknown as SrsLogEvent;
			if (this.status?.run_id && e.run_id !== this.status.run_id) return;
			const have = new Set(this.log.map((x) => x.seq));
			this.log = [...this.log, ...e.entries.filter((x) => !have.has(x.seq))];
		});
	}

	// =========================================================================================
	// Derived state

	get connected(): boolean {
		return this.acq.connected;
	}

	get isController(): boolean {
		return this.acq.isController;
	}

	get rows(): ChannelRow[] {
		return this.acq.rows;
	}

	/** The channels the run's streams would hold: every enabled input and the drive. */
	get recordChannels(): number {
		return this.rows.filter((r) => r.enabled).length;
	}

	/** The sample rate the run asks for, for the size its streams take. */
	get recordRateHz(): number {
		return this.setup.acquisition.fs_hz;
	}

	/** The run's record, from arm on: its folder, whether it keeps the time histories, its problem. */
	get record(): RunRecordRef | null {
		return this.status?.record ?? null;
	}

	get isLive(): boolean {
		return srsIsLive(this.status);
	}

	/** Setup, station and table are locked while a run is live. */
	get locked(): boolean {
		return this.isLive || this.acq.isLive;
	}

	/** The SRS world's laws: built in first, then imported, as the service lists them. */
	get srsLaws(): LawEntry[] {
		return lawsOfWorld(this.laws, 'shock_srs');
	}

	/** The law chosen, from the catalog. */
	get law(): LawEntry | null {
		return this.srsLaws.find((l) => l.id === this.setup.law.id) ?? null;
	}

	readonly wire = $derived<SrsConfig>(toSrsConfig(this.setup, this.law));
	readonly key = $derived(canonical(this.wire));
	readonly previewInput = $derived(previewParams(this.setup));
	readonly previewInputKey = $derived(this.previewInput ? canonical(this.previewInput) : '');

	/** The service's synthesis, when it answered the draft's target as it stands. */
	get synthesis(): SrsPreviewResult | null {
		return this.preview && this.previewKey === this.previewInputKey ? this.preview : null;
	}

	/** The band grid the service derived for the draft, or null until it has said. */
	get bandsHz(): number[] | null {
		const fromCheck = this.check && this.checkedKey === this.key ? this.check.derived?.bands_hz : null;
		return this.synthesis?.derived.bands_hz ?? fromCheck ?? this.preview?.derived.bands_hz ?? null;
	}

	readonly local = $derived.by((): FieldErrors => mergeErrors(localErrors(this.setup, this.acq.rows), lawParamErrors(this.law, this.setup.law.params)));

	/** Findings by key: the editor's own and the dry run's for the draft as it stands. */
	get fieldErrors(): FieldErrors {
		const fromCheck = this.check && this.checkedKey === this.key ? srsByKey(this.check.errors) : {};
		const lawMissing: FieldErrors = this.laws && !this.law ? { 'law.id': ['The chosen law is not in the catalog; choose another.'] } : {};
		return mergeErrors(mergeErrors(this.local, fromCheck), lawMissing);
	}

	get fieldWarnings(): FieldErrors {
		return this.check && this.checkedKey === this.key ? srsByKey(this.check.warnings) : {};
	}

	/** Errors and warnings per section. */
	get counts(): Record<SrsSection, { errors: number; warnings: number }> {
		const out = Object.fromEntries(SRS_SECTIONS.map((s) => [s.id, { errors: 0, warnings: 0 }])) as Record<SrsSection, { errors: number; warnings: number }>;
		for (const [k, v] of Object.entries(this.fieldErrors)) out[srsSectionOf(k)].errors += v.length;
		for (const [k, v] of Object.entries(this.fieldWarnings)) out[srsSectionOf(k)].warnings += v.length;
		out.station.errors += this.acq.tableErrorCount;
		return out;
	}

	get setupErrorCount(): number {
		return Object.values(this.counts).reduce((n, c) => n + c.errors, 0);
	}

	get setupStatus(): SetupStatus {
		const s = this.status;
		if (!s || s.state === 'unconfigured') return 'none';
		const held = this.held;
		if (!held || held.runId !== s.run_id) return 'unread';
		return held.key === this.key ? 'applied' : 'changed';
	}

	/** Why Apply and Start wait, in words, or null. */
	get pending(): string | null {
		if (this.acq.stationDirty) return 'Use the chosen station, or put the choice back, first.';
		if (!this.laws) return 'The law catalog has not been read yet.';
		return null;
	}

	/** The configuration the run holds, as the service has it. */
	get runConfig(): SrsConfig | null {
		return this.served?.config ?? null;
	}

	private get verbContext(): SrsVerbContext {
		return {
			connected: this.connected,
			controller: this.isController,
			// the latest numbers: a live frame counts the time to the next pulse down between state events
			run: this.now,
			setup: this.setupStatus,
			setupErrors: this.setupErrorCount,
			busy: this.busy,
			pending: this.pending
		};
	}

	/** The run bar's verbs, by the shared bar's names. */
	get actions(): Record<ActionId, ActionState> {
		return srsBarActions(this.verbContext);
	}

	get primary(): ActionId | null {
		return srsPrimary(this.verbContext);
	}

	/** The manual ladder's verbs: Fire, the level steps, Go to a level. */
	get verbs(): Record<ShockVerbId, ActionState> {
		return srsModuleVerbs(this.verbContext);
	}

	/** The pulses the run's schedule plans. */
	get planned(): number {
		const c = this.runConfig ?? this.wire;
		return plannedPulses(c.schedule.pulses, c.schedule.levels_db.length);
	}

	/** The latest numbers: the live frame's where it is for this run, else the state's. */
	get now(): SrsState | null {
		const s = this.status;
		const l = this.live;
		if (!s || !l || l.run_id !== s.run_id) return s;
		return { ...s, phase: l.phase, state: l.state, level_db: l.level_db, rung: l.rung, pulses: l.pulses, next_pulse_s: l.next_pulse_s };
	}

	// ---- the run as the shared run bar and notices read it ---------------------------------

	get run(): RunHead | null {
		const s = this.now;
		if (!s) return null;
		return {
			run_id: s.run_id,
			state: s.state,
			phase: s.phase,
			title: s.title,
			mode: s.mode,
			levels_db: s.levels_db,
			level_db: s.level_db,
			at_level_s: null,
			remaining_at_level_s: null,
			end: s.end as unknown as RunHead['end'],
			record: s.record ?? null
		};
	}

	/** SRS shock sends no live head the bar reads; the bar reads the state. */
	readonly frame = null;
	/** The run bar's words for SRS shock's outcomes and phases. */
	readonly outcomeWords: Record<string, string> = SRS_OUTCOME_WORDS;
	readonly phaseWords: Record<string, string> = SRS_PHASE_WORDS;

	get elapsed(): number | null {
		return this.status?.elapsed_s ?? null;
	}

	get source(): { source: 'SIM' | 'HARDWARE'; cls: 'sim' | 'ni_simulated' | 'ni_real' } | null {
		const s = this.status;
		if (s?.source && s.station) return { source: s.source, cls: s.station.class };
		return this.acq.source;
	}

	get session() {
		return this.acq.session;
	}

	get orphanDeadline() {
		return this.acq.orphanDeadline;
	}

	get refusal(): Refusal | null {
		return this.#refusal ?? this.acq.refusal;
	}

	set refusal(r: Refusal | null) {
		this.#refusal = r;
	}

	get note() {
		return this.acq.note;
	}

	claim() {
		return this.acq.claim();
	}

	release() {
		return this.acq.release();
	}

	/** The shared bar's level and mode verbs are not offered: the module's verbs steer the level. */
	setMode() {}
	goTo() {}

	dismissRefusal() {
		this.#refusal = null;
		this.acq.dismissRefusal();
	}

	dismissNote() {
		this.acq.dismissNote();
	}

	// =========================================================================================
	// Wiring

	/** Keep the draft across reloads, and preview and re-check it as it changes. Returns the stopper. */
	watch(): () => void {
		return $effect.root(() => {
			$effect(() => {
				const text = saveSrsSetup($state.snapshot(this.setup) as SrsDraft);
				untrack(() => write(this.localStore, SETUP_KEY, text));
			});
			$effect(() => {
				const key = this.previewInputKey;
				untrack(() => {
					if (this.drafts && key && key !== this.previewKey) this.schedulePreview();
				});
			});
			$effect(() => {
				const key = this.key;
				untrack(() => {
					if (this.drafts && key && key !== this.checkedKey) this.scheduleCheck();
				});
			});
		});
	}

	/** Read what the service holds: the run, its configuration and pulses, the law catalog. */
	async sync(): Promise<void> {
		const c = this.control;
		try {
			const [status, served, laws] = await Promise.all([
				c.request<SrsState>('srs.state'),
				c.request<SrsConfigResult>('srs.config'),
				c.request<LawsListResult>('laws.list', { world: 'shock_srs' })
			]);
			this.syncError = null;
			this.served = served;
			this.onStatus(status);
			this.setLaws(laws);
		} catch (err) {
			this.syncError = toRefusal('Read the control service', err).message;
			return;
		}
		if (!this.drafts) return;
		this.schedulePreview(0);
		this.scheduleCheck(0);
	}

	private onStatus(s: SrsState) {
		const was = this.status;
		// A reply older than the run's own word that it finished is not applied.
		if (regresses(was, s)) return;
		this.status = s;
		// The state is newer than any live frame held: the next frame takes over from it.
		if (this.live && this.live.run_id === s.run_id) this.live = null;
		if (s.run_id !== this.pulsesRun) void this.loadPulses(s.run_id);
		else if (s.last_pulse && !this.pulses.some((p) => p.index === s.last_pulse!.index)) this.pulses = [...this.pulses, s.last_pulse];
		if (was?.run_id !== s.run_id && s.run_id) void this.readServed();
		// The run went live from this screen or another: the run is the next thing to watch.
		if (srsIsLive(s) && !srsIsLive(was) && this.step === 'setup' && s.state !== 'armed') this.step = 'run';
	}

	private onPulse(e: SrsPulseEvent) {
		if (this.status?.run_id && e.run_id !== this.status.run_id) return;
		if (!this.pulses.some((p) => p.index === e.pulse.index)) this.pulses = [...this.pulses, e.pulse].sort((a, b) => a.index - b.index);
		this.lastViews = e.views;
	}

	/** The run's pulse rows from the start, after a new run or a reconnection. */
	async loadPulses(runId: string | null): Promise<void> {
		this.pulsesRun = runId;
		this.overlay = [];
		this.overlayViews = {};
		this.lastViews = [];
		if (!runId) {
			this.pulses = [];
			this.log = [];
			return;
		}
		try {
			const [res, log] = await Promise.all([this.control.request<SrsPulsesResult>('srs.pulses', { since: 0 }), this.control.request<SrsLogResult>('srs.log', { since: 0 })]);
			if (this.pulsesRun !== runId) return;
			const have = new Map(this.pulses.map((p) => [p.index, p]));
			for (const p of res.pulses) have.set(p.index, p);
			this.pulses = [...have.values()].sort((a, b) => a.index - b.index);
			this.log = log.entries;
		} catch {
			/* the rows come with each pulse's event */
		}
	}

	private async readServed(): Promise<void> {
		try {
			this.served = await this.control.request<SrsConfigResult>('srs.config');
		} catch {
			/* kept as it was */
		}
	}

	private setLaws(res: LawsListResult) {
		this.laws = res.laws;
		this.lawProblems = res.problems;
		const law = this.law;
		if (law) this.setup.law.params = lawValues(law, this.setup.law.params, 1);
	}

	/** Choose a law: its parameters start from the operator's last valid values, else its defaults. */
	chooseLaw(id: string) {
		const law = this.srsLaws.find((l) => l.id === id);
		if (!law || this.locked) return;
		this.setup.law = { id, params: lawValues(law, id === this.setup.law.id ? this.setup.law.params : undefined, 1) };
	}

	async refreshLaws(): Promise<void> {
		try {
			this.setLaws(await this.control.request<LawsListResult>('laws.list', { world: 'shock_srs' }));
		} catch (err) {
			this.refusal = toRefusal('Read the control laws', err);
		}
	}

	/** Import a law file by its path on the computer that runs the test. */
	async importLaw(path: string): Promise<boolean> {
		const res = await this.act<{ law: LawEntry }>('Import Law', 'laws.import', { path: path.trim() });
		if (!res) return false;
		await this.refreshLaws();
		if (res.law.world === 'shock_srs') {
			this.chooseLaw(res.law.id);
			this.acq.note = { tone: 'pass', text: `Imported ${res.law.name} ${res.law.version}.` };
		} else this.acq.note = { tone: 'warn', text: `${res.law.name} is not an SRS shock law; it was imported for its own test type.` };
		return true;
	}

	/** Start the draft from the service's configuration (the one the run holds). */
	adoptServed(): boolean {
		const c = this.served?.config;
		if (!c || this.locked) return false;
		this.setup = fromSrsConfig(c);
		return true;
	}

	// =========================================================================================
	// Tables read from files, and the per-band lists

	/**
	 * Read a target from a breakpoint table on the computer that runs the test. The service reads
	 * it and says what it found; the draft takes the target only when it read cleanly.
	 */
	async importTarget(path: string): Promise<boolean> {
		if (this.locked) return false;
		const p = path.trim();
		const res = await this.act<SrsTargetImportResult>('Read Target', 'srs.target.import', { path: p });
		if (!res) return false;
		this.imported = { what: 'target', path: p, errors: res.errors, warnings: res.warnings };
		if (!res.target) return false;
		const t = res.target;
		if (t.kind === 'flat') this.setup.target = { ...this.setup.target, kind: 'flat', accel_g: t.accel_g };
		else this.setup.target = { ...this.setup.target, kind: 'breakpoints', points: t.points.map(([freq_hz, accel_g]) => ({ freq_hz, accel_g })) };
		return true;
	}

	/** Read a component table for the draft's engine, as `importTarget` reads a target. */
	async importTable(path: string): Promise<boolean> {
		if (this.locked) return false;
		const p = path.trim();
		const engine = this.setup.synthesis.engine;
		const res = await this.act<SrsTableImportResult>('Read Component Table', 'srs.table.import', { path: p, engine });
		if (!res) return false;
		this.imported = { what: 'table', path: p, errors: res.errors, warnings: res.warnings };
		const c = res.components;
		if (!c) return false;
		this.setup.synthesis.engine = c.engine;
		if (c.engine === 'wavsyn') this.setup.synthesis.wavelets = c.rows.map((r) => ({ ...r }));
		else this.setup.synthesis.sines = c.rows.map((r) => ({ ...r }));
		this.setup.synthesis.table = true;
		return true;
	}

	/** Turn the per-band envelope on (filled from the uniform one on the service's band grid) or off. */
	setPerBandEnvelope(on: boolean): void {
		if (this.locked) return;
		const e = this.setup.envelope;
		if (on) {
			const n = this.bandsHz?.length ?? 0;
			e.bands = fitToBands(e.bands, n, () => ({ alarm_db: [e.alarm_db[0], e.alarm_db[1]], abort_db: [e.abort_db[0], e.abort_db[1]] }));
		}
		e.per_band = on;
	}

	/** A limit channel's bound: flat, or one per band (filled from the flat bound on the service's grid). */
	setBoundKind(i: number, kind: 'flat' | 'per_band'): void {
		const ch = this.setup.limit_channels.channels[i];
		if (!ch || this.locked) return;
		if (kind === 'per_band') {
			const n = this.bandsHz?.length ?? 0;
			const flat = Number.isFinite(ch.bound.accel_g) ? ch.bound.accel_g : null;
			ch.bound.bounds_g = fitToBands(ch.bound.bounds_g, n, () => flat);
		}
		ch.bound.kind = kind;
	}

	/** Fit every per-band list the draft holds to the band grid the service now gives. */
	fitPerBand(): boolean {
		const n = this.bandsHz?.length ?? 0;
		if (!n || this.locked) return false;
		let changed = false;
		const e = this.setup.envelope;
		if (e.per_band && e.bands.length !== n) {
			e.bands = fitToBands(e.bands, n, () => ({ alarm_db: [e.alarm_db[0], e.alarm_db[1]], abort_db: [e.abort_db[0], e.abort_db[1]] }));
			changed = true;
		}
		for (const ch of this.setup.limit_channels.channels) {
			if (ch.bound.kind === 'per_band' && ch.bound.bounds_g.length !== n) {
				const flat = Number.isFinite(ch.bound.accel_g) ? ch.bound.accel_g : null;
				ch.bound.bounds_g = fitToBands(ch.bound.bounds_g, n, () => flat);
				changed = true;
			}
		}
		return changed;
	}

	// =========================================================================================
	// The synthesis and the dry run

	schedulePreview(delay = this.checkDelayMs) {
		if (this.previewTimer) clearTimeout(this.previewTimer);
		this.previewTimer = setTimeout(() => void this.refreshPreview(), delay);
	}

	/** The service synthesizes the target: the band grid, the target on it, the pulse and its kinematics. */
	async refreshPreview(): Promise<SrsPreviewResult | null> {
		if (this.previewTimer) clearTimeout(this.previewTimer);
		this.previewTimer = null;
		const params = this.previewInput;
		if (!this.connected || !params) return null;
		const key = this.previewInputKey;
		const seq = ++this.previewSeq;
		this.previewBusy = true;
		try {
			const res = await this.control.request<SrsPreviewResult>('srs.preview', params as unknown as Record<string, unknown>);
			if (seq === this.previewSeq) {
				this.preview = res;
				this.previewKey = key;
				this.previewError = null;
			}
			return res;
		} catch (err) {
			// A target the service will not synthesize is named by the dry run, on its field.
			if (seq === this.previewSeq) {
				this.preview = null;
				this.previewKey = key;
				this.previewError = toRefusal('Synthesize', err).message;
			}
			return null;
		} finally {
			if (seq === this.previewSeq) this.previewBusy = false;
		}
	}

	scheduleCheck(delay = this.checkDelayMs) {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = setTimeout(() => void this.checkSetup(), delay);
	}

	/** The service's verdict on the draft (`dry_run`: needs no control, replaces nothing). */
	async checkSetup(): Promise<SrsDryRunResult | null> {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = null;
		if (!this.connected || errorCount(this.local) > 0) return null;
		const config = this.wire;
		const key = this.key;
		const seq = ++this.checkSeq;
		this.checking = true;
		try {
			const res = await this.control.request<SrsDryRunResult>('srs.configure', { config, dry_run: true });
			if (seq === this.checkSeq) {
				this.check = res;
				this.checkedKey = key;
			}
			return res;
		} catch (err) {
			// A configuration whose shape is wrong is refused by field; show it as a finding there.
			const r = toRefusal('Check the setup', err);
			if (seq === this.checkSeq && r.reason === 'bad_params') {
				const field = typeof r.detail?.param === 'string' ? r.detail.param : 'title';
				this.check = { errors: [{ reason: 'out_of_range', field, message: r.message }], warnings: [], derived: null, views: [] };
				this.checkedKey = key;
			}
			return null;
		} finally {
			if (seq === this.checkSeq) this.checking = false;
		}
	}

	// =========================================================================================
	// The run

	private setHeld(h: { runId: string | null; key: string } | null) {
		this.held = h;
		write(this.localStore, HELD_KEY, h ? JSON.stringify(h) : null);
	}

	/** Send the setup. A table with edits is applied first, since the run uses its rows. */
	async apply(): Promise<boolean> {
		if (!this.actions.apply.enabled) return false;
		if (this.acq.tableDirty) {
			const ok = await this.acq.applyTable();
			if (!ok) return false;
		}
		const config = this.wire;
		const key = this.key;
		const res = await this.act<SrsConfigureResult>('Apply Setup', 'srs.configure', { config });
		if (!res) {
			const r = this.refusal;
			if (r?.reason === 'config_invalid' && r.detail) {
				const d = r.detail as unknown as { errors?: SrsDryRunResult['errors']; warnings?: SrsDryRunResult['warnings'] };
				this.check = { errors: d.errors ?? [], warnings: d.warnings ?? [], derived: null, views: [] };
				this.checkedKey = key;
			}
			return false;
		}
		this.status = res.state;
		this.setHeld({ runId: res.state.run_id, key });
		this.served = { run_id: res.state.run_id, config, derived: res.derived, views: [] };
		void this.loadPulses(res.state.run_id);
		void this.readServed();
		this.acq.note = { tone: 'pass', text: 'The service holds this setup.' };
		return true;
	}

	/** Arm: the station opens and gates; nothing is driven. With words, past a feasibility block. */
	async arm(ack?: string): Promise<boolean> {
		const res = await this.act<SrsState>(ack ? 'Arm Past Feasibility' : 'Arm', 'srs.arm', ack ? { override_ack: ack } : {});
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** Identification first, then the review hold or the first pulse. */
	async start(): Promise<boolean> {
		const res = await this.act<SrsState>('Start', 'srs.start');
		if (res) {
			this.onStatus(res);
			this.step = 'run';
		}
		return res !== null;
	}

	/** Seed the law after the review, plan the limit notch, and go on with the schedule. */
	async proceed(): Promise<boolean> {
		const res = await this.act<SrsState>('Proceed', 'srs.proceed');
		if (res) this.onStatus(res);
		return res !== null;
	}

	async fire(): Promise<boolean> {
		const res = await this.act<SrsState>('Fire', 'srs.fire');
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** Step the level 1 dB down or up, manual ladder only. */
	async level(deltaDb: number): Promise<number | null> {
		const res = await this.act<SrsLevelResult>('Level', 'srs.level', { delta_db: deltaDb > 0 ? 1 : -1 });
		return res?.level_db ?? null;
	}

	/** Go to a level of the table, by its place from 0. */
	async toRung(rung: number): Promise<number | null> {
		const res = await this.act<SrsLevelResult>('Go to Level', 'srs.level', { rung });
		return res?.level_db ?? null;
	}

	/** SRS shock fires no trains and no bursts; the verbs are never offered. */
	train(): boolean {
		return false;
	}

	burst(): boolean {
		return false;
	}

	async stop(): Promise<boolean> {
		const res = await this.act<SrsState>('Stop', 'srs.stop', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	async abort(): Promise<boolean> {
		const res = await this.act<SrsState>('Abort', 'srs.abort', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** A new run from the same configuration. */
	async reset(): Promise<boolean> {
		const res = await this.act<SrsState>('New Run', 'srs.reset');
		if (!res) return false;
		this.onStatus(res);
		if (this.held && this.setupStatus !== 'changed') this.setHeld({ runId: res.run_id, key: this.key });
		return true;
	}

	// =========================================================================================
	// Pulses laid over the plots

	/** Pick a pulse to lay over the SRS and Pulse pages, or take it off. */
	async toggleOverlay(index: number): Promise<void> {
		const next = toggleOverlay(this.overlay, index, MAX_OVERLAY);
		this.overlay = next;
		const views: Record<number, View[]> = {};
		for (const i of next) if (this.overlayViews[i]) views[i] = this.overlayViews[i];
		this.overlayViews = views;
		if (!next.includes(index) || this.overlayViews[index]) return;
		try {
			const res = await this.control.request<SrsPulseResult>('srs.pulse', { index, points: 1024 });
			if (this.overlay.includes(index)) this.overlayViews = { ...this.overlayViews, [index]: res.views };
		} catch (err) {
			this.refusal = toRefusal('Show the pulse', err);
			this.overlay = this.overlay.filter((i) => i !== index);
		}
	}

	/** The picked pulses with their views, in the order they were picked. */
	get overlaid(): { index: number; views: View[] }[] {
		return this.overlay.filter((i) => this.overlayViews[i]).map((i) => ({ index: i, views: this.overlayViews[i] }));
	}

	private async act<R>(verb: string, method: string, params: Record<string, unknown> = {}, exclusive = true): Promise<R | null> {
		if (exclusive) this.busy = verb;
		this.#refusal = null;
		try {
			return await this.control.request<R>(method, params);
		} catch (err) {
			this.#refusal = toRefusal(verb, err);
			return null;
		} finally {
			if (exclusive && this.busy === verb) this.busy = null;
		}
	}
}
