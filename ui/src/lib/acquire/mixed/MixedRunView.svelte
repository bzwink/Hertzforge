<script lang="ts">
	/**
	 * The Mixed Mode run as it happens, on the test screen's frame: the pages of plots, and the dock
	 * with the Run card (the shared verbs and Hold and Resume), the level card, the parts table (which
	 * also switches a part on or off), the tone law's live parameters and the station. Every plot is
	 * the service's view as it comes: the spectra stepped across each line's bin, the per-part
	 * histories against station time. The window on another monitor shows the same, compact.
	 */
	import { untrack, type Snippet } from 'svelte';
	import PulseRunView from '../pulse/PulseRunView.svelte';
	import type { PlotPanel } from '../screen/screen.svelte';
	import type { CardKind } from '../screen/types';
	import type { RunOpen } from '../runOpen.svelte';
	import SineLiveParams from '../sine/SineLiveParams.svelte';
	import MixedLevelCard from './MixedLevelCard.svelte';
	import MixedLogCard from './MixedLogCard.svelte';
	import MixedInputsCard from './MixedInputsCard.svelte';
	import MixedPartsCard from './MixedPartsCard.svelte';
	import MixedRunVerbs from './MixedRunVerbs.svelte';
	import type { MixedStore } from './mixedStore.svelte';
	import type { MixedViewsState } from './mixedViews.svelte';

	interface Props {
		mixed: MixedStore;
		views: MixedViewsState;
		station?: Snippet;
		onpopout?: (pageId: string) => boolean;
		onoverride?: () => void;
		abortKeys: string;
		compact?: boolean;
		/** A finished run's way to its data on the analysis side; absent in a window that only watches. */
		opener?: RunOpen | null;
	}
	let { mixed, views, station, onpopout, onoverride, abortKeys, compact = false, opener = null }: Props = $props();

	const status = $derived(mixed.now);
	const live = $derived(mixed.frameOfRun);
	const state = $derived(mixed.run?.state ?? null);
	const streaming = $derived(state === 'running' || state === 'review' || state === 'stopping' || state === 'finished');
	const limits = $derived(!!mixed.runConfig?.limits.channels?.length);
	const channels = $derived(mixed.rows.filter((r) => r.enabled && r.role !== 'drive').map((r) => ({ index: r.index, name: r.description || r.role })));

	// The plots' part choice offers the run's tones and narrowbands (the setup's before a run).
	$effect(() => {
		const list = mixed.parts.length
			? mixed.parts.filter((p) => p.kind !== 'floor').map((p) => ({ id: p.id, name: p.name, kind: p.kind as 'tone' | 'narrowband' }))
			: [...mixed.setup.tones.map((t) => ({ id: t.id, name: t.name, kind: 'tone' as const })), ...mixed.setup.bands.map((b) => ({ id: b.id, name: b.name, kind: 'narrowband' as const }))];
		untrack(() => (views.partChoices = list));
	});

	function empty(p: PlotPanel): string {
		if (!streaming) return 'Appears when a run starts.';
		switch (p.plot.quantity) {
			case 'control':
			case 'residual':
			case 'drive':
				return 'Drawn once the floor engages.';
			case 'response':
				if (p.plot.channels === 'limited' && !limits) return 'This run has no limit channels.';
				return 'Each input is drawn once the floor engages.';
			case 'time':
				return 'The samples are drawn once the floor engages.';
			case 'coherence':
			case 'identification':
				return 'Drawn once identification is done.';
			case 'amplitude':
				return 'Each tone is drawn from when it first comes in.';
			case 'level':
				return 'Each narrowband is drawn from when it first comes in.';
			case 'frequency':
				return 'Each part is drawn from when it first comes in.';
			case 'law':
				return "The laws' values are shown after their first calls.";
			default:
				return 'Appears with the first frame.';
		}
	}
</script>

{#snippet verbs()}<MixedRunVerbs {mixed} />{/snippet}

{#snippet cards(kind: CardKind)}
	{#if kind === 'level'}
		<MixedLevelCard {status} {live} partName={(id) => mixed.partName(id)} {compact} />
	{:else if kind === 'parts'}
		<MixedPartsCard {mixed} {compact} />
	{:else if kind === 'law'}
		<SineLiveParams sine={mixed} />
	{:else if kind === 'inputs'}
		<MixedInputsCard {live} />
	{:else if kind === 'log'}
		<MixedLogCard log={mixed.log} />
	{:else if kind === 'station' && station}
		{@render station()}
	{/if}
{/snippet}

<PulseRunView
	host={mixed}
	screen={views}
	prefix="mixed"
	attrs={{
		'data-state': status?.state ?? '',
		'data-phase': status?.phase ?? '',
		'data-t': live?.t_s ?? '',
		'data-parts-s': status?.parts_s ?? '',
		'data-held': status?.held ? 'true' : 'false',
		'data-level': live?.level_db ?? ''
	}}
	{verbs}
	review="The identified gain and its coherence are drawn; nothing plays until you proceed."
	{cards}
	{channels}
	{empty}
	live={streaming}
	{onpopout}
	{onoverride}
	{abortKeys}
	{compact}
	record={status?.record ?? null}
	{opener}
/>
