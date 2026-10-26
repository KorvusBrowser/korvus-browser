//! korvus-webapi test kit (`webapi_gen`): the surface grader (`quality`), the behaviour oracle
//! (`oracle`), the live-surface snapshot reader (`snapshot`), the WebIDL corpus merge (`merge`/`webidl`),
//! WPT accounting (`wpt`), case skeletons (`gencases`) and the per-family status dashboard (`status`).
//!
//! The WebIDL -> Rust scaffold emitter that used to live beside these (emit/scaffold/layout, the
//! `webapi scaffold` subcommand, the build.rs OUT_DIR loop) was deleted: the one-table
//! `interface!` model is how an interface's shape reaches Rust now, and the emitter's output was never
//! included anywhere. It is in git history.

pub mod blink;
pub mod coverage;
pub mod gencases;
pub mod html_tags;
pub mod deps;
pub mod merge;
pub mod oracle;
/// Own-global ORDER fidelity — the one implementation both realms' gates measure with.
pub mod order;
pub mod profile;
pub mod quality;
pub mod snapshot;
pub mod spec;
pub mod status;
pub mod webidl;
pub mod wpt;
