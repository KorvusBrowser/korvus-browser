//! Which rules apply to an element.
//!
//! # The shape of the problem
//!
//! Styling a document is, naively, `nodes × rules` selector tests, each one a walk up the tree.
//! A 5,000-node page with 3,000 rules is 15 million of them, which is not slow — it is a page that
//! never finishes. So the work has to be cut before it starts, and [`RuleIndex`] is where: an
//! element only ever sees the rules keyed on its tag, its id, its classes, and the universal
//! residue. Every other rule is skipped without being looked at.
//!
//! That is not enough on its own. A stylesheet where thousands of rules share a rightmost key --
//! `.section-1 .row .cell a`, `.section-2 .row .cell a`, … -- hands every `<a>` all of them, and
//! each costs a walk up the tree to find out it does not match. Measured on a 10,000-node page with
//! 3,000 such rules: 655ms, with zero matches. A bloom filter over the element's ancestors rejects
//! those in constant time, which is why [`AncestorBloom`] is built per element here rather than
//! left to the caller.
//!
//! # What comes out
//!
//! Matched *rules*, not a resolved style. The cascade is a separate step because a rule can carry
//! declarations at two different precedences at once (`color: red; width: 1px !important`), so
//! there is no single rank to sort rules by — only declarations. Matching answers "which", the
//! cascade answers "which wins".

use crate::style::media::MediaContext;
use crate::style::container::ContainerCondition;
use crate::style::rules::{ElementKeys, RuleIndex, RuleRef, ScopeInfo, Stylesheet};
use korvus_dom::{AncestorBloom, Dom, NodeId, NodeKind, Selector};
use std::collections::HashMap;

/// A rule that applies to an element, with the specificity of the selector that made it apply.
///
/// The specificity is not the rule's — a rule has as many as it has selectors. `h1, .lede` matching
/// an `<h1 class="lede">` matches through both, and the cascade uses the higher one, because that
/// is the selector a browser would say won.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedRule {
    pub sheet: u32,
    /// Index into the sheet's [`Stylesheet::cascade_rules`].
    pub rule: u32,
    pub specificity: u32,
    /// The rule's cascade layer as a document-wide rank: a later-declared layer ranks higher, and
    /// unlayered rules rank above every layer. See [`Matcher::layer_rank`].
    pub layer: u32,
    /// `@scope` proximity (CSS Cascade 6 §6.6): `u32::MAX - hops` to the scoping root, and `0` for a
    /// rule in no scope. Higher wins, after layers and before specificity.
    pub proximity: u32,
    /// How many shadow boundaries in the rule's tree is from the element's: `0` for a
    /// rule of the element's own tree, `1` for a host's `:host` rule and for a slotted element's
    /// `::slotted` rule, one more per slot that slot is itself assigned to. CSS Cascade 5 §6.1
    /// compares this CONTEXT right after origin and importance: the outer tree wins a normal
    /// declaration and the inner one wins an `!important` one.
    pub context: u32,
    /// The sheet's place in its tree's cascade order: its index, except for a sheet the tree adopts,
    /// which follows every other sheet in the order `adoptedStyleSheets` lists it
    /// (Chrome 150: `[s2, s1]` lets `s1` win though it was constructed first).
    pub position: u32,
}

/// Where a stylesheet's rules apply: in the tree its owner node is in (CSS Scoping §3.1).
///
/// Chrome 150, measured (`tools/realweb/seed/ss/scope.html`): a shadow root's `<style>` styles that
/// shadow tree and nothing in the document (`[part~=button] { border-top: 4px solid }` in it leaves a
/// document `<div part=button>` at `0px`), the document's sheets style nothing inside a shadow tree,
/// and the tree's host is styled only by `:host` rules from inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SheetScope {
    /// Every tree: the user-agent sheets, and a sheet whose owner is in no document at all.
    Everywhere,
    /// Only the tree rooted at this node: a document, or a shadow root.
    Tree(NodeId),
    /// A constructed sheet (`new CSSStyleSheet()`): only the trees whose `adoptedStyleSheets` list it
    /// ([`Adoptions`]), and none until one does. Chrome 150, measured
    /// (`tools/realweb/seed/adopted`): a shadow root's adopted `:host` rules style its host, and one
    /// tree's adopted rules reach no other tree -- each used to be indexed for every tree, so a Lit
    /// component's `:host { display: flex }` styled nothing and its `#i { width: 20px }` shrank every
    /// other component's `#i`.
    Adopted,
}

/// Which constructed sheets each tree adopts, by the tree's root (a document or a shadow root), in
/// the order its `adoptedStyleSheets` lists them ([`SheetScope::Adopted`]).
pub type Adoptions = HashMap<NodeId, Vec<u32>>;

/// One tree's rules, split by what they can match.
///
/// A shadow tree's `:host` and `::slotted()` rules are kept out of `index`, because nothing in the
/// tree can match them: `:host` matches only the host, which is outside the tree, and `::slotted()`
/// only a light child of the host. On a web-component page they are most of the selectors -- on
/// www.salesforce.com, 9,083 of the universal bucket's selectors were `:host(…)` and `[part]` rules
/// repeated in every component's own `<style>`, tried against every element of the document.
#[derive(Debug, Default)]
struct TreeRules {
    index: RuleIndex,
    /// The selectors that style a pseudo-element (`p::before`), by the pseudo-element's name.
    pseudo: HashMap<String, RuleIndex>,
    /// `:host`, `:host(…)`.
    host: Vec<RuleRef>,
    /// `:host::before` and the like, by pseudo-element name.
    host_pseudo: HashMap<String, Vec<RuleRef>>,
    /// `::slotted(…)`.
    slotted: Vec<RuleRef>,
}

impl TreeRules {
    /// `shared`: how many of the sheets' selectors key on each class (see [`RuleIndex::insert_rarest`]).
    fn insert(&mut self, selector: &Selector, at: RuleRef, chain: u32, shared: &HashMap<String, u32>) {
        match (selector.pseudo_element(), selector.is_featureless_host()) {
            (Some(name), true) => self.host_pseudo.entry(name.to_string()).or_default().push(at),
            (Some(name), false) => self.pseudo.entry(name.to_string()).or_default().insert_rarest(selector, at, chain, shared),
            (None, true) => self.host.push(at),
            (None, false) if selector.is_slotted() => self.slotted.push(at),
            (None, false) => self.index.insert_rarest(selector, at, chain, shared),
        }
    }

    fn len(&self) -> usize {
        self.index.len() + self.host.len() + self.slotted.len()
    }
}

/// The rule index over a document's stylesheets, in cascade order.
///
/// The selectors that style a PSEUDO-ELEMENT (`p::before`) are indexed apart, by the pseudo-element's
/// name and bucketed on their originating compound: in `MatchingMode::Normal` a
/// pseudo-element selector matches nothing, so every one of them was a wasted test against every
/// element it shared a bucket with.
#[derive(Debug, Default)]
pub struct Matcher {
    /// The rules of the sheets that style every tree ([`SheetScope::Everywhere`]).
    everywhere: TreeRules,
    /// The rules of the sheets owned in one tree, by the tree's root.
    trees: korvus_dom::FxHashMap<NodeId, TreeRules>,
    /// Every pseudo-element name some rule styles, in any tree.
    pseudo_names: std::collections::HashSet<String>,
    /// `layer_ranks[sheet][local layer]` -- see [`Matcher::layer_rank`].
    layer_ranks: Vec<Vec<u32>>,
    /// The tree with the most rules (the only one, on a page without shadow-tree or frame styles):
    /// its root, and one index over its rules and [`Matcher::everywhere`]'s together. Two indexes cost an
    /// element two candidate walks; gitlab's hydration paid ~130 ms for it (C3 VERIFY).
    solo: Option<(NodeId, TreeRules)>,
    /// Whether each sheet's media chain applies -- the sheet's own media list and every list of the
    /// chain -- under the last media context asked about. `test` used to evaluate
    /// both for every candidate of every element -- 34 % of wolfram.com's load CPU (2026-09-29) --
    /// though they depend on nothing but the context. The matcher is dropped on every sheet edit and
    /// toggle; a different context (a resize, or a frame's own viewport) empties the answers.
    media_memo: std::cell::RefCell<MediaAnswers>,
    /// `(tree, sheet)` -> [`MatchedRule::position`] for each sheet a tree adopts; empty on a page
    /// that adopts none, which then pays one emptiness test per matched rule.
    adopted_positions: HashMap<(NodeId, u32), u32>,
    /// Sheet `n`'s chain `c` is answer `chain_base[n] + c`: one number per (sheet, chain), which each
    /// index entry carries so that a rule under a `@media` that does not hold is dropped in the bucket
    /// walk by one byte read, before it is a candidate.
    chain_base: Vec<u32>,
    chains: u32,
    /// The candidate list each collect fills and empties, kept so a collect does not grow a vector
    /// from nothing.
    candidates: std::cell::RefCell<Vec<RuleRef>>,
}

/// See [`Matcher::media_memo`]: the context the answers are for, and one answer per (sheet, chain)
/// number -- 0 not asked yet, 1 no, 2 yes -- each computed on first use.
#[derive(Debug, Default)]
struct MediaAnswers {
    ctx: Option<MediaContext>,
    answers: Vec<u8>,
}

impl MediaAnswers {
    /// Answer for `media` from now on: kept when it is the context already answered for, else emptied.
    /// Asked once per `collect`, so a candidate never compares contexts.
    fn serve(&mut self, media: &MediaContext, chains: u32) {
        if self.ctx.as_ref() != Some(media) || self.answers.len() != chains as usize {
            self.ctx = Some(*media);
            self.answers.clear();
            self.answers.resize(chains as usize, 0);
        }
    }

    /// [`Stylesheet::rule_applies`] for answer number `chain` -- sheet `sheet`'s chain `media` -- from
    /// the memo (computed on first use).
    #[inline(always)]
    fn applies(&mut self, chain: u32, sheets: &[Stylesheet], sheet: u32, media_chain: impl FnOnce() -> u32, media: &MediaContext) -> bool {
        match self.answers.get(chain as usize).copied() {
            Some(2) => true,
            Some(1) => false,
            Some(_) => {
                let sheet = &sheets[sheet as usize];
                let yes = sheet.sheet_applies(media) && sheet.chain_matches(media_chain(), media);
                self.answers[chain as usize] = 1 + yes as u8;
                yes
            }
            None => sheets.get(sheet as usize).is_some_and(|s| s.sheet_applies(media) && s.chain_matches(media_chain(), media)),
        }
    }
}

/// CSS Cascade 5 §6.4: every sheet's layers, merged into one document-wide order.
///
/// A layer NAME is the same layer in every sheet that writes it, so the order is decided across
/// sheets: the first sheet (in cascade order) to declare a name places it. Nested layers sort among
/// themselves inside their parent, and a parent's own rules come AFTER all of its sublayers -- the
/// same way unlayered rules come after every layer. That is a post-order walk of the name tree, with
/// the unlayered root last.
///
/// ```text
/// @layer a, b;  @layer a { @layer x {} }      ranks: a.x 0 · a 1 · b 2 · unlayered 3
/// ```
fn layer_ranks(sheets: &[Stylesheet]) -> Vec<Vec<u32>> {
    use crate::style::rules::LayerSeg;
    // The name tree, children in declaration order.
    let mut children: Vec<Vec<(LayerSeg, usize)>> = vec![Vec::new()];
    let mut node_of: Vec<Vec<usize>> = Vec::with_capacity(sheets.len());
    for sheet in sheets {
        let mut nodes = Vec::with_capacity(sheet.layers().len());
        for path in sheet.layers() {
            let mut node = 0;
            for seg in path {
                node = match children[node].iter().find(|(s, _)| s == seg) {
                    Some((_, n)) => *n,
                    None => {
                        children.push(Vec::new());
                        let n = children.len() - 1;
                        children[node].push((seg.clone(), n));
                        n
                    }
                };
            }
            nodes.push(node);
        }
        node_of.push(nodes);
    }
    // Post-order, iteratively: a node's rank is assigned after all of its children's.
    let mut rank = vec![0u32; children.len()];
    let mut next = 0u32;
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    while let Some((node, child)) = stack.pop() {
        if child < children[node].len() {
            stack.push((node, child + 1));
            stack.push((children[node][child].1, 0));
        } else {
            rank[node] = next;
            next += 1;
        }
    }
    node_of.into_iter().map(|nodes| nodes.into_iter().map(|n| rank[n]).collect()).collect()
}

impl Matcher {
    /// Index every selector of every rule. `sheets` is in cascade order, and a rule's position in
    /// it is what breaks ties between equally specific rules later.
    pub fn build(sheets: &[Stylesheet]) -> Matcher {
        Matcher::build_scoped(sheets, &[], &Adoptions::new(), |_, _| true)
    }

    /// [`Matcher::build`] for sheets owned in different trees: sheet `n` styles only
    /// the tree `scopes[n]` names -- an [`SheetScope::Adopted`] one the trees `adopted` lists it
    /// under -- and a sheet past the end of `scopes` styles every tree. Only the rules
    /// `keep(sheet, rule)` accepts are indexed.
    pub fn build_scoped(sheets: &[Stylesheet], scopes: &[SheetScope], adopted: &Adoptions, keep: impl Fn(u32, u32) -> bool) -> Matcher {
        let mut everywhere = TreeRules::default();
        let mut trees: korvus_dom::FxHashMap<NodeId, TreeRules> = Default::default();
        let mut pseudo_names = std::collections::HashSet::new();
        let scope_of = |s: usize| scopes.get(s).copied().unwrap_or(SheetScope::Everywhere);
        let mut chain_base = Vec::with_capacity(sheets.len());
        let mut chains = 0u32;
        for sheet in sheets {
            chain_base.push(chains);
            chains += sheet.chain_count();
        }
        // How many selectors key on each class, over every sheet: a selector goes under its rarest class.
        let mut shared: HashMap<String, u32> = HashMap::new();
        for sheet in sheets {
            for rule in sheet.cascade_rules() {
                for selector in &rule.selectors {
                    for class in selector.key_classes() {
                        *shared.entry(class).or_default() += 1;
                    }
                }
            }
        }
        let shared = &shared;
        let fill = |rules: &mut TreeRules, pseudo_names: &mut std::collections::HashSet<String>, s: usize| {
            for (r, rule) in sheets[s].cascade_rules().iter().enumerate() {
                if !keep(s as u32, r as u32) {
                    continue;
                }
                for (n, selector) in rule.selectors.iter().enumerate() {
                    if let Some(name) = selector.pseudo_element() {
                        pseudo_names.insert(name.to_string());
                    }
                    rules.insert(selector, RuleRef { sheet: s as u32, rule: r as u32, selector: n as u32 }, chain_base[s] + rule.media, shared);
                }
            }
        };
        // The sheets a tree adopts, each once -- at its last place in the list -- and only while it
        // is still a constructed sheet.
        let adopted_in = |root: NodeId| -> Vec<usize> {
            let mut out: Vec<usize> = Vec::new();
            for &s in adopted.get(&root).map_or(&[][..], |v| &v[..]) {
                let s = s as usize;
                if s < sheets.len() && scope_of(s) == SheetScope::Adopted {
                    out.retain(|&o| o != s);
                    out.push(s);
                }
            }
            out
        };
        let mut adopted_positions = HashMap::new();
        for s in 0..sheets.len() {
            let rules = match scope_of(s) {
                SheetScope::Everywhere => &mut everywhere,
                SheetScope::Tree(root) => trees.entry(root).or_default(),
                SheetScope::Adopted => continue,
            };
            fill(rules, &mut pseudo_names, s);
        }
        for &root in adopted.keys() {
            let list = adopted_in(root);
            if list.is_empty() {
                continue;
            }
            let rules = trees.entry(root).or_default();
            for (k, s) in list.into_iter().enumerate() {
                fill(rules, &mut pseudo_names, s);
                adopted_positions.insert((root, s as u32), (sheets.len() + k) as u32);
            }
        }
        // The tree with the most rules -- the document, on a page of components that adopt their
        // sheets -- reads one merged index; before S35 every adopted sheet was indexed for every
        // tree, which left a component page ONE tree that owned sheets and so a merged index.
        let main = trees.iter().max_by_key(|(root, rules)| (rules.len(), std::cmp::Reverse(**root))).map(|(root, _)| *root);
        let solo = main.map(|root| {
            let mut merged = TreeRules::default();
            let mut names = std::collections::HashSet::new();
            let mine = adopted_in(root);
            for s in 0..sheets.len() {
                match scope_of(s) {
                    SheetScope::Tree(r) if r != root => continue,
                    SheetScope::Adopted if !mine.contains(&s) => continue,
                    _ => {}
                }
                fill(&mut merged, &mut names, s);
            }
            (root, merged)
        });
        Matcher { everywhere, trees, pseudo_names, layer_ranks: layer_ranks(sheets), solo, media_memo: Default::default(), adopted_positions, chain_base, chains, candidates: Default::default() }
    }

    /// Does any rule style the pseudo-element `name`? A document with no `::before` rule pays nothing
    /// for generated content.
    pub fn has_pseudo(&self, name: &str) -> bool {
        self.pseudo_names.contains(name)
    }

    /// The document-wide rank of `sheet`'s layer `local` ([`CascadeRule::layer`]). Higher wins
    /// among normal declarations; the cascade reverses it for `!important` ones.
    ///
    /// [`CascadeRule::layer`]: crate::style::rules::CascadeRule::layer
    pub fn layer_rank(&self, sheet: u32, local: u32) -> u32 {
        self.layer_ranks
            .get(sheet as usize)
            .and_then(|l| l.get(local as usize))
            .copied()
            .unwrap_or(u32::MAX)
    }

    /// Every sheet's layer ranks, by sheet and local layer ([`Matcher::layer_rank`]).
    pub(crate) fn layer_ranks(&self) -> &[Vec<u32>] {
        &self.layer_ranks
    }

    /// How many selectors are indexed, pseudo-element selectors aside.
    pub fn len(&self) -> usize {
        self.everywhere.len() + self.trees.values().map(TreeRules::len).sum::<usize>()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// [`Matcher::match_element`] for the `name` pseudo-element of `node` (`"before"`, `"marker"`,
    /// `"-webkit-scrollbar"`): the rules whose selector ends in `::name` and whose originating part
    /// matches `node`.
    pub fn match_pseudo(
        &self,
        sheets: &[Stylesheet],
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        name: &str,
        env: &MatchEnv<'_>,
        out: &mut Vec<MatchedRule>,
    ) {
        out.clear();
        if self.has_pseudo(name) {
            self.collect(sheets, dom, node, media, Some(name), env, out);
        }
    }

    /// Append every rule that matches `node` to `out`, which is cleared first.
    ///
    /// `out` is a caller-owned buffer because this runs once per element in the document and the
    /// result is consumed immediately — allocating a vector per node to throw it away is the sort
    /// of thing that makes a style pass twice as slow as it needs to be.
    ///
    /// No `@container` condition holds here: a container query needs a laid-out ancestor, which only
    /// [`Matcher::match_element_in`]'s environment can supply.
    pub fn match_element(
        &self,
        sheets: &[Stylesheet],
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        out: &mut Vec<MatchedRule>,
    ) {
        self.match_element_in(sheets, dom, node, media, &MatchEnv::default(), out)
    }

    /// [`Matcher::match_element`], with the page's answers to `@container` and prelude-less `@scope`.
    pub fn match_element_in(
        &self,
        sheets: &[Stylesheet],
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        env: &MatchEnv<'_>,
        out: &mut Vec<MatchedRule>,
    ) {
        out.clear();
        self.collect(sheets, dom, node, media, None, env, out);
    }

    /// Append to `out` every rule that matches `node` (`pseudo` `None`) or its `pseudo`
    /// pseudo-element, from every tree whose rules can reach it (CSS Scoping §3):
    ///
    /// * the sheets that style every tree, and the sheets of `node`'s own tree -- with `:host`
    ///   resolving to that tree's host when it is a shadow tree;
    /// * when `node` is a shadow host, its shadow tree's `:host` rules;
    /// * when `node` is assigned to a slot, the `::slotted()` rules of the slot's tree, and of the
    ///   tree of every slot that slot is in turn assigned to.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn collect(
        &self,
        sheets: &[Stylesheet],
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        pseudo: Option<&str>,
        env: &MatchEnv<'_>,
        out: &mut Vec<MatchedRule>,
    ) {
        // The selector caches, borrowed once for every candidate (`korvus_dom::with_match_caches`).
        korvus_dom::with_match_caches(|caches| self.collect_with(sheets, dom, node, media, pseudo, env, out, caches))
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_with(
        &self,
        sheets: &[Stylesheet],
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        pseudo: Option<&str>,
        env: &MatchEnv<'_>,
        out: &mut Vec<MatchedRule>,
        caches: &mut korvus_dom::MatchCaches<'_>,
    ) {
        let Some((local_name, id, class_attr, attrs)) = element_keys(dom, node) else { return };
        // One borrow and one context comparison per element, not per candidate.
        let mut memo = self.media_memo.try_borrow_mut().ok();
        if let Some(m) = memo.as_deref_mut() {
            m.serve(media, self.chains);
        }
        let classes: Vec<&str> = class_attr.split_ascii_whitespace().collect();
        let keys = ElementKeys { local_name, id, classes: &classes, attrs, quirks: is_quirks(dom, node) };
        // No shadow root in the arena: the element is in no shadow tree, hosts none and is slotted
        // nowhere, so the shadow questions below need no asking -- and inside the layout walk, which
        // names the tree it walks in `env.tree`, the element's tree is known without a walk up. Those per-element walks and lookups cost gitlab's
        // hydration ~140 ms (C3 VERIFY).
        let shadows = dom.has_shadow_roots();
        let tree = match env.tree {
            Some(t) if !shadows => t,
            _ => dom.root_of(node),
        };
        let tree_host = if shadows { dom.shadow_host_of(tree) } else { None };
        let mut built = None;
        // An element of the ONE tree that owns sheets reads a single index over both sets -- the cost
        // of matching before S25 scoped them; otherwise the two sets are enumerated apart.
        let sets: [Option<&TreeRules>; 2] = match &self.solo {
            Some((root, merged)) if *root == tree => [Some(merged), None],
            _ => [Some(&self.everywhere), self.trees.get(&tree)],
        };
        for rules in sets.into_iter().flatten() {
            let index = match pseudo {
                None => Some(&rules.index),
                Some(name) => rules.pseudo.get(name),
            };
            let Some(index) = index else { continue };
            // Built once for the element, then reused for every candidate. A stylesheet where
            // thousands of rules share a rightmost key gives each element thousands of candidates,
            // and without this each one costs a walk up the tree to find out it does not match.
            // The index asks it first, before a candidate is even taken.
            //
            // Not in a shadow tree: the filter holds the element's ancestors in its own tree, and a
            // shadow tree's selector can also name the host (`:host(.x) p`). A shadow tree is small.
            if tree_host.is_none() && env.ancestors.is_none() && built.is_none() {
                built = Some(AncestorBloom::for_node(dom, node));
            }
            let bloom = if tree_host.is_none() { env.ancestors.or(built.as_ref()) } else { None };
            // Collected first, then tested: `for_each_candidate` borrows the index for the duration
            // of its callback, and testing a selector needs `sheets` and `dom` at the same time.
            let mut kept = self.candidates.try_borrow_mut().ok();
            let mut fresh = Vec::new();
            let candidates: &mut Vec<RuleRef> = kept.as_deref_mut().unwrap_or(&mut fresh);
            candidates.clear();
            match memo.as_deref_mut() {
                // A rule under a `@media` that does not hold is no candidate: one byte read here.
                Some(m) => index.for_each_candidate(keys, bloom, |r, chain| {
                    if m.applies(chain, sheets, r.sheet, || sheets[r.sheet as usize].cascade_rules()[r.rule as usize].media, media) {
                        candidates.push(r)
                    }
                }),
                None => index.for_each_candidate(keys, bloom, |r, _| candidates.push(r)),
            }
            if candidates.is_empty() {
                continue;
            }
            for &candidate in candidates.iter() {
                self.test(candidate, sheets, dom, node, media, pseudo, bloom, tree_host, 0, tree, env, memo.as_deref_mut(), out, caches);
            }
        }
        if !shadows {
            return;
        }
        // A shadow host is styled from inside its own shadow tree by that tree's `:host` rules.
        if let Some((shadow, rules)) = dom.shadow_root_of(node).and_then(|root| Some((root, self.trees.get(&root)?))) {
            let list = match pseudo {
                None => &rules.host[..],
                Some(name) => rules.host_pseudo.get(name).map_or(&[][..], |v| &v[..]),
            };
            for &candidate in list {
                self.test(candidate, sheets, dom, node, media, pseudo, None, Some(node), 1, shadow, env, memo.as_deref_mut(), out, caches);
            }
        }
        if pseudo.is_none() {
            let (mut slotted, mut context) = (node, 1u32);
            while let Some(slot) = dom.assigned_slot(slotted) {
                let slot_tree = dom.root_of(slot);
                if let Some(rules) = self.trees.get(&slot_tree) {
                    for &candidate in &rules.slotted {
                        self.test(candidate, sheets, dom, node, media, None, None, dom.shadow_host_of(slot_tree), context, slot_tree, env, memo.as_deref_mut(), out, caches);
                    }
                }
                // A slot is itself slottable: assigned to a slot of an enclosing host's shadow tree.
                slotted = slot;
                context += 1;
                if context > 64 {
                    break;
                }
            }
        }
    }

    /// Test one candidate selector against `node` and record its rule in `out` if it matches.
    /// Inlined: it is the inner loop of every style pass, run per candidate.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn test(
        &self,
        candidate: RuleRef,
        sheets: &[Stylesheet],
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        pseudo: Option<&str>,
        bloom: Option<&AncestorBloom>,
        host: Option<NodeId>,
        context: u32,
        rules_tree: NodeId,
        env: &MatchEnv<'_>,
        memo: Option<&mut MediaAnswers>,
        out: &mut Vec<MatchedRule>,
        caches: &mut korvus_dom::MatchCaches<'_>,
    ) {
        let Some(sheet) = sheets.get(candidate.sheet as usize) else { return };
        let Some(rule) = sheet.cascade_rules().get(candidate.rule as usize) else { return };
        let applies = match memo {
            Some(m) => m.applies(self.chain_base.get(candidate.sheet as usize).map_or(u32::MAX, |b| b + rule.media), sheets, candidate.sheet, || rule.media, media),
            None => sheet.rule_applies(rule, media),
        };
        if !applies {
            return;
        }
        let Some(selector) = rule.selectors.get(candidate.selector as usize) else { return };
        // `@scope`: the nearest root this element is in scope of, and how far up it is.
        let proximity = match sheet.scope_of(rule) {
            None => {
                if !selector.matches_in_tree_cached(dom, node, bloom, pseudo, None, host, caches) {
                    return;
                }
                0
            }
            Some(scope) => match scope_hops(scope, candidate.sheet, selector, dom, node, bloom, pseudo, host, env) {
                Some(hops) => u32::MAX - hops,
                None => return,
            },
        };
        if rule.container != 0 {
            let holds = match env.containers {
                Some(f) => sheet.container_chain(rule).all(|c| f(node, c)),
                None => false,
            };
            if !holds {
                return;
            }
        }
        let specificity = selector.specificity();
        // The same rule can match through more than one of its selectors; it applies once, at
        // the highest specificity that matched.
        match out.iter_mut().find(|m| m.sheet == candidate.sheet && m.rule == candidate.rule) {
            Some(existing) => existing.specificity = existing.specificity.max(specificity),
            None => out.push(MatchedRule {
                sheet: candidate.sheet,
                rule: candidate.rule,
                specificity,
                layer: self.layer_rank(candidate.sheet, rule.layer),
                proximity,
                context,
                position: match self.adopted_positions.is_empty() {
                    true => candidate.sheet,
                    false => self.adopted_positions.get(&(rules_tree, candidate.sheet)).copied().unwrap_or(candidate.sheet),
                },
            }),
        }
    }
}

/// What matching needs from the page that a stylesheet cannot say.
#[derive(Clone, Copy, Default)]
pub struct MatchEnv<'a> {
    /// Does an `@container` condition hold for this element? `None`: no layout to ask, so none does.
    pub containers: Option<&'a dyn Fn(NodeId, &ContainerCondition) -> bool>,
    /// The root of a prelude-less `@scope` in sheet `n`: its owner node's parent element.
    pub owner_parent: Option<&'a dyn Fn(u32) -> Option<NodeId>>,
    /// A filter over exactly the element's ancestors, kept by a caller that walks the tree and
    /// pushes and pops them. `None`: built per element, by walking up.
    pub ancestors: Option<&'a AncestorBloom>,
    /// The root of the tree a caller is walking, when every element it matches is in that tree's
    /// light DOM (the layout walk). `None`: found per element, by walking up.
    pub tree: Option<NodeId>,
}

/// How many generations up the nearest `@scope` root `node` is in scope of, for which `selector`
/// matches with `:scope` meaning that root -- or `None`, the rule does not apply (CSS Cascade 6 §3.5).
///
/// A root is an inclusive ancestor matching the scope's start selectors. The element is out of that
/// root's scope when it or an ancestor below the root matches a `to (…)` limit. Chrome 150:
/// `@scope (.sc2) to (.stop) { p { color } }` styles `.sc2 > p` and not `.sc2 .stop > p`.
#[allow(clippy::too_many_arguments)]
fn scope_hops(
    scope: &ScopeInfo,
    sheet: u32,
    selector: &korvus_dom::Selector,
    dom: &Dom,
    node: NodeId,
    bloom: Option<&AncestorBloom>,
    pseudo: Option<&str>,
    host: Option<NodeId>,
    env: &MatchEnv<'_>,
) -> Option<u32> {
    let is_element = |n: NodeId| matches!(dom.node(n).kind, NodeKind::Element { .. });
    let fixed_root = match &scope.start {
        Some(_) => None,
        None => Some(env.owner_parent.and_then(|f| f(sheet))?),
    };
    let mut hops = 0u32;
    let mut cur = Some(node);
    // Whether an element between `node` (inclusive) and the candidate root (exclusive) is a limit.
    let mut limited = false;
    while let Some(root) = cur {
        if !is_element(root) {
            break;
        }
        let is_root = match (&scope.start, fixed_root) {
            (Some(start), _) => start.iter().any(|s| s.matches(dom, root)),
            (None, Some(r)) => r == root,
            (None, None) => false,
        };
        if is_root && !limited && selector.matches_in_tree(dom, node, bloom, pseudo, Some(root), host) {
            return Some(hops);
        }
        if !scope.end.is_empty() && scope.end.iter().any(|e| e.matches_at(dom, root, None, None, Some(root)) || e.matches(dom, root)) {
            limited = true;
        }
        if fixed_root == Some(root) {
            break;
        }
        cur = dom.node(root).parent;
        hops += 1;
    }
    None
}

/// Is `node` in a quirks-mode document, where class and id selectors fold case?
pub(crate) fn is_quirks(dom: &Dom, node: NodeId) -> bool {
    dom.document_mode_of(node) == korvus_dom::DocumentMode::Quirks
}

/// An element's local name, id, class attribute and attributes. `None` for anything that is not an
/// element — text and comments are never matched against.
#[allow(clippy::type_complexity)]
fn element_keys(dom: &Dom, node: NodeId) -> Option<(&str, Option<&str>, &str, &[(String, String)])> {
    let (local_name, attrs) = match &dom.node(node).kind {
        NodeKind::Element { name, attrs, .. } => (name.as_str(), attrs.as_slice()),
        _ => return None,
    };
    let id = dom.get_attribute(node, "id").filter(|v| !v.is_empty());
    let class = dom.get_attribute(node, "class").unwrap_or("");
    Some((local_name, id, class, attrs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::parse::parse_stylesheet;
    use crate::style::rules::Origin;

    fn find(dom: &Dom, id: &str) -> NodeId {
        (0..dom.len() as NodeId)
            .find(|&n| dom.get_attribute(n, "id") == Some(id))
            .unwrap_or_else(|| panic!("no #{id}"))
    }

    /// Match `#target` in `html` against `css`, returning the matched selectors in index order.
    fn matched(css: &str, html: &str) -> Vec<String> {
        let sheets = vec![parse_stylesheet(Origin::Author, css)];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse(html);
        let mut out = Vec::new();
        matcher.match_element(&sheets, &dom, find(&dom, "target"), &MediaContext::default(), &mut out);
        out.iter()
            .map(|m| {
                let r = &sheets[0].cascade_rules()[m.rule as usize];
                r.selectors.iter().map(|s| s.to_css_string()).collect::<Vec<_>>().join(", ")
            })
            .collect()
    }

    #[test]
    fn only_the_rules_that_apply_come_back() {
        let got = matched(
            "p { color: red } #target { color: blue } .lede { color: teal } span { color: pink }",
            "<p id='target' class='lede'>hi</p>",
        );
        assert_eq!(got.len(), 3);
        assert!(got.contains(&"p".to_string()));
        assert!(got.contains(&"#target".to_string()));
        assert!(got.contains(&".lede".to_string()));
        assert!(!got.contains(&"span".to_string()));
    }

    #[test]
    fn a_descendant_selector_is_tested_against_the_tree_not_just_the_element() {
        assert_eq!(
            matched("nav a { color: red }", "<nav><a id='target'>x</a></nav>"),
            vec!["nav a"]
        );
        assert!(matched("nav a { color: red }", "<div><a id='target'>x</a></div>").is_empty());
    }

    // One matcher answers a page (800 px) and a frame (300 px) in turn. Its media answers
    // belong to the context they were asked under, and a rule under a `@media` that does not hold is
    // never a candidate -- whichever context asked first.
    #[test]
    fn media_answers_follow_the_context_each_collect_asks_under() {
        let sheets = vec![
            parse_stylesheet(Origin::Author, "@media (min-width: 500px) { p { color: red } } @media (max-width: 400px) { #target { color: blue } } p { color: teal }"),
            parse_stylesheet(Origin::Author, "p { margin: 0 }"),
        ];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse("<p id='target'>x</p>");
        let target = find(&dom, "target");
        let at = |width: f32| {
            let media = MediaContext { viewport_width: width, ..MediaContext::default() };
            let mut out = Vec::new();
            matcher.match_element(&sheets, &dom, target, &media, &mut out);
            let mut got: Vec<(u32, u32)> = out.iter().map(|m| (m.sheet, m.rule)).collect();
            got.sort_unstable();
            got
        };
        let wide = at(800.0);
        let narrow = at(300.0);
        assert_eq!(wide, vec![(0, 0), (0, 2), (1, 0)], "the min-width rule, not the max-width one");
        assert_eq!(narrow, vec![(0, 1), (0, 2), (1, 0)], "the max-width rule, not the min-width one");
        assert_eq!(at(800.0), wide, "and back");
        assert_eq!(at(300.0), narrow);
    }

    #[test]
    fn the_universal_bucket_is_tried_against_everything() {
        assert_eq!(matched("* { color: red }", "<p id='target'>x</p>"), vec!["*"]);
        assert_eq!(
            matched("[data-x] { color: red }", "<p id='target' data-x>x</p>"),
            vec!["[data-x]"]
        );
    }

    // A rule applies once however many of its selectors match, at the highest specificity among
    // them -- that is the selector a browser would say won.
    #[test]
    fn a_rule_matching_through_two_selectors_applies_once_at_the_higher_specificity() {
        let sheets = vec![parse_stylesheet(Origin::Author, "p, #target { color: red }")];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse("<p id='target'>x</p>");
        let mut out = Vec::new();
        matcher.match_element(&sheets, &dom, find(&dom, "target"), &MediaContext::default(), &mut out);
        assert_eq!(out.len(), 1, "one rule, not two");
        let by_id = sheets[0].cascade_rules()[0].selectors[1].specificity();
        assert_eq!(out[0].specificity, by_id, "the id selector's, not the type selector's");
    }

    #[test]
    fn a_rule_under_a_media_query_only_matches_when_it_applies() {
        let sheets = vec![parse_stylesheet(
            Origin::Author,
            "@media (min-width: 600px) { p { color: red } }",
        )];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse("<p id='target'>x</p>");
        let mut out = Vec::new();
        let node = find(&dom, "target");
        matcher.match_element(&sheets, &dom, node, &MediaContext::default(), &mut out);
        assert_eq!(out.len(), 1);
        let narrow = MediaContext { viewport_width: 400.0, ..MediaContext::default() };
        matcher.match_element(&sheets, &dom, node, &narrow, &mut out);
        assert!(out.is_empty(), "the same parsed sheet, a different viewport");
    }

    #[test]
    fn sheets_are_matched_in_the_order_they_are_given() {
        let sheets = vec![
            parse_stylesheet(Origin::UserAgent, "p { color: black }"),
            parse_stylesheet(Origin::Author, "p { color: red }"),
        ];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse("<p id='target'>x</p>");
        let mut out = Vec::new();
        matcher.match_element(&sheets, &dom, find(&dom, "target"), &MediaContext::default(), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out.iter().filter(|m| m.sheet == 0).count(), 1);
        assert_eq!(out.iter().filter(|m| m.sheet == 1).count(), 1);
    }

    // The layer order is document-wide and post-order.
    #[test]
    fn layers_rank_in_declaration_order_with_sublayers_before_their_parent_and_unlayered_last() {
        let sheets = vec![
            parse_stylesheet(Origin::Author, "@layer a, b; @layer a { @layer x { p {} } p {} }"),
            parse_stylesheet(Origin::Author, "@layer b { p {} } @layer c { p {} } p {}"),
        ];
        let matcher = Matcher::build(&sheets);
        let rank = |s: u32, r: usize| matcher.layer_rank(s, sheets[s as usize].cascade_rules()[r].layer);
        let (ax, a, b, c, unlayered) = (rank(0, 0), rank(0, 1), rank(1, 0), rank(1, 1), rank(1, 2));
        assert!(ax < a && a < b && b < c && c < unlayered, "{ax} {a} {b} {c} {unlayered}");
    }

    #[test]
    fn a_text_node_matches_nothing_at_all() {
        let sheets = vec![parse_stylesheet(Origin::Author, "* { color: red }")];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse("<p id='target'>hello</p>");
        let p = find(&dom, "target");
        let text = dom.children(p)[0];
        let mut out = Vec::new();
        matcher.match_element(&sheets, &dom, text, &MediaContext::default(), &mut out);
        assert!(out.is_empty());
    }

    // The buffer is the caller's, and it is cleared rather than appended to -- a second call must
    // not leave the first call's matches in it.
    #[test]
    fn the_output_buffer_is_reused_and_not_accumulated() {
        let sheets = vec![parse_stylesheet(Origin::Author, "p { color: red } b { color: blue }")];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse("<p id='target'>x</p><b id='other'>y</b>");
        let mut out = Vec::new();
        let media = MediaContext::default();
        matcher.match_element(&sheets, &dom, find(&dom, "target"), &media, &mut out);
        assert_eq!(out.len(), 1);
        matcher.match_element(&sheets, &dom, find(&dom, "other"), &media, &mut out);
        assert_eq!(out.len(), 1);
    }
}
