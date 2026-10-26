pub mod cookie_store;
pub mod fetch;
pub mod crypto;
pub mod indexeddb;
pub mod broadcastchannel;
pub mod url;
pub mod cache_storage;
pub mod storage;
pub mod notifications;
/// `WebAssembly.instantiateStreaming` / `compileStreaming`.
pub mod wasm_streaming;
/// `structuredClone` -- the prototype-preserving deep copy, in Rust so it can stamp a
/// cloned platform object's BRAND (page JS cannot, which is the point of a brand).
/// The state codec of `[Serializable]` platform objects and transferred ports.
pub mod clone_state;
pub mod structured_clone;
/// `btoa` / `atob`, the WebIDL wrapper over the runtime's encode/decode ops.
pub mod base64;
/// The `console` NAMESPACE -- Chrome's twenty-five members, native, over `op_kprint`.
pub mod console_ns;
/// `window.chrome` -- `loadTimes`, `csi` and the `app` object, native, off the kernel.
pub mod chrome_object;
/// `WebSocket` -- Blink's DOMWebSocket over the kernel's WebSocket service (`op_ws_*`, transport::websocket).
pub mod websocket;
