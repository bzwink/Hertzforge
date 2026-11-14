<script lang="ts">
	/**
	 * The MIMO run as it happens, in the single-output run view's frame: the page bar, the page's
	 * view, and beside it the run's state, the level card, the station panel and the drives. The run
	 * bar with its verbs (and Stop and Abort) and the station panel come from the MIMO screen
	 * around this view; the window on another monitor shows the same frame, compact. From arm on
	 * the run card names the folder the run is recorded in (the shared record line), and a
	 * finished run offers Open in Analysis and Copy Folder Path (the shared actions).
	 */
	import type { Snippet } from 'svelte';
	import type { LawEntry } from '$lib/protocol/mimo';
	import type { RandomState } from '$lib/protocol/types';
	import StatusMark from '$lib/viz/StatusMark.svelte';
	import { formatFixed } from '$lib/viz/format';
	import type { MimoGeometry } from '$lib/viz/geometry';
	import type { RowBand } from '$lib/viz/scales';
	import RunOpenActions from '../../RunOpenActions.svelte';
	import RunRecordLine from '../../RunRecordLine.svelte';
	import type { RunOpen } from '../../runOpen.svelte';
	import StateChip from '../../StateChip.svelte';
	import MimoLevelCard from './MimoLevelCard.svelte';
	import MimoPageBar from './MimoPageBar.svelte';
	import MimoPageView from './MimoPageView.svelte';
	import type { MimoViewsState } from './mimoViews.svelte';

	interface Props {
		views: MimoViewsState;
		/** The run bar (verbs, Stop and Abort), from the MIMO screen. */
		bar?: Snippet;
		/** The station panel, from the MIMO screen. */
		station?: Snippet;
		/** The page bar's right end (the way back to the setup). */
		end?: Snippet;
		onpopout?: (pageId: string) => boolean;
		geometry?: MimoGeometry | null;
		bands?: RowBand[];
		ceilings?: number[] | null;
		diagnostics?: LawEntry['diagnostics'];
		compact?: boolean;
		/** A finished run's way to its data on the analysis side; absent in a window that only watches. */
		opener?: RunOpen | null;
	}
	let { views, bar, station, end, onpopout, geometry = null, bands = [], ceilings = null, diagnostics = [], compact = false, opener = null }: Props = $props();

	const status = $derived(views.status);
	/** The state chip reads the run's state and its end, which MIMO's status carries in the same form. */
	const chipRun = $derived(status ? ({ state: status.state, end: status.end } as unknown as RandomState) : null);
	const ceil = $derived(ceilings ?? status?.prediction?.ceiling_vrms ?? []);
	/** Where the run is recorded, from arm on. */
	const record = $derived(status?.record ?? null);
	const ended = $derived(status?.state === 'finished');
</script>

<div class="live" class:compact data-testid="mimo-run-view" data-state={status?.state ?? ""} data-phase={status?.phase ?? ""} data-loop={status?.loop ?? 0}>
	{#if bar}{@render bar()}{/if}
	<MimoPageBar {views} {onpopout} {end} />

	<div class="grid">
		<MimoPageView {views} {geometry} {bands} {ceilings} {diagnostics} />
		<aside class="side" aria-label="Run numbers">
			<section class="card head">
				<StateChip run={chipRun} />
				{#if status?.law}<span class="law">{status.law.name} <span class="v">{status.law.version}</span></span>{/if}
				{#if status?.source}<span class="src">{status.source === 'SIM' ? 'Simulator' : status.source}</span>{/if}
			</section>
			<section class="card"><MimoLevelCard {status} {bands} {compact} /></section>
			{#if record}
				<section class="card rec" aria-label="Run record" data-testid="mimo-record">
					<RunRecordLine {record} {ended} />
					{#if ended && opener && !compact}<RunOpenActions {record} {opener} />{/if}
				</section>
			{/if}
			{#if station && !compact}
				<section class="card">{@render station()}</section>
			{/if}
			{#if status?.drive_rms_v.length && !compact}
				<section class="card" aria-label="Drives" data-testid="mimo-drives">
					<h3>Drives</h3>
					<table class="chans">
						<thead><tr><th>Drive</th><th class="r">V rms</th><th class="r">Ceiling</th><th class="r">Cap</th></tr></thead>
						<tbody>
							{#each status.shape?.drive_labels ?? [] as d, k (k)}
								{@const r = status.drive_rms_v[k]}
								{@const c = ceil[k]}
								{@const over = r !== undefined && c !== undefined && r > c}
								<tr class:over>
									<td>{d}</td>
									<td class="r num">{r === undefined ? '-' : formatFixed(r, 3)}</td>
									<td class="r num">{c === undefined ? '-' : formatFixed(c, 3)}</td>
									<td class="r num">
										{#if status.caps[k] !== undefined && status.caps[k] < 1}<StatusMark status="warn" word={false} />{/if}
										{status.caps[k] === undefined ? '-' : formatFixed(status.caps[k], 3)}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</section>
			{/if}
			{#if status?.faults.length}
				<section class="card faults" role="alert" data-testid="mimo-faults">
					<h3>Faults</h3>
					<ul>
						{#each status.faults as f, i (i)}<li><StatusMark status="fail" word={false} />{f.message}</li>{/each}
					</ul>
				</section>
			{/if}
		</aside>
	</div>
</div>

<style>
	.live {
		display: flex;
		flex: 1;
		flex-direction: column;
		gap: var(--sp-3);
		min-height: 0;
	}

	.grid {
		display: grid;
		flex: 1;
		grid-template-columns: minmax(0, 1fr) 320px;
		gap: var(--sp-3);
		min-height: 0;
	}

	.compact .grid {
		grid-template-columns: minmax(0, 1fr) 280px;
	}

	.side {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		min-width: 0;
		min-height: 0;
		overflow-y: auto;
	}

	.card {
		flex: none;
		padding: var(--sp-3);
		background: var(--bg-1);
		border: 1px solid var(--line-1);
		border-radius: var(--radius-lg);
	}

	.card h3 {
		margin-bottom: var(--sp-2);
		font-size: var(--text-sm);
		font-weight: var(--weight-strong);
	}

	.head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px var(--sp-3);
		font-size: var(--text-xs);
		color: var(--text-2);
	}

	.law {
		color: var(--text-1);
	}

	.v {
		color: var(--text-3);
	}

	.src {
		margin-left: auto;
	}

	.chans {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--text-xs);
	}

	.chans th {
		padding: 3px 4px;
		font-size: var(--text-2xs);
		font-weight: var(--weight-strong);
		color: var(--text-3);
		text-align: left;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		border-bottom: 1px solid var(--line-2);
	}

	.chans td {
		padding: 3px 4px;
		border-bottom: 1px solid var(--line-1);
	}

	.chans .r {
		text-align: right;
	}

	tr.over td {
		font-weight: var(--weight-strong);
		background: var(--amber-weak);
	}

	.faults ul {
		margin: 0;
		padding: 0;
		list-style: none;
		font-size: var(--text-sm);
	}

	.faults li {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	@media (max-width: 1100px) {
		.grid {
			grid-template-columns: 1fr;
		}
	}
</style>
