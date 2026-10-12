export class RecorderEnvelope {
  private values: number[];
  private lastSample = 0;
  readonly size: number;
  readonly interval: number;
  constructor(size = 80, interval = 125) { this.size = size; this.interval = interval; this.values = Array(size).fill(0); }
  reset() { this.values.fill(0); this.lastSample = 0; }
  sample(level: number, now: number, recording: boolean): boolean {
    if (!recording || (this.lastSample && now - this.lastSample < this.interval)) return false;
    // A suspended UI cannot supply samples: clear the old display instead of
    // inventing a waveform for time spent in the background.
    if (this.lastSample && now - this.lastSample > 1000) this.values.fill(0);
    this.lastSample = now;
    this.values.shift(); this.values.push(Number.isFinite(level) ? Math.max(0, Math.min(1, level)) : 0);
    return true;
  }
  levels(): readonly number[] { return this.values; }
}
