//! `korvus --help` and `korvus <command> --help`: what each command takes. The settings' flags (`settings::Flags`) are
//! every page-running command's, so they are listed once, under `serve`, and named by the others.

/// The settings' flags, as `korvus serve --help` lists them (the README's table, `Every flag of korvus serve`).
const SETTINGS: &str = "\
  --identity stealth|korvus    the stealth persona, or korvus as itself
  --rotate rotate|fixed|named|off
                               a new persona per session, one from a fixed seed, a named persona's, or the build's own
  --seed S, --device D         the seed; a device
  --proxy URL                  an HTTP or SOCKS5 proxy (with a login); time zone and locale follow its exit IP
  --timezone TZ, --locale L    fixed values instead
  --humanize, --no-humanize    human-like pointer paths
  --host ignore|strict         run on a machine unlike the persona's device, or refuse
  --plugin PATH[:CONFIG]       load a plugin
";

/// The text `korvus <command> --help` prints; korvus's own overview for any other word.
pub fn usage(command: &str) -> String {
    match command {
        "serve" | "browser" => format!(
            "\
usage: korvus serve [--port N] [--listen ADDR] [flags]

A DevTools endpoint Puppeteer and Playwright connect to, as they connect to Chrome; each browser context is one session
with its own persona.

  --port N                     the port (9222)
  --listen ADDR                the address (127.0.0.1)
  --allow-origin ORIGIN        a web page's origin that may open the endpoint (`*` for any)
  --allow-host NAME            a name the endpoint is reached by besides IPs and `localhost`
  --fresh                      every named persona starts with no cookies or storage and keeps none
  --block KINDS                drop requests of these Fetch kinds (`image`, `font`, ...) or `host:<substr>`
  --http-cache-max MB          cap the in-memory HTTP cache below 50 MB (`0` turns it off)
  --max-heap MB                cap each page's JavaScript heap (a page past it dies, as a Chrome tab does)
{SETTINGS}"
        ),
        "fetch" => format!(
            "\
usage: korvus fetch <url> [options] [flags]

Load a page, run it, and print its title, or what an option asks for.

  --eval EXPR                  print EXPR's value instead (a string as is, else JSON)
  --eval-early EXPR            run EXPR in the page before its own scripts
  --dump html|links|text       print the document, its links or its text
  --wait SECS                  run the page up to SECS seconds (sooner if it settles)
  --wait-until load|domcontentloaded|networkidle0|networkidle2
                               stop when the document reaches that point
  --timeout SECS               headless Chrome's --timeout: when it falls due the page is printed as it stands, and the
                               run ends
  --errors                     print the page's uncaught errors and unhandled rejections (one JSON line, first)
  --requests                   print every request the page made (one JSON line, first)
  --console                    print what the page wrote with console.* (one JSON line, first)
  --screenshot PATH            write the rendered page as a PNG
The settings' flags (`korvus serve --help`) apply.
"
        ),
        "run" => "\
usage: korvus run <file.js> [--url <page url> [--html <file>] [--map <url prefix>=<dir>]... [--on-request <cmd>]
                  [--offline] [--wait SECS] [--cookies] [--as <name>]]

Run a script in a page's origin and print its value (a promise is awaited); without --url, in a bare realm with no
document. The settings' flags (`korvus serve --help`) apply.
"
        .into(),
        "probe" => "usage: korvus probe <url>\n\nLoad a page and report how far its scripts got: uncaught errors, console output, challenge markers, cookies.\n".into(),
        "setup" => "\
usage: korvus setup [--non-interactive] [--no-plugins | --plugins]

Ask two questions (stealth by default? a new persona per session?) and write the answers to config.toml, then have
each plugin those answers load install what it needs (the stealth identity's data); --no-plugins skips that, and
--plugins does only that, config.toml kept as it is.
"
        .into(),
        "update" => "usage: korvus update [--check]\n\nInstall the latest release if it is newer; --check only says whether there is one.\n".into(),
        "stats" => "usage: korvus stats [--port N]\n\nThe active sessions of the `korvus serve` on this port (9222).\n".into(),
        "persona" => "\
usage: korvus persona new <name> [--from <seed:<os>:<x> | document.json>]
       korvus persona set <name> <key>=<value>...
       korvus persona ls | rm <name> | reset <name> | export <name> [file] | import <name> <file>

Named personas: a session at http://127.0.0.1:9222/persona/<name> wears one and keeps its cookies and storage.
"
        .into(),
        "plugin" => "usage: korvus plugin ls [flags] | korvus plugin check <path>[:<config JSON>]\n\nThe loaded plugins and the slots they fill; or what one library declares, loading nothing else.\n".into(),
        _ => format!(
            "\
korvus {} -- a browser engine with a Chrome identity, driven over DevTools

usage: korvus <command> [options] [flags]

  serve      a DevTools endpoint for Puppeteer and Playwright
  fetch      load a page and print it
  run        run a script in a page's origin
  probe      report how far a page's scripts get
  persona    named personas: new, set, ls, rm, reset, export, import
  plugin     ls, check
  stats      a running endpoint's sessions
  setup      write config.toml's two answers
  update     install the latest release
  --version  the version

`korvus <command> --help` says what a command takes.
",
            crate::VERSION
        ),
    }
}
