//! `install` — the one-table-rule skeleton.
//!
//! An interface defined with `webapi::interface!` compiles to an [`InterfaceDef`]: a static
//! descriptor of its shape (name, parent, model, exposure) and its members (kind, IDL type, length,
//! and the native function that mounts each). The single [`installer`] joins that union of
//! descriptors with the active persona bundle's `surface.json` selector at context-creation time --
//! it mounts only the interfaces and members the bundle chose, in the bundle's order. There is no
//! JS glue and so no `[native code]` masking or bridge slots.
//!
//! This module is the descriptor vocabulary, the single D2 stub ([`stub`]), THE installer
//! ([`installer`]: the generated registries and the hand tables through one member mount), the
//! generated registries and their companions, and the kits a table names by word.

pub mod stub;
/// THE receiver check, derived from the table and applied by the one member mount.
pub mod receiver;
/// THE bundle selector: which of the union's rows the ACTIVE capture says this device
/// has, and in what order. Read by [`installer`]'s one create and its two backfills.
pub mod surface;
pub mod installer;
pub(crate) mod templates;
/// What an interface's instances carry beyond its members -- the interceptors -- set on its instance
/// template by the builder.
pub(crate) mod instance_shapes;
/// THE constructor body a generated row's `CtorDef` cannot carry, read by the one create.
pub mod ctors;
pub mod verify;
pub mod live;
pub mod dom_chain;
/// THE generated one-table registry: an `InterfaceDef` per interface of BOTH realms, from the capture,
/// each row (and each member row) carrying the realm(s) that expose it. Emitted by
/// `tools/sync/gen_shells.py`; `installer::install_registry` walks it, filtered by realm, at context
/// creation, and `installer::install_worker_registry` walks its `WORKER_ORDER` view at worker isolate
/// creation. There is no second registry -- that was the split invariant I8 closed.
pub mod generated_registry;
/// The registry shell constructors' init dictionaries, flattened in WebIDL's conversion order -- emitted
/// by `tools/sync/gen_ctor_dicts.py` from Blink 150's IDL (vendored in `gen/blink/idl`).
pub mod generated_ctor_dicts;
/// WebIDL dictionary conversion over [`generated_ctor_dicts`], for `cb_shell_ctor`.
pub mod ctor_dicts;
/// One-fact Blink rows from tools/sync/data/blink_members.json.
pub mod blink_member;
/// THE `FrozenArray<T>` attribute: frozen, and reused on a second read exactly where Blink 150's bindings reuse it
/// (`gen/blink/idl/blink-frozen-arrays.json`, C2).
pub(crate) mod frozen_array;
/// W1.3: the shared member bodies a table names by word (`reflect_str`, `event_handler`, `slot_get_set`, …).
pub mod kits;
/// The per-interface BRAND (so a row rejects a foreign receiver the way Chrome does) and the WebIDL
/// argument / dictionary readers every hand-written native constructor shares. Written by W4.b for
/// the event family, lifted here by W4.c when nine non-event interfaces needed the same thing.
pub mod webidl;

/// How an interface's `this` is realised -- the receiver the member bodies get.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    /// A cppgc Rust value object, one per instance (DOMRect). Bodies get `&self`.
    Value,
    /// An arena node reached through a `JsElement` wrapper (HTMLImageElement). Bodies get a node id.
    ///
    /// It is also the WebIDL fact `[HTMLConstructor]`, and the two coincide by
    /// construction: `[HTMLConstructor]` is declared by every HTML element interface and by nothing
    /// else. It is what separates this model's interface object from [`Model::Interface`]'s -- both
    /// answer `new X()` with `Failed to construct 'X': Illegal constructor`, and only this one
    /// answers a BARE `X()` with `Failed to construct 'X': Please use the 'new' operator, this DOM
    /// object constructor cannot be called as a function.`, because it really does declare a
    /// constructor (that is how a custom element is upgraded). `Element`, `HTMLMediaElement` and
    /// `HTMLModelElement` are therefore [`Model::Interface`] despite the name: webref gives them no
    /// `[HTMLConstructor]`, and Chrome answers the bare `Illegal constructor` for all three
    /// (measured over loopback, 2026-09-12).
    Element,
    /// One host object per page (Navigator). Bodies read the persona/context, not instance state.
    Singleton,
    /// A NON-constructible interface object: it exposes a prototype + statics, but `new X()` throws
    /// `Failed to construct 'X': Illegal constructor` and a bare `X()` throws `Illegal constructor`
    /// (the shape of most WebIDL interfaces, which declare no constructor at all). The bulk of the
    /// generated registry is this — what `binding::protos::build_illegal_ctor` builds.
    Interface,
}

/// A row's EXPOSURE CONDITION — the second half of "may this name be published", beside the first
/// half [`Exposed`] answers. Defined with the one gate in `core/html/exposure.rs` and re-exported
/// here so a descriptor (generated or hand) spells it `install::Exposure`.
pub use crate::core::html::exposure::Exposure;

/// Which global(s) expose an interface -- or one of its members. The worker global installs the same
/// table's `Worker`/`Both` rows, so there is never a second shell set (goal invariant I8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exposed {
    Window,
    Worker,
    Both,
}

impl Exposed {
    /// Does a row marked `self` belong in a realm installing as `realm`?
    ///
    /// `Both` on EITHER side means yes: a `Both` row is in every realm, and `Exposed::Both` passed as
    /// the realm means "install this table whole, do not filter" -- which is what every hand-table
    /// install site wants, because an `interface!` table states its exposure once, for the whole table.
    /// Only the two generated-registry walks pass a concrete realm.
    pub const fn covers(self, realm: Exposed) -> bool {
        matches!(self, Exposed::Both) || matches!(realm, Exposed::Both) || (self as u8) == (realm as u8)
    }
}

/// The IDL type of a member, reduced to what the installer and the D2 stub need: enough to derive a
/// shape-correct empty value for an unimplemented member (see [`stub::ShapeDefault`]). The full type
/// is in the vendored IDL; this is the projection the table carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeTag {
    /// A `void`/`undefined`-returning method.
    Undefined,
    Boolean,
    /// Any integer IDL type (`short`/`long`/`unsigned long`/...).
    Integer,
    /// `float`/`double`/`unrestricted double`.
    Double,
    DomString,
    UsvString,
    /// A nullable object/interface/dictionary reference.
    Object,
    Promise,
    /// `sequence<T>` / `FrozenArray<T>`.
    Sequence,
    /// `any` -- the widest type; defaults to `undefined`.
    Any,
}

/// Does `iface.member` count its arguments BEFORE it checks its receiver? An OVERLOADED promise-returning operation
/// does in Blink: its binding is an overload dispatcher, with no V8 signature (a promise operation refuses by
/// rejecting), that counts the fewest arguments any overload needs before the chosen overload checks `this` --
/// `CookieStore.prototype.set.call({})` rejects "1 argument required", `NavigatorLogin.prototype.setStatus`
/// likewise (Chrome 150, measured 2026-10-09). Every other operation checks the receiver first. The overload
/// sets are Blink 150's own (`gen/blink/idl/blink-overloads.json`, `tools/sync/extract_blink_idl.py`).
pub(crate) fn counts_arguments_first(iface: &str, member: &str, ty: TypeTag, required: u32) -> bool {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static OVERLOADED: OnceLock<HashSet<String>> = OnceLock::new();
    if !ty.rejects_rather_than_throws() || required == 0 {
        return false;
    }
    let set = OVERLOADED.get_or_init(|| {
        serde_json::from_str::<Vec<String>>(include_str!("../../gen/blink/idl/blink-overloads.json")).unwrap_or_default().into_iter().collect()
    });
    set.contains(&format!("{iface}.{member}"))
}

impl TypeTag {
    /// **THE production of WebIDL's "a promise-returning operation does not throw".** Every input
    /// failure a promise-returning operation discovers -- the receiver check, the argument count,
    /// an argument's type -- is delivered as a REJECTION of the promise it already committed to
    /// returning, not as a synchronous throw.
    ///
    /// MEASURED, Google Chrome 150.0.7871.46 `--headless=new` over a loopback server, 2026-09-23,
    /// awaiting each promise rather than inferring from `typeof r.then` (the committed probe
    /// an arity probe page only inferred):
    ///
    /// ```text
    ///   navigator.permissions.query()   REJECT TypeError: Failed to execute 'query' on
    ///                                          'Permissions': 1 argument required, but only 0 present.
    ///   caches.open()                   REJECT (same sentence, 'open' on 'CacheStorage')
    ///   crypto.subtle.digest()          REJECT (2 arguments required)
    ///   ... 25 promise members in all, every one a REJECT
    ///   document.getElementById()       THROW  <- the control: the probe CAN see a sync throw
    /// ```
    ///
    /// It is ONE fact and it reads off ONE column, so it is written once here rather than re-derived
    /// per guard: `install::receiver::policy` asks it for the receiver check and
    /// `binding::protos::cb_arity_guard` asks it for the argument count. Before ARITY1 those two
    /// spelled `m.ty == TypeTag::Promise` and `is_void`-and-nothing-else respectively, which is how
    /// the same rule came to be true of receivers and false of arity.
    pub const fn rejects_rather_than_throws(self) -> bool {
        matches!(self, TypeTag::Promise)
    }

    /// A WebIDL `undefined` return is not `null`: op2 renders a Rust `()` as null, so a void method
    /// answered null where Chrome answers undefined. `binding::protos::cb_arity_guard` needs this.
    pub const fn is_undefined_return(self) -> bool {
        matches!(self, TypeTag::Undefined)
    }

    /// The tag as a small integer, for packing into a V8 callback's associated data (which holds
    /// values, not Rust enums). [`TypeTag::from_ordinal`] is its inverse.
    ///
    /// AN EXHAUSTIVE `match` AND NOT `self as i32`, so a variant added to `TypeTag` is a COMPILE
    /// ERROR here rather than a silent drift between this and its inverse. A hand-maintained pair
    /// with nothing forcing them to agree is how a `Promise` row would start unpacking as something
    /// else and quietly go back to throwing.
    pub const fn ordinal(self) -> i32 {
        match self {
            TypeTag::Undefined => 0,
            TypeTag::Boolean => 1,
            TypeTag::Integer => 2,
            TypeTag::Double => 3,
            TypeTag::DomString => 4,
            TypeTag::UsvString => 5,
            TypeTag::Object => 6,
            TypeTag::Promise => 7,
            TypeTag::Sequence => 8,
            TypeTag::Any => 9,
        }
    }

    /// [`TypeTag::ordinal`]'s inverse. An unknown ordinal is `Any`, the tag this tree already uses
    /// for "no IDL type recorded" (see `install::stub::make_member`) -- and the tag whose behaviour
    /// matches what an absent slot used to mean: throw, and do not blank the return value.
    pub const fn from_ordinal(n: i64) -> TypeTag {
        match n {
            0 => TypeTag::Undefined,
            1 => TypeTag::Boolean,
            2 => TypeTag::Integer,
            3 => TypeTag::Double,
            4 => TypeTag::DomString,
            5 => TypeTag::UsvString,
            6 => TypeTag::Object,
            7 => TypeTag::Promise,
            8 => TypeTag::Sequence,
            _ => TypeTag::Any,
        }
    }

    /// Every variant, so the round trip below can be exhaustive without naming them a third time.
    #[cfg(test)]
    const ALL: [TypeTag; 10] = [
        TypeTag::Undefined, TypeTag::Boolean, TypeTag::Integer, TypeTag::Double, TypeTag::DomString,
        TypeTag::UsvString, TypeTag::Object, TypeTag::Promise, TypeTag::Sequence, TypeTag::Any,
    ];
}

#[cfg(test)]
mod type_tag_tests {
    use super::TypeTag;

    /// The packing round trip, because `ordinal`/`from_ordinal` are a HAND-MAINTAINED PAIR and the
    /// only thing a guard reads the return type out of. If they disagree, a `Promise` row unpacks as
    /// some other tag and silently goes back to THROWING its argument-count refusal where Chrome
    /// rejects -- a regression with no compile error and no other test to catch it.
    #[test]
    fn the_ordinal_round_trip_is_the_identity_for_every_tag() {
        for t in TypeTag::ALL {
            assert_eq!(TypeTag::from_ordinal(t.ordinal() as i64), t, "{t:?}");
        }
        // Distinctness: a pair that mapped two tags to one number would still round-trip for one of
        // them, so assert the ordinals are 0..N with no collision.
        let mut ords: Vec<i32> = TypeTag::ALL.iter().map(|t| t.ordinal()).collect();
        ords.sort_unstable();
        assert_eq!(ords, (0..TypeTag::ALL.len() as i32).collect::<Vec<_>>());
    }

    /// The two facts the arity guard reads off the tag, stated once as assertions: exactly ONE tag
    /// rejects instead of throwing, and exactly ONE renders a WebIDL `undefined` return.
    #[test]
    fn exactly_one_tag_rejects_and_exactly_one_is_a_void_return() {
        assert_eq!(TypeTag::ALL.iter().filter(|t| t.rejects_rather_than_throws()).count(), 1);
        assert!(TypeTag::Promise.rejects_rather_than_throws());
        assert_eq!(TypeTag::ALL.iter().filter(|t| t.is_undefined_return()).count(), 1);
        assert!(TypeTag::Undefined.is_undefined_return());
        // An out-of-range ordinal (an absent data slot) must be the SAFE one: throw, not reject.
        assert!(!TypeTag::from_ordinal(99).rejects_rather_than_throws());
        assert!(!TypeTag::from_ordinal(-1).is_undefined_return());
    }
}

/// The kind of a member -- how it is installed and, for the stub, how it behaves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemberKind {
    Attr { readonly: bool },
    Method,
    Const,
    /// An `on*` event handler pair (get/set of the handler).
    Handler,
    StaticMethod,
    StaticAttr { readonly: bool },
    /// A named property getter (`obj[name]`) -- installed as a V8 interceptor, not a Proxy.
    NamedGetter,
    /// An indexed property getter (`obj[i]`).
    IndexedGetter,
}

/// The native function that mounts one member onto `proto`. It reuses the existing `build_fn` /
/// `define_method` / accessor / interceptor primitives; a `stub` body uses [`stub::install_stub`].
pub type InstallFn = fn(&mut deno_core::v8::PinScope, proto: deno_core::v8::Local<deno_core::v8::Object>, member: &'static MemberDef);

/// A WebIDL constant's value, carried in the table (`= <literal>` in the macro). A `const` is never
/// "absent" — its value IS its observable surface (`XMLHttpRequest.DONE === 4`), so the installer's
/// stub path reads this rather than mounting a getter. `None` for every non-const member. Const so it
/// lives in a `static InterfaceDef`; `Copy` so `MemberDef` construction stays trivial.
#[derive(Clone, Copy)]
pub enum ConstVal {
    None,
    Num(f64),
    Bool(bool),
    Str(&'static str),
    Null,
}

/// Which of the three body forms a row has. `Stub` is generator-owned (the installer mounts the D2
/// stub); `Install` names an `InstallFn` (a kit or a hand-written body); `Cppgc` says the member is
/// realized by this file's `#[op2] impl` class template -- the installer VERIFIES it (presence, kind,
/// WebIDL length, tag) rather than mounting it, and the ledger counts it as native.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BodyKind {
    Stub,
    Install,
    Cppgc,
}

/// One row of a table: its shape half (name/kind/ty/length, from the IDL) plus the native installer
/// its body half compiles to. `label` is the `"Iface::member"` coverage key -- static, so it can key
/// the same registry the corpus reads.
pub struct MemberDef {
    /// The property key. A well-known symbol is spelled `@@iterator` / `@@asyncIterator` /
    /// `@@toPrimitive` / `@@hasInstance` / `@@isConcatSpreadable` / `@@unscopables`
    /// (the installer's `member_key` resolves it); anything else is the string key itself.
    pub name: &'static str,
    pub label: &'static str,
    pub kind: MemberKind,
    pub ty: TypeTag,
    pub length: u32,
    /// The body form (see [`BodyKind`]). `install` is read only for `Install`; the other two carry the
    /// [`stub_install_sentinel`].
    pub body: BodyKind,
    pub install: InstallFn,
    /// For a `Const` member, its value (installed with the WebIDL constant descriptor). `None` for all
    /// other kinds.
    pub cval: ConstVal,
    /// True if this member lives on the interface OBJECT (a static), false if on the prototype. The
    /// `StaticMethod`/`StaticAttr` kinds already imply true, but a `Const` can be either (a WebIDL
    /// constant is on both; a JS-intrinsic static like `Number.MAX_SAFE_INTEGER` is static-only), so
    /// the split is carried explicitly — the single installer reads it to place each member.
    pub static_member: bool,
    /// Which global(s) expose THIS ROW. Usually the interface's own `exposed`, but WebIDL declares
    /// `[Exposed]` per member and the two captures disagree on twelve of them —
    /// `DOMMatrix.setMatrixValue`, `DOMMatrixReadOnly.toString`, `StorageManager.persist`,
    /// `USB.requestDevice` and seven more are window-only on an interface both realms expose, and
    /// `FileSystemFileHandle.createSyncAccessHandle` is worker-only. The installer filters on it, which
    /// is what lets ONE table serve both globals instead of two registries (goal invariant I8). Macro
    /// default: the interface's exposure, because an `interface!` table states it once for the table.
    pub exposed: Exposed,
    /// The property descriptor's `enumerable` flag as the capture recorded it. Almost always `true` for
    /// a web-interface member, but an ES-intrinsic member (Object.prototype.hasOwnProperty, …) is
    /// `false`; carried faithfully so the registry reproduces the shell surface byte-for-byte. Macro
    /// default `true`.
    pub enumerable: bool,
    /// The property descriptor's `configurable` flag. `true` for methods/accessors; a WebIDL constant is
    /// `false` (non-configurable), while an ES-intrinsic constant (Error.captureStackTrace and the like)
    /// can be `true`. Carried so seal/order/redefine reproduce the captured shape. Macro default `true`.
    pub configurable: bool,
    /// The member returns `null` (not `undefined`) when empty — a nullable IDL attribute. Drives the
    /// stub/slot getter to hand back `null`. `ShellMember.z`. Macro default `false`.
    pub nullable: bool,
    /// The WebIDL required-argument count: below it a method throws `TypeError` (the arity guard). 0 =
    /// all-optional (no guard). `ShellMember.q`; usually equals `length` but not always (a defaulted
    /// overload). Macro default 0.
    pub required: u32,
    /// WebIDL `[LegacyLenientThis]`: reached through a receiver that is not an instance, this member
    /// answers `undefined` instead of throwing `TypeError: Illegal invocation` — for the getter AND
    /// the setter (measured on Chrome 150.0.7871.46). Read by [`receiver::policy`], the one place the
    /// installer decides a row's receiver check.
    ///
    /// Exactly three members in all of webref carry it and all three are `EventHandler` attributes
    /// (`Document.onreadystatechange`, `GlobalEventHandlers.onmouseenter`/`onmouseleave`), which is
    /// why it is a per-row flag and not a mechanism: `tools/sync/gen_shells.py` reads it from the
    /// same webref parse that answers nullability, and a hand table spells it `lenient`. Macro
    /// default `false`.
    pub lenient_this: bool,
    /// The kit configuration string a body reads (`cfg "…"` in the macro): the content attribute (and
    /// `|default|mode`, `|keywords|missing|invalid`) for the `reflect_*` kits. Empty = the kit derives it
    /// from the member name.
    pub cfg: &'static str,
    /// THIS ROW'S EXPOSURE CONDITION. It matters for the rows of a `[Global]`
    /// interface, whose members ARE own properties of the global: four of `Window`'s event-handler
    /// rows — `ondevicemotion`, `ondeviceorientation`, `ondeviceorientationabsolute` and
    /// `onpointerrawupdate` — are gone from `Object.getOwnPropertyNames(window)` on an insecure
    /// origin on all four browsers, while the other ~90 `on*` rows beside them are not.
    ///
    /// Macro default [`Exposure::Unconditional`], and NOT inherited from the interface the way
    /// [`MemberDef::exposed`] is: an interface that is `[SecureContext]` takes its whole object away,
    /// so restating the condition on each of its members would say nothing. A row spelled `secure`
    /// is making a claim of its own. Spelled `secure` in a hand table.
    pub exposure: Exposure,
}

/// The constructor row, if the interface is constructible (`ctor` / legacy factory like `Image`).
pub struct CtorDef {
    pub name: &'static str,
    pub label: &'static str,
    pub length: u32,
    pub is_stub: bool,
    pub install: InstallFn,
}

/// The static descriptor an `interface!` invocation compiles to -- the union entry the installer
/// joins against the bundle selector. `sync` collects every one of these into `registry.rs`.
pub struct InterfaceDef {
    pub name: &'static str,
    pub parent: Option<&'static str>,
    pub model: Model,
    pub exposed: Exposed,
    pub ctor: Option<CtorDef>,
    pub members: &'static [MemberDef],
    /// `Symbol.toStringTag` for the prototype. Usually `Some(name)`, but a legacy alias reports its
    /// canonical interface (`Image` -> `"HTMLImageElement"`) and an ES intrinsic (Object, Array, …)
    /// reports `None` (no tag — the installer leaves V8's own). The installer sets the tag only when
    /// `Some`, so a table can't accidentally stamp a tag an ES builtin never had.
    pub tag: Option<&'static str>,
    /// The interface is natively IMPLEMENTED, so the member backfill additively mounts the catalogued
    /// members its real prototype does not already own (`Element.animate`, the ~189 `w:1` interfaces).
    /// A pure shell (no native prototype) is `false`. `ShellDef.w`. Macro default `false`.
    pub backfill: bool,
    /// The interface is slot-backed: an accessor reads the instance's private slot rather than a stub's
    /// `undefined` (MessageEvent.data, Response.*, the 23 `sl:1` interfaces). Drives the slotted-member
    /// backfill path. `ShellDef.sl`. Macro default `false`.
    pub slot_backed: bool,
    /// Other global names the same table installs as (`alias "WorkerNavigator"`: the Navigator table is
    /// the worker's `WorkerNavigator`). Read by the exclusivity scanners; the installer uses it to name
    /// the interface object in the other realm.
    pub aliases: &'static [&'static str],
    /// UNDER WHAT CONDITION THIS INTERFACE OBJECT IS ON THE GLOBAL.
    ///
    /// The companion of [`InterfaceDef::exposed`] and a different kind of fact: `exposed` says WHICH
    /// REALM has the name, this says which ORIGINS do. 230 of the ~248 names an insecure origin takes
    /// off a Chromium window are interface objects — the whole `GPU*`, `XR*`, `USB*`, `MIDI*`,
    /// `Presentation*` and `Sensor` families, `CacheStorage`, `SubtleCrypto`, `ServiceWorker*`,
    /// `PublicKeyCredential`, `WebTransport*` — and the condition rides on the row so that the one
    /// publish site in `installer::create_interface_in` (and the one in `live::rows::build_newable`)
    /// can pass it to `exposure::gate` without any of them holding a list.
    ///
    /// Generated rows get it from `tools/sync/data/secure_context.json`, which is the MEASUREMENT
    /// (four browsers, two origins, one server); a hand table spells it `secure`. Macro default
    /// [`Exposure::Unconditional`].
    pub exposure: Exposure,
}

impl InterfaceDef {
    /// The member with this name, if the union has it. `None` is a bundle that selected a member the
    /// table does not carry -- the installer reports an implementation gap and mounts a plain stub.
    pub fn member(&self, name: &str) -> Option<&'static MemberDef> {
        let members: &'static [MemberDef] = self.members;
        members.iter().find(|m| m.name == name)
    }

    /// The member with this name ON THE HOLDER the caller means: the prototype (`static_member`
    /// false) or the interface object (true).
    ///
    /// [`InterfaceDef::member`] cannot answer that, and the difference is not academic: a WebIDL
    /// CONSTANT is one name carried by TWO rows, because `Node.ELEMENT_NODE` sits on the prototype
    /// and on the interface object both. A by-name lookup always finds the prototype's, so a selector
    /// walking the capture's `statics` list through `member()` would mount the prototype row on the
    /// constructor and the interface object would lose its constants.
    pub fn member_on(&self, name: &str, static_member: bool) -> Option<&'static MemberDef> {
        let members: &'static [MemberDef] = self.members;
        members.iter().find(|m| m.name == name && m.static_member == static_member)
    }
}

/// The `install` an `interface!` table gives a `stub` (or `cppgc`) member. It is never called: the
/// installer's stub path (`installer::install_members`) sees the [`BodyKind`] and mounts the D2 stub itself. It exists
/// only so a stub row can name a function of the right type without the human writing one.
pub fn stub_install_sentinel(_: &mut deno_core::v8::PinScope, _: deno_core::v8::Local<deno_core::v8::Object>, _: &'static MemberDef) {
    debug_assert!(false, "stub_install_sentinel must not be called -- the installer's stub path handles is_stub members");
}
