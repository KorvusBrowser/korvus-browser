//! The WebSocket KERNEL SERVICE: the ops `core/korvus-webapi/src/modules/websocket/web_socket.rs`
//! (the `WebSocket` interface) calls, in both realms.
//!
//! The split is Chrome's own: Blink's `DOMWebSocket` + `WebSocketChannelImpl` own the page-visible
//! state machine (readyState, bufferedAmount, the events and their order), and the network service owns
//! the connection (`network::WebSocket` -> `net::WebSocketChannel`). Here the first half is the webapi
//! table and the second is `transport::websocket`; these ops are the mojo pipe between them.
//!
//! * [`op_ws_open`] -- `WebSocketConnector::Connect`: the document's origin and the realm's persona
//!   (headers AND wire, from one persona) go with the URL. Answers a socket id at once.
//! * [`op_ws_next`] -- the next `WebSocketClient` call, encoded (see [`encode`]). An EMPTY answer means
//!   the socket is gone. The table's receive pump awaits it, one event per turn, which is the task
//!   boundary Blink's `TaskType::kWebSocket` / `kNetworking` posts give each event.
//! * [`op_ws_send_text`] / [`op_ws_send_binary`] -- `WebSocket::SendMessage`.
//! * [`op_ws_close`] -- `StartClosingHandshake`.
//! * [`op_ws_abort`] -- `Disconnect`: drop the connection without a closing handshake.
//!
//! The pump's promise is NOT unref'd: while a socket is live the realm has work that can still arrive,
//! which is exactly what a pending op tells `Engine::pump` (a `Drained` answer would be a lie -- the
//! `has_live_worker` comment in `event_loop.rs` is the same reasoning for workers).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::{op2, OpState};

/// This realm's live sockets, by id.
#[derive(Default)]
pub(crate) struct WsSockets {
    next: u32,
    map: HashMap<u32, WsEntry>,
}

struct WsEntry {
    commands: tokio::sync::mpsc::UnboundedSender<transport::websocket::WsCommand>,
    events: Rc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<transport::websocket::WsEvent>>>,
}

fn sockets(state: &mut OpState) -> &mut WsSockets {
    if state.try_borrow::<WsSockets>().is_none() {
        state.put(WsSockets::default());
    }
    state.borrow_mut::<WsSockets>()
}

/// The serialised origin of `url` (`null` for an opaque one) -- the `Origin` header of the handshake.
/// A `blob:` URL's origin is its inner URL's, which is how a Blob-URL worker gets its creator's.
fn origin_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => u.origin().ascii_serialization(),
        Err(_) => "null".to_string(),
    }
}

/// `WebSocketConnector::Connect`. `protocols` is Blink's joined form (`", "`); the tokens are already
/// validated, so splitting it back is exact.
#[op2(fast)]
#[smi]
pub(crate) fn op_ws_open(state: &mut OpState, #[string] url: String, #[string] protocols: String) -> u32 {
    let origin = origin_of(&state.borrow::<crate::HostLocation>().get());
    let protocols: Vec<String> = if protocols.is_empty() { Vec::new() } else { protocols.split(", ").map(str::to_string).collect() };
    let mut req = transport::websocket::WsRequest { url, protocols, origin, headers: Vec::new(), wire: None };
    if let Some(hp) = state.try_borrow::<crate::HostPersona>() {
        req = req.worn_by(&transport::persona_identity(&*hp.0));
    }
    let handle = transport::websocket::open(req);
    let s = sockets(state);
    s.next = s.next.wrapping_add(1).max(1);
    let id = s.next;
    s.map.insert(id, WsEntry { commands: handle.commands, events: Rc::new(tokio::sync::Mutex::new(handle.events)) });
    id
}

/// One event, as bytes the table decodes without a serde round trip:
///
/// ```text
/// 1 OPEN     [u32 LE protocol length][protocol][extensions]
/// 2 TEXT     [utf-8]
/// 3 BINARY   [bytes]
/// 4 DRAINED  [u64 LE byte count]
/// 5 CLOSING
/// 6 CLOSED   [was_clean u8][code u16 LE][reason utf-8]
/// 7 FAILED   [message utf-8]
/// ```
fn encode(e: transport::websocket::WsEvent) -> Vec<u8> {
    use transport::websocket::WsEvent::*;
    match e {
        Open { protocol, extensions } => {
            let mut v = vec![1u8];
            v.extend_from_slice(&(protocol.len() as u32).to_le_bytes());
            v.extend_from_slice(protocol.as_bytes());
            v.extend_from_slice(extensions.as_bytes());
            v
        }
        Text(s) => {
            let mut v = Vec::with_capacity(s.len() + 1);
            v.push(2);
            v.extend_from_slice(s.as_bytes());
            v
        }
        Binary(b) => {
            let mut v = Vec::with_capacity(b.len() + 1);
            v.push(3);
            v.extend_from_slice(&b);
            v
        }
        Drained(n) => {
            let mut v = vec![4u8];
            v.extend_from_slice(&n.to_le_bytes());
            v
        }
        ClosingHandshake => vec![5u8],
        Closed { was_clean, code, reason } => {
            let mut v = vec![6u8, was_clean as u8];
            v.extend_from_slice(&code.to_le_bytes());
            v.extend_from_slice(reason.as_bytes());
            v
        }
        Failed(m) => {
            let mut v = vec![7u8];
            v.extend_from_slice(m.as_bytes());
            v
        }
    }
}

/// The next event of socket `id`; empty when the socket is gone. A terminal event (CLOSED, FAILED)
/// also forgets the socket, so the pump's next call answers empty and stops.
#[op2(async(lazy), fast)]
#[buffer]
pub(crate) async fn op_ws_next(state: Rc<RefCell<OpState>>, #[smi] id: u32) -> Vec<u8> {
    let rx = {
        let mut s = state.borrow_mut();
        match sockets(&mut s).map.get(&id) {
            Some(e) => e.events.clone(),
            None => return Vec::new(),
        }
    };
    let ev = {
        let mut guard = rx.lock().await;
        guard.recv().await
    };
    match ev {
        Some(e) => {
            let terminal = matches!(e, transport::websocket::WsEvent::Closed { .. } | transport::websocket::WsEvent::Failed(_));
            if terminal {
                sockets(&mut state.borrow_mut()).map.remove(&id);
            }
            encode(e)
        }
        None => {
            sockets(&mut state.borrow_mut()).map.remove(&id);
            Vec::new()
        }
    }
}

fn command(state: &mut OpState, id: u32, c: transport::websocket::WsCommand) {
    if let Some(e) = sockets(state).map.get(&id) {
        let _ = e.commands.send(c);
    }
}

#[op2(fast)]
pub(crate) fn op_ws_send_text(state: &mut OpState, #[smi] id: u32, #[string] data: String) {
    command(state, id, transport::websocket::WsCommand::Send { text: true, data: data.into_bytes() });
}

#[op2(fast)]
pub(crate) fn op_ws_send_binary(state: &mut OpState, #[smi] id: u32, #[buffer] data: &[u8]) {
    command(state, id, transport::websocket::WsCommand::Send { text: false, data: data.to_vec() });
}

/// `code` 1005 asks for a Close frame with no payload (`close()` with no code).
#[op2(fast)]
pub(crate) fn op_ws_close(state: &mut OpState, #[smi] id: u32, #[smi] code: u32, #[string] reason: String) {
    command(state, id, transport::websocket::WsCommand::Close { code: code as u16, reason });
}

/// Drop the connection, no closing handshake: the sender goes, and the transport's channel sees its
/// command queue close.
#[op2(fast)]
pub(crate) fn op_ws_abort(state: &mut OpState, #[smi] id: u32) {
    sockets(state).map.remove(&id);
}
