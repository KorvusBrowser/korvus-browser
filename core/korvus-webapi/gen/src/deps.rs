//! Interface dependency graph (for the migration's implementation order + parallelism).
//!
//! There are TWO kinds of edge here and conflating them destroys the order.
//!
//! **Hard (inheritance).** `Element : Node` means Element's prototype chain, and its every inherited
//! member, is Node's. You cannot finish Element before Node. WebIDL forbids an inheritance cycle, so
//! these edges alone are a forest, and [`DepGraph::implementation_layers`] turns them into the
//! layer order an implementer actually follows: EventTarget, then Node, then Element, then
//! HTMLElement, then HTMLInputElement.
//!
//! **Soft (type reference).** `Document.createElement()` returns an Element and `Node.ownerDocument`
//! is a Document. Those are real relationships but NOT ordering constraints: `createElement` can be
//! written and graded long before Element is finished. Counted as dependencies they also make the
//! graph hopelessly cyclic -- Document -> Element -> Node -> Document -- and measured on the real
//! corpus that is not a corner case: [`DepGraph::layers`] puts 529 of 940 interfaces, including
//! EventTarget, Event, Node, Element, Document, Range and every SVG element, into ONE "migrate
//! together" layer. An order that says "do 529 things at once" is not an order.
//!
//! So soft edges are kept, but for a different job: [`DepGraph::dependents_of`] answers "what could
//! this change have broken", which is what a campaign re-checks after finishing a layer.

use std::collections::{BTreeMap, BTreeSet};

use crate::spec::{InterfaceSpec, MemberSpec};

/// `iface -> the interfaces it depends on` (only edges to interfaces present in the parsed set).
#[derive(Debug, Default)]
pub struct DepGraph {
    /// Every edge, hard and soft together. Cyclic in practice; see the module note.
    pub deps: BTreeMap<String, BTreeSet<String>>,
    /// The hard edges alone: `iface -> the interface it inherits`, when that parent was parsed too.
    pub inherits: BTreeMap<String, String>,
}

impl DepGraph {
    /// Build the graph from a set of interface specs. Non-interface type references (primitives,
    /// dictionaries, `sequence`, unions of strings, ...) are ignored — only edges to other parsed
    /// interfaces count.
    pub fn build(specs: &[InterfaceSpec]) -> DepGraph {
        let names: BTreeSet<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut inherits: BTreeMap<String, String> = BTreeMap::new();
        for s in specs {
            let set = deps.entry(s.name.clone()).or_default();
            if let Some(parent) = &s.inherits {
                if names.contains(parent.as_str()) && parent != &s.name {
                    set.insert(parent.clone());
                    inherits.insert(s.name.clone(), parent.clone());
                }
            }
            for m in &s.members {
                for ty in member_types(m) {
                    for token in interface_tokens(&ty, &names) {
                        if token != s.name {
                            set.insert(token);
                        }
                    }
                }
            }
        }
        DepGraph { deps, inherits }
    }

    /// Topological layers (Kahn): layer 0 = interfaces with no in-set deps, layer k depends only on
    /// layers < k. Each layer is an independent set (parallelizable). **Cycle-tolerant:** a
    /// base/derived pair often cycles (a base method returns the derived type while the derived
    /// inherits the base), so when no node is ready the remaining strongly-connected nodes are
    /// emitted as one layer (migrate them together) — `cyclic` lists those, for the caller's note.
    pub fn layers(&self) -> Layered {
        let mut remaining: BTreeMap<&str, BTreeSet<&str>> = self
            .deps
            .iter()
            .map(|(k, v)| (k.as_str(), v.iter().map(String::as_str).collect()))
            .collect();
        let mut layers = Vec::new();
        let mut cyclic = Vec::new();
        while !remaining.is_empty() {
            let mut ready: Vec<String> = remaining
                .iter()
                .filter(|(_, d)| d.iter().all(|dep| !remaining.contains_key(dep)))
                .map(|(k, _)| k.to_string())
                .collect();
            if ready.is_empty() {
                // A cycle remains: emit everything left as one "migrate together" layer.
                ready = remaining.keys().map(|k| k.to_string()).collect();
                ready.sort();
                cyclic = ready.clone();
                layers.push(ready);
                break;
            }
            ready.sort();
            for r in &ready {
                remaining.remove(r.as_str());
            }
            layers.push(ready);
        }
        Layered { layers, cyclic }
    }
}

impl DepGraph {
    /// The implementation order: layers by INHERITANCE depth alone, roots first.
    ///
    /// Layer 0 is every interface that inherits nothing (or whose parent was not parsed); layer k
    /// inherits from layer k-1. WebIDL forbids inheritance cycles, so this always terminates and
    /// every interface lands exactly once -- no "migrate together" escape hatch is needed, unlike
    /// [`layers`](Self::layers).
    ///
    /// Members of one layer never inherit from each other, which is exactly the property a parallel
    /// campaign needs: they can be worked at the same time without one's prototype chain waiting on
    /// another's.
    pub fn implementation_layers(&self) -> Vec<Vec<String>> {
        let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
        let all: BTreeSet<&str> = self
            .deps
            .keys()
            .map(String::as_str)
            .chain(self.inherits.keys().map(String::as_str))
            .collect();
        for name in &all {
            // Walk to the root, remembering the path, then fill the whole chain in one pass. `seen`
            // is belt-and-braces: WebIDL forbids an inheritance cycle, but a corpus is not a compiler
            // and a self-referential declaration must not hang the campaign that consumes this.
            let mut chain: Vec<&str> = Vec::new();
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            let mut cur = *name;
            // `start` is the depth the SHALLOWEST unresolved node gets. The two exits differ by one:
            // hitting a known depth means `cur` is the parent of the chain (so the chain starts one
            // below it), while running out of parents means `cur` is IN the chain and is itself a root.
            let start = loop {
                if let Some(d) = depth.get(cur) {
                    break d + 1;
                }
                if !seen.insert(cur) {
                    break 0; // cycle: treat where we re-entered as a root
                }
                chain.push(cur);
                match self.inherits.get(cur).map(String::as_str) {
                    Some(p) if all.contains(p) => cur = p,
                    _ => break 0,
                }
            };
            for (i, n) in chain.iter().rev().enumerate() {
                depth.entry(n).or_insert(start + i);
            }
        }
        let mut out: Vec<Vec<String>> = Vec::new();
        for (name, d) in depth {
            if out.len() <= d {
                out.resize(d + 1, Vec::new());
            }
            out[d].push(name.to_string());
        }
        for layer in &mut out {
            layer.sort();
        }
        out
    }

    /// Everything that references `iface` by any edge, hard or soft -- the blast radius of a change.
    ///
    /// This is the soft edges' real job. `Element` is referenced by `Document`, `Range`, `MutationRecord`
    /// and a hundred others; finishing Element is exactly when those are worth re-grading, and a campaign
    /// that re-grades everything instead pays a full sweep per layer for no extra information.
    pub fn dependents_of(&self, iface: &str) -> BTreeSet<String> {
        self.deps
            .iter()
            .filter(|(k, v)| k.as_str() != iface && v.iter().any(|d| d == iface))
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// The result of [`DepGraph::layers`]: ordered layers (leaves first) plus any interfaces that could
/// only be placed by breaking a cycle (they share the final layer and should be migrated together).
#[derive(Debug)]
pub struct Layered {
    pub layers: Vec<Vec<String>>,
    pub cyclic: Vec<String>,
}

/// The WebIDL type strings a member references (attribute type, return type, argument types).
fn member_types(m: &MemberSpec) -> Vec<String> {
    match m {
        MemberSpec::Constructor { args, .. } => args.iter().map(|a| a.ty.clone()).collect(),
        MemberSpec::Attribute { ty, .. } => vec![ty.clone()],
        MemberSpec::Operation { ret, args, .. } => {
            let mut v = vec![ret.clone()];
            v.extend(args.iter().map(|a| a.ty.clone()));
            v
        }
        MemberSpec::Const { ty, .. } => vec![ty.clone()],
        // A special's types are the machinery's own (DOMString keys/values) — no interface edges.
        MemberSpec::Special { .. } => Vec::new(),
    }
}

/// Identifier tokens in a type string that name a known interface. Tokenizes on non-identifier
/// characters so `sequence<DOMPoint>` -> {DOMPoint} and `(DOMString or sequence<double>)` -> {} (no
/// interface). Word-boundary safe: `DOMPointReadOnly` never matches `DOMPoint`.
fn interface_tokens(ty: &str, names: &BTreeSet<&str>) -> Vec<String> {
    ty.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| names.contains(t))
        .map(|t| t.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webidl;

    const GEOMETRY: &str = include_str!("../idl/geometry.idl");

    /// Geometry's edges: DOMPoint inherits DOMPointReadOnly; DOMMatrixReadOnly.transformPoint returns
    /// DOMPoint; DOMRect inherits DOMRectReadOnly. The rect family is acyclic (the read-only base has
    /// no interface deps); the point/matrix families cycle (a base method returns the derived type
    /// while the derived inherits the base) — layers() groups those instead of dropping them.
    #[test]
    fn builds_geometry_edges_and_layers() {
        let specs = webidl::parse(GEOMETRY).expect("parse");
        let g = DepGraph::build(&specs);

        assert!(g.deps["DOMPoint"].contains("DOMPointReadOnly"), "DOMPoint inherits DOMPointReadOnly");
        assert!(g.deps["DOMMatrixReadOnly"].contains("DOMPoint"), "DOMMatrixReadOnly.transformPoint -> DOMPoint");
        assert!(g.deps["DOMRect"].contains("DOMRectReadOnly"), "DOMRect inherits DOMRectReadOnly");

        let l = g.layers();
        let layer_of = |iface: &str| l.layers.iter().position(|x| x.iter().any(|n| n == iface));
        assert!(layer_of("DOMRectReadOnly").unwrap() < layer_of("DOMRect").unwrap(), "base before derived (rect)");
        assert!(!l.cyclic.is_empty(), "point/matrix base<->derived return-type cycles are detected");
        assert!(l.cyclic.iter().any(|n| n == "DOMPoint" || n == "DOMMatrix"), "cyclic set holds the point/matrix families");
        let placed: usize = l.layers.iter().map(|x| x.len()).sum();
        assert_eq!(placed, specs.len(), "every interface is placed somewhere");
    }

    /// The order an implementer actually follows. Type-reference edges make this cyclic (Document
    /// returns Element, Element inherits Node, Node returns Document) and `layers()` then dumps the
    /// whole DOM into one "migrate together" bucket; inheritance alone keeps the spine.
    #[test]
    fn implementation_layers_follow_the_prototype_chain() {
        let idl = r#"
            interface EventTarget { undefined addEventListener(DOMString t); };
            interface Node : EventTarget { readonly attribute Document ownerDocument; };
            interface Document : Node { Element createElement(DOMString t); };
            interface Element : Node { readonly attribute Document doc; };
            interface HTMLElement : Element { attribute DOMString title; };
            interface HTMLInputElement : HTMLElement { attribute DOMString value; };
        "#;
        let specs = webidl::parse(idl).expect("parse");
        let g = DepGraph::build(&specs);

        // The type-reference graph really is cyclic here -- that is the situation this exists for.
        assert!(!g.layers().cyclic.is_empty(), "Document <-> Element <-> Node cycle via return types");

        let l = g.implementation_layers();
        let depth = |n: &str| l.iter().position(|x| x.iter().any(|m| m == n)).expect(n);
        assert_eq!(depth("EventTarget"), 0);
        assert_eq!(depth("Node"), 1);
        assert_eq!(depth("Element"), 2);
        assert_eq!(depth("Document"), 2, "a sibling of Element, not behind it");
        assert_eq!(depth("HTMLElement"), 3);
        assert_eq!(depth("HTMLInputElement"), 4);
        let placed: usize = l.iter().map(|x| x.len()).sum();
        assert_eq!(placed, specs.len(), "every interface is placed exactly once");
    }

    /// A layer must be an INDEPENDENT set under inheritance -- that is what licenses running it in
    /// parallel. Element and Document share layer 2 and neither inherits the other.
    #[test]
    fn a_layer_never_contains_a_parent_of_its_own_member() {
        let idl = r#"
            interface EventTarget { };
            interface Node : EventTarget { };
            interface Document : Node { };
            interface Element : Node { };
        "#;
        let g = DepGraph::build(&webidl::parse(idl).expect("parse"));
        for layer in g.implementation_layers() {
            for n in &layer {
                if let Some(p) = g.inherits.get(n) {
                    assert!(!layer.contains(p), "{n} shares a layer with its parent {p}");
                }
            }
        }
    }

    /// The blast radius of a change: who has to be re-graded once `Node` moves.
    #[test]
    fn dependents_are_the_reverse_of_every_edge() {
        let idl = r#"
            interface Node { };
            interface Element : Node { };
            interface Range { undefined selectNode(Node n); };
            interface Blob { };
        "#;
        let g = DepGraph::build(&webidl::parse(idl).expect("parse"));
        let d = g.dependents_of("Node");
        assert!(d.contains("Element"), "inherits it");
        assert!(d.contains("Range"), "takes it as an argument");
        assert!(!d.contains("Blob"));
        assert!(!d.contains("Node"), "never itself");
    }

    /// A word-boundary check: a type mentioning `DOMPointReadOnly` must not create an edge to
    /// `DOMPoint` by substring.
    #[test]
    fn tokenization_is_word_boundary_safe() {
        let names: BTreeSet<&str> = ["DOMPoint", "DOMPointReadOnly"].into_iter().collect();
        assert_eq!(interface_tokens("sequence<DOMPointReadOnly>", &names), vec!["DOMPointReadOnly"]);
        assert_eq!(interface_tokens("DOMPoint", &names), vec!["DOMPoint"]);
    }
}
