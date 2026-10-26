// The fdlibm functions Web Audio's DynamicsCompressor calls -- fdlibm::powf and fdlibm::asinf are pow
// and asin rounded to float; exp makes its metering time constant -- with C linkage for korvus-render.
// sin and cos build SincResampler's windowed-sinc kernels (decodeAudioData's rate conversion).
#include "third_party/fdlibm/ieee754.h"

extern "C" double korvus_fdlibm_pow(double x, double y) {
  return fdlibm::pow(x, y);
}

extern "C" double korvus_fdlibm_asin(double x) {
  return fdlibm::asin(x);
}

extern "C" double korvus_fdlibm_exp(double x) {
  return fdlibm::exp(x);
}

extern "C" double korvus_fdlibm_sin(double x) {
  return fdlibm::sin(x);
}

extern "C" double korvus_fdlibm_cos(double x) {
  return fdlibm::cos(x);
}
