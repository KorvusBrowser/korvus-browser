//! The INSTANCE axis of the stealth board (Track 0.8), as a standing gate.
//!
//! The other five axes walk prototypes. This one reads LIVE INSTANCES and asks the four questions a
//! prototype scan structurally cannot: is every function reachable on the instance `[native code]`,
//! does reading a function-valued member twice return the SAME object, is that function's `name` the
//! member's own name (rather than `bind`'s "bound x"), and does a value whose IDL type is an interface
//! carry that interface's brand (and not `Array`'s)?
//!
//! See `fixtures/instance-tells.js` for the probe and the defects that motivated it.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/instance-tells.js");
/// The page carries a stylesheet and a laid-out element so the CSSOM and layout sites resolve.
const PAGE: &str = r#"<!doctype html><html><head><style id="s">
    #probe, .probe { color: red; margin: 0 auto }
    p { }
  </style></head><body>
    <div id="probe" class="probe" data-kind="panel"><p>one</p><p>two</p></div>
  </body></html>"#;

async fn instance_tells() -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    let raw = e.eval_value("instance-tells", PROBE.to_string()).expect("instance-tells probe threw");
    let json = raw.as_str().expect("instance-tells.js must return a JSON string");
    serde_json::from_str::<Vec<String>>(json).expect("instance-tells.js must return a JSON array of strings")
}

/// The instance tells this engine still carries, each spelled out in full.
///
/// This is a RATCHET, not a tolerance -- the same shape as `worker_surface`'s ceilings. The gate below
/// fails on any tell NOT in this list, and equally on any entry in this list the engine no longer
/// produces. So the list can only shrink, and it cannot be padded to make a new defect pass: adding a
/// line here is a visible, reviewable claim that a specific tell is knowingly outstanding.
// The two `document.querySelectorAll` tells that stood here -- `Array.isArray()` true and `not
// instanceof NodeList` -- are gone: querySelectorAll, getElementsByName and the seven labelable
// elements' `labels` answer a real NodeList, minted from an ObjectTemplate with an indexed property
// interceptor (core/dom/collections.rs). The blast radius the old note warned about was real and is
// handled where it lands: the engine's own helpers snapshot the list into an Array (`qsaArray` in
// dom/03-element-glue.js) and the selector tests take their own snapshot, rather than the platform
// type being bent to suit them.
const KNOWN: &[&str] = &[];

#[tokio::test]
async fn no_platform_instance_mints_or_mislabels_a_member() {
    let tells = instance_tells().await;
    println!("  instance-tells: {} {:?}", tells.len(), tells);
    let known: std::collections::BTreeSet<&str> = KNOWN.iter().copied().collect();
    let got: std::collections::BTreeSet<&str> = tells.iter().map(String::as_str).collect();
    let fresh: Vec<&&str> = got.difference(&known).collect();
    let gone: Vec<&&str> = known.difference(&got).collect();
    assert!(
        fresh.is_empty(),
        "{} NEW instance tell(s): {:#?}\n\
         Each line is a member a real browser answers differently. `not [native code]`, `a fresh \
         function object per read` and `bound by Function.prototype.bind` all mean a JS closure is \
         being handed out where a native binding belongs (see \
         core/korvus-webapi/src/binding/protos.rs for build_fn, and \
         modules/storage/storage.rs for the named-property interceptor that replaces a Proxy). An \
         `Array.isArray()` or `not instanceof` line means a platform list is a plain JS Array.\n\
         Fix the member. Do NOT add the line to KNOWN -- that list is a ratchet that only shrinks.",
        fresh.len(),
        fresh
    );
    assert!(
        gone.is_empty(),
        "KNOWN lists {} tell(s) the engine no longer produces -- delete them from KNOWN so the \
         ratchet stays tight: {:#?}",
        gone.len(),
        gone
    );
}
