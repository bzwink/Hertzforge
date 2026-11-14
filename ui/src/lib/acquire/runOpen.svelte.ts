/**
 * A finished run's way to its data: Open in Analysis hands the run folder's path to the analysis
 * service, which converts the folder and opens it as the project, or imports it into the project
 * already open; the shell then shows the analysis workspace that suits the run. Copy Folder Path
 * puts the folder's path on the clipboard. The shell converts nothing itself: the folder is the
 * control service's, the project file the analysis service's.
 */

import type { Workspace } from '$lib/protocol/types';
import { toRefusal, type Refusal } from './words';

export type RunOpenStatus = 'idle' | 'working' | 'done' | 'refused';

export interface RunOpenHost {
	/** Bring the run folder into the analysis side; resolves with the objects it added, rejects with the service's refusal. */
	bring(folder: string): Promise<string[]>;
	/** Show a workspace with these objects of the project selected. */
	show(workspace: Workspace, names: string[]): void;
}

/** The analysis workspace a run's data opens in, by the test that made it. */
export function workspaceForRun(test: string): Workspace {
	// PSDs against their specification are the Random workspace's, sine levels against theirs the
	// Sine workspace's, and a classical shock run's pulses, SRS and their band the Shock
	// workspace's; any other run's recordings, Signals'.
	if (test === 'random') return 'random';
	if (test === 'sine') return 'sine';
	if (test === 'classical_shock') return 'shock';
	// A TWR run's plays against the target it was controlled to: Transient Analysis.
	if (test === 'twr') return 'transient';
	return 'signals';
}

const VERB = 'Open in Analysis';

/** What the Open in Analysis action shows: its label, and the line under it. */
export function runOpenWords(status: RunOpenStatus, refusal: Refusal | null, added: string[]): { label: string; line: string | null } {
	switch (status) {
		case 'working':
			return { label: 'Opening in Analysis', line: 'Converting the run folder for the analysis side.' };
		case 'done':
			return { label: 'Show in Analysis', line: `Opened in Analysis: ${added.length} ${added.length === 1 ? 'object' : 'objects'} from this run in the project.` };
		case 'refused':
			if (!refusal) return { label: VERB, line: 'The run did not open in Analysis.' };
			// a refusal without a heading of its own says what did not happen
			return { label: VERB, line: `${refusal.title === 'Not Done' ? 'The run did not open in Analysis' : refusal.title}. ${refusal.message}` };
		default:
			return { label: VERB, line: null };
	}
}

export class RunOpen {
	/** The folder the present status is about; a different folder starts again from idle. */
	folder = $state<string | null>(null);
	status = $state<RunOpenStatus>('idle');
	refusal = $state.raw<Refusal | null>(null);
	/** The objects the last open brought into the project. */
	added = $state.raw<string[]>([]);
	/** Copy Folder Path: done, or why it could not be. */
	copy = $state<'idle' | 'copied' | 'failed'>('idle');
	private copyTimer: ReturnType<typeof setTimeout> | null = null;

	constructor(
		private readonly host: RunOpenHost,
		private readonly test = 'random'
	) {}

	/** The status for `folder`: what was done for it, or idle for a run not opened yet. */
	at(folder: string | null): RunOpenStatus {
		return folder !== null && folder === this.folder ? this.status : 'idle';
	}

	/** Bring the run into the analysis side; once it is there, only show it again. */
	async open(folder: string): Promise<boolean> {
		if (this.folder === folder && this.status === 'working') return false;
		if (this.folder === folder && this.status === 'done') {
			this.host.show(workspaceForRun(this.test), this.added);
			return true;
		}
		this.folder = folder;
		this.status = 'working';
		this.refusal = null;
		this.added = [];
		try {
			const added = await this.host.bring(folder);
			if (this.folder !== folder) return false;
			this.added = added;
			this.status = 'done';
			this.host.show(workspaceForRun(this.test), added);
			return true;
		} catch (err) {
			if (this.folder !== folder) return false;
			this.refusal = toRefusal(VERB, err);
			this.status = 'refused';
			return false;
		}
	}

	/** The folder's path on the clipboard. */
	async copyPath(folder: string, clipboard: Pick<Clipboard, 'writeText'> | undefined = globalThis.navigator?.clipboard): Promise<boolean> {
		let ok = false;
		try {
			if (clipboard) {
				await clipboard.writeText(folder);
				ok = true;
			}
		} catch {
			ok = false;
		}
		this.copy = ok ? 'copied' : 'failed';
		if (this.copyTimer) clearTimeout(this.copyTimer);
		this.copyTimer = setTimeout(() => (this.copy = 'idle'), ok ? 2500 : 6000);
		return ok;
	}
}
