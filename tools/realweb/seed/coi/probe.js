// The clock probe Cloudflare's challenge runs, plus the isolation facts it can read.
function gran() { var a = 1, b = 1, c, d = 0; for (; 5E3 > d; d++) { var e = performance.now(), f = performance.now(); e < f && (c = f - e, c > a && c < b ? b = c : c < a && (b = a, a = c)); } return +a.toFixed(6); }
function facts() { return { isolated: self.crossOriginIsolated, sab: typeof SharedArrayBuffer, clock: gran() }; }
