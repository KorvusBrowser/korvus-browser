#ifndef KORVUS_PLUGIN_H_
#define KORVUS_PLUGIN_H_

// korvus plugin interface -- a stable C ABI. A plugin is a shared library (a .so on Linux, a .dylib on macOS, a .dll on
// Windows) built STANDALONE against this header alone: it includes nothing of korvus's, and korvus includes nothing of
// the plugin's. korvus loads it at run time (`korvus serve --plugin <path>[:<config.json>]`, `[[plugin]]` in
// `~/.korvus/config.toml`, or a context's `korvus.plugins`), calls its one exported function, `korvus_plugin_init`, and
// from then on calls the slot callbacks the plugin filled in.
//
// A plugin runs in-process with korvus's own rights. Nothing sandboxes it: what it does is its author's responsibility.
//
// SLOTS. Each is optional; a NULL callback means "this plugin does not fill that slot".
//
//   rotation, device, identity   asked when a browser context is made, in the endpoint's process (the router)
//   on_call                      Web API answers: the exit of every member the plugin claims in `labels`
//   on_request, on_response      the network: before each request hop, after each response
//   plan_move, type_text,        input: the mouse paths, keystrokes and wheel steps a CDP client's input becomes when
//   scroll, press_dwell_us       humanize is on
//   on_event                     an observer: context, page, navigation, request, response and console events
//   readback_kinds, on_readback  readbacks (ABI 2): the pixels a page reads back from a canvas or a WebGL drawing
//                                buffer, and the samples of an offline audio render, to perturb in place
//
// ORDER. Several plugins may fill one slot. For the answer slots (rotation, device, identity, the input ones) korvus asks
// the plugins from the LAST given to the first; the first that answers wins, and one that passes (returns 0 / NULL)
// hands over to the next, korvus's own answer last. For the chain slots (on_call, on_request, on_response) every
// plugin runs in the order given, each seeing what the previous left: for on_call the last `set_*` wins, for
// on_request the first BLOCK, FULFILL or HOLD ends the chain (HOLD parks the hop until a later resume_request).
// Observers all see every event.
//
// THREADS. on_call runs on the thread of the realm that made the call (a page's, or a worker's own); on_request and
// on_response on the network threads; several at once. A plugin is thread-safe.
//
// RULES. Never unwind across this boundary (a C++ plugin catches everything). Do not call back into page APIs from a
// callback. Invalid input (a NULL string, unparseable JSON, a buffer asked of a call that returns none) is ignored and
// korvus's own answer stays. A crash in a page-side callback ends that page's process; in a router-side one (rotation,
// device, identity) it ends the endpoint.

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The ABI version this header describes. korvus loads a plugin built for any version up to its own; a plugin uses a
// host field only if `host->abi` is at least the version that added it. New fields and callbacks are only ever appended.
//   ABI 1: the slots above but readbacks.
//   ABI 2: readback_kinds and on_readback (KorvusReadback); KorvusContextInfo.exit_ip; KorvusHost.persona_json.
//   ABI 3: KorvusHost.metric (a per-context counter the router serves) and KorvusHost.page_eval (run a script on the
//          page's own thread and read its result), then emit_cdp_event, res_set_body, res_remote_ip, res_http_version
//          and the plugin's cdp_domains / on_cdp_call. Wave 1 appends, at the end of each struct: KorvusHost's
//          set_rng_seed, set_viewport, inject_mouse / inject_key / inject_wheel and capture_png, and KorvusPlugin's
//          on_call_pre, verify_cert, init_script, want_frames / on_frame (KorvusFrame), on_dialog and on_download.
//          Step 5 appends KorvusPlugin's storage_load / storage_save (E1 profiles).
//          Step 9 appends KorvusHost's virtual_time_freeze / virtual_time_advance / virtual_time_resume (F5 virtual
//          time).
//          Step 8 appends, at the end of each struct: KorvusHost's hibernate_context / resume_context (G5) and
//          kv_get / kv_set (A4 shared KV), and KorvusPlugin's on_connect / context_allow / queue_order
//          (G1 / G2 / G4 multi-tenancy).
//          Step 10 appends, at the end of each struct: KorvusHost's navigate / reload (B5, drive the page) and
//          add_binding (B4, page bindings), and KorvusPlugin's on_binding (B4), on_file_chooser (7.3, file
//          chooser) and choose_proxy (D2, a new context's proxy).
//          Step 12 (Frontier S) appends, at the end of each struct: KorvusHost's inject_touch / inject_ime (B6,
//          touch + IME input, beside inject_mouse) and clipboard_write (C5), and KorvusPlugin's on_proxy_auth
//          (D3), on_cookie (D9), on_storage_change (E2), on_clipboard_read (C5), on_permission and geolocation,
//          and on_cursor / on_focus_state (C3).
//          Step 13 (Frontier M) appends, at the end of KorvusPlugin: resolve (D4, DNS resolution) and
//          on_target_open (popup / target control). No new KorvusHost call; the worker realm constants
//          (KORVUS_REALM_DEDICATED_WORKER / _SHARED_WORKER / _SERVICE_WORKER) were already defined.
//          Step 14 (Frontier L) appends, at the end of each struct: KorvusHost's req_hold_id / resume_request /
//          modify_and_resume (D7, async on_request hold -- a plugin parks a request and releases it later), with
//          which the on_request verdict KORVUS_HOLD is defined; and KorvusPlugin's cache_get / cache_put (D10, a
//          plugin-backed response cache).
//          More ABI-3 calls are appended as the plugin surface opens.
//   ABI 4: KorvusHost.return_json and KorvusHost.call_stack, appended at the end of KorvusHost -- read a claimed
//          member's whole call at on_call (its return value as JSON, and the JS stack it was called from), so a
//          tracer plugin records args + result + stack rather than only a per-member tally. A host built before
//          ABI 4 does not carry them, so check `host->abi >= 4` before calling them.
#define KORVUS_PLUGIN_ABI 4u

// The realm a call is made from.
#define KORVUS_REALM_PAGE 0
#define KORVUS_REALM_DEDICATED_WORKER 1
#define KORVUS_REALM_SHARED_WORKER 2
#define KORVUS_REALM_SERVICE_WORKER 3
#define KORVUS_REALM_OTHER 4

// What a readback is (KorvusReadback.kind).
#define KORVUS_READBACK_CANVAS 0  // a 2D canvas read back: getImageData, toDataURL, toBlob, convertToBlob
#define KORVUS_READBACK_WEBGL 1   // a WebGL drawing buffer read back: readPixels, a WebGL canvas's exports
#define KORVUS_READBACK_AUDIO 2   // an offline audio render's samples (OfflineAudioContext.startRendering)
#define KORVUS_READBACK_TEXT 3    // ABI 3: a text measurement -- measureText / TextMetrics widths. See KorvusReadback.

// ── THE FINGERPRINT TAP SET ──
// korvus's anti-detection surface reaches a plugin through two mechanisms that share one seed discipline. korvus
// exposes only the POINT a fingerprint is read at and a per-persona seed; WHAT to perturb and BY HOW MUCH is the
// plugin's -- there is no noise model in the engine or in this header (the J6 boundary).
//
//   (1) READBACKS (readback_kinds / on_readback, KorvusReadback): a value korvus produced is handed to the plugin
//       to perturb IN PLACE before the page reads it.
//         KORVUS_READBACK_CANVAS  2D canvas pixels       (getImageData, toDataURL, toBlob, convertToBlob)
//         KORVUS_READBACK_WEBGL   WebGL buffer pixels     (readPixels, a WebGL canvas's exports)
//         KORVUS_READBACK_AUDIO   offline-render samples  (OfflineAudioContext.startRendering)
//         KORVUS_READBACK_TEXT    text measurement: metrics = [width, fontBoundingBoxAscent, fontBoundingBoxDescent]
//                                 in CSS px (measureText) -- the font-metric fingerprint. See KorvusReadback.
//
//   (2) PARAMETER SURFACES (labels / on_call / on_call_pre): the member reports a device/build constant, so the
//       plugin claims the member's label and replaces its answer with set_number / set_bool / set_string / set_json
//       (on_call at the member's exit; on_call_pre before its body, with KORVUS_SKIP to stand in for the body). A
//       member no plugin claims costs exactly what it did with no plugin (I7). Labels are "<Interface>.<method>" for
//       a method and "<Interface>.get <attr>" for a getter. The fingerprint members:
//         WebGLRenderingContext.getParameter / WebGL2RenderingContext.getParameter
//             the GPU oracle. pname is arg 0; gate on it and answer the matching type. UNMASKED_RENDERER_WEBGL
//             (37446) and UNMASKED_VENDOR_WEBGL (37445) are strings, gated behind
//             getExtension("WEBGL_debug_renderer_info"); so are the GL identity strings VENDOR (7936) / RENDERER
//             (7937) / VERSION (7938) / SHADING_LANGUAGE_VERSION (35724); the device limits and ranges are numbers
//             or typed arrays.
//         WebGLRenderingContext.getSupportedExtensions / WebGL2RenderingContext.getSupportedExtensions
//             the advertised extension list (set_json, an array of strings).
//         WebGLShaderPrecisionFormat.get rangeMin / .get rangeMax / .get precision
//             the shader-precision triple (getShaderPrecisionFormat mints the object; the clean points are its getters).
//         MediaCapabilities.decodingInfo / MediaCapabilities.encodingInfo, RTCRtpSender.getCapabilities /
//             RTCRtpReceiver.getCapabilities, HTMLMediaElement.canPlayType (+ its video/audio twins),
//             MediaSource.isTypeSupported / MediaRecorder.isTypeSupported
//             the codec lists and the "" / "maybe" / "probably" / bool codec answers (set_json / set_string).
//         GPU.getPreferredCanvasFormat / GPU.get wgslLanguageFeatures (WebGPU build constants). GPUAdapterInfo.*
//             is a FORWARD point only: navigator.gpu.requestAdapter() resolves null in this build, so no adapter
//             info is handed to a page to tap yet.
//
// THE SEED. host->seed(domain) returns a uint32 stable for one persona and one domain, different for another (keyed
// by the context's license). Key a per-persona, per-site shift off it -- the same persona reproduces the same
// fingerprint, a different persona gets a different one -- whether perturbing readback metrics or a parameter answer.
// korvus hands you the point and the seed; the shift is yours.

// on_request's verdicts.
#define KORVUS_CONTINUE 0
#define KORVUS_BLOCK 1
#define KORVUS_FULFILL 2
// step 14 (D7): PARK this hop -- do not send it, and run no later network plugin for it, until a resume. The
// plugin reads the hop's id with host->req_hold_id (during on_request) and later releases it with
// host->resume_request / modify_and_resume. Ends the on_request chain, like BLOCK and FULFILL.
#define KORVUS_HOLD 3

// on_call_pre's result (ABI 3, wave 1): skip the claimed member's own body and use what the plugin set with set_*.
// Any other value (0) lets the body run.
#define KORVUS_SKIP 1

// Log levels.
#define KORVUS_LOG_INFO 0
#define KORVUS_LOG_WARN 1
#define KORVUS_LOG_ERROR 2

// Opaque handles, read and written only through KorvusHost.
typedef struct KorvusCall KorvusCall;          // one Web API call, at its exit
typedef struct KorvusRequest KorvusRequest;    // one request hop, before it is sent
typedef struct KorvusResponse KorvusResponse;  // one response, as it arrives

// A browser context being made (router side).
typedef struct KorvusContextInfo {
  const char* context_id;   // the context's CDP id
  const char* name;         // the persona's name (`/persona/<name>`, `korvus.name`), or ""
  const char* host_os;      // "linux", "macos" or "windows"
  const char* proxy;        // the context's proxy URL, or ""
  const char* identity;     // the identity mode asked for ("stealth", "korvus"), or ""
  const char* config_json;  // the context's `korvus.*` settings as one JSON object
  const char* exit_ip;      // ABI 2: where the context's proxy exits, when it has one and it answered; else ""
} KorvusContextInfo;

// A readback (ABI 2): pixels (canvas, WebGL: `rgba`, width * height * 4 bytes, top-left origin) or samples (audio:
// `samples`, every channel as rendered). Perturb in place; the page reads what the plugins leave.
typedef struct KorvusReadback {
  int kind;  // KORVUS_READBACK_*
  uint32_t width, height;
  uint8_t* rgba;
  float* samples;
  size_t count;  // bytes of rgba, or samples -- or, for KORVUS_READBACK_TEXT, the number of `metrics`
  // ABI 3: set only when kind == KORVUS_READBACK_TEXT (NULL / 0 otherwise). A per-call tap on text measurement
  // (`measureText`), the font-enumeration-by-width fingerprint. `text` is the measured string (read-only).
  // `metrics` is perturbed IN PLACE, in CSS px, in a FIXED order that is now part of the ABI:
  //   metrics[0] = width, metrics[1] = fontBoundingBoxAscent, metrics[2] = fontBoundingBoxDescent   (count == 3)
  // korvus derives the full TextMetrics (actualBoundingBox*, the baselines) from the perturbed values, so a shift
  // stays self-consistent across them. What to shift, and by how much, is the plugin's (key it with
  // host->seed(domain) for a stable-per-persona, per-site noise); korvus exposes only the point, never a model.
  const char* text;
  double* metrics;
} KorvusReadback;

typedef struct KorvusPoint {
  double x, y;
} KorvusPoint;

typedef struct KorvusMotionSample {
  int32_t x, y;
  double t_us;  // microseconds from the move's start
} KorvusMotionSample;

typedef struct KorvusKeyEvent {
  uint32_t codepoint;  // the character the key types
  double down_us;      // when it goes down, microseconds from the text's start
  double up_us;        // when it comes up
} KorvusKeyEvent;

typedef struct KorvusWheelEvent {
  double dy;    // the step's delta, CSS pixels
  double t_us;  // microseconds from the scroll's start
} KorvusWheelEvent;

// An observer's event: what happened (`kind`: "context.created", "context.closed", "page.created", "page.closed",
// "navigation", "request", "response", "console") and its details as one JSON object.
typedef struct KorvusEvent {
  const char* kind;
  const char* json;
} KorvusEvent;

// A painted frame handed to a plugin's on_frame (ABI 3, wave 1, C1 -- the D29 streaming core): `rgba` is
// width * height * 4 bytes, top-left origin, `len` its length in bytes, and `t_us` the frame's timestamp in
// microseconds from the page's start. The pixels are valid only for the on_frame call; copy what you keep.
typedef struct KorvusFrame {
  uint32_t width, height;
  const uint8_t* rgba;
  size_t len;
  double t_us;
} KorvusFrame;

// What korvus offers a plugin, handed to `korvus_plugin_init` (store the pointer: it lives as long as the process).
typedef struct KorvusHost {
  uint32_t abi;  // the host's ABI version
  uint32_t size; // sizeof(KorvusHost) as the host knows it

  void (*log)(int level, const char* utf8);
  const char* (*context_id)(void);    // the process's browser context ("" outside one)
  const char* (*persona_name)(void);  // the context's persona name, or ""
  // The context's noise seed for `domain`, keyed by its license: stable for one persona, different for another.
  uint32_t (*seed)(const char* domain);

  // ── on_call (a compatible host ABI, ABI 2 — same names and meaning) ──
  const char* (*label)(KorvusCall*);              // "Navigator.get platform", "WebGLRenderingContext.getParameter", ...
  int (*argc)(KorvusCall*);
  int (*arg_is_number)(KorvusCall*, int index);
  double (*arg_number)(KorvusCall*, int index);   // 0 when absent or not a number
  int (*arg_is_string)(KorvusCall*, int index);
  // Argument `index` as UTF-8 into buf[0..cap), NUL-terminated and cut to fit; its full length in bytes.
  size_t (*arg_string)(KorvusCall*, int index, char* buf, size_t cap);
  void (*set_number)(KorvusCall*, double value);
  void (*set_bool)(KorvusCall*, int value);
  void (*set_string)(KorvusCall*, const char* utf8);
  // Any JSON value, built as an array/object; wrapped in a resolved Promise when the call returns one.
  void (*set_json)(KorvusCall*, const char* json_utf8);
  // The returned RGBA bytes (ImageData.data, a byte view), to perturb in place: 1 and the buffer, or 0.
  int (*return_bytes)(KorvusCall*, uint8_t** ptr, size_t* len);
  int (*return_floats)(KorvusCall*, float** ptr, size_t* count);  // a Float32Array (getChannelData)
  int (*realm)(KorvusCall*);                                       // KORVUS_REALM_*

  // ── on_request ──
  const char* (*req_url)(KorvusRequest*);
  const char* (*req_method)(KorvusRequest*);
  const char* (*req_header)(KorvusRequest*, const char* name);  // NULL when absent
  const char* (*req_headers_json)(KorvusRequest*);              // [[name, value], ...]
  // "document", "script", "style", "image", "font", "fetch", "xhr", "websocket", "preflight", "worker", "other"
  const char* (*req_destination)(KorvusRequest*);
  const char* (*req_top_level_url)(KorvusRequest*);
  void (*req_set_url)(KorvusRequest*, const char* url);
  void (*req_set_method)(KorvusRequest*, const char* method);
  void (*req_set_header)(KorvusRequest*, const char* name, const char* value_or_null);  // NULL removes it
  void (*req_set_body)(KorvusRequest*, const uint8_t* data, size_t len);
  void (*req_set_proxy)(KorvusRequest*, const char* proxy_url_or_direct);  // "socks5://u:p@h:1080", "direct"
  // The answer this hop gets instead of being sent (with KORVUS_FULFILL): status, [[name, value], ...], body.
  void (*req_fulfill)(KorvusRequest*, int status, const char* headers_json, const uint8_t* body, size_t len);

  // ── on_response ──
  int (*res_status)(KorvusResponse*);
  const char* (*res_url)(KorvusResponse*);
  const char* (*res_header)(KorvusResponse*, const char* name);
  const char* (*res_headers_json)(KorvusResponse*);
  int (*res_body)(KorvusResponse*, const uint8_t** ptr, size_t* len);  // when the plugin asked for bodies
  void (*res_set_header)(KorvusResponse*, const char* name, const char* value_or_null);

  // ── ABI 2 ──
  // The persona document this process wears (shared/personas/FORMAT.md), as JSON; "" while it wears korvus's own
  // identity.
  const char* (*persona_json)(void);

  // ── ABI 3 ──
  // Record a measurement for this context. `name` is a short metric key ("requests", "bytes_in", "blocked"),
  // `value` is added to that key's running total, and `labels_json` is an object of string labels or "" / NULL for
  // none. The router keeps the per-context totals and serves them (GET /korvus/metrics); `korvus stats` and an
  // exporter read them. Cheap and lock-light: call it from any thread, on any slot.
  void (*metric)(const char* name, double value, const char* labels_json);

  // Run `script` (JavaScript source) on the page's own thread and read its result as JSON. Returns the length the
  // result would need; writes up to `cap`-1 bytes into `out` (NUL-terminated) when `out` is non-NULL. This is the
  // one safe way to touch the page from a callback -- the host hops to the realm's thread, runs the script there at
  // the next task boundary, and hands the result back; `world` names an isolated world or is "" / NULL for the
  // page's own. 0 when there is no page (a router-side callback), when the script throws, or when a page thread is
  // not reachable. Blocking: it does not return until the page has run the script.
  size_t (*page_eval)(const char* world, const char* script, char* out, size_t cap);

  // Emit a CDP event to the connected clients, under a domain this plugin claims in `cdp_domains`. `method` is the
  // full event name (`"Korvus_acme.progress"`) and `params_json` is its params object as JSON (or "" / NULL for
  // none). The event reaches every client attached to the browser endpoint, the way a browser-level CDP event
  // does. Call it from any thread.
  void (*emit_cdp_event)(const char* method, const char* params_json);

  // Replace a response's body, from `on_response` (the decoded bytes, after any content-coding). The page then
  // parses/reads these bytes, and Content-Length is updated to match. Pass the whole new body. Use it to rewrite
  // what a page receives -- inject a script, strip a block, serve a stub. Set the plugin's `want_bodies` to use
  // this: it tells korvus to buffer the whole response (rather than stream it to the page) so `on_response` can
  // rewrite it before the page reads a byte; it also lets you read the old body first (`res_body`).
  void (*res_set_body)(KorvusResponse*, const uint8_t* data, size_t len);

  // The connection a response came over, from `on_response`: `res_remote_ip` is the address the terminal hop
  // connected to (`"93.184.216.34:443"`), `res_http_version` the version it spoke (`"http/1.1"`, `"h2"`, `"h3"`).
  // Both are "" when there was no live connection (a cache hit, or a response another plugin fulfilled). The
  // strings are valid until `on_response` returns.
  const char* (*res_remote_ip)(KorvusResponse*);
  const char* (*res_http_version)(KorvusResponse*);

  // ── ABI 3 (wave 1) ──
  // F6: force the page's pseudo-random stream. `seed` seeds every Math.random()-class source the page draws from in
  // this process, so a persona's randomness is reproducible. Call it once, early (from `korvus_plugin_init`); the
  // host keeps the value per process and the last one set wins.
  void (*set_rng_seed)(uint64_t seed);

  // C2: override the viewport this process presents -- inner size `width` x `height` in CSS pixels and device pixel
  // ratio `dpr`. The host keeps it per process; the engine lays out and reports to the page accordingly. Call it
  // early; the last value set wins.
  void (*set_viewport)(uint32_t width, uint32_t height, double dpr);

  // B6: inject a synthetic input event into the page, as though the user made it (D29 interaction). `inject_mouse`'s
  // `type` is "move"|"down"|"up" (`x`,`y` in CSS pixels; `button` 0 left / 1 middle / 2 right), `inject_key`'s is
  // "down"|"up"|"char" (`key` a DOM key name or the typed character), `inject_wheel` scrolls by `dy` at `x`,`y`. Each
  // is queued and delivered on the page's own thread, in the order called.
  void (*inject_mouse)(const char* type, double x, double y, int button);
  void (*inject_key)(const char* type, const char* key);
  void (*inject_wheel)(double x, double y, double dy);

  // C6: capture the page as a PNG. On success it writes a pointer to the encoded bytes into `*out`, their length into
  // `*len`, and returns 1; it returns 0 when no page can be captured here (a router-side process, or nothing rendered
  // yet). The bytes belong to the host and stay valid until this thread's next capture_png call -- copy them to keep
  // them longer, and do not free them.
  int (*capture_png)(uint8_t** out, size_t* len);

  // ── ABI 3 (step 9) ──
  // F5: virtual time. Pin the engine's clock and step it by hand, so a page's timers, animations and
  // `performance.now()` run on a clock the plugin controls rather than the wall clock -- a run is
  // reproducible and a long wait costs no real time. Until a plugin calls one of these the clock is the
  // real OS clock, unchanged.
  //
  //   virtual_time_freeze  pins the clock at the moment of the call: it stops advancing on its own.
  //                        `performance.now()`, `Event.timeStamp`, `requestAnimationFrame` timestamps,
  //                        the document timeline and the event-loop timer queue all read this pinned
  //                        time. Calling it again while already pinned keeps the advance already applied.
  //   virtual_time_advance jumps the pinned clock forward by `ms` and lets every timer then due fire, in
  //                        order, with no real waiting -- a setTimeout(f, 10000) runs as soon as 10000 ms
  //                        have been advanced. `ms` adds to however far the clock has already advanced; a
  //                        non-positive or non-finite `ms` is ignored. It implies a freeze if none was
  //                        taken yet.
  //   virtual_time_resume  drops virtual time: the clock is the real OS clock again.
  //
  // Call them from any thread; the engine applies an advance at its next event-loop turn. NOTE: the V8
  // `Date` object is seeded from the OS clock by V8 itself, so `Date.now()` and `new Date()` are NOT
  // virtualized -- only the monotonic clock that the page's scheduling and `performance.now()` read is.
  void (*virtual_time_freeze)(void);
  void (*virtual_time_advance)(double ms);
  void (*virtual_time_resume)(void);

  // ── ABI 3 (step 8 / step 9 G5): tenancy, the router's side ──
  // These are router services a tenancy plugin triggers from the endpoint process (where on_connect, context_allow
  // and queue_order run). The router installs the handler; until it does they are no-ops and kv_get returns NULL.
  // Each takes a context's CDP id, not the opaque handle on_call's host calls take -- a router-side plugin manages
  // many contexts and names the one it means. (Note: the host calls above -- metric, virtual_time_* -- take no
  // plugin self either; the plugin's self is handed to its own slots, not to the host.)
  //
  // G5: idle a context and bring it back (D55 multi-tenancy). hibernate_context tears the context's page process
  // down but keeps its saved state; resume_context brings it back on demand. A context id the router does not know
  // is ignored.
  void (*hibernate_context)(const char* context);
  void (*resume_context)(const char* context);

  // A4: a small per-context key/value store the router keeps, for a tenancy plugin to carry state across a context's
  // hibernate/resume (a quota counter, a tenant tag). kv_get returns the value for `key` under `context`, or NULL
  // when absent; the returned string belongs to the host and stays valid until this thread's next kv_get call -- copy
  // it to keep it longer, and do not free it. kv_set stores `value` under `(context, key)`.
  const char* (*kv_get)(const char* context, const char* key);
  void (*kv_set)(const char* context, const char* key, const char* value);

  // ── ABI 3 (step 10) ──
  // B5 (2.4): drive this process's page from a plugin. `navigate` loads `url` in the current page; `reload`
  // reloads it. Each is queued and carried out on the page's own loop at its next turn, in the order called --
  // the one safe way to steer navigation from a callback, as `page_eval` is the one safe way to touch the page's
  // script. No-op in a process with no page (a router-side callback). Like the other host calls, they take no
  // plugin self: a page process drives its one page.
  void (*navigate)(const char* url);
  void (*reload)(void);

  // B4 (2.3): ask the engine to install a global function `name` on every page realm. When page JS calls
  // `name(payloadString)`, the engine calls every plugin's `on_binding(name, payload)` with that one string
  // argument. Call it early (from `korvus_plugin_init`); the set of names is kept per process and a name added
  // twice is installed once. A binding no plugin adds is never installed, so a page gains no extra global.
  void (*add_binding)(const char* name);

  // ── ABI 3 (step 12, Frontier S) ──
  // B6: inject a synthetic TOUCH or IME event, beside inject_mouse / inject_key / inject_wheel (D29 interaction).
  // inject_touch's `type` is "start"|"move"|"end" at (`x`,`y`) in CSS pixels -- a single-point touch, as a
  // finger makes it. inject_ime commits `text` as composed input -- the string an input method delivers when it
  // commits (a CJK word, an emoji), not a run of key presses. Each is queued and delivered on the page's own
  // thread, in the order called, on the same queue inject_mouse uses.
  void (*inject_touch)(const char* type, double x, double y);
  void (*inject_ime)(const char* text);

  // C5: write `text` to the page's clipboard -- a synthetic copy, so a later paste reads it. Queued and carried
  // out on the page's own loop at its next turn, in the order called (like navigate); a process with no page
  // drops it. Pair with the plugin's on_clipboard_read slot, which answers a page's clipboard READ.
  void (*clipboard_write)(const char* text);

  // ── ABI 3 (step 14, Frontier L) ──
  // D7: async on_request hold -- let a plugin PARK a request and release it later, from another thread or a
  // separate callback. From INSIDE on_request a plugin calls req_hold_id(req) to learn the id korvus assigns this
  // hop (idempotent: the same id if asked again for the same hop) and returns KORVUS_HOLD. The hop is then parked
  // -- not sent, and no later network plugin runs for it -- until the plugin releases it. From any thread
  // afterwards it calls resume_request(id, KORVUS_CONTINUE) to release the hop (send it) or resume_request(id,
  // KORVUS_BLOCK) to abort it; any verdict other than KORVUS_BLOCK releases it. modify_and_resume first replaces
  // the hop's url / method / headers / body and then applies the verdict: each string argument is NULL (and
  // `body` a NULL pointer) to leave that part unchanged, `headers_json` is [[name, value], ...] as
  // req_headers_json gives it, and `body`/`len` the new body (len 0 with a non-NULL pointer empties it). An id
  // korvus does not know (already released, or never held) is ignored. req_hold_id is valid only during the
  // on_request call it is asked in. Cost is nothing until a plugin first holds. Like the other host calls these
  // take no plugin self -- the id names the parked hop, and req_hold_id takes the request handle as the other
  // req_* accessors do.
  unsigned long (*req_hold_id)(KorvusRequest* req);
  void (*resume_request)(unsigned long id, int verdict);
  void (*modify_and_resume)(unsigned long id, int verdict, const char* url, const char* method,
                            const char* headers_json, const uint8_t* body, size_t len);

  // ABI 3 (fake-media, D-media): feed one RGBA frame (top-left origin, width*height*4 bytes) for synthetic track
  // `track_id` -- the id from an on_user_media descriptor. korvus copies the bytes and paints the latest frame
  // into any <video> whose srcObject carries that track; a null or mis-sized buffer is ignored. Call it on your
  // own cadence; korvus presents the most recent frame. Appended at the struct end -- ABI stays 3.
  void (*push_video_frame)(const char* track_id, uint32_t width, uint32_t height, const uint8_t* rgba, size_t len);

  // ABI 3 (WebExtension message bus, P1b): install a global function `name` in the NAMED ISOLATED world `world`
  // (the world an init_world_script named), NOT the main world -- e.g. an extension's content<->layer message
  // channel. A page call to it reaches on_binding, exactly like add_binding, but it is visible ONLY in that
  // isolated world, so it never leaks into any page's main world. A world no plugin binds gets nothing. Call it
  // in korvus_plugin_init beside init_world_count/init_world_script. Appended at the struct end -- ABI stays 3.
  void (*add_world_binding)(const char* world, const char* name);

  // ABI 3 (WebExtension MV3 background): spawn a background realm that runs `src` at `url`. `kind` is "service" (a
  // ServiceWorkerGlobalScope realm with its own isolate, thread and event loop) -- any other kind returns 0. `url`
  // is the realm's base (a chrome-extension:// origin) that importScripts / a relative fetch resolve against, and
  // `src` is the background script (prepend any shim, e.g. a chrome.runtime, into `src` -- do NOT rely on an
  // init_script, which would also inject into a page's OWN service worker). Returns the realm's worker id (> 0) or
  // 0 (no page yet / unsupported kind / failed). Call it once the page exists (e.g. from the plugin's worker
  // thread, retrying until it returns > 0). A realm is created ONLY if a plugin calls this, so a no-plugin page is
  // unaffected. Appended at the struct end -- ABI stays 3.
  int (*spawn_background)(const char* kind, const char* url, const char* src);

  // ── ABI 4 ──
  // Read a claimed member's whole call, at on_call. Each writes up to cap-1 bytes + a NUL into buf and returns the
  // full byte length (as arg_string does), so a 0 return is "none" and a return >= cap means "call again with a
  // bigger buffer". Present only when host->abi >= 4.
  //   return_json  the member's return value as JSON; 0 for undefined, a function or a symbol.
  //   call_stack   the JS stack at the call, up to `depth` frames, deepest first, one "func url:line:col" per line;
  //                0 when there is no stack. depth <= 0 is treated as 1.
  size_t (*return_json)(KorvusCall*, char* buf, size_t cap);
  size_t (*call_stack)(KorvusCall*, int depth, char* buf, size_t cap);
} KorvusHost;

// What a plugin is, returned by `korvus_plugin_init`. Every callback is optional.
typedef struct KorvusPlugin {
  uint32_t abi;   // the KORVUS_PLUGIN_ABI the plugin was built with
  uint32_t size;  // sizeof(KorvusPlugin) as the plugin knows it: korvus reads no field past it
  const char* name;
  const char* version;
  void* self;     // handed back to every callback
  // The Web API members on_call answers, NULL-terminated ("Navigator.get platform"); "*" claims every member. A
  // member no loaded plugin claims costs exactly what it cost with no plugin loaded.
  const char* const* labels;
  int want_bodies;  // non-zero: on_response may read response bodies

  // rotation: the seed and stable name of a new context. 1 when answered, 0 to pass.
  int (*rotation)(void* self, const KorvusContextInfo* ctx, uint64_t* seed, char* name, size_t name_cap);
  // device: the device a new context presents (a persona document's `device` section), as JSON; NULL to pass.
  const char* (*device)(void* self, const KorvusContextInfo* ctx, uint64_t seed);
  // identity: the whole persona document a new context wears, given the device chosen above; NULL to pass.
  const char* (*identity)(void* self, const KorvusContextInfo* ctx, const char* device_json);

  void (*on_call)(void* self, KorvusCall* call);
  int (*on_request)(void* self, KorvusRequest* req);  // KORVUS_CONTINUE / KORVUS_BLOCK / KORVUS_FULFILL / KORVUS_HOLD
  void (*on_response)(void* self, KorvusResponse* res);

  // input: fill out[0..cap) and return how many samples the whole path has (call again with more room if larger).
  size_t (*plan_move)(void* self, KorvusPoint from, KorvusPoint to, double target_width, KorvusMotionSample* out, size_t cap);
  size_t (*type_text)(void* self, const char* utf8, KorvusKeyEvent* out, size_t cap);
  size_t (*scroll)(void* self, double dy, KorvusWheelEvent* out, size_t cap);
  double (*press_dwell_us)(void* self);  // how long a click's button stays down; < 0 to pass

  void (*on_event)(void* self, const KorvusEvent* ev);

  // Frees a string this plugin returned (from device, identity); NULL when they are static.
  void (*free_string)(void* self, const char* s);
  void (*shutdown)(void* self);

  // ── ABI 2 ──
  // Which readbacks this plugin perturbs now: a mask of (1u << KORVUS_READBACK_*). Asked before each readback, so the
  // answer may follow the persona; a readback no plugin asks for is read exactly as with no plugin loaded. NULL with
  // on_readback set: every kind -- but only the kinds that existed at ABI 2 (CANVAS, WEBGL, AUDIO); a kind added
  // later (TEXT, ABI 3) is delivered ONLY to a plugin that names it in this mask, so an older plugin is never handed
  // a readback shape it was not written for.
  uint32_t (*readback_kinds)(void* self);
  void (*on_readback)(void* self, KorvusReadback* rb);

  // ── ABI 3 ──
  // CDP domains this plugin serves: a NULL-terminated array of domain names (`"Korvus_acme"`), or NULL for none. A
  // client's CDP call whose method is `<domain>.<member>` for one of these is handed to `on_cdp_call` instead of
  // the engine; the plugin may also push events under the domain with `host->emit_cdp_event`. A domain name must
  // begin `Korvus_` so it cannot shadow a real CDP domain. Loaded in the endpoint (router) process.
  const char* const* cdp_domains;
  // Answer one CDP call on a claimed domain: `method` is the full name, `params_json` its params as JSON ("" when
  // none). Return the result object as JSON (freed via `free_string`), or NULL to fail the call. Runs on the
  // endpoint's thread, so it must not block.
  const char* (*on_cdp_call)(void* self, const char* method, const char* params_json);

  // ── ABI 3 (wave 1) ──
  // F1: run BEFORE a claimed member's body (on_call runs after it). Claimed by the same `labels` as on_call; read the
  // call and set its answer through the host table exactly as in on_call. Return KORVUS_SKIP to skip the member's own
  // body and use what you set with set_*; return 0 to let the body run (your set_* seeds it, and on_call still runs
  // after). Every claiming plugin's on_call_pre runs in load order; the body is skipped if any returns KORVUS_SKIP.
  int (*on_call_pre)(void* self, KorvusCall* call);

  // 4.5: verify a TLS server certificate. `host` is the name connected to and `leaf_der_b64` the leaf certificate's
  // DER, base64-encoded. Return 1 to allow the connection, 0 to deny it, or -1 to defer to the next plugin and then
  // korvus's own verification. The first plugin that returns 1 or 0 decides.
  int (*verify_cert)(void* self, const char* host, const char* leaf_der_b64);

  // B3: a script to run before the page's own scripts in a new realm. `realm_kind` is a KORVUS_REALM_* value; return
  // the JavaScript source to inject (freed via `free_string`), or NULL to inject nothing in that realm. Every plugin's
  // init_script runs, in load order, each before any of the page's scripts.
  const char* (*init_script)(void* self, int realm_kind);

  // C1: the frame tap (the D29 streaming core). Set want_frames non-zero to receive on_frame each time this process's
  // page paints a CHANGED picture -- right after the page's animation-frame callbacks, and at most once per display
  // refresh (the persona's screen rate, 60 Hz when not captured) for changes made otherwise (a timer, input, a fetch).
  // A still page sends nothing, so do not use on_frame as a clock: work a plugin must do whatever the page shows (a
  // queued click) belongs on a thread of its own. on_frame is handed a KorvusFrame (RGBA, top-left origin) valid only
  // for the call -- copy what you keep; do not block. A frame no plugin wants is never captured.
  int want_frames;
  void (*on_frame)(void* self, const KorvusFrame* f);

  // Step 7: a JavaScript dialog is being shown. `kind` is "alert", "confirm" or "prompt" and `message` its text.
  // Return 1 to accept it (for a prompt, write the reply as UTF-8 into out[0..cap), NUL-terminated and cut to fit), 0
  // to dismiss it, or -1 to defer to the next plugin and then korvus's default. The first plugin that returns 1 or 0
  // decides.
  int (*on_dialog)(void* self, const char* kind, const char* message, char* out, size_t cap);

  // Step 7: a download finished. `url` is where it came from, `suggested_name` the file name the page proposed, and
  // `data`[0..len) its bytes (valid only for the call). Every plugin with on_download set is told, in load order.
  void (*on_download)(void* self, const char* url, const char* suggested_name, const uint8_t* data, size_t len);

  // ── ABI 3 (step 5) ──
  // E1 profiles: persist and restore a context's state so a login survives a context's teardown and reuse.
  // storage_load returns a previously-saved state blob -- an opaque UTF-8 string the plugin stored (e.g. base64) -- to
  // SEED a new context, or NULL for none; korvus frees it via free_string. storage_save persists the context's state
  // blob at its teardown. The first plugin whose storage_load returns non-NULL seeds the context; every plugin with
  // storage_save set is told at teardown, in load order.
  const char* (*storage_load)(void* self, const KorvusContextInfo* ctx);
  void (*storage_save)(void* self, const KorvusContextInfo* ctx, const char* blob);

  // ── ABI 3 (step 8) ──
  // Multi-tenancy (D55). Like rotation / device / identity, these are asked in the endpoint (router) process; they
  // decide who gets in, how many contexts a tenant may run, and who waits. Filling any of them makes the plugin a
  // tenancy authority.
  //
  // G1: a CDP/HTTP client is connecting. `auth` is its bearer token (from the Authorization header or `?token=`, ""
  // when none) and `peer` its address. Return a tenant id >= 0 to admit the client -- the connection is tagged with
  // that tenant -- or a value < 0 to REFUSE it before any session starts. Every plugin with on_connect is asked in
  // load order; the first that refuses wins, else the first non-negative id is the admitted tenant.
  long (*on_connect)(void* self, const char* auth, const char* peer);

  // G2: may `tenant` create the context `context` now? Asked as a context is made, for a quota decision. Return 1 to
  // allow, 0 to refuse. Every plugin with context_allow is asked and all must allow. A refusal blocks only the new
  // context; it never stops a context already running (invariant I3).
  int (*context_allow)(void* self, long tenant, const char* context);

  // G4: the sessions waiting under the license cap belong to the tenants in `tenants`[0..n). Return the INDEX into
  // that array of the session to admit next -- tenant fairness WITHIN the cap (it cannot change the cap). Optional:
  // korvus's own order stands when no plugin fills it or a plugin returns an out-of-range index.
  long (*queue_order)(void* self, const long* tenants, unsigned n);

  // ── ABI 3 (step 10) ──
  // B4 (2.3): a page called a bound global that a plugin registered with `host->add_binding`. `name` is the
  // binding's name and `payload` the single string argument the page passed (`name(payloadString)`). Every plugin
  // with on_binding set is told, in load order. Runs on the calling realm's own thread.
  void (*on_binding)(void* self, const char* name, const char* payload);

  // 7.3: a page opened a file chooser (an `<input type=file>` click, `showOpenFilePicker`). `accept` is the
  // chooser's accept hint ("image/*,.pdf", "" when none). Return a newline-separated list of ABSOLUTE file paths
  // to supply to the input, or NULL / "" to cancel the chooser (the page gets no file). The first plugin with
  // on_file_chooser set decides; its returned string is freed via `free_string`. Mirrors on_dialog.
  const char* (*on_file_chooser)(void* self, const char* accept);

  // D2 (4.2): choose the proxy a NEW context exits through, given the context being made. Return a proxy URL
  // ("socks5://u:p@h:1080", "http://h:8080") to route the context through it, or NULL / "" to leave the context's
  // own configured proxy in place. Asked in the endpoint (router) process as a context is made, from the last
  // plugin given to the first; the first non-empty answer wins. The returned string is freed via `free_string`.
  const char* (*choose_proxy)(void* self, const KorvusContextInfo* ctx);

  // ── ABI 3 (step 12, Frontier S) ──
  // D3: supply the credentials for a proxy that challenges (answers a 407). `proxy_url` is the proxy being
  // connected to. Return "user:pass" to authenticate, or NULL to pass. Asked from the last plugin given to the
  // first; the first non-empty answer wins. The returned string is freed via `free_string`.
  const char* (*on_proxy_auth)(void* self, const char* proxy_url);

  // D9: a cookie was set or deleted (a Set-Cookie, a `document.cookie` write). `ctx` is the context's CDP id,
  // `name` and `domain` the cookie's, and `op` is "set" or "delete". Every plugin with on_cookie set is told,
  // in load order -- an observer of cookie changes. A cookie no plugin watches costs nothing.
  void (*on_cookie)(void* self, const char* ctx, const char* name, const char* domain, const char* op);

  // E2: a storage area changed, beside the E1 storage slot. `ctx` is the context's CDP id and `slot` names the
  // area touched ("local", "session", "indexeddb", "cache", ...). Every plugin with on_storage_change set is
  // told, in load order.
  void (*on_storage_change)(void* self, const char* ctx, const char* slot);

  // C5: answer a page's clipboard READ (a paste). Return the text the page pastes, or NULL to pass (the page
  // reads the real clipboard). Asked from the last plugin given to the first; the first non-NULL answer wins.
  // The returned string is freed via `free_string`. Pair with host->clipboard_write, which seeds a copy.
  const char* (*on_clipboard_read)(void* self);

  // A permission prompt: `name` is the permission asked for ("geolocation", "notifications", "camera", ...).
  // Return 1 to grant, 0 to deny, or -1 to defer to the next plugin and then korvus's default. The first plugin
  // that returns 1 or 0 decides. Dynamic, unlike the static persona. Mirrors on_dialog.
  int (*on_permission)(void* self, const char* name);

  // The geolocation a context reports, for `navigator.geolocation` to read. `ctx` is the context's CDP id;
  // return a JSON object {"lat":<deg>,"lng":<deg>,"acc":<metres>} to supply a position, or NULL to pass (the
  // real position, or none). Asked from the last plugin given to the first; the first non-empty answer wins.
  // The returned string is freed via `free_string`.
  const char* (*geolocation)(void* self, const char* ctx);

  // C3: the cursor's shape changed. `shape` is a CSS cursor name ("default", "pointer", "text", ...). Every
  // plugin with on_cursor set is told, in load order -- an observer of the cursor the page would show (e.g.
  // "pointer" over a hovered link).
  void (*on_cursor)(void* self, const char* shape);

  // C3: the page's focus state changed. `focused` is non-zero when the page has focus; `editable` is non-zero
  // when the focused node takes text input (an input, a textarea, a contenteditable) -- an IME would compose
  // into it. Every plugin with on_focus_state set is told, in load order.
  void (*on_focus_state)(void* self, int focused, int editable);

  // ── ABI 3 (step 13, Frontier M) ──
  // D4: resolve a host name. `host` is the name being looked up. Return a comma-separated list of IP addresses to
  // use for it ("93.184.216.34", "93.184.216.34,2606:2800:220:1:248:1893:25c8:1946"), the literal "block" to
  // refuse the lookup, or NULL to pass (korvus resolves it normally). Asked from the last plugin given to the
  // first; the first non-NULL answer wins. The returned string is freed via `free_string`.
  const char* (*resolve)(void* self, const char* host);

  // Popup / target control: a page is about to open a new browsing target. `url` is the target's URL and
  // `disposition` how it would open ("newTab", "newWindow", "popup"). Return 1 to allow it, 0 to block it. Every
  // plugin with on_target_open set is asked, in load order, and all must allow; a target no plugin blocks opens as
  // normal. It gates only a NEW target; the page's own navigation is never asked.
  int (*on_target_open)(void* self, const char* url, const char* disposition);

  // ── ABI 3 (step 14, Frontier L) ──
  // D10: a plugin-backed response cache. korvus asks cache_get for a stored response before it fetches one, and
  // offers cache_put a response to store; a plugin backs the store with Redis, a file, memory or anything, and
  // korvus just calls the slot. cache_get returns base64 of the stored response bytes for `key`, or NULL for a
  // miss (korvus decodes the base64 itself); it is asked from the LAST plugin given to the first, and the first
  // non-NULL, decodable answer wins -- its string is freed via free_string. cache_put hands `key` and `b64` (the
  // base64 of the response bytes korvus wants stored) to every plugin with the slot, in load order. What a
  // response is keyed by is korvus's; the plugin is only the store.
  const char* (*cache_get)(void* self, const char* key);
  void (*cache_put)(void* self, const char* key, const char* b64);

  // ABI 3 (fake-media, D-media): a synthetic media source. korvus asks this what getUserMedia("user") or
  // getDisplayMedia("display") resolves to for `constraints_json` (the MediaStreamConstraints as JSON). Return a
  // JSON array of track descriptors, or NULL to pass -- the next plugin, then korvus's own honest answer, which
  // still rejects when no capture device exists (I9/J2). Asked from the last plugin given to the first; the first
  // non-NULL answer wins, and its string is freed via free_string. A descriptor is
  //   {"id":"v0","kind":"video","label":"FaceTime HD","settings":{"width":1280,"height":720,"frameRate":30}}
  // korvus mints a real MediaStreamTrack (not a canvas-capture track) per descriptor -- live, with that
  // getSettings(). Feeding the track's pixels is the push_video_frame host call (a KorvusHost member, below).
  const char* (*on_user_media)(void* self, const char* kind, const char* constraints_json);

  // ABI 3 (WebExtension MV3 compat, B3 isolated worlds): beside init_script (which feeds the realm's OWN world)
  // these feed init scripts for NAMED ISOLATED worlds -- a world with its own global scope that reads the same DOM
  // but shares no JS globals with the page or other worlds. For a realm of `realm_kind` (KORVUS_REALM_*),
  // init_world_count returns how many (world, script) pairs this plugin supplies (0 or negative = none);
  // init_world_script returns the i-th pair's JavaScript source (0 <= i < count; freed via free_string, NULL to
  // skip that index) and writes that pair's world NAME as UTF-8 into name_out[0..cap), NUL-terminated, cut to fit.
  // Pairs naming the same world run there in order; every plugin's pairs are gathered in load order. A world no
  // plugin names is never created, so a no-plugin run creates no extra world (I9/J2). Both NULL: none.
  int (*init_world_count)(void* self, int realm_kind);
  const char* (*init_world_script)(void* self, int realm_kind, int i, char* name_out, size_t cap);

  // ABI 3 (WebExtension message bus, P1b): a binding call that RETURNS a response, unlike the void on_binding. When
  // a page calls a bound global (host->add_binding / add_world_binding), korvus calls this with the binding `name`
  // and the call's argument `payload`; the returned string (freed via free_string) becomes the call's return value
  // -- so e.g. chrome.runtime.sendMessage can resolve to the plugin's answer synchronously. The first loaded plugin
  // that answers non-NULL wins; return NULL to pass (korvus then falls back to the void on_binding). Runs on the
  // page's thread, so answer inline without blocking.
  const char* (*on_binding_call)(void* self, const char* name, const char* payload);
} KorvusPlugin;

#if defined(_WIN32)
#define KORVUS_PLUGIN_EXPORT __declspec(dllexport)
#else
#define KORVUS_PLUGIN_EXPORT __attribute__((visibility("default")))
#endif

// The one exported symbol. `config_json` is this plugin's own configuration (the JSON after `--plugin path:`, or
// `config` in config.toml's [[plugin]]), "{}" when none. Return the plugin, or NULL to decline the load (say why with
// host->log first).
KORVUS_PLUGIN_EXPORT const KorvusPlugin* korvus_plugin_init(const KorvusHost* host, const char* config_json);

#ifdef __cplusplus
}  // extern "C"
#endif

#endif  // KORVUS_PLUGIN_H_
