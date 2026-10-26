//! Processing instructions in HTML: the html5ever sink every HTML parse goes through.
//!
//! html5ever 0.39 tokenizes `<?` the way HTML always did, as a bogus comment running to the first `>`.
//! Chrome makes a `ProcessingInstruction` of it when the target is valid, and a page can see the
//! difference (`nodeType` 7, `target`, `data`, the serialization `<?target data?>`, and the markers of
//! `<template for>` partial updates). Measured (Chrome 150.0.7871.46, `tools/realweb/seed/htmlpi`: every
//! input of WPT's html5lib `processing-instructions.dat`, the `<?` cases of `tests1` / `comments01` /
//! `html5test-com`, and edge cases):
//!
//! - the target is an ASCII letter or `_`, then ASCII letters, digits, `-` and `_`, its case kept
//!   (`<?A B?>` is target `A`; Chrome 152's `kProcessingInstructionTargetState` -- Chrome 150 lowercased it and
//!   took no `_`); it ends at whitespace, `?` or `>`. Any other character in it (`. : ! = " /`, NUL, NBSP, a
//!   non-ASCII letter) leaves the whole construct a bogus comment, as does a target of `xml` or
//!   `xml-stylesheet` (ASCII case-insensitive; `xml-` and `xmlfoo` are PIs).
//! - whitespace after the target is skipped; a `?` there starts the data (`<?a?b?>` is data `?b`).
//! - the PI ends at the FIRST `>`, like the bogus comment, and one `?` right before it is dropped
//!   (`<?a??>` is data `?`). The data is raw: no character references.
//! - a PI still open at end of input makes no node at all, and neither does a bare `<?`; a bogus
//!   comment there stays (`<? `, `<?#`, `<?a.b`, `<?xml version`).
//!
//! The tokenizer is html5ever's, so this sink does not re-tokenize: html5ever reports `<?` as the parse
//! error "Saw ? in state TagOpen" immediately before it starts the bogus comment, and nothing else is
//! created between that error and the comment. The comment's text is the construct's (`?` and all), so
//! the rules above read it. The one thing a sink cannot see is WHY the comment ended; only the last
//! `<?` construct can have run to end of input, and it did exactly when the input ends with `<` and its
//! text.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, ExpandedName, QualName};
use markup5ever_rcdom::{Handle, RcDom};

/// html5ever's message for `<?` in the tag-open state (its non-`exact_errors` wording, which is the
/// default every parse here uses).
const PI_OPEN: &str = "Saw ? in state TagOpen";

/// An [`RcDom`] that turns html5ever's `<?` bogus comments into processing instructions.
pub(crate) struct PiSink<'i> {
    inner: RcDom,
    /// The input, for the end-of-input rule.
    input: &'i str,
    /// html5ever just reported `<?`: the next comment it creates is that construct.
    pi_open: Cell<bool>,
    /// The last `<?` construct, when it is one end of input would erase (a PI, or a bare `<?`): the
    /// node and its comment text.
    last: RefCell<Option<(Handle, String)>>,
    /// A `<template for>` was made: the parse has partial updates to apply (`partial_update`). Every other
    /// parse -- every `innerHTML` -- skips that pass's walk.
    template_for: Cell<bool>,
    /// The tokenizer's line (1-based), as the tree builder last reported it.
    line: Cell<u64>,
    /// Every `<script>` element the parse made, with the line its start tag ended on -- for the script's
    /// source position (`Dom::script_position`).
    scripts: RefCell<Vec<(Handle, u64)>>,
}

impl<'i> PiSink<'i> {
    pub(crate) fn new(input: &'i str) -> Self {
        PiSink {
            inner: RcDom::default(),
            input,
            pi_open: Cell::new(false),
            last: RefCell::new(None),
            template_for: Cell::new(false),
            line: Cell::new(1),
            scripts: RefCell::new(Vec::new()),
        }
    }
}

/// The `(target, data)` Chrome makes of a `<?` construct whose bogus-comment text is `text`, or `None`
/// when it stays a comment.
fn pi_of(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('?')?;
    let b = rest.as_bytes();
    let first = *b.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let end = b.iter().position(|c| !(c.is_ascii_alphanumeric() || *c == b'-' || *c == b'_')).unwrap_or(b.len());
    let target = rest[..end].to_string();
    let after = &rest[end..];
    let data = match after.as_bytes().first() {
        None => after,
        Some(b'\t' | b'\n' | b'\x0c' | b' ') => after.trim_start_matches(['\t', '\n', '\x0c', ' ']),
        Some(b'?') => after,
        Some(_) => return None,
    };
    if target.eq_ignore_ascii_case("xml") || target.eq_ignore_ascii_case("xml-stylesheet") {
        return None;
    }
    Some((target, data.strip_suffix('?').unwrap_or(data).to_string()))
}

/// Whether `input` ends inside the `<?` construct whose text is `text` -- the input after html5ever's
/// preprocessing (CR LF and CR to LF, NUL to U+FFFD) ends with `<` and the text. Only the input's tail
/// is normalized: the text is at least as many characters as its source minus the CRs it dropped.
fn ends_in(input: &str, text: &str) -> bool {
    let n = text.chars().count() * 2 + 2;
    let start = input.char_indices().rev().nth(n).map_or(0, |(i, _)| i);
    let mut tail = String::with_capacity(input.len() - start);
    let mut chars = input[start..].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                tail.push('\n');
            }
            '\0' => tail.push('\u{fffd}'),
            c => tail.push(c),
        }
    }
    tail.strip_suffix(text).is_some_and(|t| t.ends_with('<'))
}

impl<'i> TreeSink for PiSink<'i> {
    type Handle = Handle;
    /// The tree, whether it holds a `<template for>`, and its `<script>` elements with their lines.
    type Output = (RcDom, bool, Vec<(Handle, u64)>);
    type ElemName<'a>
        = ExpandedName<'a>
    where
        Self: 'a;

    fn finish(self) -> (RcDom, bool, Vec<(Handle, u64)>) {
        if let Some((node, text)) = self.last.take() {
            if ends_in(self.input, &text) {
                self.inner.remove_from_parent(&node);
            }
        }
        let scripts = self.scripts.take();
        (self.inner.finish(), self.template_for.get(), scripts)
    }

    fn parse_error(&self, msg: Cow<'static, str>) {
        if msg == PI_OPEN {
            self.pi_open.set(true);
        }
        self.inner.parse_error(msg);
    }

    fn get_document(&self) -> Handle {
        self.inner.get_document()
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> ExpandedName<'a> {
        self.inner.elem_name(target)
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        if flags.template && attrs.iter().any(|a| &*a.name.local == "for") {
            self.template_for.set(true);
        }
        let is_script = &*name.local == "script";
        let h = self.inner.create_element(name, attrs, flags);
        if is_script {
            self.scripts.borrow_mut().push((h.clone(), self.line.get()));
        }
        h
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        if !self.pi_open.replace(false) {
            return self.inner.create_comment(text);
        }
        let (node, erasable) = match pi_of(&text) {
            Some((target, data)) => (self.inner.create_pi(target.into(), data.into()), true),
            None => {
                let bare = &*text == "?";
                (self.inner.create_comment(text.clone()), bare)
            }
        };
        *self.last.borrow_mut() = erasable.then(|| (node.clone(), text.to_string()));
        node
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Handle {
        self.inner.create_pi(target, data)
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        self.inner.append(parent, child)
    }

    fn append_based_on_parent_node(&self, element: &Handle, prev_element: &Handle, child: NodeOrText<Handle>) {
        self.inner.append_based_on_parent_node(element, prev_element, child)
    }

    fn append_doctype_to_document(&self, name: StrTendril, public_id: StrTendril, system_id: StrTendril) {
        self.inner.append_doctype_to_document(name, public_id, system_id)
    }

    fn mark_script_already_started(&self, node: &Handle) {
        self.inner.mark_script_already_started(node)
    }

    fn pop(&self, node: &Handle) {
        self.inner.pop(node)
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        self.inner.get_template_contents(target)
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        self.inner.same_node(x, y)
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.inner.set_quirks_mode(mode)
    }

    fn append_before_sibling(&self, sibling: &Handle, new_node: NodeOrText<Handle>) {
        self.inner.append_before_sibling(sibling, new_node)
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        self.inner.add_attrs_if_missing(target, attrs)
    }

    fn associate_with_form(&self, target: &Handle, form: &Handle, nodes: (&Handle, Option<&Handle>)) {
        self.inner.associate_with_form(target, form, nodes)
    }

    fn remove_from_parent(&self, target: &Handle) {
        self.inner.remove_from_parent(target)
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        self.inner.reparent_children(node, new_parent)
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Handle) -> bool {
        self.inner.is_mathml_annotation_xml_integration_point(handle)
    }

    fn set_current_line(&self, line_number: u64) {
        self.line.set(line_number);
        self.inner.set_current_line(line_number)
    }

    fn allow_declarative_shadow_roots(&self, intended_parent: &Handle) -> bool {
        self.inner.allow_declarative_shadow_roots(intended_parent)
    }

    fn attach_declarative_shadow(&self, location: &Handle, template: &Handle, attrs: &[Attribute]) -> bool {
        self.inner.attach_declarative_shadow(location, template, attrs)
    }

    fn maybe_clone_an_option_into_selectedcontent(&self, option: &Handle) {
        self.inner.maybe_clone_an_option_into_selectedcontent(option)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pi(t: &str) -> Option<(String, String)> {
        pi_of(t)
    }

    fn some(t: &str, d: &str) -> Option<(String, String)> {
        Some((t.to_string(), d.to_string()))
    }

    // Chrome 152, measured (tools/realweb/seed/htmlpi).
    #[test]
    fn target_and_data() {
        assert_eq!(pi("?a d?"), some("a", "d"));
        assert_eq!(pi("?a?"), some("a", ""));
        assert_eq!(pi("?a"), some("a", ""));
        assert_eq!(pi("?a??"), some("a", "?"));
        assert_eq!(pi("?a?b?"), some("a", "?b"));
        assert_eq!(pi("?A B?"), some("A", "B"));
        assert_eq!(pi("?COM--MENT?"), some("COM--MENT", ""));
        assert_eq!(pi("?a_b d?"), some("a_b", "d"));
        assert_eq!(pi("?_x132 d"), some("_x132", "d"));
        assert_eq!(pi("?something ? "), some("something", "? "));
        assert_eq!(pi("?a\x0cb d?"), some("a", "b d"));
        assert_eq!(pi("?php echo 1; ?"), some("php", "echo 1; "));
        assert_eq!(pi("?a <b"), some("a", "<b"));
        assert_eq!(pi("?xml- d?"), some("xml-", "d"));
        assert_eq!(pi("?xmlfoo x?"), some("xmlfoo", "x"));
    }

    #[test]
    fn stays_a_comment() {
        for t in ["?", "? x?", "?1a?", "?a.b d?", "?a:b x?", "?a\u{fffd}b", "?a\u{a0}b", "?\u{e9}a x?", "?xml version=\"1.0\"", "?XML x?", "?xml-stylesheet href=\"a\"?", "?#"] {
            assert_eq!(pi(t), None, "{t:?}");
        }
    }

    #[test]
    fn end_of_input() {
        assert!(ends_in("<body><?a d", "?a d"));
        assert!(ends_in("<body><?", "?"));
        assert!(ends_in("<p>\r\n<?a d\r\ne", "?a d\ne"));
        assert!(ends_in("<?a d\0", "?a d\u{fffd}"));
        assert!(!ends_in("<body><?a d>", "?a d"));
        assert!(!ends_in("<body><?a d>?a d", "?a d"));
    }
}
