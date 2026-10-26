# korvus client

A browser front end you can run anywhere: tabs, the address bar, back / forward / reload, popups (a Google sign-in)
as windows over the page, the page's `alert` / `confirm` / `prompt`, and typing in any language, Korean and other IME
input included. It shows a browser that runs elsewhere, as a live picture, and sends it what you click and type.

It speaks only the standard DevTools protocol, so it drives korvus, and any Chrome started with
`--remote-allow-origins`.

## Use korvus's own

`korvus serve` serves this client from its port and prints its address when it starts:

```
korvus serve
korvus client: http://127.0.0.1:9222/client/#ticket=…
```

Open that address in your browser. The ticket after `#` is the key: anyone with the address can use the browser, so
keep it to yourself. Add `&persona=<name>` to it to open a named persona's session.

## Put it in your own page

```html
<script type="module" src="https://your.host/korvus-client.js"></script>
<korvus-browser endpoint="wss://korvus.your.host/?ticket=…"></korvus-browser>
```

`<korvus-browser>` is a web component, so it works in plain HTML and in React, Vue, Svelte or Angular alike. Its size
is the size of the browser window it shows.

Never put a long-lived key in a page. Your server holds korvus's automation channel and mints a ticket for each
visitor:

```js
// on your server, over korvus's automation channel
const { ticket, webSocketUrl } = await cdp.send("Korvus.createViewer", {
  browserContextId,            // the visitor's session; omit for the connection's own
  interactive: true,           // false: the visitor watches and cannot touch
  ttl: 900,                    // seconds; the ticket and its connections end then
  origins: ["https://your.site"], // pages it may be opened from; omit for any
});
// … hand webSocketUrl to the visitor's page; later, if you need to:
await cdp.send("Korvus.revokeViewer", { ticket });
```

A ticket lets its page do what a person at a browser does in that one session: see, click, type, open and close tabs,
move through the address bar and history, answer dialogs, size the window. It cannot run script, read cookies,
storage or the network log, open `file:` or `javascript:` addresses, or reach any other session.

The page that opens the endpoint must be allowed to reach it. From `https` pages that means `wss://` (put korvus behind
a TLS proxy), and `korvus serve --allow-host <name>` for the name the proxy forwards. Recent Chrome versions ask a
person before a public site may reach `localhost` (Local Network Access).

### Attributes and events

| | |
|---|---|
| `endpoint` | the DevTools WebSocket URL, with korvus's `?ticket=` |
| `home` | the page a new tab opens (`about:blank`) |
| `search` | where text that is not an address goes, `%s` for the words (Google's) |
| `persona` | the name shown beside the session's state |
| `korvus-popup` event | a page opened a popup; `preventDefault()` keeps it from opening as a window (`event.detail.tab`) |
| `korvus-state` event | the connection is `connecting`, `live`, `view-only` or `off` (`event.detail.state`) |

Colours are custom properties (`--korvus-accent`, `--korvus-ink`, `--korvus-strip`, …) you can set on the element.

`<korvus-view endpoint="…" target="…">` is one page's screen and input alone. `Client`, exported beside the
elements, is the headless core underneath both, for building a different interface.

## Build

```
npm install
npm run build      # type-checks, then writes build/
```

`build/` is committed: korvus compiles it into its own binary, so building korvus needs no Node. Rebuild and commit it
with every change to `src/` or `static/`.

## Licence

Apache-2.0 (`LICENSE`). Copyright 2026 KorvusBrowser.
