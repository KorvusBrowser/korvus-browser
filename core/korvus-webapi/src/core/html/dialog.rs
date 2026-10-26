//! `alert`, `confirm` and `prompt` asked of a person (D88 step 2).
//!
//! Chrome shows a page's dialog in its browser process and the page's thread waits for the answer; a DevTools client
//! that enabled `Page` hears `Page.javascriptDialogOpening` and answers it with `Page.handleJavaScriptDialog`, and the
//! command that raised it is answered only then. A page process under `korvus serve` asks its router the same way
//! (`cdp::router`'s `DIALOG_*` lines): the router knows which sessions enabled `Page`, and says at once that no one
//! will answer when none did, so a page with no one watching goes on as it did before -- dismissed. Without a bridge
//! (`korvus fetch`, the single-page server) nothing is asked.

use std::sync::OnceLock;

/// The page process's way to its endpoint.
pub trait DialogBridge: Send + Sync {
    /// Ask for a dialog of `kind` (`alert`, `confirm`, `prompt`) and wait: `Some((accepted, text))` once a client
    /// answered, `None` when no one will.
    fn ask(&self, kind: &str, message: &str, default_prompt: &str) -> Option<(bool, String)>;
}

static BRIDGE: OnceLock<std::sync::Arc<dyn DialogBridge>> = OnceLock::new();

/// Set this process's bridge, once, before its page runs.
pub fn set_bridge(b: std::sync::Arc<dyn DialogBridge>) {
    let _ = BRIDGE.set(b);
}

/// Ask a person, when there is a way to: `None` when there is not, or no one answers.
pub(crate) fn ask(kind: &str, message: &str, default_prompt: &str) -> Option<(bool, String)> {
    BRIDGE.get()?.ask(kind, message, default_prompt)
}
