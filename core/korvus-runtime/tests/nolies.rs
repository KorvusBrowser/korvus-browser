//! G1.12: the native-binding thesis — 0 lies. Checks that every native getter/method/constructor
//! across the implemented surface reports [native code] and carries no JS-shim tells (no own
//! `prototype`/`arguments`/`caller`), the CreepJS `queryLies` core applied to our surface.
use engine::Engine;


const HARNESS: &str = r#"
const lies = [];
const isNative = (fn) => typeof fn === "function" && Function.prototype.toString.call(fn).includes("[native code]");
const noShimTells = (fn) => !Object.prototype.hasOwnProperty.call(fn, "prototype")
    && !Object.prototype.hasOwnProperty.call(fn, "arguments")
    && !Object.prototype.hasOwnProperty.call(fn, "caller");

function ownDescOnChain(obj, prop) {
  // Walk the prototype chain (members may live on an ancestor prototype, e.g. Element.tagName is
  // reached from an HTMLElement instance through HTMLElement.prototype -> Element.prototype).
  let p = Object.getPrototypeOf(obj);
  while (p) { const d = Object.getOwnPropertyDescriptor(p, prop); if (d) return d; p = Object.getPrototypeOf(p); }
  return Object.getOwnPropertyDescriptor(obj, prop);
}
function checkGetter(obj, prop, label) {
  const d = ownDescOnChain(obj, prop);
  if (!d || typeof d.get !== "function") { lies.push(label + ": no native getter"); return; }
  if (!isNative(d.get)) lies.push(label + ": getter not [native code]");
  if (!noShimTells(d.get)) lies.push(label + ": getter has shim tell");
}
function checkMethod(obj, prop, label) {
  const fn = obj[prop] || (Object.getPrototypeOf(obj) && Object.getPrototypeOf(obj)[prop]);
  if (typeof fn !== "function") { lies.push(label + ": missing"); return; }
  if (!isNative(fn)) lies.push(label + ": not [native code]");
  if (!noShimTells(fn)) lies.push(label + ": has shim tell");
}
function checkCtor(fn, label) {
  if (!isNative(fn)) lies.push(label + ": ctor not [native code]");
}

// navigator + screen (native persona-routed getters)
["userAgent", "platform", "hardwareConcurrency", "deviceMemory"].forEach(p => checkGetter(navigator, p, "navigator." + p));
["width", "height", "colorDepth"].forEach(p => checkGetter(screen, p, "screen." + p));

// URL: native ctor + getters + method
checkCtor(URL, "URL");
const u = new URL("https://a.b/c?x=1");
["href", "protocol", "host", "hostname", "pathname", "search", "hash"].forEach(p => checkGetter(u, p, "URL." + p));

// TextEncoder/Decoder
checkCtor(TextEncoder, "TextEncoder");
checkMethod(new TextEncoder(), "encode", "TextEncoder.encode");

// document + element (native DOM)
checkCtor(Document, "Document");
checkGetter(document, "title", "document.title");
checkMethod(document, "getElementById", "document.getElementById");
const el = document.getElementById("only");
if (el) {
  checkGetter(el, "tagName", "element.tagName");
  checkMethod(el, "getAttribute", "element.getAttribute");
}

print(JSON.stringify(lies));
"#;

#[tokio::test]
async fn zero_lies() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body><div id=\"only\">x</div></body></html>").unwrap();
    e.eval("harness", HARNESS.to_string()).unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0], "[]", "expected ZERO lies across the native surface, got: {}", out[0]);
}
