# Plugins

A korvus plugin is a shared library built **standalone** against one header, [`korvus_plugin.h`](korvus_plugin.h):
a `.so` on Linux, a `.dylib` on macOS, a `.dll` on Windows. korvus loads it at run time — no korvus rebuild — and
calls it at the points it chooses to fill: korvus's own behaviour fills the same points.

A plugin runs inside korvus with korvus's own rights. Nothing sandboxes it: what it does is its author's
responsibility. **A plugin you load that korvus did not ship runs at your own risk**: KorvusBrowser does not review,
support or answer for it ([Korvus Binary License](../BINARY-LICENSE.md), Plugins). The plugin is yours, under the
license you choose; the header and the examples here are MIT. Every release carries the header beside its plugin,
`plugins/korvus_plugin.h`, so you can build against the release you run.

## The points (slots)

| slot | callback | korvus calls it | runs in |
|---|---|---|---|
| rotation | `rotation` | when a browser context is made: its seed and stable name | the endpoint |
| device | `device` | after rotation: the machine the context presents | the endpoint |
| identity | `identity` | after device: the whole persona document the context wears | the endpoint |
| Web API answers | `on_call` | at the exit of every member listed in `labels` | each page |
| network | `on_request`, `on_response` | before each request hop (a WebSocket's handshake too), after each response | each page |
| input | `plan_move`, `type_text`, `scroll`, `press_dwell_us` | when a CDP client's mouse moves (the path it takes) | each page |
| observer | `on_event` | context, page, request and response events | the endpoint and each page |
| readback (ABI 2) | `readback_kinds`, `on_readback` | when a page reads back a canvas's pixels, a WebGL drawing buffer, or an offline audio render's samples: perturb them in place (`readback_kinds` says which kinds, asked before each one; a readback no plugin takes is read as with none loaded) | each page |

Each callback is optional. A Web API member no loaded plugin lists in `labels` is built and called exactly as with
no plugin loaded: claiming nothing costs nothing.

**Several plugins in one slot.** The answer slots (rotation, device, identity, input) ask the plugins from the last
given to the first; the first that answers wins, and one that passes hands over to the next, korvus's own answer
last. `on_call`, `on_request` and `on_response` run every plugin in the order given, each seeing what the previous
left: for `on_call` the last `set_*` wins, for `on_request` the first `KORVUS_BLOCK` or `KORVUS_FULFILL` ends the
chain.

## Identities

What a context presents is its **identity**. With no identity plugin, korvus is korvus: Chrome's UA tokens with
`Korvus/<version>` after them, "Korvus" in the client hints' brands, this machine's hardware, timezone and locale,
`navigator.webdriver` true, and nothing read back perturbed. An identity plugin answers the `identity` slot with a
persona document -- a JSON object with its `device` (the machine), `build` (which Chrome) and `session` (timezone,
locale) -- and korvus wears it: the Chrome build it names, the device's hardware, screen, GPU and WebGL set, and the
session's timezone and locale.

korvus's stealth identity is one such plugin, `korvus-stealth`, shipped beside the binary and loaded through this
header like any other:

```sh
korvus serve --identity stealth      # korvus-stealth's identity (the default)
korvus serve --identity korvus       # korvus's own, no identity plugin
```

Your plugin replaces it: given after it (`--plugin`), your `identity` is asked first; under `--identity korvus` it is
the only one. [`examples/identity_plugin.c`](examples/identity_plugin.c) answers a device with the cores and memory its
configuration names. `host->persona_json()` (ABI 2) gives every plugin, in every page, the document the page wears.

## The ABI

A plugin exports one symbol:

```c
KORVUS_PLUGIN_EXPORT const KorvusPlugin* korvus_plugin_init(const KorvusHost* host, const char* config_json);
```

It is called once per process that loads the plugin. Keep `host` (it lives as long as the process). `config_json` is
the plugin's own configuration, `"{}"` when none was given. Return a `KorvusPlugin` whose `abi` is the
`KORVUS_PLUGIN_ABI` you built with and whose `size` is `sizeof(KorvusPlugin)`, or `NULL` to decline the load (say why
with `host->log` first).

- **Versions.** korvus loads a plugin built for any ABI up to its own and refuses a later one. Use a `KorvusHost`
  field only if `host->abi` reaches the version that added it. Fields and callbacks are only ever appended.
- **Labels** take the form `Interface.member`: `Navigator.get platform`, `WebGLRenderingContext.getParameter`,
  `HTMLCanvasElement.toDataURL`, the global's own members under `Window`. `"*"` claims every member.
- korvus prints `[korvus] plugin: loaded <name> <version> (<slots>)` on stderr when it takes a plugin, and
  `[korvus] plugin: refused <path>: <why>` when it does not. A refused plugin stops the run.

`KorvusHost` provides, for `on_call`, a compatible host ABI's calls (ABI 2) under the same names: `label`, `argc`, `arg_is_number`,
`arg_number`, `arg_is_string`, `arg_string`, `set_number`, `set_bool`, `set_string`, `set_json` (wrapped in a
resolved Promise when the member returns one), `return_bytes`, `return_floats` and `realm`; for `on_request` the
`req_*` calls (read the url, method, headers, destination; set the url, method, a header, the body, a proxy for this
request, or a fulfilment); for `on_response` the `res_*` calls; and `log`, `context_id`, `persona_name` and
`seed(domain)` everywhere. ABI 2 adds `persona_json` (the persona document this process wears, `""` for korvus's
own identity), the readback slot (`readback_kinds`, `on_readback`), and `exit_ip` in `KorvusContextInfo` (where the
context's proxy exits).

ABI 3 adds two host calls, usable from any slot:

- `metric(name, value, labels_json)` — add `value` to this context's running total for `name`, under the labels
  in `labels_json` (a JSON object, or `""` / `NULL` for none). korvus keeps the per-context totals and serves
  them, so a plugin can count requests, bytes, blocks or anything else and have it show up in the context's
  metrics without a channel of its own. Cheap and thread-safe: call it from `on_request`, `on_response`,
  `on_call`, the observer — wherever.
- `page_eval(world, script, out, cap)` — run `script` (JavaScript) on the page's own thread and read its result
  as JSON. This is the one safe way to touch the page from a callback: korvus hops to the realm's thread, runs
  the script there at the next task boundary, and hands the result back. `world` names an isolated world or is
  `""` / `NULL` for the page's own. It returns the length the result needs and writes up to `cap`-1 bytes
  (NUL-terminated) into `out`; it returns 0 when there is no page (a router-side callback), when the script
  throws, or when a page thread cannot be reached. It **blocks** until the page has run the script, so: never call
  it from the page's own thread while that thread is mid-call (an `on_call` exit — it would deadlock), and never
  call it on the **top-level document request** (the page is still fetching that document, so the call would stall
  the navigation until it times out). Call it from a sub-request, a response, or the plugin's own worker —
  somewhere the page already exists.

```c
// count every request; read the page's title on a sub-request (by when the document, and its title, exist)
static int on_request(void* self, KorvusRequest* req) {
  gHost->metric("requests", 1.0, "{\"slot\":\"network\"}");
  if (strcmp(gHost->req_destination(req), "document") != 0) {   // not the navigation itself
    char title[256];
    if (gHost->page_eval("", "document.title", title, sizeof title) > 0)
      gHost->log(KORVUS_LOG_INFO, title);
  }
  return KORVUS_CONTINUE;
}
```

## ABI 3 reference

ABI 3 opens more of korvus's own surface to a plugin: a handful of host calls, and more slots a plugin may fill.
Use any of them only when the version that added it is reached — `host->abi` for a host call, the engine's ABI for
a slot. Everything here is appended, so an older korvus simply never calls a slot it does not know and never offers
a host field past its own `size`; the two above (`metric`, `page_eval`) came first, the rest with wave 1 and after.

### Host calls (`KorvusHost`)

Usable from any slot — mind each call's thread and page notes:

- `metric(name, value, labels_json)` — add `value` to this context's running total for `name` (detailed above,
  with the example).
- `page_eval(world, script, out, cap)` — run JavaScript on the page's own thread and read its JSON result
  (detailed above). It is also how you take a **DOM or accessibility snapshot** (B7): evaluate a walk of `document`
  (or build the AX tree in script) and return it as JSON — korvus adds no snapshot call of its own, because
  `page_eval` already reaches every bit of page state.
- `emit_cdp_event(method, params_json)` — push a CDP event to every client attached to the endpoint, under a
  domain this plugin claims in `cdp_domains`. `method` is the full event name (`"Korvus_acme.progress"`),
  `params_json` its params object (`""` / `NULL` for none). It reaches clients the way a browser-level CDP event
  does. Any thread.
- `res_set_body(res, data, len)` — from `on_response`, replace the response body with `data[0..len)` (the decoded
  bytes, after any content-coding); the page parses these and Content-Length is updated to match. Set the plugin's
  `want_bodies` to use it: korvus then buffers the whole response (rather than streaming it to the page) so you can
  read the old body (`res_body`) and rewrite it before the page reads a byte — inject a script, strip a block, serve
  a stub.
- `res_remote_ip(res)`, `res_http_version(res)` — from `on_response`, the address the terminal hop connected to
  (`"93.184.216.34:443"`) and the version it spoke (`"http/1.1"`, `"h2"`, `"h3"`); both `""` for a cache hit or a
  response another plugin fulfilled. Valid until `on_response` returns.
- `set_rng_seed(seed)` — F6: force this process's pseudo-random stream, so a persona's `Math.random()`-class draws
  are reproducible. Call it once, early (from `korvus_plugin_init`); the last value set wins.
- `set_viewport(width, height, dpr)` — C2: override the viewport this process presents — inner CSS size and device
  pixel ratio. Call it early; the last value wins.
- `inject_mouse(type, x, y, button)`, `inject_key(type, key)`, `inject_wheel(x, y, dy)` — B6: inject a synthetic
  input event as though the user made it. `inject_mouse`'s `type` is `"move"`/`"down"`/`"up"` (`x`,`y` in CSS
  pixels, `button` 0 left / 1 middle / 2 right), `inject_key`'s is `"down"`/`"up"`/`"char"` (`key` a DOM key name or
  the typed character), `inject_wheel` scrolls by `dy` at `x`,`y`. Each is queued and delivered on the page's own
  thread, in the order called.
- `capture_png(out, len)` — C6: capture the page as a PNG. Writes the encoded bytes' pointer into `*out` and their
  length into `*len` and returns 1, or returns 0 off a page (a router process, or nothing rendered yet). The bytes
  belong to the host and live until this thread's next `capture_png` — copy to keep them, never free them.
- `virtual_time_freeze()`, `virtual_time_advance(ms)`, `virtual_time_resume()` — F5: control the page's clock.
  `freeze` pins it where it is (so `performance.now()`, `Event.timeStamp`, `requestAnimationFrame` and the
  `setTimeout`/`setInterval` queue stop advancing on their own); `advance(ms)` jumps it forward `ms` and fires every
  timer that falls due, with no real waiting; `resume` drops virtual time and the OS clock is back. Engage it
  **before the page loads** (as Chrome's `Emulation.setVirtualTimePolicy` is set before a navigation): a timer the
  page arms afterwards lands on the virtual timeline, so `advance` can make it due, whereas a timer armed earlier
  keeps its real deadline. These must be called from the **page's** process — a plugin reaches them from a slot that
  runs there, such as a page-level CDP domain (a session-scoped `Korvus_*` call, see `firstparty/fastforward.c`),
  not from an endpoint-only slot. Virtual time is entirely off (the OS clock, byte-for-byte) until a plugin calls
  one of these — `V8`'s `Date` and `performance.timeOrigin` stay on the real clock even then (a known limit).

### Slots (`KorvusPlugin`)

Each is optional, chosen the same way as the ABI-1/2 slots (the table at the top), and each runs where its note
says:

- `cdp_domains` / `on_cdp_call` — serve a CDP domain of your own. `cdp_domains` is a NULL-terminated array of domain
  names, each beginning `Korvus_` so it cannot shadow a real CDP domain; a client's `<domain>.<member>` call is
  handed to `on_cdp_call(method, params_json)` instead of the engine. Return the result object as JSON (freed via
  `free_string`), or NULL to fail the call. Loaded in the endpoint (router) and run on its thread, so it must not
  block. Push events under the domain with `emit_cdp_event`.
- `on_call_pre` — F1: runs BEFORE a claimed member's body (`on_call` runs after it). Claimed by the same `labels`
  as `on_call`; read the call and set its answer through the host table exactly as in `on_call`. Return
  `KORVUS_SKIP` to skip the member's own body and use what you set with `set_*`, or 0 to let the body run (your
  `set_*` seeds it, and `on_call` still runs after). Every claiming plugin's `on_call_pre` runs in load order; the
  body is skipped if any returns `KORVUS_SKIP`.
- `want_frames` / `on_frame` — C1, the frame tap (the D29 streaming core). Set `want_frames` non-zero to receive
  `on_frame` for each painted frame of this process's page, handed a `KorvusFrame` (RGBA, top-left origin, with its
  timestamp) valid only for the call — copy what you keep; do not block. A frame no plugin wants is never captured.
- `init_script` — B3: a script to run before the page's own scripts in a new realm. `realm_kind` is a
  `KORVUS_REALM_*` value; return the JavaScript source to inject (freed via `free_string`), or NULL to inject
  nothing in that realm. Every plugin's `init_script` runs, in load order, each before any of the page's scripts.
- `verify_cert` — 4.5: verify a TLS server certificate. `host` is the name connected to and `leaf_der_b64` the leaf
  certificate's DER, base64-encoded. Return 1 to allow the connection, 0 to deny it, or -1 to defer to the next
  plugin and then korvus's own verification. The first plugin that returns 1 or 0 decides.
- `on_dialog` — a JavaScript dialog is being shown. `kind` is `"alert"`, `"confirm"` or `"prompt"` and `message`
  its text. Return 1 to accept it (for a prompt, write the reply as UTF-8 into `out[0..cap)`), 0 to dismiss it, or
  -1 to defer to the next plugin and then korvus's default. The first plugin that returns 1 or 0 decides.
- `on_download` — a download finished. `url` is where it came from, `suggested_name` the file name the page
  proposed, and `data[0..len)` its bytes (valid only for the call). Every plugin with `on_download` set is told, in
  load order.
- `readback_kinds` / `on_readback` (ABI 2) — perturb a canvas, WebGL or offline-audio readback in place; see the
  slots table above.

### Storage slot — profiles (step 5)

`storage_load` / `storage_save` (E1) persist and restore a context's state, so a login survives a context's
teardown and reuse:

- `storage_load(self, ctx)` returns a previously-saved state blob — an opaque UTF-8 string the plugin stored
  (base64, say) — to **seed** a new context, or NULL for none; korvus frees it via `free_string`. The first plugin
  whose `storage_load` returns non-NULL seeds the context.
- `storage_save(self, ctx, blob)` persists the context's state blob at its teardown. Every plugin with
  `storage_save` set is told, in load order.

Treat the blob as opaque — store it keyed by `ctx->name` (or `ctx->context_id`) and hand the same bytes back on the
next `storage_load` for that persona.

## ABI 5: what a plugin installs, and a context it refuses

A plugin that needs a file to run (a database, a model) installs it itself, and one that cannot serve a context
coherently says so instead of serving it:

- `setup(self, home)` — `korvus setup` asks each loaded plugin that fills it, in load order, after config.toml is
  written (`korvus setup --plugins` asks only this, keeping config.toml; the installers run it on every install and
  update). `home` is korvus's home directory (`~/.korvus`, or `$KORVUS_HOME`): install under it. Return 0 when what
  the plugin needs is there, non-zero when it is not, and say why with `host->log`.
- `host->download(url, path, timeout_ms, err, cap)` — korvus's own GET of `url` into the file `path`, written whole or
  not at all, and only for a 200: no persona, no proxy, no page. It returns the HTTP status, or 0 for no answer, with
  why it failed in `err[0..cap)`. It blocks for up to `timeout_ms`, so call it from `setup` or a thread of your own,
  never from a page's callback. Check `host->abi >= 5` first.
- `refuse_context(self, ctx)` — a new context, before its identity is drawn (`ctx->exit_ip` is `""`, not yet known).
  Return NULL to let it be made, or the reason it must not be (freed via `free_string`): a `Target.createBrowserContext`
  then fails with it, a one-shot run ends with it, and `korvus serve` does not start when its own default context is
  refused. Every plugin that fills it is asked; the first refusal is the answer.

```c
// the file this plugin reads, installed at `korvus setup`; a context is refused while it is missing
static int setup(void* self, const char* home) {
  char path[4096], err[512];
  snprintf(path, sizeof path, "%s/example/data.bin", home);
  if (gHost->abi < 5) return 1;
  if (gHost->download("https://example.com/data.bin", path, 60000, err, sizeof err) == 200) return 0;
  gHost->log(KORVUS_LOG_WARN, err);
  return 1;
}
```

## Debugging a plugin

- **Trace the slots.** Set `KORVUS_PLUGIN_TRACE=1` in korvus's environment and korvus logs one stderr line each
  time it dispatches a plugin callback — which slot, and a short detail (a request's url, a frame's size). It is off
  by default and effectively free when off; turn it on to see exactly which of your slots korvus is calling, and in
  what order.

  ```sh
  KORVUS_PLUGIN_TRACE=1 korvus serve --plugin ./sample.so
  ```
- **Check a plugin loads.** `korvus plugin check ./sample.so` loads the plugin's descriptor alone — it calls
  `korvus_plugin_init`, then prints the `name`, `version`, `abi`, `size`, the slots it fills, its `labels` and its
  `cdp_domains`, without starting the engine. A fast way to confirm a build is a valid plugin, that its `abi` and
  `size` are what you set, and that korvus sees the slots you meant to fill.

## Build

```sh
cd examples
cc -shared -fPIC -O2 -I.. sample_plugin.c -o sample.so            # Linux (identity_plugin.c the same way)
cc -shared -O2 -I.. sample_plugin.c -o sample.dylib               # macOS
clang -shared -O2 -I.. sample_plugin.c -o sample.dll              # Windows (or cl /LD /MT /O2 /I.. sample_plugin.c)
```

## Run

```sh
korvus serve --plugin ./sample.so                      # every context of the endpoint
korvus serve --plugin ./sample.so:'{"cores": 12}'      # with its configuration (JSON after the first `:{`)
korvus fetch https://example.com/ --plugin ./sample.so --eval navigator.hardwareConcurrency
```

or in `~/.korvus/config.toml`:

```toml
[[plugin]]
path = "/abs/path/sample.so"
config = { cores = 12 }
```

A context made with `Target.createBrowserContext` can add plugins of its own (`"korvus": {"plugins": ["/abs/x.so"]}`).
Every page process of the context loads the endpoint's plugins, then the context's.

[`examples/sample_plugin.c`](examples/sample_plugin.c) answers `navigator.hardwareConcurrency` (in the page and its
workers), adds `X-Korvus-Sample: 1` to every request, refuses any host ending in `.blocked.invalid`, and counts the
events it observed.

## Rules for callbacks

- Never unwind across the boundary. A C++ plugin catches everything.
- Callbacks arrive on several threads at once: `on_call` on the thread of the realm that made the call (a page's, a
  worker's own), `on_request` and `on_response` on the network threads. Be thread-safe.
- Do not call back into page APIs from a callback.
- Invalid input (a NULL string, unparseable JSON, a buffer asked of a call that returns none) is ignored, and korvus's
  own answer stays.
- A crash in a page-side callback ends that page's process; in a rotation, device or identity callback it ends the
  endpoint.
