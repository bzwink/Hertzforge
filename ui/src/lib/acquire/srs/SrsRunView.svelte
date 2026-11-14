<script lang="ts">
	/**
	 * The SRS shock run as it happens, on the test screen's frame: the pages of plots, and the dock
	 * with the Run card (the shared verbs and the shock's own: Fire, the train and the burst, the
	 * level moves), the level card with the law, the pulse table and the station. The SRS and Pulse
	 * plots lay the pulses picked in the pulse table over the latest one. From arm on the Run card
	 * names the run's folder, and a finished run's end line opens it in the Shock workspace. The
	 * window on another monitor shows the same, compact.
	 */
	import type { Snippet } from 'svelte';
	import LawLine from '../pulse/LawLine.svelte';
	import PulseRunView from '../pulse/PulseRunView.svelte';
	import type { RunOpen } from '../runOpen.svelte';
	import type { PlotPanel } from '../screen/screen.svelte';
	import type { CardKind } from '../screen/types';
	import ShockRunVerbs from '../shock/ShockRunVerbs.svelte';
	import SrsLevelCard from './SrsLevelCard.svelte';
	import SrsPulseTable from './SrsPulseTable.svelte';
	import type { SrsStore } from './srsStore.svelte';
	import type { SrsViewsState } from './srsViews.svelte';

	interface Props {
		srs: SrsStore;
		views: SrsViewsState;
		/** The station panel. */
		station?: Snippet;
		onpopout?: (pageId: string) => boolean;
		onoverride?: () => void;
		abortKeys: string;
		compact?: boolean;
		/** A finished run's way to its data on the analysis side; absent in a window that only watches. */
		opener?: RunOpen | null;
	}
	let { srs, views, station, onpopout, onoverride, abortKeys, compact = false, opener = null }: Props = $props();

	const status = $derived(srs.now);
	const state = $derived(srs.run?.state ?? null);
	const streaming = $derived(state === 'running' || state === 'review' || state === 'stopping' || state === 'finished');
	const limits = $derived(!!srs.runConfig?.limit_channels?.channels.length);
	const channels = $derived(srs.rows.filter((r) => r.enabled && r.role !== 'drive').map((r) => ({ index: r.index, name: r.description || r.role })));

	function empty(p: PlotPanel): string {
		if (!streaming) return 'Appears when a run starts.';
		switch (p.plot.quantity) {
			case 'srs':
				return "The pulse's SRS is drawn once one has fired.";
			case 'pulse':
				return 'The pulse is drawn once one has fired.';
			case 'velocity':
			case 'displacement':
				return 'Drawn once a pulse has fired.';
			case 'identification':
				return 'The identified gain is drawn once identification is done.';
			case 'limit_channels':
				return limits ? "Each limit channel's prediction is drawn once a pulse has fired." : 'This run has no limit channels.';
			case 'limit_frf':
				return limits ? "Each limit channel's gain is drawn once identification is done." : 'This run has no limit channels.';
			case 'components':
				return 'The component table is drawn once the run has started.';
			case 'law':
				return "The law's values are shown after its first call.";
			default:
				return 'Appears with the first frame.';
		}
	}
</script>

{#snippet verbs()}<ShockRunVerbs shock={srs} />{/snippet}

{#snippet cards(kind: CardKind)}
	{#if kind === 'level'}
		<LawLine law={status?.law} testid="srs-run-law" />
		<SrsLevelCard {status} planned={srs.planned} {compact} />
	{:else if kind === 'pulses'}
		<SrsPulseTable pulses={srs.pulses} picked={srs.overlay} ontoggle={(i) => void srs.toggleOverlay(i)} />
	{:else if kind === 'station' && station}
		{@render station()}
	{/if}
{/snippet}

<PulseRunView
	host={srs}
	screen={views}
	prefix="srs"
	attrs={{ 'data-state': status?.state ?? '', 'data-phase': status?.phase ?? '', 'data-pulses': status?.pulses.fired ?? 0 }}
	{verbs}
	review="The identified gain and its coherence are drawn; nothing fires until you proceed."
	{cards}
	{channels}
	{empty}
	live={streaming}
	{onpopout}
	{onoverride}
	{abortKeys}
	{compact}
	{opener}
/>
