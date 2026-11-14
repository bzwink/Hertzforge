<script lang="ts">
	/**
	 * The operator's screen for Explorer, on the test screen's frame: Setup in sections with its
	 * summary beside every one, then Run with its pages of plots and the control dock, where the
	 * steering sits. The header carries the tone's frequency, the drive and the sweep beside the
	 * run's state.
	 */
	import type { AppState } from '$lib/app/state.svelte';
	import ChannelTable from '../ChannelTable.svelte';
	import StationPanel from '../StationPanel.svelte';
	import PulseScreen from '../pulse/PulseScreen.svelte';
	import ExplorerCapture from './ExplorerCapture.svelte';
	import ExplorerRunView from './ExplorerRunView.svelte';
	import ExplorerSetup from './ExplorerSetup.svelte';
	import ExplorerSummary from './ExplorerSummary.svelte';
	import { EXPLORER_SECTIONS, explorerChannels, type ExplorerSection } from './explorerSetup';
	import { hz, sweepWords, vpk } from './explorerWords';

	interface Props {
		app: AppState;
	}
	let { app }: Props = $props();

	const explorer = $derived(app.explorer);
	const views = $derived(app.explorerViews);
	const tests = $derived(app.tests);

	const now = $derived(explorer.now);
	const channels = $derived(explorerChannels(explorer.rows));
	const sweep = $derived(sweepWords(now?.sweep ?? null, now?.frequency_hz ?? null, now?.band_hz ?? null));

	const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
	const abortKeys = isMac ? 'Cmd .' : 'Ctrl .';
</script>

{#snippet nums()}
	<div>
		<dt>Frequency</dt>
		<dd class="num" data-testid="run-frequency">{hz(now?.frequency_hz)}</dd>
	</div>
	<div>
		<dt>Drive</dt>
		<dd class="num" data-testid="run-drive">{vpk(now?.drive_vpk)}</dd>
	</div>
	{#if sweep}
		<div>
			<dt>Sweep</dt>
			<dd class="num" data-testid="run-sweep">{sweep}</dd>
		</div>
	{/if}
{/snippet}

{#snippet setup(section: string)}
	{#if section === 'station'}
		<div class="stack">
			<StationPanel acq={explorer.acq} />
			<p class="aq-small aq-muted" data-testid="explorer-run-channels">The run drives the table's one enabled drive row ({channels.drive ? `channel ${channels.drive.index}` : 'none yet'}) and reads the survey from its control row: the first enabled control row, else the first enabled input ({channels.control ? `channel ${channels.control.index}` : 'none yet'}). Every enabled input is acquired, and each row's abort limits are the run's.</p>
			<ChannelTable acq={explorer.acq} />
		</div>
	{:else if section === 'test'}
		<ExplorerSetup {explorer} />
	{:else}
		<ExplorerCapture {explorer} />
	{/if}
{/snippet}

{#snippet summary(open: (id: string) => void)}
	<ExplorerSummary {explorer} onopen={(s: ExplorerSection) => open(s)} />
{/snippet}

{#snippet station()}<StationPanel acq={explorer.acq} />{/snippet}

{#snippet run()}
	<ExplorerRunView {explorer} {views} {station} {abortKeys} onpopout={(id) => app.openDisplayWindow(id, 'explorer')} opener={app.explorerRunOpen} />
{/snippet}

<PulseScreen
	host={explorer}
	screen={views}
	name="Explorer"
	testid="explorer-screen"
	ids="explorer"
	sections={EXPLORER_SECTIONS}
	{setup}
	{summary}
	{run}
	{nums}
	onleave={tests.canLeave ? () => tests.toChooser() : null}
	takeFocus={() => app.tests.takeFocus()}
	liveNote="The run is live; the setup is locked until it ends. The steering steps can still change."
	proceedTitle="Apply the setup and show the run: start the tone from there."
	{abortKeys}
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
