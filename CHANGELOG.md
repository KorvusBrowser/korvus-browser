# Changelog

What changed in each korvus release, newest first. A release's notes on GitHub are its section here.

## Unreleased

- Puppeteer's locators (`locator.click()`, `fill()`, `select()`) and chrome-devtools-mcp's `click`, `fill` and
  `fill_form` work: the observer Puppeteer's isolated world makes to see an element in the viewport was dropped the
  first time the page's own script ran a frame, so a locator waited until it timed out ("did not become
  interactive"). chrome-devtools-mcp's `hover` works too: `DOM.resolveNode` resolves a text node (a snapshot's
  `StaticText`) and `DOM.getContentQuads` measures one, as Chrome does.
- A page's session answers the browser's window commands as Chrome's does (`Browser.getWindowForTarget` with no target
  is the page's own window), and `Browser.setContentsSize` and `Browser.getWindowBounds` are answered, so Puppeteer's
  `page.resize()` (chrome-devtools-mcp's `resize_page`) sizes the viewport. `Emulation.setCPUThrottlingRate` takes a
  rate that throttles nothing (1 or less) and refuses a higher one rather than ignoring it.
- `Page.navigate` to a `data:` URL loads its document, as Chrome does (`page.goto('data:text/html,…')`); it loaded an
  empty page.
- What a page requests after its `load` event is reported after `Page.loadEventFired`, as Chrome reports it. It came
  before, so a client that waits for network quiet counted from `load` on (Stagehand's `networkidle`) missed the
  requests a `load` handler's timers start and read a page before its data had been asked for.
- The `--no-render` / `--render` flag is gone. It was never a memory lever (a page's cost is its V8 process either
  way — about 3 MB between the two), and a browser with no WebGL is a louder fingerprint, not a quieter one, for the
  one job korvus has. A host with no usable GPU already gets no WebGL on its own, exactly as Chrome does, so nothing
  is lost. (Render can still be turned off for a GPU-less CI job with the `KORVUS_RENDER=off` environment variable.)
- `korvus serve --block=image,font,media,host:…` drops requests before the network (a scrape spends no bandwidth on
  what it does not need), and `--http-cache-max=<MB>` caps the in-memory HTTP cache (0 turns it off) so more pages run
  at once.
- `korvus run --on-request <cmd>` answers a request dynamically from a command (for an SDK's challenge, with no
  `serve`+CDP relay to build); `--as <name>` names the script (default `<anonymous>`, so a run leaves no `korvus:` tell
  in a captured stack).
- `korvus --analysis-out <dir>` turns the call recorder on by flag; the release ships an analysis build
  (`korvus-analysis`) beside the stock binary, and a build without the recorder says so instead of recording nothing.
- A CDP `Debugger` domain: `Debugger.enable` reports each classic script the page parses (`scriptParsed`) and hands
  back its text (`getScriptSource`); `setBreakpointByUrl` and `setPauseOnExceptions` are accepted. The Sources side of
  DevTools (and JS-analysis tools) now attach to korvus instead of erroring. Live pause/step is not offered yet.
- Plugin ABI 4: an `on_call` tracer can read a claimed member's return value as JSON (`host->return_json`) and the JS
  call stack it was called from (`host->call_stack`), so a tracer records args + result + stack, not just a tally.
- What a DevTools client asks for is honoured as Chrome honours it: Playwright's context options (`userAgent`, `locale`,
  `timezoneId`, `viewport`, `colorScheme`, `geolocation`, `permissions`, `javaScriptEnabled`, `bypassCSP`, `offline`,
  `extraHTTPHeaders`, `ignoreHTTPSErrors`, `acceptDownloads`) and their page-level twins.
- `context.cookies()`, `addCookies()`, `clearCookies()` and `storageState()` work over `connectOverCDP`.
- Console messages and page errors reach the client; `page.exposeFunction`, response bodies, downloads, file choosers,
  `tap()`, JPEG/WebP, clipped and full-page screenshots work; `fill()` replaces a prefilled value and the arrow keys
  move the caret.
- `korvus serve --fresh`, `korvus persona reset|export|import`, and `"persistent": false` in a persona document.
- `korvus run <file.js> --url <page>`: a script run in a page, its files from disk, offline if asked.
- Frames are reported and driven as Chrome's are: `page.frames`, `frameLocator`, `frame.evaluate`, clicks and typing
  inside same-origin and cross-origin `<iframe>`s.
- A popup a page opens is a page of its own to the client, same-site or not (`page.on('popup')`, `context.on('page')`),
  and a `target=_blank` link opens one.
- `worker.evaluate` and a worker's console messages; `page.setContent`; `page.pdf()`; HTML drag and drop (`dragTo`);
  `httpCredentials`; `page.on('websocket')` with its frames; touch emulation's `ontouchstart` and taps at the element;
  files uploaded through a form arrive with their contents.

## v152.0.5

- A second navigation no longer ends the page's process when the page being left logged something as it went (a
  console message, an error in its `unload`); what it logged is reported as Chrome reports it.
- `Network.requestWillBeSent` carries a request's body (`hasPostData`, `postData`, `postDataEntries`) and
  `Network.getRequestPostData` answers it, so a client sees what a form or a `fetch()` POSTed.
- `Runtime.evaluate` honours `timeout`: a script that runs past it is stopped ("Execution was terminated") and the page
  runs on. An awaited promise is not cut short by it, as in Chrome.
- `korvus fetch --timeout` bounds the whole run, the load included: a page still waiting on the network when the
  deadline falls due is printed as it stands, as headless Chrome's `--timeout` does, and the run ends.
- `korvus <command> --help` says what the command takes and runs nothing (`korvus serve --help` started a server).
- The macOS persona's `sec-ch-ua` and `navigator.userAgentData` list "Google Chrome" beside Chromium, as Google Chrome
  does, and its speech voices are Google Chrome's: it had been captured from Chrome for Testing, which lists Chromium
  alone.
- `korvus setup` installs what the stealth identity needs to follow a proxy: DB-IP's IP-to-city database ("IP
  Geolocation by DB-IP", CC BY 4.0), fetched from db-ip.com under `~/.korvus/geo/`, so a persona's time zone and
  locale are those of where its proxy exits. The installers run it; `korvus setup --plugins` runs it again. Without
  the database, a context behind a proxy is refused with the reason, rather than given a time zone its IP
  contradicts.
- `Korvus.getIdentity` on a page's session: the headers its requests carry (`sec-ch-ua`, `user-agent`,
  `accept-language` and the rest, in their order), the client hints, the TLS and HTTP/2 preset, the proxy, the time
  zone, the locale and the context's cookies -- what a client needs to carry a session on with an HTTP client of its
  own, wearing the same identity.
- `encodedDataLength` (`Network.responseReceived`, `Network.loadingFinished`) is the body as it came over the wire,
  still compressed, not its decoded length; `--requests` gives both (`bytes`, `encoded_bytes`).
- `--max-heap <MB>` caps each page's JavaScript heap, so a machine running many sessions knows what each may take.
- Plugin ABI 5: `setup` (what `korvus setup` asks of a plugin), `refuse_context` (a new context a plugin cannot serve,
  refused with its reason) and `host->download` (korvus's own GET of a file).
- A CDP `Accessibility` domain: `getFullAXTree` (with `depth` and `frameId`), `getPartialAXTree`, `queryAXTree`,
  `getRootAXNode`, `getChildAXNodes` and `getAXNodeAndAncestors` answer the page's accessibility tree as Chrome builds
  it: each element's role, name, description, value and states, the nodes Chrome keeps as ignored, and text as its
  lines lay it out. On a page with every HTML element and ARIA role, the tree a client reads matches Chrome 150's
  node for node. Stagehand's `act`, `observe` and `extract`, Puppeteer's `page.accessibility.snapshot()` and its
  `aria/` selectors now work against korvus.
- `DOM.getDocument` and `DOM.describeNode` honour `depth` and `pierce` (shadow roots and frame documents), so a client
  can read the whole DOM tree in one call, and `DOM.querySelector`, `querySelectorAll`, `requestNode` and
  `getNodeForLocation` are answered.
- `Page.navigateToHistoryEntry` goes back or forward at once, as Chrome does: the next command finds the tab at the
  entry it was sent to. It used to wait a round trip first, so a client reading the URL straight after going back
  read the page it had left, and one going back and then straight forward (Stagehand's `goBack()`, `goForward()`)
  found nothing ahead.

## v152.0.4

- `korvus update` installs the latest release the way korvus was installed: the installer's way, beside the version
  you have with `~/.korvus/current` moved to it, or with pip. Run in a terminal, korvus checks for a newer release at
  most once a day and asks `Update now? [Y/n]` (Enter updates), as Oh My Zsh does; `[update] mode` in `config.toml`
  (`prompt`, `auto`, `reminder`, `off`) or `KORVUS_UPDATE` changes that, and outside a terminal it never checks. The
  binary's license is Version 1.2, which says so under "Data collection".
- `korvus serve` says why it cannot listen and what to do: a port another process holds, or, on Windows, one the OS
  reserves for Hyper-V or WSL (`os error 10013`), with the `netsh` command that lists those ranges; `--port` picks
  another.

## v152.0.3

- The Windows build (x86-64) is back in the release: `korvus-windows-x86_64.zip`, and
  `irm https://korvusbrowser.com/install.ps1 | iex` installs it. From this release on, every release ships Linux,
  macOS and Windows together, or none of them; 152.0.2 shipped Linux and macOS only.
- The installers say what they do, a line a step: the platform, the release, the download (its size, then a progress
  bar), the checksum, where korvus went and what changed on the PATH, in colour on a terminal and as plain text in a
  log. Until now `curl … | bash` printed nothing while it downloaded.
- Otherwise the engine is 152.0.2's, under the Korvus Binary License Version 1.1.

## v152.0.2

- No limits: korvus runs as many sessions at once as the machine holds, with no account, plan or license key. The
  session cap (one session without a license) and its waiting queue are gone, and `korvus serve` calls no server of
  korvus's: no activation, no heartbeat, no install id sent. `korvus login`, `logout`, `status` and `activate` are
  gone with them (they now only say so), and `korvus stats` shows the active sessions.
- korvus's site is korvusbrowser.com: `curl -fsSL https://korvusbrowser.com/install.sh | bash` and
  `irm https://korvusbrowser.com/install.ps1 | iex` install the latest release. The licensing contact is
  licensing@korvusbrowser.com (korvus.dev was never korvus's).
- The installers install a release whose archive holds korvus at its root, as 152.0.0's and 152.0.1's do (until now
  they stopped with "holds no korvus"), and take its version from the binary.
- The binary's license is the [Korvus Binary License](BINARY-LICENSE.md) Version 1.1, which draws the source's line:
  production use is free under USD 10 million of yearly revenue (self-attested), and any non-production use is free;
  at or above it, a commercial license. Version 1.0 made internal use free at any revenue; 152.0.0 and 152.0.1 stay
  under it.
- `korvus --version` prints the release's version (the banner of 152.0.0 and 152.0.1 both said 152.0.0).

## v152.0.1

- The `pip install korvus-browser` launcher now verifies the downloaded archive against the release's
  `SHA256SUMS` before extracting it.
- First release cut through the automated one-tag-push build/release pipeline; the engine binary is
  unchanged from v152.0.0.

## v152.0.0

The first release. korvus presents itself as Google Chrome 152 (152.0.7977.82) on Linux x86-64: its user agent and
client hints, its network fingerprint and the web platform a desktop Chrome 152 exposes.

- Unpack `korvus-linux-x86_64.tar.gz` and run `./korvus`. ANGLE's `libEGL.so` and `libGLESv2.so` and the
  `personas/` folder stay next to it.
- `THIRD-PARTY-NOTICES.md` in the archive lists the components korvus carries and their licenses.
- Check the download against `korvus-linux-x86_64.tar.gz.sha256` (`sha256sum -c`).
- The binary is under the [Korvus Binary License](https://github.com/KorvusBrowser/korvus-browser/blob/main/BINARY-LICENSE.md).
