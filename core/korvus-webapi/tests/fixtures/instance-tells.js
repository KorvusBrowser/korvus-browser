//! `instance-tells.js` -- the INSTANCE axis of the stealth board.
//!
//! WHY THIS EXISTS. Every other axis on the board walks PROTOTYPES: `scan_tells` reads
//! `Object.getOwnPropertyNames(Iface.prototype)` and grades what it finds there. That is blind to the
//! two ways a member can be fake without ever appearing on a prototype:
//!
//!   1. A `Proxy` `get` trap that MINTS a fresh JS arrow per read. Nothing reaches the prototype, so
//!      the prototype scan sees a clean interface -- while `String(localStorage.getItem)` prints JS
//!      source and `localStorage.getItem !== localStorage.getItem`. No browser does either.
//!   2. A member whose value is a plain JS `Array` wearing a platform-list's name. `Array.isArray()`
//!      is TRUE, `instanceof CSSRuleList` is FALSE, and the value carries all of `Array.prototype`.
//!
//! Both were live in this engine while the board read "own-tells 0, name-defects 0, cross-layer
//! incoherences 0" at every commit. This probe reads LIVE INSTANCES instead, and asks four questions
//! a real browser answers the same way every time:
//!
//!   A. NATIVE      every function reachable on a platform instance is `[native code]`.
//!   B. STABLE      reading a function-valued member twice returns the SAME function object.
//!   C. BRANDED     an instance whose IDL type is an interface is `instanceof` that interface and is
//!                  NOT an Array. (`sequence<T>` members really are Arrays; those are not on the
//!                  roster -- the roster only carries sites whose declared type is an interface.)
//!   D. UNBOUND     no member's `name` starts with "bound ", the signature `Function.prototype.bind`
//!                  leaves when a JS wrapper re-binds a receiver.
//!
//! THE ROSTER is the list below. It covers every instance reachable from a `new Proxy` site in this
//! engine (Storage, element.style, HTMLCollection, URL, getComputedStyle) plus the list types and the
//! window singletons a detector reads first. It is meant to GROW: adding a site costs one line, and a
//! site that comes back red is a finding, not a reason to drop the line.
//!
//! Returns a JSON array of strings, one per defect. Empty is the passing state; the gate that reads
//! this (`tests/instance_tells.rs`) carries the ratchet of what is knowingly still outstanding.
(function () {
  const tells = [];
  const toSrc = Function.prototype.toString;
  const isNative = (f) => {
    try { return /\{\s*\[native code\]\s*\}/.test(toSrc.call(f)); } catch (e) { return false; }
  };

  // A <style> sheet and a laid-out element, so the CSSOM/layout sites below have something real.
  // Self-provisioning: the probe runs against whatever page the caller loaded, and an empty
  // `document.styleSheets` would silently skip the two CSSOM sites -- the exact half of this axis
  // that caught `cssRules`. Provision instead of skip.
  let sheet = null;
  try {
    if (!document.styleSheets || document.styleSheets.length === 0) {
      const s = document.createElement("style");
      s.appendChild(document.createTextNode("#probe, .probe { color: red; margin: 0 auto }"));
      (document.head || document.documentElement).appendChild(s);
    }
    sheet = document.styleSheets[0];
  } catch (e) {}
  const body = document.body;
  const el = document.getElementById("probe") || body;

  // [label, () => value, expected interface name | null]
  // The interface name is the member's DECLARED WebIDL type. `null` means "brand not asserted, but
  // still scanned for A and B".
  const sites = [
    ["localStorage", () => localStorage, "Storage"],
    ["sessionStorage", () => sessionStorage, "Storage"],
    ["location", () => location, "Location"],
    ["navigator", () => navigator, "Navigator"],
    ["history", () => history, "History"],
    ["screen", () => screen, "Screen"],
    ["performance", () => performance, "Performance"],
    ["document", () => document, "Document"],
    ["new URL()", () => new URL("https://example.com/a?b=1#c"), "URL"],
    ["new URLSearchParams()", () => new URLSearchParams("a=1"), "URLSearchParams"],
    ["el.classList", () => el.classList, "DOMTokenList"],
    ["el.attributes", () => el.attributes, "NamedNodeMap"],
    ["el.style", () => el.style, "CSSStyleDeclaration"],
    ["el.children", () => el.children, "HTMLCollection"],
    ["document.getElementsByTagName", () => document.getElementsByTagName("div"), "HTMLCollection"],
    ["document.querySelectorAll", () => document.querySelectorAll("div"), "NodeList"],
    ["el.getBoundingClientRect()", () => el.getBoundingClientRect(), "DOMRect"],
    ["el.getClientRects()", () => el.getClientRects(), "DOMRectList"],
    ["sheet", () => sheet, "CSSStyleSheet"],
    ["sheet.cssRules", () => sheet.cssRules, "CSSRuleList"],
    ["sheet.rules", () => sheet.rules, "CSSRuleList"],
    ["getComputedStyle(el)", () => getComputedStyle(el), "CSSStyleDeclaration"],
  ];

  // Prototypes whose members are ECMAScript, not Web IDL -- walking into them would grade
  // `Array.prototype.map` as a platform member. A value that reaches Array.prototype at all is
  // already reported by check C.
  const STOP = new Set([Object.prototype, Function.prototype, Array.prototype]);
  const SKIP_MEMBER = new Set(["constructor", "caller", "arguments", "callee"]);

  for (const entry of sites) {
    const label = entry[0], thunk = entry[1], iface = entry[2];
    let v;
    try { v = thunk(); } catch (e) { tells.push(label + ": threw " + e); continue; }
    if (v === null || v === undefined) { tells.push(label + ": absent (" + v + ")"); continue; }

    // C -- brand.
    if (iface) {
      if (Array.isArray(v)) tells.push(label + ": Array.isArray() is true, but the IDL type is " + iface);
      const C = globalThis[iface];
      if (typeof C !== "function") {
        tells.push(label + ": interface " + iface + " is not exposed");
      } else if (!(v instanceof C)) {
        tells.push(label + ": not instanceof " + iface);
      }
    }

    // A + B -- every function reachable on the instance, own and inherited.
    const names = [];
    const seen = new Set();
    for (let o = v; o && !STOP.has(o); o = Object.getPrototypeOf(o)) {
      let own;
      try { own = Object.getOwnPropertyNames(o); } catch (e) { break; }
      for (const n of own) if (!seen.has(n)) { seen.add(n); names.push(n); }
    }
    for (const n of names) {
      if (SKIP_MEMBER.has(n)) continue;
      let a, b;
      try { a = v[n]; } catch (e) { continue; }
      if (typeof a !== "function") continue;
      if (!isNative(a)) tells.push(label + "." + n + ": not [native code]");
      // D -- `Function.prototype.bind` produces a native function whose `name` is "bound <name>". It
      // is the usual way a JS wrapper re-binds a receiver, and the renaming survives into the page.
      if (typeof a.name === "string" && a.name.indexOf("bound ") === 0) {
        tells.push(label + "." + n + ": name is " + JSON.stringify(a.name) + " -- bound by Function.prototype.bind");
      }
      try { b = v[n]; } catch (e) { continue; }
      if (a !== b) tells.push(label + "." + n + ": a fresh function object per read (minted, not installed)");
    }
  }

  return JSON.stringify(tells);
})()
