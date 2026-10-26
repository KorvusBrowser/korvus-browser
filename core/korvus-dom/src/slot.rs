//! Shadow DOM slot assignment (Korvus GS ②-B1): the name-based algorithm that projects a shadow
//! host's LIGHT children (its ordinary DOM children) into `<slot>` elements inside the host's
//! shadow tree.
//!
//! v8-free, computed **on demand** -- there is no cached assignment map. Every query here is a
//! fresh walk of the shadow tree / host's children, so a mutation to either tree (moving a
//! slottable, adding/removing a `<slot>`, changing a `slot`/`name` attribute) can never leave a
//! stale assignment lying around; there is nothing to invalidate. korvus-dom's shadow trees are
//! small (test-fixture-sized, not thousand-node app shells), so the O(shadow-tree-size) walk per
//! call is the right trade for correctness-by-construction over a cache with a sync-bug surface.
//!
//! Only "slottable" light children participate: **element and text nodes**, never comments/PIs/
//! etc. (WHATWG "slottable" concept).
//!
//! Scope: this module computes the algorithm only. `::slotted()` matching and the
//! `HTMLSlotElement`/`assignedSlot`/`slotchange` webapi surface are Korvus GS ②-B2.
//!
//! Adapted (not copied) from a reference DOM implementation
//! (`is_html_slot_element`/`assigned_slot`/`assigned_nodes`/`slot_rendered_children`) onto
//! korvus-dom's arena `Dom` API (`shadow_root_of`/`is_shadow_root`/
//! `shadow_host_of`/`descendant_elements`/`get_attribute` in `tree.rs`).

use crate::tree::{Dom, NodeId, NodeKind};

impl Dom {
    /// Whether `id` is an HTML `<slot>` element. Local-name check only: korvus-dom's arena has no
    /// per-element namespace, so (unlike a namespace-aware DOM) this can't distinguish an XHTML
    /// `<slot>` from a same-named element in another vocabulary -- not a concern for the HTML-only
    /// documents korvus-dom parses.
    pub fn is_html_slot_element(&self, id: NodeId) -> bool {
        self.tag_name(id) == Some("slot")
    }

    /// Whether `id` is "slottable" -- an Element or a Text node. Only slottables participate in
    /// slot assignment; comments and other node kinds never do.
    fn is_slottable(&self, id: NodeId) -> bool {
        matches!(self.node(id).kind, NodeKind::Element { .. } | NodeKind::Text(_))
    }

    /// A `<slot>` element's declared name: its `name` attribute, or `""` (the default slot) if
    /// absent.
    pub fn slot_name(&self, slot_id: NodeId) -> String {
        self.get_attribute(slot_id, "name").unwrap_or("").to_string()
    }

    /// A slottable light child's effective slot name: its `slot` attribute, or `""` if absent.
    /// Text nodes carry no attributes at all, so `get_attribute` is always `None` for them and
    /// this always returns `""` -- text is always a default-slot candidate.
    pub fn slottable_slot_name(&self, node_id: NodeId) -> String {
        self.get_attribute(node_id, "slot").unwrap_or("").to_string()
    }

    /// The shadow root `id` sits inside, if any: `id`'s light-tree root ([`Dom::root_of`]) when
    /// that root is itself a shadow-root arena node. `None` for a node in the main document tree,
    /// a detached subtree, or a `DOMParser`/`createHTMLDocument` document.
    fn containing_shadow_root(&self, id: NodeId) -> Option<NodeId> {
        let root = self.root_of(id);
        self.is_shadow_root(root).then_some(root)
    }

    /// The slot `node_id` is assigned to.
    ///
    /// `node_id` must be a slottable (Element or Text) that is a direct LIGHT child of a shadow
    /// host -- i.e. its parent has an attached shadow root. Per the "first same-name slot wins"
    /// rule, this returns the FIRST slot in shadow-tree order whose name equals `node_id`'s
    /// effective slot name.
    ///
    /// `None` when: `node_id` isn't a slottable, its parent has no shadow root, or no slot in that
    /// shadow tree has a matching name.
    pub fn assigned_slot(&self, node_id: NodeId) -> Option<NodeId> {
        if !self.is_slottable(node_id) {
            return None;
        }
        let parent = self.parent(node_id)?;
        let root = self.shadow_root_of(parent)?;
        let name = self.slottable_slot_name(node_id);
        self.descendant_elements(root)
            .into_iter()
            .find(|&candidate| self.is_html_slot_element(candidate) && self.slot_name(candidate) == name)
    }

    /// The host's slottable light children (tree order) assigned to `slot_id`.
    ///
    /// `slot_id` must be a `<slot>` element that lives inside a shadow tree. Per "the first
    /// same-name slot wins": if an earlier slot (shadow-tree order) already shares this slot's
    /// name, nodes assign to *that* slot instead, so this returns an empty list even though the
    /// name matches -- a duplicate `<slot name="x">` always gets nothing.
    ///
    /// `None` when `slot_id` is not a `<slot>` element inside a shadow tree.
    pub fn assigned_nodes(&self, slot_id: NodeId) -> Option<Vec<NodeId>> {
        if !self.is_html_slot_element(slot_id) {
            return None;
        }
        let root = self.containing_shadow_root(slot_id)?;
        let host = self.shadow_host_of(root)?;
        let name = self.slot_name(slot_id);

        let is_first_same_name_slot = self
            .descendant_elements(root)
            .into_iter()
            .filter(|&id| self.is_html_slot_element(id))
            .take_while(|&id| id != slot_id)
            .all(|id| self.slot_name(id) != name);
        if !is_first_same_name_slot {
            return Some(Vec::new());
        }

        Some(
            self.children(host)
                .into_iter()
                .filter(|&child| self.is_slottable(child) && self.slottable_slot_name(child) == name)
                .collect(),
        )
    }

    /// The flattened children `slot_id` renders: its assigned nodes, or -- when unassigned (no
    /// light child targets it) -- the slot's own children as fallback content.
    ///
    /// `None` when `slot_id` is not a `<slot>` element inside a shadow tree.
    pub fn slot_rendered_children(&self, slot_id: NodeId) -> Option<Vec<NodeId>> {
        let assigned = self.assigned_nodes(slot_id)?;
        if assigned.is_empty() {
            Some(self.children(slot_id))
        } else {
            Some(assigned)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::ShadowRootMode;

    /// Shared fixture (Korvus GS ②-B1 task spec): host `<div id=h>` with light children, in
    /// order, `<span slot=a>`, `<em slot=a>`, `<p>` (no slot attr), text "hi"; an open shadow root
    /// whose own children are `<slot name=a>` then `<slot>` (default).
    struct Basic {
        dom: Dom,
        host: NodeId,
        root: NodeId,
        named_slot: NodeId,
        default_slot: NodeId,
        span: NodeId,
        em: NodeId,
        p: NodeId,
        text: NodeId,
    }

    fn build_basic() -> Basic {
        let mut dom = Dom::empty();
        let host = dom.create_element("div");
        dom.set_attribute(host, "id", "h");

        let span = dom.create_element("span");
        dom.set_attribute(span, "slot", "a");
        dom.append_child(host, span);

        let em = dom.create_element("em");
        dom.set_attribute(em, "slot", "a");
        dom.append_child(host, em);

        let p = dom.create_element("p");
        dom.append_child(host, p);

        let text = dom.create_text_node("hi");
        dom.append_child(host, text);

        let root = dom.attach_shadow(host, ShadowRootMode::Open).expect("attach should succeed");

        let named_slot = dom.create_element("slot");
        dom.set_attribute(named_slot, "name", "a");
        dom.append_child(root, named_slot);

        let default_slot = dom.create_element("slot");
        dom.append_child(root, default_slot);

        Basic { dom, host, root, named_slot, default_slot, span, em, p, text }
    }

    #[test]
    fn is_html_slot_element_checks_local_name_only() {
        let mut dom = Dom::empty();
        let slot = dom.create_element("slot");
        let div = dom.create_element("div");
        assert!(dom.is_html_slot_element(slot));
        assert!(!dom.is_html_slot_element(div));
    }

    #[test]
    fn assigned_nodes_named_slot_returns_matching_children_in_tree_order() {
        let Basic { dom, named_slot, span, em, .. } = build_basic();
        assert_eq!(dom.assigned_nodes(named_slot), Some(vec![span, em]));
    }

    #[test]
    fn assigned_nodes_default_slot_returns_unnamed_children_in_tree_order() {
        let Basic { dom, default_slot, p, text, .. } = build_basic();
        assert_eq!(dom.assigned_nodes(default_slot), Some(vec![p, text]));
    }

    #[test]
    fn assigned_slot_maps_light_children_to_their_matching_slot() {
        let Basic { dom, named_slot, default_slot, span, p, text, .. } = build_basic();
        assert_eq!(dom.assigned_slot(span), Some(named_slot));
        assert_eq!(dom.assigned_slot(p), Some(default_slot));
        assert_eq!(dom.assigned_slot(text), Some(default_slot));
    }

    #[test]
    fn assigned_slot_is_none_when_no_slot_name_matches() {
        let Basic { mut dom, host, .. } = build_basic();
        let orphan = dom.create_element("i");
        dom.set_attribute(orphan, "slot", "zzz");
        dom.append_child(host, orphan);
        assert_eq!(dom.assigned_slot(orphan), None);
    }

    #[test]
    fn first_same_name_slot_wins_a_later_duplicate_gets_nothing() {
        let Basic { mut dom, root, named_slot, span, em, .. } = build_basic();
        let second_named_slot = dom.create_element("slot");
        dom.set_attribute(second_named_slot, "name", "a");
        dom.append_child(root, second_named_slot);

        assert_eq!(dom.assigned_nodes(second_named_slot), Some(Vec::new()));
        // The first same-name slot is unaffected by the later duplicate.
        assert_eq!(dom.assigned_nodes(named_slot), Some(vec![span, em]));
    }

    #[test]
    fn slot_rendered_children_falls_back_to_own_children_when_unassigned() {
        let Basic { mut dom, root, .. } = build_basic();
        let empty_slot = dom.create_element("slot");
        dom.set_attribute(empty_slot, "name", "unused");
        dom.append_child(root, empty_slot);
        let b = dom.create_element("b");
        dom.append_child(empty_slot, b);

        assert_eq!(dom.assigned_nodes(empty_slot), Some(Vec::new()));
        assert_eq!(dom.slot_rendered_children(empty_slot), Some(vec![b]));
    }

    #[test]
    fn slot_rendered_children_uses_assigned_nodes_over_fallback_when_present() {
        let Basic { dom, named_slot, span, em, .. } = build_basic();
        // The named slot has its own no children (fallback would be empty) but IS assigned --
        // assigned nodes must win, not an empty fallback.
        assert_eq!(dom.slot_rendered_children(named_slot), Some(vec![span, em]));
    }

    #[test]
    fn comment_light_children_are_not_slottable() {
        let Basic { mut dom, host, default_slot, p, text, .. } = build_basic();
        let comment = dom.create_comment("note");
        dom.append_child(host, comment);

        assert_eq!(dom.assigned_slot(comment), None);
        // The default slot's assignment is unaffected -- the comment never participates.
        assert_eq!(dom.assigned_nodes(default_slot), Some(vec![p, text]));
    }

    #[test]
    fn assigned_nodes_none_when_not_a_slot_or_not_inside_a_shadow_tree() {
        let mut dom = Dom::empty();
        let div = dom.create_element("div");
        assert_eq!(dom.assigned_nodes(div), None);

        // A <slot> in the ordinary light tree (no shadow-root ancestor) is not "in a shadow tree".
        let light_slot = dom.create_element("slot");
        dom.append_child(div, light_slot);
        assert_eq!(dom.assigned_nodes(light_slot), None);
        assert_eq!(dom.slot_rendered_children(light_slot), None);
    }

    #[test]
    fn assigned_slot_none_when_parent_has_no_shadow_root() {
        let mut dom = Dom::empty();
        let div = dom.create_element("div");
        let span = dom.create_element("span");
        dom.append_child(div, span);
        assert_eq!(dom.assigned_slot(span), None);
    }
}
