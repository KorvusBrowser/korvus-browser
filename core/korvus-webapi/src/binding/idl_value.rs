//! `binding::idl_value` -- a Rust result handed to the page as the WebIDL value Blink 150 makes of it.
//!
//! THE RULE. What a page receives from a member is an ORDINARY object of a realm: a dictionary or an `object` a
//! `toJSON()` builds has that realm's `%Object.prototype%` and own data properties made by CreateDataProperty, in the
//! dictionary's (or the builder's) order; a sequence is an `Array` of that realm. serde_v8, which every `#[serde]`
//! result used to go through, makes neither: it builds every struct and map with
//! `v8::Object::with_prototype_and_properties(null, ..)` (serde_v8 0.319.0 `ser.rs:215-224`, `:370-380`) -- a
//! NULL-prototype object, so `rect.toJSON().hasOwnProperty('x')` threw (BrowserScan's Client Rects row went blank)
//! and `Object.getPrototypeOf(new TextEncoder().encodeInto(..))` was `null` where Chrome answers `Object.prototype`.
//!
//! WHICH REALM, as Blink's generated bindings choose it (`bindings/scripts/bind_gen/interface.py`):
//! * a typed result -- a dictionary, a sequence, a record -- of a regular attribute or operation is converted with
//!   `${script_state}`, which is `ScriptState::ForRelevantRealm(isolate, receiver)` (`interface.py:321-343`): the
//!   realm the RECEIVER was made in. `ToV8Traits<IDLSequence<T>>` builds its `v8::Array::New(script_state->
//!   GetContext(), ..)` there (`to_v8_traits.h:339`) and a dictionary its `DictionaryTemplate::NewInstance(
//!   script_state->GetContext(), ..)` (`dictionary.py:712-717`). A static member's is the current realm. Measured on
//!   Chrome 150: a frame's `TextEncoder.prototype.encodeInto` called on the page's encoder answers the PAGE's
//!   `Object.prototype`, a frame's `Headers.prototype.getSetCookie` on the page's headers the page's `Array`.
//!   [`to_v8_for`].
//! * an `object` the implementation builds -- every `toJSON()` (`[CallWith=ScriptState] object toJSON()`) -- is a
//!   `V8ObjectBuilder`, whose object is `v8::Object::New(isolate)` (`v8_object_builder.cc:11-13`): the CURRENT realm,
//!   the realm of the function called. Measured: a frame's `DOMRect.prototype.toJSON` on the page's rect answers the
//!   FRAME's `Object.prototype`. [`to_v8`].
//!
//! THE VALUES, as WebIDL converts them: a number is a Number (an `unsigned long long` too -- WebIDL's integer types
//! never become a BigInt; serde_v8 made one past 2^53), NaN, the infinities and -0 kept; a string a String; a Rust
//! `Option::None` `null` (a dictionary member that is NOT PRESENT is left out with `#[serde(skip_serializing_if)]`, as
//! Blink's `FillValuesImpl` leaves it unset); a unit enum variant its name (a WebIDL enum value). serde_v8's magic
//! types (a buffer, a `BigInt`, a `serde_v8::Value`) are still serde_v8's own conversion.
//!
//! The engine's PRIVATE bags stay null-prototype on purpose: an `#[op2]` fn's `#[serde]` result that only engine code
//! reads (`op_fetch`, `op_ice_stun`, ...), an init dictionary the engine hands a constructor
//! (`protos::init_dict`). Nothing of those reaches a page.

use std::cell::RefCell;

use deno_core::serde_v8;
use deno_core::v8;
use serde::ser::{self, Serialize};

type JsValue<'s> = v8::Local<'s, v8::Value>;
type JsResult<'s> = Result<JsValue<'s>, serde_v8::Error>;
/// The scope the serializers share, as serde_v8's own do (`ser.rs:26`).
type ScopeCell<'r, 'b, 's, 'i> = &'r RefCell<&'b mut v8::PinScope<'s, 'i>>;

/// `value` as the page's value, made in the CURRENT realm -- an `object` a `toJSON()` builds, or any result of a
/// static member.
pub fn to_v8<'s, T: Serialize>(scope: &mut v8::PinScope<'s, '_>, value: T) -> JsResult<'s> {
    let cell = RefCell::new(scope);
    value.serialize(IdlSerializer { scope: &cell })
}

/// `value` as the page's value, made in `receiver`'s RELEVANT realm -- a typed result (a dictionary, a sequence) of
/// a regular attribute or operation. The current realm when the receiver has no creation context.
pub fn to_v8_for<'s, T: Serialize>(scope: &mut v8::PinScope<'s, '_>, receiver: v8::Local<v8::Object>, value: T) -> JsResult<'s> {
    let realm = match receiver.get_creation_context(scope) {
        Some(r) if r != scope.get_current_context() => r,
        _ => return to_v8(scope, value),
    };
    let made = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        match to_v8(scope, value) {
            Ok(v) => Ok(v8::Global::new(scope, v)),
            Err(e) => Err(e),
        }
    };
    made.map(|g| v8::Local::new(scope, &g))
}

/// The prefix of the names serde_v8's magic types serialize under (`magic/transl8.rs:111`).
const MAGIC_PREFIX: &str = "$__v8_magic_";

/// An internalized key, as Blink's `V8AtomicString` and serde_v8's `v8_struct_key`.
fn key<'s>(scope: &v8::PinScope<'s, '_>, k: &str) -> Result<v8::Local<'s, v8::Name>, serde_v8::Error> {
    v8::String::new_from_utf8(scope, k.as_bytes(), v8::NewStringType::Internalized)
        .map(Into::into)
        .ok_or_else(|| <serde_v8::Error as ser::Error>::custom("Cannot allocate String: buffer exceeds maximum length."))
}

/// An ordinary object of the current realm with `keys` -> `values` defined in order, as `V8ObjectBuilder::AddInternal`
/// defines them (CreateDataProperty: an accessor a page put on `Object.prototype` is never called).
fn ordinary<'s>(scope: &mut v8::PinScope<'s, '_>, keys: &[v8::Local<'s, v8::Name>], values: &[JsValue<'s>]) -> JsResult<'s> {
    let obj = v8::Object::new(scope);
    for (k, v) in keys.iter().zip(values) {
        if obj.create_data_property(scope, *k, *v) != Some(true) {
            return Err(serde_v8::Error::V8Exception);
        }
    }
    Ok(obj.into())
}

struct IdlSerializer<'r, 'b, 's, 'i> {
    scope: ScopeCell<'r, 'b, 's, 'i>,
}

impl<'r, 'b, 's, 'i> IdlSerializer<'r, 'b, 's, 'i> {
    /// serde_v8's serializer over the same scope, for the leaves its conversion already makes as WebIDL does.
    fn leaf(&self) -> serde_v8::Serializer<'s, 'b, 'r, 'i> {
        serde_v8::Serializer::new(self.scope)
    }
}

impl<'r, 'b, 's, 'i> ser::Serializer for IdlSerializer<'r, 'b, 's, 'i> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    type SerializeSeq = IdlArray<'r, 'b, 's, 'i>;
    type SerializeTuple = IdlArray<'r, 'b, 's, 'i>;
    type SerializeTupleStruct = IdlArray<'r, 'b, 's, 'i>;
    type SerializeTupleVariant = IdlVariant<'r, 'b, 's, 'i, IdlArray<'r, 'b, 's, 'i>>;
    type SerializeMap = IdlObject<'r, 'b, 's, 'i>;
    type SerializeStruct = IdlStruct<'r, 'b, 's, 'i>;
    type SerializeStructVariant = IdlVariant<'r, 'b, 's, 'i, IdlStruct<'r, 'b, 's, 'i>>;

    fn serialize_bool(self, v: bool) -> JsResult<'s> { ser::Serializer::serialize_bool(self.leaf(), v) }
    fn serialize_i8(self, v: i8) -> JsResult<'s> { ser::Serializer::serialize_i8(self.leaf(), v) }
    fn serialize_i16(self, v: i16) -> JsResult<'s> { ser::Serializer::serialize_i16(self.leaf(), v) }
    fn serialize_i32(self, v: i32) -> JsResult<'s> { ser::Serializer::serialize_i32(self.leaf(), v) }
    fn serialize_u8(self, v: u8) -> JsResult<'s> { ser::Serializer::serialize_u8(self.leaf(), v) }
    fn serialize_u16(self, v: u16) -> JsResult<'s> { ser::Serializer::serialize_u16(self.leaf(), v) }
    fn serialize_u32(self, v: u32) -> JsResult<'s> { ser::Serializer::serialize_u32(self.leaf(), v) }
    // WebIDL `long long` / `unsigned long long`: "the Number value closest to" it -- never a BigInt.
    fn serialize_i64(self, v: i64) -> JsResult<'s> { ser::Serializer::serialize_f64(self.leaf(), v as f64) }
    fn serialize_u64(self, v: u64) -> JsResult<'s> { ser::Serializer::serialize_f64(self.leaf(), v as f64) }
    fn serialize_f32(self, v: f32) -> JsResult<'s> { ser::Serializer::serialize_f32(self.leaf(), v) }
    fn serialize_f64(self, v: f64) -> JsResult<'s> { ser::Serializer::serialize_f64(self.leaf(), v) }
    fn serialize_char(self, v: char) -> JsResult<'s> { ser::Serializer::serialize_char(self.leaf(), v) }
    fn serialize_str(self, v: &str) -> JsResult<'s> { ser::Serializer::serialize_str(self.leaf(), v) }
    fn serialize_bytes(self, v: &[u8]) -> JsResult<'s> { ser::Serializer::serialize_bytes(self.leaf(), v) }
    fn serialize_none(self) -> JsResult<'s> { ser::Serializer::serialize_none(self.leaf()) }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> JsResult<'s> { value.serialize(self) }
    fn serialize_unit(self) -> JsResult<'s> { ser::Serializer::serialize_unit(self.leaf()) }
    fn serialize_unit_struct(self, name: &'static str) -> JsResult<'s> { ser::Serializer::serialize_unit_struct(self.leaf(), name) }
    /// A WebIDL enum value: the variant's (renamed) name.
    fn serialize_unit_variant(self, name: &'static str, index: u32, variant: &'static str) -> JsResult<'s> {
        ser::Serializer::serialize_unit_variant(self.leaf(), name, index, variant)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _name: &'static str, value: &T) -> JsResult<'s> { value.serialize(self) }
    /// `{ "Variant": payload }`, as serde_v8 and serde_json shape it -- an ordinary object.
    fn serialize_newtype_variant<T: ?Sized + Serialize>(self, _name: &'static str, _index: u32, variant: &'static str, value: &T) -> JsResult<'s> {
        let scope = self.scope;
        let v = value.serialize(IdlSerializer { scope })?;
        let s = &mut *scope.borrow_mut();
        let k = key(s, variant)?;
        ordinary(s, &[k], &[v])
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, serde_v8::Error> {
        Ok(IdlArray { scope: self.scope, items: Vec::with_capacity(len.unwrap_or(0)) })
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, serde_v8::Error> { self.serialize_seq(Some(len)) }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Self::SerializeTupleStruct, serde_v8::Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<Self::SerializeTupleVariant, serde_v8::Error> {
        let scope = self.scope;
        Ok(IdlVariant { scope, variant, inner: self.serialize_seq(Some(len))? })
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, serde_v8::Error> {
        let n = len.unwrap_or(0);
        Ok(IdlObject { scope: self.scope, keys: Vec::with_capacity(n), values: Vec::with_capacity(n) })
    }
    fn serialize_struct(self, name: &'static str, len: usize) -> Result<Self::SerializeStruct, serde_v8::Error> {
        if name.starts_with(MAGIC_PREFIX) {
            return Ok(IdlStruct::Magic(ser::Serializer::serialize_struct(self.leaf(), name, len)?));
        }
        Ok(IdlStruct::Plain(IdlObject { scope: self.scope, keys: Vec::with_capacity(len), values: Vec::with_capacity(len) }))
    }
    fn serialize_struct_variant(self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<Self::SerializeStructVariant, serde_v8::Error> {
        let scope = self.scope;
        Ok(IdlVariant { scope, variant, inner: self.serialize_struct(variant, len)? })
    }
}

/// A sequence: an `Array` of the current realm, its elements in order.
struct IdlArray<'r, 'b, 's, 'i> {
    scope: ScopeCell<'r, 'b, 's, 'i>,
    items: Vec<JsValue<'s>>,
}

impl<'r, 'b, 's, 'i> ser::SerializeSeq for IdlArray<'r, 'b, 's, 'i> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), serde_v8::Error> {
        let v = value.serialize(IdlSerializer { scope: self.scope })?;
        self.items.push(v);
        Ok(())
    }
    fn end(self) -> JsResult<'s> {
        let s = &mut *self.scope.borrow_mut();
        Ok(v8::Array::new_with_elements(s, &self.items).into())
    }
}

impl<'r, 'b, 's, 'i> ser::SerializeTuple for IdlArray<'r, 'b, 's, 'i> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), serde_v8::Error> { ser::SerializeSeq::serialize_element(self, value) }
    fn end(self) -> JsResult<'s> { ser::SerializeSeq::end(self) }
}

impl<'r, 'b, 's, 'i> ser::SerializeTupleStruct for IdlArray<'r, 'b, 's, 'i> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), serde_v8::Error> { ser::SerializeSeq::serialize_element(self, value) }
    fn end(self) -> JsResult<'s> { ser::SerializeSeq::end(self) }
}

/// A dictionary, a record or a builder's object: an ordinary object of the current realm, its members defined in the
/// order they are serialized -- a struct's field order, which is the dictionary's (Blink's `kOwnPropertyNames`, the
/// inherited dictionary's first) or the `toJSON` builder's.
struct IdlObject<'r, 'b, 's, 'i> {
    scope: ScopeCell<'r, 'b, 's, 'i>,
    keys: Vec<v8::Local<'s, v8::Name>>,
    values: Vec<JsValue<'s>>,
}

impl<'r, 'b, 's, 'i> ser::SerializeMap for IdlObject<'r, 'b, 's, 'i> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), serde_v8::Error> {
        let k = k.serialize(IdlSerializer { scope: self.scope })?;
        // a record's keys are strings (WebIDL `record<DOMString, T>`), as serde_v8 requires them
        let k = v8::Local::<v8::Name>::try_from(k).map_err(|_| <serde_v8::Error as ser::Error>::custom("Serialized Maps expect String keys"))?;
        self.keys.push(k);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), serde_v8::Error> {
        let v = value.serialize(IdlSerializer { scope: self.scope })?;
        self.values.push(v);
        Ok(())
    }
    fn end(self) -> JsResult<'s> {
        let s = &mut *self.scope.borrow_mut();
        ordinary(s, &self.keys, &self.values)
    }
}

impl<'r, 'b, 's, 'i> ser::SerializeStruct for IdlObject<'r, 'b, 's, 'i> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, name: &'static str, value: &T) -> Result<(), serde_v8::Error> {
        let v = value.serialize(IdlSerializer { scope: self.scope })?;
        let k = {
            let s = &*self.scope.borrow();
            key(s, name)?
        };
        self.keys.push(k);
        self.values.push(v);
        Ok(())
    }
    fn end(self) -> JsResult<'s> { ser::SerializeMap::end(self) }
}

/// A struct: an ordinary object, or one of serde_v8's magic types, which keeps serde_v8's conversion.
enum IdlStruct<'r, 'b, 's, 'i> {
    Plain(IdlObject<'r, 'b, 's, 'i>),
    Magic(<serde_v8::Serializer<'s, 'b, 'r, 'i> as ser::Serializer>::SerializeStruct),
}

impl<'r, 'b, 's, 'i> ser::SerializeStruct for IdlStruct<'r, 'b, 's, 'i> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, name: &'static str, value: &T) -> Result<(), serde_v8::Error> {
        match self {
            IdlStruct::Plain(o) => ser::SerializeStruct::serialize_field(o, name, value),
            IdlStruct::Magic(m) => ser::SerializeStruct::serialize_field(m, name, value),
        }
    }
    fn end(self) -> JsResult<'s> {
        match self {
            IdlStruct::Plain(o) => ser::SerializeStruct::end(o),
            IdlStruct::Magic(m) => ser::SerializeStruct::end(m),
        }
    }
}

/// An enum variant with data: `{ "Variant": payload }`, an ordinary object.
struct IdlVariant<'r, 'b, 's, 'i, S> {
    scope: ScopeCell<'r, 'b, 's, 'i>,
    variant: &'static str,
    inner: S,
}

impl<'r, 'b, 's, 'i, S> IdlVariant<'r, 'b, 's, 'i, S> {
    fn wrap(scope: ScopeCell<'r, 'b, 's, 'i>, variant: &'static str, payload: JsValue<'s>) -> JsResult<'s> {
        let s = &mut *scope.borrow_mut();
        let k = key(s, variant)?;
        ordinary(s, &[k], &[payload])
    }
}

impl<'r, 'b, 's, 'i> ser::SerializeTupleVariant for IdlVariant<'r, 'b, 's, 'i, IdlArray<'r, 'b, 's, 'i>> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), serde_v8::Error> {
        ser::SerializeSeq::serialize_element(&mut self.inner, value)
    }
    fn end(self) -> JsResult<'s> {
        let (scope, variant) = (self.scope, self.variant);
        let payload = ser::SerializeSeq::end(self.inner)?;
        Self::wrap(scope, variant, payload)
    }
}

impl<'r, 'b, 's, 'i> ser::SerializeStructVariant for IdlVariant<'r, 'b, 's, 'i, IdlStruct<'r, 'b, 's, 'i>> {
    type Ok = JsValue<'s>;
    type Error = serde_v8::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, name: &'static str, value: &T) -> Result<(), serde_v8::Error> {
        ser::SerializeStruct::serialize_field(&mut self.inner, name, value)
    }
    fn end(self) -> JsResult<'s> {
        let (scope, variant) = (self.scope, self.variant);
        let payload = ser::SerializeStruct::end(self.inner)?;
        Self::wrap(scope, variant, payload)
    }
}
