/**
 * Mixed Mode's working state, against the control service's `mixed.*` and `laws.*` methods
 * (schema/PROTOCOL.md, "Mixed Mode", "Control Laws").
 *
 * The station, the channel table and the controller seat are the service's and shared by every
 * test type: they stay in the Acquire store, and this one reads them from there. What is Mixed
 * Mode's own is here: the setup draft (kept across reloads), the dry run of the draft (the service's
 * derived values and its two preview views), the law catalogs of the two roles (the floor's from the
 * `single` world, the tones' from the `sine` world), the run's state and its live numbers, and the
 * verbs, `mixed.part` and `mixed.adjust` among them. As everywhere in Acquire, the service holds the
 * truth and computes every number; this store keeps the operator's edits beside it and sends them
 * when the operator says so.
 *
 * A reply or a frame of another run than the one on screen is not applied: a stop's reply can land
 * after the run has finished, and a frame from the run before after a new one is configured.
 */

import { regresses } from '../runOrder';
import { untrack } from 'svelte';
import type { ServiceClient } from '$lib/protocol/client.svelte';
import type { ChannelRow, LadderMode } from '$lib/protocol/types';
import type { LawEntry, LawsListResult } from '$lib/protocol/mimo';
import {
	MIXED_LAW_WORLDS,
	MIXED_TOPICS,
	type MixedAdjustResult,
	type MixedConfig,
	type MixedConfigResult,
	type MixedConfigureResult,
	type MixedDryRunResult,
	type MixedLevelResult,
	type MixedLiveHead,
	type MixedLogEntry,
	type MixedLogEvent,
	type MixedLogResult,
	type MixedModeResult,
	type MixedPartResult,
	type MixedPartRow,
	type MixedState
} from '$lib/protocol/mixed';
import type { ActionId, ActionState, RunHead, SetupStatus } from '../actions';
import { lawParamErrors, lawsOfWorld, lawValues, type LawErrors } from '../law/lawForm';
import { canonical } from '../mimo/mimoSetup';
import { errorCount, mergeErrors, type FieldErrors, type ProfileHost, type ScheduleHost } from '../setup';
import type { AcquireState } from '../store.svelte';
import { toRefusal, type Refusal } from '../words';
import type { LiveParamState } from '../sine/sineVerbs';
import {
	defaultMixedSetup,
	freeId,
	fromMixedConfig,
	localErrors,
	MIXED_SECTIONS,
	toneLawStart,
	mergeMixedSetup,
	mixedByKey,
	mixedSectionOf,
	newBand,
	newTone,
	toMixedConfig,
	type MixedDraft,
	type MixedSection
} from './mixedSetup';
import { mixedBarActions, mixedIsLive, mixedLiveParams, mixedModuleVerbs, mixedPrimary, partToggle, type MixedVerbContext, type MixedVerbId } from './mixedVerbs';
import { MIXED_OUTCOME_WORDS, MIXED_PHASE_WORDS, PART_REFUSED_WORDS, partStateWord } from './mixedWords';
import { MIXED_MAX_BANDS, MIXED_MAX_TONES } from '$lib/protocol/mixed';

const SETUP_KEY = 'hertzforge.acquire.mixed.setup.v1';
/** The setup this screen applied, by run: `{runId, key}`. */
const HELD_KEY = 'hertzforge.acquire.mixed.held.v1';
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

const nanOut = (_k: string, v: unknown) => (typeof v === 'number' && Number.isNaN(v) ? '__NaN__' : v);
const nanIn = (_k: string, v: unknown) => (v === '__NaN__' ? NaN : v);

export function saveMixedSetup(d: MixedDraft): string {
	return JSON.stringify(d, nanOut);
}

export function loadMixedSetup(raw: string | null): MixedDraft {
	if (!raw) return defaultMixedSetup();
	try {
		return mergeMixedSetup(JSON.parse(raw, nanIn) as Partial<MixedDraft>);
	} catch {
		return defaultMixedSetup();
	}
}

export interface MixedOptions {
	local?: Storage | null;
	/** Delay before a dry run after an edit, ms. */
	checkDelayMs?: number;
	/** A display window observes: it keeps no draft and checks nothing. */
	drafts?: boolean;
}

/** The law keys a law panel reads: `law.id`, `law.params.<key>`. */
function lawKeys(E: FieldErrors, prefix: 'lawFloor' | 'lawTones'): LawErrors {
	const out: LawErrors = {};
	for (const [k, v] of Object.entries(E)) if (k.startsWith(`${prefix}.`)) out[`law.${k.slice(prefix.length + 1)}`] = v;
	return out;
}

export class MixedStore {
	readonly control: ServiceClient;
	private readonly localStore: Storage | null;
	private readonly checkDelayMs: number;
	private readonly drafts: boolean;

	// ---- the operator's draft --------------------------------------------------------------
	setup = $state<MixedDraft>(defaultMixedSetup());
	section = $state<MixedSection>('floor');
	step = $state<'setup' | 'run'>('setup');

	// ---- from the service ------------------------------------------------------------------
	status = $state.raw<MixedState | null>(null);
	/** The run's numbers from the last frame: `mixed.live`, or this screen's own `mixed.data`. */
	live = $state.raw<MixedLiveHead | null>(null);
	served = $state.raw<MixedConfigResult | null>(null);
	check = $state.raw<MixedDryRunResult | null>(null);
	checkedKey = $state('');
	checking = $state(false);
	/** The laws of the two worlds the roles draw from, built in first. */
	floorCatalog = $state.raw<LawEntry[] | null>(null);
	toneCatalog = $state.raw<LawEntry[] | null>(null);
	lawProblems = $state.raw<{ file: string; reason: string }[]>([]);
	log = $state.raw<MixedLogEntry[]>([]);
	held = $state.raw<{ runId: string | null; key: string } | null>(null);
	/** The last `mixed.part` refusal, against the part it concerns. */
	partRefusal = $state.raw<{ id: string; message: string } | null>(null);
	/** A part switch on its way, by id. */
	partBusy = $state<string | null>(null);

	busy = $state<string | null>(null);
	#refusal = $state.raw<Refusal | null>(null);
	syncError = $state<string | null>(null);

	private liveNewer = $state(false);
	private checkSeq = 0;
	private logRun: string | null = null;
	private checkTimer: ReturnType<typeof setTimeout> | null = null;

	constructor(
		readonly acq: AcquireState,
		options: MixedOptions = {}
	) {
		this.control = acq.control;
		this.localStore = options.local === undefined ? storageOf('localStorage') : options.local;
		this.checkDelayMs = options.checkDelayMs ?? 300;
		this.drafts = options.drafts ?? true;
		this.setup = loadMixedSetup(read(this.localStore, SETUP_KEY));
		const held = read(this.localStore, HELD_KEY);
		if (held) {
			try {
				this.held = JSON.parse(held);
			} catch {
				this.held = null;
			}
		}
		this.control.on(MIXED_TOPICS.state, (d) => this.onStatus(d as unknown as MixedState));
		const onFrame = (d: Record<string, unknown>) => {
			const l = d as unknown as MixedLiveHead;
			if (this.status?.run_id && l.run_id !== this.status.run_id) return;
			this.live = l;
			this.liveNewer = true;
		};
		this.control.on(MIXED_TOPICS.live, onFrame);
		this.control.on(MIXED_TOPICS.data, onFrame);
		this.control.on(MIXED_TOPICS.log, (d) => {
			const e = d as unknown as MixedLogEvent;
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

	/** The channels a run's streams would hold: every enabled input and drive of the table being edited. */
	get recordChannels(): number {
		return this.rows.filter((r) => r.enabled).length;
	}

	/** The sample rate the run asks for, for the size its streams take. */
	get recordRateHz(): number {
		return this.setup.acquisition.fs_hz;
	}

	get isLive(): boolean {
		return mixedIsLive(this.status);
	}

	get locked(): boolean {
		return this.isLive || this.acq.isLive;
	}

	get tableFiles() {
		return this.acq.tableFiles;
	}

	get floorLaws(): LawEntry[] {
		return lawsOfWorld(this.floorCatalog, MIXED_LAW_WORLDS.floor);
	}

	get toneLaws(): LawEntry[] {
		return lawsOfWorld(this.toneCatalog, MIXED_LAW_WORLDS.tones);
	}

	get floorLaw(): LawEntry | null {
		return this.floorLaws.find((l) => l.id === this.setup.lawFloor.id) ?? null;
	}

	get toneLaw(): LawEntry | null {
		return this.toneLaws.find((l) => l.id === this.setup.lawTones.id) ?? null;
	}

	readonly wire = $derived.by((): MixedConfig => toMixedConfig(this.setup, { floor: this.floorLaw, tones: this.toneLaw }, this.acq.rows));
	readonly key = $derived(canonical(this.wire));

	readonly local = $derived.by((): FieldErrors => {
		const lawErr = (prefix: 'lawFloor' | 'lawTones', law: LawEntry | null, params: Record<string, unknown>) =>
			Object.fromEntries(Object.entries(lawParamErrors(law, params as never)).map(([k, v]) => [k.replace(/^law\./, `${prefix}.`), v]));
		return mergeErrors(mergeErrors(localErrors(this.setup), lawErr('lawFloor', this.floorLaw, this.setup.lawFloor.params)), lawErr('lawTones', this.toneLaw, this.setup.lawTones.params));
	});

	/** Findings by key: the editor's own and the dry run's for the draft as it stands. */
	get fieldErrors(): FieldErrors {
		const fromCheck = this.check && this.checkedKey === this.key ? mixedByKey(this.setup, this.check.errors) : {};
		const missing: FieldErrors = {};
		if (this.floorCatalog && !this.floorLaw) missing['lawFloor.id'] = ['The chosen floor law is not in the catalog; choose another.'];
		if (this.toneCatalog && !this.toneLaw) missing['lawTones.id'] = ['The chosen tone law is not in the catalog; choose another.'];
		return mergeErrors(mergeErrors(this.local, fromCheck), missing);
	}

	get fieldWarnings(): FieldErrors {
		return this.check && this.checkedKey === this.key ? mixedByKey(this.setup, this.check.warnings) : {};
	}

	/** The two law panels' findings. */
	get floorLawErrors(): LawErrors {
		return lawKeys(this.fieldErrors, 'lawFloor');
	}

	get toneLawErrors(): LawErrors {
		return lawKeys(this.fieldErrors, 'lawTones');
	}

	get counts(): Record<MixedSection, { errors: number; warnings: number }> {
		const out = Object.fromEntries(MIXED_SECTIONS.map((s) => [s.id, { errors: 0, warnings: 0 }])) as Record<MixedSection, { errors: number; warnings: number }>;
		for (const [k, v] of Object.entries(this.fieldErrors)) out[mixedSectionOf(k)].errors += v.length;
		for (const [k, v] of Object.entries(this.fieldWarnings)) out[mixedSectionOf(k)].warnings += v.length;
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

	get pending(): string | null {
		if (this.acq.stationDirty) return 'Use the chosen station, or put the choice back, first.';
		if (!this.floorCatalog || !this.toneCatalog) return 'The law catalog has not been read yet.';
		return null;
	}

	get runConfig(): MixedConfig | null {
		return this.served?.config ?? null;
	}

	/** The service's derived values for the draft as it stands, or for the run it holds. */
	get derived() {
		if (this.check && this.checkedKey === this.key && this.check.derived) return this.check.derived;
		return this.locked ? (this.served?.derived ?? null) : null;
	}

	/** The latest numbers: the state, with this run's live frame laid over it when it came after. */
	get now(): MixedState | null {
		const s = this.status;
		const l = this.live;
		if (!s || !l || l.run_id !== s.run_id || !this.liveNewer) return s;
		return {
			...s,
			state: l.state,
			phase: l.phase,
			mode: l.mode,
			level_db: l.commanded_db ?? s.level_db,
			t_s: l.t_s ?? s.t_s,
			elapsed_s: l.elapsed_s ?? s.elapsed_s,
			parts_s: l.parts_s ?? s.parts_s,
			at_level_s: l.at_level_s ?? s.at_level_s,
			remaining_at_level_s: l.remaining_at_level_s ?? s.remaining_at_level_s,
			held: l.held,
			alarm: l.alarm,
			limiting: l.limiting,
			parts: l.parts,
			total: l.total,
			law: s.law && l.law ? { floor: { ...s.law.floor, misses: l.law.floor.misses, capped: l.law.floor.capped }, tones: { ...s.law.tones, misses: Math.max(0, ...l.law.tones.map((t) => t.misses)), capped: l.law.tones.reduce((n, t) => n + t.capped, 0) } } : s.law
		};
	}

	get frameOfRun(): MixedLiveHead | null {
		const s = this.status;
		const l = this.live;
		return s && l && l.run_id === s.run_id ? l : null;
	}

	/** One row per part, as the service last said, in the parts' order. */
	get parts(): MixedPartRow[] {
		return this.now?.parts ?? [];
	}

	partName(id: string): string {
		return this.parts.find((p) => p.id === id)?.name ?? this.served?.derived?.parts.find((p) => p.id === id)?.name ?? id;
	}

	private get verbContext(): MixedVerbContext {
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

	get actions(): Record<ActionId, ActionState> {
		return mixedBarActions(this.verbContext);
	}

	get primary(): ActionId | null {
		return mixedPrimary(this.verbContext);
	}

	get verbs(): Record<MixedVerbId, ActionState> {
		return mixedModuleVerbs(this.verbContext);
	}

	/** Whether the part's switch may be used now, and which way it goes. */
	toggleOf(row: MixedPartRow): ActionState & { to: boolean } {
		const t = partToggle(this.verbContext, row);
		if (t.enabled && this.partBusy) return { ...t, enabled: false, reason: 'Waiting for the last switch.' };
		// A part whose window has closed or whose schedule has run out cannot come back; say so on the
		// switch rather than offer it and show the service's refusal.
		if (t.enabled && partStateWord(row, { rows: this.parts, config: this.runConfig, parts_s: this.now?.parts_s ?? null }) === 'Ended') return { ...t, enabled: false, reason: 'The part has ended.' };
		return t;
	}

	/** The tone law of the run: the one the service runs, else the draft's choice. */
	get runLaw(): LawEntry | null {
		const id = this.status?.law?.tones.id ?? this.served?.derived?.law.tones.id ?? null;
		return (id ? this.toneLaws.find((l) => l.id === id) : null) ?? this.toneLaw;
	}

	get liveParams(): LiveParamState[] {
		const values = (this.runConfig?.control?.law?.tones?.params ?? this.setup.lawTones.params) as Record<string, unknown>;
		return mixedLiveParams(this.runLaw, this.now?.law?.tones ?? null, values, this.verbContext);
	}

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
			at_level_s: s.at_level_s,
			remaining_at_level_s: s.remaining_at_level_s,
			end: s.end as unknown as RunHead['end'],
			record: s.record ?? null
		};
	}

	get frame(): { level_db: number | null; at_level_s: number | null; remaining_at_level_s: number | null } | null {
		const l = this.frameOfRun;
		if (!l || this.status?.state === 'finished') return null;
		return { level_db: l.level_db, at_level_s: l.at_level_s, remaining_at_level_s: l.remaining_at_level_s };
	}

	readonly outcomeWords: Record<string, string> = MIXED_OUTCOME_WORDS;
	readonly phaseWords: Record<string, string> = MIXED_PHASE_WORDS;

	get elapsed(): number | null {
		return this.frameOfRun?.elapsed_s ?? this.status?.elapsed_s ?? null;
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
	// What the shared editors read: the floor as Random's profile, the ladder as Random's schedule

	/** The floor's breakpoint table, as Random's profile editor reads one. */
	readonly floorHost: ProfileHost = (() => {
		// eslint-disable-next-line @typescript-eslint/no-this-alias
		const self = this;
		return {
			get setup() {
				const f = self.setup.floor;
				return {
					get title() {
						return self.setup.title;
					},
					set title(v: string) {
						self.setup.title = v;
					},
					breakpoints: f.breakpoints,
					alarm_db: f.alarm_db,
					abort_db: f.abort_db
				};
			},
			get locked() {
				return self.locked;
			},
			get fieldErrors() {
				return self.fieldErrors;
			},
			get fieldWarnings() {
				return self.fieldWarnings;
			},
			get tableFiles() {
				return self.acq.tableFiles;
			}
		};
	})();

	readonly scheduleHost: ScheduleHost = (() => {
		// eslint-disable-next-line @typescript-eslint/no-this-alias
		const self = this;
		return {
			get setup() {
				return { schedule: self.setup.schedule };
			},
			get locked() {
				return self.locked;
			},
			get fieldErrors() {
				return self.fieldErrors;
			}
		};
	})();

	// =========================================================================================
	// Wiring

	watch(): () => void {
		return $effect.root(() => {
			$effect(() => {
				const text = saveMixedSetup($state.snapshot(this.setup) as MixedDraft);
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

	async sync(): Promise<void> {
		const c = this.control;
		try {
			const [status, served, floor, tones] = await Promise.all([
				c.request<MixedState>('mixed.state'),
				c.request<MixedConfigResult>('mixed.config'),
				c.request<LawsListResult>('laws.list', { world: MIXED_LAW_WORLDS.floor }),
				c.request<LawsListResult>('laws.list', { world: MIXED_LAW_WORLDS.tones })
			]);
			this.syncError = null;
			this.served = served;
			this.onStatus(status);
			this.setLaws(floor, tones);
		} catch (err) {
			this.syncError = toRefusal('Read the control service', err).message;
			return;
		}
		if (this.drafts) this.scheduleCheck(0);
	}

	private onStatus(s: MixedState) {
		const was = this.status;
		if (regresses(was, s)) return;
		this.status = s;
		this.liveNewer = false;
		if (this.live && this.live.run_id === s.run_id && (s.state === 'unconfigured' || s.state === 'configured' || s.state === 'armed' || s.state === 'blocked')) this.live = null;
		if (s.run_id !== this.logRun) void this.loadLog(s.run_id);
		if (was?.run_id !== s.run_id && s.run_id) void this.readServed();
		if (was?.run_id !== s.run_id) this.partRefusal = null;
		if (mixedIsLive(s) && !mixedIsLive(was) && this.step === 'setup' && s.state !== 'armed') this.step = 'run';
	}

	async loadLog(runId: string | null): Promise<void> {
		this.logRun = runId;
		if (!runId) {
			this.log = [];
			this.live = null;
			return;
		}
		try {
			const res = await this.control.request<MixedLogResult>('mixed.log', { since: 0 });
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
			this.served = await this.control.request<MixedConfigResult>('mixed.config');
		} catch {
			/* kept as it was */
		}
	}

	private setLaws(floor: LawsListResult, tones: LawsListResult) {
		this.floorCatalog = floor.laws;
		this.toneCatalog = tones.laws;
		this.lawProblems = [...floor.problems, ...tones.problems.filter((p) => !floor.problems.some((q) => q.file === p.file))];
		const f = this.floorLaw;
		if (f) this.setup.lawFloor.params = lawValues(f, this.setup.lawFloor.params, 1);
		const t = this.toneLaw;
		if (t) this.setup.lawTones.params = lawValues(t, toneLawStart(t.id, this.setup.lawTones.params) as never, 1);
	}

	/** Choose a role's law: its parameters start from the operator's last valid values, else its defaults. */
	chooseLaw(role: 'floor' | 'tones', id: string) {
		const law = (role === 'floor' ? this.floorLaws : this.toneLaws).find((l) => l.id === id);
		if (!law || this.locked) return;
		const draft = role === 'floor' ? this.setup.lawFloor : this.setup.lawTones;
		const previous = id === draft.id ? draft.params : undefined;
		const next = { id, params: lawValues(law, (role === 'tones' ? toneLawStart(id, previous) : previous) as never, 1) };
		if (role === 'floor') this.setup.lawFloor = next;
		else this.setup.lawTones = next;
	}

	async refreshLaws(): Promise<void> {
		try {
			const [floor, tones] = await Promise.all([this.control.request<LawsListResult>('laws.list', { world: MIXED_LAW_WORLDS.floor }), this.control.request<LawsListResult>('laws.list', { world: MIXED_LAW_WORLDS.tones })]);
			this.setLaws(floor, tones);
		} catch (err) {
			this.refusal = toRefusal('Read the control laws', err);
		}
	}

	/** Import a law file by its path; it is chosen for the role whose world it is. */
	async importLaw(role: 'floor' | 'tones', path: string): Promise<boolean> {
		const res = await this.act<{ law: LawEntry }>('Import Law', 'laws.import', { path: path.trim() });
		if (!res) return false;
		await this.refreshLaws();
		const world = MIXED_LAW_WORLDS[role];
		if (res.law.world === world) {
			this.chooseLaw(role, res.law.id);
			this.acq.note = { tone: 'pass', text: `Imported ${res.law.name} ${res.law.version}.` };
		} else this.acq.note = { tone: 'warn', text: `${res.law.name} is not a ${role === 'floor' ? 'floor' : 'tone'} law; it was imported for its own test type.` };
		return true;
	}

	adoptServed(): boolean {
		const c = this.served?.config;
		if (!c || this.locked) return false;
		this.setup = fromMixedConfig(c);
		return true;
	}

	// =========================================================================================
	// The parts tables

	addTone(): boolean {
		if (this.locked || this.setup.tones.length >= MIXED_MAX_TONES) return false;
		const id = freeId(this.setup, 't');
		this.setup.tones.push(newTone(id, this.setup.tones.length + 1, 100 * (this.setup.tones.length + 1)));
		return true;
	}

	addBand(): boolean {
		if (this.locked || this.setup.bands.length >= MIXED_MAX_BANDS) return false;
		const id = freeId(this.setup, 'b');
		this.setup.bands.push(newBand(id, this.setup.bands.length + 1, 250 * (this.setup.bands.length + 1)));
		return true;
	}

	/** Remove rows of a table; a follower of a removed part is left naming it, for the editor to flag. */
	removeRows(table: 'tones' | 'bands', rows: number[]): void {
		if (this.locked) return;
		const drop = new Set(rows);
		if (table === 'tones') this.setup.tones = this.setup.tones.filter((_, i) => !drop.has(i));
		else this.setup.bands = this.setup.bands.filter((_, i) => !drop.has(i));
	}

	addLimitChannel(): boolean {
		if (this.locked) return false;
		const used = new Set(this.setup.limits.channels.map((c) => c.channel));
		const row = this.rows.find((r) => r.enabled && r.role !== 'drive' && r.role !== 'control' && !used.has(r.index));
		const f = this.setup.floor.breakpoints;
		const lo = f[0]?.freq_hz ?? 20;
		const hi = f[f.length - 1]?.freq_hz ?? 2000;
		this.setup.limits.channels.push({ channel: row?.index ?? 0, kind: 'response', rows: [{ freq_hz: lo, ceiling: NaN }, { freq_hz: hi, ceiling: NaN }] });
		return true;
	}

	// =========================================================================================
	// The dry run

	scheduleCheck(delay = this.checkDelayMs) {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = setTimeout(() => void this.checkSetup(), delay);
	}

	async checkSetup(): Promise<MixedDryRunResult | null> {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = null;
		if (!this.connected || errorCount(this.local) > 0) return null;
		const config = this.wire;
		const key = this.key;
		const seq = ++this.checkSeq;
		this.checking = true;
		try {
			const res = await this.control.request<MixedDryRunResult>('mixed.configure', { config, dry_run: true });
			if (seq === this.checkSeq) {
				this.check = res;
				this.checkedKey = key;
			}
			return res;
		} catch (err) {
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

	async apply(): Promise<boolean> {
		if (!this.actions.apply.enabled) return false;
		if (this.acq.tableDirty) {
			const ok = await this.acq.applyTable();
			if (!ok) return false;
		}
		const config = this.wire;
		const key = this.key;
		const res = await this.act<MixedConfigureResult>('Apply Setup', 'mixed.configure', { config });
		if (!res) {
			const r = this.refusal;
			if (r?.reason === 'config_invalid' && r.detail) {
				const d = r.detail as unknown as { errors?: MixedDryRunResult['errors']; warnings?: MixedDryRunResult['warnings'] };
				this.check = { errors: d.errors ?? [], warnings: d.warnings ?? [], derived: null, views: [] };
				this.checkedKey = key;
			}
			return false;
		}
		this.status = res.state;
		this.live = null;
		this.setHeld({ runId: res.state.run_id, key });
		this.served = { run_id: res.state.run_id, config, derived: res.derived, views: [] };
		void this.loadLog(res.state.run_id);
		void this.readServed();
		// The setup's footer says the service holds it; no notice is laid over the screen.
		return true;
	}

	async arm(ack?: string): Promise<boolean> {
		const res = await this.act<MixedState>(ack ? 'Arm Past Feasibility' : 'Arm', 'mixed.arm', ack ? { override_ack: ack } : {});
		if (res) this.onStatus(res);
		return res !== null;
	}

	async start(): Promise<boolean> {
		const res = await this.act<MixedState>('Start', 'mixed.start');
		if (res) {
			this.onStatus(res);
			this.step = 'run';
		}
		return res !== null;
	}

	async proceed(): Promise<boolean> {
		const res = await this.act<MixedState>('Proceed', 'mixed.proceed');
		if (res) this.onStatus(res);
		return res !== null;
	}

	async setMode(mode: LadderMode): Promise<boolean> {
		return (await this.act<MixedModeResult>('Ladder Mode', 'mixed.mode', { mode })) !== null;
	}

	async level(deltaDb: number): Promise<number | null> {
		return (await this.act<MixedLevelResult>('Level', 'mixed.level', { delta_db: deltaDb }))?.level_db ?? null;
	}

	async goTo(db: number): Promise<number | null> {
		return (await this.act<MixedLevelResult>('Go to Level', 'mixed.level', { to_db: db }))?.level_db ?? null;
	}

	async hold(): Promise<boolean> {
		const res = await this.act<MixedState>('Hold', 'mixed.hold');
		if (res) this.onStatus(res);
		return res !== null;
	}

	async resume(): Promise<boolean> {
		const res = await this.act<MixedState>('Resume', 'mixed.resume');
		if (res) this.onStatus(res);
		return res !== null;
	}

	/**
	 * Switch a part on or off live, as a ramp. A refusal is kept against the part's row, in the
	 * service's words with the reason it named.
	 */
	async part(id: string, on: boolean): Promise<MixedPartResult | null> {
		const runId = this.status?.run_id ?? null;
		this.partBusy = id;
		this.partRefusal = null;
		try {
			const res = await this.control.request<MixedPartResult>('mixed.part', { id, on });
			return this.status?.run_id === runId ? res : null;
		} catch (err) {
			const r = toRefusal(on ? 'Switch On' : 'Switch Off', err);
			if (this.status?.run_id === runId) {
				const why = typeof r.detail?.why === 'string' ? PART_REFUSED_WORDS[r.detail.why] : undefined;
				this.partRefusal = { id, message: why ? `${why} ${r.message}`.trim() : r.message };
			}
			return null;
		} finally {
			if (this.partBusy === id) this.partBusy = null;
		}
	}

	async adjust(key: string, value: number): Promise<MixedAdjustResult | null> {
		return this.act<MixedAdjustResult>('Change Parameter', 'mixed.adjust', { key, value });
	}

	async stop(): Promise<boolean> {
		const res = await this.act<MixedState>('Stop', 'mixed.stop', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	async abort(): Promise<boolean> {
		const res = await this.act<MixedState>('Abort', 'mixed.abort', {}, false);
		if (res) this.onStatus(res);
		return res !== null;
	}

	async reset(): Promise<boolean> {
		const res = await this.act<MixedState>('New Run', 'mixed.reset');
		if (!res) return false;
		this.onStatus(res);
		this.live = null;
		if (this.held && this.setupStatus !== 'changed') this.setHeld({ runId: res.run_id, key: this.key });
		return true;
	}

	/** Forget the run the service holds (`mixed.clear`): nothing is configured after it. */
	async clear(): Promise<boolean> {
		const res = await this.act<MixedState>('Clear', 'mixed.clear');
		if (!res) return false;
		this.onStatus(res);
		this.live = null;
		this.served = null;
		this.setHeld(null);
		return true;
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
