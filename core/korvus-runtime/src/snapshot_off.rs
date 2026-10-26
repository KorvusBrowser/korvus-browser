//! [`crate::snapshot`] in an analysis build, which makes no snapshot of the page realm: its function records are
//! heap `External`s the snapshot cannot carry, and `webapi::snapshot` is compiled out. Every engine builds the realm
//! the ordinary way.
use std::path::PathBuf;

use deno_core::v8;

pub const MAKE_ARG: &str = "__korvus-make-snapshot";

pub fn use_snapshots(_exe: PathBuf) {}

/// No maker runs, so there is no one to hand the identity's arguments to (`snapshot::child_args`).
pub fn child_args(_args: Vec<String>) {}

pub fn make_main(_persona: std::sync::Arc<dyn spec::Persona>) -> i32 {
    2
}

#[derive(Clone, Copy)]
pub(crate) struct Loaded {
    pub(crate) blob: &'static [u8],
}

impl Loaded {
    pub(crate) fn refs_ext(&self) -> deno_core::Extension {
        deno_core::Extension::default()
    }
}

pub(crate) fn load(_persona: &dyn spec::Persona) -> Option<Loaded> {
    None
}

pub(crate) fn installed_fresh() {}

pub(crate) fn failed(_why: &str) {}

pub(crate) fn restore(_scope: &mut v8::PinScope) -> Result<(), String> {
    Err("an analysis build makes no snapshot".to_string())
}
