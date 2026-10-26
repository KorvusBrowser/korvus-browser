//! THE LOAD PRIORITY OF A REQUEST, as Blink computes it and Chromium puts it on the wire.
//!
//! Chromium sends every HTTP/2 request with an RFC 9218 `priority` field, and the field is Blink's
//! `ResourceLoadPriority` for the load: `ResourceFetcher::ComputeLoadPriority` (Chromium 150,
//! `resource_fetcher.cc`) starts from the resource type's priority and moves it for what only the loader
//! knows -- a classic script that is `async`, `defer` or inserted by script, a script or style sheet the
//! preload scanner met after the document's first image, a style sheet whose `media` does not match, the
//! first five sizeable images of a document, `fetchpriority`, render blocking. The level maps to the net
//! stack's `RequestPriority` (`WebURLRequest::ConvertToNetPriority`) and that to the urgency
//! (`HIGHEST - priority`, `quic_http_utils.cc`); a type that loads incrementally adds `i`, and the
//! default urgency 3 without `i` is no field at all (`spdy_http_utils.cc`).
//!
//! korvus derived the field from the destination alone: every script `u=1`, every image `u=2, i`, every
//! style sheet `u=0`. Measured on google-chrome 150.0.7871.46 over h2 (a loopback TLS server, two runs
//! agreeing on every row), against korvus:
//!
//! ```text
//!                                                   Chrome 150   korvus (before)
//!   <script async>, <script defer>, a script-inserted <script> (async=false too)   none   u=1
//!   a parser <script> after the first <img>                          u=2          u=1
//!   <link rel=stylesheet> after the first <img>                      u=2          u=0
//!   <link rel=stylesheet media=print>, rel="alternate stylesheet"    u=4          u=0
//!   the first five sizeable <img>, from the document                 u=2, i       u=2, i
//!   a sixth, a small one (width=10 height=10), <input type=image>,
//!     <video poster>, a script-created image, a CSS background       i            u=2, i
//!   <img fetchpriority=high>, from the document or from script       u=1, i       u=2, i
//!   <script async fetchpriority=high>                                u=1          u=1
//!   <script fetchpriority=low> in the head (render-blocking)         u=1          u=1
//!   <link rel=preload as=font>                                       u=1          u=0
//!   <link rel=preload as=image>                                      i            u=2, i
//! ```
//!
//! and live, nih.gov behind a bot challenge: the challenge's own script-inserted `chl_page` and
//! `api.js` none (korvus `u=1`), the challenge frame's script-created image `i`, the site's scripts after
//! its first image `u=2`.

use std::collections::HashMap;

/// Blink's `ResourceLoadPriority`, lowest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Level {
    VeryLow,
    Low,
    Medium,
    High,
    VeryHigh,
}

/// The `fetchpriority` attribute's state (`mojom::FetchPriorityHint`): an enumerated attribute whose
/// missing and invalid values are `auto`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Hint {
    #[default]
    Auto,
    High,
    Low,
}

impl Hint {
    pub(crate) fn parse(value: Option<&str>) -> Hint {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("high") => Hint::High,
            Some("low") => Hint::Low,
            _ => Hint::Auto,
        }
    }
}

/// Blink's `ResourceType`, for the loads a document's elements and style sheets make.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Style,
    Font,
    Script,
    /// `fetch()`, XHR, `<link rel=preload as=fetch>`.
    Raw,
    Image,
    Media,
    Track,
    /// `<link rel=prefetch>` (`kLinkPrefetch`).
    Prefetch,
}

impl Kind {
    /// The kind a request destination loads as.
    pub(crate) fn of_destination(destination: &str) -> Option<Kind> {
        Some(match destination {
            "style" => Kind::Style,
            "font" => Kind::Font,
            "script" => Kind::Script,
            "" | "empty" | "json" => Kind::Raw,
            "image" => Kind::Image,
            "audio" | "video" => Kind::Media,
            "track" => Kind::Track,
            _ => return None,
        })
    }

    /// `TypeToPriority`.
    fn base(self) -> Level {
        match self {
            Kind::Style | Kind::Font => Level::VeryHigh,
            Kind::Script | Kind::Raw => Level::High,
            Kind::Image | Kind::Media | Kind::Track => Level::Low,
            Kind::Prefetch => Level::VeryLow,
        }
    }

    /// `ShouldLoadIncremental`: everything but scripts, style sheets and fonts can use a partial body.
    fn incremental(self) -> bool {
        !matches!(self, Kind::Style | Kind::Font | Kind::Script)
    }
}

/// What `ComputeLoadPriority` reads off one load.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Load {
    pub kind: Kind,
    /// A classic script: the `async`/`defer` and late-in-document rules are classic scripts' only.
    pub classic: bool,
    /// `FetchParameters::kLazyLoad`, for a classic script that is `async`, `defer`, or was not inserted
    /// by the parser (`script_loader.cc`: `!parser_inserted_ || async || defer`).
    pub lazy: bool,
    /// `kIdleLoad`: a style sheet whose `media` does not match, or an alternate one (`link_style.cc`).
    pub idle: bool,
    /// Found by the preload scanner after the document's first image (`image_fetched_`).
    pub late: bool,
    /// `RenderBlockingBehavior::kBlocking`: a parser-blocking script or a style sheet before the body, or
    /// one carrying `blocking=render`.
    pub render_blocking: bool,
    /// A `<link rel=preload>`.
    pub link_preload: bool,
    /// An image the preload scanner counted among a document's first five that are not small.
    pub boost: bool,
    pub hint: Hint,
}

impl Load {
    pub(crate) fn of(kind: Kind) -> Load {
        Load { kind, classic: true, lazy: false, idle: false, late: false, render_blocking: false, link_preload: false, boost: false, hint: Hint::Auto }
    }
}

/// `ResourceFetcher::ComputeLoadPriority`, the parts an element's or a style sheet's load reaches.
pub(crate) fn level(l: &Load) -> Level {
    let mut p = l.kind.base();
    if l.kind == Kind::Font && l.link_preload {
        // a preloaded font should not take precedence over critical CSS or parser-blocking scripts
        p = Level::High;
    }
    if l.idle {
        p = Level::VeryLow;
    } else if l.kind == Kind::Script {
        if l.classic {
            if l.lazy {
                p = Level::Low;
            } else if l.late {
                p = Level::Medium;
            }
        }
    } else if l.kind == Kind::Style && l.late {
        p = Level::Medium;
    }
    // `AdjustPriorityWithPriorityHintAndRenderBlocking`
    p = match l.hint {
        Hint::Auto => p,
        Hint::High => p.max(Level::High),
        // an early render-blocking style sheet only comes down to High
        Hint::Low if l.kind == Kind::Style && p == Level::VeryHigh => Level::High,
        Hint::Low => p.min(Level::Low),
    };
    if l.render_blocking {
        p = p.max(Level::High);
    }
    // `AdjustImagePriority`: the scanner's boost, unless a hint already decided
    if l.kind == Kind::Image && l.boost && p < Level::Medium && l.hint == Hint::Auto {
        p = Level::Medium;
    }
    p
}

/// The `priority` field Chromium writes for `level` on a `kind` load over HTTP/2; `""` is none.
pub(crate) fn field(kind: Kind, level: Level) -> String {
    let urgency = match level {
        Level::VeryHigh => 0,
        Level::High => 1,
        Level::Medium => 2,
        Level::Low => 3,
        Level::VeryLow => 4,
    };
    match (urgency, kind.incremental()) {
        (3, false) => String::new(),
        (3, true) => "i".into(),
        (u, false) => format!("u={u}"),
        (u, true) => format!("u={u}, i"),
    }
}

/// [`level`] and [`field`] in one.
pub(crate) fn field_for(l: &Load) -> String {
    field(l.kind, level(l))
}

// ---- the preload scanner's view of a document ------------------------------------------------------

/// The scanner's in-document loads by element: each one's URL and priority field. A later load of the
/// element is its own -- a rewritten `src`, a script's re-insertion -- and takes the dynamic rules, so the
/// entry is TAKEN by the load it was computed for. Node ids are arena-unique, so one table serves the page
/// and its frames' documents; it is cleared with the page's other per-document loader state.
#[derive(Default)]
pub(crate) struct ScannedPriorities(pub(crate) HashMap<u32, String>);

/// The preload scanner's walk, in tree order (Blink's `TokenPreloadScanner` over the whole document):
/// what each load is, given what came before it. `late` is "an image has been requested", `boosted` the
/// count of sizeable images so far (`kBoostedImageTarget` is 5).
#[derive(Default)]
pub(crate) struct ScanState {
    pub image_seen: bool,
    pub boosted: u32,
}

/// `kSmallImageMaxSize`: an image whose declared width x height is at most this many px^2 is small.
const SMALL_IMAGE_AREA: f64 = 10_000.0;
const BOOSTED_IMAGE_TARGET: u32 = 5;

impl ScanState {
    /// One in-document image (`<img>`, `<input type=image>`, `<video poster>`): its load, and the walk moved
    /// past it. `width`/`height` are the declared dimensions, when absolute.
    pub(crate) fn image(&mut self, width: Option<f64>, height: Option<f64>, hint: Hint) -> Load {
        let small = match (width, height) {
            (Some(w), Some(h)) => w * h <= SMALL_IMAGE_AREA,
            (Some(w), None) => w == 0.0,
            (None, Some(h)) => h == 0.0,
            (None, None) => false,
        };
        let mut boost = false;
        if !small && self.boosted < BOOSTED_IMAGE_TARGET {
            self.boosted += 1;
            boost = true;
        }
        self.image_seen = true;
        Load { boost, hint, ..Load::of(Kind::Image) }
    }
}

/// An HTML dimension attribute's value when it is absolute (`HTMLDimension` `IsAbsolute`): leading
/// whitespace, digits with an optional fraction, and no `%` or `*` after them.
pub(crate) fn absolute_dimension(v: Option<&str>) -> Option<f64> {
    let s = v?.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let n: f64 = s[..end].trim_end_matches('.').parse().ok()?;
    let rest = s[end..].trim_start();
    if rest.starts_with('%') || rest.starts_with('*') {
        return None;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> Load {
        Load::of(Kind::Script)
    }

    /// Every row of the measured table above, through the model.
    #[test]
    fn the_levels_chrome_150_put_on_the_wire() {
        let f = |l: Load| field_for(&l);
        // scripts
        assert_eq!(f(Load { render_blocking: true, ..script() }), "u=1");
        assert_eq!(f(Load { lazy: true, ..script() }), "");
        assert_eq!(f(Load { late: true, ..script() }), "u=2");
        assert_eq!(f(Load { lazy: true, late: true, ..script() }), "");
        assert_eq!(f(Load { lazy: true, hint: Hint::High, ..script() }), "u=1");
        assert_eq!(f(Load { render_blocking: true, hint: Hint::Low, ..script() }), "u=1");
        assert_eq!(f(Load { classic: false, ..script() }), "u=1");
        assert_eq!(f(Load { classic: false, late: true, ..script() }), "u=1");
        assert_eq!(f(Load { link_preload: true, ..script() }), "u=1");
        // style sheets
        assert_eq!(f(Load::of(Kind::Style)), "u=0");
        assert_eq!(f(Load { late: true, ..Load::of(Kind::Style) }), "u=2");
        assert_eq!(f(Load { idle: true, ..Load::of(Kind::Style) }), "u=4");
        assert_eq!(f(Load { idle: true, late: true, ..Load::of(Kind::Style) }), "u=4");
        assert_eq!(f(Load { hint: Hint::Low, render_blocking: true, ..Load::of(Kind::Style) }), "u=1");
        // fonts
        assert_eq!(f(Load::of(Kind::Font)), "u=0");
        assert_eq!(f(Load { link_preload: true, ..Load::of(Kind::Font) }), "u=1");
        // images
        assert_eq!(f(Load::of(Kind::Image)), "i");
        assert_eq!(f(Load { boost: true, ..Load::of(Kind::Image) }), "u=2, i");
        assert_eq!(f(Load { hint: Hint::High, ..Load::of(Kind::Image) }), "u=1, i");
        assert_eq!(f(Load { hint: Hint::High, boost: true, ..Load::of(Kind::Image) }), "u=1, i");
        assert_eq!(f(Load { hint: Hint::Low, boost: true, ..Load::of(Kind::Image) }), "i");
        // fetch, media, tracks
        assert_eq!(f(Load::of(Kind::Raw)), "u=1, i");
        assert_eq!(f(Load::of(Kind::Media)), "i");
        assert_eq!(f(Load::of(Kind::Track)), "i");
        assert_eq!(f(Load::of(Kind::Prefetch)), "u=4, i");
    }

    /// The scanner's image boost, in the measured document's order: `i1`, a small `i-small`, `i2`,
    /// `i-high` (fetchpriority=high, counted), `i3`, `i4`, then `i5`, `i6`, `<input type=image>` and a
    /// poster past the five.
    #[test]
    fn the_first_five_sizeable_images_are_boosted() {
        let mut s = ScanState::default();
        let mut got = Vec::new();
        for (w, h, hint) in [
            (None, None, Hint::Auto),
            (Some(10.0), Some(10.0), Hint::Auto),
            (None, None, Hint::Auto),
            (None, None, Hint::High),
            (None, None, Hint::Auto),
            (None, None, Hint::Auto),
            (None, None, Hint::Auto),
            (None, None, Hint::Auto),
            (None, None, Hint::Auto),
            (None, None, Hint::Auto),
        ] {
            got.push(field_for(&s.image(w, h, hint)));
        }
        assert_eq!(got, ["u=2, i", "i", "u=2, i", "u=1, i", "u=2, i", "u=2, i", "i", "i", "i", "i"]);
        assert!(s.image_seen);
    }

    #[test]
    fn a_dimension_is_absolute_or_nothing() {
        assert_eq!(absolute_dimension(Some("10")), Some(10.0));
        assert_eq!(absolute_dimension(Some(" 12.5px")), Some(12.5));
        assert_eq!(absolute_dimension(Some("50%")), None);
        assert_eq!(absolute_dimension(Some("2*")), None);
        assert_eq!(absolute_dimension(Some("x")), None);
        assert_eq!(absolute_dimension(None), None);
    }
}
