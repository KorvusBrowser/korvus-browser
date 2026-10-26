//! THE BLUEPRINT.
//!
//! What the first isolate of a process recorded about an interface is kept as plain data for the whole
//! process (`korvus-webapi`'s `install::templates`), and a later isolate wearing the same persona builds
//! its templates from it instead of running the interface's install. The later isolate that matters is
//! a document's SUCCESSOR (`Engine::successor`, a cross-document navigation): it wears the very `Arc`
//! its predecessor did, so every interface its first realm switches comes from the blueprint.
//!
//! Asserted both ways, because a count alone would pass on an isolate that built something else and a
//! comparison alone would pass on one that never took the blueprint path:
//!   * the second isolate RECORDED NOTHING and built the switched interfaces from the blueprint
//!     (`Engine::template_counts`);
//!   * the pilot's surface -- every own key of every interface object and prototype, with its
//!     descriptor, name and length -- and a little of its behaviour answer the same in both isolates.
//!
//! And an isolate wearing ANOTHER persona builds from the same blueprints, all but the two whose installs read
//! the persona.
//!
//! The surface lock's `page-successor` realm (`tools/templates/lock`) checks the WHOLE surface the same
//! way, byte for byte against the page golden; this is the gate's copy of the question.
use engine::Engine;
use std::sync::Arc;

const PAGE: &str = "<!doctype html><html><head></head><body><div id=root></div></body></html>";

/// The pilot's shape, and a little of what it does.
const PROBE: &str = r#"(() => {
  const PILOT = ['EventTarget', 'Node', 'Element', 'HTMLElement', 'CharacterData', 'Text', 'Comment', 'Document',
    'HTMLDocument', 'DocumentFragment', 'DOMTokenList', 'NodeList', 'HTMLCollection', 'Event', 'CustomEvent',
    'ErrorEvent', 'PromiseRejectionEvent', 'ProgressEvent', 'MessageEvent', 'CookieChangeEvent', 'UIEvent',
    'MouseEvent', 'PointerEvent', 'WheelEvent', 'KeyboardEvent', 'InputEvent', 'FocusEvent', 'TouchEvent', 'Touch'];
  const fn = (f) => typeof f === 'function' ? f.name + '/' + f.length : typeof f;
  const desc = (o, k) => {
    const d = Object.getOwnPropertyDescriptor(o, k);
    const flags = (d.writable ? 'w' : '') + (d.enumerable ? 'e' : '') + (d.configurable ? 'c' : '');
    return String(k) + ':' + ('value' in d ? 'v' + flags + ':' + fn(d.value) : 'a' + flags + ':' + fn(d.get) + ':' + fn(d.set));
  };
  const out = [];
  for (const n of PILOT) {
    const I = globalThis[n];
    if (typeof I !== 'function') { out.push(n + ' absent'); continue; }
    out.push(n + ' ' + Reflect.ownKeys(I).map((k) => desc(I, k)).join(','));
    out.push(n + '.prototype ' + Reflect.ownKeys(I.prototype).map((k) => desc(I.prototype, k)).join(','));
    const up = Object.getPrototypeOf(I.prototype);
    out.push(n + ' extends ' + (up && up.constructor && up.constructor.name));
  }
  const d = document.createElement('div');
  d.className = 'a b';
  d.classList.add('c');
  d.appendChild(document.createTextNode('t'));
  document.getElementById('root').appendChild(d);
  out.push(document.body.innerHTML);
  let got = '';
  d.addEventListener('x', (e) => { got = e.type + ':' + e.detail + ':' + (e.target === d); });
  d.dispatchEvent(new CustomEvent('x', { detail: 1 }));
  out.push(got);
  try { Node.prototype.appendChild.call({}, d); out.push('no throw'); } catch (e) { out.push(e.name + ': ' + e.message); }
  try { new Node(); out.push('no throw'); } catch (e) { out.push(e.name + ': ' + e.message); }
  return out.join('\n');
})()"#;

fn probe(persona: Arc<dyn spec::Persona>) -> (String, webapi::TemplateCounts) {
    let mut e = Engine::assemble_shared(persona).expect("the default persona assembles");
    e.load_document(PAGE).unwrap();
    let v = e.eval_value("templates-blueprint", PROBE.to_string()).unwrap();
    let v = v.as_str().unwrap_or_else(|| panic!("the probe answers a string, not {v}")).to_string();
    (v, e.template_counts())
}

#[tokio::test]
async fn a_successor_builds_the_switched_interfaces_from_the_first_isolates_blueprint() {
    // A successor wears its predecessor's persona (`Engine::successor`).
    let persona: Arc<dyn spec::Persona> = Arc::new(persona::CapturedPersona::default());
    let (first, a) = probe(persona.clone());
    let (second, b) = probe(persona);
    assert_eq!(first, second, "the successor answers the pilot probe differently from the isolate that recorded it");
    assert!(first.lines().count() >= 29 * 3, "the probe saw the pilot:\n{first}");
    // (the key names the surface by content, so in a test process the first isolate may itself build from
    // another test's blueprints: counted either way)
    assert!(a.recorded + a.from_blueprint >= 29, "the first isolate recorded or built the pilot: {a:?}");
    assert_eq!(b.recorded, 0, "the successor recorded nothing -- it had the blueprints: {b:?}");
    assert!(b.from_blueprint >= 29, "the successor built the pilot from the first isolate's blueprints: {b:?}");
    assert!(b.instantiated >= 29, "and instantiated it: {b:?}");
}

/// An isolate wearing ANOTHER persona -- every engine a process assembles has its own persona `Arc` -- builds
/// every interface whose install did not read the persona from the same blueprints. Engine work V8 runs around
/// an install's own calls is not the install's: the long-animation-frame monitor's persona read there (its
/// refresh grid) marked every capture, so each blueprint was kept per persona, every isolate recorded all 890
/// keys again, and the runtime suite's memory climbed ~50 MB a test to 35 GB (2026-10-09). Two installs read
/// the persona themselves -- `Performance` (its heap limit) and the global (the window's geometry) -- and are
/// kept per persona.
#[tokio::test]
async fn an_isolate_wearing_another_persona_builds_from_the_same_blueprints() {
    let _ = probe(Arc::new(persona::CapturedPersona::default()));
    let (_, b) = probe(Arc::new(persona::CapturedPersona::default()));
    assert!(b.recorded <= 2, "only the installs that read the persona record again for another persona: {b:?}");
    // A page interface put lazily is one the blueprints have, built on its first read
    assert!(b.from_blueprint + b.lazy >= 800, "every other interface is built from the blueprints, or put lazily from them: {b:?}");
}
