/* Windows-only definitions for three symbols Chrome's Skia static library references but does not define
 * on Windows (their source files are not compiled into skia.lib): libwebp's AVX2 lossless dispatch inits
 * and expat's rand_s entropy source. Linked into korvus.exe so the final MSVC link resolves.
 *
 * VP8LDspInitAVX2 / VP8LEncDspInitAVX2: libwebp's own lossless_avx2.c / lossless_enc_avx2.c define these
 * as empty WEBP_DSP_INIT_STUB stubs when AVX2 is not built, leaving the dispatch tables at their C/SSE
 * entries. VP8 LOSSLESS output is bit-identical regardless of SIMD, so an empty init is the exact
 * behaviour libwebp ships for the no-AVX2 case -- no fingerprint change.
 *
 * writeRandomBytes_rand_s: expat's random_rand_s.c; seeds the parser's hash salt (anti-DoS), which never
 * affects parse output. Implemented with rand_s, exactly as expat does, so the entropy is real.
 */

void VP8LDspInitAVX2(void) {}
void VP8LEncDspInitAVX2(void) {}

#define _CRT_RAND_S
#include <stdlib.h>
#include <string.h>

/* expat declares `bool writeRandomBytes_rand_s(void *target, size_t count)`; returning int 0/1 is ABI
 * compatible with a bool-reading caller on x64 (low byte of EAX). */
int writeRandomBytes_rand_s(void *target, size_t count) {
  unsigned char *p = (unsigned char *)target;
  size_t i = 0;
  while (i < count) {
    unsigned int r;
    if (rand_s(&r) != 0) {
      return 0;
    }
    size_t n = (count - i < sizeof(r)) ? (count - i) : sizeof(r);
    memcpy(p + i, &r, n);
    i += n;
  }
  return 1;
}
