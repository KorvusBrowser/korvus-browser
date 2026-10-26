// Shim for Chromium's base/bit_cast.h, all fdlibm's ieee754.cc uses of base (korvus vendoring).
#pragma once
#include <bit>
namespace base {
using std::bit_cast;
}
