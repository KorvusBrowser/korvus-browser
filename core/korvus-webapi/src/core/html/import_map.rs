//! Import maps — `<script type="importmap">`, parsed and resolved the way Chrome 150 does it.
//!
//! ## What was wrong
//!
//! Nothing here existed. `HTMLScriptElement.supports("importmap")` answered `true` and
//! `script_type_or_language_runs` correctly declined to *execute* an import map — and that was the
//! whole of it: the element's text was never read, so every **bare specifier** on the page reached
//! deno_core's `resolve_import`, which has no notion of a map and said
//!
//! ```text
//! Relative import path "@theme/section-hydration" not prefixed with / or ./ or ../
//! ```
//!
//! That is the sentence a browser *without* import-map support reports, and it took out six rows of
//! the 513-row live corpus, on every one of which the paired Chrome 150 oracle emitted nothing at
//! all (`chrome_rows = 0`). The five pages behind those rows are named in
//! a demand map and deliberately not here (M6: the engine carries no corpus
//! hostname). All five carry a parser-found inline `<script type="importmap">` in their initial
//! HTML; read on 2026-09-21, one read-only fetch each.
//!
//! ## Everything below was MEASURED on the installed Chrome 150.0.7871.46
//!
//! `--headless --disable-gpu --no-sandbox --virtual-time-budget=4000 --dump-dom`, over a loopback
//! `python3 -m http.server`, twenty-three probe pages on 2026-09-21. Each page writes its readings
//! into a `<pre id=out>` so the DOM dump *is* the transcript.
//! `tests/fixtures/import-map-probe.chrome-150.txt` is the part of it this engine reproduces.
//!
//! ### Resolution
//!
//! | probe | specifier | map | Chrome |
//! |---|---|---|---|
//! | exact | `dep` | `"dep": "/js/dep.js"` | `/js/dep.js` |
//! | prefix | `@theme/nested/deep.js` | `"@theme/": "/js/theme/"` | `/js/theme/nested/deep.js` |
//! | exact beats prefix | `@theme/section` | both `"@theme/"` and `"@theme/section"` | the exact key's value |
//! | longest prefix wins | `@theme/nested/deep.js` | `"@theme/"` **and** `"@theme/nested/"` | the longer key's, whatever the declaration order |
//! | key IS the prefix | `@theme/` | `"@theme/": "/js/theme/"` | resolves to `/js/theme/` (then 404s) |
//! | backtracking | `a/../evil.js` | `"a/": "/js/theme/"` | **refused** — see below |
//! | URL-like specifier | `./relative-a.js` | `"./relative-a.js": "/js/root-a.js"` | `/js/root-a.js` — a map remaps URL-like specifiers too |
//! | unmapped URL-like | `./js/plain.js` | — | resolves normally against the referrer |
//! | `import()` from a CLASSIC script | `dep` | — | uses the map |
//! | inside a module reached THROUGH the map | `dep` from `@theme/chain.js` | — | uses the map |
//! | a WORKER's module | `dep` | — | **does not** — `Failed to resolve module specifier 'dep'` |
//!
//! ### `scopes`
//!
//! The scope whose prefix matches the **referrer** and is longest wins, regardless of declaration
//! order; a scope that matches but has no key for this specifier falls through to the next scope and
//! then to `imports`. A scope key need not end in `/` — an exact file URL matches that file.
//! Measured with `"/"`, `"/sub/"`, `"/sub/a/"` and `"/sub/exact.js"` declared in that jumbled order:
//! `/sub/a/importer.js` took `/sub/a/`, `/sub/importer.js` took `/sub/`, `/sub/exact.js` took its own
//! entry, `/deep/importer.js` took `/`, and a page-level `import` took `/` rather than `imports`.
//!
//! ### Several maps
//!
//! Chrome 150 accepts **more than one** import map, and one that arrives **after** a module script has
//! already run — both measured, both of which older documentation says are errors. They MERGE, and the
//! FIRST map wins every key it already has: with `{"dep": "/js/dep.js"}` followed by
//! `{"dep": "/js/other.js", "extra": …}`, `dep` stayed `/js/dep.js` and `extra` was added. Scopes merge
//! the same way, per scope and then per key. A map the page **injects** from script also applies.
//!
//! ### What a bad map does
//!
//! | what | Chrome |
//! |---|---|
//! | invalid JSON | uncaught **SyntaxError** `Failed to parse import map: invalid JSON`; the map is discarded whole |
//! | top level not an object | uncaught **TypeError** `Failed to parse import map: not an object` |
//! | `"imports"` not an object | uncaught **TypeError** `Failed to parse import map: "imports" top-level key must be a JSON object.`; the whole map goes, `scopes` included |
//! | a non-string value (`42`, `["…"]`) | the key SURVIVES with a **null** value — it is blocked, not absent |
//! | an explicit `null` value | blocked |
//! | a `"k/"` key whose value does NOT end in `/` | blocked, by the same null-value message |
//! | the empty key `""` | dropped silently |
//! | an unknown top-level key | ignored |
//! | `<script type="importmap" src=…>` | refused: the element fires `error` and nothing is mapped, text content and all |
//!
//! ### The three error messages, which are three different strings for one failure
//!
//! ```text
//! static import, uncaught   TypeError: Failed to resolve module specifier "X". <tail>
//! dynamic import(), reject  TypeError: Failed to resolve module specifier 'X'          <- no tail at all
//! import.meta.resolve()     TypeError: Failed to resolve module specifier X: <tail>
//! ```
//!
//! and `<tail>` is one of
//!
//! ```text
//! Relative references must start with either "/", "./", or "../".
//! Import Map: "X" matches with "KEY" but is blocked by a null value
//! Import Map: "X" matches with "KEY" but is blocked due to backtracking
//! ```
//!
//! "Backtracking" is the guard that makes a trailing-slash key a *directory* and not a string splice:
//! after the prefix is substituted the resulting URL must still start with the mapped URL, or the
//! resolution is refused. `"a/" -> "/js/theme/"` with `a/../evil.js` would otherwise escape to
//! `/js/evil.js`.

use url::Url;

/// One *specifier map*: normalized key → the URL it maps to, or `None` for a key that is **blocked**.
///
/// A blocked key is not an absent key, and the difference is page-visible: an absent key falls
/// through to the next scope and then to `imports`, while a blocked one ends resolution with a
/// TypeError naming the key. Chrome produces blocked entries for an explicit `null`, for a value that
/// is not a string at all, and for a `"k/"` key whose value does not end in `/` — all measured.
///
/// **Sorted by key, descending, and the order is the algorithm.** Resolution walks the entries and
/// takes the first match, so descending code-unit order is what makes the *most specific* key win:
/// an exact key sorts above every prefix key that could also match it (it is strictly longer than any
/// `"k/"` prefix of itself), and of two prefix keys that both match, one must be a prefix of the
/// other and the longer sorts first. That is HTML's sort, not an observation about it — implementing
/// "the longest one wins" instead would be a second definition of the same rule.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpecifierMap(Vec<(String, Option<Url>)>);

impl SpecifierMap {
    /// Add every key of `other` this map does not already have, and keep the sort. HTML's "merge
    /// module specifier maps": the FIRST map to claim a key keeps it (measured — see the file header).
    fn absorb(&mut self, other: SpecifierMap) {
        for (k, v) in other.0 {
            if !self.0.iter().any(|(have, _)| *have == k) {
                self.0.push((k, v));
            }
        }
        self.sort();
    }

    fn sort(&mut self) {
        self.0.sort_by(|a, b| b.0.cmp(&a.0));
    }
}

/// A document's import map: the top-level `imports`, plus the per-referrer `scopes`.
///
/// `scopes` keeps the same descending order for the same reason `SpecifierMap` does — the longest
/// matching scope prefix must be tried first, and `/sub/a/` sorts above `/sub/` above `/`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportMap {
    imports: SpecifierMap,
    scopes: Vec<(String, SpecifierMap)>,
}

/// Why a map's text could not be used, with the message Chrome reports and the JS error type it
/// reports it as — which are not the same type for the two cases, measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportMapParseError {
    /// `SyntaxError` for invalid JSON, `TypeError` for a JSON value of the wrong shape.
    pub error_type: &'static str,
    pub message: String,
}

impl ImportMapParseError {
    fn syntax(msg: &str) -> Self {
        Self { error_type: "SyntaxError", message: format!("Failed to parse import map: {msg}") }
    }
    fn ty(msg: &str) -> Self {
        Self { error_type: "TypeError", message: format!("Failed to parse import map: {msg}") }
    }
}

/// Why a specifier did not resolve. Carries the pieces the three message shapes need rather than a
/// pre-rendered string, because the same failure is worded three different ways depending on which
/// surface asked (see the file header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A bare specifier with no mapping anywhere.
    Bare { specifier: String },
    /// A key matched and its value is null — explicitly, or because the value was unusable.
    Blocked { specifier: String, key: String },
    /// A trailing-slash key matched and the substitution climbed back out of the mapped directory.
    Backtracking { specifier: String, key: String },
}

impl ResolveError {
    /// The clause after the colon or the full stop. Identical in all three shapes.
    pub fn tail(&self) -> String {
        match self {
            Self::Bare { .. } => r#"Relative references must start with either "/", "./", or "../"."#.to_string(),
            Self::Blocked { specifier, key } => {
                format!(r#"Import Map: "{specifier}" matches with "{key}" but is blocked by a null value"#)
            }
            Self::Backtracking { specifier, key } => {
                format!(r#"Import Map: "{specifier}" matches with "{key}" but is blocked due to backtracking"#)
            }
        }
    }

    fn specifier(&self) -> &str {
        match self {
            Self::Bare { specifier } | Self::Blocked { specifier, .. } | Self::Backtracking { specifier, .. } => specifier,
        }
    }

    /// The message a **static** `import` reports uncaught. Double quotes, then the tail.
    pub fn static_message(&self) -> String {
        format!(r#"Failed to resolve module specifier "{}". {}"#, self.specifier(), self.tail())
    }

    /// The message a **dynamic** `import()` rejects with. Single quotes, and NO tail — Chrome really
    /// does drop the explanation on this one path, measured on every ending.
    pub fn dynamic_message(&self) -> String {
        format!("Failed to resolve module specifier '{}'", self.specifier())
    }

    /// The message **`import.meta.resolve`** throws. No quotes, a colon, then the tail.
    pub fn meta_resolve_message(&self) -> String {
        format!("Failed to resolve module specifier {}: {}", self.specifier(), self.tail())
    }
}

/// HTML's *parse a URL-like module specifier*: `/`, `./` and `../` resolve against `base`; anything
/// else must be an absolute URL on its own. A bare specifier is neither and answers `None`.
///
/// `//host/path` takes the first branch — it starts with `/` — and `Url::join` gives it the base's
/// scheme. That is not a curiosity: two of the six corpus rows write every value in their map
/// protocol-relative, so a `parse_url_like` that only handled `/`, `./` and `../` after a scheme
/// would drop those maps entirely.
fn parse_url_like(specifier: &str, base: &Url) -> Option<Url> {
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        return base.join(specifier).ok();
    }
    Url::parse(specifier).ok()
}

/// HTML's *normalize a specifier key*. The empty key is dropped (Chrome ignores it); any key that
/// [`parse_url_like`] resolves becomes its SERIALIZED URL, and only a key that does not parse at all
/// stays the raw string.
///
/// **It is the serialization, not the spelling, and the difference is reachable.** Measured on
/// Chrome 150: a key `"https://example.invalid"` matches an import of `"https://example.invalid/"`
/// (the serializer adds the path), `"HTTPS://EXAMPLE2.INVALID/X.JS"` matches
/// `"https://example2.invalid/X.JS"` (scheme and host lowercase, the path does not), and
/// `"https://example3.invalid/a/../b.js"` matches `"https://example3.invalid/b.js"` (dot segments
/// are removed). An earlier draft of this function normalized only the three RELATIVE prefixes and
/// left absolute-URL keys verbatim — which matches on the spelling a page happens to use and misses
/// on the other three rows above.
fn normalize_specifier_key(key: &str, base: &Url) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    // A bare specifier is exactly what does NOT parse: `react`, `react/jsx-runtime`, `@theme/`.
    Some(parse_url_like(key, base).map(|u| u.to_string()).unwrap_or_else(|| key.to_string()))
}

/// HTML's *parse a specifier map*. Every entry that cannot be used becomes a BLOCKED key rather than
/// a missing one — Chrome keeps `"numval": 42` and `"arrval": ["/js/dep.js"]` as null values, and a
/// page importing them is told the key matched and was blocked, not that it was never mapped.
fn parse_specifier_map(obj: &serde_json::Map<String, serde_json::Value>, base: &Url) -> SpecifierMap {
    let mut out = SpecifierMap::default();
    for (key, value) in obj {
        let Some(k) = normalize_specifier_key(key, base) else { continue };
        let v = value.as_str().and_then(|s| parse_url_like(s, base)).filter(|u| {
            // A key that names a directory must map to one: `"k/"` whose value does not end in `/`
            // is blocked, measured, and blocking it is what keeps the prefix substitution below from
            // splicing strings together.
            //
            // THE TEST IS ON THE KEY AS WRITTEN, NOT ON ITS NORMALIZED FORM, and the difference is
            // reachable: `"https://example.invalid"` normalizes to `https://example.invalid/`, which
            // ENDS IN A SLASH the page never typed, so testing the normalized key would block a
            // mapping Chrome 150 honours (measured, probe `p26`). Resolution below does use the
            // normalized key for its prefix test — that is HTML's asymmetry, not a slip.
            !key.ends_with('/') || u.as_str().ends_with('/')
        });
        out.0.push((k, v));
    }
    out.sort();
    out
}

impl ImportMap {
    /// Parse one `<script type="importmap">`'s text, with `base` the document's base URL at the time
    /// the element is processed — which `<base href>` moves, measured: under `<base href="/js/theme/">`
    /// a value of `"./section.js"` resolves to `/js/theme/section.js`.
    pub fn parse(text: &str, base: &Url) -> Result<Self, ImportMapParseError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| ImportMapParseError::syntax("invalid JSON"))?;
        let serde_json::Value::Object(obj) = value else {
            return Err(ImportMapParseError::ty("not an object"));
        };
        let imports = match obj.get("imports") {
            None | Some(serde_json::Value::Null) => SpecifierMap::default(),
            Some(serde_json::Value::Object(m)) => parse_specifier_map(m, base),
            // The WHOLE map is discarded, `scopes` included — measured: a page whose `imports` was an
            // array lost a perfectly well-formed `scopes` entry too.
            Some(_) => return Err(ImportMapParseError::ty(r#""imports" top-level key must be a JSON object."#)),
        };
        let mut scopes: Vec<(String, SpecifierMap)> = Vec::new();
        match obj.get("scopes") {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Object(m)) => {
                for (prefix, inner) in m {
                    let serde_json::Value::Object(inner) = inner else {
                        return Err(ImportMapParseError::ty(r#"the value of a scope must be a JSON object."#));
                    };
                    // A scope prefix that is not a URL at all is dropped; the rest of the map stands.
                    let Ok(url) = base.join(prefix) else { continue };
                    scopes.push((url.to_string(), parse_specifier_map(inner, base)));
                }
            }
            Some(_) => return Err(ImportMapParseError::ty(r#""scopes" top-level key must be a JSON object."#)),
        }
        // Unknown top-level keys are ignored (measured with `"wat": 1`).
        scopes.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(Self { imports, scopes })
    }

    /// HTML's *merge existing and new import maps*, FIRST HALF: a new map may not re-point a
    /// specifier that has ALREADY RESOLVED. Every such key is dropped from the arriving map before
    /// anything is merged.
    ///
    /// Measured on Chrome 150 (probes `p27`-`p29`, 2026-09-21), and every row of it is load-bearing:
    ///
    /// ```text
    ///   import("./js/r1.js") then a map keyed "./js/r1.js"   the key is DROPPED
    ///   ... then a map keyed "/js/r1.js"                     ALSO dropped -- it is the SERIALIZED
    ///                                                        specifier that is remembered, not the
    ///                                                        spelling the page used
    ///   ... then a map keyed "./js/"                         dropped -- a prefix key that COVERS a
    ///                                                        resolved specifier goes too, even
    ///                                                        though `./js/plain.js` never resolved
    ///   a key that never resolved                            applies
    ///   after `import.meta.resolve(x)` alone, no fetch       x is in the set: the key is dropped
    ///   after a resolution that THREW                        not in the set: the key applies
    ///   a BARE specifier resolved inside /sub/               blocks the TOP-LEVEL `imports` key,
    ///                                                        because a bare specifier's normalized
    ///                                                        form does not depend on a base
    /// ```
    ///
    /// The base URL is remembered too, and only `scopes` consults it: a scope's key is dropped only
    /// for records resolved UNDER that scope, which is what keeps `/sub/`'s entries alive when the
    /// document resolved the same string at the top.
    fn drop_already_resolved(&mut self, resolved: &ResolvedModuleSet) {
        self.imports.0.retain(|(key, _)| !resolved.0.iter().any(|(spec, _)| covers(key, spec)));
        for (prefix, map) in &mut self.scopes {
            map.0.retain(|(key, _)| {
                !resolved.0.iter().any(|(spec, base)| base.starts_with(prefix.as_str()) && covers(key, spec))
            });
        }
    }

    /// HTML's *merge existing and new import maps*, SECOND HALF: the map already installed keeps
    /// every key it has, and the new one contributes only what is missing — per scope, and then per
    /// key inside it.
    pub fn merge(&mut self, other: ImportMap) {
        self.imports.absorb(other.imports);
        for (prefix, map) in other.scopes {
            match self.scopes.iter_mut().find(|(have, _)| *have == prefix) {
                Some((_, mine)) => mine.absorb(map),
                None => self.scopes.push((prefix, map)),
            }
        }
        self.scopes.sort_by(|a, b| b.0.cmp(&a.0));
    }

    /// HTML's *resolve a module specifier*, with `referrer` the base URL of the script doing the
    /// importing.
    ///
    /// `Ok(None)` is "this map has nothing to say" — the caller resolves the specifier the ordinary
    /// way. It is distinct from `Err`, which is a resolution that ENDED here.
    pub fn resolve(&self, specifier: &str, referrer: &Url) -> Result<Option<Url>, ResolveError> {
        let as_url = parse_url_like(specifier, referrer);
        // What the map's keys are matched against: the serialized URL for a URL-like specifier, so
        // that `"./relative-a.js"` in a map matches a page's `import "./relative-a.js"`, and the raw
        // string for a bare one.
        let normalized = as_url.as_ref().map(|u| u.to_string()).unwrap_or_else(|| specifier.to_string());
        let referrer_str = referrer.as_str();
        for (prefix, map) in &self.scopes {
            let applies = prefix == referrer_str || (prefix.ends_with('/') && referrer_str.starts_with(prefix.as_str()));
            if !applies {
                continue;
            }
            // A scope that matches but has no key for this specifier FALLS THROUGH, to the next scope
            // and then to `imports` — measured, and the reason this is a `continue` and not a return.
            if let Some(hit) = resolve_in(&normalized, map)? {
                return Ok(Some(hit));
            }
        }
        if let Some(hit) = resolve_in(&normalized, &self.imports)? {
            return Ok(Some(hit));
        }
        match as_url {
            Some(_) => Ok(None),
            None => Err(ResolveError::Bare { specifier: specifier.to_string() }),
        }
    }
}

/// Would a map keyed `key` have had anything to say about the already-resolved `spec`? The same two
/// tests resolution itself uses — an exact key, or a trailing-slash key that is a prefix of it.
fn covers(key: &str, spec: &str) -> bool {
    key == spec || (key.ends_with('/') && spec.starts_with(key))
}

/// HTML's *resolved module set*: the normalized specifier of every SUCCESSFUL resolution, with the
/// base URL it was resolved against. A failed resolution is not a member (measured), which is why
/// this is written from the `Ok` arm only.
#[derive(Debug, Default)]
pub struct ResolvedModuleSet(std::collections::HashSet<(String, String)>);

/// HTML's *resolve an imports match*, over one already-sorted specifier map.
fn resolve_in(normalized: &str, map: &SpecifierMap) -> Result<Option<Url>, ResolveError> {
    for (key, value) in &map.0 {
        if key == normalized {
            return match value {
                Some(u) => Ok(Some(u.clone())),
                None => Err(ResolveError::Blocked { specifier: normalized.to_string(), key: key.clone() }),
            };
        }
        if key.ends_with('/') && normalized.starts_with(key.as_str()) {
            let Some(base) = value else {
                return Err(ResolveError::Blocked { specifier: normalized.to_string(), key: key.clone() });
            };
            let after = &normalized[key.len()..];
            // BACKTRACKING. `"a/" -> "/js/theme/"` must mean the directory `/js/theme/`, so
            // `a/../evil.js` may not come out as `/js/evil.js`. HTML checks the resolved URL still
            // starts with the mapped one, and so does Chrome, with a message that names both.
            let Ok(url) = base.join(after) else {
                return Err(ResolveError::Backtracking { specifier: normalized.to_string(), key: key.clone() });
            };
            if !url.as_str().starts_with(base.as_str()) {
                return Err(ResolveError::Backtracking { specifier: normalized.to_string(), key: key.clone() });
            }
            return Ok(Some(url));
        }
    }
    Ok(None)
}

/// A document's import maps as they accumulate, shared by `Rc` between the places that can install one
/// (the parser's script pass, and the inserted-script path) and the module loader, which reads it with
/// no `OpState` and no scope of its own.
///
/// It is NOT reset by `merge`: HTML has one import map per document and a navigation replaces it,
/// which is [`ImportMapStore::clear`]'s job.
#[derive(Debug, Default)]
pub struct ImportMapStore {
    map: Option<ImportMap>,
    /// Every specifier this document has already resolved. It is read when a map ARRIVES, never
    /// when one is used, and it is what stops a second map moving a module a page already has.
    resolved: ResolvedModuleSet,
}

impl ImportMapStore {
    /// Install one map's text. `Ok(())` even when the text contributed nothing; `Err` carries the
    /// message and JS error type the page must be told about.
    pub fn install(&mut self, text: &str, base: &Url) -> Result<(), ImportMapParseError> {
        let mut parsed = ImportMap::parse(text, base)?;
        parsed.drop_already_resolved(&self.resolved);
        match &mut self.map {
            Some(existing) => existing.merge(parsed),
            None => self.map = Some(parsed),
        }
        Ok(())
    }

    /// Resolve through the document's map, if it has one. `Ok(None)` means "nothing to say".
    ///
    /// `&mut` because a successful resolution JOINS THE RESOLVED MODULE SET — see
    /// [`ImportMap::drop_already_resolved`]. That is the only write, and leaving it out is not a
    /// smaller implementation of import maps but a different one: a page whose second map re-points
    /// a module it already loaded would get the second answer where Chrome keeps the first.
    pub fn resolve(&mut self, specifier: &str, referrer: &Url) -> Result<Option<Url>, ResolveError> {
        let out = match &self.map {
            Some(m) => m.resolve(specifier, referrer),
            // NO MAP AT ALL is not the same as a map with no entry: a document without an import map
            // still refuses a bare specifier, and with the same message.
            None => match parse_url_like(specifier, referrer) {
                Some(_) => Ok(None),
                None => Err(ResolveError::Bare { specifier: specifier.to_string() }),
            },
        };
        if out.is_ok() {
            // THE NORMALIZED SPECIFIER, not the spelling: `./js/r1.js` and `/js/r1.js` are one
            // record, and a later map keyed either way is dropped. Measured.
            let normalized =
                parse_url_like(specifier, referrer).map(|u| u.to_string()).unwrap_or_else(|| specifier.to_string());
            self.resolved.0.insert((normalized, referrer.to_string()));
        }
        out
    }

    /// A navigation starts with no import map — and with nothing resolved.
    pub fn clear(&mut self) {
        self.map = None;
        self.resolved = ResolvedModuleSet::default();
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_none()
    }
}

/// Keyed by nothing — a plain newtype so the shared handle can live in `OpState`, which is keyed by
/// type. The loader holds the same `Rc`, so a map installed from a scope is visible to a resolution
/// that happens with no scope at all.
#[derive(Clone, Default)]
pub struct ImportMaps(pub std::rc::Rc<std::cell::RefCell<ImportMapStore>>);

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("http://127.0.0.1:8931/p.html").unwrap()
    }

    fn map(text: &str) -> ImportMap {
        ImportMap::parse(text, &base()).expect("parses")
    }

    fn got(m: &ImportMap, spec: &str, referrer: &str) -> String {
        let r = Url::parse(referrer).unwrap();
        match m.resolve(spec, &r) {
            Ok(Some(u)) => u.to_string(),
            Ok(None) => "PASS-THROUGH".to_string(),
            Err(e) => format!("THROW {}", e.tail()),
        }
    }

    /// The `imports` rows of the Chrome 150 transcript in the file header, asserted against the
    /// algorithm rather than against a browser, so the rule is testable without a browser present.
    #[test]
    fn imports_resolve_the_way_chrome_150_resolves_them() {
        let m = map(
            r#"{"imports":{
                 "dep":"/js/dep.js",
                 "@theme/":"/js/theme/",
                 "@theme/section":"/js/other.js",
                 "nonslash/":"/js/theme",
                 "./relative-a.js":"/js/root-a.js",
                 "":"/js/evil.js"
               }}"#,
        );
        let page = "http://127.0.0.1:8931/p.html";
        assert_eq!(got(&m, "dep", page), "http://127.0.0.1:8931/js/dep.js");
        assert_eq!(got(&m, "@theme/nested/deep.js", page), "http://127.0.0.1:8931/js/theme/nested/deep.js");
        assert_eq!(got(&m, "@theme/section", page), "http://127.0.0.1:8931/js/other.js", "an exact key beats a prefix key");
        assert_eq!(got(&m, "./relative-a.js", page), "http://127.0.0.1:8931/js/root-a.js", "a URL-like specifier is remapped");
        assert_eq!(got(&m, "./js/plain.js", page), "PASS-THROUGH", "an unmapped URL-like specifier resolves normally");
        assert_eq!(
            got(&m, "nonslash/section.js", page),
            r#"THROW Import Map: "nonslash/section.js" matches with "nonslash/" but is blocked by a null value"#,
            "a trailing-slash key whose value is not a directory is BLOCKED, not dropped"
        );
        assert_eq!(
            got(&m, "anything", page),
            r#"THROW Relative references must start with either "/", "./", or "../"."#,
            "the empty key maps nothing"
        );
    }

    /// The longest matching prefix wins whatever order the keys were written in, and the substitution
    /// may not climb out of the directory it was given.
    #[test]
    fn the_longest_prefix_wins_and_cannot_backtrack() {
        let m = map(r#"{"imports":{"@theme/":"/js/theme/","@theme/nested/":"/js/alt/","a/":"/js/theme/"}}"#);
        let page = "http://127.0.0.1:8931/p.html";
        assert_eq!(got(&m, "@theme/index.js", page), "http://127.0.0.1:8931/js/theme/index.js");
        assert_eq!(got(&m, "@theme/nested/deep.js", page), "http://127.0.0.1:8931/js/alt/deep.js");
        assert_eq!(got(&m, "@theme/", page), "http://127.0.0.1:8931/js/theme/");
        assert_eq!(
            got(&m, "a/../evil.js", page),
            r#"THROW Import Map: "a/../evil.js" matches with "a/" but is blocked due to backtracking"#
        );
    }

    /// A non-string value is a BLOCKED key. The distinction costs nothing to implement and is the
    /// difference between "the page is told its key was refused" and "the page is told there is no
    /// such key", which are different messages in Chrome.
    #[test]
    fn a_value_that_is_not_a_string_blocks_its_key() {
        let m = map(r#"{"imports":{"numval":42,"arrval":["/js/dep.js"],"nullish":null,"ok":"/js/dep.js"}}"#);
        let page = "http://127.0.0.1:8931/p.html";
        for k in ["numval", "arrval", "nullish"] {
            assert_eq!(got(&m, k, page), format!(r#"THROW Import Map: "{k}" matches with "{k}" but is blocked by a null value"#));
        }
        assert_eq!(got(&m, "ok", page), "http://127.0.0.1:8931/js/dep.js", "a sibling entry is unaffected");
    }

    /// The longest matching SCOPE wins, a scope that matches but has no key falls through, and the
    /// order the scopes were declared in does not matter.
    #[test]
    fn scopes_are_tried_most_specific_first_and_fall_through() {
        let m = map(
            r#"{"imports":{"dep":"/js/imports.js"},
                "scopes":{"/sub/":{"dep":"/js/sub.js"},
                          "/sub/a/":{"dep":"/js/deep.js"},
                          "/sub/exact.js":{"dep":"/js/exact.js"},
                          "/":{"dep":"/js/root.js"},
                          "/fall/":{"other":"/js/other.js"}}}"#,
        );
        let at = |u: &str| got(&m, "dep", u);
        assert_eq!(at("http://127.0.0.1:8931/sub/a/importer.js"), "http://127.0.0.1:8931/js/deep.js");
        assert_eq!(at("http://127.0.0.1:8931/sub/importer.js"), "http://127.0.0.1:8931/js/sub.js");
        assert_eq!(at("http://127.0.0.1:8931/sub/exact.js"), "http://127.0.0.1:8931/js/exact.js");
        assert_eq!(at("http://127.0.0.1:8931/deep/importer.js"), "http://127.0.0.1:8931/js/root.js");
        assert_eq!(
            at("http://127.0.0.1:8931/fall/importer.js"),
            "http://127.0.0.1:8931/js/root.js",
            "a scope with no entry for this key falls through to the next scope"
        );
    }

    /// Two maps merge, and the FIRST one keeps every key it already had.
    #[test]
    fn a_second_map_adds_but_never_overwrites() {
        let mut store = ImportMapStore::default();
        store.install(r#"{"imports":{"dep":"/js/dep.js"},"scopes":{"/sub/":{"dep":"/js/a.js"}}}"#, &base()).unwrap();
        store
            .install(
                r#"{"imports":{"dep":"/js/other.js","extra":"/js/other.js"},
                    "scopes":{"/sub/":{"dep":"/js/b.js","other":"/js/other.js"},"/deep/":{"dep":"/js/b.js"}}}"#,
                &base(),
            )
            .unwrap();
        let mut at = |s: &str, u: &str| match store.resolve(s, &Url::parse(u).unwrap()) {
            Ok(Some(x)) => x.to_string(),
            other => format!("{other:?}"),
        };
        assert_eq!(at("dep", "http://127.0.0.1:8931/p.html"), "http://127.0.0.1:8931/js/dep.js");
        assert_eq!(at("extra", "http://127.0.0.1:8931/p.html"), "http://127.0.0.1:8931/js/other.js");
        assert_eq!(at("dep", "http://127.0.0.1:8931/sub/i.js"), "http://127.0.0.1:8931/js/a.js");
        assert_eq!(at("dep", "http://127.0.0.1:8931/deep/i.js"), "http://127.0.0.1:8931/js/b.js");
    }

    /// The two parse failures are two different JS error types, and both discard the whole map.
    #[test]
    fn a_bad_map_reports_the_type_chrome_reports() {
        let e = ImportMap::parse(r#"{ "imports": { "dep": "/js/dep.js", }"#, &base()).unwrap_err();
        assert_eq!(e.error_type, "SyntaxError");
        assert_eq!(e.message, "Failed to parse import map: invalid JSON");
        let e = ImportMap::parse("[1,2,3]", &base()).unwrap_err();
        assert_eq!(e.error_type, "TypeError");
        assert_eq!(e.message, "Failed to parse import map: not an object");
        let e = ImportMap::parse(r#"{"imports":[1,2],"scopes":{"/":{"ok":"/js/dep.js"}}}"#, &base()).unwrap_err();
        assert_eq!(e.error_type, "TypeError");
        assert_eq!(e.message, r#"Failed to parse import map: "imports" top-level key must be a JSON object."#);
    }

    /// The map's values resolve against the DOCUMENT BASE, which `<base href>` moves.
    #[test]
    fn values_resolve_against_the_document_base() {
        let b = Url::parse("http://127.0.0.1:8931/js/theme/").unwrap();
        let m = ImportMap::parse(r#"{"imports":{"dep":"./section.js","up":"../dep.js"}}"#, &b).unwrap();
        assert_eq!(got(&m, "dep", "http://127.0.0.1:8931/js/theme/"), "http://127.0.0.1:8931/js/theme/section.js");
        assert_eq!(got(&m, "up", "http://127.0.0.1:8931/js/theme/"), "http://127.0.0.1:8931/js/dep.js");
    }

    /// The three shapes of one failure. The dynamic one really does drop the explanation.
    #[test]
    fn one_failure_is_worded_three_ways() {
        let e = ResolveError::Bare { specifier: "@vendor/thing".into() };
        assert_eq!(
            e.static_message(),
            r#"Failed to resolve module specifier "@vendor/thing". Relative references must start with either "/", "./", or "../"."#
        );
        assert_eq!(e.dynamic_message(), "Failed to resolve module specifier '@vendor/thing'");
        assert_eq!(
            e.meta_resolve_message(),
            r#"Failed to resolve module specifier @vendor/thing: Relative references must start with either "/", "./", or "../"."#
        );
    }

    /// A key is matched by its SERIALIZATION, so a page's spelling of the same URL finds it. All four
    /// rows measured on Chrome 150 (probe `p26`), and all four miss if only relative keys are
    /// normalized.
    #[test]
    fn a_url_key_is_matched_by_its_serialization() {
        let m = map(
            r#"{"imports":{
                 "https://example.invalid":"/js/dep.js",
                 "HTTPS://EXAMPLE2.INVALID/X.JS":"/js/other.js",
                 "https://example3.invalid/a/../b.js":"/js/plain.js",
                 "/pathkey/../flat.js":"/js/root-a.js"
               }}"#,
        );
        let page = "http://127.0.0.1:8931/p.html";
        assert_eq!(got(&m, "https://example.invalid/", page), "http://127.0.0.1:8931/js/dep.js");
        assert_eq!(got(&m, "https://example.invalid", page), "http://127.0.0.1:8931/js/dep.js");
        assert_eq!(got(&m, "https://example2.invalid/X.JS", page), "http://127.0.0.1:8931/js/other.js");
        assert_eq!(got(&m, "https://example3.invalid/b.js", page), "http://127.0.0.1:8931/js/plain.js");
        assert_eq!(got(&m, "/flat.js", page), "http://127.0.0.1:8931/js/root-a.js");
    }

    /// A second map may not re-point what has already resolved. Every row measured on Chrome 150
    /// (probes `p27`-`p29`); the table is on `drop_already_resolved`.
    #[test]
    fn a_second_map_cannot_move_a_specifier_that_already_resolved() {
        let mut store = ImportMapStore::default();
        let page = Url::parse("http://127.0.0.1:8931/p.html").unwrap();
        let sub = Url::parse("http://127.0.0.1:8931/sub/m.js").unwrap();
        store.install(r#"{"scopes":{"/sub/":{"scopedbare":"/js/dep.js"}}}"#, &base()).unwrap();
        // Resolve four things: a relative URL, a bare one from a SUBDIRECTORY, one that FAILS, and
        // one only `import.meta.resolve` ever asked about (this method is that call's path too).
        assert_eq!(store.resolve("./js/r1.js", &page), Ok(None));
        assert!(store.resolve("scopedbare", &sub).is_ok());
        assert!(store.resolve("failed-bare", &page).is_err());
        assert_eq!(store.resolve("./js/meta-only.js", &page), Ok(None));
        store
            .install(
                r#"{"imports":{
                     "/js/r1.js":"/js/other.js",
                     "/js/":"/js/alt/",
                     "scopedbare":"/js/other.js",
                     "failed-bare":"/js/other.js",
                     "./js/meta-only.js":"/js/other.js",
                     "./js/never-resolved.js":"/js/other.js"
                   }}"#,
                &base(),
            )
            .unwrap();
        let at = |s: &str| match store.map.as_ref().unwrap().resolve(s, &page) {
            Ok(Some(u)) => u.to_string(),
            Ok(None) => "PASS-THROUGH".to_string(),
            Err(e) => format!("THROW {}", e.tail()),
        };
        assert_eq!(at("/js/r1.js"), "PASS-THROUGH", "the ABSOLUTE spelling of a resolved relative specifier is dropped");
        assert_eq!(at("/js/plain.js"), "PASS-THROUGH", "a prefix key covering a resolved specifier is dropped whole");
        assert_eq!(at("./js/meta-only.js"), "PASS-THROUGH", "a resolution with no fetch still counts");
        assert!(
            matches!(at("scopedbare").as_str(), s if s.starts_with("THROW")),
            "a bare specifier resolved in a subdirectory blocks the TOP-LEVEL key: {}",
            at("scopedbare")
        );
        assert_eq!(at("failed-bare"), "http://127.0.0.1:8931/js/other.js", "a FAILED resolution does not block");
        assert_eq!(at("./js/never-resolved.js"), "http://127.0.0.1:8931/js/other.js", "an untouched key applies");
    }

    /// A document with NO map still refuses a bare specifier, and still lets a relative one past.
    #[test]
    fn no_map_refuses_a_bare_specifier_and_passes_a_relative_one() {
        let mut store = ImportMapStore::default();
        let r = base();
        assert!(matches!(store.resolve("react/jsx-runtime", &r), Err(ResolveError::Bare { .. })));
        assert_eq!(store.resolve("./x.js", &r), Ok(None));
    }
}
