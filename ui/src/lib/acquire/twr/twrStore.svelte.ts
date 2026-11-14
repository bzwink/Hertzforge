/**
 * Time waveform replication's (TWR's) working state, against the control service's `twr.*` and
 * `laws.*` methods (schema/PROTOCOL.md, "Time Waveform Replication", "Control Laws").
 *
 * The station, the channel table and the controller seat are the service's and shared by every
 * test type: they stay in the Acquire store, and this one reads them from there. What is TWR's own
 * is here: the setup draft (kept across reloads), the reference imported by path or pasted text
 * (`twr.reference.import`) and the service's preparation of it (`twr.reference.preview`), the dry
 * run of the setup, the law catalog of the TWR world, the run's state, its live numbers and its play
 * rows, one play's records on request, and the verbs. As everywhere in Acquire, the service holds
 * the truth and computes every number; this store keeps the operator's edits beside it and sends
 * them when the operator says so.
 */

import { regresses } from '../runOrder';
import { untrack } from 'svelte';
import type { ServiceClient } from '$lib/protocol/client.svelte';
import type { ChannelRow, LogEntry, RunRecordRef, View } from '$lib/protocol/types';
import type { LawEntry, LawsListResult } from '$lib/protocol/mimo';
import {
	TWR_TOPICS,
	type TwrConfig,
	type TwrConfigResult,
	type TwrConfigureResult,
	type TwrData,
	type TwrDryRunResult,
	type TwrFreezeResult,
	type TwrLastMetrics,
	type TwrLevelResult,
	type TwrLive,
	type TwrLogEvent,
	type TwrLogResult,
	type TwrPlayInFlight,
	type TwrPlayRow,
	type TwrPlaysResult,
	type TwrRecordResult,
	type TwrReferenceFacts,
	type TwrReferenceImportParams,
	type TwrReferenceImportResult,
	type TwrReferencePreviewResult,
	type TwrReportItem,
	type TwrState
} from '$lib/protocol/twr';
import type { ActionId, ActionState, RunHead, SetupStatus } from '../actions';
import { lawParamErrors, lawsOfWorld, lawValues } from '../law/lawForm';
import { canonical } from '../mimo/mimoSetup';
import { errorCount, mergeErrors, type FieldErrors } from '../setup';
import type { AcquireState } from '../store.svelte';
import { toRefusal, type Refusal } from '../words';
import {
	controlUnit,
	defaultTwrSetup,
	fromTwrConfig,
	localErrors,
	mergeTwrSetup,
	previewParams,
	toTwrConfig,
	TWR_SECTIONS,
	twrByKey,
	twrSectionOf,
	type TwrDraft,
	type TwrSetupSection
} from './twrSetup';
import { twrBarActions, twrIsLive, twrModuleVerbs, twrPrimary, type TwrVerbContext, type TwrVerbId } from './twrVerbs';
import { TWR_OUTCOME_WORDS, TWR_PHASE_WORDS, TWR_STATE_WORDS } from './twrWords';

const SETUP_KEY = 'hertzforge.acquire.twr.setup.v1';
/** The setup this screen applied, by run: `{runId, key}`. */
const HELD_KEY = 'hertzforge.acquire.twr.held.v1';
/** Most log entries kept on screen; the service keeps the full log with the run. */
const LOG_LIMIT = 500;
/** The points one play's records are asked at when the operator opens them. */
export const RECORD_POINTS = 2048;

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

export function saveTwrSetup(d: TwrDraft): string {
	return JSON.stringify(d, nanOut);
}

export function loadTwrSetup(raw: string | null): TwrDraft {
	if (!raw) return defaultTwrSetup();
	try {
		return mergeTwrSetup(JSON.parse(raw, nanIn) as Partial<TwrDraft>);
	} catch {
		return defaultTwrSetup();
	}
}

export interface TwrOptions {
	local?: Storage | null;
	/** Delay before a dry run or a preview after an edit, ms. */
	checkDelayMs?: number;
	/** A display window observes: it keeps no draft and checks nothing. */
	drafts?: boolean;
}

/** What the last import of a reference said, for the reference editor. */
export interface ReferenceImportNote {
	/** The file read, or null for pasted text. */
	path: string | null;
	facts: TwrReferenceFacts;
	warnings: TwrReportItem[];
	/** The Reference view the service drew. */
	views: View[];
}

/** The numbers every frame carries: `twr.live` and `twr.data` alike. */
export type TwrFrame = Omit<TwrLive, 'views'> & Partial<Pick<TwrLive, 'views'>> & Partial<Pick<TwrData, 'plots'>>;

/** The run as the screen reads it now: the state, with the latest frame over it. */
export type TwrNow = TwrState & {
	/** The play in flight; null between plays. */
	play: TwrPlayInFlight | null;
	/** The last play's metrics; null before any. */
	last: TwrLastMetrics | null;
};

export class TwrStore {
	readonly control: ServiceClient;
	private readonly localStore: Storage | null;
	private readonly checkDelayMs: number;
	private readonly drafts: boolean;

	// ---- the operator's draft --------------------------------------------------------------
	setup = $state<TwrDraft>(defaultTwrSetup());
	section = $state<TwrSetupSection>('reference');
	/** Where the operator is: the setup, or the run. */
	step = $state<'setup' | 'run'>('setup');

	// ---- from the service ------------------------------------------------------------------
	status = $state.raw<TwrState | null>(null);
	/**
	 * The run's numbers from the last frame: `twr.live`, or this screen's own `twr.data` once it has
	 * subscribed to plots. Whichever came last is read.
	 */
	live = $state.raw<TwrFrame | null>(null);
	/** What the service holds: the configuration of the present run. */
	served = $state.raw<TwrConfigResult | null>(null);
	/** The dry run of the draft's setup, and the key of the setup it checked. */
	check = $state.raw<TwrDryRunResult | null>(null);
	checkedKey = $state('');
	checking = $state(false);
	/** The service's preparation of the draft's reference, and the key of what it prepared. */
	preview = $state.raw<TwrReferencePreviewResult | null>(null);
	previewKey = $state('');
	previewBusy = $state(false);
	/** Why the service would not prepare the reference as it stands, in its words. */
	previewError = $state<string | null>(null);
	/** The last reference imported, and what the service said of it. */
	imported = $state.raw<ReferenceImportNote | null>(null);
	laws = $state.raw<LawEntry[] | null>(null);
	lawProblems = $state.raw<{ file: string; reason: string }[]>([]);
	/** The run's plays, in order. */
	plays = $state.raw<TwrPlayRow[]>([]);
	/** One play's records, opened from the plays table: its row and its views. */
	record = $state.raw<{ index: number; views: View[] } | null>(null);
	recordBusy = $state<number | null>(null);
	log = $state.raw<LogEntry[]>([]);
	/** The setup this screen applied: the run it made and the setup's key. */
	held = $state.raw<{ runId: string | null; key: string } | null>(null);

	// ---- feedback --------------------------------------------------------------------------
	busy = $state<string | null>(null);
	#refusal = $state.raw<Refusal | null>(null);
	syncError = $state<string | null>(null);

	/** Which came last for this run: a frame (true) or the state. */
	private liveNewer = $state(false);
	private checkSeq = 0;
	private previewSeq = 0;
	private playsRun: string | null = null;
	private checkTimer: ReturnType<typeof setTimeout> | null = null;
	private previewTimer: ReturnType<typeof setTimeout> | null = null;

	constructor(
		readonly acq: AcquireState,
		options: TwrOptions = {}
	) {
		this.control = acq.control;
		this.localStore = options.local === undefined ? storageOf('localStorage') : options.local;
		this.checkDelayMs = options.checkDelayMs ?? 300;
		this.drafts = options.drafts ?? true;
		this.setup = loadTwrSetup(read(this.localStore, SETUP_KEY));
		const held = read(this.localStore, HELD_KEY);
		if (held) {
			try {
				this.held = JSON.parse(held);
			} catch {
				this.held = null;
			}
		}
		this.control.on(TWR_TOPICS.state, (d) => this.onStatus(d as unknown as TwrState));
		const onFrame = (d: Record<string, unknown>) => {
			const f = d as unknown as TwrFrame;
			if (this.status?.run_id && f.run_id !== this.status.run_id) return;
			this.live = f;
			this.liveNewer = true;
		};
		this.control.on(TWR_TOPICS.live, onFrame);
		this.control.on(TWR_TOPICS.data, onFrame);
		this.control.on(TWR_TOPICS.log, (d) => {
			const e = d as unknown as TwrLogEvent;
			if (this.status?.run_id && e.run_id !== this.status.run_id) return;
			const have = new Set(this.log.map((x) => x.seq));
			this.log = [...this.log, ...e.entries.filter((x) => !have.has(x.seq))].slice(-LOG_LIMIT);
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

	get isLive(): boolean {
		return twrIsLive(this.status);
	}

	/** Setup, station and table are locked while a run is live. */
	get locked(): boolean {
		return this.isLive || this.acq.isLive;
	}

	/** The TWR world's laws: built in first, then imported, as the service lists them. */
	get twrLaws(): LawEntry[] {
		return lawsOfWorld(this.laws, 'twr');
	}

	/** The law chosen, from the catalog. */
	get law(): LawEntry | null {
		return this.twrLaws.find((l) => l.id === this.setup.law.id) ?? null;
	}

	/** The unit the reference must be in: the control row's. */
	get unit(): string {
		return controlUnit(this.acq.rows);
	}

	readonly wire = $derived<TwrConfig>(toTwrConfig(this.setup, this.law));
	readonly key = $derived(canonical(this.wire));
	readonly previewInput = $derived(previewParams(this.setup));
	readonly previewInputKey = $derived(this.previewInput ? canonical(this.previewInput) : '');

	/** The service's preparation, when it answered the draft's reference as it stands. */
	get prepared(): TwrReferencePreviewResult | null {
		return this.preview && this.previewKey === this.previewInputKey ? this.preview : null;
	}

	readonly local = $derived.by((): FieldErrors => mergeErrors(localErrors(this.setup), lawParamErrors(this.law, this.setup.law.params)));

	/** Findings by key: the editor's own and the dry run's for the draft as it stands. */
	get fieldErrors(): FieldErrors {
		const fromCheck = this.check && this.checkedKey === this.key ? twrByKey(this.check.errors) : {};
		const lawMissing: FieldErrors = this.laws && !this.law ? { 'law.id': ['The chosen law is not in the catalog; choose another.'] } : {};
		return mergeErrors(mergeErrors(this.local, fromCheck), lawMissing);
	}

	get fieldWarnings(): FieldErrors {
		return this.check && this.checkedKey === this.key ? twrByKey(this.check.warnings) : {};
	}

	/** Errors and warnings per section. */
	get counts(): Record<TwrSetupSection, { errors: number; warnings: number }> {
		const out = Object.fromEntries(TWR_SECTIONS.map((s) => [s.id, { errors: 0, warnings: 0 }])) as Record<TwrSetupSection, { errors: number; warnings: number }>;
		for (const [k, v] of Object.entries(this.fieldErrors)) out[twrSectionOf(k)].errors += v.length;
		for (const [k, v] of Object.entries(this.fieldWarnings)) out[twrSectionOf(k)].warnings += v.length;
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
	get runConfig(): TwrConfig | null {
		return this.served?.config ?? null;
	}

	/** The last frame's numbers, when they are this run's. */
	get frameOfRun(): TwrFrame | null {
		const s = this.status;
		const l = this.live;
		return s && l && l.run_id === s.run_id ? l : null;
	}

	/**
	 * The latest numbers: the state, with this run's frame laid over it when the frame came after
	 * it. A state that came after the frame (a change of phase, a play judged) is the run's word
	 * until the next frame; the play in flight and the last metrics are the frame's while it is.
	 */
	get now(): TwrNow | null {
		const s = this.status;
		if (!s) return null;
		const fromState = (): TwrLastMetrics | null => {
			const p = s.last_play;
			return p ? { rms_error_pct: p.rms_error_pct, peak_error_pct: p.peak_error_pct, spectral_error_db: p.spectral_error_db, gated: p.gated } : null;
		};
		const l = this.frameOfRun;
		// A state newer than the frame keeps the frame's play in flight while the state still plays.
		const inFlight = (s.phase === 'iterating' || s.phase === 'playing' || s.state === 'stopping') && l?.play ? l.play : null;
		if (!l || !this.liveNewer) return { ...s, play: inFlight, last: fromState() };
		return {
			...s,
			state: l.state,
			phase: l.phase,
			mode: l.mode ?? s.mode,
			t_s: l.t_s,
			level_db: l.level_db,
			rung: l.rung,
			plays: l.plays,
			play: l.play,
			last: l.last ?? fromState()
		};
	}

	private get verbContext(): TwrVerbContext {
		return {
			connected: this.connected,
			controller: this.isController,
			run: this.now,
			setup: this.setupStatus,
			setupErrors: this.setupErrorCount,
			busy: this.busy,
			pending: this.pending
		};
	}

	/** The run bar's verbs, by the shared bar's names. */
	get actions(): Record<ActionId, ActionState> {
		return twrBarActions(this.verbContext);
	}

	get primary(): ActionId | null {
		return twrPrimary(this.verbContext);
	}

	/** Iterate, Play, the level steps, Go to a level, Freeze and Resume. */
	get verbs(): Record<TwrVerbId, ActionState> {
		return twrModuleVerbs(this.verbContext);
	}

	/** The run's law: the one the service runs, else the draft's choice. */
	get runLaw(): LawEntry | null {
		const id = this.status?.law?.id ?? this.runConfig?.control?.law?.id ?? null;
		return (id ? this.twrLaws.find((l) => l.id === id) : null) ?? this.law;
	}

	/** The gate the run holds each play to, percent. */
	get gatePct(): number | null {
		return this.runConfig?.schedule?.gate_pct ?? (this.setupStatus === 'applied' ? this.setup.schedule.gate_pct : null);
	}

	// ---- the run as the shared run bar and notices read it ---------------------------------

	get run(): RunHead | null {
		const s = this.now;
		if (!s) return null;
		return {
			run_id: s.run_id,
			// The coherence hold is TWR's own state; the bar reads it through `stateWords`.
			state: s.state as RunHead['state'],
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

	/** The channels the run's streams would hold: every enabled input and the drive. */
	get recordChannels(): number {
		return this.rows.filter((r) => r.enabled).length;
	}

	/** The sample rate the run asks for (the reference's own when none is given), for the size its streams take. */
	get recordRateHz(): number {
		return this.setup.acquisition.fs_hz ?? this.setup.facts?.fs_hz ?? Number.NaN;
	}

	/** The run's record, from arm on: its folder, whether it keeps the time histories, its problem. */
	get runRecord(): RunRecordRef | null {
		return this.status?.record ?? null;
	}

	/** TWR sends no level the drive ramps through; the bar reads the state's commanded level. */
	readonly frame = null;
	/** The run bar's words for TWR's outcomes, states and phases. */
	readonly outcomeWords: Record<string, string> = TWR_OUTCOME_WORDS;
	readonly stateWords: Record<string, string> = TWR_STATE_WORDS;
	readonly phaseWords: Record<string, string> = TWR_PHASE_WORDS;

	/** Station time since the run armed. */
	get elapsed(): number | null {
		return this.now?.t_s ?? null;
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

	/** Keep the draft across reloads, and prepare and re-check it as it changes. Returns the stopper. */
	watch(): () => void {
		return $effect.root(() => {
			$effect(() => {
				const text = saveTwrSetup($state.snapshot(this.setup) as TwrDraft);
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

	/** Read what the service holds: the run, its configuration and plays, the law catalog. */
	async sync(): Promise<void> {
		const c = this.control;
		try {
			const [status, served, laws] = await Promise.all([c.request<TwrState>('twr.state'), c.request<TwrConfigResult>('twr.config'), c.request<LawsListResult>('laws.list', { world: 'twr' })]);
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

	private onStatus(s: TwrState) {
		const was = this.status;
		// A reply older than the run's own word that it finished is not applied.
		if (regresses(was, s)) return;
		this.status = s;
		// The state is newer than the frame held: it is the run's word until the next frame.
		this.liveNewer = false;
		if (this.live && this.live.run_id === s.run_id && (s.state === 'unconfigured' || s.state === 'configured' || s.state === 'blocked')) this.live = null;
		if (s.run_id !== this.playsRun) void this.loadPlays(s.run_id);
		else if (s.last_play && !this.plays.some((p) => p.index === s.last_play!.index)) this.plays = [...this.plays, s.last_play].sort((a, b) => a.index - b.index);
		if (was?.run_id !== s.run_id && s.run_id) void this.readServed();
		// The run went live from this screen or another: the run is the next thing to watch.
		if (twrIsLive(s) && !twrIsLive(was) && this.step === 'setup' && s.state !== 'armed') this.step = 'run';
	}

	/** The run's play rows and log from the start, after a new run or a reconnection. */
	async loadPlays(runId: string | null): Promise<void> {
		this.playsRun = runId;
		this.record = null;
		if (!runId) {
			this.plays = [];
			this.log = [];
			this.live = null;
			return;
		}
		try {
			const [res, log] = await Promise.all([this.control.request<TwrPlaysResult>('twr.plays', { since: 0 }), this.control.request<TwrLogResult>('twr.log', { since: 0 })]);
			if (this.playsRun !== runId) return;
			const have = new Map(this.plays.map((p) => [p.index, p]));
			for (const p of res.plays) have.set(p.index, p);
			this.plays = [...have.values()].sort((a, b) => a.index - b.index);
			const seen = new Map(this.log.map((e) => [e.seq, e]));
			for (const e of log.entries) seen.set(e.seq, e);
			this.log = [...seen.values()].sort((a, b) => a.seq - b.seq).slice(-LOG_LIMIT);
		} catch {
			/* the rows come with each state, the entries with each log event */
		}
	}

	private async readServed(): Promise<void> {
		try {
			this.served = await this.control.request<TwrConfigResult>('twr.config');
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
		const law = this.twrLaws.find((l) => l.id === id);
		if (!law || this.locked) return;
		this.setup.law = { id, params: lawValues(law, id === this.setup.law.id ? this.setup.law.params : undefined, 1) };
	}

	async refreshLaws(): Promise<void> {
		try {
			this.setLaws(await this.control.request<LawsListResult>('laws.list', { world: 'twr' }));
		} catch (err) {
			this.refusal = toRefusal('Read the control laws', err);
		}
	}

	/** Import a law file by its path on the computer that runs the test. */
	async importLaw(path: string): Promise<boolean> {
		const res = await this.act<{ law: LawEntry }>('Import Law', 'laws.import', { path: path.trim() });
		if (!res) return false;
		await this.refreshLaws();
		if (res.law.world === 'twr') {
			this.chooseLaw(res.law.id);
			this.acq.note = { tone: 'pass', text: `Imported ${res.law.name} ${res.law.version}.` };
		} else this.acq.note = { tone: 'warn', text: `${res.law.name} is not a TWR law; it was imported for its own test type.` };
		return true;
	}

	/** Start the draft from the service's configuration (the one the run holds). */
	adoptServed(): boolean {
		const c = this.served?.config;
		if (!c || this.locked) return false;
		this.setup = fromTwrConfig(c, this.setup.facts);
		return true;
	}

	// =========================================================================================
	// The reference

	/**
	 * Read a reference: a file by its full path on the computer that runs the test, or pasted
	 * text. The service keeps the raw record under its SHA-256 and says what it read; the draft then
	 * replicates it. Its unit is the control row's unless the operator names another.
	 */
	async importReference(from: { path: string } | { text: string }, opts: { fs_hz?: number | null; column?: number | null; units?: string; name?: string } = {}): Promise<boolean> {
		if (this.locked) return false;
		const params: Record<string, unknown> = 'path' in from ? { path: from.path.trim() } : { text: from.text };
		if (opts.fs_hz !== null && opts.fs_hz !== undefined && Number.isFinite(opts.fs_hz)) params.fs_hz = opts.fs_hz;
		if (opts.column !== null && opts.column !== undefined && Number.isInteger(opts.column)) params.column = opts.column;
		params.units = opts.units?.trim() || this.unit;
		if (opts.name?.trim()) params.name = opts.name.trim();
		const res = await this.act<TwrReferenceImportResult>('Read Reference', 'twr.reference.import', params as unknown as TwrReferenceImportParams as unknown as Record<string, unknown>);
		if (!res) return false;
		const f = res.reference;
		this.imported = { path: f.path, facts: f, warnings: res.warnings, views: res.views };
		this.setup.reference = { sha256: f.sha256, path: f.path };
		this.setup.facts = f;
		return true;
	}

	// =========================================================================================
	// The preview and the dry run

	schedulePreview(delay = this.checkDelayMs) {
		if (this.previewTimer) clearTimeout(this.previewTimer);
		this.previewTimer = setTimeout(() => void this.refreshPreview(), delay);
	}

	/** The service prepares the reference: the grid, the section, its kinematics and coverage, four views. */
	async refreshPreview(): Promise<TwrReferencePreviewResult | null> {
		if (this.previewTimer) clearTimeout(this.previewTimer);
		this.previewTimer = null;
		const params = this.previewInput;
		if (!this.connected || !params) return null;
		const key = this.previewInputKey;
		const seq = ++this.previewSeq;
		this.previewBusy = true;
		try {
			const res = await this.control.request<TwrReferencePreviewResult>('twr.reference.preview', params as unknown as Record<string, unknown>);
			if (seq === this.previewSeq) {
				this.preview = res;
				this.previewKey = key;
				this.previewError = null;
			}
			return res;
		} catch (err) {
			if (seq === this.previewSeq) {
				this.preview = null;
				this.previewKey = key;
				// What a configuration would be refused comes as items, each in its own words.
				const r = toRefusal('Prepare the reference', err);
				const items = r.reason === 'config_invalid' ? ((r.detail?.errors as { message: string }[] | undefined) ?? []) : [];
				this.previewError = items.length ? items.map((i) => i.message).join(' ') : r.message;
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
	async checkSetup(): Promise<TwrDryRunResult | null> {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = null;
		if (!this.connected || errorCount(this.local) > 0) return null;
		const config = this.wire;
		const key = this.key;
		const seq = ++this.checkSeq;
		this.checking = true;
		try {
			const res = await this.control.request<TwrDryRunResult>('twr.configure', { config, dry_run: true });
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
		const res = await this.act<TwrConfigureResult>('Apply Setup', 'twr.configure', { config });
		if (!res) {
			const r = this.refusal;
			if (r?.reason === 'config_invalid' && r.detail) {
				const d = r.detail as unknown as { errors?: TwrDryRunResult['errors']; warnings?: TwrDryRunResult['warnings'] };
				this.check = { errors: d.errors ?? [], warnings: d.warnings ?? [], derived: null, views: [] };
				this.checkedKey = key;
			}
			return false;
		}
		this.status = res.state;
		this.live = null;
		this.setHeld({ runId: res.state.run_id, key });
		this.served = { run_id: res.state.run_id, config, derived: res.derived, views: [] };
		void this.loadPlays(res.state.run_id);
		void this.readServed();
		this.acq.note = { tone: 'pass', text: 'The service holds this setup.' };
		return true;
	}

	/** Arm: the station opens and gates; nothing is driven. With words, past a feasibility block. */
	async arm(ack?: string): Promise<boolean> {
		const res = await this.act<TwrState>(ack ? 'Arm Past Feasibility' : 'Arm', 'twr.arm', ack ? { override_ack: ack } : {});
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** Identification first, then the review hold or the law seeded and the first play. */
	async start(): Promise<boolean> {
		const res = await this.act<TwrState>('Start', 'twr.start');
		if (res) {
			this.onStatus(res);
			this.step = 'run';
		}
		return res !== null;
	}

	/** Build the law from the identification, seed it, and go on with the schedule. */
	async proceed(): Promise<boolean> {
		const res = await this.act<TwrState>('Proceed', 'twr.proceed');
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** One correction play at the present level, manual ladder only. */
	async iterate(): Promise<boolean> {
		const res = await this.act<TwrState>('Iterate', 'twr.iterate');
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** Begin playback from the present level, once it has gated; manual ladder only. */
	async play(): Promise<boolean> {
		const res = await this.act<TwrState>('Play', 'twr.play');
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** Step the level 1 dB, between plays. */
	async level(deltaDb: number): Promise<number | null> {
		const res = await this.act<TwrLevelResult>('Level', 'twr.level', { delta_db: deltaDb });
		return res?.level_db ?? null;
	}

	/** Go to a level of the table, by its place from 0. */
	async toRung(rung: number): Promise<number | null> {
		const res = await this.act<TwrLevelResult>('Go to Level', 'twr.level', { rung });
		return res?.level_db ?? null;
	}

	/** Hold the law's corrections off (the drive in force replays), or let them go on. */
	async freeze(frozen: boolean): Promise<boolean> {
		// The answer says what was asked; `state.frozen` follows when it takes effect, before the next play.
		const res = await this.act<TwrFreezeResult>(frozen ? 'Freeze' : 'Resume Corrections', 'twr.freeze', { frozen });
		return res !== null;
	}

	async stop(): Promise<boolean> {
		const res = await this.act<TwrState>('Stop', 'twr.stop', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	async abort(): Promise<boolean> {
		const res = await this.act<TwrState>('Abort', 'twr.abort', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** A new run from the same configuration: the only release from the coherence hold. */
	async reset(): Promise<boolean> {
		const res = await this.act<TwrState>('New Run', 'twr.reset');
		if (!res) return false;
		this.onStatus(res);
		this.live = null;
		if (this.held && this.setupStatus !== 'changed') this.setHeld({ runId: res.run_id, key: this.key });
		return true;
	}

	// =========================================================================================
	// One play's records

	/** Open a play's records (Replication, Error, Drive), or close them when that play is open. */
	async toggleRecord(index: number): Promise<void> {
		if (this.record?.index === index) {
			this.record = null;
			return;
		}
		this.recordBusy = index;
		try {
			const res = await this.control.request<TwrRecordResult>('twr.record', { index, points: RECORD_POINTS });
			this.record = { index, views: res.views };
		} catch (err) {
			const r = toRefusal('Show the play', err);
			this.refusal = r.code === 'not_found' ? { ...r, title: 'Not Held Any More', message: `${r.message} The plays table keeps its row.` } : r;
			if (this.record?.index === index) this.record = null;
		} finally {
			if (this.recordBusy === index) this.recordBusy = null;
		}
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
