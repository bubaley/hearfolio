// Records waiting for, or being given, a transcription. The native queue owns
// the work; this is the interface's mirror of it, rebuilt from the queue's
// events and snapshots. Pure, so it is testable without a window.
import type {Progress, QueueEvent, QueueItem} from './api';

export type Job = {state: 'queued' | 'running'; progress: Progress | null; started: number; text: string};
export type Jobs = Map<string, Job>;
export type JobChange = 'started' | 'ended' | 'changed' | 'ignored';

/** Applies one `transcription-queue` event. `ended` means the run is over (finished, failed or cancelled). */
export function applyQueueEvent(jobs: Jobs, event: QueueEvent, now: number): JobChange {
  const job = jobs.get(event.recordId);
  switch (event.state) {
    case 'queued':
      if (job) return 'ignored';
      jobs.set(event.recordId, {state: 'queued', progress: null, started: now, text: ''});
      return 'started';
    case 'running':
      jobs.set(event.recordId, {state: 'running', progress: null, started: now, text: ''});
      return job ? 'changed' : 'started';
    default:
      return jobs.delete(event.recordId) ? 'ended' : 'ignored';
  }
}

/**
 * Aligns the mirror with the native queue (after the window was away or
 * recreated). Returns the records that are no longer in the queue: they ended
 * while nobody was listening, so their result has to be read from the archive.
 */
export function reconcileJobs(jobs: Jobs, snapshot: QueueItem[], now: number): string[] {
  const alive = new Set(snapshot.map(item => item.recordId));
  const gone = [...jobs.keys()].filter(id => !alive.has(id));
  for (const id of gone) jobs.delete(id);
  for (const item of snapshot) {
    const job = jobs.get(item.recordId);
    if (!job) jobs.set(item.recordId, {state: item.state, progress: null, started: now, text: ''});
    else if (job.state !== item.state) {
      job.state = item.state;
      if (item.state === 'running') job.started = now;
    }
  }
  return gone;
}

/** Stores a progress report for the record it belongs to; false when no such run is known. */
export function applyJobProgress(jobs: Jobs, progress: Progress): boolean {
  const job = progress.recordId ? jobs.get(progress.recordId) : undefined;
  if (!job || progress.kind !== 'transcribe') return false;
  if (job.state === 'queued') job.state = 'running';
  job.progress = progress;
  return true;
}

/** Live text of a run; kept per record, so it is still there when the user comes back to it. */
export function applyJobText(jobs: Jobs, recordId: string | null | undefined, text: string): boolean {
  const job = recordId ? jobs.get(recordId) : undefined;
  if (!job || !text) return false;
  job.text = text;
  return true;
}

/** A record in the queue must not be deleted, and its model must not change under it. */
export const isProcessing = (jobs: Jobs, recordId: string | null | undefined): boolean => Boolean(recordId && jobs.has(recordId));

/** The archive may be moved, cleared or the app restarted only when nothing is queued. */
export const archiveBusy = (jobs: Jobs): boolean => jobs.size > 0;

/** Position among the waiting records, starting at 1; 0 when the record runs or is not queued. */
export function queuePosition(jobs: Jobs, recordId: string): number {
  let position = 0;
  for (const [id, job] of jobs) {
    if (job.state === 'queued') position++;
    if (id === recordId) return job.state === 'queued' ? position : 0;
  }
  return 0;
}
