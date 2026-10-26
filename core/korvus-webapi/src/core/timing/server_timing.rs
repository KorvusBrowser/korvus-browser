//! `core/timing/server_timing.rs` -- `PerformanceServerTiming`, and the `Server-Timing` header it is read from
//! (a challenge provider's answers carry `Server-Timing: cfL4;desc="?proto=TCP&rtt=..."` -- its own measure of
//! the connection -- and its challenge reads it back off the navigation entry; korvus's `serverTiming` was
//! always `[]`, and `PerformanceServerTiming` a registry stub).
//!
//! The header as Blink parses it (`platform/network/http_parsers.cc` `ParseServerTimingHeader` over
//! `header_field_tokenizer.cc`, the response's `Server-Timing` fields combined): a metric is a token, then
//! anything up to the next `,` or `;` is skipped; each `;param[=token-or-quoted-string]` follows, `dur` and
//! `desc` (any case) taking their FIRST value -- `dur` through `StringToDouble`, 0 when that fails -- and the
//! list ends at the first item that does not start with a token. Chrome 150.0.7871.46, measured on 29 header
//! values (tests/fixtures/server-timing/): `a;dur=" 2"` is 2 and `a;dur="1.5 "` is 0, `Infinity`/`NaN`/`0x10`
//! are 0, `"q";dur=1` is no metric at all, `café` is the metric `caf`.
//!
//! On an entry: `serverTiming` is a NEW frozen array on every read, of the SAME `PerformanceServerTiming`
//! objects; a cross-origin resource without `Timing-Allow-Origin` has none. The interface's constructor is
//! illegal, its instances carry no own property.

use deno_core::v8;

/// One metric: `name`, `duration` (ms), `description`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Metric {
    pub name: String,
    pub duration: f64,
    pub description: String,
}

struct Tokenizer<'a> {
    s: &'a [u8],
    i: usize,
}

/// RFC 7230 token characters in Blink's `kNormal` mode (DEL is accepted, as Blink accepts it).
fn token_char(c: u8) -> bool {
    !(c >= 128 || c < 0x20 || b" ;\"(),<>@:\\/[]?=".contains(&c))
}

impl<'a> Tokenizer<'a> {
    fn new(s: &'a str) -> Self {
        let mut t = Tokenizer { s: s.as_bytes(), i: 0 };
        t.skip_ws();
        t
    }
    fn done(&self) -> bool {
        self.i >= self.s.len()
    }
    fn skip_ws(&mut self) {
        while !self.done() && (self.s[self.i] == b' ' || self.s[self.i] == b'\t') {
            self.i += 1;
        }
    }
    fn consume(&mut self, c: u8) -> bool {
        if self.done() || self.s[self.i] != c {
            return false;
        }
        self.i += 1;
        self.skip_ws();
        true
    }
    fn token(&mut self) -> Option<String> {
        let start = self.i;
        while !self.done() && token_char(self.s[self.i]) {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        let out = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        self.skip_ws();
        Some(out)
    }
    fn quoted(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = Vec::new();
        while !self.done() {
            match self.s[self.i] {
                b'"' => {
                    self.i += 1;
                    self.skip_ws();
                    return Some(String::from_utf8_lossy(&out).into_owned());
                }
                b'\\' => {
                    self.i += 1;
                    if self.done() {
                        return None;
                    }
                }
                _ => {}
            }
            out.push(self.s[self.i]);
            self.i += 1;
        }
        None
    }
    fn token_or_quoted(&mut self) -> Option<String> {
        if self.done() {
            None
        } else if self.s[self.i] == b'"' {
            self.quoted()
        } else {
            self.token()
        }
    }
    fn skip_to(&mut self, stops: &[u8]) {
        while !self.done() && !stops.contains(&self.s[self.i]) {
            self.i += 1;
        }
    }
}

/// WTF's `StringToDouble`: leading whitespace allowed, nothing after the number, finite only.
fn to_double(v: &str) -> Option<f64> {
    let t = v.trim_start_matches(|c: char| c.is_ascii_whitespace());
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) {
        return None;
    }
    t.parse::<f64>().ok().filter(|d| d.is_finite())
}

/// A response's `Server-Timing` fields (combined) as its metrics.
pub(crate) fn parse(value: &str) -> Vec<Metric> {
    let mut out = Vec::new();
    let mut t = Tokenizer::new(value);
    while !t.done() {
        let Some(name) = t.token() else { break };
        let mut m = Metric { name, duration: 0.0, description: String::new() };
        let (mut dur_set, mut desc_set) = (false, false);
        t.skip_to(b",;");
        while t.consume(b';') {
            let Some(param) = t.token() else { break };
            let mut value = String::new();
            if t.consume(b'=') {
                if let Some(v) = t.token_or_quoted() {
                    value = v;
                }
                t.skip_to(b",;");
            }
            if param.eq_ignore_ascii_case("dur") && !dur_set {
                m.duration = to_double(&value).unwrap_or(0.0);
                dur_set = true;
            } else if param.eq_ignore_ascii_case("desc") && !desc_set {
                m.description = value;
                desc_set = true;
            }
        }
        out.push(m);
        if !t.consume(b',') {
            break;
        }
    }
    out
}

// ── the interface ───────────────────────────────────────────────────────────────────────────────

use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::live::method_row;
use crate::install::MemberDef;

/// The metrics as `PerformanceServerTiming` objects of this realm, in an internal array: an entry keeps
/// it, and its `serverTiming` getter hands out a frozen copy each time ([`cb_entry_server_timing`]).
pub(crate) fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, metrics: &[Metric]) -> v8::Local<'s, v8::Array> {
    let items: Vec<v8::Local<v8::Value>> = metrics
        .iter()
        .map(|m| {
            let o = crate::install::live::rows::mint_table(scope, "PerformanceServerTiming");
            let n = crate::binding::protos::str_val(scope, &m.name);
            evt_slot_write(scope, o, "name", n);
            let d: v8::Local<v8::Value> = v8::Number::new(scope, m.duration).into();
            evt_slot_write(scope, o, "duration", d);
            let s = crate::binding::protos::str_val(scope, &m.description);
            evt_slot_write(scope, o, "description", s);
            o.into()
        })
        .collect();
    v8::Array::new_with_elements(scope, &items)
}

/// An entry's `serverTiming`: a NEW frozen array of the SAME metric objects on every read (Chrome 150).
pub(crate) fn cb_entry_server_timing(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let items: Vec<v8::Local<v8::Value>> = match evt_slot_read(scope, args.this(), "serverTiming").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) => (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect(),
        None => Vec::new(),
    };
    let out = v8::Array::new_with_elements(scope, &items);
    out.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    rv.set(out.into());
}

/// An entry's `serverTiming` as its `toJSON()` writes it: each metric's `toJSON()` -- plain objects.
pub(crate) fn json_list<'s>(scope: &mut v8::PinScope<'s, '_>, entry: v8::Local<v8::Object>) -> v8::Local<'s, v8::Array> {
    let mut items: Vec<v8::Local<v8::Value>> = Vec::new();
    if let Some(a) = evt_slot_read(scope, entry, "serverTiming").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        for i in 0..a.length() {
            if let Some(m) = a.get_index(scope, i).and_then(|m| v8::Local::<v8::Object>::try_from(m).ok()) {
                items.push(metric_json(scope, m).into());
            }
        }
    }
    v8::Array::new_with_elements(scope, &items)
}

fn metric_json<'s>(scope: &mut v8::PinScope<'s, '_>, m: v8::Local<v8::Object>) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    for k in ["name", "duration", "description"] {
        if let Some(v) = evt_slot_read(scope, m, k) {
            crate::binding::protos::set_prop(scope, o, k, v);
        }
    }
    o
}

fn cb_metric_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let o = metric_json(scope, args.this());
    rv.set(o.into());
}

/// The body the one method row of [`PERFORMANCESERVERTIMING`] names.
pub fn install_server_timing(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" => method_row(scope, proto, member, cb_metric_to_json),
        other => debug_assert!(false, "install_server_timing: unexpected member {other}"),
    }
}

/// Created at its capture position by the registry walk (`live::ORDERED_HAND`), in both realms.
pub fn create_performance_server_timing<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    use crate::install::installer::{self, Flavor};
    let built = installer::create_interface(scope, global, &PERFORMANCESERVERTIMING, "PerformanceServerTiming", None, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] server-timing install gap: {}.{}", g.interface, g.member);
    }
}

// Rows VERBATIM from the `PerformanceServerTiming` block of `install/generated_registry.rs` it replaces
// (`[Exposed=(Window,Worker)]`); the attributes read the private slots [`mint`] writes, so an instance has no
// own property, and the constructor is the illegal one.
webapi::interface! {
    "PerformanceServerTiming" [interface, both] {
        ctor   "PerformanceServerTiming"         len 0 = stub,
        attr   "name"        : DomString readonly    = slot_get,
        attr   "duration"    : Double readonly       = slot_get,
        attr   "description" : DomString readonly    = slot_get,
        method "toJSON"      : Any len 0             = install_server_timing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chrome 150.0.7871.46's `serverTiming` for each `Server-Timing` value (one field each, measured with
    /// a navigation entry: `[name, duration, description]`).
    #[test]
    fn blinks_parse_of_29_measured_values() {
        let cases: Vec<(&str, Vec<(&str, f64, &str)>)> = vec![
        ("a", vec![("a", 0.0, "")]),
        ("a;dur=1.5", vec![("a", 1.5, "")]),
        ("a;dur=x", vec![("a", 0.0, "")]),
        ("a;DUR=2;dur=3", vec![("a", 2.0, "")]),
        ("a;desc=\"q\\\"x\"", vec![("a", 0.0, "q\"x")]),
        ("a;desc=tok", vec![("a", 0.0, "tok")]),
        ("a, b;dur=1", vec![("a", 0.0, ""), ("b", 1.0, "")]),
        ("a;dur", vec![("a", 0.0, "")]),
        (" a ; dur = 2 ", vec![("a", 2.0, "")]),
        ("a;dur=1;desc", vec![("a", 1.0, "")]),
        ("\"q\";dur=1", vec![]),
        ("a,,b", vec![("a", 0.0, "")]),
        ("a;foo=bar;dur=4", vec![("a", 4.0, "")]),
        ("a;dur=1e3", vec![("a", 1000.0, "")]),
        ("a;dur=-1", vec![("a", -1.0, "")]),
        ("a;dur=\"1.5 \"", vec![("a", 0.0, "")]),
        ("a;dur=\" 2\"", vec![("a", 2.0, "")]),
        ("a;dur=0x10", vec![("a", 0.0, "")]),
        ("a;dur=.5", vec![("a", 0.5, "")]),
        ("a;dur=+3", vec![("a", 3.0, "")]),
        ("a;dur=Infinity", vec![("a", 0.0, "")]),
        ("a;dur=NaN", vec![("a", 0.0, "")]),
        ("a;dur=1.", vec![("a", 1.0, "")]),
        ("a;desc=\"unterminated", vec![("a", 0.0, "")]),
        ("a;dur=5x;desc=y", vec![("a", 0.0, "y")]),
        ("a b;dur=1", vec![("a", 1.0, "")]),
        ("a;dur=2,b", vec![("a", 2.0, ""), ("b", 0.0, "")]),
        ("caf\u{e9};dur=1", vec![("caf", 1.0, "")]),
        ("a;desc=\u{e9}", vec![("a", 0.0, "")]),
        ];
        for (value, want) in cases {
            let got: Vec<(String, f64, String)> = parse(value).into_iter().map(|m| (m.name, m.duration, m.description)).collect();
            let want: Vec<(String, f64, String)> = want.into_iter().map(|(n, d, s)| (n.to_string(), d, s.to_string())).collect();
            assert_eq!(got, want, "Server-Timing: {value}");
        }
    }
}
