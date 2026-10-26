//! Typed reader over the runtime surface-snapshot JSON (the `webapi quality` system)
//!
//! The probe (`core/korvus-webapi/tests/quality.rs`, run by `scripts/coverage.sh`) boots the engine ONCE with the real
//! `CapturedPersona`, walks every interface's prototype + a curated set of live singletons/instances,
//! and writes one JSON `{ meta, globals, interfaces, singletons, instances }`. Each member record
//! carries the fields `fixtures/surface-dump.js` emits:
//!   `{ name, native, ownTell, kind, length, enumerable, configurable, get, set, fnName,
//!      getName, getLength, setName, setLength, liveValue?, type, value? }`.
//!
//! This module gives the aggregator a typed view. The one non-trivial job is [`SurfaceSnapshot::member`]:
//! a member's SHAPE/native meta lives on `interfaces[iface].members` (the prototype), but its live
//! VALUE (axis ④) is only captured on a live object — the `navigator` singleton for `Navigator`,
//! the constructed `URL` instance for `URL`, and so on. `member` UNIONS the two: it resolves the
//! prototype record and folds in the matching singleton/instance `liveValue`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// One resolved member: its shape/native meta (from the interface prototype) unioned with the live
/// value read off the matching singleton/instance. All shape fields mirror `surface-dump.js`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberRec {
    pub name: String,
    /// `"accessor"` | `"method"` | `"value"`.
    pub kind: String,
    /// `[native code]` and (for accessors) both present halves native.
    pub native: bool,
    /// The member fn owns `prototype`/`arguments`/`caller` — a JS-shim tell.
    pub own_tell: bool,
    pub enumerable: bool,
    pub configurable: bool,
    // --- accessor halves ---
    pub get: bool,
    pub set: bool,
    pub get_name: Option<String>,
    pub get_length: Option<i64>,
    pub set_name: Option<String>,
    pub set_length: Option<i64>,
    // --- method ---
    pub length: Option<i64>,
    pub fn_name: Option<String>,
    // --- value / const ---
    /// A captured primitive constant (Node.ELEMENT_NODE etc.); only for `kind == "value"`.
    pub value: Option<Value>,
    /// The member's live value read off a real singleton/instance (axis ④), when JSON-safe.
    pub live_value: Option<Value>,
}

impl MemberRec {
    /// Build a record from one raw member `Value` (a `surface-dump.js` member object), folding in a
    /// resolved `live` value (from the matching singleton/instance) when the prototype record itself
    /// did not already carry one.
    fn from_value(v: &Value, live: Option<Value>) -> Option<MemberRec> {
        let name = v.get("name")?.as_str()?.to_string();
        let str_field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        let i64_field = |k: &str| v.get(k).and_then(Value::as_i64);
        let bool_field = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        // Prefer a liveValue already on the record (instances/singletons embed it), else the resolved one.
        let live_value = v.get("liveValue").cloned().or(live);
        Some(MemberRec {
            name,
            kind: str_field("kind").unwrap_or_default(),
            native: v.get("native").and_then(Value::as_bool).unwrap_or(true),
            own_tell: bool_field("ownTell"),
            enumerable: bool_field("enumerable"),
            configurable: bool_field("configurable"),
            get: bool_field("get"),
            set: bool_field("set"),
            get_name: str_field("getName"),
            get_length: i64_field("getLength"),
            set_name: str_field("setName"),
            set_length: i64_field("setLength"),
            length: i64_field("length"),
            fn_name: str_field("fnName"),
            value: v.get("value").cloned(),
            live_value,
        })
    }

    /// True for members that carry a native function (accessor/method) — the only ones the native
    /// axis (②) and the length checks apply to. `value`/const members are plain data properties.
    pub fn is_callable(&self) -> bool {
        self.kind == "accessor" || self.kind == "method"
    }
}

/// One interface constructor's OWN meta (not its members): `[native code]`, `.name`, `.length`, the
/// prototype chain by constructor name (`Element → Node → EventTarget`), and `Symbol.toStringTag`.
/// Feeds the Phase-A ctor axis (constructor nativeness) and structure axis (name/chain/tag/extra-member
/// diffs vs the baseline). Mirrors the interface-level fields `surface-dump.js`'s `iface()` emits.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CtorRec {
    /// The constructor is `[native code]`.
    pub native: bool,
    /// `ctor.name` (empty string is a raw-`v8::Function` tell).
    pub name: Option<String>,
    /// `ctor.length` (declared arg count).
    pub length: Option<i64>,
    /// The prototype chain by constructor name, most-derived first, `Object` excluded.
    pub proto_chain: Vec<String>,
    /// `proto[Symbol.toStringTag]`, when the interface sets one.
    pub to_string_tag: Option<String>,
    /// Whether `new X()` succeeds (argless). `None` when the snapshot predates the newable probe.
    pub newable: Option<bool>,
    /// Whether the prototype is iterable (`Symbol.iterator` present).
    pub iterable: Option<bool>,
}

/// The whole snapshot, with the lookup indices the aggregator needs pre-built:
///   - `proto`  : `iface -> (member name -> shape record)` from `interfaces[iface].members`.
///   - `statics`: `iface -> (member name -> shape record)` from `interfaces[iface].statics` — a WebIDL
///     `static` member lives on the CONSTRUCTOR, not the prototype (DOMMatrix.fromMatrix), so it must
///     be resolved there or it reads as a false "missing".
///   - `live`   : `iface -> (member name -> liveValue)` UNIONED from the `singletons` (keyed by their
///     `ctor` name, so `navigator` → `Navigator`) and the `instances` (keyed by their label, e.g.
///     `URL`, and by `ctor` too).
#[derive(Debug, Clone)]
pub struct SurfaceSnapshot {
    meta: Value,
    /// `globalThis` own-property names, verbatim (order preserved) — the automation-tell scan reads this.
    globals: Vec<String>,
    /// `document` own-property names — ChromeDriver injects `cdc_` keys here too.
    document_keys: Vec<String>,
    /// Whether `Function.prototype.toString` is itself native (a stealth override target). `None` when
    /// the snapshot predates the probe.
    fp_tostring_native: Option<bool>,
    proto: BTreeMap<String, BTreeMap<String, Value>>,
    statics: BTreeMap<String, BTreeMap<String, Value>>,
    /// `iface -> prototype member names in ENUMERATION ORDER` (the raw array order — the BTreeMaps above
    /// sort by name and lose it; the order is itself a fingerprint signal).
    order: BTreeMap<String, Vec<String>>,
    /// `iface -> constructor own meta` (native/name/length/proto-chain/toStringTag).
    ctors: BTreeMap<String, CtorRec>,
    live: BTreeMap<String, BTreeMap<String, Value>>,
}

impl SurfaceSnapshot {
    pub fn from_json(json: &str) -> serde_json::Result<SurfaceSnapshot> {
        let root: Value = serde_json::from_str(json)?;
        Ok(SurfaceSnapshot::from_value(root))
    }

    pub fn from_path(path: &Path) -> std::io::Result<SurfaceSnapshot> {
        let json = std::fs::read_to_string(path)?;
        SurfaceSnapshot::from_json(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    fn from_value(root: Value) -> SurfaceSnapshot {
        let mut proto: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        let mut statics: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        let mut live: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();

        // interfaces[iface].{members,statics} -> the prototype / constructor shape records, plus the
        // constructor's own meta (native/name/length/proto-chain/toStringTag) for the ctor/structure axes.
        let mut ctors: BTreeMap<String, CtorRec> = BTreeMap::new();
        let mut order: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(ifaces) = root.get("interfaces").and_then(Value::as_object) {
            for (iface, rec) in ifaces {
                proto.insert(iface.clone(), records_by_name(rec.get("members")));
                statics.insert(iface.clone(), records_by_name(rec.get("statics")));
                ctors.insert(iface.clone(), ctor_rec(rec));
                order.insert(iface.clone(), ordered_names(rec.get("members")));
            }
        }

        // globalThis + document own-property names, verbatim.
        let names_at = |key: &str| {
            root.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let globals = names_at("globals");
        let document_keys = names_at("documentKeys");
        let fp_tostring_native = root.get("fpToStringNative").and_then(Value::as_bool);

        // singletons: keyed by the global name (navigator/screen/...), but the WebIDL interface is
        // the singleton's constructor NAME (`ctor`). Index the liveValues under that ctor so
        // `Navigator` members resolve `navigator.hardwareConcurrency`.
        if let Some(singletons) = root.get("singletons").and_then(Value::as_object) {
            for rec in singletons.values() {
                if let Some(ctor) = rec.get("ctor").and_then(Value::as_str) {
                    merge_live(live.entry(ctor.to_string()).or_default(), rec);
                }
            }
        }

        // instances: keyed by their interface label already (URL, DOMMatrix, Event, ...). Index under
        // both the label and the ctor name (usually identical) so either spelling resolves.
        if let Some(instances) = root.get("instances").and_then(Value::as_object) {
            for (label, rec) in instances {
                merge_live(live.entry(label.clone()).or_default(), rec);
                if let Some(ctor) = rec.get("ctor").and_then(Value::as_str) {
                    if ctor != label {
                        merge_live(live.entry(ctor.to_string()).or_default(), rec);
                    }
                }
            }
        }

        SurfaceSnapshot {
            meta: root.get("meta").cloned().unwrap_or(Value::Null),
            globals,
            document_keys,
            fp_tostring_native,
            proto,
            statics,
            order,
            ctors,
            live,
        }
    }

    /// `document` own-property names (for the automation-tell scan).
    pub fn document_keys(&self) -> &[String] {
        &self.document_keys
    }

    /// Whether `Function.prototype.toString` is itself native (`None` if not probed).
    pub fn fp_tostring_native(&self) -> Option<bool> {
        self.fp_tostring_native
    }

    /// The `meta` block (`engine`, `ua`).
    pub fn meta(&self) -> &Value {
        &self.meta
    }

    /// `globalThis` own-property names (order preserved) — the automation-tell / extra-global scan.
    pub fn globals(&self) -> &[String] {
        &self.globals
    }

    /// One interface constructor's own meta (native/name/length/proto-chain/toStringTag), or `None`
    /// when the interface was not walked.
    pub fn ctor(&self, iface: &str) -> Option<&CtorRec> {
        self.ctors.get(iface)
    }

    /// The live prototype member NAMES of an interface (for the structure extra-member diff). Empty
    /// when the interface is absent.
    pub fn member_names(&self, iface: &str) -> Vec<&str> {
        self.proto
            .get(iface)
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// Every interface name the snapshot walked (the full dumped surface, not just the corpus subset) —
    /// for the full-surface tell scan.
    pub fn interface_names(&self) -> Vec<&str> {
        self.proto.keys().map(String::as_str).collect()
    }

    /// The interface's prototype member names in ENUMERATION ORDER (the raw dump order), for the order
    /// axis. Empty when the interface is absent.
    pub fn ordered_member_names(&self, iface: &str) -> &[String] {
        self.order.get(iface).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Is this interface present at all on the live surface (its constructor was walked)?
    pub fn has_interface(&self, iface: &str) -> bool {
        self.proto.contains_key(iface)
    }

    /// Resolve one instance member: its prototype shape/native record UNIONED with the matching
    /// singleton/instance `liveValue`. `None` when the member is not on `iface`'s live prototype
    /// (axis ① "present" = this returning `Some`).
    pub fn member(&self, iface: &str, name: &str) -> Option<MemberRec> {
        let rec = self.proto.get(iface)?.get(name)?;
        let live = self.live.get(iface).and_then(|m| m.get(name)).cloned();
        MemberRec::from_value(rec, live)
    }

    /// Resolve one `static` member off the interface's constructor (`interfaces[iface].statics`).
    /// A WebIDL `static` member is NOT a prototype property, so [`member`](Self::member) never finds
    /// it; assess routes static members here so they aren't miscounted as missing.
    pub fn static_member(&self, iface: &str, name: &str) -> Option<MemberRec> {
        let rec = self.statics.get(iface)?.get(name)?;
        MemberRec::from_value(rec, None)
    }
}

/// Fold a singleton/instance block's `members[].liveValue` into a `name -> liveValue` map (only
/// members that actually carried a JSON-safe liveValue).
fn merge_live(into: &mut BTreeMap<String, Value>, block: &Value) {
    if let Some(members) = block.get("members").and_then(Value::as_array) {
        for m in members {
            if let (Some(name), Some(lv)) =
                (m.get("name").and_then(Value::as_str), m.get("liveValue"))
            {
                into.entry(name.to_string()).or_insert_with(|| lv.clone());
            }
        }
    }
}

/// An array of member records → `member name -> full member record` (first-seen wins).
fn records_by_name(members: Option<&Value>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if let Some(members) = members.and_then(Value::as_array) {
        for m in members {
            if let Some(name) = m.get("name").and_then(Value::as_str) {
                out.entry(name.to_string()).or_insert_with(|| m.clone());
            }
        }
    }
    out
}

/// The member names of a raw members array in ENUMERATION ORDER (not sorted, not deduped-by-map).
fn ordered_names(members: Option<&Value>) -> Vec<String> {
    members
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Extract one interface's constructor own meta from its raw `interfaces[iface]` record (the
/// interface-level fields `surface-dump.js`'s `iface()` emits alongside `members`/`statics`).
fn ctor_rec(rec: &Value) -> CtorRec {
    CtorRec {
        native: rec.get("ctorNative").and_then(Value::as_bool).unwrap_or(false),
        name: rec.get("ctorName").and_then(Value::as_str).map(str::to_string),
        length: rec.get("length").and_then(Value::as_i64),
        proto_chain: rec
            .get("proto")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        to_string_tag: rec.get("toStringTag").and_then(Value::as_str).map(str::to_string),
        newable: rec.get("newable").and_then(Value::as_bool),
        iterable: rec.get("iterable").and_then(Value::as_bool),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tiny snapshot exercising both resolution paths: a Navigator prototype member whose value
    // lives on the `navigator` singleton, and a URL instance member whose value is embedded.
    const SNAP: &str = r#"{
      "meta": { "engine": "korvus", "ua": "UA" },
      "globals": ["Navigator", "URL"],
      "interfaces": {
        "Navigator": { "members": [
          { "name": "hardwareConcurrency", "kind": "accessor", "get": true, "set": false,
            "native": true, "ownTell": false, "enumerable": true, "configurable": true,
            "getName": "get hardwareConcurrency", "getLength": 0 },
          { "name": "javaEnabled", "kind": "method", "native": true, "ownTell": false,
            "enumerable": true, "configurable": true, "length": 0, "fnName": "javaEnabled" }
        ]},
        "URL": { "members": [
          { "name": "protocol", "kind": "accessor", "get": true, "set": true, "native": true,
            "ownTell": false, "enumerable": true, "configurable": true,
            "getName": "get protocol", "getLength": 0, "setName": "set protocol", "setLength": 1 }
        ]}
      },
      "singletons": {
        "navigator": { "ctor": "Navigator", "members": [
          { "name": "hardwareConcurrency", "liveValue": 10 }
        ]}
      },
      "instances": {
        "URL": { "ctor": "URL", "proto": ["URL"], "members": [
          { "name": "protocol", "liveValue": "https:" }
        ]}
      }
    }"#;

    #[test]
    fn resolves_prototype_shape_and_singleton_live_value() {
        let snap = SurfaceSnapshot::from_json(SNAP).expect("parse");
        let hc = snap.member("Navigator", "hardwareConcurrency").expect("present");
        assert_eq!(hc.kind, "accessor");
        assert!(hc.get && !hc.set && hc.native);
        assert_eq!(hc.get_length, Some(0));
        // The liveValue was pulled from the `navigator` singleton (indexed by its ctor "Navigator").
        assert_eq!(hc.live_value, Some(Value::from(10)));
    }

    #[test]
    fn resolves_instance_live_value_by_label() {
        let snap = SurfaceSnapshot::from_json(SNAP).expect("parse");
        let p = snap.member("URL", "protocol").expect("present");
        assert!(p.get && p.set);
        assert_eq!(p.set_length, Some(1));
        assert_eq!(p.live_value, Some(Value::from("https:")));
    }

    #[test]
    fn absent_member_is_none() {
        let snap = SurfaceSnapshot::from_json(SNAP).expect("parse");
        assert!(snap.member("Navigator", "getBattery").is_none());
        assert!(snap.member("Nope", "x").is_none());
        assert!(snap.has_interface("Navigator") && !snap.has_interface("Nope"));
    }

    // Phase A: the globals array (for the automation-tell scan) and the interface constructor's own
    // meta — ctorNative / name / length / prototype chain / toStringTag (for the ctor + structure axes)
    // — must survive parsing (they were previously dropped).
    const CTOR_SNAP: &str = r#"{
      "meta": { "engine": "korvus" },
      "globals": ["Event", "cdc_adoQpoasnfa76pfcZLmcfl_Array"],
      "interfaces": {
        "Event": {
          "typeof": "function", "ctorNative": true, "length": 1, "ctorName": "Event",
          "proto": ["Object"], "toStringTag": "Event",
          "members": [
            { "name": "type", "kind": "accessor", "get": true, "set": false, "native": true,
              "enumerable": true, "configurable": true, "getName": "get type", "getLength": 0 }
          ]
        }
      },
      "singletons": {}, "instances": {}
    }"#;

    #[test]
    fn exposes_globals_and_interface_ctor_meta() {
        let snap = SurfaceSnapshot::from_json(CTOR_SNAP).expect("parse");
        // The globals list is preserved verbatim (order + all entries, tells included).
        assert_eq!(snap.globals().len(), 2);
        assert!(snap.globals().iter().any(|g| g == "Event"));
        assert!(snap.globals().iter().any(|g| g.starts_with("cdc_")));
        // The constructor's own meta is exposed for the ctor/structure axes.
        let ctor = snap.ctor("Event").expect("Event ctor meta");
        assert!(ctor.native);
        assert_eq!(ctor.name.as_deref(), Some("Event"));
        assert_eq!(ctor.length, Some(1));
        assert_eq!(ctor.proto_chain, vec!["Object".to_string()]);
        assert_eq!(ctor.to_string_tag.as_deref(), Some("Event"));
        // The live prototype member NAMES (for the structure extra-member diff).
        assert_eq!(snap.member_names("Event"), vec!["type"]);
    }
}
