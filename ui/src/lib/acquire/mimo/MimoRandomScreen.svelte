<script lang="ts">
	/**
	 * The operator's screen for MIMO Random, in the format single-output Random set: a link back
	 * to the test-type chooser, three steps (Setup, Predict, Run), the run bar always in view, and
	 * the setup in sections (Channels, Reference, Control, Schedule, Run Record, Backend) with the setup
	 * summary beside every section, so whatever is missing is one click away. After
	 * identification the service holds for the prediction, which comes forward by itself; engaging
	 * brings the run forward.
	 */
	import { onMount } from 'svelte';
	import '$lib/acquire/acquire.css';
	import type { AppState } from '$lib/app/state.svelte';
	import Findings from '../Findings.svelte';
	import Notices from '../Notices.svelte';
	import RunBar from '../RunBar.svelte';
	import RunRecordEditor from '../RunRecordEditor.svelte';
	import ScheduleEditor from '../ScheduleEditor.svelte';
	import MimoBackend from './MimoBackend.svelte';
	import MimoChannels from './MimoChannels.svelte';
	import MimoControl from './MimoControl.svelte';
	import MimoPredict from './MimoPredict.svelte';
	import MimoReference from './MimoReference.svelte';
	import StationPanel from '../StationPanel.svelte';
	import { runBandsOf, runGeometryOf } from './runFrame';
	import MimoRunView from './views/MimoRunView.svelte';
	import MimoSpace from './MimoSpace.svelte';
	import MimoSummary from './MimoSummary.svelte';
	import type { MimoSection } from './mimoSetup';

	interface Props {
		app: AppState;
	}
	let { app }: Props = $props();

	const mimo = $derived(app.mimo);
	const tests = $derived(app.tests);
	let heading: HTMLElement;

	onMount(() => {
		if (app.tests.takeFocus()) heading.focus();
	});

	type SetupSection = Exclude<MimoSection, 'predict'>;
	const SECTIONS: { id: SetupSection; label: string }[] = [
		{ id: 'channels', label: 'Channels' },
		{ id: 'reference', label: 'Reference' },
		{ id: 'control', label: 'Control' },
		{ id: 'schedule', label: 'Schedule' },
		{ id: 'record', label: 'Run Record' },
		{ id: 'backend', label: 'Backend' }
	];

	const tabs = $derived(SECTIONS.map((s) => ({ ...s, errors: mimo.counts[s.id].errors, warnings: mimo.counts[s.id].warnings })));
	const section = $derived((mimo.section === 'predict' ? 'channels' : mimo.section) as SetupSection);
	const step = $derived(mimo.step);
	const review = $derived(mimo.status?.state === 'review');
	/** What the run views draw against: each controlled quantity's bands, and where the sensors are. */
	const runBands = $derived(runBandsOf(mimo.status));
	const geometry = $derived(runGeometryOf(mimo.setup, mimo.status));
	const liveDot = $derived(mimo.status?.state === 'running' || mimo.status?.state === 'stopping');

	/** Why Proceed to Run waits, or null. An observer only looks, so it never waits. */
	const proceedWhy = $derived.by(() => {
		if (!mimo.isController || mimo.isLive) return null;
		const a = mimo.actions.apply;
		if (mimo.setupStatus === 'applied' || mimo.setupStatus === 'unread') return null;
		return a.enabled ? null : a.reason;
	});
	const flowNote = $derived.by(() => {
		if (mimo.isLive) return { tone: 'info', text: 'The run is live; the setup is locked until it ends.' };
		if (!mimo.isController) return { tone: 'info', text: 'Observing: the run the service holds is shown.' };
		if (proceedWhy) return { tone: 'warn', text: proceedWhy };
		if (mimo.setupStatus === 'applied') return { tone: 'pass', text: 'The service holds this setup.' };
		const w = Object.values(mimo.counts).reduce((n, c) => n + c.warnings, 0);
		return { tone: 'info', text: `The setup checks clean${w ? `, with ${w} ${w === 1 ? 'warning' : 'warnings'}` : ''}. Proceed to Run applies it.` };
	});
	let proceeding = $state(false);

	async function proceed() {
		if (proceedWhy || proceeding) return;
		if (mimo.isController && !mimo.isLive && mimo.setupStatus !== 'applied' && mimo.setupStatus !== 'unread' && mimo.actions.apply.enabled) {
			proceeding = true;
			const ok = await mimo.apply();
			proceeding = false;
			if (!ok) return;
		}
		mimo.step = 'run';
	}

	function open(s: SetupSection | MimoSection) {
		mimo.step = 'setup';
		mimo.section = s;
	}

	let tablist = $state<HTMLElement | null>(null);
	let body = $state<HTMLElement | null>(null);

	$effect(() => {
		void mimo.section;
		void mimo.step;
		body?.scrollTo({ top: 0 });
	});

	function onTabKey(e: KeyboardEvent) {
		const ids = SECTIONS.map((t) => t.id);
		const i = ids.indexOf(section);
		let next = -1;
		if (e.key === 'ArrowRight') next = (i + 1) % ids.length;
		else if (e.key === 'ArrowLeft') next = (i - 1 + ids.length) % ids.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = ids.length - 1;
		if (next < 0) return;
		e.preventDefault();
		open(ids[next]);
		tablist?.querySelector<HTMLButtonElement>(`[data-tab="${ids[next]}"]`)?.focus();
	}

	const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
	const abortKeys = isMac ? 'Cmd .' : 'Ctrl .';
	const STEPS = [
		{ id: 'setup' as const, n: 1, label: 'Setup' },
		{ id: 'predict' as const, n: 2, label: 'Predict' },
		{ id: 'run' as const, n: 3, label: 'Run' }
	];
</script>

<div class="acquire" data-testid="mimo-screen">
	<div class="top">
		<div class="crumb">
			{#if tests.canLeave && mimo.status?.state !== 'review'}
				<button class="btn ghost sm back" data-testid="test-type-back" onclick={() => tests.toChooser()}>
					<svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true"><path d="M7.5 2.5L4 6l3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
					Test type
				</button>
				<span class="sep" aria-hidden="true">/</span>
			{/if}
			<h1 class="here" tabindex="-1" bind:this={heading}>MIMO Random</h1>
			<ol class="flow" aria-label="Steps">
				{#each STEPS as st, i (st.id)}
					{#if i > 0}
						<li aria-hidden="true" class="arrow"><svg viewBox="0 0 12 12" width="10" height="10"><path d="M4.5 2.5L8 6l-3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg></li>
					{/if}
					<li>
						<button
							class="step"
							class:on={step === st.id}
							aria-current={step === st.id ? 'step' : undefined}
							data-testid="flow-{st.id}"
							onclick={() => (st.id === 'run' && step === 'setup' ? proceed() : (mimo.step = st.id))}
						>
							<span class="n">{st.n}</span>{st.label}
							{#if st.id === 'predict' && review}<span class="mark wait" aria-label="waiting for you"><svg viewBox="0 0 10 10" width="8" height="8" aria-hidden="true"><rect x="1.5" y="1" width="2.4" height="8" rx="0.6" fill="currentColor" /><rect x="6.1" y="1" width="2.4" height="8" rx="0.6" fill="currentColor" /></svg></span>{/if}
							{#if st.id === 'run' && liveDot}<span class="mark dot" aria-label="running"><svg viewBox="0 0 10 10" width="8" height="8" aria-hidden="true"><circle cx="5" cy="5" r="4" fill="currentColor" /></svg></span>{/if}
						</button>
					</li>
				{/each}
			</ol>
		</div>
		<RunBar acq={mimo} onoverride={() => (mimo.step = 'predict')} {abortKeys} />
		<Notices acq={mimo} />
	</div>

	{#if step === 'setup'}
		<div class="tabs" role="tablist" aria-label="MIMO Random setup sections" bind:this={tablist} tabindex="-1" onkeydown={onTabKey}>
			{#each tabs as t (t.id)}
				<button role="tab" id="mm-tab-{t.id}" data-tab={t.id} data-testid="tab-{t.id}" aria-selected={section === t.id} aria-controls="mm-panel" tabindex={section === t.id ? 0 : -1} class:on={section === t.id} onclick={() => open(t.id)}>
					{t.label}
					{#if t.errors > 0}
						<span class="mark err" title="{t.errors} {t.errors === 1 ? 'error' : 'errors'}">
							<svg viewBox="0 0 14 14" width="11" height="11" aria-hidden="true"><path d="M4.5 1h5L13 4.5v5L9.5 13h-5L1 9.5v-5z" fill="currentColor" /><path d="M5 5l4 4M9 5l-4 4" stroke="var(--status-fail-text)" stroke-width="1.7" stroke-linecap="round" /></svg>
							<span class="num">{t.errors}</span><span class="visually-hidden">{t.errors === 1 ? 'error' : 'errors'}</span>
						</span>
					{:else if t.warnings > 0}
						<span class="mark warn" title="{t.warnings} {t.warnings === 1 ? 'warning' : 'warnings'}">
							<svg viewBox="0 0 14 14" width="11" height="11" aria-hidden="true"><path d="M7 1.6l5.6 10H1.4z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" /></svg>
							<span class="num">{t.warnings}</span><span class="visually-hidden">{t.warnings === 1 ? 'warning' : 'warnings'}</span>
						</span>
					{/if}
				</button>
			{/each}
			{#if mimo.locked}
				<span class="locked" data-testid="setup-locked">
					<svg viewBox="0 0 14 14" width="12" height="12" aria-hidden="true"><rect x="3" y="6.5" width="8" height="6" rx="1" fill="currentColor" /><path d="M4.8 6.5V4.8a2.2 2.2 0 014.4 0v1.7" fill="none" stroke="currentColor" stroke-width="1.4" /></svg>
					Locked while the run is live
				</span>
			{/if}
		</div>

		<div class="body" bind:this={body} id="mm-panel" role="tabpanel" aria-labelledby="mm-tab-{section}" tabindex="-1">
			<div class="withsum">
				<div class="main">
					{#if section === 'channels'}
						<div class="stack">
							<section class="aq-panel titlep">
								<label for="mm-title" class="lbl">Test title</label>
								<input id="mm-title" class="field" bind:value={mimo.setup.title} disabled={mimo.locked} aria-invalid={(mimo.fieldErrors.title ?? []).length > 0} data-testid="mimo-title" />
								<Findings items={(mimo.fieldErrors.title ?? []).map((message) => ({ level: 'error' as const, message }))} />
							</section>
							<MimoChannels {mimo} />
							<MimoSpace {mimo} />
						</div>
					{:else if section === 'reference'}
						<MimoReference {mimo} onanalyze={() => app.go('signals')} />
					{:else if section === 'control'}
						<MimoControl {mimo} />
					{:else if section === 'schedule'}
						<div class="narrow">
							<ScheduleEditor acq={mimo} />
						</div>
					{:else if section === 'record'}
						<div class="narrow">
							<RunRecordEditor acq={mimo} kept="The setup, the channel table, the log, the reference, the identification, every loop's spectra and the law's calls are still kept." />
						</div>
					{:else}
						<MimoBackend {mimo} />
					{/if}
				</div>
				<MimoSummary {mimo} onopen={open} />
			</div>
		</div>

		<footer class="flowbar" data-testid="setup-footer">
			<p class="flownote {flowNote.tone}" data-testid="flow-check" aria-live="polite">
				{#if flowNote.tone === 'warn'}
					<svg viewBox="0 0 14 14" width="12" height="12" aria-hidden="true"><path d="M7 1.6l5.6 10H1.4z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" /><path d="M7 5.4v3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg>
				{:else if flowNote.tone === 'pass'}
					<svg viewBox="0 0 14 14" width="12" height="12" aria-hidden="true"><path d="M2.5 7.3l2.8 2.8 6-6.3" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" /></svg>
				{/if}
				{flowNote.text}
			</p>
			<button class="btn primary" data-testid="proceed-to-run" aria-disabled={!!proceedWhy || proceeding} class:off={!!proceedWhy} title={proceedWhy ?? 'Apply the setup and show the run: start identification from there.'} onclick={proceed}>
				Proceed to Run
				<svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true"><path d="M4.5 2.5L8 6l-3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
			</button>
		</footer>
	{:else if step === 'predict'}
		<div class="body" bind:this={body} data-testid="predict-step">
			<MimoPredict {mimo} />
		</div>
	{:else}
		<div class="body fit" bind:this={body} data-testid="run-view">
			<MimoRunView views={app.mimoViews} onpopout={(id) => app.openDisplayWindow(id, 'mimo_random')} {geometry} bands={runBands} diagnostics={mimo.law?.diagnostics ?? []} opener={app.mimoRunOpen}>
				{#snippet station()}<StationPanel acq={mimo.acq} />{/snippet}
				{#snippet end()}
					<button class="btn sm" data-testid="back-to-setup" onclick={() => (mimo.step = 'setup')} title={mimo.isLive ? 'Look at the setup. It stays locked while the run is live.' : 'Back to the setup.'}>
						<svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true"><path d="M7.5 2.5L4 6l3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
						Back to Setup
					</button>
				{/snippet}
			</MimoRunView>
		</div>
	{/if}
</div>

<style>
	.acquire {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
	}

	.top {
		display: flex;
		flex-direction: column;
		gap: var(--sp-2);
		padding: var(--sp-2) var(--sp-5) 0;
	}

	.crumb {
		display: flex;
		align-items: center;
		gap: var(--sp-1);
		min-height: 24px;
		min-width: 0;
	}

	.crumb .back {
		gap: 4px;
		margin-left: calc(-1 * var(--sp-2));
		color: var(--text-2);
	}

	.crumb .sep {
		color: var(--text-4);
	}

	.here {
		font-size: var(--text-md);
		font-weight: var(--weight-strong);
		color: var(--text-1);
	}

	.here:focus {
		outline: none;
	}

	.flow {
		display: flex;
		align-items: center;
		gap: 2px;
		margin: 0 0 0 var(--sp-3);
		padding: 0;
		list-style: none;
	}

	.flow .arrow {
		display: inline-flex;
		color: var(--text-4);
	}

	.step {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		height: 26px;
		padding: 0 10px 0 4px;
		font-size: var(--text-sm);
		font-weight: var(--weight-medium);
		color: var(--text-3);
		background: transparent;
		border: 1px solid transparent;
		border-radius: 13px;
	}

	.step:hover {
		color: var(--text-1);
		background: var(--bg-2);
	}

	.step .n {
		display: inline-grid;
		place-items: center;
		width: 18px;
		height: 18px;
		font-size: var(--text-2xs);
		font-weight: var(--weight-strong);
		border: 1.3px solid currentColor;
		border-radius: 50%;
	}

	.step.on {
		color: var(--text-1);
		font-weight: var(--weight-strong);
		background: var(--blue-weak);
		border-color: var(--blue-line);
	}

	.step.on .n {
		color: var(--bg-1);
		background: var(--blue-2);
		border-color: var(--blue-2);
	}

	.tabs {
		display: flex;
		align-items: flex-end;
		gap: 2px;
		margin: var(--sp-3) var(--sp-5) 0;
		border-bottom: 1px solid var(--line-2);
		overflow-x: auto;
		scrollbar-width: none;
	}

	.tabs button {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 6px;
		height: 34px;
		padding: 0 14px;
		font-size: var(--text-sm);
		font-weight: var(--weight-medium);
		color: var(--text-3);
		background: transparent;
		border: 0;
		border-radius: var(--radius-md) var(--radius-md) 0 0;
		white-space: nowrap;
	}

	.tabs button:hover {
		color: var(--text-1);
		background: var(--bg-2);
	}

	.tabs button.on {
		color: var(--text-1);
		font-weight: var(--weight-strong);
	}

	.tabs button.on::after {
		content: '';
		position: absolute;
		left: 8px;
		right: 8px;
		bottom: -1px;
		height: 3px;
		background: var(--blue-2);
		border-radius: 2px 2px 0 0;
	}

	.mark {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-size: var(--text-2xs);
	}

	.mark.err {
		color: var(--amber-1);
	}

	.mark.warn {
		color: var(--amber-2);
	}

	.mark.dot {
		color: var(--blue-2);
		animation: pulse 1.2s ease-in-out infinite;
	}

	.mark.wait {
		color: var(--blue-2);
	}

	.locked {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		margin: 0 0 8px auto;
		padding-left: var(--sp-3);
		font-size: var(--text-xs);
		color: var(--text-2);
		white-space: nowrap;
	}

	.body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: var(--sp-3) var(--sp-5) var(--sp-6);
	}

	.body:focus {
		outline: none;
	}

	/* The run fills the window when there is room, so the pages and the numbers are all in view
	   during a run; on a small window it scrolls like the rest. */
	@media (min-height: 700px) and (min-width: 1101px) {
		.body.fit {
			display: flex;
			flex-direction: column;
			overflow: hidden;
			padding-bottom: var(--sp-3);
		}
	}

	.withsum {
		display: grid;
		grid-template-columns: minmax(0, 1fr) 300px;
		gap: var(--sp-3);
		align-items: start;
	}

	.main {
		min-width: 0;
	}

	.stack {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		min-width: 0;
	}

	.titlep {
		display: grid;
		grid-template-columns: auto minmax(0, 420px);
		align-items: center;
		gap: 4px var(--sp-3);
	}

	.titlep :global(.aq-findings) {
		grid-column: 2;
	}

	.lbl {
		font-size: var(--text-xs);
		font-weight: var(--weight-medium);
		color: var(--text-2);
	}

	.narrow {
		max-width: 720px;
	}

	.back {
		margin-top: var(--sp-3);
	}

	.flowbar {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding: var(--sp-2) var(--sp-5);
		background: var(--bg-1);
		border-top: 1px solid var(--line-1);
	}

	.flownote {
		display: inline-flex;
		flex: 1;
		align-items: center;
		gap: 6px;
		min-width: 0;
		font-size: var(--text-sm);
		color: var(--text-2);
	}

	.flownote.warn {
		font-weight: var(--weight-strong);
		color: var(--amber-1);
	}

	.flownote.pass {
		color: var(--blue-1);
	}

	.flowbar .btn.off {
		opacity: 0.55;
		cursor: default;
	}

	@keyframes pulse {
		50% {
			opacity: 0.35;
		}
	}

	@media (max-width: 1100px) {
		.withsum {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	@media (max-width: 760px) {
		.top,
		.body {
			padding-left: var(--sp-3);
			padding-right: var(--sp-3);
		}
		.tabs {
			margin-left: var(--sp-3);
			margin-right: var(--sp-3);
		}
	}
</style>
