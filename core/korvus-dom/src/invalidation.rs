//! What a selector reads, and from where: the invalidation sets.
//!
//! A write to an element's `class` can change which rules match that element, its descendants, or its
//! later siblings, depending on where in a selector the class appears. Blink indexes every feature a
//! rule mentions by that position (its RuleFeatureSet) and, on a write, restyles only what the written
//! feature can reach. This is the same index, at the granularity this engine needs:
//!
//! - [`Selector::invalidation_features`] walks one selector and reports each feature it reads -- a class,
//!   an id, an attribute name, a state pseudo-class -- with its [`Reach`]: the compound it sits in
//!   decides it. The rightmost compound is the element itself; any compound to the left of it reaches
//!   through the combinator immediately to ITS right: a descendant or child combinator keeps every
//!   element the selector can end on inside the feature's element's subtree, a sibling combinator
//!   inside its later siblings' subtrees (whatever further combinators follow).
//! - [`record_selector_flags`] runs matching with the `selectors` crate's own structural flags turned
//!   on -- an element whose children were tested by `:nth-child`, `:first-child`, a sibling combinator or
//!   `:empty` -- which is what an insertion or removal needs: which parent's children it can restyle.
//!
//! Anything this cannot bound is reported as such ([`Feature::Has`], [`Feature::Shadow`]), and the
//! reader styles the whole document again for it.

use crate::select_engine::{KorvusPseudoClass, KorvusSelectorImpl};
use crate::NodeId;
use selectors::parser::{self, Combinator, Component};
use std::cell::{Cell, RefCell};

/// Where a change to a feature can change a match, relative to the element whose feature changed.
pub type Reach = u8;
/// The element itself.
pub const REACH_SELF: Reach = 1;
/// Its descendants (and so, with [`REACH_SELF`], its subtree).
pub const REACH_SUBTREE: Reach = 2;
/// Its later siblings and their subtrees.
pub const REACH_LATER_SIBLINGS: Reach = 4;
/// All its siblings and their subtrees: the parent's subtree (`:nth-child(... of S)`).
pub const REACH_PARENT_SUBTREE: Reach = 8;

/// A state a pseudo-class reads that is not an attribute of the element alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoFeature {
    /// `:hover`: the hover chain.
    Hover,
    /// The form-control state the arena keeps (`forms.rs`): `:checked`, `:disabled`, `:valid` and kin,
    /// which read more than the element's own attributes (a fieldset's `disabled`, a radio group). The
    /// [`form_state`] bits say which.
    FormState(u16),
    /// `:empty`: the element's children. Changed only by a child list or a text change, which the
    /// structural flags cover; reported for its reach.
    Empty,
}

/// What a `:has()` argument reads (see [`Feature::InHas`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InHas<'a> {
    Class(&'a str),
    Id(&'a str),
    Attribute(&'a str),
    Pseudo(PseudoFeature),
    /// The key of the element an argument ends on: inserting or removing a subtree changes an anchor's
    /// answer only if the subtree holds one.
    Subject(&'a Subject),
    /// An argument that reads position (`:nth-*`, `:first-child`, `:empty`) or looks sideways: any
    /// insertion or removal can change an answer.
    Structural,
}

/// One thing a selector reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature<'a> {
    /// A class name, ASCII-lowercased (a quirks document matches classes without case).
    Class(&'a str),
    /// An id, ASCII-lowercased.
    Id(&'a str),
    /// An attribute name, ASCII-lowercased. The pseudo-classes that read one attribute of the element
    /// itself are reported as it: `:link` as `href`, `:required`/`:optional` as `required`,
    /// `:read-only`/`:read-write` as `readonly`.
    Attribute(&'a str),
    Pseudo(PseudoFeature),
    /// A compound with a `:has()`, by its key (the anchors have it): its reach is what a change to the
    /// `:has()` answer reaches.
    Has(&'a Subject),
    /// A `:has()` whose argument looks at later siblings (`:has(+ a)`, `:has(~ a)`), not only below.
    HasSibling,
    /// A feature INSIDE a `:has()` argument: a change to it can change the answer of any anchor above
    /// (or, with [`Feature::HasSibling`], before) the element it changed on.
    InHas(InHas<'a>),
    /// Shadow-tree scoping (`:host`, `:host-context`, `::slotted`, `::part`).
    Shadow,
}

/// The elements a selector can style, by one key each of them must have: the rightmost compound's id,
/// else a class, else its type, else an attribute it names (ASCII-lowercased). [`Subject::Any`] when
/// the compound names none of them (`*`, `:not(.a)`, `:is(...)`). A change whose reach is a subtree
/// restyles only the elements below that have the key -- Blink's descendant invalidation sets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Any,
    Id(Box<str>),
    Class(Box<str>),
    Tag(Box<str>),
    Attribute(Box<str>),
    /// `:is(.a, .b)` / `:where(...)` with no key beside it: the element has one of the alternatives'.
    OneOf(Vec<Subject>),
}

fn lower(s: &str) -> Box<str> {
    s.to_ascii_lowercase().into()
}

/// How narrow a key is: an id names fewest elements, an attribute name most.
fn key_rank(s: &Subject) -> u8 {
    match s {
        Subject::Any => 0,
        Subject::OneOf(_) => 1,
        Subject::Attribute(_) => 2,
        Subject::Tag(_) => 3,
        Subject::Class(_) => 4,
        Subject::Id(_) => 5,
    }
}

/// The strongest key of one compound (see [`Subject`]).
fn compound_key(compound: &[&Component<KorvusSelectorImpl>]) -> Subject {
    let rank = key_rank;
    let mut best = Subject::Any;
    for component in compound {
        let candidate = match component {
            Component::ID(id) => Subject::Id(id.0.to_ascii_lowercase().into()),
            Component::Class(class) => Subject::Class(class.0.to_ascii_lowercase().into()),
            Component::LocalName(name) => Subject::Tag(lower(&name.lower_name.0)),
            Component::AttributeInNoNamespaceExists { local_name_lower, .. } => Subject::Attribute(lower(&local_name_lower.0)),
            Component::AttributeInNoNamespace { local_name, .. } => Subject::Attribute(lower(&local_name.0)),
            Component::AttributeOther(other) => Subject::Attribute(lower(&other.local_name_lower.0)),
            // Each alternative's own subject; any alternative without one makes the list keyless.
            Component::Is(list) | Component::Where(list) => {
                let alternatives: Vec<Subject> = list.slice().iter().map(subject).collect();
                if alternatives.iter().any(|a| *a == Subject::Any) {
                    continue;
                }
                Subject::OneOf(alternatives)
            }
            _ => continue,
        };
        if rank(&candidate) > rank(&best) {
            best = candidate;
        }
    }
    best
}

pub(crate) fn subject(selector: &parser::Selector<KorvusSelectorImpl>) -> Subject {
    let mut iter = selector.iter();
    loop {
        let compound: Vec<&Component<KorvusSelectorImpl>> = (&mut iter).collect();
        // `.b::before`: the compound before the pseudo-element is the element's.
        let only_pseudo = compound.iter().all(|c| matches!(c, Component::PseudoElement(_)));
        if only_pseudo && matches!(iter.next_sequence(), Some(Combinator::PseudoElement)) {
            continue;
        }
        return compound_key(&compound);
    }
}

/// For a selector whose subject has no key ([`Subject::Any`]: `.menu *`, `#banner h3 *`,
/// `.list > :first-child`): a key that some ANCESTOR of every element it matches has -- the strongest
/// key of a compound reached leftward across a descendant or child combinator. A sibling combinator on
/// the way keeps it an ancestor (siblings share their parent); a slot or part combinator, which leaves
/// the tree, gives `None`, as does a selector with no such keyed compound (`* *`, `:target`).
pub(crate) fn ancestor_key(selector: &parser::Selector<KorvusSelectorImpl>) -> Option<Subject> {
    let mut iter = selector.iter();
    let mut above = false;
    let mut best: Option<Subject> = None;
    loop {
        let compound: Vec<&Component<KorvusSelectorImpl>> = (&mut iter).collect();
        if above {
            let key = compound_key(&compound);
            if key != Subject::Any && best.as_ref().is_none_or(|b| key_rank(&key) > key_rank(b)) {
                best = Some(key);
            }
        }
        match iter.next_sequence() {
            None => return best,
            Some(Combinator::Child | Combinator::Descendant) => above = true,
            // A sibling shares the parent; `a::before`'s `a` is the element itself.
            Some(Combinator::NextSibling | Combinator::LaterSibling | Combinator::PseudoElement) => {}
            Some(Combinator::SlotAssignment | Combinator::Part) => return None,
        }
    }
}

/// Does `selector`'s subject compound hold `:root`? Then only a document element can match it.
pub(crate) fn subject_is_root(selector: &parser::Selector<KorvusSelectorImpl>) -> bool {
    let mut iter = selector.iter();
    loop {
        let compound: Vec<&Component<KorvusSelectorImpl>> = (&mut iter).collect();
        // `:root::before`: the compound before the pseudo-element is the element's.
        let only_pseudo = compound.iter().all(|c| matches!(c, Component::PseudoElement(_)));
        if only_pseudo && matches!(iter.next_sequence(), Some(Combinator::PseudoElement)) {
            continue;
        }
        return compound.iter().any(|c| matches!(c, Component::Root));
    }
}

impl crate::Dom {
    /// Does `id` have the key `subject` names (see [`Subject`])?
    pub fn has_subject_key(&self, id: NodeId, subject: &Subject) -> bool {
        match subject {
            Subject::Any => true,
            Subject::Id(i) => self.get_attribute(id, "id").is_some_and(|v| v.eq_ignore_ascii_case(i)),
            Subject::Class(c) => self.get_attribute(id, "class").is_some_and(|v| v.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case(c))),
            Subject::Tag(t) => self.tag_name(id).is_some_and(|n| n.eq_ignore_ascii_case(t)),
            Subject::Attribute(a) => match &self.node(id).kind {
                crate::NodeKind::Element { attrs, .. } => attrs.iter().any(|(k, _)| k.eq_ignore_ascii_case(a)),
                _ => false,
            },
            Subject::OneOf(list) => list.iter().any(|s| self.has_subject_key(id, s)),
        }
    }
}

/// Report every feature of `selector` to `sink`, with its reach.
pub(crate) fn features(selector: &parser::Selector<KorvusSelectorImpl>, outer: Reach, sink: &mut dyn FnMut(Feature<'_>, Reach)) {
    let mut iter = selector.iter();
    // The rightmost compound reaches what the selector's own position reaches (the element itself
    // at the top level).
    let mut reach = outer;
    loop {
        let compound: Vec<&Component<KorvusSelectorImpl>> = (&mut iter).collect();
        let key = if compound.iter().any(|c| matches!(c, Component::Has(_))) { compound_key(&compound) } else { Subject::Any };
        for component in compound {
            component_features(component, reach, &key, sink);
        }
        let Some(combinator) = iter.next_sequence() else { break };
        let here = match combinator {
            Combinator::Child | Combinator::Descendant => REACH_SUBTREE,
            Combinator::NextSibling | Combinator::LaterSibling => REACH_LATER_SIBLINGS,
            // `a::before`: `a` is the element the pseudo-element belongs to.
            Combinator::PseudoElement => REACH_SELF,
            Combinator::SlotAssignment | Combinator::Part => {
                sink(Feature::Shadow, REACH_SELF);
                REACH_SELF
            }
        };
        reach = compose(here, outer);
    }
}

/// The reach of a feature `inner` from the element its selector ends on, when that selector sits at a
/// position that itself reaches `outer`: a nested `:is(.a .b)` in a compound left of a descendant
/// combinator, say. Anything beyond the element itself is already contained in what `inner` names (a
/// later sibling's subtree, or the feature element's own subtree, holds everything the outer
/// combinators can reach from there), so only an inner [`REACH_SELF`] takes the outer reach.
fn compose(inner: Reach, outer: Reach) -> Reach {
    if inner == REACH_SELF {
        outer
    } else {
        inner
    }
}

fn list_features(list: &[parser::Selector<KorvusSelectorImpl>], reach: Reach, sink: &mut dyn FnMut(Feature<'_>, Reach)) {
    for s in list {
        features(s, reach, sink);
    }
}

fn component_features(component: &Component<KorvusSelectorImpl>, reach: Reach, key: &Subject, sink: &mut dyn FnMut(Feature<'_>, Reach)) {
    let attr = |name: &str, sink: &mut dyn FnMut(Feature<'_>, Reach)| {
        let lower = name.to_ascii_lowercase();
        sink(Feature::Attribute(&lower), reach);
    };
    match component {
        Component::ID(id) => {
            let lower = id.0.to_ascii_lowercase();
            sink(Feature::Id(&lower), reach);
        }
        Component::Class(class) => {
            let lower = class.0.to_ascii_lowercase();
            sink(Feature::Class(&lower), reach);
        }
        Component::AttributeInNoNamespaceExists { local_name_lower, .. } => attr(&local_name_lower.0, sink),
        Component::AttributeInNoNamespace { local_name, .. } => attr(&local_name.0, sink),
        Component::AttributeOther(other) => attr(&other.local_name_lower.0, sink),
        Component::Empty => sink(Feature::Pseudo(PseudoFeature::Empty), reach),
        Component::Negation(list) | Component::Is(list) | Component::Where(list) => list_features(list.slice(), reach, sink),
        Component::NthOf(of) => list_features(of.selectors(), REACH_PARENT_SUBTREE | reach, sink),
        Component::Has(relative) => {
            sink(Feature::Has(key), reach);
            for r in relative.iter() {
                if !r.match_hint.is_descendant_direction() {
                    sink(Feature::HasSibling, reach);
                }
                let arg_subject = subject(&r.selector);
                sink(Feature::InHas(InHas::Subject(&arg_subject)), reach);
                let mut iter = r.selector.iter();
                let mut sideways = false;
                loop {
                    for c in &mut iter {
                        sideways |= matches!(c, Component::Nth(_) | Component::NthOf(_) | Component::Empty | Component::Root);
                    }
                    match iter.next_sequence() {
                        Some(Combinator::NextSibling | Combinator::LaterSibling) => sideways = true,
                        Some(_) => {}
                        None => break,
                    }
                }
                if sideways {
                    sink(Feature::InHas(InHas::Structural), reach);
                }
                features(&r.selector, REACH_SELF, &mut |inner, _| {
                    let inner = match inner {
                        Feature::Class(c) => InHas::Class(c),
                        Feature::Id(i) => InHas::Id(i),
                        Feature::Attribute(a) => InHas::Attribute(a),
                        Feature::Pseudo(p) => InHas::Pseudo(p),
                        // A nested `:has()` or shadow part: the outer one already says "anchors above".
                        other => return sink(other, reach),
                    };
                    sink(Feature::InHas(inner), reach);
                });
            }
        }
        Component::Host(inner) => {
            sink(Feature::Shadow, reach);
            if let Some(inner) = inner {
                features(inner, reach, sink);
            }
        }
        Component::Slotted(inner) => {
            sink(Feature::Shadow, reach);
            features(inner, reach, sink);
        }
        Component::Part(_) => sink(Feature::Shadow, reach),
        Component::NonTSPseudoClass(pc) => {
            use KorvusPseudoClass::*;
            match pc {
                Hover => sink(Feature::Pseudo(PseudoFeature::Hover), reach),
                Enabled | Disabled => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::DISABLED)), reach),
                Checked => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::CHECKED)), reach),
                Default => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::DEFAULT)), reach),
                Indeterminate => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::INDETERMINATE)), reach),
                PlaceholderShown => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::PLACEHOLDER_SHOWN)), reach),
                Valid | Invalid => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::VALIDITY)), reach),
                InRange | OutOfRange => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::RANGE)), reach),
                PopoverOpen => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::POPOVER_OPEN)), reach),
                Modal => sink(Feature::Pseudo(PseudoFeature::FormState(form_state::MODAL)), reach),
                Required | Optional => attr("required", sink),
                ReadOnly | ReadWrite => attr("readonly", sink),
                Link | AnyLink(_) => attr("href", sink),
                WebkitAny(list) => list_features(list.slice(), reach, sink),
                // The focus ring: a move journals a change that restyles the document (`Dom::set_focus`).
                Focus | FocusVisible | FocusWithin => {}
                // The element's language and direction are inherited from the nearest ancestor that says: a change
                // to one of those attributes reaches everything below it.
                Lang(_) => {
                    sink(Feature::Attribute("lang"), reach | REACH_SUBTREE);
                    sink(Feature::Attribute("xml:lang"), reach | REACH_SUBTREE);
                }
                Dir(_) => sink(Feature::Attribute("dir"), reach | REACH_SUBTREE),
                // Never matches: nothing it reads can change a match.
                UnsupportedValid(_) => {}
            }
        }
        // A type selector, a namespace, `:root`, `:scope`, `:nth-*` without `of`, a pseudo-element, the
        // `&` of nesting: what they read is the element's name and place in the tree, which only an
        // insertion or removal changes -- and the structural flags cover that.
        _ => {}
    }
}

/// Which form-control states a selector set asks about, and an element's answers to them: one bit per
/// pair of pseudo-classes that read the same state.
pub mod form_state {
    /// `:enabled` / `:disabled`.
    pub const DISABLED: u16 = 1;
    pub const CHECKED: u16 = 2;
    pub const DEFAULT: u16 = 4;
    pub const INDETERMINATE: u16 = 8;
    pub const PLACEHOLDER_SHOWN: u16 = 16;
    /// `:valid` / `:invalid`: one bit for "has a validity", one for its answer.
    pub const VALIDITY: u16 = 32;
    pub const VALID: u16 = 64;
    /// `:in-range` / `:out-of-range`, the same way.
    pub const RANGE: u16 = 128;
    pub const IN_RANGE: u16 = 256;
    /// `:enabled` asks this too: an element that cannot be disabled is neither.
    pub const CAN_BE_DISABLED: u16 = 512;
    /// `:popover-open` and `:modal` -- top-layer states script sets (`Dom::is_popover_open`/`is_modal`).
    pub const POPOVER_OPEN: u16 = 1024;
    pub const MODAL: u16 = 2048;
}

impl crate::Dom {
    /// Can `id` answer any form-state pseudo-class at all? The elements a form-state signature is kept
    /// for: the controls (a form-associated custom element among them), forms, and `<progress>`.
    pub fn has_form_state(&self, id: NodeId) -> bool {
        self.can_be_disabled(id)
            || (self.is_html_ns(id) && matches!(self.tag_name(id), Some("form" | "progress" | "dialog")))
            || self.get_attribute(id, "popover").is_some()
            || self.open_states.contains_key(&id)
    }

    /// `id`'s answers to the form-state pseudo-classes `asked` names ([`form_state`] bits), from the same
    /// questions the matcher asks: equal answers match the same rules.
    pub fn form_state_bits(&self, id: NodeId, asked: u16) -> u16 {
        use form_state::*;
        let mut bits = 0;
        if asked & DISABLED != 0 && self.can_be_disabled(id) {
            bits |= CAN_BE_DISABLED;
            if self.actually_disabled(id) {
                bits |= DISABLED;
            }
        }
        if asked & CHECKED != 0 && self.is_checked(id) {
            bits |= CHECKED;
        }
        if asked & DEFAULT != 0 && self.is_default(id) {
            bits |= DEFAULT;
        }
        if asked & INDETERMINATE != 0 && self.is_indeterminate(id) {
            bits |= INDETERMINATE;
        }
        if asked & PLACEHOLDER_SHOWN != 0 && self.placeholder_shown(id) {
            bits |= PLACEHOLDER_SHOWN;
        }
        if asked & VALIDITY != 0 {
            match self.validity_state(id) {
                Some(true) => bits |= VALIDITY | VALID,
                Some(false) => bits |= VALIDITY,
                None => {}
            }
        }
        if asked & POPOVER_OPEN != 0 && self.is_popover_open(id) {
            bits |= POPOVER_OPEN;
        }
        if asked & MODAL != 0 && self.is_modal(id) {
            bits |= MODAL;
        }
        if asked & RANGE != 0 {
            match self.range_state(id) {
                Some(true) => bits |= RANGE | IN_RANGE,
                Some(false) => bits |= RANGE,
                None => {}
            }
        }
        bits
    }
}

thread_local! {
    /// Whether [`record_selector_flags`] is running, and what it has collected.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static FLAGS: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

/// Is a [`record_selector_flags`] open on this thread?
#[inline]
pub(crate) fn recording() -> bool {
    RECORDING.with(|r| r.get())
}

/// Record one element's flags (see [`record_selector_flags`]).
pub(crate) fn record(node: NodeId, bits: u16) {
    FLAGS.with(|f| {
        if let Some(slot) = f.borrow_mut().get_mut(node as usize) {
            *slot |= bits;
        }
    });
}

/// Run `f` with the structural selector flags of every match it makes ORed into `flags`, one slot per
/// node, and hand `flags` back: `selectors::matching::ElementSelectorFlags` bits, put on the element they
/// describe -- its own (`:empty`, `:has()`) on it, its children's (`:nth-*`, `:first-child`, a sibling
/// combinator) on its parent, as the crate splits them. In place, because a page whose sheet has
/// sibling combinators raises one per element matched. A node past the end is not recorded: size
/// `flags` to the arena first. Not re-entrant: an inner call records nothing of its own.
pub fn record_selector_flags<R>(flags: Vec<u16>, f: impl FnOnce() -> R) -> (R, Vec<u16>) {
    if RECORDING.with(|r| r.get()) {
        return (f(), flags);
    }
    let outer = FLAGS.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), flags));
    RECORDING.with(|r| r.set(true));
    let out = f();
    RECORDING.with(|r| r.set(false));
    (out, FLAGS.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), outer)))
}

/// The flag bits [`record_selector_flags`] reports, under names this crate's readers can use without
/// depending on `selectors`.
pub mod flags {
    use selectors::matching::ElementSelectorFlags as F;
    /// An insertion or removal among this element's children restyles all of them.
    pub const ALL_CHILDREN: u16 = (F::HAS_SLOW_SELECTOR.bits() | F::HAS_SLOW_SELECTOR_NTH_OF.bits()) as u16;
    /// ... restyles the children after the change.
    pub const LATER_CHILDREN: u16 = F::HAS_SLOW_SELECTOR_LATER_SIBLINGS.bits() as u16;
    /// ... restyles the first and last child, and the ones next to the change.
    pub const EDGE_CHILDREN: u16 = F::HAS_EDGE_CHILD_SELECTOR.bits() as u16;
    /// `:empty` was asked of this element.
    pub const EMPTY: u16 = F::HAS_EMPTY_SELECTOR.bits() as u16;
    /// `:has()` was asked of this element: a change below it (or after it) can change its answer.
    pub const ANCHOR: u16 = (F::ANCHORS_RELATIVE_SELECTOR.bits() | F::ANCHORS_RELATIVE_SELECTOR_NON_SUBJECT.bits()) as u16;
    /// `:has()` was anchored at, or searched through, this element.
    pub const RELATIVE: u16 = (F::ANCHORS_RELATIVE_SELECTOR.bits()
        | F::ANCHORS_RELATIVE_SELECTOR_NON_SUBJECT.bits()
        | F::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING.bits()
        | F::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR.bits()) as u16;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dom, Selector};

    fn features_of(text: &str) -> Vec<(String, Reach)> {
        let mut out = Vec::new();
        for s in Selector::parse_list(text).unwrap() {
            s.invalidation_features(&mut |f, r| out.push((format!("{f:?}"), r)));
        }
        out
    }

    #[test]
    fn a_feature_reaches_through_the_combinator_to_its_right() {
        assert_eq!(features_of(".a"), vec![("Class(\"a\")".into(), REACH_SELF)]);
        assert_eq!(
            features_of(".a .b > #C"),
            vec![("Id(\"c\")".into(), REACH_SELF), ("Class(\"b\")".into(), REACH_SUBTREE), ("Class(\"a\")".into(), REACH_SUBTREE)]
        );
        assert_eq!(
            features_of(".a + .b .c"),
            vec![("Class(\"c\")".into(), REACH_SELF), ("Class(\"b\")".into(), REACH_SUBTREE), ("Class(\"a\")".into(), REACH_LATER_SIBLINGS)]
        );
        assert_eq!(
            features_of(".a .b ~ .c"),
            vec![("Class(\"c\")".into(), REACH_SELF), ("Class(\"b\")".into(), REACH_LATER_SIBLINGS), ("Class(\"a\")".into(), REACH_SUBTREE)]
        );
        assert_eq!(features_of("[Data-X=y]::before"), vec![("Attribute(\"data-x\")".into(), REACH_SELF)]);
    }

    #[test]
    fn a_nested_list_takes_the_reach_of_its_compound_unless_its_own_is_wider() {
        assert_eq!(features_of(":is(.a, .b) .c")[1..], [("Class(\"a\")".into(), REACH_SUBTREE), ("Class(\"b\")".into(), REACH_SUBTREE)]);
        assert_eq!(features_of(".c:not(.a + .b)")[1..], [("Class(\"b\")".into(), REACH_SELF), ("Class(\"a\")".into(), REACH_LATER_SIBLINGS)]);
        assert_eq!(features_of(":is(.a + .b) .c")[2], ("Class(\"a\")".into(), REACH_LATER_SIBLINGS));
    }

    #[test]
    fn state_pseudo_classes_are_named_by_what_they_read_and_has_and_shadow_are_flagged() {
        assert_eq!(features_of("li:hover > ul"), vec![("Pseudo(Hover)".into(), REACH_SUBTREE)]);
        assert_eq!(features_of(":checked + label"), vec![("Pseudo(FormState(2))".into(), REACH_LATER_SIBLINGS)]);
        assert_eq!(features_of("a:link"), vec![("Attribute(\"href\")".into(), REACH_SELF)]);
        assert_eq!(
            features_of(".x:has(.y) .z"),
            vec![
                ("Class(\"z\")".into(), REACH_SELF),
                ("Class(\"x\")".into(), REACH_SUBTREE),
                ("Has(Class(\"x\"))".into(), REACH_SUBTREE),
                ("InHas(Subject(Class(\"y\")))".into(), REACH_SUBTREE),
                ("InHas(Class(\"y\"))".into(), REACH_SUBTREE)
            ]
        );
        let sibling = features_of("a:has(+ b.q)");
        assert!(sibling.iter().any(|(f, _)| f == "HasSibling") && sibling.iter().any(|(f, _)| f == "InHas(Structural)"), "{sibling:?}");
        assert!(features_of("ul:has(li:first-child)").iter().any(|(f, _)| f == "InHas(Structural)"));
        assert!(!features_of("ul:has(> li.a)").iter().any(|(f, _)| f == "InHas(Structural)"));
        assert!(features_of(":host(.a) .b").iter().any(|(f, _)| f == "Shadow"));
    }

    #[test]
    fn matching_under_the_recorder_flags_the_parent_of_a_positional_test() {
        let dom = Dom::parse("<html><body><ul id='u'><li id='a'></li><li id='b'></li></ul><p id='p'></p></body></html>");
        let ul = dom.get_element_by_id("u").unwrap();
        let b = dom.get_element_by_id("b").unwrap();
        let p = dom.get_element_by_id("p").unwrap();
        let nth = &Selector::parse_list("li:nth-child(2)").unwrap()[0];
        let empty = &Selector::parse_list("p:empty").unwrap()[0];
        let (hit, recorded) = record_selector_flags(vec![0; dom.len()], || {
            crate::with_match_caches(|c| {
                nth.matches_in_tree_cached(&dom, b, None, None, None, None, c) && empty.matches_in_tree_cached(&dom, p, None, None, None, None, c)
            })
        });
        assert!(hit);
        assert!(recorded[ul as usize] & LATER_OR_ALL != 0, "{recorded:?}");
        assert!(recorded[p as usize] & flags::EMPTY != 0, "{recorded:?}");
        assert_eq!(recorded.iter().filter(|&&f| f != 0).count(), 2, "only the two");
        assert!(!recording(), "no recorder open");
    }

    const LATER_OR_ALL: u16 = flags::LATER_CHILDREN | flags::ALL_CHILDREN;

    #[test]
    fn the_subject_is_the_strongest_key_of_the_rightmost_compound() {
        let subject = |t: &str| Selector::parse_list(t).unwrap()[0].invalidation_subject();
        assert_eq!(subject(".a .B"), Subject::Class("b".into()));
        assert_eq!(subject("div.x#Y[data-k]"), Subject::Id("y".into()));
        assert_eq!(subject(".a LI[open]"), Subject::Tag("li".into()));
        assert_eq!(subject(".a [data-k]"), Subject::Attribute("data-k".into()));
        assert_eq!(subject(".a > *"), Subject::Any);
        assert_eq!(subject(".a :not(.b)"), Subject::Any);
        assert_eq!(subject(".a p.q::before"), Subject::Class("q".into()));
        assert_eq!(subject(".a > :is(.b, li)"), Subject::OneOf(vec![Subject::Class("b".into()), Subject::Tag("li".into())]));
        assert_eq!(subject(".a > :is(.b, *)"), Subject::Any);
        let dom = Dom::parse("<p id='P' class='q r' data-k=1>x</p>");
        let p = dom.get_element_by_id("P").unwrap();
        for (s, yes) in [(".r", true), ("#p", true), ("p", true), ("[data-k]", true), (".z", false), ("li", false)] {
            assert_eq!(dom.has_subject_key(p, &subject(s)), yes, "{s}");
        }
    }

    /// A keyless subject is bounded by a keyed ANCESTOR, across descendant and child
    /// combinators (and the siblings between, which share the parent), or by `:root`.
    #[test]
    fn a_keyless_subject_is_bounded_by_a_keyed_ancestor_or_root() {
        let first = |t: &str| Selector::parse_list(t).unwrap().remove(0);
        let ancestor = |t: &str| first(t).invalidation_ancestor();
        assert_eq!(ancestor(".menu *"), Some(Subject::Class("menu".into())));
        assert_eq!(ancestor("#banner h3 *"), Some(Subject::Id("banner".into())), "the strongest key on the way");
        assert_eq!(ancestor(".list > :first-child"), Some(Subject::Class("list".into())));
        assert_eq!(ancestor(".x .y ~ *"), Some(Subject::Class("x".into())), "a sibling's ancestor is the subject's");
        assert_eq!(ancestor(".a *::before"), Some(Subject::Class("a".into())));
        assert_eq!(ancestor(".c ~ *"), None, "a sibling is not an ancestor");
        assert_eq!(ancestor("* *"), None);
        assert_eq!(ancestor(":target"), None);
        let root = |t: &str| first(t).invalidation_subject_is_root();
        assert!(root(":root"));
        assert!(root(":root::before"));
        assert!(root(".a :root:not(.b)"));
        assert!(!root(":root .a"));
        assert!(!root("*"));
    }
}
