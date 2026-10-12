export type OperationSnapshot = {
  id?: string; kind?: 'transcribe' | 'postprocess' | 'import' | 'transfer_send' | 'transfer_receive'; recordId?: string | null;
  state: 'idle' | 'running' | 'finished' | 'error' | 'cancelled' | 'interrupted';
  progress?: {kind: string; stage: string; current: number; total: number; elapsed: number} | null;
  error?: string | null; result?: unknown;
};
type Pending = {
  kind: string; recordId: string | null; excludedId: string; id?: string;
  resolve: (value: unknown) => void; reject: (error: Error) => void;
};
// The persisted native result can arrive before the original invoke callback,
// or instead of it after Android has recreated the activity.
export class OperationMailbox {
  private pending = new Set<Pending>();
  begin(value: Pending) { this.pending.add(value); return () => this.pending.delete(value); }
  observe(snapshot: OperationSnapshot): {matched: boolean; terminal: boolean} {
    if (!snapshot.id || snapshot.state === 'idle') return {matched: false, terminal: false};
    for (const request of this.pending) {
      if (request.excludedId === snapshot.id || request.kind !== snapshot.kind ||
          (request.recordId !== null && request.recordId !== snapshot.recordId) ||
          (request.id && request.id !== snapshot.id)) continue;
      request.id = snapshot.id;
      if (snapshot.state === 'running') return {matched: true, terminal: false};
      this.pending.delete(request);
      if (snapshot.state === 'finished') request.resolve(snapshot.result);
      else request.reject(new Error(snapshot.error || 'errors.operationInterrupted'));
      return {matched: true, terminal: true};
    }
    return {matched: false, terminal: false};
  }
}
