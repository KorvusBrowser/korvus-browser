//! The cascade: stylesheet text in, a computed style per node out.
//!
//! The stages are separate files because they are separate concerns with separate failure modes,
//! not because they are separately usable — nothing outside this crate calls into the middle of a
//! cascade. See `docs/KORVUS-PAGE-DESIGN.html` §02 for the shape of the pipeline.

pub mod at_rules;
/// The CSS property names Blink exposes to CSSOM (generated from the Chromium reference).
pub mod blink_names;
/// CSS math functions as Blink parses, simplifies and serializes them.
pub mod calc;
pub mod compute;
pub mod container;
pub mod computed;
pub mod cssom;
pub mod declarative;
pub mod declared;
pub mod font_family;
pub mod generated;
pub mod hints;
/// An element's inline declaration block, as Blink holds and serializes it.
pub mod inline;
pub mod logical;
pub mod matching;
pub mod math;
pub mod media;
/// The bounds of the style-write memos.
pub mod memo;
pub mod parse;
pub mod property;
pub mod registered;
pub mod resolve;
pub mod rules;
pub mod serialize;
pub mod shorthand;
pub mod supports;
pub mod ua;
pub mod urls;
pub mod variables;
