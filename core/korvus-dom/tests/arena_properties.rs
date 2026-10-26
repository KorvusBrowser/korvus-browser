//! Randomized structural properties of the arena DOM.
//!
//! The arena stores children Chrome/Blink-style as sibling links (`first_child`/`last_child` on the
//! parent, `prev_sibling`/`next_sibling` on each child). That representation has no single place
//! that "owns" a child list, so a mutator that repairs five pointers can corrupt the tree by getting
//! one of them wrong — and every existing test is example-based, so a wrong pointer only shows up if
//! someone happened to write that example.
//!
//! The regression that motivated this file was exactly of that shape: `insertBefore(n, null)` passes
//! a null reference encoded as the out-of-range sentinel `u32::MAX`, the mutator indexed the arena
//! with it, and the panic crossed an FFI frame and took the whole WPT run down with SIGABRT.
//!
//! Two properties, both driven by a deterministic PRNG so a failure is reproducible from its seed:
//!
//! 1. [`arena_invariants_hold_under_random_mutation`] — hostile input, structure-only. Random ops
//!    including null/out-of-range sentinels; after EVERY op the full link invariant set must hold.
//! 2. [`children_match_an_independent_model`] — well-formed input, behavior. The same mutators run
//!    against a plain `Vec<Vec<NodeId>>` model and must agree on child order and parentage.
//!
//! No test dependency is used: this crate deliberately carries a small dep set, and a hand-rolled
//! SplitMix64 keeps failures reproducible from a printed seed rather than a shrinker's output.
//!
//! The committed budget (256 seeds x 300 ops per property, ~0.8 s in debug) is sized to stay cheap
//! on every `cargo test`. To hunt harder, raise the two loop bounds and run `--release`: 1500 x 400
//! takes about 4 s. Both bugs recorded below were caught inside the committed budget.

use korvus_dom::{Dom, NodeId, ShadowRootMode};

// ---------------------------------------------------------------------------------------------
// Deterministic PRNG (SplitMix64) — no dev-dependency, exactly reproducible from a seed.
// ---------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform-ish in `0..n` (n > 0).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

// ---------------------------------------------------------------------------------------------
// Invariant checker — structural only, uses just the public API.
// ---------------------------------------------------------------------------------------------

/// Assert every structural invariant of the sibling-linked arena. `ctx` identifies the failing
/// step (seed + op index + the op itself) so a failure is directly reproducible.
fn check_invariants(dom: &Dom, ctx: &str) {
    let len = dom.len() as NodeId;
    let bound = |id: Option<NodeId>, what: &str| {
        if let Some(i) = id {
            assert!(i < len, "{ctx}: {what} = {i} is out of arena bounds (len {len})");
        }
    };

    // Each node may be claimed as a child by at most one parent chain.
    let mut seen_as_child: Vec<Option<NodeId>> = vec![None; len as usize];

    for id in 0..len {
        let n = dom.node(id);
        bound(n.parent, "parent");
        bound(n.first_child, "first_child");
        bound(n.last_child, "last_child");
        bound(n.prev_sibling, "prev_sibling");
        bound(n.next_sibling, "next_sibling");

        // 1. first_child and last_child are both set or both clear.
        assert_eq!(
            n.first_child.is_none(),
            n.last_child.is_none(),
            "{ctx}: node {id} has first_child {:?} but last_child {:?}",
            n.first_child,
            n.last_child
        );

        // 2. The forward chain is finite, ends at last_child, and every link is consistent.
        let mut forward = Vec::new();
        let mut cur = n.first_child;
        let mut budget = len as usize + 1;
        while let Some(c) = cur {
            assert!(budget > 0, "{ctx}: node {id}'s sibling chain does not terminate (cycle)");
            budget -= 1;

            // 3. Every node in the chain points back at this parent.
            assert_eq!(dom.parent(c), Some(id), "{ctx}: child {c} of {id} has parent {:?}", dom.parent(c));

            // 4. No node is claimed by two parents.
            if let Some(other) = seen_as_child[c as usize] {
                panic!("{ctx}: node {c} appears in the child chain of both {other} and {id}");
            }
            seen_as_child[c as usize] = Some(id);

            // 5. prev/next are mutual inverses.
            match dom.next_sibling(c) {
                Some(nx) => assert_eq!(
                    dom.previous_sibling(nx),
                    Some(c),
                    "{ctx}: next_sibling({c}) = {nx} but previous_sibling({nx}) != {c}"
                ),
                None => assert_eq!(
                    dom.last_child(id),
                    Some(c),
                    "{ctx}: {c} ends {id}'s chain but is not its last_child"
                ),
            }
            forward.push(c);
            cur = dom.next_sibling(c);
        }

        // 6. The first child has no previous sibling; the chain's ends are the recorded ends.
        if let Some(&first) = forward.first() {
            assert_eq!(dom.previous_sibling(first), None, "{ctx}: first_child {first} of {id} has a prev_sibling");
            assert_eq!(dom.first_child(id), Some(first), "{ctx}: first_child mismatch on {id}");
            assert_eq!(dom.last_child(id), forward.last().copied(), "{ctx}: last_child mismatch on {id}");
        }

        // 7. Walking backward from last_child reproduces the forward chain reversed.
        let mut backward = Vec::new();
        let mut cur = n.last_child;
        let mut budget = len as usize + 1;
        while let Some(c) = cur {
            assert!(budget > 0, "{ctx}: node {id}'s reverse sibling chain does not terminate");
            budget -= 1;
            backward.push(c);
            cur = dom.previous_sibling(c);
        }
        backward.reverse();
        assert_eq!(forward, backward, "{ctx}: forward and backward child chains of {id} disagree");

        // 8. `children()` is exactly the forward chain.
        assert_eq!(dom.children(id), forward, "{ctx}: children({id}) disagrees with its sibling chain");
    }

    // 9. A node with a parent must actually appear in that parent's chain, EXCEPT for the state
    //    `replace_child` documents: when `old_node` is not a child of `parent` it still leaves
    //    `new_node.parent = Some(parent)` while unlinked. Such a node must then be fully unlinked.
    for id in 0..len {
        let n = dom.node(id);
        if let Some(p) = n.parent {
            if seen_as_child[id as usize].is_none() {
                assert!(
                    n.prev_sibling.is_none() && n.next_sibling.is_none(),
                    "{ctx}: node {id} claims parent {p} and is absent from its chain, yet has sibling links"
                );
            } else {
                assert_eq!(seen_as_child[id as usize], Some(p), "{ctx}: node {id} parent/chain mismatch");
            }
        }
    }

    // 10. No node is its own ancestor: every parent walk terminates.
    for id in 0..len {
        let mut cur = id;
        let mut budget = len as usize + 1;
        while let Some(p) = dom.parent(cur) {
            assert!(budget > 0, "{ctx}: parent chain from {id} cycles");
            budget -= 1;
            cur = p;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Property 1 — hostile input, structure only.
// ---------------------------------------------------------------------------------------------

/// The null/absent reference marker the webapi layer passes for `insertBefore(node, null)`.
const NULL_REF: NodeId = u32::MAX;

#[test]
fn arena_invariants_hold_under_random_mutation() {
    for seed in 0..256u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x1234_5678_9ABC_DEF).wrapping_add(1));
        let mut dom = Dom::empty();
        let root = dom.root();
        let mut pool: Vec<NodeId> = vec![root];

        for step in 0..300 {
            // Only `reference` (insert_before) and `old_node` (replace_child) are documented to
            // receive an out-of-range/null marker, so those are the only params fed hostile ids.
            // `parent`/`child` always come from a real wrapped node at the webapi boundary.
            let hostile = |rng: &mut Rng, pool: &Vec<NodeId>, len: usize| -> NodeId {
                match rng.below(4) {
                    0 => NULL_REF,
                    1 => len as NodeId + rng.below(3) as NodeId,
                    _ => pool[rng.below(pool.len())],
                }
            };

            let pick = |rng: &mut Rng, pool: &Vec<NodeId>| pool[rng.below(pool.len())];
            let op = rng.below(10);
            let desc: String;

            match op {
                0 | 1 => {
                    let id = match rng.below(3) {
                        0 => dom.create_text_node("t"),
                        1 => dom.create_comment("c"),
                        _ => dom.create_element("div"),
                    };
                    pool.push(id);
                    desc = format!("create -> {id}");
                }
                2 | 3 => {
                    let (p, c) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    desc = format!("append_child({p}, {c})");
                    dom.append_child(p, c);
                }
                4 => {
                    let (p, c) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    desc = format!("prepend_child({p}, {c})");
                    dom.prepend_child(p, c);
                }
                5 | 6 => {
                    let (p, n) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    let r = hostile(&mut rng, &pool, dom.len());
                    desc = format!("insert_before({p}, {n}, {r})");
                    dom.insert_before(p, n, r);
                }
                7 => {
                    let (p, n) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    let o = hostile(&mut rng, &pool, dom.len());
                    desc = format!("replace_child({p}, {n}, {o})");
                    dom.replace_child(p, n, o);
                }
                8 => {
                    let (p, c) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    desc = format!("remove_child({p}, {c})");
                    dom.remove_child(p, c);
                }
                _ => {
                    let id = pick(&mut rng, &pool);
                    if rng.below(2) == 0 {
                        let v = format!("id{}", rng.below(4));
                        desc = format!("set_attribute({id}, id, {v})");
                        dom.set_attribute(id, "id", &v);
                    } else {
                        desc = format!("remove_attribute({id}, id)");
                        dom.remove_attribute(id, "id");
                    }
                }
            }

            check_invariants(&dom, &format!("seed {seed} step {step} [{desc}]"));
        }

        // The churned arena must still be safe for every downstream consumer.
        let ctx = format!("seed {seed} post-run");
        assert!(dom.query_selector_all("*").is_ok(), "{ctx}: query_selector_all failed");
        let _ = dom.inner_html(root);
        let _ = dom.get_element_by_id("id0");
        for id in 0..dom.len() as NodeId {
            let _ = dom.text_content(id);
            let _ = dom.node_name(id);
            let _ = dom.is_in_document(id);
        }
    }
}

/// The exact shape of the shipped regression, pinned as its own case: `insertBefore(n, null)` is
/// spec-defined as "append", and the null reference arrives as an out-of-range sentinel.
#[test]
fn null_and_out_of_range_references_append_instead_of_panicking() {
    let mut dom = Dom::empty();
    let root = dom.root();
    let a = dom.create_element("a");
    let b = dom.create_element("b");
    dom.append_child(root, a);

    dom.insert_before(root, b, NULL_REF);
    assert_eq!(dom.children(root), vec![a, b], "a null reference means append");

    let c = dom.create_element("c");
    dom.insert_before(root, c, dom.len() as NodeId + 99);
    assert_eq!(dom.children(root), vec![a, b, c], "an out-of-range reference also appends");

    // `replace_child` carries the same guard, and a non-child `old_node` is a full no-op: the DOM
    // spec throws NotFoundError *before* mutating, so nothing may be re-parented.
    let d = dom.create_element("d");
    dom.replace_child(root, d, NULL_REF);
    assert_eq!(dom.children(root), vec![a, b, c], "a null old_node replaces nothing");
    assert_eq!(dom.parent(d), None, "and leaves new_node untouched");

    check_invariants(&dom, "null-reference case");
}

#[test]
fn a_node_can_never_become_its_own_ancestor() {
    let mut dom = Dom::empty();
    let root = dom.root();
    let outer = dom.create_element("outer");
    let inner = dom.create_element("inner");
    dom.append_child(root, outer);
    dom.append_child(outer, inner);

    // Every mutator guards with `would_create_cycle`; each of these must be a no-op.
    dom.append_child(inner, outer);
    dom.prepend_child(inner, outer);
    dom.insert_before(inner, outer, NULL_REF);
    dom.replace_child(inner, outer, inner);
    dom.append_child(outer, outer);

    assert_eq!(dom.children(root), vec![outer]);
    assert_eq!(dom.children(outer), vec![inner]);
    assert_eq!(dom.children(inner), Vec::<NodeId>::new());
    check_invariants(&dom, "cycle guard");
}

/// Shadow roots are arena nodes that are deliberately never linked into any child chain, so they
/// must not disturb the light-tree invariants.
#[test]
fn shadow_roots_do_not_disturb_light_tree_invariants() {
    let mut rng = Rng::new(0xC0FFEE);
    let mut dom = Dom::empty();
    let root = dom.root();
    let mut pool = vec![root];
    let mut hosts = Vec::new();

    for step in 0..120 {
        match rng.below(4) {
            0 => {
                let el = dom.create_element("div");
                pool.push(el);
            }
            1 => {
                let (p, c) = (pool[rng.below(pool.len())], pool[rng.below(pool.len())]);
                dom.append_child(p, c);
            }
            2 => {
                let host = pool[rng.below(pool.len())];
                if let Ok(sr) = dom.attach_shadow(host, ShadowRootMode::Open) {
                    hosts.push((host, sr));
                    let inner = dom.create_element("span");
                    dom.append_child(sr, inner);
                }
            }
            _ => {
                let (p, c) = (pool[rng.below(pool.len())], pool[rng.below(pool.len())]);
                dom.remove_child(p, c);
            }
        }
        check_invariants(&dom, &format!("shadow step {step}"));
    }

    for (host, sr) in hosts {
        assert_eq!(dom.shadow_root_of(host), Some(sr));
        assert_eq!(dom.shadow_host_of(sr), Some(host));
        // A shadow root is never a child of anything in the light tree.
        assert_eq!(dom.parent(sr), None, "shadow root {sr} leaked into a light-tree child chain");
        assert!(!dom.children(host).contains(&sr));
    }
}

// ---------------------------------------------------------------------------------------------
// Property 2 — well-formed input, checked against an independent model.
// ---------------------------------------------------------------------------------------------

/// A dead-simple `Vec<Vec<NodeId>>` tree: the obvious representation the sibling-link arena is an
/// optimization of. It is the oracle — if the two disagree, the pointer surgery is wrong.
#[derive(Default)]
struct Model {
    children: Vec<Vec<NodeId>>,
    parent: Vec<Option<NodeId>>,
}

impl Model {
    fn push_node(&mut self) {
        self.children.push(Vec::new());
        self.parent.push(None);
    }
    /// Is `anc` an ancestor of, or equal to, `desc`? (mirrors `Dom::contains`)
    fn contains(&self, anc: NodeId, desc: NodeId) -> bool {
        let mut cur = Some(desc);
        while let Some(c) = cur {
            if c == anc {
                return true;
            }
            cur = self.parent[c as usize];
        }
        false
    }
    fn unlink(&mut self, c: NodeId) {
        if let Some(p) = self.parent[c as usize] {
            self.children[p as usize].retain(|&x| x != c);
        }
        self.parent[c as usize] = None;
    }
    fn append(&mut self, p: NodeId, c: NodeId) {
        if self.contains(c, p) {
            return;
        }
        self.unlink(c);
        self.children[p as usize].push(c);
        self.parent[c as usize] = Some(p);
    }
    fn prepend(&mut self, p: NodeId, c: NodeId) {
        if self.contains(c, p) {
            return;
        }
        self.unlink(c);
        self.children[p as usize].insert(0, c);
        self.parent[c as usize] = Some(p);
    }
    fn insert_before(&mut self, p: NodeId, n: NodeId, r: NodeId) {
        if self.contains(n, p) {
            return;
        }
        self.unlink(n); // matches the arena: unlink FIRST, then test the reference
        match self.children[p as usize].iter().position(|&x| x == r) {
            Some(i) => self.children[p as usize].insert(i, n),
            None => self.children[p as usize].push(n),
        }
        self.parent[n as usize] = Some(p);
    }
    fn remove(&mut self, p: NodeId, c: NodeId) {
        if self.parent[c as usize] == Some(p) {
            self.unlink(c);
        }
    }
}

#[test]
fn children_match_an_independent_model() {
    for seed in 0..256u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(7));
        let mut dom = Dom::empty();
        let mut model = Model::default();
        // `Dom::empty()` already contains the Document root at id 0.
        for _ in 0..dom.len() {
            model.push_node();
        }
        let mut pool: Vec<NodeId> = (0..dom.len() as NodeId).collect();

        for step in 0..300 {
            let pick = |rng: &mut Rng, pool: &Vec<NodeId>| pool[rng.below(pool.len())];
            let desc: String;

            match rng.below(8) {
                0 | 1 => {
                    let id = dom.create_element("div");
                    model.push_node();
                    pool.push(id);
                    desc = format!("create -> {id}");
                }
                2 | 3 => {
                    let (p, c) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    desc = format!("append_child({p}, {c})");
                    dom.append_child(p, c);
                    model.append(p, c);
                }
                4 => {
                    let (p, c) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    desc = format!("prepend_child({p}, {c})");
                    dom.prepend_child(p, c);
                    model.prepend(p, c);
                }
                5 | 6 => {
                    let (p, n) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                    // Bias toward a real child of `p` so the "insert before an actual sibling"
                    // branch is exercised, not just the append fallback.
                    let kids = dom.children(p);
                    let r = if !kids.is_empty() && rng.below(4) > 0 {
                        kids[rng.below(kids.len())]
                    } else {
                        NULL_REF
                    };
                    desc = format!("insert_before({p}, {n}, {r})");
                    dom.insert_before(p, n, r);
                    model.insert_before(p, n, r);
                }
                _ => {
                    let p = pick(&mut rng, &pool);
                    let kids = dom.children(p);
                    let c = if !kids.is_empty() && rng.below(4) > 0 {
                        kids[rng.below(kids.len())]
                    } else {
                        pick(&mut rng, &pool)
                    };
                    desc = format!("remove_child({p}, {c})");
                    dom.remove_child(p, c);
                    model.remove(p, c);
                }
            }

            let ctx = format!("seed {seed} step {step} [{desc}]");
            check_invariants(&dom, &ctx);
            for id in 0..dom.len() as NodeId {
                assert_eq!(
                    dom.children(id),
                    model.children[id as usize],
                    "{ctx}: children({id}) disagrees with the model"
                );
                assert_eq!(
                    dom.parent(id),
                    model.parent[id as usize],
                    "{ctx}: parent({id}) disagrees with the model"
                );
            }
        }
    }
}

/// The arena is append-only: `remove_child` unlinks but never reclaims a slot (`④ free-list` is a
/// documented deferral). Pinned here so the day a free-list lands, this test
/// fails and forces the decision to be explicit rather than silently changing NodeId reuse
/// semantics under every holder of a NodeId.
#[test]
fn removing_a_node_does_not_reclaim_its_arena_slot() {
    let mut dom = Dom::empty();
    let root = dom.root();
    let doomed = dom.create_element("div");
    dom.append_child(root, doomed);
    let len_before = dom.len();

    dom.remove_child(root, doomed);
    assert_eq!(dom.len(), len_before, "remove_child must not shrink the arena");

    let fresh = dom.create_element("span");
    assert_ne!(fresh, doomed, "a new node must not reuse the removed node's id");
    assert_eq!(dom.len(), len_before + 1);
}

/// Regression, found by [`arena_invariants_hold_under_random_mutation`] (seed 0, step 61).
///
/// `replace_child` documents leaving `new_node` with `parent = Some(parent)` while unlinked when
/// `old_node` is not a child of `parent`. `unlink` assumed the opposite — that a set parent implies
/// membership in that parent's chain — so unlinking such a phantom node rewrote the parent's
/// `first_child`/`last_child` to `None` and dropped every real child: they kept their parent
/// pointers (so `is_in_document` stayed true and `getElementById` kept resolving them) while
/// disappearing from `children()` and `innerHTML`.
///
/// Reachable from three lines of page script, because `Node.replaceChild` does not yet raise the
/// spec's `NotFoundError` for a non-child `old`:
/// `ul.replaceChild(x, notAChild); ul.appendChild(x);` emptied the list.
#[test]
fn unlinking_a_phantom_parent_node_does_not_drop_the_parents_children() {
    let mut dom = Dom::empty();
    let root = dom.root();
    let a = dom.create_element("a");
    let b = dom.create_element("b");
    dom.append_child(root, a);
    dom.append_child(root, b);

    let x = dom.create_element("x");
    let stray = dom.create_element("stray"); // never a child of root
    dom.replace_child(root, x, stray);

    // No phantom state: a non-child `old_node` leaves `x` completely alone.
    assert_eq!(dom.parent(x), None, "x must not claim a parent it was never linked into");
    assert_eq!(dom.children(root), vec![a, b], "replace_child must not disturb the real chain");

    // Re-parenting x unlinks it — which must leave root's real children alone.
    dom.append_child(root, x);
    assert_eq!(dom.children(root), vec![a, b, x], "a and b must survive");
    assert!(dom.is_in_document(a) && dom.is_in_document(b));
    check_invariants(&dom, "phantom-parent unlink");

    // The same hazard via every other mutator that unlinks.
    for &op in &["prepend", "insert_before", "remove"] {
        let mut dom = Dom::empty();
        let root = dom.root();
        let a = dom.create_element("a");
        dom.append_child(root, a);
        let x = dom.create_element("x");
        let stray = dom.create_element("stray");
        dom.replace_child(root, x, stray);
        match op {
            "prepend" => dom.prepend_child(root, x),
            "insert_before" => dom.insert_before(root, x, a),
            _ => dom.remove_child(root, x),
        }
        assert!(dom.children(root).contains(&a), "{op}: child `a` was dropped");
        check_invariants(&dom, &format!("phantom-parent unlink via {op}"));
    }
}

/// Regression, found by [`arena_invariants_hold_under_random_mutation`] (seed 19, step 17).
///
/// `replaceChild(n, n)` — replacing a child with itself — is a no-op per spec (the "replace"
/// algorithm re-points the reference child when it *is* the node being inserted, so `n` lands back
/// where it started). The arena instead ran the full splice: `unlink(new_node)` cleared the node's
/// links, the splice re-established them, and then the trailing "clear `old_node`" block wiped them
/// again — because `old_node` and `new_node` were the same node. The parent was left with
/// `first_child`/`last_child` pointing at a node whose `parent` was `None`.
#[test]
fn replacing_a_child_with_itself_is_a_no_op() {
    let mut dom = Dom::empty();
    let root = dom.root();
    let a = dom.create_element("a");
    let b = dom.create_element("b");
    let c = dom.create_element("c");
    for &n in &[a, b, c] {
        dom.append_child(root, n);
    }

    for &target in &[a, b, c] {
        dom.replace_child(root, target, target);
        assert_eq!(dom.children(root), vec![a, b, c], "replacing {target} with itself changed the tree");
        assert_eq!(dom.parent(target), Some(root), "{target} lost its parent pointer");
        check_invariants(&dom, &format!("self-replace of {target}"));
    }

    // The only child case exercises both the first_child and last_child branches at once.
    let mut dom = Dom::empty();
    let root = dom.root();
    let only = dom.create_element("only");
    dom.append_child(root, only);
    dom.replace_child(root, only, only);
    assert_eq!(dom.children(root), vec![only]);
    assert_eq!(dom.parent(only), Some(root));
    check_invariants(&dom, "self-replace of an only child");
}
