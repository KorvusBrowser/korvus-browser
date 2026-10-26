//! Goal 1020 invariants I5 and I6. The engine holds none of the persona layer -- no rotation, device pool, persona OS
//! map, IP database or readback noise -- outside the korvus-stealth plugin (D12), and korvus-stealth's source never
//! enters `korvus/public/`, the tree mirrored to the public repository (D18).
//!
//! A grep, deliberately: the layer is a set of names (the noise and its seed, the pool and its file, the OS map), and a
//! name that comes back into an engine crate is how the layer comes back. `KORVUS_LAYER_ROOT` points the check at
//! another korvus tree (the probe's red run, on the tree before step 4).

use std::path::{Path, PathBuf};

/// What the persona layer's code is named, and what each name is.
const LAYER: &[(&str, &str)] = &[
    ("spec::noise", "readback noise"),
    ("noise::canvas(", "readback noise"),
    ("noise::audio(", "readback noise"),
    ("noise_seed", "readback noise's seed"),
    ("seed_from(", "a persona seed"),
    ("KORVUS_PERSONA_SEED", "a persona seed"),
    ("random_pool_index", "rotation"),
    ("worn_family", "the persona OS map"),
    ("desktop.json.gz", "the device pool"),
    ("pool_draw", "the device pool"),
    ("pool_entry", "the device pool"),
    ("dbip-city", "the IP database"),
    (".mmdb", "the IP database"),
];

/// Every stealth source file carries this line, so the public tree can be searched for it.
const MARKER: &str = "korvus-stealth: private source (D18), never in korvus/public/.";

fn root() -> PathBuf {
    match std::env::var_os("KORVUS_LAYER_ROOT") {
        Some(r) => PathBuf::from(r),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
    }
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() {
            if name != "target" && name != "node_modules" && name != ".git" {
                files(&p, out);
            }
        } else {
            out.push(p);
        }
    }
}

/// The engine's Rust: every crate under `core/`, `plugins/` and `korvus/` but korvus-stealth, and not this file.
fn engine_sources(root: &Path) -> Vec<PathBuf> {
    let mut all = Vec::new();
    for top in ["core", "plugins", "korvus"] {
        files(&root.join(top), &mut all);
    }
    all.retain(|p| {
        let s = p.to_string_lossy().replace('\\', "/");
        s.ends_with(".rs") && !s.contains("/plugins/korvus-stealth/") && !s.ends_with("/korvus-runtime/tests/persona_layer.rs")
    });
    all
}

#[test]
fn the_engine_holds_none_of_the_persona_layer() {
    let root = root();
    let sources = engine_sources(&root);
    assert!(sources.len() > 100, "the engine's sources under {}: {}", root.display(), sources.len());
    let mut found = Vec::new();
    for path in &sources {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        for (n, line) in text.lines().enumerate() {
            for (name, what) in LAYER {
                if line.contains(name) {
                    found.push(format!("{}:{}: {what} (`{name}`)", path.strip_prefix(&root).unwrap_or(path).display(), n + 1));
                }
            }
        }
    }
    assert!(found.is_empty(), "the persona layer is korvus-stealth's (I5), and the engine holds {}:\n{}", found.len(), found.join("\n"));
}

#[test]
fn korvus_stealth_s_source_stays_out_of_the_public_tree() {
    let root = root();
    let stealth = root.join("plugins/korvus-stealth");
    let mut theirs = Vec::new();
    files(&stealth, &mut theirs);
    theirs.retain(|p| p.extension().is_some_and(|e| e == "rs" || e == "toml"));
    assert!(!theirs.is_empty(), "korvus-stealth's sources under {}", stealth.display());
    for p in &theirs {
        let text = std::fs::read_to_string(p).unwrap();
        assert!(text.contains(MARKER), "{} does not carry the private-source marker", p.display());
    }
    let mut public = Vec::new();
    files(&root.join("public"), &mut public);
    assert!(!public.is_empty(), "the public tree");
    let leaked: Vec<String> = public
        .iter()
        .filter(|p| std::fs::read(p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(MARKER)))
        .map(|p| p.display().to_string())
        .collect();
    assert!(leaked.is_empty(), "korvus-stealth's source in korvus/public/ (I6): {leaked:?}");
}

#[test]
fn with_no_identity_plugin_the_engine_is_korvus() {
    // No identity answered in this process: what the engine wears is its own (D11), saying it is korvus.
    let p = persona::from_env().expect("the engine's own identity");
    let ua = p.navigator().user_agent;
    assert!(ua.ends_with(&format!(" Korvus/{}", persona::KORVUS_VERSION)), "{ua}");
    assert!(p.client_hints().brands.iter().any(|b| b.brand == "Korvus"), "{:?}", p.client_hints().brands);
    assert!(p.webdriver(), "navigator.webdriver while korvus is automated");
}
