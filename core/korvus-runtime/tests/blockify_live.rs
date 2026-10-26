//! An out-of-flow box is **blockified**: its computed `display` is the block-level
//! equivalent, and the inline content on its two sides is still ONE white-space run.
//!
//! # The oracle
//!
//! Every string asserted here is **stock Google Chrome 150.0.7871.46**'s own answer, measured on
//! 2026-09-14 over CDP on a loopback fixture — the binary launched by us with
//! `--remote-debugging-port=0` and attached to with the client's `Origin` header suppressed, never by
//! Playwright's launcher and never with `--remote-allow-origins` added to the browser.
//! The `display` table below is 542 measured cells: nine-and-twenty `display` keywords crossed with
//! `position: static|relative|absolute|fixed|sticky` and `float: none|left|right`. Nothing here is
//! read off CSS Display §2.7, and four of its rows are not what that section's prose would suggest.
//!
//! # What was wrong, and why a character count could not see it
//!
//! P7a closed the *over-emission* half of the corpus's `thin` cluster and took the median character
//! ratio to 1.0024. This is the **fusion** half, which is the opposite failure: korvus computed
//! `display: inline` for a `position: absolute` `<span>`, so the rendered-text collection steps —
//! which branch on computed `display` — put no line break around it and the runs on either side
//! FUSED into one token. `visible<span style=position:absolute>absolute</span>` read
//! `"visibleabsolute"` where Chrome reads `"visible\nabsolute"`: the right number of characters and
//! the wrong words, which is exactly what a ratio measurement is blind to.
//!
//! # The fix is in the CASCADE, not in the consumer
//!
//! `korvus-page`'s `style::compute::blockify` runs at the end of the cascade, so
//! `getComputedStyle(el).display` answers what Chrome answers and every consumer gets the right
//! input. `INLINE_DISPLAYS` in `korvus-webapi`'s `html_element.rs` — the consumer that made the bug
//! visible — was not touched, and `display_drives_the_line_break_with_no_list_of_its_own` is the row
//! that says so: the same `<span>` breaks or does not break purely according to its computed
//! `display`, with no mention of `position` anywhere in the decision.
//!
//! # The one thing the consumer DID have to learn
//!
//! Blockifying correctly exposed a second, separate rule, and without it this unit would have been a
//! net regression on two rows. An in-flow block ENDS the white-space run on both sides of it
//! (`A <div>H</div> B` is `"A\nH\nB"`, both spaces gone). An out-of-flow box does not, because it is
//! not on the line at all — so `A <span style=position:absolute>H</span> B` keeps one space. Before
//! that rule existed, blockification alone turned `A <span abs style=visibility:hidden>H</span> B`
//! from `"A  B"` into `"AB"` — fusing `A` and `B`, the very defect this lane exists to remove. The
//! rule lives in the collection steps, which is where white-space runs are decided, and it reads
//! computed `position`/`float` for that question only. See `TextItem::OutOfFlow`.

use engine::Engine;

/// Evaluate one expression against one page and return the JSON it printed.
async fn eval_on(markup: &str, expr: &str) -> String {
    let html = format!("<!doctype html><html><body>{markup}</body></html>");
    let mut e = Engine::new(persona::Overridden::new().boxed());
    e.run().await.unwrap();
    e.load_html(&html).unwrap();
    e.eval("p7b", format!("print(JSON.stringify({expr}))")).unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    let line = out.last().cloned().unwrap_or_default();
    serde_json::from_str::<String>(line.trim()).unwrap_or_else(|_| panic!("not a JSON string: {line:?}"))
}

/// The computed `display` of `#t` as a CHILD of a box whose `display` is `container`.
///
/// The parent's declaration is the whole measurement here, so it is a parameter rather than the
/// fixed `<div>` [`display_and_float`] wraps everything in.
async fn display_in(container: &str, style: &str) -> String {
    eval_on(
        &format!("<div style=\"display:{container}\"><span id=t style=\"{style}\">x</span></div>"),
        "(function(){var s=getComputedStyle(document.getElementById('t'));\
         return s.display+'|'+s.getPropertyValue('float')})()",
    )
    .await
}

/// `want` is what Chrome 150 answered for this declaration inside this container.
async fn chrome_computes_in(container: &str, style: &str, want: &str) {
    let got = display_in(container, style).await;
    assert_eq!(
        got, want,
        "\n  parent: display:{container}\n  style:  {style}\n  chrome: {want:?}\n  korvus: {got:?}"
    );
}

/// The computed `display` of `#t` in arbitrary markup, for the rows a wrapper cannot express.
async fn chrome_computes_markup(markup: &str, want: &str) {
    let got = eval_on(
        markup,
        "(function(){var s=getComputedStyle(document.getElementById('t'));\
         return s.display+'|'+s.getPropertyValue('float')})()",
    )
    .await;
    assert_eq!(got, want, "\n  markup: {markup}\n  chrome: {want:?}\n  korvus: {got:?}");
}

/// The computed `display` of `#t`, and its computed `float`, as `"display|float"`.
///
/// Both in one page load because they are one measurement: `float` is the other half of what an
/// out-of-flow box does to a computed style, and asserting them apart would let one regress while
/// the other passed.
async fn display_and_float(style: &str) -> String {
    eval_on(
        &format!("<div><span id=t style=\"{style}\">x</span></div>"),
        "(function(){var s=getComputedStyle(document.getElementById('t'));\
         return s.display+'|'+s.getPropertyValue('float')})()",
    )
    .await
}

/// `want` is what Chrome 150 answered for this declaration block.
async fn chrome_computes(style: &str, want: &str) {
    let got = display_and_float(style).await;
    assert_eq!(got, want, "\n  style:  {style}\n  chrome: {want:?}\n  korvus: {got:?}");
}

/// `want` is what Chrome 150 answered for `innerText` on this markup.
async fn chrome_reads(markup: &str, want: &str) {
    let got = eval_on(markup, "document.getElementById('t').innerText").await;
    assert_eq!(got, want, "\n  markup: {markup}\n  chrome: {want:?}\n  korvus: {got:?}");
}

// ---------------------------------------------------------------------------------------------
// THE TABLE
// ---------------------------------------------------------------------------------------------

/// Every inline-level `display`, blockified by `position: absolute`.
///
/// The mapping is not "everything becomes `block`": a flex box stays a flex box and only stops being
/// inline-level, which is why `inline-flex` becomes `flex` and not `block`. Chrome 150, measured.
#[tokio::test]
async fn absolute_positioning_blockifies_every_inline_level_display() {
    chrome_computes("position:absolute", "block|none").await;
    chrome_computes("display:inline;position:absolute", "block|none").await;
    chrome_computes("display:inline-block;position:absolute", "block|none").await;
    chrome_computes("display:inline-flex;position:absolute", "flex|none").await;
    chrome_computes("display:inline-grid;position:absolute", "grid|none").await;
    chrome_computes("display:inline-table;position:absolute", "table|none").await;
}

/// `position: fixed` blockifies exactly as `absolute` does. Measured separately rather than assumed:
/// they are two values of one property and nothing guarantees a browser treats them alike.
#[tokio::test]
async fn fixed_positioning_blockifies_the_same_way() {
    chrome_computes("display:inline;position:fixed", "block|none").await;
    chrome_computes("display:inline-block;position:fixed", "block|none").await;
    chrome_computes("display:inline-flex;position:fixed", "flex|none").await;
    chrome_computes("display:inline-grid;position:fixed", "grid|none").await;
    chrome_computes("display:inline-table;position:fixed", "table|none").await;
}

/// A `float` blockifies too, and — unlike `position` — it SURVIVES on the computed style.
#[tokio::test]
async fn a_float_blockifies_and_keeps_its_own_value() {
    chrome_computes("display:inline;float:left", "block|left").await;
    chrome_computes("display:inline;float:right", "block|right").await;
    chrome_computes("display:inline-block;float:left", "block|left").await;
    chrome_computes("display:inline-flex;float:left", "flex|left").await;
    chrome_computes("display:inline-grid;float:right", "grid|right").await;
    chrome_computes("display:inline-table;float:left", "table|left").await;
}

/// `float` has four values that mean "floated", not two, and all four blockify.
///
/// `inline-start` and `inline-end` are the logical pair, and Chrome 150 computes them through
/// unchanged, blockifies the box for both, and gives all four the same shrink-to-fit width.
#[tokio::test]
async fn the_logical_float_keywords_blockify_too() {
    chrome_computes("display:inline;float:inline-start", "block|inline-start").await;
    chrome_computes("display:inline;float:inline-end", "block|inline-end").await;
    chrome_computes("display:inline-flex;float:inline-start", "flex|inline-start").await;
}

/// **AN INVALID `float` MUST NOT BLOCKIFY**, and the reason this row exists is that it did.
///
/// `Computed::Keyword` lowercases a value and does not validate it, so `float: bogus` survives the
/// cascade here where Chrome drops the declaration. A blockification written as `float != "none"`
/// therefore turned a typo into `display: block`, and `<span style="float:bogus">` measured 600px
/// against Chrome's 27. `is_floated` names the four real values instead.
///
/// The SECOND assertion is the still-open half, pinned rather than hidden: the computed `float`
/// string is still `"bogus"` where Chrome says `"none"`. Closing that means validating keyword
/// values in the cascade, which is a much larger job than this unit and is not it.
#[tokio::test]
async fn an_invalid_float_does_not_blockify() {
    let got = display_and_float("display:inline;float:bogus").await;
    let (display, float) = got.split_once('|').unwrap();
    assert_eq!(
        display, "inline",
        "an unrecognised `float` must leave `display` alone; Chrome 150 answers \"inline\""
    );
    assert_eq!(
        float, "bogus",
        "the cascade still does not validate keyword values; Chrome answers \"none\". \
         If this row now reads \"none\", keyword validation has arrived — delete this assertion."
    );
}

/// THE BRIEF'S FOURTH QUESTION — when both are set, which wins, and is the loser merely ignored?
///
/// Neither. `position` wins, and the losing `float` is **gone from the computed style**: Chrome
/// reports `float: none` on an absolutely positioned box however loudly the author wrote `float:
/// left`. A reader who assumed "the float is ignored at layout time" would have left the computed
/// value alone and `getComputedStyle(el).float` would still be lying.
#[tokio::test]
async fn an_out_of_flow_position_erases_the_float_from_the_computed_style() {
    chrome_computes("display:inline;position:absolute;float:left", "block|none").await;
    chrome_computes("display:inline;position:fixed;float:right", "block|none").await;
    chrome_computes("display:block;position:absolute;float:left", "block|none").await;
    chrome_computes("display:inline-flex;position:absolute;float:left", "flex|none").await;
    // ...and a float on an IN-FLOW positioned box is untouched, which is what makes the row above a
    // rule about being out of flow rather than a rule about `position` being present.
    chrome_computes("display:inline;position:relative;float:left", "block|left").await;
    chrome_computes("display:inline;position:sticky;float:left", "block|left").await;
}

/// THE TRAP THE BRIEF ASKED ABOUT — `relative` and `sticky` are IN flow and blockify NOTHING.
///
/// Both are routinely called "positioned", and `sticky` in particular reads like an out-of-flow
/// thing. Measured, neither changes a computed `display` at all.
#[tokio::test]
async fn relative_and_sticky_are_in_flow_and_blockify_nothing() {
    for position in ["static", "relative", "sticky"] {
        chrome_computes(&format!("display:inline;position:{position}"), "inline|none").await;
        chrome_computes(&format!("display:inline-block;position:{position}"), "inline-block|none").await;
        chrome_computes(&format!("display:inline-flex;position:{position}"), "inline-flex|none").await;
        chrome_computes(&format!("display:inline-table;position:{position}"), "inline-table|none").await;
    }
}

/// The internal table displays blockify to `block` — which CSS Display §2.7's prose does not say,
/// because it is written about inline-level boxes and these are not inline-level.
///
/// This is one of the four rows that would have been got wrong by reading the specification.
#[tokio::test]
async fn the_internal_table_displays_blockify_to_block() {
    for display in [
        "table-cell", "table-row", "table-row-group", "table-header-group", "table-footer-group",
        "table-column", "table-column-group", "table-caption",
    ] {
        chrome_computes(&format!("display:{display};position:absolute"), "block|none").await;
        chrome_computes(&format!("display:{display};float:left"), "block|left").await;
        // ...and at rest each one keeps itself, so the rows above are blockification and not a
        // wholesale collapse of the table displays.
        chrome_computes(&format!("display:{display}"), &format!("{display}|none")).await;
    }
}

/// What blockification leaves ALONE. Three different reasons, one answer.
///
/// `block`, `flex`, `grid`, `flow-root` and `list-item` are already block-level. `table` is too —
/// `inline-table` becomes `table` but `table` does not go on to become `block`, which is the second
/// row a reader of the prose tends to get wrong. `none` suppresses the box and `contents` replaces it
/// with its children, so in both cases there is no box for `position` to take out of flow at all.
#[tokio::test]
async fn blockification_leaves_block_level_and_boxless_displays_alone() {
    for display in ["block", "flex", "grid", "flow-root", "list-item", "table", "none", "contents"] {
        chrome_computes(&format!("display:{display};position:absolute"), &format!("{display}|none")).await;
        chrome_computes(&format!("display:{display};position:fixed"), &format!("{display}|none")).await;
        chrome_computes(&format!("display:{display};float:left"), &format!("{display}|left")).await;
    }
}

/// `ruby` blockifies to the two-keyword `block ruby`, and `ruby-text` to plain `block`.
///
/// `block ruby` is the only multi-keyword `display` this engine produces, and it is here because it
/// is what Chrome serializes — not because a two-value syntax was being implemented.
#[tokio::test]
async fn ruby_blockifies_to_block_ruby() {
    chrome_computes("display:ruby", "ruby|none").await;
    chrome_computes("display:ruby;position:absolute", "block ruby|none").await;
    chrome_computes("display:ruby;float:left", "block ruby|left").await;
    chrome_computes("display:ruby-text;position:absolute", "block|none").await;
}

/// **THE GAP P7b PINNED, CLOSED (P7c)** — the two-keyword syntax computes to the shortest spelling.
///
/// P7b left this row asserting that `display: inline flex` computed `"inline flex"`, with the note
/// "if this row now reads `inline-flex`, the serialisation defect has been fixed — delete this
/// assertion rather than updating it." It is deleted; this is the table that replaces it.
///
/// CSS Display Level 3 writes `display` as `<display-outside> || <display-inside> || list-item`, so
/// `block flex`, `flex block` and `flex` are three spellings of one value and the computed value is
/// the shortest of them. 1,110 combinations were measured on Chrome 150 and it accepts 79; every
/// accepted one is asserted here or in the rows below.
#[tokio::test]
async fn the_two_keyword_display_syntax_computes_to_the_shortest_spelling() {
    for (written, computed) in [
        ("block flow", "block"),
        ("block flow-root", "flow-root"),
        ("block flex", "flex"),
        ("block grid", "grid"),
        ("block table", "table"),
        ("block ruby", "block ruby"),
        ("inline flow", "inline"),
        ("inline flow-root", "inline-block"),
        ("inline flex", "inline-flex"),
        ("inline grid", "inline-grid"),
        ("inline table", "inline-table"),
        ("inline ruby", "ruby"),
    ] {
        chrome_computes(&format!("display:{written}"), &format!("{computed}|none")).await;
    }
}

/// **THE ORDER CARRIES NO INFORMATION**, and that is measured rather than assumed.
///
/// `||` in the grammar means "one or more, in any order", so `flex inline` is `inline flex`. Chrome
/// accepts every permutation and computes them identically — but a REPEATED category is rejected,
/// which is why the parser tracks the three slots separately instead of counting keywords.
#[tokio::test]
async fn the_display_keywords_are_a_set_and_not_a_sequence() {
    chrome_computes("display:flex block", "flex|none").await;
    chrome_computes("display:flex inline", "inline-flex|none").await;
    chrome_computes("display:flow inline", "inline|none").await;
    chrome_computes("display:flow-root block", "flow-root|none").await;
    chrome_computes("display:flow-root inline", "inline-block|none").await;
    chrome_computes("display:grid inline", "inline-grid|none").await;
    chrome_computes("display:ruby block", "block ruby|none").await;
    chrome_computes("display:ruby inline", "ruby|none").await;
    chrome_computes("display:table inline", "inline-table|none").await;
    chrome_computes("display:list-item inline", "inline list-item|none").await;
}

/// `list-item` combines with `flow` and `flow-root` only, and three of its four forms stay
/// multi-keyword because no shorter spelling exists.
///
/// The three-keyword rows are the ones a two-keyword-shaped implementation gets wrong.
#[tokio::test]
async fn list_item_keeps_the_keywords_that_have_no_shorter_spelling() {
    for (written, computed) in [
        ("list-item", "list-item"),
        ("block list-item", "list-item"),
        ("flow list-item", "list-item"),
        ("inline list-item", "inline list-item"),
        ("list-item flow-root", "flow-root list-item"),
        ("block flow list-item", "list-item"),
        ("block flow-root list-item", "flow-root list-item"),
        ("inline flow list-item", "inline list-item"),
        ("inline flow-root list-item", "inline flow-root list-item"),
        ("list-item inline flow-root", "inline flow-root list-item"),
        ("flow-root list-item inline", "inline flow-root list-item"),
    ] {
        chrome_computes(&format!("display:{written}"), &format!("{computed}|none")).await;
    }
}

/// The two answers the prose would have got wrong, asserted as themselves.
///
/// `block flow-root` is **`flow-root`**, not `block` — the single keyword exists and it is shorter,
/// so a normaliser that collapsed "block + anything" to `block` would silently destroy a block
/// formatting context. And **`ruby` alone means `inline ruby`**: every other `<display-inside>`
/// used alone takes the `block` default, and this one does not.
#[tokio::test]
async fn two_rows_of_the_normalisation_are_not_what_the_prose_suggests() {
    chrome_computes("display:block flow-root", "flow-root|none").await;
    chrome_computes("display:flow-root", "flow-root|none").await;
    // ...and the contrast that makes it a rule: `block flow` really is `block`.
    chrome_computes("display:block flow", "block|none").await;
    // `ruby` alone == `inline ruby` != `block ruby`.
    chrome_computes("display:ruby", "ruby|none").await;
    chrome_computes("display:inline ruby", "ruby|none").await;
    chrome_computes("display:block ruby", "block ruby|none").await;
    // ...while every other inside-only spelling takes the `block` default.
    chrome_computes("display:flex", "flex|none").await;
    chrome_computes("display:grid", "grid|none").await;
    chrome_computes("display:table", "table|none").await;
}

/// **`display: flow` is accepted and computes to `block`** — the row a badly built scan called
/// rejected.
///
/// The acceptance scan behind this table writes `display: <sentinel>; display: <value>` and asks
/// what came back: if the sentinel survives, the declaration was dropped. With `block` as the
/// sentinel it cannot tell "rejected" from "accepted and computes to `block`", which is exactly
/// what these three values are — and a special case had been written into the normaliser for the
/// bare `flow` before the scan was re-run with a sentinel no combination of these keywords can
/// produce. Three of 1,110 were wrong, and all three are here.
#[tokio::test]
async fn the_bare_flow_keyword_is_accepted_and_is_block() {
    chrome_computes("display:flow", "block|none").await;
    chrome_computes("display:block flow", "block|none").await;
    chrome_computes("display:flow block", "block|none").await;
    // ...and a REPEATED category is genuinely rejected, which is what the sentinel now proves
    // rather than assumes: `flow flow` leaves the span at its UA `inline`, while `flow` does not.
    chrome_computes("display:flow;position:absolute", "block|none").await;
}

/// Normalisation feeds blockification, and the blockified column still matches Chrome.
///
/// These are the rows P7b asserted through the un-normalised spellings. They have to keep passing
/// through the canonical ones, which is the whole point of normalising in the cascade rather than
/// at a consumer.
#[tokio::test]
async fn a_normalised_display_still_blockifies_to_the_same_answer() {
    chrome_computes("display:inline flex;position:absolute", "flex|none").await;
    chrome_computes("display:inline grid;position:absolute", "grid|none").await;
    chrome_computes("display:inline flow-root;position:absolute", "block|none").await;
    chrome_computes("display:block flex;position:absolute", "flex|none").await;
    chrome_computes("display:inline list-item;position:absolute", "list-item|none").await;
    chrome_computes("display:inline flow-root list-item;float:left", "flow-root list-item|left").await;
    chrome_computes("display:inline ruby;position:absolute", "block ruby|none").await;
    chrome_computes("display:flow-root list-item;position:absolute", "flow-root list-item|none").await;
}

/// **A GAP THIS UNIT DID NOT CLOSE, PINNED SO IT STAYS VISIBLE** — normalising is not validating.
///
/// Chrome DROPS a `display` declaration it does not accept, leaving the element at its previous
/// value: `display: run-in`, `display: ruby-base`, `display: block block` and `display: flex
/// list-item` all compute `inline` on a `<span>` because the declaration never lands. This engine
/// validates no keyword value anywhere — the same hole `an_invalid_float_does_not_blockify` pins
/// for `float: bogus` — so it carries the text through instead. Normalising what Chrome accepts
/// without learning to reject what it does not is the narrow half of that job; the wide half is a
/// cascade-level change.
///
/// These assertions are korvus's CURRENT answers, not Chrome's. If one starts failing because it
/// now reads `inline`, keyword validation has arrived — delete this test rather than updating it.
#[tokio::test]
async fn an_unaccepted_display_value_is_carried_through_rather_than_dropped() {
    for written in ["run-in", "ruby-base", "block block", "flex grid", "flex list-item", "ruby ruby"] {
        let got = display_and_float(&format!("display:{written}")).await;
        assert_eq!(
            got,
            format!("{written}|none"),
            "the cascade still does not validate keyword values; Chrome drops \
             `display: {written}` and answers \"inline\". If this row now reads \"inline\", \
             keyword validation has arrived — delete this test."
        );
    }
}

// ---------------------------------------------------------------------------------------------
// THE SECOND TRIGGER (P7c): the PARENT's formatting context
// ---------------------------------------------------------------------------------------------

/// A child of a flex or grid container is an ITEM, and Chrome blockifies it by the SAME table.
///
/// All four container displays, every `display` keyword: 116 cells measured on Chrome 150, and each
/// one is the answer the out-of-flow trigger already gives. That is the finding — there is one map,
/// not two — and it is asserted across all four containers rather than on `flex` alone, because
/// "`inline-flex` is an inline-level box so perhaps it does not blockify its items" is exactly the
/// kind of guess this lane exists to replace with a measurement. It does.
#[tokio::test]
async fn a_flex_or_grid_item_is_blockified_by_the_same_table() {
    for container in ["flex", "inline-flex", "grid", "inline-grid"] {
        // The inline-level displays, blockified.
        chrome_computes_in(container, "display:inline", "block|none").await;
        chrome_computes_in(container, "display:inline-block", "block|none").await;
        chrome_computes_in(container, "display:inline-flex", "flex|none").await;
        chrome_computes_in(container, "display:inline-grid", "grid|none").await;
        chrome_computes_in(container, "display:inline-table", "table|none").await;
        // The internal table displays, which blockify here for the same reason they do out of flow.
        for display in [
            "table-cell", "table-row", "table-row-group", "table-header-group",
            "table-footer-group", "table-column", "table-column-group", "table-caption",
        ] {
            chrome_computes_in(container, &format!("display:{display}"), "block|none").await;
        }
        // `ruby` takes the two-keyword answer here too, and `ruby-text` plain `block`.
        chrome_computes_in(container, "display:ruby", "block ruby|none").await;
        chrome_computes_in(container, "display:ruby-text", "block|none").await;
        // ...and what the table leaves alone is left alone here as well. `contents` and `none` are
        // on this list for the same reason as out of flow: there is no box to make an item of.
        for display in ["block", "flex", "grid", "flow-root", "list-item", "table", "none", "contents"] {
            chrome_computes_in(container, &format!("display:{display}"), &format!("{display}|none")).await;
        }
    }
}

/// **A `<span>` with NO declarations at all**, which is the row the live corpus actually turns on.
///
/// Nothing on the element says anything; the parent's `display` decides the whole answer. This is
/// why blockification cannot be a function of the element's own declarations.
#[tokio::test]
async fn an_undeclared_child_blockifies_on_its_parents_display_alone() {
    chrome_computes_markup("<div style=\"display:flex\"><span id=t>x</span></div>", "block|none").await;
    chrome_computes_markup("<div style=\"display:grid\"><span id=t>x</span></div>", "block|none").await;
    // The control: the same span under everything that is NOT a flex or grid container.
    for container in ["block", "inline", "inline-block", "flow-root", "table", "table-cell", "list-item", "ruby"] {
        chrome_computes_in(container, "", "inline|none").await;
    }
    // A `<li>`, whose UA `display` is already block-level, is untouched by being an item.
    chrome_computes_markup("<div style=\"display:flex\"><li id=t>x</li></div>", "list-item|none").await;
}

/// **`display: contents` FORWARDS the context and `display: none` STOPS it.**
///
/// The brief's third question, and the reason the trigger is threaded down the tree rather than read
/// off the parent's computed `display`. A `contents` box generates no box of its own, so ITS
/// children are the flex container's items — and the `contents` box itself still computes
/// `contents`, so a rule that looked one level up would answer "my parent is not a flex container"
/// and get the grandchild wrong. Measured; two levels of `contents` forward it just the same.
#[tokio::test]
async fn display_contents_forwards_the_item_context_and_display_none_stops_it() {
    chrome_computes_markup(
        "<div style=\"display:flex\"><div style=\"display:contents\"><span id=t>x</span></div></div>",
        "block|none",
    )
    .await;
    chrome_computes_markup(
        "<div style=\"display:grid\"><div style=\"display:contents\"><span id=t>x</span></div></div>",
        "block|none",
    )
    .await;
    chrome_computes_markup(
        "<div style=\"display:flex\"><div style=\"display:contents\"><div style=\"display:contents\">\
         <span id=t>x</span></div></div></div>",
        "block|none",
    )
    .await;
    // `display: none` is the other direction: the subtree is not laid out at all, and the span
    // below it computes `inline`.
    chrome_computes_markup(
        "<div style=\"display:flex\"><div style=\"display:none\"><span id=t>x</span></div></div>",
        "inline|none",
    )
    .await;
    // ...and a `contents` box NOT under a flex container forwards the ordinary context, so its
    // child is left alone. Without this row, "contents always blockifies its children" would pass.
    chrome_computes_markup(
        "<div style=\"display:block\"><div style=\"display:contents\"><span id=t>x</span></div></div>",
        "inline|none",
    )
    .await;
}

/// The rule reaches exactly ONE level, and the container's own blockification feeds it.
#[tokio::test]
async fn the_item_context_reaches_one_level_and_follows_the_computed_display() {
    // A grandchild of a flex container is a child of an ITEM, not of the container.
    chrome_computes_markup(
        "<div style=\"display:flex\"><div><span id=t>x</span></div></div>",
        "inline|none",
    )
    .await;
    // The container's OWN `display` is the blockified one: `inline-flex` out of flow computes
    // `flex`, and it is still a flex container, so its child is still an item.
    chrome_computes_markup(
        "<div style=\"display:inline-flex;position:absolute\"><span id=t>x</span></div>",
        "block|none",
    )
    .await;
    // A float on the container does the same.
    chrome_computes_markup(
        "<div style=\"display:inline-grid;float:left\"><span id=t>x</span></div>",
        "block|none",
    )
    .await;
}

/// **`float` SURVIVES on a flex item**, where `position: absolute` erases it.
///
/// P7b measured that an out-of-flow `position` takes the `float` off the computed style entirely.
/// Being an item is NOT that: it blockifies and touches nothing else, so `float: left` on a flex
/// item still reports `left`. Asserted because the obvious generalisation — "blockification erases
/// the float" — is wrong, and the two triggers had to be kept apart in the code because of it.
#[tokio::test]
async fn being_a_flex_item_blockifies_but_does_not_erase_the_float() {
    chrome_computes_in("flex", "float:left", "block|left").await;
    chrome_computes_in("grid", "float:right", "block|right").await;
    chrome_computes_in("flex", "display:inline-flex;float:right", "flex|right").await;
    // ...and the erasure still happens when `position` is what takes the box out of flow, inside a
    // flex container exactly as outside one.
    chrome_computes_in("flex", "position:absolute;float:left", "block|none").await;
}

/// An absolutely positioned child of a flex container is **not a flex item** — and it does not
/// matter, because the two maps are the same one.
///
/// The brief's second question. If the flex-item map and the out-of-flow map differed, this row
/// would be where the difference showed; measured cell by cell, it does not. `contents` and `none`
/// are the interesting cells, since those are the two the table leaves alone for both triggers.
#[tokio::test]
async fn an_abspos_child_of_a_flex_container_lands_on_the_same_map() {
    for (display, want) in [
        ("inline", "block|none"),
        ("inline-block", "block|none"),
        ("inline-flex", "flex|none"),
        ("inline-table", "table|none"),
        ("table-cell", "block|none"),
        ("ruby", "block ruby|none"),
        ("contents", "contents|none"),
        ("none", "none|none"),
    ] {
        chrome_computes_in("flex", &format!("display:{display};position:absolute"), want).await;
    }
}

/// **ONLY flex and grid containers do this.** The control, across every other container display.
///
/// Without it, "blockify every child of every box" would pass every row above. `ruby` is the one
/// container with a rule of its own and it goes the OTHER way — it *inlinifies* a `block` child to
/// `inline-block` — which is measured here as the still-open gap it is rather than left to look
/// like agreement.
#[tokio::test]
async fn no_other_container_display_blockifies_its_children() {
    for container in ["block", "inline", "inline-block", "flow-root", "table", "table-cell",
                      "table-row", "list-item", "contents"] {
        chrome_computes_in(container, "display:inline", "inline|none").await;
        chrome_computes_in(container, "display:inline-block", "inline-block|none").await;
        chrome_computes_in(container, "display:inline-flex", "inline-flex|none").await;
        chrome_computes_in(container, "display:table-cell", "table-cell|none").await;
        chrome_computes_in(container, "display:ruby", "ruby|none").await;
    }
    // A `ruby` container leaves the inline-level displays alone too...
    chrome_computes_in("ruby", "display:inline", "inline|none").await;
    chrome_computes_in("ruby", "display:inline-flex", "inline-flex|none").await;
}

/// **A GAP THIS UNIT DID NOT CLOSE, PINNED SO IT STAYS VISIBLE** — `ruby` INLINIFIES a block child.
///
/// The opposite direction to everything above: Chrome 150 computes `display: block` on a child of a
/// `display: ruby` box as `inline-block`, and `block flex` as `inline-flex`. That is *inlinification*
/// — a third rule, with its own map — and it is deliberately not implemented here: it is two cells
/// of the 3,145 measured, it needs its own table, and guessing that table from these two would be
/// exactly the mistake this lane keeps finding in briefs.
///
/// This test asserts what korvus answers TODAY so the gap is visible rather than silent. If a row
/// here starts failing because it now reads `inline-block`, ruby inlinification has arrived — delete
/// this test rather than updating it.
#[tokio::test]
async fn ruby_inlinifies_a_block_child_and_this_engine_does_not() {
    let got = display_in("ruby", "display:block").await;
    assert_eq!(
        got, "block|none",
        "a `ruby` container INLINIFIES a block child; Chrome 150 answers \"inline-block\". \
         If this row now reads \"inline-block\", ruby inlinification has arrived — delete this test."
    );
}

// ---------------------------------------------------------------------------------------------
// WHAT THE TABLE BUYS: innerText
// ---------------------------------------------------------------------------------------------

/// THE UNIT, in one row: the fusion is gone.
///
/// This is `textprobe.py`'s `abs_span`, `fixed_span`, `float_span` and `sronly_span` — four of its
/// thirty-one rules, and the four P7a ranked first of the ten it left.
#[tokio::test]
async fn an_out_of_flow_span_no_longer_fuses_the_runs_around_it() {
    chrome_reads("<div id=t>visible<span style=\"position:absolute;left:-9999px\">absolute</span></div>", "visible\nabsolute").await;
    chrome_reads("<div id=t>visible<span style=\"position:fixed;top:0\">fixedpos</span></div>", "visible\nfixedpos").await;
    chrome_reads("<div id=t>visible<span style=\"float:left\">floated</span></div>", "visible\nfloated").await;
    // `sronly_span`: the skip-link idiom, which is on a large share of the corpus by itself.
    chrome_reads(
        "<div id=t>visible<span style=\"position:absolute;width:1px;height:1px;overflow:hidden;\
         clip:rect(0 0 0 0);clip-path:inset(50%);white-space:nowrap\">skip to navigation</span></div>",
        "visible\nskip to navigation",
    )
    .await;
    // With a run on BOTH sides, and nested inside an inline that does not break.
    chrome_reads("<div id=t>visible<span style=\"position:absolute\">absolute</span>tail</div>", "visible\nabsolute\ntail").await;
    chrome_reads("<div id=t>A<span>pre<span style=\"position:absolute\">H</span>post</span>B</div>", "Apre\nH\npostB").await;
    // An out-of-flow box whose display is an atomic inline breaks too, because it is blockified.
    chrome_reads("<div id=t>visible<span style=\"position:absolute;display:inline-block\">ib</span>tail</div>", "visible\nib\ntail").await;
    chrome_reads("<div id=t>visible<span style=\"position:absolute;display:inline-flex\">ifx</span>tail</div>", "visible\nifx\ntail").await;
}

/// **THE P7c UNIT, in one row: the flex-item fusion is gone.**
///
/// `<div style=display:flex><span>Tools</span><span>Solutions</span><span>Pricing</span></div>` read
/// `"ToolsSolutionsPricing"` here and `"Tools\nSolutions\nPricing"` in Chrome — the right number of
/// characters and the wrong words, on the navigation bar of a large share of the live corpus. Not
/// one character of the rendered-text collection steps changed to fix it: the three `<span>`s now
/// compute `display: block`, and the consumer that already branches on computed `display` does the
/// rest. `INLINE_DISPLAYS` was not touched.
#[tokio::test]
async fn a_flex_item_no_longer_fuses_with_its_siblings() {
    chrome_reads(
        "<div id=t style=\"display:flex\"><span>Tools</span><span>Solutions</span><span>Pricing</span></div>",
        "Tools\nSolutions\nPricing",
    )
    .await;
    // All four container displays, because all four blockify their items.
    chrome_reads(
        "<div id=t style=\"display:inline-flex\"><span>Tools</span><span>Solutions</span><span>Pricing</span></div>",
        "Tools\nSolutions\nPricing",
    )
    .await;
    chrome_reads(
        "<div id=t style=\"display:grid\"><span>Tools</span><span>Solutions</span><span>Pricing</span></div>",
        "Tools\nSolutions\nPricing",
    )
    .await;
    chrome_reads("<div id=t style=\"display:inline-grid\"><span>a</span><span>b</span></div>", "a\nb").await;
    // The live signature: a nav bar of anchors in a flex row.
    chrome_reads(
        "<div id=t style=\"display:flex\"><a href=\"#\">Home</a><a href=\"#\">About</a></div>",
        "Home\nAbout",
    )
    .await;
    // Through a `display: contents` box, which is how a component library emits a list.
    chrome_reads(
        "<div id=t style=\"display:flex\"><div style=\"display:contents\"><span>A</span><span>B</span></div></div>",
        "A\nB",
    )
    .await;
    // An inline INSIDE an item is still an inline: the break is between items, not everywhere.
    chrome_reads(
        "<div id=t style=\"display:flex\"><span>A<span>B</span></span><span>C</span></div>",
        "AB\nC",
    )
    .await;
    // White space BETWEEN two items is gone, because each item is now a block and a block ends the
    // white-space run on both sides. That falls out of the computed `display`; it is not a rule.
    chrome_reads(
        "<div id=t style=\"display:flex\"><span>A</span> <span>B</span></div>",
        "A\nB",
    )
    .await;
    // A `display: none` item contributes nothing and does not fuse its neighbours either.
    chrome_reads(
        "<div id=t style=\"display:flex\"><span>A</span><span style=\"display:none\">X</span><span>B</span></div>",
        "A\nB",
    )
    .await;
    // THE CONTROL: the same three spans in a BLOCK parent still fuse, and must. Without this row,
    // "break between every pair of spans" would pass everything above.
    chrome_reads(
        "<div id=t><span>Tools</span><span>Solutions</span><span>Pricing</span></div>",
        "ToolsSolutionsPricing",
    )
    .await;
}

/// The other side of the same rule: an IN-FLOW span still fuses, and must.
///
/// Without this row, "blockify everything positioned" would pass the test above.
#[tokio::test]
async fn an_in_flow_span_still_does_not_break() {
    chrome_reads("<div id=t>visible<span style=\"position:relative\">rel</span>tail</div>", "visiblereltail").await;
    chrome_reads("<div id=t>visible<span style=\"position:sticky;top:0\">sticky</span>tail</div>", "visiblestickytail").await;
    chrome_reads("<div id=t>visible<span>inline</span>tail</div>", "visibleinlinetail").await;
    chrome_reads("<div id=t>visible<span style=\"display:inline-block\">ib</span>tail</div>", "visibleibtail").await;
}

/// THE INVARIANT THE BRIEF NAMED: the consumer knows about `display` and nothing else.
///
/// The same `<span>`, taken out of flow three different ways and left in flow two, breaks the line
/// exactly when its computed `display` is block-level. Were the line break decided by consulting
/// `position` directly — "blockify in `innerText`" — this row would still pass while
/// `getComputedStyle` went on reporting `inline`; what makes it meaningful is that it is asserted
/// beside the `display` table above, which pins the computed value itself.
#[tokio::test]
async fn display_drives_the_line_break_with_no_list_of_its_own() {
    for (style, breaks) in [
        ("position:absolute", true),
        ("position:fixed", true),
        ("float:left", true),
        ("position:relative", false),
        ("position:sticky", false),
        ("", false),
    ] {
        let want = if breaks { "A\nH\nB" } else { "AHB" };
        chrome_reads(&format!("<div id=t>A<span style=\"{style}\">H</span>B</div>"), want).await;
        // ...and it is the computed `display` that says which, in the same direction.
        let computed = display_and_float(style).await;
        let is_block = !computed.starts_with("inline");
        assert_eq!(is_block, breaks, "style {style:?} computed {computed:?} but breaks={breaks}");
    }
}

// ---------------------------------------------------------------------------------------------
// THE SECOND RULE: an out-of-flow box does not interrupt the white-space run
// ---------------------------------------------------------------------------------------------

/// An in-flow block ends the white-space run on both sides; an out-of-flow box does not.
///
/// Measured side by side on one page, because the pair is the rule: `A <div>H</div> B` is `"A\nH\nB"`
/// and `A <span style=position:absolute>H</span> B` is `"A \nH\nB"` — one space, and it survives.
#[tokio::test]
async fn an_out_of_flow_box_does_not_end_the_white_space_run() {
    // The control: in flow, both spaces go. `relative` and `sticky` are in flow and behave alike.
    chrome_reads("<div id=t>A <div>H</div> B</div>", "A\nH\nB").await;
    chrome_reads("<div id=t>A <div style=\"position:relative\">H</div> B</div>", "A\nH\nB").await;
    chrome_reads("<div id=t>A <div style=\"position:sticky;top:0\">H</div> B</div>", "A\nH\nB").await;
    // Out of flow: one space survives, whichever way the box is taken out of flow.
    chrome_reads("<div id=t>A <span style=\"position:absolute\">H</span> B</div>", "A \nH\nB").await;
    chrome_reads("<div id=t>A <div style=\"position:absolute\">H</div> B</div>", "A \nH\nB").await;
    chrome_reads("<div id=t>A <div style=\"position:fixed\">H</div> B</div>", "A \nH\nB").await;
    chrome_reads("<div id=t>A <span style=\"float:left\">H</span> B</div>", "A \nH\nB").await;
    chrome_reads("<div id=t>A <span style=\"position:absolute;display:inline-block\">H</span> B</div>", "A \nH\nB").await;
}

/// WHERE the surviving space lands, which is the giveaway that the two sides are ONE run.
///
/// The run `"A "` + `" B"` collapses to a single space, and Chrome puts it on the side the run's
/// FIRST space was on. So the space moves with the markup, and a rule that simply "kept a space"
/// would get three of these four wrong.
#[tokio::test]
async fn the_surviving_space_lands_where_the_run_first_had_one() {
    // Space on both sides: one space, before the break.
    chrome_reads("<div id=t>A <span style=\"position:absolute\">H</span> B</div>", "A \nH\nB").await;
    // Space only before: same place.
    chrome_reads("<div id=t>A <span style=\"position:absolute\">H</span>B</div>", "A \nH\nB").await;
    // Space only AFTER: it lands after the break instead.
    chrome_reads("<div id=t>A<span style=\"position:absolute\">H</span> B</div>", "A\nH\n B").await;
    // No space either side: none appears.
    chrome_reads("<div id=t>A<span style=\"position:absolute\">H</span>B</div>", "A\nH\nB").await;
    // Two out-of-flow boxes in a row still share the run's ONE space, before the FIRST of them.
    chrome_reads(
        "<div id=t>A <span style=\"position:absolute\">H</span> <span style=\"position:absolute\">I</span> B</div>",
        "A \nH\nI\nB",
    )
    .await;
    // At the very end of the run the space is TRIMMED, exactly as a trailing space always is — which
    // is why it cannot simply be written out when the box is opened.
    chrome_reads("<div id=t>A <span style=\"position:absolute\">H</span></div>", "A\nH").await;
    // ...and at the very start, likewise.
    chrome_reads("<div id=t><span style=\"position:absolute\">H</span> B</div>", "H\nB").await;
}

/// THE REGRESSION THIS RULE EXISTS TO PREVENT, asserted as itself.
///
/// A hidden or content-skipped out-of-flow box contributes no text and no line break — `Gap(0)`. With
/// blockification but without the white-space rule, its gap swallowed the space that separated the
/// two runs and `"A B"` became `"AB"`: two tokens fused into one, which is the exact defect this lane
/// exists to remove. Chrome keeps the space.
#[tokio::test]
async fn a_hidden_out_of_flow_box_does_not_fuse_the_runs_around_it() {
    chrome_reads("<div id=t>A <span style=\"visibility:hidden;position:absolute\">H</span> B</div>", "A B").await;
    chrome_reads("<div id=t>A <span style=\"content-visibility:hidden;position:absolute\">H</span> B</div>", "A B").await;
    chrome_reads("<div id=t>A <span style=\"content-visibility:hidden;float:left\">H</span> B</div>", "A B").await;
    chrome_reads("<div id=t>A <span style=\"display:none;position:absolute\">H</span> B</div>", "A B").await;
}

/// P7a's three rules still answer what P7a measured, on boxes that are now blockified.
///
/// The brief's third question: this unit changes the INPUT to that code, so its answers had to be
/// re-measured rather than assumed. `content-visibility` is the interesting one — it is inert on a
/// non-atomic inline, so blockifying a `position: absolute` span makes containment START applying to
/// it. That is Chrome's behaviour too, and it falls out of the blockified `display` with no new list.
#[tokio::test]
async fn p7as_rules_still_hold_on_a_blockified_box() {
    // content-visibility was INERT on the inline and APPLIES once the box is out of flow...
    chrome_reads("<div id=t>A <span style=\"content-visibility:hidden\">H</span> B</div>", "A H B").await;
    chrome_reads("<div id=t>A <span style=\"content-visibility:hidden;position:absolute\">H</span> B</div>", "A B").await;
    // ...except on `display:contents`, which blockification leaves alone, so containment stays inert
    // even out of flow. One list, `NO_CONTAINMENT`, gets both right because its input got right.
    chrome_reads("<div id=t>A <span style=\"content-visibility:hidden;display:contents;position:absolute\">H</span> B</div>", "A H B").await;
    // A `visibility:visible` descendant of a hidden out-of-flow box still comes back.
    chrome_reads("<div id=t>A<span style=\"visibility:hidden;position:absolute\">P<span style=\"visibility:visible\">C</span></span>B</div>", "ACB").await;
    // `display:none` and `display:contents` are transparent whether or not they are positioned.
    chrome_reads("<div id=t>A<span style=\"display:contents;position:absolute\">H</span>B</div>", "AHB").await;
    chrome_reads("<div id=t>A<span style=\"display:none;position:absolute\">H</span>B</div>", "AB").await;
    // A `<p>` carries TWO required breaks out of flow just as it does in flow.
    chrome_reads("<div id=t>A <p style=\"position:absolute\">H</p> B</div>", "A \n\nH\n\nB").await;
    // An out-of-flow box BETWEEN two blocks adds one line, not two.
    chrome_reads("<div id=t><div>X</div> <span style=\"position:absolute\">H</span> <div>Y</div></div>", "X\nH\nY").await;
}
