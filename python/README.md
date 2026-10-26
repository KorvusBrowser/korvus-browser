# korvus-browser (Python launcher)

`pip install korvus-browser` gives you a `korvus` command that downloads the matching korvus engine
binary from this project's GitHub Release (cached under `~/.cache/korvus-browser/`) and runs it.

```
pip install korvus-browser
korvus serve
```

The engine itself is under the Business Source License 1.1 (see the repository `LICENSE`); this launcher
is a thin convenience wrapper. **MVP/template** — finalize alongside `.github/workflows/release.yml`
(asset names, checksum verification, Windows path) before publishing to PyPI.
