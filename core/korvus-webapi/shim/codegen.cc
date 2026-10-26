// The two V8 entry points rusty_v8 150 does not bind, which `eval(TrustedScript)` needs.
//
// Chrome marks TrustedScript objects CODE-LIKE (ObjectTemplate::SetCodeLike) and gives V8 a
// ModifyCodeGenerationFromStrings callback that turns such an object into its script; `eval` of any
// other object returns the object. Both calls below are NON-INLINE V8 API functions, so the only thing
// this file shares with the prebuilt V8 is their symbols and the pointer-sized `Local` handle -- no V8
// internal layout (pointer compression, sandbox) is read here. The human chose this shim over a V8
// source build (2026-09-26).
#include "v8-callbacks.h"
#include "v8-context.h"
#include "v8-isolate.h"
#include "v8-local-handle.h"
#include "v8-primitive.h"
#include "v8-template.h"

// The Rust side, handed over at registration: a function POINTER, not an exported symbol, because a
// test binary can link two copies of the crate and two exports of one name do not link.
// `*allowed` starts true; the Rust side clears it to refuse (CSP `unsafe-eval`, a Trusted Types failure).
typedef const v8::String* (*korvus_modify_fn)(const v8::Context* context, const v8::Value* source, bool is_code_like, bool* allowed);
static korvus_modify_fn korvus_codegen_modify = nullptr;

template <class T>
static inline v8::Local<T> to_local(const T* ptr) {
  static_assert(sizeof(v8::Local<T>) == sizeof(T*), "Local is one pointer");
  return *reinterpret_cast<const v8::Local<T>*>(&ptr);
}

template <class T>
static inline const T* to_ptr(v8::Local<T> local) {
  return *reinterpret_cast<const T* const*>(&local);
}

static v8::ModifyCodeGenerationFromStringsResult korvus_modify(v8::Local<v8::Context> context, v8::Local<v8::Value> source, bool is_code_like) {
  v8::ModifyCodeGenerationFromStringsResult result;
  // The realm's CSP and Trusted Types decide (core/trusted_types/tables.rs): every korvus context has
  // code generation from strings OFF, so every string `eval` / `Function` comes here, as in Blink.
  bool allowed = true;
  const v8::String* text = korvus_codegen_modify ? korvus_codegen_modify(to_ptr(context), to_ptr(source), is_code_like, &allowed) : nullptr;
  result.codegen_allowed = allowed;
  if (allowed && text != nullptr) {
    result.modified_source = to_local(text);
  }
  return result;
}

extern "C" void korvus_set_codegen_callback(v8::Isolate* isolate, korvus_modify_fn modify) {
  korvus_codegen_modify = modify;
  isolate->SetModifyCodeGenerationFromStringsCallback(korvus_modify);
}

extern "C" void korvus_template_set_code_like(const v8::ObjectTemplate* templ) {
  to_local(templ)->SetCodeLike();
}
