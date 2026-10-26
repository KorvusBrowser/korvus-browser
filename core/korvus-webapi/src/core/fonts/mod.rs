//! `core/fonts/` — the CSS Font Loading API: `FontFace`, `FontFaceSet` and `document.fonts`.
//!
//! A face's descriptors are CSS values the page assigned, parsed by the CSS grammar in
//! [`css_descriptors`]; a set is the list of faces the page added. A face LOADS: its
//! source list is fetched in order and the bytes are asked [`font_probe`]'s question -- is this a
//! font? -- which is what decides `loaded` against `NetworkError`. Korvus renders text with its
//! persona's fonts, so a loaded face changes no layout; what a page observes of the load is right.

pub mod css_descriptors;
pub mod font_face;
pub mod font_probe;
