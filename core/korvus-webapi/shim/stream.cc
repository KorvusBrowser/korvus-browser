// A classic script compiled OFF the page's thread (ScriptCompiler::StartStreaming), as Blink compiles a fetched
// script while the main thread runs other tasks (ScriptStreamer); rusty_v8 152 binds none of it. As codegen.cc's,
// NON-INLINE V8 API functions -- with one difference: StreamedSource's constructor takes a std::unique_ptr, a type of
// the libc++ the prebuilt V8 was built with (Chromium's, namespace __Cr), not this file's. That libc++ gives its
// unique_ptr the trivial ABI (_LIBCPP_ABI_VERSION 2: _LIBCPP_ABI_ENABLE_UNIQUE_PTR_TRIVIAL_ABI), so the argument is
// passed as the bare pointer and the callee owns it; the constructor is called by its symbol with exactly that.
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <new>

#include "v8-context.h"
#include "v8-isolate.h"
#include "v8-local-handle.h"
#include "v8-primitive.h"
#include "v8-script.h"

template <class T>
static inline v8::Local<T> to_local(const T* ptr) {
  static_assert(sizeof(v8::Local<T>) == sizeof(T*), "Local is one pointer");
  return *reinterpret_cast<const v8::Local<T>*>(&ptr);
}

template <class T>
static inline const T* to_ptr(v8::Local<T> local) {
  return *reinterpret_cast<const T* const*>(&local);
}

// StreamedSource::StreamedSource(std::__Cr::unique_ptr<ExternalSourceStream>, Encoding), the complete-object
// constructor the prebuilt defines: `this`, the stream (its unique_ptr, passed as the pointer it holds), the encoding.
extern void korvus_streamed_source_ctor(void* self, v8::ScriptCompiler::ExternalSourceStream* stream, int encoding) __asm__(
    "_ZN2v814ScriptCompiler14StreamedSourceC1ENSt4__Cr10unique_ptrINS0_20ExternalSourceStreamENS2_14default_deleteIS4_EEEENS1_8EncodingE");

// The whole body, handed to V8 in one chunk (V8 takes ownership of it and frees it with delete[]).
class KorvusSourceStream : public v8::ScriptCompiler::ExternalSourceStream {
 public:
  KorvusSourceStream(const uint8_t* data, size_t len) : len_(len) {
    data_ = new uint8_t[len];
    std::memcpy(data_, data, len);
  }
  ~KorvusSourceStream() override { delete[] data_; }
  size_t GetMoreData(const uint8_t** src) override {
    if (data_ == nullptr) {
      *src = nullptr;
      return 0;
    }
    *src = data_;
    data_ = nullptr;
    return len_;
  }

 private:
  uint8_t* data_;
  size_t len_;
};

struct KorvusStream {
  v8::ScriptCompiler::StreamedSource* source;
  v8::ScriptCompiler::ScriptStreamingTask* task;
};

// Room for the source: its own size as this file's headers lay it out (a pointer and a plain struct, whichever
// library), with a margin.
static constexpr size_t kSourceSize = sizeof(v8::ScriptCompiler::StreamedSource) + 64;

// On the isolate's thread: the source and the task that compiles it. Null when V8 makes no task.
extern "C" KorvusStream* korvus_stream_start(v8::Isolate* isolate, const uint8_t* utf8, size_t len) {
  void* mem = ::operator new(kSourceSize);
  std::memset(mem, 0, kSourceSize);
  korvus_streamed_source_ctor(mem, new KorvusSourceStream(utf8, len), v8::ScriptCompiler::StreamedSource::UTF8);
  auto* source = static_cast<v8::ScriptCompiler::StreamedSource*>(mem);
  v8::ScriptCompiler::ScriptStreamingTask* task = v8::ScriptCompiler::StartStreaming(isolate, source);
  if (task == nullptr) {
    source->~StreamedSource();
    ::operator delete(mem);
    return nullptr;
  }
  return new KorvusStream{source, task};
}

// On any other thread: parse and compile.
extern "C" void korvus_stream_run(KorvusStream* s) { s->task->Run(); }

// On the isolate's thread, after the run: the script, from the source string and origin a direct compile would take;
// null with the exception pending when it does not compile.
extern "C" const v8::Script* korvus_stream_finish(const v8::Context* context, KorvusStream* s, const v8::String* full,
                                                  const v8::ScriptOrigin* origin) {
  v8::MaybeLocal<v8::Script> r = v8::ScriptCompiler::Compile(to_local(context), s->source, to_local(full), *origin);
  v8::Local<v8::Script> script;
  return r.ToLocal(&script) ? to_ptr(script) : nullptr;
}

// After the run (finished or not): everything the start made.
extern "C" void korvus_stream_free(KorvusStream* s) {
  delete s->task;
  s->source->~StreamedSource();
  ::operator delete(static_cast<void*>(s->source));
  delete s;
}
