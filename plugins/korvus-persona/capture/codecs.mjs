// THE 9,104 QUESTIONS: what to ask a real browser about "can you play this?", and the one program
// that knows them. It records the visited browser's answers into the bundle's `codecs.json`; the
// three members that answer those questions read that file through the persona.
//
// WHY THIS EXISTS
//
// `MediaSource.isTypeSupported` returned false for every input. `HTMLMediaElement.canPlayType`
// returned "" for every input. `MediaRecorder.isTypeSupported` returned undefined. All three were
// argued for in a comment -- "Korvus has no decoders, so '' is the truthful answer" -- and all three
// are wrong for the only definition of right this project has: the recorded browser. Brave 151
// answers `"probably"` for `video/mp4; codecs="avc1.42E01E"` and `true` for
// `MediaSource.isTypeSupported('video/webm; codecs="vp8"')`.
//
// It is also the cheapest fingerprint on the page. A browser that claims to support NO codec at all
// does not exist; `canPlayType` over a dozen strings is three lines of script and no stealth axis
// this project owns looks at whether an answer is PLAUSIBLE -- they check shape, nativeness, brand
// and identity, and a constant `false` passes every one of them.
//
// WHAT IS RECORDED, AND WHAT IS NOT
//
// The grid below is a set of QUESTIONS: containers, codec families, and parameter spellings. Every
// ANSWER comes from a capture: collect.js asks this exact grid in the browser the user opened the
// capture page with, twice, and keeps what agreed. Nothing here is hand-written.
//
// This file used to drive a headless browser over CDP for the answers itself. That browser had no
// proprietary codecs -- the table it produced said `video/mp4` was "maybe" with no H.264 and no HEVC
// at all -- and it was a second session beside the one every other artifact of the persona came
// from. Retired 2026-09-07; the capture is the only input now.
//
// WHAT THE ENGINE DOES WITH IT IS NOT A MEMORISED QUERY->ANSWER MAP. The grid is reduced to a
// per-container record of the answer with no `codecs=` parameter and which codec spellings the
// container accepted -- the shape Chrome's own mime table has
// (`media/base/mime_util_internal.cc`: a container map, a codec map, and a per-container
// allow-list) -- and the parameter grammar `avc1.PPCCLL` / `vp09.PP.LL.DD` / `mp4a.40.N` /
// `av01.P.LLT.DD` is a parser in `core/korvus-webapi/src/core/dom/media_codecs.rs`. So a spelling
// the grid never asked about still gets a real answer and can still be WRONG. That is the point: a
// member that cannot answer wrongly is a stub.
//
// THIS FILE DERIVES NOTHING ANY MORE, AND THAT IS THE POINT
//
// It used to do two jobs: know the questions, and reduce the answers to a per-container table that
// `tools/sync/sync.py` compiled into `core/.../generated_media_codecs.rs`. The `.rs` came from the
// SHIPPED bundle and only the shipped bundle, so every device the engine wore answered Brave 151's
// codecs -- 274 wrong answers under Opera 135, whose Linux build ships no proprietary H.264/AAC
// decoder, and 16 under Edge 151.
//
// The reduction moved to `plugins/korvus-persona/src/codecs.rs`, which does it ONCE PER PROCESS from
// whichever bundle the persona was loaded from, and reads the container and codec lists off the
// grid's OWN KEYS -- so it cannot drift from the questions below, because it does not restate them.
// This file keeps the half nothing else can do: knowing what the 9,104 questions ARE.
//
// USAGE
//   node plugins/korvus-persona/capture/codecs.mjs --from-capture <raw-capture.json> [--source <id>] --raw <path>
// Reads a collect.js capture and writes the answer grid it read -- the bundle's `codecs.json` (goal
// I6: the bundle is one directory; emit.py --role codecs writes it there). It exits 1 if the capture
// does not answer every probe this file asks, which is the check that keeps the collector's lists and
// this file's lists together.
import { writeFileSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

// Walk up to the workspace root instead of counting `..` hops. Every tool here computed its root as
// a fixed number of parents, and every one broke the moment the file moved -- these six were three
// levels out after moving into the persona crate. A hop count is a fact about where a file sits,
// which is exactly the thing that changes.
const _repoRoot = () => {
  let d = dirname(fileURLToPath(import.meta.url));
  for (;;) {
    const m = join(d, "Cargo.toml");
    try { if (readFileSync(m, "utf8").includes("[workspace]")) return d; } catch (e) {}
    const up = dirname(d);
    if (up === d) throw new Error("cannot find the workspace root");
    d = up;
  }
};
const ROOT = _repoRoot();
const args = process.argv.slice(2);
const flag = (n, d) => { const i = args.indexOf(`--${n}`); return i >= 0 ? args[i + 1] : d; };
const RAW = flag("raw", null);
// The grid comes from a collect.js capture's `codecs.answers`: the collector asks exactly this
// file's questions in the browser the user visited it with, so the table describes the same
// session as every other artifact in the bundle. `--source` names that session.
const FROM_CAPTURE = flag("from-capture", null);
const SOURCE_FLAG = flag("source", null);
if (!FROM_CAPTURE) { console.error("usage: codecs.mjs --from-capture <raw-capture.json> [--source <id>] --raw <path>"); process.exit(2); }

// ---- THE QUESTIONS -----------------------------------------------------------------------------
// Containers worth asking about: everything Chrome is documented to have an opinion on, plus a few
// it should reject, plus the legacy spellings pages actually send.
const CONTAINERS = [
  "video/mp4", "audio/mp4", "video/webm", "audio/webm", "video/ogg", "audio/ogg",
  "application/ogg", "audio/mpeg", "audio/mp3", "audio/x-mp3", "audio/wav", "audio/x-wav",
  "audio/wave", "audio/x-pn-wav", "audio/aac", "audio/x-aac", "audio/flac", "audio/x-flac",
  "video/mp2t", "video/3gpp", "audio/3gpp", "video/quicktime", "video/x-matroska",
  "audio/x-matroska", "video/x-msvideo", "video/avi", "application/x-mpegurl",
  "application/vnd.apple.mpegurl", "audio/mpegurl", "video/mpeg", "audio/midi",
  "application/octet-stream", "text/plain", "application/x-korvus-nonsense", "", "video/", "/mp4",
];
// Codec strings: one or more per family, with the parameter spellings that decide the grammar.
const CODECS = [
  // H.264 -- profile/constraint/level triplets, plus the malformed ones that pin the parser.
  "avc1.42E01E", "avc1.42001E", "avc1.4D401E", "avc1.4D401F", "avc1.64001E", "avc1.640028",
  "avc1.58A01E", "avc1.42E0", "avc1.42E01", "avc1.ZZZZZZ", "avc1", "avc3.42E01E", "avc3",
  // H.265
  "hev1.1.6.L93.B0", "hvc1.1.6.L93.B0", "hev1", "hvc1",
  // VP8 / VP9 / AV1
  "vp8", "vp8.0", "vp9", "vp9.0", "vp09.00.10.08", "vp09.02.10.10.01.09.16.09.01",
  "vp09.00.10", "vp09", "av01.0.04M.08", "av01.0.05M.08", "av01", "av1",
  // AAC and the rest of the audio families
  "mp4a.40.2", "mp4a.40.5", "mp4a.40.29", "mp4a.40.34", "mp4a.40", "mp4a.66", "mp4a.67",
  "mp4a.69", "mp4a.6B", "mp4a.a5", "mp4a.a6", "mp4a", "mp3", "mp4a.40.99",
  "opus", "vorbis", "flac", "alac", "ac-3", "ec-3", "theora", "ulaw", "pcm", "1", "2",
  "Opus", "VP8", "AVC1.42E01E",
  // Not a codec at all.
  "x", "nonsense", "",
];
// A few multi-codec lists, which is how a page actually asks.
const COMBOS = [
  ['video/mp4', 'avc1.42E01E, mp4a.40.2'],
  ['video/mp4', 'avc1.42E01E,mp4a.40.2'],
  ['video/mp4', 'avc1.42E01E, nonsense'],
  ['video/webm', 'vp8, opus'],
  ['video/webm', 'vp9,vorbis'],
  ['video/webm', 'vp8, mp4a.40.2'],
  ['audio/webm', 'opus, vorbis'],
  ['video/mp4', 'avc1.42E01E, avc1.4D401E'],
];
// Quoting: `codecs="vp8"` and `codecs=vp8` are both legal, and a page sends both.
const QUOTINGS = [
  ['video/webm; codecs="vp8"', 'quoted'],
  ['video/webm; codecs=vp8', 'unquoted'],
  ['video/webm;codecs="vp8"', 'no-space'],
  ['video/webm ; codecs="vp8"', 'space-before-semi'],
  ['VIDEO/WEBM; CODECS="VP8"', 'upper'],
  ['video/webm; codecs="vp8 "', 'trailing-space-in-codec'],
  ['video/webm; codecs=" vp8"', 'leading-space-in-codec'],
  ['video/webm; profiles="x"; codecs="vp8"', 'extra-param'],
  ['video/webm; codecs="vp8"; codecs="vorbis"', 'duplicate-param'],
  ['video/webm; codecs=""', 'empty-codecs'],
  ['video/webm; codecs', 'bare-codecs'],
];

/** Every {id, expr} this run asks. `id` encodes what the answer is about. */
function buildProbes() {
  const p = [];
  const ask = (id, mime) => {
    const m = JSON.stringify(mime);
    p.push({ id: `v|${id}`, expr: `document.createElement("video").canPlayType(${m})` });
    p.push({ id: `a|${id}`, expr: `document.createElement("audio").canPlayType(${m})` });
    p.push({ id: `s|${id}`, expr: `MediaSource.isTypeSupported(${m})` });
    p.push({ id: `r|${id}`, expr: `MediaRecorder.isTypeSupported(${m})` });
  };
  for (const c of CONTAINERS) ask(`base\u0001${c}`, c);
  for (const c of CONTAINERS) {
    for (const k of CODECS) ask(`pair\u0001${c}\u0001${k}`, `${c}; codecs="${k}"`);
  }
  for (const [c, k] of COMBOS) ask(`combo\u0001${c}\u0001${k}`, `${c}; codecs="${k}"`);
  for (const [mime, tag] of QUOTINGS) ask(`quote\u0001${tag}\u0001${mime}`, mime);
  return p;
}

// ---- run ---------------------------------------------------------------------------------------
const probes = buildProbes();
const cap = JSON.parse(readFileSync(FROM_CAPTURE, "utf8"));
const grid = cap.codecs && cap.codecs.answers;
if (!grid) { console.error(`${FROM_CAPTURE}: no codecs.answers (captureVersion < 2)`); process.exit(2); }
// The collector keys its answers `kind|part|part...` with "|" throughout; this file keys
// `kind|part\u0001part...`. Same questions, one separator apart.
const answers = new Map();
for (const [k, v] of Object.entries(grid)) {
  const bar = k.indexOf("|");
  const id = k.slice(bar + 1).replace(/\|/g, "\u0001");
  answers.set(`${k.slice(0, bar)}|${id}`, { v });
}
let unasked = 0;
for (const p of probes) if (!answers.has(p.id)) unasked++;
const cond = cap.conditions || {};
const m = /Chrome\/(\d+)/.exec(cond.ua || "");
const major = m ? m[1] : "unknown";
const engine = cond.isBrave ? "brave" : "chrome";
const SOURCE = SOURCE_FLAG || `${engine}-${major}-${/Macintosh/.test(cond.ua || "") ? "mac" : /Windows/.test(cond.ua || "") ? "windows" : "linux"}-capture`;
console.log(`from capture: ${answers.size} answers for ${probes.length} probes` +
  (unasked ? `  (${unasked} probes this capture did not ask -- the collector's lists have drifted from this file's)` : "") +
  `  -> ${SOURCE}`);
if (unasked) process.exit(1);

if (!RAW) {
  console.error("codecs.mjs: --raw <path> is required -- the answer grid IS the output now");
  process.exit(2);
}
const raw = {};
for (const [k, v] of answers) raw[k.replace(/\u0001/g, " :: ")] = v;
writeFileSync(RAW, JSON.stringify({ source: SOURCE, answers: raw }, null, 2) + "\n");
console.log(`  raw grid -> ${RAW}  (${Object.keys(raw).length} answers)`);

// THE SWEEP ANSWERED SOMETHING FOR EVERY SHAPE IT ASKS. Not a derivation -- a check that the capture
// is a capture and not a page of `undefined`. `combo` and `quote` have no consumer in the engine (the
// grammar computes them from the `base`/`pair` answers, which is exactly the property that makes it a
// grammar and not a lookup table), so they are counted here rather than emitted anywhere.
let blank = 0;
for (const p of probes) { const r = answers.get(p.id); if (!r || !("v" in r) || r.v === undefined) blank++; }
if (blank) { console.error(`  ${blank} of ${probes.length} probes answered \`undefined\` -- this capture did not run the grid`); process.exit(1); }
const containers = new Set();
for (const k of answers.keys()) { const p = k.split("\u0001"); if (p[0].endsWith("|base")) containers.add(p[1]); }
console.log(`  ${containers.size} containers asked, every probe answered`);
