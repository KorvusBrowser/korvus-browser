//! The normalized interface model — the contract every downstream stage agrees on.
//!
//! `webidl.rs` fills this from the weedle AST; `surface.rs` (later) filters it to the exposed set
//! and attaches descriptor flags; `emit.rs` / `coverage.rs` read it. Nothing downstream re-parses
//! WebIDL — this is the single normalized form. Kept deliberately small and `Debug`/`PartialEq`
//! so tests can assert on it directly.

/// Which anonymous special operation a [`MemberSpec::Special`] is — the WebIDL named-property
/// machinery (`getter DOMString (DOMString name);` & friends) that makes an interface's instances
/// exotic objects rather than plain prototype carriers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialOp {
    NamedGetter,
    NamedSetter,
    NamedDeleter,
}

impl SpecialOp {
    /// The synthetic member name the quality system grades (and behavior cases key on). Deliberately
    /// contains a space so it can never collide with a real WebIDL member name.
    pub fn member_name(&self) -> &'static str {
        match self {
            SpecialOp::NamedGetter => "named getter",
            SpecialOp::NamedSetter => "named setter",
            SpecialOp::NamedDeleter => "named deleter",
        }
    }
}

/// One WebIDL `interface` block, normalized. (Dictionaries / typedefs / enums are not modeled here
/// yet — only `interface` declarations become an `InterfaceSpec`.)
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceSpec {
    /// The interface name, e.g. `DOMPointReadOnly`.
    pub name: String,
    /// The single inherited interface (`interface DOMPoint : DOMPointReadOnly`), if any.
    pub inherits: Option<String>,
    /// Global scopes from `[Exposed=...]`. Populated later (surface selection); empty for now.
    pub exposed: Vec<String>,
    /// The interface's members, in source order.
    pub members: Vec<MemberSpec>,
}

/// One interface member, discriminated by WebIDL member kind.
#[derive(Debug, Clone, PartialEq)]
pub enum MemberSpec {
    /// `constructor(args);`
    Constructor {
        args: Vec<Arg>,
        /// `[HTMLConstructor]` — declared by every HTML element interface, and NOT a way to build one.
        /// It throws "Illegal constructor" everywhere except inside a custom-element upgrade, so a
        /// consumer looking for a receiver must treat the interface as having none.
        html_constructor: bool,
    },
    /// `[static|inherit]? readonly? attribute T name;`
    Attribute {
        name: String,
        ty: String,
        readonly: bool,
        is_static: bool,
        /// `inherit attribute` — redeclares a base attribute (distinct from `static`).
        inherited: bool,
        /// `[PutForwards=value]` — the attribute is `readonly` in the IDL and STILL has a setter on
        /// the prototype, one that assigns to a member of the object the getter returns. Every
        /// `DOMTokenList`-valued attribute is like this (`link.relList = "x"` is
        /// `link.relList.value = "x"`), and a real browser reports `set: true` for them. Without
        /// this flag the readonly check below faults a correct implementation.
        put_forwards: bool,
    },
    /// `[static]? ReturnType name(args);`
    Operation {
        name: String,
        ret: String,
        args: Vec<Arg>,
        is_static: bool,
    },
    /// `const T name = value;`
    Const { name: String, ty: String, value: String },
    /// An anonymous special operation — `getter DOMString (DOMString name);`, `setter undefined
    /// (DOMString name, DOMString value);`, `deleter undefined (DOMString name);`. NOT a prototype
    /// member: it is the exotic-object [[Get]]/[[Set]]/[[Delete]] behavior, invisible to a prototype
    /// walk. The quality system therefore grades it ONLY through the ⑥ behavior axis (dynamic cases
    /// keyed by [`SpecialOp::member_name`]); an untested special contributes nothing (no false
    /// "missing member", no free verification).
    Special { special: SpecialOp },
}

/// One argument of a constructor or operation.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: String,
    /// The WebIDL type, spelled out (e.g. `unrestricted double`, `Float32Array`, `DOMPointInit`).
    pub ty: String,
    /// `optional T name` (or `optional T name = default`).
    pub optional: bool,
    /// `T... name` — the variadic final argument.
    pub variadic: bool,
}
