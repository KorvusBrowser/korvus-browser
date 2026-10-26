//! LIVE one-table gate for the observers / custom-elements / dialog interfaces.
//!
//! Boots the real engine and asserts, for `CustomElementRegistry`, `MutationRecord`,
//! `HTMLDialogElement` and `MutationObserver`:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the captured bundle's member
//!      list for that interface, in the capture's order, with the capture's descriptors, followed by
//!      the non-enumerable `constructor` -- the hand-table shape.
//!   2. **The interface objects**: the capture's `Symbol.toStringTag`, `length`, `name` and statics,
//!      the capture's NEWABILITY (`new CustomElementRegistry()` succeeds -- the bundle records
//!      `newable: true` -- while the other three answer Chrome's "Illegal constructor"), and
//!      `HTMLDialogElement`'s recorded chain onto `HTMLElement`.
//!   3. **Real behaviour through the real machinery**: a `MutationObserver` seeing a childList
//!      mutation and delivering it to its callback; `customElements.define` upgrading a PRE-PARSED
//!      element (class fields exist, `connectedCallback` fires); `dialog.showModal()` / `close(v)`
//!      driving the real `open` attribute and queueing a trusted `close` event.
//!
//! Window-only: the bundle's `worker.surface.json` lists none of the four.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body><dialog id=\"d\">hi</dialog><x-pre id=\"p\"></x-pre></body></html>";

/// The four this lane's tables own, in the order the installer builds them.
const IFACES: &[&str] = &["CustomElementRegistry", "MutationRecord", "HTMLDialogElement", "MutationObserver"];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        "value" => format!(
            "{n}:data:e={}:c={}:w=0",
            m["enumerable"].as_bool().unwrap() as u8,
            m["configurable"].as_bool().unwrap() as u8
        ),
        k => panic!("unexpected kind {k}"),
    }
}

/// The `describe_own` lines the capture's member list implies, with `constructor` at its recorded seat.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx = bundle();
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the bundle surface"))
        .clone();
    let mut out: Vec<String> = members.iter().map(describe_line).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// Pump the event loop until `globalThis.__done` is true, then read `globalThis.__out`.
async fn pump_for(e: &mut Engine, name: &'static str, script: &str) -> String {
    e.eval(name, script.to_string()).unwrap();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        let done = e
            .eval_value("l11-done", "!!globalThis.__done".to_string())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if done || start.elapsed() > std::time::Duration::from_secs(15) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    str_of(e, "l11-out", "String(globalThis.__out)".to_string())
}

// ── 1. the four prototypes, byte-identical to the capture ───────────────────────────────────────

#[tokio::test]
async fn every_l11_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l11-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l11_interface_object_carries_the_capture_tag_length_and_statics() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                const native = Function.prototype.toString.call(C).includes("[native code]");
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, statics.join(","), native].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l11-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        let want_statics: Vec<&str> = fx["interfaces"][iface]["statics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            got,
            format!("{iface}|0|0|1|{want_len}|{iface}|{}|true", want_statics.join(",")),
            "{iface} interface object != the capture"
        );
    }
}

/// Newability is the capture's, per interface: the bundle records `new X()` (zero arguments) working
/// for `CustomElementRegistry` and NOT for the other three. The deleted `dom/08g-observers.js` made
/// `new CustomElementRegistry()` an "Illegal constructor" TypeError -- a structural diff against the
/// bundle this lane closes.
#[tokio::test]
async fn newability_is_the_captures() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{ try {{ const o = new {iface}(); return "ok:" + (Object.getPrototypeOf(o) === {iface}.prototype); }}
                        catch (err) {{ return err.constructor.name + ":" + err.message; }} }})()"#
        );
        let got = str_of(&mut e, "l11-new", src);
        let newable = fx["interfaces"][iface]["newable"].as_bool().unwrap();
        let want = match *iface {
            // Chrome 150.0.7871.46 on this machine, `--headless=new --dump-dom`:
            //   new CustomElementRegistry() -> no-throw
            //   new MutationObserver()      -> "Failed to construct 'MutationObserver': 1 argument required, but only 0 present."
            //   new MutationRecord()        -> "Failed to construct 'MutationRecord': Illegal constructor"
            //   new HTMLDialogElement()     -> "Failed to construct 'HTMLDialogElement': Illegal constructor"
            // The divergence this comment recorded -- "the last two carry a per-interface prefix this
            // engine does not" -- is CLOSED: the shared `cb_illegal_ctor` carries the
            // interface name now, so all four lines are Chrome's verbatim.
            "MutationObserver" => "TypeError:Failed to construct 'MutationObserver': 1 argument required, but only 0 present.".to_string(),
            _ => format!("TypeError:Failed to construct '{iface}': Illegal constructor"),
        };
        if newable {
            assert_eq!(got, "ok:true", "{iface} is newable in the capture");
        } else {
            assert_eq!(got, want, "{iface} is not newable in the capture");
        }
    }
}

/// `HTMLDialogElement` is spliced onto the REAL `HTMLElement` chain the spine built, and the `dialog`
/// tag maps onto the TABLE's prototype through the one `korvus_dom::tags` pass -- not a second
/// prototype for the same element.
#[tokio::test]
async fn the_dialog_interface_sits_on_the_recorded_chain_and_owns_the_tag() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-dialog-chain",
        r#"(() => {
            const d = document.getElementById("d");
            const made = document.createElement("dialog");
            return [
                Object.getPrototypeOf(HTMLDialogElement.prototype) === HTMLElement.prototype,
                Object.getPrototypeOf(HTMLDialogElement) === HTMLElement,
                d instanceof HTMLDialogElement, made instanceof HTMLDialogElement,
                Object.getPrototypeOf(d) === HTMLDialogElement.prototype,
                Object.getPrototypeOf(made) === HTMLDialogElement.prototype,
                d.constructor === HTMLDialogElement,
                Object.prototype.toString.call(d),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|true|true|true|true|true|true|[object HTMLDialogElement]",
        "the HTMLDialogElement chain is not the table's"
    );
}

/// `customElements` is one `CustomElementRegistry` on the table's prototype, held by the own
/// {writable:false, enumerable:true, configurable:true} window property the deleted JS wrote.
#[tokio::test]
async fn the_custom_elements_singleton_is_the_tables() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-singleton",
        r#"(() => {
            const d = Object.getOwnPropertyDescriptor(globalThis, "customElements");
            return [
                Object.getPrototypeOf(customElements) === CustomElementRegistry.prototype,
                customElements instanceof CustomElementRegistry,
                Object.prototype.toString.call(customElements),
                Object.getOwnPropertyNames(customElements).length,
                typeof d.get, typeof d.set, +d.enumerable, +d.configurable,
                ["define","get","getName","upgrade","whenDefined","initialize"]
                    .every(n => Function.prototype.toString.call(customElements[n]).includes("[native code]")),
            ].join("|");
        })()"#
            .to_string(),
    );
    // An own getter-only ACCESSOR, enumerable and configurable, as in every browser of the family
    // (`fixtures/p11a/desc.*.txt`). It asserted the `{writable: false}` data property the
    // deleted JS defined until Chrome, Brave, Edge and Opera were asked.
    assert_eq!(got, "true|true|[object CustomElementRegistry]|0|function|undefined|1|1|true", "the customElements singleton");
}

// ── 2. behaviour through the real machinery ─────────────────────────────────────────────────────

/// A `MutationObserver` observing `childList` sees a real `appendChild` -- synchronously in
/// `takeRecords`, and asynchronously in its callback (the microtask delivery `KB.__mutobs` schedules).
#[tokio::test]
async fn a_mutation_observer_sees_a_childlist_mutation() {
    let mut e = page().await;
    let got = pump_for(
        &mut e,
        "l11-mo",
        r#"(() => {
            const seen = [];
            const o = new MutationObserver((records, obs) => {
                for (const r of records) seen.push([r.type, r.target === document.body, r.addedNodes.length].join(":"));
                globalThis.__out = [seen.join(","), obs === o, taken].join(" / ");
                globalThis.__done = true;
            });
            o.observe(document.body, { childList: true });
            document.body.appendChild(document.createElement("i"));
            const taken = o.takeRecords().length;
            document.body.appendChild(document.createElement("b"));
        })();"#,
    )
    .await;
    assert_eq!(got, "childList:true:1 / true / 1", "the MutationObserver delivery path");
}

/// `customElements.define` upgrades an element that was ALREADY PARSED into the document: the class
/// constructor runs on it (so class field initializers exist) and `connectedCallback` fires.
#[tokio::test]
async fn define_upgrades_a_pre_parsed_element() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-upgrade",
        r#"(() => {
            const log = [];
            class Pre extends HTMLElement {
                field = "set";
                constructor() { super(); log.push("ctor"); }
                connectedCallback() { log.push("connected"); }
            }
            customElements.define("x-pre", Pre);
            const el = document.getElementById("p");
            return [
                el instanceof Pre, el.field, log.join("+"),
                customElements.get("x-pre") === Pre,
                customElements.getName(Pre),
                String(customElements.getName(class {})),
                typeof customElements.whenDefined("x-pre").then,
                (() => { try { customElements.upgrade(); return "no-throw"; } catch (err) { return err.constructor.name; } })(),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|set|ctor+connected|true|x-pre|null|function|TypeError",
        "the custom-element upgrade pass"
    );
}

/// `showModal()` / `close(v)` drive the REAL `open` content attribute and QUEUE a trusted `close`
/// event -- the event must not be dispatched synchronously (WPT dialog-close.html).
#[tokio::test]
async fn a_dialog_shows_closes_and_fires_a_queued_trusted_close_event() {
    let mut e = page().await;
    let got = pump_for(
        &mut e,
        "l11-dialog",
        r#"(() => {
            const d = document.getElementById("d");
            let queued = false;
            d.addEventListener("close", (ev) => {
                globalThis.__out = [before, d.hasAttribute("open"), d.returnValue, ev.type, ev.isTrusted,
                                    ev.bubbles, ev.cancelable, queued].join("|");
                globalThis.__done = true;
            });
            d.showModal();
            const before = [d.hasAttribute("open"), d.open, d.returnValue === ""].join(",");
            d.close("bye");
            queued = true;
        })();"#,
    )
    .await;
    assert_eq!(got, "true,true,true|false|bye|close|true|false|false|true", "the dialog close path");
}


// ── 3. the records themselves ───────────────────────────────────────────────────────

/// A DELIVERED `MutationRecord` is a real one, and the nine rows are the prototype's.
///
/// Until W4.j the records were JS OBJECT LITERALS minted by `host.js`'s `KB.__mutobs`, so
/// `Object.prototype.toString.call(record)` read `[object Object]`, `record instanceof
/// MutationRecord` was false, and the nine rows of this interface sat on a prototype nothing was ever
/// an instance of -- nine honest `= stub`s that nothing could reach. The registry, the delivery and
/// the mint are Rust now (`core/dom/mutation_observer.rs`), the rows are `slot_get`, and the
/// `KB.__mutobs` bridge slot is deleted.
///
/// EVERY LINE BELOW IS CHROME'S, measured with `google-chrome --headless=new` 150.0.7871.46 running
/// this same script over a `file:` URL -- not an engine answer written down. The mutation sequence is
/// two `setAttribute`s on a child (the second with an old value), a `characterData` edit, an append
/// and a remove, under ONE subtree observer asking for all three kinds with both old values.
#[tokio::test]
async fn a_delivered_mutation_record_is_a_real_mutation_record() {
    let mut e = page().await;
    let got = pump_for(
        &mut e,
        "l11-records",
        r#"(() => {
            const out = [];
            const host = document.createElement("div");
            host.innerHTML = '<p id="ra">hi</p><p id="rb">there</p>';
            document.body.appendChild(host);
            const a = host.firstChild;
            out.push("proto=" + Object.getOwnPropertyNames(MutationRecord.prototype).join(","));
            out.push("tagOnProto=" + Object.prototype.toString.call(MutationRecord.prototype));
            const recs = [];
            const mo = new MutationObserver((rs) => {
                for (const r of rs) recs.push(r);
                out.push("count=" + recs.length);
                for (const r of recs) {
                    out.push(r.type
                        + " tag=" + Object.prototype.toString.call(r)
                        + " own=[" + Object.getOwnPropertyNames(r).join(",") + "]"
                        + " proto=" + (Object.getPrototypeOf(r) === MutationRecord.prototype)
                        + " inst=" + (r instanceof MutationRecord)
                        + " added=" + Object.prototype.toString.call(r.addedNodes) + "/" + r.addedNodes.length
                        + " removed=" + Object.prototype.toString.call(r.removedNodes) + "/" + r.removedNodes.length
                        + " attrName=" + r.attributeName + " ns=" + r.attributeNamespace
                        + " old=" + JSON.stringify(r.oldValue)
                        + " prev=" + (r.previousSibling && r.previousSibling.nodeName)
                        + " next=" + (r.nextSibling && r.nextSibling.nodeName));
                }
                out.push("addedNodesCtor=" + recs[0].addedNodes.constructor.name);
                // The two records of ONE mutation get their own lists, as Chrome's do.
                out.push("listsAreDistinct=" + (recs[3].addedNodes !== recs[4].addedNodes));
                const d = Object.getOwnPropertyDescriptor(MutationRecord.prototype, "type");
                out.push("desc=e" + (+d.enumerable) + "c" + (+d.configurable)
                    + " native=" + String(d.get).includes("[native code]") + " name=" + d.get.name);
                globalThis.__out = out.join("\n");
                globalThis.__done = true;
            });
            mo.observe(host, { childList: true, subtree: true, attributes: true, attributeOldValue: true,
                               characterData: true, characterDataOldValue: true });
            a.setAttribute("data-x", "1");
            a.setAttribute("data-x", "2");
            a.firstChild.data = "yo";
            const n = document.createElement("span");
            host.appendChild(n);
            host.removeChild(n);
        })();"#,
    )
    .await;
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "proto=type,target,addedNodes,removedNodes,previousSibling,nextSibling,attributeName,attributeNamespace,oldValue,constructor",
            "tagOnProto=[object MutationRecord]",
            "count=5",
            // `[object Object]`, `own=[the nine names]`, `proto=false`, `inst=false` and
            // `added=[object Array]` were the five differences, on every one of these records.
            "attributes tag=[object MutationRecord] own=[] proto=true inst=true added=[object NodeList]/0 removed=[object NodeList]/0 attrName=data-x ns=null old=null prev=null next=null",
            "attributes tag=[object MutationRecord] own=[] proto=true inst=true added=[object NodeList]/0 removed=[object NodeList]/0 attrName=data-x ns=null old=\"1\" prev=null next=null",
            "characterData tag=[object MutationRecord] own=[] proto=true inst=true added=[object NodeList]/0 removed=[object NodeList]/0 attrName=null ns=null old=\"hi\" prev=null next=null",
            "childList tag=[object MutationRecord] own=[] proto=true inst=true added=[object NodeList]/1 removed=[object NodeList]/0 attrName=null ns=null old=null prev=P next=null",
            "childList tag=[object MutationRecord] own=[] proto=true inst=true added=[object NodeList]/0 removed=[object NodeList]/1 attrName=null ns=null old=null prev=P next=null",
            "addedNodesCtor=NodeList",
            "listsAreDistinct=true",
            "desc=e1c1 native=true name=get type",
        ],
        "a delivered MutationRecord disagrees with Chrome 150"
    );
}

/// `oldValue` and `attributeFilter` are PER REGISTRATION: of several observers of the SAME mutation,
/// only the one that asked for the old value sees it, and a filtered observer sees only its names.
///
/// This is the half of the algorithm a single-observer test cannot see, and the one a port is most
/// likely to lose -- the JS built each record inside `enqueue`'s per-registration `make(r)` callback
/// for exactly this reason, and the Rust keeps the same shape. Measured on Chrome 150.
#[tokio::test]
async fn old_value_and_the_attribute_filter_are_per_registration() {
    let mut e = page().await;
    let got = pump_for(
        &mut e,
        "l11-oldvalue",
        r#"(() => {
            const a = document.createElement("div");
            document.body.appendChild(a);
            const seen = { withOld: [], withoutOld: [], filtered: [] };
            let left = 3;
            const push = (k) => (rs) => {
                for (const r of rs) seen[k].push(r.attributeName + ":" + JSON.stringify(r.oldValue));
                if (--left === 0) {
                    globalThis.__out = ["withOld", "withoutOld", "filtered"]
                        .map((k) => k + "=" + seen[k].join(" ")).join("\n");
                    globalThis.__done = true;
                }
            };
            new MutationObserver(push("withOld")).observe(a, { attributes: true, attributeOldValue: true });
            new MutationObserver(push("withoutOld")).observe(a, { attributes: true });
            // An allow-list observer sees ONLY `data-keep`, and `attributes` is IMPLIED by the filter.
            new MutationObserver(push("filtered")).observe(a, { attributeFilter: ["data-keep"] });
            a.setAttribute("data-drop", "1");
            a.setAttribute("data-drop", "2");
            a.setAttribute("data-keep", "k");
        })();"#,
    )
    .await;
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "withOld=data-drop:null data-drop:\"1\" data-keep:null",
            "withoutOld=data-drop:null data-drop:null data-keep:null",
            "filtered=data-keep:null",
        ],
        "oldValue or attributeFilter is not per-registration"
    );
}

/// **The DOM's insertion steps reach every node the insertion CONNECTS.**
///
/// DOM §4.2.3 "insert" enqueues the `connectedCallback` reaction for each *shadow-including inclusive
/// descendant* of the inserted node, from every insertion API. This engine ran it for the inserted
/// node alone, and only from `appendChild` / `insertBefore` / `replaceChild` -- so a custom element
/// that arrived as a child, as a grandchild, inside a shadow tree, or through `innerHTML`,
/// `insertAdjacentHTML`, `append` or `replaceChildren` was never told it had been connected. On
/// `flask.palletsprojects.com` that cost the page its entire readthedocs UI: three Lit elements never
/// built their shadow roots, so the Font Awesome bundle's `beforeDOMElementCreation` hook never fired
/// and the 33 KB `<style>` it injects into `<head>` was missing -- one node, and the whole `shape`
/// verdict.
///
/// THE ASSERTION IS THE RULE, NOT THE SITE. Each row is an insertion SHAPE; the expected value is
/// "every shape connects, in shadow-including tree order", which is what a browser does. The
/// shadow-tree row and its POSITION are measured against Chrome 150 over the same fixture: a host's
/// shadow tree is traversed BEFORE the host's own light children.
#[tokio::test]
async fn every_insertion_shape_connects_the_custom_elements_it_brings_in() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-insertion-steps",
        r#"(() => {
            const log = [];
            class Base extends HTMLElement {
                connectedCallback() { log.push(this.getAttribute("k")); }
            }
            for (const n of ["a","b","c","d","e","f","g","h"]) {
                customElements.define("x-ins-" + n, class extends Base {});
            }
            const mk = (tag, k) => { const el = document.createElement(tag); el.setAttribute("k", k); return el; };
            const host = document.createElement("div");
            document.body.appendChild(host);

            // 1. the inserted node itself
            host.appendChild(mk("x-ins-a", "self"));
            // 2. a child of the inserted node
            const w2 = document.createElement("div"); w2.appendChild(mk("x-ins-b", "child"));
            host.appendChild(w2);
            // 3. a grandchild
            const w3 = document.createElement("div"), m3 = document.createElement("section");
            m3.appendChild(mk("x-ins-c", "grandchild")); w3.appendChild(m3); host.appendChild(w3);
            // 4. inside the shadow tree of an inserted host -- and BEFORE that host's light children
            const w4 = document.createElement("div"), sh = document.createElement("div");
            sh.attachShadow({ mode: "open" }).appendChild(mk("x-ins-d", "in-shadow"));
            sh.appendChild(mk("x-ins-e", "light-of-shadow-host"));
            w4.appendChild(sh); host.appendChild(w4);
            // 5. parsed into a CONNECTED parent by innerHTML
            const w5 = document.createElement("div"); host.appendChild(w5);
            w5.innerHTML = '<x-ins-f k="innerHTML"></x-ins-f>';
            // 6. written in by insertAdjacentHTML
            const w6 = document.createElement("div"); host.appendChild(w6);
            w6.insertAdjacentHTML("beforeend", '<x-ins-g k="insertAdjacentHTML"></x-ins-g>');
            // 7. the variadic batch path (append), one level down
            const w7 = document.createElement("div"); w7.appendChild(mk("x-ins-h", "append-child"));
            host.append(w7);
            return log.join(" ");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "self child grandchild in-shadow light-of-shadow-host innerHTML insertAdjacentHTML append-child",
        "an insertion connects every node it brings in, in shadow-including tree order"
    );
}

/// **And a `<script>` written in by `innerHTML` still does not run.**
///
/// The companion to the test above, and the reason that walk stops short of the script insertion
/// steps. HTML's fragment parsing algorithm sets a script's "already started" flag as it creates the
/// element, which is WHY `el.innerHTML = "<script>..."` is inert. This engine sets that flag nowhere
/// -- `html_script_element.rs`'s `STARTED` has exactly one writer, the runner -- so the inertness
/// rests today on the insertion steps not reaching the element at all. Widening the walk to scripts
/// before the fragment parser sets the flag would execute every `innerHTML` script on every page, so
/// this asserts the behaviour that must survive whoever does widen it.
#[tokio::test]
async fn a_script_parsed_in_by_inner_html_does_not_run_while_an_appended_one_does() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-inner-html-script",
        r#"(() => {
            globalThis.__ran = [];
            const w = document.createElement("div");
            document.body.appendChild(w);
            w.innerHTML = '<scr' + 'ipt>globalThis.__ran.push("innerHTML")</scr' + 'ipt>';
            const s = document.createElement("script");
            s.textContent = 'globalThis.__ran.push("appended")';
            document.body.appendChild(s);
            return globalThis.__ran.join(" ") || "(none)";
        })()"#
            .to_string(),
    );
    assert_eq!(got, "appended", "an innerHTML script is inert; an appended one runs");
}

/// **An element the FRAGMENT PARSER created is upgraded with its CONSTRUCTOR, not just
/// its prototype.**
///
/// The prerequisite the test above uncovered. `innerHTML` and `insertAdjacentHTML` build their nodes
/// in the arena, and the wrapper those nodes reach JS through already wears the definition's
/// prototype -- so `el instanceof MyEl` was true and `customElements.get(tag)` agreed, while the
/// constructor had never run and every property the constructor assigns was missing. Invisible while
/// `connectedCallback` never reached those elements; a `TypeError` the moment it did.
///
/// THE ASSERTION IS THE RULE. Each row is a way of CREATING the element; the class puts its method on
/// the INSTANCE rather than the prototype, which is the only shape that can tell "upgraded" from
/// "prototype swapped". Measured identical on Chrome 150 over the same fixture, served from
/// `web-platform.test`.
#[tokio::test]
async fn a_fragment_parsed_custom_element_runs_its_constructor_not_just_its_prototype() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-parser-upgrade",
        r#"(() => {
            const log = [];
            for (const n of ["q1","q2","q3","q4"]) {
                customElements.define("x-ctor-" + n, class extends HTMLElement {
                    constructor() { super(); this.mark = () => log.push("ctor-ran:" + n); }
                    connectedCallback() {
                        try { this.mark(); } catch (err) { log.push("THREW:" + n + ":" + err.message); }
                    }
                });
            }
            const host = document.createElement("div");
            document.body.appendChild(host);
            // 1. createElement -- the constructor runs at creation, and always did
            host.appendChild(document.createElement("x-ctor-q1"));
            // 2. parsed into a CONNECTED parent
            const w2 = document.createElement("div"); host.appendChild(w2);
            w2.innerHTML = "<x-ctor-q2></x-ctor-q2>";
            // 3. parsed into a DETACHED parent, which is then appended
            const w3 = document.createElement("div");
            w3.innerHTML = "<x-ctor-q3></x-ctor-q3>";
            host.appendChild(w3);
            // 4. insertAdjacentHTML
            const w4 = document.createElement("div"); host.appendChild(w4);
            w4.insertAdjacentHTML("beforeend", "<x-ctor-q4></x-ctor-q4>");
            const own = ["q1","q2","q3","q4"]
                .map((n) => Object.prototype.hasOwnProperty.call(
                    document.querySelector("x-ctor-" + n), "mark") ? "own" : "MISSING")
                .join(",");
            return log.join(" ") + " | " + own;
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "ctor-ran:q1 ctor-ran:q2 ctor-ran:q3 ctor-ran:q4 | own,own,own,own",
        "every way of creating a defined element runs its constructor before its connect reaction"
    );
}

/// **A removal disconnects every node it takes out, from every removal API.**
///
/// The partner of `every_insertion_shape_connects_the_custom_elements_it_brings_in`, and the reason
/// FP14 raised this as a unit rather than a footnote: until this test existed korvus fired
/// `connectedCallback` faithfully everywhere and `disconnectedCallback` NOWHERE. A component that is
/// connected and never disconnected leaks its own state -- its observers stay registered, its timers
/// keep running -- and a framework that pairs setup with teardown runs every setup twice on a
/// re-attach and every teardown never.
///
/// THE ASSERTION IS THE RULE, NOT THE SITE. Each row is a removal SHAPE and the expected value is
/// "every shape disconnects what it took out, in shadow-including tree order, self before
/// descendants". Three of the rows are MEASURED against Chrome 150 over one fixture on
/// `web-platform.test` rather than read off the spec:
///
///   * a MOVE fires both halves -- `b.appendChild(e)` on an `e` already under `a` logs `disc` then
///     `conn`, and so does a reorder within ONE parent;
///   * `moveBefore` fires them too, though its "state-preserving move" reads as "no reactions";
///   * a host's shadow tree is walked BEFORE its own light children, on the way out as on the way in.
///
/// And one row is the gate: a removal from a parent that was never connected fires nothing.
#[tokio::test]
async fn every_removal_shape_disconnects_the_custom_elements_it_takes_out() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-removal-steps",
        r#"(() => {
            const log = [];
            let uid = 0;
            const def = () => {
                const tag = "x-rm-" + (++uid);
                customElements.define(tag, class extends HTMLElement {
                    connectedCallback() { log.push("conn:" + this.getAttribute("k")); }
                    disconnectedCallback() { log.push("disc:" + this.getAttribute("k")); }
                });
                return tag;
            };
            const mk = (tag, k) => { const el = document.createElement(tag); el.setAttribute("k", k); return el; };
            // Only the disconnects are compared; the connects are the sibling test's subject.
            const row = (name, fn) => {
                log.length = 0; fn();
                return name + "=" + log.filter(x => x.startsWith("disc:")).join(",");
            };
            const host = () => { const h = document.createElement("div"); document.body.appendChild(h); return h; };
            const out = [];

            out.push(row("removeChild", () => { const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); h.removeChild(el); }));
            out.push(row("remove", () => { const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); el.remove(); }));
            out.push(row("replaceChild", () => { const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); h.replaceChild(document.createElement("i"), el); }));
            out.push(row("replaceChildren", () => { const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); h.replaceChildren(); }));
            out.push(row("replaceWith", () => { const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); el.replaceWith(document.createElement("i")); }));
            out.push(row("innerHTML", () => { const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); h.innerHTML = ""; }));
            out.push(row("textContent", () => { const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); h.textContent = ""; }));
            // an ancestor goes, and the DESCENDANTS must hear it -- self before descendants
            out.push(row("ancestor", () => {
                const t = def(), h = host(), mid = document.createElement("div"), deep = document.createElement("section");
                const top = mk(t, "top"); top.appendChild(mk(t, "inner"));
                deep.appendChild(top); mid.appendChild(deep); h.appendChild(mid); mid.remove();
            }));
            // a host's SHADOW tree, traversed before its own light children
            out.push(row("shadow", () => {
                const t = def(), h = host(), sh = document.createElement("div");
                sh.attachShadow({ mode: "open" }).appendChild(mk(t, "in-shadow"));
                sh.appendChild(mk(t, "light-child-of-shadowhost"));
                h.appendChild(sh); h.appendChild(mk(t, "sibling-after")); h.innerHTML = "";
            }));
            // straight into and out of an ATTACHED shadow root: "connected" is shadow-INCLUDING
            out.push(row("in-shadow-root", () => {
                const t = def(), h = host(), r = h.attachShadow({ mode: "open" }), el = mk(t, "a");
                r.appendChild(el); r.removeChild(el);
            }));
            // MOVES: both halves fire (measured on Chrome 150, not read off the spec)
            out.push(row("move-appendChild", () => {
                const t = def(), a = host(), b = host(), el = mk(t, "a"); a.appendChild(el); b.appendChild(el);
            }));
            out.push(row("move-insertBefore", () => {
                const t = def(), a = host(), b = host(), el = mk(t, "a"), anchor = document.createElement("i");
                b.appendChild(anchor); a.appendChild(el); b.insertBefore(el, anchor);
            }));
            out.push(row("reorder-same-parent", () => {
                const t = def(), h = host(), el = mk(t, "a"); h.appendChild(el); h.appendChild(document.createElement("i")); h.appendChild(el);
            }));
            out.push(row("moveBefore", () => {
                const t = def(), a = host(), b = host(), w = document.createElement("div");
                w.appendChild(mk(t, "descendant")); a.appendChild(w); b.moveBefore(w, null);
            }));
            // THE GATE: a parent that was never connected owes no reaction
            out.push(row("detached-parent", () => {
                const t = def(), h = document.createElement("div"), el = mk(t, "a"); h.appendChild(el); h.removeChild(el);
            }));
            return out.join(" | ");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "removeChild=disc:a",
            "remove=disc:a",
            "replaceChild=disc:a",
            "replaceChildren=disc:a",
            "replaceWith=disc:a",
            "innerHTML=disc:a",
            "textContent=disc:a",
            "ancestor=disc:top,disc:inner",
            "shadow=disc:in-shadow,disc:light-child-of-shadowhost,disc:sibling-after",
            "in-shadow-root=disc:a",
            "move-appendChild=disc:a",
            "move-insertBefore=disc:a",
            "reorder-same-parent=disc:a",
            "moveBefore=disc:descendant",
            "detached-parent=",
        ]
        .join(" | "),
        "a removal disconnects every node it takes out, in shadow-including tree order"
    );
}

/// **No setup without its teardown.**
///
/// The property the two halves exist to give a page, asserted as a property rather than as a list of
/// APIs: over a ladder of attach / detach / re-attach the reactions ALTERNATE, and at no prefix of
/// the log has the element been connected twice without a disconnect in between. That is what a
/// framework's `connectedCallback`-registers / `disconnectedCallback`-unregisters pair depends on,
/// and it is exactly the shape the half-implemented set broke: every setup ran, no teardown ever did.
#[tokio::test]
async fn a_custom_element_never_hears_a_setup_without_its_teardown() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-pairing",
        r#"(() => {
            const log = [];
            customElements.define("x-pair", class extends HTMLElement {
                connectedCallback() { log.push("+"); }
                disconnectedCallback() { log.push("-"); }
            });
            const a = document.createElement("div"), b = document.createElement("div");
            document.body.appendChild(a); document.body.appendChild(b);
            const el = document.createElement("x-pair");
            a.appendChild(el); el.remove();
            b.appendChild(el); b.innerHTML = "";
            a.appendChild(el); a.replaceChildren();
            b.appendChild(el);
            let live = 0, broke = false;
            for (const c of log) { live += c === "+" ? 1 : -1; if (live > 1 || live < 0) broke = true; }
            return log.join("") + " live=" + live + " broke=" + broke + " isConnected=" + el.isConnected;
        })()"#
            .to_string(),
    );
    assert_eq!(
        got, "+-+-+-+ live=1 broke=false isConnected=true",
        "every connectedCallback is paired with a disconnectedCallback, and the balance is the element's own state"
    );
}

/// **`attributeChangedCallback`, its `observedAttributes` filter, and the upgrade replay.**
///
/// Three rules, each measured against Chrome 150 over one fixture on `web-platform.test`:
///
///   * the callback fires for an OBSERVED name and is silent for one that is not observed, whatever
///     wrote the attribute -- `setAttribute`, `removeAttribute`, `toggleAttribute` or a reflected
///     IDL setter -- and a write of the SAME value is still a change, while a `removeAttribute` of
///     a name that was never there is not;
///   * `observedAttributes` is read ONCE, at `define`, and not at all when the prototype has no
///     `attributeChangedCallback` (Chrome: `reads === 1` and `reads === 0`). A page that mutates the
///     list afterwards changes nothing, which is why this asserts the read COUNT and not only the
///     filtering it produces;
///   * the UPGRADE REPLAY hands the element the attributes it was parsed with, in attribute-list
///     order, with a `null` old value, after the constructor and before `connectedCallback`.
#[tokio::test]
async fn attribute_changed_fires_for_observed_names_and_replays_them_at_upgrade() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-attribute-changed",
        r#"(() => {
            const out = [];
            const fmt = (n, ov, nv) => "attr:" + n + ":" + (ov === null ? "N" : ov) + ">" + (nv === null ? "N" : nv);

            // 1. every write path, and a name that is NOT observed
            {
                const log = [];
                let reads = 0;
                customElements.define("x-at-1", class extends HTMLElement {
                    static get observedAttributes() { reads++; return ["a", "id"]; }
                    attributeChangedCallback(n, ov, nv) { log.push(fmt(n, ov, nv)); }
                });
                const el = document.createElement("x-at-1");
                el.setAttribute("a", "1");
                el.setAttribute("a", "2");
                el.setAttribute("a", "2");        // the same value is still a change
                el.setAttribute("zz", "9");       // not observed
                el.removeAttribute("a");
                el.removeAttribute("a");          // was not there: not a change
                el.toggleAttribute("a");
                el.id = "i";                      // a reflected IDL setter is a write like any other
                out.push("writes=" + log.join(",") + " reads=" + reads);
            }

            // 2. observedAttributes is not consulted without an attributeChangedCallback
            {
                let reads = 0;
                customElements.define("x-at-2", class extends HTMLElement {
                    static get observedAttributes() { reads++; return ["a"]; }
                });
                document.body.appendChild(document.createElement("x-at-2"));
                out.push("no-callback-reads=" + reads);
            }

            // 3. the upgrade replay, on an element the fragment parser built with attributes already on
            {
                const log = [];
                const w = document.createElement("div");
                document.body.appendChild(w);
                w.innerHTML = '<x-at-3 a="1" zz="9" b="2"></x-at-3>';
                customElements.define("x-at-3", class extends HTMLElement {
                    static get observedAttributes() { return ["a", "b"]; }
                    constructor() { super(); log.push("ctor"); }
                    connectedCallback() { log.push("conn"); }
                    attributeChangedCallback(n, ov, nv) { log.push(fmt(n, ov, nv)); }
                });
                out.push("upgrade=" + log.join(","));
            }

            // 4. the same replay through customElements.upgrade() on a DETACHED tree: no connect
            {
                const log = [];
                const w = document.createElement("div");
                w.innerHTML = '<x-at-4 a="1"></x-at-4>';
                customElements.define("x-at-4", class extends HTMLElement {
                    static get observedAttributes() { return ["a"]; }
                    constructor() { super(); log.push("ctor"); }
                    connectedCallback() { log.push("conn"); }
                    attributeChangedCallback(n, ov, nv) { log.push(fmt(n, ov, nv)); }
                });
                customElements.upgrade(w);
                out.push("detached-upgrade=" + log.join(","));
            }
            return out.join(" | ");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "writes=attr:a:N>1,attr:a:1>2,attr:a:2>2,attr:a:2>N,attr:a:N>,attr:id:N>i reads=1",
            "no-callback-reads=0",
            "upgrade=ctor,attr:a:N>1,attr:b:N>2,conn",
            "detached-upgrade=ctor,attr:a:N>1",
        ]
        .join(" | "),
        "attributeChangedCallback fires for observed names only, and the upgrade replays them"
    );
}

/// **One element, one constructor run, whichever way it was made.**
///
/// HTML upgrades an element EXACTLY once. FP14 taught the insertion walk to upgrade what it inserts,
/// and that walk did not know an element `document.createElement("x-thing")` or `new XThing()` had
/// ALREADY been constructed -- so appending one ran its definition's constructor a second time on
/// the same object (measured `ctor ctor-end ctor ctor-end` against Chrome's single `ctor`). For a
/// class with a private field the second run is not cosmetic: it throws "Cannot initialize private
/// methods of class E twice on the same object", which is the scar the one-shot rule was written for
/// in the first place. So this asserts the RULE through a constructor that cannot survive running
/// twice, rather than by counting log lines.
#[tokio::test]
async fn a_custom_element_constructor_runs_exactly_once_per_element() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-one-construction",
        r#"(() => {
            const runs = [];
            class Once extends HTMLElement {
                #mine = 1;                       // a second construction on this object THROWS
                constructor() { super(); runs.push(this.tagName.toLowerCase()); }
                own() { return this.#mine; }
            }
            customElements.define("x-once-a", class extends Once {});
            customElements.define("x-once-b", class extends Once {});
            customElements.define("x-once-c", class extends Once {});
            const host = document.createElement("div");
            document.body.appendChild(host);
            const errs = [];
            try { host.appendChild(document.createElement("x-once-a")); } catch (e) { errs.push("createElement:" + e.name); }
            try { const el = new (customElements.get("x-once-b"))(); host.appendChild(el); } catch (e) { errs.push("new:" + e.name); }
            host.insertAdjacentHTML("beforeend", "<x-once-c></x-once-c>");   // parser-built: the walk DOES upgrade it
            const owned = [...host.children].map(el => { try { return el.own(); } catch (e) { return "X"; } }).join("");
            return "runs=" + runs.join(",") + " errs=" + (errs.join(",") || "(none)") + " own=" + owned;
        })()"#
            .to_string(),
    );
    assert_eq!(
        got, "runs=x-once-a,x-once-b,x-once-c errs=(none) own=111",
        "each element's definition constructor runs exactly once, however the element was made"
    );
}

/// **`adoptedCallback`, and the adoption that has to exist for it to mean anything.**
///
/// `document.adoptNode` was documented here as reducing to its removal, on the reasoning that one
/// shared arena makes every node's owner document already this one. It does not: `ownerDocument`
/// answers from the `OwnerMap`, so `d2.adoptNode(el)` handed back an `el` still owned by the page.
/// Measured against Chrome 150 over one fixture on `web-platform.test` — korvus before the change in
/// brackets:
///
/// ```text
///   d2.adoptNode(detachedEl).ownerDocument === d2      true   [FALSE]
///   d2.adoptNode(detachedEl)                           adop   [SILENT]
///   d2.adoptNode(connectedEl)                          disc adop   [SILENT]
///   d2.adoptNode(wrapper) with a custom el inside      disc adop   [SILENT]
/// ```
///
/// THE ORDER IS THE MEASUREMENT: the adopt sits between the removal and any insertion, a DESCENDANT
/// hears it as well as the adopted node, and adopting a node that is already this document's owns
/// nothing and fires nothing.
#[tokio::test]
async fn adopting_a_node_moves_its_document_and_tells_its_custom_elements() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-adopted",
        r#"(() => {
            const out = [];
            const log = [];
            let uid = 0;
            const def = () => {
                const tag = "x-ad-" + (++uid);
                customElements.define(tag, class extends HTMLElement {
                    connectedCallback() { log.push("conn"); }
                    disconnectedCallback() { log.push("disc"); }
                    adoptedCallback(o, n) { log.push("adop:" + (o === document) + ":" + (n === globalThis.__d2)); }
                });
                return tag;
            };
            // Each row's log includes its own SETUP, so the connect that puts an element in the tree
            // is part of the sequence being asserted -- which is how the Chrome fixture reads it too.
            const row = (name, fn) => { log.length = 0; const extra = fn() || ""; out.push(name + "=" + log.join(",") + extra); };
            const d2 = document.implementation.createHTMLDocument("d2");
            globalThis.__d2 = d2;

            row("detached", () => {
                const el = document.createElement(def());
                d2.adoptNode(el);
                return " owner=" + (el.ownerDocument === d2);
            });
            row("connected", () => {
                const h = document.createElement("div"); document.body.appendChild(h);
                const el = document.createElement(def()); h.appendChild(el);
                d2.adoptNode(el);
                return " owner=" + (el.ownerDocument === d2);
            });
            row("descendant", () => {
                const h = document.createElement("div"); document.body.appendChild(h);
                const w = document.createElement("div"); w.appendChild(document.createElement(def()));
                h.appendChild(w);
                d2.adoptNode(w);
                return " owner=" + (w.firstChild.ownerDocument === d2);
            });
            // A node this document already owns is not adopted, and hears nothing.
            row("already-ours", () => {
                const el = document.createElement(def());
                document.adoptNode(el);
                return " owner=" + (el.ownerDocument === document);
            });
            // ...and back out again: the entry follows the node, so the reaction reports the return trip.
            row("back", () => {
                const el = document.createElement(def());
                d2.adoptNode(el);
                log.length = 0;
                document.adoptNode(el);
                return " owner=" + (el.ownerDocument === document);
            });
            return out.join(" | ");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "detached=adop:true:true owner=true",
            "connected=conn,disc,adop:true:true owner=true",
            "descendant=conn,disc,adop:true:true owner=true",
            "already-ours= owner=true",
            "back=adop:false:false owner=true",
        ]
        .join(" | "),
        "adoptNode moves the node's document and runs adoptedCallback for every custom element in it"
    );
}

/// **`importNode` upgrades what it imports, because that is where lit-html's elements
/// get their constructor.**
///
/// DOM's clone creates each element with "create an element" against the TARGET document; for a
/// document with a browsing context that enqueues an upgrade reaction, drained before `importNode`
/// returns. korvus upgraded only on INSERTION, so a custom element imported out of a `<template>`
/// reached JS wearing its definition's prototype with its constructor never run -- the same
/// "a name without a body at instance scope" shape FP14 found for the fragment parser, at the other
/// creation point.
///
/// MEASURED, openlibrary.org replayed off its own recording, and it is why this is not cosmetic.
/// lit-html renders by importing the template, committing property values onto the COPIES, and
/// inserting the fragment last -- so it assigns `element.items = ...` while the copy is still
/// detached. That setter is a Lit reactive-property accessor which `finalize()` had already put on
/// the prototype, and on an instance whose constructor never ran it reaches
/// `this._$changedProperties.has(name)` with `_$changedProperties` undefined:
///
/// ```text
///   TypeError: Cannot read properties of undefined (reading 'has')
///     at requestUpdate ... at set [as items] ... at PropertyPart._$AI ...
/// ```
///
/// One unhandled rejection, `ok` -> `threw`, on a page whose DOM was otherwise byte-identical.
///
/// THE ASSERTION IS THE RULE: an imported element arrives CONSTRUCTED, and it arrives that way while
/// still detached, so nothing it owns is missing by the time a caller touches it -- and no connect
/// reaction has run yet, because it is not connected.
#[tokio::test]
async fn import_node_hands_back_a_constructed_custom_element() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ce-import-node",
        r#"(() => {
            const log = [];
            customElements.define("x-imp", class extends HTMLElement {
                constructor() { super(); this.built = true; log.push("ctor"); }
                connectedCallback() { log.push("conn"); }
            });
            const t = document.createElement("template");
            t.innerHTML = '<div><x-imp id="a"></x-imp></div>';
            const copy = document.importNode(t.content, true);
            const el = copy.querySelector("x-imp");
            // the whole point: the copy is CONSTRUCTED while still detached, so a property set on it
            // here reaches a real instance rather than a bare prototype
            // read BEFORE the insertion: `ctor` has already run and `conn` has not
            const before = "built=" + (el.built === true) + " connected=" + el.isConnected;
            const atImport = log.join(",");
            document.body.appendChild(copy);
            return before + " | " + atImport + "/" + log.join(",") + " | built=" + (el.built === true);
        })()"#
            .to_string(),
    );
    assert_eq!(
        got, "built=true connected=false | ctor/ctor,conn | built=true",
        "importNode upgrades the copy, once, before it is inserted"
    );
}

/// A CSSOM write to `el.style` IS a `style` attribute mutation: an `attributes` record exactly when the block
/// changed (always for `cssText`), its `oldValue` the attribute as it was, and an observed `style`'s
/// `attributeChangedCallback` on every write of the named setter or `cssFloat`, the unchanged and rejected ones too.
/// A Typed OM write's reaction waits for a microtask checkpoint (`StylePropertyMap` has no [CEReactions]): none runs
/// synchronously. korvus queued no record and ran no reaction for any of them, then ran the Typed OM's synchronously.
/// (Chrome runs a page's own `setProperty` / `removeProperty` / `cssText` reaction synchronously too; korvus, which
/// cannot yet tell those from the Typed OM writes that go through them, runs none, and they are not asserted.) Every
/// expected value is Chrome 152's on this machine (headless, 2026-10-23, probe r2/l3/f2/www/t_style.html).
#[tokio::test]
async fn a_cssom_style_write_is_a_style_attribute_mutation() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-style-mutation",
        r#"(() => {
            const out = [];
            const fmt = (rs) => rs.map((x) => x.attributeName + ":" + x.attributeNamespace + ":" + JSON.stringify(x.oldValue)).join(",");
            const o = new MutationObserver(() => {});
            const run = (mk, f, opts) => { const e = mk(); o.observe(e, opts || { attributes: true, attributeOldValue: true }); f(e); out.push(fmt(o.takeRecords())); };
            const div = () => document.createElement("div");
            const styled = () => { const e = div(); e.setAttribute("style", "color: blue;"); return e; };
            run(div, (e) => { e.style.color = "red"; });
            run(styled, (e) => { e.style.color = "red"; });
            run(styled, (e) => { e.style.color = "blue"; });
            run(div, (e) => { e.style.color = "red"; e.style.color = "red"; });
            run(styled, (e) => { e.style.color = "notacolor"; });
            run(styled, (e) => { e.style.setProperty("color", "blue", "important"); });
            run(styled, (e) => { e.style.setProperty("foo", "1"); });
            run(styled, (e) => { e.style.removeProperty("color"); });
            run(styled, (e) => { e.style.removeProperty("width"); });
            run(styled, (e) => { e.style.cssText = "color: blue;"; });
            run(div, (e) => { e.style.cssText = ""; });
            run(div, (e) => { e.style.cssFloat = "left"; });
            run(div, (e) => { e.setAttribute("style", "color:red"); e.style.width = "1px"; });
            run(styled, (e) => { e.style.color = "red"; }, { attributes: true, attributeFilter: ["class"] });
            run(styled, (e) => { e.style.color = "red"; }, { attributes: true });
            run(div, (e) => { e.attributeStyleMap.set("color", "red"); e.attributeStyleMap.delete("color"); });
            const L = [];
            class XS extends HTMLElement {
                static get observedAttributes() { return ["style"]; }
                attributeChangedCallback(n, o, v) { L.push(JSON.stringify(o) + ">" + JSON.stringify(v)); }
            }
            customElements.define("x-style-probe", XS);
            const c = document.createElement("x-style-probe");
            const step = (f) => { L.length = 0; f(); out.push(L.join(",")); };
            step(() => { c.style.color = "red"; });
            step(() => { c.style.color = "red"; });
            step(() => { c.style.color = "zz"; });
            step(() => { c.style.cssFloat = "left"; });
            step(() => { c.attributeStyleMap.set("width", "1px"); });
            step(() => { c.attributeStyleMap.delete("width"); });
            step(() => { c.attributeStyleMap.append("transition-duration", "1s"); });
            step(() => { c.attributeStyleMap.clear(); });
            return out.join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "style:null:null",
            "style:null:\"color: blue;\"",
            "",
            "style:null:null",
            "",
            "style:null:\"color: blue;\"",
            "",
            "style:null:\"color: blue;\"",
            "",
            "style:null:\"color: blue;\"",
            "style:null:null",
            "style:null:null",
            "style:null:null,style:null:\"color:red\"",
            "",
            "style:null:null",
            "style:null:null,style:null:\"color: red;\"",
            "null>\"color: red;\"",
            "\"color: red;\">\"color: red;\"",
            "\"color: red;\">\"color: red;\"",
            "\"color: red;\">\"color: red; float: left;\"",
            "",
            "",
            "",
            "",
        ]
        .join("|")
    );
}

/// `setAttributeNS` / `removeAttributeNS` records name the attribute by its LOCAL name in its namespace, a removal
/// queues one, and an `attributeFilter` never admits a namespaced attribute. korvus named it by its qualified name
/// in no namespace (`xlink:href`, `p:foo`), queued nothing for `removeAttributeNS`, and let `attributeFilter:
/// ['p:foo']` through. Every expected value is Chrome 152's on this machine (headless, 2026-10-23).
#[tokio::test]
async fn a_namespaced_attribute_record_names_its_local_name_and_namespace() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ns-records",
        r##"(() => {
            const XL = "http://www.w3.org/1999/xlink", SVG = "http://www.w3.org/2000/svg";
            const fmt = (rs) => rs.map((x) => x.attributeName + ":" + x.attributeNamespace + ":" + JSON.stringify(x.oldValue)).join(",");
            const out = [];
            const run = (mk, f, opts) => { const o = new MutationObserver(() => {}); const e = mk(); o.observe(e, opts || { attributes: true, attributeOldValue: true }); f(e); out.push(fmt(o.takeRecords())); };
            const use = () => document.createElementNS(SVG, "use"), div = () => document.createElement("div");
            run(use, (e) => { e.setAttributeNS(XL, "xlink:href", "#a"); e.setAttributeNS(XL, "xlink:href", "#b"); });
            run(use, (e) => { e.setAttributeNS(XL, "xlink:href", "#a"); e.removeAttributeNS(XL, "href"); e.removeAttributeNS(XL, "href"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "p:foo", "1"); e.removeAttributeNS("urn:x", "foo"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "foo", "1"); });
            run(div, (e) => { e.setAttributeNS(null, "foo", "1"); e.setAttributeNS("", "bar", "2"); e.removeAttributeNS(null, "foo"); });
            run(div, (e) => { e.setAttributeNS("http://www.w3.org/XML/1998/namespace", "xml:lang", "en"); });
            run(div, (e) => { e.setAttributeNS("http://www.w3.org/2000/xmlns/", "xmlns:z", "urn:z"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "p:foo", "1"); e.setAttributeNS(null, "foo", "2"); }, { attributes: true, attributeFilter: ["foo"] });
            run(div, (e) => { e.setAttributeNS("urn:x", "p:foo", "1"); }, { attributes: true, attributeFilter: ["p:foo"] });
            return out.join("|");
        })()"##
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "href:http://www.w3.org/1999/xlink:null,href:http://www.w3.org/1999/xlink:\"#a\"",
            "href:http://www.w3.org/1999/xlink:null,href:http://www.w3.org/1999/xlink:\"#a\"",
            "foo:urn:x:null,foo:urn:x:\"1\"",
            "foo:urn:x:null",
            "foo:null:null,bar:null:null,foo:null:\"1\"",
            "lang:http://www.w3.org/XML/1998/namespace:null",
            "z:http://www.w3.org/2000/xmlns/:null",
            "foo:null:null",
            "",
        ]
        .join("|")
    );
}

/// A CSSOM style write whose argument's conversion throws (a throwing `toString`, a Symbol) throws and writes
/// nothing: no declaration, no `style` record, no reaction, the attribute's text as it was. The writers converted
/// with a lossy `""` and wrote that (a throwing `cssText` emptied the block), and once a write was a `style`
/// mutation the record or reaction it ran swallowed the exception. Every expected value is Chrome 152's on this
/// machine (headless, 2026-10-23): an observed `<div>` on a page that defines a custom element, then a custom element
/// observing `style`, through the named setter, `setProperty` (value, priority), `cssText`, `cssFloat`,
/// `removeProperty` and a Symbol.
#[tokio::test]
async fn a_cssom_style_write_whose_conversion_throws_writes_nothing() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-style-throws",
        r#"(() => {
            const out = [];
            const bad = { toString() { throw new Error("x"); } };
            const fmt = (rs) => rs.map((x) => x.attributeName + ":" + JSON.stringify(x.oldValue)).join(",") || "none";
            const L = [];
            customElements.define("x-throw-probe", class extends HTMLElement {
                static get observedAttributes() { return ["style"]; }
                attributeChangedCallback(n, o, v) { L.push(JSON.stringify(o) + ">" + JSON.stringify(v)); }
            });
            const acts = [
                (e) => { e.style.color = bad; },
                (e) => { e.style.setProperty("color", bad); },
                (e) => { e.style.setProperty("color", "red", bad); },
                (e) => { e.style.cssText = bad; },
                (e) => { e.style.cssFloat = bad; },
                (e) => { e.style.removeProperty(bad); },
                (e) => { e.style.color = Symbol(); },
            ];
            for (const mk of [() => document.createElement("div"), () => document.createElement("x-throw-probe")]) {
                for (const f of acts) {
                    const e = mk();
                    e.setAttribute("style", "width:1px");
                    const o = new MutationObserver(() => {});
                    o.observe(e, { attributes: true, attributeOldValue: true });
                    L.length = 0;
                    let th = "nothrow";
                    try { f(e); } catch (err) { th = "threw"; }
                    out.push(th + " " + fmt(o.takeRecords()) + " " + (L.join(",") || "none") + " " + e.getAttribute("style"));
                    o.disconnect();
                }
            }
            return out.join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
            "threw none none width:1px",
        ]
        .join("|")
    );
}

/// `removeAttributeNS` removes the attribute in THAT namespace with that local name and nothing else, and its
/// arguments' conversions end the call when they throw. It took the first attribute whose local name matched in any
/// namespace -- removing it with a record, a reaction, a canvas reset where Chrome removes nothing -- and went on
/// with `""` past a throwing namespace. The namespace an attribute is in is kept for what `setAttributeNS` made (a
/// clone keeps it), and the records of `setAttribute` / `toggleAttribute` / `removeAttribute` name a namespaced one
/// by its local name in it; `setAttributeNS` over an attribute in its namespace under another prefix changes that
/// one. Every expected value is Chrome 152's on this machine (headless, 2026-10-23).
#[tokio::test]
async fn remove_attribute_ns_removes_only_in_its_namespace() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-remove-ns",
        r##"(() => {
            const XL = "http://www.w3.org/1999/xlink", SVG = "http://www.w3.org/2000/svg";
            const fmt = (rs) => rs.map((x) => x.attributeName + ":" + x.attributeNamespace + ":" + JSON.stringify(x.oldValue)).join(",") || "none";
            const attrs = (e) => Array.from(e.attributes).map((a) => a.name + "=" + a.value).join(",");
            const out = [];
            const run = (mk, f) => {
                const o = new MutationObserver(() => {});
                const e = mk();
                o.observe(e, { attributes: true, attributeOldValue: true });
                let th = "nothrow";
                try { f(e); } catch (err) { th = "threw"; }
                out.push(th + " " + fmt(o.takeRecords()) + " [" + attrs(e) + "]");
            };
            const div = () => document.createElement("div"), use = () => document.createElementNS(SVG, "use");
            const parsed = () => { const d = document.createElement("div"); d.innerHTML = '<svg><use xlink:href="#a" href="#b"></use></svg>'; return d.querySelector("use"); };
            const bad = { toString() { throw new Error("x"); } };
            run(div, (e) => { e.setAttribute("foo", "1"); e.removeAttributeNS("urn:x", "foo"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "p:foo", "1"); e.removeAttributeNS(null, "foo"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "p:foo", "1"); e.removeAttributeNS("urn:y", "foo"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "foo", "1"); e.removeAttributeNS(null, "foo"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "p:foo", "1"); e.removeAttributeNS("urn:x", "foo"); });
            run(use, (e) => { e.setAttributeNS(XL, "xlink:href", "#a"); e.removeAttributeNS(XL, "xlink:href"); });
            run(use, (e) => { e.setAttribute("xlink:href", "#a"); e.removeAttributeNS(XL, "href"); });
            run(parsed, (e) => { e.removeAttributeNS(null, "href"); });
            run(parsed, (e) => { e.removeAttributeNS(XL, "href"); });
            run(parsed, (e) => { e.setAttributeNS(XL, "q:href", "#q"); });
            run(parsed, (e) => { e.removeAttribute("xlink:href"); });
            run(div, (e) => { e.setAttributeNS("urn:x", "p:foo", "1"); e.setAttribute("p:foo", "2"); e.toggleAttribute("p:foo"); });
            run(div, (e) => { const s = div(); s.setAttributeNS("urn:x", "p:foo", "1"); const c = s.cloneNode(); c.removeAttributeNS("urn:x", "foo"); e.setAttribute("n", c.attributes.length); });
            run(div, (e) => { e.setAttribute("foo", "1"); e.removeAttributeNS(bad, "foo"); });
            run(div, (e) => { e.setAttribute("foo", "1"); e.removeAttributeNS(Symbol(), "foo"); });
            run(div, (e) => { e.setAttribute("foo", "1"); e.removeAttributeNS(null, bad); });
            run(div, (e) => { e.setAttribute("foo", "1"); e.setAttributeNS(bad, "foo", "2"); });
            run(div, (e) => { e.setAttribute("foo", "1"); e.setAttributeNS(null, bad, "2"); });
            const L = [];
            customElements.define("x-ns-probe", class extends HTMLElement {
                static get observedAttributes() { return ["foo"]; }
                attributeChangedCallback(n, o, v) { L.push(n + ":" + JSON.stringify(o) + ">" + JSON.stringify(v)); }
            });
            const c = document.createElement("x-ns-probe");
            c.setAttributeNS("urn:x", "p:foo", "1");
            L.length = 0;
            c.removeAttributeNS("urn:q", "foo");
            try { c.removeAttributeNS(bad, "foo"); } catch (err) { L.push("threw"); }
            c.removeAttributeNS("urn:x", "foo");
            out.push(L.join(",") || "none");
            const cv = document.createElement("canvas");
            cv.width = 10;
            const g = cv.getContext("2d");
            g.fillRect(0, 0, 10, 10);
            cv.removeAttributeNS("urn:x", "width");
            out.push(cv.width + "," + g.getImageData(0, 0, 1, 1).data[3]);
            return out.join("|");
        })()"##
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "nothrow foo:null:null [foo=1]",
            "nothrow foo:urn:x:null [p:foo=1]",
            "nothrow foo:urn:x:null [p:foo=1]",
            "nothrow foo:urn:x:null [foo=1]",
            "nothrow foo:urn:x:null,foo:urn:x:\"1\" []",
            "nothrow href:http://www.w3.org/1999/xlink:null [xlink:href=#a]",
            "nothrow xlink:href:null:null [xlink:href=#a]",
            "nothrow href:null:\"#b\" [xlink:href=#a]",
            "nothrow href:http://www.w3.org/1999/xlink:\"#a\" [href=#b]",
            "nothrow href:http://www.w3.org/1999/xlink:\"#a\" [xlink:href=#q,href=#b]",
            "nothrow href:http://www.w3.org/1999/xlink:\"#a\" [href=#b]",
            "nothrow foo:urn:x:null,foo:urn:x:\"1\",foo:urn:x:\"2\" []",
            "nothrow n:null:null [n=0]",
            "threw foo:null:null [foo=1]",
            "threw foo:null:null [foo=1]",
            "threw foo:null:null [foo=1]",
            "threw foo:null:null [foo=1]",
            "threw foo:null:null [foo=1]",
            "threw,foo:\"1\">null",
            "10,255",
        ]
        .join("|")
    );
}

/// An attribute's namespace is kept by its element, for as long as the attribute is there, and copied by every clone:
/// one element's `setAttributeNS('urn:x', 'foo')` changes how no other element's `foo` is named, an attribute a
/// `setAttribute` makes after a removal that went around the table (`delete el.dataset.k`) is in no namespace, and a
/// `<template>`'s content keeps its namespaces through `cloneNode(true)`. korvus named the remade `data-k` in `urn:x`
/// and lost the cloned content's namespaces (`removeAttributeNS('urn:x', 'foo')` removed nothing). Every expected
/// value is Chrome 152's on this machine (headless, 2026-10-23, probe r2/l3/f2/www/t_ns.html).
#[tokio::test]
async fn an_attribute_namespace_is_kept_by_its_element_and_its_clones() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11-ns-kept",
        r##"(() => {
            const XL = "http://www.w3.org/1999/xlink", SVG = "http://www.w3.org/2000/svg";
            const fmt = (rs) => rs.map((x) => x.attributeName + ":" + x.attributeNamespace + ":" + JSON.stringify(x.oldValue)).join(",") || "none";
            const attrs = (e) => Array.from(e.attributes).map((a) => a.name + "=" + a.value).join(",");
            const out = [];
            const run = (mk, f) => {
                const o = new MutationObserver(() => {});
                const e = mk();
                o.observe(e, { attributes: true, attributeOldValue: true });
                let th = "nothrow";
                try { f(e); } catch (err) { th = "threw"; }
                out.push(th + " " + fmt(o.takeRecords()) + " [" + attrs(e) + "]");
            };
            const div = () => document.createElement("div"), use = () => document.createElementNS(SVG, "use");
            const kept = (mk, ns, name) => () => { const e = mk(); e.setAttributeNS(ns, name, "1"); return e; };
            run(() => { const d = div(); d.setAttributeNS("urn:x", "data-k", "1"); delete d.dataset.k; return d; }, (e) => { e.setAttribute("data-k", "2"); });
            run(() => { const d = use(); d.setAttributeNS("urn:x", "p:k", "1"); d.removeAttributeNS("urn:x", "k"); return d; }, (e) => { e.setAttribute("p:k", "2"); e.removeAttribute("p:k"); });
            run(() => { const t = document.createElement("template"); t.innerHTML = "<b></b>"; t.content.firstChild.setAttributeNS("urn:x", "p:foo", "1"); return t.cloneNode(true).content.firstChild; }, (e) => { e.removeAttributeNS("urn:x", "foo"); });
            run(() => { const t = document.createElement("template"); t.innerHTML = "<i><b></b></i>"; t.content.firstChild.firstChild.setAttributeNS(XL, "xlink:href", "#a"); const h = document.createElement("div"); h.appendChild(t); return h.cloneNode(true).firstChild.content.firstChild.firstChild; }, (e) => { e.setAttribute("xlink:href", "#b"); });
            run(() => { kept(div, "urn:x", "foo")(); return div(); }, (e) => { e.setAttribute("foo", "1"); e.removeAttribute("foo"); });
            run(kept(div, "urn:x", "foo"), (e) => { e.setAttribute("foo", "2"); e.removeAttribute("foo"); e.setAttribute("foo", "3"); });
            run(kept(use, XL, "href"), (e) => { e.setAttributeNS(XL, "href", "#b"); e.setAttribute("href", "#c"); });
            run(kept(div, "urn:x", "p:foo"), (e) => { e.setAttributeNS(null, "foo", "2"); e.setAttribute("p:foo", "3"); });
            run(div, (e) => { e.setAttribute("p:foo", "1"); e.removeAttribute("p:foo"); e.setAttribute("xmlns", "urn:z"); });
            run(use, (e) => { e.setAttribute("xlink:href", "#a"); e.removeAttribute("xlink:href"); e.setAttributeNS(XL, "xlink:href", "#b"); });
            return out.join("|");
        })()"##
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "nothrow data-k:null:null [data-k=2]",
            "nothrow p:k:null:null,p:k:null:\"2\" []",
            "nothrow foo:urn:x:\"1\" []",
            "nothrow href:http://www.w3.org/1999/xlink:\"#a\" [xlink:href=#b]",
            "nothrow foo:null:null,foo:null:\"1\" []",
            "nothrow foo:urn:x:\"1\",foo:urn:x:\"2\",foo:null:null [foo=3]",
            "nothrow href:http://www.w3.org/1999/xlink:\"1\",href:http://www.w3.org/1999/xlink:\"#b\" [href=#c]",
            "nothrow foo:null:null,foo:urn:x:\"1\" [p:foo=3,foo=2]",
            "nothrow p:foo:null:null,p:foo:null:\"1\",xmlns:null:null [xmlns=urn:z]",
            "nothrow xlink:href:null:null,xlink:href:null:\"#a\",href:http://www.w3.org/1999/xlink:null [xlink:href=#b]",
        ]
        .join("|")
    );
}
