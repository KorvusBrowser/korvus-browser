// Shim for Chromium's base/compiler_specific.h: the one macro ieee754.cc uses (korvus vendoring).
#pragma once
// As Chromium's own macro does, pick the compiler's always-inline spelling: MSVC's cl.exe (the cc crate's
// Windows compiler) rejects __attribute__, and wants __forceinline; clang and gcc take the attribute.
#if defined(_MSC_VER) && !defined(__clang__)
#define ALWAYS_INLINE __forceinline
#else
#define ALWAYS_INLINE inline __attribute__((__always_inline__))
#endif
