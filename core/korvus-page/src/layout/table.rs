//! Table layout: CSS 2 §17 in the separated-borders model, as Blink's `TableLayoutAlgorithm` answers it
//! for the tables pages build.
//!
//! taffy has no table algorithm, and a table laid out as a block was as wide as its container with
//! every cell on a line of its own: an interactive challenge widget's probe table (a caption, a `<th>` and a
//! `<td>`) read 743 x 67 where Chrome 150 answers 132.2 x 81. Measured on Chrome 150.0.7871.46
//! (`fixtures/tables/*.js`):
//!
//! * the grid: rows in row groups -- the first `thead` first, the first `tfoot` last, whatever the DOM
//!   order -- cells placed left to right past the slots earlier rows' `rowspan`s hold; consecutive cells
//!   (or rows) with no row (or row group) of their own share an anonymous one;
//! * columns: each cell's min- and max-content border-box width; a length `width` makes its column
//!   fixed. An `auto`-width table is as wide as its columns' max-content sum, no wider than its container
//!   allows and no narrower than their min-content sum (or its captions'); extra width goes to the
//!   auto columns in proportion to their max-content widths, a shortfall comes out of each between its
//!   min and max; a `colspan` cell's excess is spread over the columns it spans in proportion to theirs.
//!   `table-layout: fixed` takes the first row's widths and shares the rest equally;
//! * `border-spacing` around and between the cells (2px for `<table>`, 0 for any other table box); a
//!   row group and a row span exactly their cells;
//! * rows as tall as their tallest cell (a `rowspan` cell's excess added to its last row) -- a cell as tall as
//!   its content or its length `height`, whichever is more -- and a table's own height shared among them
//!   ([`distribute_rows`]); every cell stretched to its row and its content placed by `vertical-align`
//!   (`middle` from the UA sheet) by the content's own height;
//! * captions laid out at the table's width above its border (below with `caption-side: bottom`), inside
//!   the table's own rect.
//!
//! `border-collapse: collapse` is laid out as separated borders with no spacing between them (each cell keeps
//! its own borders rather than sharing them with its neighbour).
//!
//! A box that is not a proper child where it stands -- text or a block straight under a table, a row
//! group or a row -- is wrapped as CSS 2 §17.2.1 wraps it: in an anonymous cell, in an anonymous row
//! where its parent is not one. Chrome 150: `<span style="display:table">` over
//! a run of text is as wide as that text and one line tall; it laid out as nothing here (0 x 0). A run
//! of such siblings shares ONE anonymous cell there; each is its own cell here, a row apiece -- the same
//! stack wherever the table's `border-spacing` is 0, which is every table box but `<table>`'s.

use crate::layout::style::{TableRole, VAlign};
use crate::layout::tree::BoxTree;
use taffy::geometry::{Line, Point, Size};
use taffy::style::AvailableSpace;
use taffy::tree::{Layout, LayoutInput, LayoutOutput, LayoutPartialTree, RequestedAxis, RunMode, SizingMode};

/// One cell placed in the grid.
struct Cell {
    index: u32,
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
    min: f32,
    max: f32,
    /// A length `width` on the cell, as a border-box width.
    fixed: Option<f32>,
}

/// One row: its box (`None` for an anonymous row) and the row group it is in.
struct Row {
    index: Option<u32>,
    group: usize,
}

/// One row group: its box (`None` for an anonymous one) and its rows' range.
struct Group {
    index: Option<u32>,
    rows: std::ops::Range<usize>,
}

fn lu(px: f32) -> f32 {
    crate::layout::style::layout_unit(px)
}

impl BoxTree {
    fn probe_input(&self, width: AvailableSpace, known: Size<Option<f32>>, mode: RunMode) -> LayoutInput {
        LayoutInput {
            run_mode: mode,
            sizing_mode: SizingMode::InherentSize,
            axis: RequestedAxis::Both,
            known_dimensions: known,
            known_dimensions_are_definite: Size { width: true, height: true },
            parent_size: Size { width: known.width, height: None },
            available_space: Size { width, height: AvailableSpace::MaxContent },
            vertical_margins_are_collapsible: Line::FALSE,
        }
    }

    /// The measurement of a cell's CONTENT height at width `w`: its own `height` left out (`SizingMode::ContentSize`;
    /// its `min-height` and `max-height` the layout style never has, see `LayoutStyles::intern`). The table adds the
    /// cell's `height` as a minimum ([`BoxTree::cell_height`]) and `vertical-align` places the content by this.
    fn cell_probe(&self, w: f32) -> LayoutInput {
        LayoutInput { sizing_mode: SizingMode::ContentSize, ..self.probe_input(AvailableSpace::Definite(w), Size { width: Some(w), height: None }, RunMode::ComputeSize) }
    }

    /// A cell's length `height` as a border-box height, if it has one. Chrome 152.0.7977.82 takes it as the least the
    /// cell's rows give it: a `<td style="height:30px">` around 50px of content is 50 tall, one with `height:60px;
    /// padding:5px` 70 and with `box-sizing:border-box` too 60 (2026-10-23).
    fn cell_height(&self, index: u32) -> Option<f32> {
        let cs = self.style_of_box(index);
        let raw = cs.size.height.into_raw();
        if raw.tag() != taffy::style::CompactLength::LENGTH_TAG {
            return None;
        }
        let (b, p) = (crate::layout::tree::rect_of(cs.border), crate::layout::tree::rect_of(cs.padding));
        let edges = b.top + b.bottom + p.top + p.bottom;
        Some(if cs.box_sizing == taffy::style::BoxSizing::ContentBox { raw.value() + edges } else { raw.value().max(edges) })
    }

    fn place(&mut self, index: u32, x: f32, y: f32, size: Size<f32>, out: Option<LayoutOutput>) {
        let style = self.style_of_box(index);
        let (border, padding) = (crate::layout::tree::rect_of(style.border), crate::layout::tree::rect_of(style.padding));
        let overflow = out.map_or(
            taffy::geometry::Rect { left: 0.0, right: size.width, top: 0.0, bottom: size.height },
            |o| o.scrollable_overflow_rect,
        );
        let layout = Layout {
            order: 0,
            location: Point { x, y },
            size,
            scrollbar_size: Size::ZERO,
            border,
            padding,
            margin: taffy::geometry::Rect::zero(),
            scrollable_overflow_rect: overflow,
        };
        LayoutPartialTree::set_unrounded_layout(self, taffy::NodeId::from(index as usize), &layout);
    }

    /// Lay out the table box `node` (see the module note) and return its size to taffy.
    pub(crate) fn layout_table(&mut self, node: taffy::NodeId, inputs: LayoutInput) -> LayoutOutput {
        let index = usize::from(node) as u32;
        let info = self.table_info(index);
        let style = self.style_of_box(index).clone();
        let (border, padding) = (crate::layout::tree::rect_of(style.border), crate::layout::tree::rect_of(style.padding));
        let (sx, sy) = info.spacing;

        // ── the grid ────────────────────────────────────────────────────────────────────────────────
        let mut captions: Vec<u32> = Vec::new();
        let mut heads: Vec<(Option<u32>, Vec<u32>)> = Vec::new();
        let mut bodies: Vec<(Option<u32>, Vec<u32>)> = Vec::new();
        let mut foots: Vec<(Option<u32>, Vec<u32>)> = Vec::new();
        let mut others: Vec<u32> = Vec::new();
        // loose rows and cells straight under the table, gathered into an anonymous group as they come
        let mut loose: Vec<u32> = Vec::new();
        let flush = |loose: &mut Vec<u32>, bodies: &mut Vec<(Option<u32>, Vec<u32>)>| {
            if !loose.is_empty() {
                bodies.push((None, std::mem::take(loose)));
            }
        };
        for child in self.children_of(index) {
            match self.table_role(child) {
                TableRole::Caption => captions.push(child),
                TableRole::RowGroup(kind) => {
                    flush(&mut loose, &mut bodies);
                    let rows = self.children_of(child);
                    match kind {
                        0 if heads.is_empty() => heads.push((Some(child), rows)),
                        2 if foots.is_empty() => foots.push((Some(child), rows)),
                        _ => bodies.push((Some(child), rows)),
                    }
                }
                TableRole::Column | TableRole::ColumnGroup => others.push(child),
                // a row, a cell, or anything else -- which an anonymous row and cell wrap
                _ => loose.push(child),
            }
        }
        flush(&mut loose, &mut bodies);
        let (nheads, nbodies) = (heads.len(), bodies.len());
        let ordered: Vec<(Option<u32>, Vec<u32>)> = heads.into_iter().chain(bodies).chain(foots).collect();

        let mut groups: Vec<Group> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut row_cells: Vec<Vec<u32>> = Vec::new();
        for (gi, (group, members)) in ordered.iter().enumerate() {
            let start = rows.len();
            let mut cells_run: Vec<u32> = Vec::new();
            for &m in members {
                match self.table_role(m) {
                    TableRole::Row => {
                        if !cells_run.is_empty() {
                            rows.push(Row { index: None, group: gi });
                            row_cells.push(std::mem::take(&mut cells_run));
                        }
                        // a child that is not a cell is wrapped in an anonymous one
                        let cells = self
                            .children_of(m)
                            .into_iter()
                            .filter(|&c| !matches!(self.table_role(c), TableRole::Column | TableRole::ColumnGroup))
                            .collect();
                        rows.push(Row { index: Some(m), group: gi });
                        row_cells.push(cells);
                    }
                    TableRole::Cell => cells_run.push(m),
                    TableRole::Column | TableRole::ColumnGroup => others.push(m),
                    // anything else: an anonymous cell in an anonymous row of its own
                    _ => {
                        if !cells_run.is_empty() {
                            rows.push(Row { index: None, group: gi });
                            row_cells.push(std::mem::take(&mut cells_run));
                        }
                        rows.push(Row { index: None, group: gi });
                        row_cells.push(vec![m]);
                    }
                }
            }
            if !cells_run.is_empty() {
                rows.push(Row { index: None, group: gi });
                row_cells.push(cells_run);
            }
            groups.push(Group { index: *group, rows: start..rows.len() });
        }
        // anything in a row that is not a cell lays out as nothing
        for (r, row) in rows.iter().enumerate() {
            if let Some(ri) = row.index {
                for c in self.children_of(ri) {
                    if !row_cells[r].contains(&c) {
                        others.push(c);
                    }
                }
            }
        }

        // cells into slots, past the ones rowspans above still hold
        let mut cells: Vec<Cell> = Vec::new();
        let mut held: Vec<usize> = Vec::new(); // per column: how many more rows it is held for
        for (r, list) in row_cells.iter().enumerate() {
            let group_end = groups[rows[r].group].rows.end;
            let mut col = 0usize;
            for &c in list {
                while col < held.len() && held[col] > 0 {
                    col += 1;
                }
                let (cs, rs) = self.spans_of(c);
                let colspan = cs.max(1) as usize;
                let rowspan = if rs == 0 { group_end - r } else { (rs as usize).min(group_end - r).max(1) };
                if held.len() < col + colspan {
                    held.resize(col + colspan, 0);
                }
                for h in &mut held[col..col + colspan] {
                    *h = rowspan;
                }
                cells.push(Cell { index: c, row: r, col, colspan, rowspan, min: 0.0, max: 0.0, fixed: None });
                col += colspan;
            }
            for h in held.iter_mut() {
                *h = h.saturating_sub(1);
            }
        }
        let ncols = cells.iter().map(|c| c.col + c.colspan).max().unwrap_or(0);
        let nrows = rows.len();

        // ── columns ─────────────────────────────────────────────────────────────────────────────────
        for cell in cells.iter_mut() {
            let n = taffy::NodeId::from(cell.index as usize);
            cell.min = LayoutPartialTree::compute_child_layout(self, n, self.probe_input(AvailableSpace::MinContent, Size::NONE, RunMode::ComputeSize)).size.width;
            cell.max = LayoutPartialTree::compute_child_layout(self, n, self.probe_input(AvailableSpace::MaxContent, Size::NONE, RunMode::ComputeSize)).size.width;
            let cs = self.style_of_box(cell.index);
            let raw = cs.size.width.into_raw();
            if raw.tag() == taffy::style::CompactLength::LENGTH_TAG {
                let (b, p) = (crate::layout::tree::rect_of(cs.border), crate::layout::tree::rect_of(cs.padding));
                let extra = if cs.box_sizing == taffy::style::BoxSizing::ContentBox { b.left + b.right + p.left + p.right } else { 0.0 };
                cell.fixed = Some(raw.value() + extra);
            }
        }
        let mut col_min = vec![0.0f32; ncols];
        let mut col_max = vec![0.0f32; ncols];
        let mut col_fixed: Vec<Option<f32>> = vec![None; ncols];
        for cell in cells.iter().filter(|c| c.colspan == 1) {
            col_min[cell.col] = col_min[cell.col].max(cell.min);
            col_max[cell.col] = col_max[cell.col].max(cell.max);
            if let Some(f) = cell.fixed {
                col_fixed[cell.col] = Some(col_fixed[cell.col].map_or(f, |g: f32| g.max(f)));
            }
        }
        for (c, f) in col_fixed.iter().enumerate() {
            if let Some(f) = f {
                col_max[c] = col_min[c].max(*f);
            }
        }
        let mut spanning: Vec<&Cell> = cells.iter().filter(|c| c.colspan > 1).collect();
        spanning.sort_by_key(|c| c.colspan);
        for cell in spanning {
            let range = cell.col..cell.col + cell.colspan;
            let between = sx * (cell.colspan - 1) as f32;
            let weights: Vec<f32> = col_max[range.clone()].to_vec();
            let total: f32 = weights.iter().sum();
            for (want, cols) in [(cell.min, &mut col_min), (cell.max, &mut col_max)] {
                let have: f32 = cols[range.clone()].iter().sum::<f32>() + between;
                if want > have {
                    for (i, c) in range.clone().enumerate() {
                        let share = if total > 0.0 { weights[i] / total } else { 1.0 / cell.colspan as f32 };
                        cols[c] += (want - have) * share;
                    }
                }
            }
            for c in range {
                col_max[c] = col_max[c].max(col_min[c]);
            }
        }

        let edges = border.left + border.right + padding.left + padding.right;
        let spacing_x = if ncols > 0 { sx * (ncols + 1) as f32 } else { 0.0 };
        let min_t = col_min.iter().sum::<f32>() + spacing_x + edges;
        let max_t = col_max.iter().sum::<f32>() + spacing_x + edges;
        let mut caption_min = 0.0f32;
        for &cap in &captions {
            let n = taffy::NodeId::from(cap as usize);
            let w = LayoutPartialTree::compute_child_layout(self, n, self.probe_input(AvailableSpace::MinContent, Size::NONE, RunMode::ComputeSize)).size.width;
            let m = self.style_of_box(cap).margin;
            caption_min = caption_min.max(w + crate::layout::tree::used_auto(m.left) + crate::layout::tree::used_auto(m.right));
        }
        let floor = min_t.max(caption_min);
        // taffy hands a table no size of its own (`is_table`): it resolves its `width` here, a
        // border-box one as the UA's `box-sizing: border-box` makes it
        let own = |raw: taffy::style::CompactLength, basis: Option<f32>, edges: f32| -> Option<f32> {
            let v = match raw.tag() {
                taffy::style::CompactLength::LENGTH_TAG => Some(raw.value()),
                taffy::style::CompactLength::PERCENT_TAG => basis.map(|b| raw.value() * b),
                _ if raw.is_calc() => basis.map(|b| crate::style::math::resolve_ptr(raw.calc_value(), b)),
                _ => None,
            }?;
            Some(if style.box_sizing == taffy::style::BoxSizing::ContentBox { v + edges } else { v })
        };
        let known_width = inputs.known_dimensions.width.or_else(|| own(style.size.width.into_raw(), inputs.parent_size.width, edges));
        // a sizing keyword's width (CSS Sizing 3 §3.2) where nothing decided one: `min-content` the columns'
        // minimum, `max-content` their maximum, `stretch` the space on offer; `fit-content` is `auto`'s
        // shrink-to-fit. Chrome 150: a `width: max-content` table of a 305px run of
        // text in a 100px box is 305 wide, a `stretch` one in a 1000px box 994 -- this sized both as `auto`.
        let keyword = match style.size.width.expand() {
            taffy::style::ExpandedDimension::MinContent => Some(floor),
            taffy::style::ExpandedDimension::MaxContent => Some(max_t.max(floor)),
            taffy::style::ExpandedDimension::Stretch => Some(match inputs.available_space.width {
                AvailableSpace::Definite(a) => a.max(floor),
                AvailableSpace::MaxContent => max_t.max(floor),
                AvailableSpace::MinContent => floor,
            }),
            _ => None,
        };
        let width = match (known_width, keyword) {
            (Some(w), _) => w.max(floor),
            (None, Some(w)) => w,
            (None, None) => match inputs.available_space.width {
                AvailableSpace::Definite(a) => floor.max(max_t.min(a)),
                AvailableSpace::MaxContent => max_t.max(floor),
                AvailableSpace::MinContent => floor,
            },
        };
        let target = (width - edges - spacing_x).max(0.0);
        let widths = distribute(target, &col_min, &col_max, &col_fixed, info.fixed && known_width.is_some());

        // ── rows ────────────────────────────────────────────────────────────────────────────────────
        let col_x: Vec<f32> = {
            let mut x = border.left + padding.left + sx;
            widths.iter().map(|w| { let at = x; x += w + sx; at }).collect()
        };
        let span_width = |cell: &Cell| widths[cell.col..cell.col + cell.colspan].iter().sum::<f32>() + sx * (cell.colspan - 1) as f32;
        let mut heights = vec![0.0f32; cells.len()];
        // a row with a length height of its own or on one of its cells: what a table's own height reaches last
        let mut fixed_row = vec![false; nrows];
        let mut cell_fixed = vec![false; cells.len()];
        for (i, cell) in cells.iter().enumerate() {
            let w = span_width(cell);
            let n = taffy::NodeId::from(cell.index as usize);
            heights[i] = LayoutPartialTree::compute_child_layout(self, n, self.cell_probe(w)).size.height;
            if let Some(h) = self.cell_height(cell.index) {
                heights[i] = heights[i].max(h);
                cell_fixed[i] = true;
                if cell.rowspan == 1 {
                    fixed_row[cell.row] = true;
                }
            }
        }
        let mut row_h = vec![0.0f32; nrows];
        for (r, row) in rows.iter().enumerate() {
            if let Some(ri) = row.index {
                let raw = self.style_of_box(ri).size.height.into_raw();
                if raw.tag() == taffy::style::CompactLength::LENGTH_TAG {
                    row_h[r] = raw.value();
                    fixed_row[r] = true;
                }
            }
        }
        for (i, cell) in cells.iter().enumerate().filter(|(_, c)| c.rowspan == 1) {
            row_h[cell.row] = row_h[cell.row].max(heights[i]);
        }
        for (i, cell) in cells.iter().enumerate().filter(|(_, c)| c.rowspan > 1) {
            let range = cell.row..cell.row + cell.rowspan;
            let have: f32 = row_h[range.clone()].iter().sum::<f32>() + sy * (cell.rowspan - 1) as f32;
            if heights[i] > have {
                row_h[range.end - 1] += heights[i] - have;
            }
        }
        for h in row_h.iter_mut() {
            *h = lu(*h);
        }
        // the table's own height, less its border, padding and spacing, where the rows need less (captions are outside it)
        let vedges = border.top + border.bottom + padding.top + padding.bottom;
        // `height` within `min-height` and `max-height`, the minimum winning; a `min-height` alone is a height to share
        // too. Chrome 152.0.7977.82 (2026-10-23), the table's height and its one cell's: `height: 100px; max-height:
        // 50px` 50 and 50 (36 with `padding: 5px; border: 2px`, 50 in a 64px table with `box-sizing: content-box`);
        // `height: 100%; max-height: 80px` in a 300px box 80 and 80; `min-height: 150px` 150 and 150, also over
        // `height: 100px`, and over `max-height: 50px`; two rows under `height: 200px; max-height: 120px` 60 each.
        // Their percentages only against a parent of a `height` of its own: an `auto`-height absolute box hands its
        // children the height it was measured at, and Chrome leaves a `max-height: 50%` table there at its 100px `height`
        // (a 120px box: 60), a `min-height: 150%` one at its content's 10.
        let basis = inputs.parent_size.height;
        let styled_height = own(style.size.height.into_raw(), basis, vedges);
        let parent_sized = self.parent_of(index).is_some_and(|p| !self.style_of_box(p).size.height.is_auto());
        let limit = |d: taffy::style::LengthPercentageAuto| {
            let raw = d.into_raw();
            let relative = raw.tag() == taffy::style::CompactLength::PERCENT_TAG || raw.is_calc();
            if relative && !parent_sized { None } else { own(raw, basis, vedges) }
        };
        let (min_height, max_height) = (limit(style.min_size.height), limit(style.max_size.height));
        let own_height = match (styled_height, min_height) {
            (Some(h), _) => Some(max_height.map_or(h, |m| h.min(m)).max(min_height.unwrap_or(0.0))),
            (None, m) => m,
        };
        let grid_height = match (own_height, inputs.known_dimensions.height) {
            (Some(h), _) => Some(h),
            (None, Some(h)) => Some(h - self.captions_height(&captions, width, false) - self.captions_height(&captions, width, true)),
            (None, None) => None,
        };
        if let Some(h) = grid_height {
            let spacing_y = if nrows > 0 { sy * (nrows + 1) as f32 } else { 0.0 };
            let extra = h - vedges - spacing_y - row_h.iter().sum::<f32>();
            if extra > 0.0 {
                let body = |r: &usize| {
                    let g = rows[*r].group;
                    g >= nheads && g < nheads + nbodies
                };
                let any_body = (0..nrows).any(|r| body(&r));
                let candidates: Vec<usize> = (0..nrows).filter(|r| !any_body || body(r)).collect();
                distribute_rows(&mut row_h, &candidates, &fixed_row, extra);
            }
        }

        // ── placing ────────────────────────────────────────────────────────────────────────────────
        let mut y = 0.0f32;
        let place_captions = |tree: &mut BoxTree, y: &mut f32, bottom: bool| {
            for &cap in &captions {
                if tree.table_info(cap).caption_bottom != bottom {
                    continue;
                }
                let m = tree.style_of_box(cap).margin;
                let (ml, mr, mt, mb) = (
                    crate::layout::tree::used_auto(m.left),
                    crate::layout::tree::used_auto(m.right),
                    crate::layout::tree::used_auto(m.top),
                    crate::layout::tree::used_auto(m.bottom),
                );
                let w = (width - ml - mr).max(0.0);
                let n = taffy::NodeId::from(cap as usize);
                let out = LayoutPartialTree::compute_child_layout(tree, n, tree.probe_input(AvailableSpace::Definite(w), Size { width: Some(w), height: None }, RunMode::PerformLayout));
                tree.place(cap, ml, *y + mt, out.size, Some(out));
                *y += mt + out.size.height + mb;
            }
        };
        if inputs.run_mode == RunMode::PerformLayout {
            place_captions(self, &mut y, false);
        } else {
            y += self.captions_height(&captions, width, false);
        }
        let grid_top = y;
        let inner_x = border.left + padding.left + sx;
        let inner_w = widths.iter().sum::<f32>() + sx * (ncols.saturating_sub(1)) as f32;
        let mut row_y = vec![0.0f32; nrows];
        let mut cursor = grid_top + border.top + padding.top + if nrows > 0 { sy } else { 0.0 };
        for (r, h) in row_h.iter().enumerate() {
            row_y[r] = cursor;
            cursor += h + sy;
        }
        let grid_bottom = if nrows > 0 { cursor } else { grid_top + border.top + padding.top } + padding.bottom + border.bottom;
        y = grid_bottom;
        if inputs.run_mode == RunMode::PerformLayout {
            place_captions(self, &mut y, true);
        } else {
            y += self.captions_height(&captions, width, true);
        }
        let height = match inputs.known_dimensions.height.or(own_height) {
            Some(h) => h.max(y),
            None => y,
        };

        if inputs.run_mode == RunMode::PerformLayout {
            for (gi, group) in groups.iter().enumerate() {
                let (top, bottom) = match (group.rows.start, group.rows.end) {
                    (s, e) if s < e => (row_y[s], row_y[e - 1] + row_h[e - 1]),
                    _ => (grid_top, grid_top),
                };
                if let Some(g) = group.index {
                    self.place(g, inner_x, top, Size { width: inner_w, height: bottom - top }, None);
                }
                // the rows of this group, from the group's box when it has one
                let (gx, gy) = if group.index.is_some() { (inner_x, top) } else { (0.0, 0.0) };
                for r in group.rows.clone() {
                    if let Some(ri) = rows[r].index {
                        self.place(ri, inner_x - gx, row_y[r] - gy, Size { width: inner_w, height: row_h[r] }, None);
                    }
                }
                let _ = gi;
            }
            for (i, cell) in cells.iter().enumerate() {
                let w = span_width(cell);
                let h = row_h[cell.row..cell.row + cell.rowspan].iter().sum::<f32>() + sy * (cell.rowspan - 1) as f32;
                let n = taffy::NodeId::from(cell.index as usize);
                // A percentage height inside the cell resolves against the cell's height only when the cell or the table
                // has a height of its own; under a row's height or a taller neighbour it is `auto`. Chrome 152.0.7977.82
                // on a 10px-wide child with `height: 50%` in a 60px row: 30 tall when the `<td>` or the `<table>` says
                // 60px, 0 when the `<tr>` does; `height: 100%` beside a 60px cell 0 (2026-10-23). A table's `min-height`
                // alone does not make it definite: 0 under `min-height: 100px`, 25 under `height: 100px; max-height: 50px`.
                let definite = cell_fixed[i] || styled_height.is_some();
                let mut input = self.probe_input(AvailableSpace::Definite(w), Size { width: Some(w), height: Some(h) }, RunMode::PerformLayout);
                input.known_dimensions_are_definite.height = definite;
                let out = LayoutPartialTree::compute_child_layout(self, n, input);
                // the cell's origin: its row's when the row has a box, else its row group's, else the table's
                let row = &rows[cell.row];
                let group = &groups[row.group];
                let (ox, oy) = match (row.index, group.index) {
                    (Some(_), _) => (inner_x, row_y[cell.row]),
                    (None, Some(_)) => (inner_x, row_y[group.rows.start]),
                    (None, None) => (0.0, 0.0),
                };
                self.place(cell.index, col_x[cell.col] - ox, row_y[cell.row] - oy, Size { width: w, height: h }, Some(out));
                // its content is placed by `vertical-align` in that layout (`align_cell_content`)
            }
            for o in others {
                self.place(o, 0.0, 0.0, Size::ZERO, None);
            }
        }
        LayoutOutput::from_sizes(
            Size { width, height },
            taffy::geometry::Rect { left: 0.0, right: width, top: 0.0, bottom: height },
        )
    }

    /// Place a table cell's content by `vertical-align` -- `middle` from the UA sheet, `top`/`bottom`, a baseline as
    /// top -- when `inputs` lay the cell out at its rows' height: called from INSIDE the cell's cached computation, so
    /// the offset is part of what its cache answers. It was added by the table after the cell's layout, and a cell
    /// answered from its cache -- a second pass for a revealed scrollbar, a relayout carrying the cell's state --
    /// had its content, still where the last offset left it, moved by the offset again: `<td>a<br>a2<br>a3</td><td>
    /// <div id=x>` read x.offsetTop 17, 33, 49, 65 over relayouts where Chrome 152.0.7977.82 reads 17 every time
    /// (2026-10-23).
    pub(crate) fn align_cell_content(&mut self, node: taffy::NodeId, inputs: LayoutInput) {
        let index = usize::from(node) as u32;
        if inputs.run_mode != RunMode::PerformLayout || self.table_role(index) != TableRole::Cell {
            return;
        }
        let (Some(w), Some(h)) = (inputs.known_dimensions.width, inputs.known_dimensions.height) else {
            return;
        };
        let valign = self.valign_of(index);
        if !matches!(valign, VAlign::Middle | VAlign::Bottom) {
            return;
        }
        // The content's own height, the cell's `height` left out: the measurement the table sized the rows from
        // (answered from the cache), or what this layout's content reaches when that is more -- a percentage height
        // resolved against the cell's, which the measurement had as `auto`. Measured with its `height` the cell was as
        // tall as its row and nothing moved: Chrome 152.0.7977.82 puts a 10px block 25px down a `<td height=60>` (50
        // with `vertical-align: bottom`), and a 30px `height: 50%` one 15px down; this had both at 0 (2026-10-23).
        let probe = self.cell_probe(w);
        let measured = LayoutPartialTree::compute_child_layout(self, node, probe).size.height;
        let style = self.style_of_box(index);
        let (b, p) = (crate::layout::tree::rect_of(style.border), crate::layout::tree::rect_of(style.padding));
        // A child's place in the flow, without the `top`/`bottom` shift block layout gave a relative or sticky one
        // (`flow_shift_y`); the inline content of a line has none on its location. Chrome 152.0.7977.82 (2026-10-23)
        // puts a 10px block with `position: relative; top: 20px` beside a 100px cell at 65 -- 45 by its own height,
        // then the shift -- and at 110 with `vertical-align: bottom`; the shift read as content had them at 55 and 90.
        let ifc = self.is_ifc(index);
        let basis = inputs.known_dimensions_are_definite.height.then_some(h - b.top - b.bottom - p.top - p.bottom);
        let mut reach = b.top + p.top;
        for child in self.children_of(index) {
            if matches!(self.css_position(child), crate::layout::style::CssPosition::Absolute | crate::layout::style::CssPosition::Fixed) {
                continue;
            }
            let shift = if ifc { 0.0 } else { self.flow_shift_y(child, basis) };
            let l = self.unrounded_of(child);
            reach = reach.max(l.location.y - shift + l.size.height + l.margin.bottom);
        }
        let content = measured.max(reach + p.bottom + b.bottom);
        let dy = match valign {
            VAlign::Middle => lu((h - content) / 2.0),
            _ => h - content,
        };
        if dy > 0.0 {
            self.shift_children(index, dy);
        }
    }

    /// How far down a cell's block layout moved its child `s` from its place in the flow by its `top`, else minus its
    /// `bottom`: a `position: relative` child's, and a `sticky` one's, which reaches taffy as `relative` (its sticky
    /// offset is the geometry's). Resolved as taffy resolved it, against `basis`, the cell's content height when that
    /// is definite: a percentage or a `calc()` with one is `auto` without it. (`place_at_static_positions` has its own
    /// rule for the same shift, the parent's `height` standing in for what its layout knew; here the cell's own input
    /// says.)
    fn flow_shift_y(&self, s: u32, basis: Option<f32>) -> f32 {
        use crate::layout::style::CssPosition;
        if !matches!(self.css_position(s), CssPosition::Relative | CssPosition::Sticky) {
            return 0.0;
        }
        let inset = self.style_of_box(s).inset;
        let side = |v: taffy::style::LengthPercentageAuto| {
            let raw = v.into_raw();
            match basis {
                None if raw.tag() == taffy::style::CompactLength::PERCENT_TAG || raw.is_calc() => None,
                _ => crate::layout::resolved::resolve_inset(v, basis.unwrap_or(0.0)),
            }
        };
        match (side(inset.top), side(inset.bottom)) {
            (Some(t), _) => t,
            (None, Some(b)) => -b,
            (None, None) => 0.0,
        }
    }

    /// The height captions on one side take, measuring rather than laying them out.
    fn captions_height(&mut self, captions: &[u32], width: f32, bottom: bool) -> f32 {
        let mut total = 0.0;
        for &cap in captions {
            if self.table_info(cap).caption_bottom != bottom {
                continue;
            }
            let m = self.style_of_box(cap).margin;
            let (ml, mr, mt, mb) = (
                crate::layout::tree::used_auto(m.left),
                crate::layout::tree::used_auto(m.right),
                crate::layout::tree::used_auto(m.top),
                crate::layout::tree::used_auto(m.bottom),
            );
            let w = (width - ml - mr).max(0.0);
            let n = taffy::NodeId::from(cap as usize);
            let h = LayoutPartialTree::compute_child_layout(self, n, self.probe_input(AvailableSpace::Definite(w), Size { width: Some(w), height: None }, RunMode::ComputeSize)).size.height;
            total += mt + h + mb;
        }
        total
    }
}

/// Add `extra` px of a table's own height to its rows, as Chrome 152.0.7977.82 does (2026-10-23, a 100px `<table>`):
/// to the rows of its body groups when it has any (a `<thead>` row of 10 and an empty `<tbody>` row: 10 and 90), else
/// to every row; among those, to the rows without a length height on the row or a cell of it, in proportion to their
/// heights (10 and 30: 25 and 75), equally when they are all empty (a 20px row and an empty one: 20 and 80, two empty
/// ones 50 and 50); with none such, to the others in proportion (20 and 30: 40 and 60). Shares are LayoutUnits of the
/// running total, so they add up to `extra` (10, 20 and 30 take 16.65625, 33.34375 and 50).
fn distribute_rows(row_h: &mut [f32], candidates: &[usize], fixed: &[bool], extra: f32) {
    let auto: Vec<usize> = candidates.iter().copied().filter(|&r| !fixed[r]).collect();
    let pick: Vec<usize> = if auto.is_empty() { candidates.to_vec() } else { auto };
    if pick.is_empty() {
        return;
    }
    let total: f32 = pick.iter().map(|&r| row_h[r]).sum();
    let mut given = 0.0f32;
    let mut so_far = 0.0f32;
    for (k, &r) in pick.iter().enumerate() {
        so_far += if total > 0.0 { row_h[r] } else { 1.0 };
        let whole = if total > 0.0 { total } else { pick.len() as f32 };
        let upto = if k + 1 == pick.len() { extra } else { lu(extra * so_far / whole) };
        row_h[r] += upto - given;
        given = upto;
    }
}

/// The columns' widths for `target` px of them (Blink's auto table layout): fixed columns their width,
/// the auto ones sharing the rest -- beyond their max-content in proportion to it, between min and max
/// in proportion to the difference, at their min-content below that. `fixed_layout` (`table-layout:
/// fixed` with a width) shares what the fixed columns leave equally.
fn distribute(target: f32, min: &[f32], max: &[f32], fixed: &[Option<f32>], fixed_layout: bool) -> Vec<f32> {
    let n = min.len();
    if n == 0 {
        return Vec::new();
    }
    let fixed_sum: f32 = fixed.iter().flatten().sum();
    let auto: Vec<usize> = (0..n).filter(|&c| fixed[c].is_none()).collect();
    let mut out: Vec<f32> = (0..n).map(|c| fixed[c].map_or(0.0, |f| f.max(if fixed_layout { 0.0 } else { min[c] }))).collect();
    let rest = target - out.iter().sum::<f32>();
    if fixed_layout {
        if !auto.is_empty() {
            let share = (rest / auto.len() as f32).max(0.0);
            for &c in &auto {
                out[c] = share;
            }
        }
        return exact(out, target);
    }
    if auto.is_empty() {
        // no auto column: the fixed ones take what is left, in proportion to their widths
        if rest > 0.0 && fixed_sum > 0.0 {
            for c in 0..n {
                out[c] += rest * out[c] / fixed_sum;
            }
        }
        return exact(out, target);
    }
    let amax: f32 = auto.iter().map(|&c| max[c]).sum();
    let amin: f32 = auto.iter().map(|&c| min[c]).sum();
    for &c in &auto {
        out[c] = if rest >= amax {
            if amax > 0.0 { max[c] + (rest - amax) * max[c] / amax } else { rest / auto.len() as f32 }
        } else if rest > amin && amax > amin {
            min[c] + (rest - amin) * (max[c] - min[c]) / (amax - amin)
        } else {
            min[c]
        };
    }
    exact(out, target)
}

/// Float widths as LayoutUnits that add up to `target` exactly: each column ends where the running
/// total, truncated to the 1/64 px grid, ends -- the last at `target` -- as Blink's distribution leaves
/// its remainder to the last column (Chrome 150: `width: 100%` over three columns, 190.828125 for the
/// last where truncating each alone gave 190.8125).
fn exact(widths: Vec<f32>, target: f32) -> Vec<f32> {
    let n = widths.len();
    let total: f32 = widths.iter().sum();
    let end = if total >= target - 1.0 / 128.0 { lu(target.max(0.0)) } else { lu(total) };
    let mut out = Vec::with_capacity(n);
    let (mut cum, mut prev) = (0.0f32, 0.0f32);
    for (i, w) in widths.iter().enumerate() {
        cum += w;
        let at = if i + 1 == n { end } else { lu(cum).min(end) };
        out.push((at - prev).max(0.0));
        prev = at;
    }
    out
}
