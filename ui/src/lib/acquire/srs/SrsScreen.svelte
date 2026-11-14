<script lang="ts">
	/**
	 * The operator's screen for SRS shock, on the test screen's frame: Setup in sections with its
	 * summary beside every one, then Run with its pages of plots and the control dock. The header
	 * carries the pulses fired and passed, and the time to the next one, beside the run's state and
	 * level. The review hold after identification brings the page with the identified gain forward,
	 * once per hold.
	 */
	import { untrack } from 'svelte';
	import type { AppState } from '$lib/app/state.svelte';
	import ChannelTable from '../ChannelTable.svelte';
	import OverrideDialog from '../OverrideDialog.svelte';
	import StationPanel from '../StationPanel.svelte';
	import PulseScreen from '../pulse/PulseScreen.svelte';
	import ShockSchedule from '../shock/ShockSchedule.svelte';
	import { clock } from '../words';
	import SrsRecord from './SrsRecord.svelte';
	import SrsControl from './SrsControl.svelte';
	import SrsEnvelope from './SrsEnvelope.svelte';
	import SrsIdentification from './SrsIdentification.svelte';
	import SrsLimitChannels from './SrsLimitChannels.svelte';
	import SrsLimits from './SrsLimits.svelte';
	import SrsRunView from './SrsRunView.svelte';
	import SrsSummary from './SrsSummary.svelte';
	import SrsSynthesis from './SrsSynthesis.svelte';
	import SrsTarget from './SrsTarget.svelte';
	import { runChannels, SRS_SECTIONS, type SrsSection } from './srsSetup';

	interface Props {
		app: AppState;
	}
	let { app }: Props = $props();

	const srs = $derived(app.srs);
	const views = $derived(app.srsViews);
	const tests = $derived(app.tests);
	let overrideOpen = $state(false);

	const now = $derived(srs.now);
	const channels = $derived(runChannels(srs.rows));

	// The review hold brings the identified system forward, once per hold.
	let reviewed: string | null = null;
	$effect(() => {
		const s = srs.status;
		if (s?.state !== 'review' || !s.run_id || reviewed === s.run_id) return;
		reviewed = s.run_id;
		untrack(() => {
			srs.step = 'run';
			views.bringForward('identification');
		});
	});

	const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
	const abortKeys = isMac ? 'Cmd .' : 'Ctrl .';
</script>

{#snippet nums()}
	<div>
		<dt>Pulses</dt>
		<dd class="num" data-testid="run-pulses">{now ? `${now.pulses.fired} fired, ${now.pulses.passed} passed` : '-'}</dd>
	</div>
	{#if now?.next_pulse_s !== null && now?.next_pulse_s !== undefined && now.state === 'running'}
		<div>
			<dt>Next Pulse</dt>
			<dd class="num" data-testid="run-next">{clock(now.next_pulse_s)}</dd>
		</div>
	{/if}
{/snippet}

{#snippet setup(section: string)}
	{#if section === 'station'}
		<div class="stack">
			<StationPanel acq={srs.acq} />
			<p class="aq-small aq-muted" data-testid="srs-run-channels">The run controls on the table's first enabled control row ({channels.control ? `channel ${channels.control.index}` : 'none yet'}) and drives on its first enabled drive row ({channels.drive ? `channel ${channels.drive.index}` : 'none yet'}).</p>
			<ChannelTable acq={srs.acq} />
		</div>
	{:else if section === 'target'}
		<SrsTarget {srs} />
	{:else if section === 'synthesis'}
		<SrsSynthesis {srs} />
	{:else if section === 'envelope'}
		<SrsEnvelope {srs} />
	{:else if section === 'schedule'}
		<ShockSchedule shock={srs} trains={false} />
	{:else if section === 'identification'}
		<SrsIdentification {srs} />
	{:else if section === 'control'}
		<SrsControl {srs} />
	{:else if section === 'limits'}
		<SrsLimits {srs} />
	{:else if section === 'limit_channels'}
		<SrsLimitChannels {srs} />
	{:else}
		<SrsRecord {srs} />
	{/if}
{/snippet}

{#snippet summary(open: (id: string) => void)}
	<SrsSummary {srs} onopen={(s: SrsSection) => open(s)} />
{/snippet}

{#snippet station()}<StationPanel acq={srs.acq} />{/snippet}

{#snippet run()}
	<SrsRunView {srs} {views} {station} {abortKeys} onoverride={() => (overrideOpen = true)} onpopout={(id) => app.openDisplayWindow(id, 'srs')} opener={app.srsRunOpen} />
{/snippet}

<PulseScreen
	host={srs}
	screen={views}
	name="SRS"
	testid="srs-screen"
	ids="rk"
	sections={SRS_SECTIONS}
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
	acq={srs.acq}
	bind:open={overrideOpen}
	variant={{
		title: 'Arm Past the Shaker Ratings',
		lead: srs.status?.feasibility?.message ?? 'The synthesized pulse exceeds the shaker ratings that were given.',
		verb: 'Arming',
		confirm: 'Arm With This Acknowledgement',
		send: (text) => srs.arm(text),
		refusal: srs.refusal && srs.refusal.verb === 'Arm Past Feasibility' ? `${srs.refusal.title}: ${srs.refusal.message}` : null,
		busy: srs.busy !== null
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
