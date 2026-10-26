//! The bundle's recorded CODEC GRID, reduced to the table the codec grammar answers from.
//!
//! # What this is for
//!
//! A bundle swap used to change what the browser SAID about itself and not what it could PLAY. The
//! container/codec grid was ONE compile-time constant — `generated_media_codecs.rs`, derived by
//! `tools/sync/sync.py` from the shipped bundle and only the shipped bundle — so the three members
//! that answer "can you play this?" answered Brave 151's grid whichever device was being worn.
//!
//! Measured over the four captures in this tree, leaf by leaf over their 9,104-answer grids:
//!
//! ```text
//!                       answers differing from the shipped bundle (chrome-150)
//!   chrome-150             0
//!   edge-151              16    the audio/3gpp family, "" -> maybe / probably
//!   opera-135            274    H.264 + AAC, probably -> "" ; 60 of them MediaSource /
//!                               MediaRecorder booleans, true -> false
//! ```
//!
//! Opera's Linux build genuinely ships without the proprietary H.264/AAC decoders, so those 274 are
//! the BROWSER and not the host — and an engine wearing Opera was telling every page it could play
//! `avc1.42E01E`. That is the largest single hard-coded difference this goal has measured.
//!
//! # It is a CLAIM, not a capability (ruling R7, said here so a green gate is not misread)
//!
//! This engine has no demuxer and no decoder. `readyState` is `HAVE_NOTHING` everywhere and `play()`
//! rejects, before this change and after it. `canPlayType` has always been the recorded browser's
//! answer rather than a statement about this process; all that changes here is WHICH recorded
//! browser's. Nothing below moves the engine one step toward decoding a frame.
//!
//! # Why the shipped bundle's grid is EMBEDDED and the rest are read from disk
//!
//! Exactly [`crate::surface`]'s argument, and the same mechanics: [`crate::SHIPPED_VALUES`] and
//! [`crate::BUNDLE_MANIFEST`] are `include_str!` so a build with nothing selected has NO RUNTIME FILE
//! DEPENDENCY, and making the grid a disk read for the default persona would have quietly traded that
//! away. So the shipped bundle's `codecs.json` is embedded on the same terms as its values, and a
//! bundle named at runtime reads its own out of its own directory on the same terms as its
//! `values.json`.
//!
//! # Why the derivation moved here, and why `generated_media_codecs.rs` is gone
//!
//! It could not stay a generated `.rs` and also be per-bundle. Four bundles' grids compiled into one
//! `core/` file would have put four capture ids — `opera-135-…`, `edge-151-…` — into engine source,
//! which is exactly the browser-family literal `scripts/persona_literals.py` forbids there and
//! `scripts/persona_literals.py` ratchets at 0. So the artifact became per-bundle, the reader moved
//! to the crate that is allowed to spell a bundle's name, and the generated file was deleted in the
//! same commit that made this the sole owner.
//!
//! The projection below — "which spellings did each member accept for each container" — is the one
//! `codecs.mjs` used to do on the way to that `.rs`. It is done ONCE, here, from the committed grid;
//! `codecs.mjs` keeps the half nothing else can do, which is knowing what the 9,104 questions ARE.
//!
//! # Cost, stated
//!
//! `codecs.json` is 590 KB against the 9 KB `.rs` it replaces, and it is parsed once per process per
//! bundle ([`cache`]) — not once per context, and not once per `canPlayType` call. The grammar
//! memoises its own reduced query table on top of this, keyed by capture id, for the same reason.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use spec::{CodecContainer, CodecGrid};

/// The shipped bundle's answer grid, embedded beside its values for the reason in the module doc.
pub const SHIPPED_CODECS: &str =
    include_str!("../bundles/chrome-152-linux-20261016T1022/codecs.json");

/// The committed grid exactly as `codecs.mjs --raw` writes it: `kind|what :: part :: part` -> `{v}`.
///
/// Only the two fields the derivation asks for; serde skips the rest without allocating.
#[derive(serde::Deserialize)]
struct RawGrid {
    #[serde(default)]
    source: String,
    #[serde(default)]
    answers: BTreeMap<String, RawAnswer>,
}

#[derive(serde::Deserialize)]
struct RawAnswer {
    #[serde(default)]
    v: serde_json::Value,
}

/// Which member a key's first field names. The grid asks every question of all four.
fn member_of(kind: &str) -> Option<char> {
    match kind {
        "v" | "a" | "s" | "r" => kind.chars().next(),
        _ => None,
    }
}

/// Split `"<member>|<what> :: <part> :: <part>"` into its member, its shape and its parts.
///
/// The separator is the one `codecs.mjs` writes and nothing else appears in a container mime or a
/// codec spelling, so this is a parse and not a guess. A key that does not have this shape is
/// ignored rather than fatal: a future rig may ask a question this derivation has no use for, and
/// refusing to load the whole grid over one would take the device's codecs away entirely.
fn split_key(key: &str) -> Option<(char, &str, Vec<&str>)> {
    let (head, rest) = key.split_once('|')?;
    let member = member_of(head)?;
    let mut parts = rest.split(" :: ");
    let what = parts.next()?;
    Some((member, what, parts.collect()))
}

fn as_answer_string(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// A codec spelling's FAMILY: the text before its first `.` (`hev1.1.6.L93.B0` -> `hev1`). The same
/// split the grammar's `family_of` makes, duplicated here only to group a spelling with its family --
/// not to parse the profile, which stays the grammar's.
fn family_prefix(c: &str) -> &str {
    match c.find('.') {
        Some(i) => &c[..i],
        None => c,
    }
}

/// Derive one device's container table from its recorded grid.
///
/// THE QUESTION LIST IS THE GRID'S OWN. Which containers exist and which codec spellings were asked
/// are read off the keys, never from a list kept here — so a bundle captured with a wider sweep
/// widens the table by being dropped in, and this file cannot drift from `codecs.mjs`'s questions
/// because it does not restate them (derived, never counted).
fn derive(capture_id: &str, json: &str) -> Result<CodecGrid, String> {
    let raw: RawGrid = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if raw.answers.is_empty() {
        return Err("the grid records no answers".to_string());
    }
    let mut containers: BTreeMap<String, CodecContainer> = BTreeMap::new();
    // BASE FIRST, so every container the sweep asked about exists before the pairs land on it. A
    // `pair` whose container has no `base` row would otherwise create one by side effect, and the
    // container set would then depend on iteration order.
    for (key, ans) in &raw.answers {
        let Some((member, "base", parts)) = split_key(key) else { continue };
        let [container] = parts[..] else { continue };
        let row = containers.entry(container.to_string()).or_default();
        match member {
            'v' => row.base_video = as_answer_string(&ans.v),
            'a' => row.base_audio = as_answer_string(&ans.v),
            's' => row.base_ms = ans.v.as_bool().unwrap_or(false),
            _ => row.base_mr = ans.v.as_bool().unwrap_or(false),
        }
    }
    let mut asked = std::collections::BTreeSet::new();
    // Recorder spellings each container REJECTED, kept until the accepted sets are complete so the
    // discriminating ones (family accepted elsewhere) can be told from the rest. See `mr_reject`.
    let mut mr_false: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, ans) in &raw.answers {
        let Some((member, "pair", parts)) = split_key(key) else { continue };
        let [container, codec] = parts[..] else { continue };
        if member == 'v' {
            asked.insert(codec.to_string());
        }
        let Some(row) = containers.get_mut(container) else { continue };
        // Only what the device ACCEPTED goes in. "probably" and `true` are the accepting answers;
        // "maybe" is the container speaking, not the codec, and never enters a codec list.
        let accepted = match member {
            'v' | 'a' => ans.v.as_str() == Some("probably"),
            _ => ans.v.as_bool() == Some(true),
        };
        if !accepted {
            // A recorder rejection is the only "false" the grammar can disagree with (its lenient
            // family match would accept it); the other three members read their accepted sets only.
            if member == 'r' {
                mr_false.entry(container.to_string()).or_default().push(codec.to_string());
            }
            continue;
        }
        let list = match member {
            'v' => &mut row.video,
            'a' => &mut row.audio,
            's' => &mut row.ms,
            _ => &mut row.mr,
        };
        list.push(codec.to_string());
    }
    // A rejected recorder spelling matters only when its FAMILY is accepted through another spelling
    // -- then the lenient family match would wrongly accept it, and the recording says no. Carry
    // exactly those (prefix before the first `.` is the family, a parser fact `codecs.mjs` shares).
    for (container, rejects) in mr_false {
        let Some(row) = containers.get_mut(&container) else { continue };
        let accepted_families: std::collections::BTreeSet<&str> =
            row.mr.iter().map(|c| family_prefix(c)).collect();
        row.mr_reject = rejects.into_iter().filter(|c| accepted_families.contains(family_prefix(c))).collect();
    }
    // A container nothing answers for at all is not worth a row — `text/plain`, `/mp4`, the empty
    // type. Dropping them here is what makes "the table has no row for this type" mean "this device
    // does not demux it", which is the answer the grammar returns for an unknown container.
    containers.retain(|_, c| {
        !c.base_video.is_empty()
            || !c.base_audio.is_empty()
            || c.base_ms
            || c.base_mr
            || !c.video.is_empty()
            || !c.audio.is_empty()
            || !c.ms.is_empty()
            || !c.mr.is_empty()
    });
    let id = if raw.source.is_empty() { capture_id.to_string() } else { raw.source };
    Ok(CodecGrid { capture_id: id, containers, asked: asked.into_iter().collect() })
}

type Cache = Mutex<std::collections::HashMap<String, Option<Arc<CodecGrid>>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// The codec grid of the bundle with this capture id — derived at most once per process.
///
/// `dir` is the bundle's directory when the persona was loaded from disk, `None` for the embedded
/// one. A grid that is missing or malformed is `None`, not a panic, on the same terms as a surface
/// that does not parse: the persona still describes a device, the three members answer `""`/`false`,
/// and the reason is printed once on stderr rather than swallowed.
pub fn for_bundle(capture_id: &str, dir: Option<&std::path::Path>) -> Option<Arc<CodecGrid>> {
    let key = capture_id.to_string();
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }
    let built = load(capture_id, dir);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, built.clone());
    }
    built
}

fn load(capture_id: &str, dir: Option<&std::path::Path>) -> Option<Arc<CodecGrid>> {
    let (json, source): (std::borrow::Cow<'static, str>, String) = match dir {
        None => (SHIPPED_CODECS.into(), "<embedded codecs>".to_string()),
        Some(dir) => {
            let path = crate::surface::artifact_in(dir, "codecs")?;
            match std::fs::read_to_string(&path) {
                Ok(s) => (s.into(), path.display().to_string()),
                Err(e) => {
                    eprintln!("[korvus] persona {capture_id}: cannot read {}: {e}", path.display());
                    return None;
                }
            }
        }
    };
    match derive(capture_id, &json) {
        Ok(g) => Some(Arc::new(g)),
        Err(e) => {
            eprintln!("[korvus] persona {capture_id}: {source} does not parse as a codec grid: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = "chrome-152-linux-20261016T1022";

    #[test]
    fn the_embedded_grid_derives_and_carries_the_shipped_device() {
        let g = for_bundle(SHIPPED, None).expect("the embedded codec grid derives");
        assert_eq!(g.capture_id, SHIPPED, "the grid names the capture it came from");
        // The recorded answers, reached through the derivation rather than looked up.
        let mp4 = g.containers.get("video/mp4").expect("Chrome 150 demuxes video/mp4");
        assert_eq!(mp4.base_video, "maybe");
        assert_eq!(mp4.base_audio, "maybe");
        assert!(mp4.video.iter().any(|c| c == "avc1.42E01E"), "Chrome has the H.264 decoder");
        assert!(mp4.ms.iter().any(|c| c == "avc1.42E01E"));
        // A container nothing answers for is not a row, which is how the grammar reads "unknown".
        assert!(!g.containers.contains_key("text/plain"));
        assert!(!g.containers.contains_key("video/"));
        assert!(g.containers.contains_key("video/webm"));
        // THE EMPTY TYPE IS A ROW, because the recording says so: `MediaRecorder.isTypeSupported("")`
        // is TRUE -- it means "choose whatever you record in by default" -- and so is every
        // `; codecs="…"` hung off it. Asserted here because it reads like a derivation bug and is
        // not one; the grammar has its own branch for it.
        let empty = g.containers.get("").expect("the empty type is recorded, and is true");
        assert!(empty.base_mr && !empty.base_ms && empty.base_video.is_empty());
        // `video/mp2t` is the mirror case: no `canPlayType` answer at all, and a supported MSE
        // byte-stream format. A device's three members do not agree and the row keeps them apart.
        let ts = g.containers.get("video/mp2t").expect("MSE demuxes MPEG-2 TS");
        assert!(ts.base_ms && ts.base_video.is_empty() && !ts.base_mr);
    }
    #[test]
    fn a_missing_bundle_directory_is_none_not_a_panic() {
        assert!(for_bundle("no-such-bundle", Some(std::path::Path::new("/nonexistent"))).is_none());
    }

    #[test]
    fn a_grid_with_no_answers_is_an_error_not_an_empty_device() {
        assert!(derive("x", r#"{"source":"x","answers":{}}"#).is_err());
        assert!(derive("x", "not json").is_err());
    }
}
