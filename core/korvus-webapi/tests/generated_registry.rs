//! The generated one-table REGISTRY (an `InterfaceDef` per window interface, from the capture) is the
//! SINGLE SOURCE of the shell surface (F3). Self-contained spot-checks of generation correctness -- the
//! whole window surface is present, every interface has a ctor, const members carry their VALUE, and the
//! shape classes the installer relies on (tag aliases, ES-intrinsic no-tag, ctor-newability) are emitted.
//! The former byte-identical-vs-shells.registry.json gate proved the flip (F3-b); the JSON golden was
//! then deleted (single source), so the descriptor-level truth now lives in the committed, reviewable
//! generated_registry.rs itself, and the LIVE surface is gated by the korvus-runtime suite.
use webapi::install::generated_registry::REGISTRY;
use webapi::install::{ConstVal, MemberKind, Model};

#[test]
fn generated_registry_reflects_the_capture() {
    // The window shell surface is ~924 interfaces; the generator emits one table each, minus the ones
    // an `interface!` table owns (tools/sync/table_inventory.py: the geometry/navigator/image tables,
    // W5.0a's ten lists, W5.0c's eighteen DOM-spine interfaces, W5.1 L7's twelve IndexedDB interfaces).
    //
    // This is a COLLAPSE FLOOR, not a pin. What the registry contains is pinned by NAME -- every row is
    // in the committed, reviewable `generated_registry.rs`, `tests/exclusion.rs` asserts the inventory
    // and the registry are disjoint, and `sync.sh --check` asserts the file regenerates to itself. The
    // count only refuses a registry that has evaporated, and it has to stay well under the real total
    // because every cutover moves interfaces OUT of it -- the goal ends with the registry gone. It read
    // `> 880` until W5.1 L7 took the twelve IndexedDB interfaces (892 -> 880).
    assert!(REGISTRY.len() > 500, "registry has only {} interfaces", REGISTRY.len());

    let mut consts = 0usize;
    let mut max_members = 0usize;
    for def in REGISTRY {
        assert!(!def.name.is_empty(), "an interface has an empty name");
        assert!(def.ctor.is_some(), "{} has no interface-object ctor", def.name);
        max_members = max_members.max(def.members.len());
        for m in def.members {
            // label is the "Iface::member" coverage key.
            assert_eq!(m.label.split("::").next(), Some(def.name), "bad label {}", m.label);
            if matches!(m.kind, MemberKind::Const) {
                consts += 1;
                // A const's value IS its surface — the generator must carry it, not ConstVal::None.
                assert!(!matches!(m.cval, ConstVal::None), "{}::{} const has no value", def.name, m.name);
            }
        }
    }
    assert!(consts > 0, "expected const members with values in the window registry");
    assert!(max_members > 10, "expected at least one rich interface (got max {max_members} members)");
}

/// The registry records the bundles it was generated from, and **the shipped one must be among
/// them** -- the drift goal I6 makes a red test (W3.3), asked the way the union makes askable.
///
/// "One bundle id everywhere". It used to be
/// `CAPTURE_ID == the shipped manifest's id`, which was the whole truth while the registry came from
/// ONE capture and the engine's surface was compiled from it. The registry is now the union over
/// every bundle in `plugins/korvus-persona/bundles/` and the ACTIVE bundle selects from it at context
/// creation, so equality with the shipped id would be false by construction. What is still true, and
/// is the drift worth failing on, is that a registry which has never seen the bundle a process wears
/// has NO ROWS for it to select.
///
/// Asserted for EVERY bundle in the tree, not just the shipped one, and the directory is the
/// denominator (I5): dropping a capture in without re-running `scripts/sync.sh` fails here.
#[test]
fn the_registry_carries_every_bundle_in_the_tree() {
    use webapi::install::generated_registry::CAPTURE_IDS;
    let shipped = persona::shipped_bundle().capture.id;
    assert!(
        CAPTURE_IDS.contains(&shipped.as_str()),
        "the registry's union {CAPTURE_IDS:?} does not include the SHIPPED bundle {shipped:?} -- \
         the default persona would have no rows to select"
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    on_disk.sort();
    let mut known: Vec<String> = CAPTURE_IDS.iter().map(|s| s.to_string()).collect();
    known.sort();
    assert_eq!(
        known, on_disk,
        "the generated registry's bundle set and the bundle DIRECTORY disagree. Run scripts/sync.sh: \
         a bundle with no rows in the union installs nothing of its own, and a row set for a bundle \
         that is gone is a surface nothing selects."
    );
}

/// Find an interface by name (panics with a clear message if the generator dropped it).
/// **THE TWO PASSES THAT BUILD AN `HTML*Element` ARE A PAIR, AND THIS IS THE GATE ON IT**.
///
/// `core/dom/spine.rs::create_html_element_shells` creates every `HTML*Element` with an EMPTY selector
/// -- constructor, tag and chain, no members -- so the hand element phases can define their members
/// first; `installer::backfill_members` then adds the registry's rows, and it skips any interface whose
/// `backfill` column is false. An interface in the first set and not the second therefore ends up with a
/// bare prototype and NOTHING REPORTS IT: no gap, no ledger row, no log line. It is just empty.
///
/// `HTMLUserMediaElement` was exactly that for as long as it existed -- six rows emitted, zero
/// installed -- and only the fourth persona bundle made it visible, as six "capture members with no row"
/// in `persona_surface`. The cause was two webref gates where one was meant: `backfillable()` has always
/// said `*Element` qualifies, and a second `name in IDL_IFACES` condition overruled it for an interface
/// webref does not declare.
///
/// This asserts the invariant rather than the instance, and it is deliberately NOT a count.
#[test]
fn every_html_element_shell_is_backfillable() {
    let bare: Vec<&str> = REGISTRY
        .iter()
        .filter(|d| webapi::is_html_element_shell(d.name))
        .filter(|d| !d.backfill && !d.members.is_empty())
        .map(|d| d.name)
        .collect();
    assert!(
        bare.is_empty(),
        "these interfaces are created with an EMPTY selector by create_html_element_shells and are not \
         backfillable, so their rows can never be installed by anything and the prototype is silently \
         bare: {bare:?}. Fix tools/sync/gen_shells.py::html_element_shell and regenerate -- never the \
         ceiling, because there is no ceiling here to raise."
    );

    // NON-VACUITY: the filter must actually select something, or an empty scan scores perfectly.
    //
    // 34 at this commit, not the ~90 an `HTML*Element` count on the whole platform would suggest, and
    // the difference is the point of I1: `HTMLVideoElement`, `HTMLCanvasElement`, `HTMLImageElement`
    // and the rest of the tabled families are owned by their own `interface!` tables and are excluded
    // from this registry. The spine pass walks REGISTRY, so REGISTRY is also exactly the right
    // denominator for this pair.
    let shells = REGISTRY.iter().filter(|d| webapi::is_html_element_shell(d.name)).count();
    assert!(shells >= 20, "only {shells} HTML*Element shells in the registry -- the scan is not working");
    // And a shell of the kind that found the defect (`HTMLUserMediaElement`, a Brave 151 interface, while the
    // persona was Brave) must still be in scope with its rows: Chrome 150's fenced frame, five members and
    // its static.
    let shell = iface("HTMLFencedFrameElement");
    assert!(shell.backfill, "HTMLFencedFrameElement must be backfillable or its six rows install nowhere");
    assert_eq!(shell.members.len(), 6, "HTMLFencedFrameElement carries six rows in the Chrome capture");
}

fn iface(name: &str) -> &'static webapi::install::InterfaceDef {
    REGISTRY.iter().copied().find(|d| d.name == name).unwrap_or_else(|| panic!("{name} missing from registry"))
}

#[test]
fn generated_registry_emits_the_shape_classes() {
    // toStringTag: a normal interface tags itself; a legacy alias tags its canonical interface; an ES
    // intrinsic has no tag (the installer leaves V8's own). These three cases drive the installer's tag
    // pass, so a regression here would mis-stamp Symbol.toStringTag.
    assert_eq!(iface("HTMLDivElement").tag, Some("HTMLDivElement"), "HTMLDivElement tags itself");
    assert_eq!(iface("Option").tag, Some("HTMLOptionElement"), "Option is a legacy alias for HTMLOptionElement");
    assert_eq!(iface("Object").tag, None, "Object (ES intrinsic) has no toStringTag");

    // ctor-newability: THREE classes, because Chrome answers a bare `X()` with three
    // different messages and the generator has to say which (`HTML_CTOR` in gen_shells.py):
    //   Model::Value      WebIDL gives a constructor            `new X()` mints
    //   Model::Element    WebIDL gives `[HTMLConstructor]`      `new X()` -> Failed to construct 'X': Illegal constructor
    //                                                           `X()`     -> Failed to construct 'X': Please use the 'new' operator, …
    //   Model::Interface  WebIDL gives none                     `new X()` -> Failed to construct 'X': Illegal constructor
    //                                                           `X()`     -> Illegal constructor
    // The example used to be `XMLHttpRequest`, which is an `interface!` table now and
    // so is no longer IN the registry (the fact it stood for is unchanged -- `tests/exclusion.rs`
    // asserts the name left, and `korvus-runtime/tests/l10_live.rs` asserts `new XMLHttpRequest()`
    // still constructs).
    assert!(matches!(iface("WebSocket").model, Model::Value), "WebSocket is newable");
    assert!(
        matches!(iface("HTMLDivElement").model, Model::Element),
        "HTMLDivElement is an [HTMLConstructor] interface: not newable, but a bare call is 'Please use the new operator'"
    );
    // The DERIVED half, so no single name carries the fact: the `[HTMLConstructor]` class is exactly
    // the HTML element interfaces, and the generator still populates it.
    let element_rows: Vec<&str> = REGISTRY.iter().filter(|d| matches!(d.model, Model::Element)).map(|d| d.name).collect();
    assert!(!element_rows.is_empty(), "no registry interface is `Model::Element` -- the generator stopped reading [HTMLConstructor]");
    let odd: Vec<&&str> = element_rows.iter().filter(|n| !(n.starts_with("HTML") && n.ends_with("Element"))).collect();
    assert!(odd.is_empty(), "`Model::Element` is `[HTMLConstructor]`, which only HTML element interfaces carry: {odd:?}");

    // native-backfill (w) + slot-backed (sl): the flags that route a natively-implemented interface
    // through the installer's backfill and slot paths.
    //
    // DERIVED, NOT NAMED, and that is the point of this paragraph. The example here was
    // `XMLHttpRequest`, then `HTMLInputElement`, then `HTMLSelectElement`, and each time a W5.1 lane
    // gave that interface an `interface!` table the name left the registry and this line went red on
    // a lane that had broken nothing -- three false alarms for one fact. The fact is "the generator
    // still emits both flags", and the registry can be asked that directly; the migration is supposed
    // to empty this registry, so no NAME in it is a stable example.
    let backfill = REGISTRY.iter().filter(|d| d.backfill).count();
    let slot_backed = REGISTRY.iter().filter(|d| d.slot_backed).count();
    assert!(backfill > 0, "no registry interface is `backfill` -- the generator stopped emitting the flag");
    assert!(slot_backed > 0, "no registry interface is `slot_backed` -- the generator stopped emitting the flag");
    // The line above this one used to be `assert!(iface("MessageEvent").slot_backed, ...)`, kept as a
    // named example with the note "if it ever becomes a lane's target, delete this line rather than
    // swapping in the next victim". W4.b tabled `MessageEvent` the same day, so the instruction is
    // followed here: deleted, not swapped. The two counted assertions above are the whole fact.
}
