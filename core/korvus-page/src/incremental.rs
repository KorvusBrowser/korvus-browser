//! Styling only what changed: the page document's styles kept between layout passes, and
//! the plan that turns the DOM's journal into the elements a pass has to style again.
//!
//! [`DocStyle`] holds what the last pass computed for each element of one document -- its
//! [`StyleKey`], what it makes of its children ([`ParentFormatting`]), its pseudo-elements' styles, its
//! matched rules, the structural selector flags its matching left -- and the journal position it is
//! current to. The next pass reads the journal entries since then ([`korvus_dom::journal`]) and [`plan`]
//! marks, per entry, what that mutation can change:
//!
//! - the element itself, for any attribute write (one element is cheap to match again);
//! - beyond it, what the written feature REACHES in the sheets' selectors ([`Features`], from
//!   `korvus_dom::invalidation`): below it and after it, only the elements with a key of the selectors'
//!   subjects ([`Subjects`]); its parent's subtree for `:nth-child(of S)`;
//! - for an insertion or removal, the new subtree, and the siblings or parent the structural flags say
//!   were tested positionally (`:nth-child`, a sibling combinator, `:empty`);
//! - for a `:has()`, the anchors around the change whose rule can see it ([`has_anchors`]);
//! - for a hover change, the elements that entered or left the chain;
//! - for a form-state change, the controls whose answers moved;
//! - for an animated value that moved, the element (cascade only).
//!
//! The walk then styles the marked elements, and goes on into a child only while a parent's style
//! changed or something below is marked. Styles that follow laid-out sizes (`@container`, `cq*` units)
//! are styled the same way: the sizes are in the basis, and an element whose container style moved has
//! everything below it styled again (`Page::style_incremental`). What this cannot bound styles the
//! document from nothing, and [`Fallback`] counts why: a sheet edit, a viewport or media change, new
//! container sizes, another root ([`Fallback::Basis`]); `@scope`; a write in or at a shadow tree; the
//! document element's font size; the journal overflowing.
//!
//! `KORVUS_VERIFY_INCREMENTAL` (`crate::verify`) checks every pass this makes against one that reuses
//! nothing.

use korvus_dom::invalidation::{self, flags, Feature, InHas, PseudoFeature, Reach, Subject};
use korvus_dom::journal::Change;
use korvus_dom::{Dom, FxHashMap, FxHashSet, NodeId, NodeKind};

use crate::style::compute::ParentFormatting;
use crate::perf_counters as c;
use crate::StyleKey;
use crate::Stylesheet;

/// Restyle this element: match it and cascade it again.
pub(crate) const RESTYLE: u8 = 1;
/// Restyle it and every element below it.
pub(crate) const SUBTREE: u8 = 2;
/// Cascade it again with the rules it matched (an animated value moved).
pub(crate) const RECASCADE: u8 = 4;
/// Something below it is marked: the walk goes in.
pub(crate) const BELOW: u8 = 8;

/// What every selector of a sheet set reads, and how far a change to it reaches. Built once per sheet
/// revision.
#[derive(Debug, Default)]
pub(crate) struct Features {
    classes: FxHashMap<Box<str>, Reaches>,
    ids: FxHashMap<Box<str>, Reaches>,
    attributes: FxHashMap<Box<str>, Reaches>,
    hover: Reaches,
    /// The form states asked ([`korvus_dom::invalidation::form_state`] bits), and how far a change to
    /// any of them reaches.
    form_asked: u16,
    form_state: Reaches,
    empty: Reaches,
    /// A `:has()` anywhere; and per anchor key, how far a change to an anchor's answer reaches.
    pub(crate) has: bool,
    has_rules: FxHashMap<Subject, Reaches>,
    /// The keys of the elements `:has()` arguments end on, and whether some argument reads position
    /// or siblings: what decides whether an insertion or removal can change an answer.
    has_subjects: Subjects,
    has_structural: bool,
    /// A `:has()` that looks at later siblings: anchors before a change, not only above it.
    has_sibling: bool,
    /// What `:has()` arguments read: a change to one of these re-asks the anchors around it.
    has_classes: FxHashSet<Box<str>>,
    has_ids: FxHashSet<Box<str>>,
    has_attributes: FxHashSet<Box<str>>,
    has_hover: bool,
    has_form_state: bool,
    /// `:host`, `::slotted` or `::part`: a write inside a shadow tree, or to a host, styles all again.
    pub(crate) shadow: bool,
    /// An `@scope` rule: its roots are matched outside the flags' reach, so every pass styles all.
    pub(crate) scope: bool,
}

impl Features {
    pub(crate) fn of(sheets: &[Stylesheet]) -> Features {
        let mut f = Features::default();
        for sheet in sheets {
            f.add_sheet(sheet);
        }
        f
    }

    /// Add what `sheet`'s selectors read. Features only ever add up: one a rule no longer has makes a
    /// change mark more than it needs to, never less (see `Page::ensure_features`).
    pub(crate) fn add_sheet(&mut self, sheet: &Stylesheet) {
        for rule in sheet.cascade_rules() {
            if rule.scope != 0 {
                self.scope = true;
            }
            for selector in &rule.selectors {
                let subject = selector.invalidation_subject();
                selector.invalidation_features(&mut |feature, reach| self.add(feature, reach, &subject));
            }
        }
    }

    fn add(&mut self, feature: Feature<'_>, reach: Reach, subject: &Subject) {
        match feature {
            Feature::Class(c) => self.classes.entry(c.into()).or_default().add(reach, subject),
            Feature::Id(i) => self.ids.entry(i.into()).or_default().add(reach, subject),
            Feature::Attribute(a) => self.attributes.entry(a.into()).or_default().add(reach, subject),
            Feature::Pseudo(PseudoFeature::Hover) => self.hover.add(reach, subject),
            Feature::Pseudo(PseudoFeature::FormState(asked)) => {
                self.form_asked |= asked;
                self.form_state.add(reach, subject);
            }
            Feature::Pseudo(PseudoFeature::Empty) => self.empty.add(reach, subject),
            Feature::Has(anchor) => {
                self.has = true;
                self.has_rules.entry(anchor.clone()).or_default().add(reach, subject);
            }
            Feature::InHas(InHas::Subject(key)) => self.has_subjects.add(key),
            Feature::InHas(InHas::Structural) => self.has_structural = true,
            Feature::HasSibling => self.has_sibling = true,
            Feature::InHas(InHas::Class(c)) => {
                self.has_classes.insert(c.into());
            }
            Feature::InHas(InHas::Id(i)) => {
                self.has_ids.insert(i.into());
            }
            Feature::InHas(InHas::Attribute(a)) => {
                self.has_attributes.insert(a.into());
            }
            Feature::InHas(InHas::Pseudo(PseudoFeature::Hover)) => self.has_hover = true,
            // Kept as a top-level one is: the answers are what tells a change.
            Feature::InHas(InHas::Pseudo(PseudoFeature::FormState(asked))) => {
                self.form_asked |= asked;
                self.has_form_state = true;
            }
            // `:has(:empty)`: a child-list or text change, which re-asks the anchors anyway.
            Feature::InHas(InHas::Pseudo(PseudoFeature::Empty)) => {}
            Feature::Shadow => self.shadow = true,
        }
    }

    /// The form states some selector asks about.
    pub(crate) fn form_asked(&self) -> u16 {
        self.form_asked
    }
}

/// What a change to one feature restyles beyond the element it changed on, merged over every selector
/// that reads the feature.
#[derive(Debug, Default, Clone)]
pub(crate) struct Reaches {
    /// The element itself.
    own: bool,
    /// Its descendants -- those with one of these keys.
    below: Subjects,
    /// Its later siblings and their descendants -- those with one of these keys.
    after: Subjects,
    /// All its parent's children and their descendants (`:nth-child(... of S)`).
    parent: bool,
}

impl Reaches {
    fn add(&mut self, reach: Reach, subject: &Subject) {
        self.own |= reach & invalidation::REACH_SELF != 0;
        if reach & invalidation::REACH_SUBTREE != 0 {
            self.below.add(subject);
        }
        if reach & invalidation::REACH_LATER_SIBLINGS != 0 {
            self.after.add(subject);
        }
        self.parent |= reach & invalidation::REACH_PARENT_SUBTREE != 0;
    }

    fn is_empty(&self) -> bool {
        !self.own && self.below.is_empty() && self.after.is_empty() && !self.parent
    }
}

/// A set of [`Subject`] keys: an element is in it when it has any one of them.
#[derive(Debug, Default, Clone)]
pub(crate) struct Subjects {
    any: bool,
    ids: FxHashSet<Box<str>>,
    classes: FxHashSet<Box<str>>,
    tags: FxHashSet<Box<str>>,
    attributes: FxHashSet<Box<str>>,
}

impl Subjects {
    fn add(&mut self, subject: &Subject) {
        match subject {
            Subject::Any => self.any = true,
            Subject::Id(i) => drop(self.ids.insert(i.clone())),
            Subject::Class(c) => drop(self.classes.insert(c.clone())),
            Subject::Tag(t) => drop(self.tags.insert(t.clone())),
            Subject::Attribute(a) => drop(self.attributes.insert(a.clone())),
            Subject::OneOf(list) => list.iter().for_each(|s| self.add(s)),
        }
    }

    fn is_empty(&self) -> bool {
        !self.any && self.ids.is_empty() && self.classes.is_empty() && self.tags.is_empty() && self.attributes.is_empty()
    }

    /// Does `node` have one of the keys?
    fn holds(&self, dom: &Dom, node: NodeId) -> bool {
        if self.any {
            return true;
        }
        let NodeKind::Element { name, attrs, .. } = &dom.node(node).kind else { return false };
        if !self.tags.is_empty() && folded_in(&self.tags, name) {
            return true;
        }
        for (k, v) in attrs {
            if !self.attributes.is_empty() && folded_in(&self.attributes, k) {
                return true;
            }
            if k == "class" && !self.classes.is_empty() && v.split_ascii_whitespace().any(|t| folded_in(&self.classes, t)) {
                return true;
            }
            if k == "id" && !self.ids.is_empty() && folded_in(&self.ids, v) {
                return true;
            }
        }
        false
    }
}

/// Is `s`, ASCII-lowercased, in `set` (whose entries are)? Without making the lowered string when `s`
/// has no capital, which is nearly always.
fn folded_in(set: &FxHashSet<Box<str>>, s: &str) -> bool {
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        set.contains(s.to_ascii_lowercase().as_str())
    } else {
        set.contains(s)
    }
}

/// Why a pass styled the whole document rather than what changed: the counts the perf counters report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// No earlier pass of this document, or its styles were made for other sheets, another viewport or
    /// media, other container sizes, or another root.
    Basis,
    /// The journal dropped entries this pass had not read.
    Overflow,
    /// An `@scope` rule.
    Scope,
    /// A write in or at a shadow tree.
    Shadow,
    /// The document element's font size moved, so `rem` did.
    RootFont,
    /// A mutation the journal names only as "other": a namespace, a shadow root attached, a mode.
    Other,
}

/// What the sheets as a whole were when a document's styles were taken ([`DocStyle::sheets`]): a sheet
/// change is planned from the changed sheets alone ([`SheetChange`]) only while none of this moves.
#[derive(Clone, PartialEq)]
pub(crate) struct SheetFacts {
    /// Each sheet's layers' document-wide ranks (`Matcher::layer_rank`), which a kept match carries.
    pub(crate) layer_ranks: Vec<Vec<u32>>,
    /// The `@property` registry the arena typed the styles against.
    pub(crate) registry: std::sync::Arc<crate::style::registered::PropertyRegistry>,
    /// Whether a sheet uses `cq*` units, or has an `@container` rule: the pass styles those otherwise.
    pub(crate) uses_cq_units: bool,
    pub(crate) has_container_rules: bool,
}

/// A change to some sheets -- their rules, their media, whether they are enabled -- that a pass can
/// style from those sheets alone: see [`plan_sheets`].
#[derive(Debug)]
pub(crate) struct SheetChange {
    /// The changed sheets.
    pub(crate) sheets: Vec<u32>,
    /// The keys of their rules' subjects: an element a rule of theirs can match now has one.
    subjects: Subjects,
    /// For a rule whose subject has no key, a key one of the subject's ancestors has (`.menu *`,
    /// `.list > :first-child`): what is below an element with one can match it now.
    below: Subjects,
    /// A `:root` rule: the document element can match it now.
    root: bool,
}

impl SheetChange {
    /// `None` where a pass has to style everything anyway, or this cannot bound what the sheets reach:
    /// a rule whose subject has no key and no keyed ancestor (`*`, `:target`, `::before` alone) can match
    /// any element, and a `:host`, `::slotted`, `::part` or `@scope` rule styles beyond the walk's own
    /// reach.
    pub(crate) fn of(sheets: &[Stylesheet], changed: Vec<u32>) -> Option<SheetChange> {
        let (mut subjects, mut below, mut root) = (Subjects::default(), Subjects::default(), false);
        for &i in &changed {
            let sheet = &sheets[i as usize];
            if sheet.disabled {
                continue;
            }
            for rule in sheet.cascade_rules() {
                if rule.scope != 0 {
                    return None;
                }
                for selector in &rule.selectors {
                    let mut shadow = false;
                    selector.invalidation_features(&mut |feature, _| shadow |= feature == Feature::Shadow);
                    if shadow {
                        return None;
                    }
                    match selector.invalidation_subject() {
                        Subject::Any if selector.invalidation_subject_is_root() => root = true,
                        Subject::Any => below.add(&selector.invalidation_ancestor()?),
                        subject => subjects.add(&subject),
                    }
                }
            }
        }
        Some(SheetChange { sheets: changed, subjects, below, root })
    }
}

/// Mark what a sheet change can restyle, each element to match again:
///
/// - an element that matched a rule of a changed sheet: the rule may be gone, or moved (a kept match
///   names it by its index in its sheet);
/// - an element with a key of one of the changed sheets' subjects: a rule there may match it now;
/// - for a rule whose subject has no key, everything below an element with its ancestor's key, and for
///   a `:root` rule the document element;
/// - an element with a pseudo-element style a changed sheet's rule had a part in: those matches are not
///   kept, so a pseudo-element rule that went away is found only this way.
///
/// What a restyled element passes on to its children is the walk's, as for any write. A form control
/// needs nothing more when the sheets start asking about another state: the user-agent sheet asks
/// `:disabled`, so every control's answers are tracked from its first matching on, and an answer kept
/// under fewer asked states differs from the next one exactly where the new state is set.
pub(crate) fn plan_sheets(doc: &mut DocStyle, dom: &Dom, root: NodeId, change: &SheetChange) {
    let bits = change.sheets.iter().fold(0, |bits, &s| bits | PseudoKeys::sheet_bit(s));
    if change.root {
        let element = std::iter::successors(dom.first_child(root), |&c| dom.next_sibling(c)).find(|&c| is_element(dom, c));
        if let Some(e) = element.filter(|&e| doc.keys.get(e as usize).is_some_and(|k| k.is_some())) {
            doc.mark(e, RESTYLE);
            count(&c::MARKS_SHEETS);
        }
    }
    for n in 0..doc.keys.len().min(dom.len()) {
        if doc.keys[n].is_none() {
            continue;
        }
        let node = n as NodeId;
        let (start, len) = doc.spans[n];
        let matched = doc.rules[start as usize..(start + len) as usize].iter().any(|r| change.sheets.contains(&r.sheet));
        let pseudo = doc.pseudo[n].as_ref().is_some_and(|p| p.sheets & bits != 0);
        if matched || pseudo || change.subjects.holds(dom, node) {
            doc.mark(node, RESTYLE);
            count(&c::MARKS_SHEETS);
        }
        if !change.below.is_empty() && change.below.holds(dom, node) {
            doc.mark(node, SUBTREE);
            count(&c::MARKS_SHEETS);
        }
    }
}

/// The page document's styles, between passes. Indexed by node.
#[derive(Default)]
pub(crate) struct DocStyle {
    /// What the styles below were computed for: the sheet revision, the media, the container-size
    /// generation, the root, and the arena. A change to any but the sheet revision, and the next pass
    /// starts over; a sheet change starts over unless [`SheetChange`] can plan it.
    pub(crate) basis: Option<(u64, crate::MediaContext, u64, NodeId, u64)>,
    /// What the sheets as a whole were at `basis`'s revision.
    pub(crate) sheets: Option<SheetFacts>,
    /// The journal position the styles are current to.
    pub(crate) seq: u64,
    pub(crate) keys: Vec<Option<StyleKey>>,
    /// What each element makes of its children.
    pub(crate) formatting: Vec<ParentFormatting>,
    /// Each element's `::-webkit-scrollbar`, `::before` and `::after` styles, computed with it.
    pub(crate) pseudo: Vec<Option<Box<PseudoKeys>>>,
    /// Under `cq*` units: the container basis the pseudo-element styles were computed with (the last
    /// element's; see `Page::style_incremental`).
    pub(crate) pseudo_basis: Option<(f32, f32)>,
    /// The structural selector flags its matching left (`korvus_dom::invalidation::flags`), kept
    /// until the next full pass: a flag that no longer applies costs a restyle, never a wrong answer.
    pub(crate) flags: Vec<u16>,
    /// The document element's font size: `rem`.
    pub(crate) root_font_size: f32,
    /// The elements with a form-control state, and their answers when last styled.
    pub(crate) forms: Vec<(NodeId, u16)>,
    /// Each element's matched rules: `(start, len)` into `rules`, which grows with each pass that
    /// matches anything and starts over with a full one.
    pub(crate) spans: Vec<(u32, u32)>,
    pub(crate) rules: Vec<crate::style::matching::MatchedRule>,
    /// Bumped by each pass that starts over: with a span, it names one matched list for good.
    pub(crate) epoch: u64,
    /// Bumped whenever an element's computed style key or its children's formatting CHANGED
    /// in a pass (`Page::style_incremental`). The box tree is a function of these keys and the DOM, so
    /// a layout whose `keys_version` and `Dom::match_revision` are unchanged builds the same tree and is
    /// kept without rebuilding. A full restyle rewrites from an empty set, so it bumps this (every key
    /// reads as changed), which is sound -- it only costs the reuse a full restyle would not have had.
    pub(crate) keys_version: u64,
    /// Elements whose animated values moved since this document's last pass: they cascade again.
    pub(crate) animated: Vec<NodeId>,
    /// The elements a style pass cascaded since the animation scan last read them, and the ones
    /// forgotten: what that scan has to look at again. Kept for the page's document only
    /// (`tracks_restyled`), the one the scan reads.
    pub(crate) restyled: Vec<NodeId>,
    pub(crate) tracks_restyled: bool,
    /// The last layout's box-tree hashes, root and per element (a measurement, diag builds).
    pub(crate) last_hashes: Option<(u64, FxHashMap<NodeId, u64>)>,
    /// This pass's marks, and the nodes that carry one (to clear them after).
    pub(crate) marks: Vec<u8>,
    pub(crate) marked: Vec<NodeId>,
}

/// A form-state answer not yet known (see [`DocStyle::forms`]): no real answer sets every bit.
pub(crate) const FORM_UNKNOWN: u16 = u16::MAX;

/// Count a pass that styled everything, by why (diag builds: `crate::perf_counters`).
pub(crate) fn count_fallback(why: Fallback) {
    use crate::perf_counters as c;
    let counter = match why {
        Fallback::Basis => &c::FALLBACK_BASIS,
        Fallback::Overflow => &c::FALLBACK_OVERFLOW,
        Fallback::Scope => &c::FALLBACK_SCOPE,
        Fallback::Shadow => &c::FALLBACK_SHADOW,
        Fallback::RootFont => &c::FALLBACK_ROOT_FONT,
        Fallback::Other => &c::FALLBACK_OTHER,
    };
    c::add(counter, 1);
}

fn count(counter: &std::sync::atomic::AtomicU64) {
    crate::perf_counters::add(counter, 1);
}

/// One element's pseudo-element styles (see [`DocStyle::pseudo`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PseudoKeys {
    pub(crate) scrollbar: Option<StyleKey>,
    pub(crate) before: Option<StyleKey>,
    pub(crate) after: Option<StyleKey>,
    /// The sheets their matched rules came from, as [`PseudoKeys::sheet_bit`]s: what a sheet change
    /// asks of them, since the matches themselves are not kept.
    pub(crate) sheets: u64,
}

impl PseudoKeys {
    /// Sheet `sheet`'s bit in [`PseudoKeys::sheets`]: one of 64, so two sheets can share one -- which
    /// costs a pseudo-element matched again, never one missed.
    pub(crate) fn sheet_bit(sheet: u32) -> u64 {
        1 << (sheet % 64)
    }
}

impl DocStyle {
    /// Start over: every element unstyled, every flag cleared, the journal read to its end.
    pub(crate) fn reset(&mut self, dom: &Dom) {
        let n = dom.len();
        self.keys.clear();
        self.keys.resize(n, None);
        self.formatting.clear();
        self.formatting.resize(n, ParentFormatting::Flow);
        self.pseudo.clear();
        self.pseudo.resize(n, None);
        self.pseudo_basis = None;
        self.flags.clear();
        self.flags.resize(n, 0);
        self.forms.clear();
        self.animated.clear();
        self.spans.clear();
        self.spans.resize(n, (0, 0));
        self.rules.clear();
        self.epoch += 1;
        self.clear_marks();
        self.marks.clear();
        self.marks.resize(n, 0);
        self.seq = dom.journal().seq();
    }

    /// Drop the matched lists no element names any more, once they are most of `rules`: each pass that
    /// matches appends, and a pass after a sheet change can match most of the document again.
    pub(crate) fn compact_rules(&mut self) {
        let live: usize = self.spans.iter().map(|&(_, len)| len as usize).sum();
        if self.rules.len() < 4096 || self.rules.len() < 2 * live {
            return;
        }
        let mut rules = Vec::with_capacity(live);
        for span in &mut self.spans {
            let (start, len) = *span;
            *span = (rules.len() as u32, len);
            rules.extend_from_slice(&self.rules[start as usize..(start + len) as usize]);
        }
        self.rules = rules;
    }

    /// Grow the per-node vectors to the arena's size: nodes made since the last pass.
    pub(crate) fn grow(&mut self, dom: &Dom) {
        let n = dom.len();
        if self.keys.len() < n {
            self.keys.resize(n, None);
            self.formatting.resize(n, ParentFormatting::Flow);
            self.pseudo.resize(n, None);
            self.flags.resize(n, 0);
            self.marks.resize(n, 0);
            self.spans.resize(n, (0, 0));
        }
    }

    /// Mark `node` as lying on the way to a marked one.
    pub(crate) fn mark_below(&mut self, node: NodeId) {
        self.mark(node, BELOW);
    }

    /// Keep `node`'s form-state answers, to tell when they move. (A page has a few dozen controls.)
    pub(crate) fn track_form(&mut self, node: NodeId, bits: u16) {
        match self.forms.iter_mut().find(|(n, _)| *n == node) {
            Some(entry) => entry.1 = bits,
            None => self.forms.push((node, bits)),
        }
    }

    pub(crate) fn clear_marks(&mut self) {
        for n in self.marked.drain(..) {
            if let Some(m) = self.marks.get_mut(n as usize) {
                *m = 0;
            }
        }
    }

    fn mark(&mut self, node: NodeId, bits: u8) {
        let m = &mut self.marks[node as usize];
        if *m == 0 {
            self.marked.push(node);
        }
        *m |= bits;
    }

    /// Was `node` styled by the last pass, or is it the root the walk starts from? A mutation anywhere
    /// else -- a detached tree, another document, a node no slot renders -- changes no style here.
    fn styled(&self, dom: &Dom, root: NodeId, node: NodeId) -> bool {
        if node == root {
            return true;
        }
        match self.keys.get(node as usize) {
            Some(Some(_)) => true,
            _ => dom.shadow_host_of(node).is_some_and(|h| self.styled(dom, root, h)),
        }
    }

    /// Forget the styles of `node` and everything below it, shadow trees included: it left the
    /// document, and a node no pass reaches has no style (as a full pass would say).
    fn forget(&mut self, dom: &Dom, node: NodeId) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            let Some(slot) = self.keys.get_mut(n as usize) else { continue };
            if slot.take().is_some() && self.tracks_restyled {
                self.restyled.push(n);
            }
            self.pseudo[n as usize] = None;
            self.flags[n as usize] = 0;
            let mut c = dom.first_child(n);
            while let Some(child) = c {
                stack.push(child);
                c = dom.next_sibling(child);
            }
            if let Some(root) = dom.shadow_root_of(n) {
                stack.push(root);
            }
        }
    }
}

fn element_children(dom: &Dom, parent: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(dom.first_child(parent), move |&c| dom.next_sibling(c)).filter(move |&c| is_element(dom, c))
}

fn is_element(dom: &Dom, n: NodeId) -> bool {
    matches!(dom.node(n).kind, NodeKind::Element { .. })
}

/// Is `node` in a shadow tree, or a host, or a host's light child? A mutation there can change slot
/// assignment and so the flat tree, which this plan does not follow.
fn near_shadow(dom: &Dom, node: NodeId) -> bool {
    if !dom.has_shadow_roots() {
        return false;
    }
    dom.shadow_root_of(node).is_some()
        || dom.is_shadow_root(dom.root_of(node))
        || dom.parent(node).is_some_and(|p| dom.shadow_root_of(p).is_some())
        || dom.is_html_slot_element(node)
}

/// Mark what the journal entries since `doc.seq` can change, for the walk from `root`. `Err` names why
/// the pass has to style everything instead.
pub(crate) fn plan(doc: &mut DocStyle, dom: &Dom, root: NodeId, features: &Features, animated: &[NodeId]) -> Result<(), Fallback> {
    let journal = dom.journal();
    let Some(entries) = journal.since(doc.seq) else { return Err(Fallback::Overflow) };
    let mut forms_moved = false;
    for change in entries {
        match change {
            Change::Attribute { node, name, old } => {
                let (node, name, old) = (*node, journal.name(*name), old.map(|k| journal.kept(k)));
                if !doc.styled(dom, root, node) {
                    continue;
                }
                if near_shadow(dom, node) {
                    return Err(Fallback::Shadow);
                }
                doc.mark(node, RESTYLE);
                count(&c::MARKS_ATTRIBUTE);
                let current = dom.get_attribute(node, name);
                let mut asks_has = false;
                match name {
                    "class" => {
                        let was: Vec<String> = old.unwrap_or("").split_ascii_whitespace().map(|t| t.to_ascii_lowercase()).collect();
                        let now: Vec<String> = current.unwrap_or("").split_ascii_whitespace().map(|t| t.to_ascii_lowercase()).collect();
                        for token in was.iter().filter(|t| !now.contains(t)).chain(now.iter().filter(|t| !was.contains(t))) {
                            if let Some(reach) = features.classes.get(token.as_str()) {
                                reach_from(doc, dom, node, reach);
                            }
                            asks_has |= features.has_classes.contains(token.as_str());
                        }
                    }
                    "id" => {
                        for id in [old, current].into_iter().flatten() {
                            let id = id.to_ascii_lowercase();
                            if let Some(reach) = features.ids.get(id.as_str()) {
                                reach_from(doc, dom, node, reach);
                            }
                            asks_has |= features.has_ids.contains(id.as_str());
                        }
                    }
                    _ => {}
                }
                let lower = name.to_ascii_lowercase();
                if let Some(reach) = features.attributes.get(lower.as_str()) {
                    reach_from(doc, dom, node, reach);
                }
                asks_has |= features.has_attributes.contains(lower.as_str());
                if asks_has {
                    has_anchors(doc, dom, features, node);
                }
                forms_moved |= dom.has_form_state(node) || matches!(dom.tag_name(node), Some("fieldset" | "optgroup" | "legend" | "form"));
            }
            Change::Inserted { parent, node } => {
                let (parent, node) = (*parent, *node);
                if dom.parent(node) != Some(parent) || !doc.styled(dom, root, parent) {
                    continue;
                }
                if near_shadow(dom, parent) {
                    return Err(Fallback::Shadow);
                }
                // A non-element with children of its own (a fragment linked as it is) brings them too.
                if !matches!(dom.node(node).kind, NodeKind::Text(_) | NodeKind::CData(_) | NodeKind::Comment(_)) {
                    doc.mark(node, SUBTREE);
                    count(&c::MARKS_INSERTED);
                }
                children_changed(doc, dom, features, parent, Some(node), None, &[node])?;
                forms_moved = true;
            }
            Change::Removed { parent, node, next } => {
                let (parent, node) = (*parent, *node);
                // Out of this document -- detached, or moved into a detached tree or another document --
                // it has no style here (a move inside it is restyled by its insertion).
                if dom.root_of(node) != dom.root_of(root) {
                    doc.forget(dom, node);
                }
                if !doc.styled(dom, root, parent) {
                    continue;
                }
                if near_shadow(dom, parent) {
                    return Err(Fallback::Shadow);
                }
                children_changed(doc, dom, features, parent, None, Some(*next), &[node])?;
                forms_moved = true;
            }
            Change::ChildrenReplaced { parent, old } => {
                let old = journal.nodes(*old);
                for &n in old {
                    if dom.root_of(n) != dom.root_of(root) {
                        doc.forget(dom, n);
                    }
                }
                if !doc.styled(dom, root, *parent) {
                    continue;
                }
                if near_shadow(dom, *parent) {
                    return Err(Fallback::Shadow);
                }
                let now: Vec<NodeId> = std::iter::successors(dom.first_child(*parent), |&c| dom.next_sibling(c)).collect();
                let moved: Vec<NodeId> = old.iter().copied().chain(now).collect();
                children_changed(doc, dom, features, *parent, None, None, &moved)?;
                forms_moved = true;
            }
            Change::Text { node } => {
                let Some(parent) = dom.parent(*node) else { continue };
                if !doc.styled(dom, root, parent) {
                    continue;
                }
                let f = doc.flags[parent as usize];
                if features.has && features.has_structural {
                    has_anchors(doc, dom, features, parent);
                }
                if f & flags::EMPTY != 0 {
                    doc.mark(parent, RESTYLE);
                    count(&c::MARKS_EMPTY);
                    reach_from(doc, dom, parent, &features.empty);
                }
                forms_moved = true;
            }
            Change::State { .. } => forms_moved = true,
            Change::Hover { old } => {
                let old = journal.nodes(*old);
                if features.hover.is_empty() && !features.has_hover {
                    continue;
                }
                let now = dom.hover_chain();
                for &n in old.iter().filter(|n| !now.contains(n)).chain(now.iter().filter(|n| !old.contains(n))) {
                    if doc.styled(dom, root, n) {
                        if near_shadow(dom, n) {
                            return Err(Fallback::Shadow);
                        }
                        if !features.hover.is_empty() {
                            doc.mark(n, RESTYLE);
                            count(&c::MARKS_HOVER);
                            reach_from(doc, dom, n, &features.hover);
                        }
                        if features.has_hover {
                            has_anchors(doc, dom, features, n);
                        }
                    }
                }
            }
            Change::Other { node, .. } => {
                if node.is_none_or(|n| doc.styled(dom, root, n) || dom.root_of(n) == dom.root_of(root)) {
                    return Err(Fallback::Other);
                }
            }
        }
    }
    if forms_moved && features.form_asked != 0 {
        for i in 0..doc.forms.len() {
            let (n, was) = doc.forms[i];
            if doc.keys[n as usize].is_none() {
                continue;
            }
            let now = dom.form_state_bits(n, features.form_asked);
            if now != was {
                doc.forms[i].1 = now;
                if near_shadow(dom, n) {
                    return Err(Fallback::Shadow);
                }
                doc.mark(n, RESTYLE);
                count(&c::MARKS_FORM);
                reach_from(doc, dom, n, &features.form_state);
                if features.has_form_state {
                    has_anchors(doc, dom, features, n);
                }
            }
        }
        doc.forms.retain(|&(n, _)| doc.keys[n as usize].is_some());
    }
    for &n in animated {
        if doc.keys.get(n as usize).is_some_and(|k| k.is_some()) {
            doc.mark(n, RECASCADE);
            count(&c::MARKS_ANIMATED);
        }
    }
    Ok(())
}

/// Something `:has()` reads changed at `node` (or below it): every element that asked a `:has()` and
/// could see the change -- `node` itself, its ancestors, and with a sibling-looking `:has()` the
/// elements before each of them -- is styled again, with what its rule's answer reaches. An element no
/// `:has()` was asked of has the flag clear and is skipped: had the rest of its compound let `:has()`
/// be asked, the flag would be set. What an answer reaches below an anchor is walked once, from the
/// highest anchor of each rule: the lower ones are inside it.
fn has_anchors(doc: &mut DocStyle, dom: &Dom, features: &Features, node: NodeId) {
    let anchor = |n: NodeId| doc.flags.get(n as usize).is_some_and(|f| f & flags::ANCHOR != 0);
    // `(anchor, on the ancestor chain)`: the chain runs upward, so its last match is its highest. A
    // sibling-looking `:has()` adds the elements before each link of the chain, whose subtrees are not
    // inside one another's.
    let mut anchors: Vec<(NodeId, bool)> = Vec::new();
    let mut cur = Some(node);
    while let Some(a) = cur {
        if anchor(a) {
            anchors.push((a, true));
        }
        if features.has_sibling {
            let mut s = dom.previous_sibling(a);
            while let Some(b) = s {
                if anchor(b) {
                    anchors.push((b, false));
                }
                s = dom.previous_sibling(b);
            }
        }
        cur = dom.parent(a);
    }
    if anchors.is_empty() {
        return;
    }
    for (key, reach) in &features.has_rules {
        let below = (!reach.below.is_empty()).then(|| Reaches { below: reach.below.clone(), ..Default::default() });
        let after = (!reach.after.is_empty()).then(|| Reaches { after: reach.after.clone(), ..Default::default() });
        let mut top = None;
        for &(a, on_chain) in &anchors {
            if !dom.has_subject_key(a, key) {
                continue;
            }
            count(&c::MARKS_HAS);
            if reach.own {
                doc.mark(a, RESTYLE);
            }
            if let Some(after) = &after {
                reach_from(doc, dom, a, after);
            }
            match (&below, on_chain) {
                (Some(_), true) => top = Some(a),
                (Some(below), false) => reach_from(doc, dom, a, below),
                (None, _) => {}
            }
        }
        if let (Some(t), Some(below)) = (top, &below) {
            reach_from(doc, dom, t, below);
        }
    }
}

/// Can inserting or removing the subtree of `node` change a `:has()` answer? When some argument reads
/// position or siblings, always; else when the subtree holds an element an argument ends on.
fn touches_has(dom: &Dom, features: &Features, node: NodeId) -> bool {
    if features.has_structural || features.has_sibling || features.has_subjects.any {
        return true;
    }
    if features.has_subjects.is_empty() {
        return false;
    }
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if features.has_subjects.holds(dom, n) {
            return true;
        }
        let mut c = dom.first_child(n);
        while let Some(child) = c {
            stack.push(child);
            c = dom.next_sibling(child);
        }
    }
    false
}

/// A feature of `node` changed, and selectors read it as `reach` says: mark what that restyles. Below
/// `node` and after it, only the elements with a key of the selectors' subjects ([`Subjects`]): a class
/// on `<body>` read by `.dark .card` restyles the cards, not the page.
fn reach_from(doc: &mut DocStyle, dom: &Dom, node: NodeId, reach: &Reaches) {
    if reach.own {
        doc.mark(node, RESTYLE);
    }
    if !reach.below.is_empty() {
        if reach.below.any {
            doc.mark(node, SUBTREE);
            count(&c::MARKS_REACH_SUBTREE);
        } else {
            mark_keyed(doc, dom, &reach.below, dom.first_child(node));
        }
    }
    if !reach.after.is_empty() {
        let mut s = dom.next_sibling(node);
        while let Some(n) = s {
            if is_element(dom, n) {
                if reach.after.any {
                    doc.mark(n, SUBTREE);
                    count(&c::MARKS_REACH_SIBLINGS);
                } else {
                    if reach.after.holds(dom, n) {
                        doc.mark(n, RESTYLE);
                        count(&c::MARKS_REACH_KEYED);
                    }
                    mark_keyed(doc, dom, &reach.after, dom.first_child(n));
                }
            }
            s = dom.next_sibling(n);
        }
    }
    if reach.parent {
        if let Some(p) = dom.parent(node) {
            let children: Vec<NodeId> = element_children(dom, p).collect();
            for child in children {
                doc.mark(child, SUBTREE);
            }
        }
    }
}

/// Mark every element of the subtrees of `first` and its later siblings that has a key of `keys`.
fn mark_keyed(doc: &mut DocStyle, dom: &Dom, keys: &Subjects, first: Option<NodeId>) {
    let mut stack: Vec<NodeId> = first.into_iter().collect();
    while let Some(n) = stack.pop() {
        if let Some(next) = dom.next_sibling(n) {
            stack.push(next);
        }
        if !is_element(dom, n) {
            continue;
        }
        if keys.holds(dom, n) {
            doc.mark(n, RESTYLE);
            count(&c::MARKS_REACH_KEYED);
        }
        if let Some(child) = dom.first_child(n) {
            stack.push(child);
        }
    }
}

/// `parent`'s children changed: `inserted` went in, or a child went out from before `next` (`Some(None)`:
/// from the end), or (both `None`) they were all replaced. What the structural flags say was tested
/// against these children is styled again.
fn children_changed(
    doc: &mut DocStyle,
    dom: &Dom,
    features: &Features,
    parent: NodeId,
    inserted: Option<NodeId>,
    removed_before: Option<Option<NodeId>>,
    moved: &[NodeId],
) -> Result<(), Fallback> {
    let f = doc.flags[parent as usize];
    if features.has && moved.iter().any(|&n| touches_has(dom, features, n)) {
        has_anchors(doc, dom, features, parent);
    }
    if f & flags::EMPTY != 0 {
        doc.mark(parent, RESTYLE);
        count(&c::MARKS_EMPTY);
        reach_from(doc, dom, parent, &features.empty);
    }
    let all = f & flags::ALL_CHILDREN != 0 || (inserted.is_none() && removed_before.is_none());
    if all {
        let children: Vec<NodeId> = element_children(dom, parent).collect();
        for child in children {
            doc.mark(child, SUBTREE);
            count(&c::MARKS_CHILDREN_ALL);
        }
        return Ok(());
    }
    if f & flags::LATER_CHILDREN != 0 {
        // The first child after the change, and every one after it.
        let start = match (inserted, removed_before) {
            (Some(n), _) => dom.next_sibling(n),
            (None, Some(Some(next))) if dom.parent(next) == Some(parent) => Some(next),
            (None, Some(Some(_))) => dom.first_child(parent),
            _ => None,
        };
        let mut s = start;
        while let Some(n) = s {
            if is_element(dom, n) {
                doc.mark(n, SUBTREE);
                count(&c::MARKS_CHILDREN_LATER);
            }
            s = dom.next_sibling(n);
        }
    }
    if f & flags::EDGE_CHILDREN != 0 {
        let first = element_children(dom, parent).next();
        let last = std::iter::successors(dom.last_child(parent), |&c| dom.previous_sibling(c)).find(|&c| is_element(dom, c));
        let mut near: Vec<NodeId> = first.into_iter().chain(last).collect();
        if let Some(n) = inserted {
            let prev = std::iter::successors(dom.previous_sibling(n), |&c| dom.previous_sibling(c)).find(|&c| is_element(dom, c));
            let next = std::iter::successors(dom.next_sibling(n), |&c| dom.next_sibling(c)).find(|&c| is_element(dom, c));
            near.extend(prev.into_iter().chain(next));
        }
        for child in near {
            doc.mark(child, SUBTREE);
            count(&c::MARKS_CHILDREN_EDGE);
        }
    }
    Ok(())
}
