// Stub for V8's src/base/ieee754.h: just the declarations ieee754.cc defines, without V8's export
// macros. The reference build compiles ieee754.cc verbatim against these stubs.
#ifndef V8_BASE_IEEE754_H_
#define V8_BASE_IEEE754_H_

namespace v8 {
namespace base {
namespace ieee754 {

double acos(double x);
double acosh(double x);
double asin(double x);
double asinh(double x);
double atan(double x);
double atan2(double y, double x);
double atanh(double x);
double cos(double x);
double cosh(double x);
double exp(double x);
double expm1(double x);
double log(double x);
double log1p(double x);
double log2(double x);
double log10(double x);
double cbrt(double x);
double sin(double x);
double sinh(double x);
double tan(double x);
double tanh(double x);
double pow(double x, double y);

}  // namespace ieee754
}  // namespace base
}  // namespace v8

#endif  // V8_BASE_IEEE754_H_
