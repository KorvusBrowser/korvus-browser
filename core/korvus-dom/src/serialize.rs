//! `innerHTML`/`outerHTML` serialization + fragment-based `innerHTML=` for the
//! [`crate::tree::Dom`] arena.

use crate::tree::{Dom, NodeId, NodeKind, ShadowRootMode};

/// The elements whose TEXT is SERIALIZED raw. HTML's fragment serialization escapes a text node
/// unless its parent is one of these -- so a `<script>`'s `a<b && c>d` comes back out unchanged and
/// a `<textarea>`'s does not (it is an *escapable* raw text element and IS escaped).
///
/// It is deliberately NOT [`is_raw_text_element`] at the bottom of this file, which is the PARSE
/// side's list: `noscript` and `plaintext` serialize raw (measured below) while that list must stay
/// as it is, because a tag added there changes how `innerHTML=` BUILDS the tree.
///
/// MEASURED, Chrome 150 over one loopback page, `div.innerHTML` after building each child by hand:
///
/// ```text
///   <script type=text/plain>if (a<b && c>d) { x = '&amp;' }        raw, U+00A0 raw too
///   <style>a::before{content:'<&>\u{a0}'}                          raw
///   <noscript>a&b<c>                                               raw
///   <!--c&<>\u{a0}d-->                                             raw
///   <textarea>a&b<c>\u{a0}      ->  a&amp;b&lt;c&gt;&nbsp;         ESCAPED
/// ```
fn serializes_text_raw(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext" | "noscript"
    )
}

/// HTML's "escape a string" for a TEXT node: `&`, U+00A0, `<`, `>`. A quote is not escaped here --
/// Chrome writes `d"e'f` through untouched.
fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// HTML's "escape a string" in ATTRIBUTE MODE: the text set plus `"`, because the value is written
/// inside double quotes. An apostrophe and a backtick are NOT escaped, which is measured rather than
/// assumed -- Chrome 150 writes `x?a=1&amp;b=2&amp;amp;c=&lt;&gt;&quot;'&nbsp;` + a literal backtick
/// for the value `x?a=1&b=2&amp;c=<>"'\u{a0}` + backtick.
///
/// `<` and `>` are in BOTH sets. They are the newer half of the rule (the HTML spec added them to
/// attribute mode as an injection mitigation) and Chrome 150 implements it, which is why this file
/// escapes them rather than following older prose.
fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// HTML void elements (no children, no closing tag).
fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr"
    )
}

impl Dom {
    /// Replace an element's children with the parsed fragment (`innerHTML` setter). Returns
    /// (removed old children, added new children) so the op layer can queue a childList record.
    ///
    /// A `<template>` element routes the whole operation into its `content` DocumentFragment: the
    /// HTML spec gives a template an empty light child list and puts parsed markup in `content`
    /// (the MutationObserver record still reports the host, which is where script observes).
    pub fn set_inner_html(&mut self, id: NodeId, html: &str) -> (Vec<NodeId>, Vec<NodeId>) {
        // A tree mutation even when nothing is linked (`innerHTML = ""`): the detach below writes the
        // chain directly, and a live collection caches on the revision (see `Dom::unlink`).
        self.revision += 1;
        self.unrendered_if(id);
        let is_template = self.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("template")) && !self.is_in_xml(id);
        let target = if is_template { self.move_children_into_template_content(id) } else { id };
        // Detach the old children first: clear their parent pointers (and sibling links), not just
        // the parent's child chain. Otherwise root_of()/is_in_document() still reach a removed
        // subtree via its stale parent edge, so getElementById keeps finding an id that was cleared.
        // -- WPT Document-getElementById.
        let old = self.children(target);
        for &c in &old {
            let n = &mut self.nodes[c as usize];
            n.parent = None;
            n.prev_sibling = None;
            n.next_sibling = None;
        }
        self.forget_roots();
        self.nodes[target as usize].first_child = None;
        self.nodes[target as usize].last_child = None;
        self.journal.push_replaced(target, &old);
        // The fragment-parsing CONTEXT is `id`, never `target`: for a `<template>` the markup goes
        // into `content` (`target`) but it is the template element that puts the parser in the
        // "in template" insertion mode.
        let new_children = self.parse_inner_html_children(id, target, html);
        for &c in &new_children {
            // Each fragment root is detached; link it as target's next child.
            self.link_as_last_child(target, c);
        }
        (old, new_children)
    }

    /// Turn an `innerHTML` string into the target's new child nodes, fragment-parsed in `context`
    /// (WHATWG "fragment parsing algorithm"; see [`Dom::parse_fragment_in`]).
    ///
    /// The raw-text special case is KEPT, and it is now REDUNDANT rather than load-bearing.
    /// It exists because the old context-free whole-document parse turned `<style>.innerHTML = " "`
    /// (a CSS-in-JS lib's sheet primer, e.g. goober under Termly on webscraper.io) into dropped
    /// document whitespace, leaving `style.firstChild === null` and the library's `firstChild.data`
    /// a `null.data` TypeError. With a real context element html5ever tokenizes in RAWTEXT and
    /// produces exactly the same single verbatim text node this branch does -- measured for all six
    /// raw-text tags, whitespace-only body included, by `parse.rs`'s
    /// `a_raw_text_context_tokenizes_its_content_verbatim`. Ruling: keep it. It is provably
    /// equivalent, it costs one string compare, and deleting it on a guess is what regressed
    /// webscraper.io once already -- the test is the evidence for whoever removes it later.
    fn parse_inner_html_children(&mut self, context: NodeId, target: NodeId, html: &str) -> Vec<NodeId> {
        if is_raw_text_element(self.tag_name(target)) {
            if html.is_empty() {
                Vec::new()
            } else {
                vec![self.push(NodeKind::Text(html.to_string()), None)]
            }
        } else {
            self.parse_fragment_in(context, html)
        }
    }

    /// `Element/ShadowRoot.setHTMLUnsafe(html)` — same "replace all children" shape as
    /// [`Dom::set_inner_html`], but through [`Dom::parse_fragment_unsafe`] so a top-level
    /// `<template shadowrootmode>` in `html` attaches a declarative shadow root to `id` itself
    /// instead of becoming a literal child (WPT declarative-shadow-dom-basic.html's "Fragment
    /// parser basic test").
    pub fn set_inner_html_unsafe(&mut self, id: NodeId, html: &str) -> (Vec<NodeId>, Vec<NodeId>) {
        self.revision += 1;
        self.unrendered_if(id);
        let is_template = self.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("template")) && !self.is_in_xml(id);
        let target = if is_template { self.move_children_into_template_content(id) } else { id };
        let old = self.children(target);
        for &c in &old {
            let n = &mut self.nodes[c as usize];
            n.parent = None;
            n.prev_sibling = None;
            n.next_sibling = None;
        }
        self.forget_roots();
        self.nodes[target as usize].first_child = None;
        self.nodes[target as usize].last_child = None;
        self.journal.push_replaced(target, &old);
        let new_children = self.parse_fragment_unsafe(id, target, html);
        for &c in &new_children {
            self.link_as_last_child(target, c);
        }
        (old, new_children)
    }

    /// Serialize a node's children to HTML (`innerHTML`). A `<template>`'s markup lives in its
    /// `content` fragment, so that is what serializes.
    pub fn inner_html(&self, id: NodeId) -> String {
        let target = self
            .template_content_of(id)
            .filter(|_| self.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("template")))
            .unwrap_or(id);
        let mut out = String::new();
        let mut c = self.at(target).first_child;
        while let Some(cid) = c {
            self.serialize(cid, &mut out);
            c = self.at(cid).next_sibling;
        }
        out
    }

    /// Serialize a node including itself (`outerHTML`).
    pub fn outer_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.serialize(id, &mut out);
        out
    }

    /// `Element/ShadowRoot.getHTML(options)`: like [`Dom::inner_html`], except a shadow root
    /// attached to a descendant element is ALSO serialized -- as its declarative
    /// `<template shadowrootmode="...">` form, immediately before that host's light children --
    /// when `serializable_shadow_roots` is true and the root's own `.serializable` flag is true,
    /// or when the root's id is in `forced_roots` (an explicit `options.shadowRoots` list is
    /// included regardless of `serializable_shadow_roots`, matching the spec's `getHTML`
    /// algorithm). With neither, this serializes identically to `inner_html` -- WPT gethtml.html.
    pub fn get_html(&self, id: NodeId, serializable_shadow_roots: bool, forced_roots: &[NodeId]) -> String {
        let target = self
            .template_content_of(id)
            .filter(|_| self.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("template")))
            .unwrap_or(id);
        let mut out = String::new();
        // `getHTML` is called ON `id`, so unlike a walk that reaches a shadow host as some OTHER
        // element's child (handled inside `serialize_with_shadow`'s Element arm), `id` itself being
        // a shadow host is never visited that way -- its shadow root has to be checked here too.
        if let Some(root) = self.shadow_root_of(id) {
            let include = forced_roots.contains(&root) || (serializable_shadow_roots && self.shadow_serializable(root) == Some(true));
            if include {
                self.serialize_declarative_shadow_template(root, serializable_shadow_roots, forced_roots, &mut out);
            }
        }
        let mut c = self.at(target).first_child;
        while let Some(cid) = c {
            self.serialize_with_shadow(cid, serializable_shadow_roots, forced_roots, &mut out);
            c = self.at(cid).next_sibling;
        }
        out
    }

    /// Is this text node's parent one of the elements that serialize their text RAW? The one place
    /// both serializers ask, so they cannot drift apart on it.
    fn text_is_raw(&self, id: NodeId) -> bool {
        self.parent(id)
            .filter(|&p| self.is_html_ns(p))
            .and_then(|p| self.tag_name(p))
            .is_some_and(|n| serializes_text_raw(&n.to_ascii_lowercase()))
    }

    fn serialize_with_shadow(&self, id: NodeId, sr: bool, forced: &[NodeId], out: &mut String) {
        match &self.at(id).kind {
            NodeKind::Text(t) => {
                if self.text_is_raw(id) {
                    out.push_str(t);
                } else {
                    escape_text(t, out);
                }
            }
            NodeKind::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
            // `<?target data?>`, the data raw (a `>` in it too) and the space kept when it is empty --
            // Chrome 150, measured (WPT html/syntax/serializing-html-fragments/
            // processing-instructions.html). It used to be dropped.
            NodeKind::ProcessingInstruction(pi) => {
                let (target, data) = (&pi.target, &pi.data);
                out.push_str("<?");
                out.push_str(target);
                out.push(' ');
                out.push_str(data);
                out.push_str("?>");
            }
            NodeKind::Element { name, attrs, .. } => {
                out.push('<');
                out.push_str(name);
                for (k, v) in attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    escape_attr(v, out);
                    out.push('"');
                }
                out.push('>');
                if is_void_element(name) {
                    return;
                }
                if let Some(root) = self.shadow_root_of(id) {
                    let include = forced.contains(&root) || (sr && self.shadow_serializable(root) == Some(true));
                    if include {
                        self.serialize_declarative_shadow_template(root, sr, forced, out);
                    }
                }
                let content = self.template_content_of(id).filter(|_| name.eq_ignore_ascii_case("template"));
                let first = content.map(|f| self.at(f).first_child).unwrap_or_else(|| self.at(id).first_child);
                let mut c = first;
                while let Some(cid) = c {
                    self.serialize_with_shadow(cid, sr, forced, out);
                    c = self.at(cid).next_sibling;
                }
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            }
            _ => {}
        }
    }

    /// The `<template shadowrootmode>` a shadow root serializes as, per `getHTML`: mode first,
    /// then delegatesfocus/serializable/slotassignment=manual/clonable/referencetarget (only the ones set, in that order --
    /// matching Chrome's own `getHTML` output, WPT gethtml.html's expected string). Empty-value
    /// boolean attributes (`shadowrootdelegatesfocus=""`), same as any other reflected boolean.
    fn serialize_declarative_shadow_template(&self, root: NodeId, sr: bool, forced: &[NodeId], out: &mut String) {
        out.push_str("<template shadowrootmode=\"");
        out.push_str(match self.shadow_mode(root) {
            Some(ShadowRootMode::Closed) => "closed",
            _ => "open",
        });
        out.push('"');
        if self.shadow_delegates_focus(root) == Some(true) {
            out.push_str(" shadowrootdelegatesfocus=\"\"");
        }
        if self.shadow_serializable(root) == Some(true) {
            out.push_str(" shadowrootserializable=\"\"");
        }
        // Chrome 152 (`markup_accumulator.cc`): a manual slot assignment between serializable and clonable, the
        // reference target last, when it is set.
        if self.shadow_slot_assignment(root) == Some(crate::tree::ShadowSlotAssignment::Manual) {
            out.push_str(" shadowrootslotassignment=\"manual\"");
        }
        if self.shadow_clonable(root) == Some(true) {
            out.push_str(" shadowrootclonable=\"\"");
        }
        if let Some(t) = self.shadow_reference_target(root) {
            out.push_str(" shadowrootreferencetarget=\"");
            escape_attr(t, out);
            out.push('"');
        }
        out.push('>');
        let mut c = self.at(root).first_child;
        while let Some(cid) = c {
            self.serialize_with_shadow(cid, sr, forced, out);
            c = self.at(cid).next_sibling;
        }
        out.push_str("</template>");
    }

    fn serialize(&self, id: NodeId, out: &mut String) {
        match &self.at(id).kind {
            // A TEXT NODE IS ESCAPED UNLESS ITS PARENT SERIALIZES RAW TEXT. Without this, markup
            // this engine produced could not be re-parsed into the tree it came from -- an `&` in a
            // URL attribute or a `<` in a paragraph came back out as itself, and a `"` inside a
            // double-quoted attribute value ENDED the attribute.
            NodeKind::Text(t) => {
                if self.text_is_raw(id) {
                    out.push_str(t);
                } else {
                    escape_text(t, out);
                }
            }
            NodeKind::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
            // `<?target data?>`, the data raw (a `>` in it too) and the space kept when it is empty --
            // Chrome 150, measured (WPT html/syntax/serializing-html-fragments/
            // processing-instructions.html). It used to be dropped.
            NodeKind::ProcessingInstruction(pi) => {
                let (target, data) = (&pi.target, &pi.data);
                out.push_str("<?");
                out.push_str(target);
                out.push(' ');
                out.push_str(data);
                out.push_str("?>");
            }
            NodeKind::Element { name, attrs, .. } => {
                out.push('<');
                out.push_str(name);
                for (k, v) in attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    escape_attr(v, out);
                    out.push('"');
                }
                out.push('>');
                if is_void_element(name) {
                    return; // void elements have no children or closing tag
                }
                // A template's serialized markup is its content fragment's children, never its
                // (always empty) light child list.
                let content = self
                    .template_content_of(id)
                    .filter(|_| name.eq_ignore_ascii_case("template"));
                let first = content.map(|f| self.at(f).first_child).unwrap_or_else(|| self.at(id).first_child);
                let mut c = first;
                while let Some(cid) = c {
                    self.serialize(cid, out);
                    c = self.at(cid).next_sibling;
                }
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod serialize_tests {
    use super::*;

    /// Build `<div id=root>` attached to the document and return it — the usual serialization target.
    fn doc_with_root() -> (Dom, NodeId) {
        let mut dom = Dom::empty();
        let document = dom.root();
        let root = dom.create_element("div");
        dom.set_attribute(root, "id", "root");
        dom.append_child(document, root);
        (dom, root)
    }

    // ---- outer_html / inner_html ----

    #[test]
    fn outer_html_includes_the_element_itself_inner_html_does_not() {
        let (mut dom, root) = doc_with_root();
        let p = dom.create_element("p");
        dom.append_child(root, p);
        let t = dom.create_text_node("hi");
        dom.append_child(p, t);

        assert_eq!(dom.outer_html(p), "<p>hi</p>");
        assert_eq!(dom.inner_html(p), "hi");
        assert_eq!(dom.outer_html(root), r#"<div id="root"><p>hi</p></div>"#);
        assert_eq!(dom.inner_html(root), "<p>hi</p>");
    }

    #[test]
    fn inner_html_of_a_childless_element_is_empty() {
        let (dom, root) = doc_with_root();
        assert_eq!(dom.inner_html(root), "");
    }

    #[test]
    fn attributes_serialize_in_insertion_order() {
        let (mut dom, root) = doc_with_root();
        let a = dom.create_element("a");
        dom.set_attribute(a, "href", "/x");
        dom.set_attribute(a, "class", "c");
        dom.append_child(root, a);
        assert_eq!(dom.outer_html(a), r#"<a href="/x" class="c"></a>"#);
    }

    #[test]
    fn void_elements_get_no_closing_tag_and_swallow_children() {
        let (mut dom, root) = doc_with_root();
        let br = dom.create_element("br");
        dom.append_child(root, br);
        let img = dom.create_element("img");
        dom.set_attribute(img, "src", "/i.png");
        dom.append_child(root, img);
        // A void element must never emit `</br>` even if something was linked under it.
        let stray = dom.create_text_node("nope");
        dom.append_child(br, stray);

        assert_eq!(dom.outer_html(br), "<br>");
        assert_eq!(dom.outer_html(img), r#"<img src="/i.png">"#);
        assert_eq!(dom.inner_html(root), r#"<br><img src="/i.png">"#);
    }

    #[test]
    fn comments_serialize_with_their_delimiters() {
        let (mut dom, root) = doc_with_root();
        let c = dom.create_comment("note");
        dom.append_child(root, c);
        assert_eq!(dom.inner_html(root), "<!--note-->");
    }

    #[test]
    fn a_processing_instruction_serializes_and_a_doctype_does_not() {
        let (mut dom, root) = doc_with_root();
        let pi = dom.create_processing_instruction("t", "d");
        dom.append_child(root, pi);
        let dt = dom.create_doctype("html", "", "");
        dom.append_child(root, dt);
        // Chrome 150 serializes a processing instruction in HTML as `<?target data?>` (measured:
        // `<?something good?>`, and `<?something ?>` with no data); `serialize`'s catch-all arm drops the rest.
        assert_eq!(dom.inner_html(root), "<?t d?>");
    }

    #[test]
    fn nested_children_serialize_in_sibling_order() {
        let (mut dom, root) = doc_with_root();
        for tag in ["one", "two", "three"] {
            let el = dom.create_element(tag);
            dom.append_child(root, el);
        }
        assert_eq!(dom.inner_html(root), "<one></one><two></two><three></three>");
    }

    /// THE GAP THIS FILE USED TO RECORD IS CLOSED. It said "Chrome escapes `&`/`<`/`>`
    /// in text and `&`/`"` in attribute values; `serialize` writes both raw", and asked to be updated
    /// when escaping landed. Every expectation below is Chrome 150's own output for the same nodes,
    /// read back through `div.innerHTML` over a loopback page.
    #[test]
    fn text_and_attribute_values_are_escaped_as_chrome_escapes_them() {
        let (mut dom, root) = doc_with_root();
        let t = dom.create_text_node("a&b<c>d\"e'f\u{a0}g");
        dom.append_child(root, t);
        assert_eq!(
            dom.inner_html(root),
            "a&amp;b&lt;c&gt;d\"e'f&nbsp;g",
            "a text node escapes & < > and U+00A0, and leaves both quotes alone"
        );

        let el = dom.create_element("i");
        dom.set_attribute(el, "title", "x?a=1&b=2&amp;c=<>\"'\u{a0}`");
        dom.append_child(root, el);
        assert_eq!(
            dom.outer_html(el),
            "<i title=\"x?a=1&amp;b=2&amp;amp;c=&lt;&gt;&quot;'&nbsp;`\"></i>",
            "an attribute value escapes & < > \" and U+00A0, and leaves ' and ` alone"
        );
    }

    /// AND THE OTHER HALF OF THE RULE: the elements whose text is serialized RAW. Chrome 150's
    /// answers for the same four nodes, and a `<textarea>` beside them because it is an ESCAPABLE
    /// raw text element and is escaped like any other text.
    #[test]
    fn a_raw_text_element_s_content_is_serialized_verbatim() {
        let (mut dom, root) = doc_with_root();
        for tag in ["script", "style", "noscript"] {
            let el = dom.create_element(tag);
            let t = dom.create_text_node("if (a<b && c>d) { x = '&amp;' }");
            dom.append_child(el, t);
            dom.append_child(root, el);
            assert_eq!(
                dom.outer_html(el),
                format!("<{tag}>if (a<b && c>d) {{ x = '&amp;' }}</{tag}>"),
                "<{tag}> serializes its text verbatim, as Chrome does"
            );
        }
        let ta = dom.create_element("textarea");
        let t = dom.create_text_node("a&b<c>\u{a0}");
        dom.append_child(ta, t);
        dom.append_child(root, ta);
        assert_eq!(
            dom.outer_html(ta),
            "<textarea>a&amp;b&lt;c&gt;&nbsp;</textarea>",
            "a <textarea> is ESCAPABLE raw text: Chrome escapes it"
        );
        let c = dom.create_comment("c&<>\u{a0}d");
        dom.append_child(root, c);
        assert_eq!(dom.outer_html(c), "<!--c&<>\u{a0}d-->", "a comment is written verbatim");
    }

    // ---- set_inner_html ----

    #[test]
    fn set_inner_html_reports_removed_and_added_children() {
        let (mut dom, root) = doc_with_root();
        let old_a = dom.create_element("old");
        let old_b = dom.create_element("old");
        dom.append_child(root, old_a);
        dom.append_child(root, old_b);

        let (removed, added) = dom.set_inner_html(root, "<new>x</new><new>y</new>");

        assert_eq!(removed, vec![old_a, old_b]);
        assert_eq!(added.len(), 2);
        assert_eq!(dom.children(root), added, "the parent now holds exactly the new children");
        assert_eq!(dom.inner_html(root), "<new>x</new><new>y</new>");
    }

    /// The WPT `Document-getElementById` case the implementation calls out: replaced children must
    /// have their *parent* pointer cleared, not merely be unlinked from the parent's chain —
    /// otherwise `root_of`/`is_in_document` still reach them and `getElementById` keeps finding a
    /// cleared id.
    #[test]
    fn set_inner_html_fully_detaches_the_old_children() {
        let (mut dom, root) = doc_with_root();
        let gone = dom.create_element("span");
        dom.set_attribute(gone, "id", "gone");
        dom.append_child(root, gone);
        assert_eq!(dom.get_element_by_id("gone"), Some(gone));

        dom.set_inner_html(root, "<p>fresh</p>");

        assert_eq!(dom.parent(gone), None, "parent pointer cleared, not just the child chain");
        assert!(!dom.is_in_document(gone));
        assert_eq!(dom.previous_sibling(gone), None);
        assert_eq!(dom.next_sibling(gone), None);
        assert_eq!(dom.get_element_by_id("gone"), None, "a cleared id must stop resolving");
    }

    #[test]
    fn set_inner_html_to_empty_string_clears_the_element() {
        let (mut dom, root) = doc_with_root();
        let child = dom.create_element("p");
        dom.append_child(root, child);

        let (removed, added) = dom.set_inner_html(root, "");

        assert_eq!(removed, vec![child]);
        assert!(added.is_empty());
        assert_eq!(dom.inner_html(root), "");
        assert_eq!(dom.first_child(root), None);
        assert_eq!(dom.last_child(root), None);
    }

    #[test]
    fn set_inner_html_then_serialize_round_trips_simple_markup() {
        let (mut dom, root) = doc_with_root();
        let html = r#"<ul><li class="a">one</li><li>two</li></ul>"#;
        dom.set_inner_html(root, html);
        assert_eq!(dom.inner_html(root), html);
    }

    #[test]
    fn set_inner_html_links_new_children_with_correct_sibling_edges() {
        let (mut dom, root) = doc_with_root();
        let (_, added) = dom.set_inner_html(root, "<a></a><b></b><c></c>");
        assert_eq!(added.len(), 3);
        assert_eq!(dom.first_child(root), Some(added[0]));
        assert_eq!(dom.last_child(root), Some(added[2]));
        assert_eq!(dom.next_sibling(added[0]), Some(added[1]));
        assert_eq!(dom.previous_sibling(added[2]), Some(added[1]));
        assert_eq!(dom.previous_sibling(added[0]), None);
        assert_eq!(dom.next_sibling(added[2]), None);
        for &a in &added {
            assert_eq!(dom.parent(a), Some(root));
        }
    }

    // ---- setHTMLUnsafe / getHTML (declarative shadow DOM) ----

    #[test]
    fn set_inner_html_unsafe_attaches_a_declarative_shadow_root_on_the_target() {
        let (mut dom, root) = doc_with_root();
        let (_, added) = dom.set_inner_html_unsafe(root, r#"<template shadowrootmode="open"><slot></slot></template>"#);
        assert!(added.is_empty(), "the sole top-level template was consumed into root's own shadow root");
        let shadow = dom.shadow_root_of(root).expect("setHTMLUnsafe attaches to the target itself");
        let slot = dom.child_elements(shadow)[0];
        assert_eq!(dom.tag_name(slot), Some("slot"));
    }

    #[test]
    fn set_inner_html_unsafe_replaces_existing_children_like_inner_html_does() {
        let (mut dom, root) = doc_with_root();
        dom.set_inner_html(root, "<p>old</p>");
        let (removed, _) = dom.set_inner_html_unsafe(root, "<p>new</p>");
        assert_eq!(removed.len(), 1);
        assert_eq!(dom.text_content(root), "new");
    }

    #[test]
    fn get_html_matches_inner_html_when_serializable_shadow_roots_is_false_or_absent() {
        let (mut dom, root) = doc_with_root();
        dom.set_inner_html_unsafe(root, r#"<template shadowrootmode="open" shadowrootserializable><b>x</b></template><p>light</p>"#);
        assert_eq!(dom.get_html(root, false, &[]), dom.inner_html(root));
    }

    #[test]
    fn get_html_serializes_a_serializable_shadow_root_as_a_declarative_template() {
        let (mut dom, root) = doc_with_root();
        dom.set_inner_html_unsafe(
            root,
            r#"<template shadowrootmode="open" shadowrootserializable shadowrootdelegatesfocus><b>x</b></template><p>light</p>"#,
        );
        let html = dom.get_html(root, true, &[]);
        assert_eq!(
            html,
            r#"<template shadowrootmode="open" shadowrootdelegatesfocus="" shadowrootserializable=""><b>x</b></template><p>light</p>"#
        );
    }

    #[test]
    fn get_html_leaves_out_a_non_serializable_shadow_root_unless_explicitly_forced() {
        let (mut dom, root) = doc_with_root();
        dom.set_inner_html_unsafe(root, r#"<template shadowrootmode="open"><b>x</b></template><p>light</p>"#);
        assert_eq!(dom.get_html(root, true, &[]), "<p>light</p>", "not serializable -> left out even with the flag on");
        let shadow = dom.shadow_root_of(root).unwrap();
        assert_eq!(
            dom.get_html(root, false, &[shadow]),
            r#"<template shadowrootmode="open"><b>x</b></template><p>light</p>"#,
            "named in shadowRoots -> included regardless of serializableShadowRoots"
        );
    }
}

/// The HTML raw text elements: their content is CDATA-like literal text (no nested markup, no entity
/// references), so `innerHTML` / `textContent` set it verbatim. `title`/`textarea` are ESCAPABLE raw
/// text (entities decode) and are deliberately NOT here -- they keep the fragment-parse path.
fn is_raw_text_element(tag: Option<&str>) -> bool {
    tag.is_some_and(|t| {
        ["style", "script", "xmp", "iframe", "noembed", "noframes"]
            .iter()
            .any(|r| t.eq_ignore_ascii_case(r))
    })
}
