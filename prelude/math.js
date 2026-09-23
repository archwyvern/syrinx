// syrinx standard math — the Math functions a sound source may call, defined here rather than by
// the JavaScript engine.
//
// The ECMAScript specification leaves the transcendental functions implementation-defined: two
// engines, or two versions of one engine, may legitimately return results that differ in the last
// bit, and a sound rendered through an IIR filter turns a last-bit difference into a different
// waveform. This script replaces every such function with a port of fdlibm (Sun's freely
// distributable libm, as vendored by V8 up to version 12) and freezes `Math`. What remains is
// IEEE-754 double add, subtract, multiply, divide and sqrt, which every engine on every platform
// computes identically. A host that runs this before anything else renders the same bytes as every
// other host that does, whatever V8 it embeds.
//
// Every host of the standard evaluates this file, as a classic script, once per isolate, before the
// prelude and before any source module. It is the first file of the standard for that reason.
//
// The port keeps the C's integer types explicit: `| 0` where the C is int32_t, `>>> 0` where it is
// uint32_t, `Math.trunc` where the C casts a double to int. Double arithmetic is written exactly as
// in the C; JavaScript never fuses a multiply and an add, so the operation order IS the result.
// The reference the port is checked against, bit for bit, is tools/fdlibm-ref/ in the syrinx
// repository; test/math.test.js is the check.
//
// Functions exact by specification (abs, ceil, floor, fround, max, min, round, sign, sqrt, trunc,
// clz32, imul) are left to the engine. `Math.random` throws: a source must seed its own noise.
(function () {
  "use strict";

  // ------------------------------------------------------------------ word access

  const F64 = new Float64Array(1);
  const U32 = new Uint32Array(F64.buffer);
  const LITTLE = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;
  const HI = LITTLE ? 1 : 0;
  const LO = LITTLE ? 0 : 1;

  /** High word as int32. */
  function hi(x) { F64[0] = x; return U32[HI] | 0; }
  /** High word as uint32. */
  function hiu(x) { F64[0] = x; return U32[HI]; }
  /** Low word as uint32. */
  function lo(x) { F64[0] = x; return U32[LO]; }
  function words(h, l) { U32[HI] = h; U32[LO] = l; return F64[0]; }
  function setHi(x, h) { F64[0] = x; U32[HI] = h; return F64[0]; }
  function setLo(x, l) { F64[0] = x; U32[LO] = l; return F64[0]; }

  const abs = Math.abs;
  const sqrt = Math.sqrt;
  const floor = Math.floor;
  const trunc = Math.trunc;

  const Infinity_ = 1 / 0;
  const NaN_ = 0 / 0;

  function copysign(x, y) {
    return setHi(abs(x), (hi(abs(x)) | (hi(y) & 0x80000000)) | 0);
  }

  // ------------------------------------------------------------------ scalbn (fdlibm s_scalbn.c)

  const SB_two54 = 1.80143985094819840000e+16;
  const SB_twom54 = 5.55111512312578270212e-17;
  const SB_huge = 1.0e+300;
  const SB_tiny = 1.0e-300;

  function scalbn(x, n) {
    let hx = hi(x);
    const lx = lo(x);
    let k = (hx & 0x7ff00000) >> 20;
    if (k === 0) {
      if ((lx | (hx & 0x7fffffff)) === 0) return x;
      x *= SB_two54;
      hx = hi(x);
      k = ((hx & 0x7ff00000) >> 20) - 54;
      if (n < -50000) return SB_tiny * x;
    }
    if (k === 0x7ff) return x + x;
    k = k + n;
    if (k > 0x7fe) return SB_huge * copysign(SB_huge, x);
    if (k > 0) {
      return setHi(x, ((hx & 0x800fffff) | (k << 20)) | 0);
    }
    if (k <= -54) {
      if (n > 50000) return SB_huge * copysign(SB_huge, x);
      return SB_tiny * copysign(SB_tiny, x);
    }
    k += 54;
    x = setHi(x, ((hx & 0x800fffff) | (k << 20)) | 0);
    return x * SB_twom54;
  }

  // ------------------------------------------------------------------ argument reduction

  const two_over_pi = new Int32Array([
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C,
    0x439041, 0xFE5163, 0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649,
    0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129, 0xA73EE8, 0x8235F5, 0x2EBB44,
    0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C, 0x845F8B,
    0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D,
    0x367ECF, 0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5,
    0xF17B3D, 0x0739F7, 0x8A5292, 0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330,
    0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3, 0x91615E, 0xE61B08,
    0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
    0x73A8C9, 0x60E27B, 0xC08C6B,
  ]);

  const npio2_hw = new Int32Array([
    0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C,
    0x4025FDBB, 0x402921FB, 0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C,
    0x40346B9C, 0x4035FDBB, 0x40378FDB, 0x403921FB, 0x403AB41B, 0x403C463A,
    0x403DD85A, 0x403F6A7A, 0x40407E4C, 0x4041475C, 0x4042106C, 0x4042D97C,
    0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB, 0x4046C6CB, 0x40478FDB,
    0x404858EB, 0x404921FB,
  ]);

  const RP_zero = 0.0;
  const RP_half = 5.00000000000000000000e-01;
  const RP_two24 = 1.67772160000000000000e+07;
  const RP_invpio2 = 6.36619772367581382433e-01;
  const RP_pio2_1 = 1.57079632673412561417e+00;
  const RP_pio2_1t = 6.07710050650619224932e-11;
  const RP_pio2_2 = 6.07710050630396597660e-11;
  const RP_pio2_2t = 2.02226624879595063154e-21;
  const RP_pio2_3 = 2.02226624871116645580e-21;
  const RP_pio2_3t = 8.47842766036889956997e-32;

  /** Output of rem_pio2: y[0] + y[1]. Read immediately after the call. */
  const Y = new Float64Array(2);
  const TX = new Float64Array(3);

  /** Returns n; leaves x rem pi/2 in Y[0] + Y[1]. */
  function rem_pio2(x) {
    let z = 0, w, t, r, fn;
    let e0, i, j, nx, n, ix, hx;
    hx = hi(x);
    ix = hx & 0x7FFFFFFF;
    if (ix <= 0x3FE921FB) {
      Y[0] = x;
      Y[1] = 0;
      return 0;
    }
    if (ix < 0x4002D97C) {
      if (hx > 0) {
        z = x - RP_pio2_1;
        if (ix !== 0x3FF921FB) {
          Y[0] = z - RP_pio2_1t;
          Y[1] = (z - Y[0]) - RP_pio2_1t;
        } else {
          z -= RP_pio2_2;
          Y[0] = z - RP_pio2_2t;
          Y[1] = (z - Y[0]) - RP_pio2_2t;
        }
        return 1;
      } else {
        z = x + RP_pio2_1;
        if (ix !== 0x3FF921FB) {
          Y[0] = z + RP_pio2_1t;
          Y[1] = (z - Y[0]) + RP_pio2_1t;
        } else {
          z += RP_pio2_2;
          Y[0] = z + RP_pio2_2t;
          Y[1] = (z - Y[0]) + RP_pio2_2t;
        }
        return -1;
      }
    }
    if (ix <= 0x413921FB) {
      t = abs(x);
      n = trunc(t * RP_invpio2 + RP_half) | 0;
      fn = n;
      r = t - fn * RP_pio2_1;
      w = fn * RP_pio2_1t;
      if (n < 32 && ix !== npio2_hw[n - 1]) {
        Y[0] = r - w;
      } else {
        let high;
        j = ix >> 20;
        Y[0] = r - w;
        high = hiu(Y[0]);
        i = j - ((high >>> 20) & 0x7FF);
        if (i > 16) {
          t = r;
          w = fn * RP_pio2_2;
          r = t - w;
          w = fn * RP_pio2_2t - ((t - r) - w);
          Y[0] = r - w;
          high = hiu(Y[0]);
          i = j - ((high >>> 20) & 0x7FF);
          if (i > 49) {
            t = r;
            w = fn * RP_pio2_3;
            r = t - w;
            w = fn * RP_pio2_3t - ((t - r) - w);
            Y[0] = r - w;
          }
        }
      }
      Y[1] = (r - Y[0]) - w;
      if (hx < 0) {
        Y[0] = -Y[0];
        Y[1] = -Y[1];
        return -n;
      }
      return n;
    }
    if (ix >= 0x7FF00000) {
      Y[0] = Y[1] = x - x;
      return 0;
    }
    const low = lo(x);
    z = setLo(z, low);
    e0 = (ix >> 20) - 1046;
    z = setHi(z, (ix - (e0 << 20)) | 0);
    for (i = 0; i < 2; i++) {
      TX[i] = trunc(z);
      z = (z - TX[i]) * RP_two24;
    }
    TX[2] = z;
    nx = 3;
    while (TX[nx - 1] === RP_zero) nx--;
    n = kernel_rem_pio2(TX, e0, nx, 2, two_over_pi);
    if (hx < 0) {
      Y[0] = -Y[0];
      Y[1] = -Y[1];
      return -n;
    }
    return n;
  }

  const init_jk = [2, 3, 4, 6];
  const PIo2 = [
    1.57079625129699707031e+00,
    7.54978941586159635335e-08,
    5.39030252995776476554e-15,
    3.28200341580791294123e-22,
    1.27065575308067607349e-29,
    1.22933308981111328932e-36,
    2.73370053816464559624e-44,
    2.16741683877804819444e-51,
  ];
  const KR_zero = 0.0;
  const KR_one = 1.0;
  const KR_two24 = 1.67772160000000000000e+07;
  const KR_twon24 = 5.96046447753906250000e-08;

  const IQ = new Int32Array(20);
  const KF = new Float64Array(20);
  const KFQ = new Float64Array(20);
  const KQ = new Float64Array(20);

  /** Writes Y[0], Y[1] (prec 2); returns n & 7. `x` is the TX array. */
  function kernel_rem_pio2(x, e0, nx, prec, ipio2) {
    let jz, jx, jv, jp, jk, carry, n, i, j, k, m, q0, ih;
    let z, fw;
    const iq = IQ, f = KF, fq = KFQ, q = KQ;

    jk = init_jk[prec];
    jp = jk;

    jx = nx - 1;
    jv = trunc((e0 - 3) / 24) | 0;
    if (jv < 0) jv = 0;
    q0 = e0 - 24 * (jv + 1);

    j = jv - jx;
    m = jx + jk;
    for (i = 0; i <= m; i++, j++) {
      f[i] = (j < 0) ? KR_zero : ipio2[j];
    }

    for (i = 0; i <= jk; i++) {
      for (j = 0, fw = 0.0; j <= jx; j++) fw += x[j] * f[jx + i - j];
      q[i] = fw;
    }

    jz = jk;
    for (;;) {
      for (i = 0, j = jz, z = q[jz]; j > 0; i++, j--) {
        fw = (KR_twon24 * z) | 0;
        iq[i] = (z - KR_two24 * fw) | 0;
        z = q[j - 1] + fw;
      }

      z = scalbn(z, q0);
      z -= 8.0 * floor(z * 0.125);
      n = trunc(z) | 0;
      z -= n;
      ih = 0;
      if (q0 > 0) {
        i = (iq[jz - 1] >> (24 - q0));
        n += i;
        iq[jz - 1] -= i << (24 - q0);
        ih = iq[jz - 1] >> (23 - q0);
      } else if (q0 === 0) {
        ih = iq[jz - 1] >> 23;
      } else if (z >= 0.5) {
        ih = 2;
      }

      if (ih > 0) {
        n += 1;
        carry = 0;
        for (i = 0; i < jz; i++) {
          j = iq[i];
          if (carry === 0) {
            if (j !== 0) {
              carry = 1;
              iq[i] = 0x1000000 - j;
            }
          } else {
            iq[i] = 0xFFFFFF - j;
          }
        }
        if (q0 > 0) {
          switch (q0) {
            case 1:
              iq[jz - 1] &= 0x7FFFFF;
              break;
            case 2:
              iq[jz - 1] &= 0x3FFFFF;
              break;
          }
        }
        if (ih === 2) {
          z = KR_one - z;
          if (carry !== 0) z -= scalbn(KR_one, q0);
        }
      }

      if (z === KR_zero) {
        j = 0;
        for (i = jz - 1; i >= jk; i--) j |= iq[i];
        if (j === 0) {
          for (k = 1; jk >= k && iq[jk - k] === 0; k++) {
            // k = number of terms needed
          }
          for (i = jz + 1; i <= jz + k; i++) {
            f[jx + i] = ipio2[jv + i];
            for (j = 0, fw = 0.0; j <= jx; j++) fw += x[j] * f[jx + i - j];
            q[i] = fw;
          }
          jz += k;
          continue;
        }
      }
      break;
    }

    if (z === 0.0) {
      jz -= 1;
      q0 -= 24;
      while (iq[jz] === 0) {
        jz--;
        q0 -= 24;
      }
    } else {
      z = scalbn(z, -q0);
      if (z >= KR_two24) {
        fw = (KR_twon24 * z) | 0;
        iq[jz] = (z - KR_two24 * fw) | 0;
        jz += 1;
        q0 += 24;
        iq[jz] = fw;
      } else {
        iq[jz] = z | 0;
      }
    }

    fw = scalbn(KR_one, q0);
    for (i = jz; i >= 0; i--) {
      q[i] = fw * iq[i];
      fw *= KR_twon24;
    }

    for (i = jz; i >= 0; i--) {
      for (fw = 0.0, k = 0; k <= jp && k <= jz - i; k++) fw += PIo2[k] * q[i + k];
      fq[jz - i] = fw;
    }

    // prec is always 2 here.
    fw = 0.0;
    for (i = jz; i >= 0; i--) fw += fq[i];
    Y[0] = (ih === 0) ? fw : -fw;
    fw = fq[0] - fw;
    for (i = 1; i <= jz; i++) fw += fq[i];
    Y[1] = (ih === 0) ? fw : -fw;
    return n & 7;
  }

  // ------------------------------------------------------------------ kernels

  const KC_one = 1.00000000000000000000e+00;
  const KC_C1 = 4.16666666666666019037e-02;
  const KC_C2 = -1.38888888888741095749e-03;
  const KC_C3 = 2.48015872894767294178e-05;
  const KC_C4 = -2.75573143513906633035e-07;
  const KC_C5 = 2.08757232129817482790e-09;
  const KC_C6 = -1.13596475577881948265e-11;

  function kernel_cos(x, y) {
    let a, iz, z, r, qx;
    let ix = hi(x);
    ix &= 0x7FFFFFFF;
    if (ix < 0x3E400000) {
      if ((trunc(x) | 0) === 0) return KC_one;
    }
    z = x * x;
    r = z * (KC_C1 + z * (KC_C2 + z * (KC_C3 + z * (KC_C4 + z * (KC_C5 + z * KC_C6)))));
    if (ix < 0x3FD33333) {
      return KC_one - (0.5 * z - (z * r - x * y));
    } else {
      if (ix > 0x3FE90000) {
        qx = 0.28125;
      } else {
        qx = words((ix - 0x00200000) | 0, 0);
      }
      iz = 0.5 * z - qx;
      a = KC_one - qx;
      return a - (iz - (z * r - x * y));
    }
  }

  const KS_half = 5.00000000000000000000e-01;
  const KS_S1 = -1.66666666666666324348e-01;
  const KS_S2 = 8.33333333332248946124e-03;
  const KS_S3 = -1.98412698298579493134e-04;
  const KS_S4 = 2.75573137070700676789e-06;
  const KS_S5 = -2.50507602534068634195e-08;
  const KS_S6 = 1.58969099521155010221e-10;

  function kernel_sin(x, y, iy) {
    let z, r, v;
    let ix = hi(x);
    ix &= 0x7FFFFFFF;
    if (ix < 0x3E400000) {
      if ((trunc(x) | 0) === 0) return x;
    }
    z = x * x;
    v = z * x;
    r = KS_S2 + z * (KS_S3 + z * (KS_S4 + z * (KS_S5 + z * KS_S6)));
    if (iy === 0) {
      return x + v * (KS_S1 + z * r);
    } else {
      return x - ((z * (KS_half * y - v * r) - y) - v * KS_S1);
    }
  }

  const KT = [
    3.33333333333334091986e-01,
    1.33333333333201242699e-01,
    5.39682539762260521377e-02,
    2.18694882948595424599e-02,
    8.86323982359930005737e-03,
    3.59207910759131235356e-03,
    1.45620945432529025516e-03,
    5.88041240820264096874e-04,
    2.46463134818469906812e-04,
    7.81794442939557092300e-05,
    7.14072491382608190305e-05,
    -1.85586374855275456654e-05,
    2.59073051863633712884e-05,
    1.00000000000000000000e+00,
    7.85398163397448278999e-01,
    3.06161699786838301793e-17,
  ];

  function kernel_tan(x, y, iy) {
    const one = KT[13], pio4 = KT[14], pio4lo = KT[15], T = KT;
    let z, r, v, w, s;
    let ix, hx;
    hx = hi(x);
    ix = hx & 0x7FFFFFFF;
    if (ix < 0x3E300000) {
      if ((trunc(x) | 0) === 0) {
        const low = lo(x);
        if (((ix | low) | (iy + 1)) === 0) {
          return one / abs(x);
        } else {
          if (iy === 1) {
            return x;
          } else {
            let a, t;
            z = w = x + y;
            z = setLo(z, 0);
            v = y - (z - x);
            t = a = -one / w;
            t = setLo(t, 0);
            s = one + t * z;
            return t + a * (s + t * v);
          }
        }
      }
    }
    if (ix >= 0x3FE59428) {
      if (hx < 0) {
        x = -x;
        y = -y;
      }
      z = pio4 - x;
      w = pio4lo - y;
      x = z + w;
      y = 0.0;
    }
    z = x * x;
    w = z * z;
    r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    s = z * x;
    r = y + z * (s * (r + v) + y);
    r += T[0] * s;
    w = x + r;
    if (ix >= 0x3FE59428) {
      v = iy;
      return (1 - ((hx >> 30) & 2)) * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if (iy === 1) {
      return w;
    } else {
      let a, t;
      z = w;
      z = setLo(z, 0);
      v = r - (z - x);
      t = a = -1.0 / w;
      t = setLo(t, 0);
      s = 1.0 + t * z;
      return t + a * (s + t * v);
    }
  }

  // ------------------------------------------------------------------ the functions

  const AS_one = 1.00000000000000000000e+00;
  const AS_huge = 1.000e+300;
  const AS_pi = 3.14159265358979311600e+00;
  const AS_pio2_hi = 1.57079632679489655800e+00;
  const AS_pio2_lo = 6.12323399573676603587e-17;
  const AS_pio4_hi = 7.85398163397448278999e-01;
  const AS_pS0 = 1.66666666666666657415e-01;
  const AS_pS1 = -3.25565818622400915405e-01;
  const AS_pS2 = 2.01212532134862925881e-01;
  const AS_pS3 = -4.00555345006794114027e-02;
  const AS_pS4 = 7.91534994289814532176e-04;
  const AS_pS5 = 3.47933107596021167570e-05;
  const AS_qS1 = -2.40339491173441421878e+00;
  const AS_qS2 = 2.02094576023350569471e+00;
  const AS_qS3 = -6.88283971605453293030e-01;
  const AS_qS4 = 7.70381505559019352791e-02;

  function acos(x) {
    let z, p, q, r, w, s, c, df;
    const hx = hi(x);
    const ix = hx & 0x7FFFFFFF;
    if (ix >= 0x3FF00000) {
      const lx = lo(x);
      if (((ix - 0x3FF00000) | lx) === 0) {
        if (hx > 0) return 0.0;
        return AS_pi + 2.0 * AS_pio2_lo;
      }
      return NaN_;
    }
    if (ix < 0x3FE00000) {
      if (ix <= 0x3C600000) return AS_pio2_hi + AS_pio2_lo;
      z = x * x;
      p = z * (AS_pS0 + z * (AS_pS1 + z * (AS_pS2 + z * (AS_pS3 + z * (AS_pS4 + z * AS_pS5)))));
      q = AS_one + z * (AS_qS1 + z * (AS_qS2 + z * (AS_qS3 + z * AS_qS4)));
      r = p / q;
      return AS_pio2_hi - (x - (AS_pio2_lo - x * r));
    } else if (hx < 0) {
      z = (AS_one + x) * 0.5;
      p = z * (AS_pS0 + z * (AS_pS1 + z * (AS_pS2 + z * (AS_pS3 + z * (AS_pS4 + z * AS_pS5)))));
      q = AS_one + z * (AS_qS1 + z * (AS_qS2 + z * (AS_qS3 + z * AS_qS4)));
      s = sqrt(z);
      r = p / q;
      w = r * s - AS_pio2_lo;
      return AS_pi - 2.0 * (s + w);
    } else {
      z = (AS_one - x) * 0.5;
      s = sqrt(z);
      df = s;
      df = setLo(df, 0);
      c = (z - df * df) / (s + df);
      p = z * (AS_pS0 + z * (AS_pS1 + z * (AS_pS2 + z * (AS_pS3 + z * (AS_pS4 + z * AS_pS5)))));
      q = AS_one + z * (AS_qS1 + z * (AS_qS2 + z * (AS_qS3 + z * AS_qS4)));
      r = p / q;
      w = r * s + c;
      return 2.0 * (df + w);
    }
  }

  const AH_one = 1.0;
  const AH_ln2 = 6.93147180559945286227e-01;

  function acosh(x) {
    let t;
    const hx = hi(x);
    const lx = lo(x);
    if (hx < 0x3FF00000) {
      return NaN_;
    } else if (hx >= 0x41B00000) {
      if (hx >= 0x7FF00000) {
        return x + x;
      } else {
        return log(x) + AH_ln2;
      }
    } else if (((hx - 0x3FF00000) | lx) === 0) {
      return 0.0;
    } else if (hx > 0x40000000) {
      t = x * x;
      return log(2.0 * x - AH_one / (x + sqrt(t - AH_one)));
    } else {
      t = x - AH_one;
      return log1p(t + sqrt(2.0 * t + t * t));
    }
  }

  function asin(x) {
    let t, w, p, q, c, r, s;
    t = 0;
    const hx = hi(x);
    const ix = hx & 0x7FFFFFFF;
    if (ix >= 0x3FF00000) {
      const lx = lo(x);
      if (((ix - 0x3FF00000) | lx) === 0) {
        return x * AS_pio2_hi + x * AS_pio2_lo;
      }
      return NaN_;
    } else if (ix < 0x3FE00000) {
      if (ix < 0x3E400000) {
        if (AS_huge + x > AS_one) return x;
      } else {
        t = x * x;
      }
      p = t * (AS_pS0 + t * (AS_pS1 + t * (AS_pS2 + t * (AS_pS3 + t * (AS_pS4 + t * AS_pS5)))));
      q = AS_one + t * (AS_qS1 + t * (AS_qS2 + t * (AS_qS3 + t * AS_qS4)));
      w = p / q;
      return x + x * w;
    }
    w = AS_one - abs(x);
    t = w * 0.5;
    p = t * (AS_pS0 + t * (AS_pS1 + t * (AS_pS2 + t * (AS_pS3 + t * (AS_pS4 + t * AS_pS5)))));
    q = AS_one + t * (AS_qS1 + t * (AS_qS2 + t * (AS_qS3 + t * AS_qS4)));
    s = sqrt(t);
    if (ix >= 0x3FEF3333) {
      w = p / q;
      t = AS_pio2_hi - (2.0 * (s + s * w) - AS_pio2_lo);
    } else {
      w = s;
      w = setLo(w, 0);
      c = (t - w * w) / (s + w);
      r = p / q;
      p = 2.0 * s * r - (AS_pio2_lo - 2.0 * c);
      q = AS_pio4_hi - 2.0 * w;
      t = AS_pio4_hi - (p - q);
    }
    if (hx > 0) return t;
    return -t;
  }

  const ASH_one = 1.00000000000000000000e+00;
  const ASH_ln2 = 6.93147180559945286227e-01;
  const ASH_huge = 1.00000000000000000000e+300;

  function asinh(x) {
    let t, w;
    const hx = hi(x);
    const ix = hx & 0x7FFFFFFF;
    if (ix >= 0x7FF00000) return x + x;
    if (ix < 0x3E300000) {
      if (ASH_huge + x > ASH_one) return x;
    }
    if (ix > 0x41B00000) {
      w = log(abs(x)) + ASH_ln2;
    } else if (ix > 0x40000000) {
      t = abs(x);
      w = log(2.0 * t + ASH_one / (sqrt(x * x + ASH_one) + t));
    } else {
      t = x * x;
      w = log1p(abs(x) + t / (ASH_one + sqrt(ASH_one + t)));
    }
    if (hx > 0) return w;
    return -w;
  }

  const atanhi = [
    4.63647609000806093515e-01,
    7.85398163397448278999e-01,
    9.82793723247329054082e-01,
    1.57079632679489655800e+00,
  ];
  const atanlo = [
    2.26987774529616870924e-17,
    3.06161699786838301793e-17,
    1.39033110312309984516e-17,
    6.12323399573676603587e-17,
  ];
  const aT = [
    3.33333333333329318027e-01,
    -1.99999999998764832476e-01,
    1.42857142725034663711e-01,
    -1.11111104054623557880e-01,
    9.09088713343650656196e-02,
    -7.69187620504482999495e-02,
    6.66107313738753120669e-02,
    -5.83357013379057348645e-02,
    4.97687799461593236017e-02,
    -3.65315727442169155270e-02,
    1.62858201153657823623e-02,
  ];
  const AT_one = 1.0;
  const AT_huge = 1.0e300;

  function atan(x) {
    let w, s1, s2, z;
    let id;
    const hx = hi(x);
    const ix = hx & 0x7FFFFFFF;
    if (ix >= 0x44100000) {
      const low = lo(x);
      if (ix > 0x7FF00000 || (ix === 0x7FF00000 && (low !== 0))) return x + x;
      if (hx > 0) return atanhi[3] + atanlo[3];
      return -atanhi[3] - atanlo[3];
    }
    if (ix < 0x3FDC0000) {
      if (ix < 0x3E400000) {
        if (AT_huge + x > AT_one) return x;
      }
      id = -1;
    } else {
      x = abs(x);
      if (ix < 0x3FF30000) {
        if (ix < 0x3FE60000) {
          id = 0;
          x = (2.0 * x - AT_one) / (2.0 + x);
        } else {
          id = 1;
          x = (x - AT_one) / (x + AT_one);
        }
      } else {
        if (ix < 0x40038000) {
          id = 2;
          x = (x - 1.5) / (AT_one + 1.5 * x);
        } else {
          id = 3;
          x = -1.0 / x;
        }
      }
    }
    z = x * x;
    w = z * z;
    s1 = z * (aT[0] + w * (aT[2] + w * (aT[4] + w * (aT[6] + w * (aT[8] + w * aT[10])))));
    s2 = w * (aT[1] + w * (aT[3] + w * (aT[5] + w * (aT[7] + w * aT[9]))));
    if (id < 0) {
      return x - x * (s1 + s2);
    } else {
      z = atanhi[id] - ((x * (s1 + s2) - atanlo[id]) - x);
      return (hx < 0) ? -z : z;
    }
  }

  const A2_tiny = 1.0e-300;
  const A2_zero = 0.0;
  const A2_pi_o_4 = 7.8539816339744827900E-01;
  const A2_pi_o_2 = 1.5707963267948965580E+00;
  const A2_pi = 3.1415926535897931160E+00;
  const A2_pi_lo = 1.2246467991473531772E-16;

  function atan2(y, x) {
    let z;
    let k, m;
    const hx = hi(x);
    const lx = lo(x);
    const ix = hx & 0x7FFFFFFF;
    const hy = hi(y);
    const ly = lo(y);
    const iy = hy & 0x7FFFFFFF;
    if (((ix | ((lx | (-lx | 0)) >>> 31)) > 0x7FF00000) ||
        ((iy | ((ly | (-ly | 0)) >>> 31)) > 0x7FF00000)) {
      return x + y;
    }
    if ((((hx - 0x3FF00000) | 0) | lx) === 0) {
      return atan(y);
    }
    m = ((hy >> 31) & 1) | ((hx >> 30) & 2);

    if ((iy | ly) === 0) {
      switch (m) {
        case 0:
        case 1:
          return y;
        case 2:
          return A2_pi + A2_tiny;
        case 3:
          return -A2_pi - A2_tiny;
      }
    }
    if ((ix | lx) === 0) return (hy < 0) ? -A2_pi_o_2 - A2_tiny : A2_pi_o_2 + A2_tiny;

    if (ix === 0x7FF00000) {
      if (iy === 0x7FF00000) {
        switch (m) {
          case 0:
            return A2_pi_o_4 + A2_tiny;
          case 1:
            return -A2_pi_o_4 - A2_tiny;
          case 2:
            return 3.0 * A2_pi_o_4 + A2_tiny;
          case 3:
            return -3.0 * A2_pi_o_4 - A2_tiny;
        }
      } else {
        switch (m) {
          case 0:
            return A2_zero;
          case 1:
            return -A2_zero;
          case 2:
            return A2_pi + A2_tiny;
          case 3:
            return -A2_pi - A2_tiny;
        }
      }
    }
    if (iy === 0x7FF00000) return (hy < 0) ? -A2_pi_o_2 - A2_tiny : A2_pi_o_2 + A2_tiny;

    k = (iy - ix) >> 20;
    if (k > 60) {
      z = A2_pi_o_2 + 0.5 * A2_pi_lo;
      m &= 1;
    } else if (hx < 0 && k < -60) {
      z = 0.0;
    } else {
      z = atan(abs(y / x));
    }
    switch (m) {
      case 0:
        return z;
      case 1:
        return -z;
      case 2:
        return A2_pi - (z - A2_pi_lo);
      default:
        return (z - A2_pi_lo) - A2_pi;
    }
  }

  function cos(x) {
    let z = 0.0;
    let n, ix;
    ix = hi(x);
    ix &= 0x7FFFFFFF;
    if (ix <= 0x3FE921FB) {
      return kernel_cos(x, z);
    } else if (ix >= 0x7FF00000) {
      return x - x;
    } else {
      n = rem_pio2(x);
      switch (n & 3) {
        case 0:
          return kernel_cos(Y[0], Y[1]);
        case 1:
          return -kernel_sin(Y[0], Y[1], 1);
        case 2:
          return -kernel_cos(Y[0], Y[1]);
        default:
          return kernel_sin(Y[0], Y[1], 1);
      }
    }
  }

  const EX_one = 1.0;
  const EX_halF = [0.5, -0.5];
  const EX_o_threshold = 7.09782712893383973096e+02;
  const EX_u_threshold = -7.45133219101941108420e+02;
  const EX_ln2HI = [6.93147180369123816490e-01, -6.93147180369123816490e-01];
  const EX_ln2LO = [1.90821492927058770002e-10, -1.90821492927058770002e-10];
  const EX_invln2 = 1.44269504088896338700e+00;
  const EX_P1 = 1.66666666666666019037e-01;
  const EX_P2 = -2.77777777770155933842e-03;
  const EX_P3 = 6.61375632143793436117e-05;
  const EX_P4 = -1.65339022054652515390e-06;
  const EX_P5 = 4.13813679705723846039e-08;
  const EX_E = 2.718281828459045;
  const EX_huge = 1.0e+300;
  const EX_twom1000 = 9.33263618503218878990e-302;
  const EX_two1023 = 8.988465674311579539e307;

  function exp(x) {
    let y, hi_ = 0.0, lo_ = 0.0, c, t, twopk;
    let k = 0, xsb;
    let hx = hiu(x);
    xsb = (hx >>> 31) & 1;
    hx &= 0x7FFFFFFF;

    if (hx >= 0x40862E42) {
      if (hx >= 0x7FF00000) {
        const lx = lo(x);
        if (((hx & 0xFFFFF) | lx) !== 0) return x + x;
        return (xsb === 0) ? x : 0.0;
      }
      if (x > EX_o_threshold) return EX_huge * EX_huge;
      if (x < EX_u_threshold) return EX_twom1000 * EX_twom1000;
    }

    if (hx > 0x3FD62E42) {
      if (hx < 0x3FF0A2B2) {
        if (x === 1.0) return EX_E;
        hi_ = x - EX_ln2HI[xsb];
        lo_ = EX_ln2LO[xsb];
        k = 1 - xsb - xsb;
      } else {
        k = trunc(EX_invln2 * x + EX_halF[xsb]) | 0;
        t = k;
        hi_ = x - t * EX_ln2HI[0];
        lo_ = t * EX_ln2LO[0];
      }
      x = hi_ - lo_;
    } else if (hx < 0x3E300000) {
      if (EX_huge + x > EX_one) return EX_one + x;
    } else {
      k = 0;
    }

    t = x * x;
    if (k >= -1021) {
      twopk = words((0x3FF00000 + (k << 20)) | 0, 0);
    } else {
      twopk = words((0x3FF00000 + ((k + 1000) << 20)) | 0, 0);
    }
    c = x - t * (EX_P1 + t * (EX_P2 + t * (EX_P3 + t * (EX_P4 + t * EX_P5))));
    if (k === 0) {
      return EX_one - ((x * c) / (c - 2.0) - x);
    } else {
      y = EX_one - ((lo_ - (x * c) / (2.0 - c)) - hi_);
    }
    if (k >= -1021) {
      if (k === 1024) return y * 2.0 * EX_two1023;
      return y * twopk;
    } else {
      return y * twopk * EX_twom1000;
    }
  }

  const ATH_one = 1.0;
  const ATH_huge = 1e300;
  const ATH_zero = 0.0;

  function atanh(x) {
    let t;
    const hx = hi(x);
    const lx = lo(x);
    const ix = hx & 0x7FFFFFFF;
    if ((ix | ((lx | (-lx | 0)) >>> 31)) > 0x3FF00000) {
      return NaN_;
    }
    if (ix === 0x3FF00000) {
      return x > 0 ? Infinity_ : -Infinity_;
    }
    if (ix < 0x3E300000 && (ATH_huge + x) > ATH_zero) return x;
    x = setHi(x, ix);
    if (ix < 0x3FE00000) {
      t = x + x;
      t = 0.5 * log1p(t + t * x / (ATH_one - x));
    } else {
      t = 0.5 * log1p((x + x) / (ATH_one - x));
    }
    if (hx >= 0) return t;
    return -t;
  }

  const LG_ln2_hi = 6.93147180369123816490e-01;
  const LG_ln2_lo = 1.90821492927058770002e-10;
  const LG_two54 = 1.80143985094819840000e+16;
  const Lg1 = 6.666666666666735130e-01;
  const Lg2 = 3.999999999940941908e-01;
  const Lg3 = 2.857142874366239149e-01;
  const Lg4 = 2.222219843214978396e-01;
  const Lg5 = 1.818357216161805012e-01;
  const Lg6 = 1.531383769920937332e-01;
  const Lg7 = 1.479819860511658591e-01;
  const LG_zero = 0.0;

  function log(x) {
    let hfsq, f, s, z, R, w, t1, t2, dk;
    let k, hx, i, j;
    let lx;
    hx = hi(x);
    lx = lo(x);

    k = 0;
    if (hx < 0x00100000) {
      if (((hx & 0x7FFFFFFF) | lx) === 0) {
        return -Infinity_;
      }
      if (hx < 0) {
        return NaN_;
      }
      k -= 54;
      x *= LG_two54;
      hx = hi(x);
    }
    if (hx >= 0x7FF00000) return x + x;
    k += (hx >> 20) - 1023;
    hx &= 0x000FFFFF;
    i = (hx + 0x95F64) & 0x100000;
    x = setHi(x, (hx | (i ^ 0x3FF00000)) | 0);
    k += (i >> 20);
    f = x - 1.0;
    if ((0x000FFFFF & (2 + hx)) < 3) {
      if (f === LG_zero) {
        if (k === 0) {
          return LG_zero;
        } else {
          dk = k;
          return dk * LG_ln2_hi + dk * LG_ln2_lo;
        }
      }
      R = f * f * (0.5 - 0.33333333333333333 * f);
      if (k === 0) {
        return f - R;
      } else {
        dk = k;
        return dk * LG_ln2_hi - ((R - dk * LG_ln2_lo) - f);
      }
    }
    s = f / (2.0 + f);
    dk = k;
    z = s * s;
    i = hx - 0x6147A;
    w = z * z;
    j = 0x6B851 - hx;
    t1 = w * (Lg2 + w * (Lg4 + w * Lg6));
    t2 = z * (Lg1 + w * (Lg3 + w * (Lg5 + w * Lg7)));
    i |= j;
    R = t2 + t1;
    if (i > 0) {
      hfsq = 0.5 * f * f;
      if (k === 0) return f - (hfsq - s * (hfsq + R));
      return dk * LG_ln2_hi - ((hfsq - (s * (hfsq + R) + dk * LG_ln2_lo)) - f);
    } else {
      if (k === 0) return f - s * (f - R);
      return dk * LG_ln2_hi - ((s * (f - R) - dk * LG_ln2_lo) - f);
    }
  }

  function log1p(x) {
    let hfsq, f = 0, c = 0, s, z, R, u;
    let k, hu = 0, ax;
    const hx = hi(x);
    ax = hx & 0x7FFFFFFF;

    k = 1;
    if (hx < 0x3FDA827A) {
      if (ax >= 0x3FF00000) {
        if (x === -1.0) return -Infinity_;
        return NaN_;
      }
      if (ax < 0x3E200000) {
        if (LG_two54 + x > LG_zero && ax < 0x3C900000) return x;
        return x - x * x * 0.5;
      }
      if (hx > 0 || hx <= (0xBFD2BEC4 | 0)) {
        k = 0;
        f = x;
        hu = 1;
      }
    }
    if (hx >= 0x7FF00000) return x + x;
    if (k !== 0) {
      if (hx < 0x43400000) {
        u = 1.0 + x;
        hu = hi(u);
        k = (hu >> 20) - 1023;
        c = (k > 0) ? 1.0 - (u - x) : x - (u - 1.0);
        c /= u;
      } else {
        u = x;
        hu = hi(u);
        k = (hu >> 20) - 1023;
        c = 0;
      }
      hu &= 0x000FFFFF;
      if (hu < 0x6A09E) {
        u = setHi(u, (hu | 0x3FF00000) | 0);
      } else {
        k += 1;
        u = setHi(u, (hu | 0x3FE00000) | 0);
        hu = (0x00100000 - hu) >> 2;
      }
      f = u - 1.0;
    }
    hfsq = 0.5 * f * f;
    if (hu === 0) {
      if (f === LG_zero) {
        if (k === 0) {
          return LG_zero;
        } else {
          c += k * LG_ln2_lo;
          return k * LG_ln2_hi + c;
        }
      }
      R = hfsq * (1.0 - 0.66666666666666666 * f);
      if (k === 0) return f - R;
      return k * LG_ln2_hi - ((R - (k * LG_ln2_lo + c)) - f);
    }
    s = f / (2.0 + f);
    z = s * s;
    R = z * (Lg1 + z * (Lg2 + z * (Lg3 + z * (Lg4 + z * (Lg5 + z * (Lg6 + z * Lg7))))));
    if (k === 0) return f - (hfsq - s * (hfsq + R));
    return k * LG_ln2_hi - ((hfsq - (s * (hfsq + R) + (k * LG_ln2_lo + c))) - f);
  }

  function k_log1p(f) {
    let hfsq, s, z, R, w, t1, t2;
    s = f / (2.0 + f);
    z = s * s;
    w = z * z;
    t1 = w * (Lg2 + w * (Lg4 + w * Lg6));
    t2 = z * (Lg1 + w * (Lg3 + w * (Lg5 + w * Lg7)));
    R = t2 + t1;
    hfsq = 0.5 * f * f;
    return s * (hfsq + R);
  }

  const L2_ivln2hi = 1.44269504072144627571e+00;
  const L2_ivln2lo = 1.67517131648865118353e-10;

  function log2(x) {
    let f, hfsq, hi_, lo_, r, val_hi, val_lo, w, y;
    let i, k, hx;
    let lx;
    hx = hi(x);
    lx = lo(x);

    k = 0;
    if (hx < 0x00100000) {
      if (((hx & 0x7FFFFFFF) | lx) === 0) {
        return -Infinity_;
      }
      if (hx < 0) {
        return NaN_;
      }
      k -= 54;
      x *= LG_two54;
      hx = hi(x);
    }
    if (hx >= 0x7FF00000) return x + x;
    if (hx === 0x3FF00000 && lx === 0) return 0.0;
    k += (hx >> 20) - 1023;
    hx &= 0x000FFFFF;
    i = (hx + 0x95F64) & 0x100000;
    x = setHi(x, (hx | (i ^ 0x3FF00000)) | 0);
    k += (i >> 20);
    y = k;
    f = x - 1.0;
    hfsq = 0.5 * f * f;
    r = k_log1p(f);

    hi_ = f - hfsq;
    hi_ = setLo(hi_, 0);
    lo_ = (f - hi_) - hfsq + r;
    val_hi = hi_ * L2_ivln2hi;
    val_lo = (lo_ + hi_) * L2_ivln2lo + lo_ * L2_ivln2hi;

    w = y + val_hi;
    val_lo += (y - w) + val_hi;
    val_hi = w;

    return val_lo + val_hi;
  }

  const L10_ivln10 = 4.34294481903251816668e-01;
  const L10_log10_2hi = 3.01029995663611771306e-01;
  const L10_log10_2lo = 3.69423907715893078616e-13;

  function log10(x) {
    let y;
    let i, k, hx;
    let lx;
    hx = hi(x);
    lx = lo(x);

    k = 0;
    if (hx < 0x00100000) {
      if (((hx & 0x7FFFFFFF) | lx) === 0) {
        return -Infinity_;
      }
      if (hx < 0) {
        return NaN_;
      }
      k -= 54;
      x *= LG_two54;
      hx = hi(x);
      lx = lo(x);
    }
    if (hx >= 0x7FF00000) return x + x;
    if (hx === 0x3FF00000 && lx === 0) return 0.0;
    k += (hx >> 20) - 1023;

    i = (k & 0x80000000) >>> 31;
    hx = ((hx & 0x000FFFFF) | ((0x3FF - i) << 20)) | 0;
    y = k + i;
    x = setHi(x, hx);
    x = setLo(x, lx);

    const z = y * L10_log10_2lo + L10_ivln10 * log(x);
    return z + y * L10_log10_2hi;
  }

  const EM_one = 1.0;
  const EM_tiny = 1.0e-300;
  const EM_o_threshold = 7.09782712893383973096e+02;
  const EM_ln2_hi = 6.93147180369123816490e-01;
  const EM_ln2_lo = 1.90821492927058770002e-10;
  const EM_invln2 = 1.44269504088896338700e+00;
  const EM_Q1 = -3.33333333333331316428e-02;
  const EM_Q2 = 1.58730158725481460165e-03;
  const EM_Q3 = -7.93650757867487942473e-05;
  const EM_Q4 = 4.00821782732936239552e-06;
  const EM_Q5 = -2.01099218183624371326e-07;
  const EM_huge = 1.0e+300;

  function expm1(x) {
    let y, hi_ = 0, lo_ = 0, c = 0, t, e, hxs, hfx, r1, twopk;
    let k, xsb;
    let hx = hiu(x);
    xsb = (hx & 0x80000000) >>> 0;
    hx &= 0x7FFFFFFF;

    if (hx >= 0x4043687A) {
      if (hx >= 0x40862E42) {
        if (hx >= 0x7FF00000) {
          const low = lo(x);
          if (((hx & 0xFFFFF) | low) !== 0) return x + x;
          return (xsb === 0) ? x : -1.0;
        }
        if (x > EM_o_threshold) return EM_huge * EM_huge;
      }
      if (xsb !== 0) {
        if (x + EM_tiny < 0.0) return EM_tiny - EM_one;
      }
    }

    if (hx > 0x3FD62E42) {
      if (hx < 0x3FF0A2B2) {
        if (xsb === 0) {
          hi_ = x - EM_ln2_hi;
          lo_ = EM_ln2_lo;
          k = 1;
        } else {
          hi_ = x + EM_ln2_hi;
          lo_ = -EM_ln2_lo;
          k = -1;
        }
      } else {
        k = trunc(EM_invln2 * x + ((xsb === 0) ? 0.5 : -0.5)) | 0;
        t = k;
        hi_ = x - t * EM_ln2_hi;
        lo_ = t * EM_ln2_lo;
      }
      x = hi_ - lo_;
      c = (hi_ - x) - lo_;
    } else if (hx < 0x3C900000) {
      t = EM_huge + x;
      return x - (t - (EM_huge + x));
    } else {
      k = 0;
    }

    hfx = 0.5 * x;
    hxs = x * hfx;
    r1 = EM_one + hxs * (EM_Q1 + hxs * (EM_Q2 + hxs * (EM_Q3 + hxs * (EM_Q4 + hxs * EM_Q5))));
    t = 3.0 - r1 * hfx;
    e = hxs * ((r1 - t) / (6.0 - x * t));
    if (k === 0) {
      return x - (x * e - hxs);
    } else {
      twopk = words((0x3FF00000 + (k << 20)) | 0, 0);
      e = (x * (e - c) - c);
      e -= hxs;
      if (k === -1) return 0.5 * (x - e) - 0.5;
      if (k === 1) {
        if (x < -0.25) return -2.0 * (e - (x + 0.5));
        return EM_one + 2.0 * (x - e);
      }
      if (k <= -2 || k > 56) {
        y = EM_one - (e - x);
        if (k === 1024) y = y * 2.0 * 8.98846567431158e+307;
        else y = y * twopk;
        return y - EM_one;
      }
      t = EM_one;
      if (k < 20) {
        t = setHi(t, (0x3FF00000 - (0x200000 >> k)) | 0);
        y = t - (e - x);
        y = y * twopk;
      } else {
        t = setHi(t, ((0x3FF - k) << 20) | 0);
        y = x - (e + t);
        y += EM_one;
        y = y * twopk;
      }
    }
    return y;
  }

  const CB_B1 = 715094163;
  const CB_B2 = 696219795;
  const CB_P0 = 1.87595182427177009643;
  const CB_P1 = -1.88497979543377169875;
  const CB_P2 = 1.621429720105354466140;
  const CB_P3 = -0.758397934778766047437;
  const CB_P4 = 0.145996192886612446982;

  function cbrt(x) {
    let hx;
    let r, s, t = 0.0, w;
    let sign;
    let high, low;
    hx = hi(x);
    low = lo(x);
    sign = (hx & 0x80000000) >>> 0;
    hx = (hx ^ sign) | 0;
    if (hx >= 0x7FF00000) return (x + x);

    if (hx < 0x00100000) {
      if ((hx | low) === 0) return (x);
      t = setHi(t, 0x43500000);
      t *= x;
      high = hiu(t);
      t = words((sign | (((high & 0x7FFFFFFF) / 3 | 0) + CB_B2)) >>> 0, 0);
    } else {
      t = words((sign | ((hx / 3 | 0) + CB_B1)) >>> 0, 0);
    }

    r = (t * t) * (t / x);
    t = t * ((CB_P0 + r * (CB_P1 + r * CB_P2)) + ((r * r) * r) * (CB_P3 + r * CB_P4));

    // bits = (bits + 0x80000000) & 0xFFFFFFFFC0000000, done on the two words.
    {
      let th = hiu(t);
      let tl = lo(t);
      const sum = tl + 0x80000000;
      if (sum >= 0x100000000) {
        th = (th + 1) >>> 0;
        tl = (sum - 0x100000000) >>> 0;
      } else {
        tl = sum >>> 0;
      }
      tl = (tl & 0xC0000000) >>> 0;
      t = words(th, tl);
    }

    s = t * t;
    r = x / s;
    w = t + t;
    r = (r - t) / (w + r);
    t = t + t * r;

    return (t);
  }

  function sin(x) {
    let z = 0.0;
    let n, ix;
    ix = hi(x);
    ix &= 0x7FFFFFFF;
    if (ix <= 0x3FE921FB) {
      return kernel_sin(x, z, 0);
    } else if (ix >= 0x7FF00000) {
      return x - x;
    } else {
      n = rem_pio2(x);
      switch (n & 3) {
        case 0:
          return kernel_sin(Y[0], Y[1], 1);
        case 1:
          return kernel_cos(Y[0], Y[1]);
        case 2:
          return -kernel_sin(Y[0], Y[1], 1);
        default:
          return -kernel_cos(Y[0], Y[1]);
      }
    }
  }

  function tan(x) {
    let z = 0.0;
    let n, ix;
    ix = hi(x);
    ix &= 0x7FFFFFFF;
    if (ix <= 0x3FE921FB) {
      return kernel_tan(x, z, 1);
    } else if (ix >= 0x7FF00000) {
      return x - x;
    } else {
      n = rem_pio2(x);
      return kernel_tan(Y[0], Y[1], 1 - ((n & 1) << 1));
    }
  }

  const CH_KCOSH_OVERFLOW = 710.4758600739439;
  const CH_one = 1.0;
  const CH_half = 0.5;
  const CH_huge = 1.0e+300;

  function cosh(x) {
    let ix = hi(x);
    ix &= 0x7FFFFFFF;

    if (ix < 0x3FD62E43) {
      const t = expm1(abs(x));
      const w = CH_one + t;
      if (ix < 0x3C800000) return w;
      return CH_one + (t * t) / (w + w);
    }

    if (ix < 0x40360000) {
      const t = exp(abs(x));
      return CH_half * t + CH_half / t;
    }

    if (ix < 0x40862E42) return CH_half * exp(abs(x));

    if (abs(x) <= CH_KCOSH_OVERFLOW) {
      const w = exp(CH_half * abs(x));
      const t = CH_half * w;
      return t * w;
    }

    if (ix >= 0x7FF00000) return x * x;

    return CH_huge * CH_huge;
  }

  const PW_bp = [1.0, 1.5];
  const PW_dp_h = [0.0, 5.84962487220764160156e-01];
  const PW_dp_l = [0.0, 1.35003920212974897128e-08];
  const PW_zero = 0.0;
  const PW_one = 1.0;
  const PW_two = 2.0;
  const PW_two53 = 9007199254740992.0;
  const PW_huge = 1.0e300;
  const PW_tiny = 1.0e-300;
  const PW_L1 = 5.99999999999994648725e-01;
  const PW_L2 = 4.28571428578550184252e-01;
  const PW_L3 = 3.33333329818377432918e-01;
  const PW_L4 = 2.72728123808534006489e-01;
  const PW_L5 = 2.30660745775561754067e-01;
  const PW_L6 = 2.06975017800338417784e-01;
  const PW_P1 = 1.66666666666666019037e-01;
  const PW_P2 = -2.77777777770155933842e-03;
  const PW_P3 = 6.61375632143793436117e-05;
  const PW_P4 = -1.65339022054652515390e-06;
  const PW_P5 = 4.13813679705723846039e-08;
  const PW_lg2 = 6.93147180559945286227e-01;
  const PW_lg2_h = 6.93147182464599609375e-01;
  const PW_lg2_l = -1.90465429995776804525e-09;
  const PW_ovt = 8.0085662595372944372e-0017;
  const PW_cp = 9.61796693925975554329e-01;
  const PW_cp_h = 9.61796700954437255859e-01;
  const PW_cp_l = -7.02846165095275826516e-09;
  const PW_ivln2 = 1.44269504088896338700e+00;
  const PW_ivln2_h = 1.44269502162933349609e+00;
  const PW_ivln2_l = 1.92596299112661746887e-08;

  function pow(x, y) {
    let z, ax, z_h, z_l, p_h, p_l;
    let y1, t1, t2, r, s, t, u, v, w;
    let i, j, k, yisint, n;
    let hx, hy, ix, iy;
    let lx, ly;

    hx = hi(x);
    lx = lo(x);
    hy = hi(y);
    ly = lo(y);
    ix = hx & 0x7fffffff;
    iy = hy & 0x7fffffff;

    if ((iy | ly) === 0) return PW_one;

    if (ix > 0x7ff00000 || ((ix === 0x7ff00000) && (lx !== 0)) || iy > 0x7ff00000 ||
        ((iy === 0x7ff00000) && (ly !== 0))) {
      return x + y;
    }

    yisint = 0;
    if (hx < 0) {
      if (iy >= 0x43400000) {
        yisint = 2;
      } else if (iy >= 0x3ff00000) {
        k = (iy >> 20) - 0x3ff;
        if (k > 20) {
          j = ly >>> (52 - k);
          if ((j << (52 - k)) === (ly | 0)) yisint = 2 - (j & 1);
        } else if (ly === 0) {
          j = iy >> (20 - k);
          if ((j << (20 - k)) === iy) yisint = 2 - (j & 1);
        }
      }
    }

    if (ly === 0) {
      if (iy === 0x7ff00000) {
        if (((ix - 0x3ff00000) | lx) === 0) {
          return y - y;
        } else if (ix >= 0x3ff00000) {
          return (hy >= 0) ? y : PW_zero;
        } else {
          return (hy < 0) ? -y : PW_zero;
        }
      }
      if (iy === 0x3ff00000) {
        if (hy < 0) return PW_one / x;
        return x;
      }
      if (hy === 0x40000000) return x * x;
      if (hy === 0x3fe00000) {
        if (hx >= 0) return sqrt(x);
      }
    }

    ax = abs(x);
    if (lx === 0) {
      if (ix === 0x7ff00000 || ix === 0 || ix === 0x3ff00000) {
        z = ax;
        if (hy < 0) z = PW_one / z;
        if (hx < 0) {
          if (((ix - 0x3ff00000) | yisint) === 0) {
            z = NaN_;
          } else if (yisint === 1) {
            z = -z;
          }
        }
        return z;
      }
    }

    n = (hx >> 31) + 1;

    if ((n | yisint) === 0) {
      return NaN_;
    }

    s = PW_one;
    if ((n | (yisint - 1)) === 0) s = -PW_one;

    if (iy > 0x41e00000) {
      if (iy > 0x43f00000) {
        if (ix <= 0x3fefffff) return (hy < 0) ? PW_huge * PW_huge : PW_tiny * PW_tiny;
        if (ix >= 0x3ff00000) return (hy > 0) ? PW_huge * PW_huge : PW_tiny * PW_tiny;
      }
      if (ix < 0x3fefffff) return (hy < 0) ? s * PW_huge * PW_huge : s * PW_tiny * PW_tiny;
      if (ix > 0x3ff00000) return (hy > 0) ? s * PW_huge * PW_huge : s * PW_tiny * PW_tiny;
      t = ax - PW_one;
      w = (t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25));
      u = PW_ivln2_h * t;
      v = t * PW_ivln2_l - w * PW_ivln2;
      t1 = u + v;
      t1 = setLo(t1, 0);
      t2 = v - (t1 - u);
    } else {
      let ss, s2, s_h, s_l, t_h, t_l;
      n = 0;
      if (ix < 0x00100000) {
        ax *= PW_two53;
        n -= 53;
        ix = hi(ax);
      }
      n += ((ix) >> 20) - 0x3ff;
      j = ix & 0x000fffff;
      ix = j | 0x3ff00000;
      if (j <= 0x3988E) {
        k = 0;
      } else if (j < 0xBB67A) {
        k = 1;
      } else {
        k = 0;
        n += 1;
        ix -= 0x00100000;
      }
      ax = setHi(ax, ix);

      u = ax - PW_bp[k];
      v = PW_one / (ax + PW_bp[k]);
      ss = u * v;
      s_h = ss;
      s_h = setLo(s_h, 0);
      t_h = PW_zero;
      t_h = setHi(t_h, (((ix >> 1) | 0x20000000) + 0x00080000 + (k << 18)) | 0);
      t_l = ax - (t_h - PW_bp[k]);
      s_l = v * ((u - s_h * t_h) - s_h * t_l);
      s2 = ss * ss;
      r = s2 * s2 * (PW_L1 + s2 * (PW_L2 + s2 * (PW_L3 + s2 * (PW_L4 + s2 * (PW_L5 + s2 * PW_L6)))));
      r += s_l * (s_h + ss);
      s2 = s_h * s_h;
      t_h = 3.0 + s2 + r;
      t_h = setLo(t_h, 0);
      t_l = r - ((t_h - 3.0) - s2);
      u = s_h * t_h;
      v = s_l * t_h + t_l * ss;
      p_h = u + v;
      p_h = setLo(p_h, 0);
      p_l = v - (p_h - u);
      z_h = PW_cp_h * p_h;
      z_l = PW_cp_l * p_h + p_l * PW_cp + PW_dp_l[k];
      t = n;
      t1 = (((z_h + z_l) + PW_dp_h[k]) + t);
      t1 = setLo(t1, 0);
      t2 = z_l - (((t1 - t) - PW_dp_h[k]) - z_h);
    }

    y1 = y;
    y1 = setLo(y1, 0);
    p_l = (y - y1) * t1 + y * t2;
    p_h = y1 * t1;
    z = p_l + p_h;
    j = hi(z);
    i = lo(z) | 0;
    if (j >= 0x40900000) {
      if (((j - 0x40900000) | i) !== 0) {
        return s * PW_huge * PW_huge;
      } else {
        if (p_l + PW_ovt > z - p_h) return s * PW_huge * PW_huge;
      }
    } else if ((j & 0x7fffffff) >= 0x4090cc00) {
      if ((((j - 0xc090cc00) | 0) | i) !== 0) {
        return s * PW_tiny * PW_tiny;
      } else {
        if (p_l <= z - p_h) return s * PW_tiny * PW_tiny;
      }
    }

    i = j & 0x7fffffff;
    k = (i >> 20) - 0x3ff;
    n = 0;
    if (i > 0x3fe00000) {
      n = j + (0x00100000 >> (k + 1));
      k = ((n & 0x7fffffff) >> 20) - 0x3ff;
      t = PW_zero;
      t = setHi(t, (n & ~(0x000fffff >> k)) | 0);
      n = ((n & 0x000fffff) | 0x00100000) >> (20 - k);
      if (j < 0) n = -n;
      p_h -= t;
    }
    t = p_l + p_h;
    t = setLo(t, 0);
    u = t * PW_lg2_h;
    v = (p_l - (t - p_h)) * PW_lg2 + t * PW_lg2_l;
    z = u + v;
    w = v - (z - u);
    t = z * z;
    t1 = z - t * (PW_P1 + t * (PW_P2 + t * (PW_P3 + t * (PW_P4 + t * PW_P5))));
    r = (z * t1) / ((t1 - PW_two) - (w + z * w));
    z = PW_one - (r - z);
    j = hi(z);
    j = (j + (n << 20)) | 0;
    if ((j >> 20) <= 0) {
      z = scalbn(z, n);
    } else {
      const tmp = hi(z);
      z = setHi(z, (tmp + (n << 20)) | 0);
    }
    return s * z;
  }

  const SH_KSINH_OVERFLOW = 710.4758600739439;
  const SH_TWO_M28 = 3.725290298461914e-9;
  const SH_LOG_MAXD = 709.7822265625;
  const SH_shuge = 1.0e307;

  function sinh(x) {
    const h = (x < 0) ? -0.5 : 0.5;
    const ax = abs(x);
    if (ax < 22) {
      if (ax < SH_TWO_M28) return x;
      const t = expm1(ax);
      if (ax < 1) {
        return h * (2 * t - t * t / (t + 1));
      }
      return h * (t + t / (t + 1));
    }
    if (ax < SH_LOG_MAXD) return h * exp(ax);
    if (ax <= SH_KSINH_OVERFLOW) {
      const w = exp(0.5 * ax);
      const t = h * w;
      return t * w;
    }
    return x * SH_shuge;
  }

  const TH_tiny = 1.0e-300;
  const TH_one = 1.0;
  const TH_two = 2.0;
  const TH_huge = 1.0e300;

  function tanh(x) {
    let t, z;
    const jx = hi(x);
    const ix = jx & 0x7FFFFFFF;

    if (ix >= 0x7FF00000) {
      if (jx >= 0) return TH_one / x + TH_one;
      return TH_one / x - TH_one;
    }

    if (ix < 0x40360000) {
      if (ix < 0x3E300000) {
        if (TH_huge + x > TH_one) return x;
      }
      if (ix >= 0x3FF00000) {
        t = expm1(TH_two * abs(x));
        z = TH_one - TH_two / (t + TH_two);
      } else {
        t = expm1(-TH_two * abs(x));
        z = -t / (t + TH_two);
      }
    } else {
      z = TH_one - TH_tiny;
    }
    return (jx >= 0) ? z : -z;
  }

  // Ported from V8 12.4's MathHypot (src/builtins/math.tq): the largest magnitude normalises the
  // rest, and the squares are Kahan-summed. Not libm, but not specified either.
  function hypot() {
    const length = arguments.length;
    if (length === 0) return 0;
    const absValues = new Float64Array(length);
    let oneArgIsNaN = false;
    let max = 0;
    for (let i = 0; i < length; ++i) {
      const value = +arguments[i];
      if (value !== value) {
        oneArgIsNaN = true;
      } else {
        const absValue = abs(value);
        absValues[i] = absValue;
        if (absValue > max) max = absValue;
      }
    }
    if (max === Infinity_) {
      return Infinity_;
    } else if (oneArgIsNaN) {
      return NaN_;
    } else if (max === 0) {
      return 0;
    }
    let sum = 0;
    let compensation = 0;
    for (let i = 0; i < length; ++i) {
      const n = absValues[i] / max;
      const summand = n * n - compensation;
      const preliminary = sum + summand;
      compensation = (preliminary - sum) - summand;
      sum = preliminary;
    }
    return sqrt(sum) * max;
  }

  function random() {
    throw new Error("Math.random is not available in a sound source; seed a Random from the prelude");
  }

  // ------------------------------------------------------------------ install

  const replacements = {
    acos, acosh, asin, asinh, atan, atanh, atan2, cbrt, cos, cosh, exp, expm1, hypot,
    log, log1p, log2, log10, pow, sin, sinh, tan, tanh, random,
  };
  for (const name of Object.keys(replacements)) {
    Object.defineProperty(Math, name, {
      value: replacements[name],
      writable: false,
      enumerable: false,
      configurable: false,
    });
  }
  Object.freeze(Math);
})();
