/**
 * Explorer's working state, against the control service's `explorer.*` methods
 * (schema/PROTOCOL.md, "Explorer").
 *
 * The station, the channel table and the controller seat are the service's and shared by every
 * test type: they stay in the Acquire store, and this one reads them from there. What is
 * Explorer's own is here: the setup draft (kept across reloads), the dry run of the draft, the
 * run's state and its live numbers, the last steering answer, and the verbs. As everywhere in
 * Acquire, the service holds the truth and computes every number; this store keeps the operator's
 * edits beside it and sends them when the operator says so. A steering verb shows what the service
 * adopted, never what was asked.
 */

import { regresses } from '../runOrder';
import { untrack } from 'svelte';
import type { ServiceClient } from '$lib/protocol/client.svelte';
import type { ChannelRow, LogEntry } from '$lib/protocol/types';
import {
	EXPLORER_TOPICS,
	type ExplorerConfig,
	type ExplorerConfigResult,
	type ExplorerConfigureResult,
	type ExplorerDerived,
	type ExplorerDryRunResult,
	type ExplorerHoldResult,
	type ExplorerLiveHead,
	type ExplorerLogEvent,
	type ExplorerLogResult,
	type ExplorerState,
	type ExplorerSteerParams,
	type ExplorerSteerResult,
	type ExplorerSurveyClearResult,
	type ExplorerSweepDirection,
	type ExplorerSweepResult
} from '$lib/protocol/explorer';
import type { ActionId, ActionState, RunHead, SetupStatus } from '../actions';
import { canonical } from '../mimo/mimoSetup';
import { errorCount, type FieldErrors } from '../setup';
import type { AcquireState } from '../store.svelte';
import { toRefusal, type Refusal } from '../words';
import {
	defaultExplorerSetup,
	EXPLORER_SECTIONS,
	explorerByKey,
	explorerSectionOf,
	fromExplorerConfig,
	localErrors,
	mergeExplorerSetup,
	toExplorerConfig,
	type ExplorerDraft,
	type ExplorerSection
} from './explorerSetup';
import { explorerBarActions, explorerIsLive, explorerPrimary, explorerSteerVerbs, type ExplorerVerbContext, type ExplorerVerbId } from './explorerVerbs';
import { answerWords, EXPLORER_OUTCOME_WORDS, EXPLORER_PHASE_WORDS, EXPLORER_STATE_WORDS, explorerStateFact } from './explorerWords';

const SETUP_KEY = 'hertzforge.acquire.explorer.setup.v1';
/** The rate the service asks for when the setup gives none (PROTOCOL.md, "Explorer Configuration"). */
const EXPLORER_DEFAULT_FS_HZ = 8192;
/** The setup this screen applied, by run: `{runId, key}`. */
const HELD_KEY = 'hertzforge.acquire.explorer.held.v1';
/** Most log entries kept on screen; the service keeps the full log with the run. */
const LOG_LIMIT = 500;

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

export function saveExplorerSetup(d: ExplorerDraft): string {
	return JSON.stringify(d, nanOut);
}

export function loadExplorerSetup(raw: string | null): ExplorerDraft {
	if (!raw) return defaultExplorerSetup();
	try {
		return mergeExplorerSetup(JSON.parse(raw, nanIn) as Partial<ExplorerDraft>);
	} catch {
		return defaultExplorerSetup();
	}
}

export interface ExplorerOptions {
	local?: Storage | null;
	/** Delay before a dry run after an edit, ms. */
	checkDelayMs?: number;
	/** A display window observes: it keeps no draft and checks nothing. */
	drafts?: boolean;
}

/** The service's answer to the last steering verb, as the panel shows it. */
export interface SteerAnswer {
	verb: 'steer' | 'sweep' | 'hold';
	/** Which parts the verb sent, so the words name only those. */
	parts: { hz: boolean; vpk: boolean };
	result: ExplorerSteerResult | ExplorerSweepResult | ExplorerHoldResult;
	words: string;
}

export class ExplorerStore {
	readonly control: ServiceClient;
	private readonly localStore: Storage | null;
	private readonly checkDelayMs: number;
	private readonly drafts: boolean;

	// ---- the operator's draft --------------------------------------------------------------
	setup = $state<ExplorerDraft>(defaultExplorerSetup());
	section = $state<ExplorerSection>('test');
	/** Where the operator is: the setup, or the run. */
	step = $state<'setup' | 'run'>('setup');

	// ---- from the service ------------------------------------------------------------------
	status = $state.raw<ExplorerState | null>(null);
	/**
	 * The run's numbers from the last frame: `explorer.live` on the default subscription, or this
	 * screen's own `explorer.data` once it has subscribed to plots (the service then sends no
	 * `explorer.live`).
	 */
	live = $state.raw<ExplorerLiveHead | null>(null);
	/** What the service holds: the configuration of the present run. */
	served = $state.raw<ExplorerConfigResult | null>(null);
	/** The dry run of the draft's setup, and the key of the setup it checked. */
	check = $state.raw<ExplorerDryRunResult | null>(null);
	checkedKey = $state('');
	checking = $state(false);
	log = $state.raw<LogEntry[]>([]);
	/** The setup this screen applied: the run it made and the setup's key. */
	held = $state.raw<{ runId: string | null; key: string } | null>(null);
	/** The service's answer to the last steering verb this screen sent, for this run. */
	answer = $state.raw<SteerAnswer | null>(null);

	// ---- feedback --------------------------------------------------------------------------
	busy = $state<string | null>(null);
	#refusal = $state.raw<Refusal | null>(null);
	syncError = $state<string | null>(null);

	/** Which came last for this run: the live frame (true) or the state. */
	private liveNewer = $state(false);
	private checkSeq = 0;
	private logRun: string | null = null;
	private checkTimer: ReturnType<typeof setTimeout> | null = null;

	constructor(
		readonly acq: AcquireState,
		options: ExplorerOptions = {}
	) {
		this.control = acq.control;
		this.localStore = options.local === undefined ? storageOf('localStorage') : options.local;
		this.checkDelayMs = options.checkDelayMs ?? 300;
		this.drafts = options.drafts ?? true;
		this.setup = loadExplorerSetup(read(this.localStore, SETUP_KEY));
		const held = read(this.localStore, HELD_KEY);
		if (held) {
			try {
				this.held = JSON.parse(held);
			} catch {
				this.held = null;
			}
		}
		this.control.on(EXPLORER_TOPICS.state, (d) => this.onStatus(d as unknown as ExplorerState));
		const onFrame = (d: Record<string, unknown>) => {
			const l = d as unknown as ExplorerLiveHead;
			if (this.status?.run_id && l.run_id !== this.status.run_id) return;
			this.live = l;
			this.liveNewer = true;
		};
		this.control.on(EXPLORER_TOPICS.live, onFrame);
		this.control.on(EXPLORER_TOPICS.data, onFrame);
		this.control.on(EXPLORER_TOPICS.log, (d) => {
			const e = d as unknown as ExplorerLogEvent;
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
		return explorerIsLive(this.status);
	}

	/** Setup, station and table are locked while a run is live. */
	get locked(): boolean {
		return this.isLive || this.acq.isLive;
	}

	readonly wire = $derived.by((): ExplorerConfig => toExplorerConfig(this.setup));
	readonly key = $derived(canonical(this.wire));
	readonly local = $derived.by((): FieldErrors => localErrors(this.setup));

	/** Findings by key: the editor's own and the dry run's for the draft as it stands. */
	get fieldErrors(): FieldErrors {
		const fromCheck = this.check && this.checkedKey === this.key ? explorerByKey(this.check.errors) : {};
		const out: FieldErrors = {};
		for (const map of [this.local, fromCheck]) for (const [k, v] of Object.entries(map)) for (const m of v) if (!(out[k] ??= []).includes(m)) out[k].push(m);
		return out;
	}

	get fieldWarnings(): FieldErrors {
		return this.check && this.checkedKey === this.key ? explorerByKey(this.check.warnings) : {};
	}

	/** Errors and warnings per section. */
	get counts(): Record<ExplorerSection, { errors: number; warnings: number }> {
		const out = Object.fromEntries(EXPLORER_SECTIONS.map((s) => [s.id, { errors: 0, warnings: 0 }])) as Record<ExplorerSection, { errors: number; warnings: number }>;
		for (const [k, v] of Object.entries(this.fieldErrors)) out[explorerSectionOf(k)].errors += v.length;
		for (const [k, v] of Object.entries(this.fieldWarnings)) out[explorerSectionOf(k)].warnings += v.length;
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
		return null;
	}

	/** The configuration the run holds, as the service has it. */
	get runConfig(): ExplorerConfig | null {
		return this.served?.config ?? null;
	}

	/** What the service derived for the draft as it stands (the dry run), else for the run it holds. */
	get derived(): ExplorerDerived | null {
		if (this.check && this.checkedKey === this.key && this.check.derived) return this.check.derived;
		return this.served?.derived ?? null;
	}

	/**
	 * The latest numbers: the state, with this run's live frame laid over it when the frame came
	 * after it. A state that came after the frame (a change of phase, a sweep parked, a stop) is the
	 * run's word until the next frame.
	 */
	get now(): ExplorerState | null {
		const s = this.status;
		const l = this.live;
		if (!s || !l || l.run_id !== s.run_id || !this.liveNewer) return s;
		return {
			...s,
			state: l.state,
			phase: l.phase,
			t_s: l.t_s ?? s.t_s,
			frequency_hz: l.frequency_hz,
			target_hz: l.target_hz,
			drive_vpk: l.drive_vpk,
			target_vpk: l.target_vpk,
			sweep: l.sweep,
			pipeline_frames: l.pipeline_frames ?? s.pipeline_frames
		};
	}

	/** The last frame's numbers, when they are this run's. */
	get frameOfRun(): ExplorerLiveHead | null {
		const s = this.status;
		const l = this.live;
		return s && l && l.run_id === s.run_id ? l : null;
	}

	private get verbContext(): ExplorerVerbContext {
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
		return explorerBarActions(this.verbContext);
	}

	get primary(): ActionId | null {
		return explorerPrimary(this.verbContext);
	}

	/** The steering verbs, with why each waits. */
	get verbs(): Record<ExplorerVerbId, ActionState> {
		return explorerSteerVerbs(this.verbContext);
	}

	// ---- the run as the shared run bar and notices read it ---------------------------------

	get run(): RunHead | null {
		const s = this.now;
		if (!s) return null;
		return {
			run_id: s.run_id,
			// The bar reads Random's states; Explorer's own (`starting`) comes with its words.
			state: s.state as RunHead['state'],
			phase: s.phase,
			title: s.title,
			mode: null,
			levels_db: null,
			level_db: null,
			at_level_s: null,
			remaining_at_level_s: null,
			end: s.end as unknown as RunHead['end'],
			record: s.record ?? null
		};
	}

	/** The channels a run's streams would hold: every enabled input and the drive of the table being edited. */
	get recordChannels(): number {
		return this.rows.filter((r) => r.enabled).length;
	}

	/** The rate the run asks for (the usual one when none is given), for the size its streams take. */
	get recordRateHz(): number {
		return this.setup.acquisition.fs_hz ?? EXPLORER_DEFAULT_FS_HZ;
	}

	/** No level in dB: the bar's level readout is not shown, and the frequency and drive take its place. */
	readonly frame = null;
	readonly levelHidden = true;

	/** The run bar's words for Explorer's outcomes, states and phases. */
	readonly outcomeWords: Record<string, string> = EXPLORER_OUTCOME_WORDS;
	readonly stateWords: Record<string, string> = EXPLORER_STATE_WORDS;
	readonly phaseWords: Record<string, string> = EXPLORER_PHASE_WORDS;

	/** Station time since start. */
	get elapsed(): number | null {
		return this.frameOfRun?.t_s ?? this.status?.t_s ?? null;
	}

	get source(): { source: 'SIM' | 'HARDWARE'; cls: 'sim' | 'ni_simulated' | 'ni_real' } | null {
		const l = this.frameOfRun;
		if (l) return { source: l.source, cls: l.class };
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

	dismissRefusal() {
		this.#refusal = null;
		this.acq.dismissRefusal();
	}

	dismissNote() {
		this.acq.dismissNote();
	}

	// =========================================================================================
	// Wiring

	/** Keep the draft across reloads, and re-check it as it changes. Returns the stopper. */
	watch(): () => void {
		return $effect.root(() => {
			$effect(() => {
				const text = saveExplorerSetup($state.snapshot(this.setup) as ExplorerDraft);
				untrack(() => write(this.localStore, SETUP_KEY, text));
			});
			$effect(() => {
				const key = this.key;
				untrack(() => {
					if (this.drafts && key && key !== this.checkedKey) this.scheduleCheck();
				});
			});
		});
	}

	/** Read what the service holds: the run and its configuration. */
	async sync(): Promise<void> {
		const c = this.control;
		try {
			const [status, served] = await Promise.all([c.request<ExplorerState>('explorer.state'), c.request<ExplorerConfigResult>('explorer.config')]);
			this.syncError = null;
			this.served = served;
			this.onStatus(status);
		} catch (err) {
			this.syncError = toRefusal('Read the control service', err).message;
			return;
		}
		if (this.drafts) this.scheduleCheck(0);
	}

	private onStatus(s: ExplorerState) {
		const was = this.status;
		// A reply older than the run's own word that it finished is not applied.
		if (regresses(was, s)) return;
		this.status = s;
		// The state is newer than the live frame held: it is the run's word until the next frame.
		this.liveNewer = false;
		if (this.live && this.live.run_id === s.run_id && (s.state === 'unconfigured' || s.state === 'configured' || s.state === 'starting')) this.live = null;
		if (s.run_id !== this.logRun) void this.loadLog(s.run_id);
		if (was?.run_id !== s.run_id) {
			this.answer = null;
			if (s.run_id) void this.readServed();
		}
		// The run went live from this screen or another: the run is the next thing to watch.
		if (explorerIsLive(s) && !explorerIsLive(was) && this.step === 'setup') this.step = 'run';
	}

	/** The run's log from the start, after a new run or a reconnection. */
	async loadLog(runId: string | null): Promise<void> {
		this.logRun = runId;
		if (!runId) {
			this.log = [];
			this.live = null;
			return;
		}
		try {
			const res = await this.control.request<ExplorerLogResult>('explorer.log', { since: 0 });
			if (this.logRun !== runId) return;
			const have = new Map(this.log.map((e) => [e.seq, e]));
			for (const e of res.entries) have.set(e.seq, e);
			this.log = [...have.values()].sort((a, b) => a.seq - b.seq).slice(-LOG_LIMIT);
		} catch {
			/* the entries come with each log event */
		}
	}

	private async readServed(): Promise<void> {
		try {
			this.served = await this.control.request<ExplorerConfigResult>('explorer.config');
		} catch {
			/* kept as it was */
		}
	}

	/** Start the draft from the service's configuration (the one the run holds); the steering steps stay. */
	adoptServed(): boolean {
		const c = this.served?.config;
		if (!c || this.locked) return false;
		this.setup = fromExplorerConfig(c, this.setup.steps);
		return true;
	}

	// =========================================================================================
	// The dry run

	scheduleCheck(delay = this.checkDelayMs) {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = setTimeout(() => void this.checkSetup(), delay);
	}

	/** The service's verdict on the draft (`dry_run`: needs no control, replaces nothing). */
	async checkSetup(): Promise<ExplorerDryRunResult | null> {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = null;
		if (!this.connected || errorCount(this.local) > 0) return null;
		const config = this.wire;
		const key = this.key;
		const seq = ++this.checkSeq;
		this.checking = true;
		try {
			const res = await this.control.request<ExplorerDryRunResult>('explorer.configure', { config, dry_run: true });
			if (seq === this.checkSeq) {
				this.check = res;
				this.checkedKey = key;
			}
			return res;
		} catch (err) {
			// A configuration whose shape is wrong is refused by field; show it as a finding there.
			const r = toRefusal('Check the setup', err);
			if (seq === this.checkSeq && r.reason === 'bad_params') {
				const field = typeof r.detail?.param === 'string' ? r.detail.param : 'setup';
				this.check = { errors: [{ reason: 'out_of_range', field, message: r.message }], warnings: [], derived: null };
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
		const res = await this.act<ExplorerConfigureResult>('Apply Setup', 'explorer.configure', { config });
		if (!res) {
			const r = this.refusal;
			if (r?.reason === 'config_invalid' && r.detail) {
				const d = r.detail as unknown as { errors?: ExplorerDryRunResult['errors']; warnings?: ExplorerDryRunResult['warnings'] };
				this.check = { errors: d.errors ?? [], warnings: d.warnings ?? [], derived: null };
				this.checkedKey = key;
			}
			return false;
		}
		this.status = res.state;
		this.live = null;
		this.answer = null;
		this.setHeld({ runId: res.state.run_id, key });
		this.served = { run_id: res.state.run_id, config, derived: res.derived };
		void this.loadLog(res.state.run_id);
		void this.readServed();
		this.acq.note = { tone: 'pass', text: 'The service holds this setup.' };
		return true;
	}

	/** The station opens, arms and starts the tone at 0 V, in one step. */
	async start(): Promise<boolean> {
		const res = await this.act<ExplorerState>('Start', 'explorer.start');
		if (res) {
			this.onStatus(res);
			this.step = 'run';
		}
		return res !== null;
	}

	/** Explorer has no separate arm or review hold: the run bar never offers them. */
	arm(): Promise<boolean> {
		return Promise.resolve(false);
	}

	proceed(): Promise<boolean> {
		return Promise.resolve(false);
	}

	setMode(): Promise<boolean> {
		return Promise.resolve(false);
	}

	level(): Promise<null> {
		return Promise.resolve(null);
	}

	goTo(): Promise<null> {
		return Promise.resolve(null);
	}

	/**
	 * Steer the tone: a frequency part, a level part, or one of each, landing in the same frame. The
	 * answer is what the service adopted, shown as it came; null when the service refused it.
	 */
	async steer(params: ExplorerSteerParams): Promise<ExplorerSteerResult | null> {
		const res = await this.act<ExplorerSteerResult>('Steer', 'explorer.steer', params as unknown as Record<string, unknown>);
		if (res) {
			const parts = { hz: 'hz' in params || 'step_hz' in params || 'step_oct' in params, vpk: 'vpk' in params || 'step_db' in params };
			this.answer = { verb: 'steer', parts, result: res, words: answerWords(res, parts) };
		}
		return res;
	}

	/** Move the frequency by a number of octaves from the target. */
	stepFrequency(oct: number) {
		return this.steer({ step_oct: oct });
	}

	/** Go to a frequency, clamped into the band by the service. */
	goToHz(hz: number) {
		return this.steer({ hz });
	}

	/** Step the level in dB from the target in force. */
	stepLevel(db: number) {
		return this.steer({ step_db: db });
	}

	/** Set the level in volts peak, clamped to the ceiling by the service. */
	setVpk(v: number) {
		return this.steer({ vpk: v });
	}

	/** Sweep from where the tone stands toward a band edge; the answer is the rate used. */
	async sweep(direction: ExplorerSweepDirection, rate: number): Promise<ExplorerSweepResult | null> {
		const res = await this.act<ExplorerSweepResult>('Sweep', 'explorer.sweep', { direction, rate_oct_per_min: rate });
		if (res) this.answer = { verb: 'sweep', parts: { hz: true, vpk: false }, result: res, words: answerWords(res, { hz: true, vpk: false }) };
		return res;
	}

	/** End the sweep: the tone holds where it is. */
	async hold(): Promise<ExplorerHoldResult | null> {
		const res = await this.act<ExplorerHoldResult>('Hold', 'explorer.hold');
		if (res) this.answer = { verb: 'hold', parts: { hz: true, vpk: false }, result: res, words: `Holding ${res.hz.toFixed(1)} Hz` };
		return res;
	}

	/** Empty the survey line; it fills again as the tone moves. */
	async clearSurvey(): Promise<number | null> {
		const res = await this.act<ExplorerSurveyClearResult>('Clear Survey', 'explorer.survey.clear');
		return res?.revision ?? null;
	}

	async stop(): Promise<boolean> {
		const res = await this.act<ExplorerState>('Stop', 'explorer.stop', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	async abort(): Promise<boolean> {
		const res = await this.act<ExplorerState>('Abort', 'explorer.abort', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	/** A new run from the same configuration. */
	async reset(): Promise<boolean> {
		const res = await this.act<ExplorerState>('New Run', 'explorer.reset');
		if (!res) return false;
		this.onStatus(res);
		this.live = null;
		this.answer = null;
		if (this.held && this.setupStatus !== 'changed') this.setHeld({ runId: res.run_id, key: this.key });
		return true;
	}

	private async act<R>(verb: string, method: string, params: Record<string, unknown> = {}, exclusive = true): Promise<R | null> {
		if (exclusive) this.busy = verb;
		this.#refusal = null;
		try {
			return await this.control.request<R>(method, params);
		} catch (err) {
			const r = toRefusal(verb, err);
			// A verb in the wrong state names the state in Explorer's own words.
			if (r.reason === 'wrong_state') {
				const fact = explorerStateFact(r.detail);
				if (fact) r.facts = [fact];
			}
			this.#refusal = r;
			return null;
		} finally {
			if (exclusive && this.busy === verb) this.busy = null;
		}
	}
}
