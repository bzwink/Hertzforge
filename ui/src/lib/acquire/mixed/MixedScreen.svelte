<script lang="ts">
	/**
	 * The operator's screen for Mixed Mode, on the test screen's frame: Setup in sections with its
	 * summary beside every one (the station and channels, the floor, the tones and narrowbands, the
	 * schedule and identification, control, limits, run record), then Run with its pages of plots and the
	 * control dock. The header carries parts time beside the run's state and level. The review hold
	 * after identification brings the page with the identified gain forward, once per hold.
	 */
	import { limitingWords } from './mixedWords';
	import { untrack } from 'svelte';
	import type { AppState } from '$lib/app/state.svelte';
	import ViewRenderer from '$lib/viz/ViewRenderer.svelte';
	import SpectrumPane from '$lib/viz/SpectrumPane.svelte';
	import { MARKER_ORDER } from '$lib/viz/roles';
	import ChannelTable from '../ChannelTable.svelte';
	import OverrideDialog from '../OverrideDialog.svelte';
	import ProfileEditor from '../ProfileEditor.svelte';
	import StationPanel from '../StationPanel.svelte';
	import PulseScreen from '../pulse/PulseScreen.svelte';
	import MixedRecord from './MixedRecord.svelte';
	import { runChannels } from '../sine/sineSetup';
	import { clock } from '../words';
	import MixedControl from './MixedControl.svelte';
	import MixedLimits from './MixedLimits.svelte';
	import MixedPartsTable from './MixedPartsTable.svelte';
	import MixedRunView from './MixedRunView.svelte';
	import MixedSchedule from './MixedSchedule.svelte';
	import MixedSummary from './MixedSummary.svelte';
	import { MIXED_SECTIONS, type MixedSection } from './mixedSetup';

	interface Props {
		app: AppState;
	}
	let { app }: Props = $props();

	const mixed = $derived(app.mixed);
	const views = $derived(app.mixedViews);
	const tests = $derived(app.tests);
	let overrideOpen = $state(false);

	const now = $derived(mixed.now);
	const channels = $derived(runChannels(mixed.rows));
	const preview = $derived(mixed.check && mixed.checkedKey === mixed.key ? mixed.check.views : (mixed.served?.views ?? []));
	const reference = $derived(preview.find((v) => v.title === 'Reference') ?? null);
	/** The service's tone view; a tone that stands still is one point, repeated so its marker is drawn. */
	const toneView = $derived.by(() => {
		const v = preview.find((x) => x.title === 'Tones' && x.series.length);
		if (!v) return null;
		return { ...v, series: v.series.map((s) => (s.x.length === 1 ? { ...s, x: Float64Array.of(s.x[0], s.x[0]), y: Float64Array.of(s.y[0], s.y[0]) } : s)) };
	});
	const on = $derived(now?.parts.filter((p) => p.on && p.kind !== 'floor').length ?? 0);
	const limitingNow = $derived(now?.state === 'running' ? limitingWords(now) : null);

	let reviewed: string | null = null;
	$effect(() => {
		const s = mixed.status;
		if (s?.state !== 'review' || !s.run_id || reviewed === s.run_id) return;
		reviewed = s.run_id;
		untrack(() => {
			mixed.step = 'run';
			views.bringForward('identification');
		});
	});

	const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
	const abortKeys = isMac ? 'Cmd .' : 'Ctrl .';
</script>

{#snippet nums()}
	<div>
		<dt>Parts time</dt>
		<dd class="num" data-testid="run-parts-s">{now?.parts_s === null || now?.parts_s === undefined ? '-' : clock(now.parts_s)}</dd>
	</div>
	<div>
		<dt>At Level</dt>
		<dd class="num" data-testid="run-at-level">{now?.at_level_s === null || now?.at_level_s === undefined ? '-' : clock(now.at_level_s)}{#if now?.remaining_at_level_s !== null && now?.remaining_at_level_s !== undefined}{` of ${clock(now.at_level_s! + now.remaining_at_level_s)}`}{/if}</dd>
	</div>
	<div>
		<dt>Full Level</dt>
		<dd class="num" data-testid="run-full-level">{now?.full_level_s === null || now?.full_level_s === undefined ? '-' : clock(now.full_level_s)}</dd>
	</div>
	<div>
		<dt>Parts on</dt>
		<dd class="num" data-testid="run-parts-on">{on}</dd>
	</div>
	{#if limitingNow}
		<div>
			<dt>Limiting</dt>
			<dd class="limiting" data-testid="run-limiting"><svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true"><rect x="1.5" y="5" width="9" height="2" fill="currentColor" /></svg> {limitingNow}</dd>
		</div>
	{/if}
{/snippet}

{#snippet setup(section: string)}
	{#if section === 'station'}
		<div class="stack">
			<StationPanel acq={mixed.acq} />
			<p class="aq-small aq-muted" data-testid="mixed-run-channels">The run controls on the table's enabled control rows ({channels.control ? `the first is channel ${channels.control.index}` : 'none yet'}) and drives on its first enabled drive row ({channels.drive ? `channel ${channels.drive.index}` : 'none yet'}).</p>
			<ChannelTable acq={mixed.acq} />
		</div>
	{:else if section === 'floor'}
		<div class="stack">
			<ProfileEditor acq={mixed.floorHost} heading="Floor" sub="The broadband random every other part plays over, at full level. Its band is the test's band." fileName="Floor Profile" />
			<section class="aq-panel" aria-labelledby="mx-ref" data-testid="mixed-reference-preview">
				<div class="aq-head"><h2 id="mx-ref">Reference</h2><span class="aq-sub">The floor and each narrowband where it starts, at full level, as the service computed it.</span></div>
				{#if reference}<ViewRenderer view={reference} showTitle={false} minHeight={220} />{:else}<p class="aq-small aq-muted">{mixed.checking ? 'Checking the setup.' : 'Drawn once the setup checks clean.'}</p>{/if}
			</section>
		</div>
	{:else if section === 'parts'}
		<div class="stack">
			<MixedPartsTable {mixed} table="tones" />
			<MixedPartsTable {mixed} table="bands" />
			{#if toneView}
				<section class="aq-panel" aria-labelledby="mx-tones" data-testid="mixed-tones-preview">
					<div class="aq-head"><h2 id="mx-tones">Tone Amplitudes</h2><span class="aq-sub">Each tone's amplitude across its motion, at full level, as the service computed it.</span></div>
					<SpectrumPane view={toneView} showTitle={false} minHeight={200} styles={toneView.series.map((_, i) => ({ marker: MARKER_ORDER[i % MARKER_ORDER.length] }))} />
				</section>
			{/if}
		</div>
	{:else if section === 'schedule'}
		<MixedSchedule {mixed} />
	{:else if section === 'control'}
		<MixedControl {mixed} />
	{:else if section === 'limits'}
		<MixedLimits {mixed} />
	{:else}
		<MixedRecord {mixed} />
	{/if}
{/snippet}

{#snippet summary(open: (id: string) => void)}
	<MixedSummary {mixed} onopen={(s: MixedSection) => open(s)} />
{/snippet}

{#snippet station()}<StationPanel acq={mixed.acq} />{/snippet}

{#snippet run()}
	<MixedRunView {mixed} {views} {station} {abortKeys} onoverride={() => (overrideOpen = true)} onpopout={(id) => app.openDisplayWindow(id, 'mixed')} opener={app.mixedRunOpen} />
{/snippet}

<PulseScreen
	host={mixed}
	screen={views}
	name="Mixed Mode"
	testid="mixed-screen"
	ids="mixed"
	sections={MIXED_SECTIONS}
	{setup}
	{summary}
	{run}
	{nums}
	onleave={tests.canLeave ? () => tests.toChooser() : null}
	takeFocus={() => app.tests.takeFocus()}
	proceedTitle="Apply the setup and show the run: start identification from there."
	{abortKeys}
/>

<OverrideDialog
	acq={mixed.acq}
	bind:open={overrideOpen}
	variant={{
		title: 'Arm Past the Shaker Ratings',
		lead: mixed.status?.feasibility?.message ?? "The test exceeds the shaker's ratings that were given.",
		verb: 'Arming',
		confirm: 'Arm With This Acknowledgement',
		send: (text) => mixed.arm(text),
		refusal: mixed.refusal && mixed.refusal.verb === 'Arm Past Feasibility' ? `${mixed.refusal.title}: ${mixed.refusal.message}` : null,
		busy: mixed.busy !== null
	}}
/>

<style>
	.limiting {
		color: var(--amber-1);
		white-space: nowrap;
	}

	.stack {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		min-width: 0;
	}

	.stack p {
		margin: 0;
	}
</style>
