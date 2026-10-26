//! `document.execCommand` and the `queryCommand*` family, as Blink answers them (core/editing/commands/
//! editor_command.cc, document_exec_command.cc, Chromium 150).
//!
//! These answered "no command is supported": every query false or `""`, every `execCommand` false. Chrome 150
//! supports 48 command names from the DOM (Blink's table: `Supported`, not `SupportedFromMenuOrKeyBinding`) and
//! answers each from the document's selection -- a caret or a range, in editable content or not (a `contenteditable`
//! host is RICHLY editable, a text control and `plaintext-only` plainly), and the computed style at its start:
//!
//! * enabled: `Enabled` always; `EnabledInEditableText` / `EnabledInRichlyEditableText` by the selection's anchor;
//!   `EnableCaretInEditableText`, `EnabledRangeInRichlyEditableText` (and `removeFormat`, by the feature Chrome 150
//!   ships); `EnabledUnselect` a range or an editable caret; `EnabledSelectAll` unless an empty editable root holds
//!   the selection; the clipboard without user activation, and undo / redo without history, never;
//! * state: a caret's from the style at it; a range's over its rendered editable nodes in tree order, the first
//!   node's answer unless a TEXT node disagrees (mixed: `queryCommandIndeterm`) -- a range in content nothing edits
//!   is false; lists by the enclosing `<ol>` / `<ul>`; `styleWithCSS` the editor's flag;
//! * value: `"true"` / `"false"` for a command with a state, `""` without; the computed `font-family`, `color`,
//!   `background-color` (the first not transparent up the tree) and the legacy font size 1-7 at the selection's
//!   start; the alignment there, `start` / `end` resolved by direction; `formatBlock`'s enclosing block element
//!   inside the editable root; the default paragraph separator.
//!
//! What runs without editing: `selectAll`, `unselect`, `styleWithCSS`, `useCSS`, `defaultParagraphSeparator`,
//! `findString`, `print`. NOT MODELLED, stated: the editing commands themselves -- `bold`, `insertText`, `delete`,
//! ... in editable content answer `false` and change nothing (Chrome's edit the DOM and answer `true`).
//!
//! A document no browsing context shows (`DOMParser`'s, `createHTMLDocument`'s) has no command at all; one that is
//! not HTML throws `InvalidStateError` in Blink's words.

use deno_core::v8;
use deno_core::OpState;

use crate::binding::protos::{str_val, throw_dom};
use crate::binding::HostDom;
use crate::core::css::cssom::css_computed_one;

#[derive(Clone, Copy)]
enum Enabled {
    Always,
    InEditable,
    InRichlyEditable,
    CaretInEditable,
    RangeInRichlyEditable,
    SelectAll,
    Unselect,
    Never,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    None,
    Style(&'static str, &'static str),
    StyleWithCss,
    List(&'static str),
}

#[derive(Clone, Copy)]
enum Value {
    StateOrNull,
    Empty,
    DefaultParagraphSeparator,
    Style(&'static str),
    FontSize,
    FormatBlock,
    Justify,
}

#[derive(Clone, Copy, PartialEq)]
enum Exec {
    Edit,
    SelectAll,
    Unselect,
    StyleWithCss,
    UseCss,
    DefaultParagraphSeparator,
    FindString,
    Print,
    Never,
}

struct Command {
    name: &'static str,
    enabled: Enabled,
    state: State,
    value: Value,
    exec: Exec,
}

const fn c(name: &'static str, enabled: Enabled, state: State, value: Value, exec: Exec) -> Command {
    Command { name, enabled, state, value, exec }
}

use Enabled as E;
use Exec as X;
use State as S;
use Value as V;

/// The commands a page can name (Blink 150's `kEditorCommands` rows whose `is_supported_from_dom` is `Supported`;
/// `paste` is supported only where DOM paste is allowed, which it is not).
const COMMANDS: [Command; 47] = [
    c("backColor", E::InRichlyEditable, S::None, V::Style("background-color"), X::Edit),
    c("bold", E::InRichlyEditable, S::Style("font-weight", "bold"), V::StateOrNull, X::Edit),
    c("copy", E::Never, S::None, V::StateOrNull, X::Never),
    c("createLink", E::InRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("cut", E::Never, S::None, V::StateOrNull, X::Never),
    c("defaultParagraphSeparator", E::Always, S::None, V::DefaultParagraphSeparator, X::DefaultParagraphSeparator),
    c("delete", E::InEditable, S::None, V::StateOrNull, X::Edit),
    c("findString", E::Always, S::None, V::StateOrNull, X::FindString),
    c("fontName", E::InRichlyEditable, S::None, V::Style("font-family"), X::Edit),
    c("fontSize", E::InRichlyEditable, S::None, V::FontSize, X::Edit),
    c("fontSizeDelta", E::InRichlyEditable, S::None, V::Empty, X::Edit),
    c("foreColor", E::InRichlyEditable, S::None, V::Style("color"), X::Edit),
    c("formatBlock", E::InRichlyEditable, S::None, V::FormatBlock, X::Edit),
    c("forwardDelete", E::InEditable, S::None, V::StateOrNull, X::Edit),
    c("hiliteColor", E::InRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("indent", E::InRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("insertHTML", E::InEditable, S::None, V::StateOrNull, X::Edit),
    c("insertHorizontalRule", E::InRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("insertImage", E::InRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("insertLineBreak", E::InEditable, S::None, V::StateOrNull, X::Edit),
    c("insertNewlineInQuotedContent", E::InRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("insertOrderedList", E::InRichlyEditable, S::List("ol"), V::StateOrNull, X::Edit),
    c("insertParagraph", E::InEditable, S::None, V::StateOrNull, X::Edit),
    c("insertText", E::InEditable, S::None, V::StateOrNull, X::Edit),
    c("insertUnorderedList", E::InRichlyEditable, S::List("ul"), V::StateOrNull, X::Edit),
    c("italic", E::InRichlyEditable, S::Style("font-style", "italic"), V::StateOrNull, X::Edit),
    c("justifyCenter", E::InRichlyEditable, S::Style("text-align", "center"), V::Justify, X::Edit),
    c("justifyFull", E::InRichlyEditable, S::Style("text-align", "justify"), V::Justify, X::Edit),
    c("justifyLeft", E::InRichlyEditable, S::Style("text-align", "left"), V::Justify, X::Edit),
    c("justifyNone", E::InRichlyEditable, S::None, V::Justify, X::Edit),
    c("justifyRight", E::InRichlyEditable, S::Style("text-align", "right"), V::Justify, X::Edit),
    c("outdent", E::InRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("pasteAndMatchStyle", E::Never, S::None, V::StateOrNull, X::Never),
    c("print", E::Always, S::None, V::StateOrNull, X::Print),
    c("redo", E::Never, S::None, V::StateOrNull, X::Never),
    c("removeFormat", E::RangeInRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("selectAll", E::SelectAll, S::None, V::StateOrNull, X::SelectAll),
    c("strikethrough", E::InRichlyEditable, S::Style("text-decoration", "line-through"), V::StateOrNull, X::Edit),
    c("styleWithCSS", E::Always, S::StyleWithCss, V::Empty, X::StyleWithCss),
    c("subscript", E::InRichlyEditable, S::Style("vertical-align", "sub"), V::StateOrNull, X::Edit),
    c("superscript", E::InRichlyEditable, S::Style("vertical-align", "super"), V::StateOrNull, X::Edit),
    c("transpose", E::CaretInEditable, S::None, V::StateOrNull, X::Edit),
    c("underline", E::InRichlyEditable, S::Style("text-decoration", "underline"), V::StateOrNull, X::Edit),
    c("undo", E::Never, S::None, V::StateOrNull, X::Never),
    c("unlink", E::RangeInRichlyEditable, S::None, V::StateOrNull, X::Edit),
    c("unselect", E::Unselect, S::None, V::StateOrNull, X::Unselect),
    c("useCSS", E::Always, S::None, V::StateOrNull, X::UseCss),
];

fn command(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// The editor's flags, per document (Blink keeps them on the frame's `Editor`).
#[derive(Default)]
struct EditorFlags(std::collections::HashMap<u32, (bool, bool)>);

fn flags(st: &OpState, doc: u32) -> (bool, bool) {
    st.try_borrow::<EditorFlags>().and_then(|f| f.0.get(&doc).copied()).unwrap_or((false, false))
}

fn set_flags(st: &mut OpState, doc: u32, f: impl FnOnce(&mut (bool, bool))) {
    if st.try_borrow::<EditorFlags>().is_none() {
        st.put(EditorFlags::default());
    }
    f(st.borrow_mut::<EditorFlags>().0.entry(doc).or_default());
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    None,
    Caret,
    Range,
}

/// The selection a command reads: a focused text control's own, else the document's.
struct Snapshot {
    kind: Kind,
    start: (u32, u32),
    end: (u32, u32),
    /// the editable root holding the selection's anchor, and whether it is richly editable
    edit: Option<(u32, bool)>,
    /// a focused text control holding the selection
    control: Option<u32>,
}

impl Snapshot {
    fn none() -> Snapshot {
        Snapshot { kind: Kind::None, start: (0, 0), end: (0, 0), edit: None, control: None }
    }
}

fn is_text_control(dom: &korvus_dom::Dom, id: u32) -> bool {
    if !dom.is_html_ns(id) {
        return false;
    }
    match dom.tag_name(id).map(|t| t.to_ascii_lowercase()).as_deref() {
        Some("textarea") => true,
        Some("input") => {
            let ty = dom.get_attribute(id, "type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
            matches!(ty.as_str(), "" | "text" | "search" | "url" | "tel" | "email" | "password" | "number")
        }
        _ => false,
    }
}

fn element_of(dom: &korvus_dom::Dom, node: u32) -> Option<u32> {
    if dom.node_type(node) == 1 {
        Some(node)
    } else {
        dom.parent(node).filter(|&p| dom.node_type(p) == 1)
    }
}

/// The editing host holding `node`, and whether it is richly editable: the innermost `contenteditable` decides
/// (`""` / `"true"` rich, `"plaintext-only"` plain, `"false"` not editable), and the host is the outermost element of
/// the editable run of ancestors.
fn editable_root(dom: &korvus_dom::Dom, node: u32) -> Option<(u32, bool)> {
    let mut cur = element_of(dom, node);
    let mut governing: Option<bool> = None;
    let mut host = None;
    while let Some(e) = cur {
        match dom.get_attribute(e, "contenteditable").map(|v| v.trim().to_ascii_lowercase()) {
            Some(v) if v.is_empty() || v == "true" => {
                governing.get_or_insert(true);
                host = Some(e);
            }
            Some(v) if v == "plaintext-only" => {
                governing.get_or_insert(false);
                host = Some(e);
            }
            Some(v) if v == "false" => break,
            _ => {}
        }
        cur = dom.parent(e).filter(|&p| dom.node_type(p) == 1);
    }
    Some((host?, governing?))
}

fn snapshot(scope: &mut v8::PinScope, doc: u32) -> Snapshot {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let control = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        crate::core::events::focus::focused_node(&st).filter(|&f| is_text_control(dom, f) && dom.root_of(f) == doc)
    };
    if let Some(f) = control {
        let obj = crate::binding::protos::wrap_cached(scope, &op_state, f);
        let (s, e) = crate::core::html::html_input_element::text_control_selection(scope, obj);
        let kind = if s == e { Kind::Caret } else { Kind::Range };
        return Snapshot { kind, start: (f, s), end: (f, e), edit: Some((f, false)), control: Some(f) };
    }
    let Some(sel) = crate::core::editing::selection::document_selection(scope, doc) else { return Snapshot::none() };
    let Some(((an, ao), (fnode, fo))) = crate::core::editing::selection::selection_points(scope, sel) else { return Snapshot::none() };
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let (a, f) = ((an, ao.max(0) as u32), (fnode, fo.max(0) as u32));
    let (start, end) = if crate::core::dom::shared::cmp_points(dom, a.0, a.1 as i64, f.0, f.1 as i64) <= 0 { (a, f) } else { (f, a) };
    let kind = if start == end { Kind::Caret } else { Kind::Range };
    Snapshot { kind, start, end, edit: editable_root(dom, an), control: None }
}

fn enabled(st: &OpState, cmd: &Command, s: &Snapshot) -> bool {
    match cmd.enabled {
        E::Always => true,
        E::Never => false,
        E::InEditable => s.kind != Kind::None && s.edit.is_some(),
        E::InRichlyEditable => s.kind != Kind::None && s.edit.is_some_and(|(_, rich)| rich),
        E::CaretInEditable => s.kind == Kind::Caret && s.edit.is_some(),
        E::RangeInRichlyEditable => s.kind == Kind::Range && s.edit.is_some_and(|(_, rich)| rich),
        E::Unselect => s.kind == Kind::Range || (s.kind == Kind::Caret && s.edit.is_some()),
        E::SelectAll => {
            let dom = &st.borrow::<HostDom>().0;
            match s.edit {
                Some((root, _)) if s.control.is_none() => dom.first_child(root).is_some(),
                _ => true,
            }
        }
    }
}

/// The element whose style a caret, or a range's start, reads (`CreateStyleAtSelectionStart`): a text control
/// itself; a text position's parent; an element position's first leaf from the child at the offset; a range that
/// starts at the end of a text node, the next node's.
fn style_element(dom: &korvus_dom::Dom, s: &Snapshot) -> Option<u32> {
    if let Some(c) = s.control {
        return Some(c);
    }
    let (node, offset) = s.start;
    let mut n = node;
    if dom.node_type(n) == 3 {
        let len = dom.node_value(n).map_or(0, |v| v.encode_utf16().count() as u32);
        if s.kind == Kind::Range && offset >= len {
            if let Some(next) = next_leaf_after(dom, n) {
                n = next;
            }
        }
    } else if dom.node_type(n) == 1 {
        if let Some(child) = dom.children(n).get(offset as usize).copied() {
            n = child;
            while let Some(f) = dom.first_child(n) {
                n = f;
            }
        }
    }
    element_of(dom, n)
}

/// A start position's node as the selection canonicalizes it: an element position's first leaf from the child at
/// its offset.
fn deep_start(dom: &korvus_dom::Dom, (node, offset): (u32, u32)) -> u32 {
    if dom.node_type(node) != 1 {
        return node;
    }
    let Some(mut n) = dom.children(node).get(offset as usize).copied() else { return node };
    while let Some(f) = dom.first_child(n) {
        n = f;
    }
    n
}

/// An end position's node: an element position's last leaf before its offset.
fn deep_end(dom: &korvus_dom::Dom, (node, offset): (u32, u32)) -> u32 {
    if dom.node_type(node) != 1 || offset == 0 {
        return node;
    }
    let Some(mut n) = dom.children(node).get(offset as usize - 1).copied() else { return node };
    while let Some(l) = dom.last_child(n) {
        n = l;
    }
    n
}

fn next_preorder(dom: &korvus_dom::Dom, n: u32) -> Option<u32> {
    if let Some(f) = dom.first_child(n) {
        return Some(f);
    }
    let mut cur = Some(n);
    while let Some(c) = cur {
        if let Some(s) = dom.next_sibling(c) {
            return Some(s);
        }
        cur = dom.parent(c);
    }
    None
}

fn next_leaf_after(dom: &korvus_dom::Dom, n: u32) -> Option<u32> {
    let mut cur = next_preorder(dom, n);
    while let Some(c) = cur {
        if dom.first_child(c).is_none() {
            return Some(c);
        }
        cur = next_preorder(dom, c);
    }
    None
}

/// Whether element `el`'s style has `property: value` as Blink's `TriStateOfStyle` compares them: a bold weight is
/// 600 or more, a decoration is any in effect up the tree, an alignment resolves `start` / `end` by direction, a
/// baseline element under a `sub` / `sup` ancestor takes its alignment.
fn has_style(st: &mut OpState, el: u32, property: &str, value: &str) -> bool {
    match property {
        "font-weight" => css_computed_one(st, el, "font-weight").trim().parse::<f32>().is_ok_and(|w| w >= 600.0),
        "font-style" => css_computed_one(st, el, "font-style").trim() == value,
        "text-decoration" => {
            let mut cur = Some(el);
            while let Some(e) = cur {
                if css_computed_one(st, e, "text-decoration-line").split_whitespace().any(|t| t == value) {
                    return true;
                }
                let dom = &st.borrow::<HostDom>().0;
                cur = dom.parent(e).filter(|&p| dom.node_type(p) == 1);
            }
            false
        }
        "vertical-align" => {
            let own = css_computed_one(st, el, "vertical-align");
            if own.trim() == value {
                return true;
            }
            if own.trim() != "baseline" {
                return false;
            }
            let mut cur = { st.borrow::<HostDom>().0.parent(el) };
            while let Some(e) = cur {
                if st.borrow::<HostDom>().0.node_type(e) != 1 {
                    break;
                }
                if css_computed_one(st, e, "vertical-align").trim() == value {
                    return true;
                }
                cur = st.borrow::<HostDom>().0.parent(e);
            }
            false
        }
        "text-align" => resolved_alignment(st, el) == value,
        _ => false,
    }
}

/// An element's alignment as the justify commands read it: `start` / `end` resolved by its direction.
fn resolved_alignment(st: &mut OpState, el: u32) -> String {
    let v = css_computed_one(st, el, "text-align").trim().to_string();
    if v == "start" || v == "end" {
        let ltr = css_computed_one(st, el, "direction").trim() != "rtl";
        return if (v == "start") == ltr { "left" } else { "right" }.to_string();
    }
    v
}

fn rendered(st: &mut OpState, node: u32) -> bool {
    let (is_text, text_ok, el) = {
        let dom = &st.borrow::<HostDom>().0;
        let is_text = dom.node_type(node) == 3;
        let text_ok = !is_text || dom.node_value(node).is_some_and(|v| !v.trim().is_empty());
        (is_text, text_ok, element_of(dom, node))
    };
    if is_text && !text_ok {
        return false;
    }
    let Some(mut e) = el else { return false };
    loop {
        if css_computed_one(st, e, "display").trim() == "none" {
            return false;
        }
        let dom = &st.borrow::<HostDom>().0;
        match dom.parent(e).filter(|&p| dom.node_type(p) == 1) {
            Some(p) => e = p,
            None => return true,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tri {
    False,
    True,
    Mixed,
}

/// `EditingStyle::TriStateOfStyle` over the selection: a caret's style at it; a range's rendered editable nodes from
/// its start in tree order, the first node's answer unless a text node disagrees.
fn style_state(st: &mut OpState, s: &Snapshot, property: &str, value: &str) -> Tri {
    let bool_tri = |b: bool| if b { Tri::True } else { Tri::False };
    match s.kind {
        Kind::None => Tri::False,
        Kind::Caret => {
            let Some(el) = style_element(&st.borrow::<HostDom>().0, s) else { return Tri::False };
            bool_tri(has_style(st, el, property, value))
        }
        Kind::Range => {
            if let Some(c) = s.control {
                return bool_tri(has_style(st, c, property, value));
            }
            // the selection's canonical ends (`VisibleSelection`): an element position's deep equivalent
            let (from, to) = {
                let dom = &st.borrow::<HostDom>().0;
                (deep_start(dom, s.start), deep_end(dom, s.end))
            };
            let mut state = Tri::False;
            let mut first = true;
            let mut cur = Some(from);
            let mut guard = 0;
            while let Some(n) = cur {
                guard += 1;
                if guard > 100_000 {
                    break;
                }
                let (editable, is_text, el) = {
                    let dom = &st.borrow::<HostDom>().0;
                    (editable_root(dom, n).is_some(), dom.node_type(n) == 3, element_of(dom, n))
                };
                if editable && rendered(st, n) {
                    if let Some(el) = el {
                        let node_state = bool_tri(has_style(st, el, property, value));
                        if first {
                            state = node_state;
                            first = false;
                        } else if node_state != state && is_text {
                            return Tri::Mixed;
                        }
                    }
                }
                if n == to {
                    break;
                }
                cur = next_preorder(&st.borrow::<HostDom>().0, n);
            }
            state
        }
    }
}

/// `SelectionListState`: a caret in a list of `tag`; a range whose start and end are in the same one, which holds no
/// list of the other kind.
fn list_state(st: &OpState, s: &Snapshot, tag: &str) -> Tri {
    let dom = &st.borrow::<HostDom>().0;
    let enclosing = |node: u32| -> Option<u32> {
        let root = editable_root(dom, node).map(|(r, _)| r);
        let mut cur = Some(node);
        while let Some(n) = cur {
            if dom.node_type(n) == 1 && dom.is_html_ns(n) && dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case(tag)) {
                return Some(n);
            }
            if Some(n) == root {
                break;
            }
            cur = dom.parent(n);
        }
        None
    };
    match s.kind {
        Kind::None => Tri::False,
        Kind::Caret => {
            if enclosing(s.start.0).is_some() {
                Tri::True
            } else {
                Tri::False
            }
        }
        Kind::Range => match (enclosing(s.start.0), enclosing(s.end.0)) {
            (Some(a), Some(b)) if a == b => {
                let other = if tag == "ul" { "ol" } else { "ul" };
                if dom.children(a).iter().any(|&c| dom.tag_name(c).is_some_and(|t| t.eq_ignore_ascii_case(other))) {
                    Tri::False
                } else {
                    Tri::True
                }
            }
            _ => Tri::False,
        },
    }
}

fn state_of(st: &mut OpState, doc: u32, cmd: &Command, s: &Snapshot) -> Tri {
    match cmd.state {
        S::None => Tri::False,
        S::StyleWithCss => {
            if flags(st, doc).0 {
                Tri::True
            } else {
                Tri::False
            }
        }
        S::List(tag) => list_state(st, s, tag),
        S::Style(p, v) => style_state(st, s, p, v),
    }
}

/// `FontSizeFunctions::LegacyFontSize`: the HTML font size 1-7 nearest the computed size, by Blink's strict table for
/// a 16px default (13px for `monospace`).
fn legacy_font_size(px: f32, monospace: bool) -> u32 {
    let table: [i32; 8] = if monospace { [9, 10, 12, 13, 16, 20, 26, 39] } else { [9, 10, 13, 16, 18, 24, 32, 48] };
    let px = px as i32;
    for i in 1..7 {
        if px * 2 < table[i] + table[i + 1] {
            return i as u32;
        }
    }
    7
}

fn value_of(st: &mut OpState, doc: u32, cmd: &Command, s: &Snapshot) -> String {
    match cmd.value {
        V::Empty => String::new(),
        V::StateOrNull => {
            if cmd.state == S::None {
                return String::new();
            }
            if state_of(st, doc, cmd, s) == Tri::True { "true" } else { "false" }.to_string()
        }
        V::DefaultParagraphSeparator => if flags(st, doc).1 { "p" } else { "div" }.to_string(),
        V::Style(property) => {
            if s.kind == Kind::None {
                return String::new();
            }
            let Some(el) = style_element(&st.borrow::<HostDom>().0, s) else { return String::new() };
            if property == "background-color" {
                // the background colour in effect: the first not transparent up the tree
                let mut cur = Some(el);
                while let Some(e) = cur {
                    let v = css_computed_one(st, e, property);
                    if !matches!(v.trim(), "rgba(0, 0, 0, 0)" | "transparent") {
                        return v;
                    }
                    let dom = &st.borrow::<HostDom>().0;
                    cur = dom.parent(e).filter(|&p| dom.node_type(p) == 1);
                }
            }
            css_computed_one(st, el, property)
        }
        V::FontSize => {
            if s.kind == Kind::None {
                return String::new();
            }
            let Some(el) = style_element(&st.borrow::<HostDom>().0, s) else { return String::new() };
            let px = css_computed_one(st, el, "font-size").trim_end_matches("px").parse::<f32>().unwrap_or(16.0);
            let monospace = css_computed_one(st, el, "font-family").trim() == "monospace";
            legacy_font_size(px, monospace).to_string()
        }
        V::Justify => {
            if s.kind == Kind::None {
                return String::new();
            }
            let Some(el) = style_element(&st.borrow::<HostDom>().0, s) else { return String::new() };
            resolved_alignment(st, el)
        }
        V::FormatBlock => {
            let Some((root, _)) = s.edit else { return String::new() };
            if s.kind == Kind::None || s.control.is_some() {
                return String::new();
            }
            let dom = &st.borrow::<HostDom>().0;
            // the range's common ancestor, up to a format-block element; none when that is the editable root or above
            let common = {
                let mut a = s.start.0;
                loop {
                    if dom.contains(a, s.end.0) {
                        break a;
                    }
                    match dom.parent(a) {
                        Some(p) => a = p,
                        None => break a,
                    }
                }
            };
            const BLOCKS: [&str; 22] = [
                "address", "article", "aside", "blockquote", "dd", "div", "dl", "dt", "footer", "h1", "h2", "h3", "h4", "h5",
                "h6", "header", "hgroup", "main", "nav", "p", "pre", "section",
            ];
            let mut cur = Some(common);
            while let Some(n) = cur {
                if dom.node_type(n) == 1 && dom.is_html_ns(n) && dom.tag_name(n).is_some_and(|t| BLOCKS.iter().any(|b| t.eq_ignore_ascii_case(b))) {
                    if dom.contains(n, root) {
                        return String::new();
                    }
                    return dom.tag_name(n).unwrap_or("").to_ascii_lowercase();
                }
                cur = dom.parent(n);
            }
            String::new()
        }
    }
}

// ── the members ──────────────────────────────────────────────────────────────────────────────────

/// The document a call is on -- its arena root -- when it is an HTML document (else Blink's InvalidStateError,
/// thrown here).
fn html_document(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) -> Option<u32> {
    let this = args.this();
    if crate::core::dom::document::doc_is_not_html(scope, this.into()) {
        throw_dom(scope, &format!("Failed to execute '{member}' on 'Document': {member} is only supported on HTML documents."), "InvalidStateError");
        return None;
    }
    let id = crate::core::dom::shared::document_node_id(scope, this.into())?;
    if id != crate::core::events::shared::ON_DOCUMENT_ID {
        return Some(id);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    Some(st.borrow::<HostDom>().0.root())
}

/// The command `name` names on document `doc`: none for a name Blink does not support from the DOM, or for a
/// document no browsing context shows (`GetCommand`'s null command).
fn command_on(scope: &mut v8::PinScope, doc: u32, name: &str) -> Option<&'static Command> {
    let cmd = command(name)?;
    crate::core::editing::selection::document_selection(scope, doc)?;
    Some(cmd)
}

fn query_name(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<String> {
    args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope))
}

pub(crate) fn cb_query_command_supported(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = query_name(scope, &args) else { return };
    let Some(doc) = html_document(scope, &args, "queryCommandSupported") else { return };
    rv.set_bool(command_on(scope, doc, &name).is_some());
}

pub(crate) fn cb_query_command_enabled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = query_name(scope, &args) else { return };
    let Some(doc) = html_document(scope, &args, "queryCommandEnabled") else { return };
    let Some(cmd) = command_on(scope, doc, &name) else {
        rv.set_bool(false);
        return;
    };
    let s = snapshot(scope, doc);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let on = enabled(&op_state.borrow(), cmd, &s);
    rv.set_bool(on);
}

fn tri_member(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) -> Option<Tri> {
    let name = query_name(scope, args)?;
    let doc = html_document(scope, args, member)?;
    let Some(cmd) = command_on(scope, doc, &name) else { return Some(Tri::False) };
    let s = snapshot(scope, doc);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    Some(state_of(&mut st, doc, cmd, &s))
}

pub(crate) fn cb_query_command_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(t) = tri_member(scope, &args, "queryCommandState") {
        rv.set_bool(t == Tri::True);
    }
}

pub(crate) fn cb_query_command_indeterm(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(t) = tri_member(scope, &args, "queryCommandIndeterm") {
        rv.set_bool(t == Tri::Mixed);
    }
}

pub(crate) fn cb_query_command_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = query_name(scope, &args) else { return };
    let Some(doc) = html_document(scope, &args, "queryCommandValue") else { return };
    let value = match command_on(scope, doc, &name) {
        Some(cmd) => {
            let s = snapshot(scope, doc);
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            value_of(&mut st, doc, cmd, &s)
        }
        None => String::new(),
    };
    let v = str_val(scope, &value);
    rv.set(v);
}

/// `execCommand(commandId, showUI, value)`: the arguments converted in order (the value a TrustedHTML read as itself,
/// null `""`), the document checked, then the command run when it is enabled -- what needs no editing does what
/// Blink's does; an editing command changes nothing and answers `false` (see the module doc).
pub(crate) fn cb_exec_command(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = query_name(scope, &args) else { return };
    let _show_ui = args.get(1).boolean_value(scope);
    let raw = args.get(2);
    let value_is_string = !(raw.is_undefined() || raw.is_null()) && !crate::core::trusted_types::tables::is_trusted(scope, raw, "TrustedHTML");
    let value = if raw.is_undefined() || raw.is_null() {
        String::new()
    } else {
        match crate::core::trusted_types::tables::sink_text(scope, raw, "TrustedHTML") {
            Some(v) => v,
            None => return,
        }
    };
    let Some(doc) = html_document(scope, &args, "execCommand") else { return };
    let Some(cmd) = command_on(scope, doc, &name) else {
        rv.set_bool(false);
        return;
    };
    // `insertHTML` reads its value as HTML: a string goes through the document's Trusted Types first
    // ("Document execCommand"; Blink `TrustedTypesCheck` in `Document::execCommand`)
    let value = if value_is_string && name.eq_ignore_ascii_case("inserthtml") {
        let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: "Document execCommand", context: "Failed to execute 'execCommand' on 'Document'" };
        let Some(v) = crate::core::trusted_types::tables::check_text(scope, value, &sink, crate::core::trusted_types::tables::SinkAt::Node(doc)) else { return };
        v
    } else {
        value
    };
    let s = snapshot(scope, doc);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !enabled(&op_state.borrow(), cmd, &s) {
        rv.set_bool(false);
        return;
    }
    let done = match cmd.exec {
        X::Never | X::Edit => false,
        X::Print => true,
        X::StyleWithCss => {
            set_flags(&mut op_state.borrow_mut(), doc, |f| f.0 = !value.eq_ignore_ascii_case("false"));
            true
        }
        X::UseCss => {
            set_flags(&mut op_state.borrow_mut(), doc, |f| f.0 = value.eq_ignore_ascii_case("false"));
            true
        }
        X::DefaultParagraphSeparator => {
            if value.eq_ignore_ascii_case("p") {
                set_flags(&mut op_state.borrow_mut(), doc, |f| f.1 = true);
            } else if value.eq_ignore_ascii_case("div") {
                set_flags(&mut op_state.borrow_mut(), doc, |f| f.1 = false);
            }
            true
        }
        X::Unselect => {
            if let Some(sel) = crate::core::editing::selection::document_selection(scope, doc) {
                crate::core::editing::selection::selection_clear(scope, sel);
            }
            true
        }
        X::SelectAll => select_all(scope, doc, &s),
        X::FindString => find_string(scope, doc, &value, &s),
    };
    rv.set_bool(done);
}

/// `FrameSelection::SelectAll`: a text control's whole value; an editable root's contents; else the document's,
/// from its first rendered text to the end of its body.
fn select_all(scope: &mut v8::PinScope, doc: u32, s: &Snapshot) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(c) = s.control {
        let obj = crate::binding::protos::wrap_cached(scope, &op_state, c);
        crate::core::html::html_input_element::text_control_select_all(scope, obj);
        return true;
    }
    let target = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let scope_root = match s.edit {
            Some((root, _)) => Some(root),
            None => crate::core::dom::document::doc_element_child(dom, doc).and_then(|html| {
                dom.children(html).into_iter().find(|&c| dom.tag_name(c).is_some_and(|t| t.eq_ignore_ascii_case("body"))).or(Some(html))
            }),
        };
        scope_root.map(|r| {
            let mut first = None;
            let mut cur = dom.first_child(r);
            while let Some(n) = cur {
                if !dom.contains(r, n) {
                    break;
                }
                if dom.node_type(n) == 3 && dom.node_value(n).is_some_and(|v| !v.trim().is_empty()) && !in_unrendered_text_parent(dom, n) {
                    first = Some(n);
                    break;
                }
                cur = next_preorder(dom, n);
            }
            let count = dom.children(r).len() as u32;
            // nothing visible to select: Blink's selection stays empty (Chrome 150: `selectAll` on a page
            // holding only its script answers true and leaves `unselect` disabled)
            first.map(|t| ((t, 0), (r, count)))
        })
    };
    let Some(Some((anchor, focus))) = target else { return true };
    if let Some(sel) = crate::core::editing::selection::document_selection(scope, doc) {
        crate::core::editing::selection::selection_set(scope, sel, anchor, focus);
    }
    true
}

fn in_unrendered_text_parent(dom: &korvus_dom::Dom, text: u32) -> bool {
    dom.parent(text)
        .and_then(|p| dom.tag_name(p))
        .is_some_and(|t| matches!(t.to_ascii_lowercase().as_str(), "script" | "style" | "template" | "noscript" | "title" | "textarea"))
}

/// `Editor::FindString` (case-insensitive, wrapping): the next occurrence of `needle` in the document's rendered text
/// after the selection, selected; `false` when there is none.
fn find_string(scope: &mut v8::PinScope, doc: u32, needle: &str, s: &Snapshot) -> bool {
    if needle.is_empty() {
        return false;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let found = {
        let mut st = op_state.borrow_mut();
        // the document's text nodes in order, rendered
        let mut texts: Vec<(u32, Vec<u16>)> = Vec::new();
        let start_from = {
            let dom = &st.borrow::<HostDom>().0;
            let mut cur = dom.first_child(doc);
            while let Some(n) = cur {
                if dom.node_type(n) == 3 && !in_unrendered_text_parent(dom, n) {
                    texts.push((n, dom.node_value(n).unwrap_or_default().encode_utf16().collect()));
                }
                cur = next_preorder(dom, n);
            }
            if s.kind == Kind::None || s.control.is_some() { None } else { Some(s.end) }
        };
        texts.retain(|(n, _)| rendered(&mut st, *n));
        let lower: Vec<u16> = needle.to_lowercase().encode_utf16().collect();
        let fold = |u: u16| -> u16 { char::from_u32(u as u32).map_or(u, |c| c.to_lowercase().next().map_or(u, |l| l as u32 as u16)) };
        // search each text node (Blink's finder spans nodes; a match inside one node is what this finds)
        let mut hits: Vec<(usize, usize)> = Vec::new();
        for (i, (_, units)) in texts.iter().enumerate() {
            if units.len() < lower.len() {
                continue;
            }
            for at in 0..=units.len() - lower.len() {
                if units[at..at + lower.len()].iter().zip(&lower).all(|(a, b)| fold(*a) == *b) {
                    hits.push((i, at));
                }
            }
        }
        let dom = &st.borrow::<HostDom>().0;
        let after = |&(i, at): &(usize, usize)| match start_from {
            None => true,
            Some((n, o)) => crate::core::dom::shared::cmp_points(dom, texts[i].0, at as i64, n, o as i64) >= 0,
        };
        hits.iter().find(|h| after(h)).or_else(|| hits.first()).map(|&(i, at)| (texts[i].0, at as u32, (at + lower.len()) as u32))
    };
    let Some((node, a, b)) = found else { return false };
    if let Some(sel) = crate::core::editing::selection::document_selection(scope, doc) {
        crate::core::editing::selection::selection_set(scope, sel, (node, a), (node, b));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legacy_font_size_is_blinks_strict_table() {
        assert_eq!(legacy_font_size(16.0, false), 3);
        assert_eq!(legacy_font_size(24.0, false), 5);
        assert_eq!(legacy_font_size(13.33, true), 3);
        assert_eq!(legacy_font_size(10.0, false), 1);
        assert_eq!(legacy_font_size(100.0, false), 7);
    }

    #[test]
    fn command_names_are_matched_ignoring_case() {
        assert!(command("BOLD").is_some());
        assert!(command("stylewithcss").is_some());
        assert!(command("paste").is_none(), "DOM paste is not allowed");
        assert!(command("backwardDelete").is_none(), "menu / key binding only");
    }
}
