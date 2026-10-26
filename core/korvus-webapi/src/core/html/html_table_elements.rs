//! `core/html/html_table_elements.rs` -- the six table interfaces as six `interface!` tables.
//!
//! `HTMLTableElement`, `HTMLTableRowElement`, `HTMLTableSectionElement`, `HTMLTableCellElement`,
//! `HTMLTableColElement` and `HTMLTableCaptionElement` used to be six generated registry blocks whose
//! algorithmic members were patched over from `dom/08s-table-element.js` (15 `KB.__tbl*` bridge slots,
//! 4 native masks, 5 `Object.defineProperty` sites) and `dom/08x-table-parts.js` (7 `KB.__tr*`/`__ts*`/
//! `__tc*`, 2 masks, 2 `defineProperty`). Both files are gone; every row below is mounted by THE
//! installer from these tables (`install::live::install_window_tables`).
//!
//! ONE FILE FOR SIX INTERFACES, not six files. Ruling (recorded in the lane's commit): the six share a
//! single set of tree rules -- which children count as this table's rows, which as this row's cells,
//! and in what order -- and the JS they replace shared them by importing one file from the other for
//! exactly that reason. Splitting them would put the same rule in two places, which is the thing this
//! goal exists to stop. The row-ordering rules themselves live in ONE place further down still:
//! `core/dom/collections.rs`'s `table_rows` / `kids_named` / `row_cells`, the same functions the live
//! `rows` / `tBodies` / `cells` collections query, so an algorithm here and the collection a page reads
//! can never disagree.
//!
//! HOW A MUTATION REACHES THE ARENA. Reads go straight to the arena (`HostDom`), which is what the
//! collections do. Writes NEVER do: every insertion and removal runs the realm's ORIGINAL `insertBefore` /
//! `appendChild` / `remove` / `createElement` (`protos::pristine_call`), so the Node mutation callbacks fire
//! and MutationObserver and the ranges registry see the change -- the same path the deleted JS took, for
//! the same reason. Never the page's: looked up on the receiver, a page that wrapped them counted the
//! engine's calls as its own, where Chrome's table algorithms call nothing a page can see
//! (`tests/engine_reads_live.rs`).
//!
//! WHAT CHROME SAYS (probed with `google-chrome --headless=new --dump-dom` on this machine,
//! 2026-09-11; the probe pages are in the lane's notes). Four things the deleted JS had wrong:
//!
//!   * `table.caption = <a div>` is a **TypeError** ("Failed to convert value to
//!     'HTMLTableCaptionElement'"), not a `HierarchyRequestError` -- the IDL type is
//!     `HTMLTableCaptionElement?`, so a wrong-typed value never reaches the algorithm. `tHead`/`tFoot`
//!     take `HTMLTableSectionElement?`, so a `<tbody>` DOES reach it and gets the spec's
//!     `HierarchyRequestError` ("Not a thead element.").
//!   * `deleteRow(-1)` / `deleteCell(-1)` on an empty table/row/section is a **no-op**, not an
//!     `IndexSizeError`; the JS threw "The table has no rows."
//!   * `insertRow(-2)` says "The index provided (-2) is less than -1.", not the too-large message.
//!   * `colSpan` clamps to [1, 1000] and `rowSpan` to [0, 65534] (`td.colSpan = 2000` reads 1000 while
//!     the attribute keeps "2000"). The JS installed them with no range at all; the registry rows
//!     carried the range already, and the rows are copied verbatim, so the clamp arrives with them.
//!
//! Every member this file bodies checks its receiver and throws Chrome's "Illegal invocation"
//! TypeError otherwise: `HTMLTableElement.prototype.rows` on a `<tr>`, on a `<div>` or on
//! `Object.create(HTMLTableElement.prototype)` throws in Chrome, where the deleted helpers answered an
//! empty collection or `-1`.

use deno_core::v8;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{global_get, pristine_call, str_val, throw_dom, throw_type_error, wrap_cached};
use crate::core::dom::collections::{is_html_named, kids_named, make_live, row_cells, table_rows, Kind};
use crate::core::dom::element::el_id_of;
use crate::install::live::{getter_row, method_row, pair_row};
use crate::install::MemberDef;

type OpStateRc = std::rc::Rc<std::cell::RefCell<deno_core::OpState>>;

fn op_state(scope: &mut v8::PinScope) -> OpStateRc {
    deno_core::JsRuntime::op_state_from(scope)
}

// ── the receiver checks ─────────────────────────────────────────────────────────────────────────

/// The receiver's arena id when it is an HTML element whose LOCAL NAME is one of `tags`; otherwise
/// Chrome's "Illegal invocation" TypeError and `None`.
fn this_tagged(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, tags: &[&str]) -> Option<u32> {
    let this = args.this();
    let id = el_id_of(scope, this).filter(|&id| tag_is(scope, id, tags));
    if id.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    id
}

/// Is `id` an HTML element named one of `tags`? IT ASKED THE ARENA FOR A BARE TAG NAME
/// (`dom.tag_name(id).eq_ignore_ascii_case(w)`), which answered wrongly in both directions at once --
/// see [`is_html_named`], the one production this and the three tabular collections now share.
fn tag_is(scope: &mut v8::PinScope, id: u32, tags: &[&str]) -> bool {
    let os = op_state(scope);
    let st = os.borrow();
    tags.iter().any(|w| is_html_named(&st, id, w))
}

const SECTIONS: &[&str] = &["thead", "tbody", "tfoot"];
const CELLS: &[&str] = &["td", "th"];

/// A VALUE's arena id when it is an element whose tag is one of `tags` -- the WebIDL interface check a
/// section/caption setter runs on its argument (not on its receiver).
fn value_tagged(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, tags: &[&str]) -> Option<u32> {
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    el_id_of(scope, obj).filter(|&id| tag_is(scope, id, tags))
}

// ── arena reads (never writes) ──────────────────────────────────────────────────────────────────

fn child_ids(scope: &mut v8::PinScope, id: u32, tag: &str) -> Vec<u32> {
    let os = op_state(scope);
    let st = os.borrow();
    kids_named(&st, id, tag)
}

fn first_child_id(scope: &mut v8::PinScope, id: u32, tag: &str) -> Option<u32> {
    child_ids(scope, id, tag).first().copied()
}

fn rows_of_table(scope: &mut v8::PinScope, id: u32) -> Vec<u32> {
    let os = op_state(scope);
    let st = os.borrow();
    table_rows(&st, id)
}

fn cells_of_row(scope: &mut v8::PinScope, id: u32) -> Vec<u32> {
    let os = op_state(scope);
    let st = os.borrow();
    row_cells(&st, id)
}

fn parent_id(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    let os = op_state(scope);
    let st = os.borrow();
    st.borrow::<HostDom>().0.parent(id)
}

/// The nearest `<table>` ANCESTOR, the answer `el.closest("table")` gave a `<tr>` (a `<tr>` is never a
/// table itself, so starting at the parent is the same walk).
fn closest_table(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    let mut cur = parent_id(scope, id);
    while let Some(n) = cur {
        if tag_is(scope, n, &["table"]) {
            return Some(n);
        }
        cur = parent_id(scope, n);
    }
    None
}

/// The canonical wrapper for an arena node, so a mutation can go through the real DOM methods.
fn node_of<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> v8::Local<'s, v8::Object> {
    let os = op_state(scope);
    wrap_cached(scope, &os, id)
}

// ── mutations: through the realm's own tree steps ────────────────────────────────────────────────

/// `createElement(tag)` on the receiver's node document -- the created element belongs to it, which is
/// what Chrome does for a `<table>` in a document made by `createHTMLDocument`.
/// `this` is a `Local<Value>` with an ELIDED lifetime on purpose: `args.this()`'s lifetime belongs to
/// `FunctionCallbackArguments`, not to `scope`, and tying the two makes the callback unassignable
/// (the `html_meter_element.rs` gotcha).
fn create_element<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, tag: &str) -> Option<v8::Local<'s, v8::Value>> {
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    let id = el_id_of(scope, obj)?;
    let os = op_state(scope);
    let doc = match crate::core::dom::node::node_document(scope, &os, id).filter(|d| d.is_object()) {
        Some(d) => d,
        None => global_get(scope, "document")?,
    };
    let name = str_val(scope, tag);
    pristine_call(scope, doc, "Document.createElement", &[name]).filter(|v| v.is_object())
}

fn append_to(scope: &mut v8::PinScope, parent: u32, node: v8::Local<v8::Value>) {
    let p = node_of(scope, parent);
    pristine_call(scope, p.into(), "Node.appendChild", &[node]);
}

/// `parent.insertBefore(node, ref)` -- `ref` `None` appends, exactly as a null reference node does.
fn insert_before(scope: &mut v8::PinScope, parent: u32, node: v8::Local<v8::Value>, reference: Option<v8::Local<v8::Value>>) {
    let p = node_of(scope, parent);
    let r: v8::Local<v8::Value> = match reference {
        Some(r) => r,
        None => v8::null(scope).into(),
    };
    pristine_call(scope, p.into(), "Node.insertBefore", &[node, r]);
}

fn remove_node(scope: &mut v8::PinScope, id: u32) {
    let n = node_of(scope, id);
    pristine_call(scope, n.into(), "Element.remove", &[]);
}

/// A node's next sibling as a reference node -- how "insert after `id`" is spelled with one primitive.
fn next_sibling_of<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    let next = {
        let os = op_state(scope);
        let st = os.borrow();
        st.borrow::<HostDom>().0.next_sibling(id)
    }?;
    Some(node_of(scope, next).into())
}

// ── the three placements a section has ──────────────────────────────────────────────────────────

/// `<caption>`: before the first child NODE (not the first child element -- the JS used `firstChild`
/// and so does Chrome, so a leading text node stays after the caption).
fn place_first(scope: &mut v8::PinScope, table: u32, node: v8::Local<v8::Value>) {
    let first = {
        let os = op_state(scope);
        let st = os.borrow();
        st.borrow::<HostDom>().0.children(table).first().copied()
    };
    let first = first.map(|f| { let w: v8::Local<v8::Value> = node_of(scope, f).into(); w });
    insert_before(scope, table, node, first);
}

/// `<thead>`: after any `<caption>`/`<colgroup>` children, before everything else.
fn place_after_colgroups(scope: &mut v8::PinScope, table: u32, node: v8::Local<v8::Value>) {
    let os = op_state(scope);
    let reference = {
        let st = os.borrow();
        let children = st.borrow::<HostDom>().0.child_elements(table);
        children.into_iter().find(|&c| !is_html_named(&st, c, "caption") && !is_html_named(&st, c, "colgroup"))
    };
    match reference {
        Some(r) => {
            let rv: v8::Local<v8::Value> = node_of(scope, r).into();
            insert_before(scope, table, node, Some(rv));
        }
        None => append_to(scope, table, node),
    }
}

/// `<tfoot>`: last.
fn place_last(scope: &mut v8::PinScope, table: u32, node: v8::Local<v8::Value>) {
    append_to(scope, table, node);
}

// ── the WebIDL `long` index argument ────────────────────────────────────────────────────────────

/// `Math.trunc(Number(v)) || 0` -- the conversion the deleted JS ran and the one WebIDL's `long`
/// produces for every value a page realistically passes (`NaN` -> 0, `1.7` -> 1, `"abc"` -> 0).
fn index_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> i64 {
    let n = v.number_value(scope).unwrap_or(f64::NAN).trunc();
    if n.is_nan() || n == 0.0 {
        0
    } else {
        n.clamp(i32::MIN as f64, i32::MAX as f64) as i64
    }
}

/// The index argument of a method whose IDL declares `optional long index = -1`: an ABSENT or
/// `undefined` argument is -1, anything else goes through [`index_of`].
fn optional_index(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> i64 {
    let v = args.get(0);
    if v.is_undefined() {
        -1
    } else {
        index_of(scope, v)
    }
}

fn index_error(scope: &mut v8::PinScope, msg: &str) {
    throw_dom(scope, msg, "IndexSizeError");
}

// ── HTMLTableElement: the section accessors ─────────────────────────────────────────────────────

fn cb_tbl_caption_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["table"]) else { return };
    match first_child_id(scope, id, "caption") {
        Some(c) => {
            let v: v8::Local<v8::Value> = node_of(scope, c).into();
            rv.set(v);
        }
        None => {
            let n = v8::null(scope);
            rv.set(n.into());
        }
    }
}

fn cb_tbl_caption_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["table"]) else { return };
    let v = args.get(0);
    if v.is_null_or_undefined() {
        if let Some(c) = first_child_id(scope, id, "caption") {
            remove_node(scope, c);
        }
        return;
    }
    if value_tagged(scope, v, &["caption"]).is_none() {
        throw_type_error(
            scope,
            "Failed to set the 'caption' property on 'HTMLTableElement': Failed to convert value to 'HTMLTableCaptionElement'.",
        );
        return;
    }
    if let Some(c) = first_child_id(scope, id, "caption") {
        remove_node(scope, c);
    }
    place_first(scope, id, v);
}

/// The shared `tHead`/`tFoot` setter: the IDL type check (any section), then the spec's
/// `HierarchyRequestError` when the section is the wrong one, then replace-and-place.
fn section_set(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    prop: &str,
    tag: &str,
    place: fn(&mut v8::PinScope, u32, v8::Local<v8::Value>),
) {
    let Some(id) = this_tagged(scope, args, &["table"]) else { return };
    let v = args.get(0);
    if v.is_null_or_undefined() {
        if let Some(c) = first_child_id(scope, id, tag) {
            remove_node(scope, c);
        }
        return;
    }
    if value_tagged(scope, v, SECTIONS).is_none() {
        throw_type_error(
            scope,
            &format!("Failed to set the '{prop}' property on 'HTMLTableElement': Failed to convert value to 'HTMLTableSectionElement'."),
        );
        return;
    }
    if value_tagged(scope, v, &[tag]).is_none() {
        throw_dom(
            scope,
            &format!("Failed to set the '{prop}' property on 'HTMLTableElement': Not a {tag} element."),
            "HierarchyRequestError",
        );
        return;
    }
    if let Some(c) = first_child_id(scope, id, tag) {
        remove_node(scope, c);
    }
    place(scope, id, v);
}

fn cb_tbl_thead_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    section_get(scope, &args, "thead", rv);
}
fn cb_tbl_tfoot_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    section_get(scope, &args, "tfoot", rv);
}
fn section_get(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, tag: &str, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, args, &["table"]) else { return };
    match first_child_id(scope, id, tag) {
        Some(c) => {
            let v: v8::Local<v8::Value> = node_of(scope, c).into();
            rv.set(v);
        }
        None => {
            let n = v8::null(scope);
            rv.set(n.into());
        }
    }
}

fn cb_tbl_thead_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    section_set(scope, &args, "tHead", "thead", place_after_colgroups);
}
fn cb_tbl_tfoot_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    section_set(scope, &args, "tFoot", "tfoot", place_last);
}

// ── HTMLTableElement: the two live collections ──────────────────────────────────────────────────

fn cb_tbl_rows(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["table"]) else { return };
    let list = make_live(scope, "HTMLCollection", id, Kind::TableRows, "", None);
    rv.set(list);
}

fn cb_tbl_tbodies(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["table"]) else { return };
    let list = make_live(scope, "HTMLCollection", id, Kind::TableBodies, "", None);
    rv.set(list);
}

// ── HTMLTableElement: create* / delete* ─────────────────────────────────────────────────────────

/// `create<Section>`: the first existing one, or a new one placed by the spec's rule.
fn create_section(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    tag: &str,
    place: fn(&mut v8::PinScope, u32, v8::Local<v8::Value>),
    rv: &mut v8::ReturnValue,
) {
    let Some(id) = this_tagged(scope, args, &["table"]) else { return };
    if let Some(existing) = first_child_id(scope, id, tag) {
        let v: v8::Local<v8::Value> = node_of(scope, existing).into();
        rv.set(v);
        return;
    }
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(node) = create_element(scope, this, tag) else { return };
    place(scope, id, node);
    rv.set(node);
}

fn cb_tbl_create_caption(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    create_section(scope, &args, "caption", place_first, &mut rv);
}
fn cb_tbl_create_thead(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    create_section(scope, &args, "thead", place_after_colgroups, &mut rv);
}
fn cb_tbl_create_tfoot(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    create_section(scope, &args, "tfoot", place_last, &mut rv);
}

/// `createTBody` is the exception: it ALWAYS creates one, after the last existing `<tbody>`.
fn cb_tbl_create_tbody(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["table"]) else { return };
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(node) = create_element(scope, this, "tbody") else { return };
    match child_ids(scope, id, "tbody").last().copied() {
        Some(last) => {
            let after = next_sibling_of(scope, last);
            insert_before(scope, id, node, after);
        }
        None => append_to(scope, id, node),
    }
    rv.set(node);
}

fn delete_section(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, tag: &str) {
    let Some(id) = this_tagged(scope, args, &["table"]) else { return };
    if let Some(existing) = first_child_id(scope, id, tag) {
        remove_node(scope, existing);
    }
}

fn cb_tbl_delete_caption(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    delete_section(scope, &args, "caption");
}
fn cb_tbl_delete_thead(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    delete_section(scope, &args, "thead");
}
fn cb_tbl_delete_tfoot(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    delete_section(scope, &args, "tfoot");
}

// ── HTMLTableElement: insertRow / deleteRow ─────────────────────────────────────────────────────

fn cb_tbl_insert_row(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["table"]) else { return };
    let i = optional_index(scope, &args);
    let rows = rows_of_table(scope, id);
    let len = rows.len() as i64;
    if i < -1 {
        index_error(scope, &format!("Failed to execute 'insertRow' on 'HTMLTableElement': The index provided ({i}) is less than -1."));
        return;
    }
    if i > len {
        index_error(scope, &format!("Failed to execute 'insertRow' on 'HTMLTableElement': The index provided ({i}) is greater than the number of rows in the table ({len})."));
        return;
    }
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(row) = create_element(scope, this, "tr") else { return };
    if len == 0 {
        // "If the table has no rows, put the row in the last tbody, creating one at the end of the
        // table when there is none" -- a <tr> is never a direct child of a table this algorithm built.
        let body = match child_ids(scope, id, "tbody").last().copied() {
            Some(b) => b,
            None => {
                let Some(tb) = create_element(scope, this, "tbody") else { return };
                append_to(scope, id, tb);
                match child_ids(scope, id, "tbody").last().copied() {
                    Some(b) => b,
                    None => return,
                }
            }
        };
        append_to(scope, body, row);
        rv.set(row);
        return;
    }
    if i == -1 || i == len {
        let last = rows[rows.len() - 1];
        let Some(parent) = parent_id(scope, last) else { return };
        let after = next_sibling_of(scope, last);
        insert_before(scope, parent, row, after);
        rv.set(row);
        return;
    }
    let reference = rows[i as usize];
    let Some(parent) = parent_id(scope, reference) else { return };
    let rvref: v8::Local<v8::Value> = node_of(scope, reference).into();
    insert_before(scope, parent, row, Some(rvref));
    rv.set(row);
}

fn cb_tbl_delete_row(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["table"]) else { return };
    let i = index_of(scope, args.get(0));
    let rows = rows_of_table(scope, id);
    let len = rows.len() as i64;
    if i < -1 {
        index_error(scope, &format!("Failed to execute 'deleteRow' on 'HTMLTableElement': The index provided ({i}) is less than -1."));
        return;
    }
    // -1 removes the last row, and does NOTHING when there is none (Chrome; the JS threw).
    if i == -1 {
        if let Some(&last) = rows.last() {
            remove_node(scope, last);
        }
        return;
    }
    if i >= len {
        index_error(scope, &format!("Failed to execute 'deleteRow' on 'HTMLTableElement': The index provided ({i}) is greater than the number of rows in the table ({len})."));
        return;
    }
    remove_node(scope, rows[i as usize]);
}

// ── HTMLTableRowElement ─────────────────────────────────────────────────────────────────────────

fn cb_tr_row_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["tr"]) else { return };
    let n = match closest_table(scope, id) {
        Some(table) => rows_of_table(scope, table).iter().position(|&r| r == id).map(|p| p as i32).unwrap_or(-1),
        None => -1,
    };
    rv.set_int32(n);
}

fn cb_tr_section_row_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["tr"]) else { return };
    // A `<tr>` WHOSE PARENT IS THE TABLE ITSELF HAS A SECTION ROW INDEX TOO. This asked only about
    // `SECTIONS`, so `<table><tr>` -- what `document.createElement("table").appendChild(tr)` builds,
    // since nothing inserts an implied `<tbody>` outside the parser -- answered -1 where Chrome
    // answers 0. Blink is explicit that the collection is chosen from the parent and that a TABLE
    // parent is one of the two answers (`third_party/blink/renderer/core/html/
    // html_table_row_element.cc:73-85`: section -> `section->rows()`, table -> `table->rows()`,
    // anything else -> -1). Both arms are the same algorithm; only the section arm was written.
    let n = match parent_id(scope, id) {
        Some(p) if tag_is(scope, p, SECTIONS) => {
            child_ids(scope, p, "tr").iter().position(|&r| r == id).map(|i| i as i32).unwrap_or(-1)
        }
        Some(p) if tag_is(scope, p, &["table"]) => {
            rows_of_table(scope, p).iter().position(|&r| r == id).map(|i| i as i32).unwrap_or(-1)
        }
        _ => -1,
    };
    rv.set_int32(n);
}

fn cb_tr_cells(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["tr"]) else { return };
    let list = make_live(scope, "HTMLCollection", id, Kind::RowCells, "", None);
    rv.set(list);
}

fn cb_tr_insert_cell(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["tr"]) else { return };
    let i = optional_index(scope, &args);
    let cells = cells_of_row(scope, id);
    let len = cells.len() as i64;
    if i < -1 || i > len {
        index_error(scope, &format!("Failed to execute 'insertCell' on 'HTMLTableRowElement': The value provided ({i}) is outside the range [-1, {len}]."));
        return;
    }
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(cell) = create_element(scope, this, "td") else { return };
    if i == -1 || i == len {
        append_to(scope, id, cell);
    } else {
        let reference: v8::Local<v8::Value> = node_of(scope, cells[i as usize]).into();
        insert_before(scope, id, cell, Some(reference));
    }
    rv.set(cell);
}

fn cb_tr_delete_cell(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, &["tr"]) else { return };
    let i = index_of(scope, args.get(0));
    let cells = cells_of_row(scope, id);
    let len = cells.len() as i64;
    if i == -1 {
        if let Some(&last) = cells.last() {
            remove_node(scope, last);
        }
        return;
    }
    if i < 0 || i >= len {
        index_error(scope, &format!("Failed to execute 'deleteCell' on 'HTMLTableRowElement': The value provided ({i}) is outside the range [0, {len})."));
        return;
    }
    remove_node(scope, cells[i as usize]);
}

// ── HTMLTableSectionElement ─────────────────────────────────────────────────────────────────────
//
// A section's rows are its OWN `<tr>` children -- never a nested table's, which is why this reads
// `kids_named` and not a descendant query.

fn cb_ts_rows(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, SECTIONS) else { return };
    let list = make_live(scope, "HTMLCollection", id, Kind::SectionRows, "", None);
    rv.set(list);
}

fn cb_ts_insert_row(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, SECTIONS) else { return };
    let i = optional_index(scope, &args);
    let rows = child_ids(scope, id, "tr");
    let len = rows.len() as i64;
    if i < -1 || i > len {
        // Chrome's message, verbatim -- including its missing close parenthesis.
        index_error(scope, &format!("Failed to execute 'insertRow' on 'HTMLTableSectionElement': The provided index ({i} is outside the range [-1, {len}]."));
        return;
    }
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(row) = create_element(scope, this, "tr") else { return };
    if i == -1 || i == len {
        append_to(scope, id, row);
    } else {
        let reference: v8::Local<v8::Value> = node_of(scope, rows[i as usize]).into();
        insert_before(scope, id, row, Some(reference));
    }
    rv.set(row);
}

fn cb_ts_delete_row(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, SECTIONS) else { return };
    let i = index_of(scope, args.get(0));
    let rows = child_ids(scope, id, "tr");
    let len = rows.len() as i64;
    if i == -1 {
        if let Some(&last) = rows.last() {
            remove_node(scope, last);
        }
        return;
    }
    if i < 0 || i >= len {
        index_error(scope, &format!("Failed to execute 'deleteRow' on 'HTMLTableSectionElement': The provided index ({i} is outside the range [-1, {len}]."));
        return;
    }
    remove_node(scope, rows[i as usize]);
}

// ── HTMLTableCellElement ────────────────────────────────────────────────────────────────────────

fn cb_tc_cell_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_tagged(scope, &args, CELLS) else { return };
    let n = match parent_id(scope, id).filter(|&p| tag_is(scope, p, &["tr"])) {
        Some(p) => cells_of_row(scope, p).iter().position(|&c| c == id).map(|p| p as i32).unwrap_or(-1),
        None => -1,
    };
    rv.set_int32(n);
}

// ── the four install fns ────────────────────────────────────────────────────────────────────────

pub fn install_html_table_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "caption" => pair_row(scope, proto, member, cb_tbl_caption_get, cb_tbl_caption_set),
        "tHead" => pair_row(scope, proto, member, cb_tbl_thead_get, cb_tbl_thead_set),
        "tFoot" => pair_row(scope, proto, member, cb_tbl_tfoot_get, cb_tbl_tfoot_set),
        "tBodies" => getter_row(scope, proto, member, cb_tbl_tbodies),
        "rows" => getter_row(scope, proto, member, cb_tbl_rows),
        "createCaption" => method_row(scope, proto, member, cb_tbl_create_caption),
        "createTBody" => method_row(scope, proto, member, cb_tbl_create_tbody),
        "createTFoot" => method_row(scope, proto, member, cb_tbl_create_tfoot),
        "createTHead" => method_row(scope, proto, member, cb_tbl_create_thead),
        "deleteCaption" => method_row(scope, proto, member, cb_tbl_delete_caption),
        "deleteRow" => method_row(scope, proto, member, cb_tbl_delete_row),
        "deleteTFoot" => method_row(scope, proto, member, cb_tbl_delete_tfoot),
        "deleteTHead" => method_row(scope, proto, member, cb_tbl_delete_thead),
        "insertRow" => method_row(scope, proto, member, cb_tbl_insert_row),
        other => debug_assert!(false, "install_html_table_element: unexpected member {other}"),
    }
}

pub fn install_html_table_row_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "rowIndex" => getter_row(scope, proto, member, cb_tr_row_index),
        "sectionRowIndex" => getter_row(scope, proto, member, cb_tr_section_row_index),
        "cells" => getter_row(scope, proto, member, cb_tr_cells),
        "deleteCell" => method_row(scope, proto, member, cb_tr_delete_cell),
        "insertCell" => method_row(scope, proto, member, cb_tr_insert_cell),
        other => debug_assert!(false, "install_html_table_row_element: unexpected member {other}"),
    }
}

pub fn install_html_table_section_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "rows" => getter_row(scope, proto, member, cb_ts_rows),
        "deleteRow" => method_row(scope, proto, member, cb_ts_delete_row),
        "insertRow" => method_row(scope, proto, member, cb_ts_insert_row),
        other => debug_assert!(false, "install_html_table_section_element: unexpected member {other}"),
    }
}

pub fn install_html_table_cell_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "cellIndex" => getter_row(scope, proto, member, cb_tc_cell_index),
        other => debug_assert!(false, "install_html_table_cell_element: unexpected member {other}"),
    }
}

// ── the six tables ──────────────────────────────────────────────────────────────────────────────
//
// Rows VERBATIM from the `HTMLTable*Element` blocks of `install/generated_registry.rs`, in the
// capture's order, with the capture's kinds, lengths, `required` counts and descriptor flags -- the
// same order `plugins/korvus-persona/bundles/.../surface.json` records. The `= stub` bodies became the
// callbacks above; the `= reflect_*` bodies are the kits the generated rows already named, cfg
// included.
//
// FIVE ROWS THE GENERATOR LEFT AS `stub` ARE REFLECTIONS HERE -- `bgColor` on the table, the row and
// the cell, plus `cellPadding`/`cellSpacing` on the table and `scope` on the cell. webref carries no
// `[Reflect]` for those five, so `gen_reflected.py` had nothing to emit, and the deleted JS installed
// them by hand; the bundle's oracle records `(o.bgColor = "x", o.bgColor) === "x"` and Chrome answers
// `["x","x"]` for value and attribute, so a `stub` row here would be a REGRESSION dressed as fidelity.
// The kit + cfg is what the JS installed, checked against Chrome.

webapi::interface! {
    "HTMLTableSectionElement" : "HTMLElement" [element, window] {
        ctor   "HTMLTableSectionElement"            len 0            = stub,
        attr   "rows"      : Object readonly                         = install_html_table_section_element,
        attr   "align"     : Object cfg "align"                      = reflect_str,
        attr   "ch"        : Object cfg "char"                       = reflect_str,
        attr   "chOff"     : Object cfg "charoff"                    = reflect_str,
        attr   "vAlign"    : Object cfg "valign"                     = reflect_str,
        method "deleteRow" : Object len 1 required 1                 = install_html_table_section_element,
        method "insertRow" : Object len 0                            = install_html_table_section_element
    }
}

webapi::interface! {
    "HTMLTableRowElement" : "HTMLElement" [element, window] {
        ctor   "HTMLTableRowElement"                len 0            = stub,
        attr   "rowIndex"        : Object readonly                   = install_html_table_row_element,
        attr   "sectionRowIndex" : Object readonly                   = install_html_table_row_element,
        attr   "cells"           : Object readonly                   = install_html_table_row_element,
        attr   "align"           : Object cfg "align"                = reflect_str,
        attr   "ch"              : Object cfg "char"                 = reflect_str,
        attr   "chOff"           : Object cfg "charoff"              = reflect_str,
        attr   "vAlign"          : Object cfg "valign"               = reflect_str,
        attr   "bgColor"         : Object cfg "bgcolor"              = reflect_str,
        method "deleteCell"      : Object len 1 required 1           = install_html_table_row_element,
        method "insertCell"      : Object len 0                      = install_html_table_row_element
    }
}

webapi::interface! {
    "HTMLTableElement" : "HTMLElement" [element, window] {
        ctor   "HTMLTableElement"                   len 0            = stub,
        attr   "caption"       : Object nullable                     = install_html_table_element,
        attr   "tHead"         : Object nullable                     = install_html_table_element,
        attr   "tFoot"         : Object nullable                     = install_html_table_element,
        attr   "tBodies"       : Object readonly                     = install_html_table_element,
        attr   "rows"          : Object readonly                     = install_html_table_element,
        attr   "align"         : Object cfg "align"                  = reflect_str,
        attr   "border"        : Object cfg "border"                 = reflect_str,
        attr   "frame"         : Object cfg "frame"                  = reflect_str,
        attr   "rules"         : Object cfg "rules"                  = reflect_str,
        attr   "summary"       : Object cfg "summary"                = reflect_str,
        attr   "width"         : Object cfg "width"                  = reflect_str,
        attr   "bgColor"       : Object cfg "bgcolor"                = reflect_str,
        attr   "cellPadding"   : Object cfg "cellpadding"            = reflect_str,
        attr   "cellSpacing"   : Object cfg "cellspacing"            = reflect_str,
        method "createCaption" : Object len 0                        = install_html_table_element,
        method "createTBody"   : Object len 0                        = install_html_table_element,
        method "createTFoot"   : Object len 0                        = install_html_table_element,
        method "createTHead"   : Object len 0                        = install_html_table_element,
        method "deleteCaption" : Object len 0                        = install_html_table_element,
        method "deleteRow"     : Object len 1 required 1             = install_html_table_element,
        method "deleteTFoot"   : Object len 0                        = install_html_table_element,
        method "deleteTHead"   : Object len 0                        = install_html_table_element,
        method "insertRow"     : Object len 0                        = install_html_table_element
    }
}

webapi::interface! {
    "HTMLTableColElement" : "HTMLElement" [element, window] {
        ctor "HTMLTableColElement"                  len 0            = stub,
        attr "span"   : Object cfg "span|1||1:1000"                  = reflect_ulong,
        attr "align"  : Object cfg "align"                           = reflect_str,
        attr "ch"     : Object cfg "char"                            = reflect_str,
        attr "chOff"  : Object cfg "charoff"                         = reflect_str,
        attr "vAlign" : Object cfg "valign"                          = reflect_str,
        attr "width"  : Object cfg "width"                           = reflect_str
    }
}

webapi::interface! {
    "HTMLTableCellElement" : "HTMLElement" [element, window] {
        ctor "HTMLTableCellElement"                 len 0            = stub,
        attr "colSpan"   : Object cfg "colspan|1||1:1000"            = reflect_ulong,
        attr "rowSpan"   : Object cfg "rowspan|1||0:65534"           = reflect_ulong,
        attr "headers"   : Object cfg "headers"                      = reflect_str,
        attr "cellIndex" : Object readonly                           = install_html_table_cell_element,
        attr "align"     : Object cfg "align"                        = reflect_str,
        attr "axis"      : Object cfg "axis"                         = reflect_str,
        attr "height"    : Object cfg "height"                       = reflect_str,
        attr "width"     : Object cfg "width"                        = reflect_str,
        attr "ch"        : Object cfg "char"                         = reflect_str,
        attr "chOff"     : Object cfg "charoff"                      = reflect_str,
        attr "noWrap"    : Object cfg "nowrap"                       = reflect_bool,
        attr "vAlign"    : Object cfg "valign"                       = reflect_str,
        attr "bgColor"   : Object cfg "bgcolor"                      = reflect_str,
        attr "abbr"      : Object cfg "abbr"                         = reflect_str,
        attr "scope"     : Object cfg "scope|row,col,rowgroup,colgroup||" = reflect_enum
    }
}

webapi::interface! {
    "HTMLTableCaptionElement" : "HTMLElement" [element, window] {
        ctor "HTMLTableCaptionElement"              len 0            = stub,
        attr "align" : Object cfg "align"                            = reflect_str
    }
}
