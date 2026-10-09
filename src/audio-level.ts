// Display an envelope, not a synthetic frequency spectrum. The -60 dB floor
// keeps digital silence still; compression makes quiet speech visible.
export function audioLevel(amplitude: number): number {
  if (!Number.isFinite(amplitude) || amplitude <= .001) return 0;
  return Math.min(1, Math.max(0, (20 * Math.log10(amplitude) + 60) / 60));
}
export function pcmLevel(samples: Float32Array): number {
  if (!samples.length) return 0;
  let energy = 0;
  for (const sample of samples) energy += sample * sample;
  return audioLevel(Math.sqrt(energy / samples.length));
}
