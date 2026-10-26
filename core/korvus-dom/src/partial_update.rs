//! `<template for>` -- declarative partial updates, which Chrome 150 ships.
//!
//! A `<template for="name">` the HTML parser meets is not inserted: its content replaces the range a
//! `<?start name="name">` processing instruction opens earlier in the same tree (up to the `<?end>` that
//! closes it, or the end of the start's parent), or goes where a `<?marker name="name">` is. The markers
//! go with it. With no such marker the template stays, an ordinary template.
//!
//! Chrome 150, measured (`tools/realweb/seed/tplfor`, `c7/repro/tplfor/attrs.html`, and the 44 WPT files of
//! `html/semantics/scripting-1/the-template-element/template-for/` + `html/dom/partial-updates/` it passes):
//!
//! - the marker is the FIRST `start` or `marker` PI in tree order, before the template and in the tree
//!   the template is parsed into (the document, a template's content, a declarative shadow root, an
//!   `innerHTML` fragment -- never the document around a fragment), whose `name` equals the template's
//!   `for`. An empty `for` patches nothing.
//! - PI targets are already lowercase (the tokenizer lowercases them, `html_pi`); `name` is an attribute
//!   of the PI's data read strictly: a quoted value (either quote, spaces around `=` allowed, character
//!   references decoded), and an unquoted value, a second `name`, or text glued to the closing quote
//!   leave the PI naming nothing. The attribute name is case-sensitive (`Name="d"` names nothing).
//! - `<?end>` pairs with starts by nesting among the start's following siblings; its own `name` is
//!   ignored. A `start` with no end among its siblings runs to the end of its parent.
//! - markers inside a patch are live: a later template can target them.
//! - a template whose `shadowrootmode` is valid is a declarative shadow root instead; an invalid one
//!   patches.
//!
//! Chrome applies a patch while it parses, so a script BETWEEN a marker and its template sees the old
//! content; this pass runs when the parse is done (like the declarative shadow root pass), which is the
//! same tree for every script after the template -- the ones the page runs once it has parsed.

use std::collections::VecDeque;

use crate::tree::{Dom, NodeId, NodeKind};

/// A page document's `<template for>` patches that wait for its script pass
/// ([`Dom::parse_page`]): the templates not reached yet, in parse order, and the one being streamed.
#[derive(Default)]
pub(crate) struct PendingPatches {
    waiting: VecDeque<NodeId>,
    open: Option<OpenPatch>,
    /// The scripts of a patch whose marker was gone when the pass reached it: its template stays an
    /// ordinary one, and they never run.
    abandoned: std::collections::HashSet<NodeId>,
}

/// A patch the pass has reached: the content not there yet, as a stack of frames -- the patch's own
/// children going into its target, and, for an element the pass stopped INSIDE of, that element's
/// children going into it (the parser streams a patch node by node, so a script deep in
/// the content runs with its element in place and none of what follows it).
struct OpenPatch {
    frames: Vec<PatchFrame>,
    /// The patch's markers, removed wherever they are once the content has all streamed in (WPT
    /// `template-for-unclosed-content`: no PI remains; `-move-start-while-appending`: a start marker a
    /// script put back is gone at the end).
    markers: Vec<NodeId>,
}

/// Where a run of streamed nodes goes, and the nodes still to come.
struct PatchFrame {
    parent: NodeId,
    before: Option<NodeId>,
    queue: VecDeque<NodeId>,
}

/// The value of the `name` attribute in a PI's data, read the strict way Chrome reads it (see the
/// module docs), or `None`.
pub(crate) fn pi_name(data: &str) -> Option<String> {
    let b = data.as_bytes();
    let mut i = 0;
    let mut found: Option<String> = None;
    let ws = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r');
    loop {
        while i < b.len() && ws(b[i]) {
            i += 1;
        }
        if i >= b.len() {
            return found;
        }
        let start = i;
        while i < b.len() && !ws(b[i]) && b[i] != b'=' {
            i += 1;
        }
        let key = &data[start..i];
        while i < b.len() && ws(b[i]) {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' || key.is_empty() {
            return None;
        }
        i += 1;
        while i < b.len() && ws(b[i]) {
            i += 1;
        }
        let q = *b.get(i)?;
        if q != b'"' && q != b'\'' {
            return None;
        }
        let vstart = i + 1;
        let vend = vstart + data[vstart..].find(q as char)?;
        i = vend + 1;
        if i < b.len() && !ws(b[i]) {
            return None;
        }
        if key == "name" {
            if found.is_some() {
                return None;
            }
            found = Some(crate::parse::unescape_lossy(&data[vstart..vend]));
        }
    }
}

impl Dom {
    fn pi_parts(&self, id: NodeId) -> Option<(&str, &str)> {
        match &self.at(id).kind {
            NodeKind::ProcessingInstruction(pi) => Some((pi.target.as_str(), pi.data.as_str())),
            _ => None,
        }
    }

    fn is_template(&self, id: NodeId) -> bool {
        self.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("template"))
    }

    /// Keep the document tree's `<template for>`s for its script pass: Chrome applies a patch
    /// WHILE it parses -- a script before the template sees the old content, a script inside it runs in the
    /// target with the content parsed up to itself there and the template already gone, and a script in the
    /// range it replaces has run (measured, `c7/repro/tplscript`, `c7/repro/tplnested`; WPT
    /// `template-for-scripting-nested`, `template-for-move-*-while-appending`). Applied when the parse is
    /// done, a patch deleted that range's scripts before they ran and put its own ahead of the scripts
    /// before its template. Template contents are inert trees of their own: their patches apply now.
    pub(crate) fn defer_template_patches(&mut self, root: NodeId) {
        let mut templates = Vec::new();
        self.collect_patch_templates(root, &mut templates);
        // Every one waits, marker or not: a script before it may insert its marker (WPT
        // `template-for-case-sensitive-dom-inserted`), as it may in Chrome before the parser reaches the
        // template. One that finds none when the pass gets there stays an ordinary template, and its
        // scripts never run (`open_patch`; WPT `sanitize-regular`).
        self.pending_patches.waiting = templates.into_iter().collect();
        let mut contents = Vec::new();
        self.collect_template_contents(root, &mut contents);
        for c in contents {
            self.apply_template_patches(c);
        }
    }

    /// The `<script>`s inside the content of the patches still to come, in parse order: the page's script
    /// pass runs them as each patch streams in (their node ids place them where their template was parsed).
    pub fn pending_patch_scripts(&self) -> Vec<NodeId> {
        let open = self.pending_patches.open.iter().flat_map(|o| o.frames.iter().flat_map(|f| f.queue.iter().copied()));
        let waiting = self.pending_patches.waiting.iter().filter_map(|&t| self.template_content_of(t)).flat_map(|f| self.children(f));
        self.scripts_among(open.chain(waiting).collect())
    }

    /// The `<script>`s in and under `nodes`, in node-id order. Not inside a nested template: a template's
    /// content, inside a patch or not, is inert -- its scripts never run (WPT `template-for-scripting-nested`).
    fn scripts_among(&self, nodes: Vec<NodeId>) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = nodes;
        while let Some(n) = stack.pop() {
            if self.is_html_ns(n) && self.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case("script")) {
                out.push(n);
            }
            if !self.is_template(n) {
                stack.extend(self.children(n));
            }
        }
        out.sort_unstable();
        out
    }

    /// Whether a page script pass has patches to stream (see [`Dom::stream_template_patches`]).
    pub fn has_pending_patches(&self) -> bool {
        !self.pending_patches.waiting.is_empty() || self.pending_patches.open.is_some()
    }

    /// Bring the document's patches up to the script the pass runs next, `next` (its node id: parse order),
    /// or all the way when the pass is over (`None`). Every patch whose template was parsed before `next`
    /// is applied, and the one whose content holds `next` streams its content up to the child that holds
    /// it: the old range goes when the pass reaches the template, as the parser's `<template for>` start
    /// tag takes it away in Chrome, and the content arrives child by child.
    ///
    /// `false` when `next` is a script of a patch that could not open (its marker was gone): it stays in an
    /// ordinary template's content, and the pass does not run it.
    pub fn stream_template_patches(&mut self, next: Option<NodeId>) -> bool {
        self.stream_to(next);
        next.is_none_or(|n| !self.pending_patches.abandoned.contains(&n))
    }

    fn stream_to(&mut self, next: Option<NodeId>) {
        loop {
            if let Some(mut open) = self.pending_patches.open.take() {
                // Node by node (S33b): everything before `next` goes in whole; the node that IS `next`
                // goes in and the pass stops there; a node that only HOLDS it goes in empty, and its
                // children become the frame streamed next.
                let mut stopped = false;
                while let Some(frame) = open.frames.last_mut() {
                    let Some(c) = frame.queue.pop_front() else {
                        open.frames.pop();
                        continue;
                    };
                    // The insertion point FOLLOWS the end marker while it is still a child of the target,
                    // and is the target's end once a script has moved or removed it (WPT
                    // `template-for-move-{end,marker}-while-appending`).
                    let parent = frame.parent;
                    let before = frame.before.filter(|&b| self.parent(b) == Some(parent));
                    match next {
                        Some(n) if c == n => {
                            self.insert_before(parent, c, before.unwrap_or(u32::MAX));
                            stopped = true;
                            break;
                        }
                        Some(n) if self.contains(c, n) => {
                            let kids: VecDeque<NodeId> = self.children(c).into();
                            for &k in &kids {
                                self.remove_child(c, k);
                            }
                            self.insert_before(parent, c, before.unwrap_or(u32::MAX));
                            open.frames.push(PatchFrame { parent: c, before: None, queue: kids });
                        }
                        _ => self.insert_before(parent, c, before.unwrap_or(u32::MAX)),
                    }
                }
                if stopped {
                    // Kept even when nothing is left to stream: the patch completes -- its markers go --
                    // only after the script it stopped at has run.
                    self.pending_patches.open = Some(open);
                    return;
                }
                for m in open.markers {
                    if let Some(p) = self.parent(m) {
                        self.remove_child(p, m);
                    }
                }
                continue;
            }
            let Some(&t) = self.pending_patches.waiting.front() else { return };
            if next.is_some_and(|n| n < t) {
                return;
            }
            self.pending_patches.waiting.pop_front();
            self.open_patch(t);
        }
    }

    /// The pass reached `template`: its marker is looked up now (a script before it may have moved or
    /// removed one), the range it replaces goes, and so does the template; its content waits to stream.
    fn open_patch(&mut self, template: NodeId) {
        let marker = self.tree_root_of(template).and_then(|scope| {
            let name = self.get_attribute(template, "for").filter(|v| !v.is_empty())?.to_string();
            self.find_marker(scope, template, &name)
        });
        let Some((marker, parent)) = marker.and_then(|m| Some((m, self.parent(m)?))) else {
            let content = self.template_content_of(template).map(|f| self.children(f)).unwrap_or_default();
            let scripts = self.scripts_among(content);
            self.pending_patches.abandoned.extend(scripts);
            return;
        };
        let gone = self.patch_range(marker);
        // The END marker -- or a lone `<?marker>` -- stays while the content streams: it is where the
        // content goes (S33b). A `<?start>` with no end has no anchor, and its content goes at the end.
        let anchor = gone.last().copied().filter(|&l| self.pi_parts(l).is_some_and(|(t, _)| t == "end" || t == "marker"));
        let before = match anchor {
            Some(a) => Some(a),
            None => gone.last().and_then(|&l| self.next_sibling(l)),
        };
        for &n in &gone {
            if Some(n) != anchor {
                self.remove_child(parent, n);
            }
        }
        let mut markers = vec![marker];
        markers.extend(anchor.filter(|&a| a != marker));
        let queue = self.template_content_of(template).map(|f| self.children(f)).unwrap_or_default().into();
        if let Some(tp) = self.parent(template) {
            self.remove_child(tp, template);
        }
        self.pending_patches.open = Some(OpenPatch { frames: vec![PatchFrame { parent, before, queue }], markers });
    }

    /// The root of the tree `n` is in (its topmost ancestor).
    fn tree_root_of(&self, n: NodeId) -> Option<NodeId> {
        let mut cur = n;
        let mut budget = self.nodes.len();
        while let Some(p) = self.parent(cur) {
            cur = p;
            budget = budget.checked_sub(1)?;
        }
        Some(cur)
    }

    /// Apply every `<template for>` in the tree under `scope`, and in every template content inside it
    /// (each its own tree), in tree order.
    pub(crate) fn apply_template_patches(&mut self, scope: NodeId) {
        let mut templates = Vec::new();
        self.collect_patch_templates(scope, &mut templates);
        for t in templates {
            // A template an earlier patch took away with its range is gone from this tree.
            if !self.contains(scope, t) {
                continue;
            }
            let Some(name) = self.get_attribute(t, "for").filter(|v| !v.is_empty()).map(str::to_string) else { continue };
            if let Some(marker) = self.find_marker(scope, t, &name) {
                self.patch(t, marker);
            }
        }
        // Each template content left in the tree is a tree of its own.
        let mut contents = Vec::new();
        self.collect_template_contents(scope, &mut contents);
        for c in contents {
            self.apply_template_patches(c);
        }
    }

    /// The `<template for>`s in `node`'s tree, in tree order, not entering template contents.
    fn collect_patch_templates(&self, node: NodeId, out: &mut Vec<NodeId>) {
        for c in self.children(node) {
            if self.is_template(c) {
                if self.get_attribute(c, "for").is_some() && self.declarative_shadow_mode_of(c).is_none() {
                    out.push(c);
                }
                continue;
            }
            self.collect_patch_templates(c, out);
        }
    }

    fn collect_template_contents(&self, node: NodeId, out: &mut Vec<NodeId>) {
        for c in self.children(node) {
            if self.is_template(c) {
                if let Some(frag) = self.template_content_of(c) {
                    out.push(frag);
                }
                continue;
            }
            self.collect_template_contents(c, out);
        }
    }

    /// The first `start`/`marker` PI named `name` before `template` in `scope`'s tree order.
    fn find_marker(&self, scope: NodeId, template: NodeId, name: &str) -> Option<NodeId> {
        let mut stack: Vec<NodeId> = self.children(scope).into_iter().rev().collect();
        while let Some(n) = stack.pop() {
            if n == template {
                return None;
            }
            if let Some((target, data)) = self.pi_parts(n) {
                if (target == "start" || target == "marker") && pi_name(data).as_deref() == Some(name) {
                    return Some(n);
                }
                continue;
            }
            if self.is_template(n) {
                continue;
            }
            stack.extend(self.children(n).into_iter().rev());
        }
        None
    }

    /// Put `template`'s content where `marker` is (with a `start`'s range and its end), and take the
    /// template out.
    fn patch(&mut self, template: NodeId, marker: NodeId) {
        let Some(parent) = self.parent(marker) else { return };
        let gone = self.patch_range(marker);
        let content = self.template_content_of(template).map(|f| self.children(f)).unwrap_or_default();
        for c in content {
            self.insert_before(parent, c, marker);
        }
        for n in gone {
            self.remove_child(parent, n);
        }
        if let Some(tp) = self.parent(template) {
            self.remove_child(tp, template);
        }
    }

    /// The nodes a patch at `marker` replaces: the marker itself, and for a `start` its range up to and
    /// including the `end` that closes it (or to the end of its parent).
    fn patch_range(&self, marker: NodeId) -> Vec<NodeId> {
        let is_start = self.pi_parts(marker).is_some_and(|(t, _)| t == "start");
        let mut gone = vec![marker];
        if is_start {
            let mut depth = 0usize;
            let mut cur = self.next_sibling(marker);
            while let Some(n) = cur {
                cur = self.next_sibling(n);
                match self.pi_parts(n).map(|(t, _)| t) {
                    Some("start") => depth += 1,
                    Some("end") if depth == 0 => {
                        gone.push(n);
                        break;
                    }
                    Some("end") => depth -= 1,
                    _ => {}
                }
                gone.push(n);
            }
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::pi_name;
    use crate::tree::Dom;

    /// `<div>{placeholder}</div>{patch}` parsed as a document, the body serialized -- WPT
    /// `template-for-multiple-cases.html`'s shape, with Chrome 150's outputs.
    fn patched(placeholder: &str, patch: &str) -> String {
        let dom = Dom::parse(&format!("<!doctype html><body><div>{placeholder}</div>{patch}"));
        let body = dom.get_elements_by_tag_name("body")[0];
        dom.inner_html(body)
    }

    #[test]
    fn patches() {
        let cases: &[(&str, &str, &str)] = &[
            (r#"<?start name="p">Old content"#, r#"<template for="p">New content</template>"#, "<div>New content</div>"),
            (r#"<?start name="p">Old<?end> content"#, r#"<template for="p">New</template>"#, "<div>New content</div>"),
            (r#"<?marker name="p"> content"#, r#"<template for="p">New</template>"#, "<div>New content</div>"),
            (r#"New <?marker name="a"> content <?marker name="a">"#, r#"<template for="a">X</template>"#, r#"<div>New X content <?marker name="a"?></div>"#),
            (r#"<?marker name="b"> content"#, r#"<template for="b">X<?marker name="b"></template><template for="b">Y</template>"#, "<div>XY content</div>"),
            (r#"<?start name="a">New <?start name="b"> content <?end> after <?end>"#, r#"<template for="a">X</template>"#, "<div>X</div>"),
            (r#"<?start name="a">New <?start name="b"> content <?end> after"#, r#"<template for="b">X</template>"#, r#"<div><?start name="a"?>New X after</div>"#),
            (r#"<?start name="a">New <?start name="b"> content <?end name="a"> after<?end>"#, r#"<template for="b">X</template>"#, r#"<div><?start name="a"?>New X after<?end ?></div>"#),
            (r#"<?start name="">Old<?end>"#, r#"<template for="">New</template>"#, r#"<div><?start name=""?>Old<?end ?></div><template for="">New</template>"#),
            (r#"<?start name="p">Old<?end>"#, r#"<template for="p"><span>New</template>"#, "<div><span>New</span></div>"),
            (r#"<?start name="p">Old<?end>"#, r#"<template for="p"></template>"#, "<div></div>"),
            (r#"New <?marker name="p"> content"#, r#"<template for="p"></template>"#, "<div>New  content</div>"),
            (r#"<?other name="p"><?marker name="p"> content"#, r#"<template for="p">New</template>"#, r#"<div><?other name="p"?>New content</div>"#),
            (r#"<?end name="p"><?marker name="p"> content"#, r#"<template for="p">New</template>"#, r#"<div><?end name="p"?>New content</div>"#),
        ];
        for (placeholder, patch, want) in cases {
            assert_eq!(patched(placeholder, patch), *want, "{placeholder} + {patch}");
        }
    }

    #[test]
    fn template_before_its_marker_stays() {
        assert_eq!(
            patched("", r#"<template for="p">New</template><p><?start name="p">Old</p>"#),
            r#"<div></div><template for="p">New</template><p><?start name="p"?>Old</p>"#
        );
    }

    // Chrome 150, measured (c7/repro/tplfor/attrs.html).
    #[test]
    fn name_attribute() {
        assert_eq!(pi_name(r#"name="p""#).as_deref(), Some("p"));
        assert_eq!(pi_name(r#" name = "b2" "#).as_deref(), Some("b2"));
        assert_eq!(pi_name(r#"x="1" name="b3""#).as_deref(), Some("b3"));
        assert_eq!(pi_name("name='b5'").as_deref(), Some("b5"));
        assert_eq!(pi_name(r#"name="b&amp;9""#).as_deref(), Some("b&9"));
        assert_eq!(pi_name(r#"name="ph#a""#).as_deref(), Some("ph#a"));
        assert_eq!(pi_name("name=b1"), None);
        assert_eq!(pi_name(r#"name="b4" name="zz""#), None);
        assert_eq!(pi_name(r#"name="b6"x"#), None);
        assert_eq!(pi_name(r#"xname="b8""#), None);
        assert_eq!(pi_name(r#"Name="d""#), None);
        assert_eq!(pi_name(""), None);
    }
}
