//! `core/dom/spine.rs` -- the DOM inheritance spine, built by THE installer from tables.
//!
//! What `dom/07-spine.js`, `08a-tag-protos.js`, `08b-reflected.js`, `09g-html-element.js`,
//! `09l-element-internals.js`, `06-misc.js`, `02-events.js` and `01-interning.js`'s DOM half did in
//! 2,000 lines of JS -- mint `Node`/`Element`/`HTMLElement`/... , relocate members between prototypes
//! by name lists, delete-and-reinstall `HTMLElement.prototype` in the recorded order, swap native
//! constructors in, register the wrapper-retype maps -- is one Rust pass over eighteen `interface!`
//! tables, in WebIDL inheritance order, through `installer::create_interface` /
//! `installer::overwrite_prototype`. Every own member of every spine prototype is a table row with a
//! Rust body, mounted once, in table order, and ledgered.
//!
//! WHEN IT RUNS. [`install_dom_spine`] is called from `install::live::install_window_bootstrap`, at
//! the point the `Document`/`Element` globals used to be assigned (it was
//! `dom/00-bootstrap.js`'s last statement): after `host.js` (so `EventTarget.prototype`, the
//! chain's root, exists) and before every other bootstrap phase and the registry's create pass, which
//! skips the names already on the global and chains every `HTML*Element` row onto the table's
//! `HTMLElement.prototype` by name. The wrapper-retype maps are filled AFTER the registry has created
//! the `HTML*Element`/`SVG*Element` prototypes (`install::dom_chain::fill_dom_protos`, from the
//! runtime's context-creation hook).
//!
//! TWO PROTOTYPES ARE OVERWRITTEN IN PLACE, NOT MINTED. `HTMLElement.prototype` IS the cppgc
//! `JsElement` class prototype and `Document.prototype` IS the cppgc `JsDocument` class prototype
//! (`installer::overwrite_prototype`, the W5.0b mechanism): `make_cppgc_object` hands every wrapper
//! the class prototype before `node_proto` retypes it, and `node_proto` deliberately leaves an
//! undefined custom-element tag on that prototype -- which is exactly `HTMLElement`, so the class
//! prototype must BE `HTMLElement.prototype`. The `#[op2] impl` blocks keep no web-visible member, so
//! after the overwrite those prototypes carry the table's rows and nothing else. `Text`, `Comment`,
//! `DocumentFragment` get native constructors the way `Observable` does (`ConstructorBehavior::Allow`,
//! with a fresh prototype the table is installed onto); everything else is
//! `create_interface`, which mints an "Illegal constructor" interface object.
//!
//! NodeFilter is a CALLBACK interface: `typeof NodeFilter === "function"`, no `prototype`, the
//! sixteen constants as own properties, `new` "is not a constructor", a call "Illegal constructor"
//! (Chrome, 2026-09-10). It is a table of `static_constant` rows mounted by the one member mount onto a
//! plain native function -- no prototype is minted because the interface has none.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, build_interface_ctor, cb_illegal_ctor, global_get};
use crate::core::dom::attr::ATTR;
use crate::core::dom::character_data::{CHARACTERDATA, COMMENT, PROCESSINGINSTRUCTION};
use crate::core::dom::document::{cb_document_ctor, cb_fragment_ctor, DOCUMENT};
use crate::core::dom::document_fragment::{DOCUMENTFRAGMENT, SHADOWROOT};
use crate::core::dom::document_type::DOCUMENTTYPE;
use crate::core::dom::element::ELEMENT;
use crate::core::dom::node::NODE;
use crate::core::dom::node_iterator::NODEITERATOR;
use crate::core::dom::text::{cb_comment_ctor, cb_text_ctor, CDATASECTION, TEXT};
use crate::core::dom::tree_walker::{NODEFILTER, TREEWALKER};
use crate::core::html::element_internals::{CUSTOMSTATESET, ELEMENTINTERNALS};
use crate::core::html::html_element::{cb_html_element_ctor, HTMLELEMENT};
use crate::install::installer::{self, create_interface, overwrite_prototype, put_member, Flavor, InstallLedger, Pass};
use crate::install::InterfaceDef;

/// A (prototype, constructor) pair -- the `parent` shape `create_interface` splices onto.
type Pair<'s> = (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>);

fn str_key<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap()
}

/// `globalThis[name]` as a (prototype, constructor) pair.
fn pair_of<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<Pair<'s>> {
    let c = global_get(scope, name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let pk = str_key(scope, "prototype");
    let p = c.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    Some((p, c))
}

/// `ctor.prototype = proto` with WebIDL's `{ writable: false, enumerable: false, configurable: false }`.
fn define_prototype(scope: &mut v8::PinScope, ctor: v8::Local<v8::Function>, proto: v8::Local<v8::Object>) {
    let pk = str_key(scope, "prototype");
    ctor.define_own_property(
        scope,
        pk.into(),
        proto.into(),
        v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::DONT_DELETE,
    );
}

/// The WebIDL inheritance splice: the prototype chain AND the constructor chain.
fn chain<'s>(scope: &mut v8::PinScope<'s, '_>, child: Pair<'s>, parent: Pair<'s>) {
    child.0.set_prototype(scope, parent.0.into());
    child.1.set_prototype(scope, parent.1.into());
}

/// A non-newable interface from its table, spliced onto `parent`, on the global.
fn create<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, def: &'static InterfaceDef, parent: Option<Pair<'s>>, ledger: &mut InstallLedger) -> Pair<'s> {
    let built = create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] dom spine install gap: {}.{}", g.interface, g.member);
    }
    (built.proto, built.ctor)
}

/// A NEWABLE interface: a native constructor (`cb`) whose non-writable `.prototype` is `proto` --
/// a fresh object, or the cppgc class prototype the wrappers are minted on -- with the table installed
/// onto that prototype (`constructor` last, the tag, the ctor's WebIDL length), on the global.
fn newable<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    proto: v8::Local<'s, v8::Object>,
    parent: Option<Pair<'s>>,
    ledger: &mut InstallLedger,
) -> Pair<'s> {
    let len = def.ctor.as_ref().map(|c| c.length as i32).unwrap_or(0);
    // An interface on a FRESH prototype takes the same template step as
    // `installer::create`. One whose prototype is a cppgc class's (the one `make_cppgc_object` mints its
    // wrappers on: `HTMLElement`, `Document`) is not a template of ours yet -- that is the CPPGC adoption
    // -- and builds the ordinary way in every realm.
    // (every own key counts: a class prototype's `constructor` is non-enumerable)
    let all_keys = v8::GetPropertyNamesArgs {
        mode: v8::KeyCollectionMode::OwnOnly,
        property_filter: v8::PropertyFilter::ALL_PROPERTIES,
        index_filter: v8::IndexFilter::IncludeIndices,
        key_conversion: v8::KeyConversionMode::KeepNumbers,
    };
    let fresh = proto.get_own_property_names(scope, all_keys).is_some_and(|k| k.length() == 0);
    let step = if fresh {
        Some(installer::tpl_step(scope, def, def.name, None, "spine", crate::install::Exposed::Both, ledger))
    } else {
        None
    };
    if let Some(installer::TplStep::Instantiated(ctor, proto, _gaps)) = step {
        crate::install::installer::publish_on_global(scope, global, def.name, ctor.into());
        if let Some(p) = parent {
            chain(scope, (proto, ctor), p);
        }
        return (proto, ctor);
    }
    // `build_interface_ctor`, which binds the interface NAME as the
    // constructor's `data`: which is how `cb_html_element_ctor` (and every other body that has to
    // spell `Failed to construct 'X': …`) knows what it is called. A body that ignores `data` is
    // unaffected.
    let ctor = build_interface_ctor(scope, cb, def.name, len);
    define_prototype(scope, ctor, proto);
    crate::install::installer::publish_on_global(scope, global, def.name, ctor.into());
    if let Err(e) = overwrite_prototype(scope, ctor, def, ledger) {
        eprintln!("[korvus] dom spine: {e}");
    }
    let (ctor, proto) = match step {
        Some(installer::TplStep::Build(key, mark)) => match installer::tpl_done(scope, key, mark, def.name, def.parent, ctor, proto, ledger, &[]) {
            // the template's pair replaces the one published above (`None`: no template, a refused interface)
            Some((c, p)) => {
                crate::install::installer::publish_on_global(scope, global, def.name, c.into());
                (c, p)
            }
            None => (ctor, proto),
        },
        _ => (ctor, proto),
    };
    if let Some(p) = parent {
        chain(scope, (proto, ctor), p);
    }
    (proto, ctor)
}

/// The `NodeFilter` legacy callback-interface object: a native function with the constant rows on it
/// and no `prototype` (`build_fn`'s `ConstructorBehavior::Throw` makes `new NodeFilter()` "is not a
/// constructor"; the body makes a call "Illegal constructor").
///
/// THE ONE INTERFACE OBJECT ON THE WHOLE SURFACE THAT KEEPS `Throw`. Measured on
/// `google-chrome --headless=new` 150.0.7871.46 over loopback, 2026-09-12:
/// `new NodeFilter()` is `TypeError: NodeFilter is not a constructor`,
/// `Reflect.construct(NodeFilter, [])` is `TypeError: function NodeFilter() { [native code] } is not
/// a constructor` -- V8 naming the expression, because there is no `[[Construct]]` to enter -- and
/// `NodeFilter()` is `TypeError: Illegal constructor`, `NodeFilter.prototype` `undefined`. Every
/// OTHER non-newable interface object in Chrome is constructible and throws the prefixed message
/// from its body; this one is a legacy callback interface and is not. `build_fn_data` rather than
/// `build_fn` only so the shared body can still name itself if it is ever reached on a construct.
///
/// Through the template step like every other interface object: a template for a function
/// with no `prototype`, its constants on the template.
fn install_node_filter<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut InstallLedger) {
    let f = match installer::tpl_step(scope, &NODEFILTER, "NodeFilter", None, "nodefilter", crate::install::Exposed::Both, ledger) {
        installer::TplStep::Instantiated(ctor, _, _) => ctor,
        installer::TplStep::Build(key, mark) => {
            let d = crate::binding::protos::str_val(scope, "NodeFilter");
            let f = build_fn_data(scope, cb_illegal_ctor, "NodeFilter", 0, d);
            let f_obj: v8::Local<v8::Object> = f.into();
            for m in NODEFILTER.members.iter().filter(|m| m.static_member) {
                put_member(scope, f_obj, m, "NodeFilter", false, ledger, Pass::Create, Flavor::Table);
            }
            // the translation reads a prototype; this interface object has none, so it reads an empty one
            let none = v8::Object::new(scope);
            match installer::tpl_done(scope, key, mark, "NodeFilter", None, f, none, ledger, &[]) {
                Some((c, _)) => c,
                None => f,
            }
        }
    };
    crate::install::installer::publish_on_global(scope, global, "NodeFilter", f.into());
}

/// Build the spine on the current realm's global, in inheritance order. Every row goes through the one
/// member mount and into the context's install ledger.
pub fn install_dom_spine(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);

    let event_target = pair_of(scope, "EventTarget");
    let node = create(scope, global, &NODE, event_target, &mut ledger);
    let element = create(scope, global, &ELEMENT, Some(node), &mut ledger);

    // HTMLElement: the table onto the cppgc `JsElement` class prototype, behind the spec's element
    // constructor (custom-element construction and upgrade; see `cb_html_element_ctor`).
    //
    // A FRESH PROTOTYPE, not the class's: the interface takes the template step like every
    // other spine interface, and `binding::protos::class_proto::<JsElement>` answers the realm's
    // `HTMLElement.prototype` for a wrapper. The class prototype is kept only as where deno_core puts
    // the class's own symbol-keyed members, which still move to `Node.prototype`.
    // (This once waited on deno_core's `JsElement` class template and moved its symbol-keyed
    // members to `Node.prototype`; the class template is gone, and it had no such members left.)
    {
        let fresh = v8::Object::new(scope);
        newable(scope, global, &HTMLELEMENT, cb_html_element_ctor, fresh, Some(element), &mut ledger);
    }

    let character_data = create(scope, global, &CHARACTERDATA, Some(node), &mut ledger);
    let text_proto = v8::Object::new(scope);
    let text = newable(scope, global, &TEXT, cb_text_ctor, text_proto, Some(character_data), &mut ledger);
    let comment_proto = v8::Object::new(scope);
    newable(scope, global, &COMMENT, cb_comment_ctor, comment_proto, Some(character_data), &mut ledger);
    create(scope, global, &CDATASECTION, Some(text), &mut ledger);
    create(scope, global, &PROCESSINGINSTRUCTION, Some(character_data), &mut ledger);
    create(scope, global, &DOCUMENTTYPE, Some(node), &mut ledger);
    let fragment_proto = v8::Object::new(scope);
    let fragment = newable(scope, global, &DOCUMENTFRAGMENT, cb_fragment_ctor, fragment_proto, Some(node), &mut ledger);
    create(scope, global, &SHADOWROOT, Some(fragment), &mut ledger);

    // Document: the table onto the cppgc `JsDocument` class prototype (the page document and every
    // nested document are minted on it), behind a native constructor that builds an empty XML document.
    // A fresh prototype, as HTMLElement's above: `class_proto::<JsDocument>` answers the realm's
    // `Document.prototype` for a document wrapper.
    {
        let fresh = v8::Object::new(scope);
        let document = newable(scope, global, &DOCUMENT, cb_document_ctor, fresh, Some(node), &mut ledger);
        // DOMParser / XMLSerializer / DOMImplementation, and the two names a document's
        // kind is read through -- HTMLDocument and XMLDocument, spliced onto Document right here, so a
        // parsed document wearing one of their prototypes answers every Document member.
        crate::core::xml::install_xml(scope, global, document, &mut ledger);
    }

    create(scope, global, &ATTR, Some(node), &mut ledger);
    // TreeWalker / NodeIterator: non-newable tables over OWN classes -- `binding::class::mint`
    // puts a walker on the realm's prototype, so the interface is an ordinary one, recorded by the templates.
    create(scope, global, &TREEWALKER, None, &mut ledger);
    create(scope, global, &NODEITERATOR, None, &mut ledger);
    install_node_filter(scope, global, &mut ledger);
    create(scope, global, &ELEMENTINTERNALS, None, &mut ledger);
    create(scope, global, &CUSTOMSTATESET, None, &mut ledger);

    create_html_element_shells(scope, global, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// The bare `HTML*Element` interface objects, in the registry's order, spliced under `HTMLElement` --
/// what `dom/07-spine.js`'s `mk()` list provided at phase 7 and the hand-written element phases
/// (08b..09d, and the Rust installs they trigger: canvas, form, media, ...) run against BEFORE the
/// registry's create pass. Each is `create_interface` with an EMPTY selector: constructor, tag and
/// chain, no members -- the members are the registry's rows, mounted by its backfill pass exactly as
/// before (a member a hand phase defined first keeps winning, `has_own_property`). The set is derived
/// from the registry's own parent links (every `HTML*Element` def whose ancestry reaches
/// `HTMLElement`), not from a list.
/// Is `name` one of the interfaces [`create_html_element_shells`] creates with an EMPTY selector?
///
/// **THIS PREDICATE AND `backfill: true` ARE A PAIR**. An interface created empty gets its
/// members from exactly one later pass -- `installer::backfill_members`, which skips any row whose
/// `InterfaceDef.backfill` is false. So an `HTML*Element` shell that is not backfillable has NO members
/// at all, from any pass, ever, and nothing says so: the prototype is built, chained and published, and
/// it is simply bare.
///
/// That is not hypothetical. `HTMLUserMediaElement` -- Chromium 151, carried by Edge 151 and Opera 135,
/// undeclared by webref -- had all six of its generated rows and none of them installed, which
/// `persona_surface` read as six "capture members with no row" under two bundles. The generator's gate
/// is fixed (`tools/sync/gen_shells.py::html_element_shell`); this function and
/// `tests/generated_registry.rs::every_html_element_shell_is_backfillable` are what stop the pair from
/// drifting apart again, because the failure mode is silence.
///
/// `HTMLElement` itself is excluded: it is created by the registry's own pass, with its whole table.
pub fn is_html_element_shell(name: &str) -> bool {
    name.starts_with("HTML") && name.ends_with("Element") && name != "HTMLElement"
}

fn create_html_element_shells<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut InstallLedger) {
    use crate::install::generated_registry::REGISTRY;
    let by_name: std::collections::HashMap<&str, &'static InterfaceDef> = REGISTRY.iter().map(|d| (d.name, *d)).collect();
    let descends_from_html_element = |name: &str| -> bool {
        let mut cur = name;
        for _ in 0..16 {
            let Some(d) = by_name.get(cur) else { return false };
            match d.parent {
                Some("HTMLElement") => return true,
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    };
    let wanted: Vec<&'static InterfaceDef> = REGISTRY
        .iter()
        .copied()
        .filter(|d| is_html_element_shell(d.name) && descends_from_html_element(d.name))
        .collect();
    // Parents before children: a pass creates what its parent already allows, until nothing is left.
    let mut pending = wanted;
    loop {
        let before = pending.len();
        let mut rest = Vec::new();
        for d in pending {
            let k = str_key(scope, d.name);
            if global.get(scope, k.into()).map(|v| !v.is_undefined()).unwrap_or(false) {
                continue;
            }
            let Some(parent) = d.parent.and_then(|p| pair_of(scope, p)) else { rest.push(d); continue };
            // A shell the member backfill finishes takes its rows inside this create, so
            // they are recorded with it; `backfill_window_members` then finds them owned.
            let built = if d.backfill && d.exposed.covers(crate::install::Exposed::Window) {
                crate::install::installer::create_interface_backfilled(scope, global, d, d.name, Some(parent), Some(&[]), Flavor::Table, crate::install::Exposed::Window, ledger)
            } else {
                create_interface(scope, global, d, d.name, Some(parent), Some(&[]), Flavor::Table, ledger)
            };
            for g in &built.gaps {
                eprintln!("[korvus] dom spine install gap: {}.{}", g.interface, g.member);
            }
        }
        if rest.is_empty() || rest.len() == before {
            for d in rest {
                eprintln!("[korvus] dom spine: {} has no parent on the global; not created", d.name);
            }
            break;
        }
        pending = rest;
    }
}

// THE OP IS GONE. `op_install_dom_spine` existed because the caller was one line of
// `js/dom/00-bootstrap.js`; that file is deleted and
// `install::live::install_window_bootstrap` calls [`install_dom_spine`] directly, at the same point.
