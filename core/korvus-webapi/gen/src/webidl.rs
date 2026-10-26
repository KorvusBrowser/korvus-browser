//! Parse the vendored WebIDL corpus (weedle) into the normalized [`crate::spec`] model.
//!
//! This is the sole WebIDL ingestion point (it replaces the Node `webidl_surface.mjs`). It walks
//! the weedle AST and lowers only what the emitter needs, in owned form (Strings) so no weedle
//! borrow escapes.

use crate::spec::{Arg, InterfaceSpec, MemberSpec};
use weedle::argument::Argument;
use weedle::interface::{InterfaceMember, Special, StringifierOrInheritOrStatic, StringifierOrStatic};
use weedle::types::{
    AttributedType, FloatingPointType, IntegerType, MayBeNull, NonAnyType, ReturnType, SingleType,
    Type, UnionMemberType, UnionType,
};

/// Parse a WebIDL source string into the `interface` specs it declares. Non-interface definitions
/// (dictionaries, typedefs, enums, ...) are ignored for now.
pub fn parse(src: &str) -> Result<Vec<InterfaceSpec>, String> {
    let defs = weedle::parse(src).map_err(|e| format!("weedle parse error: {e:?}"))?;
    let mut specs = Vec::new();
    for def in &defs {
        if let weedle::Definition::Interface(iface) = def {
            specs.push(InterfaceSpec {
                name: iface.identifier.0.to_string(),
                inherits: iface.inheritance.as_ref().map(|inh| inh.identifier.0.to_string()),
                exposed: Vec::new(),
                members: lower_members(&iface.members.body),
            });
        }
    }
    Ok(specs)
}

/// Lower the weedle interface-member AST into `MemberSpec`s, in source order. Members that geometry
/// (the P0 fixture) does not use — iterable/maplike/setlike and bare stringifiers — are skipped for
/// now; later domains add them under test. `const` IS lowered: it is a quarter of the whole surface
/// (2,249 of 9,128 captured members, 1,714 of them WebGL's `gl.TRIANGLES`-style enum values), and a
/// constant is fully implemented exactly when its value is right — so the ⑦ spec axis can verify it
/// outright, with no behavior case and no implementation work. Anonymous specials (a `getter` with no name) lower to
/// [`MemberSpec::Special`] — the named-property machinery — while specials WITH a name (`getter
/// DOMString item(...)`) stay ordinary named operations.
///
/// `pub(crate)` so the corpus merger ([`crate::merge`]) reuses it verbatim for base `interface X`
/// and every `partial interface X` block (both are weedle `Vec<InterfaceMember>`).
/// A `const`'s declared type as a plain string (`unsigned short`, `boolean`, `double`).
pub(crate) fn const_type_to_string(t: &weedle::types::ConstType<'_>) -> String {
    use weedle::types::ConstType;
    match t {
        ConstType::Integer(i) => integer_to_string(&i.type_),
        ConstType::FloatingPoint(f) => floating_to_string(&f.type_),
        ConstType::Boolean(_) => "boolean".to_string(),
        ConstType::Byte(_) => "byte".to_string(),
        ConstType::Octet(_) => "octet".to_string(),
        ConstType::Identifier(i) => i.type_.0.to_string(),
    }
}

/// A `const`'s literal as written in the IDL. Hex stays hex; the grader normalizes when comparing.
pub(crate) fn const_value_to_string(v: &weedle::literal::ConstValue<'_>) -> String {
    use weedle::literal::{ConstValue, FloatLit, IntegerLit};
    match v {
        ConstValue::Boolean(b) => b.0.to_string(),
        ConstValue::Null(_) => "null".to_string(),
        ConstValue::Integer(i) => match i {
            IntegerLit::Dec(d) => d.0.to_string(),
            IntegerLit::Hex(h) => h.0.to_string(),
            IntegerLit::Oct(o) => o.0.to_string(),
        },
        ConstValue::Float(f) => match f {
            FloatLit::Value(x) => x.0.to_string(),
            FloatLit::NegInfinity(_) => "-Infinity".to_string(),
            FloatLit::Infinity(_) => "Infinity".to_string(),
            FloatLit::NaN(_) => "NaN".to_string(),
        },
    }
}

/// Whether an extended-attribute list carries `[Name=Value]`, e.g. `[PutForwards=value]`.
pub(crate) fn has_ident_attr(attrs: &Option<weedle::attribute::ExtendedAttributeList<'_>>, name: &str) -> bool {
    let Some(list) = attrs else { return false };
    list.body.list.iter().any(|a| {
        matches!(a, weedle::attribute::ExtendedAttribute::Ident(n) if n.lhs_identifier.0 == name)
    })
}

/// Whether an extended-attribute list carries a bare `[Name]`, e.g. `[HTMLConstructor]`.
fn has_no_args_attr(attrs: &Option<weedle::attribute::ExtendedAttributeList<'_>>, name: &str) -> bool {
    let Some(list) = attrs else { return false };
    list.body.list.iter().any(|a| {
        matches!(a, weedle::attribute::ExtendedAttribute::NoArgs(n) if n.0.0 == name)
    })
}

/// `[PutForwards=value]` on an attribute, wherever it is declared.
pub(crate) fn has_put_forwards(attrs: &Option<weedle::attribute::ExtendedAttributeList<'_>>) -> bool {
    has_ident_attr(attrs, "PutForwards")
}

pub(crate) fn lower_members(members: &[InterfaceMember]) -> Vec<MemberSpec> {
    let mut out = Vec::new();
    for member in members {
        match member {
            InterfaceMember::Constructor(c) => {
                out.push(MemberSpec::Constructor {
                    args: lower_args(&c.args.body.list),
                    html_constructor: has_no_args_attr(&c.attributes, "HTMLConstructor"),
                });
            }
            InterfaceMember::Const(c) => {
                out.push(MemberSpec::Const {
                    name: c.identifier.0.to_string(),
                    ty: const_type_to_string(&c.const_type),
                    // The literal VERBATIM (`0x0004`, `-1`, `true`): `quality::parse_const` already
                    // understands hex/decimal/float/bool, and keeping the source spelling means a
                    // mismatch report shows what the spec actually wrote.
                    value: const_value_to_string(&c.const_value),
                });
            }
            InterfaceMember::Attribute(a) => {
                // `static` and `inherit` are distinct weedle modifier variants — never conflated.
                let (is_static, inherited) = match &a.modifier {
                    Some(StringifierOrInheritOrStatic::Static(_)) => (true, false),
                    Some(StringifierOrInheritOrStatic::Inherit(_)) => (false, true),
                    _ => (false, false),
                };
                out.push(MemberSpec::Attribute {
                    name: a.identifier.0.to_string(),
                    ty: attributed_type_to_string(&a.type_),
                    readonly: a.readonly.is_some(),
                    is_static,
                    inherited,
                    put_forwards: has_ident_attr(&a.attributes, "PutForwards"),
                });
            }
            InterfaceMember::Operation(o) => {
                // An anonymous special (`getter DOMString (DOMString name);` — no identifier) is the
                // named-property machinery, not a prototype member: lower it as a Special.
                if o.identifier.is_none() {
                    if let Some(special) = o.special.as_ref().and_then(special_op) {
                        out.push(MemberSpec::Special { special });
                    }
                    continue;
                }
                let name = o.identifier.as_ref().unwrap().0.to_string();
                out.push(MemberSpec::Operation {
                    name,
                    ret: return_type_to_string(&o.return_type),
                    args: lower_args(&o.args.body.list),
                    is_static: matches!(&o.modifier, Some(StringifierOrStatic::Static(_))),
                });
            }
            // A bare `stringifier;` is a `toString()` -> DOMString operation (as webidl_surface.mjs
            // synthesizes it). A `stringifier attribute`/named-`stringifier` operation is already an
            // Attribute/Operation above, so only the anonymous form reaches here.
            InterfaceMember::Stringifier(_) => {
                out.push(MemberSpec::Operation {
                    name: "toString".to_string(),
                    ret: "DOMString".to_string(),
                    args: Vec::new(),
                    is_static: false,
                });
            }
            _ => {}
        }
    }
    out
}

/// Map a weedle `getter`/`setter`/`deleter` special keyword to its [`SpecialOp`]. `legacycaller`
/// specials stay unmodeled (none in the corpus's exposed surface) and yield `None`.
pub(crate) fn special_op(special: &Special) -> Option<crate::spec::SpecialOp> {
    use crate::spec::SpecialOp;
    match special {
        Special::Getter(_) => Some(SpecialOp::NamedGetter),
        Special::Setter(_) => Some(SpecialOp::NamedSetter),
        Special::Deleter(_) => Some(SpecialOp::NamedDeleter),
        Special::LegacyCaller(_) => None,
    }
}

/// `pub(crate)` so [`crate::merge`]'s mixin-member lowering reuses the same argument lowering (the
/// weedle `Argument` type is shared by interface, mixin, and partial members alike).
pub(crate) fn lower_args(args: &[Argument]) -> Vec<Arg> {
    args.iter()
        .map(|arg| match arg {
            Argument::Single(s) => Arg {
                name: s.identifier.0.to_string(),
                ty: attributed_type_to_string(&s.type_),
                optional: s.optional.is_some(),
                variadic: false,
            },
            Argument::Variadic(v) => Arg {
                name: v.identifier.0.to_string(),
                ty: type_to_string(&v.type_),
                optional: false,
                variadic: true,
            },
        })
        .collect()
}

// ---- WebIDL type -> its spelled-out string. Covers the geometry surface; unmodeled variants fall
// back to `{:?}` (completed per-domain, under test, at P1/emit where the string is actually mapped).

// `pub(crate)`: the mixin-member lowering in [`crate::merge`] spells out attribute/return types
// through the exact same helpers, so no type-stringification logic is duplicated.
pub(crate) fn attributed_type_to_string(t: &AttributedType) -> String {
    type_to_string(&t.type_)
}

pub(crate) fn return_type_to_string(r: &ReturnType) -> String {
    match r {
        ReturnType::Undefined(_) => "undefined".to_string(),
        ReturnType::Type(t) => type_to_string(t),
    }
}

fn type_to_string(t: &Type) -> String {
    match t {
        Type::Single(SingleType::Any(_)) => "any".to_string(),
        Type::Single(SingleType::NonAny(n)) => non_any_type_to_string(n),
        Type::Union(u) => union_to_string(u),
    }
}

fn union_to_string(u: &MayBeNull<UnionType>) -> String {
    let members: Vec<String> = u.type_.body.list.iter().map(union_member_to_string).collect();
    let s = format!("({})", members.join(" or "));
    if u.q_mark.is_some() { format!("{s}?") } else { s }
}

fn union_member_to_string(m: &UnionMemberType) -> String {
    match m {
        UnionMemberType::Single(a) => non_any_type_to_string(&a.type_),
        UnionMemberType::Union(u) => union_to_string(u),
    }
}

fn non_any_type_to_string(n: &NonAnyType) -> String {
    use NonAnyType as N;
    let (base, nullable) = match n {
        N::Boolean(m) => ("boolean".to_string(), m.q_mark.is_some()),
        N::Object(m) => ("object".to_string(), m.q_mark.is_some()),
        N::DOMString(m) => ("DOMString".to_string(), m.q_mark.is_some()),
        N::ByteString(m) => ("ByteString".to_string(), m.q_mark.is_some()),
        N::USVString(m) => ("USVString".to_string(), m.q_mark.is_some()),
        N::Float32Array(m) => ("Float32Array".to_string(), m.q_mark.is_some()),
        N::Float64Array(m) => ("Float64Array".to_string(), m.q_mark.is_some()),
        N::FloatingPoint(m) => (floating_to_string(&m.type_), m.q_mark.is_some()),
        N::Integer(m) => (integer_to_string(&m.type_), m.q_mark.is_some()),
        N::Sequence(m) => (
            format!("sequence<{}>", type_to_string(&m.type_.generics.body)),
            m.q_mark.is_some(),
        ),
        // `Promise<T>` was falling through to the `{:?}` arm, so every promise-returning member in
        // the corpus carried a WEEDLE DEBUG DUMP as its return type --
        // `Promise(PromiseType { promise: Promise, generics: Generics { ... } })`. Nothing crashed on
        // it, which is why it survived: the string is only ever compared against known type names, and
        // it matched none of them, so a promise-returning member read as "some non-scalar type" and
        // got the same brand probe an interface handle gets. What it COST was the settled type: the
        // case generator cannot ask what `estimate()` resolves to without knowing that it resolves at
        // all. A promise is never nullable in WebIDL, hence `false`.
        // The body is a ReturnType, not a Type: `Promise<undefined>` is legal and `undefined` is not
        // spellable as a Type.
        N::Promise(m) => (format!("Promise<{}>", return_type_to_string(&m.generics.body)), false),
        N::Identifier(m) => (m.type_.0.to_string(), m.q_mark.is_some()),
        other => (format!("{other:?}"), false),
    };
    if nullable { format!("{base}?") } else { base }
}

fn floating_to_string(f: &FloatingPointType) -> String {
    match f {
        FloatingPointType::Double(d) => if d.unrestricted.is_some() { "unrestricted double" } else { "double" },
        FloatingPointType::Float(f) => if f.unrestricted.is_some() { "unrestricted float" } else { "float" },
    }
    .to_string()
}

fn integer_to_string(i: &IntegerType) -> String {
    match i {
        IntegerType::Short(s) => if s.unsigned.is_some() { "unsigned short" } else { "short" },
        IntegerType::Long(l) => if l.unsigned.is_some() { "unsigned long" } else { "long" },
        IntegerType::LongLong(ll) => if ll.unsigned.is_some() { "unsigned long long" } else { "long long" },
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Arg, MemberSpec};

    const GEOMETRY: &str = include_str!("../idl/geometry.idl");

    fn parse_geometry() -> Vec<InterfaceSpec> {
        parse(GEOMETRY).expect("geometry.idl parses")
    }

    fn iface<'a>(specs: &'a [InterfaceSpec], name: &str) -> &'a InterfaceSpec {
        specs
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no interface named {name}"))
    }

    /// Every `interface` block in geometry.idl becomes an `InterfaceSpec` — and the `dictionary`
    /// blocks (DOMPointInit, DOMRectInit, DOMQuadInit, DOMMatrix2DInit, DOMMatrixInit) do NOT.
    #[test]
    fn parses_all_interface_names() {
        let specs = parse_geometry();
        let mut names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "DOMMatrix",
                "DOMMatrixReadOnly",
                "DOMPoint",
                "DOMPointReadOnly",
                "DOMQuad",
                "DOMRect",
                "DOMRectList",
                "DOMRectReadOnly",
            ]
        );
    }

    /// `interface DOMPoint : DOMPointReadOnly` -> `inherits == Some("DOMPointReadOnly")`; a base
    /// interface with no `:` clause has `inherits == None`.
    #[test]
    fn resolves_inheritance() {
        let specs = parse_geometry();
        assert_eq!(iface(&specs, "DOMPoint").inherits.as_deref(), Some("DOMPointReadOnly"));
        assert_eq!(iface(&specs, "DOMRect").inherits.as_deref(), Some("DOMRectReadOnly"));
        assert_eq!(iface(&specs, "DOMMatrix").inherits.as_deref(), Some("DOMMatrixReadOnly"));
        assert_eq!(iface(&specs, "DOMPointReadOnly").inherits, None);
        assert_eq!(iface(&specs, "DOMQuad").inherits, None);
    }

    /// DOMPointReadOnly's members, in source order, carry the right kinds and flags: a constructor,
    /// a `static` operation, four `readonly` instance attributes, then two instance operations.
    #[test]
    fn dompointreadonly_members_have_correct_kinds_and_flags() {
        let specs = parse_geometry();
        let m = &iface(&specs, "DOMPointReadOnly").members;
        assert_eq!(m.len(), 8, "ctor + static fromPoint + x/y/z/w + matrixTransform + toJSON");
        assert!(matches!(&m[0], MemberSpec::Constructor { args, .. } if args.len() == 4));
        assert!(matches!(&m[1], MemberSpec::Operation { name, is_static: true, .. } if name.as_str() == "fromPoint"));
        for (i, n) in [(2usize, "x"), (3, "y"), (4, "z"), (5, "w")] {
            assert!(
                matches!(&m[i], MemberSpec::Attribute { name, readonly: true, is_static: false, inherited: false, .. } if name.as_str() == n),
                "member {i} should be a readonly instance attribute {n}, got {:?}", m[i]
            );
        }
        assert!(matches!(&m[6], MemberSpec::Operation { name, is_static: false, .. } if name.as_str() == "matrixTransform"));
        assert!(matches!(&m[7], MemberSpec::Operation { name, is_static: false, .. } if name.as_str() == "toJSON"));
    }

    /// The weedle-risk case the plan flagged: `static`, `inherit`, and plain-instance must stay
    /// distinct (not conflated). DOMPoint redeclares x as `inherit attribute`; DOMPointReadOnly's x
    /// is a plain instance readonly attribute and its `fromPoint` is `static`.
    #[test]
    fn distinguishes_static_inherit_and_instance_members() {
        let specs = parse_geometry();

        let dp = &iface(&specs, "DOMPoint").members;
        let dpx = dp.iter().find(|m| matches!(m, MemberSpec::Attribute { name, .. } if name.as_str() == "x")).unwrap();
        assert!(matches!(dpx, MemberSpec::Attribute { inherited: true, is_static: false, readonly: false, .. }),
            "DOMPoint.x is an `inherit attribute`, got {dpx:?}");

        let ro = &iface(&specs, "DOMPointReadOnly").members;
        let rox = ro.iter().find(|m| matches!(m, MemberSpec::Attribute { name, .. } if name.as_str() == "x")).unwrap();
        assert!(matches!(rox, MemberSpec::Attribute { inherited: false, is_static: false, readonly: true, .. }),
            "DOMPointReadOnly.x is a plain instance readonly attribute, got {rox:?}");
        let from = ro.iter().find(|m| matches!(m, MemberSpec::Operation { name, .. } if name.as_str() == "fromPoint")).unwrap();
        assert!(matches!(from, MemberSpec::Operation { is_static: true, .. }));
        let mt = ro.iter().find(|m| matches!(m, MemberSpec::Operation { name, .. } if name.as_str() == "matrixTransform")).unwrap();
        assert!(matches!(mt, MemberSpec::Operation { is_static: false, .. }));
    }

    /// Argument `optional` and `...` variadic flags are captured. Geometry has no variadic member,
    /// so that one branch is proven against a tiny inline snippet.
    #[test]
    fn extracts_argument_optionality_and_variadic() {
        let specs = parse_geometry();

        let ctor = iface(&specs, "DOMPointReadOnly").members.iter()
            .find_map(|m| if let MemberSpec::Constructor { args, .. } = m { Some(args) } else { None })
            .expect("DOMPointReadOnly constructor");
        assert_eq!(
            ctor.iter().map(|a| (a.name.as_str(), a.optional, a.variadic)).collect::<Vec<_>>(),
            vec![("x", true, false), ("y", true, false), ("z", true, false), ("w", true, false)]
        );

        let ff = iface(&specs, "DOMMatrixReadOnly").members.iter()
            .find_map(|m| match m {
                MemberSpec::Operation { name, args, .. } if name.as_str() == "fromFloat32Array" => Some(args),
                _ => None,
            })
            .expect("fromFloat32Array");
        assert_eq!(ff.len(), 1);
        assert_eq!((ff[0].name.as_str(), ff[0].optional, ff[0].variadic), ("array32", false, false));

        let x = parse("interface X { undefined f(long... nums); };").unwrap();
        let a = match &iface(&x, "X").members[0] {
            MemberSpec::Operation { args, .. } => &args[0],
            other => panic!("expected an operation, got {other:?}"),
        };
        assert_eq!((a.name.as_str(), a.optional, a.variadic), ("nums", false, true));
    }

    /// A bare `stringifier;` lowers to a `toString()` operation returning `DOMString` — matching
    /// webidl_surface.mjs, so the from-WebIDL emit keeps the member. DOMMatrixReadOnly has one
    /// (`[Exposed=Window] stringifier;`), in source position just before `toJSON`.
    #[test]
    fn bare_stringifier_lowers_to_tostring_operation() {
        let specs = parse_geometry();
        let m = &iface(&specs, "DOMMatrixReadOnly").members;
        let ts = m.iter().find(|x| matches!(x, MemberSpec::Operation { name, .. } if name.as_str() == "toString"));
        assert!(
            matches!(ts, Some(MemberSpec::Operation { name, ret, args, is_static: false }) if name == "toString" && ret == "DOMString" && args.is_empty()),
            "bare `stringifier;` should lower to toString() -> DOMString, got {ts:?}"
        );
    }

    /// Types are spelled out (the emitter maps these strings at P1). Covers the whole geometry type
    /// surface: `unrestricted double`, `boolean`, `unsigned long`, an interface/dictionary
    /// identifier, `object`, a nullable interface (`DOMRect?`), and a `(DOMString or sequence<…>)`
    /// union.
    #[test]
    fn stringifies_geometry_types() {
        let specs = parse_geometry();
        let attr_ty = |iface_name: &str, an: &str| -> String {
            iface(&specs, iface_name).members.iter().find_map(|m| match m {
                MemberSpec::Attribute { name, ty, .. } if name.as_str() == an => Some(ty.clone()),
                _ => None,
            }).unwrap_or_else(|| panic!("no attribute {iface_name}.{an}"))
        };
        assert_eq!(attr_ty("DOMPointReadOnly", "x"), "unrestricted double");
        assert_eq!(attr_ty("DOMMatrixReadOnly", "is2D"), "boolean");
        assert_eq!(attr_ty("DOMRectList", "length"), "unsigned long");

        let op = |iface_name: &str, on: &str| -> (String, Vec<Arg>) {
            iface(&specs, iface_name).members.iter().find_map(|m| match m {
                MemberSpec::Operation { name, ret, args, .. } if name.as_str() == on => Some((ret.clone(), args.clone())),
                _ => None,
            }).unwrap_or_else(|| panic!("no operation {iface_name}.{on}"))
        };
        let (mt_ret, mt_args) = op("DOMPointReadOnly", "matrixTransform");
        assert_eq!(mt_ret, "DOMPoint");
        assert_eq!(mt_args[0].ty, "DOMMatrixInit");
        assert_eq!(op("DOMPointReadOnly", "toJSON").0, "object");
        assert_eq!(op("DOMRectList", "item").0, "DOMRect?");

        let ctor = iface(&specs, "DOMMatrixReadOnly").members.iter()
            .find_map(|m| if let MemberSpec::Constructor { args, .. } = m { Some(args) } else { None })
            .expect("DOMMatrixReadOnly constructor");
        let u = &ctor[0].ty;
        assert!(
            u.contains("DOMString") && u.contains("sequence<unrestricted double>") && u.contains(" or "),
            "constructor arg should be the DOMString|sequence union, got {u}"
        );
    }

    /// Anonymous specials lower to `MemberSpec::Special` (the named-property machinery), in source
    /// order, while a special WITH an identifier stays an ordinary named operation. DOMStringMap's
    /// whole WebIDL surface is the three specials.
    #[test]
    fn anonymous_specials_lower_to_special_members() {
        use crate::spec::SpecialOp;
        let src = r#"
            [Exposed=Window, LegacyOverrideBuiltIns]
            interface DOMStringMap {
              getter DOMString (DOMString name);
              [CEReactions] setter undefined (DOMString name, DOMString value);
              [CEReactions] deleter undefined (DOMString name);
            };
        "#;
        let specs = parse(src).unwrap();
        let m = &iface(&specs, "DOMStringMap").members;
        assert_eq!(
            m.iter().map(|x| match x {
                MemberSpec::Special { special } => *special,
                other => panic!("expected a Special, got {other:?}"),
            }).collect::<Vec<_>>(),
            vec![SpecialOp::NamedGetter, SpecialOp::NamedSetter, SpecialOp::NamedDeleter]
        );
        assert_eq!(SpecialOp::NamedGetter.member_name(), "named getter");

        // A NAMED special (e.g. `getter DOMString namedItem(...)`) is a regular operation.
        let named = parse("interface NL { getter DOMString namedItem(unsigned long i); };").unwrap();
        assert!(matches!(
            &iface(&named, "NL").members[0],
            MemberSpec::Operation { name, .. } if name == "namedItem"
        ));
    }
}
