// A getter template with a CACHED PROPERTY NAME (FunctionTemplate::NewWithCache, which rusty_v8 152 does not bind):
// Blink's `[CachedAccessor]`. V8 cannot keep an inline-cache handler for an API getter whose holder has dictionary
// properties -- the global object always has -- so every unqualified `document` ran the getter through the runtime
// (LoadGlobalIC_Slow, ~300 ns where Chrome's read is ~5 ns). With a cached name, an IC that finds the getter on the
// receiver and the private as a data property of the holder loads that private instead (LookupIterator::
// LookupCachedProperty). As codegen.cc's, a NON-INLINE V8 API function: only its symbol and the pointer-sized `Local`
// handle are shared with the prebuilt V8.
#include "v8-function-callback.h"
#include "v8-isolate.h"
#include "v8-local-handle.h"
#include "v8-primitive.h"
#include "v8-template.h"

template <class T>
static inline v8::Local<T> to_local(const T* ptr) {
  static_assert(sizeof(v8::Local<T>) == sizeof(T*), "Local is one pointer");
  return *reinterpret_cast<const v8::Local<T>*>(&ptr);
}

template <class T>
static inline const T* to_ptr(v8::Local<T> local) {
  return *reinterpret_cast<const T* const*>(&local);
}

// A template for a non-constructor function (ConstructorBehavior::kThrow, as the builder's) whose IC loads may read
// `cache` off the holder in place of calling `callback`. `data` may be null (no data).
extern "C" const v8::FunctionTemplate* korvus_function_template_with_cache(v8::Isolate* isolate, v8::FunctionCallback callback,
                                                                           const v8::Private* cache, const v8::Value* data,
                                                                           int length) {
  v8::Local<v8::FunctionTemplate> t = v8::FunctionTemplate::NewWithCache(
      isolate, callback, to_local(cache), data ? to_local(data) : v8::Local<v8::Value>(), v8::Local<v8::Signature>(), length);
  t->RemovePrototype();
  return to_ptr(t);
}
