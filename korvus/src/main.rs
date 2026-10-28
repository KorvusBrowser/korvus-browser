//! `korvus` — the binary: CLI, config, the Coherence Engine, and assembly of all crates.
//!
//! G1.1: `korvus run <file.js>` boots the engine, runs the script, drives the event loop, and
//! flushes `print(...)` output to stdout. With no args it prints an assembly banner (proving every
//! crate links and the plugin seams resolve through `spec`).

use spec::{Persona, RenderBackend};

mod cli;
mod help;
mod settings;
mod update;

/// korvus's version, as its banner states it.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The persona korvus wears, resolved by the crate that owns the question ([`persona::from_env`]): the persona
/// plugged in (`--persona <folder>`, `$KORVUS_PERSONA_DIR`, or `personas/` beside the executable), with
/// `$KORVUS_PERSONA=tell` selecting the deliberately-tellable control. None plugged in: korvus does not start.
///
/// This file USED to be that resolution — so every other binary in the workspace had to answer the
/// same question for itself, and they disagreed: a fetch adapter wore the embedded bundle no
/// matter what `$KORVUS_BUNDLE` said (W9.7).
fn build_persona() -> Box<dyn Persona> {
    match persona::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("korvus: {e}");
            std::process::exit(2);
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // This binary is also its own GPU service process, as Chrome's is (`--type=gpu-process`): WebGL's
    // questions cross a process boundary to the GPU as Chrome's do. Before anything else -- the service needs none of it.
    if std::env::args().nth(1).as_deref() == Some(render::gpu::SERVICE_ARG) {
        render::gpu::service_main();
        return Ok(());
    }
    if let Ok(exe) = std::env::current_exe() {
        render::gpu::use_service_process(exe);
    }
    // korvus's own GET for a plugin's `download` (ABI 5): what a plugin installs at `korvus setup`
    plugin_host::set_downloader(download_to);
    // `--persona <folder>` plugs in the persona this run wears (`persona::CapturedPersona::plugged_in`), anywhere on
    // the command line: as `KORVUS_PERSONA_DIR`, so the page and frame processes this binary starts wear it too.
    let mut argv: Vec<String> = std::env::args().collect();
    if let Some(i) = argv.iter().position(|a| a == "--persona") {
        let Some(dir) = argv.get(i + 1).cloned() else {
            eprintln!("korvus: --persona needs a persona folder");
            std::process::exit(2);
        };
        std::env::set_var("KORVUS_PERSONA_DIR", &dir);
        argv.drain(i..i + 2);
    }
    // `--analysis-out <dir>` / `--analysis-stack <n>`: turn the korvus-analysis call recorder on for this run by the
    // flag rather than only by `$KORVUS_ANALYSIS_OUT` (the env the recorder reads). Set anywhere on the line, before
    // anything starts an engine, so a page process this binary spawns inherits it. On a binary NOT built with the
    // `analysis` feature the recorder is not linked in (the hot call path carries no tracing branch -- a deliberate
    // zero-overhead choice, korvus/Cargo.toml), so the flag says so plainly rather than recording nothing in silence.
    if let Some(dir) = take_value(&mut argv, "--analysis-out", "a directory for the trace") {
        std::env::set_var("KORVUS_ANALYSIS_OUT", &dir);
        if let Some(n) = take_value(&mut argv, "--analysis-stack", "a stack depth") {
            std::env::set_var("KORVUS_ANALYSIS_STACK", &n);
        }
        if !cfg!(feature = "analysis") {
            eprintln!(
                "korvus: --analysis-out: this build does not carry the call recorder (it is the `analysis` feature, \
                 kept out of the release binary so every page call stays branch-free). Run the analysis build beside \
                 it -- the release ships it as `korvus-analysis` -- or build with `--features analysis`."
            );
        }
    }
    // `--block=<kinds,host:…>` / `--http-cache-max=<MB>`: spend no bandwidth or memory on what a scrape or a mint
    // does not need. Set as the env the transport reads (`KORVUS_BLOCK`, `KORVUS_HTTP_CACHE_MAX`), before any engine,
    // so a `korvus serve` endpoint's page processes inherit them and a one-shot `fetch`/`run` reads them itself.
    // `--block=image,font,media` drops those request kinds; `host:<substr>` drops by host; `--http-cache-max=0` turns
    // the HTTP cache off, any N caps it at N MB (never above Chrome's 50).
    if let Some(block) = take_value(&mut argv, "--block", "a list like image,font,media,host:ads.example") {
        std::env::set_var("KORVUS_BLOCK", &block);
    }
    if let Some(mb) = take_value(&mut argv, "--http-cache-max", "a size in MB (0 turns the cache off)") {
        if mb.trim().parse::<usize>().is_err() {
            exit_with("--http-cache-max: a whole number of megabytes (0 turns the cache off)");
        }
        std::env::set_var("KORVUS_HTTP_CACHE_MAX", mb.trim());
    }
    // `--context <spec>`: this process is a page of a browser endpoint's context (`cdp::router`), which fixed the
    // context's proxy, timezone and locale when it made it (goal 1020 invariant I1) and holds what its pages share.
    // Applied before anything below reads the environment it sets.
    let mut context_spec: Option<(cdp::router::ContextSpec, std::path::PathBuf)> = None;
    if let Some(path) = take_value(&mut argv, cdp::router::CONTEXT_FLAG, "a context spec") {
        match cdp::router::ContextSpec::load(std::path::Path::new(&path)) {
            Ok(spec) => {
                apply_context(&spec);
                context_spec = Some((spec, std::path::PathBuf::from(path)));
            }
            Err(e) => {
                eprintln!("korvus: {e}");
                std::process::exit(2);
            }
        }
    }
    // `--wear <persona document>`: wear this identity as answered, asking no plugin -- how this binary hands the
    // snapshot maker the identity it wears.
    let wear_arg = take_value(&mut argv, WEAR_FLAG, "a persona document");
    // The settings' flags, anywhere on the line (`settings::Flags`: `--identity`, `--rotate`, `--seed`, `--device`,
    // `--proxy`, `--timezone`, `--locale`, `--humanize`, `--noise`, `--plugin`), over the environment and config.toml.
    let flags = settings::Flags::take(&mut argv).unwrap_or_else(|e| exit_with(&e));
    // `--fresh` (`korvus serve`): every named persona starts with no cookies or storage and keeps none.
    let fresh = match argv.iter().position(|a| a == "--fresh") {
        Some(i) => {
            argv.remove(i);
            true
        }
        None => false,
    };
    let command = argv.get(1).cloned().unwrap_or_default();
    // `--help` (`-h`) after a command, or as one: what it takes, and nothing run -- `korvus serve --help` started a server.
    if matches!(command.as_str(), "--help" | "-h" | "help") || argv.iter().skip(2).any(|a| a == "--help" || a == "-h") {
        let about = if command.starts_with('-') || command == "help" { argv.get(2).cloned().unwrap_or_default() } else { command };
        print!("{}", help::usage(&about));
        std::process::exit(0);
    }
    let endpoint = command == "serve" || command == "browser";
    let making_snapshot = command == engine::snapshot::MAKE_ARG;
    // Run in a terminal, a person's command first checks for a newer release, at most once a day, and asks to update
    // as Oh My Zsh does (D183, `update::on_start`); an update it installs runs in this one's place at once. Not
    // `setup`, which the installer itself runs, nor the processes korvus starts for itself.
    if matches!(command.as_str(), "serve" | "browser" | "persona" | "plugin" | "stats" | "fetch" | "run" | "probe") && update::on_start() {
        update::run_current(&std::env::args_os().collect::<Vec<_>>());
    }
    // The commands that run no page.
    match command.as_str() {
        "--version" | "-V" | "version" => {
            println!("korvus {VERSION}");
            std::process::exit(0);
        }
        // config.toml's answers, then each plugin they load installs what it needs (`setup`, ABI 5)
        // `--plugins`: the plugins' part alone, config.toml kept as it is (the installer's, over an existing install)
        "setup" => {
            let plugins_only = argv.iter().any(|a| a == "--plugins");
            let code = if plugins_only { 0 } else { cli::setup(&argv[2..]) };
            if code != 0 || argv.iter().any(|a| a == "--no-plugins") {
                std::process::exit(code);
            }
            let s = resolve(&flags);
            let mut list = s.plugins.clone();
            if s.identity == "stealth" {
                // a build without the stealth library (from the public source) has nothing of it to set up
                match stealth_library() {
                    Ok(lib) => list.insert(0, format!("{lib}:{}", s.stealth_config())),
                    Err(e) => eprintln!("korvus: {e}"),
                }
            }
            load_plugins(None, list);
            let ready = settings::home().is_some_and(|home| plugin_host::setup(&home));
            std::process::exit(if ready { 0 } else { 1 });
        }
        "update" => std::process::exit(update::update_command(&argv[2..])),
        "stats" => std::process::exit(cli::stats(&argv[2..])),
        "persona" => std::process::exit(cli::persona(&argv[2..])),
        // The account commands of releases before 152.0.2: korvus has no accounts or plans now, and no session limit.
        "login" | "logout" | "status" | "activate" => {
            eprintln!("korvus: there is no account to sign in to: every session is free, with no limit");
            std::process::exit(0);
        }
        "plugin" => match argv.get(2).map(String::as_str) {
            Some("ls") => {
                let s = resolve(&flags);
                load_plugins(None, run_plugins(&s));
                std::process::exit(cli::plugin_ls(&s));
            }
            // `plugin check <path>` loads just that one library and reports what it declares -- no identity, no run.
            Some("check") => std::process::exit(cli::plugin_check(&argv[3..])),
            _ => exit_with("usage: korvus plugin ls [--identity ...] [--plugin ...] | korvus plugin check <path>"),
        },
        _ => {}
    }
    // The plugins first, then the identity they answer, then everything that reads it. The snapshot's maker loads
    // none: it wears what its parent wears (`--wear`).
    if making_snapshot {
        if let Some(path) = &wear_arg {
            wear(&read_or_exit(path), None);
        }
    } else {
        let (doc, wear_file) = match &context_spec {
            // a page of a context: the plugins and the identity its endpoint fixed when it made the context
            Some((spec, path)) => {
                load_plugins(Some(spec), Vec::new());
                cdp::set_humanize(spec.humanize.unwrap_or(false));
                (context_identity(spec), path.with_extension("identity.json"))
            }
            None => {
                let s = resolve(&flags);
                apply_host(&s, endpoint);
                if let Some(path) = s.device.as_deref().filter(|d| d.ends_with(".json")) {
                    check_document(path, &read_or_exit(path));
                }
                load_plugins(None, run_plugins(&s));
                cdp::set_humanize(s.humanize);
                let own = cdp::router::ContextSpec {
                    context: "default".into(),
                    identity: Some(s.identity.clone()),
                    seed: s.seed.clone(),
                    device: s.device.clone(),
                    humanize: Some(s.humanize),
                    ..Default::default()
                };
                let own = if endpoint {
                    // the endpoint's settings are every context's unless it states its own (`cdp::router::Defaults`)
                    cdp::router::set_defaults(cdp::router::Defaults {
                        seed: s.seed.clone(),
                        device: s.device.clone(),
                        proxy: s.proxy.clone(),
                        timezone: s.timezone.clone(),
                        locale: s.locale.clone(),
                        humanize: Some(s.humanize),
                        personas: settings::home().map(|h| h.join("personas")),
                        fresh,
                    });
                    own
                } else {
                    // a one-shot run is its own one context: its proxy, zone and locale, as a page process takes its
                    // context's
                    let proxy = s.proxy.clone().map(|server| cdp::router::ProxySpec { server, bypass: None });
                    let own = cdp::router::ContextSpec { proxy, timezone: s.timezone.clone(), locale: s.locale.clone(), ..own };
                    if let Some(why) = cdp::router::refused(&own) {
                        exit_with(&why);
                    }
                    apply_context(&own);
                    own
                };
                let doc = match &wear_arg {
                    Some(path) => Some(read_or_exit(path)),
                    None => context_identity(&cdp::router::ContextSpec { drawn: cdp::router::drawn(&own), ..own }),
                };
                (doc, std::env::temp_dir())
            }
        };
        if let Some(doc) = doc {
            wear(&doc, Some(wear_file));
        }
    }
    // Coherence (DoD-5): the process timezone drives Intl.DateTimeFormat().resolvedOptions().timeZone.
    // Set it from the persona before V8/ICU reads it, so the JS timezone matches the worn identity
    // instead of the host machine's zone (a UA/locale <-> timezone mismatch is a bot tell). Respect a
    // pre-set TZ (real deployments should match TZ to the exit IP). Must run before any Intl use.
    // Not in a browser endpoint (`korvus serve`), which runs no page: its page processes inherit its environment, and
    // a zone it set would read there as a deployment's, over each context's own.
    if std::env::var_os("TZ").is_none() && !endpoint {
        let tz = build_persona().client_hints().timezone;
        if !tz.is_empty() {
            std::env::set_var("TZ", &tz);
        }
    }
    // Build interface templates from the blueprint file an earlier run of THIS binary wrote,
    // instead of recording them again (`KORVUS_TPL_CACHE=off` keeps it off).
    webapi::enable_template_disk_cache();
    // One path: this binary's page realm comes from its snapshot whenever the template cache is on, and
    // the binary also MAKES that snapshot, in a child of its own that a miss starts (`engine::snapshot`) -- a V8 FATAL
    // there ends the child, not the page.
    if making_snapshot {
        std::process::exit(engine::snapshot::make_main(std::sync::Arc::from(build_persona())));
    }
    // A realm whose members a plugin claims is built from templates, never restored from the snapshot (whose functions
    // were made before any plugin was loaded).
    if !plugin_host::claims_any() {
        if let Ok(exe) = std::env::current_exe() {
            engine::snapshot::use_snapshots(exe);
        }
    }
    let args: Vec<String> = argv.into_iter().skip(1).collect();
    // Every loaded plugin's `shutdown`, when the run ends (not on a signal or `process::exit`).
    let _plugins = PluginsDown;
    // The page-driving subcommands record what the run learned -- which members were called, which of
    // those were shells, and the exact LAYER of every member the one installer mounted -- to
    // $KORVUS_COVERAGE_OUT / $KORVUS_SHELLS_OUT / $KORVUS_LAYERS_OUT. Off unless those are set.
    //
    // This is the wire that makes a real-site corpus mean something. The recorder has existed for
    // weeks with exactly one caller, the WPT harness, so the only pages that ever reported their
    // member usage were conformance fixtures -- a corpus that exercises the platform evenly, which
    // the open web emphatically does not. Ranking implementation work by what WPT touches is how you
    // end up with a precisely-graded surface that still cannot load a shop.
    let record = |r| {
        webapi::behavior_rt::dump_run_artifacts();
        // korvus-analysis: drain the call recorder and close its trace (a no-op without a recorder).
        #[cfg(feature = "analysis")]
        analysis::shutdown();
        r
    };
    // For the process floor: a long-lived `serve` starts its GPU service with the run,
    // as Chrome starts its GPU process with the browser; `browser` starts ONE shared service for all its page processes
    // (`render::gpu::start_shared_service`) and a page process of it connects to that one; a one-shot command
    // (`fetch`, `run`, `probe`) starts the service when its page first makes a canvas or asks for a context
    // (`RenderBackend::prepare_gl`), so a page that never draws pays nothing
    if render::wanted() && args.first().map(String::as_str) == Some(SERVE_PAGE) && !args.iter().any(|a| a == "--cdp-child") {
        render::gpu::warm();
    }
    match args.first().map(String::as_str) {
        Some("run") => {
            let path = args.get(1).ok_or_else(|| anyhow::anyhow!("usage: korvus run <file.js> [--url <page url> [--html <file>] [--map <url prefix>=<dir>]... [--on-request <cmd>] [--offline] [--wait SECS] [--cookies] [--as <name>]]"))?;
            if args.iter().any(|a| a == "--url") {
                record(run_page(path, &args[2..]).await)
            } else {
                record(run_file(path).await)
            }
        }
        // The single-page server (internal since goal 1020 step 5: the tools here drive it), and a page target's own
        // process (`serve-page <port> --cdp-child <target> <context>`).
        Some(SERVE_PAGE) => {
            let port: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(9222);
            if args.get(2).map(String::as_str) == Some(cdp::router::CHILD_FLAG) {
                cdp::router::set_child_ids(args.get(3).cloned().unwrap_or_default(), args.get(4).cloned().unwrap_or_default());
                // the browser's one GPU service, when it started one (`browser_endpoint`); else this page starts its own,
                // lazily, at its first canvas
                if let Some(i) = args.iter().position(|a| a == GPU_SOCKET_FLAG) {
                    if let Some(path) = args.get(i + 1) {
                        render::gpu::connect_service(std::path::PathBuf::from(path));
                    }
                }
                // a popup a page's script opened (D74 step 3): its opener's origin, `-` when it was opened `noopener`
                if let Some(i) = args.iter().position(|a| a == cdp::router::POPUP_FLAG) {
                    let opener = args.get(i + 1).filter(|o| o.as_str() != "-").cloned();
                    webapi::set_opened_by_script(opener);
                }
                return serve_target_child(port).await;
            }
            serve(port).await
        }
        // The browser endpoint Puppeteer and Playwright connect to (`cdp::router`): `korvus serve [--port 9222]
        // [--listen 127.0.0.1]`. `korvus browser [<port>]`, its name before goal 1020 step 5, stays.
        Some("serve") => {
            if args.get(1).is_some_and(|a| a.parse::<u16>().is_ok()) {
                exit_with("`korvus serve` takes `--port <n>`; the single-page server is `korvus serve-page <port>`");
            }
            let port: u16 = flag_value(&args, "--port").map(|p| p.parse().unwrap_or_else(|_| exit_with("--port: a port number"))).unwrap_or(9222);
            let ip: std::net::IpAddr = flag_value(&args, "--listen").map(|a| a.parse().unwrap_or_else(|_| exit_with("--listen: an IP address"))).unwrap_or([127, 0, 0, 1].into());
            // D88 step 1: the web pages that may open the endpoint (`--allow-origin`, `*` for all), and the names it
            // is reached by besides IPs and `localhost` (`--allow-host`, a reverse proxy's)
            cdp::access::allow(cdp::access::Allowed { origins: flag_values(&args, "--allow-origin"), hosts: flag_values(&args, "--allow-host") });
            browser_endpoint(std::net::SocketAddr::new(ip, port)).await
        }
        Some("browser") => {
            let port: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(9222);
            browser_endpoint(std::net::SocketAddr::from(([127, 0, 0, 1], port))).await
        }
        // The run ends with its output: what the page left in flight (a request through a slow proxy, a worker's) is not
        // waited for, as this runtime's teardown would wait for it -- `--timeout` bounds the whole run, not its load alone.
        Some("fetch") => end_now(record(fetch_cmd(&args[1..]).await)),
        Some("probe") => {
            let url = args.get(1).ok_or_else(|| anyhow::anyhow!("usage: korvus probe <url>"))?;
            record(probe_url(url).await)
        }
        // `panel` and `a production site` used to be here: a dev UI and a site-specific demo, both since deleted.
        _ => {
            banner();
            Ok(())
        }
    }
}

/// `korvus fetch <url> [--eval EXPR] [--dump MODE] [--wait SECS]` — load a page END TO END through the
/// S0 subresource pipeline (runs inline + external `<script src>`, so real framework fixtures render),
/// pump the event loop up to `--wait` seconds so async work (React commit, timers, observers) completes,
/// then print the requested output. This matches the CLI contract a reference engine's obstacle-course benchmark
/// drives (`<bin> fetch <url> --eval <expr>`), so the same fixtures can score Korvus head-to-head.
/// Flags Korvus doesn't need (`--allow-private-network`, `--quiet`) are accepted and ignored so the
/// harness invocation works unchanged.
///
/// WHEN IT STOPS. Without `--wait-until` / `--timeout` exactly as before: the page runs for
/// `--wait` seconds, or less if its event loop drains -- some 30 tools under `tools/` read that meaning.
/// * `--wait-until load|domcontentloaded|networkidle0|networkidle2` -- Puppeteer's `waitUntil` tokens,
///   which a reference engine's `--wait-until` takes too -- returns as soon as the document reaches that point, on
///   Chrome's definitions (`networkidle*` is Blink's `IdlenessDetector`, `Engine::network_idle`). A
///   number keeps the old meaning of `--wait-until`, an alias of `--wait`. Its cap is `--wait` when
///   given, else `--timeout` (30 s by default, as Puppeteer's `goto`).
/// * `--timeout SECS` (seconds, as a reference engine's) is headless Chrome's `--timeout`
///   (`components/headless/command_handler/headless_command.js`): when it falls due the load stops and
///   the page is printed as it stands; only a page that never reached `DOMContentLoaded` is a failure.
///   A script that never yields is cut by [`Deadline`]'s thread.
async fn fetch_cmd(args: &[String]) -> anyhow::Result<()> {
    let mut url: Option<String> = None;
    let mut eval: Option<String> = None;
    let mut dump: Option<String> = None;
    let mut screenshot: Option<String> = None;
    let mut errors = false;
    let mut requests = false;
    let mut console = false;
    // Scripts to run BEFORE the page's own, with `document` already there -- the CLI face of the
    // `injected` list `Engine::load_page_delivered` has always taken (CDP's
    // `Page.addScriptToEvaluateOnNewDocument` semantics). Repeatable; they run in the order given.
    //
    // WHY IT EXISTS: `--eval` runs after the page has settled, which cannot see anything a page did to
    // itself on the way. Two corpus failures are invisible without this -- rakuten.co.jp throws inside
    // jQuery at load time and reproduces nowhere else, and svelte.dev renders SvelteKit's own "500"
    // page with NO uncaught error, so whatever it caught is gone by the time `--eval` runs. Hooking
    // console.error / addEventListener("error") from here is how you see them.
    let mut early: Vec<String> = Vec::new();
    let mut wait_secs: u64 = 3;
    let mut wait_given = false;
    let mut wait_until: Option<WaitUntil> = None;
    let mut timeout_secs: Option<f64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--eval" => { eval = args.get(i + 1).cloned(); i += 1; }
            "--dump" => { dump = args.get(i + 1).cloned(); i += 1; }
            // Print the page's uncaught errors + unhandled rejections (from the engine's own stores) as
            // one JSON line BEFORE the eval/dump result, so a caller that reads the last line still gets
            // the result and one that wants the errors reads the line above it.
            "--errors" => errors = true,
            // Print the engine's REQUEST LEDGER -- every request the transport made for this page --
            // as one JSON line, on the same terms as `--errors`: before the eval/dump result, so a
            // caller that reads the last line still gets the result. This is the non-CDP face of
            // `Engine::requests()`; `tools/parity/driver.py` reads the same ledger over CDP's
            // `Network.*` events, and the two must agree (they are one store).
            "--requests" => requests = true,
            "--console" => console = true,
            "--eval-early" => { if let Some(v) = args.get(i + 1) { early.push(v.clone()); } i += 1; }
            "--wait" | "--wait-until" => {
                if let Some(v) = args.get(i + 1) {
                    match v.parse() {
                        Ok(n) => {
                            wait_secs = n;
                            wait_given = true;
                        }
                        Err(_) if args[i] == "--wait-until" => wait_until = Some(WaitUntil::parse(v)?),
                        Err(_) => {}
                    }
                }
                i += 1;
            }
            "--timeout" => { if let Some(v) = args.get(i + 1) { timeout_secs = v.parse().ok().filter(|s: &f64| *s > 0.0); } i += 1; }
            // `--screenshot <path>` writes the compositor's rendered frame as a PNG.
            "--screenshot" => { screenshot = args.get(i + 1).cloned(); i += 1; }
            "--selector" | "--output" | "--file" => { i += 1; } // accepted, ignored
            "--allow-private-network" | "--quiet" => {} // accepted, ignored (Korvus never blocks loopback)
            s if !s.starts_with("--") && url.is_none() => url = Some(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    let url = url.ok_or_else(|| anyhow::anyhow!("usage: korvus fetch <url> [--eval EXPR] [--eval-early EXPR] [--dump MODE] [--wait SECS] [--wait-until load|domcontentloaded|networkidle0|networkidle2] [--timeout SECS] [--errors] [--requests] [--console]"))?;

    let mut e = engine::Engine::new(build_persona());
    e.set_url(&url);
    // The deadline counts from here, the moment the browser exists, as headless Chrome's timer starts
    // before its `Page.navigate`.
    let timeout_at = timeout_secs.map(|s| std::time::Instant::now() + std::time::Duration::from_secs_f64(s));
    let deadline = timeout_at.map(|at| Deadline::start(at, e.isolate_interrupt()));
    // A NEW NAVIGATION IS A NEW LEDGER GENERATION, and it starts here -- before the document fetch,
    // because the document's own request is the ledger's first row. Resetting inside the engine's
    // load path instead would erase exactly that row.
    transport::ledger_reset();
    // The navigation to `url` begins now, once the engine exists -- as Chrome's begins with its browser and a
    // renderer already up: its `fetchStart` is a fraction of a millisecond after `navigationStart` (Chrome 150,
    // 0.4-0.5 ms on a typed navigation). It began before `Engine::new`, so the engine's construction (60-70 ms)
    // stood between this document's time origin and its `fetchStart` (korvus 71.5 ms, measured 2026-10-10).
    let navigation_start = std::time::Instant::now();
    let resp = within(timeout_at, transport::fetch(transport::FetchRequest {
        navigation_start: Some(navigation_start),
        ..transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())
    }))
    .await
    .ok_or_else(|| anyhow::anyhow!("--timeout: no response from {url} within {}s", timeout_secs.unwrap_or_default()))??;
    // The response's own Content-Type picks the parser (XML vs HTML), the way a browser does, and its
    // BYTES are decoded by the engine's encoding sniff: `resp.body` is a UTF-8-lossy view
    // that turned every legacy-encoded page into mojibake. The document is the RESPONSE's URL, after
    // redirects, not the one asked for (L19 E11).
    let doc_url = if resp.url.is_empty() { url.clone() } else { resp.url.clone() };
    // A load the deadline cut short is printed as it stands, not failed: a script that never yields, which the deadline's
    // thread terminates, and a load still waiting on the network -- a parser-blocking script on its way -- which no
    // termination reaches, so the wait itself ends at the deadline (`cut`). It ran on, and the run ended 5 s past the
    // deadline with nothing printed (`PRINT_GRACE`).
    let mut cut = false;
    match within(timeout_at, e.load_page_response(&resp.bytes, &doc_url, &early, &resp.headers)).await {
        Some(Err(err)) if !deadline.as_ref().is_some_and(Deadline::fired) => return Err(err),
        Some(_) => {}
        None => cut = true,
    }
    // Advance the event loop up to `wait_secs`. `pump` returns early if the page settles (no perpetual
    // timers); framework fixtures that finish their async render return fast, heavy ones use the budget.
    //
    // THE NAVIGATIONS THE PAGE ASKS FOR ARE FOLLOWED within that budget: a
    // `location.href = …`, or a `<meta http-equiv=refresh>` / `Refresh:` header whose delay falls inside
    // it. Each lands in a new realm (`Engine::successor`), after the old document's unload events, as
    // Chrome's navigation does; the ledger is not reset, so `--requests` lists every document's requests.
    let cap = match (wait_until, wait_given) {
        (Some(_), false) => std::time::Duration::from_secs_f64(timeout_secs.unwrap_or(30.0)),
        _ => std::time::Duration::from_secs(wait_secs),
    };
    let mut until = tokio::time::Instant::now() + cap;
    if let Some(at) = timeout_at {
        until = until.min(tokio::time::Instant::from_std(at));
    }
    let deadline_fired = || deadline.as_ref().is_some_and(Deadline::fired);
    loop {
        if cut || deadline_fired() {
            break;
        }
        if let Some(nav) = e.take_due_navigation() {
            e.fire_beforeunload();
            // A `method=post` form submission navigates with the body its `enctype` serialised;
            // every other navigation is the plain `GET`. The document that asked is the
            // request's initiator.
            let req = engine::navigation_request(&nav, e.persona().as_ref());
            let next = within(timeout_at, transport::fetch(req)).await;
            match next {
                Some(Ok(resp)) => {
                    e.fire_unload();
                    e = e.successor();
                    let doc_url = if resp.url.is_empty() { nav.url.clone() } else { resp.url.clone() };
                    e.arrive(engine::arrival_for(&nav, &doc_url));
                    if let Some(d) = &deadline {
                        d.follow(e.isolate_interrupt());
                    }
                    match within(timeout_at, e.load_page_response(&resp.bytes, &doc_url, &early, &resp.headers)).await {
                        Some(Err(err)) if !deadline_fired() => return Err(err),
                        Some(_) => {}
                        None => cut = true,
                    }
                    continue;
                }
                Some(Err(err)) => {
                    eprintln!("korvus: the page's navigation to {} failed: {err}", nav.url);
                    break;
                }
                None => break, // `--timeout` fell due while the next document was on its way
            }
        }
        if let Some(point) = wait_until {
            if !e.navigation_pending() && point.reached(&mut e) {
                break;
            }
        }
        let now = tokio::time::Instant::now();
        if now >= until {
            break;
        }
        // Run the page in short slices, so a refresh scheduled by its `load` (which happens inside a
        // slice) is noticed when it falls due rather than when the whole budget is spent.
        let due = e.navigation_due_at().map(tokio::time::Instant::from_std);
        let slice_end = [Some(until), Some(now + std::time::Duration::from_millis(50)), due].into_iter().flatten().min().unwrap_or(until);
        let end = e.pump(slice_end.saturating_duration_since(now)).await;
        if end.is_err() {
            break;
        }
        if matches!(end, Ok(engine::PumpEnd::Drained)) && !e.navigation_pending() {
            // Nothing left to run. Only a refresh still to come can change the page; wait for it.
            match e.navigation_due_at().map(tokio::time::Instant::from_std) {
                Some(d) if d < until => tokio::time::sleep_until(d).await,
                _ => break,
            }
        }
    }

    // The load is over; what follows prints. A deadline that fell due left a termination pending on the
    // isolate, which every script the printing runs would meet: clear it first.
    let timed_out = deadline.as_ref().is_some_and(|d| d.finish_loading()) || cut;
    if timed_out {
        e.clear_termination();
    }
    let reached_dcl = e.lifecycle_marks().0.is_some();
    // The deadline cut the load and the page is printed as it stood: said where a caller can see it, without changing
    // what is printed or how the run ends. A script that never yields is cut, the parser goes on, and the document
    // reaches DOMContentLoaded after it -- stock headless Chrome 150 on the same page never returns at all
    // (`--timeout=3000 --dump-dom`, killed after 20 s with no output, 2026-10-12).
    if timed_out && reached_dcl {
        eprintln!("korvus: --timeout: the deadline fell due after {}s; the page is printed as it stood", timeout_secs.unwrap_or_default());
    }

    if errors {
        let unc = e.uncaught_errors();
        let rej = e.unhandled_rejections();
        // `loadFired` RIDES THIS LINE RATHER THAN THE PROBE. It is the same reason the
        // two error stores do: it is the ENGINE's fact, and asking the page for it needed a slot on
        // the page (`KB.__loadFired`) that a page could read, forge or freeze. `tools/corpus/run.py`
        // scores `no-load` off this field and reads it from here now.
        println!("{}", serde_json::json!({ "uncaught": unc, "rejections": rej, "loadFired": e.load_fired() }));
    }
    if requests {
        // `dropped` rides the same line rather than being left to inference: a resource set that hit
        // the ledger cap must say so, or it reads as an engine that fetched less than Chrome.
        println!(
            "{}",
            serde_json::json!({ "requests": e.requests(), "dropped": e.requests_dropped() })
        );
    }
    if console {
        // What the last document's realms (the page and its frames) wrote with `console.*`: the channel
        // a `KORVUS_FRAME_EARLY` probe reports through from a cross-origin frame.
        println!("{}", serde_json::json!({ "console": e.take_output() }));
    }
    if let Some(expr) = eval {
        // Print the eval result: a string as-is (obstacle-course wraps its own JSON.stringify), else JSON.
        match e.eval_value("korvus:fetch-eval", expr)? {
            serde_json::Value::String(s) => println!("{s}"),
            other => println!("{}", serde_json::to_string(&other).unwrap_or_default()),
        }
    } else if let Some(mode) = dump {
        // Extraction. Korvus implements the structural dumps (html/links/text); readability-style
        // article + markdown extraction is a reference engine feature Korvus doesn't have -> those print empty.
        let expr = match mode.as_str() {
            "html" => "document.documentElement.outerHTML",
            "links" => "Array.from(document.querySelectorAll('a[href]')).map(a=>a.href).join('\\n')",
            "text" => "document.body ? (document.body.innerText || document.body.textContent || '') : ''",
            _ => "''",
        };
        let v = e.eval_value("korvus:fetch-dump", expr.to_string())?;
        println!("{}", v.as_str().unwrap_or(""));
    } else {
        let title = e.eval_value("korvus:fetch", "document.title".to_string())?;
        println!("{}", title.as_str().unwrap_or(""));
    }
    // Write the rendered frame (the document painted by the compositor) if asked.
    if let Some(path) = screenshot {
        match e.render_png() {
            Some(png) => std::fs::write(&path, &png)?,
            None => eprintln!("korvus: --screenshot: nothing rendered (no page, layout, or render backend)"),
        }
    }
    // headless Chrome reports `pageLoadTimedOut` only when there was no content at all: the deadline fell
    // due before `DOMContentLoaded`. What there was has been printed above either way.
    if timed_out && !reached_dcl {
        anyhow::bail!("--timeout: the page did not reach DOMContentLoaded within {}s", timeout_secs.unwrap_or_default());
    }
    // THE PROCESS ENDS HERE, AND THE ENGINE WITH IT. Tearing it down first -- every realm, the
    // isolate's heaps, its worker threads -- was 4 % of a minimal page's whole run (ANALYSIS §3.1) and
    // changes nothing anyone can see: the page is done, its output is printed, and what the run records
    // (`record`'s artifacts) is written after this returns. The operating system takes the memory back.
    std::mem::forget(e);
    Ok(())
}

/// Where `--wait-until` lets `korvus fetch` stop: Puppeteer's `waitUntil` points, on Chrome's definitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitUntil {
    DomContentLoaded,
    Load,
    /// Blink's `networkIdle`: no request of the document's in flight for 500 ms of idle time.
    NetworkIdle0,
    /// Blink's `networkAlmostIdle`: at most two in flight for 500 ms of idle time.
    NetworkIdle2,
}

impl WaitUntil {
    fn parse(token: &str) -> anyhow::Result<Self> {
        Ok(match token {
            "domcontentloaded" => Self::DomContentLoaded,
            "load" => Self::Load,
            "networkidle0" => Self::NetworkIdle0,
            "networkidle2" => Self::NetworkIdle2,
            other => anyhow::bail!("--wait-until {other:?}: expected load, domcontentloaded, networkidle0 or networkidle2"),
        })
    }

    /// Whether the document `e` holds has reached this point. The engine's own records, never the page's
    /// `document.readyState`, which a page can redefine.
    fn reached(self, e: &mut engine::Engine) -> bool {
        match self {
            Self::DomContentLoaded => e.lifecycle_marks().0.is_some(),
            Self::Load => e.load_fired(),
            Self::NetworkIdle0 => e.network_idle().idle.is_some(),
            Self::NetworkIdle2 => e.network_idle().almost_idle.is_some(),
        }
    }
}

/// `fut`, unless `--timeout`'s deadline falls due first (`None`).
async fn within<T>(at: Option<std::time::Instant>, fut: impl std::future::Future<Output = T>) -> Option<T> {
    match at {
        Some(at) => tokio::time::timeout_at(tokio::time::Instant::from_std(at), fut).await.ok(),
        None => Some(fut.await),
    }
}

/// `--timeout`'s deadline, kept on a thread of its own. This thread's event loop can stop pumping at the
/// deadline, but it cannot stop a script that never yields: only another thread can, by terminating the
/// isolate (`engine::IsolateInterrupt`). The page's engine changes with every navigation it follows, so
/// each new one's handle is sent along ([`Deadline::follow`]).
///
/// Two phases. Until [`Deadline::finish_loading`], the deadline cuts the LOAD: the isolate is terminated
/// and the page is printed as it stands. After it, printing gets [`PRINT_GRACE`]: it runs the engine's
/// own scripts, but against the page's DOM, and a page can make a getter spin -- past the grace the
/// process ends rather than outlive its deadline (a reference engine's harness counts a fetch that does as its
/// worst failure, a hang).
struct Deadline {
    tx: std::sync::mpsc::Sender<DeadlineMsg>,
    fired: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

enum DeadlineMsg {
    Follow(engine::IsolateInterrupt),
    FinishLoading(std::sync::mpsc::Sender<bool>),
}

/// How long printing the result may take once the load is over.
const PRINT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

impl Deadline {
    fn start(at: std::time::Instant, first: engine::IsolateInterrupt) -> Self {
        use std::sync::atomic::Ordering;
        use std::sync::mpsc::RecvTimeoutError;
        let (tx, rx) = std::sync::mpsc::channel::<DeadlineMsg>();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = fired.clone();
        std::thread::Builder::new()
            .name("korvus-deadline".into())
            .spawn(move || {
                let mut current = first;
                // Phase 1: the load.
                let print_from = loop {
                    match rx.recv_timeout(at.saturating_duration_since(std::time::Instant::now())) {
                        Ok(DeadlineMsg::Follow(next)) => current = next,
                        Ok(DeadlineMsg::FinishLoading(ack)) => {
                            let _ = ack.send(false);
                            break std::time::Instant::now();
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            flag.store(true, Ordering::SeqCst);
                            current.terminate();
                            break at;
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                };
                // Phase 2: printing. A `FinishLoading` that crossed the deadline still gets its answer.
                let stop = print_from + PRINT_GRACE;
                loop {
                    match rx.recv_timeout(stop.saturating_duration_since(std::time::Instant::now())) {
                        Ok(DeadlineMsg::FinishLoading(ack)) => {
                            let _ = ack.send(flag.load(Ordering::SeqCst));
                        }
                        // An engine the page moved to after the deadline cut the load is cut as it arrives:
                        // the termination went to the engine before it, which is gone.
                        Ok(DeadlineMsg::Follow(next)) => {
                            if flag.load(Ordering::SeqCst) {
                                next.terminate();
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            eprintln!("korvus: --timeout: printing the result did not finish; exiting");
                            std::process::exit(124);
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("the deadline thread starts");
        Deadline { tx, fired }
    }

    /// The page moved to a new engine (a navigation it asked for): the deadline cuts that one now.
    fn follow(&self, next: engine::IsolateInterrupt) {
        let _ = self.tx.send(DeadlineMsg::Follow(next));
    }

    /// Whether the deadline has cut the load.
    fn fired(&self) -> bool {
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The load is over and printing begins; returns whether the deadline cut the load, in which case a
    /// termination is pending on the isolate. Answered by the deadline's thread itself, so a deadline that
    /// falls due at this very moment is never missed.
    fn finish_loading(&self) -> bool {
        let (ack, answer) = std::sync::mpsc::channel();
        if self.tx.send(DeadlineMsg::FinishLoading(ack)).is_err() {
            return self.fired();
        }
        answer.recv().unwrap_or_else(|_| self.fired())
    }
}


/// `korvus probe <url>` — load a REAL page end-to-end through the subresource pipeline (S0): fetch the
/// HTML, execute inline + external `<script src>` (the site's scripts), then dump diagnostics so we can
/// see how far a page's scripts get (uncaught errors, console output, challenge markers, cookies).
async fn probe_url(url: &str) -> anyhow::Result<()> {
    let persona = build_persona();
    let mut e = engine::Engine::new(build_persona());
    e.set_url(url);
    e.run().await?;
    let resp = transport::fetch(transport::FetchRequest::navigation(url).worn_by(persona.as_ref())).await?;
    let html = resp.body;
    println!("── probe {url}");
    println!("  HTTP: {} bytes", html.len());
    let is_challenge = html.contains("captcha") || html.contains("Access Denied") || html.contains("verify you are human") || html.contains("checking your browser");
    println!("  looks like challenge/interstitial: {is_challenge}");
    let ext_count = html.matches("<script").filter(|_| true).count();
    println!("  <script> tags (approx): {ext_count}");

    e.load_page(&html, url, &[]).await?;
    // A real page's event loop never naturally ends (timers, sensor polling, pending fetches keep it
    // alive), so run it for a bounded window, then inspect where the sensor got. A hard event-loop error
    // is reported but non-fatal to the probe.
    match tokio::time::timeout(std::time::Duration::from_secs(8), e.run()).await {
        Ok(Ok(())) => println!("  event loop: settled (idle) before deadline"),
        Ok(Err(err)) => println!("  event-loop error (continuing): {err}"),
        Err(_) => println!("  event loop: still active at 8s deadline (normal for a live page)"),
    }

    // Optional: drive human-like behavior so a page's scripts collect interaction signals (a static, idle
    // page can read as a bot). Then run the loop again so the page re-posts any telemetry.
    if std::env::var_os("KORVUS_PROBE_BEHAVE").is_some() {
        let mut acts = 0usize;
        for (x, y) in [(220.0, 180.0), (540.0, 320.0), (760.0, 240.0), (400.0, 520.0)] {
            acts += e.move_mouse(x, y, 24.0).await.unwrap_or(0);
        }
        acts += e.scroll(900.0).await.unwrap_or(0);
        acts += e.ambient(6).await.unwrap_or(0);
        println!("  behavior driven: {acts} trusted events");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(8), e.run()).await;
    }

    let probe = |e: &mut engine::Engine, label: &str, expr: &str| {
        match e.eval_value(Box::leak(format!("probe:{label}").into_boxed_str()), expr.to_string()) {
            Ok(v) => println!("  {label}: {v}"),
            Err(err) => println!("  {label}: <err {err}>"),
        }
    };
    probe(&mut e, "title", "document.title");
    println!("  uncaught: {}", serde_json::json!(e.uncaught_errors()));
    println!("  rejections: {}", serde_json::json!(e.unhandled_rejections()));
    probe(&mut e, "cookie", "document.cookie");
    probe(&mut e, "ua", "navigator.userAgent");
    probe(&mut e, "webdriver", "navigator.webdriver");
    // The NETWORK cookie jar (transport) -- what the shared client would send on the next request. This
    // is where a Set-Cookie challenge/session token accumulates and persists across the sequence,
    // independent of the JS document.cookie store.
    let net = transport::document_cookie(url);
    println!("  net-cookies: {net:?}");
    for line in e.take_output() {
        println!("  console> {line}");
    }
    Ok(())
}



/// Why the endpoint cannot listen on `addr`, with what to do about it: a port another process holds, or one the OS keeps
/// for itself (Windows reserves ranges for Hyper-V and WSL, and refuses a bind there with WSAEACCES, os error 10013).
fn bind_error(addr: std::net::SocketAddr, e: std::io::Error) -> anyhow::Error {
    let port = addr.port();
    let hint = match e.kind() {
        std::io::ErrorKind::AddrInUse => format!("port {port} is in use: stop what holds it, or pick another with --port"),
        std::io::ErrorKind::PermissionDenied if cfg!(windows) => format!(
            "Windows reserves port {port} (`netsh int ipv4 show excludedportrange protocol=tcp` lists the ranges): pick \
             another with --port"
        ),
        std::io::ErrorKind::PermissionDenied => format!("not allowed to listen on port {port}: pick another with --port"),
        _ => return anyhow::anyhow!("cannot listen on {addr}: {e}"),
    };
    anyhow::anyhow!("cannot listen on {addr}: {e}; {hint}")
}

/// `korvus serve` (and `korvus browser [port]`): the browser endpoint. Each page target it makes is this binary's own
/// `serve-page` in a process of its own ([`serve_target_child`]); see `cdp::router`.
async fn browser_endpoint(addr: std::net::SocketAddr) -> anyhow::Result<()> {
    // No license and no session limit (D153): the endpoint calls no server of korvus's and runs every session it is
    // asked for.
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| bind_error(addr, e))?;
    println!("korvus serve on http://{} (WebSocket url: GET /json/version)", listener.local_addr()?);
    let persona: std::sync::Arc<dyn Persona> = std::sync::Arc::from(build_persona());
    // ONE GPU service for every page process of this browser, as Chrome has one GPU process per browser
    // (for the process floor): started now, off the navigation path, and handed to each page process. `None` (no GPU,
    // or no socket): each page starts its own at its first canvas. None at all with render off (`KORVUS_RENDER=off`).
    let tail = match render::wanted().then(render::gpu::start_shared_service).flatten() {
        Some(path) => vec![GPU_SOCKET_FLAG.to_string(), path.to_string_lossy().into_owned()],
        None => Vec::new(),
    };
    let launch = cdp::router::Launcher { exe: own_binary()?, args: vec![SERVE_PAGE.into()], tail };
    cdp::router::serve_browser(listener, persona, launch).await
}

/// This binary's own file, links resolved: what page processes run, so they stay the endpoint's release when an
/// update re-points `~/.korvus/current` while it serves (public/install.sh, install.ps1).
fn own_binary() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let real = std::fs::canonicalize(&exe).unwrap_or(exe);
    #[cfg(windows)]
    if let Some(plain) = real.to_str().and_then(|s| s.strip_prefix(r"\\?\")).filter(|s| s.as_bytes().get(1) == Some(&b':')) {
        return Ok(plain.into());
    }
    Ok(real)
}

/// `korvus … --wear <persona document>`.
const WEAR_FLAG: &str = "--wear";

/// The single-page server's command: `korvus serve-page <port>` (`korvus serve` is the browser endpoint).
const SERVE_PAGE: &str = "serve-page";

/// `--<flag> <value>` in a command's own arguments.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Every `--<flag> <value>` in a command's own arguments, a value's commas splitting it: `--allow-origin a,b
/// --allow-origin c`.
fn flag_values(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == flag).flat_map(|w| w[1].split(',').map(str::to_string).collect::<Vec<_>>()).collect()
}

/// The run's end, with why.
fn exit_with(msg: &str) -> ! {
    eprintln!("korvus: {msg}");
    std::process::exit(2);
}

/// This run's settings (`settings::Settings`): its flags over the environment over config.toml; a setting that
/// cannot hold is the run's end.
fn resolve(flags: &settings::Flags) -> settings::Settings {
    settings::read_config().and_then(|c| settings::Settings::resolve(flags, &c)).unwrap_or_else(|e| exit_with(&e))
}

/// Apply the host posture (`settings::Settings::host`, goal 1020 step 7). The engine reads `KORVUS_HOST`, and a
/// browser's page processes inherit this process's environment, so one setting here governs the whole browser:
/// `ignore` (the stealth default) lets a persona that describes another machine run; `strict` (korvus's own
/// identity, or `--host strict`) keeps the engine's refusal of a mismatch. The endpoint says it once and keeps its
/// page processes quiet (`KORVUS_HOST_QUIET`); a one-shot run keeps the engine's own per-page line.
fn apply_host(s: &settings::Settings, endpoint: bool) {
    if s.host == "ignore" {
        std::env::set_var("KORVUS_HOST", "ignore");
        if endpoint {
            std::env::set_var("KORVUS_HOST_QUIET", "1");
            if s.identity == "stealth" {
                eprintln!(
                    "[korvus] the stealth persona presents a device unlike this machine; a page measuring the host \
                     (worker timing, fonts, GPU strings) can tell -- this is stealth by design. `korvus serve --host \
                     strict` refuses instead."
                );
            }
        }
    } else {
        // strict: no stale KORVUS_HOST=ignore reaches the page processes, so the engine refuses a mismatch
        std::env::remove_var("KORVUS_HOST");
    }
}

/// The plugins a run loads, in load order: the identity's own first (korvus-stealth, with the settings' rotation and
/// noise), so a plugin given after it answers before it; then config.toml's `[[plugin]]`s and the `--plugin`s.
fn run_plugins(s: &settings::Settings) -> Vec<String> {
    let mut list = Vec::new();
    if s.identity == "stealth" {
        let lib = stealth_library().unwrap_or_else(|e| exit_with(&e));
        list.push(format!("{lib}:{}", s.stealth_config()));
    }
    list.extend(s.plugins.iter().cloned());
    list
}

/// `--<flag> <value>`, anywhere on the line, taken out of it; a flag without its value is the run's end.
fn take_value(argv: &mut Vec<String>, flag: &str, what: &str) -> Option<String> {
    let i = argv.iter().position(|a| a == flag)?;
    let Some(v) = argv.get(i + 1).cloned() else {
        eprintln!("korvus: {flag} needs {what}");
        std::process::exit(2);
    };
    argv.drain(i..i + 2);
    Some(v)
}

/// The korvus-stealth library beside this binary: `plugins/korvus-stealth.<so|dylib|dll>` in a release, or the one
/// cargo builds next to it (`libkorvus_stealth.so`, `libkorvus_stealth.dylib`, `korvus_stealth.dll`).
pub(crate) fn stealth_library() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("the stealth identity: this binary's path: {e}"))?;
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    let (dll, ext) = if cfg!(windows) {
        ("korvus_stealth.dll", "dll")
    } else if cfg!(target_os = "macos") {
        ("libkorvus_stealth.dylib", "dylib")
    } else {
        ("libkorvus_stealth.so", "so")
    };
    let candidates = [dir.join("plugins").join(format!("korvus-stealth.{ext}")), dir.join(dll)];
    candidates.iter().find(|p| p.is_file()).map(|p| p.display().to_string()).ok_or_else(|| {
        format!(
            "the stealth identity is the korvus-stealth plugin, and it is not beside this binary (looked for {}); \
             run with `--identity korvus` for korvus's own identity",
            candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        )
    })
}

/// The identity a context's slots answered, as one persona document: the identity slot's document, else the device
/// slot's device in one. `None` when nothing answered: korvus's own identity.
fn context_identity(spec: &cdp::router::ContextSpec) -> Option<String> {
    let d = spec.drawn.as_ref()?;
    if let Some(doc) = d.identity.as_ref().filter(|i| i.is_object()) {
        return Some(doc.to_string());
    }
    let device = d.device.as_ref().filter(|d| d.is_object())?;
    Some(
        serde_json::json!({
            "persona": 1,
            "name": d.name.clone().or_else(|| spec.name.clone()).unwrap_or_else(|| spec.context.clone()),
            "device": device,
        })
        .to_string(),
    )
}

/// A persona document that does not read is the run's end, with its reasons.
fn check_document(path: &str, text: &str) -> persona::document::Document {
    persona::document::read(text).unwrap_or_else(|reasons| {
        eprintln!("korvus: the persona document {path} is refused:");
        for r in reasons {
            eprintln!("  - {r}");
        }
        std::process::exit(2);
    })
}

/// Wear the identity `text` (a persona document) in every engine of this process, show it to the plugins
/// (`persona_json`), and hand it to the snapshot's maker through a file in `dir` (`--wear`).
fn wear(text: &str, dir: Option<std::path::PathBuf>) {
    let doc = check_document("the identity answered", text);
    if let Err(e) = persona::wear_document(doc) {
        eprintln!("korvus: {e}");
        std::process::exit(2);
    }
    plugin_host::set_persona_document(text);
    if let Some(dir) = dir {
        let file = if dir.is_dir() {
            let h = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
            dir.join(format!("korvus-identity-{h:016x}.json"))
        } else {
            dir
        };
        if std::fs::write(&file, text).is_ok() {
            engine::snapshot::child_args(vec![WEAR_FLAG.to_string(), file.display().to_string()]);
        }
    }
}

fn read_or_exit(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("korvus: cannot read the persona document {path}: {e}");
        std::process::exit(2);
    })
}

/// Calls the plugins' `shutdown` as `main` returns.
struct PluginsDown;

/// A plugin's `download` (ABI 5): `url` into `path`, written whole (through a `.partial` beside it) and only for a 200.
fn download_to(url: &str, path: &std::path::Path, within: std::time::Duration) -> Result<u16, String> {
    let (status, body) = transport::http_request("GET", url, &[], None, None, within)?;
    if status == 200 {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let partial = path.with_extension("partial");
        std::fs::write(&partial, &body).and_then(|()| std::fs::rename(&partial, path)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(status)
}

/// End the process with `result`, as returning it from `main` would -- the plugins' `shutdown`, the error printed as
/// `main` prints one, exit status 1 -- without the runtime's teardown, which waits for every task still running.
fn end_now(result: anyhow::Result<()>) -> ! {
    use std::io::Write as _;
    plugin_host::shutdown();
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

impl Drop for PluginsDown {
    fn drop(&mut self) {
        plugin_host::shutdown();
    }
}

/// Load this process's plugins: a page process of a browser endpoint loads exactly the list its context's spec names
/// (the endpoint's own plugins first, then the context's); any other run the list it is given (the identity plugin,
/// config.toml's `[[plugin]]` entries, then its `--plugin` arguments). A plugin that is refused stops the run, after
/// saying why.
fn load_plugins(spec: Option<&cdp::router::ContextSpec>, list: Vec<String>) {
    let list = match spec {
        Some(s) => {
            plugin_host::set_context(&s.context, s.name.as_deref().unwrap_or(""), &s.context);
            s.plugins.clone()
        }
        None => list,
    };
    for arg in list {
        let (path, config) = plugin_host::split_arg(&arg);
        if plugin_host::load(std::path::Path::new(&path), &config).is_err() {
            std::process::exit(2);
        }
    }
}

/// What a page process takes from its context's spec, before its engine exists: the proxy every request of the
/// context goes through (the transport reads the proxy environment when it builds its clients), its timezone and
/// locale, and the context store its cookies and storage live in.
fn apply_context(spec: &cdp::router::ContextSpec) {
    if let Some(p) = &spec.proxy {
        let server = transport::proxy_url(&if p.server.contains("://") { p.server.clone() } else { format!("http://{}", p.server) });
        for k in ["ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"] {
            std::env::set_var(k, &server);
        }
        // Chrome sends nothing to the loopback through a proxy unless the bypass list says `<-loopback>`.
        let mut bypass: Vec<String> = p.bypass.as_deref().unwrap_or("").split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        if !bypass.iter().any(|b| b == "<-loopback>") {
            bypass.extend(["localhost", "127.0.0.1", "[::1]"].map(String::from));
        }
        bypass.retain(|b| b != "<-loopback>");
        let list = bypass.join(",");
        std::env::set_var("NO_PROXY", &list);
        std::env::set_var("no_proxy", &list);
    }
    if let Some(tz) = spec.timezone.as_deref().filter(|t| !t.is_empty()) {
        std::env::set_var("TZ", tz);
    }
    if let Some(locale) = spec.locale.as_deref().filter(|l| !l.is_empty()) {
        std::env::set_var("KORVUS_LOCALE", locale);
    }
    // the permissions a DevTools client granted the context before this page opened
    spec::devtools::set_permissions(spec.permissions.clone());
    spec::devtools::set_download_behavior(spec.downloads.clone());
    if let Some(store) = &spec.store {
        if let Err(e) = transport::context_store::connect(store.clone()) {
            eprintln!("korvus: cannot reach context {}'s store at {}: {e}", store.context, store.addr);
            std::process::exit(2);
        }
    }
}

/// `korvus serve-page … --cdp-child … --gpu-socket <path>`: the browser's shared GPU service ([`browser_endpoint`]).
const GPU_SOCKET_FLAG: &str = "--gpu-socket";

/// Pending durable-storage `kv_get`s awaiting the router's reply (A4, the chrome.storage.local bridge): a
/// page-thread `kv_get` registers `id -> sender`, prints `KORVUS-KV-GET`, and blocks on the receiver; the
/// korvus-lifeline thread routes the matching `KORVUS-KV-REPLY` to it by `id`. Empty + untouched on any no-KV page.
static KV_PENDING: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, std::sync::mpsc::Sender<Option<String>>>>> = std::sync::OnceLock::new();
static KV_NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn kv_pending() -> &'static std::sync::Mutex<std::collections::HashMap<u64, std::sync::mpsc::Sender<Option<String>>>> {
    KV_PENDING.get_or_init(Default::default)
}

/// The page process's end of the durable chrome.storage.local bridge (A4). It implements the tenancy host calls that
/// are no-ops in a page process today (no router-side KV there): `kv_set`/`kv_get` forward to the ROUTER's KV actor
/// over stdout, and `kv_get` parks the page thread for the reply, which the korvus-lifeline thread delivers. The
/// page thread may block safely -- the work is done by the router (another process), and the reply arrives on a
/// SEPARATE OS thread (the lifeline), not through the parked page runtime. `hibernate`/`resume` are router-only.
struct PageKvBridge;
impl plugin_host::Tenancy for PageKvBridge {
    fn hibernate(&self, _ctx: &str) {}
    fn resume(&self, _ctx: &str) {}
    fn kv_set(&self, ctx: &str, key: &str, val: &str) {
        println!("{}{}", cdp::router::KV_SET_LINE, serde_json::json!({ "ctx": ctx, "key": key, "value": val }));
    }
    fn kv_get(&self, ctx: &str, key: &str) -> Option<String> {
        let id = KV_NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        kv_pending().lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        println!("{}{}", cdp::router::KV_GET_LINE, serde_json::json!({ "id": id, "ctx": ctx, "key": key }));
        let val = rx.recv_timeout(std::time::Duration::from_secs(5)).ok().flatten();
        kv_pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        val
    }
}

/// The page process's end of the popup lines (D74 step 3, `webapi`'s `core/html/popup.rs`): a popup this page opens
/// cross-site becomes a target of the endpoint's, and what crosses between it and this page -- its opening, messages,
/// navigations, closing -- goes out on stdout as one line each (`cdp::router`'s `POPUP_*` lines). The endpoint's
/// answers come back on stdin and the korvus-lifeline thread hands them to `webapi`.
struct PagePopupBridge;
impl webapi::PopupBridge for PagePopupBridge {
    fn open(&self, rid: u64, url: &str, name: &str, noopener: bool, referrer: &str) {
        println!("{}{}", cdp::router::POPUP_OPEN_LINE, serde_json::json!({ "rid": rid, "url": url, "name": name, "noopener": noopener, "referrer": referrer }));
    }
    fn post(&self, rid: u64, data: &[u8], origin: &str) {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(data);
        println!("{}{}", cdp::router::POPUP_POST_LINE, serde_json::json!({ "rid": rid, "data": data, "origin": origin }));
    }
    fn navigate(&self, rid: u64, url: &str) {
        println!("{}{}", cdp::router::POPUP_NAV_LINE, serde_json::json!({ "rid": rid, "url": url }));
    }
    fn close(&self, rid: u64) {
        println!("{}{}", cdp::router::POPUP_CLOSE_LINE, serde_json::json!({ "rid": rid }));
    }
    fn post_to_opener(&self, data: &[u8], origin: &str) {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(data);
        println!("{}{}", cdp::router::OPENER_POST_LINE, serde_json::json!({ "data": data, "origin": origin }));
    }
    fn close_self(&self) {
        // said once the command that ran it is answered (`cdp::request_window_close`)
        cdp::request_window_close();
    }
}

/// The page process's end of a JavaScript dialog (D88 step 2, `webapi`'s `core/html/dialog.rs`): the dialog goes out on
/// stdout (`cdp::router::DIALOG_OPEN_LINE`) and the page's thread waits for the router's answer, which the
/// korvus-lifeline thread hands back from stdin (`DIALOG_CLOSE_LINE`).
struct PageDialogBridge;
impl webapi::DialogBridge for PageDialogBridge {
    fn ask(&self, kind: &str, message: &str, default_prompt: &str) -> Option<(bool, String)> {
        let (tx, rx) = std::sync::mpsc::channel();
        *dialog_pending().lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        println!("{}{}", cdp::router::DIALOG_OPEN_LINE, serde_json::json!({ "type": kind, "message": message, "defaultPrompt": default_prompt }));
        rx.recv().ok().flatten()
    }
}

/// The dialog this page's thread is waiting on: where the router's answer goes.
fn dialog_pending() -> &'static std::sync::Mutex<Option<std::sync::mpsc::Sender<Option<(bool, String)>>>> {
    static P: std::sync::OnceLock<std::sync::Mutex<Option<std::sync::mpsc::Sender<Option<(bool, String)>>>>> = std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

/// The router's answer to this page's dialog (`DIALOG_CLOSE_LINE` `{accept, promptText}`, or `{unhandled: true}` when no
/// client will answer). `false` when the line is not one.
fn dialog_line(line: &str) -> bool {
    let Some(j) = line.strip_prefix(cdp::router::DIALOG_CLOSE_LINE) else { return false };
    let v: serde_json::Value = serde_json::from_str(j).unwrap_or_default();
    let answer = (!v.get("unhandled").and_then(|u| u.as_bool()).unwrap_or(false)).then(|| {
        (v.get("accept").and_then(|a| a.as_bool()).unwrap_or(false), v.get("promptText").and_then(|t| t.as_str()).unwrap_or("").to_string())
    });
    if let Some(tx) = dialog_pending().lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = tx.send(answer);
    }
    true
}

/// One line the endpoint wrote to this page process about a popup (`cdp::router`'s `POPUP_EV_LINE`, `OPENER_*`):
/// handed to `webapi`. `false` when the line is none of them.
fn popup_line(line: &str) -> bool {
    use base64::Engine as _;
    let bytes = |v: &serde_json::Value| v.get("data").and_then(|d| d.as_str()).and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()).unwrap_or_default();
    let origin = |v: &serde_json::Value| v.get("origin").and_then(|o| o.as_str()).unwrap_or("").to_string();
    if let Some(j) = line.strip_prefix(cdp::router::POPUP_EV_LINE) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
            let rid = v.get("rid").and_then(|x| x.as_u64()).unwrap_or(0);
            match v.get("kind").and_then(|k| k.as_str()) {
                Some("message") => webapi::popup_message(rid, bytes(&v), origin(&v)),
                Some("closed") => webapi::popup_closed(rid),
                _ => {}
            }
        }
        return true;
    }
    if let Some(j) = line.strip_prefix(cdp::router::OPENER_MSG_LINE) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
            webapi::opener_message(bytes(&v), origin(&v));
        }
        return true;
    }
    if line.starts_with(cdp::router::OPENER_GONE_LINE.trim_end()) {
        webapi::opener_gone();
        return true;
    }
    false
}

/// A page target's own process, started by `korvus serve` (`serve-page 0 --cdp-child <target id> <context id>`, set into
/// `cdp::router` before this runs; arguments, not environment, because the environment keys the template cache). It
/// serves the one page on an ephemeral loopback port and
/// says which on stdout, its only line there (`cdp::router::PORT_LINE`). Its stdin is the router's lifeline:
/// when it closes, the router is gone, and so is this process -- a target never outlives its browser.
async fn serve_target_child(port: u16) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], port))).await?;
    println!("{}{}", cdp::router::PORT_LINE, listener.local_addr()?.port());
    std::thread::Builder::new().name("korvus-lifeline".into()).spawn(|| {
        // Still the router's lifeline: EOF on stdin (the router is gone) -> exit(0), so a target never outlives its
        // browser. It ALSO routes the router's KORVUS-KV-REPLY lines (chrome.storage.local get answers, A4) to the
        // parked kv_get by id; any other line is ignored. Demand-driven -- a no-KV child just blocks until EOF,
        // exactly as before -- and it sends NOTHING upstream.
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(std::io::stdin());
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => std::process::exit(0),
                Ok(_) => {
                    if popup_line(line.trim_end()) || dialog_line(line.trim_end()) {
                        continue;
                    }
                    if let Some(j) = line.trim_end().strip_prefix(cdp::router::KV_REPLY_LINE) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
                            let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                            let val = v.get("value").and_then(|x| x.as_str()).map(str::to_string);
                            if let Some(tx) = kv_pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
                                let _ = tx.send(val);
                            }
                        }
                    }
                }
            }
        }
    })?;
    // A4: make the durable chrome.storage.local host calls (kv_get/kv_set) reach the router's KV from this page
    // process (they no-op without a tenancy). Inert until a plugin actually calls them.
    plugin_host::set_tenancy(std::sync::Arc::new(PageKvBridge));
    // D74 step 3: a popup this page opens cross-site opens as a target of the endpoint's.
    webapi::set_popup_bridge(std::sync::Arc::new(PagePopupBridge));
    // D88 step 2: a dialog is asked of the clients that enabled `Page`, through the endpoint.
    webapi::set_dialog_bridge(std::sync::Arc::new(PageDialogBridge));
    let persona: std::sync::Arc<dyn Persona> = std::sync::Arc::from(build_persona());
    cdp::serve(listener, || engine::Engine::new(build_persona()), persona).await
}

async fn serve(port: u16) -> anyhow::Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    // The HTTP discovery endpoint, which is what a CDP client is given (Playwright's
    // `connectOverCDP` takes exactly this and reads the WebSocket url out of `/json/version`). The
    // WebSocket path itself carries a per-process UUID now (W9.16), so printing a fixed one here
    // would be advertising a url this process does not answer with.
    println!("korvus CDP server on http://{addr} (WebSocket url: GET /json/version)");
    // Serving is a plugin (D5): load the CDP serving plugin to run as a daemon (fetch/probe skip it =
    // library mode). The persona is the shipped capture bundle (coherent navigator/screen/UA-CH/plugins).
    // The persona is resolved ONCE here, for the discovery endpoints: `/json/version` answers four
    // strings off the worn identity and must not build a realm to do it. Every
    // WebSocket connection still gets an engine of its own, built from a persona of its own.
    let discovery_persona: std::sync::Arc<dyn Persona> = std::sync::Arc::from(build_persona());
    let serving: Box<dyn spec::ServingPlugin> =
        Box::new(cdp::CdpServing::new(|| engine::Engine::new(build_persona()), discovery_persona));
    serving.serve(addr).await.map_err(|e| anyhow::anyhow!(e.to_string()))

}

async fn run_file(path: &str) -> anyhow::Result<()> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?;
    // The shipped capture bundle drives all persona-visible values. The script's own name is `<anonymous>`, as
    // Chrome names code given to `eval()` with no `//# sourceURL`: a stack the script captures carries no `korvus:`
    // resource name that would mark the run as korvus's (the CDP path never had one; W: the run path did).
    let mut e = engine::Engine::new(build_persona());
    e.eval("<anonymous>", src)?;
    e.run().await?;
    for line in e.take_output() {
        println!("{line}");
    }
    Ok(())
}

/// `korvus run <file.js> --url <page url> ...`: the script run IN A PAGE -- a document at `--url`, with its DOM, its
/// origin, its cookies and storage -- rather than in a bare realm. What an offline token-minting run needs: a vendor's
/// SDK booted in a real document of the target site, with no request reaching the network unless the run allows it.
///
///   --html <file>            the document's markup, served as the response to `--url` (no request is made for it)
///   --map <prefix>=<dir>     a request whose URL starts with `prefix` is answered from `dir` (the rest of its path
///                            under it), as the network would answer it: the SDK files the page's `<script src>` loads
///   --offline                every other request fails `net::ERR_INTERNET_DISCONNECTED`
///   --on-request <cmd>       a dynamic responder: for a request no `--html`/`--map` answered, run <cmd> with the
///                            request as one JSON line on stdin and read its answer as one JSON line on stdout --
///                            `{"fulfill":{status,headers,body}}`, `{"continue":{}}`, `{"fail":"net::ERR_…"}`, or
///                            null/empty to fall through. So an SDK's dynamic challenge is answered here, with no
///                            `serve`+CDP+`Fetch`-relay to stand up (`body` is text, or `{"base64":"…"}` for bytes)
///   --wait SECS              how long the page runs before the script (3 by default; less when it settles)
///   --cookies                print every cookie of the page's store, HttpOnly ones included, as `Storage.getCookies`
///   --as <name>              the script's own source name (default `<anonymous>`, as `eval()` names it): what a
///                            stack the script captures shows for its frames
///
/// The script runs after the page's own; its completion value is printed (a string as it is, anything else as JSON),
/// a promise awaited for up to 30 s. What the page logged goes to stderr.
async fn run_page(path: &str, args: &[String]) -> anyhow::Result<()> {
    let src = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?;
    let mut url: Option<String> = None;
    let mut html: Option<Vec<u8>> = None;
    let mut maps: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut offline = false;
    let mut cookies = false;
    let mut wait = 3.0f64;
    let mut on_request: Option<String> = None;
    let mut as_name: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let next = args.get(i + 1).cloned();
        match args[i].as_str() {
            "--url" => url = next,
            "--html" => html = Some(std::fs::read(next.as_deref().unwrap_or("")).map_err(|e| anyhow::anyhow!("--html: {e}"))?),
            "--map" => {
                let m = next.unwrap_or_default();
                let (prefix, dir) = m.split_once('=').ok_or_else(|| anyhow::anyhow!("--map takes <url prefix>=<dir>"))?;
                maps.push((prefix.to_string(), std::path::PathBuf::from(dir)));
            }
            "--on-request" => on_request = next,
            "--as" => as_name = next,
            "--wait" => wait = next.and_then(|v| v.parse().ok()).unwrap_or(wait),
            "--offline" => {
                offline = true;
                i += 1;
                continue;
            }
            "--cookies" => {
                cookies = true;
                i += 1;
                continue;
            }
            other => anyhow::bail!("korvus run: unknown argument {other}"),
        }
        i += 2;
    }
    let url = url.ok_or_else(|| anyhow::anyhow!("--url needs the page's URL"))?;
    if html.is_some() || offline || !maps.is_empty() || on_request.is_some() {
        let doc = html.map(|h| (url.split('#').next().unwrap_or(&url).to_string(), h));
        transport::intercept::set_interceptor(Some(std::sync::Arc::new(LocalFiles { doc, maps, offline, on_request })));
    }
    let mut e = engine::Engine::new(build_persona());
    e.set_url(&url);
    transport::ledger_reset();
    let navigation_start = std::time::Instant::now();
    let resp = transport::fetch(transport::FetchRequest {
        navigation_start: Some(navigation_start),
        ..transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())
    })
    .await?;
    let doc_url = if resp.url.is_empty() { url.clone() } else { resp.url.clone() };
    e.load_page_response(&resp.bytes, &doc_url, &[], &resp.headers).await?;
    let until = tokio::time::Instant::now() + std::time::Duration::from_secs_f64(wait.max(0.0));
    loop {
        let now = tokio::time::Instant::now();
        if now >= until {
            break;
        }
        let end = e.pump((until - now).min(std::time::Duration::from_millis(50))).await;
        if end.is_err() || (matches!(end, Ok(engine::PumpEnd::Drained)) && !e.navigation_pending()) {
            break;
        }
    }
    // `<anonymous>` by default, as `eval()` names the code it runs when no `//# sourceURL` is given, so a stack the
    // script captures carries no `korvus:` name (`--as` overrides). `&'static` because the engine's eval takes one;
    // leaking it is harmless -- this process runs one script and exits.
    let name: &'static str = Box::leak(as_name.unwrap_or_else(|| "<anonymous>".to_string()).into_boxed_str());
    let value = e.eval_value_settled(name, src, std::time::Duration::from_secs(30)).await;
    for line in e.take_output() {
        eprintln!("{line}");
    }
    match value? {
        serde_json::Value::String(s) => println!("{s}"),
        other => println!("{}", serde_json::to_string(&other).unwrap_or_default()),
    }
    if cookies {
        let list: Vec<serde_json::Value> = transport::cookies::devtools_cookies(None)
            .iter()
            .map(|c| serde_json::json!({ "name": c.name, "value": c.value, "domain": c.domain, "path": c.path, "expires": c.expires, "httpOnly": c.http_only, "secure": c.secure, "sameSite": c.same_site }))
            .collect();
        println!("{}", serde_json::json!({ "cookies": list }));
    }
    Ok(())
}

/// `korvus run --url`'s network: the document from `--html`, the `--map`ped files from disk, the `--on-request`
/// responder's dynamic answers, and with `--offline` nothing else -- answered through the transport's interception
/// seam, the one a DevTools client's `Fetch` domain uses, so the page's requests are made, recorded and answered as
/// Chrome's are when a client fulfils them. Precedence: the document, then a static `--map`, then the responder, then
/// `--offline` (else the request reaches the network).
struct LocalFiles {
    doc: Option<(String, Vec<u8>)>,
    maps: Vec<(String, std::path::PathBuf)>,
    offline: bool,
    /// `--on-request <cmd>`: the shell command a request no static rule answered is handed to.
    on_request: Option<String>,
}

impl transport::intercept::Interceptor for LocalFiles {
    fn pause(&self, req: &transport::intercept::PausedRequest) -> Option<transport::intercept::PauseFuture> {
        use transport::intercept::{Fulfilment, Paused};
        let ok = |body: Vec<u8>, mime: &str| Paused::Fulfill(Fulfilment {
            status: 200,
            phrase: "OK".into(),
            headers: vec![("Content-Type".into(), mime.into()), ("Content-Length".into(), body.len().to_string())],
            body,
        });
        let url = req.url.split('#').next().unwrap_or(&req.url);
        // The static rules first, synchronously.
        let answer = if let Some((u, body)) = self.doc.as_ref().filter(|(u, _)| u == url) {
            let _ = u;
            Some(ok(body.clone(), "text/html; charset=utf-8"))
        } else if let Some((prefix, dir)) = self.maps.iter().find(|(p, _)| url.starts_with(p.as_str())) {
            let rest = url[prefix.len()..].split('?').next().unwrap_or("").trim_start_matches('/');
            let file = dir.join(if rest.is_empty() { "index.html" } else { rest });
            Some(match std::fs::read(&file) {
                Ok(body) => ok(body, mime_of(&file)),
                Err(_) => Paused::Fail("net::ERR_FILE_NOT_FOUND".into()),
            })
        } else {
            None
        };
        if let Some(a) = answer {
            return Some(Box::pin(async move { a }));
        }
        // No static rule matched. The responder answers if there is one; else offline fails, else the request goes to
        // the network (a plain continue).
        let fallthrough = if self.offline { Paused::Fail("net::ERR_INTERNET_DISCONNECTED".into()) } else { Paused::Continue(Default::default()) };
        let Some(cmd) = self.on_request.clone() else {
            // No responder: keep the old behaviour exactly -- offline fails, otherwise the request is not intercepted.
            return self.offline.then(|| Box::pin(async move { fallthrough }) as transport::intercept::PauseFuture);
        };
        // Hand the request to the responder as one JSON line on stdin; its one JSON line of stdout is the answer. Run
        // on the blocking pool -- this is the current-thread runtime, so a child process must not be awaited on it.
        let input = serde_json::json!({
            "url": req.url,
            "method": req.method,
            "headers": req.headers.iter().map(|(k, v)| [k.as_str(), v.as_str()]).collect::<Vec<_>>(),
            "body_base64": req.body.as_ref().map(|b| { use base64::Engine as _; base64::engine::general_purpose::STANDARD.encode(b) }),
            "destination": req.destination,
        }).to_string();
        Some(Box::pin(async move {
            match tokio::task::spawn_blocking(move || run_responder(&cmd, &input)).await {
                Ok(Ok(Some(p))) => p,
                Ok(Ok(None)) => fallthrough,
                Ok(Err(e)) => {
                    eprintln!("korvus: --on-request: {e}");
                    fallthrough
                }
                Err(_) => fallthrough,
            }
        }))
    }
}

/// Run the `--on-request` responder `cmd` with `input` (one JSON line) on its stdin, and read its one JSON line of
/// stdout as the answer. `Ok(None)` is null/empty -- fall through. An answer is `{"fulfill":{status,headers,body}}`,
/// `{"continue":{}}` or `{"fail":"net::ERR_…"}`; a `fulfill` body is text, or `{"base64":"…"}` for bytes.
fn run_responder(cmd: &str, input: &str) -> anyhow::Result<Option<transport::intercept::Paused>> {
    use std::io::Write;
    use transport::intercept::{Fulfilment, Overrides, Paused};
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("cannot run {cmd:?}: {e}"))?;
    child.stdin.take().expect("stdin was piped").write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() || text == "null" {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| anyhow::anyhow!("the answer is not JSON: {e} ({text:?})"))?;
    if let Some(f) = v.get("fulfill") {
        let status = f.get("status").and_then(|s| s.as_u64()).unwrap_or(200) as u16;
        let headers = responder_headers(f.get("headers"));
        let body = responder_body(f.get("body"))?;
        let phrase = f.get("phrase").and_then(|p| p.as_str()).unwrap_or("OK").to_string();
        return Ok(Some(Paused::Fulfill(Fulfilment { status, phrase, headers, body })));
    }
    if let Some(fail) = v.get("fail").and_then(|x| x.as_str()) {
        return Ok(Some(Paused::Fail(fail.to_string())));
    }
    if let Some(c) = v.get("continue") {
        let o = Overrides {
            url: c.get("url").and_then(|x| x.as_str()).map(String::from),
            method: c.get("method").and_then(|x| x.as_str()).map(String::from),
            body: c.get("body").and_then(|_| responder_body(c.get("body")).ok()),
            headers: c.get("headers").map(|h| responder_headers(Some(h))),
        };
        return Ok(Some(Paused::Continue(o)));
    }
    Ok(None)
}

/// A responder's `headers`: an array of `[name, value]` pairs or an object of `name: value`.
fn responder_headers(v: Option<&serde_json::Value>) -> Vec<(String, String)> {
    match v {
        Some(serde_json::Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect(),
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|p| {
                let p = p.as_array()?;
                Some((p.first()?.as_str()?.to_string(), p.get(1)?.as_str().unwrap_or("").to_string()))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A responder's `body`: a string as UTF-8 bytes, or `{"base64":"…"}` for raw bytes.
fn responder_body(v: Option<&serde_json::Value>) -> anyhow::Result<Vec<u8>> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::String(s)) => Ok(s.clone().into_bytes()),
        Some(serde_json::Value::Object(m)) => {
            use base64::Engine as _;
            let b64 = m.get("base64").and_then(|x| x.as_str()).ok_or_else(|| anyhow::anyhow!("a body object needs a base64 field"))?;
            base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| anyhow::anyhow!("body base64: {e}"))
        }
        Some(other) => Ok(other.to_string().into_bytes()),
    }
}

/// The `Content-Type` a mapped file is served with, by its extension.
fn mime_of(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn banner() {
    let persona = build_persona();
    let render_backend = render::NullBackend;
    let _cfg = spec::Config::default();
    let persona_ref: &dyn Persona = persona.as_ref();
    let render_ref: &dyn RenderBackend = &render_backend;
    let _ = persona_ref.navigator();
    // Coherence Engine (DoD-5): audit the assembled persona for cross-layer contradictions before it
    // ever wears the identity. A "Frankenstein" persona is an instant bot tell, so a non-empty result
    // is a launch-time error surfaced to the operator.
    let incoherences = spec::coherence::audit(persona_ref);

    println!("korvus {VERSION} (commands: serve, fetch, run, probe, setup, persona, plugin ls, plugin check, stats, update, --version)");
    println!("  engine:    {}", engine::engine_name());
    println!("  webapi:    {}", webapi::surface_name());
    println!("  render:    {}", render_ref.name());
    println!("  transport: {}", transport::transport_name());
    println!("  cdp:       {}", cdp::cdp_name());
    println!("  behavior:  {}", behavior::behavior_name());
    if incoherences.is_empty() {
        println!("  coherence: OK (persona threads all layers)");
    } else {
        println!("  coherence: {} contradiction(s):", incoherences.len());
        for i in &incoherences {
            println!("    - [{}] {}", i.layers, i.detail);
        }
    }
}
