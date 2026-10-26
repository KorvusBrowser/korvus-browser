# Korvus Binary License (DRAFT)

> **Draft outline, not in force.** These are the intended terms, in a standard binary-license structure.
> The licensor's legal name, governing law and venue, liability cap and contact address are **TBD**, and the final
> legal wording of every row below is for counsel. Have counsel write the final text before the first paid release.
> Until then, [Version 1.0 (October 2026)](BINARY-LICENSE-1.0.md) is the license in force.

This license covers the compiled korvus browser ("the Binary"), including the `korvus-stealth` plugin and the
personas a release ships with, as obtained from official channels. It does not cover the files in this repository,
which are MIT-licensed ([`LICENSE`](LICENSE)): the plugin header `plugins/korvus_plugin.h`, its documentation and the
sample plugins are among them. The Binary includes open-source components (V8, Skia, ANGLE, BoringSSL and a number
of Rust libraries); each stays under its own license, listed in the third-party notices every release ships with.
Everything else in the Binary -- the engine, its build and the releases -- belongs to **TBD (licensor)**.

| section | intended position |
|---|---|
| Grant of use | Personal, commercial and internal business use. Non-exclusive and non-transferable. |
| Versions and paid tiers | Free without an account (1 session per machine), a free account (3 sessions; 10 for the first 30 days), and paid plans by concurrent sessions, listed on korvus.dev (**TBD**: the pricing page). Every plan has every feature. A korvus subscription is its own: it is honoured only by korvus, under korvus's key. A license key is for the person or organization it was issued to: you may not share, sell or publish it. |
| Restrictions | You may not reverse engineer, decompile or disassemble the Binary, except where the law allows it despite this restriction; and you may not remove, disable or circumvent its license checks, session limits or signature verification, or publish or distribute anything made to do so. You may not redistribute, resell, sublicense or repackage the Binary. Normal use is allowed: flags, configuration, profiles, personas, driving it through the DevTools protocol or an automation library, and plugins. |
| Plugins | korvus loads plugins through its public C interface (`plugins/korvus_plugin.h`), including plugins that replace the one it ships with. **A plugin you load that KorvusBrowser did not ship runs at your own risk**: it runs inside korvus with korvus's own access to your machine, your pages and your data. KorvusBrowser does not review, support or answer for it, and nothing in this license's warranty or support covers it or what it does. Writing and loading your own plugins is allowed; a plugin is yours, under whatever license you choose. Loading a plugin does not change the session limits: a plugin made to get around them is circumvention (see Restrictions). |
| Enforcement | Copies or tools published in breach of these terms are taken down, including by notices under the U.S. Digital Millennium Copyright Act (DMCA) and its equivalents. |
| Third-party use (SaaS / OEM) | Internal use, including your own automation, needs nothing more. Letting third parties control the browser, or offering it as a service, needs an Enterprise/OEM license (contact below). |
| Official distribution | GitHub Releases of github.com/KorvusBrowser/korvus-browser/releases and the install scripts in that repository (`install.sh`, `install.ps1`). Internal mirrors are allowed. |
| Acceptable use | Lawful use only. You are responsible for how you use the Binary. In particular, no unauthorized access to banking, financial, healthcare or government login systems; no credential stuffing, password guessing or automated account creation; no getting around authentication on systems you do not own and are not authorized to test; no fraud, identity theft, or collecting data you have no right to collect. |
| Trademarks | No rights to the KorvusBrowser or korvus names or logos, beyond referring to them (for example, saying your project works with korvus). |
| Data collection | `korvus serve`, and only it, sends our server (https://api.korvus.dev unless you configure another): on its first start with no key, this machine's install id and the korvus version (the server also sees your IP address, which limits new free activations); on `korvus activate`, or a start with a key, the license key, install id and version; on `korvus login`, a GitHub token (checked, then discarded) and the install id; while it serves, a heartbeat once a minute with the key, install id, active session count and version. Pages and their processes send nothing to us, and nothing about pages, URLs, proxies or personas is ever sent. Other traffic comes from the pages you load, your configuration and a browser's normal workings. |
| Warranty and liability | Provided as-is. Liability cap **TBD**. |
| Termination | Automatic on breach. |
| Licensor, governing law, contact | **TBD**. Until then, licensing questions (including a separate license for redistribution, bundling or hosted use) go to https://github.com/KorvusBrowser/korvus-browser/issues with a title that starts with "Licensing:". |
