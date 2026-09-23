// The sweep every function is digested over, and the digest itself.
//
// test/sweep.js is a line-for-line mirror of this file. The two must produce the same inputs in
// the same order and hash the outputs the same way, or the golden digests mean nothing. Every
// operation here is one both languages define identically: uint32 arithmetic, IEEE double
// add/multiply/divide, and bit construction of doubles from two words.
#ifndef SYRINX_FDLIBM_REF_SWEEP_H_
#define SYRINX_FDLIBM_REF_SWEEP_H_

#include <cmath>
#include <cstdint>
#include <cstring>

namespace sweep {

inline double words(uint32_t hi, uint32_t lo) {
  uint64_t bits = (static_cast<uint64_t>(hi) << 32) | lo;
  double d;
  std::memcpy(&d, &bits, sizeof d);
  return d;
}

inline uint32_t hiw(double d) {
  uint64_t bits;
  std::memcpy(&bits, &d, sizeof bits);
  return static_cast<uint32_t>(bits >> 32);
}

inline uint32_t low(double d) {
  uint64_t bits;
  std::memcpy(&bits, &d, sizeof bits);
  return static_cast<uint32_t>(bits & 0xFFFFFFFFu);
}

/** The double one ulp below a positive finite value. */
inline double prev(double d) {
  uint64_t bits;
  std::memcpy(&bits, &d, sizeof bits);
  bits -= 1;
  std::memcpy(&d, &bits, sizeof d);
  return d;
}

/** The double one ulp above a positive finite value. */
inline double next(double d) {
  uint64_t bits;
  std::memcpy(&bits, &d, sizeof bits);
  bits += 1;
  std::memcpy(&d, &bits, sizeof d);
  return d;
}

/** Marsaglia xorshift128, fixed seed. */
struct XorShift {
  uint32_t x = 0x9E3779B9u;
  uint32_t y = 0x243F6A88u;
  uint32_t z = 0xB7E15162u;
  uint32_t w = 0x6A09E667u;
  uint32_t next() {
    uint32_t t = x ^ (x << 11);
    x = y;
    y = z;
    z = w;
    w = w ^ (w >> 19) ^ (t ^ (t >> 8));
    return w;
  }
};

inline uint32_t rotl(uint32_t v, int n) { return (v << n) | (v >> (32 - n)); }

/**
 * Two independent 32-bit lanes over the output bit stream. NaN outputs are canonicalised first:
 * which NaN an engine hands back is not part of the contract.
 */
struct Digest {
  uint32_t fnv = 0x811C9DC5u;
  uint32_t mix = 0x9747B28Cu;
  uint32_t count = 0;

  void add(double y) {
    uint32_t hi = hiw(y);
    uint32_t lo = low(y);
    if (y != y) {
      hi = 0x7FF80000u;
      lo = 0;
    }
    const uint32_t bytes[8] = {
        lo & 255u, (lo >> 8) & 255u, (lo >> 16) & 255u, lo >> 24,
        hi & 255u, (hi >> 8) & 255u, (hi >> 16) & 255u, hi >> 24,
    };
    for (int i = 0; i < 8; i++) {
      fnv ^= bytes[i];
      fnv *= 0x01000193u;
    }
    mix = (mix ^ lo) * 0x9E3779B1u;
    mix = rotl(mix, 13);
    mix = (mix ^ hi) * 0x85EBCA6Bu;
    mix = rotl(mix, 13);
    count++;
  }
};

template <class Visit>
inline void lin(double a, double b, int n, Visit&& visit) {
  const double span = b - a;
  for (int i = 0; i < n; i++) {
    visit(a + span * (static_cast<double>(i) / static_cast<double>(n - 1)));
  }
}

/** Every input of a one-argument function's sweep, in order. */
template <class Visit>
inline void unary(Visit&& visit) {
  XorShift rng;
  for (int i = 0; i < 200000; i++) {
    uint32_t h = rng.next();
    uint32_t l = rng.next();
    visit(words(h, l));
  }
  lin(-1.0, 1.0, 40001, visit);
  lin(-8.0, 8.0, 40001, visit);
  lin(-800.0, 800.0, 40001, visit);
  lin(-1e18, 1e18, 20001, visit);
  for (int i = 0; i < 40000; i++) {
    uint32_t e = static_cast<uint32_t>(i % 2047);
    uint32_t mh = rng.next() & 0xFFFFFu;
    uint32_t l = rng.next();
    uint32_t sign = static_cast<uint32_t>(i & 1) << 31;
    visit(words(sign | (e << 20) | mh, l));
  }
  for (int k = 1; k <= 2000; k++) {
    double t = k * 1.5707963267948966;
    visit(t);
    visit(t - 1e-9);
    visit(t + 1e-9);
    visit(prev(t));
    visit(next(t));
  }
}

/** The special values paired with each other in the binary sweep. */
inline int specials(double* out) {
  int n = 0;
  out[n++] = 0.0;
  out[n++] = -0.0;
  out[n++] = 1.0;
  out[n++] = -1.0;
  out[n++] = 0.5;
  out[n++] = -0.5;
  out[n++] = 2.0;
  out[n++] = -2.0;
  out[n++] = 3.0;
  out[n++] = 1.0 / 3.0;
  out[n++] = 10.0;
  out[n++] = -10.0;
  out[n++] = 1e-300;
  out[n++] = -1e-300;
  out[n++] = 1e300;
  out[n++] = -1e300;
  out[n++] = words(0, 1);
  out[n++] = INFINITY;
  out[n++] = -INFINITY;
  out[n++] = NAN;
  out[n++] = 0.1;
  out[n++] = 1.5;
  out[n++] = 100.0;
  out[n++] = 4503599627370496.0;
  out[n++] = 9007199254740992.0;
  out[n++] = 1e10;
  out[n++] = -1e10;
  return n;
}

/** Every input pair of a two-argument function's sweep, in order. */
template <class Visit>
inline void binary(Visit&& visit) {
  XorShift rng;
  for (int i = 0; i < 200000; i++) {
    uint32_t xh = rng.next();
    uint32_t xl = rng.next();
    uint32_t yh = rng.next();
    uint32_t yl = rng.next();
    visit(words(xh, xl), words(yh, yl));
  }
  lin(-8.0, 8.0, 201, [&](double x) {
    lin(-8.0, 8.0, 201, [&](double y) { visit(x, y); });
  });
  double s[32];
  const int ns = specials(s);
  for (int i = 0; i < ns; i++) {
    for (int j = 0; j < ns; j++) {
      visit(s[i], s[j]);
    }
  }
  for (int i = 0; i < 2000; i++) {
    uint32_t e = static_cast<uint32_t>((i * 2047) / 2000);
    uint32_t mh = rng.next() & 0xFFFFFu;
    uint32_t l = rng.next();
    uint32_t sign = static_cast<uint32_t>(i & 1) << 31;
    const double x = words(sign | (e << 20) | mh, l);
    lin(-100.0, 100.0, 41, [&](double y) { visit(x, y); });
  }
}

}  // namespace sweep

#endif  // SYRINX_FDLIBM_REF_SWEEP_H_
