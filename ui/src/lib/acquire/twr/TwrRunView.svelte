<script lang="ts">
	/**
	 * The TWR run as it happens, on the test screen's frame: the pages of plots, and the dock with the
	 * Run card (the shared verbs and TWR's own: Play, Iterate, Freeze and the level moves), the level
	 * card with the law, the play table and the station. The plots are the service's views as they
	 * come: the last play's records against time over the record, the identification against
	 * frequency, each play's error against the play index. The coherence hold shows as a line in the
	 * Run card. From arm on the Run card names the run's folder, and a finished run's end line opens
	 * it in Transient Analysis. The window on another monitor shows the same, compact.
	 */
	import type { Snippet } from 'svelte';
	import LawLine from '../pulse/LawLine.svelte';
	import PulseRunView from '../pulse/PulseRunView.svelte';
	import RunLine from '../pulse/RunLine.svelte';
	import type { RunOpen } from '../runOpen.svelte';
	import type { PlotPanel } from '../screen/screen.svelte';
	import type { CardKind } from '../screen/types';
	import TwrLevelCard from './TwrLevelCard.svelte';
	import TwrPlayTable from './TwrPlayTable.svelte';
	import TwrRunVerbs from './TwrRunVerbs.svelte';
	import type { TwrStore } from './twrStore.svelte';
	import type { TwrViewsState } from './twrViews.svelte';

	interface Props {
		twr: TwrStore;
		views: TwrViewsState;
		/** The station panel. */
		station?: Snippet;
		onpopout?: (pageId: string) => boolean;
		onoverride?: () => void;
		abortKeys: string;
		compact?: boolean;
		/** A finished run's way to its data on the analysis side; absent in a window that only watches. */
		opener?: RunOpen | null;
	}
	let { twr, views, station, onpopout, onoverride, abortKeys, compact = false, opener = null }: Props = $props();

	const status = $derived(twr.now);
	const state = $derived(status?.state ?? null);
	const streaming = $derived(state === 'running' || state === 'review' || state === 'held' || state === 'stopping' || state === 'finished');
	const progress = $derived(status?.play && status.play.frames > 0 ? status.play.frames_done / status.play.frames : null);
	const channels = $derived(twr.rows.filter((r) => r.enabled && r.role !== 'drive').map((r) => ({ index: r.index, name: r.description || r.role })));

	function empty(p: PlotPanel): string {
		if (!streaming) return 'Appears when a run starts.';
		switch (p.plot.quantity) {
			case 'response':
				return 'The response is drawn once a play has been judged.';
			case 'error':
				return 'The error is drawn once a play has been judged.';
			case 'target':
				return 'The target is drawn once the run has a level.';
			case 'drive':
				return 'The drive is drawn once a play has been judged.';
			case 'envelope':
				return 'The envelope is drawn once a play has been judged.';
			case 'scope':
				return 'The control channel is drawn while a play is in flight.';
			case 'spectra':
				return 'The spectra are drawn once a play has been judged.';
			case 'frf':
			case 'coherence':
			case 'mask':
				return 'Drawn once identification is done.';
			case 'convergence':
				return "Each play's error is drawn once a play has been judged.";
			case 'law':
				return "The law's values are shown after its first call.";
			default:
				return 'Appears with the first frame.';
		}
	}
</script>

{#snippet verbs()}<TwrRunVerbs {twr} />{/snippet}

{#snippet notes()}
	{#if status?.state === 'held'}
		<RunLine tone="warn" testid="twr-held" head="Held at the Coherence Check" msg="The identification's mean coherence stayed under its floor, so nothing drives. New Run releases the hold; raise the identification drive in the setup first if it was too low." />
	{/if}
{/snippet}

{#snippet cards(kind: CardKind)}
	{#if kind === 'level'}
		<LawLine law={status?.law} testid="twr-run-law" />
		<TwrLevelCard {status} gatePct={twr.gatePct} {compact} />
	{:else if kind === 'plays'}
		<TwrPlayTable plays={twr.plays} law={twr.runLaw} record={twr.record} busy={twr.recordBusy} ontoggle={(i) => void twr.toggleRecord(i)} />
	{:else if kind === 'station' && station}
		{@render station()}
	{/if}
{/snippet}

<PulseRunView
	host={twr}
	screen={views}
	prefix="twr"
	attrs={{
		'data-state': status?.state ?? '',
		'data-phase': status?.phase ?? '',
		'data-plays': status?.plays.done ?? 0,
		'data-t': status?.t_s ?? '',
		'data-level': status?.level_db ?? '',
		'data-progress': progress === null ? '' : progress.toFixed(4),
		'data-frozen': status?.frozen ? 'true' : 'false'
	}}
	{verbs}
	review="The identified system, its coherence and the bins the correction trusts are drawn; nothing plays until you proceed."
	{notes}
	{cards}
	{channels}
	{empty}
	live={streaming}
	{onpopout}
	{onoverride}
	{abortKeys}
	{compact}
	record={twr.runRecord}
	{opener}
/>
