// Shared bits for the example sounds. Plain JavaScript, imported by relative path.
import { Noise, Env, Biquad } from "syrinx";

// A short filtered-noise transient: the "crack" at the front of a laser or a hit.
export function crack(ctx, { seed = 0, freq = 3200, q = 4, decay = 0.0025, gain = 2 } = {}) {
  const noise = new Noise(seed);
  const bp = Biquad.bandpass(ctx.sr, freq, q);
  const env = Env.exp(decay);
  return (t) => bp.process(noise.white()) * env(t) * gain;
}
