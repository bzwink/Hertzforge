/**
 * MIMO Random's working state, against the control service's `mimo_random.*` and `laws.*`
 * methods (schema/PROTOCOL.md, "MIMO Random", "MIMO Random Reference", "Control Laws").
 *
 * The station, the channel table and the controller seat are the service's and are shared with
 * every test type: they stay in the Acquire store, and this one reads them from there. What is
 * MIMO Random's own is here: the setup draft (kept across reloads), the reference the service
 * built or read from it, the law catalog, the dry run of the setup, the run's status, and the
 * verbs. As in single-output Random, the service holds the truth and computes every number; this
 * store keeps the operator's edits beside it and sends them when the operator says so.
 */

import { regresses } from '../runOrder';
import { untrack } from 'svelte';
import type { ServiceClient } from '$lib/protocol/client.svelte';
import type { ChannelRow, ParamOption } from '$lib/protocol/types';
import type {
	LawEntry,
	LawsListResult,
	MimoReference,
	MimoReferenceResult,
	MimoReport,
	MimoReportResult,
	MimoRigidResult,
	MimoSetup,
	MimoSetupResult,
	MimoOverrideReason,
	MimoStatus,
	MimoValidation
} from '$lib/protocol/mimo';
import { MIMO_TOPICS } from '$lib/protocol/mimo';
import { MimoFeed } from '../display/mimoFeed.svelte';
import { gridWindow, newMimoPlot, type MimoPlotSpec } from '../display/mimoLayout';
import { LIVE_STATES, type ActionId, type ActionState, type SetupStatus } from '../actions';
import { errorCount, mergeErrors, type FieldErrors } from '../setup';
import type { AcquireState } from '../store.svelte';
import { toRefusal, type Refusal } from '../words';
import {
	breakpointParams,
	byKey,
	canonical,
	channelErrors,
	controlledLabels,
	defaultMimoSetup,
	ensureSettings,
	lawParamError,
	lawValues,
	referenceErrors,
	rigidParams,
	RIGID_DOFS,
	sectionOf,
	settingErrors,
	shapeSpace,
	spaceErrors,
	testChannels,
	toWireSetup,
	type EffectiveSpace,
	type MatrixDraft,
	type MimoDraft,
	type MimoSection,
	type ReferenceMethod
} from './mimoSetup';

const SETUP_KEY = 'hertzforge.acquire.mimo.setup.v1';
/** The setup this screen applied, by run: `{runId, key}`. */
const HELD_KEY = 'hertzforge.acquire.mimo.held.v1';

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

export function loadMimoSetup(raw: string | null): MimoDraft {
	const base = defaultMimoSetup();
	if (!raw) return base;
	try {
		const saved = JSON.parse(raw, nanIn) as Partial<MimoDraft>;
		const out = { ...base, ...saved } as MimoDraft;
		out.space = { ...base.space, ...(saved.space ?? {}) };
		out.reference = { ...base.reference, ...(saved.reference ?? {}) };
		out.reference.rigid = { ...base.reference.rigid, ...(saved.reference?.rigid ?? {}) };
		out.estimation = { ...base.estimation, ...(saved.estimation ?? {}) };
		out.identification = { ...base.identification, ...(saved.identification ?? {}) };
		out.control = { ...base.control, ...(saved.control ?? {}) };
		out.law = { ...base.law, ...(saved.law ?? {}) };
		out.record = { ...base.record, ...(saved.record ?? {}) };
		if (!Array.isArray(out.schedule?.levels)) out.schedule = base.schedule;
		return out;
	} catch {
		return base;
	}
}

export interface MimoOptions {
	local?: Storage | null;
	/** Delay before a dry run or a reference build after an edit, ms. */
	checkDelayMs?: number;
	/** A display window observes: it keeps no draft and checks nothing. */
	drafts?: boolean;
}

/** One run-bar verb's state, as the single-output bar reads it. */
const hidden: ActionState = { visible: false, enabled: false, reason: null };

export class MimoState {
	readonly control: ServiceClient;
	private readonly localStore: Storage | null;
	private readonly checkDelayMs: number;
	private readonly drafts: boolean;

	// ---- the operator's draft --------------------------------------------------------------
	setup = $state<MimoDraft>(defaultMimoSetup());
	section = $state<MimoSection>('channels');
	/** Where the operator is: the setup, the prediction (after identification) or the run. */
	step = $state<'setup' | 'predict' | 'run'>('setup');

	// ---- from the service ------------------------------------------------------------------
	status = $state.raw<MimoStatus | null>(null);
	report = $state.raw<MimoReport | null>(null);
	/** The dry run of the draft's setup. */
	check = $state.raw<MimoValidation | null>(null);
	checking = $state(false);
	/** The key of the setup the dry run checked. */
	checkedKey = $state('');
	laws = $state.raw<LawEntry[] | null>(null);
	lawProblems = $state.raw<{ file: string; reason: string }[]>([]);
	/** The reference the service built or read from the draft, and the inputs it answered. */
	ref = $state.raw<MimoReferenceResult | null>(null);
	refKey = $state('');
	refBusy = $state(false);
	rigid = $state.raw<MimoRigidResult | null>(null);
	rigidKey = $state('');
	/** The setup this screen applied: the run it made and the setup's key. */
	held = $state.raw<{ runId: string | null; key: string } | null>(null);

	// ---- the Predict page's plots -----------------------------------------------------------
	/** The first control of the achievable reference's grid window on the Predict page. */
	predictFirst = $state(0);
	/** The grid's width on screen, in points the service decimates to. */
	predictPoints = $state(256);
	/** Whether the Predict page is on screen and showing the achievable reference. */
	predictShown = $state(false);
	/** The Predict page's subscription: the achievable reference's block, while it is shown. */
	readonly predictFeed: MimoFeed;

	// ---- feedback --------------------------------------------------------------------------
	busy = $state<string | null>(null);
	#refusal = $state.raw<Refusal | null>(null);
	syncError = $state<string | null>(null);

	private checkSeq = 0;
	private refSeq = 0;
	private rigidSeq = 0;
	private checkTimer: ReturnType<typeof setTimeout> | null = null;
	private refTimer: ReturnType<typeof setTimeout> | null = null;
	private rigidTimer: ReturnType<typeof setTimeout> | null = null;

	constructor(
		readonly acq: AcquireState,
		options: MimoOptions = {}
	) {
		this.control = acq.control;
		this.localStore = options.local === undefined ? storageOf('localStorage') : options.local;
		this.checkDelayMs = options.checkDelayMs ?? 300;
		this.drafts = options.drafts ?? true;
		this.setup = loadMimoSetup(read(this.localStore, SETUP_KEY));
		const held = read(this.localStore, HELD_KEY);
		if (held) {
			try {
				this.held = JSON.parse(held);
			} catch {
				this.held = null;
			}
		}
		this.predictFeed = new MimoFeed(this.control, {
			plots: () => this.predictPlots,
			rate: () => 2
		});
		this.control.on(MIMO_TOPICS.status, (d) => this.onStatus(d as unknown as MimoStatus));
		this.control.on(MIMO_TOPICS.report, (d) => (this.report = (d as unknown as MimoReportResult).report ?? null));
	}

	// =========================================================================================
	// Derived state

	/** The plots the Predict page asks for: the achievable reference, when there is one. */
	get predictPlots(): MimoPlotSpec[] {
		const p = this.status?.prediction;
		const n = this.status?.shape?.labels.length ?? 0;
		if (!this.predictShown || !p?.has_achievable || !n) return [];
		const block = gridWindow(Math.min(this.predictFirst, Math.max(0, n - 1)), n);
		return [{ ...newMimoPlot('predict:achievable', 'pred_sdm', { kind: 'block', block }), points: this.predictPoints }];
	}

	get connected(): boolean {
		return this.acq.connected;
	}

	get isController(): boolean {
		return this.acq.isController;
	}

	get rows(): ChannelRow[] {
		return this.acq.rows;
	}

	/** The station selected on the service, for the setup's backend. */
	get stationKind(): 'sim' | 'ni' | null {
		return this.acq.selected?.kind ?? null;
	}

	get isLive(): boolean {
		return !!this.status && LIVE_STATES.includes(this.status.state);
	}

	/** Setup, station and table are locked while a run is live. */
	get locked(): boolean {
		return this.isLive || this.acq.isLive;
	}

	/** Enabled inputs plus drives in the table being edited: the channels a run's streams hold. */
	get recordChannels(): number {
		return this.rows.filter((r) => r.enabled).length;
	}

	/** The sample rate the run asks for, Hz. */
	get recordRateHz(): number {
		return this.setup.estimation.fs_hz;
	}

	readonly channels = $derived(testChannels(this.rows, this.setup));

	/** The control space the setup uses: the operator's, or the one a reference brings. */
	readonly space = $derived.by((): EffectiveSpace => {
		const d = this.setup;
		const method = d.reference.method;
		const driveMatrix = d.space.driveOn ? d.space.drive : null;
		if (method === 'rigid') {
			const t = this.rigid?.ok ? this.rigid.transform : null;
			const response: MatrixDraft | null = t ? { rows: t.rows, cols: t.cols, values: t.values, source: 'Rigid-body fit' } : null;
			return { kind: 'virtual', dofs: RIGID_DOFS, response, drive: driveMatrix, from: 'rigid' };
		}
		if (method === 'import' && this.ref?.transforms.response) {
			const t = this.ref.transforms.response;
			const drive = this.ref.transforms.drive;
			return {
				kind: 'virtual',
				dofs: t.row_labels,
				response: { rows: t.rows, cols: t.cols, values: t.values, source: 'The imported reference' },
				drive: drive ? { rows: drive.rows, cols: drive.cols, values: drive.values, source: 'The imported reference' } : driveMatrix,
				from: 'import'
			};
		}
		return { kind: d.space.kind, dofs: d.space.dofs, response: d.space.kind === 'virtual' ? d.space.response : null, drive: d.space.kind === 'virtual' ? driveMatrix : null, from: 'operator' };
	});

	readonly labels = $derived(controlledLabels(this.rows, this.setup, this.space));

	/** The law chosen, from the catalog. */
	get law(): LawEntry | null {
		return this.laws?.find((l) => l.id === this.setup.law.id) ?? null;
	}

	/** Laws of the multiple-output world: built in first, then imported, as the service lists them. */
	get mimoLaws(): LawEntry[] {
		return (this.laws ?? []).filter((l) => l.world === 'mimo');
	}

	/** What the reference request is for the draft as it stands, as a key. */
	readonly refInputKey = $derived.by(() => {
		const d = this.setup;
		if (d.reference.method === 'import') return canonical({ m: 'import', p: d.reference.importPath.trim(), q: d.reference.importPairPath.trim(), g: [d.estimation.fs_hz, d.estimation.n_pts], r: d.reference.acceptRepair, u: d.reference.unit, v: d.reference.previewPair });
		if (d.reference.method === 'field') return '';
		return canonical({ m: 'bp', p: breakpointParams(d, this.labels) });
	});

	/** The reference for the setup, when the service's answer is for the draft as it stands. */
	get reference(): MimoReference | null {
		return this.ref && this.refKey === this.refInputKey ? this.ref.reference : null;
	}

	readonly wire = $derived<MimoSetup | null>(
		toWireSetup(this.setup, { rows: this.rows, space: this.space, reference: this.reference, law: this.law, backend: this.stationKind })
	);
	readonly key = $derived(this.wire ? canonical(this.wire) : '');

	readonly local = $derived.by((): FieldErrors => {
		const d = this.setup;
		const out = mergeErrors(mergeErrors(channelErrors(this.acq.rows, d), spaceErrors(this.acq.rows, d, this.space)), mergeErrors(settingErrors(d), referenceErrors(d, this.labels)));
		const law = this.law;
		if (law) {
			for (const p of law.params) {
				const e = lawParamError(p, d.law.params[p.key]);
				if (e) (out[`law.params.${p.key}`] ??= []).push(e);
			}
		}
		return out;
	});

	/**
	 * The dry run's errors. The service checks the setup against the table it holds; while the
	 * table has edits here (applied with the setup), a channel the edits give the right role is
	 * not an error.
	 */
	get checkErrors() {
		const items = this.check?.errors ?? [];
		if (!this.acq.tableDirty) return items;
		const want: Record<string, ChannelRow['role']> = { control_unknown_channel: 'control', drive_unknown_channel: 'drive', limit_unknown_channel: 'monitor' };
		return items.filter((it) => {
			const role = want[it.reason];
			if (!role || it.channel === undefined || it.channel === null) return true;
			return !this.acq.rows.some((r) => r.index === it.channel && r.enabled && r.role === role);
		});
	}

	/** Findings by key: the editor's own, the dry run's, and the reference request's. */
	get fieldErrors(): FieldErrors {
		const fromCheck = this.check && this.checkedKey === this.key ? byKey(this.checkErrors, this.wire) : {};
		const fromRef = this.ref && this.refKey === this.refInputKey ? byKey(this.ref.errors, null) : {};
		const fromRigid = this.setup.reference.method === 'rigid' && this.rigid && !this.rigid.ok ? byKey(this.rigid.errors, null) : {};
		return mergeErrors(mergeErrors(this.local, fromCheck), mergeErrors(fromRef, fromRigid));
	}

	get fieldWarnings(): FieldErrors {
		const fromCheck = this.check && this.checkedKey === this.key ? byKey(this.check.warnings, this.wire) : {};
		const fromRef = this.ref && this.refKey === this.refInputKey ? byKey(this.ref.warnings, null) : {};
		const fromRigid = this.setup.reference.method === 'rigid' && this.rigid?.warnings ? byKey(this.rigid.warnings, null) : {};
		return mergeErrors(mergeErrors(fromCheck, fromRef), fromRigid);
	}

	/** Errors and warnings per section. */
	get counts(): Record<MimoSection, { errors: number; warnings: number }> {
		const out = Object.fromEntries((['channels', 'reference', 'control', 'schedule', 'record', 'backend', 'predict'] as MimoSection[]).map((s) => [s, { errors: 0, warnings: 0 }])) as Record<MimoSection, { errors: number; warnings: number }>;
		for (const [k, v] of Object.entries(this.fieldErrors)) out[sectionOf(k)].errors += v.length;
		for (const [k, v] of Object.entries(this.fieldWarnings)) out[sectionOf(k)].warnings += v.length;
		out.backend.errors += this.acq.tableErrorCount;
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
		if (!this.reference) {
			if (this.refBusy) return 'The reference is being built.';
			if (this.setup.reference.method === 'field') return 'Import the reference built from field data first.';
			return 'The setup needs its reference first.';
		}
		return null;
	}

	/** The run bar's verbs, by the single-output bar's names. */
	get actions(): Record<ActionId, ActionState> {
		const s = this.status;
		const state = s?.state ?? 'unconfigured';
		const live = LIVE_STATES.includes(state);
		const gate = (visible: boolean, verb: string, own: string | null, needsControl = true): ActionState => {
			if (!visible) return hidden;
			let reason: string | null = null;
			if (!this.connected) reason = 'The control service is not connected.';
			else if (needsControl && !this.isController) reason = `Take control to ${verb}.`;
			else if (this.busy && needsControl) reason = `Waiting for ${this.busy}.`;
			else reason = own;
			return { visible, enabled: reason === null, reason };
		};
		const errors = this.setupErrorCount;
		const errorWords = errors > 0 ? `Fix the ${errors === 1 ? 'error' : `${errors} errors`} in the setup first.` : null;
		const status = this.setupStatus;
		const apply = gate(!live && !this.acq.isLive, 'apply the setup', this.pending ?? errorWords ?? (status === 'applied' ? 'The service already holds this setup.' : null));
		const changed = status === 'changed' ? 'The setup changed since it was applied. Apply it first.' : null;
		const start = gate(state === 'configured', 'start', this.acq.stationDirty ? 'Use the chosen station first.' : changed);
		const p = s?.prediction ?? null;
		const proceed = gate(
			state === 'review',
			'engage',
			s?.phase === 'gate_failed'
				? 'A gate failed at identification. Engage only through an override, on the Predict page.'
				: s?.phase === 'prediction_exceeded' || (p && !p.drives_pass)
					? 'The prediction asks a drive for more than its ceiling. Change the law or its parameters and predict again, or engage through an override on the Predict page.'
					: null
		);
		const stop = gate(live, 'stop', state === 'stopping' ? 'The run is already ramping down.' : null, false);
		const abort = gate(live, 'abort', null, false);
		return { apply, arm: hidden, override: hidden, start, proceed, level: hidden, mode: hidden, stop, abort, reset: hidden };
	}

	get primary(): ActionId | null {
		switch (this.status?.state ?? 'unconfigured') {
			case 'unconfigured':
			case 'finished':
				return 'apply';
			case 'configured':
				return this.setupStatus === 'changed' ? 'apply' : 'start';
			case 'review':
				return 'proceed';
			default:
				return null;
		}
	}

	// ---- the refusal, the note and the connection, as the shared notices read them -----------

	get refusal(): Refusal | null {
		return this.#refusal ?? this.acq.refusal;
	}

	set refusal(r: Refusal | null) {
		this.#refusal = r;
	}

	get note() {
		return this.acq.note;
	}

	get run() {
		return this.status;
	}

	// ---- the controller seat and the data source, as the run bar reads them ----------------

	get session() {
		return this.acq.session;
	}

	get orphanDeadline() {
		return this.acq.orphanDeadline;
	}

	/** MIMO Random sends no live head; the bar reads the status. */
	readonly frame = null;

	get elapsed(): number | null {
		return this.status?.elapsed_s ?? null;
	}

	get source(): { source: 'SIM' | 'HARDWARE'; cls: 'sim' | 'ni_simulated' | 'ni_real' } | null {
		const s = this.status;
		if (s?.source && s.station) return { source: s.source, cls: s.station.class };
		return this.acq.source;
	}

	claim() {
		return this.acq.claim();
	}

	release() {
		return this.acq.release();
	}

	/** Verbs the bar never offers for MIMO Random (no arm step, no manual level yet). */
	arm() {}
	reset() {}
	setMode() {}
	level() {}
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

	/** Rebuild the reference and re-check the setup as the draft changes. Returns the stopper. */
	watch(): () => void {
		return $effect.root(() => {
			$effect(() => {
				const snapshot = $state.snapshot(this.setup);
				untrack(() => write(this.localStore, SETUP_KEY, JSON.stringify(snapshot, nanOut)));
			});
			$effect(() => {
				// Every drive and control row has its settings; matrices follow the channels.
				const rows = this.acq.rows;
				void rows.map((r) => [r.role, r.enabled, r.index]);
				// The grids also follow the degrees of freedom named and the virtual drives.
				void [this.setup.space.dofs.length, this.setup.space.kind, this.setup.space.driveOn];
				untrack(() => {
					if (this.locked) return;
					ensureSettings(rows, this.setup);
					const t = testChannels(rows, this.setup);
					shapeSpace(this.setup, t.controls.length, t.drives.length);
				});
			});
			$effect(() => {
				const key = this.refInputKey;
				untrack(() => {
					if (this.drafts && key && key !== this.refKey && this.setup.reference.method !== 'import') this.scheduleReference();
				});
			});
			$effect(() => {
				const method = this.setup.reference.method;
				const key = method === 'rigid' ? canonical(rigidParams(this.acq.rows, this.setup)) : '';
				untrack(() => {
					if (this.drafts && key && key !== this.rigidKey) this.scheduleRigid();
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

	/** Read what the service holds: the run, the law catalog, the report. */
	async sync(): Promise<void> {
		const c = this.control;
		try {
			const [status, laws, report] = await Promise.all([
				c.request<MimoStatus>('mimo_random.status'),
				c.request<LawsListResult>('laws.list', { world: 'mimo' }),
				c.request<MimoReportResult>('mimo_random.report')
			]);
			this.syncError = null;
			this.onStatus(status);
			this.setLaws(laws);
			this.report = report.report;
		} catch (err) {
			this.syncError = toRefusal('Read the control service', err).message;
			return;
		}
		if (!this.drafts) return;
		this.scheduleReference(0);
		this.scheduleCheck(0);
	}

	private onStatus(s: MimoStatus) {
		const was = this.status;
		// A reply older than the run's own word that it finished is not applied.
		if (regresses(was, s)) return;
		this.status = s;
		// Identification is done and the service waits: the prediction is the next thing to read.
		if (s.state === 'review' && was?.state !== 'review') this.step = 'predict';
		// Engaged from the prediction: the run is the next thing to watch.
		else if (s.state === 'running' && was?.state === 'review') this.step = 'run';
	}

	private setLaws(res: LawsListResult) {
		this.laws = res.laws;
		this.lawProblems = res.problems;
		const law = this.law;
		if (law) this.setup.law.params = lawValues(law, this.setup.law.params, this.labels.length);
	}

	/** Choose a law: its parameters start from the operator's last valid values, else its defaults. */
	chooseLaw(id: string) {
		const law = this.laws?.find((l) => l.id === id);
		if (!law || (this.locked && this.status?.state !== 'review')) return;
		this.setup.law = { id, params: lawValues(law, id === this.setup.law.id ? this.setup.law.params : undefined, this.labels.length) };
	}

	async refreshLaws(): Promise<void> {
		try {
			this.setLaws(await this.control.request<LawsListResult>('laws.list', { world: 'mimo' }));
		} catch (err) {
			this.refusal = toRefusal('Read the control laws', err);
		}
	}

	/** Import a law file by its path on the computer that runs the test. */
	async importLaw(path: string): Promise<boolean> {
		const res = await this.act<{ law: LawEntry }>('Import Law', 'laws.import', { path: path.trim() });
		if (!res) return false;
		await this.refreshLaws();
		if (res.law.world === 'mimo') this.chooseLaw(res.law.id);
		this.acq.note = { tone: 'pass', text: `Imported ${res.law.name} ${res.law.version}.` };
		return true;
	}

	/** The channels a per-channel law parameter is given for, as the shared form names options. */
	get channelOptions(): ParamOption[] {
		return this.labels.map((l, i) => ({ value: i, label: l }));
	}

	// =========================================================================================
	// The reference

	scheduleReference(delay = this.checkDelayMs) {
		if (this.refTimer) clearTimeout(this.refTimer);
		this.refTimer = setTimeout(() => void this.buildReference(), delay);
	}

	/** Build the reference from the draft's tables and pairs (the service computes it). */
	async buildReference(): Promise<MimoReferenceResult | null> {
		if (this.refTimer) clearTimeout(this.refTimer);
		this.refTimer = null;
		const d = this.setup;
		if (!this.connected || d.reference.method === 'import' || d.reference.method === 'field') return null;
		const labels = this.labels;
		const local = referenceErrors(d, labels);
		if (!labels.length || errorCount(local) > 0) return null;
		const key = this.refInputKey;
		const seq = ++this.refSeq;
		this.refBusy = true;
		try {
			const res = await this.control.request<MimoReferenceResult>('mimo_random.reference.build_breakpoints', breakpointParams(d, labels) as unknown as Record<string, unknown>);
			if (seq === this.refSeq) {
				this.ref = res;
				this.refKey = key;
			}
			return res;
		} catch (err) {
			if (seq === this.refSeq) this.refusal = toRefusal('Build the reference', err);
			return null;
		} finally {
			if (seq === this.refSeq) this.refBusy = false;
		}
	}

	/** Read a reference file by path (the service reads it; files are never uploaded). */
	async importReference(): Promise<MimoReferenceResult | null> {
		const d = this.setup;
		const path = d.reference.importPath.trim();
		if (!path) return null;
		const key = this.refInputKey;
		const seq = ++this.refSeq;
		this.refBusy = true;
		this.refusal = null;
		try {
			const res = await this.control.request<MimoReferenceResult>('mimo_random.reference.import', {
				path,
				pair_path: d.reference.importPairPath.trim() || null,
				grid: { fs_hz: d.estimation.fs_hz, n_pts: d.estimation.n_pts },
				unit: d.reference.unit,
				repair: d.reference.acceptRepair,
				preview_pair: d.reference.previewPair
			});
			if (seq === this.refSeq) {
				this.ref = res;
				this.refKey = key;
			}
			return res;
		} catch (err) {
			if (seq === this.refSeq) this.refusal = toRefusal('Import the reference', err);
			return null;
		} finally {
			if (seq === this.refSeq) this.refBusy = false;
		}
	}

	/** Choose how the reference is made. */
	setMethod(m: ReferenceMethod) {
		if (this.locked) return;
		this.setup.reference.method = m;
		this.setup.reference.acceptRepair = false;
	}

	scheduleRigid(delay = this.checkDelayMs) {
		if (this.rigidTimer) clearTimeout(this.rigidTimer);
		this.rigidTimer = setTimeout(() => void this.fitRigid(), delay);
	}

	/** The rigid-body fit of the control channels' geometry. */
	async fitRigid(): Promise<MimoRigidResult | null> {
		if (this.rigidTimer) clearTimeout(this.rigidTimer);
		this.rigidTimer = null;
		if (!this.connected) return null;
		const params = rigidParams(this.acq.rows, this.setup);
		if (params.sensors.some((s) => [...s.position, ...s.direction].some((v) => !Number.isFinite(v)))) return null;
		const key = canonical(params);
		const seq = ++this.rigidSeq;
		try {
			const res = await this.control.request<MimoRigidResult>('mimo_random.reference.build_rigid_body', params as unknown as Record<string, unknown>);
			if (seq === this.rigidSeq) {
				this.rigid = res;
				this.rigidKey = key;
			}
			return res;
		} catch (err) {
			if (seq === this.rigidSeq) this.refusal = toRefusal('Fit the rigid body', err);
			return null;
		}
	}

	// =========================================================================================
	// The setup on the service

	scheduleCheck(delay = this.checkDelayMs) {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = setTimeout(() => void this.checkSetup(), delay);
	}

	/** The service's verdict on the draft (`dry_run`: needs no control, replaces nothing). */
	async checkSetup(): Promise<MimoValidation | null> {
		if (this.checkTimer) clearTimeout(this.checkTimer);
		this.checkTimer = null;
		const setup = this.wire;
		if (!this.connected || !setup) return null;
		const key = this.key;
		const seq = ++this.checkSeq;
		this.checking = true;
		try {
			const res = await this.control.request<{ validation: MimoValidation }>('mimo_random.setup', { setup, dry_run: true });
			if (seq === this.checkSeq) {
				this.check = res.validation;
				this.checkedKey = key;
			}
			return res.validation;
		} catch (err) {
			// A setup whose shape is wrong is refused by field; show it as a finding there.
			const r = toRefusal('Check the setup', err);
			if (seq === this.checkSeq && r.reason === 'bad_params') {
				const field = typeof r.detail?.param === 'string' ? r.detail.param : 'setup';
				this.check = { ok: false, errors: [{ reason: 'out_of_range', field, message: r.message }], warnings: [], shape: { controls: 0, drives: 0, space: 'physical', dofs: null, shape: 'square' }, transforms: { response: null, drive: null }, reference: null, feasibility: [], law: null };
				this.checkedKey = key;
			}
			return null;
		} finally {
			if (seq === this.checkSeq) this.checking = false;
		}
	}

	private setHeld(h: { runId: string | null; key: string } | null) {
		this.held = h;
		write(this.localStore, HELD_KEY, h ? JSON.stringify(h) : null);
	}

	/** Send the setup. A table with edits is applied first, since the setup names its rows. */
	async apply(): Promise<boolean> {
		if (!this.actions.apply.enabled) return false;
		if (this.acq.tableDirty) {
			const ok = await this.acq.applyTable();
			if (!ok) return false;
		}
		const setup = this.wire;
		if (!setup) return false;
		const key = this.key;
		const res = await this.act<MimoSetupResult>('Apply Setup', 'mimo_random.setup', { setup });
		if (!res) {
			const r = this.refusal;
			if (r?.reason === 'setup_invalid' && r.detail) {
				this.check = r.detail as unknown as MimoValidation;
				this.checkedKey = key;
			}
			return false;
		}
		this.check = res.validation;
		this.checkedKey = key;
		this.status = res.state;
		this.setHeld({ runId: res.state.run_id, key });
		this.acq.note = { tone: 'pass', text: 'The service holds this setup.' };
		return true;
	}

	/** Identification first: the noise floor, then the plant, then the review hold. */
	async start(): Promise<boolean> {
		const res = await this.act<MimoStatus>('Start', 'mimo_random.start');
		if (res) {
			this.status = res;
			this.step = 'run';
		}
		return res !== null;
	}

	/** Engage after the review. */
	async proceed(): Promise<boolean> {
		const res = await this.act<MimoStatus>('Proceed', 'mimo_random.proceed');
		if (res) this.onStatus(res);
		return res !== null;
	}

	/**
	 * Engage past what holds the run, with the operator's own words: a failed gate, or a
	 * prediction over a drive ceiling. Each hold needs its own acknowledgement.
	 */
	async override(ack: string, reason: MimoOverrideReason = 'gate_failed'): Promise<boolean> {
		const res = await this.act<MimoStatus>('Override', 'mimo_random.override', { ack, reason });
		if (res) this.onStatus(res);
		return res !== null;
	}

	/**
	 * Take controls out of control at a failed gate, kept as monitors, with the operator's reason:
	 * the reference is reduced to the rest and the gate is judged again.
	 */
	async exclude(positions: number[], ack: string): Promise<boolean> {
		const res = await this.act<MimoStatus>('Exclude', 'mimo_random.exclude', { controls: positions, ack });
		if (res) this.onStatus(res);
		return res !== null;
	}

	/**
	 * Predict again during the review hold: with the law and parameters the draft now holds (the
	 * one that engages), or the run's own.
	 */
	async iterate(withDraftLaw: boolean): Promise<boolean> {
		const params: Record<string, unknown> = {};
		if (withDraftLaw && this.wire?.law) params.law = this.wire.law;
		const res = await this.act<{ prediction: MimoStatus['prediction'] }>('Predict Again', 'mimo_random.iterate_prediction', params);
		if (res && this.status) this.status = { ...this.status, prediction: res.prediction };
		return res !== null;
	}

	async stop(): Promise<boolean> {
		const res = await this.act<MimoStatus>('Stop', 'mimo_random.stop', {}, false);
		if (res && !regresses(this.status, res)) this.status = res;
		return res !== null;
	}

	async abort(): Promise<boolean> {
		const res = await this.act<MimoStatus>('Abort', 'mimo_random.abort', {}, false);
		if (res && !regresses(this.status, res)) this.status = res;
		return res !== null;
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
