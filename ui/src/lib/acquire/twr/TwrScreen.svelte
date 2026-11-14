<script lang="ts">
	/**
	 * The operator's screen for time waveform replication, on the test screen's frame: Setup in
	 * sections with its summary beside every one, then Run with its pages of plots and the control
	 * dock. The header carries the plays judged and the playback beside the run's state and level.
	 * The review hold and the coherence hold bring the page with the identified system forward, once
	 * per hold.
	 */
	import { untrack } from 'svelte';
	import type { AppState } from '$lib/app/state.svelte';
	import ChannelTable from '../ChannelTable.svelte';
	import OverrideDialog from '../OverrideDialog.svelte';
	import StationPanel from '../StationPanel.svelte';
	import PulseScreen from '../pulse/PulseScreen.svelte';
	import TwrCapture from './TwrCapture.svelte';
	import TwrControl from './TwrControl.svelte';
	import TwrIdentification from './TwrIdentification.svelte';
	import TwrLimits from './TwrLimits.svelte';
	import TwrReference from './TwrReference.svelte';
	import TwrRunView from './TwrRunView.svelte';
	import TwrSchedule from './TwrSchedule.svelte';
	import TwrSummary from './TwrSummary.svelte';
	import { runChannels, TWR_SECTIONS, type TwrSetupSection } from './twrSetup';
	import { playbackWords } from './twrWords';

	interface Props {
		app: AppState;
	}
	let { app }: Props = $props();

	const twr = $derived(app.twr);
	const views = $derived(app.twrViews);
	const tests = $derived(app.tests);
	let overrideOpen = $state(false);

	const now = $derived(twr.now);
	const channels = $derived(runChannels(twr.rows));
	const playback = $derived(now ? playbackWords(now.plays, now.play) : null);

	// The review hold and the coherence hold bring the identified system forward, once per hold.
	let reviewed: string | null = null;
	$effect(() => {
		const s = twr.status;
		if ((s?.state !== 'review' && s?.state !== 'held') || !s.run_id || reviewed === `${s.run_id}:${s.state}`) return;
		reviewed = `${s.run_id}:${s.state}`;
		untrack(() => {
			twr.step = 'run';
			views.bringForward('frf');
		});
	});

	const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
	const abortKeys = isMac ? 'Cmd .' : 'Ctrl .';
</script>

{#snippet nums()}
	<div>
		<dt>Plays</dt>
		<dd class="num" data-testid="run-plays">{now ? `${now.plays.done} judged` : '-'}</dd>
	</div>
	{#if playback}
		<div>
			<dt>Playback</dt>
			<dd class="num" data-testid="run-playback">{playback.replace(/^Playback /, '')}</dd>
		</div>
	{/if}
{/snippet}

{#snippet setup(section: string)}
	{#if section === 'station'}
		<div class="stack">
			<StationPanel acq={twr.acq} />
			<p class="aq-small aq-muted" data-testid="twr-run-channels">The run controls on the table's first enabled control row ({channels.control ? `channel ${channels.control.index}, in ${channels.control.units}` : 'none yet'}) and drives on its first enabled drive row ({channels.drive ? `channel ${channels.drive.index}` : 'none yet'}). The reference is in the control row's unit.</p>
			<ChannelTable acq={twr.acq} />
		</div>
	{:else if section === 'reference'}
		<TwrReference {twr} />
	{:else if section === 'identification'}
		<TwrIdentification {twr} />
	{:else if section === 'control'}
		<TwrControl {twr} />
	{:else if section === 'schedule'}
		<TwrSchedule {twr} />
	{:else if section === 'limits'}
		<TwrLimits {twr} />
	{:else}
		<TwrCapture {twr} />
	{/if}
{/snippet}

{#snippet summary(open: (id: string) => void)}
	<TwrSummary {twr} onopen={(s: TwrSetupSection) => open(s)} />
{/snippet}

{#snippet station()}<StationPanel acq={twr.acq} />{/snippet}

{#snippet run()}
	<TwrRunView {twr} {views} {station} {abortKeys} onoverride={() => (overrideOpen = true)} onpopout={(id) => app.openDisplayWindow(id, 'twr')} opener={app.twrRunOpen} />
{/snippet}

<PulseScreen
	host={twr}
	screen={views}
	name="TWR"
	testid="twr-screen"
	ids="twr"
	sections={TWR_SECTIONS}
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
	acq={twr.acq}
	bind:open={overrideOpen}
	variant={{
		title: 'Arm Past a Feasibility Ceiling',
		lead: twr.status?.feasibility?.message ?? "The section's kinematics are over a feasibility ceiling that was set.",
		verb: 'Arming',
		confirm: 'Arm With This Acknowledgement',
		send: (text) => twr.arm(text),
		refusal: twr.refusal && twr.refusal.verb === 'Arm Past Feasibility' ? `${twr.refusal.title}: ${twr.refusal.message}` : null,
		busy: twr.busy !== null
	}}
/>

<style>
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
