//! Korvus GS ②-B2: Shadow-DOM slot webapi surface end-to-end from page JS.
//! Exercises HTMLSlotElement.assignedNodes / assignedElements (incl. the `flatten` fallback),
//! the Slottable-mixin Element/Text.assignedSlot (the inverse edge), the reflected `name` IDL
//! attribute, and `::slotted()` shadow-scoped matching via ShadowRoot.querySelector[All].
use engine::Engine;
#[tokio::test]
async fn shadow_slot_webapi_surface_is_usable_from_js() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    // Host's light children are written with NO inter-element whitespace: any text between them
    // would itself be a slottable assigned to the default slot, perturbing the assertions.
    e.load_html(
        r#"<!doctype html><html><body>
             <div id="host"><span slot="a" id="s">span</span><p id="p">para</p><u slot="zzz" id="orphan">u</u></div>
           </body></html>"#,
    )
    .unwrap();
    e.eval(
        "shadow_slots",
        r##"
        try {
          const host = document.getElementById("host");
          const sr = host.attachShadow({ mode: "open" });
          // Shadow tree: a named slot "a", the default slot (with fallback <i>, but assigned <p>),
          // and an unmatched-name slot "empty" whose fallback <i> is what flatten must surface.
          sr.innerHTML =
            '<slot name="a" id="namedSlot"></slot>' +
            '<slot id="defaultSlot"><i id="dfb">fd</i></slot>' +
            '<slot name="empty" id="emptySlot"><i id="efb">fe</i></slot>';

          const named = sr.getElementById("namedSlot");
          const dflt  = sr.getElementById("defaultSlot");
          const empty = sr.getElementById("emptySlot");
          const s      = document.getElementById("s");
          const p      = document.getElementById("p");
          const orphan = document.getElementById("orphan");
          const ids = (a) => Array.prototype.map.call(a, (n) => n.id).join(",");

          // (1) assignedNodes(): the directly-assigned light children, in tree order.
          print("namedAssigned=" + ids(named.assignedNodes()));      // s
          print("defaultAssigned=" + ids(dflt.assignedNodes()));     // p

          // (2) assignedElements(): assignedNodes filtered to Element nodes.
          print("defaultElements=" + ids(dflt.assignedElements()));  // p

          // (3) assignedSlot: the inverse edge, a slotted light node -> its <slot> (canonical identity).
          print("spanSlot=" + (s.assignedSlot === named));           // true
          print("pSlot=" + (p.assignedSlot === dflt));               // true
          // (4) an unmatched slot name -> assignedSlot === null (never assigned).
          print("orphanSlot=" + (orphan.assignedSlot === null));     // true

          // (5) an empty slot: assignedNodes() is [], but flatten:true surfaces its fallback children.
          print("emptyAssigned=" + empty.assignedNodes().length);              // 0
          print("emptyFlatten=" + ids(empty.assignedNodes({ flatten: true }))); // efb

          // (6) the `name` IDL attribute reflects the `name` content attribute.
          print("slotName=" + named.name);                          // a

          // (7) ::slotted() — shadow-scoped, matches a slot's ASSIGNED light nodes (not shadow content).
          print("slottedSpan=" + ids(sr.querySelectorAll("::slotted(span)")));  // s
          print("slottedStar=" + ids(sr.querySelectorAll("::slotted(*)")));     // s,p
          // orphan (<u slot="zzz">) is assigned to NO slot, so ::slotted never matches it.
          print("slottedU=" + sr.querySelectorAll("::slotted(u)").length);      // 0
          // a qualified slot compound before ::slotted crosses to the assigned slot and matches it.
          print("slottedQualified=" + (sr.querySelector('slot[name="a"]::slotted(span)') === s)); // true
        } catch (e) {
          print("ERR=" + (e && e.stack ? e.stack : e));
        }
        "##
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec![
            "namedAssigned=s",
            "defaultAssigned=p",
            "defaultElements=p",
            "spanSlot=true",
            "pSlot=true",
            "orphanSlot=true",
            "emptyAssigned=0",
            "emptyFlatten=efb",
            "slotName=a",
            "slottedSpan=s",
            "slottedStar=s,p",
            "slottedU=0",
            "slottedQualified=true",
        ]
    );
}
