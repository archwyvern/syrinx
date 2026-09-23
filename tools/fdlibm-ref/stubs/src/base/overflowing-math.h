// Stub for V8's src/base/overflowing-math.h: the helpers ieee754.cc uses, with the semantics of
// the originals in Node.js v22.23.1's deps/v8.
#ifndef V8_BASE_OVERFLOWING_MATH_H_
#define V8_BASE_OVERFLOWING_MATH_H_

#include <cmath>
#include <cstdint>
#include <limits>
#include <type_traits>

namespace v8 {
namespace base {

// Wrapping two's-complement negation; the template parameter picks the width.
template <typename T>
inline T NegateWithWraparound(T a) {
  using U = typename std::make_unsigned<T>::type;
  return static_cast<T>(static_cast<U>(0) - static_cast<U>(a));
}

// Wrapping subtraction.
template <typename T>
inline T SubWithWraparound(T a, T b) {
  using U = typename std::make_unsigned<T>::type;
  return static_cast<T>(static_cast<U>(a) - static_cast<U>(b));
}

// IEEE division spelled out so integer instantiations are well defined; for doubles this is
// exactly x / y.
template <typename T>
inline T Divide(T x, T y) {
  if (y != 0) return x / y;
  if (x == 0 || x != x) return std::numeric_limits<T>::quiet_NaN();
  if ((x >= 0) == (std::signbit(y) == 0)) {
    return std::numeric_limits<T>::infinity();
  }
  return -std::numeric_limits<T>::infinity();
}

}  // namespace base
}  // namespace v8

#endif  // V8_BASE_OVERFLOWING_MATH_H_
