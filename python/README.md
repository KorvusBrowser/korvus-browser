# korvus-browser

`pip install korvus-browser` gives you the `korvus` command: a browser engine for AI agents and automation, written
from scratch in Rust, that Playwright, Puppeteer and any Chrome DevTools Protocol client drive as they drive Chrome.

```
pip install korvus-browser
korvus serve          # a DevTools endpoint on 127.0.0.1:9222
```

```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.connect_over_cdp("http://127.0.0.1:9222")
    page = browser.new_context().new_page()
    page.goto("https://example.com")
    print(page.title())
```

This package is a launcher: the first `korvus` downloads the engine release of the same version from
[GitHub](https://github.com/KorvusBrowser/korvus-browser/releases), checks it against the release's `SHA256SUMS`,
keeps it under `~/.cache/korvus-browser/` and runs it. Linux (x86-64), macOS (Apple silicon) and Windows (x86-64).

## Updates

`korvus update` upgrades this package with pip (`pip install --upgrade korvus-browser`), and the next `korvus` runs
the new engine. Run in a terminal, korvus also checks for a newer release at most once a day and asks
`Update now? [Y/n]`; `KORVUS_UPDATE=off` (or `reminder`, or `auto`) changes that. It never checks or asks outside a
terminal, so scripts and CI are untouched.

## License

The engine's source is under the Business Source License 1.1, and the release binary under the Korvus Binary License:
free in production for a company under USD 10 million of yearly revenue, and free for any non-production use. See
<https://korvusbrowser.com/#license>. Docs: <https://korvusbrowser.com/docs>.
