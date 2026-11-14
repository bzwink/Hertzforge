<script lang="ts">
	/**
	 * The Explorer run as it happens, on the test screen's frame: the pages of plots, and the dock
	 * with the Run card (the shared verbs and Explorer's own: Hold and Clear Survey), the level card,
	 * the steering and the station. A survey plot carries the tone's frequency as a line and the
	 * survey's highest point as a thin one, from the run's own numbers, with the two named under it.
	 * A stall shows as a line in the Run card. From start the Run card names the run's folder, and a
	 * finished run's end line offers Open in Analysis and Copy Folder Path (the shared pieces). The
	 * window on another monitor shows the same, compact.
	 */
	import type { Snippet } from 'svelte';
	import type { SpectrumView } from '$lib/protocol/types';
	import { formatNumber } from '$lib/viz/format';
	import PulseRunView from '../pulse/PulseRunView.svelte';
	import RunLine from '../pulse/RunLine.svelte';
	import type { RunOpen } from '../runOpen.svelte';
	import type { BodyPanel, PlotExtras } from '../screen/bodies';
	import type { PlotPanel } from '../screen/screen.svelte';
	import type { CardKind } from '../screen/types';
	import ExplorerLevelCard from './ExplorerLevelCard.svelte';
	import ExplorerRunVerbs from './ExplorerRunVerbs.svelte';
	import ExplorerSteer from './ExplorerSteer.svelte';
	import type { ExplorerStore } from './explorerStore.svelte';
	import type { ExplorerViewsState } from './explorerViews.svelte';
	import { hz } from './explorerWords';

	interface Props {
		explorer: ExplorerStore;
		views: ExplorerViewsState;
		/** The station panel. */
		station?: Snippet;
		onpopout?: (pageId: string) => boolean;
		abortKeys: string;
		compact?: boolean;
		/** A finished run's way to its data on the analysis side; absent in a window that only watches. */
		opener?: RunOpen | null;
	}
	let { explorer, views, station, onpopout, abortKeys, compact = false, opener = null }: Props = $props();

	const status = $derived(explorer.now);
	const live = $derived(explorer.frameOfRun);
	const state = $derived(status?.state ?? null);
	const streaming = $derived(state === 'starting' || state === 'running' || state === 'stopping' || state === 'finished');
	const peak = $derived(live?.peak ?? null);
	const channels = $derived(explorer.rows.filter((r) => r.enabled && r.role !== 'drive').map((r) => ({ index: r.index, name: r.description || r.role })));

	function empty(p: PlotPanel): string {
		if (!streaming) return 'Appears when a run starts.';
		switch (p.plot.quantity) {
			case 'survey':
				return 'The survey line is drawn once the tone has run at a level for a moment at one frequency.';
			case 'spectrum':
				return 'The spectra are drawn after the first analysis pass.';
			case 'time':
				return 'The samples are drawn after the first analysis pass.';
			default:
				return 'The readings are shown after the first analysis pass.';
		}
	}

	const surveyOf = (id: string) => (views.views(id).find((v) => v.view === 'spectrum') as SpectrumView | undefined) ?? null;
	const unitOf = (v: SpectrumView | null) => v?.y.unit ?? live?.channels.find((c) => c.index === live?.control?.index)?.unit ?? '';
</script>

{#snippet verbs()}<ExplorerRunVerbs {explorer} />{/snippet}

{#snippet notes()}
	{#if status?.stall}
		<RunLine tone="warn" testid="explorer-stall" head="Stalled" msg="{status.stall.message} Abort stays open." />
	{/if}
{/snippet}

{#snippet survey(panel: BodyPanel, draw: Snippet<[PlotExtras]>)}
	{@const v = surveyOf(panel.id)}
	<div class="survey" data-testid="explorer-survey-plot" data-points={v?.series[0]?.x.length ?? 0}>
		{@render draw({ marker: status?.frequency_hz ?? null, guides: peak ? [peak.hz] : [] })}
		{#if v?.series.length}
			<p class="aq-small marks" data-testid="explorer-survey-marks">
				The line at {hz(status?.frequency_hz)} is the tone{#if peak}; the thin line at {hz(peak.hz)} is the highest point, {formatNumber(peak.level, 4)} {unitOf(v)}{/if}.
			</p>
		{/if}
	</div>
{/snippet}

{#snippet cards(kind: CardKind)}
	{#if kind === 'level'}
		{#if status?.grid}
			<p class="grid-words aq-small num" data-testid="explorer-run-grid">Sample rate {formatNumber(status.grid.fs_hz, 6)} Hz{status.grid.actual && status.grid.fs_hz !== status.grid.requested_fs_hz ? ` (asked ${formatNumber(status.grid.requested_fs_hz, 6)})` : ''}</p>
		{/if}
		<ExplorerLevelCard {status} {live} {compact} />
	{:else if kind === 'steer'}
		<ExplorerSteer {explorer} />
	{:else if kind === 'station' && station}
		{@render station()}
	{/if}
{/snippet}

<PulseRunView
	host={explorer}
	screen={views}
	prefix="explorer"
	attrs={{
		'data-state': status?.state ?? '',
		'data-phase': status?.phase ?? '',
		'data-hz': status?.frequency_hz ?? '',
		'data-vpk': status?.drive_vpk ?? '',
		'data-t': live?.t_s ?? '',
		'data-sweep': status?.sweep ? `${status.sweep.direction}${status.sweep.halted ? ' halted' : ''}` : ''
	}}
	{verbs}
	{notes}
	{cards}
	bodies={{ survey }}
	{channels}
	{empty}
	live={streaming}
	{onpopout}
	{abortKeys}
	{compact}
	record={status?.record ?? null}
	{opener}
/>

<style>
	.survey {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-height: 0;
	}

	.marks {
		flex: none;
		margin: 0;
		padding: 0 4px;
		color: var(--text-2);
	}

	.grid-words {
		margin: 0 0 var(--sp-2);
		color: var(--text-2);
	}
</style>
