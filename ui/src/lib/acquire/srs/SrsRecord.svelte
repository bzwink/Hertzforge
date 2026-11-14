<script lang="ts">
	/**
	 * The run's record (the shared Run Record panel: who, on what, to what, and whether the folder
	 * keeps the time histories) and the fields kept with the run.
	 */
	import RunRecordEditor from '../RunRecordEditor.svelte';
	import type { SrsStore } from './srsStore.svelte';

	interface Props {
		srs: SrsStore;
	}
	let { srs }: Props = $props();

	const d = $derived(srs.setup);
	const locked = $derived(srs.locked);
</script>

<div class="stack" data-testid="srs-record">
	<!-- The run's record is Random's: the operator's words and whether the folder keeps the time histories. -->
	<RunRecordEditor acq={srs} kept="The setup, the channel table, the log, the target, the synthesis, the identification and every pulse's results are still kept." />
	<section class="aq-panel" aria-labelledby="rk-meta">
		<div class="aq-head"><h2 id="rk-meta">Kept With the Run</h2><span class="aq-sub">Fields the controller's own report carries.</span></div>
		<div class="aq-group">
			<table class="aq-table" data-testid="srs-metadata">
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
	.stack {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		min-width: 0;
		max-width: 860px;
	}

	.spaced {
		margin-top: var(--sp-3);
	}
</style>
