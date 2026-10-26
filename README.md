# korvus

A browser engine for AI agents and automation, written from scratch in Rust. It speaks the Chrome DevTools Protocol,
so Playwright, Puppeteer and any CDP client drive it as they drive Chrome.

## Install

Linux (x86-64, glibc) and macOS (Apple silicon):

```sh
curl -fsSL https://github.com/KorvusBrowser/korvus-browser/releases/latest/download/install.sh | sh
```

Windows (x86-64), in PowerShell:

```powershell
irm https://github.com/KorvusBrowser/korvus-browser/releases/latest/download/install.ps1 | iex
```

The installer checks the download against the release's `SHA256SUMS`, unpacks it into `~/.korvus/dist/<version>`,
points `~/.korvus/current` at it, adds that to your `PATH` and runs `korvus setup`. Run it again to update. On a Linux
server without fontconfig, install it first (`sudo apt-get install libfontconfig1`). Linux on arm64 has no build yet.

You can also unpack an archive from [Releases](https://github.com/KorvusBrowser/korvus-browser/releases) yourself and
run `korvus` in it: ANGLE's two libraries, `plugins/` and `personas/` stay beside the binary.

## First run

`korvus setup` asks two questions and writes the answers to `~/.korvus/config.toml`:

- **Stealth by default?** Yes: pages see the persona the `korvus-stealth` plugin gives them (a desktop device and its
  browser). No: pages see korvus as itself (`Korvus/<version>` in the user agent, `navigator.webdriver` true).
- **Rotate the persona for new sessions?** Yes: every new browser context gets a new persona. No: one persona until
  you change it.

Without a terminal (CI, a container) it writes stealth with rotation and asks nothing.

## Serve

```sh
korvus serve                 # a DevTools endpoint on 127.0.0.1:9222
```

```js
// Playwright
const browser = await chromium.connectOverCDP("http://127.0.0.1:9222");
const context = await browser.newContext();   // one session
```

Every setting has a flag that overrides its config value for this run, and a context's own parameters override
both for that context:

| flag | what |
|---|---|
| `--port N`, `--listen ADDR` | where the endpoint listens |
| `--allow-origin ORIGIN` | a web page's origin that may open the endpoint (`*` for any); without it a WebSocket from a page is refused, as Chrome's `--remote-allow-origins` refuses it |
| `--allow-host NAME` | a name the endpoint is reached by besides IPs and `localhost` (a reverse proxy's); any other `Host` is refused, against DNS rebinding |
| `--identity stealth\|korvus` | the stealth persona, or korvus as itself |
| `--rotate rotate\|fixed\|named\|off`, `--seed S`, `--device D` | a new persona per session, one from a fixed seed, a named persona's, or the build's own; the seed; a device |
| `--proxy URL` | an HTTP or SOCKS5 proxy (with a login); time zone and locale follow its exit IP |
| `--timezone TZ`, `--locale L` | fixed values instead |
| `--humanize` / `--no-humanize` | human-like pointer paths |
| `--host ignore\|strict` | a stealth persona presents a device unlike this machine; `ignore` (the default under stealth) runs anyway, `strict` refuses |
| `--plugin PATH[:CONFIG]` | load a plugin ([plugin SDK](plugin-sdk/README.md)) |
| `--fresh` | every named persona starts with no cookies or storage and keeps none (a clean, repeatable run) |
| `--block KINDS` | drop requests before the network, to spend no bandwidth on what a scrape does not need: a list of Fetch kinds (`image`, `font`, `media`, `script`, `style`, `xhr`, …) and `host:<substr>` (a top-level navigation is never dropped by a kind rule). The page sees what DevTools' blocked-URL list makes it see |
| `--http-cache-max MB` | cap the in-memory HTTP cache below its 50 MB default (`0` turns it off), so more pages run at once; never above 50 (Chrome's own budget) |

`korvus serve` also prints the address of its own client -- a browser front end with tabs, popups as windows and
dialogs -- to open in your browser: `korvus client: http://127.0.0.1:9222/client/#ticket=…`. The ticket is the key to
the browser; keep the address to yourself. The client is source-available ([client](client/README.md)) and can be put in
your own page.

Named personas keep their profile (cookies, storage) across runs: `korvus persona new <name>`, then connect to
`http://127.0.0.1:9222/persona/<name>`. `korvus persona set <name> proxy=… timezone=…` gives one its own session
settings; `korvus persona ls` and `korvus persona rm <name>` manage them. `korvus persona reset <name>` drops a
profile, `korvus persona export <name> [file]` writes it as JSON (with its cookies, HttpOnly ones included) and
`korvus persona import <name> <file>` puts one back; `"persistent": false` in a persona's `session` keeps none. `korvus
plugin ls` lists the loaded plugins and the slots they fill. `korvus stats` shows the active and queued sessions.

A context starts as its persona, and what a client asks for over DevTools is honoured as Chrome honours it: Playwright's
`newContext({ userAgent, locale, timezoneId, viewport, colorScheme, geolocation, permissions, javaScriptEnabled,
bypassCSP, offline, extraHTTPHeaders, ignoreHTTPSErrors, acceptDownloads })`, `context.cookies()` and `storageState()`,
`page.exposeFunction`, screenshots and downloads. An overridden user agent changes the strings, not the engine under
them -- as in Chrome; pass `userAgentMetadata` (`Emulation.setUserAgentOverride`) for the client hints to match it.

Frames, popups and workers are what Chrome makes them: `page.frames` and `frameLocator` reach into same-origin and
cross-origin `<iframe>`s, a popup is a page of its own (`page.on('popup')`) while its opener keeps scripting it, and
`worker.evaluate` runs in the worker. `page.setContent`, `page.pdf()`, `dragTo`, `httpCredentials`,
`page.on('websocket')`, touch emulation and file uploads through a form work as against Chrome.

## Run a script in a page

```sh
korvus run sign.js --url https://www.example.com/ --html page.html --map https://www.example.com/sdk/=./sdk --offline --cookies
```

`sign.js` runs in a document at `--url` -- its origin, DOM, cookies and storage -- after the page's own scripts; its
value is printed (a promise is awaited). `--html` is the document's markup (otherwise it is fetched), `--map
<prefix>=<dir>` answers requests from disk, `--offline` fails every other request, `--cookies` prints the page's
cookies, `--wait SECS` lets the page run first (3 by default). Without `--url`, `korvus run` runs the file in a bare
realm with no document.

For a challenge the SDK fetches dynamically, `--on-request <cmd>` is a responder: a request no `--html`/`--map`
answered is handed to `<cmd>` as one JSON line on stdin (`url`, `method`, `headers`, `body_base64`, `destination`),
and its one JSON line on stdout is the answer -- `{"fulfill":{"status":200,"headers":{…},"body":"…"}}` (a body is
text, or `{"base64":"…"}` for bytes), `{"continue":{}}`, `{"fail":"net::ERR_…"}`, or `null` to fall through to
`--offline`/the network. So an SDK's challenge is answered with no `serve`+CDP relay to build. `--as <name>` sets the
script's own source name (default `<anonymous>`, as `eval()` names it).

The script runs as the browser itself -- as Chrome 152, the milestone korvus presents -- so mint the token under the
client that ships it. For a clean mint each time, give each run its own session: over CDP, a `newContext()` per mint
(each is a fresh persona with no cookies or storage under D8); with `korvus run`, each invocation is already its own.

## Plans

A session is a browser context a client opened a page in. korvus is licensed by how many run at once:

| | sessions at once |
|---|---|
| no account | 1 per machine |
| free account (`korvus login`) | 3; 10 for the first 30 days |
| paid plans | by sessions |

Every plan has every feature. Over your plan's limit, a new session waits for a free place, first come first
served; a running session is never stopped. `korvus login` signs in with GitHub, `korvus status` shows your plan,
and `korvus activate <key>` takes a key from your account page. A plan change reaches a running `korvus serve`
within a minute.

Only `korvus serve` talks to korvus's license server, once a minute: your license key, this install's id, how many
sessions are active and the korvus version. Pages send nothing to us, and nothing about pages, URLs, proxies or
personas is ever sent. The [binary license](BINARY-LICENSE.md) says exactly what is sent and when.

## Plugins

korvus is built from plugins behind a stable C interface, and you can load your own: a page's Web APIs, its network
requests, its identity, what it reads back from a canvas, and more. A plugin is a shared library built against
[`plugin-sdk/korvus_plugin.h`](plugin-sdk/korvus_plugin.h) alone. See [plugin-sdk/README.md](plugin-sdk/README.md)
and the [examples](plugin-sdk/examples).

**A plugin you load that korvus did not ship runs at your own risk**: it runs inside korvus with korvus's access to
your machine and your pages.

## License

- The engine source and documentation in this repository are under the [Business Source License 1.1](LICENSE)
  (source-available; see the license for the change date and the additional-use grant).
- The plugin interface — the C ABI header (`plugin-sdk/korvus_plugin.h`), the SDKs and the examples — is under the
  [MIT License](LICENSE.plugins), so anyone may write and share plugins.
- The korvus binary is under the [Korvus Binary License](BINARY-LICENSE.md). Running it on your own machines for your
  own work, including scraping, automation and testing, needs no further license. You need a separate license to
  redistribute the binary, bundle it into a product, or offer it to others as a hosted service. For that, open an issue
  that starts with "Licensing:".
