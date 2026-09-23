// Stub for V8's src/base/macros.h: the three things ieee754.cc uses.
#ifndef V8_BASE_MACROS_H_
#define V8_BASE_MACROS_H_

#include <cstring>

#define V8_WARN_UNUSED_RESULT
#define V8_INLINE inline

namespace v8 {
namespace base {

template <class Dest, class Source>
inline Dest bit_cast(const Source& source) {
  static_assert(sizeof(Dest) == sizeof(Source), "bit_cast needs equal sizes");
  Dest dest;
  std::memcpy(&dest, &source, sizeof(dest));
  return dest;
}

}  // namespace base
}  // namespace v8

#endif  // V8_BASE_MACROS_H_
