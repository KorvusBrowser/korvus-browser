<!-- prettier-ignore -->
<div align="center">

<img src=".github/assets/korvus.svg" alt="" height="88" />

# korvus

*The browser, rebuilt for agents.*

[![Latest release](https://img.shields.io/github/v/release/KorvusBrowser/korvus-browser?style=flat-square&label=release)](https://github.com/KorvusBrowser/korvus-browser/releases/latest)
[![PyPI](https://img.shields.io/pypi/v/korvus-browser?style=flat-square&label=pypi)](https://pypi.org/project/korvus-browser/)
![Linux · macOS · Windows](https://img.shields.io/badge/Linux%20·%20macOS%20·%20Windows-2e3956?style=flat-square)
![Written in Rust](https://img.shields.io/badge/Rust-from%20scratch-b7410e?style=flat-square&logo=rust&logoColor=white)
[![Docs](https://img.shields.io/badge/docs-korvusbrowser.com-7e6a80?style=flat-square)](https://korvusbrowser.com/docs)
[![Source: BSL 1.1](https://img.shields.io/badge/source-BSL%201.1-616779?style=flat-square)](LICENSE)

[Benchmarks](#benchmarks) • [Install](#install) • [Quick start](#quick-start) • [Features](#features) • [Docs](https://korvusbrowser.com/docs)

</div>

korvus is a real browser engine, written from scratch in Rust for AI agents and automation: its Web APIs are native
Rust bindings in the engine, not imitations in JavaScript, and it runs at a fraction of Chrome's memory. It speaks the
Chrome DevTools Protocol, so Playwright, Puppeteer and any CDP client drive it as they drive Chrome.

> [!TIP]
> korvus is free, with no account, no license key and no limit on sessions: in production for a company under
> USD 10 million of yearly revenue, and for any non-production use at any size. See [Free, with no limits](#free-with-no-limits).

## Benchmarks

The same pages on the same machine, in korvus and in headless Chrome, and 100 live sites beside Playwright's headless
Chrome:

<!-- bench: korvusbrowser.com's benchmark figures -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset=".github/assets/bench-dark.svg">
  <img alt="Bar charts of korvus against headless Chrome on memory, speed and pages a minute, and against Playwright on 100 live sites" src=".github/assets/bench-light.svg" width="100%">
</picture>

- **Memory**: the peak memory of the whole browser, every process counted: idle, on one page, and with ten tabs open. On one page, korvus peaks at 0.38× headless Chrome's memory, over the 50 pages of the benchmark.
- **Speed**: from launch to a DevTools connection (cold start), and to a first page loaded, cold and warm.
- **Scale**: the pages one browser loads a minute, with 4 or 16 tabs at work at once.
- **Real sites**: of 100 live sites, half of them behind bot protection, the share each browser loads whole.

<details>
<summary>The figures, as tables</summary>

| Against headless Chrome | | korvus | Chrome | korvus's lead |
| :-- | :-- | --: | --: | :-- |
| **Memory** (MiB) | Idle | 113 | 1,025 | **9.1× less** |
|  | One page | 307 | 844 | **2.7× less** |
|  | Ten tabs | 1,618 | 3,676 | **2.3× less** |
| **Speed** (ms) | Cold start | 68 | 756 | **11× faster** |
|  | First page, cold | 146 | 1,003 | **6.9× faster** |
|  | First page, warm | 56 | 309 | **5.5× faster** |
| **Scale** (pages a minute) | 4 tabs at once | 106 | 68 | **1.6× more** |
|  | 16 tabs at once | 126 | 63 | **2.0× more** |

| On 100 live sites | | korvus | Playwright | korvus's lead |
| :-- | :-- | --: | --: | :-- |
| **Real sites** (% of sites loaded) | Bot-protected | 86% | 48% | **+38 points** |
|  | All 100 | 92% | 70% | **+22 points** |

</details>

<sub>korvus 152.0.0 (its stealth persona, the default), headless Chrome 152.0.7977.82 and Playwright 1.64.1's headless shell (Chromium 156), on one machine (AMD Ryzen 5 9600X, 12 threads, 60 GiB, Linux 7.0), 2026-10-24. Medians of repeated runs.</sub>
<!-- /bench -->

## Install

Linux (x86-64, glibc) and macOS (Apple silicon):

```sh
curl -fsSL https://korvusbrowser.com/install.sh | bash
```

Windows (x86-64), in PowerShell:

```powershell
irm https://korvusbrowser.com/install.ps1 | iex
```

Or with pip, on any of the three:

```sh
pip install korvus-browser
```

The installer checks the download against the release's `SHA256SUMS`, unpacks it into `~/.korvus/dist/<version>`,
points `~/.korvus/current` at it, adds that to your `PATH` and runs `korvus setup`. korvusbrowser.com hands out the
installers of the latest release; the same files are in every [release](https://github.com/KorvusBrowser/korvus-browser/releases),
and you can also unpack an archive yourself and run `korvus` in it (ANGLE's two libraries, `plugins/` and `personas/`
stay beside the binary).

> [!NOTE]
> On a Linux server without fontconfig, install it first: `sudo apt-get install libfontconfig1`. Linux on arm64 has
> no build yet.

The installer runs `korvus setup` (with pip, run it once yourself), which asks two questions and writes the answers
to `~/.korvus/config.toml`:

- **Stealth by default?** Yes: pages see the persona the `korvus-stealth` plugin gives them, a desktop device and its
  browser. No: pages see korvus as itself (`Korvus/<version>` in the user agent, `navigator.webdriver` true).
- **Rotate the persona for new sessions?** Yes: every new browser context gets a new persona. No: one persona until
  you change it.

Without a terminal (CI, a container) it chooses stealth with rotation and asks nothing.

## Quick start

Start the endpoint:

```sh
korvus serve                 # a DevTools endpoint on 127.0.0.1:9222
```

Then connect as you connect to Chrome. Each browser context is one session, with its own persona:

```js
// Playwright
import { chromium } from "playwright";

const browser = await chromium.connectOverCDP("http://127.0.0.1:9222");
const page = await (await browser.newContext()).newPage();
await page.goto("https://example.com");
console.log(await page.title());
```

```python
# Playwright for Python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.connect_over_cdp("http://127.0.0.1:9222")
    page = browser.new_context().new_page()
    page.goto("https://example.com")
    print(page.title())
```

```js
// Puppeteer
import puppeteer from "puppeteer-core";

const browser = await puppeteer.connect({ browserURL: "http://127.0.0.1:9222" });
```

`korvus serve` also prints the address of its own client, a browser front end with tabs, popups as windows and
dialogs, to open in your browser: `korvus client: http://127.0.0.1:9222/client/#ticket=…`. The ticket is the key to
the browser, so keep the address to yourself. The client is source-available ([client](client/README.md)) and can be
put in your own page.

## Features

- **Your code, unchanged**: contexts, frames (same-origin and cross-origin), popups, workers, cookies and
  `storageState()`, `exposeFunction`, `setContent`, screenshots, `page.pdf()`, downloads, uploads, drag and drop,
  WebSockets, touch emulation and `httpCredentials` work as against Chrome, and so do Playwright's `newContext()`
  options (`userAgent`, `locale`, `timezoneId`, `viewport`, `geolocation`, `permissions`, `extraHTTPHeaders`, …).
- **A persona per session**: under stealth, the default, every context presents a desktop device and its browser.
  Rotate it per session, fix it with a seed, or keep a named persona whose cookies and storage last across runs.
- **Proxies that match**: an HTTP or SOCKS5 proxy per run or per persona, with the time zone and locale following its
  exit IP.
- **Built for servers**: no display needed; one endpoint runs as many sessions as the machine holds.
- **Scripts in a page**: `korvus run` runs a script in a page's origin, offline or with your own responder, to mint a
  token under the client that ships it.
- **Plugins, not forks**: change requests, Web API answers, identity and input with a shared library korvus loads at
  run time, against one C header.
- **Updates built in**: `korvus update`, and in a terminal korvus asks `Update now? [Y/n]` when a release is out.

## Sessions and personas

Named personas keep their profile (cookies, storage) across runs:

```sh
korvus persona new shopper        # then connect to http://127.0.0.1:9222/persona/shopper
korvus persona set shopper proxy=socks5://user:pass@host:1080 timezone=Europe/Berlin
korvus persona ls
```

`korvus persona reset <name>` drops a profile, `korvus persona export <name> [file]` writes it as JSON (its cookies
included, HttpOnly ones too) and `korvus persona import <name> <file>` puts one back; `"persistent": false` in a
persona's `session` keeps none. `korvus stats` shows the active sessions.

A context starts as its persona, and what a client asks for over DevTools is honoured as Chrome honours it. An
overridden user agent changes the strings, not the engine under them, as in Chrome; pass `userAgentMetadata`
(`Emulation.setUserAgentOverride`) for the client hints to match it.

<details>
<summary><b>Every flag of <code>korvus serve</code></b></summary>

Every setting has a flag that overrides its config value for this run, and a context's own parameters override both
for that context:

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
| `--max-heap MB` | cap each page's JavaScript heap at MB, rather than size it from the machine's memory as Chrome does (4 GB from 8 GB up): a page that needs more dies, as a Chrome tab past its limit does |

</details>

## Run a script in a page

```sh
korvus run sign.js --url https://www.example.com/ --html page.html \
  --map https://www.example.com/sdk/=./sdk --offline --cookies
```

`sign.js` runs in a document at `--url` (its origin, DOM, cookies and storage) after the page's own scripts, and its
value is printed (a promise is awaited). Without `--url`, `korvus run` runs the file in a bare realm with no document.

| option | what |
|---|---|
| `--html FILE` | the document's markup (otherwise it is fetched) |
| `--map PREFIX=DIR` | answer requests from disk |
| `--offline` | fail every other request |
| `--cookies` | print the page's cookies |
| `--wait SECS` | let the page run first (3 by default) |
| `--on-request CMD` | a responder for what nothing else answered (below) |
| `--as NAME` | the script's own source name (default `<anonymous>`, as `eval()` names it) |

For a challenge the SDK fetches dynamically, `--on-request <cmd>` is handed each unanswered request as one JSON line
on stdin (`url`, `method`, `headers`, `body_base64`, `destination`), and its one JSON line on stdout is the answer:
`{"fulfill":{"status":200,"headers":{…},"body":"…"}}` (a body is text, or `{"base64":"…"}` for bytes),
`{"continue":{}}`, `{"fail":"net::ERR_…"}`, or `null` to fall through to `--offline` or the network. So an SDK's
challenge is answered with no `serve` and CDP relay to build.

> [!TIP]
> The script runs as the browser itself, as Chrome 152, the milestone korvus presents, so mint a token under the
> client that ships it. For a clean mint each time, give each its own session: over CDP, a `newContext()` per mint;
> with `korvus run`, each invocation is already its own.

## Updates

`korvus update` installs the latest release the way you installed korvus: beside the version you have, with
`~/.korvus/current` moved to it (a `korvus serve` already running stays on its version until it restarts), or with pip
for `pip install korvus-browser`. Run in a terminal, korvus checks for a newer release at most once a day and asks
`Update now? [Y/n]` (Enter updates), as Oh My Zsh does:

```toml
# ~/.korvus/config.toml
[update]
# prompt (asks; the default), auto (updates without asking), reminder (only says so), or off
mode = "prompt"
frequency = 1       # days between checks
```

`KORVUS_UPDATE=prompt|auto|reminder|off` sets the mode for one run. Outside a terminal (a script, CI, a container)
korvus never checks or asks.

## Plugins

korvus is built from plugins behind a stable C interface, and you can load your own: a page's Web APIs, its network
requests, its identity, what it reads back from a canvas, and more. A plugin is a shared library built against
[`plugin-sdk/korvus_plugin.h`](plugin-sdk/korvus_plugin.h) alone; see the [plugin SDK](plugin-sdk/README.md) and its
[examples](plugin-sdk/examples). `korvus plugin ls` lists the loaded plugins and the slots they fill.

> [!WARNING]
> A plugin you load that korvus did not ship runs at your own risk: it runs inside korvus with korvus's access to your
> machine and your pages.

## Free, with no limits

korvus has no plans, no accounts and no license key. A release runs as many sessions at once as your machine holds,
with every feature, the stealth persona included. It calls no server of ours: its traffic is the pages you load, your
own settings (a proxy, a plugin), a browser's own work such as certificate checks, and the update check above.

- The **release binary** is under the [Korvus Binary License](BINARY-LICENSE.md): free in production while your yearly
  revenue is under USD 10 million (self-attested), and free at any revenue for development, testing, research and
  education.
- The **engine source** is under the [Business Source License 1.1](LICENSE), with the same USD 10 million line, and
  not as a hosted service for others; each version becomes Apache-2.0 three years after its release. The **plugin
  interface** (the header, the SDKs and the examples) is [MIT](LICENSE.plugins).
- For production at or above that revenue, a hosted service, or redistributing korvus, open an
  [issue](https://github.com/KorvusBrowser/korvus-browser/issues) that starts with "Licensing:".

## Resources

- [Documentation](https://korvusbrowser.com/docs): install, sessions, clients, plugins and configuration
- [Changelog](CHANGELOG.md) and [releases](https://github.com/KorvusBrowser/korvus-browser/releases)
- [Plugin SDK](plugin-sdk/README.md) and the [korvus client](client/README.md)
- [korvus-browser on PyPI](https://pypi.org/project/korvus-browser/)
