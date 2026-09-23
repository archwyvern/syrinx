// syrinx-framework: master -- a section-level gain curve, a master chain, a fade and a stereo
// widener: the last stage of a track. They take a stereo pair or a layer (music.js) alike; on a
// layer they queue work that runs on each block as the stream is pulled.
//
// `master` is the chain the Firmament album was mastered with: a highpass, a trim, a gentle bus
// compressor, a look-ahead limiter to the ceiling and a linear fade over the end. On a layer the
// limiter delays the mix by its look-ahead plus a rounding margin (104 frames at 48 kHz) instead
// of looking back over a buffer it does not have.

import { db } from "./dsp.js";
import { curve, isLayer } from "./music.js";
import { eqStereo, scale, compressStereo, limitStereo } from "./fx.js";

/**
 * A section gain curve from [[bar, dB], ...]: each level holds from its bar until the next
 * entry, with a 10 ms ramp at the change. Bars may be fractional; the first entry is bar 0.
 */
export function levels(ctx, g, points) {
  const pts = [];
  for (let k = 0; k < points.length; k++) {
    const t = g.at(points[k][0]);
    if (k > 0) pts.push([t - 0.01, db(points[k - 1][1])]);
    pts.push([t, db(points[k][1])]);
  }
  return curve(ctx, pts);
}

/**
 * The album's master chain, in place: highpass, trim, bus compressor, look-ahead limiter to the
 * ceiling, and a linear fade over the last `fade` seconds. Returns the pair.
 */
export function master(ctx, mix, o) {
  const p = o || {};
  const sr = ctx.sr;
  eqStereo(mix, sr, "highpass", p.hp === undefined ? 26 : p.hp, 0.7, 0);
  scale(mix, p.trim === undefined ? 0.85 : p.trim);
  if (p.compress !== false) {
    compressStereo(mix, sr, {
      threshold: p.threshold === undefined ? -10 : p.threshold,
      ratio: p.ratio === undefined ? 1.7 : p.ratio,
      attack: 0.02, release: 0.25, knee: 6, makeup: 1,
    });
  }
  limitStereo(mix, sr, { ceiling: db(p.ceiling === undefined ? -0.8 : p.ceiling), lookahead: 0.002, release: 0.08 });
  return fadeOut(ctx, mix, p.fade === undefined ? 3 : p.fade);
}

/** A linear fade over the last `seconds` of a pair or a layer, in place. */
export function fadeOut(ctx, mix, seconds) {
  const n = Math.min(ctx.frames, Math.round(seconds * ctx.sr));
  const frames = ctx.frames;
  if (isLayer(mix)) {
    mix.jobs.push((planes, offset, count) => {
      for (let k = 0; k < count; k++) {
        const i = frames - 1 - (offset + k);
        if (i < n) { const gn = i / n; for (let c = 0; c < planes.length; c++) planes[c][k] *= gn; }
      }
    });
    return mix;
  }
  const L = mix[0], R = mix[1];
  for (let i = 0; i < n; i++) { const gn = i / n; L[frames - 1 - i] *= gn; R[frames - 1 - i] *= gn; }
  return mix;
}

/** Mid/side width in place: 1 leaves the image, 0 is mono, 2 doubles the side. */
export function widen(pair, amount) {
  const run = (L, R, n) => {
    for (let i = 0; i < n; i++) {
      const m = (L[i] + R[i]) * 0.5, s = (L[i] - R[i]) * 0.5 * amount;
      L[i] = m + s; R[i] = m - s;
    }
  };
  if (isLayer(pair)) { pair.jobs.push((planes, offset, n) => run(planes[0], planes[1], n)); return pair; }
  run(pair[0], pair[1], pair[0].length);
  return pair;
}
