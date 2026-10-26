// The INCUMBENT realm -- the realm of the JavaScript function that made the call now running in a
// native callback (HTML "incumbent settings object"), which rusty_v8 150 does not bind: Isolate::GetIncumbentContext.
// As codegen.cc's and console.cc's, a NON-INLINE V8 API function, so this file shares only its symbol and the
// pointer-sized `Local` handle with the prebuilt V8.
#include "v8-context.h"
#include "v8-isolate.h"
#include "v8-local-handle.h"

// The incumbent context, or null when there is none (no JavaScript on the stack).
extern "C" const v8::Context* korvus_incumbent_context(v8::Isolate* isolate) {
  v8::Local<v8::Context> ctx = isolate->GetIncumbentContext();
  if (ctx.IsEmpty()) return nullptr;
  return *reinterpret_cast<const v8::Context* const*>(&ctx);
}
