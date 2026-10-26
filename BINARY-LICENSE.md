# Korvus Binary License

**Version 1.0 — October 2026**

Copyright (c) 2026 The Base (더베이스). All rights reserved.

This license covers the compiled korvus browser ("the Binary") as The Base (더베이스) distributes it through its
official channels, which today are the GitHub Releases of
[KorvusBrowser/korvus-browser](https://github.com/KorvusBrowser/korvus-browser/releases).

The source code and documentation in this repository are not covered by this license. The engine source is under the
[Business Source License 1.1](LICENSE); the plugin interface — the C ABI header (`plugin-sdk/korvus_plugin.h`), the
SDKs and the examples — is under the [MIT License](LICENSE.plugins).

You accept this license by downloading, installing or using the Binary.

## Ownership

korvus is a browser engine that The Base (더베이스) develops. The Binary includes open-source components, among them
V8, Skia, ANGLE, BoringSSL and a number of Rust libraries. Each of those components stays under its own license, and
every release lists them in the third-party notices it ships with. Nothing in this license limits the rights those
licenses give you for their components.

Everything else in the Binary is the property of The Base (더베이스): the engine, its build and the releases.

## What you may do

The Base (더베이스) grants you a license to use the Binary for personal, commercial and internal business purposes. The
license is non-exclusive and non-transferable, and you may not sublicense it. It is subject to the rest of this
document.

## Free and paid versions

Today every version of the Binary is free to use under this license.

The Base (더베이스) may later require a paid subscription for some versions, for example the newest major version, or for
some features or usage limits. If it does, it will announce the change on its official channels at least thirty (30)
days before the change takes effect. A version you obtained for free stays free to use under the terms that came with
it. Every term of this license applies to every version, paid or free, no matter how old the version is.

A paid subscription, if one is offered, may come with a license key. Only the person or organization it was issued to
may use the key. You may not share, sell or publish it, or use it to get around license checks or usage limits.

## What you may not do

You may not:

1. **Redistribute** the Binary, in whole or in part, modified or not.
2. **Resell, sublicense or repackage** the Binary, or include it in a product or service you provide to others,
   unless you have a separate license.
3. **Reverse engineer, decompile or disassemble** the Binary, or try to derive its source code, except where the law
   allows it.
4. **Modify** the Binary or make derivative works from it.
5. **Remove or change** the copyright notices, license files or third-party notices that come with the Binary.

The following are normal use, not modification: running the Binary with command-line flags, driving it through the
DevTools protocol or an automation library, and using your own profiles or data directories.

## Internal use, hosted services and bundling

**Internal use is free.** You do not need a separate license for internal use, whatever the size or revenue of your
organization. Internal use means using the Binary for your own browsing, scraping, data collection, automation,
testing, research or business workflows, as long as you run the browsing yourself.

- You may keep and run the unmodified Binary on your own infrastructure for internal purposes, including Docker
  images, CI runners and internal artifact stores.
- A customer may ask you for a result, such as a report, a dataset or an analysis. That is still internal use if you
  decide how the browsing is done and run it yourself.

**Listing korvus as a dependency is allowed.** You may name korvus as a dependency in package files, build scripts or
documentation without a separate license, provided your users get the Binary from the official channels.

**A separate license is needed** when you provide the Binary's browser capability to others. This includes:

- bundling or embedding the Binary in a product you distribute;
- exposing it through an API;
- offering it as a hosted service or browser-as-a-service;
- giving customers control of the browser itself.

"Control" means being able to run, configure or steer the Binary, its sessions or its settings. Asking you for data
or results is not control. Running the Binary on your own machines does not make a use internal if those machines
provide browser capacity to others.

## Official distribution

You must get the Binary from The Base (더베이스)'s official channels. A mirror inside your organization for internal
use, as described above, is allowed.

## Names and marks

This license gives you no right to use the KorvusBrowser or korvus names or logos. The exception is the use you need
to refer to them, such as saying that your project works with korvus.

Credit is welcome but not required. If you want to give it, you can link to
https://github.com/KorvusBrowser/korvus-browser from your documentation.

## Acceptable use

You are responsible for how you use the Binary, including any harm it causes to others. You must not use it for
anything that breaks the laws that apply to you. In particular, you must not use it for:

- getting into banking, financial, healthcare or government login systems without authorization;
- credential stuffing, password guessing or automated account creation;
- getting around authentication on systems you do not own and are not authorized to test;
- fraud, identity theft or collecting data you have no right to collect.

## Indemnification

You agree to defend The Base (더베이스), its contributors and its licensors against claims that arise from:

- your use of the Binary;
- your breach of this license;
- your breach of any law or of anyone else's rights.

You agree to cover the losses and costs that follow from such claims, including reasonable legal fees.

## No warranty

THE BINARY IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.

## Limitation of liability

IN NO EVENT SHALL THE BASE (더베이스), ITS CONTRIBUTORS OR ITS LICENSORS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY ARISING OUT OF OR RELATING TO THE BINARY OR ITS USE. THIS INCLUDES INDIRECT, INCIDENTAL, SPECIAL,
CONSEQUENTIAL OR PUNITIVE DAMAGES AND LOSS OF PROFITS, DATA OR GOODWILL, WHETHER IN CONTRACT, TORT OR OTHERWISE.

WHERE THE LAW DOES NOT ALLOW LIABILITY TO BE EXCLUDED, THE BASE (더베이스)'S TOTAL LIABILITY IS LIMITED TO THE LESSER
OF: ONE HUNDRED US DOLLARS (US $100), OR THE AMOUNT YOU PAID THE BASE (더베이스) FOR THE BINARY IN THE THREE (3)
MONTHS BEFORE THE CLAIM.

## Data collection

The Binary contains no telemetry, analytics or tracking. Its network traffic comes from:

- the pages and services you load;
- your own configuration;
- the normal workings of a browser, such as certificate checks.

If a paid version ever needs to check a license key over the network, this section will say so before that version
is released.

## Updates

The Base (더베이스) may release updates and new versions, and may offer support, as it chooses. Updates are covered by
this license unless The Base (더베이스) says otherwise.

## Termination

This license ends automatically if you break any of its terms. When it ends, you must stop using the Binary and delete
every copy you have.

These sections still apply after the license ends: Ownership, What you may not do, Names and marks, Acceptable use,
Indemnification, No warranty, Limitation of liability and Governing law.

## Governing law

This license is governed by the laws of the place where The Base (더베이스), or its successor, is established. The
courts of that place have exclusive jurisdiction over disputes about this license.

## General

- **Reservation of rights.** The Base (더베이스) keeps every right this license does not expressly grant.
- **Entire agreement.** This license is the whole agreement between you and The Base (더베이스) about the Binary.
- **No waiver.** If The Base (더베이스) does not enforce a term, it has not given up that term or any other.
- **Assignment.** You may not transfer this license without The Base (더베이스)'s written consent. The Base (더베이스)
  may transfer it to a successor, including a legal entity formed to carry on the project.
- **Severability.** If a court holds a term unenforceable, that term is changed only as far as needed to make it
  enforceable, and the rest of the license stays in force.
- **Changes.** The Base (더베이스) may update this license. Changes are announced on its official channels. If you keep
  using the Binary after a change takes effect, you accept the updated license.

## Contact

For licensing questions, including a separate license for redistribution, bundling or hosted use, open an issue at
https://github.com/KorvusBrowser/korvus-browser/issues with a title that starts with "Licensing:".
