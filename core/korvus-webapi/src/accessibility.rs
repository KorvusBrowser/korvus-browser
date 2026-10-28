//! The accessibility tree a DevTools client reads (`Accessibility.getFullAXTree` and the rest of the domain), built
//! from the arena, the cascade and the controls' state -- never through the page's members, which a page could
//! replace to steer what an agent sees.
//!
//! The model is Chrome 150's tree, recorded with a raw CDP client on `plugins/korvus-cdp/tests/fixtures/
//! accessibility.html` (`accessibility.chrome-150.json`) and on a page holding every HTML element and ARIA role: the
//! role and internal role number of each, how a name is found (`aria-labelledby`, `aria-label`, the host language's
//! label, the content, `title`), the states each role reports and in which order, which nodes Chrome keeps as ignored
//! and which it leaves out, and how a text node's whitespace collapses. What a client reads off the tree -- Stagehand's
//! `act`/`observe`/`extract`, Puppeteer's `page.accessibility.snapshot()` and its `aria/` selectors -- is the role,
//! the name, the states and the parentage of the nodes that are not ignored.
//!
//! What is NOT modelled, because this engine has no such node: the user-agent shadow trees of form controls (a
//! Chrome text field's `generic` editor child, a `<select>`'s inner slots), `::marker` and `::before`/`::after`
//! content as DOM nodes (a list marker is a node with no `backendDOMNodeId` here), one `InlineTextBox` per laid-out
//! line (here one per text node), and the `sources` Chrome lists beside a name.
use std::collections::{HashMap, HashSet};

use deno_core::v8;
use korvus_dom::{Dom, NodeId};
use page::style::property::PropertyId;

/// A role as CDP names it: `role.value`, whether it is one of Chrome's internal roles (`role.type` `internalRole`,
/// e.g. `StaticText`) rather than an ARIA one, and Chrome's own role number (`chromeRole`, `ax::mojom::Role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    pub name: &'static str,
    pub internal: bool,
    pub chrome: u32,
}

const fn aria(name: &'static str, chrome: u32) -> Role {
    Role { name, internal: false, chrome }
}

const fn internal(name: &'static str, chrome: u32) -> Role {
    Role { name, internal: true, chrome }
}

const NONE: Role = aria("none", 0);
const GENERIC: Role = aria("generic", 88);
const STATIC_TEXT: Role = internal("StaticText", 158);
const INLINE_TEXT_BOX: Role = internal("InlineTextBox", 101);
const ROOT_WEB_AREA: Role = internal("RootWebArea", 144);
const LIST_MARKER: Role = internal("ListMarker", 116);
const LINE_BREAK: Role = internal("LineBreak", 109);
const MENU_LIST_POPUP: Role = internal("MenuListPopup", 128);

/// The ARIA roles Chrome 150 maps a `role` token to (the first token it knows wins), with its numbers. `none` and
/// `presentation` are not here: they take an element out of the tree (Chrome lists neither). The roles that need a
/// context (`listitem`, `option`, `treeitem`) and the ones that need a name (`form`, `region`) are decided in
/// [`Builder::role_of`].
const ARIA_ROLES: &[(&str, Role)] = &[
    ("alert", aria("alert", 2)),
    ("alertdialog", aria("alertdialog", 3)),
    ("application", aria("application", 4)),
    ("article", aria("article", 5)),
    ("banner", aria("banner", 7)),
    ("blockquote", aria("blockquote", 8)),
    ("button", aria("button", 9)),
    ("caption", aria("caption", 11)),
    ("cell", aria("cell", 13)),
    ("checkbox", aria("checkbox", 14)),
    ("code", aria("code", 16)),
    ("columnheader", aria("columnheader", 19)),
    ("combobox", aria("combobox", 20)),
    ("comment", aria("comment", 23)),
    ("complementary", aria("complementary", 22)),
    ("contentinfo", aria("contentinfo", 26)),
    ("definition", aria("definition", 29)),
    ("deletion", aria("deletion", 24)),
    ("dialog", aria("dialog", 35)),
    ("directory", aria("list", 111)),
    ("document", aria("document", 79)),
    ("emphasis", aria("emphasis", 81)),
    ("feed", aria("feed", 82)),
    ("figure", aria("figure", 84)),
    ("form", aria("form", 87)),
    ("generic", GENERIC),
    ("grid", aria("grid", 92)),
    ("gridcell", aria("gridcell", 212)),
    ("group", aria("group", 93)),
    ("heading", aria("heading", 96)),
    ("image", aria("image", 99)),
    ("img", aria("image", 99)),
    ("insertion", aria("insertion", 25)),
    ("link", aria("link", 110)),
    ("list", aria("list", 111)),
    ("listbox", aria("listbox", 112)),
    ("listitem", aria("listitem", 115)),
    ("log", aria("log", 117)),
    ("main", aria("main", 118)),
    ("mark", aria("mark", 119)),
    ("marquee", aria("marquee", 120)),
    ("math", aria("math", 121)),
    ("menu", aria("menu", 122)),
    ("menubar", aria("menubar", 123)),
    ("menuitem", aria("menuitem", 124)),
    ("menuitemcheckbox", aria("menuitemcheckbox", 125)),
    ("menuitemradio", aria("menuitemradio", 126)),
    ("meter", aria("meter", 129)),
    ("navigation", aria("navigation", 130)),
    ("note", aria("note", 131)),
    ("option", aria("option", 113)),
    ("paragraph", aria("paragraph", 133)),
    ("progressbar", aria("progressbar", 140)),
    ("radio", aria("radio", 141)),
    ("radiogroup", aria("radiogroup", 142)),
    ("region", aria("region", 143)),
    ("row", aria("row", 145)),
    ("rowgroup", aria("rowgroup", 146)),
    ("rowheader", aria("rowheader", 147)),
    ("scrollbar", aria("scrollbar", 150)),
    ("search", aria("search", 152)),
    ("searchbox", aria("searchbox", 153)),
    ("sectionfooter", aria("sectionfooter", 86)),
    ("sectionheader", aria("sectionheader", 95)),
    ("separator", aria("separator", 157)),
    ("slider", aria("slider", 155)),
    ("spinbutton", aria("spinbutton", 156)),
    ("status", aria("status", 159)),
    ("strong", aria("strong", 160)),
    ("subscript", aria("subscript", 185)),
    ("suggestion", aria("suggestion", 161)),
    ("superscript", aria("superscript", 186)),
    ("switch", aria("switch", 163)),
    ("tab", aria("tab", 164)),
    ("table", aria("table", 167)),
    ("tablist", aria("tablist", 165)),
    ("tabpanel", aria("tabpanel", 166)),
    ("term", aria("term", 169)),
    ("textbox", aria("textbox", 170)),
    ("time", aria("time", 172)),
    ("timer", aria("timer", 173)),
    ("toolbar", aria("toolbar", 176)),
    ("tooltip", aria("tooltip", 177)),
    ("tree", aria("tree", 178)),
    ("treegrid", aria("treegrid", 179)),
    ("treeitem", aria("treeitem", 180)),
    ("doc-abstract", aria("doc-abstract", 38)),
    ("doc-chapter", aria("doc-chapter", 46)),
    ("graphics-document", aria("graphics-document", 89)),
    ("graphics-symbol", aria("graphics-symbol", 91)),
];

/// The roles whose name comes from their content when nothing names them otherwise (accname's "name from content",
/// as Chrome applies it: a `row`, a `listitem`, a `LabelText` and a `caption` are not among them -- Chrome 150 names
/// each `""`).
fn name_from_content(role: &str) -> bool {
    matches!(
        role,
        "button" | "cell" | "checkbox" | "columnheader" | "DisclosureTriangle" | "gridcell" | "heading" | "link" | "math"
            | "menuitem" | "menuitemcheckbox" | "menuitemradio" | "option" | "radio" | "rowheader" | "switch" | "tab"
            | "term" | "tooltip" | "treeitem"
    )
}

/// A value of an AX node or of one of its properties, typed as CDP's `AXValue` types it.
#[derive(Debug, Clone, PartialEq)]
pub enum AxValue {
    Boolean(bool),
    BooleanOrUndefined(bool),
    /// `"true"`, `"false"` or `"mixed"`.
    Tristate(&'static str),
    Token(String),
    TokenList(String),
    Integer(i64),
    Number(f64),
    String(String),
    /// A relation's `relatedNodes`, and the reason an ignored node names (`labelFor`, `ariaHiddenSubtree`).
    Idref(Vec<Related>),
    /// `describedby`: the attribute's value and the nodes it names.
    IdrefList(String, Vec<Related>),
    /// `labelledby`: the nodes a name was taken from.
    NodeList(Vec<Related>),
}

/// One of a relation's `relatedNodes`: the node (its arena id), the idref it was named by, its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Related {
    pub node: NodeId,
    pub idref: Option<String>,
    pub text: Option<String>,
}

/// One node of the tree.
#[derive(Debug, Clone)]
pub struct AxNode {
    /// `nodeId`: the DOM node's backend id (arena id + 1) for a node made for one -- Chrome's are the same number. A
    /// node for a box Chrome makes of a node this engine does not have (a `::marker`, a `<select>`'s popup, a button's
    /// label text) has a positive id past the arena's; one Chrome makes of no node at all (an `InlineTextBox`) a
    /// negative one, as Chrome's -- Stagehand drops a node with a negative id, and everything under it.
    pub id: i64,
    /// The arena node it is made for.
    pub dom: Option<NodeId>,
    pub role: Role,
    /// `ignoredReasons`: empty for a node that is not ignored.
    pub ignored: Vec<(&'static str, AxValue)>,
    pub name: String,
    pub description: Option<String>,
    pub value: Option<AxValue>,
    pub properties: Vec<(&'static str, AxValue)>,
    /// Indices into [`AxTree::nodes`].
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

impl AxNode {
    pub fn is_ignored(&self) -> bool {
        !self.ignored.is_empty()
    }
}

/// A document's tree: `nodes[0]` is its `RootWebArea`.
#[derive(Debug, Clone, Default)]
pub struct AxTree {
    pub nodes: Vec<AxNode>,
}

impl AxTree {
    /// The node made for arena node `dom`.
    pub fn find_dom(&self, dom: NodeId) -> Option<usize> {
        self.nodes.iter().position(|n| n.dom == Some(dom))
    }

    /// The node whose `nodeId` is `id`.
    pub fn find_id(&self, id: i64) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// The nodes in the order Chrome lists them: breadth first from the root. With `depth`, only the nodes with at
    /// most that many unignored ancestors (Chrome counts the levels of the tree a client sees: `html` and `body`,
    /// ignored, do not use one up).
    pub fn breadth_first(&self, depth: Option<usize>) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut queue = std::collections::VecDeque::new();
        if !self.nodes.is_empty() {
            queue.push_back((0usize, 0usize));
        }
        while let Some((i, above)) = queue.pop_front() {
            if depth.is_some_and(|d| above > d) {
                continue;
            }
            out.push(i);
            let below = above + usize::from(!self.nodes[i].is_ignored());
            for &c in &self.nodes[i].children {
                queue.push_back((c, below));
            }
        }
        out
    }

    /// `i`'s ancestors, nearest first.
    pub fn ancestors(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut cur = self.nodes.get(i).and_then(|n| n.parent);
        while let Some(p) = cur {
            out.push(p);
            cur = self.nodes[p].parent;
        }
        out
    }

    /// `i` and its descendants, in document order.
    pub fn subtree(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![i];
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev().copied());
        }
        out
    }
}

/// The tree of document `doc` (the page's arena root or a frame's), whose url is `url`. `None` when `doc` is no
/// document.
pub fn ax_tree(scope: &mut v8::PinScope, doc: NodeId, url: &str) -> Option<AxTree> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let elements = {
        let st = op_state.borrow();
        let dom = &st.try_borrow::<crate::HostDom>()?.0;
        if (doc as usize) >= dom.len() || dom.node_type(doc) != 9 {
            return None;
        }
        let mut all = Vec::new();
        collect_elements(dom, doc, &mut all);
        all
    };
    let props = [PropertyId::Display, PropertyId::Visibility, PropertyId::ContentVisibility, PropertyId::ListStyleType, PropertyId::WhiteSpaceCollapse];
    let rows = crate::core::css::cssom::computed_texts(&mut op_state.borrow_mut(), &elements, &props);
    let mut styles = HashMap::with_capacity(elements.len());
    for (&el, row) in elements.iter().zip(rows) {
        if let Some(row) = row {
            styles.insert(el, Style::of(&row));
        }
    }
    // An element with a `click`, `mousedown` or `mouseup` listener of its own is clickable (Blink's `IsClickable`):
    // an `<a>` with no `href` is a link for it (vantechjournal.com's "Sign Up"), a generic element a node of its own.
    // Chrome 152: an `onclick` attribute counts, a `keydown`, `pointerdown` or `mouseover` listener does not, nor a
    // parent's, nor `tabindex`.
    let clickable = crate::core::events::event_target::nodes_listening(scope, &["click", "mousedown", "mouseup"], &elements);
    let st = op_state.borrow();
    let dom = &st.try_borrow::<crate::HostDom>()?.0;
    let theme = crate::core::css::cssom::media_context(&st).theme;
    let focused = crate::core::events::focus::focused_node(&st);
    let base = if doc == dom.root() { crate::core::dom::document::document_base_url(dom, url) } else { url.to_string() };
    let mut b = Builder {
        dom,
        styles,
        labels: HashMap::new(),
        text: HashMap::new(),
        clickable,
        focused,
        submit_label: if theme.submit_label.is_empty() { "Submit" } else { theme.submit_label },
        reset_label: if theme.reset_label.is_empty() { "Reset" } else { theme.reset_label },
        file_labels: page::layout::replaced::file_labels(&theme, false),
        base,
        nodes: Vec::new(),
        next_synthetic: 0,
        next_box: dom.len() as i64 + 1,
    };
    b.find_labels(doc);
    b.lay_out_text(doc);
    b.build(doc, url);
    Some(AxTree { nodes: b.nodes })
}

/// Every element of the document rooted at `root`, shadow trees included.
fn collect_elements(dom: &Dom, root: NodeId, out: &mut Vec<NodeId>) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if dom.is_element(n) {
            out.push(n);
            if let Some(sr) = dom.shadow_root_of(n) {
                stack.push(sr);
            }
        }
        stack.extend(dom.children(n).into_iter().rev());
    }
}

/// What the tree reads off an element's computed style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Style {
    display: Display,
    visible: bool,
    /// `content-visibility: hidden`: the element is rendered, its contents are not.
    contents_hidden: bool,
    marker: Marker,
    /// `white-space-collapse` keeps the spaces (`pre`, `pre-wrap`, `break-spaces`) / keeps only the line breaks
    /// (`pre-line`).
    preserve: Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Display {
    None,
    Contents,
    Inline,
    /// An inline-level box with a block inside it (`inline-block`, `inline-flex` ...): a line of its own inside.
    InlineBlock,
    Block,
    ListItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    /// A `<summary>`'s triangle: Chrome lists it as an ignored node, not a `ListMarker`.
    Disclosure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Preserve {
    No,
    Spaces,
    Breaks,
}

impl Style {
    fn of(row: &[String]) -> Style {
        let d = row[0].as_str();
        let display = match d {
            "none" => Display::None,
            "contents" => Display::Contents,
            "inline" | "ruby" | "ruby-text" | "ruby-base" => Display::Inline,
            _ if d.contains("list-item") => Display::ListItem,
            _ if d.starts_with("inline") => Display::InlineBlock,
            _ => Display::Block,
        };
        let marker = match row[3].as_str() {
            "none" => Marker::None,
            "circle" => Marker::Circle,
            "square" => Marker::Square,
            "decimal" | "decimal-leading-zero" => Marker::Decimal,
            "lower-alpha" | "lower-latin" => Marker::LowerAlpha,
            "upper-alpha" | "upper-latin" => Marker::UpperAlpha,
            "lower-roman" => Marker::LowerRoman,
            "upper-roman" => Marker::UpperRoman,
            "disclosure-closed" | "disclosure-open" => Marker::Disclosure,
            _ => Marker::Disc,
        };
        let preserve = match row[4].as_str() {
            "preserve" | "break-spaces" | "preserve-spaces" => Preserve::Spaces,
            "preserve-breaks" => Preserve::Breaks,
            _ => Preserve::No,
        };
        Style { display, visible: !matches!(row[1].as_str(), "hidden" | "collapse"), contents_hidden: row[2] == "hidden", marker, preserve }
    }
}

const UNSTYLED: Style = Style { display: Display::Inline, visible: true, contents_hidden: false, marker: Marker::None, preserve: Preserve::No };

/// How an element enters the tree.
#[derive(Debug, Clone, PartialEq)]
enum Keep {
    /// As a node of its role.
    Node(Role),
    /// As an ignored node (role `none`) with this reason, its children under it.
    Ignored(&'static str, AxValue),
    /// Not at all: its children take its place.
    Drop,
}

struct Builder<'a> {
    dom: &'a Dom,
    styles: HashMap<NodeId, Style>,
    /// Each labelable control's `<label>` elements (its own `for`, or the label it is the first control of).
    labels: HashMap<NodeId, Vec<NodeId>>,
    /// Each rendered text node's text, its white space collapsed as the line it sits on collapses it.
    text: HashMap<NodeId, String>,
    /// The elements a click listener makes clickable ([`ax_tree`]).
    clickable: HashSet<NodeId>,
    focused: Option<NodeId>,
    submit_label: &'static str,
    reset_label: &'static str,
    /// A file input's button label and its no-file text, in the worn browser's language.
    file_labels: (&'static str, &'static str),
    base: String,
    nodes: Vec<AxNode>,
    next_synthetic: i64,
    /// The next id of a box with no node here ([`AxNode::id`]).
    next_box: i64,
}

/// The state a walk carries down the tree.
#[derive(Debug, Clone, Copy, Default)]
struct Walk {
    /// The `aria-hidden="true"` element above, if any: everything under it is ignored.
    hidden_by: Option<NodeId>,
    /// Under a `<label>` that labels a checkbox or radio: its text is part of the control's name, not a node.
    in_check_label: bool,
    /// The walk is passing a `<slot>`'s assigned nodes.
    slotted: bool,
}

impl<'a> Builder<'a> {
    fn style(&self, n: NodeId) -> Style {
        self.styles.get(&n).copied().unwrap_or(UNSTYLED)
    }

    fn tag(&self, n: NodeId) -> &str {
        if self.dom.is_html_ns(n) {
            self.dom.tag_name(n).unwrap_or("")
        } else {
            ""
        }
    }

    fn attr(&self, n: NodeId, k: &str) -> Option<&'a str> {
        self.dom.get_attribute(n, k)
    }

    /// The children a node renders, in the flat tree: a host's shadow tree, a slot's assigned nodes (or its fallback
    /// content), anything else's own children. A `<template>`'s content renders nothing.
    fn flat_children(&self, n: NodeId) -> Vec<NodeId> {
        if self.dom.is_element(n) {
            if let Some(sr) = self.dom.shadow_root_of(n) {
                return self.dom.children(sr);
            }
            if self.dom.is_html_slot_element(n) {
                if let Some(c) = self.dom.slot_rendered_children(n) {
                    return c;
                }
            }
            if self.tag(n) == "template" {
                return Vec::new();
            }
        }
        self.dom.children(n)
    }

    /// Does element `el` render -- not `display: none`, not a `<noscript>`, not an unslotted child of a host?
    fn renders(&self, el: NodeId) -> bool {
        self.style(el).display != Display::None && !page::layout::tree::renders_nothing(self.dom, el)
    }

    /// Do `parent`'s contents render, `child` among them (a closed `<details>` renders only its summary)?
    fn contents_render(&self, parent: NodeId, child: NodeId) -> bool {
        !self.style(parent).contents_hidden && !page::layout::tree::closed_details_content(self.dom, parent, child)
    }

    fn block_level(&self, el: NodeId) -> bool {
        matches!(self.style(el).display, Display::Block | Display::ListItem)
    }

    /// A box that sits on a line as one piece: a replaced element, a form control, an inline-block.
    fn atomic_inline(&self, el: NodeId) -> bool {
        self.style(el).display == Display::InlineBlock
            || matches!(self.tag(el), "img" | "input" | "select" | "textarea" | "button" | "iframe" | "video" | "audio" | "canvas" | "object" | "embed" | "meter" | "progress")
            || self.dom.tag_name(el).is_some_and(|t| t.eq_ignore_ascii_case("svg"))
    }

    fn form_control(&self, el: NodeId) -> bool {
        matches!(self.tag(el), "input" | "select" | "textarea" | "button")
    }

    // ── Labels ───────────────────────────────────────────────────────────────────────────────

    fn labelable(&self, el: NodeId) -> bool {
        match self.tag(el) {
            "input" => self.dom.input_type(el) != "hidden",
            "button" | "meter" | "output" | "progress" | "select" | "textarea" => true,
            _ => false,
        }
    }

    /// Each `<label>`'s labeled control (HTML: its `for`'s element if that is labelable, else -- with no `for` -- its
    /// first labelable descendant), recorded from the control's side.
    fn find_labels(&mut self, doc: NodeId) {
        let mut found: Vec<(NodeId, NodeId)> = Vec::new();
        let mut stack = vec![doc];
        while let Some(n) = stack.pop() {
            if self.dom.is_element(n) {
                if self.tag(n) == "label" {
                    if let Some(c) = self.label_control(n) {
                        found.push((c, n));
                    }
                }
                if let Some(sr) = self.dom.shadow_root_of(n) {
                    stack.push(sr);
                }
            }
            stack.extend(self.dom.children(n).into_iter().rev());
        }
        for (control, label) in found {
            self.labels.entry(control).or_default().push(label);
        }
    }

    fn label_control(&self, label: NodeId) -> Option<NodeId> {
        if let Some(id) = self.attr(label, "for") {
            let found = self.dom.get_element_by_id_in(self.dom.root_of(label), id)?;
            return self.labelable(found).then_some(found);
        }
        self.dom.find_descendant(label, |d| self.dom.is_element(d) && self.labelable(d))
    }

    /// The control a `<label>` labels when Chrome leaves the label out as a node (`labelFor`), its text being the
    /// control's name: a checkbox or a radio button, the label holding nothing but text and the control itself. Chrome
    /// 152 keeps a `LabelText` round the control for anything more -- a `<span>`, a `<strong>`, an `<img>`, a link, a
    /// `<span>` round the control (Stagehand's `login` and YC's company filters: `<label><input type=checkbox><span>`).
    fn check_label_control(&self, label: NodeId) -> Option<NodeId> {
        let c = self.label_control(label)?;
        let plain = self.dom.children(label).into_iter().all(|k| k == c || !self.dom.is_element(k));
        (self.tag(c) == "input" && matches!(self.dom.input_type(c), "checkbox" | "radio") && plain).then_some(c)
    }

    // ── Text on its lines ──────────────────────────────────────────────────────────────────

    /// Collapse each rendered text node's white space as its line does (CSS Text 3 §4.1.1): a run of spaces, tabs and
    /// line breaks is one space, a space at the start of a line -- or after another collapsible space -- goes, and so
    /// does one at the end of a line. A block's edges and a `<br>` end a line; an inline-block is a line of its own
    /// inside one.
    fn lay_out_text(&mut self, doc: NodeId) {
        let mut line = Line { at_start: true, trailing: None };
        for c in self.flat_children(doc) {
            self.text_walk(c, true, &mut line);
        }
        self.end_line(&mut line);
    }

    fn end_line(&mut self, line: &mut Line) {
        if let Some(t) = line.trailing.take() {
            if let Some(s) = self.text.get_mut(&t) {
                if s.ends_with(' ') {
                    s.pop();
                }
            }
        }
        line.at_start = true;
    }

    fn text_walk(&mut self, n: NodeId, visible: bool, line: &mut Line) {
        match self.dom.node_type(n) {
            3 => {
                if !visible {
                    return;
                }
                let raw = self.dom.node_value(n).unwrap_or_default();
                let preserve = self.dom.parent(n).map(|p| self.style(p).preserve).unwrap_or(Preserve::No);
                let mut out = String::with_capacity(raw.len());
                match preserve {
                    Preserve::Spaces => out.push_str(&raw),
                    Preserve::Breaks | Preserve::No => {
                        let keep_breaks = preserve == Preserve::Breaks;
                        let mut space = line.at_start;
                        for ch in raw.chars() {
                            if keep_breaks && ch == '\n' {
                                while out.ends_with(' ') {
                                    out.pop();
                                }
                                out.push('\n');
                                space = true;
                            } else if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                                if !space {
                                    out.push(' ');
                                }
                                space = true;
                            } else {
                                out.push(ch);
                                space = false;
                            }
                        }
                    }
                }
                if !out.is_empty() {
                    line.at_start = out.ends_with(' ') || out.ends_with('\n');
                    line.trailing = out.ends_with(' ').then_some(n);
                }
                self.text.insert(n, out);
            }
            1 => {
                if !self.renders(n) {
                    return;
                }
                let st = self.style(n);
                let visible = st.visible;
                if self.tag(n) == "br" {
                    self.end_line(line);
                    return;
                }
                let block = self.block_level(n);
                let atomic = !block && self.atomic_inline(n);
                if block {
                    self.end_line(line);
                }
                if atomic {
                    // its own line inside; to the line it sits on, one piece of content
                    let mut inner = Line { at_start: true, trailing: None };
                    if !st.contents_hidden {
                        for c in self.flat_children(n) {
                            if self.contents_render(n, c) {
                                self.text_walk(c, visible, &mut inner);
                            }
                        }
                    }
                    self.end_line(&mut inner);
                    line.at_start = false;
                    line.trailing = None;
                    return;
                }
                if !st.contents_hidden {
                    for c in self.flat_children(n) {
                        if self.contents_render(n, c) {
                            self.text_walk(c, visible, line);
                        }
                    }
                }
                if block {
                    self.end_line(line);
                }
            }
            _ => {}
        }
    }

    // ── Roles ──────────────────────────────────────────────────────────────────────────────

    /// The ARIA role `el`'s `role` attribute names: the first token Chrome knows. `Some(None)` for `none` /
    /// `presentation` (out of the tree), `None` for no attribute or no token it knows.
    fn explicit_role(&self, el: NodeId) -> Option<Option<Role>> {
        let attr = self.attr(el, "role")?;
        for token in attr.split_ascii_whitespace() {
            let token = token.to_ascii_lowercase();
            if token == "none" || token == "presentation" {
                return Some(None);
            }
            if let Some((_, r)) = ARIA_ROLES.iter().find(|(t, _)| *t == token) {
                return Some(Some(*r));
            }
        }
        None
    }

    fn has_attr(&self, el: NodeId, k: &str) -> bool {
        self.attr(el, k).is_some()
    }

    fn has_aria_name(&self, el: NodeId) -> bool {
        self.attr(el, "aria-label").is_some_and(|v| !v.trim().is_empty()) || self.attr(el, "aria-labelledby").is_some_and(|v| !v.trim().is_empty()) || self.attr(el, "title").is_some_and(|v| !v.trim().is_empty())
    }

    /// Has `el` an ancestor (light tree) with one of `tags`?
    fn inside(&self, el: NodeId, tags: &[&str]) -> bool {
        let mut cur = self.dom.parent(el);
        while let Some(p) = cur {
            if tags.contains(&self.tag(p)) {
                return true;
            }
            cur = self.dom.parent(p);
        }
        false
    }

    /// The role of `el`'s parent in the tree, for the ARIA roles that need one: the nearest ancestor that is a node.
    fn parent_role(&self, el: NodeId) -> Option<Role> {
        let mut cur = self.dom.parent(el);
        while let Some(p) = cur {
            if self.dom.is_element(p) {
                if let Keep::Node(r) = self.native_or_explicit(p) {
                    if r != GENERIC {
                        return Some(r);
                    }
                }
            }
            cur = self.dom.parent(p);
        }
        None
    }

    fn native_or_explicit(&self, el: NodeId) -> Keep {
        match self.explicit_role(el) {
            Some(None) => return Keep::Drop,
            Some(Some(r)) => return Keep::Node(r),
            None => {}
        }
        match self.native_role(el) {
            Some(r) => Keep::Node(r),
            None => Keep::Drop,
        }
    }

    /// The role the host language gives `el` (HTML-AAM, as Chrome 150 maps it), `None` for an element with none of
    /// its own (a `<span>`, a `<b>`, an `<a>` with no `href`: a `generic` Chrome lists only when it must).
    fn native_role(&self, el: NodeId) -> Option<Role> {
        let dom = self.dom;
        if !dom.is_html_ns(el) {
            return match dom.tag_name(el) {
                Some(t) if t.eq_ignore_ascii_case("svg") => Some(aria("image", 99)),
                Some("math") => Some(aria("math", 121)),
                _ => None,
            };
        }
        let in_section = |el| self.inside(el, &["article", "aside", "main", "nav", "section"]);
        Some(match self.tag(el) {
            "a" | "area" if self.has_attr(el, "href") => aria("link", 110),
            "a" if self.clickable.contains(&el) => aria("link", 110),
            "abbr" => internal("Abbr", 1),
            "address" | "hgroup" | "fieldset" | "optgroup" => aria("group", 93),
            "article" => aria("article", 5),
            "aside" => aria("complementary", 22),
            "blockquote" => aria("blockquote", 8),
            "button" if self.has_attr(el, "aria-pressed") => aria("button", 175),
            "button" if self.attr(el, "aria-haspopup").is_some_and(|v| v != "false") => aria("button", 137),
            "button" => aria("button", 9),
            "canvas" => internal("Canvas", 10),
            "caption" => aria("caption", 11),
            "code" => aria("code", 16),
            "del" | "s" => aria("deletion", 24),
            "details" => aria("group", 34),
            "dfn" | "dt" => aria("term", 169),
            "dd" => aria("definition", 29),
            "dialog" => aria("dialog", 35),
            "div" | "pre" | "center" | "marquee" => GENERIC,
            "dl" => internal("DescriptionList", 30),
            "em" => aria("emphasis", 81),
            "figcaption" => internal("Figcaption", 83),
            "figure" => aria("figure", 84),
            "footer" if in_section(el) => aria("sectionfooter", 86),
            "footer" => aria("contentinfo", 85),
            "header" if in_section(el) => aria("sectionheader", 95),
            "header" => aria("banner", 94),
            "form" => aria("form", 87),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => aria("heading", 96),
            "hr" => aria("separator", 157),
            "br" => LINE_BREAK,
            "iframe" => internal("Iframe", 97),
            "img" if self.attr(el, "alt") == Some("") && !self.has_aria_name(el) => return None,
            "img" => aria("image", 99),
            "input" => return self.input_role(el),
            "ins" => aria("insertion", 25),
            "label" => internal("LabelText", 104),
            "legend" => internal("Legend", 108),
            "li" => aria("listitem", 115),
            "main" => aria("main", 118),
            "mark" => aria("mark", 119),
            "menu" | "ol" | "ul" => aria("list", 111),
            "meter" => aria("meter", 129),
            "nav" => aria("navigation", 130),
            "object" => internal("PluginObject", 136),
            "option" if self.inside(el, &["select"]) && !self.listbox_select(el) => aria("option", 127),
            "option" => aria("option", 113),
            "output" => aria("status", 159),
            "p" => aria("paragraph", 133),
            "progress" => aria("progressbar", 140),
            "ruby" => internal("Ruby", 148),
            "search" => aria("search", 152),
            "section" if self.has_aria_name(el) => aria("region", 143),
            "section" => aria("generic", 211),
            "select" if self.is_listbox(el) => aria("listbox", 112),
            "select" => aria("combobox", 209),
            "strong" => aria("strong", 160),
            "sub" => aria("subscript", 185),
            "summary" if dom.parent(el).is_some_and(|p| self.tag(p) == "details") => internal("DisclosureTriangle", 37),
            "sup" => aria("superscript", 186),
            "table" => aria("table", 167),
            "thead" | "tfoot" => aria("rowgroup", 146),
            "tr" => aria("row", 145),
            "th" if self.attr(el, "scope").is_some_and(|s| s.eq_ignore_ascii_case("row")) => aria("rowheader", 147),
            "th" => aria("columnheader", 19),
            "td" => aria("cell", 13),
            "textarea" => aria("textbox", 170),
            "time" => aria("time", 172),
            "video" => internal("Video", 182),
            "audio" => internal("Audio", 6),
            _ => return None,
        })
    }

    fn input_role(&self, el: NodeId) -> Option<Role> {
        Some(match self.dom.input_type(el) {
            "hidden" => return None,
            "button" | "submit" | "reset" | "image" | "file" => aria("button", 9),
            "checkbox" => aria("checkbox", 14),
            "radio" => aria("radio", 141),
            "color" => internal("ColorWell", 17),
            "date" => internal("Date", 27),
            "datetime-local" | "month" | "week" => internal("DateTime", 28),
            "time" => internal("InputTime", 102),
            "number" => aria("spinbutton", 156),
            "range" => aria("slider", 155),
            "search" => aria("searchbox", 153),
            _ => aria("textbox", 170),
        })
    }

    fn is_listbox(&self, select: NodeId) -> bool {
        self.has_attr(select, "multiple") || self.attr(select, "size").and_then(|s| s.trim().parse::<u32>().ok()).is_some_and(|s| s > 1)
    }

    fn listbox_select(&self, option: NodeId) -> bool {
        let mut cur = self.dom.parent(option);
        while let Some(p) = cur {
            if self.tag(p) == "select" {
                return self.is_listbox(p);
            }
            cur = self.dom.parent(p);
        }
        false
    }

    /// Does a `generic` element carry something that makes Chrome keep it: an id (a relation may point at it), a
    /// `title`, a `tabindex`, an editing host, an ARIA attribute?
    fn generic_kept(&self, el: NodeId) -> bool {
        self.dom.attribute_pairs(el).iter().any(|(k, v)| {
            matches!(k.as_str(), "id" | "title" | "tabindex" | "contenteditable") || (k.starts_with("aria-") && !(k == "aria-hidden" && v == "false"))
        })
    }

    fn has_rendered_children(&self, el: NodeId) -> bool {
        self.flat_children(el).into_iter().any(|c| match self.dom.node_type(c) {
            1 => self.renders(c) && self.contents_render(el, c),
            3 => self.text.get(&c).is_some_and(|t| !t.is_empty()),
            _ => false,
        })
    }

    /// How element `el` enters the tree.
    fn keep(&self, el: NodeId, walk: &Walk) -> Keep {
        let tag = self.tag(el);
        if tag == "html" || tag == "body" {
            return Keep::Ignored("uninteresting", AxValue::Boolean(true));
        }
        if self.dom.is_html_slot_element(el) && self.dom.is_shadow_root(self.dom.root_of(el)) {
            return Keep::Ignored("uninteresting", AxValue::Boolean(true));
        }
        if tag == "label" {
            if let Some(c) = self.check_label_control(el) {
                return Keep::Ignored("labelFor", AxValue::Idref(vec![Related { node: c, idref: None, text: None }]));
            }
        }
        if tag == "tbody" && self.explicit_role(el).is_none() {
            return Keep::Ignored("uninteresting", AxValue::Boolean(true));
        }
        if matches!(tag, "wbr" | "datalist" | "rt" | "rp" | "area" | "map" | "track" | "source" | "param") {
            return Keep::Drop;
        }
        let role = match self.explicit_role(el) {
            Some(None) if !crate::core::events::focus::focusable(self.dom, el) => return Keep::Drop,
            Some(Some(r)) => Some(self.contextual(el, r)),
            _ => self.native_role(el),
        };
        // a `<p>` with no box (`display: contents`) is only a paragraph with what keeps a generic (Chrome 152: an `id`
        // keeps it, else its text stands in its place); every other role keeps its element without a box
        let role = role.filter(|r| !(r.name == "paragraph" && tag == "p" && self.style(el).display == Display::Contents && !self.generic_kept(el)));
        let keep = match role {
            Some(r) if r != GENERIC => Keep::Node(r),
            _ => {
                // a `<marquee>`'s text scrolls in a box of its own, which Chrome always lists; a clickable element is
                // one Chrome lists (Blink's `IsClickable`: `<span onclick>`, even an empty `<div>` with a listener)
                if self.generic_kept(el) || tag == "marquee" || self.clickable.contains(&el) {
                    Keep::Node(role.unwrap_or(GENERIC))
                } else if self.block_level(el) {
                    if self.has_rendered_children(el) {
                        Keep::Node(GENERIC)
                    } else {
                        Keep::Ignored("uninteresting", AxValue::Boolean(true))
                    }
                } else if walk.slotted {
                    Keep::Ignored("uninteresting", AxValue::Boolean(true))
                } else {
                    Keep::Drop
                }
            }
        };
        if let Some(h) = walk.hidden_by {
            // a link only for its click listener is none under `aria-hidden` (Chrome 152 lists nothing for it; an
            // `<a href>` there is an ignored node)
            let clicked_link = tag == "a" && !self.has_attr(el, "href") && self.explicit_role(el).is_none();
            return match keep {
                Keep::Node(r) if r != GENERIC && !clicked_link => Keep::Ignored("ariaHiddenSubtree", AxValue::Idref(vec![Related { node: h, idref: None, text: None }])),
                _ => Keep::Drop,
            };
        }
        keep
    }

    /// An ARIA role checked against what it needs: `listitem`, `option` and `treeitem` only inside the container they
    /// belong to, `form` and `region` only with a name (Chrome 150 maps each of them alone to `generic`).
    fn contextual(&self, el: NodeId, r: Role) -> Role {
        match r.name {
            "listitem" => self.parent_role(el).filter(|p| matches!(p.name, "list" | "directory")).map(|_| r).unwrap_or(GENERIC),
            "option" => self.parent_role(el).filter(|p| matches!(p.name, "listbox" | "group" | "combobox")).map(|_| r).unwrap_or(GENERIC),
            "treeitem" => self.parent_role(el).filter(|p| matches!(p.name, "tree" | "group")).map(|_| r).unwrap_or(GENERIC),
            "form" | "region" if !self.has_aria_name(el) => GENERIC,
            _ => r,
        }
    }

    // ── The tree ───────────────────────────────────────────────────────────────────────────

    fn push(&mut self, node: AxNode) -> usize {
        let i = self.nodes.len();
        if let Some(p) = node.parent {
            self.nodes[p].children.push(i);
        }
        self.nodes.push(node);
        i
    }

    fn synthetic_id(&mut self) -> i64 {
        self.next_synthetic += 1;
        -1_000_000_000 - self.next_synthetic
    }

    fn box_id(&mut self) -> i64 {
        self.next_box += 1;
        self.next_box
    }

    fn build(&mut self, doc: NodeId, url: &str) {
        let mut props = vec![("focusable", AxValue::BooleanOrUndefined(true))];
        if self.focused.is_none() {
            props.push(("focused", AxValue::BooleanOrUndefined(true)));
        }
        props.push(("url", AxValue::String(url.to_string())));
        let title = self.document_title(doc);
        let root = self.push(AxNode {
            id: doc as i64 + 1,
            dom: Some(doc),
            role: ROOT_WEB_AREA,
            ignored: Vec::new(),
            name: title,
            description: None,
            value: None,
            properties: props,
            parent: None,
            children: Vec::new(),
        });
        let walk = Walk::default();
        for c in self.flat_children(doc) {
            self.walk(c, root, walk, true);
        }
    }

    fn document_title(&self, doc: NodeId) -> String {
        let Some(t) = self.dom.get_elements_by_tag_name_in(doc, "title").into_iter().find(|&t| self.dom.is_html_ns(t)) else {
            return String::new();
        };
        collapse(&self.dom.text_content(t)).trim().to_string()
    }

    fn ignored_node(&mut self, dom: Option<NodeId>, parent: usize, reason: &'static str, value: AxValue) -> usize {
        let id = match dom {
            Some(d) => d as i64 + 1,
            None => self.synthetic_id(),
        };
        self.push(AxNode {
            id,
            dom,
            role: NONE,
            ignored: vec![(reason, value)],
            name: String::new(),
            description: None,
            value: None,
            properties: Vec::new(),
            parent: Some(parent),
            children: Vec::new(),
        })
    }

    fn text_node(&mut self, dom: Option<NodeId>, parent: usize, role: Role, text: String) -> usize {
        let id = match dom {
            Some(d) => d as i64 + 1,
            None => self.box_id(),
        };
        let i = self.push(AxNode {
            id,
            dom,
            role,
            ignored: Vec::new(),
            name: text.clone(),
            description: None,
            value: None,
            properties: Vec::new(),
            parent: Some(parent),
            children: Vec::new(),
        });
        let box_id = self.synthetic_id();
        self.push(AxNode {
            id: box_id,
            dom: None,
            role: INLINE_TEXT_BOX,
            ignored: Vec::new(),
            name: text,
            description: None,
            value: None,
            properties: Vec::new(),
            parent: Some(i),
            children: Vec::new(),
        });
        i
    }

    /// Node `n` (a flat-tree child of the node at `parent`) and what is under it, `visible` when its parent element's
    /// `visibility` lets it show.
    fn walk(&mut self, n: NodeId, parent: usize, walk: Walk, visible: bool) {
        match self.dom.node_type(n) {
            3 => {
                let Some(text) = self.text.get(&n).cloned() else { return };
                if text.is_empty() || !visible {
                    return;
                }
                if text.trim().is_empty() && self.beside_control(n) {
                    return;
                }
                if walk.in_check_label {
                    self.ignored_node(Some(n), parent, "presentationalRole", AxValue::Boolean(true));
                } else if let Some(h) = walk.hidden_by {
                    self.ignored_node(Some(n), parent, "ariaHiddenSubtree", AxValue::Idref(vec![Related { node: h, idref: None, text: None }]));
                } else {
                    let i = self.text_node(Some(n), parent, STATIC_TEXT, text);
                    if self.dom.parent(n).is_some_and(|p| crate::core::events::focus::contenteditable(self.dom, p)) {
                        self.nodes[i].properties.push(("editable", AxValue::Token("richtext".to_string())));
                    }
                }
            }
            1 => self.walk_element(n, parent, walk),
            _ => {}
        }
    }

    /// Can whitespace-only text `n` go? Blink's `CanIgnoreSpaceNextTo`, on either side of it: Chrome 150 lists the
    /// space between two images, and between a link and a label, as a `StaticText`, and never one beside a block, a
    /// form control, or an inline element that starts (or ends, before the space) with one -- a `<label>` that wraps a
    /// checkbox.
    fn beside_control(&self, n: NodeId) -> bool {
        self.space_goes_beside(self.rendered_sibling(n, false), true, 0) || self.space_goes_beside(self.rendered_sibling(n, true), false, 0)
    }

    /// The nearest sibling of `n` that is laid out (an element that renders, or text with something to show).
    fn rendered_sibling(&self, n: NodeId, next: bool) -> Option<NodeId> {
        let step = |s: NodeId| if next { self.dom.next_sibling(s) } else { self.dom.previous_sibling(s) };
        let mut cur = step(n);
        while let Some(s) = cur {
            let laid_out = match self.dom.node_type(s) {
                1 => self.renders(s),
                3 => self.text.contains_key(&s),
                _ => false,
            };
            if laid_out {
                return Some(s);
            }
            cur = step(s);
        }
        None
    }

    /// Does a space need `neighbour` to be kept apart from (`before`: the neighbour comes before the space)?
    fn space_goes_beside(&self, neighbour: Option<NodeId>, before: bool, depth: u32) -> bool {
        let Some(nb) = neighbour else { return true };
        if depth > 3 {
            return false;
        }
        match self.dom.node_type(nb) {
            3 => {
                let t = self.text.get(&nb).map(String::as_str).unwrap_or("");
                t.trim().is_empty() || if before { t.ends_with(' ') } else { t.starts_with(' ') }
            }
            1 => {
                if self.block_level(nb) || self.form_control(nb) {
                    return true;
                }
                // an image keeps the space beside it (Blink's `LayoutImage`, media elements among them); any other
                // box that sits on the line whole -- a canvas, a frame, an SVG, a progress bar, an inline-block --
                // does not need it
                if matches!(self.tag(nb), "img" | "video" | "audio") {
                    return false;
                }
                if self.atomic_inline(nb) {
                    return true;
                }
                let kids = self.flat_children(nb);
                let edge = if before { kids.last() } else { kids.first() };
                match edge {
                    Some(&c) => self.space_goes_beside(Some(c), before, depth + 1),
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn walk_element(&mut self, el: NodeId, parent: usize, mut walk: Walk) {
        // a ruby annotation is its base's description (Chrome 150: `Ruby` with `description` "kan"), not a node
        if !self.renders(el) || matches!(self.tag(el), "rt" | "rp") {
            return;
        }
        let st = self.style(el);
        if self.attr(el, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) && walk.hidden_by.is_none() {
            walk.hidden_by = Some(el);
        }
        // `display: contents` -- no box of its own -- is not out of the tree: Chrome 152 keeps the element as the node its
        // role makes it, its children under it (a `<form class=contents>`, Tailwind's, is still a `form`; a `<nav>`,
        // `<ul>`, `<button>`, link or heading too); a generic one goes as any generic does. A `<slot>` (`display:
        // contents` by the user agent's sheet) is its assigned nodes in its place, as Chrome 150 lists them.
        if !st.visible || st.display == Display::Contents && self.dom.is_html_slot_element(el) {
            // not shown (`visibility: hidden`): out of the tree, its children in its place -- a `visibility: visible`
            // child of a hidden parent still shows
            for c in self.flat_children(el) {
                if self.contents_render(el, c) {
                    self.walk(c, parent, walk, st.visible);
                }
            }
            return;
        }
        if self.tag(el) == "br" {
            if walk.hidden_by.is_none() {
                self.text_node(Some(el), parent, LINE_BREAK, "\n".to_string());
            }
            return;
        }
        let keep = self.keep(el, &walk);
        let me = match keep {
            Keep::Drop => {
                if !st.contents_hidden {
                    let slot = self.dom.is_html_slot_element(el);
                    for c in self.flat_children(el) {
                        if self.contents_render(el, c) {
                            self.walk(c, parent, Walk { slotted: slot, ..walk }, true);
                        }
                    }
                }
                return;
            }
            Keep::Ignored(reason, value) => {
                if reason == "labelFor" {
                    walk.in_check_label = true;
                }
                self.ignored_node(Some(el), parent, reason, value)
            }
            Keep::Node(role) => self.element_node(el, parent, role),
        };
        let role = self.nodes[me].role;
        // what Chrome reads out of a control's own parts, not out of the DOM under it
        if self.tag(el) == "input" {
            self.input_children(el, me);
            return;
        }
        if self.tag(el) == "select" {
            self.select_children(el, me, role);
            return;
        }
        if matches!(self.tag(el), "textarea" | "iframe" | "img" | "progress" | "meter" | "video" | "audio" | "canvas" | "option") && role.name != "Canvas" {
            return;
        }
        if role.name == "image" && !self.dom.is_html_ns(el) {
            return;
        }
        if st.display == Display::ListItem && st.marker == Marker::Disclosure && walk.hidden_by.is_none() {
            let m = self.ignored_node(None, me, "presentationalRole", AxValue::Boolean(true));
            self.nodes[m].id = self.box_id();
            self.ignored_node(None, m, "presentationalRole", AxValue::Boolean(true));
        } else if st.display == Display::ListItem && st.marker != Marker::None && walk.hidden_by.is_none() {
            let marker = self.marker_text(el, st.marker);
            let m_id = self.box_id();
            let m = self.push(AxNode {
                id: m_id,
                dom: None,
                role: LIST_MARKER,
                ignored: Vec::new(),
                name: marker,
                description: None,
                value: None,
                properties: Vec::new(),
                parent: Some(me),
                children: Vec::new(),
            });
            self.ignored_node(None, m, "presentationalRole", AxValue::Boolean(true));
        }
        if st.contents_hidden {
            return;
        }
        let slot = self.dom.is_html_slot_element(el);
        for c in self.flat_children(el) {
            if self.contents_render(el, c) {
                self.walk(c, me, Walk { slotted: slot, ..walk }, true);
            }
        }
    }

    /// A button-type `<input>`'s label, as Chrome shows it: a `StaticText` (its user-agent shadow text).
    fn input_children(&mut self, el: NodeId, me: usize) {
        if self.nodes[me].is_ignored() {
            return;
        }
        let label = match self.dom.input_type(el) {
            "submit" => Some(self.attr(el, "value").map(str::to_string).unwrap_or_else(|| self.submit_label.to_string())),
            "reset" => Some(self.attr(el, "value").map(str::to_string).unwrap_or_else(|| self.reset_label.to_string())),
            "button" => self.attr(el, "value").filter(|v| !v.is_empty()).map(str::to_string),
            _ => None,
        };
        if let Some(l) = label.filter(|l| !l.is_empty()) {
            self.text_node(None, me, STATIC_TEXT, l);
        }
    }

    /// A `<select>`'s options: under a `MenuListPopup` for a drop-down, straight under a list box.
    fn select_children(&mut self, el: NodeId, me: usize, role: Role) {
        let holder = if role.name == "combobox" {
            let id = self.box_id();
            self.push(AxNode {
                id,
                dom: None,
                role: MENU_LIST_POPUP,
                ignored: Vec::new(),
                name: String::new(),
                description: None,
                value: None,
                properties: Vec::new(),
                parent: Some(me),
                children: Vec::new(),
            })
        } else {
            me
        };
        for c in self.dom.children(el) {
            if !self.dom.is_element(c) || !self.renders(c) && self.tag(c) != "option" {
                continue;
            }
            match self.tag(c) {
                "option" => {
                    let r = self.native_role(c).unwrap_or(aria("option", 127));
                    self.element_node(c, holder, r);
                }
                "optgroup" => {
                    let g = self.element_node(c, holder, aria("group", 93));
                    for o in self.dom.children(c) {
                        if self.tag(o) == "option" {
                            let r = self.native_role(o).unwrap_or(aria("option", 127));
                            self.element_node(o, g, r);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn marker_text(&self, li: NodeId, marker: Marker) -> String {
        let ordinal = || -> i64 {
            let parent = self.dom.parent(li);
            let start = parent.filter(|&p| self.tag(p) == "ol").and_then(|p| self.attr(p, "start")).and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(1);
            let reversed = parent.is_some_and(|p| self.has_attr(p, "reversed"));
            let mut n = start;
            for sib in parent.map(|p| self.dom.children(p)).unwrap_or_default() {
                if sib == li {
                    break;
                }
                if self.dom.is_element(sib) && self.style(sib).display == Display::ListItem {
                    n += if reversed { -1 } else { 1 };
                }
            }
            self.attr(li, "value").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(n)
        };
        match marker {
            Marker::None | Marker::Disclosure => String::new(),
            Marker::Disc => "\u{2022} ".to_string(),
            Marker::Circle => "\u{25e6} ".to_string(),
            Marker::Square => "\u{25aa} ".to_string(),
            Marker::Decimal => format!("{}. ", ordinal()),
            Marker::LowerAlpha => format!("{}. ", alphabetic(ordinal(), b'a')),
            Marker::UpperAlpha => format!("{}. ", alphabetic(ordinal(), b'A')),
            Marker::LowerRoman => format!("{}. ", roman(ordinal()).to_ascii_lowercase()),
            Marker::UpperRoman => format!("{}. ", roman(ordinal())),
        }
    }

    // ── A node of its role ─────────────────────────────────────────────────────────────────

    fn element_node(&mut self, el: NodeId, parent: usize, role: Role) -> usize {
        let (name, labelled) = self.name_of(el, role);
        let description = self.description_of(el, &labelled);
        let value = self.value_of(el, role);
        let properties = self.properties_of(el, role, labelled);
        self.push(AxNode {
            id: el as i64 + 1,
            dom: Some(el),
            role,
            ignored: Vec::new(),
            name,
            description,
            value,
            properties,
            parent: Some(parent),
            children: Vec::new(),
        })
    }

    fn idrefs(&self, el: NodeId, attr: &str) -> Vec<(String, NodeId)> {
        let Some(v) = self.attr(el, attr) else { return Vec::new() };
        let root = self.dom.root_of(el);
        v.split_ascii_whitespace().filter_map(|id| self.dom.get_element_by_id_in(root, id).map(|n| (id.to_string(), n))).collect()
    }

    /// `el`'s accessible name and where it came from (accname 1.2 §4.3, as Chrome 150 computes it): its
    /// `aria-labelledby`, its `aria-label`, the host language's label, its content, its `title`.
    fn name_of(&self, el: NodeId, role: Role) -> (String, Name) {
        let refs = self.idrefs(el, "aria-labelledby");
        if !refs.is_empty() {
            let mut visited = HashSet::from([el]);
            let parts: Vec<(String, NodeId, String)> = refs.into_iter().map(|(id, n)| (id, n, self.alternative(n, &mut visited, true, false))).collect();
            let joined = parts.iter().map(|p| collapse(&p.2).trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
            if !joined.is_empty() {
                let related = parts.into_iter().map(|(id, n, t)| Related { node: n, idref: Some(id), text: Some(collapse(&t).trim().to_string()) }).collect();
                return (joined, Name::Labelledby(related));
            }
        }
        if let Some(l) = self.attr(el, "aria-label").filter(|l| !l.trim().is_empty()) {
            return (collapse(l), Name::Other);
        }
        if let Some((n, labels)) = self.native_name(el, role) {
            if !n.is_empty() {
                return (n, labels);
            }
        }
        if name_from_content(role.name) {
            let mut visited = HashSet::from([el]);
            let n = collapse(&self.content_text(el, &mut visited, false, false)).trim().to_string();
            if !n.is_empty() {
                return (n, Name::Other);
            }
        }
        // a `generic` is not named by its title, it is described by it (Chrome 150: `<span title=t>` is `generic ''`,
        // `description` "t")
        if let Some(t) = self.attr(el, "title").filter(|t| !t.trim().is_empty()).filter(|_| role.name != "generic") {
            return (collapse(t), Name::Title);
        }
        if matches!(self.tag(el), "video" | "audio") && self.media_unplayable(el) {
            return ("Unable to play media.".to_string(), Name::Other);
        }
        if self.tag(el) == "input" && self.dom.input_type(el) == "file" {
            return (self.file_labels.0.to_string(), Name::Other);
        }
        (String::new(), Name::Other)
    }

    /// The host language's own label for `el` (HTML-AAM §4.1): a control's `<label>`s, an image's `alt`, a fieldset's
    /// legend, a table's caption, a figure's caption, an SVG's `<title>`, an input's placeholder.
    fn native_name(&self, el: NodeId, role: Role) -> Option<(String, Name)> {
        let dom = self.dom;
        if !dom.is_html_ns(el) {
            if dom.tag_name(el).is_some_and(|t| t.eq_ignore_ascii_case("svg")) {
                let t = dom.children(el).into_iter().find(|&c| dom.tag_name(c) == Some("title"))?;
                return Some((collapse(&dom.text_content(t)).trim().to_string(), Name::Other));
            }
            return None;
        }
        let tag = self.tag(el);
        if self.labelable(el) {
            if let Some(labels) = self.labels.get(&el).filter(|l| !l.is_empty()) {
                // A label's text as its line shows it, not trimmed: a space at a line's start or end goes, one beside the
                // control stays. Chrome 152: `<label>Search <input></label>` names the textbox `"Search "`, a checkbox
                // after which the text runs `" Gift wrap"`, `<label for=j>  Spaced   label  </label><input id=j>`
                // `"Spaced label "`, a label that ends its block `"Two lines"` -- every labelable control alike. A
                // block in it is a space between its neighbours and none at the label's edges (`<input><div>B</div>`:
                // `"B"`; `A<div>B</div>C `: `"A B C "`), a `<br>` a space wherever it is (`<br>B `: `" B "`).
                let mut related = Vec::new();
                let mut parts = Vec::new();
                for &l in labels {
                    let mut visited = HashSet::from([el]);
                    let t = self.content_text(l, &mut visited, true, true);
                    let t = collapse(&t.trim_matches(BLOCK_GAP).replace(BLOCK_GAP, " "));
                    related.push(Related { node: l, idref: None, text: Some(t.trim().to_string()) });
                    if !t.trim().is_empty() {
                        parts.push(t);
                    }
                }
                let joined = collapse(&parts.join(" "));
                if !joined.trim().is_empty() {
                    return Some((joined, Name::Labels(related)));
                }
            }
        }
        let text = |s: &str| (collapse(s).trim().to_string(), Name::Other);
        match tag {
            "input" => match dom.input_type(el) {
                "submit" => Some(text(self.attr(el, "value").unwrap_or(self.submit_label))),
                "reset" => Some(text(self.attr(el, "value").unwrap_or(self.reset_label))),
                "button" => self.attr(el, "value").map(text),
                "image" => Some(text(self.attr(el, "alt").or(self.attr(el, "value")).filter(|v| !v.trim().is_empty()).unwrap_or(self.submit_label))),
                _ if role.name == "textbox" || role.name == "searchbox" || role.name == "spinbutton" => {
                    self.attr(el, "placeholder").filter(|p| !p.trim().is_empty()).filter(|_| self.attr(el, "title").is_none()).map(text)
                }
                _ => None,
            },
            "textarea" => self.attr(el, "placeholder").filter(|p| !p.trim().is_empty()).filter(|_| self.attr(el, "title").is_none()).map(text),
            "img" => self.attr(el, "alt").map(text),
            "fieldset" => {
                let legend = dom.children(el).into_iter().find(|&c| self.tag(c) == "legend")?;
                let mut visited = HashSet::from([el]);
                Some((collapse(&self.content_text(legend, &mut visited, false, false)).trim().to_string(), Name::Labels(vec![Related { node: legend, idref: None, text: None }])))
            }
            "table" => {
                let cap = dom.children(el).into_iter().find(|&c| self.tag(c) == "caption")?;
                let mut visited = HashSet::from([el]);
                Some((collapse(&self.content_text(cap, &mut visited, false, false)).trim().to_string(), Name::Labels(vec![Related { node: cap, idref: None, text: None }])))
            }
            "figure" => {
                let cap = dom.children(el).into_iter().find(|&c| self.tag(c) == "figcaption")?;
                let mut visited = HashSet::from([el]);
                Some((collapse(&self.content_text(cap, &mut visited, false, false)).trim().to_string(), Name::Other))
            }
            "optgroup" => self.attr(el, "label").map(text),
            "option" => self.attr(el, "label").filter(|l| !l.trim().is_empty()).map(text).or_else(|| Some(text(&dom.text_content(el)))),
            _ => None,
        }
    }

    /// The text alternative of a node reached while naming another (accname §4.3.2, steps 2B-2I): `labelledby` when
    /// the walk came through `aria-labelledby` -- a hidden node then counts, as Chrome 150 names an input after a
    /// `display: none` span.
    fn alternative(&self, n: NodeId, visited: &mut HashSet<NodeId>, via_labelledby: bool, laid_out: bool) -> String {
        let dom = self.dom;
        if dom.node_type(n) == 3 {
            return dom.node_value(n).unwrap_or_default();
        }
        if !dom.is_element(n) || !visited.insert(n) {
            return String::new();
        }
        if !via_labelledby && (!self.renders(n) || self.attr(n, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))) {
            return String::new();
        }
        if !via_labelledby {
            let refs = self.idrefs(n, "aria-labelledby");
            let joined = refs.into_iter().map(|(_, r)| collapse(&self.alternative(r, visited, true, false)).trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
            if !joined.is_empty() {
                return joined;
            }
        }
        let role = match self.native_or_explicit(n) {
            Keep::Node(r) => r,
            _ => GENERIC,
        };
        // an embedded control contributes its value, not its label (accname 2E) -- the node being named is never here
        {
            match role.name {
                "textbox" | "searchbox" => {
                    return if self.tag(n) == "input" || self.tag(n) == "textarea" { dom.control_value(n) } else { dom.text_content(n) };
                }
                "combobox" | "listbox" if self.tag(n) == "select" => {
                    return dom.selected_options(n).first().map(|&o| self.option_label(o)).unwrap_or_default();
                }
                "slider" | "spinbutton" | "scrollbar" | "progressbar" | "meter" => {
                    if let Some(v) = self.attr(n, "aria-valuetext").or(self.attr(n, "aria-valuenow")) {
                        return v.to_string();
                    }
                    if self.tag(n) == "input" {
                        return dom.control_value(n);
                    }
                }
                _ => {}
            }
        }
        if let Some(l) = self.attr(n, "aria-label").filter(|l| !l.trim().is_empty()) {
            return l.to_string();
        }
        if let Some((t, _)) = self.native_name(n, role) {
            if !t.is_empty() {
                return t;
            }
        }
        let content = self.content_text(n, visited, via_labelledby, laid_out);
        if !content.trim_matches(|c: char| c.is_whitespace() || c == BLOCK_GAP).is_empty() {
            return content;
        }
        self.attr(n, "title").map(str::to_string).unwrap_or_default()
    }

    /// `el`'s content as text (accname 2F): each child's text alternative in the flat tree, a block-level child set
    /// apart by spaces. `include_hidden` when the walk came through `aria-labelledby`. `laid_out`, for a label's text:
    /// the text the tree holds -- a rendered text node as its line collapses it ([`Builder::text`]), none for a space
    /// the tree leaves out beside a control, a `<br>` as the space its line break collapses to -- and a block set apart
    /// by [`BLOCK_GAP`]s, which the label trims at its edges; the DOM's text otherwise.
    fn content_text(&self, el: NodeId, visited: &mut HashSet<NodeId>, include_hidden: bool, laid_out: bool) -> String {
        let mut out = String::new();
        for c in self.flat_children(el) {
            match self.dom.node_type(c) {
                3 => match self.text.get(&c).filter(|_| laid_out) {
                    Some(t) if t.trim().is_empty() && self.beside_control(c) => {}
                    Some(t) => out.push_str(t),
                    None if include_hidden || self.text.contains_key(&c) => out.push_str(&self.dom.node_value(c).unwrap_or_default()),
                    None => {}
                },
                1 => {
                    if !include_hidden && (!self.renders(c) || !self.style(c).visible && self.style(c).display != Display::Contents) {
                        if self.renders(c) {
                            // a hidden parent's visible child still shows
                            out.push_str(&self.content_text(c, visited, include_hidden, laid_out));
                        }
                        continue;
                    }
                    if visited.contains(&c) {
                        continue;
                    }
                    if laid_out && self.tag(c) == "br" {
                        out.push(' ');
                        continue;
                    }
                    let block = self.block_level(c);
                    let t = self.alternative(c, visited, include_hidden, laid_out);
                    if block {
                        let gap = if laid_out { BLOCK_GAP } else { ' ' };
                        out.push(gap);
                        out.push_str(&t);
                        out.push(gap);
                    } else {
                        out.push_str(&t);
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn option_label(&self, o: NodeId) -> String {
        self.attr(o, "label").filter(|l| !l.trim().is_empty()).map(|l| collapse(l).trim().to_string()).unwrap_or_else(|| collapse(&self.dom.text_content(o)).trim().to_string())
    }

    /// A media element with nothing to play -- no `src`, no `<source>` -- whose controls Chrome shows as an error.
    fn media_unplayable(&self, el: NodeId) -> bool {
        !self.has_attr(el, "src") && !self.dom.children(el).into_iter().any(|c| self.tag(c) == "source")
    }

    fn description_of(&self, el: NodeId, name: &Name) -> Option<String> {
        if self.tag(el) == "ruby" {
            let rt: String = self.dom.children(el).into_iter().filter(|&c| self.tag(c) == "rt").map(|c| self.dom.text_content(c)).collect();
            let rt = collapse(&rt).trim().to_string();
            if !rt.is_empty() {
                return Some(rt);
            }
        }
        // a `<summary>` outside a `<details>` is a `generic`, which its content describes
        if self.tag(el) == "summary" && !self.dom.parent(el).is_some_and(|p| self.tag(p) == "details") {
            let mut visited = HashSet::from([el]);
            let t = collapse(&self.content_text(el, &mut visited, false, false)).trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
        let refs = self.idrefs(el, "aria-describedby");
        if !refs.is_empty() {
            let mut visited = HashSet::from([el]);
            let d = refs.into_iter().map(|(_, n)| collapse(&self.alternative(n, &mut visited, true, false)).trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
            if !d.is_empty() {
                return Some(d);
            }
        }
        if let Some(d) = self.attr(el, "aria-description").filter(|d| !d.trim().is_empty()) {
            return Some(collapse(d));
        }
        if *name != Name::Title {
            if let Some(t) = self.attr(el, "title").filter(|t| !t.trim().is_empty()) {
                return Some(collapse(t));
            }
        }
        None
    }

    // ── Values and states ──────────────────────────────────────────────────────────────────

    fn disabled(&self, el: NodeId) -> bool {
        self.dom.actually_disabled(el) || self.attr(el, "aria-disabled").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
    }

    fn number_attr(&self, el: NodeId, k: &str) -> Option<f64> {
        self.attr(el, k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())
    }

    /// A range's or a meter's numbers: its value (none for an indeterminate progress bar), min and max.
    fn range_numbers(&self, el: NodeId, role: Role) -> (Option<f64>, f64, f64) {
        let tag = self.tag(el);
        if tag == "input" && role.name == "slider" {
            let min = self.number_attr(el, "min").unwrap_or(0.0);
            let max = self.number_attr(el, "max").unwrap_or(100.0).max(min);
            let default = min + (max - min) / 2.0;
            let v = self.dom.control_value(el).trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(default).clamp(min, max);
            return (Some(v), min, max);
        }
        if tag == "input" && role.name == "spinbutton" {
            let v = self.dom.control_value(el).trim().parse::<f64>().ok().filter(|v| v.is_finite());
            return (v, self.number_attr(el, "min").unwrap_or(0.0), self.number_attr(el, "max").unwrap_or(0.0));
        }
        if tag == "progress" {
            let max = self.number_attr(el, "max").filter(|m| *m > 0.0).unwrap_or(1.0);
            return (self.number_attr(el, "value").map(|v| v.clamp(0.0, max)), 0.0, max);
        }
        if tag == "meter" {
            let min = self.number_attr(el, "min").unwrap_or(0.0);
            let max = self.number_attr(el, "max").unwrap_or(1.0).max(min);
            return (Some(self.number_attr(el, "value").unwrap_or(0.0).clamp(min, max)), min, max);
        }
        let now = self.number_attr(el, "aria-valuenow");
        let (min, max) = match role.name {
            "spinbutton" => (self.number_attr(el, "aria-valuemin").unwrap_or(0.0), self.number_attr(el, "aria-valuemax").unwrap_or(0.0)),
            _ => (self.number_attr(el, "aria-valuemin").unwrap_or(0.0), self.number_attr(el, "aria-valuemax").unwrap_or(100.0)),
        };
        let v = match role.name {
            "progressbar" => now,
            "slider" | "scrollbar" => Some(now.unwrap_or(min + (max - min) / 2.0)),
            _ => Some(now.unwrap_or(0.0)),
        };
        (v, min, max)
    }

    fn value_of(&self, el: NodeId, role: Role) -> Option<AxValue> {
        let dom = self.dom;
        let tag = self.tag(el);
        match role.name {
            "textbox" | "searchbox" => {
                let v = if tag == "input" || tag == "textarea" { dom.control_value(el) } else { collapse(&dom.text_content(el)).trim().to_string() };
                (!v.is_empty()).then_some(AxValue::String(v))
            }
            "combobox" if tag == "select" => dom.selected_options(el).first().map(|&o| AxValue::String(self.option_label(o))),
            "slider" | "progressbar" | "meter" | "spinbutton" | "scrollbar" => self.range_numbers(el, role).0.map(AxValue::Number),
            "ColorWell" => Some(AxValue::String({
                let v = dom.control_value(el);
                if v.is_empty() { "#000000".to_string() } else { v }
            })),
            "button" if tag == "input" && dom.input_type(el) == "file" => Some(AxValue::String(match dom.file_count(el) {
                0 => self.file_labels.1.to_string(),
                1 => "1 file".to_string(),
                n => format!("{n} files"),
            })),
            "generic" if self.editing_host(el) => {
                let v = collapse(&dom.text_content(el)).trim().to_string();
                (!v.is_empty()).then_some(AxValue::String(v))
            }
            _ => None,
        }
    }

    fn editing_host(&self, el: NodeId) -> bool {
        self.attr(el, "contenteditable").is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "" | "true" | "plaintext-only"))
    }

    fn tristate(v: Option<&str>) -> Option<&'static str> {
        match v.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("true") => Some("true"),
            Some("mixed") => Some("mixed"),
            Some("false") => Some("false"),
            _ => None,
        }
    }

    /// `el`'s properties, in the order Chrome 150 lists them: live-region ones, the global states, the widget
    /// properties, the widget states, its url, then its relations.
    fn properties_of(&self, el: NodeId, role: Role, name: Name) -> Vec<(&'static str, AxValue)> {
        let dom = self.dom;
        let tag = self.tag(el);
        let r = role.name;
        let native_control = matches!(tag, "input" | "select" | "textarea" | "button" | "fieldset" | "output" | "object");
        let text_input = tag == "textarea" || tag == "input" && matches!(r, "textbox" | "searchbox" | "spinbutton");
        let mut p: Vec<(&'static str, AxValue)> = Vec::new();

        let live = self.attr(el, "aria-live").map(|v| v.trim().to_ascii_lowercase()).or(match r {
            "alert" => Some("assertive".to_string()),
            "status" | "log" => Some("polite".to_string()),
            _ => None,
        });
        if let Some(live) = live.filter(|l| l != "off") {
            p.push(("live", AxValue::Token(live)));
            let atomic = self.attr(el, "aria-atomic").map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(matches!(r, "alert" | "status"));
            p.push(("atomic", AxValue::Boolean(atomic)));
            p.push(("relevant", AxValue::TokenList(self.attr(el, "aria-relevant").unwrap_or("additions text").to_string())));
        }

        let disabled = self.disabled(el) || tag == "object" || matches!(tag, "video" | "audio") && self.media_unplayable(el);
        if disabled {
            p.push(("disabled", AxValue::Boolean(true)));
        }
        if native_control || self.has_attr(el, "aria-invalid") {
            let inv = match self.attr(el, "aria-invalid").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("true") => "true",
                Some("grammar") => "grammar",
                Some("spelling") => "spelling",
                _ => "false",
            };
            p.push(("invalid", AxValue::Token(inv.to_string())));
        }
        // HTML's focusable areas as `focus::focusable` reads them, but an iframe (Chrome 150 lists none as focusable)
        // and the inside of an editing host (only the host is); a dialog, an option and a media element with controls
        // are, as Chrome lists them
        let natively = self.has_attr(el, "tabindex") || matches!(tag, "input" | "textarea" | "select" | "button" | "a" | "area") || r == "DisclosureTriangle";
        let focusable = match tag {
            "iframe" | "object" => false,
            // media controls stay focusable with nothing to play (Chrome 150: `disabled`, `focusable`)
            "video" | "audio" => self.has_attr(el, "controls"),
            "dialog" | "option" => !disabled,
            "summary" if r != "DisclosureTriangle" => false,
            _ => !disabled && crate::core::events::focus::focusable(dom, el) && (natively || self.editing_host(el)),
        };
        if focusable {
            p.push(("focusable", AxValue::BooleanOrUndefined(true)));
        }
        if self.focused == Some(el) {
            p.push(("focused", AxValue::BooleanOrUndefined(true)));
        }
        if text_input {
            p.push(("editable", AxValue::Token("plaintext".to_string())));
        } else if self.editing_host(el) {
            p.push(("editable", AxValue::Token("richtext".to_string())));
        }
        let readonly_attr = self.has_attr(el, "readonly") || self.attr(el, "aria-readonly").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        let settable = if tag == "input" || tag == "textarea" {
            !disabled && !readonly_attr && (text_input || matches!(r, "slider" | "ColorWell" | "Date" | "DateTime" | "InputTime"))
        } else {
            matches!(r, "textbox" | "searchbox" | "slider" | "scrollbar" | "separator" | "spinbutton")
        };
        if settable {
            p.push(("settable", AxValue::BooleanOrUndefined(true)));
        }

        // widget properties
        let popup = if tag == "select" && r == "combobox" {
            Some("menu".to_string())
        } else {
            match self.attr(el, "aria-haspopup").map(|v| v.trim().to_ascii_lowercase()) {
                Some(v) if v == "true" => Some("menu".to_string()),
                Some(v) if matches!(v.as_str(), "menu" | "listbox" | "tree" | "grid" | "dialog") => Some(v),
                _ if r == "combobox" => Some("listbox".to_string()),
                _ => None,
            }
        };
        if let Some(v) = popup {
            p.push(("hasPopup", AxValue::Token(v)));
        }
        let level = match r {
            "heading" => Some(self.number_attr(el, "aria-level").map(|l| l as i64).filter(|l| *l >= 1).unwrap_or_else(|| match tag {
                "h1" => 1,
                "h2" => 2,
                "h3" => 3,
                "h4" => 4,
                "h5" => 5,
                "h6" => 6,
                _ => 2,
            })),
            "listitem" => Some(self.list_level(el)),
            "comment" => Some(1),
            "treeitem" => self.number_attr(el, "aria-level").map(|l| l as i64),
            _ => None,
        };
        if let Some(l) = level {
            p.push(("level", AxValue::Integer(l)));
        }
        let multiselectable = match r {
            "listbox" => Some(tag == "select" && self.has_attr(el, "multiple") || self.attr(el, "aria-multiselectable").is_some_and(|v| v.trim() == "true")),
            "tablist" | "grid" | "tree" | "treegrid" => Some(self.attr(el, "aria-multiselectable").is_some_and(|v| v.trim() == "true")),
            _ => None,
        };
        if let Some(m) = multiselectable {
            p.push(("multiselectable", AxValue::Boolean(m)));
        }
        let orientation = self.attr(el, "aria-orientation").map(|v| v.trim().to_ascii_lowercase()).filter(|v| v == "horizontal" || v == "vertical").or(match r {
            "slider" | "tablist" | "menubar" | "toolbar" | "separator" => Some("horizontal".to_string()),
            "menu" | "listbox" | "tree" | "scrollbar" => Some("vertical".to_string()),
            _ => None,
        });
        if let Some(o) = orientation {
            p.push(("orientation", AxValue::Token(o)));
        }
        if r == "textbox" {
            let multiline = tag == "textarea" || tag != "input" && self.attr(el, "aria-multiline").is_some_and(|v| v.trim() == "true");
            p.push(("multiline", AxValue::Boolean(multiline)));
        }
        if matches!(r, "textbox" | "grid" | "treegrid" | "gridcell" | "columnheader" | "rowheader") {
            p.push(("readonly", AxValue::Boolean(readonly_attr)));
        }
        let required_attr = self.has_attr(el, "required") || self.attr(el, "aria-required").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        let reports_required = matches!(r, "textbox" | "spinbutton" | "listbox" | "radiogroup" | "tree" | "treegrid" | "gridcell" | "columnheader" | "rowheader")
            || r == "combobox" && tag != "select";
        if reports_required {
            p.push(("required", AxValue::Boolean(required_attr)));
        }
        if matches!(r, "slider" | "progressbar" | "meter" | "spinbutton" | "scrollbar") {
            let (v, min, max) = self.range_numbers(el, role);
            p.push(("valuemin", AxValue::Number(min)));
            p.push(("valuemax", AxValue::Number(max)));
            let text = self.attr(el, "aria-valuetext").map(str::to_string).unwrap_or_else(|| {
                if tag == "input" && r == "spinbutton" { v.map(fmt_number).unwrap_or_default() } else { String::new() }
            });
            p.push(("valuetext", AxValue::String(text)));
        }

        // widget states
        let checked = match r {
            "checkbox" | "radio" if tag == "input" => Some(if dom.input_type(el) == "checkbox" && dom.indeterminate(el) {
                "mixed"
            } else if dom.checkedness(el) {
                "true"
            } else {
                "false"
            }),
            "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio" => Some(Self::tristate(self.attr(el, "aria-checked")).unwrap_or("false")),
            _ => None,
        };
        if let Some(c) = checked {
            p.push(("checked", AxValue::Tristate(c)));
        }
        let expanded = if tag == "select" && r == "combobox" {
            Some(false)
        } else if r == "DisclosureTriangle" {
            Some(dom.parent(el).is_some_and(|d| self.has_attr(d, "open")))
        } else {
            match self.attr(el, "aria-expanded").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            }
        };
        if let Some(e) = expanded {
            p.push(("expanded", AxValue::BooleanOrUndefined(e)));
        }
        if matches!(r, "dialog" | "alertdialog") {
            let modal = tag == "dialog" && dom.is_modal(el) || self.attr(el, "aria-modal").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
            p.push(("modal", AxValue::Boolean(modal)));
        }
        if let Some(pressed) = Self::tristate(self.attr(el, "aria-pressed")).filter(|_| r == "button") {
            p.push(("pressed", AxValue::Tristate(pressed)));
        }
        let selected = match r {
            "option" if tag == "option" => Some(dom.option_selectedness(el)),
            "option" | "tab" => Some(self.attr(el, "aria-selected").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))),
            "gridcell" | "row" | "treeitem" | "columnheader" | "rowheader" => self.attr(el, "aria-selected").map(|v| v.trim().eq_ignore_ascii_case("true")),
            _ => None,
        };
        if let Some(s) = selected {
            p.push(("selected", AxValue::BooleanOrUndefined(s)));
        }

        let url = match (r, tag) {
            ("link", "a" | "area") => self.attr(el, "href").map(|h| self.resolve(h)),
            ("image", "img") => self.attr(el, "src").map(|s| self.resolve(s)),
            _ => None,
        };
        if let Some(u) = url {
            p.push(("url", AxValue::String(u)));
        }

        // relations
        let described = self.idrefs(el, "aria-describedby");
        if !described.is_empty() {
            let related = described.into_iter().map(|(id, n)| Related { node: n, idref: Some(id), text: None }).collect();
            p.push(("describedby", AxValue::IdrefList(self.attr(el, "aria-describedby").unwrap_or("").to_string(), related)));
        }
        match name {
            Name::Labelledby(related) | Name::Labels(related) => p.push(("labelledby", AxValue::NodeList(related))),
            _ => {}
        }
        p
    }

    fn list_level(&self, li: NodeId) -> i64 {
        let mut level = 0;
        let mut cur = self.dom.parent(li);
        while let Some(p) = cur {
            if matches!(self.tag(p), "ul" | "ol" | "menu") || self.attr(p, "role").is_some_and(|r| r.split_ascii_whitespace().any(|t| t == "list" || t == "directory")) {
                level += 1;
            }
            cur = self.dom.parent(p);
        }
        level.max(1)
    }

    fn resolve(&self, href: &str) -> String {
        match url::Url::parse(&self.base).ok().and_then(|b| b.join(href.trim()).ok()) {
            Some(u) => u.to_string(),
            None => href.to_string(),
        }
    }
}

/// Where a name came from: the relation a client is told about (`labelledby`), and whether `title` was spent on it
/// (then it is no description).
#[derive(Debug, Clone, PartialEq)]
enum Name {
    Labelledby(Vec<Related>),
    Labels(Vec<Related>),
    Title,
    Other,
}

/// The line a text walk is on.
struct Line {
    /// Nothing but collapsible space since the line began: a space here goes.
    at_start: bool,
    /// The text node whose last character is the line's last, a space a line end would take.
    trailing: Option<NodeId>,
}

/// What sets a block apart in a label's text ([`Builder::content_text`]): a space between its neighbours, none at the
/// label's edges.
const BLOCK_GAP: char = '\u{1}';

/// Every run of ASCII white space as one space.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for ch in s.chars() {
        if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(ch);
            space = false;
        }
    }
    out
}

/// A number as JavaScript prints it: `4`, not `4.0`.
fn fmt_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// `n` in `lower-alpha` / `upper-alpha` (CSS Counter Styles: bijective base 26), `start` its first letter.
fn alphabetic(n: i64, start: u8) -> String {
    if n < 1 {
        return n.to_string();
    }
    let mut n = n;
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push((start + (n % 26) as u8) as char);
        n /= 26;
    }
    out.iter().rev().collect()
}

/// `n` in `upper-roman` (1 to 3999; anything else in decimal, as CSS's range fallback).
fn roman(n: i64) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    let table = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut n = n;
    let mut out = String::new();
    for (v, s) in table {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_read_as_css_writes_them() {
        assert_eq!(alphabetic(1, b'a'), "a");
        assert_eq!(alphabetic(3, b'a'), "c");
        assert_eq!(alphabetic(27, b'a'), "aa");
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(fmt_number(4.0), "4");
        assert_eq!(fmt_number(0.5), "0.5");
    }

    #[test]
    fn white_space_collapses_to_one_space() {
        assert_eq!(collapse("  lots   of\n   space  "), " lots of space ");
        assert_eq!(collapse("a\tb"), "a b");
    }

    #[test]
    fn breadth_first_counts_only_unignored_levels() {
        let node = |id: i64, ignored: bool, parent: Option<usize>, children: Vec<usize>| AxNode {
            id,
            dom: None,
            role: if ignored { NONE } else { GENERIC },
            ignored: if ignored { vec![("uninteresting", AxValue::Boolean(true))] } else { Vec::new() },
            name: String::new(),
            description: None,
            value: None,
            properties: Vec::new(),
            parent,
            children,
        };
        // root -> ignored -> a -> b
        let t = AxTree { nodes: vec![node(1, false, None, vec![1]), node(2, true, Some(0), vec![2]), node(3, false, Some(1), vec![3]), node(4, false, Some(2), vec![])] };
        assert_eq!(t.breadth_first(None), vec![0, 1, 2, 3]);
        assert_eq!(t.breadth_first(Some(1)), vec![0, 1, 2], "the ignored node uses no level up");
        assert_eq!(t.ancestors(3), vec![2, 1, 0]);
    }
}
