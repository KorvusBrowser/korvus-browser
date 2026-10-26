use super::*;

/// `sk_relax`: a strike's scale is the transform's rounded half up to 1/1024 -- `scale(0.384)` outlines at
/// 393/1024 -- and an exact scale stays exact.
#[test]
fn a_strike_scale_is_relaxed_to_1024ths() {
    assert_eq!(relax(0.384), 393.0 / 1024.0);
    assert_eq!(relax(1.0), 1.0);
    assert_eq!(relax(0.4), 410.0 / 1024.0);
    // half a step rounds up, as `floor(x + 0.5)`
    assert_eq!(relax(0.5 / 1024.0), 1.0 / 1024.0);
}

/// The pen draws what the Fontations bridge's does: a contour's move only once it draws, a segment that goes
/// nowhere dropped, a close only after a drawing verb, y flipped.
#[test]
fn the_pen_is_the_fontations_bridges() {
    let mut pen = Pen { verbs: Vec::new(), points: Vec::new(), started: false, current: Point::default() };
    pen.move_to(1.0, 2.0);
    pen.line_to(1.0, 2.0); // nowhere: dropped, and no move yet
    pen.line_to(3.0, 4.0);
    pen.quad_to(5.0, 6.0, 7.0, 8.0);
    pen.close();
    pen.move_to(0.0, 0.0);
    pen.close(); // a contour that never drew closes nothing new
    assert_eq!(pen.verbs, vec![MOVE, LINE, QUAD, CLOSE]);
    assert_eq!(pen.points, vec![Point::new(1.0, -2.0), Point::new(3.0, -4.0), Point::new(5.0, -6.0), Point::new(7.0, -8.0)]);
}

/// An outline face's glyphs come out as paths at the strike's size, slightly hinted to whole pixels vertically; a
/// face with colour tables is not drawn here.
#[test]
fn outline_glyphs_and_colour_faces() {
    let mgr = skia_safe::FontMgr::new();
    let Some(tf) = mgr.match_family_style("Liberation Sans", skia_safe::FontStyle::normal()) else { return };
    let Some((data, index)) = tf.to_font_data() else { return };
    let gid = tf.unichar_to_glyph('H' as i32);
    let paths = run_paths(&data, index as u32, 16.0, Strike { hinting: Hinting::Slight, force_autohint: false, mono: false }, &[gid], &[]).expect("an outline");
    let b = paths[0].bounds();
    // light hinting snaps the cap height and the baseline to the pixel grid
    assert_eq!((b.top.fract(), b.bottom.fract()), (0.0, 0.0), "{b:?}");
    assert!(!has_color_tables(&data, index as u32));
    if let Some(emoji) = mgr.match_family_style("Noto Color Emoji", skia_safe::FontStyle::normal()) {
        if let Some((d, i)) = emoji.to_font_data() {
            assert!(has_color_tables(&d, i as u32));
        }
    }
}
