//! Layout: boxes, and where they end up.
//!
//! Style answers what an element looks like; this answers *where it is*, which is the half a page
//! measures. See `docs/KORVUS-PAGE-DESIGN.html` §04.

pub mod caret;
pub mod geometry;
pub mod hit;
pub mod ifc;
pub mod inline;
pub mod replaced;
pub mod resolved;
pub mod style;
pub mod table;
pub mod tree;
