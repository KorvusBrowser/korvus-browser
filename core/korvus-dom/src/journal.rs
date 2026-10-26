//! What changed, node by node, for the passes that keep their results between runs.
//!
//! [`Dom::revision`] says THAT something changed; a style pass that wants to redo only what a change
//! can reach has to know WHAT changed and WHERE. Every mutation a document can see appends one
//! [`Change`] here as it happens, and a pass that keeps its results reads the entries since its own
//! [`Journal::seq`] and works out from them which elements to style again (`korvus-page`'s
//! invalidation). The revision counters stay as they were, for the caches keyed on them.
//!
//! Each entry names its mutation, and the reader names the pass that consumes it: the journal itself
//! is never cleared by a reader, because the page's style pass, a frame's and the animation scan each
//! read it from where they last stopped. It is bounded: past [`CAPACITY`] entries the oldest half is
//! dropped, and a reader whose position fell inside the dropped part ([`Journal::since`] answers
//! `None`) starts over from nothing, which is always correct.
//!
//! A write that the journal's last entry already names folds into it while no reader has been given a
//! position past that entry (`Journal::read`): the attribute written again, the text node changed
//! again, the children replaced again. A reader works out what changed from the entry and the DOM as
//! it is, so what it needs is the state before the first write, which the entry keeps. A loop of writes
//! between two reads costs one entry, not one per write (the per-call cost of `setAttribute`,
//! `className` and `textContent` went up 5-11 % with an entry per call).
//!
//! An entry is a plain value (`Copy`): what an entry holds beyond node numbers -- an attribute's name,
//! the value a `class` or `id` had, the nodes a replacement unlinked -- is kept beside the entries and
//! read through [`Journal::name`], [`Journal::kept`] and [`Journal::nodes`]. Dropping the oldest half
//! is then a move of plain values, not a drop of each entry, and a push copies 32 bytes, not 56
//! (an `appendChild`'s two entries cost 113 instructions at the DOM with owned entries).

use std::cell::Cell;

use crate::{FxHashMap, NodeId};

/// An attribute name the journal holds, by number ([`Journal::name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Name(u32);

/// The value an attribute had before a `class` or `id` change ([`Journal::kept`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kept(u64);

/// Nodes the journal holds for a change ([`Journal::nodes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nodes {
    start: u64,
    len: u32,
}

/// One mutation, as the style pass needs to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// An attribute of `node` was added, changed or removed. `old` is the value it had, kept only for
    /// `class` and `id` -- the two whose OLD tokens decide what the change can reach -- and `None`
    /// when it was absent.
    Attribute { node: NodeId, name: Name, old: Option<Kept> },
    /// `node` was linked into `parent`'s children.
    Inserted { parent: NodeId, node: NodeId },
    /// `node` was unlinked from `parent`; `next` was its next sibling at that moment.
    Removed { parent: NodeId, node: NodeId, next: Option<NodeId> },
    /// `parent`'s children were replaced all at once (`innerHTML`, `textContent`): `old` were unlinked
    /// without an entry each, and the new ones were linked with one.
    ChildrenReplaced { parent: NodeId, old: Nodes },
    /// The data of the text-like node `node` changed.
    Text { node: NodeId },
    /// A form control's state that is not an attribute changed (value, checkedness, a pattern's
    /// answer): `None` when it is not one control's (a form reset, a pattern answer).
    State { node: Option<NodeId> },
    /// The hover chain changed; `old` is the chain it replaced.
    Hover { old: Nodes },
    /// Anything else a document can see (a namespace rewrite, a shadow root attached, a document mode):
    /// a reader starts over.
    Other { node: Option<NodeId>, what: &'static str },
}

/// `a == b`, without a call for the short names attributes have: two loads from each end of a name of
/// 4 to 16 bytes cover all of it.
#[inline]
fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let n = a.len();
    if n != b.len() {
        return false;
    }
    let word = |s: &[u8], at: usize| u64::from_le_bytes(s[at..at + 8].try_into().unwrap());
    let half = |s: &[u8], at: usize| u32::from_le_bytes(s[at..at + 4].try_into().unwrap());
    match n {
        8..=16 => word(a, 0) == word(b, 0) && word(a, n - 8) == word(b, n - 8),
        4..=7 => half(a, 0) == half(b, 0) && half(a, n - 4) == half(b, n - 4),
        _ => a == b,
    }
}

/// The bound on the entries kept: a page that mutates this much between two style passes is styled
/// from nothing by the next one.
pub const CAPACITY: usize = 1 << 16;

/// The entries, and the position of the first one kept.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    /// The sequence number of `entries[0]`.
    base: u64,
    entries: Vec<Change>,
    /// The furthest position a reader has been given ([`Journal::seq`], [`Journal::since`]): an entry at
    /// or after it is one no reader has passed, and a write it names folds into it.
    read: Cell<u64>,
    /// Every attribute name an entry has named, and its number: a page writes few names many times.
    names: Vec<Box<str>>,
    name_ids: FxHashMap<Box<str>, u32>,
    /// The values `Kept` numbers, from `kept_base` on.
    kept: Vec<String>,
    kept_base: u64,
    /// The nodes `Nodes` spans, from `nodes_base` on.
    nodes: Vec<NodeId>,
    nodes_base: u64,
    /// The entries that hold a value or nodes beside them, oldest first: `(sequence number, where its
    /// own start)` -- what [`Journal::drop_oldest`] reads to drop what only dropped entries held,
    /// without walking the entries.
    kept_at: std::collections::VecDeque<(u64, u64)>,
    nodes_at: std::collections::VecDeque<(u64, u64)>,
}

impl Journal {
    #[inline]
    pub(crate) fn push(&mut self, change: Change) {
        if self.entries.len() >= CAPACITY {
            self.drop_oldest();
        }
        self.entries.push(change);
    }

    /// Drop the oldest half of the entries, and what only they held beside them.
    #[cold]
    #[inline(never)]
    fn drop_oldest(&mut self) {
        let drop = CAPACITY / 2;
        self.entries.drain(..drop);
        self.base += drop as u64;
        // Side values are added in entry order: the first entry left that holds one says where they
        // are still needed from.
        let base = self.base;
        while self.kept_at.front().is_some_and(|&(seq, _)| seq < base) {
            self.kept_at.pop_front();
        }
        let kept_from = self.kept_at.front().map_or(self.kept_base + self.kept.len() as u64, |&(_, at)| at);
        self.kept.drain(..(kept_from - self.kept_base) as usize);
        self.kept_base = kept_from;
        while self.nodes_at.front().is_some_and(|&(seq, _)| seq < base) {
            self.nodes_at.pop_front();
        }
        let nodes_from = self.nodes_at.front().map_or(self.nodes_base + self.nodes.len() as u64, |&(_, at)| at);
        self.nodes.drain(..(nodes_from - self.nodes_base) as usize);
        self.nodes_base = nodes_from;
    }

    fn end(&self) -> u64 {
        self.base + self.entries.len() as u64
    }

    /// The last `n` entries, when no reader has been given a position past the first of them.
    fn unread_tail(&self, n: usize) -> Option<&[Change]> {
        let first = self.end().checked_sub(n as u64)?;
        (first >= self.read.get() && first >= self.base).then(|| &self.entries[self.entries.len() - n..])
    }

    /// An attribute name's number.
    fn intern(&mut self, name: &str) -> Name {
        if let Some(&id) = self.name_ids.get(name) {
            return Name(id);
        }
        let id = self.names.len() as u32;
        self.names.push(name.into());
        self.name_ids.insert(name.into(), id);
        Name(id)
    }

    /// Push an entry that holds `nodes` beside it.
    fn push_with_nodes(&mut self, nodes: &[NodeId], change: impl FnOnce(Nodes) -> Change) {
        if self.entries.len() >= CAPACITY {
            self.drop_oldest();
        }
        let start = self.nodes_base + self.nodes.len() as u64;
        self.nodes.extend_from_slice(nodes);
        self.nodes_at.push_back((self.end(), start));
        self.entries.push(change(Nodes { start, len: nodes.len() as u32 }));
    }

    /// `node`'s attribute `name` was written: one entry, unless the last one is the same attribute's and
    /// unread -- it keeps the value from before the first write. `old` is asked only for a new entry.
    #[inline]
    pub(crate) fn push_attribute(&mut self, node: NodeId, name: &str, old: impl FnOnce() -> Option<String>) {
        if self.end() > self.read.get() {
            if let Some(Change::Attribute { node: n, name: k, .. }) = self.entries.last() {
                if *n == node && same_name(&self.names[k.0 as usize], name) {
                    return;
                }
            }
        }
        let name = self.intern(name);
        if self.entries.len() >= CAPACITY {
            self.drop_oldest();
        }
        let old = old().map(|v| {
            let at = self.kept_base + self.kept.len() as u64;
            self.kept.push(v);
            self.kept_at.push_back((self.end(), at));
            Kept(at)
        });
        self.entries.push(Change::Attribute { node, name, old });
    }

    /// The data of the text-like node `node` changed: one entry, unless the last one is its and unread.
    pub(crate) fn push_text(&mut self, node: NodeId) {
        if matches!(self.unread_tail(1), Some([Change::Text { node: n }]) if *n == node) {
            return;
        }
        self.push(Change::Text { node });
    }

    /// `parent`'s children were replaced; `old` are the ones unlinked.
    pub(crate) fn push_replaced(&mut self, parent: NodeId, old: &[NodeId]) {
        self.push_with_nodes(old, |old| Change::ChildrenReplaced { parent, old });
    }

    /// The hover chain changed; `old` is the chain it replaced.
    pub(crate) fn push_hover(&mut self, old: &[NodeId]) {
        self.push_with_nodes(old, |old| Change::Hover { old });
    }

    /// Will replacing `parent`'s children fold into the entries already there, unread? Then the caller
    /// records nothing more: the entry that is kept names the children from before the first
    /// replacement, and the new child the last one linked (`Inserted`) -- now unlinked again, never seen
    /// by a reader -- is dropped. `textContent` written in a loop.
    pub(crate) fn fold_children_replaced(&mut self, parent: NodeId) -> bool {
        match self.unread_tail(2) {
            Some([Change::ChildrenReplaced { parent: p, .. }, Change::Inserted { parent: q, .. }]) if *p == parent && *q == parent => {
                self.entries.pop();
                return true;
            }
            _ => {}
        }
        matches!(self.unread_tail(1), Some([Change::ChildrenReplaced { parent: p, .. }]) if *p == parent)
    }

    /// The position after the last entry: what a reader that has seen everything stores.
    pub fn seq(&self) -> u64 {
        let end = self.end();
        self.read.set(self.read.get().max(end));
        end
    }

    /// The entries since `seq`, or `None` when some of them were dropped.
    pub fn since(&self, seq: u64) -> Option<&[Change]> {
        self.read.set(self.read.get().max(self.end()));
        let start = seq.checked_sub(self.base)? as usize;
        self.entries.get(start..)
    }

    /// The attribute name an entry names.
    pub fn name(&self, name: Name) -> &str {
        &self.names[name.0 as usize]
    }

    /// The value an entry kept. Valid for an entry [`Journal::since`] gave.
    pub fn kept(&self, kept: Kept) -> &str {
        &self.kept[(kept.0 - self.kept_base) as usize]
    }

    /// The nodes an entry holds. Valid for an entry [`Journal::since`] gave.
    pub fn nodes(&self, nodes: Nodes) -> &[NodeId] {
        let start = (nodes.start - self.nodes_base) as usize;
        &self.nodes[start..start + nodes.len as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dom;

    /// An entry with what it holds beside it, for comparing.
    #[derive(Debug, PartialEq)]
    enum Seen {
        Attribute { node: NodeId, name: String, old: Option<String> },
        Inserted { parent: NodeId, node: NodeId },
        Removed { parent: NodeId, node: NodeId, next: Option<NodeId> },
        ChildrenReplaced { parent: NodeId, old: Vec<NodeId> },
        Text { node: NodeId },
        Hover { old: Vec<NodeId> },
        Other,
    }

    fn seen(j: &Journal, c: &Change) -> Seen {
        match *c {
            Change::Attribute { node, name, old } => Seen::Attribute { node, name: j.name(name).into(), old: old.map(|k| j.kept(k).into()) },
            Change::Inserted { parent, node } => Seen::Inserted { parent, node },
            Change::Removed { parent, node, next } => Seen::Removed { parent, node, next },
            Change::ChildrenReplaced { parent, old } => Seen::ChildrenReplaced { parent, old: j.nodes(old).to_vec() },
            Change::Text { node } => Seen::Text { node },
            Change::Hover { old } => Seen::Hover { old: j.nodes(old).to_vec() },
            Change::State { .. } | Change::Other { .. } => Seen::Other,
        }
    }

    fn since(dom: &Dom, seq: u64) -> Vec<Seen> {
        let j = dom.journal();
        j.since(seq).expect("nothing dropped").iter().map(|c| seen(j, c)).collect()
    }

    #[test]
    fn each_mutation_a_document_sees_is_one_entry_naming_it() {
        let mut dom = Dom::parse("<html><body><div id='a' class='x y'>t</div></body></html>");
        let a = dom.get_element_by_id("a").unwrap();
        let text = dom.first_child(a).unwrap();
        let body = dom.parent(a).unwrap();
        let start = dom.journal().seq();

        dom.set_attribute(a, "class", "y z");
        dom.set_attribute(a, "class", "y z");
        dom.set_attribute(a, "title", "t");
        dom.remove_attribute(a, "title");
        dom.remove_attribute(a, "title");
        assert_eq!(
            since(&dom, start),
            vec![
                Seen::Attribute { node: a, name: "class".into(), old: Some("x y".into()) },
                Seen::Attribute { node: a, name: "title".into(), old: None },
            ],
            "a write that leaves the value as it was, and removing what is absent, change nothing; \
             removing `title` folds into its unread entry"
        );

        let start = dom.journal().seq();
        dom.set_node_value(text, "u");
        let b = dom.create_element("b");
        dom.append_child(body, b);
        dom.remove_child(body, b);
        assert_eq!(
            since(&dom, start),
            vec![
                Seen::Text { node: text },
                Seen::Inserted { parent: body, node: b },
                Seen::Removed { parent: body, node: b, next: None },
            ]
        );

        let start = dom.journal().seq();
        dom.set_text_content(a, "v");
        let new_text = dom.first_child(a).unwrap();
        assert_eq!(
            since(&dom, start),
            vec![Seen::ChildrenReplaced { parent: a, old: vec![text] }, Seen::Inserted { parent: a, node: new_text }]
        );

        let start = dom.journal().seq();
        dom.set_hover_chain(vec![body, a]);
        assert_eq!(since(&dom, start), vec![Seen::Hover { old: vec![] }]);
    }

    /// A write the last entry already names folds into it while no reader has passed it, and
    /// the entry keeps the state from before the first write; a reader's position fences it.
    #[test]
    fn a_write_the_last_unread_entry_names_folds_into_it() {
        let mut dom = Dom::parse("<html><body><div id='a' class='x'>t</div></body></html>");
        let a = dom.get_element_by_id("a").unwrap();
        let text = dom.first_child(a).unwrap();
        let start = dom.journal().seq();
        dom.set_attribute(a, "class", "y");
        dom.set_attribute(a, "class", "z");
        dom.set_attribute(a, "title", "1");
        dom.set_attribute(a, "title", "2");
        dom.set_node_value(text, "u");
        dom.set_node_value(text, "v");
        assert_eq!(
            since(&dom, start),
            vec![
                Seen::Attribute { node: a, name: "class".into(), old: Some("x".into()) },
                Seen::Attribute { node: a, name: "title".into(), old: None },
                Seen::Text { node: text },
            ]
        );
        // A reader was given the position after those: the next write is an entry of its own.
        let mid = dom.journal().seq();
        dom.set_node_value(text, "w");
        dom.set_attribute(a, "title", "3");
        dom.set_attribute(a, "class", "q");
        dom.set_attribute(a, "class", "r");
        assert_eq!(
            since(&dom, mid),
            vec![
                Seen::Text { node: text },
                Seen::Attribute { node: a, name: "title".into(), old: None },
                Seen::Attribute { node: a, name: "class".into(), old: Some("z".into()) },
            ]
        );
        // `textContent` in a loop: the first replacement, with the children from before it, and the
        // last child linked.
        let start = dom.journal().seq();
        dom.set_text_content(a, "1");
        dom.set_text_content(a, "2");
        dom.set_text_content(a, "3");
        let last = dom.first_child(a).unwrap();
        assert_eq!(
            since(&dom, start),
            vec![Seen::ChildrenReplaced { parent: a, old: vec![text] }, Seen::Inserted { parent: a, node: last }]
        );
    }

    /// What an entry holds beside it survives the oldest half being dropped while the entry stays, and
    /// goes with it.
    #[test]
    fn values_and_nodes_beside_the_entries_follow_them_through_the_bound() {
        let mut j = Journal::default();
        let text = |j: &mut Journal, n: usize| (0..n).for_each(|i| j.push(Change::Text { node: i as u32 }));
        // An old value and old nodes in the half that goes, and in the half that stays.
        j.push_attribute(1, "class", || Some("gone".into()));
        j.push_replaced(2, &[7, 8]);
        text(&mut j, CAPACITY / 2);
        let stay = j.end();
        j.push_attribute(3, "id", || Some("kept".into()));
        j.push_replaced(4, &[9]);
        j.push_hover(&[5, 6]);
        text(&mut j, CAPACITY / 2);
        assert!(j.since(0).is_none(), "the oldest half was dropped");
        let seen: Vec<Seen> = j.since(stay).unwrap().iter().take(3).map(|c| seen(&j, c)).collect();
        assert_eq!(
            seen,
            vec![
                Seen::Attribute { node: 3, name: "id".into(), old: Some("kept".into()) },
                Seen::ChildrenReplaced { parent: 4, old: vec![9] },
                Seen::Hover { old: vec![5, 6] },
            ]
        );
        assert_eq!((j.kept.len(), j.nodes.len()), (1, 3), "what only dropped entries held is gone");
    }

    #[test]
    fn same_name_is_string_equality() {
        let names = ["", "a", "id", "src", "href", "class", "data-y", "data-yy", "data-yyy", "data-testid", "aria-labelledby", "data-sixteen-ch", "data-seventeen-c", "a".repeat(40).leak()];
        for x in names {
            for y in names {
                assert_eq!(same_name(x, y), x == y, "{x:?} {y:?}");
            }
            // One byte different anywhere.
            for i in 0..x.len() {
                let mut z = x.as_bytes().to_vec();
                z[i] ^= 1;
                let z = String::from_utf8(z).unwrap();
                assert!(!same_name(x, &z), "{x:?} {z:?}");
            }
        }
    }

    #[test]
    fn a_reader_that_fell_behind_the_bound_is_told_to_start_over() {
        let mut j = Journal::default();
        for n in 0..CAPACITY as u32 + 1 {
            j.push(Change::Text { node: n });
        }
        assert_eq!(j.seq(), CAPACITY as u64 + 1);
        assert!(j.since(0).is_none(), "the oldest half was dropped");
        assert_eq!(j.since(j.seq() - 1).unwrap(), &[Change::Text { node: CAPACITY as u32 }]);
        assert_eq!(j.since(j.seq()).unwrap(), &[] as &[Change]);
        assert!(j.since(j.seq() + 1).is_none(), "a position this journal never had");
    }
}
