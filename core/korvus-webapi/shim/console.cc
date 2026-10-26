// The V8 calls V8's inspector makes when it builds a console message's text
// (inspector/v8-console-message.cc, V8ValueStringBuilder) that rusty_v8 150 does not bind:
// Object::ObjectProtoToString and the primitive wrappers' ValueOf. As codegen.cc's, they are NON-INLINE V8
// API functions, so the only thing this file shares with the prebuilt V8 is their symbols and the
// pointer-sized `Local` handle -- no V8 internal layout is read here (no inline root access: a boolean or a
// number goes back to Rust as a C value, and Rust makes the handle).
#include "v8-context.h"
#include "v8-local-handle.h"
#include "v8-object.h"
#include "v8-primitive-object.h"
#include "v8-primitive.h"

template <class T>
static inline v8::Local<T> to_local(const T* ptr) {
  static_assert(sizeof(v8::Local<T>) == sizeof(T*), "Local is one pointer");
  return *reinterpret_cast<const v8::Local<T>*>(&ptr);
}

template <class T>
static inline const T* to_ptr(v8::Local<T> local) {
  return *reinterpret_cast<const T* const*>(&local);
}

// `Object.prototype.toString` of `object` -- "[object Tag]", its Symbol.toStringTag READ (a getter there
// runs) -- or null when that threw.
extern "C" const v8::String* korvus_object_proto_to_string(const v8::Context* context, const v8::Object* object) {
  v8::Local<v8::String> out;
  if (!to_local(object)->ObjectProtoToString(to_local(context)).ToLocal(&out)) return nullptr;
  return to_ptr(out);
}

// A primitive wrapper's value, read from its slot (no page code runs). The caller has checked the kind.
extern "C" bool korvus_boolean_object_value(const v8::Value* value) {
  return to_local(value).As<v8::BooleanObject>()->ValueOf();
}

extern "C" double korvus_number_object_value(const v8::Value* value) {
  return to_local(value).As<v8::NumberObject>()->ValueOf();
}

extern "C" const v8::String* korvus_string_object_value(const v8::Value* value) {
  return to_ptr(to_local(value).As<v8::StringObject>()->ValueOf());
}

extern "C" const v8::Symbol* korvus_symbol_object_value(const v8::Value* value) {
  return to_ptr(to_local(value).As<v8::SymbolObject>()->ValueOf());
}

extern "C" const v8::BigInt* korvus_bigint_object_value(const v8::Value* value) {
  return to_ptr(to_local(value).As<v8::BigIntObject>()->ValueOf());
}
