//! `macros` — proc-macros for the one-table Web-API definition
//!
//! - [`interface`] parses an interface's table and emits its `webapi::install::InterfaceDef` -- the
//!   union descriptor the single installer walks. This is the one-table rule made mechanical: one file,
//!   one table, and the shape half of every member spelled once.
//! - [`webapi`] (the older attribute macro) generates the `deno_core` `#[op2]` + `GarbageCollected`
//!   glue for a native cppgc interface `impl`. A table declares such a member with the body word
//!   `cppgc` (the class template realizes it; the installer verifies it).

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::punctuated::Punctuated;
use syn::{braced, parse_macro_input, Ident, ItemImpl, LitInt, LitStr, Token, Type};

// ---------------------------------------------------------------------------------------------------
// interface! { "Name" [: "Parent"] [model, exposed, flags...] { member, member, ... } }
// ---------------------------------------------------------------------------------------------------
//
// Header brackets: `model` (value|element|singleton|interface), then optional `exposed`
// (window|worker|both), then any of the flags `backfill`, `slot_backed`, `secure`, `tag "X"`,
// `tag none`, `alias "Y"` (repeatable).
//
// `secure` is the table's EXPOSURE CONDITION: the interface OBJECT is not on the
// global of a non-potentially-trustworthy origin at all. It is the hand-table spelling of the column
// `tools/sync/data/secure_context.json` stamps onto every generated row, so one interface states its
// condition the same way whichever table it lives in.
//
// Member grammar (one per row, comma-separated):
//   ctor           "name"              [len N]                          = body
//   attr           "name" : Type [readonly] [nullable] [lenient] [hidden] [fixed] [cfg "…"]  = body
//   method         "name" : Type [len N] [required N] [hidden] [fixed]  = body
//   static_method  "name" : Type [len N] [required N]                   = body
//   static_attr    "name" : Type [readonly]                             = body
//   constant       "name" : Type                                        = <literal | null>
//   handler        "name" [lenient]                                      = body
//   named_getter   "name" : Type                                        = body
//   indexed_getter "name" : Type                                        = body
//
// ANY member row may also carry `secure` -- its own exposure CONDITION, which matters
// on a `[Global]` interface because `Window`'s members ARE own properties of `globalThis`: four of
// its ~95 handler rows are gone on an insecure origin on all four browsers. It is NOT inherited from
// the table -- a `secure` interface takes its whole object away, so restating it per member would say
// nothing.
//
// ANY member row may also carry `window` / `worker` / `both` -- its own exposure, when it differs
// from the table's. WebIDL puts `[Exposed=Window]` on individual members of interfaces
// that are themselves `[Exposed=(Window,Worker)]`; `ServiceWorkerRegistration.cookies` is one. The
// generated registry has always carried the column, so without this word such an interface could not
// leave the registry for a hand table without changing its surface. Omitted, the row inherits the
// table's exposure, which is what every row did before the word existed.
//
// Type is one of the TypeTag idents (Undefined Boolean Integer Double DomString UsvString Object
// Promise Sequence Any). `body` is `stub` (generator-owned; the installer mounts the D2 stub), `cppgc`
// (realized by this file's `#[op2] impl` class template; the installer verifies it), one of the shared
// body kits (`reflect_str`, `reflect_url`, `reflect_bool`, `reflect_ulong`, `reflect_long`,
// `reflect_enum`, `reflect_nullable_enum`, `reflect_element`, `event_handler`, `slot_get`,
// `slot_get_null`, `slot_get_set`), or the ident of a `fn(&mut PinScope, Local<Object>, &'static
// MemberDef)` the human wrote.

struct InterfaceInput {
    name: LitStr,
    parent: Option<LitStr>,
    model: Ident,
    exposed: Ident,
    backfill: bool,
    slot_backed: bool,
    tag: Tag,
    aliases: Vec<LitStr>,
    /// The table's EXPOSURE CONDITION: `secure` in the header brackets means the
    /// interface OBJECT is not on the global of a non-potentially-trustworthy origin at all, which
    /// is what all four browsers do with ~230 of them. Omitted = unconditional.
    secure: bool,
    /// `class <Type>`: the OWN class (`#[webapi(own)]`) whose generated install fns the
    /// table's `cppgc` rows name -- `<Type>::__ki_<name>`, `<Type>::__kis_<name>` for a static.
    class: Option<syn::Path>,
    members: Punctuated<MemberInput, Token![,]>,
}

enum Tag {
    SameAsName,
    Some(LitStr),
    None,
}

struct MemberInput {
    kind: Ident, // attr | method | static_method | static_attr | constant | handler | ctor | named_getter | indexed_getter
    name: LitStr,
    ty: Option<Ident>,
    readonly: bool,
    length: u32,
    required: u32,
    nullable: bool,
    /// WebIDL `[LegacyLenientThis]`: the member answers `undefined` for a foreign receiver instead
    /// of throwing. Three members in all of webref carry it, all `EventHandler` attributes.
    lenient_this: bool,
    hidden: bool,
    fixed: bool,
    cfg: Option<LitStr>,
    /// PER-MEMBER exposure, when the row differs from the table's. WebIDL puts
    /// `[Exposed=Window]` on individual members of an interface that is itself `[Exposed=*]` --
    /// `ServiceWorkerRegistration.cookies` is one, on an interface both captures list. The generated
    /// registry has always carried the column; until this word existed a hand table could not say it,
    /// so an interface with one such member could not be tabled at all.
    exposed: Option<Ident>,
    /// PER-MEMBER exposure CONDITION: `secure` on a row of a `[Global]` interface --
    /// `Window`'s members are own properties of `globalThis` -- means the name is gone on an insecure
    /// origin. Four of `Window`'s ~95 event-handler rows carry it on all four browsers.
    secure: bool,
    body: Body,
    cval: proc_macro2::TokenStream, // a `ConstVal::…` expr; `ConstVal::None` for non-const members
}

enum Body {
    Stub,
    Cppgc,
    Install(Ident),
}

impl Parse for InterfaceInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: LitStr = input.parse()?;
        let parent = if input.peek(Token![:]) {
            input.parse::<Token![:]>()?;
            Some(input.parse::<LitStr>()?)
        } else {
            None
        };
        // `[model, exposed?, flags*]` -- model required, exposed optional (defaults to window).
        let bracket;
        syn::bracketed!(bracket in input);
        let model: Ident = bracket.parse()?;
        let mut exposed = Ident::new("window", model.span());
        let mut backfill = false;
        let mut slot_backed = false;
        let mut tag = Tag::SameAsName;
        let mut aliases = Vec::new();
        let mut secure = false;
        let mut class = None;
        let mut first = true;
        while bracket.peek(Token![,]) {
            bracket.parse::<Token![,]>()?;
            if bracket.is_empty() {
                break;
            }
            let word: Ident = bracket.parse()?;
            match word.to_string().as_str() {
                "window" | "worker" | "both" => exposed = word,
                "backfill" => backfill = true,
                "slot_backed" => slot_backed = true,
                "tag" => {
                    if bracket.peek(LitStr) {
                        tag = Tag::Some(bracket.parse::<LitStr>()?);
                    } else {
                        let none: Ident = bracket.parse()?;
                        if none != "none" {
                            return Err(syn::Error::new(none.span(), "`tag` takes a string literal or `none`"));
                        }
                        tag = Tag::None;
                    }
                }
                "alias" => aliases.push(bracket.parse::<LitStr>()?),
                // The table's own exposure condition, stated beside its name.
                "secure" => secure = true,
                "class" => class = Some(bracket.parse::<syn::Path>()?),
                other => return Err(syn::Error::new(word.span(), format!("unknown header flag `{other}` (expected window|worker|both, backfill, slot_backed, secure, tag, alias, class)"))),
            }
            first = false;
        }
        let _ = first;
        let body;
        braced!(body in input);
        let members = Punctuated::parse_terminated(&body)?;
        Ok(InterfaceInput { name, parent, model, exposed, backfill, slot_backed, tag, aliases, secure, class, members })
    }
}

impl Parse for MemberInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let kind: Ident = input.parse()?;
        let kind_s = kind.to_string();
        let name: LitStr = input.parse()?;
        let mut ty = None;
        if input.peek(Token![:]) {
            input.parse::<Token![:]>()?;
            ty = Some(input.parse::<Ident>()?);
        }
        let mut readonly = false;
        let mut length: u32 = 0;
        let mut required: u32 = 0;
        let mut nullable = false;
        let mut lenient_this = false;
        let mut hidden = false;
        let mut fixed = false;
        let mut cfg = None;
        let mut exposed = None;
        let mut secure = false;
        // Optional bare modifiers before `=`.
        while !input.peek(Token![=]) {
            let m: Ident = input.parse()?;
            match m.to_string().as_str() {
                "readonly" => readonly = true,
                "len" => length = input.parse::<LitInt>()?.base10_parse()?,
                "required" => required = input.parse::<LitInt>()?.base10_parse()?,
                "nullable" => nullable = true,
                "lenient" => lenient_this = true,
                "hidden" => hidden = true,
                "fixed" => fixed = true,
                "cfg" => cfg = Some(input.parse::<LitStr>()?),
                // PER-MEMBER exposure. Omitted, the row inherits the table's — which is what every
                // row of every table did before this word existed.
                "window" | "worker" | "both" => exposed = Some(m),
                // This ROW's exposure condition. Not inherited from the table: an
                // interface that is `secure` takes its whole object away, so a member restating it
                // would say nothing.
                "secure" => secure = true,
                other => return Err(syn::Error::new(m.span(), format!("unknown member modifier `{other}`"))),
            }
        }
        input.parse::<Token![=]>()?;
        // A `constant`'s RHS is its VALUE (a literal or `null`), carried as a ConstVal; every other
        // kind's RHS is a body word (`stub`, `cppgc`, a kit, or an `InstallFn` ident).
        let (body, cval) = if kind_s == "constant" || kind_s == "static_constant" {
            (Body::Stub, parse_const_val(input)?)
        } else {
            let body_ident: Ident = input.parse()?;
            let b = match body_ident.to_string().as_str() {
                "stub" => Body::Stub,
                "cppgc" => Body::Cppgc,
                _ => Body::Install(body_ident),
            };
            (b, quote!(::webapi::install::ConstVal::None))
        };
        // A handler and a ctor carry no IDL return type in the table; everything else names one.
        if !matches!(kind_s.as_str(), "handler" | "ctor") && ty.is_none() {
            return Err(syn::Error::new(name.span(), format!("member kind `{kind_s}` needs a `: Type`")));
        }
        Ok(MemberInput { kind, name, ty, readonly, length, required, nullable, lenient_this, hidden, fixed, cfg, exposed, secure, body, cval })
    }
}

/// Parse a `constant`'s value: a number / bool / string literal, or the `null` keyword, into a
/// `ConstVal::…` expression. WebIDL constants are boolean/numeric (DOM consts are all integers); a
/// string/null is accepted for generality.
fn parse_const_val(input: ParseStream) -> syn::Result<proc_macro2::TokenStream> {
    if input.peek(Ident) {
        let id: Ident = input.parse()?;
        if id == "null" {
            return Ok(quote!(::webapi::install::ConstVal::Null));
        }
        return Err(syn::Error::new(id.span(), format!("const value must be a literal or `null`, got `{id}`")));
    }
    let lit: syn::Lit = input.parse()?;
    Ok(match lit {
        syn::Lit::Bool(b) => { let v = b.value; quote!(::webapi::install::ConstVal::Bool(#v)) }
        syn::Lit::Int(i) => { let v: f64 = i.base10_parse()?; quote!(::webapi::install::ConstVal::Num(#v)) }
        syn::Lit::Float(f) => { let v: f64 = f.base10_parse()?; quote!(::webapi::install::ConstVal::Num(#v)) }
        syn::Lit::Str(s) => quote!(::webapi::install::ConstVal::Str(#s)),
        other => return Err(syn::Error::new(other.span(), "const value must be a number, bool, string, or `null`")),
    })
}

fn type_tag(ty: &Ident) -> syn::Result<proc_macro2::TokenStream> {
    let ok = matches!(
        ty.to_string().as_str(),
        "Undefined" | "Boolean" | "Integer" | "Double" | "DomString" | "UsvString" | "Object" | "Promise" | "Sequence" | "Any"
    );
    if !ok {
        return Err(syn::Error::new(ty.span(), format!("unknown TypeTag `{ty}`")));
    }
    Ok(quote!(::webapi::install::TypeTag::#ty))
}

/// The shared body kits a table may name without importing anything: the installer-side functions
/// that mount one member from its row (`cfg`, `name`, kind). Anything else is a user `InstallFn`.
fn kit_path(ident: &Ident) -> Option<proc_macro2::TokenStream> {
    let ok = matches!(
        ident.to_string().as_str(),
        "reflect_str" | "reflect_nullable_str" | "reflect_url" | "reflect_bool" | "reflect_ulong" | "reflect_long"
            | "reflect_enum" | "reflect_nullable_enum" | "reflect_element" | "reflect_tokens" | "event_handler" | "slot_get"
            | "slot_get_null" | "slot_get_set" | "array_proto_fn"
            // `@@iterator` as the prototype's own `values`/`entries` (a setlike's, a maplike's);
            // `@@unscopables`, the null-prototype object of the names in `cfg`.
            | "own_alias" | "unscopables"
            // The BRANDED slot kits. Same slot read as `slot_*`, behind the per-interface
            // receiver check `core/events/ctor.rs` stamps, so a row of the event family answers
            // `TypeError: Illegal invocation` on a foreign receiver the way every browser does. The
            // slot is the row's `cfg` when it carries one, which is how `MouseEvent.x` reads `clientX`.
            | "evt_get" | "evt_get_null" | "evt_get_set" | "evt_handler"
    );
    if ok { Some(quote!(::webapi::install::kits::#ident)) } else { None }
}

/// `interface! { ... }` -> `pub static <SCREAMING>: InterfaceDef = ...;`
#[proc_macro]
pub fn interface(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as InterfaceInput);
    let name = input.name.value();
    let static_ident = format_ident!("{}", screaming(&name));

    let model = match input.model.to_string().as_str() {
        "value" => quote!(::webapi::install::Model::Value),
        "element" => quote!(::webapi::install::Model::Element),
        "singleton" => quote!(::webapi::install::Model::Singleton),
        "interface" => quote!(::webapi::install::Model::Interface),
        other => return err(input.model.span(), format!("model must be value|element|singleton|interface, got `{other}`")),
    };
    let exposed = match input.exposed.to_string().as_str() {
        "window" => quote!(::webapi::install::Exposed::Window),
        "worker" => quote!(::webapi::install::Exposed::Worker),
        "both" => quote!(::webapi::install::Exposed::Both),
        other => return err(input.exposed.span(), format!("exposed must be window|worker|both, got `{other}`")),
    };
    // The table's exposure CONDITION, beside its realm set. `secure` in the header.
    let iface_exposure = if input.secure {
        quote!(::webapi::install::Exposure::SecureContext)
    } else {
        quote!(::webapi::install::Exposure::Unconditional)
    };
    let parent = match &input.parent {
        Some(p) => { let s = p.value(); quote!(Some(#s)) }
        None => quote!(None),
    };
    let tag = match &input.tag {
        Tag::SameAsName => quote!(Some(#name)),
        Tag::Some(s) => { let v = s.value(); quote!(Some(#v)) }
        Tag::None => quote!(None),
    };
    let backfill = input.backfill;
    let slot_backed = input.slot_backed;
    let aliases: Vec<String> = input.aliases.iter().map(|a| a.value()).collect();

    let mut members = Vec::new();
    let mut ctor_tokens = quote!(None);
    for m in &input.members {
        let mname = m.name.value();
        let label = format!("{name}::{mname}");
        // A `ctor` row becomes the interface's CtorDef, not a prototype member.
        if m.kind == "ctor" {
            let length = m.length;
            let (is_stub, install) = match &m.body {
                Body::Stub | Body::Cppgc => (true, quote!(::webapi::install::stub_install_sentinel)),
                Body::Install(f) => (false, quote!(#f)),
            };
            ctor_tokens = quote!(Some(::webapi::install::CtorDef {
                name: #mname, label: #label, length: #length, is_stub: #is_stub, install: #install,
            }));
            continue;
        }
        let kind = match m.kind.to_string().as_str() {
            "attr" => { let ro = m.readonly; quote!(::webapi::install::MemberKind::Attr { readonly: #ro }) }
            "method" => quote!(::webapi::install::MemberKind::Method),
            "static_method" => quote!(::webapi::install::MemberKind::StaticMethod),
            "static_attr" => { let ro = m.readonly; quote!(::webapi::install::MemberKind::StaticAttr { readonly: #ro }) }
            // A WebIDL constant lives on BOTH holders: `constant` is the prototype's row and
            // `static_constant` the interface object's (the registry carries every constant as two rows
            // the same way; the installer mounts each on its holder).
            "constant" | "static_constant" => quote!(::webapi::install::MemberKind::Const),
            "handler" => quote!(::webapi::install::MemberKind::Handler),
            // A named/indexed property getter is a V8 instance interceptor, not a prototype member;
            // the table declares it (for coverage) and the live phase mounts it at ctor-template time.
            "named_getter" => quote!(::webapi::install::MemberKind::NamedGetter),
            "indexed_getter" => quote!(::webapi::install::MemberKind::IndexedGetter),
            other => return err(m.kind.span(), format!("unknown member kind `{other}`")),
        };
        // A handler's type is fixed (its getter reads null while stubbed); tag it Object.
        let ty = match &m.ty {
            Some(t) => match type_tag(t) { Ok(t) => t, Err(e) => return e.to_compile_error().into() },
            None => quote!(::webapi::install::TypeTag::Object),
        };
        let length = m.length;
        let required = m.required;
        let nullable = m.nullable;
        let lenient_this = m.lenient_this;
        let (body, install) = match &m.body {
            Body::Stub => (quote!(::webapi::install::BodyKind::Stub), quote!(::webapi::install::stub_install_sentinel)),
            // An OWN class's member: the install fn `#[webapi(own)]` generated for this name.
            Body::Cppgc if input.class.is_some() => {
                let class = input.class.as_ref().unwrap();
                let f = if m.kind.to_string().starts_with("static") { format_ident!("__kis_{}", mname) } else { format_ident!("__ki_{}", mname) };
                (quote!(::webapi::install::BodyKind::Install), quote!(#class::#f))
            }
            Body::Cppgc => (quote!(::webapi::install::BodyKind::Cppgc), quote!(::webapi::install::stub_install_sentinel)),
            Body::Install(f) => {
                let path = kit_path(f).unwrap_or_else(|| quote!(#f));
                (quote!(::webapi::install::BodyKind::Install), path)
            }
        };
        let cval = &m.cval;
        let sm = m.kind.to_string().starts_with("static");
        // Descriptor flags. A WebIDL constant is non-configurable; every other member is configurable +
        // enumerable unless the row says `hidden` / `fixed`.
        let is_const = matches!(m.kind.to_string().as_str(), "constant" | "static_constant");
        let enumerable = !m.hidden;
        let configurable = !is_const && !m.fixed;
        let cfg = match &m.cfg { Some(c) => c.value(), None => String::new() };
        let member_exposure = if m.secure {
            quote!(::webapi::install::Exposure::SecureContext)
        } else {
            quote!(::webapi::install::Exposure::Unconditional)
        };
        // A hand table states its exposure ONCE, for the whole table, and every row inherits it --
        // UNLESS the row says otherwise. The per-member word exists because the generated
        // registry has always carried the column and a hand table could not: `ServiceWorkerRegistration`
        // is `[Exposed=(Window,Worker)]` with a WINDOW-ONLY `cookies` member, so before this the
        // interface could not be moved out of the registry at all without changing its surface.
        let member_exposed = match &m.exposed {
            None => exposed.clone(),
            Some(e) => match e.to_string().as_str() {
                "window" => quote!(::webapi::install::Exposed::Window),
                "worker" => quote!(::webapi::install::Exposed::Worker),
                "both" => quote!(::webapi::install::Exposed::Both),
                other => return err(e.span(), format!("member exposure must be window|worker|both, got `{other}`")),
            },
        };
        members.push(quote! {
            ::webapi::install::MemberDef {
                name: #mname, label: #label, kind: #kind, ty: #ty,
                length: #length, body: #body, install: #install, cval: #cval, static_member: #sm,
                exposed: #member_exposed,
                enumerable: #enumerable, configurable: #configurable, nullable: #nullable, required: #required, lenient_this: #lenient_this,
                cfg: #cfg,
                exposure: #member_exposure,
            }
        });
    }

    quote! {
        pub static #static_ident: ::webapi::install::InterfaceDef = ::webapi::install::InterfaceDef {
            name: #name,
            parent: #parent,
            model: #model,
            exposed: #exposed,
            ctor: #ctor_tokens,
            members: &[ #(#members),* ],
            tag: #tag,
            backfill: #backfill,
            slot_backed: #slot_backed,
            aliases: &[ #(#aliases),* ],
            exposure: #iface_exposure,
        };
    }
    .into()
}

/// "HTMLImageElement" -> "HTMLIMAGEELEMENT"; non-alphanumerics become `_`. A stable static name.
fn screaming(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect()
}

fn err(span: proc_macro2::Span, msg: String) -> TokenStream {
    syn::Error::new(span, msg).to_compile_error().into()
}

// ---------------------------------------------------------------------------------------------------
// #[webapi] -- the native-binding attribute macro.
// ---------------------------------------------------------------------------------------------------
//
// `#[webapi(base)]`, `#[webapi(inherit = Parent)]` or `#[webapi]` over an interface `impl` is the
// deno_core `#[op2]` class it always was: deno_core builds the class template, realises it on
// `Deno.core.ops`, and the install adopts it.
//
// `own` makes it an OWN class instead: no op2, no deno_core class template. The macro
// emits, for the struct `X`:
//   * the `GarbageCollected` impl (as before);
//   * every member's body as a private associated fn (`__kb_*`), the op2 attributes stripped;
//   * one V8 callback per member (`__kc_*`, a `fn(scope, args, rv)`), converting arguments and results
//     through the SAME `deno_core::_ops` helpers the op2 expansion calls -- read off that expansion
//     (`RUSTC_BOOTSTRAP=1 cargo rustc -p korvus-webapi --lib --profile check -- -Zunpretty=expanded`),
//     so a page sees the same conversions. The intended differences: a foreign receiver throws
//     `TypeError: Illegal invocation` (Chrome's) where op2 threw `expected X` with `code: ERR_INVALID_THIS`; and a
//     `#[serde]` / `#[object]` result is `binding::idl_value`'s -- ordinary objects of the realm Blink picks, where
//     serde_v8 made null-prototype ones;
//   * the constructor callback `__kc_ctor`: `new` required (Chrome's sentence, with the interface name),
//     then the body, then `wrap_object(this, value)` -- `this` is the API object V8 made for `new.target`;
//   * one install fn per JS name (`__ki_<name>`, statics `__kis_<name>`), which an `interface!` table
//     with `class X` in its header names for its `cppgc` rows: the ordinary member mount
//     (`binding::class::mount_*`), so the TemplateBuilder records it like every other row;
//   * `impl ::webapi::binding::class::OwnClass for X` -- `IFACE` is `name = "..."` or the ident.
// Any argument or result form this generator does not know is a compile error, never a guess.

struct WebapiArgs {
    base: bool,
    inherit: Option<Type>,
    own: bool,
    name: Option<LitStr>,
}

impl Parse for WebapiArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut a = WebapiArgs { base: false, inherit: None, own: false, name: None };
        while !input.is_empty() {
            let w: Ident = input.parse()?;
            match w.to_string().as_str() {
                "base" => a.base = true,
                "own" => a.own = true,
                "inherit" => {
                    input.parse::<Token![=]>()?;
                    a.inherit = Some(input.parse()?);
                }
                "name" => {
                    input.parse::<Token![=]>()?;
                    a.name = Some(input.parse()?);
                }
                other => return Err(syn::Error::new(w.span(), format!("unknown #[webapi] word `{other}` (base, inherit = T, own, name = \"..\")"))),
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(a)
    }
}

/// Expand an interface `impl` into native bindings (see the section comment above).
#[proc_macro_attribute]
pub fn webapi(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as WebapiArgs);
    let impl_block = parse_macro_input!(item as ItemImpl);

    let ident = match &*impl_block.self_ty {
        Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.clone()),
        _ => None,
    };
    let Some(ident) = ident else {
        return syn::Error::new_spanned(&impl_block.self_ty, "#[webapi] requires a simple named type")
            .to_compile_error()
            .into();
    };

    // The cppgc heap's name for the object: the interface's (`name = ".."`) when the struct's ident is not it.
    let gc_name = args.name.as_ref().map(|n| n.value()).unwrap_or_else(|| ident.to_string());
    let gc = quote! {
        unsafe impl ::deno_core::GarbageCollected for #ident {
            fn trace(&self, _v: &mut ::deno_core::v8::cppgc::Visitor) {}
            fn get_name(&self) -> &'static ::std::ffi::CStr {
                ::std::ffi::CStr::from_bytes_with_nul(concat!(#gc_name, "\0").as_bytes())
                    .expect("interface name has no interior nul")
            }
        }
    };

    if args.own {
        return match own_class(&ident, &args, impl_block) {
            Ok(t) => quote!(#gc #t).into(),
            Err(e) => e.to_compile_error().into(),
        };
    }

    // An op2 class's `#[serde]` result is deno_core's serde_v8 conversion, whose every object has a NULL
    // prototype -- never what a page may receive. Such a member belongs to an OWN class (`binding::idl_value`).
    for item in &impl_block.items {
        if let syn::ImplItem::Fn(f) = item {
            if let Some(a) = f.attrs.iter().find(|a| attr_is(a, "serde")) {
                return syn::Error::new(a.span(), "#[webapi] without `own`: a `#[serde]` result would reach the page as a null-prototype object; make the class `own`")
                    .to_compile_error()
                    .into();
            }
        }
    }

    let op2_attr = if args.base {
        quote!(#[::deno_core::op2(base)])
    } else if let Some(p) = &args.inherit {
        quote!(#[::deno_core::op2(inherit = #p)])
    } else {
        quote!(#[::deno_core::op2])
    };

    quote! {
        #gc

        #op2_attr
        #impl_block
    }
    .into()
}

#[derive(Clone, Copy, PartialEq)]
enum OwnKind {
    Ctor,
    Getter,
    Setter,
    Method,
    Static,
}

/// The op2 attributes an own class understands on a member fn. Anything else (docs, `allow`, `cfg`)
/// stays on the body fn.
fn is_op2_fn_attr(a: &syn::Attribute) -> bool {
    let Some(id) = a.path().get_ident() else { return false };
    matches!(
        id.to_string().as_str(),
        "constructor" | "getter" | "setter" | "static_method" | "rename" | "fast" | "nofast" | "reentrant" | "cppgc" | "serde"
            | "object" | "string" | "smi" | "buffer" | "undefined" | "required" | "validate"
    )
}

fn attr_is(a: &syn::Attribute, name: &str) -> bool {
    a.path().is_ident(name)
}

/// Normalised token text of a type: `Option < f64 >` -> `Option<f64>`.
fn ty_text(ty: &Type) -> String {
    quote!(#ty).to_string().replace(' ', "")
}

/// What an own member's Rust RESULT becomes (inside `Ok(..)` when the fn returns a `Result`).
enum RetConv {
    /// `()`: nothing is set -- a WebIDL `undefined` (the op2 class's install finished its `()` to this).
    Unit,
    /// a primitive op2 set through `RustToV8RetVal::to_v8_rv`
    Primitive,
    /// `#[cppgc] T`: a new wrapper of the calling realm
    Cppgc,
    /// the constructor's `#[cppgc] Self`: wrap `this`
    CtorWrap,
    /// `#[serde] T`: a typed WebIDL result -- a dictionary, a sequence, a record -- as Blink's binding converts it
    /// (`binding::idl_value::to_v8_for`): ordinary objects and arrays of the RECEIVER's relevant realm (a static
    /// member's: the current realm). Not deno_core's serde_v8, whose objects have no prototype.
    Serde,
    /// `#[object] T`: an IDL `object` the member builds itself -- a `toJSON()`, which Blink makes with a
    /// `V8ObjectBuilder` (`v8::Object::New(isolate)`): an ordinary object of the CURRENT realm
    /// (`binding::idl_value::to_v8`).
    Object,
    /// `#[string]` / `#[buffer]`: `RustToV8Fallible` straight
    Fallible,
    /// a V8 handle type with no attribute (`v8::Global<v8::Object>`): the `ToV8Marker` route
    ToV8,
    /// a `v8::Local<..>` with no attribute: `RustToV8NoScope`
    NoScope,
}

fn own_class(ident: &Ident, args: &WebapiArgs, imp: ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    let iface = args.name.as_ref().map(|n| n.value()).unwrap_or_else(|| ident.to_string());
    let unwrap = if args.base {
        quote!(::deno_core::_ops::try_unwrap_cppgc_base_object::<#ident>)
    } else {
        quote!(::deno_core::_ops::try_unwrap_cppgc_object::<#ident>)
    };

    let mut bodies = Vec::new();
    let mut cbs = Vec::new();
    // (js name, getter cb, setter cb) in first-seen order
    let mut accessors: Vec<(String, Option<Ident>, Option<Ident>)> = Vec::new();
    let mut methods: Vec<(String, Ident)> = Vec::new();
    let mut statics: Vec<(String, Ident)> = Vec::new();
    let mut has_ctor = false;
    let mut passthrough = Vec::new();

    for item in imp.items {
        let syn::ImplItem::Fn(f) = item else {
            passthrough.push(item);
            continue;
        };
        let span = f.sig.ident.span();
        let mut kind = OwnKind::Method;
        let mut rename: Option<String> = None;
        let mut ret_attr: Option<String> = None;
        let mut validate: Option<syn::Path> = None;
        let mut keep_attrs = Vec::new();
        for a in &f.attrs {
            if !is_op2_fn_attr(a) {
                keep_attrs.push(a.clone());
                continue;
            }
            let n = a.path().get_ident().unwrap().to_string();
            match n.as_str() {
                "constructor" => kind = OwnKind::Ctor,
                "getter" => kind = OwnKind::Getter,
                "setter" => kind = OwnKind::Setter,
                "static_method" => kind = OwnKind::Static,
                "rename" => {
                    let lit: LitStr = a.parse_args()?;
                    rename = Some(lit.value());
                }
                "cppgc" | "serde" | "object" | "string" | "smi" | "buffer" => ret_attr = Some(n),
                // op2's `#[validate(f)]`: `f(&mut scope, &args)` before anything else, its `Err` thrown
                "validate" => validate = Some(a.parse_args()?),
                // op2 dispatch hints with no meaning for a slow V8 callback
                "fast" | "nofast" | "reentrant" => {}
                other => return Err(syn::Error::new(a.span(), format!("#[webapi(own)]: op2 attribute `{other}` is not supported yet"))),
            }
        }
        let rust_name = f.sig.ident.to_string();
        let js = rename.clone().unwrap_or_else(|| stringcase::camel_case(&rust_name));
        let tag = match kind {
            OwnKind::Ctor => "ctor".to_string(),
            OwnKind::Getter => format!("get_{js}"),
            OwnKind::Setter => format!("set_{js}"),
            OwnKind::Method => format!("m_{js}"),
            OwnKind::Static => format!("s_{js}"),
        };
        // The member's context, as Blink's messages put it in front of a conversion's refusal (V8's exception
        // propagation callback, filled from the binding's interface and property names).
        let prefix = match kind {
            OwnKind::Ctor => format!("Failed to construct '{iface}': "),
            OwnKind::Setter => format!("Failed to set the '{js}' property on '{iface}': "),
            OwnKind::Getter => format!("Failed to read the '{js}' property from '{iface}': "),
            OwnKind::Method | OwnKind::Static => format!("Failed to execute '{js}' on '{iface}': "),
        };
        let body_ident = format_ident!("__kb_{}", tag);
        let cb_ident = if kind == OwnKind::Ctor { format_ident!("__kc_ctor") } else { format_ident!("__kc_{}", tag) };

        // ── parameters ───────────────────────────────────────────────────────────────────────────
        let mut has_self = false;
        let mut conv = Vec::new();
        let mut call_args = Vec::new();
        let mut body_inputs: Punctuated<syn::FnArg, Token![,]> = Punctuated::new();
        let mut js_idx: i32 = 0;
        let mut needs_state = false;
        for (i, input) in f.sig.inputs.iter().enumerate() {
            match input {
                syn::FnArg::Receiver(r) => {
                    if r.reference.is_none() || r.mutability.is_some() {
                        return Err(syn::Error::new_spanned(r, "#[webapi(own)]: only `&self` receivers"));
                    }
                    has_self = true;
                    body_inputs.push(input.clone());
                }
                syn::FnArg::Typed(pt) => {
                    let mut clean = pt.clone();
                    clean.attrs.retain(|a| !matches!(a.path().get_ident().map(|i| i.to_string()).as_deref(), Some("string" | "serde" | "dict" | "smi" | "buffer" | "anybuffer" | "webidl" | "this" | "cppgc")));
                    body_inputs.push(syn::FnArg::Typed(clean));
                    let a = format_ident!("__a{}", i);
                    let t = ty_text(&pt.ty);
                    let has = |n: &str| pt.attrs.iter().any(|x| attr_is(x, n));
                    let idx = js_idx;
                    let (stmts, expr, consumes) = if has("this") {
                        (quote! { let #a = ::deno_core::v8::Global::new(scope, args.this()); }, quote!(#a), false)
                    } else if t.starts_with("&mutv8::PinScope") || t.starts_with("&mutPinScope") {
                        (quote! {}, quote!(&mut *scope), false)
                    } else if t == "&OpState" || t == "&deno_core::OpState" {
                        needs_state = true;
                        (quote! { let #a = __state.borrow(); }, quote!(&#a), false)
                    } else if t == "&mutOpState" || t == "&mutdeno_core::OpState" {
                        needs_state = true;
                        (quote! { let mut #a = __state.borrow_mut(); }, quote!(&mut #a), false)
                    } else if let Some(d) = pt.attrs.iter().find(|x| attr_is(x, "dict")) {
                        // A WebIDL dictionary: converted as WebIDL converts it -- members in
                        // WebIDL's order, each read once, ToNumber/ToBoolean/enum/interface checks with Blink's
                        // messages (`ctor_dicts::convert_dict`) -- and the CONVERTED object handed to serde. serde_v8
                        // over the page's object read the struct's fields in declaration order and refused a "3" for
                        // a number with its own sentence ("serde_v8 error: invalid type; expected: number, got: string").
                        let dict: LitStr = d.parse_args()?;
                        (quote! {
                            let #a = args.get(#idx);
                            let Ok(#a) = ::webapi::install::ctor_dicts::convert_dict(scope, #a, #dict, #prefix) else { return };
                            let #a = match ::deno_core::_ops::serde_v8_to_rust(scope, #a.into()) {
                                Ok(t) => t,
                                Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); return; }
                            };
                        }, quote!(#a), true)
                    } else if has("serde") {
                        (quote! {
                            let #a = args.get(#idx);
                            let #a = match ::deno_core::_ops::serde_v8_to_rust(scope, #a) {
                                Ok(t) => t,
                                Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); return; }
                            };
                        }, quote!(#a), true)
                    } else if has("string") && t == "String" {
                        // a DOMString: ToString (its exception propagating), a Symbol refused with the member's context
                        // -- deno's `to_string` made a Symbol "" (`new TextDecoder(Symbol())` read the label "")
                        (quote! {
                            let #a = args.get(#idx);
                            let Some(#a) = ::webapi::binding::class::idl_string(scope, #a, #prefix) else { return };
                        }, quote!(#a), true)
                    } else if has("string") && t == "Option<String>" {
                        (quote! {
                            let #a = args.get(#idx);
                            let #a = if #a.is_null_or_undefined() { None } else {
                                let Some(v) = ::webapi::binding::class::idl_string(scope, #a, #prefix) else { return };
                                Some(v)
                            };
                        }, quote!(#a), true)
                    } else if has("string") && (t == "&str" || t.starts_with("&'")) && t.ends_with("str") {
                        // op2's `&str`: ToString (its exception propagates), a Symbol refused with the member's context
                        (quote! {
                            let #a = args.get(#idx);
                            let Some(#a) = ::webapi::binding::class::idl_string(scope, #a, #prefix) else { return };
                        }, quote!(#a.as_str()), true)
                    } else if t == "f64" {
                        // An `unrestricted double` -- an argument or a writable attribute: WebIDL's ToNumber (a
                        // non-numeric string is NaN, `valueOf` is called), a Symbol or a BigInt refused with the
                        // member's context (Chrome 150: `new DOMPoint(Symbol())` is "Failed to construct 'DOMPoint':
                        // Cannot convert a Symbol value to a number"). op2's converter refused every non-number with
                        // deno's own sentence ("expected f64"): `new DOMPoint('2')` threw where Chrome makes x = 2.
                        (quote! {
                            let #a = args.get(#idx);
                            let Some(#a) = ::webapi::binding::class::idl_double(scope, #a, #prefix) else { return };
                        }, quote!(#a), true)
                    } else if t == "Option<f64>" {
                        // an optional `unrestricted double`: absent (`undefined`) is `None`; `null` converts, to 0
                        (quote! {
                            let #a = args.get(#idx);
                            let #a = if #a.is_undefined() { None } else {
                                let Some(v) = ::webapi::binding::class::idl_double(scope, #a, #prefix) else { return };
                                Some(v)
                            };
                        }, quote!(#a), true)
                    } else if matches!(t.as_str(), "f64" | "f32" | "u32" | "i32" | "u8" | "u16" | "i8" | "i16") {
                        let (helper, msg) = numeric_helper(&t, has("smi"));
                        (quote! {
                            let #a = args.get(#idx);
                            let Some(#a) = ::deno_core::_ops::#helper(&#a) else { ::webapi::binding::class::throw_expected(scope, #msg); return; };
                            let #a = #a as _;
                        }, quote!(#a), true)
                    } else if let Some(inner) = t.strip_prefix("Option<").and_then(|x| x.strip_suffix('>')).filter(|x| matches!(*x, "f64" | "f32" | "u32" | "i32" | "u8" | "u16" | "i8" | "i16")) {
                        let (helper, msg) = numeric_helper(inner, has("smi"));
                        (quote! {
                            let #a = args.get(#idx);
                            let #a = if #a.is_null_or_undefined() { None } else {
                                let Some(v) = ::deno_core::_ops::#helper(&#a) else { ::webapi::binding::class::throw_expected(scope, #msg); return; };
                                Some(v as _)
                            };
                        }, quote!(#a), true)
                    } else if has("buffer") && (t.starts_with("&[") || t.starts_with("&mut[")) && t.ends_with(']') {
                        // op2's typed-array view: `to_v8_slice::<T>`, its refusal sentence thrown as a TypeError
                        let elem: Type = syn::parse_str(t.trim_start_matches("&mut[").trim_start_matches("&[").trim_end_matches(']'))?;
                        let tmp = format_ident!("__t{}", i);
                        let view = if t.starts_with("&mut[") { quote!(#tmp.as_mut()) } else { quote!(#tmp.as_ref()) };
                        (quote! {
                            let #a = args.get(#idx);
                            let mut #tmp = match unsafe { ::deno_core::_ops::to_v8_slice::<#elem>(#a) } {
                                Ok(v) => v,
                                Err(msg) => { ::webapi::binding::class::throw_expected(scope, msg); return; }
                            };
                        }, view, true)
                    } else if has("webidl") && t == "String" {
                        // op2's `#[webidl]` DOMString: deno_core's WebIdlConverter, with the prefix and context
                        // op2 passes (`'call' on '<RustType>'`, `Argument <n>` -- verbatim, a B-row candidate);
                        // `#[webidl(default = "..")]` binds an ABSENT (undefined) argument to the default first.
                        let wa = pt.attrs.iter().find(|x| attr_is(x, "webidl")).unwrap();
                        let default: Option<LitStr> = match &wa.meta {
                            syn::Meta::List(l) => {
                                let nv: syn::MetaNameValue = syn::parse2(l.tokens.clone())?;
                                match nv.value {
                                    syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(ls), .. }) if nv.path.is_ident("default") => Some(ls),
                                    _ => return Err(syn::Error::new_spanned(&wa.meta, "#[webidl(..)]: only `default = \"..\"`")),
                                }
                            }
                            _ => None,
                        };
                        let fill = match default {
                            Some(d) => quote! { let #a: ::deno_core::v8::Local<::deno_core::v8::Value> = if #a.is_undefined() { ::deno_core::v8::String::new(scope, #d).unwrap().into() } else { #a }; },
                            None => quote! {},
                        };
                        let prefix = format!("Failed to execute 'call' on '{}'", ident);
                        let ctx = format!("Argument {}", idx);
                        (quote! {
                            let #a = args.get(#idx);
                            #fill
                            let #a = match <String as ::deno_core::webidl::WebIdlConverter>::convert(scope, #a, #prefix.into(), ::deno_core::webidl::ContextFn::new_borrowed(&|| ::std::borrow::Cow::Borrowed(#ctx)), &Default::default()) {
                                Ok(t) => t,
                                Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); return; }
                            };
                        }, quote!(#a), true)
                    } else if has("anybuffer") && t == "Option<&[u8]>" {
                        // op2's `#[anybuffer]`: an ArrayBuffer or any view (`to_v8_slice_any`), null/undefined -> None
                        let tmp = format_ident!("__t{}", i);
                        (quote! {
                            let #a = args.get(#idx);
                            let #tmp;
                            let #a = if #a.is_null_or_undefined() { None } else {
                                #tmp = match ::deno_core::_ops::to_v8_slice_any(#a) {
                                    Ok(v) => v,
                                    Err(msg) => { ::webapi::binding::class::throw_expected(scope, msg); return; }
                                };
                                Some(#tmp.as_ref())
                            };
                        }, quote!(#a), true)
                    } else if t == "bool" {
                        (quote! { let #a = args.get(#idx).is_true(); }, quote!(#a), true)
                    } else if t == "v8::Local<v8::Value>" || t == "v8::Local<'s,v8::Value>" || t == "deno_core::v8::Local<deno_core::v8::Value>" {
                        (quote! { let #a = args.get(#idx); }, quote!(#a), true)
                    } else {
                        return Err(syn::Error::new_spanned(&pt.ty, format!("#[webapi(own)]: parameter form `{t}` is not supported yet")));
                    };
                    if consumes {
                        js_idx += 1;
                    }
                    conv.push(stmts);
                    call_args.push(expr);
                }
            }
        }

        // ── the result ───────────────────────────────────────────────────────────────────────────
        let (is_result, ok_ty) = match &f.sig.output {
            syn::ReturnType::Default => (false, None),
            syn::ReturnType::Type(_, ty) => {
                let t = ty_text(ty);
                if let Some(inner) = result_ok(ty) { (true, Some((inner, t))) } else { (false, Some((ty_text(ty), t))) }
            }
        };
        let ok_text = ok_ty.as_ref().map(|(i, _)| i.clone()).unwrap_or_else(|| "()".to_string());
        let conv_kind = if kind == OwnKind::Ctor {
            RetConv::CtorWrap
        } else {
            match ret_attr.as_deref() {
                Some("cppgc") => RetConv::Cppgc,
                Some("serde") => RetConv::Serde,
                Some("object") => RetConv::Object,
                Some("string") | Some("buffer") => RetConv::Fallible,
                Some(other) => return Err(syn::Error::new(span, format!("#[webapi(own)]: result attribute `{other}` is not supported yet"))),
                None => match ok_text.as_str() {
                    "()" => RetConv::Unit,
                    "f64" | "f32" | "u32" | "i32" | "u8" | "u16" | "i8" | "i16" | "bool" => RetConv::Primitive,
                    t if t.starts_with("v8::Global<") || t.starts_with("deno_core::v8::Global<") => RetConv::ToV8,
                    t if t.starts_with("v8::Local<") || t.starts_with("deno_core::v8::Local<") => RetConv::NoScope,
                    other => return Err(syn::Error::new(span, format!("#[webapi(own)]: result form `{other}` is not supported yet"))),
                },
            }
        };
        let set_ok = match conv_kind {
            RetConv::Unit => quote! { let _ = __v; },
            RetConv::Primitive => quote! { ::deno_core::_ops::RustToV8RetVal::to_v8_rv(__v, &mut rv); },
            RetConv::Cppgc => quote! { let __o = ::webapi::binding::class::mint(scope, __v); rv.set(__o.into()); },
            RetConv::CtorWrap => quote! {
                let __o = ::deno_core::cppgc::wrap_object(scope, args.this(), __v);
                rv.set(__o.into());
            },
            // Blink's `${script_state}`: the receiver's relevant realm for a regular member, the current one for a static
            RetConv::Serde if kind == OwnKind::Static => quote! {
                match ::webapi::binding::idl_value::to_v8(scope, __v) {
                    Ok(v) => rv.set(v),
                    Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); }
                }
            },
            RetConv::Serde => quote! {
                match ::webapi::binding::idl_value::to_v8_for(scope, args.this(), __v) {
                    Ok(v) => rv.set(v),
                    Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); }
                }
            },
            RetConv::Object => quote! {
                match ::webapi::binding::idl_value::to_v8(scope, __v) {
                    Ok(v) => rv.set(v),
                    Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); }
                }
            },
            RetConv::Fallible => quote! {
                match ::deno_core::_ops::RustToV8Fallible::to_v8_fallible(__v, scope) {
                    Ok(v) => rv.set(v),
                    Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); }
                }
            },
            RetConv::NoScope => quote! { rv.set(::deno_core::_ops::RustToV8NoScope::to_v8(__v)); },
            RetConv::ToV8 => quote! {
                match ::deno_core::_ops::RustToV8Fallible::to_v8_fallible(::deno_core::_ops::RustToV8Marker::<::deno_core::_ops::ToV8Marker, _>::from(__v), scope) {
                    Ok(v) => rv.set(v),
                    Err(e) => { let __x = ::deno_core::error::to_v8_error(scope, &e); scope.throw_exception(__x); }
                }
            },
        };
        let finish = if is_result {
            quote! {
                match __r {
                    Ok(__v) => { #set_ok }
                    Err(__e) => {
                        let __x = ::deno_core::error::to_v8_error(scope, &__e);
                        scope.throw_exception(__x);
                    }
                }
            }
        } else {
            quote! { let __v = __r; #set_ok }
        };

        let receiver = if has_self {
            quote! {
                let Some(__self) = #unwrap(scope, args.this().into()) else {
                    ::webapi::binding::class::illegal_invocation(scope);
                    return;
                };
                let __self = unsafe { __self.as_ref() };
            }
        } else {
            quote! {}
        };
        let self_arg = if has_self { quote!(__self,) } else { quote!() };
        let state = if needs_state { quote! { let __state = ::deno_core::JsRuntime::op_state_from(scope); } } else { quote! {} };
        let validation = match &validate {
            Some(f) => quote! {
                if let Err(e) = #f(scope, &args) {
                    let __x = ::deno_core::error::to_v8_error(scope, &e);
                    scope.throw_exception(__x);
                    return;
                }
            },
            None => quote! {},
        };
        let new_check = if kind == OwnKind::Ctor {
            quote! {
                if args.new_target().is_undefined() {
                    ::webapi::binding::class::throw_use_new(scope, #iface);
                    return;
                }
            }
        } else {
            quote! {}
        };

        // the body fn: the member's own signature and block, op2 attributes off
        let generics = &f.sig.generics;
        let output = &f.sig.output;
        let block = &f.block;
        bodies.push(quote! {
            #(#keep_attrs)*
            fn #body_ident #generics (#body_inputs) #output #block
        });
        cbs.push(quote! {
            #[doc(hidden)]
            pub fn #cb_ident<'s>(scope: &mut ::deno_core::v8::PinScope<'s, '_>, args: ::deno_core::v8::FunctionCallbackArguments<'s>, mut rv: ::deno_core::v8::ReturnValue<'s>) {
                #new_check
                #validation
                #receiver
                #state
                #(#conv)*
                let __r = Self::#body_ident(#self_arg #(#call_args),*);
                #finish
            }
        });
        match kind {
            OwnKind::Ctor => has_ctor = true,
            OwnKind::Getter | OwnKind::Setter => {
                let at = match accessors.iter().position(|(n, _, _)| *n == js) {
                    Some(i) => i,
                    None => {
                        accessors.push((js.clone(), None, None));
                        accessors.len() - 1
                    }
                };
                if kind == OwnKind::Getter { accessors[at].1 = Some(cb_ident) } else { accessors[at].2 = Some(cb_ident) }
            }
            OwnKind::Method => methods.push((js, cb_ident)),
            OwnKind::Static => statics.push((js, cb_ident)),
        }
    }

    // ── the install fns a table's `cppgc` rows name ──────────────────────────────────────────────
    let mut installs = Vec::new();
    let install_sig = |name: &Ident| quote! {
        #[doc(hidden)]
        pub fn #name(scope: &mut ::deno_core::v8::PinScope, holder: ::deno_core::v8::Local<::deno_core::v8::Object>, m: &'static ::webapi::install::MemberDef)
    };
    for (js, get, set) in &accessors {
        let name = format_ident!("__ki_{}", js);
        let sig = install_sig(&name);
        let body = match (get, set) {
            (Some(g), Some(s)) => quote! { ::webapi::binding::class::mount_pair(scope, holder, m, Self::#g, Self::#s) },
            (Some(g), None) => quote! { ::webapi::binding::class::mount_getter(scope, holder, m, Self::#g) },
            (None, _) => return Err(syn::Error::new(ident.span(), format!("#[webapi(own)]: `{js}` has a setter and no getter"))),
        };
        installs.push(quote! { #sig { #body } });
    }
    for (js, cb) in &methods {
        let name = format_ident!("__ki_{}", js);
        let sig = install_sig(&name);
        installs.push(quote! { #sig { ::webapi::binding::class::mount_method(scope, holder, m, Self::#cb) } });
    }
    for (js, cb) in &statics {
        let name = format_ident!("__kis_{}", js);
        let sig = install_sig(&name);
        installs.push(quote! { #sig { ::webapi::binding::class::mount_method(scope, holder, m, Self::#cb) } });
    }
    let _ = has_ctor;

    let self_ty = &imp.self_ty;
    Ok(quote! {
        #[allow(non_snake_case, dead_code, unused_mut, unused_variables, clippy::too_many_arguments, clippy::needless_lifetimes, clippy::unit_arg, clippy::let_unit_value)]
        impl #self_ty {
            #(#passthrough)*
            #(#bodies)*
            #(#cbs)*
            #(#installs)*
        }
        impl ::webapi::binding::class::OwnClass for #ident {
            const IFACE: &'static str = #iface;
        }
    })
}

/// The `deno_core::_ops` numeric converter op2 uses for a Rust type, and its refusal sentence.
fn numeric_helper(t: &str, smi: bool) -> (Ident, String) {
    let base = match t {
        "u8" | "u16" | "u32" => "u32",
        "i8" | "i16" | "i32" => "i32",
        "f32" => "f32",
        _ => "f64",
    };
    let _ = smi;
    (format_ident!("to_{}_option", base), format!("expected {base}"))
}

/// `Result<T, E>` -> `T`'s text, for any path ending in `Result`.
fn result_ok(ty: &Type) -> Option<String> {
    let Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    if last.ident != "Result" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(ab) = &last.arguments else { return None };
    match ab.args.first()? {
        syn::GenericArgument::Type(t) => Some(ty_text(t)),
        _ => None,
    }
}
