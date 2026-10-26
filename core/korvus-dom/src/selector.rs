//! Public DOM query methods — `querySelector[All]`, `matches`, `closest`, the scoped
//! `element.querySelector[All]` variant, and the `getElementsBy*` collections — over the
//! [`crate::tree::Dom`] arena.
//!
//! T2'b: CSS matching here routes through the `selectors`-crate engine in [`crate::select_engine`]
//! (strict parsing + spec-driven matching), replacing the old hand-rolled matcher. The
//! selector-taking methods return [`SelectorParseError`] on an invalid selector so the webapi layer
//! can raise a DOM `SyntaxError`. The class-name collections match class tokens directly and never
//! parse a selector, so a class that is not a valid CSS identifier (e.g. a leading digit) still
//! works and they never raise.

use crate::select_engine::{self, CompiledQuery, Facts, Narrow, SelectorParseError};
use crate::tree::{Dom, NodeId};
use std::cell::Cell;

impl Dom {
    /// All element nodes with the given tag name, in document order.
    ///
    /// Whole-arena: kept for the callers that genuinely want every node the arena holds (the tag
    /// scan behind `Dom::title`, and the collection ops that intersect with their own subtree).
    /// A *document-level* `getElementsByTagName` must use [`Dom::get_elements_by_tag_name_in`]
    /// with that document's root, or a frame's query answers with the parent page's elements.
    pub fn get_elements_by_tag_name(&self, tag: &str) -> Vec<NodeId> {
        (0..self.nodes.len() as NodeId)
            .filter(|&id| self.tag_name(id) == Some(tag))
            .collect()
    }

    /// `document.getElementsByTagName(tag)` scoped to ONE document tree: elements with this tag
    /// whose root is `root`. With `root == self.root()` this is the main document; with a frame's
    /// (or a DOMParser result's) root it is that document, and the two never see each other.
    ///
    /// In arena (creation) order, as it always was: a frame's scripts run in this order, and twelve
    /// callers read `<base>`, `<meta>`, `<title>` and `<script>` through it. The elements whose root is
    /// `root` are `root` and its descendants, so it walks the tree and sorts what it found, where it
    /// scanned the whole arena and climbed to the root of every node with the tag.
    pub fn get_elements_by_tag_name_in(&self, root: NodeId, tag: &str) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.for_tree_of(root, |n| {
            if self.tag_name(n) == Some(tag) {
                out.push(n);
            }
        });
        out.sort_unstable();
        out
    }

    /// `root` and every descendant of it, in tree order -- the nodes [`Dom::is_in_tree`] puts in the
    /// tree rooted at `root` -- or nothing when `root` is not a tree's root (it has a parent, or names
    /// no node), since no node's root is then `root`.
    fn for_tree_of(&self, root: NodeId, mut f: impl FnMut(NodeId)) {
        if !self.has(root) || self.at(root).parent.is_some() {
            return;
        }
        f(root);
        self.for_each_descendant(root, f);
    }

    /// Elements whose `name` attribute equals `name` (document.getElementsByName), document order.
    pub fn get_elements_by_name(&self, name: &str) -> Vec<NodeId> {
        (0..self.nodes.len() as NodeId).filter(|&id| self.get_attribute(id, "name") == Some(name)).collect()
    }

    /// `document.getElementsByName(name)` scoped to the document tree rooted at `root`, in tree order
    /// (Chrome's; it was arena order, which differs once an element is inserted before an older one).
    /// The name index holds every element carrying the name, so none or one of them is answered
    /// from it; more walk the tree.
    pub fn get_elements_by_name_in(&self, root: NodeId, name: &str) -> Vec<NodeId> {
        match self.elements_named(name) {
            [] => Vec::new(),
            &[only] => {
                if self.get_attribute(only, "name") == Some(name) && self.is_in_tree(root, only) {
                    vec![only]
                } else {
                    Vec::new()
                }
            }
            _ => {
                let mut out = Vec::new();
                self.for_tree_of(root, |n| {
                    if self.get_attribute(n, "name") == Some(name) {
                        out.push(n);
                    }
                });
                out
            }
        }
    }

    /// `<a href>` elements in document order (document.links).
    pub fn document_links(&self) -> Vec<NodeId> {
        self.get_elements_by_tag_name("a").into_iter().filter(|&id| self.get_attribute(id, "href").is_some()).collect()
    }

    /// `document.links` scoped to the document tree rooted at `root`.
    pub fn document_links_in(&self, root: NodeId) -> Vec<NodeId> {
        self.get_elements_by_tag_name_in(root, "a")
            .into_iter()
            .filter(|&id| self.get_attribute(id, "href").is_some())
            .collect()
    }

    /// `Element.matches(selector)` — does this element itself match the selector? The element is its
    /// own `:scope` root (per spec: `el.matches(":scope")` is true). Invalid selector => `Err` (the
    /// webapi layer raises a DOM `SyntaxError`).
    pub fn matches_selector(&self, id: NodeId, sel: &str) -> Result<bool, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        Ok(select_engine::with_matcher(self, &q, Facts::of(self, id, Some(id), None), |m| m.matches(id)))
    }

    /// `document.querySelector(selector)` — first *in-document* element, in arena (document) order,
    /// that matches. Parsed/detached subtrees are excluded. Invalid selector => `Err`.
    pub fn query_selector(&self, sel: &str) -> Result<Option<NodeId>, SelectorParseError> {
        self.query_selector_in(self.root(), sel)
    }

    /// `document.querySelectorAll(selector)` — all *in-document* elements, in arena (document) order,
    /// that match. Invalid selector => `Err`.
    pub fn query_selector_all(&self, sel: &str) -> Result<Vec<NodeId>, SelectorParseError> {
        self.query_selector_all_in(self.root(), sel)
    }

    /// `querySelector` on the document tree rooted at `root` — the generalisation the two methods
    /// above are now the `root == self.root()` case of. Only elements whose tree root IS `root`
    /// are candidates, so a nested browsing context's query never reaches the parent page's nodes
    /// and vice versa.
    ///
    /// The candidates are walked in TREE order under `root`. They were every arena id in CREATION
    /// order, each asked whether it was under `root` -- quadratic once a page had created many nodes,
    /// and the wrong first match whenever an element was inserted before an older one.
    pub fn query_selector_in(&self, root: NodeId, sel: &str) -> Result<Option<NodeId>, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        Ok(self.first_matching_under(root, &q, Facts::of(self, root, None, None)))
    }

    /// `querySelectorAll` on the document tree rooted at `root`, in tree order. See
    /// [`Dom::query_selector_in`].
    pub fn query_selector_all_in(&self, root: NodeId, sel: &str) -> Result<Vec<NodeId>, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        Ok(self.all_matching_under(root, &q, Facts::of(self, root, None, None)))
    }

    /// The descendants of `t` that match `q`, in tree order, through the candidates the plan's ids
    /// leave ([`select_engine::narrow`]).
    fn all_matching_under(&self, t: NodeId, q: &CompiledQuery, facts: Facts) -> Vec<NodeId> {
        let mut out = Vec::new();
        select_engine::with_matcher(self, q, facts, |m| match select_engine::narrow(self, q, t, &facts) {
            Narrow::Walk(w) if !q.plan.ancestor_classes.is_empty() => self.walk_counting(w, q, &facts, |n| {
                if m.matches(n) {
                    out.push(n);
                }
                true
            }),
            Narrow::Walk(w) => self.for_each_descendant(w, |n| {
                if m.matches(n) {
                    out.push(n);
                }
            }),
            Narrow::One(c) => {
                if m.matches(c) {
                    out.push(c);
                }
            }
            Narrow::Empty => {}
        });
        out
    }

    /// The first element [`Dom::all_matching_under`] would return.
    fn first_matching_under(&self, t: NodeId, q: &CompiledQuery, facts: Facts) -> Option<NodeId> {
        select_engine::with_matcher(self, q, facts, |m| match select_engine::narrow(self, q, t, &facts) {
            Narrow::Walk(w) if !q.plan.ancestor_classes.is_empty() => {
                let mut first = None;
                self.walk_counting(w, q, &facts, |n| {
                    if m.matches(n) {
                        first = Some(n);
                        return false;
                    }
                    true
                });
                first
            }
            Narrow::Walk(w) => self.find_descendant(w, |n| m.matches(n)),
            Narrow::One(c) => m.matches(c).then_some(c),
            Narrow::Empty => None,
        })
    }

    /// The descendants of `w` in tree order, each element offered to `visit` (which returns `false` to
    /// stop) unless no proper ancestor of it may carry one of the classes the plan requires of an
    /// ancestor ([`select_engine::class_hash_present`]) -- an element the selector cannot match. The counts are
    /// of `w`'s ancestors-or-self and the open nodes between `w` and the element: exactly its proper
    /// ancestors. `.modal .btn` on a page with no `.modal` matches nothing and now asks nothing.
    fn walk_counting(&self, w: NodeId, q: &CompiledQuery, facts: &Facts, mut visit: impl FnMut(NodeId) -> bool) {
        let classes = &q.plan.ancestor_classes[..q.plan.ancestor_classes.len().min(4)];
        // Per class, how many open ancestors may carry it; `missing`, how many classes none does.
        let counts: [Cell<u32>; 4] = Default::default();
        let missing = Cell::new(classes.len());
        let add = |x: NodeId| {
            for (class, count) in classes.iter().zip(&counts) {
                if select_engine::class_hash_present(self, x, class, facts.case) {
                    if count.get() == 0 {
                        missing.set(missing.get().saturating_sub(1));
                    }
                    count.set(count.get().wrapping_add(1));
                }
            }
        };
        let remove = |x: NodeId| {
            for (class, count) in classes.iter().zip(&counts) {
                if select_engine::class_hash_present(self, x, class, facts.case) {
                    count.set(count.get().wrapping_sub(1));
                    if count.get() == 0 {
                        missing.set(missing.get().saturating_add(1));
                    }
                }
            }
        };
        let mut x = Some(w);
        let mut budget = self.len();
        while let Some(n) = x {
            add(n);
            if budget == 0 {
                break;
            }
            budget -= 1;
            x = self.at(n).parent;
        }
        self.walk_descendants(
            w,
            |n| {
                let at = self.at(n);
                if missing.get() == 0 && self.is_element(n) && !visit(n) {
                    return false;
                }
                if at.first_child.is_some() {
                    add(n);
                }
                true
            },
            remove,
        );
    }

    /// `Element.closest(selector)` — nearest ancestor-or-self that matches, walking up the real tree
    /// (`self` first, then each parent). The element `closest` was called on is the `:scope` root for
    /// every step (per spec: `el.closest(":scope")` returns `el`; `el.closest("div > :scope")` only
    /// matches when `el` itself is that scoped child), so `:scope` is pinned to `id`, not to the
    /// ancestor currently under test. Invalid selector => `Err`.
    ///
    /// One matcher for the whole walk: the scope is the same at every step and every ancestor shares
    /// `id`'s root, so its quirks mode too.
    pub fn closest(&self, id: NodeId, sel: &str) -> Result<Option<NodeId>, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        Ok(select_engine::with_matcher(self, &q, Facts::of(self, id, Some(id), None), |m| {
            let mut cur = Some(id);
            while let Some(c) = cur {
                if m.matches(c) {
                    return Some(c);
                }
                cur = self.at(c).parent;
            }
            None
        }))
    }

    /// Scoped `element.querySelector[All]` engine: descendant elements of `id` (tree order) that
    /// match `sel`, with `id` itself as the `:scope` root. Combinators still walk the real tree, so
    /// e.g. `div p` may reach a `div` ancestor above the scope; only descendants of `id` are
    /// returned. Unlike the document-level query this does NOT filter on `is_in_document`, so it also
    /// works on detached subtrees. `querySelector` takes the first element of the result. Invalid
    /// selector => `Err`.
    pub fn descendants_matching(&self, id: NodeId, sel: &str) -> Result<Vec<NodeId>, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        // Tree order through the sibling links, `scope` fixed at the query root so `:scope` resolves to
        // it at every depth. The recursion this replaced could overflow the stack on a deep enough tree.
        Ok(self.all_matching_under(id, &q, Facts::of(self, id, Some(id), None)))
    }

    /// `element.querySelector(selector)`: the first element [`Dom::descendants_matching`] would return,
    /// found without matching the rest of the subtree.
    pub fn first_descendant_matching(&self, id: NodeId, sel: &str) -> Result<Option<NodeId>, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        Ok(self.first_matching_under(id, &q, Facts::of(self, id, Some(id), None)))
    }

    /// Shadow-scoped `shadowRoot.querySelector[All]` engine (Korvus GS ②-A3): descendant elements of
    /// the shadow root `root` (tree order) that match `sel`, matched from *within* the shadow tree so
    /// a featureless `:host` resolves to the shadow's host (via
    /// [`select_engine::matches_scoped_in_shadow`]). `root` must be a shadow root; a non-shadow-root
    /// (no recorded host) yields an empty result. Combinators still walk the real (shadow) tree, and
    /// there is no `is_in_document` filter — the shadow subtree is by definition not in the light
    /// document. `querySelector` takes the first element. Invalid selector => `Err` (the webapi layer
    /// raises a DOM `SyntaxError`), like every other selector-taking query.
    pub fn shadow_descendants_matching(
        &self,
        root: NodeId,
        sel: &str,
    ) -> Result<Vec<NodeId>, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        let mut out = Vec::new();
        if let Some(host) = self.shadow_host_of(root) {
            // Tree order, `host` fixed so `:host` resolves to it at every depth; every candidate is in
            // the shadow tree, so they share one matcher.
            select_engine::with_matcher(self, &q, Facts::of(self, root, None, Some(host)), |m| {
                self.for_each_descendant(root, |n| {
                    if m.matches(n) {
                        out.push(n);
                    }
                })
            });
            // `::slotted(...)` (Korvus GS ②-B2) matches a slot's ASSIGNED light-DOM nodes, which are
            // the host's light children — NOT shadow-tree descendants, so the walk above never
            // visits them. When the selector is slotted, additionally enumerate each slot's assigned
            // nodes as candidates. The two candidate sets stay cleanly separated: a non-slotted
            // selector never reaches this branch, and a `::slotted` selector never matches a shadow
            // descendant (its `assigned_slot` is `None`, so the slot-assignment combinator fails).
            if q.plan.slotted {
                self.collect_slotted_matches(root, host, &q, &mut out);
            }
        }
        Ok(out)
    }

    /// `shadowRoot.querySelector(selector)`: the first element [`Dom::shadow_descendants_matching`]
    /// would return. That list is the shadow tree's matches, then the slotted ones; when the tree has a
    /// match it is the first, and when it has none the slotted pass starts from an empty list, so its
    /// first match is the list's first.
    pub fn shadow_first_descendant_matching(&self, root: NodeId, sel: &str) -> Result<Option<NodeId>, SelectorParseError> {
        let q = select_engine::compile(sel)?;
        let Some(host) = self.shadow_host_of(root) else { return Ok(None) };
        let first = select_engine::with_matcher(self, &q, Facts::of(self, root, None, Some(host)), |m| {
            self.find_descendant(root, |n| m.matches(n))
        });
        if first.is_some() || !q.plan.slotted {
            return Ok(first);
        }
        for slot in self.descendant_elements(root) {
            if !self.is_html_slot_element(slot) {
                continue;
            }
            let Some(assigned) = self.assigned_nodes(slot) else { continue };
            if let Some(n) = assigned.into_iter().find(|&n| select_engine::matches_scoped_in_shadow(self, n, &q, host)) {
                return Ok(Some(n));
            }
        }
        Ok(None)
    }

    /// Collect the shadow tree's slotted light-DOM matches for a `::slotted()` selector. For every
    /// `<slot>` in the tree, walks its ASSIGNED nodes (via the ②-B1 assignment algorithm
    /// [`Dom::assigned_nodes`], never raw slot children) and matches each from within the shadow
    /// scope, so the crate can cross from the light node to its `<slot>`. Non-element slottables
    /// (text) never match (`matches_scoped_in_shadow` rejects them); results are de-duplicated.
    fn collect_slotted_matches(
        &self,
        root: NodeId,
        host: NodeId,
        list: &select_engine::CompiledSelector,
        out: &mut Vec<NodeId>,
    ) {
        for slot in self.descendant_elements(root) {
            if !self.is_html_slot_element(slot) {
                continue;
            }
            let Some(assigned) = self.assigned_nodes(slot) else { continue };
            for node in assigned {
                if select_engine::matches_scoped_in_shadow(self, node, list, host)
                    && !out.contains(&node)
                {
                    out.push(node);
                }
            }
        }
    }

    /// `document.getElementsByClassName(classNames)` — *in-document* elements whose class set
    /// contains every whitespace-separated token in `class` (case-sensitive, NoQuirks). Matches
    /// class tokens directly (no selector parse), so numeric / punctuated class names still work and
    /// it never raises. Empty / whitespace-only input matches nothing.
    pub fn get_elements_by_class_name(&self, class: &str) -> Vec<NodeId> {
        self.get_elements_by_class_name_in(self.root(), class)
    }

    /// `getElementsByClassName` on the document tree rooted at `root` — the generalisation the
    /// method above is the `root == self.root()` case of. In tree order (Chrome's, and what
    /// `element.getElementsByClassName` already answered); it was arena order, which differs once an
    /// element is inserted before an older one. A live collection recounts it after every change: it
    /// was a scan of the whole arena, a climb to the root and a split of the class value into a `Vec`
    /// for every node, and is a walk of the document comparing the element's cached class hashes.
    pub fn get_elements_by_class_name_in(&self, root: NodeId, class: &str) -> Vec<NodeId> {
        let tokens = class_query(class);
        let mut out = Vec::new();
        if tokens.is_empty() {
            return out;
        }
        self.for_tree_of(root, |n| {
            if self.has_all_classes(n, &tokens) {
                out.push(n);
            }
        });
        out
    }

    /// Scoped `element.getElementsByClassName(classNames)` — like [`Dom::get_elements_by_class_name`]
    /// but restricted to descendants of `root` (tree order) and WITHOUT the `is_in_document` filter,
    /// so it works on detached subtrees (WPT `Element-getElementsByClassName`).
    pub fn descendants_by_class_name(&self, root: NodeId, class: &str) -> Vec<NodeId> {
        let tokens = class_query(class);
        let mut out = Vec::new();
        if tokens.is_empty() {
            return out;
        }
        self.for_each_descendant(root, |n| {
            if self.has_all_classes(n, &tokens) {
                out.push(n);
            }
        });
        out
    }

    /// Does `id` carry every class in `tokens` ([`class_query`])? Case-sensitively, from the element's
    /// cached tokens -- `split_whitespace` of its `class` value, the split this compared against.
    fn has_all_classes(&self, id: NodeId, tokens: &[(u32, &str)]) -> bool {
        tokens.iter().all(|&(hash, t)| self.has_class_token(id, hash, t))
    }
}

/// The class names a `getElementsByClassName` argument asks for, each with its
/// [`string_hash`](select_engine::string_hash), hashed once per call rather than per element.
fn class_query(class: &str) -> Vec<(u32, &str)> {
    class.split_whitespace().map(|t| (select_engine::string_hash(t), t)).collect()
}

#[cfg(test)]
mod selector_tests {
    use super::*;

    /// ```text
    /// #root
    ///   ul#list.box
    ///     li.item.first  "one"
    ///     li.item        "two"
    ///   a[href]  "link"      a (no href)  "plain"
    ///   p#lonely "text"
    /// ```
    /// plus a DETACHED `div#ghost > span.item` subtree, so every test can distinguish the methods
    /// that filter on `is_in_document` from the ones that do not.
    struct Fixture {
        dom: Dom,
        root: NodeId,
        list: NodeId,
        item_first: NodeId,
        item_second: NodeId,
        link: NodeId,
        plain: NodeId,
        lonely: NodeId,
        ghost: NodeId,
        ghost_span: NodeId,
    }

    fn fixture() -> Fixture {
        let mut dom = Dom::empty();
        let document = dom.root();

        let root = dom.create_element("div");
        dom.set_attribute(root, "id", "root");
        dom.append_child(document, root);

        let list = dom.create_element("ul");
        dom.set_attribute(list, "id", "list");
        dom.set_attribute(list, "class", "box");
        dom.append_child(root, list);

        let item_first = dom.create_element("li");
        dom.set_attribute(item_first, "class", "item first");
        dom.append_child(list, item_first);
        let t = dom.create_text_node("one");
        dom.append_child(item_first, t);

        let item_second = dom.create_element("li");
        dom.set_attribute(item_second, "class", "item");
        dom.append_child(list, item_second);
        let t = dom.create_text_node("two");
        dom.append_child(item_second, t);

        let link = dom.create_element("a");
        dom.set_attribute(link, "href", "/x");
        dom.set_attribute(link, "name", "n1");
        dom.append_child(root, link);
        let t = dom.create_text_node("link");
        dom.append_child(link, t);

        let plain = dom.create_element("a");
        dom.set_attribute(plain, "name", "n1");
        dom.append_child(root, plain);

        let lonely = dom.create_element("p");
        dom.set_attribute(lonely, "id", "lonely");
        dom.append_child(root, lonely);

        // Detached subtree — never linked into the document.
        let ghost = dom.create_element("div");
        dom.set_attribute(ghost, "id", "ghost");
        let ghost_span = dom.create_element("span");
        dom.set_attribute(ghost_span, "class", "item");
        dom.append_child(ghost, ghost_span);

        Fixture { dom, root, list, item_first, item_second, link, plain, lonely, ghost, ghost_span }
    }

    // ---- getElementsBy* collections (no selector parsing, never raise) ----

    #[test]
    fn get_elements_by_tag_name_scans_the_whole_arena_including_detached() {
        let f = fixture();
        assert_eq!(f.dom.get_elements_by_tag_name("li"), vec![f.item_first, f.item_second]);
        assert_eq!(f.dom.get_elements_by_tag_name("a"), vec![f.link, f.plain]);
        // Deliberately unfiltered: `parse_fragment` relies on finding the <body> of a detached
        // sub-document this way.
        assert_eq!(f.dom.get_elements_by_tag_name("span"), vec![f.ghost_span]);
        assert!(f.dom.get_elements_by_tag_name("nope").is_empty());
    }

    #[test]
    fn get_elements_by_name_matches_the_name_attribute() {
        let f = fixture();
        assert_eq!(f.dom.get_elements_by_name("n1"), vec![f.link, f.plain]);
        assert!(f.dom.get_elements_by_name("missing").is_empty());
    }

    #[test]
    fn document_links_are_anchors_that_actually_have_href() {
        let f = fixture();
        assert_eq!(f.dom.document_links(), vec![f.link], "the href-less <a> is not a link");
    }

    #[test]
    fn get_elements_by_class_name_requires_every_token_and_is_document_scoped() {
        let f = fixture();
        assert_eq!(f.dom.get_elements_by_class_name("item"), vec![f.item_first, f.item_second]);
        // AND across tokens, not OR.
        assert_eq!(f.dom.get_elements_by_class_name("item first"), vec![f.item_first]);
        assert_eq!(f.dom.get_elements_by_class_name("first item"), vec![f.item_first], "token order is irrelevant");
        // Case-sensitive, and the detached span.item is excluded by the in-document filter.
        assert!(f.dom.get_elements_by_class_name("ITEM").is_empty());
        assert!(f.dom.get_elements_by_class_name("").is_empty());
        assert!(f.dom.get_elements_by_class_name("   ").is_empty());
    }

    #[test]
    fn get_elements_by_class_name_never_raises_on_a_non_identifier_class() {
        let mut dom = Dom::empty();
        let document = dom.root();
        let el = dom.create_element("div");
        // "1x" is not a valid CSS identifier — a selector-based implementation would reject it.
        dom.set_attribute(el, "class", "1x");
        dom.append_child(document, el);
        assert_eq!(dom.get_elements_by_class_name("1x"), vec![el]);
    }

    #[test]
    fn descendants_by_class_name_is_scoped_and_works_on_detached_subtrees() {
        let f = fixture();
        assert_eq!(f.dom.descendants_by_class_name(f.list, "item"), vec![f.item_first, f.item_second]);
        assert!(f.dom.descendants_by_class_name(f.item_first, "item").is_empty(), "self is not a descendant");
        // No is_in_document filter here, unlike the document-level collection.
        assert_eq!(f.dom.descendants_by_class_name(f.ghost, "item"), vec![f.ghost_span]);
        assert!(f.dom.descendants_by_class_name(f.list, "").is_empty());
    }

    /// The collections against the arena scans they replaced, kept here as the reference. The tag
    /// collection keeps their exact (arena) order; the class and name collections keep their set and
    /// move to tree order -- which differs only once an element was inserted before an older one.
    #[test]
    fn collections_keep_or_fix_order() {
        let mut dom = Dom::parse(
            "<!doctype html><body><div class='a b'><p name=n1 class=x>1</p><p class='  x   y '>2</p></div>\
             <span class='a\u{a0}b'></span><span class=X name=n3></span><input name=n3><b name=n3 class=a></b></body>",
        );
        let doc = dom.root();
        let body = dom.descendant_elements(doc).into_iter().find(|&n| dom.tag_name(n) == Some("body")).unwrap();
        // Created last, inserted first: arena order and tree order now disagree.
        let early = dom.create_element("p");
        dom.set_attribute(early, "class", "a x");
        dom.set_attribute(early, "name", "n3");
        let first = dom.first_element_child(body).unwrap();
        dom.insert_before(body, early, first);
        let other = dom.parse_into("<!doctype html><p class=a name=n3>q</p><div class=a></div>");
        let lone = dom.create_element("div");
        dom.set_attribute(lone, "class", "a");
        let lone_child = dom.create_element("p");
        dom.set_attribute(lone_child, "class", "a x");
        dom.set_attribute(lone_child, "name", "n3");
        dom.append_child(lone, lone_child);
        let roots = [doc, other, lone, body, lone_child, NodeId::MAX];
        let arena = |f: &dyn Fn(NodeId) -> bool, root: NodeId| -> Vec<NodeId> {
            (0..dom.len() as NodeId).filter(|&id| f(id) && dom.is_in_tree(root, id)).collect()
        };
        let tree = |f: &dyn Fn(NodeId) -> bool, root: NodeId| -> Vec<NodeId> {
            let mut out = Vec::new();
            if dom.has(root) && dom.parent(root).is_none() {
                if f(root) {
                    out.push(root);
                }
                dom.for_each_descendant(root, |n| {
                    if f(n) {
                        out.push(n)
                    }
                });
            }
            out
        };
        let sorted = |mut v: Vec<NodeId>| {
            v.sort();
            v
        };
        let has_all = |id: NodeId, class: &str| {
            let tokens: Vec<&str> = class.split_whitespace().collect();
            !tokens.is_empty()
                && dom.get_attribute(id, "class").is_some_and(|c| {
                    let present: Vec<&str> = c.split_whitespace().collect();
                    tokens.iter().all(|t| present.contains(t))
                })
        };
        for root in roots {
            for tag in ["p", "div", "span", "body", "html", "nope"] {
                let f = |id: NodeId| dom.tag_name(id) == Some(tag);
                assert_eq!(dom.get_elements_by_tag_name_in(root, tag), arena(&f, root), "{tag} under #{root}: arena order");
            }
            for class in ["a", "x", "a x", "b", "y", "X", "x y", "", "  ", "b a", "nope"] {
                let f = |id: NodeId| has_all(id, class);
                let got = dom.get_elements_by_class_name_in(root, class);
                assert_eq!(got, tree(&f, root), "{class:?} under #{root}: tree order");
                assert_eq!(sorted(got), arena(&f, root), "{class:?} under #{root}: the same elements");
            }
            for name in ["n1", "n3", "missing"] {
                let f = |id: NodeId| dom.get_attribute(id, "name") == Some(name);
                let got = dom.get_elements_by_name_in(root, name);
                assert_eq!(got, tree(&f, root), "{name} under #{root}: tree order");
                assert_eq!(sorted(got), arena(&f, root), "{name} under #{root}: the same elements");
            }
        }
        // The current tokenization, pinned: Unicode whitespace splits a class value.
        let nbsp = dom.descendant_elements(body).into_iter().find(|&n| dom.get_attribute(n, "class") == Some("a\u{a0}b")).unwrap();
        assert!(dom.get_elements_by_class_name("a b").contains(&nbsp));
        assert_eq!(dom.get_elements_by_class_name("a")[0], early, "tree order puts the inserted element first");
        assert_eq!(dom.get_elements_by_name_in(doc, "n3")[0], early);
        assert_eq!(dom.get_elements_by_name_in(doc, "n1").len(), 1);
        assert_eq!(dom.descendants_by_class_name(lone, "a x"), vec![lone_child]);
        assert_eq!(dom.descendants_by_class_name(body, "x"), tree(&|n| has_all(n, "x"), doc).into_iter().filter(|&n| n != body && dom.contains(body, n)).collect::<Vec<_>>());
    }

    // ---- matches_selector ----

    #[test]
    fn matches_selector_tests_the_element_itself() {
        let f = fixture();
        assert_eq!(f.dom.matches_selector(f.item_first, "li").unwrap(), true);
        assert_eq!(f.dom.matches_selector(f.item_first, ".item.first").unwrap(), true);
        assert_eq!(f.dom.matches_selector(f.item_first, "#list").unwrap(), false);
        assert_eq!(f.dom.matches_selector(f.item_first, "ul > li").unwrap(), true);
        assert_eq!(f.dom.matches_selector(f.item_second, "li:first-child").unwrap(), false);
        assert_eq!(f.dom.matches_selector(f.item_first, "li:first-child").unwrap(), true);
    }

    #[test]
    fn matches_selector_treats_the_element_as_its_own_scope() {
        let f = fixture();
        // Per spec `el.matches(":scope")` is true — the element is its own :scope root.
        assert_eq!(f.dom.matches_selector(f.item_first, ":scope").unwrap(), true);
    }

    #[test]
    fn matches_selector_works_on_a_detached_element() {
        let f = fixture();
        assert_eq!(f.dom.matches_selector(f.ghost_span, "span.item").unwrap(), true);
    }

    // ---- query_selector / query_selector_all ----

    #[test]
    fn query_selector_returns_the_first_match_in_document_order() {
        let f = fixture();
        assert_eq!(f.dom.query_selector("li").unwrap(), Some(f.item_first));
        assert_eq!(f.dom.query_selector(".item").unwrap(), Some(f.item_first));
        assert_eq!(f.dom.query_selector("#lonely").unwrap(), Some(f.lonely));
        assert_eq!(f.dom.query_selector("table").unwrap(), None);
    }

    #[test]
    fn query_selector_all_returns_every_match_in_document_order() {
        let f = fixture();
        assert_eq!(f.dom.query_selector_all("li").unwrap(), vec![f.item_first, f.item_second]);
        assert_eq!(f.dom.query_selector_all("a").unwrap(), vec![f.link, f.plain]);
        assert_eq!(f.dom.query_selector_all("ul > li.item").unwrap(), vec![f.item_first, f.item_second]);
        assert_eq!(f.dom.query_selector_all("nothing").unwrap(), vec![]);
    }

    /// The document-level queries must never surface a detached (or DOMParser-subtree) element —
    /// this is the filter that also keeps them out of shadow trees.
    #[test]
    fn document_queries_exclude_detached_subtrees() {
        let f = fixture();
        assert_eq!(f.dom.query_selector("span").unwrap(), None);
        assert_eq!(f.dom.query_selector("#ghost").unwrap(), None);
        assert_eq!(f.dom.query_selector_all(".item").unwrap(), vec![f.item_first, f.item_second]);
    }

    #[test]
    fn selector_lists_and_attribute_selectors_work() {
        let f = fixture();
        assert_eq!(f.dom.query_selector_all("a[href]").unwrap(), vec![f.link]);
        assert_eq!(f.dom.query_selector_all(r#"a[name="n1"]"#).unwrap(), vec![f.link, f.plain]);
        assert_eq!(f.dom.query_selector_all("p, ul").unwrap(), vec![f.list, f.lonely], "a list yields document order, not selector order");
    }

    // ---- closest ----

    #[test]
    fn closest_walks_self_then_ancestors() {
        let f = fixture();
        assert_eq!(f.dom.closest(f.item_first, "li").unwrap(), Some(f.item_first), "self matches first");
        assert_eq!(f.dom.closest(f.item_first, "ul").unwrap(), Some(f.list));
        assert_eq!(f.dom.closest(f.item_first, "#root").unwrap(), Some(f.root));
        assert_eq!(f.dom.closest(f.item_first, "table").unwrap(), None);
    }

    #[test]
    fn closest_pins_scope_to_the_starting_element() {
        let f = fixture();
        // Per spec :scope stays the element `closest` was called on, at every step of the walk.
        assert_eq!(f.dom.closest(f.item_first, ":scope").unwrap(), Some(f.item_first));
    }

    // ---- descendants_matching (scoped element.querySelectorAll) ----

    #[test]
    fn descendants_matching_excludes_self_and_stays_inside_the_scope() {
        let f = fixture();
        assert_eq!(f.dom.descendants_matching(f.list, "li").unwrap(), vec![f.item_first, f.item_second]);
        assert_eq!(f.dom.descendants_matching(f.list, "ul").unwrap(), vec![], "the scope root itself is never returned");
        assert_eq!(f.dom.descendants_matching(f.root, "a").unwrap(), vec![f.link, f.plain]);
    }

    #[test]
    fn descendants_matching_returns_tree_order() {
        let f = fixture();
        assert_eq!(f.dom.descendants_matching(f.root, "*").unwrap(), vec![f.list, f.item_first, f.item_second, f.link, f.plain, f.lonely]);
    }

    /// Combinators are matched against the REAL tree, so an ancestor above the scope root can
    /// satisfy the left side — but only descendants of the scope are ever returned.
    #[test]
    fn descendants_matching_combinators_may_reach_above_the_scope() {
        let f = fixture();
        assert_eq!(f.dom.descendants_matching(f.list, "div li").unwrap(), vec![f.item_first, f.item_second]);
    }

    #[test]
    fn descendants_matching_works_on_a_detached_subtree() {
        let f = fixture();
        // Unlike the document-level query, this has no is_in_document filter.
        assert_eq!(f.dom.descendants_matching(f.ghost, "span").unwrap(), vec![f.ghost_span]);
    }

    // ---- invalid selectors ----

    #[test]
    fn every_selector_taking_method_reports_a_parse_error() {
        let f = fixture();
        // A lone combinator is a syntax error; the webapi layer turns this into DOM SyntaxError.
        let bad = "div >>";
        assert!(f.dom.matches_selector(f.item_first, bad).is_err());
        assert!(f.dom.query_selector(bad).is_err());
        assert!(f.dom.query_selector_all(bad).is_err());
        assert!(f.dom.closest(f.item_first, bad).is_err());
        assert!(f.dom.descendants_matching(f.root, bad).is_err());
        assert!(f.dom.shadow_descendants_matching(f.root, bad).is_err());
    }

    #[test]
    fn an_empty_selector_is_a_parse_error() {
        let f = fixture();
        assert!(f.dom.query_selector("").is_err());
    }

    // ---- shadow_descendants_matching ----

    #[test]
    fn shadow_descendants_matching_on_a_non_shadow_root_is_empty() {
        let f = fixture();
        assert_eq!(f.dom.shadow_descendants_matching(f.root, "*").unwrap(), vec![]);
    }

    #[test]
    fn shadow_descendants_matching_finds_shadow_tree_elements() {
        let mut dom = Dom::empty();
        let document = dom.root();
        let host = dom.create_element("div");
        dom.append_child(document, host);
        let shadow = dom.attach_shadow(host, crate::tree::ShadowRootMode::Open).expect("attach");
        let inner = dom.create_element("b");
        dom.set_attribute(inner, "class", "in-shadow");
        dom.append_child(shadow, inner);

        assert_eq!(dom.shadow_descendants_matching(shadow, "b.in-shadow").unwrap(), vec![inner]);
        // ...and the document-level query must NOT see it (encapsulation).
        assert_eq!(dom.query_selector("b").unwrap(), None);
    }

    // ---- the native query bench (no V8, no bindings) ----

    /// The page of the query microbench, natively: 40 rows of 40 `.cell`s (odd rows `.a`, even rows `.b`),
    /// 360 `span.f`, and 30 chains of ten nested `.filler.fN` under `#deep`. With no doctype it is a QUIRKS
    /// document, which is what turns on the per-element `root_of` climb in `quirks_mode_of`.
    fn bench_dom(doctype: bool) -> Dom {
        let mut h = String::from(if doctype { "<!doctype html>" } else { "" });
        h += "<html><head><style>.r{color:red}</style><link rel=stylesheet href=x></head><body><div id=grid>";
        for r in 0..40 {
            h += if r % 2 == 1 { "<div class=\"a row\">" } else { "<div class=\"b row\">" };
            for c in 0..40 { h += &format!("<div class=cell>{c}</div>"); }
            h += "</div>";
        }
        for _ in 0..360 { h += "<span class=f></span>"; }
        h += "</div><div id=deep>";
        for _ in 0..30 { for d in 0..10 { h += &format!("<div class=\"filler f{d}\">"); } for _ in 0..10 { h += "</div>"; } }
        h += "</div></body></html>";
        Dom::parse(&h)
    }

    /// `cargo test --release -p korvus-dom qsa_microbench -- --ignored --nocapture`. The result column
    /// must not move between changes; only the time may.
    #[test]
    #[ignore]
    fn qsa_microbench() {
        // QSA_ONLY=<case> and QSA_MODE=<no-quirks|quirks> run one line, for `perf stat -e instructions:u`.
        let mode = std::env::var("QSA_MODE").ok();
        for doctype in [true, false] {
            let label = if doctype { "no-quirks" } else { "quirks" };
            if mode.as_deref().is_some_and(|m| m != label) { continue; }
            let dom = bench_dom(doctype);
            eprintln!("--- {label}");
            let grid = dom.query_selector("#grid").unwrap().unwrap();
            let cell = *dom.query_selector_all(".a .cell").unwrap().last().unwrap();
            // The thread's own CPU time (`/proc/thread-self/schedstat`), not the wall clock: on a shared,
            // loaded machine a preempted run reads slower by the wall clock and the same by this.
            let cpu_ns = || -> f64 {
                std::fs::read_to_string("/proc/thread-self/schedstat")
                    .ok()
                    .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
                    .unwrap_or(f64::NAN)
            };
            let only = std::env::var("QSA_ONLY").ok();
            let t = |name: &str, n: u32, f: &mut dyn FnMut() -> usize| {
                if only.as_deref().is_some_and(|o| o != name) { return; }
                let (s, c) = (std::time::Instant::now(), cpu_ns());
                let mut k = 0usize;
                for _ in 0..n { k += std::hint::black_box(f()); }
                let cpu = (cpu_ns() - c) / 1e3 / n as f64;
                eprintln!("{name:40} {cpu:9.3} us  (result {})  wall {:.3} us  n={n}", k / n as usize, s.elapsed().as_secs_f64() * 1e6 / n as f64);
            };
            for (s, n) in [(".a .cell", 2000u32), (".zz .cell", 2000), (".cell .zz", 2000), (".zz div", 2000), ("style, link", 5000), ("#grid > .a > .cell", 2000), ("#deep .f9", 5000), ("div[class=cell]", 2000), ("div:not(.row)", 500), (".cell:nth-child(7)", 500), (".row > .cell + .cell", 500), (":-webkit-any(.a,.b) > .cell:nth-child(2)", 500)] {
                t(s, n, &mut || dom.query_selector_all(s).unwrap().len());
            }
            t("qs #grid", 200_000, &mut || dom.query_selector("#grid").unwrap().is_some() as usize);
            t("qs .a .cell", 50_000, &mut || dom.query_selector(".a .cell").unwrap().is_some() as usize);
            t("el qs .a .cell", 2000, &mut || dom.descendants_matching(grid, ".a .cell").unwrap().first().is_some() as usize);
        t("el qs1 .a .cell", 200_000, &mut || dom.first_descendant_matching(grid, ".a .cell").unwrap().is_some() as usize);
            t("matches .a .cell", 200_000, &mut || dom.matches_selector(cell, ".a .cell").unwrap() as usize);
            t("closest #grid", 200_000, &mut || dom.closest(cell, "#grid").unwrap().is_some() as usize);
            t("closest .zz", 200_000, &mut || dom.closest(cell, ".zz").unwrap().is_some() as usize);
            t("gebcn cell", 2000, &mut || dom.get_elements_by_class_name("cell").len());
            t("tag_in div", 2000, &mut || dom.get_elements_by_tag_name_in(dom.root(), "div").len());
        }
    }
}
