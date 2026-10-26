//! The flat tree the page renders: a shadow host renders its SHADOW tree, not its light
//! children; a `<slot>` in that tree renders the host's light children assigned to it (or, with none, its own
//! children -- the fallback content); a light child no slot takes renders nowhere.
//!
//! The style walk and the box builder both walk this tree: an element inside a shadow root had no style and no
//! box, so a web component's host measured 0x0 whatever its shadow tree held (the A6 page, and the fixture
//! `L-shadow-host-layout`: Chrome 150 lays a 33px shadow child out inside its host).
//!
//! Slot assignment is computed once per host per walk (korvus-dom's own `assigned_nodes` walks the shadow tree
//! for every call, which a walk asking per slot would make quadratic in a component-heavy page). An arena with
//! no shadow root at all takes the plain light-tree path.

use std::collections::{HashMap, HashSet};

use korvus_dom::{Dom, NodeId, NodeKind};

/// One walk's slot assignments, filled per host as the walk reaches it.
#[derive(Default)]
pub(crate) struct FlatTree {
    /// Hosts whose assignment is below.
    hosts: HashSet<NodeId>,
    /// A slot's assigned light children, in tree order (empty: it renders its fallback).
    assigned: HashMap<NodeId, Vec<NodeId>>,
    /// A host's light child -> the slot that renders it.
    slot_of: HashMap<NodeId, NodeId>,
}

impl FlatTree {
    /// HTML's slot assignment for `host`: the FIRST slot of each name in the shadow tree's order takes every
    /// slottable light child (element or text) whose `slot` attribute names it -- `""` for the default slot.
    fn assign(&mut self, dom: &Dom, host: NodeId, root: NodeId) {
        if !self.hosts.insert(host) {
            return;
        }
        let mut by_name: HashMap<String, NodeId> = HashMap::new();
        for s in dom.descendant_elements(root) {
            if dom.is_html_slot_element(s) {
                let name = dom.slot_name(s);
                by_name.entry(name).or_insert(s);
                self.assigned.entry(s).or_default();
            }
        }
        if by_name.is_empty() {
            return;
        }
        for c in dom.children(host) {
            if !matches!(dom.node(c).kind, NodeKind::Element { .. } | NodeKind::Text(_)) {
                continue;
            }
            if let Some(&slot) = by_name.get(&dom.slottable_slot_name(c)) {
                self.assigned.entry(slot).or_default().push(c);
                self.slot_of.insert(c, slot);
            }
        }
    }

    /// The children `node` renders, in order.
    pub(crate) fn children(&mut self, dom: &Dom, node: NodeId) -> Vec<NodeId> {
        if !dom.has_shadow_roots() {
            return dom.children(node);
        }
        if let Some(root) = dom.shadow_root_of(node) {
            self.assign(dom, node, root);
            return dom.children(root);
        }
        if dom.is_html_slot_element(node) {
            let tree = dom.root_of(node);
            if let Some(host) = dom.shadow_host_of(tree) {
                self.assign(dom, host, tree);
                if let Some(a) = self.assigned.get(&node).filter(|a| !a.is_empty()) {
                    return a.clone();
                }
            }
        }
        dom.children(node)
    }

    /// The node `node` inherits from: a shadow tree's top-level node inherits from the host, a slotted light
    /// child from its slot; `None` for a light child no slot renders (and for a root).
    pub(crate) fn parent(&mut self, dom: &Dom, node: NodeId) -> Option<NodeId> {
        let p = dom.parent(node)?;
        if !dom.has_shadow_roots() {
            return Some(p);
        }
        if dom.is_shadow_root(p) {
            return dom.shadow_host_of(p);
        }
        if let Some(root) = dom.shadow_root_of(p) {
            self.assign(dom, p, root);
            return self.slot_of.get(&node).copied();
        }
        Some(p)
    }
}
