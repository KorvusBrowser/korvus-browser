//! Form-control STATE and the questions asked of it: checkedness, selectedness,
//! `indeterminate`, custom validity, "actually disabled", and constraint validation.
//!
//! # Why this lives in the arena
//!
//! Two very different readers need the same answer. Script reads it through IDL (`input.checked`,
//! `option.selected`, `input.validity`, `new FormData(form)`), and the CASCADE reads it through
//! selectors (`:checked`, `:default`, `:indeterminate`, `:disabled`, `:placeholder-shown`, `:valid`,
//! `:in-range`, ...). The selector engine sees only a [`Dom`], so the state has to be here -- and it
//! was not: checkedness sat in an OpState map, selectedness and `indeterminate` in V8 private slots,
//! and `:checked` read the `checked` ATTRIBUTE, so a page that opened a menu with
//! `input.checked = true` (kubernetes.io's docs nav, L19 E16) kept it closed. Every mutator bumps
//! [`Dom::revision`], so a state change invalidates matching exactly as an attribute write does.
//!
//! # The model
//!
//! HTML's checkedness/selectedness are a value plus a DIRTY flag: until script assigns, the value
//! follows the content attribute; once assigned, attribute writes stop moving it. [`ControlState`]
//! stores the value only when it has parted from the attribute (`None` = "whatever the attribute
//! says"), so an element nobody touched costs nothing. The two "only one may be on" rules -- a radio
//! group, a single-selection `<select>` -- are applied eagerly by the IDL setters (the others are
//! turned off) and lazily when read (of several that are on, the LAST in tree order wins), which is
//! what the parser's `<input type=radio checked>` twice in one group needs: Chrome 150 checks the
//! second.
//!
//! Everything below was asked of stock Chrome 150.0.7871.46; the probe is
//! `core/korvus-runtime/tests/stylesheet_state_live.rs`'s forms section.

use crate::tree::{Dom, NodeId};

/// The per-element state a form control carries that is NOT a content attribute.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControlState {
    /// Checkedness when it differs from "has a `checked` attribute" (`None`: it does not).
    pub checked: Option<bool>,
    /// The dirty checkedness flag: script assigned `checked`, so the attribute no longer speaks.
    pub dirty_checked: bool,
    /// Selectedness when it differs from "has a `selected` attribute".
    pub selected: Option<bool>,
    /// The option's dirtiness.
    pub dirty_selected: bool,
    /// `input.indeterminate`.
    pub indeterminate: bool,
    /// `setCustomValidity(message)`; empty means no custom error.
    pub custom_validity: String,
    /// How many files a `type=file` input holds.
    pub file_count: u32,
    /// The API value once script has assigned one ("the dirty value flag" is this being `Some`): an
    /// `<input>`'s `value` IDL attribute and a `<textarea>`'s raw value part company with the markup
    /// the moment they are written, which is why `input.value = "2001-01-03"` does not move the step
    /// base (Chrome 150: with `step` and no `min`, that value IS step-mismatched).
    pub value: Option<String>,
    /// This element is a FORM-ASSOCIATED CUSTOM ELEMENT: its definition declares
    /// `static formAssociated = true` and `attachInternals()` has run on it. A FACE is a listed,
    /// labelable, submittable element and a constraint-validation candidate, so every question below
    /// that dispatches on `tag_name` has to be told about it -- the tag is whatever the page chose.
    pub form_associated_custom: bool,
    /// The flags `ElementInternals.setValidity(flags, message)` last set, and that message. A FACE
    /// has NO computed constraints of its own: these are the whole of its validity, which is why
    /// `customError` is false until `setValidity` sets it and `setCustomValidity` has no part in it.
    pub face_validity: Validity,
    pub face_message: String,
}

/// The constraint-validation flags `ValidityState` reports, computed from current state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Validity {
    pub value_missing: bool,
    pub type_mismatch: bool,
    pub pattern_mismatch: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub step_mismatch: bool,
    pub custom_error: bool,
    /// The three a UA can only learn from a real control a user typed into -- and that
    /// `ElementInternals.setValidity()` can set outright, which is why they are fields and not a
    /// hardcoded `false` in the `ValidityState` getter. No `<input>` ever sets them here.
    pub too_long: bool,
    pub too_short: bool,
    pub bad_input: bool,
}

impl Validity {
    pub fn valid(&self) -> bool {
        !(self.value_missing
            || self.type_mismatch
            || self.pattern_mismatch
            || self.range_underflow
            || self.range_overflow
            || self.step_mismatch
            || self.custom_error
            || self.too_long
            || self.too_short
            || self.bad_input)
    }
}

// ── the input type and what applies to it ──────────────────────────────────────────────────────

/// `type`'s keywords: an absent or unknown `type` IS `text`.
pub const INPUT_TYPES: [&str; 22] = [
    "hidden", "text", "search", "tel", "url", "email", "password", "date", "month", "week", "time",
    "datetime-local", "number", "range", "color", "checkbox", "radio", "file", "submit", "image",
    "reset", "button",
];

/// `min` / `max` / `step` and the range pseudo-classes apply to the numeric and temporal types.
pub fn supports_number(ty: &str) -> bool {
    matches!(ty, "date" | "month" | "week" | "time" | "datetime-local" | "number" | "range")
}
/// `pattern` applies to the types whose value is a free-form string.
pub fn supports_pattern(ty: &str) -> bool {
    matches!(ty, "text" | "search" | "url" | "tel" | "email" | "password")
}
/// `required` applies to everything except the button-ish and decorative types.
pub fn supports_required(ty: &str) -> bool {
    !matches!(ty, "hidden" | "range" | "color" | "submit" | "image" | "reset" | "button")
}
/// The types barred from constraint validation outright.
pub fn barred_from_validation(ty: &str) -> bool {
    matches!(ty, "hidden" | "reset" | "button")
}
/// `readonly` applies (and so bars validation) only on these.
fn supports_readonly(ty: &str) -> bool {
    matches!(
        ty,
        "text" | "search" | "url" | "tel" | "email" | "password" | "date" | "month" | "week" | "time" | "datetime-local" | "number"
    )
}
/// `placeholder` is shown only on these (Chrome 150: a `type=number` shows it, a checkbox does not).
fn supports_placeholder(ty: &str) -> bool {
    matches!(ty, "text" | "search" | "url" | "tel" | "email" | "password" | "number")
}

// ── number and date conversions (HTML §4.10.5.4), pure string <-> number ───────────────────────

/// An HTML "valid floating-point number" -- NOT JS `Number()`: "" is NaN, "0x10" is NaN, and
/// "Infinity"/"NaN" are not numbers.
pub fn parse_html_number(s: &str) -> f64 {
    if s.is_empty() {
        return f64::NAN;
    }
    let lower = s.to_ascii_lowercase();
    if lower.contains("inf") || lower.contains("nan") || lower.contains('x') {
        return f64::NAN;
    }
    s.parse::<f64>().unwrap_or(f64::NAN)
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's `days_from_civil`).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
/// The inverse (`civil_from_days`).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_uint(s: &str, len: usize) -> Option<i64> {
    if s.len() != len || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}
/// "YYYY-MM-DD" -> days since the epoch.
pub fn parse_date_days(s: &str) -> Option<i64> {
    let (y, rest) = s.split_once('-')?;
    let (m, d) = rest.split_once('-')?;
    if y.len() < 4 || !y.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (y, m, d) = (y.parse::<i64>().ok()?, parse_uint(m, 2)?, parse_uint(d, 2)?);
    if !(1..=12).contains(&m) || d < 1 {
        return None;
    }
    // A day number is valid only if it round-trips (rejects 2021-02-30 without a month-length table).
    let days = days_from_civil(y, m, d);
    if civil_from_days(days) != (y, m, d) {
        return None;
    }
    Some(days)
}
/// "HH:MM", "HH:MM:SS" or "HH:MM:SS.sss" -> milliseconds since midnight.
pub fn parse_time_ms(s: &str) -> Option<f64> {
    let mut it = s.split(':');
    let h = parse_uint(it.next()?, 2)?;
    let mi = parse_uint(it.next()?, 2)?;
    if h > 23 || mi > 59 {
        return None;
    }
    let mut ms = (h * 3_600_000 + mi * 60_000) as f64;
    if let Some(sec) = it.next() {
        let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
        let sv = parse_uint(whole, 2)?;
        if sv > 59 {
            return None;
        }
        ms += (sv * 1000) as f64;
        if !frac.is_empty() {
            if frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            ms += format!("0.{frac}").parse::<f64>().ok()? * 1000.0;
        }
    }
    if it.next().is_some() {
        return None;
    }
    Some(ms)
}
/// "YYYY-Www" -> milliseconds of the Monday that starts that ISO week.
pub fn parse_week_ms(s: &str) -> Option<f64> {
    let (y, w) = s.split_once("-W")?;
    if y.len() < 4 || !y.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (y, w) = (y.parse::<i64>().ok()?, parse_uint(w, 2)?);
    if !(1..=53).contains(&w) {
        return None;
    }
    // ISO 8601: week 1 contains January 4th; weeks start on Monday.
    let jan4 = days_from_civil(y, 1, 4);
    let dow = (jan4 + 3).rem_euclid(7); // 0 = Monday (1970-01-01 was a Thursday, index 3)
    Some(((jan4 - dow + (w - 1) * 7) * 86_400_000) as f64)
}

/// "convert a string to a number" (HTML §4.10.5.4), per type. NaN when the value is not conforming.
pub fn value_to_number(ty: &str, s: &str) -> f64 {
    match ty {
        "number" | "range" => parse_html_number(s),
        "date" => parse_date_days(s).map(|d| (d * 86_400_000) as f64).unwrap_or(f64::NAN),
        // A month's number is a MONTH COUNT since 1970-01, not a millisecond count.
        "month" => {
            let Some((y, m)) = s.split_once('-') else { return f64::NAN };
            if y.len() < 4 || !y.bytes().all(|b| b.is_ascii_digit()) {
                return f64::NAN;
            }
            match (y.parse::<i64>(), parse_uint(m, 2)) {
                (Ok(y), Some(m)) if (1..=12).contains(&m) => ((y - 1970) * 12 + (m - 1)) as f64,
                _ => f64::NAN,
            }
        }
        "week" => parse_week_ms(s).unwrap_or(f64::NAN),
        "time" => parse_time_ms(s).unwrap_or(f64::NAN),
        "datetime-local" => {
            let Some((d, t)) = s.split_once(['T', ' ']) else { return f64::NAN };
            match (parse_date_days(d), parse_time_ms(t)) {
                (Some(d), Some(t)) => (d * 86_400_000) as f64 + t,
                _ => f64::NAN,
            }
        }
        _ => f64::NAN,
    }
}

/// The type's "step scale factor" -- the unit one `step="1"` denotes, in the type's own number space.
pub fn step_scale(ty: &str) -> f64 {
    match ty {
        "date" => 86_400_000.0,
        "week" => 604_800_000.0,
        "time" | "datetime-local" => 1000.0,
        _ => 1.0,
    }
}
/// The type's "default step".
pub fn default_step(ty: &str) -> f64 {
    if ty == "time" || ty == "datetime-local" {
        60.0
    } else {
        1.0
    }
}

/// A "valid e-mail address" per HTML's own ABNF, which is deliberately WILLFULLY non-RFC-compliant.
pub fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    let atext = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~-".contains(c);
    if !local.split('.').all(|p| !p.is_empty() && p.chars().all(atext)) {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// One `OpaqueRange` (Chrome 152): offsets into its control's value in UTF-16 code units, and the control it
/// follows until it is disconnected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpaqueRange {
    pub element: Option<NodeId>,
    pub start: u32,
    pub end: u32,
}

impl OpaqueRange {
    /// `OpaqueRange::UpdateOffsetsForTextChange`, verbatim: a pure insertion keeps a collapsed range before the
    /// text, extends a range it lands at the start of and leaves one it lands at the end of; deleting the whole
    /// old value collapses to `[0, 0]`; otherwise each offset before the change stays, one inside the deleted
    /// part moves to its start, one after it shifts, clamped to the new length, a crossed range collapsing up.
    pub fn update_for_text_change(&mut self, change: u32, deleted: u32, inserted: u32, value_len: u32) {
        if self.element.is_none() || (deleted == 0 && inserted == 0) {
            return;
        }
        let (pre_start, pre_end) = (self.start, self.end);
        if deleted == 0 && inserted > 0 {
            if pre_start == pre_end && change == pre_start {
                return;
            }
            if pre_start != pre_end && change == pre_start {
                self.end = pre_end + inserted;
                return;
            }
            if pre_start != pre_end && change == pre_end {
                return;
            }
        }
        if deleted > 0 && change == 0 && pre_end <= deleted {
            self.start = 0;
            self.end = 0;
            return;
        }
        let change_end = change + deleted;
        let moved = |pos: u32| -> u32 {
            if pos < change {
                pos
            } else if pos < change_end {
                change
            } else {
                pos - deleted + inserted
            }
        };
        let start = moved(pre_start).min(value_len);
        let end = moved(pre_end).min(value_len).max(start);
        self.start = start;
        self.end = end;
    }
}

// ── the Dom's side ──────────────────────────────────────────────────────────────────────────────

impl Dom {
    /// Is `id` an HTML element named `tag`?
    fn is_html(&self, id: NodeId, tag: &str) -> bool {
        self.is_html_ns(id) && self.tag_name(id) == Some(tag)
    }

    fn has_attr(&self, id: NodeId, name: &str) -> bool {
        self.get_attribute(id, name).is_some()
    }

    /// The state script has given `id`, if any.
    pub fn control_state(&self, id: NodeId) -> Option<&ControlState> {
        self.controls.get(&id)
    }

    fn control_mut(&mut self, id: NodeId) -> &mut ControlState {
        self.revision += 1;
        self.journal.push(crate::journal::Change::State { node: Some(id) });
        self.controls.entry(id).or_default()
    }

    /// An `<input>`'s `type` keyword, `text` when absent or unknown.
    pub fn input_type(&self, id: NodeId) -> &'static str {
        let Some(raw) = self.get_attribute(id, "type") else { return "text" };
        let lower = raw.trim_matches(|c: char| c.is_ascii_whitespace()).to_ascii_lowercase();
        INPUT_TYPES.iter().find(|t| **t == lower).copied().unwrap_or("text")
    }

    /// A control's API value: the value script assigned if it has ([`ControlState::value`]), else the
    /// markup's -- an `<input>`'s `value` attribute, a `<textarea>`'s child text, a `<select>`'s
    /// selected option's value.
    pub fn control_value(&self, id: NodeId) -> String {
        if let Some(v) = self.control_state(id).and_then(|s| s.value.clone()) {
            return v;
        }
        match self.tag_name(id) {
            Some("textarea") => self.text_content(id),
            Some("select") => self.selected_options(id).first().map(|&o| self.option_value(o)).unwrap_or_default(),
            _ => self.get_attribute(id, "value").unwrap_or("").to_string(),
        }
    }

    /// `input.value = v` / `textarea.value = v`: the API value, with the dirty value flag set. The
    /// content attribute is NOT written -- Chrome leaves `defaultValue` where it was.
    ///
    /// The control's `OpaqueRange`s follow the change (Chrome 152, `TextControlElement::ApplyOpaqueRangeUpdate`):
    /// a programmatic write is diffed over the whole old value, a `setRangeText` one over the range it replaced
    /// ([`Dom::set_value_edit_bounds`]).
    pub fn set_control_value(&mut self, id: NodeId, value: &str) {
        let bounds = self.value_edit_bounds.take().filter(|b| b.0 == id);
        let old = if self.opaque_by_element.contains_key(&id) { Some(self.control_value(id)) } else { None };
        self.control_mut(id).value = Some(value.to_string());
        if let Some(old) = old {
            let old: Vec<u16> = old.encode_utf16().collect();
            let (s, e) = bounds.map(|b| (b.1, b.2)).unwrap_or((0, old.len() as u32));
            self.apply_opaque_range_update(id, &old, s, e);
        }
    }

    /// The next [`Dom::set_control_value`] of `id` replaced `[start, end)` of the old value (`setRangeText`).
    pub fn set_value_edit_bounds(&mut self, id: NodeId, start: u32, end: u32) {
        self.value_edit_bounds = Some((id, start, end));
    }

    // ── OpaqueRange (Chrome 152, `core/dom/opaque_range.cc`) ──

    /// `createValueRange(start, end)` after its checks: a range over `[start, end)` of `id`'s value, connected.
    pub fn create_opaque_range(&mut self, id: NodeId, start: u32, end: u32) -> u32 {
        let rid = self.opaque_ranges.len() as u32;
        self.opaque_ranges.push(OpaqueRange { element: Some(id), start, end });
        let ty = self.input_type_or_textarea(id);
        self.opaque_by_element.entry(id).or_insert_with(|| (Vec::new(), ty)).0.push(rid);
        rid
    }

    /// The range `rid`, if it was made.
    pub fn opaque_range(&self, rid: u32) -> Option<OpaqueRange> {
        self.opaque_ranges.get(rid as usize).copied()
    }

    /// `OpaqueRange.disconnect()`: it stops following its control and reads `[0, 0]`.
    pub fn disconnect_opaque_range(&mut self, rid: u32) {
        let Some(r) = self.opaque_ranges.get_mut(rid as usize) else { return };
        if let Some(el) = r.element.take() {
            if let Some((ids, _)) = self.opaque_by_element.get_mut(&el) {
                ids.retain(|&x| x != rid);
                if ids.is_empty() {
                    self.opaque_by_element.remove(&el);
                }
            }
        }
        r.start = 0;
        r.end = 0;
    }

    fn disconnect_all_opaque_ranges(&mut self, el: NodeId) {
        let ids = self.opaque_by_element.get(&el).map(|(ids, _)| ids.clone()).unwrap_or_default();
        for rid in ids {
            self.disconnect_opaque_range(rid);
        }
    }

    fn input_type_or_textarea(&self, id: NodeId) -> &'static str {
        if self.tag_name(id) == Some("textarea") { "textarea" } else { self.input_type(id) }
    }

    /// `HTMLInputElement::UpdateType`: any type change of a selectable input disconnects its ranges.
    pub(crate) fn opaque_ranges_type_changed(&mut self, id: NodeId) {
        let Some(&(_, made_under)) = self.opaque_by_element.get(&id) else { return };
        if self.input_type_or_textarea(id) != made_under {
            self.disconnect_all_opaque_ranges(id);
        }
    }

    /// `TextControlElement::RemovedFrom`: a control leaving a CONNECTED tree (itself or with an ancestor)
    /// disconnects its ranges. Called by `unlink` before `child` comes out.
    pub(crate) fn opaque_ranges_subtree_removed(&mut self, child: NodeId) {
        if !self.is_connected(child) {
            return;
        }
        let els: Vec<NodeId> = self.opaque_by_element.keys().copied().collect();
        for el in els {
            let mut cur = Some(el);
            while let Some(n) = cur {
                if n == child {
                    self.disconnect_all_opaque_ranges(el);
                    break;
                }
                cur = self.at(n).parent.or_else(|| self.shadow_host_of(n));
            }
        }
    }

    /// `ApplyOpaqueRangeUpdate`: the old and new values' common prefix (no further than the edit's start) and
    /// suffix (no further than its end) leave one delete + insert at the prefix.
    fn apply_opaque_range_update(&mut self, id: NodeId, old: &[u16], sel_start: u32, sel_end: u32) {
        let new: Vec<u16> = self.control_value(id).encode_utf16().collect();
        if old == new.as_slice() {
            return;
        }
        let (ol, nl) = (old.len(), new.len());
        let (mut s, mut e) = ((sel_start as usize).min(ol), (sel_end as usize).min(ol));
        if s > e {
            std::mem::swap(&mut s, &mut e);
        }
        let mut prefix = 0;
        while prefix < ol && prefix < nl && old[prefix] == new[prefix] {
            prefix += 1;
        }
        prefix = prefix.min(s);
        let limit = (ol - prefix).min(nl - prefix).min(ol - e);
        let mut suffix = 0;
        while suffix < limit && old[ol - 1 - suffix] == new[nl - 1 - suffix] {
            suffix += 1;
        }
        let deleted = (ol - prefix - suffix) as u32;
        let inserted = (nl - prefix - suffix) as u32;
        if deleted == 0 && inserted == 0 {
            return;
        }
        let ids = self.opaque_by_element.get(&id).map(|(ids, _)| ids.clone()).unwrap_or_default();
        for rid in ids {
            if let Some(r) = self.opaque_ranges.get_mut(rid as usize) {
                r.update_for_text_change(prefix as u32, deleted, inserted, nl as u32);
            }
        }
    }

    /// An `<option>`'s value: its `value` attribute, else its text stripped and collapsed.
    pub fn option_value(&self, id: NodeId) -> String {
        match self.get_attribute(id, "value") {
            Some(v) => v.to_string(),
            None => self.text_content(id).split(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0C' | '\r')).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "),
        }
    }

    // ── checkedness ──

    /// An `<input>`'s checkedness, with a radio group's "only the last one on" applied.
    pub fn checkedness(&self, id: NodeId) -> bool {
        if !self.base_checked(id) {
            return false;
        }
        if self.input_type(id) != "radio" || self.control_state(id).is_some_and(|s| s.dirty_checked) {
            return true;
        }
        // A radio checked only by its attribute loses to a LATER radio of its group that is on too.
        let group = self.radio_group(id);
        let me = group.iter().position(|&r| r == id);
        me.is_none_or(|i| !group[i + 1..].iter().any(|&r| self.base_checked(r) && !self.control_state(r).is_some_and(|s| s.dirty_checked)))
    }

    fn base_checked(&self, id: NodeId) -> bool {
        self.control_state(id).and_then(|s| s.checked).unwrap_or_else(|| self.has_attr(id, "checked"))
    }

    /// `input.checked = b`: set checkedness and its dirty flag; a radio turned on turns the rest of its
    /// group off (Chrome 150: `r2.checked = true` makes `r1.checked` false).
    pub fn set_checkedness(&mut self, id: NodeId, b: bool) {
        let s = self.control_mut(id);
        s.checked = Some(b);
        s.dirty_checked = true;
        if b && self.input_type(id) == "radio" {
            for r in self.radio_group(id) {
                if r != id {
                    self.control_mut(r).checked = Some(false);
                }
            }
        }
    }

    /// The `checked` content attribute of `id` changed. While the dirty flag is clear the
    /// checkedness follows it, and a radio it turned on turns the rest of its group off.
    pub(crate) fn checked_attribute_changed(&mut self, id: NodeId) {
        if self.control_state(id).is_some_and(|s| s.dirty_checked) {
            return;
        }
        if let Some(s) = self.controls.get_mut(&id) {
            s.checked = None;
        }
        if self.has_attr(id, "checked") && self.is_html(id, "input") && self.input_type(id) == "radio" {
            for r in self.radio_group(id) {
                if r != id {
                    self.control_mut(r).checked = Some(false);
                }
            }
        }
    }

    /// HTML's form RESET, after the `reset` event was not cancelled: every resettable element `form`
    /// owns -- `<input>`, `<textarea>`, `<select>` (and its options) -- forgets what script and the
    /// user did to it, so its value, checkedness and selectedness are the markup's again
    /// (Chrome 150: a changed text value reads `orig` and an unchecked `checked` box reads true after
    /// `form.reset()`).
    pub fn reset_form(&mut self, form: NodeId) {
        let root = self.root_of(form);
        let owned: Vec<NodeId> = self
            .descendant_elements(root)
            .into_iter()
            .filter(|&id| (self.is_html(id, "input") || self.is_html(id, "textarea") || self.is_html(id, "select")) && self.form_owner(id) == Some(form))
            .collect();
        for id in owned {
            if self.is_html(id, "select") {
                for o in self.list_of_options(id) {
                    self.controls.remove(&o);
                }
            }
            self.controls.remove(&id);
        }
        self.revision += 1;
        self.journal.push(crate::journal::Change::State { node: Some(form) });
    }

    /// The form owner of a form-associated element: the `<form>` its `form` attribute names in the
    /// same tree (none when that is not a form), else its nearest `<form>` ancestor.
    pub fn form_owner(&self, id: NodeId) -> Option<NodeId> {
        if let Some(name) = self.get_attribute(id, "form") {
            let root = self.root_of(id);
            return self.get_element_by_id_in(root, name).filter(|&f| self.is_html(f, "form"));
        }
        let mut cur = self.parent(id);
        while let Some(p) = cur {
            if self.is_html(p, "form") {
                return Some(p);
            }
            cur = self.parent(p);
        }
        None
    }

    /// The radio button group of a radio `<input>`, in tree order, itself included: radios of the same
    /// tree and form owner whose `name` is the same non-empty string. A radio with no name is a group
    /// of one. (Chrome 150: a checked radio outside the form does not uncheck one inside it.)
    pub fn radio_group(&self, id: NodeId) -> Vec<NodeId> {
        let name = match self.get_attribute(id, "name") {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return vec![id],
        };
        let owner = self.form_owner(id);
        let root = self.root_of(id);
        let mut out: Vec<NodeId> = self
            .descendant_elements(root)
            .into_iter()
            .filter(|&r| {
                self.is_html(r, "input")
                    && self.input_type(r) == "radio"
                    && self.get_attribute(r, "name") == Some(name.as_str())
                    && self.form_owner(r) == owner
            })
            .collect();
        if out.is_empty() {
            out.push(id);
        }
        out
    }

    // ── selectedness ──

    /// The `<select>` whose list of options `option` is in: its parent, or its `<optgroup>` parent's.
    pub fn option_select(&self, option: NodeId) -> Option<NodeId> {
        let p = self.parent(option)?;
        if self.is_html(p, "select") {
            return Some(p);
        }
        let g = self.parent(p).filter(|_| self.is_html(p, "optgroup"))?;
        self.is_html(g, "select").then_some(g)
    }

    /// A `<select>`'s list of options: every `<option>` DESCENDANT, in tree order. Descendants rather
    /// than the spec's "children, and children of optgroup children", because that is what Chrome 150
    /// answers and what WPT's `the-select-element/select-value.html` ("option is child of div")
    /// asserts: an option moved into a `<div>` inside the select still carries the select's value.
    pub fn list_of_options(&self, select: NodeId) -> Vec<NodeId> {
        self.descendant_elements(select).into_iter().filter(|&o| self.is_html(o, "option")).collect()
    }

    /// Does this `<select>` show one option at a time and allow only one selected?
    fn single_select(&self, select: NodeId) -> bool {
        if self.has_attr(select, "multiple") {
            return false;
        }
        match self.get_attribute(select, "size").and_then(|s| s.trim().parse::<u32>().ok()) {
            Some(n) => n <= 1,
            None => true,
        }
    }

    fn base_selected(&self, id: NodeId) -> bool {
        self.control_state(id).and_then(|s| s.selected).unwrap_or_else(|| self.has_attr(id, "selected"))
    }

    /// Is `option` disabled: its own attribute, or its `<optgroup>` parent's.
    pub fn option_disabled(&self, option: NodeId) -> bool {
        self.has_attr(option, "disabled") || self.parent(option).is_some_and(|p| self.is_html(p, "optgroup") && self.has_attr(p, "disabled"))
    }

    /// The selected options of a `<select>`, in tree order, with the selectedness setting algorithm
    /// applied: a single-selection select with several on keeps the LAST; with none on, the first
    /// option that is not disabled (Chrome 150: `<option disabled>1<option>2` selects `2`).
    pub fn selected_options(&self, select: NodeId) -> Vec<NodeId> {
        let options = self.list_of_options(select);
        let on: Vec<NodeId> = options.iter().copied().filter(|&o| self.base_selected(o)).collect();
        if !self.single_select(select) {
            return on;
        }
        if let Some(&last) = on.last() {
            return vec![last];
        }
        options.into_iter().find(|&o| !self.option_disabled(o)).into_iter().collect()
    }

    /// An `<option>`'s selectedness.
    pub fn option_selectedness(&self, option: NodeId) -> bool {
        match self.option_select(option) {
            Some(select) if self.single_select(select) => self.selected_options(select).first() == Some(&option),
            _ => self.base_selected(option),
        }
    }

    /// `option.selected = b`: set selectedness and dirtiness; turned on inside a single-selection
    /// `<select>`, every other option of it goes off.
    pub fn set_option_selectedness(&mut self, option: NodeId, b: bool) {
        let s = self.control_mut(option);
        s.selected = Some(b);
        s.dirty_selected = true;
        if !b {
            return;
        }
        if let Some(select) = self.option_select(option).filter(|&s| self.single_select(s)) {
            for o in self.list_of_options(select) {
                if o != option {
                    self.control_mut(o).selected = Some(false);
                }
            }
        }
    }

    /// `select.selectedIndex = i`: the option at `i` on, every other off (none on for an index out of
    /// range).
    pub fn set_selected_index(&mut self, select: NodeId, index: i32) {
        for (i, o) in self.list_of_options(select).into_iter().enumerate() {
            self.control_mut(o).selected = Some(i as i32 == index);
        }
    }

    /// The `selected` content attribute of `id` changed: without dirtiness, selectedness follows it.
    pub(crate) fn selected_attribute_changed(&mut self, id: NodeId) {
        if self.control_state(id).is_some_and(|s| s.dirty_selected) {
            return;
        }
        if let Some(s) = self.controls.get_mut(&id) {
            s.selected = None;
        }
    }

    // ── the other script-set state ──

    pub fn indeterminate(&self, id: NodeId) -> bool {
        self.control_state(id).is_some_and(|s| s.indeterminate)
    }
    pub fn set_indeterminate(&mut self, id: NodeId, b: bool) {
        self.control_mut(id).indeterminate = b;
    }
    /// A popover's "showing" state -- what `:popover-open` matches.
    pub fn is_popover_open(&self, id: NodeId) -> bool {
        self.open_states.get(&id).is_some_and(|f| f & 1 != 0)
    }
    pub fn set_popover_open(&mut self, id: NodeId, b: bool) {
        self.set_open_state(id, 1, b);
    }
    /// A dialog's "is modal" flag -- what `:modal` matches.
    pub fn is_modal(&self, id: NodeId) -> bool {
        self.open_states.get(&id).is_some_and(|f| f & 2 != 0)
    }
    pub fn set_modal(&mut self, id: NodeId, b: bool) {
        self.set_open_state(id, 2, b);
    }
    fn set_open_state(&mut self, id: NodeId, bit: u8, b: bool) {
        let cur = self.open_states.get(&id).copied().unwrap_or(0);
        let next = if b { cur | bit } else { cur & !bit };
        if next == cur {
            return;
        }
        self.revision += 1;
        self.journal.push(crate::journal::Change::State { node: Some(id) });
        if next == 0 {
            self.open_states.remove(&id);
        } else {
            self.open_states.insert(id, next);
        }
    }
    pub fn custom_validity(&self, id: NodeId) -> &str {
        self.control_state(id).map_or("", |s| s.custom_validity.as_str())
    }
    pub fn set_custom_validity(&mut self, id: NodeId, message: &str) {
        self.control_mut(id).custom_validity = message.to_string();
    }
    pub fn file_count(&self, id: NodeId) -> u32 {
        self.control_state(id).map_or(0, |s| s.file_count)
    }
    pub fn set_file_count(&mut self, id: NodeId, n: u32) {
        self.control_mut(id).file_count = n;
    }

    /// Record whether `value` matches `pattern` in full. The pattern is an ECMAScript regular
    /// expression (the `v` flag) and only the script engine can evaluate one, so it answers and the
    /// arena remembers; [`Dom::validity`] reads the answer. Keyed by the two strings, so an answer is
    /// never stale -- only possibly absent, which applies no constraint.
    pub fn set_pattern_result(&mut self, pattern: &str, value: &str, matches: bool) {
        if self.pattern_results.get(&(pattern.to_string(), value.to_string())) != Some(&matches) {
            self.revision += 1;
            self.journal.push(crate::journal::Change::State { node: None });
            self.pattern_results.insert((pattern.to_string(), value.to_string()), matches);
        }
    }

    /// Has [`Dom::set_pattern_result`] been told about this pair?
    pub fn has_pattern_result(&self, pattern: &str, value: &str) -> bool {
        self.pattern_results.contains_key(&(pattern.to_string(), value.to_string()))
    }

    // ── disabled ──

    /// HTML's "actually disabled" for `button`/`input`/`select`/`textarea`/`fieldset` ("disabled"
    /// attribute, or inside a `<fieldset disabled>` but not inside that fieldset's first `<legend>`
    /// child), `optgroup` (its attribute) and `option` (its or its optgroup's). Chrome 150: an input in
    /// the first legend is enabled, one in a second legend is disabled, a nested fieldset is disabled.
    pub fn actually_disabled(&self, id: NodeId) -> bool {
        match self.tag_name(id) {
            Some("button" | "input" | "select" | "textarea" | "fieldset") if self.is_html_ns(id) => {
                self.has_attr(id, "disabled") || self.in_disabled_fieldset(id)
            }
            Some("optgroup") if self.is_html_ns(id) => self.has_attr(id, "disabled"),
            Some("option") if self.is_html_ns(id) => self.option_disabled(id),
            // A form-associated custom element is disabled by the same two rules as a built-in
            // control; its TAG carries no information, so it is asked of the element instead.
            _ if self.is_form_associated_custom(id) => self.has_attr(id, "disabled") || self.in_disabled_fieldset(id),
            _ => false,
        }
    }

    fn in_disabled_fieldset(&self, id: NodeId) -> bool {
        let mut child = id;
        let mut cur = self.parent(id);
        while let Some(p) = cur {
            if self.is_html(p, "fieldset") && self.has_attr(p, "disabled") {
                let first_legend = self.child_elements(p).into_iter().find(|&c| self.is_html(c, "legend"));
                if first_legend != Some(child) {
                    return true;
                }
            }
            child = p;
            cur = self.parent(p);
        }
        false
    }

    /// Does `:enabled`/`:disabled` apply to `id` at all?
    pub fn can_be_disabled(&self, id: NodeId) -> bool {
        self.is_html_ns(id)
            && (matches!(self.tag_name(id), Some("button" | "input" | "select" | "textarea" | "optgroup" | "option" | "fieldset"))
                || self.is_form_associated_custom(id))
    }

    // ── form-associated custom elements ──
    //
    // A FACE is a listed, labelable, submittable element whose TAG NAME says nothing: the page picks
    // it. So every question above and below that matched on `tag_name` has to ask the element instead,
    // and that is what the two accessors here are for. The flag is set by
    // `ElementInternals`'s `attachInternals()` (`core/html/element_internals.rs`), the one moment the
    // definition's `static formAssociated` is known to be true for THIS element.

    /// Is `id` a form-associated custom element?
    pub fn is_form_associated_custom(&self, id: NodeId) -> bool {
        self.control_state(id).map(|s| s.form_associated_custom).unwrap_or(false)
    }

    /// Mark `id` as one. Idempotent; `attachInternals()` runs once per element.
    pub fn set_form_associated_custom(&mut self, id: NodeId) {
        self.control_mut(id).form_associated_custom = true;
    }

    /// `ElementInternals.setValidity(flags, message)`: the WHOLE of a FACE's validity, replaced each
    /// call. An empty `flags` clears it, which is how a page reports "now valid".
    pub fn set_face_validity(&mut self, id: NodeId, v: Validity, message: &str) {
        let s = self.control_mut(id);
        s.face_validity = v;
        s.face_message = message.to_string();
    }

    /// `ElementInternals.validationMessage`: the message `setValidity` stored, or "" when the element
    /// is barred from constraint validation or satisfies its constraints (HTML, verbatim).
    pub fn face_validation_message(&self, id: NodeId) -> String {
        if !self.will_validate(id) || self.validity(id).valid() {
            return String::new();
        }
        self.control_state(id).map(|s| s.face_message.clone()).unwrap_or_default()
    }

    // ── constraint validation ──

    fn in_datalist(&self, id: NodeId) -> bool {
        let mut cur = self.parent(id);
        while let Some(p) = cur {
            if self.is_html(p, "datalist") {
                return true;
            }
            cur = self.parent(p);
        }
        false
    }

    /// `willValidate`: is `id` a candidate for constraint validation?
    pub fn will_validate(&self, id: NodeId) -> bool {
        if !self.is_html_ns(id) {
            return false;
        }
        let candidate = match self.tag_name(id) {
            Some("input") => {
                let ty = self.input_type(id);
                !barred_from_validation(ty) && !(self.has_attr(id, "readonly") && supports_readonly(ty))
            }
            // A reset or plain button is barred; a submit button (the missing and invalid default) is not.
            Some("button") => self.is_submit_button(id),
            Some("select") => true,
            Some("textarea") => !self.has_attr(id, "readonly"),
            // A form-associated custom element is a candidate unless its `readonly` attribute is
            // specified. The other three barring conditions -- the `disabled` attribute, a disabled
            // ancestor fieldset, a datalist ancestor -- are the two shared clauses below, which is
            // why they are not repeated here.
            _ => self.is_form_associated_custom(id) && !self.has_attr(id, "readonly"),
        };
        candidate && !self.actually_disabled(id) && !self.in_datalist(id)
    }

    /// The constraint-validation flags of a submittable element, from current state. `tooLong`,
    /// `tooShort` and `badInput` are defined over what a USER typed and are never set here.
    pub fn validity(&self, id: NodeId) -> Validity {
        let custom_error = !self.custom_validity(id).is_empty();
        match self.tag_name(id) {
            Some("input") if self.is_html_ns(id) => self.input_validity(id, custom_error),
            Some("select") if self.is_html_ns(id) => {
                let value_missing = self.has_attr(id, "required") && {
                    let selected = self.selected_options(id);
                    selected.is_empty() || (self.single_select(id) && selected.first() == self.placeholder_label_option(id).as_ref())
                };
                Validity { value_missing, custom_error, ..Validity::default() }
            }
            Some("textarea") if self.is_html_ns(id) => {
                let value_missing = self.has_attr(id, "required") && self.control_value(id).is_empty();
                Validity { value_missing, custom_error, ..Validity::default() }
            }
            // A form-associated custom element has no constraints of its own: its validity is
            // exactly what `ElementInternals.setValidity()` last stored, `customError` included.
            // (WPT: "validity.customError should be false if not explicitly set via setValidity()".)
            _ if self.is_form_associated_custom(id) => {
                self.control_state(id).map(|s| s.face_validity).unwrap_or_default()
            }
            _ => Validity { custom_error, ..Validity::default() },
        }
    }

    /// A required single-selection `<select>`'s placeholder label option: its first option, when that
    /// option's value is empty and its parent is the select itself.
    fn placeholder_label_option(&self, select: NodeId) -> Option<NodeId> {
        let first = *self.list_of_options(select).first()?;
        (self.parent(first) == Some(select) && self.option_value(first).is_empty()).then_some(first)
    }

    fn input_validity(&self, id: NodeId, custom_error: bool) -> Validity {
        let ty = self.input_type(id);
        let value = self.control_value(id);
        let required = self.has_attr(id, "required") && supports_required(ty);
        let value_missing = match ty {
            // A radio is missing when ANY radio of its group is required and none is checked -- every
            // member of the group, required or not (Chrome 150).
            "radio" => {
                let group = self.radio_group(id);
                group.iter().any(|&r| self.has_attr(r, "required")) && !group.iter().any(|&r| self.checkedness(r))
            }
            "checkbox" => required && !self.checkedness(id),
            "file" => required && self.file_count(id) == 0,
            _ => required && value.is_empty(),
        };
        let type_mismatch = !value.is_empty()
            && match ty {
                "email" if self.has_attr(id, "multiple") => !value.split(',').all(|p| valid_email(p.trim())),
                "email" => !valid_email(&value),
                "url" => url::Url::parse(&value).is_err(),
                _ => false,
            };
        let pattern_mismatch = match self.get_attribute(id, "pattern") {
            Some(p) if supports_pattern(ty) && !value.is_empty() => {
                self.pattern_results.get(&(p.to_string(), value.clone())) == Some(&false)
            }
            _ => false,
        };
        let (mut range_underflow, mut range_overflow, mut step_mismatch) = (false, false, false);
        if supports_number(ty) {
            let n = value_to_number(ty, &value);
            if !n.is_nan() {
                if let Some(mn) = self.get_attribute(id, "min").map(|s| value_to_number(ty, s.trim())).filter(|v| !v.is_nan()) {
                    range_underflow = n < mn;
                }
                if let Some(mx) = self.get_attribute(id, "max").map(|s| value_to_number(ty, s.trim())).filter(|v| !v.is_nan()) {
                    range_overflow = n > mx;
                }
                if let Some(step) = self.allowed_step(id, ty) {
                    let base = self.step_base(id, ty);
                    let off = ((n - base) / step).fract().abs();
                    // Floating-point steps ("0.1") never land exactly, so compare against the spec's
                    // "integral multiple" with the tolerance a binary double forces on anyone.
                    step_mismatch = off > 1e-9 && off < 1.0 - 1e-9;
                }
            }
        }
        // too_long / too_short / bad_input stay at their default false: they are defined over what a
        // user typed into a real control, and only `ElementInternals.setValidity()` sets them here.
        Validity { value_missing, type_mismatch, pattern_mismatch, range_underflow, range_overflow, step_mismatch, custom_error, ..Validity::default() }
    }

    /// The allowed value step, already scaled. `None` means `step="any"`.
    pub fn allowed_step(&self, id: NodeId, ty: &str) -> Option<f64> {
        match self.get_attribute(id, "step") {
            Some(s) if s.trim().eq_ignore_ascii_case("any") => None,
            Some(s) => {
                let n = parse_html_number(s.trim());
                if n.is_finite() && n > 0.0 {
                    Some(n * step_scale(ty))
                } else {
                    Some(default_step(ty) * step_scale(ty))
                }
            }
            None => Some(default_step(ty) * step_scale(ty)),
        }
    }

    /// The "step base": the `min` attribute when it converts, else the `value` ATTRIBUTE when it
    /// converts, else 0. Chrome 150: `<input type=number step=2 value=3>` is not step-mismatched --
    /// its base is 3. (This engine's script-set value writes that attribute too, so a later
    /// `value = "4"` moves the base with it where Chrome's stays at 3.)
    pub fn step_base(&self, id: NodeId, ty: &str) -> f64 {
        ["min", "value"]
            .iter()
            .filter_map(|a| self.get_attribute(id, a))
            .map(|s| value_to_number(ty, s.trim()))
            .find(|n| !n.is_nan())
            .unwrap_or(0.0)
    }

    /// `:valid` (`Some(true)`) / `:invalid` (`Some(false)`) / neither (`None`). A candidate answers for
    /// itself; a `<form>` for every candidate whose form owner it is, and a `<fieldset>` for every
    /// candidate below it -- a disabled fieldset's are barred, so it is `:valid` (Chrome 150).
    pub fn validity_state(&self, id: NodeId) -> Option<bool> {
        if !self.is_html_ns(id) {
            return None;
        }
        match self.tag_name(id) {
            Some("form") => {
                let root = self.root_of(id);
                // The LISTED elements -- and a form-associated custom element is one, so a custom
                // control's `setValidity()` makes its owner form `:invalid` too.
                let listed = |n: NodeId| {
                    self.is_html_ns(n)
                        && (matches!(self.tag_name(n), Some("button" | "input" | "select" | "textarea"))
                            || self.is_form_associated_custom(n))
                };
                Some(
                    self.descendant_elements(root)
                        .into_iter()
                        .filter(|&n| listed(n) && self.form_owner(n) == Some(id) && self.will_validate(n))
                        .all(|n| self.validity(n).valid()),
                )
            }
            Some("fieldset") => Some(
                self.descendant_elements(id).into_iter().filter(|&n| self.will_validate(n)).all(|n| self.validity(n).valid()),
            ),
            _ if self.will_validate(id) => Some(self.validity(id).valid()),
            _ => None,
        }
    }

    // ── the other pseudo-classes' questions ──

    /// `:default`: a checkbox/radio with a `checked` attribute, an option with a `selected` attribute,
    /// or its form's default button (the first submit button in tree order whose form owner it is).
    pub fn is_default(&self, id: NodeId) -> bool {
        if !self.is_html_ns(id) {
            return false;
        }
        match self.tag_name(id) {
            Some("input") if matches!(self.input_type(id), "checkbox" | "radio") => self.has_attr(id, "checked"),
            Some("option") => self.has_attr(id, "selected"),
            Some("input" | "button") if self.is_submit_button(id) => {
                let Some(form) = self.form_owner(id) else { return false };
                let root = self.root_of(id);
                self.descendant_elements(root)
                    .into_iter()
                    .find(|&n| self.is_submit_button(n) && self.form_owner(n) == Some(form))
                    == Some(id)
            }
            _ => false,
        }
    }

    fn is_submit_button(&self, id: NodeId) -> bool {
        match self.tag_name(id) {
            Some("button") if self.is_html_ns(id) => {
                !matches!(self.get_attribute(id, "type").map(|t| t.trim().to_ascii_lowercase()).as_deref(), Some("reset" | "button"))
            }
            Some("input") if self.is_html_ns(id) => matches!(self.input_type(id), "submit" | "image"),
            _ => false,
        }
    }

    /// `:indeterminate`: a checkbox whose `indeterminate` is set, a radio none of whose group is
    /// checked, a `<progress>` with no `value`.
    pub fn is_indeterminate(&self, id: NodeId) -> bool {
        if !self.is_html_ns(id) {
            return false;
        }
        match self.tag_name(id) {
            Some("input") => match self.input_type(id) {
                "checkbox" => self.indeterminate(id),
                "radio" => !self.radio_group(id).iter().any(|&r| self.checkedness(r)),
                _ => false,
            },
            Some("progress") => !self.has_attr(id, "value"),
            _ => false,
        }
    }

    /// `:checked`: a checkbox's or radio's checkedness, an option's selectedness.
    pub fn is_checked(&self, id: NodeId) -> bool {
        if !self.is_html_ns(id) {
            return false;
        }
        match self.tag_name(id) {
            Some("input") => matches!(self.input_type(id), "checkbox" | "radio") && self.checkedness(id),
            Some("option") => self.option_selectedness(id),
            _ => false,
        }
    }

    /// `:placeholder-shown`: an input of a type that shows a placeholder, or a textarea, with a
    /// `placeholder` attribute (an empty one counts, Chrome 150) and an empty value.
    pub fn placeholder_shown(&self, id: NodeId) -> bool {
        if !self.is_html_ns(id) || !self.has_attr(id, "placeholder") {
            return false;
        }
        match self.tag_name(id) {
            Some("input") => supports_placeholder(self.input_type(id)) && self.control_value(id).is_empty(),
            Some("textarea") => self.control_value(id).is_empty(),
            _ => false,
        }
    }

    /// `:in-range` (`Some(true)`) / `:out-of-range` (`Some(false)`): a candidate input with range
    /// limitations -- a `range`, or a numeric/temporal type with a `min` or `max` that converts.
    pub fn range_state(&self, id: NodeId) -> Option<bool> {
        if !self.is_html(id, "input") || !self.will_validate(id) {
            return None;
        }
        let ty = self.input_type(id);
        if !supports_number(ty) {
            return None;
        }
        let converts = |a: &str| self.get_attribute(id, a).is_some_and(|s| !value_to_number(ty, s.trim()).is_nan());
        if ty != "range" && !converts("min") && !converts("max") {
            return None;
        }
        let v = self.validity(id);
        Some(!(v.range_underflow || v.range_overflow))
    }
}
