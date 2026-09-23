// Generates test/fixtures/math-golden.json: for every function of the standard math, the digest
// of its outputs over the sweep, plus 64 sample input/output pairs for diagnosis.
//
//   golden            JSON on stdout
//   golden --dump fn  every input and output of one function as hex, one line each, for
//                     tools/fdlibm-ref/diff.mjs to find the first divergence
//
// Built by the Makefile beside it with floating-point contraction OFF: with it on, a compiler may
// fuse a*b+c into one rounding and the reference would no longer be the fdlibm arithmetic the port
// reproduces.

#include <cmath>
#include <cstdio>
#include <cstring>

#include "src/base/ieee754.h"
#include "sweep.h"

namespace {

// Ported from V8 12.4's MathHypot (src/builtins/math.tq) for two arguments: the largest magnitude
// normalises the rest and the squares are Kahan-summed.
double hypot2(double a, double b) {
  const double values[2] = {a, b};
  double absValues[2] = {0, 0};
  bool oneArgIsNaN = false;
  double max = 0;
  for (int i = 0; i < 2; ++i) {
    const double value = values[i];
    if (value != value) {
      oneArgIsNaN = true;
    } else {
      const double absValue = std::fabs(value);
      absValues[i] = absValue;
      if (absValue > max) max = absValue;
    }
  }
  if (max == INFINITY) {
    return INFINITY;
  } else if (oneArgIsNaN) {
    return NAN;
  } else if (max == 0) {
    return 0;
  }
  double sum = 0;
  double compensation = 0;
  for (int i = 0; i < 2; ++i) {
    const double n = absValues[i] / max;
    const double summand = n * n - compensation;
    const double preliminary = sum + summand;
    compensation = (preliminary - sum) - summand;
    sum = preliminary;
  }
  return std::sqrt(sum) * max;
}

using Unary = double (*)(double);
using Binary = double (*)(double, double);

struct UnaryEntry {
  const char* name;
  Unary fn;
};
struct BinaryEntry {
  const char* name;
  Binary fn;
};

namespace f = v8::base::ieee754;

const UnaryEntry UNARY[] = {
    {"acos", f::acos},   {"acosh", f::acosh}, {"asin", f::asin},   {"asinh", f::asinh},
    {"atan", f::atan},   {"atanh", f::atanh}, {"cbrt", f::cbrt},   {"cos", f::cos},
    {"cosh", f::cosh},   {"exp", f::exp},     {"expm1", f::expm1}, {"log", f::log},
    {"log1p", f::log1p}, {"log2", f::log2},   {"log10", f::log10}, {"sin", f::sin},
    {"sinh", f::sinh},   {"tan", f::tan},     {"tanh", f::tanh},
};
const BinaryEntry BINARY[] = {
    {"atan2", f::atan2},
    {"pow", f::pow},
    {"hypot", hypot2},
};

const int SAMPLES = 64;

void printUnary(const UnaryEntry& e, bool last) {
  uint32_t total = 0;
  sweep::unary([&](double) { total++; });
  sweep::Digest d;
  std::printf("    \"%s\": {\"count\": %u, ", e.name, total);
  uint32_t index = 0;
  int taken = 0;
  std::printf("\"samples\": [");
  sweep::unary([&](double x) {
    const double y = e.fn(x);
    d.add(y);
    if (taken < SAMPLES && index == static_cast<uint32_t>((static_cast<uint64_t>(taken) * total) / SAMPLES)) {
      std::printf("%s[\"%08x\", \"%08x\", \"%08x\", \"%08x\"]", taken == 0 ? "" : ", ",
                  sweep::hiw(x), sweep::low(x), sweep::hiw(y), sweep::low(y));
      taken++;
    }
    index++;
  });
  std::printf("], \"fnv\": \"%08x\", \"mix\": \"%08x\"}%s\n", d.fnv, d.mix, last ? "" : ",");
}

void printBinary(const BinaryEntry& e, bool last) {
  uint32_t total = 0;
  sweep::binary([&](double, double) { total++; });
  sweep::Digest d;
  std::printf("    \"%s\": {\"count\": %u, ", e.name, total);
  uint32_t index = 0;
  int taken = 0;
  std::printf("\"samples\": [");
  sweep::binary([&](double x, double y) {
    const double r = e.fn(x, y);
    d.add(r);
    if (taken < SAMPLES && index == static_cast<uint32_t>((static_cast<uint64_t>(taken) * total) / SAMPLES)) {
      std::printf("%s[\"%08x\", \"%08x\", \"%08x\", \"%08x\", \"%08x\", \"%08x\"]", taken == 0 ? "" : ", ",
                  sweep::hiw(x), sweep::low(x), sweep::hiw(y), sweep::low(y), sweep::hiw(r), sweep::low(r));
      taken++;
    }
    index++;
  });
  std::printf("], \"fnv\": \"%08x\", \"mix\": \"%08x\"}%s\n", d.fnv, d.mix, last ? "" : ",");
}

int dump(const char* name) {
  for (const auto& e : UNARY) {
    if (std::strcmp(e.name, name) == 0) {
      sweep::unary([&](double x) {
        const double y = e.fn(x);
        std::printf("%08x%08x %08x%08x\n", sweep::hiw(x), sweep::low(x), sweep::hiw(y), sweep::low(y));
      });
      return 0;
    }
  }
  for (const auto& e : BINARY) {
    if (std::strcmp(e.name, name) == 0) {
      sweep::binary([&](double x, double y) {
        const double r = e.fn(x, y);
        std::printf("%08x%08x %08x%08x %08x%08x\n", sweep::hiw(x), sweep::low(x), sweep::hiw(y),
                    sweep::low(y), sweep::hiw(r), sweep::low(r));
      });
      return 0;
    }
  }
  std::fprintf(stderr, "unknown function %s\n", name);
  return 2;
}

}  // namespace

int main(int argc, char** argv) {
  if (argc == 3 && std::strcmp(argv[1], "--dump") == 0) {
    return dump(argv[2]);
  }
  if (argc != 1) {
    std::fprintf(stderr, "usage: golden [--dump <function>]\n");
    return 2;
  }
  std::printf("{\n");
  std::printf("  \"generator\": \"tools/fdlibm-ref/golden.cc over tools/fdlibm-ref/ieee754.cc (V8 12.4 fdlibm), sweep.h\",\n");
  std::printf("  \"functions\": {\n");
  const int nu = sizeof(UNARY) / sizeof(UNARY[0]);
  const int nb = sizeof(BINARY) / sizeof(BINARY[0]);
  for (int i = 0; i < nu; i++) printUnary(UNARY[i], false);
  for (int i = 0; i < nb; i++) printBinary(BINARY[i], i == nb - 1);
  std::printf("  }\n}\n");
  return 0;
}
