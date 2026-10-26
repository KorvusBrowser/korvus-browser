//! The caret position at a point: `document.caretPositionFromPoint`.
//!
//! A hit test answers WHICH ELEMENT is at a point. A caret position answers where, between which two
//! characters, an insertion point there would go -- a node and an offset into it. Every answer below
//! is Chrome 150.0.7871.46's, measured over loopback on a document scrolled to `(50, 900)` with 16px
//! Arial (Liberation Sans on the recording machine, metric-compatible):
//!
//! ```text
//! point on a box with no text               (#a, 0)         getClientRect 0,0,0,0
//! "hello world" at x 10: x 12               (text, 0)       getClientRect 10,420,0,17
//!                        x 19.5              (text, 1)                     18.89,420,0,17
//!                        x 36                (text, 4)                     34.91,420,0,17
//!                        x 250 (past the end) (text, 11)                   86.48,420,0,17
//! a point outside the viewport              null
//! ```
//!
//! The caret goes to the NEAREST boundary: 19.5 is past the middle of `h` (10 + 8.90 / 2), so 1. Its
//! rectangle is zero wide and as tall as the text's content area, at the boundary -- which Chrome
//! places at the FLOOR of the advance in layout units (18.890625 for 8.8984375), and at the end of a
//! line at the text's own snapped end.

use crate::layout::geometry::{Rect, ScrollSnapshot};
use crate::layout::tree::BoxTree;
use korvus_dom::{Dom, NodeId, NodeKind};

/// A caret position: the node, the offset into it (UTF-16 units for a text node), and the caret's
/// rectangle in client coordinates -- all zeros when the position is not in text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caret {
    pub node: NodeId,
    pub offset: u32,
    pub rect: Rect,
}

impl BoxTree {
    /// The caret position at a client point, or `None` outside the viewport.
    pub fn caret_position(&self, dom: &Dom, x: f32, y: f32, scroll: &ScrollSnapshot) -> Option<Caret> {
        let hit = self.elements_from_point_for_document(x, y, scroll).into_iter().next()?;
        let at_element = Caret { node: hit, offset: 0, rect: Rect::ZERO };
        // The hit element's own text: the line box the point is level with, nearest in x.
        let mut best: Option<(f32, u32, usize, Rect)> = None;
        for index in 0..self.len() as u32 {
            if self.node_of(index) != hit || !self.text_nodes.contains_key(&index) {
                continue;
            }
            for (line, rect) in self.viewport_client_rects(index, scroll).into_iter().enumerate() {
                if !(y >= rect.y && y < rect.bottom()) {
                    continue;
                }
                let distance = if x < rect.x {
                    rect.x - x
                } else if x > rect.right() {
                    x - rect.right()
                } else {
                    0.0
                };
                if best.is_none_or(|(d, ..)| distance < d) {
                    best = Some((distance, index, line, rect));
                }
            }
        }
        let Some((_, index, line, rect)) = best else { return Some(at_element) };
        let Some(&text_node) = self.text_nodes.get(&index) else { return Some(at_element) };
        let Some(lines) = self.maps.text_lines.get(&index) else { return Some(at_element) };
        let Some(&first) = lines.get(line) else { return Some(at_element) };
        let last = lines.get(line + 1).copied();
        let atoms = self.text_atom_strings(index);
        let last = last.unwrap_or(atoms.len());
        // Collapsed characters before this line's first atom.
        let mut k: usize = atoms[..first.min(atoms.len())].iter().map(|a| a.chars().count()).sum();
        let mut advance = 0.0f32;
        let mut end = k;
        let mut found = None;
        'line: for atom in &atoms[first.min(atoms.len())..last.min(atoms.len())] {
            if atom == "\n" {
                k += 1;
                continue;
            }
            for ch in atom.chars() {
                let w = self.text_advance(index, ch.encode_utf8(&mut [0u8; 4]));
                if x < rect.x + advance + w / 2.0 {
                    found = Some((k, advance));
                    break 'line;
                }
                advance += w;
                k += 1;
            }
            // A space that ends the line hangs past it; the line's end is before it.
            if atom != " " {
                end = k;
            }
        }
        let (k, cx) = match found {
            Some((k, advance)) => {
                // Float noise in a sum of advances must not floor a whole layout unit away.
                let units = advance * 64.0;
                let units = if (units - units.round()).abs() < 1e-3 { units.round() } else { units.floor() };
                (k, rect.x + units / 64.0)
            }
            None => (end, rect.right()),
        };
        let NodeKind::Text(text) = &dom.node(text_node).kind else { return Some(at_element) };
        let offsets = crate::layout::inline::collapsed_offsets(text, self.text_preserved(index));
        let offset = offsets.get(k).copied().or_else(|| offsets.last().copied()).unwrap_or(0);
        Some(Caret { node: text_node, offset, rect: Rect { x: cx, y: rect.y, width: 0.0, height: rect.height } })
    }

    /// The rects of text node `node`'s characters from UTF-16 offset `start` to `end`, one per line they lie on, in
    /// client coordinates: what a `Range` covering that part of the text reports for it (Blink's
    /// `LayoutText::AbsoluteQuadsForRange`). Each is as tall as the line's text and runs from the first selected
    /// character's pen position to the last one's end, a line's whole text being exactly its fragment. Chrome 150:
    /// `selectNodeContents` of a `<div>` holding only text answers the text's width, not the block's.
    pub fn text_range_rects(&self, dom: &Dom, node: NodeId, start: u32, end: u32, scroll: &ScrollSnapshot) -> Vec<Rect> {
        let NodeKind::Text(text) = &dom.node(node).kind else { return Vec::new() };
        let mut boxes: Vec<u32> = self.text_nodes.iter().filter(|&(_, &n)| n == node).map(|(&i, _)| i).collect();
        boxes.sort_unstable();
        // a position in layout units, floored as Blink floors a caret's (float noise not counting)
        let snap = |v: f32| {
            let units = v * 64.0;
            (if (units - units.round()).abs() < 1e-3 { units.round() } else { units.floor() }) / 64.0
        };
        let mut out = Vec::new();
        for index in boxes {
            let Some(lines) = self.maps.text_lines.get(&index) else { continue };
            let offsets = crate::layout::inline::collapsed_offsets(text, self.text_preserved(index));
            let atoms = self.text_atom_strings(index);
            for (line, rect) in self.viewport_client_rects(index, scroll).into_iter().enumerate() {
                let Some(&first) = lines.get(line) else { break };
                let first = first.min(atoms.len());
                let last = lines.get(line + 1).copied().unwrap_or(atoms.len()).clamp(first, atoms.len());
                // collapsed characters before this line
                let mut k: usize = atoms[..first].iter().map(|a| a.chars().count()).sum();
                let mut line_text = String::new();
                let (mut from, mut to): (Option<usize>, usize) = (None, 0);
                for atom in &atoms[first..last] {
                    if atom == "\n" {
                        k += 1;
                        continue;
                    }
                    for ch in atom.chars() {
                        let at = offsets.get(k).copied().unwrap_or(u32::MAX);
                        let byte = line_text.len();
                        line_text.push(ch);
                        if at >= start && at < end {
                            from.get_or_insert(byte);
                            to = line_text.len();
                        }
                        k += 1;
                    }
                }
                let Some(from) = from else { continue };
                let x0 = if from == 0 { 0.0 } else { snap(self.text_advance(index, &line_text[..from])) };
                // a line's text runs to its fragment's end; a space hanging past it is not in the line
                let x1 = if to == line_text.len() { rect.width } else { snap(self.text_advance(index, &line_text[..to])) }.min(rect.width);
                out.push(Rect { x: rect.x + x0, y: rect.y, width: (x1 - x0).max(0.0), height: rect.height });
            }
        }
        out
    }
}
