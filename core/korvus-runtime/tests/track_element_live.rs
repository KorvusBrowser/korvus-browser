//! `<track>` owns a `TextTrack`, and a media element's `textTracks` is derived from its children.
//!
//! EVERY expected value below was measured in Chrome 151 headless (`--headless=new --dump-dom`) over a
//! loopback HTTP server on 2026-09-11, not reasoned about. The three probe pages that produced them
//! answer byte-identically in both engines; these are the rows that would have caught the bug.
//!
//! WHY IT MATTERS: `HTMLTrackElement.track` was a D2 stub answering `null`, and that is what made
//! `svelte.dev` render SvelteKit's own 500 page -- its `<track>` binding does `e.track.cues` and threw
//! `TypeError: Cannot read properties of null (reading 'cues')`. Note that it copes with `cues` being
//! NULL perfectly well (it falls back to a `load` listener); only a null `track` broke it.

use engine::Engine;

async fn probe(html: &str, js: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("i", js.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// A `<track>` element has a real `TextTrack` whether or not it is in a document, whether or not it is
/// in a media element, and whether or not its resource was fetched (it never is here).
#[tokio::test]
async fn a_track_element_owns_a_text_track() {
    let out = probe(
        r#"<!doctype html><html><body></body></html>"#,
        r#"
        const t = document.createElement("track");
        print("type=" + typeof t.track);
        print("str=" + String(t.track));
        print("brand=" + (t.track instanceof TextTrack));
        print("stable=" + (t.track === t.track));
        print("mode=" + t.track.mode);
        print("kind=" + t.track.kind);
        print("label=" + JSON.stringify(t.track.label));
        print("language=" + JSON.stringify(t.track.language));
        print("id=" + JSON.stringify(t.track.id));
        print("cues=" + String(t.track.cues));
        print("activeCues=" + String(t.track.activeCues));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "type=object",
            "str=[object TextTrack]",
            "brand=true",
            "stable=true",
            // Chrome: a track with no `default` attribute, selected or not, is `disabled`.
            "mode=disabled",
            // The element's own `kind` missing-value default, read THROUGH the element.
            "kind=subtitles",
            "label=\"\"",
            "language=\"\"",
            "id=\"\"",
            // Chrome: a `disabled` track exposes NO cue list at all.
            "cues=null",
            "activeCues=null",
        ]
    );
}

/// The four string members are the ELEMENT's `kind` / `label` / `srclang` / `id`, live -- not a copy
/// taken when the track was minted. Chrome moves all four after the track object exists.
#[tokio::test]
async fn the_track_reads_its_element_live() {
    let out = probe(
        r#"<!doctype html><html><body></body></html>"#,
        r#"
        const t = document.createElement("track");
        t.label = "before";
        const tt = t.track;
        t.label = "after"; t.srclang = "de"; t.kind = "chapters"; t.id = "newid";
        print("label=" + tt.label);
        print("language=" + tt.language);
        print("kind=" + tt.kind);
        print("id=" + tt.id);
        t.setAttribute("kind", "bogus");
        print("invalid-kind=" + tt.kind);
        "#,
    )
    .await;
    assert_eq!(
        out,
        // The last row is the element's INVALID-value default for `kind`, which is "metadata" while
        // its MISSING-value default is "subtitles" -- the two differ, and the track sees the element's.
        vec!["label=after", "language=de", "kind=chapters", "id=newid", "invalid-kind=metadata"]
    );
}

/// `cues` / `activeCues` follow `mode`, both ways.
#[tokio::test]
async fn cues_appear_and_vanish_with_mode() {
    let out = probe(
        r#"<!doctype html><html><body><video id="v"><track id="t" kind="captions"></video></body></html>"#,
        r#"
        const tt = document.getElementById("t").track;
        print("disabled=" + String(tt.cues));
        tt.mode = "hidden";
        print("hidden=" + String(tt.cues) + " len=" + tt.cues.length);
        print("hidden-active=" + String(tt.activeCues));
        tt.mode = "disabled";
        print("again=" + String(tt.cues));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "disabled=null",
            // Empty because nothing has parsed a WebVTT resource, not because the list is missing.
            "hidden=[object TextTrackCueList] len=0",
            "hidden-active=[object TextTrackCueList]",
            "again=null",
        ]
    );
}

/// `textTracks` is DERIVED: the `<track>` children in tree order, then the `addTextTrack` tracks --
/// Blink's own concatenation, which Chrome shows by putting an element track added SECOND at index 0.
/// Deriving is also what makes `removeChild` drop a track, which a stored array cannot do.
#[tokio::test]
async fn the_media_elements_list_is_derived_from_its_children() {
    let out = probe(
        r#"<!doctype html><html><body><video id="v"><track id="t" kind="captions" label="L1" srclang="en"></video></body></html>"#,
        r#"
        const v = document.getElementById("v"), t = document.getElementById("t");
        print("len=" + v.textTracks.length);
        print("same=" + (v.textTracks[0] === t.track));
        print("list-stable=" + (v.textTracks === v.textTracks));
        print("keys=" + JSON.stringify(Object.keys(v.textTracks)));
        print("past-end=" + String(v.textTracks[5]));
        const added = v.addTextTrack("subtitles", "ADDED", "de");
        print("after-add=" + v.textTracks.length + " " + (v.textTracks[0] === t.track) + " " + (v.textTracks[1] === added));
        print("byId=" + v.textTracks.getTrackById("t").label);
        print("byId-added=" + v.textTracks.getTrackById("").label);
        v.removeChild(t);
        print("after-remove=" + v.textTracks.length + " " + (v.textTracks[0] === added));
        const n = document.createElement("track"); n.kind = "subtitles"; n.label = "N";
        v.appendChild(n);
        print("after-append=" + v.textTracks.length + " " + v.textTracks[0].label);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "len=1",
            "same=true",
            "list-stable=true",
            "keys=[\"0\"]",
            "past-end=undefined",
            "after-add=2 true true",
            "byId=L1",
            "byId-added=ADDED",
            "after-remove=1 true",
            // The element track goes back to index 0, ahead of the added one.
            "after-append=2 N",
        ]
    );
}

/// A `<track>` nested one element deep is NOT the media element's: Chrome counts CHILDREN, not
/// descendants.
#[tokio::test]
async fn only_children_count() {
    let out = probe(
        r#"<!doctype html><html><body><video id="v"><div><track kind="captions"></div></video></body></html>"#,
        r#"print("len=" + document.getElementById("v").textTracks.length);"#,
    )
    .await;
    assert_eq!(out, vec!["len=0"]);
}

/// HTML's automatic text track selection, as Chrome runs it: within a kind GROUP the first
/// `<track default>` wins, `metadata` lands on `hidden` and every other group on `showing`, and two
/// defaults in DIFFERENT groups are both selected.
#[tokio::test]
async fn the_first_default_of_each_group_is_selected() {
    let out = probe(
        r#"<!doctype html><html><body>
        <video id="a"><track id="a1" kind="captions" default><track id="a2" kind="subtitles" default></video>
        <video id="b"><track id="b1" kind="metadata" default></video>
        <video id="c"><track id="c1" kind="chapters" default></video>
        <video id="d"><track id="d1" kind="descriptions" default><track id="d2" kind="captions" default></video>
        <video id="e"><track id="e1" kind="subtitles"></video>
        </body></html>"#,
        r#"
        const m = id => document.getElementById(id).track.mode;
        print("captions-first=" + m("a1"));
        print("subtitles-second-same-group=" + m("a2"));
        print("metadata=" + m("b1"));
        print("chapters=" + m("c1"));
        print("descriptions+captions=" + m("d1") + "," + m("d2"));
        print("no-default=" + m("e1"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "captions-first=showing",
            // subtitles and captions share ONE group, so the second default stays off.
            "subtitles-second-same-group=disabled",
            "metadata=hidden",
            "chapters=showing",
            // Different groups: both selected.
            "descriptions+captions=showing,showing",
            "no-default=disabled",
        ]
    );
}

/// The selection runs at the media element's INSERTION, once. Chrome leaves a `<track default>`
/// appended afterwards `disabled` -- the video's resource selection had already run, over no children.
#[tokio::test]
async fn the_selection_runs_once_at_insertion() {
    let out = probe(
        r#"<!doctype html><html><body></body></html>"#,
        r#"
        const v = document.createElement("video");
        document.body.appendChild(v);
        const t = document.createElement("track"); t.kind = "captions"; t.default = true;
        v.appendChild(t);
        print("appended-after=" + t.track.mode);
        // ...but a video inserted WITH its children already attached does select one.
        const w = document.createElement("video");
        const u = document.createElement("track"); u.kind = "captions"; u.default = true;
        w.appendChild(u);
        document.body.appendChild(w);
        print("inserted-with=" + u.track.mode);
        const orphan = document.createElement("track"); orphan.kind = "captions"; orphan.default = true;
        print("orphan=" + orphan.track.mode);
        "#,
    )
    .await;
    assert_eq!(out, vec!["appended-after=disabled", "inserted-with=showing", "orphan=disabled"]);
}
