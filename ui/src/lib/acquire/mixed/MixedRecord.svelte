<script lang="ts">
	/**
	 * Mixed Mode's run record: the shared Run Record panel (who runs the test, on what, to which
	 * document and clause, notes, and whether the run folder keeps the time histories), then the
	 * fields kept with the run's report. Every run is recorded; none of this changes how the run
	 * drives.
	 */
	import RunRecordEditor from '../RunRecordEditor.svelte';
	import type { RecordHost } from '../setup';
	import MetadataFields from '../sine/MetadataFields.svelte';
	import type { MixedDraft } from './mixedSetup';

	interface Props {
		mixed: RecordHost & { setup: Pick<MixedDraft, 'record' | 'metadata'> };
	}
	let { mixed }: Props = $props();
</script>

<div class="stack" data-testid="mixed-record">
	<RunRecordEditor acq={mixed} kept="The setup, the channel table, the log, the parts' levels, the identification, the spectra and the laws' calls are still kept." />
	<section class="aq-panel" aria-label="Fields kept with the run">
		<MetadataFields metadata={mixed.setup.metadata} locked={mixed.locked} />
	</section>
</div>

<style>
	.stack {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		max-width: 860px;
	}
</style>
