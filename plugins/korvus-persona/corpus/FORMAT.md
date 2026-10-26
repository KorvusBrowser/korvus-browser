# The persona format, version 1

One persona logic for both browsers (D16) and one format (D17): what a persona IS, written once, read by the sibling browser and by
korvus alike. A persona document is one JSON object with three sections:

| section | what it is | changes when |
|---|---|---|
| `device` | the machine: its OS, processor, memory, screen, GPU and WebGL set, voices | never, for one persona |
| `build` | which browser build it presents: what a given Chrome on a given OS exposes | the browser's milestone moves |
| `session` | how a run wears it: proxy, timezone, locale, humanize, persistence, plugins | per run, per context |

```json
{
  "persona": 1,
  "name": "shop-7",
  "device": {
    "os": "windows",
    "os_version": "15.0.0",
    "architecture": "x86",
    "bitness": "64",
    "hardware": { "cores": 8, "memory_gb": 8 },
    "screen": { "width": 1536, "height": 864, "avail_width": 1536, "avail_height": 824, "dpr": 1.25 },
    "gpu": {
      "vendor": "Google Inc. (Intel)",
      "renderer": "ANGLE (Intel, Intel(R) UHD Graphics 630 (0x00003E9B) Direct3D11 vs_5_0 ps_5_0, D3D11)"
    },
    "noise": ["canvas", "audio", "webgl"]
  },
  "build": { "browser": "chrome" },
  "session": { "timezone": "Asia/Seoul", "locale": "ko-KR" }
}
```

The validator is [`format/validate.py`](format/validate.py); the corpus every reader must agree with is
[`fixtures/`](fixtures/) (§5).

## 1. The document

| field | | |
|---|---|---|
| `persona` | required | `1`, this version |
| `name` | required | a non-empty string. It names the persona and seeds its noise: one name, one device's noise. |
| `device` | required | §2 |
| `build` | optional | §3; absent: the build of the browser that wears it |
| `session` | optional | §4; absent: the run's own settings |

Fields starting with `_` (`_doc`, `_expect`) are comments: every reader ignores them. Any other field a reader does not
know is refused, so a misspelled field is an error rather than a silent default.

## 2. `device`

The facts of one machine, as the sibling browser's persona compiler (`the sibling browser's persona compiler`)
has always read them in its own file: a the sibling browser persona's facts ARE a device section.

| field | | rule |
|---|---|---|
| `os` | required | `windows`, `macos`, `linux` or `android` |
| `os_version` | optional | a string (the client hints' platform version) |
| `architecture` | optional | `x86` or `arm` |
| `bitness` | optional | `64` or `32` |
| `model` | optional | a phone's model (`android` only) |
| `brand` | optional | `chrome` (the only brand either browser presents) |
| `hardware.cores` | optional | an integer, 1-256 (`navigator.hardwareConcurrency`) |
| `hardware.memory_gb` | optional | what Chrome reports on a desktop: 2, 4, 8, 16 or 32 (`navigator.deviceMemory`) |
| `screen.width`, `screen.height` | required in `screen` | positive integers, CSS pixels |
| `screen.avail_width`, `screen.avail_height` | optional | inside the screen (default: the screen) |
| `screen.avail_top`, `screen.avail_left` | optional | the available area stays inside the screen |
| `screen.dpr` | optional | 0.5-4 |
| `screen.color_depth` | optional | 24 or 30 |
| `screen.color_gamut` | optional | `srgb`, `p3` or `rec2020` |
| `screen.hdr` | optional | a boolean |
| `screen.refresh_rate` | optional | 24-500 (Hz) |
| `screen.touch_points` | optional | a phone's (`android` only) |
| `gpu.vendor`, `gpu.renderer` | required in `gpu` | the unmasked strings; the renderer names the OS's backend: Direct3D11 (or D3D11) on Windows, Metal (or OpenGL) on macOS, OpenGL (or Vulkan) on Linux |
| `webgl.webgl1`, `webgl.webgl2` | optional | `params` (by name: `MAX_TEXTURE_SIZE`), `extensions`, `precision` (`VERTEX_LOW_FLOAT`: `[rangeMin, rangeMax, precision]`), `attributes`, `max_anisotropy` -- the sets `dataset/webgl/` derives from ANGLE's source |
| `webgpu` | optional | `vendor`, `architecture`, `device`, `description`, `limits` |
| `voices` | optional | `[{"name", "lang"}]`, the OS's speech voices |
| `window.frame` | optional | `[outerWidth - innerWidth, outerHeight - innerHeight]`, each 0-400 |
| `noise` | optional | the readbacks perturbed by the persona's seed: `canvas`, `audio`, `webgl`, `text`. `text` is the `measureText`/`TextMetrics` font box — korvus taps it (ABI-3 `KORVUS_READBACK_TEXT`); other engines accept-and-ignore it. (the sibling browser's compiler + the dataset validator still gate it to the first three until their lockstep update lands — decisions.md D94.) |

A device never states the browser's version: that is the build's (§3).

## 3. `build`

Which browser build the persona presents.

| field | | rule |
|---|---|---|
| `browser` | required in `build` | `chrome` |
| `milestone` | optional | a positive integer; absent: the wearing browser's own |
| `os` | optional | the build's OS, when it is not the device's (it must not be, today: a build is the device's OS's) |
| `bundle` | optional | korvus: the captured build to wear (`chrome-152-windows-20261017T0248`), when not the one its milestone and OS name |

the sibling browser IS a build (Chromium 155) and reads this section only to refuse a persona of another browser or milestone.
korvus wears a build as a captured bundle (`korvus/plugins/korvus-persona/bundles/`): the API surface, the CSS, the
codecs, the TLS fingerprint of that Chrome on that OS.

## 4. `session`

How a run wears the persona. Every field is optional, and a run's own setting (a flag, a context's parameter) wins
over the document's.

| field | rule |
|---|---|
| `proxy` | a URL: `http://`, `https://` or `socks5://`, a login in it when the proxy wants one |
| `timezone` | an IANA zone (`Asia/Seoul`); absent: from the proxy's exit, or the host's |
| `locale` | a BCP 47 tag (`ko-KR`) |
| `humanize` | a boolean |
| `persistent` | a boolean: the profile (cookies, storage) is kept between runs |
| `plugins` | plugin library paths, in load order |

## 5. Readers, and the corpus

A reader is right when it agrees with [`fixtures/`](fixtures/):

- every document in `fixtures/valid/` is accepted;
- every document in `fixtures/invalid/` is refused, and the reason the reader gives contains the document's
  `_expect` string.

The readers held to it today: [`format/validate.py`](format/validate.py) (`format/check_fixtures.py`), and korvus's
(`korvus/plugins/korvus-persona/src/document.rs`, its `the_shared_corpus` test). the sibling browser's compiler joins them when it
reads this format (goal 1020 step 8).

## 6. The pool

[`dataset/`](dataset/) draws the desktop pool from browserforge's model of real traffic: the devices seeds 1..8192
give, each with its weight. It is emitted twice from one draw: for the sibling build as `builtin_pool.cc`, and as data,
[`pool/desktop.json.gz`](pool/) -- each entry a `device` section with its weight and its WebGL set's index.

A selector `seed:<os>:<anything>` draws one device of that OS: FNV-1a (32-bit) of the selector, modulo the total
weight of the OS's devices, walked over the pool in order -- the same device in either browser
(`format/pool.py`, `builtin_personas.cc`'s `PoolPersona`). The draw's document is named by the selector.
