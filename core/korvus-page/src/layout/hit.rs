//! Hit testing: which element is at a point.
//!
//! `document.elementFromPoint(x, y)` is a member a page calls directly, and one an automation
//! detector calls to check that the thing it just clicked is the thing that was there. With no
//! layout it answered `null` for every point on the page, which is not a plausible answer for any
//! coordinate inside the viewport.
//!
//! # No pixels are involved
//!
//! Hit testing is a question about *boxes*, not about what was painted: a transparent `<div>` on
//! top of a button still receives the click. So this lives in the layout half of the pipeline and
//! needs no rasterizer — which is what lets a headless build answer it at all.
//!
//! # Paint order, approximately
//!
//! The topmost box at a point is the last one that would have been painted there. v1 approximates
//! that with document order: a child paints over its parent, and a later sibling over an earlier
//! one, which is exactly the order boxes sit in the tree. What it does *not* model is `z-index` and
//! stacking contexts, so a `z-index: -1` element under its parent's background will be found where
//! a browser would find the parent. That is a wrong answer in a case a page has to construct
//! deliberately, rather than a missing answer in every case.

use crate::layout::geometry::{Rect, ScrollSnapshot};
use crate::layout::tree::BoxTree;
use korvus_dom::NodeId;

impl BoxTree {
    /// The topmost element at a CLIENT point, or `None` if the point is outside every box.
    ///
    /// `scroll` is what has been scrolled: the point is in client coordinates, and so is every box it
    /// is compared against (see `layout::geometry`).
    pub fn element_from_point(&self, x: f32, y: f32, scroll: &ScrollSnapshot) -> Option<NodeId> {
        self.elements_from_point(x, y, scroll).into_iter().next()
    }

    /// Every element at a point, topmost first — `document.elementsFromPoint`.
    ///
    /// Topmost first because that is the order the DOM method is specified in, and because the
    /// first entry is the answer to the much more common single-element question.
    ///
    /// A point outside the viewport hits nothing, and a box is only hit inside every box that clips
    /// it. Both measured on Chrome 150.0.7871.46: `elementFromPoint(35, 1110)` in a
    /// window a thousand-odd pixels tall answers `null` over a 5000px-tall element, and a point on a
    /// child scrolled out of its `overflow: auto` parent's scrollport answers the element behind the
    /// parent, not the child.
    pub fn elements_from_point(&self, x: f32, y: f32, scroll: &ScrollSnapshot) -> Vec<NodeId> {
        let mut hits: Vec<NodeId> = Vec::new();
        let viewport = self.viewport();
        if !(x >= 0.0 && y >= 0.0 && x < viewport.width && y < viewport.height) {
            return hits;
        }
        if self.effects().is_some_and(|f| f.any_transform) {
            return self.elements_from_point_transformed(x, y, scroll);
        }
        let geometry = self.hit_geometry(scroll);
        // Reverse document order: the last box painted at a point is the topmost, and boxes sit in
        // the tree in paint order (a child after its parent, a later sibling after an earlier one).
        for index in (0..self.len() as u32).rev() {
            if !self.is_hit_testable(index) {
                continue;
            }
            let (rects, clip) = &geometry[index as usize];
            if clip.is_some_and(|c| !c.contains(x, y)) || !self.hits(index, rects, x, y) {
                continue;
            }
            let node = self.node_of(index);
            // A text box carries its parent element's id, so a point on the text is a hit on the
            // element -- and must not be reported twice when the element's own box also contains it.
            if !hits.contains(&node) {
                hits.push(node);
            }
        }
        hits
    }

    /// [`BoxTree::elements_from_point`] on a page where something transforms (D77): a box is hit where it SHOWS --
    /// the point is taken back through the box's own transform and those above it -- and each clip a box sits inside
    /// is tested where ITS box shows, so a carousel's track moves under a frame that stays put. A box a transform
    /// flattens to nothing (`scale(0)`) is hit nowhere.
    fn elements_from_point_transformed(&self, x: f32, y: f32, scroll: &ScrollSnapshot) -> Vec<NodeId> {
        let n = self.len();
        let translations = self.box_translations(scroll);
        let maps = self.screen_maps(&translations);
        // `None`: the identity; `Some(None)`: singular, hit nowhere.
        let inverse: Vec<Option<Option<crate::effects::Affine>>> = maps.iter().map(|m| m.map(|m| m.invert())).collect();
        let back = |index: usize| -> Option<(f32, f32)> {
            match inverse[index] {
                None => Some((x, y)),
                Some(Some(inv)) => {
                    let (px, py) = inv.apply(x as f64, y as f64);
                    Some((px as f32, py as f32))
                }
                Some(None) => None,
            }
        };
        // Each box's nearest clipping ancestor, and each clipping box's padding box as scrolled.
        let mut clip_owner: Vec<Option<u32>> = Vec::with_capacity(n);
        for index in 0..n as u32 {
            let owner = self.parent_of(index).and_then(|p| if self.clips_contents(p) { Some(p) } else { clip_owner.get(p as usize).copied().flatten() });
            clip_owner.push(owner);
        }
        let clip_rect = |c: u32| {
            let (tx, ty) = translations[c as usize];
            let pb = self.padding_box(c);
            Rect { x: pb.x + tx, y: pb.y + ty, ..pb }
        };
        let mut hits: Vec<NodeId> = Vec::new();
        for index in (0..n as u32).rev() {
            if !self.is_hit_testable(index) {
                continue;
            }
            let Some((px, py)) = back(index as usize) else { continue };
            let rects = self.viewport_rects_with(index, scroll, &|b| translations[b as usize]);
            if !self.hits(index, &rects, px, py) {
                continue;
            }
            let mut clipped = false;
            let mut c = clip_owner[index as usize];
            while let Some(owner) = c {
                match back(owner as usize) {
                    Some((cx, cy)) if clip_rect(owner).contains(cx, cy) => {}
                    _ => {
                        clipped = true;
                        break;
                    }
                }
                c = clip_owner[owner as usize];
            }
            if clipped {
                continue;
            }
            let node = self.node_of(index);
            if !hits.contains(&node) {
                hits.push(node);
            }
        }
        hits
    }

    /// `document.elementsFromPoint`: [`BoxTree::elements_from_point`], and then the document element.
    ///
    /// CSSOM View: "if document has a root element, and the last item in sequence is not the root
    /// element, append the root element". Chrome 150 does, measured: a frame whose only
    /// content is an absolutely positioned `#in` has a ZERO-height `<html>` box, and
    /// `elementsFromPoint` on `#in` still answers `#in html`. Only the DOM members append it; the
    /// hover chain is the boxes under the pointer and nothing else.
    pub fn elements_from_point_for_document(&self, x: f32, y: f32, scroll: &ScrollSnapshot) -> Vec<NodeId> {
        let mut hits = self.elements_from_point(x, y, scroll);
        if hits.is_empty() {
            let viewport = self.viewport();
            if !(x >= 0.0 && y >= 0.0 && x < viewport.width && y < viewport.height) {
                return hits;
            }
        }
        if let Some(root) = self.root_box().map(|b| self.node_of(b)) {
            if hits.last() != Some(&root) {
                hits.retain(|n| *n != root);
                hits.push(root);
            }
        }
        hits
    }

    /// Is the point on this box — on any **fragment** of it?
    ///
    /// The distinction is not academic. A `<span>` broken across two lines has a bounding rect
    /// whose centre falls in the gap between them, and Chrome does not report the span there: on
    /// the probe page, `elementFromPoint` at the centre of a two-line span's bounding rect answered
    /// the **block**, while the centre of each of its two `getClientRects()` entries answered the
    /// span. A hit test against the union would have claimed the leading between the lines, and a
    /// probe aiming at a rect centre would then click something that is not there.
    fn hits(&self, _index: u32, rects: &[Rect], x: f32, y: f32) -> bool {
        rects.iter().any(|r| r.contains(x, y))
    }

    /// The rect a hit test compares against, for tests and for a caller that wants to draw it.
    pub fn hit_rect(&self, node: NodeId) -> Option<Rect> {
        self.box_of(node).map(|b| self.border_box(b))
    }

    /// The hover chain for a viewport point: the element the pointer is over and every element
    /// ancestor of it, **outermost first**. Empty for a point on no box at all.
    ///
    /// This is what `:hover` matches (`Dom::hover_chain`), and it is deliberately *not*
    /// [`BoxTree::elements_from_point`]. The box stack is every box whose rect contains the point,
    /// which for a `position: fixed` overlay includes unrelated content sitting underneath it —
    /// content a browser does not consider hovered. The ancestor walk answers the question the
    /// cascade is actually asking: "is this element, or something inside it, under the pointer?"
    ///
    /// Both shapes were checked against stock Chrome 150 over CDP. Entering
    /// `div#outer > div#mid > div#inner` gave `[html, body, #outer, #mid, #inner]`; a point past
    /// `<body>`'s box but still inside `<html>`'s gave `[html]` — `<body>` had left the chain while
    /// `<html>` stayed, which is the ancestor walk and not the box stack.
    ///
    /// Non-element ancestors (the Document node) are skipped: `:hover` is a selector and only
    /// elements are selected. Chrome agrees — its `querySelectorAll(":hover")` starts at `html`.
    pub fn hover_chain_at(&self, dom: &korvus_dom::Dom, x: f32, y: f32, scroll: &ScrollSnapshot) -> Vec<NodeId> {
        let Some(hit) = self.element_from_point(x, y, scroll) else { return Vec::new() };
        let mut chain = Vec::new();
        let mut cur = Some(hit);
        // A parent chain visits each node once, but the walk is capped anyway: the arena's own
        // insertion guards call `contains`, which caps for the same reason, and a hover chain is
        // built on every pointer sample where a hang would be invisible until the page froze.
        let mut budget = dom.len();
        while let Some(node) = cur {
            if budget == 0 {
                break;
            }
            budget -= 1;
            if matches!(dom.node(node).kind, korvus_dom::NodeKind::Element { .. }) {
                chain.push(node);
            }
            cur = dom.parent(node);
        }
        chain.reverse();
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::media::MediaContext;
    use crate::Page;
    use korvus_dom::Dom;

    /// Lay `html` out and return a hit-test closure over it.
    fn page(html: &str) -> (Dom, crate::LaidOut) {
        let page = Page::default();
        let dom = Dom::parse(html);
        let out = page.layout(&dom, &MediaContext::default());
        (dom, out)
    }

    fn id_of(dom: &Dom, node: NodeId) -> String {
        dom.get_attribute(node, "id").unwrap_or("<no id>").to_string()
    }

    #[test]
    fn a_point_inside_a_box_finds_it() {
        let (dom, out) = page(
            "<html><body><div id='a' style='width:100px;height:50px'></div></body></html>",
        );
        // The div sits at (8, 8) after body's margin.
        assert_eq!(out.element_from_point(50.0, 30.0).map(|n| id_of(&dom, n)), Some("a".into()));
        assert_eq!(out.element_from_point(50.0, 30.0).is_some(), true);
    }

    #[test]
    fn a_point_outside_every_box_finds_nothing() {
        let (_, out) = page(
            "<html><body><div id='a' style='width:100px;height:50px'></div></body></html>",
        );
        assert_eq!(out.element_from_point(-5.0, -5.0), None);
        assert_eq!(out.element_from_point(5000.0, 5000.0), None);
    }

    // A child paints over its parent, so a point inside both belongs to the child.
    #[test]
    fn the_innermost_box_wins() {
        let (dom, out) = page(
            "<html><body><div id='outer' style='width:200px;height:100px'>\
               <div id='inner' style='width:50px;height:20px'></div></div></body></html>",
        );
        assert_eq!(out.element_from_point(20.0, 15.0).map(|n| id_of(&dom, n)), Some("inner".into()));
        // A point inside the outer box but not the inner one finds the outer.
        assert_eq!(out.element_from_point(150.0, 80.0).map(|n| id_of(&dom, n)), Some("outer".into()));
    }

    // `elementsFromPoint` is the whole stack, topmost first.
    #[test]
    fn the_whole_stack_comes_back_topmost_first() {
        let (dom, out) = page(
            "<html><body><div id='outer' style='width:200px;height:100px'>\
               <div id='inner' style='width:50px;height:20px'></div></div></body></html>",
        );
        let stack: Vec<String> =
            out.elements_from_point(20.0, 15.0).into_iter().map(|n| id_of(&dom, n)).collect();
        assert_eq!(stack, vec!["inner", "outer", "<no id>", "<no id>"], "inner, outer, body, html");
    }

    // Each element appears once, even though its text is a box of its own carrying the same id.
    #[test]
    fn an_element_with_text_is_reported_once() {
        let (dom, out) = page(
            "<html><body><div id='a' style='width:200px;height:50px'>hello</div></body></html>",
        );
        let stack: Vec<String> =
            out.elements_from_point(20.0, 15.0).into_iter().map(|n| id_of(&dom, n)).collect();
        assert_eq!(stack.iter().filter(|s| *s == "a").count(), 1, "{stack:?}");
    }

    // A later sibling paints over an earlier one where they overlap.
    #[test]
    fn a_later_sibling_paints_over_an_earlier_one() {
        let (dom, out) = page(
            "<html><body>\
               <div id='under' style='height:50px'></div>\
               <div id='over' style='height:50px;margin-top:-25px'></div>\
             </body></html>",
        );
        // The overlap is 25px tall, starting where `over`'s negative margin pulls it up.
        assert_eq!(out.element_from_point(50.0, 40.0).map(|n| id_of(&dom, n)), Some("over".into()));
    }

    #[test]
    fn a_hidden_element_is_not_hit() {
        let (dom, out) = page(
            "<html><body>\
               <div id='gone' style='display:none;width:100px;height:50px'></div>\
               <div id='there' style='width:100px;height:50px'></div>\
             </body></html>",
        );
        assert_eq!(out.element_from_point(50.0, 30.0).map(|n| id_of(&dom, n)), Some("there".into()));
    }

    // -----------------------------------------------------------------------
    // The hover chain. Every shape below is what stock Chrome
    // 150.0.7871.46 answered to `querySelectorAll(":hover")` when the same
    // document was driven over CDP (`Input.dispatchMouseEvent` mouseMoved).
    // -----------------------------------------------------------------------

    fn ids(dom: &Dom, chain: &[NodeId]) -> Vec<String> {
        chain.iter().map(|n| id_of(dom, *n)).collect()
    }

    #[test]
    fn the_hover_chain_is_the_hit_element_and_its_ancestors_outermost_first() {
        let (dom, out) = page(
            "<html><body><div id='outer' style='width:200px;height:100px'>\
               <div id='inner' style='width:50px;height:20px'></div></div></body></html>",
        );
        // Chrome: [html, body, #outer, #inner] -- <html> and <body> are in the chain too.
        assert_eq!(
            ids(&dom, &out.hover_chain_at(&dom, 20.0, 15.0)),
            vec!["<no id>", "<no id>", "outer", "inner"],
            "html, body, outer, inner"
        );
        // A point inside the outer box but not the inner one stops at the outer.
        assert_eq!(
            ids(&dom, &out.hover_chain_at(&dom, 150.0, 80.0)),
            vec!["<no id>", "<no id>", "outer"]
        );
    }

    #[test]
    fn a_point_on_no_box_has_an_empty_hover_chain() {
        let (dom, out) = page(
            "<html><body><div id='a' style='width:100px;height:50px'></div></body></html>",
        );
        assert!(out.hover_chain_at(&dom, -5.0, -5.0).is_empty());
        assert!(out.hover_chain_at(&dom, 5000.0, 5000.0).is_empty());
    }

    // The distinction the doc comment draws, as a test: an overlapping box that is NOT an ancestor
    // of the hit element is in the box stack and must NOT be in the hover chain.
    #[test]
    fn an_overlapping_non_ancestor_is_in_the_stack_but_not_the_chain() {
        let (dom, out) = page(
            "<html><body>\
               <div id='under' style='height:50px'></div>\
               <div id='over' style='height:50px;margin-top:-25px'></div>\
             </body></html>",
        );
        let stack = ids(&dom, &out.elements_from_point(50.0, 40.0));
        assert!(stack.contains(&"under".to_string()), "the stack sees both: {stack:?}");
        assert!(stack.contains(&"over".to_string()), "{stack:?}");
        let chain = ids(&dom, &out.hover_chain_at(&dom, 50.0, 40.0));
        assert_eq!(chain, vec!["<no id>", "<no id>", "over"], "only the hit and its ancestors");
    }

    // Half-open edges: two boxes sharing one never both claim the point.
    #[test]
    fn boxes_that_share_an_edge_do_not_both_claim_it() {
        let (dom, out) = page(
            "<html><body>\
               <div id='top' style='height:20px'></div>\
               <div id='bottom' style='height:20px'></div>\
             </body></html>",
        );
        // y=28 is the last row of `top` (it starts at 8); y=28+20=28 is `bottom`'s first.
        assert_eq!(out.element_from_point(50.0, 27.9).map(|n| id_of(&dom, n)), Some("top".into()));
        assert_eq!(out.element_from_point(50.0, 28.0).map(|n| id_of(&dom, n)), Some("bottom".into()));
    }
}
