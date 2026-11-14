<script lang="ts">
	/**
	 * The run's record (the shared Run Record panel: who, on what, to what, and whether the folder
	 * keeps the time histories) and the fields kept with the run, which the controller's own report
	 * carries.
	 */
	import RunRecordEditor from '../RunRecordEditor.svelte';
	import type { TwrStore } from './twrStore.svelte';

	interface Props {
		twr: TwrStore;
	}
	let { twr }: Props = $props();

	const d = $derived(twr.setup);
	const locked = $derived(twr.locked);
</script>

<div class="stack narrow" data-testid="twr-capture">
	<!-- The run's record is Random's: the operator's words and whether the folder keeps the time histories. -->
	<RunRecordEditor acq={twr} kept="The setup, the channel table, the log, the reference, every play's results and every law call are still kept." />
	<section class="aq-panel" aria-labelledby="tk-meta">
		<div class="aq-head"><h2 id="tk-meta">Kept With the Run</h2><span class="aq-sub">Fields the controller's own report carries.</span></div>
		<div class="aq-group">
			<table class="aq-table" data-testid="twr-metadata">
				<thead><tr><th>Field</th><th>Value</th><th aria-label="Remove"></th></tr></thead>
				<tbody>
					{#each d.metadata as m, i (i)}
						<tr>
							<td><input class="field" aria-label="Field {i + 1} name" bind:value={m.label} disabled={locked} oninput={() => (m.key = m.label.trim().toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_|_$/g, '') || `field_${i + 1}`)} /></td>
							<td><input class="field" aria-label="{m.label || `Field ${i + 1}`} value" bind:value={m.value} disabled={locked} /></td>
							<td>
								<button class="aq-icon-btn" aria-label="Remove field {i + 1}" disabled={locked} onclick={() => d.metadata.splice(i, 1)}>
									<svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true"><path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg>
								</button>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
			<button class="btn sm ghost spaced" disabled={locked} onclick={() => d.metadata.push({ key: `field_${d.metadata.length + 1}`, label: '', value: '' })}>Add Field</button>
		</div>
	</section>
</div>

<style>
	.narrow {
		max-width: 860px;
	}

	.stack {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		min-width: 0;
	}

	.spaced {
		margin-top: var(--sp-3);
	}
</style>
