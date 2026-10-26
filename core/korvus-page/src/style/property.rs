//! The property table: every longhand v1 knows, with the three facts the cascade needs about it.
//!
//! Mechanical, and load-bearing. A wrong `inherited` flag is a silent cascade bug -- the value is
//! never *wrong*, it is just computed for the wrong set of elements, and nothing crashes. So the
//! flags are not from memory: every name here was checked against Blink's own
//! `core/css/css_properties.json5`, and the one disagreement it reported is explained below.
//!
//! # `initial` is what Chrome says, not what the spec says
//!
//! Each entry's initial value is the **computed** value Chrome serializes for an element with
//! nothing set -- exactly the string `getComputedStyle` gives on a bare element. That is the useful
//! form (the paint tail can answer straight from it) and the checkable one, and it already accounts
//! for interactions between initial values. `border-top-width` is `"0px"`, not the spec's `medium`,
//! because the initial `border-top-style` is `none` and a border with no style has no width.
//!
//! It is now RECORDED rather than reasoned: every value here was read back from the Brave 151 on this
//! machine, per property, with `el.style.setProperty(name, "initial")` -- which is the only way to ask
//! a browser for a computed initial without a laid-out box answering for it. One property that was
//! already here disagreed with it and was wrong: `outline-width` keeps `medium`'s `3px` where a border
//! with no style would have collapsed to zero. The recording also said `min-width` and `min-height`
//! were `0px`, and that one was the RESOLVED value talking: the computed initial is `auto`, and Blink
//! reports `0px` only for a box that is not a flex or grid item (see
//! `Page::resolve_at_read`). Storing the `0px` took the automatic minimum size away from every flex
//! and grid item, which is what taffy does with `auto`. `inline-size`, `block-size`, `perspective-origin` and `transform-origin`
//! are the four the recording CANNOT answer -- their resolved value is the used value, so the probe
//! box's own width comes back -- and they carry their spec initial instead.
//!
//! `block-size` was recorded as `0px`, which is the probe box's used height and not an initial value
//! at all: it is the fourth property with this defect and it was missed because the first three were
//! spotted by hand. Ask the control for `block-size` on an element that has NO used value -- one that
//! is `display: none`, or an inline -- and it answers `auto`, which is the spec initial and what
//! stands here now.
//!
//! Two entries come from user preferences rather than any spec, and the persona owns them:
//! `font-size`'s `16px` is Chrome's default and happens to match, and `font-family` is left empty
//! because there is no single default -- P1's UA layer fills it from the persona. An empty computed
//! `font-family` would itself be a tell, so this is a gap with an owner, not a value.
//!
//! # The one disagreement with Blink
//!
//! Blink has no `inherited` flag on `border-spacing`, because it implements it as a shorthand over
//! two private longhands (`-webkit-border-horizontal-spacing` and its vertical twin) which *are*
//! inherited. `border-spacing` is the web-facing name -- the one `getComputedStyle` answers to --
//! so it stays here, inherited, and the check that flagged it is right to have flagged it.
//!
//! # Scope
//!
//! Longhands only: a shorthand is a parse-time expansion (`style/shorthand.rs`), never a thing the
//! cascade stores. v1 covers what layout reads plus the paint properties a page actually asks for;
//! Chrome enumerates roughly three times as many, and closing that gap is P1's problem, where
//! `getComputedStyle` is wired and the difference becomes measurable.
/// What has to happen to a specified value before `getComputedStyle` can hand it back.
///
/// The computed value is not the value the page wrote. `color: red` computes to `rgb(255, 0, 0)`,
/// `margin: 2em` computes to a number of pixels, `font-weight: bold` computes to `700`. A page can
/// read all three, so echoing the specified text is a tell rather than a shortcut -- and for the
/// lengths it is also the difference between having geometry and not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Computed {
    /// Parse as a colour and serialize; `currentColor` becomes the element's own `color`.
    Color,
    /// Resolve lengths to pixels. Percentages, `auto`, and keywords stay as written -- a percentage
    /// resolves against a containing block that layout has not established yet.
    Lengthy,
    /// Always an absolute length, resolved against the *parent's* font size.
    FontSize,
    /// Keywords become numbers, and `bolder`/`lighter` are relative to the parent.
    FontWeight,
    /// `normal`, a bare number kept as a number, or a length resolved to pixels.
    LineHeight,
    /// A keyword, lowercased. Nothing else to do.
    Keyword,
    /// `display`, whose two-keyword syntax is normalised to the spelling Chrome serializes:
    /// `block flex` computes to `flex`, `inline flow-root` to `inline-block`. The table lives in
    /// `style::resolve`'s `canonical_display`.
    Display,
    /// Handed back exactly as written. Either there is nothing to resolve (`font-family`) or
    /// resolving it needs layout (`background-size`, `transform`).
    AsSpecified,
}

/// Declares the table once and derives the enum, the lookups, and `ALL` from it, so a property is
/// exactly one line and the four facts about it cannot drift apart across four `match` arms.
macro_rules! properties {
    ($($variant:ident => $name:literal, $inherited:literal, $layout:literal, $kind:ident, $initial:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum PropertyId { $($variant),* }

        /// Every property v1 knows, in declaration order.
        pub const ALL: &[PropertyId] = &[$(PropertyId::$variant),*];

        /// How many there are. A dense index, so the cascade can keep one slot per property in a
        /// stack array instead of a map.
        pub const COUNT: usize = ALL.len();

        /// The prefix of [`ALL`] that `getComputedStyle` enumerates, in the order it enumerates them.
        ///
        /// Both halves of that sentence are load-bearing and neither is a spec question -- they were
        /// recorded from the Brave 151 on this machine, and Chrome 152's capture agrees with two more
        /// (`ruby-overhang`, `window-drag`, 2026-10-16). The COUNT is 476, and a page reads it as
        /// `getComputedStyle(el).length`, which is one line and was answering 127. The ORDER is
        /// observable too, through `cs[i]`, so the table is declared in Chrome's order rather than in
        /// any order of ours.
        ///
        /// Everything after the prefix is a property the cascade needs and Chrome does not enumerate.
        pub const ENUMERATED: &[PropertyId] = ALL.split_at(476).0;

        impl PropertyId {
            /// The property's position in [`ALL`] — a dense index into a per-property array.
            pub fn index(self) -> usize {
                self as usize
            }

            /// The web-facing name, lowercase, as it is written in CSS and read back by the CSSOM.
            pub fn name(self) -> &'static str {
                match self { $(PropertyId::$variant => $name),* }
            }

            /// Whether an unset value comes from the parent instead of from `initial`.
            pub fn is_inherited(self) -> bool {
                match self { $(PropertyId::$variant => $inherited),* }
            }

            /// Whether layout reads it. This is the D7 split: `true` means the value belongs in the
            /// compact typed struct every node's layout pass touches, `false` means it can live in
            /// the declaration tail and only ever be turned back into a string for the CSSOM.
            pub fn affects_layout(self) -> bool {
                match self { $(PropertyId::$variant => $layout),* }
            }

            /// How a specified value for this property becomes a computed one. See [`Computed`].
            pub fn computed_kind(self) -> Computed {
                match self { $(PropertyId::$variant => Computed::$kind),* }
            }

            /// The initial computed value, as Chrome serializes it. See the module note.
            pub fn initial(self) -> &'static str {
                match self { $(PropertyId::$variant => $initial),* }
            }

            /// Look a property up by name. CSS property names are ASCII case-insensitive, so
            /// `COLOR` and `color` are the same property; an unknown name is not an error anywhere,
            /// it is a declaration the parser drops.
            pub fn from_name(name: &str) -> Option<PropertyId> {
                // Lowercasing allocates only when the name is not already lowercase, which is the
                // rare case -- stylesheets are written lowercase and this runs per declaration.
                if name.bytes().any(|b| b.is_ascii_uppercase()) {
                    return Self::from_lowercase(&name.to_ascii_lowercase());
                }
                Self::from_lowercase(name)
            }

            fn from_lowercase(name: &str) -> Option<PropertyId> {
                match name { $($name => Some(PropertyId::$variant),)* _ => None }
            }
        }
    };
}

properties! {
    // ---- the box ----
    AccentColor                         => "accent-color",                         true,  false, AsSpecified, "auto";
    AlignContent                        => "align-content",                        false, true,  Keyword,     "normal";
    AlignItems                          => "align-items",                          false, true,  Keyword,     "normal";
    AlignSelf                           => "align-self",                           false, true,  Keyword,     "auto";
    AlignmentBaseline                   => "alignment-baseline",                   false, false, AsSpecified, "auto";
    AnchorName                          => "anchor-name",                          false, false, AsSpecified, "none";
    AnchorScope                         => "anchor-scope",                         false, false, AsSpecified, "none";
    AnimationComposition                => "animation-composition",                false, false, AsSpecified, "replace";
    AnimationDelay                      => "animation-delay",                      false, false, AsSpecified, "0s";
    AnimationDirection                  => "animation-direction",                  false, false, AsSpecified, "normal";
    AnimationDuration                   => "animation-duration",                   false, false, AsSpecified, "0s";
    AnimationFillMode                   => "animation-fill-mode",                  false, false, AsSpecified, "none";
    AnimationIterationCount             => "animation-iteration-count",            false, false, AsSpecified, "1";
    AnimationName                       => "animation-name",                       false, false, AsSpecified, "none";
    AnimationPlayState                  => "animation-play-state",                 false, false, AsSpecified, "running";
    AnimationRangeEnd                   => "animation-range-end",                  false, false, AsSpecified, "normal";
    AnimationRangeStart                 => "animation-range-start",                false, false, AsSpecified, "normal";
    AnimationTimeline                   => "animation-timeline",                   false, false, AsSpecified, "auto";
    AnimationTimingFunction             => "animation-timing-function",            false, false, AsSpecified, "ease";
    AnimationTrigger                    => "animation-trigger",                    false, false, AsSpecified, "none";
    AppRegion                           => "app-region",                           false, false, AsSpecified, "none";
    Appearance                          => "appearance",                           false, false, AsSpecified, "none";
    AspectRatio                         => "aspect-ratio",                         false, true,  AsSpecified, "auto";
    BackdropFilter                      => "backdrop-filter",                      false, false, AsSpecified, "none";
    BackfaceVisibility                  => "backface-visibility",                  false, false, AsSpecified, "visible";
    BackgroundAttachment                => "background-attachment",                false, false, Keyword,     "scroll";
    BackgroundBlendMode                 => "background-blend-mode",                false, false, AsSpecified, "normal";
    BackgroundClip                      => "background-clip",                      false, false, Keyword,     "border-box";
    BackgroundColor                     => "background-color",                     false, false, Color,       "rgba(0, 0, 0, 0)";
    BackgroundImage                     => "background-image",                     false, false, AsSpecified, "none";
    BackgroundOrigin                    => "background-origin",                    false, false, Keyword,     "padding-box";
    BackgroundPosition                  => "background-position",                  false, false, AsSpecified, "0% 0%";
    BackgroundRepeat                    => "background-repeat",                    false, false, Keyword,     "repeat";
    BackgroundSize                      => "background-size",                      false, false, Lengthy,     "auto";
    BaselineShift                       => "baseline-shift",                       false, false, AsSpecified, "0px";
    BaselineSource                      => "baseline-source",                      false, false, AsSpecified, "auto";
    BlockSize                           => "block-size",                           false, false, AsSpecified, "auto";
    BorderBlockEndColor                 => "border-block-end-color",               false, false, Color,       "currentcolor";
    BorderBlockEndStyle                 => "border-block-end-style",               false, false, AsSpecified, "none";
    BorderBlockEndWidth                 => "border-block-end-width",               false, false, AsSpecified, "3px";
    BorderBlockStartColor               => "border-block-start-color",             false, false, Color,       "currentcolor";
    BorderBlockStartStyle               => "border-block-start-style",             false, false, AsSpecified, "none";
    BorderBlockStartWidth               => "border-block-start-width",             false, false, AsSpecified, "3px";
    BorderBottomColor                   => "border-bottom-color",                  false, false, Color,       "currentcolor";
    BorderBottomLeftRadius              => "border-bottom-left-radius",            false, false, Lengthy,     "0px";
    BorderBottomRightRadius             => "border-bottom-right-radius",           false, false, Lengthy,     "0px";
    BorderBottomStyle                   => "border-bottom-style",                  false, true,  Keyword,     "none";
    BorderBottomWidth                   => "border-bottom-width",                  false, true,  Lengthy,     "3px";
    BorderCollapse                      => "border-collapse",                      true,  true,  Keyword,     "separate";
    BorderEndEndRadius                  => "border-end-end-radius",                false, false, AsSpecified, "0px";
    BorderEndStartRadius                => "border-end-start-radius",              false, false, AsSpecified, "0px";
    BorderImageOutset                   => "border-image-outset",                  false, false, AsSpecified, "0";
    BorderImageRepeat                   => "border-image-repeat",                  false, false, AsSpecified, "stretch";
    BorderImageSlice                    => "border-image-slice",                   false, false, AsSpecified, "100%";
    BorderImageSource                   => "border-image-source",                  false, false, AsSpecified, "none";
    BorderImageWidth                    => "border-image-width",                   false, false, AsSpecified, "1";
    BorderInlineEndColor                => "border-inline-end-color",              false, false, Color,       "currentcolor";
    BorderInlineEndStyle                => "border-inline-end-style",              false, false, AsSpecified, "none";
    BorderInlineEndWidth                => "border-inline-end-width",              false, false, AsSpecified, "3px";
    BorderInlineStartColor              => "border-inline-start-color",            false, false, Color,       "currentcolor";
    BorderInlineStartStyle              => "border-inline-start-style",            false, false, AsSpecified, "none";
    BorderInlineStartWidth              => "border-inline-start-width",            false, false, AsSpecified, "3px";
    BorderLeftColor                     => "border-left-color",                    false, false, Color,       "currentcolor";
    BorderLeftStyle                     => "border-left-style",                    false, true,  Keyword,     "none";
    BorderLeftWidth                     => "border-left-width",                    false, true,  Lengthy,     "3px";
    BorderRightColor                    => "border-right-color",                   false, false, Color,       "currentcolor";
    BorderRightStyle                    => "border-right-style",                   false, true,  Keyword,     "none";
    BorderRightWidth                    => "border-right-width",                   false, true,  Lengthy,     "3px";
    BorderShape                         => "border-shape",                         false, false, AsSpecified, "none";
    BorderStartEndRadius                => "border-start-end-radius",              false, false, AsSpecified, "0px";
    BorderStartStartRadius              => "border-start-start-radius",            false, false, AsSpecified, "0px";
    BorderTopColor                      => "border-top-color",                     false, false, Color,       "currentcolor";
    BorderTopLeftRadius                 => "border-top-left-radius",               false, false, Lengthy,     "0px";
    BorderTopRightRadius                => "border-top-right-radius",              false, false, Lengthy,     "0px";
    BorderTopStyle                      => "border-top-style",                     false, true,  Keyword,     "none";
    BorderTopWidth                      => "border-top-width",                     false, true,  Lengthy,     "3px";
    Bottom                              => "bottom",                               false, true,  Lengthy,     "auto";
    BoxDecorationBreak                  => "box-decoration-break",                 false, false, AsSpecified, "slice";
    BoxShadow                           => "box-shadow",                           false, false, AsSpecified, "none";
    BoxSizing                           => "box-sizing",                           false, true,  Keyword,     "content-box";
    BreakAfter                          => "break-after",                          false, false, AsSpecified, "auto";
    BreakBefore                         => "break-before",                         false, false, AsSpecified, "auto";
    BreakInside                         => "break-inside",                         false, false, AsSpecified, "auto";
    BufferedRendering                   => "buffered-rendering",                   false, false, AsSpecified, "auto";
    CaptionSide                         => "caption-side",                         true,  true,  Keyword,     "top";
    CaretAnimation                      => "caret-animation",                      true,  false, AsSpecified, "auto";
    CaretColor                          => "caret-color",                          true,  false, Color,       "currentcolor";
    CaretShape                          => "caret-shape",                          true,  false, AsSpecified, "auto";
    Clear                               => "clear",                                false, true,  Keyword,     "none";
    Clip                                => "clip",                                 false, false, AsSpecified, "auto";
    ClipPath                            => "clip-path",                            false, false, AsSpecified, "none";
    ClipRule                            => "clip-rule",                            true,  false, AsSpecified, "nonzero";
    Color                               => "color",                                true,  false, Color,       "rgb(0, 0, 0)";
    ColorInterpolation                  => "color-interpolation",                  true,  false, AsSpecified, "srgb";
    ColorInterpolationFilters           => "color-interpolation-filters",          true,  false, AsSpecified, "linearrgb";
    ColorRendering                      => "color-rendering",                      true,  false, AsSpecified, "auto";
    ColorScheme                         => "color-scheme",                         true,  false, AsSpecified, "normal";
    ColumnCount                         => "column-count",                         false, false, AsSpecified, "auto";
    ColumnFill                          => "column-fill",                          false, false, AsSpecified, "balance";
    ColumnGap                           => "column-gap",                           false, true,  Lengthy,     "normal";
    ColumnHeight                        => "column-height",                        false, false, AsSpecified, "auto";
    ColumnRuleBreak                     => "column-rule-break",                    false, false, AsSpecified, "normal";
    ColumnRuleColor                     => "column-rule-color",                    false, false, Color,       "currentcolor";
    ColumnRuleInsetCapEnd               => "column-rule-inset-cap-end",            false, false, AsSpecified, "0px";
    ColumnRuleInsetCapStart             => "column-rule-inset-cap-start",          false, false, AsSpecified, "0px";
    ColumnRuleInsetJunctionEnd          => "column-rule-inset-junction-end",       false, false, AsSpecified, "0px";
    ColumnRuleInsetJunctionStart        => "column-rule-inset-junction-start",     false, false, AsSpecified, "0px";
    ColumnRuleStyle                     => "column-rule-style",                    false, false, AsSpecified, "none";
    ColumnRuleVisibilityItems           => "column-rule-visibility-items",         false, false, AsSpecified, "normal";
    ColumnRuleWidth                     => "column-rule-width",                    false, false, AsSpecified, "3px";
    ColumnSpan                          => "column-span",                          false, false, AsSpecified, "none";
    ColumnWidth                         => "column-width",                         false, false, AsSpecified, "auto";
    ColumnWrap                          => "column-wrap",                          false, false, AsSpecified, "auto";
    Contain                             => "contain",                              false, false, AsSpecified, "none";
    ContainIntrinsicBlockSize           => "contain-intrinsic-block-size",         false, false, AsSpecified, "none";
    ContainIntrinsicHeight              => "contain-intrinsic-height",             false, false, AsSpecified, "none";
    ContainIntrinsicInlineSize          => "contain-intrinsic-inline-size",        false, false, AsSpecified, "none";
    ContainIntrinsicSize                => "contain-intrinsic-size",               false, false, AsSpecified, "none";
    ContainIntrinsicWidth               => "contain-intrinsic-width",              false, false, AsSpecified, "none";
    ContainerName                       => "container-name",                       false, false, AsSpecified, "none";
    ContainerType                       => "container-type",                       false, false, AsSpecified, "normal";
    Content                             => "content",                              false, false, AsSpecified, "normal";
    ContentVisibility                   => "content-visibility",                   false, false, AsSpecified, "visible";
    CornerBottomLeftShape               => "corner-bottom-left-shape",             false, false, AsSpecified, "superellipse(1)";
    CornerBottomRightShape              => "corner-bottom-right-shape",            false, false, AsSpecified, "superellipse(1)";
    CornerEndEndShape                   => "corner-end-end-shape",                 false, false, AsSpecified, "superellipse(1)";
    CornerEndStartShape                 => "corner-end-start-shape",               false, false, AsSpecified, "superellipse(1)";
    CornerStartEndShape                 => "corner-start-end-shape",               false, false, AsSpecified, "superellipse(1)";
    CornerStartStartShape               => "corner-start-start-shape",             false, false, AsSpecified, "superellipse(1)";
    CornerTopLeftShape                  => "corner-top-left-shape",                false, false, AsSpecified, "superellipse(1)";
    CornerTopRightShape                 => "corner-top-right-shape",               false, false, AsSpecified, "superellipse(1)";
    CounterIncrement                    => "counter-increment",                    false, false, AsSpecified, "none";
    CounterReset                        => "counter-reset",                        false, false, AsSpecified, "none";
    CounterSet                          => "counter-set",                          false, false, AsSpecified, "none";
    Cursor                              => "cursor",                               true,  false, Keyword,     "auto";
    Cx                                  => "cx",                                   false, false, AsSpecified, "0px";
    Cy                                  => "cy",                                   false, false, AsSpecified, "0px";
    D                                   => "d",                                    false, false, AsSpecified, "none";
    Direction                           => "direction",                            true,  true,  Keyword,     "ltr";
    Display                             => "display",                              false, true,  Display,     "inline";
    DominantBaseline                    => "dominant-baseline",                    true,  false, AsSpecified, "auto";
    DynamicRangeLimit                   => "dynamic-range-limit",                  true,  false, AsSpecified, "no-limit";
    EmptyCells                          => "empty-cells",                          true,  true,  Keyword,     "show";
    FieldSizing                         => "field-sizing",                         false, false, AsSpecified, "fixed";
    Fill                                => "fill",                                 true,  false, Color,       "rgb(0, 0, 0)";
    FillOpacity                         => "fill-opacity",                         true,  false, AsSpecified, "1";
    FillRule                            => "fill-rule",                            true,  false, AsSpecified, "nonzero";
    Filter                              => "filter",                               false, false, AsSpecified, "none";
    FlexBasis                           => "flex-basis",                           false, true,  Lengthy,     "auto";
    FlexDirection                       => "flex-direction",                       false, true,  Keyword,     "row";
    FlexGrow                            => "flex-grow",                            false, true,  AsSpecified, "0";
    FlexLineCount                       => "flex-line-count",                      false, false, AsSpecified, "1";
    FlexShrink                          => "flex-shrink",                          false, true,  AsSpecified, "1";
    FlexWrap                            => "flex-wrap",                            false, true,  Keyword,     "nowrap";
    Float                               => "float",                                false, true,  Keyword,     "none";
    FloodColor                          => "flood-color",                          false, false, Color,       "rgb(0, 0, 0)";
    FloodOpacity                        => "flood-opacity",                        false, false, AsSpecified, "1";
    FontFamily                          => "font-family",                          true,  true,  AsSpecified, "";
    FontFeatureSettings                 => "font-feature-settings",                true,  false, AsSpecified, "normal";
    FontKerning                         => "font-kerning",                         true,  false, AsSpecified, "auto";
    FontLanguageOverride                => "font-language-override",               true,  false, AsSpecified, "normal";
    FontOpticalSizing                   => "font-optical-sizing",                  true,  false, AsSpecified, "auto";
    FontPalette                         => "font-palette",                         true,  false, AsSpecified, "normal";
    FontSize                            => "font-size",                            true,  true,  FontSize,    "16px";
    FontSizeAdjust                      => "font-size-adjust",                     true,  false, AsSpecified, "none";
    FontStretch                         => "font-stretch",                         true,  true,  AsSpecified, "100%";
    FontStyle                           => "font-style",                           true,  true,  Keyword,     "normal";
    FontSynthesisSmallCaps              => "font-synthesis-small-caps",            true,  false, AsSpecified, "auto";
    FontSynthesisStyle                  => "font-synthesis-style",                 true,  false, AsSpecified, "auto";
    FontSynthesisWeight                 => "font-synthesis-weight",                true,  false, AsSpecified, "auto";
    FontVariant                         => "font-variant",                         false, false, AsSpecified, "normal";
    FontVariantAlternates               => "font-variant-alternates",              true,  false, AsSpecified, "normal";
    FontVariantCaps                     => "font-variant-caps",                    true,  true,  Keyword,     "normal";
    FontVariantEastAsian                => "font-variant-east-asian",              true,  false, AsSpecified, "normal";
    FontVariantEmoji                    => "font-variant-emoji",                   true,  false, AsSpecified, "normal";
    FontVariantLigatures                => "font-variant-ligatures",               true,  false, AsSpecified, "normal";
    FontVariantNumeric                  => "font-variant-numeric",                 true,  false, AsSpecified, "normal";
    FontVariantPosition                 => "font-variant-position",                true,  false, AsSpecified, "normal";
    FontVariationSettings               => "font-variation-settings",              true,  false, AsSpecified, "normal";
    FontWeight                          => "font-weight",                          true,  true,  FontWeight,  "400";
    ForcedColorAdjust                   => "forced-color-adjust",                  true,  false, AsSpecified, "auto";
    GridAutoColumns                     => "grid-auto-columns",                    false, true,  AsSpecified, "auto";
    GridAutoFlow                        => "grid-auto-flow",                       false, true,  Keyword,     "row";
    GridAutoRows                        => "grid-auto-rows",                       false, true,  AsSpecified, "auto";
    GridColumnEnd                       => "grid-column-end",                      false, true,  AsSpecified, "auto";
    GridColumnStart                     => "grid-column-start",                    false, true,  AsSpecified, "auto";
    GridRowEnd                          => "grid-row-end",                         false, true,  AsSpecified, "auto";
    GridRowStart                        => "grid-row-start",                       false, true,  AsSpecified, "auto";
    GridTemplateAreas                   => "grid-template-areas",                  false, true,  AsSpecified, "none";
    GridTemplateColumns                 => "grid-template-columns",                false, true,  AsSpecified, "none";
    GridTemplateRows                    => "grid-template-rows",                   false, true,  AsSpecified, "none";
    Height                              => "height",                               false, true,  Lengthy,     "auto";
    HyphenateCharacter                  => "hyphenate-character",                  true,  false, AsSpecified, "auto";
    HyphenateLimitChars                 => "hyphenate-limit-chars",                true,  false, AsSpecified, "auto";
    Hyphens                             => "hyphens",                              true,  false, AsSpecified, "manual";
    ImageOrientation                    => "image-orientation",                    true,  false, AsSpecified, "from-image";
    ImageRendering                      => "image-rendering",                      true,  false, AsSpecified, "auto";
    InitialLetter                       => "initial-letter",                       false, false, AsSpecified, "normal";
    InlineSize                          => "inline-size",                          false, false, AsSpecified, "auto";
    InsetBlockEnd                       => "inset-block-end",                      false, false, AsSpecified, "auto";
    InsetBlockStart                     => "inset-block-start",                    false, false, AsSpecified, "auto";
    InsetInlineEnd                      => "inset-inline-end",                     false, false, AsSpecified, "auto";
    InsetInlineStart                    => "inset-inline-start",                   false, false, AsSpecified, "auto";
    Interactivity                       => "interactivity",                        true,  false, AsSpecified, "auto";
    InterestDelayEnd                    => "interest-delay-end",                   false, false, AsSpecified, "normal";
    InterestDelayStart                  => "interest-delay-start",                 false, false, AsSpecified, "normal";
    InterpolateSize                     => "interpolate-size",                     true,  false, AsSpecified, "numeric-only";
    Isolation                           => "isolation",                            false, false, Keyword,     "auto";
    JustifyContent                      => "justify-content",                      false, true,  Keyword,     "normal";
    JustifyItems                        => "justify-items",                        false, true,  Keyword,     "normal";
    JustifySelf                         => "justify-self",                         false, true,  Keyword,     "auto";
    Left                                => "left",                                 false, true,  Lengthy,     "auto";
    LetterSpacing                       => "letter-spacing",                       true,  true,  Lengthy,     "normal";
    LightingColor                       => "lighting-color",                       false, false, Color,       "rgb(255, 255, 255)";
    LineBreak                           => "line-break",                           true,  false, AsSpecified, "auto";
    LineHeight                          => "line-height",                          true,  true,  LineHeight,  "normal";
    ListStyleImage                      => "list-style-image",                     true,  false, AsSpecified, "none";
    ListStylePosition                   => "list-style-position",                  true,  true,  Keyword,     "outside";
    ListStyleType                       => "list-style-type",                      true,  false, Keyword,     "disc";
    MarginBlockEnd                      => "margin-block-end",                     false, false, AsSpecified, "0px";
    MarginBlockStart                    => "margin-block-start",                   false, false, AsSpecified, "0px";
    MarginBottom                        => "margin-bottom",                        false, true,  Lengthy,     "0px";
    MarginInlineEnd                     => "margin-inline-end",                    false, false, AsSpecified, "0px";
    MarginInlineStart                   => "margin-inline-start",                  false, false, AsSpecified, "0px";
    MarginLeft                          => "margin-left",                          false, true,  Lengthy,     "0px";
    MarginRight                         => "margin-right",                         false, true,  Lengthy,     "0px";
    MarginTop                           => "margin-top",                           false, true,  Lengthy,     "0px";
    MarkerEnd                           => "marker-end",                           true,  false, AsSpecified, "none";
    MarkerMid                           => "marker-mid",                           true,  false, AsSpecified, "none";
    MarkerStart                         => "marker-start",                         true,  false, AsSpecified, "none";
    MaskClip                            => "mask-clip",                            false, false, AsSpecified, "border-box";
    MaskComposite                       => "mask-composite",                       false, false, AsSpecified, "add";
    MaskImage                           => "mask-image",                           false, false, AsSpecified, "none";
    MaskMode                            => "mask-mode",                            false, false, AsSpecified, "match-source";
    MaskOrigin                          => "mask-origin",                          false, false, AsSpecified, "border-box";
    MaskPosition                        => "mask-position",                        false, false, AsSpecified, "0% 0%";
    MaskRepeat                          => "mask-repeat",                          false, false, AsSpecified, "repeat";
    MaskSize                            => "mask-size",                            false, false, Lengthy,     "auto";
    MaskType                            => "mask-type",                            false, false, AsSpecified, "luminance";
    MathDepth                           => "math-depth",                           true,  false, AsSpecified, "0";
    MathShift                           => "math-shift",                           true,  false, AsSpecified, "normal";
    MathStyle                           => "math-style",                           true,  false, AsSpecified, "normal";
    MaxBlockSize                        => "max-block-size",                       false, false, AsSpecified, "none";
    MaxHeight                           => "max-height",                           false, true,  Lengthy,     "none";
    MaxInlineSize                       => "max-inline-size",                      false, false, AsSpecified, "none";
    MaxWidth                            => "max-width",                            false, true,  Lengthy,     "none";
    MinBlockSize                        => "min-block-size",                       false, false, AsSpecified, "auto";
    MinHeight                           => "min-height",                           false, true,  Lengthy,     "auto";
    MinInlineSize                       => "min-inline-size",                      false, false, AsSpecified, "auto";
    MinWidth                            => "min-width",                            false, true,  Lengthy,     "auto";
    MixBlendMode                        => "mix-blend-mode",                       false, false, Keyword,     "normal";
    ObjectFit                           => "object-fit",                           false, true,  Keyword,     "fill";
    ObjectPosition                      => "object-position",                      false, false, Lengthy,     "50% 50%";
    ObjectViewBox                       => "object-view-box",                      false, false, AsSpecified, "none";
    OffsetAnchor                        => "offset-anchor",                        false, false, AsSpecified, "auto";
    OffsetDistance                      => "offset-distance",                      false, false, Lengthy,     "0px";
    OffsetPath                          => "offset-path",                          false, false, AsSpecified, "none";
    OffsetPosition                      => "offset-position",                      false, false, AsSpecified, "normal";
    OffsetRotate                        => "offset-rotate",                        false, false, AsSpecified, "auto 0deg";
    Opacity                             => "opacity",                              false, false, AsSpecified, "1";
    Order                               => "order",                                false, true,  AsSpecified, "0";
    Orphans                             => "orphans",                              true,  false, AsSpecified, "2";
    OutlineColor                        => "outline-color",                        false, false, Color,       "currentcolor";
    OutlineOffset                       => "outline-offset",                       false, false, Lengthy,     "0px";
    OutlineStyle                        => "outline-style",                        false, false, Keyword,     "none";
    OutlineWidth                        => "outline-width",                        false, false, Lengthy,     "3px";
    OverflowAnchor                      => "overflow-anchor",                      false, false, AsSpecified, "auto";
    OverflowBlock                       => "overflow-block",                       false, false, AsSpecified, "visible";
    OverflowClipMargin                  => "overflow-clip-margin",                 false, false, AsSpecified, "0px";
    OverflowInline                      => "overflow-inline",                      false, false, AsSpecified, "visible";
    OverflowWrap                        => "overflow-wrap",                        true,  true,  Keyword,     "normal";
    OverflowX                           => "overflow-x",                           false, true,  Keyword,     "visible";
    OverflowY                           => "overflow-y",                           false, true,  Keyword,     "visible";
    Overlay                             => "overlay",                              false, false, AsSpecified, "none";
    OverscrollBehaviorBlock             => "overscroll-behavior-block",            false, false, AsSpecified, "auto";
    OverscrollBehaviorInline            => "overscroll-behavior-inline",           false, false, AsSpecified, "auto";
    OverscrollBehaviorX                 => "overscroll-behavior-x",                false, false, AsSpecified, "auto";
    OverscrollBehaviorY                 => "overscroll-behavior-y",                false, false, AsSpecified, "auto";
    PaddingBlockEnd                     => "padding-block-end",                    false, false, AsSpecified, "0px";
    PaddingBlockStart                   => "padding-block-start",                  false, false, AsSpecified, "0px";
    PaddingBottom                       => "padding-bottom",                       false, true,  Lengthy,     "0px";
    PaddingInlineEnd                    => "padding-inline-end",                   false, false, AsSpecified, "0px";
    PaddingInlineStart                  => "padding-inline-start",                 false, false, AsSpecified, "0px";
    PaddingLeft                         => "padding-left",                         false, true,  Lengthy,     "0px";
    PaddingRight                        => "padding-right",                        false, true,  Lengthy,     "0px";
    PaddingTop                          => "padding-top",                          false, true,  Lengthy,     "0px";
    PaintOrder                          => "paint-order",                          true,  false, AsSpecified, "normal";
    Perspective                         => "perspective",                          false, false, AsSpecified, "none";
    PerspectiveOrigin                   => "perspective-origin",                   false, false, AsSpecified, "50% 50%";
    PointerEvents                       => "pointer-events",                       true,  false, Keyword,     "auto";
    Position                            => "position",                             false, true,  Keyword,     "static";
    PositionAnchor                      => "position-anchor",                      false, false, AsSpecified, "normal";
    PositionArea                        => "position-area",                        false, false, AsSpecified, "none";
    PositionTryFallbacks                => "position-try-fallbacks",               false, false, AsSpecified, "none";
    PositionTryOrder                    => "position-try-order",                   false, false, AsSpecified, "normal";
    PositionVisibility                  => "position-visibility",                  false, false, AsSpecified, "anchors-visible";
    PrintColorAdjust                    => "print-color-adjust",                   true,  false, AsSpecified, "economy";
    Quotes                              => "quotes",                               true,  false, AsSpecified, "auto";
    R                                   => "r",                                    false, false, AsSpecified, "0px";
    ReadingFlow                         => "reading-flow",                         false, false, AsSpecified, "normal";
    ReadingOrder                        => "reading-order",                        false, false, AsSpecified, "0";
    Resize                              => "resize",                               false, false, AsSpecified, "none";
    Right                               => "right",                                false, true,  Lengthy,     "auto";
    Rotate                              => "rotate",                               false, false, AsSpecified, "none";
    RowGap                              => "row-gap",                              false, true,  Lengthy,     "normal";
    RowRuleBreak                        => "row-rule-break",                       false, false, AsSpecified, "normal";
    RowRuleColor                        => "row-rule-color",                       false, false, Color,       "currentcolor";
    RowRuleInsetCapEnd                  => "row-rule-inset-cap-end",               false, false, AsSpecified, "0px";
    RowRuleInsetCapStart                => "row-rule-inset-cap-start",             false, false, AsSpecified, "0px";
    RowRuleInsetJunctionEnd             => "row-rule-inset-junction-end",          false, false, AsSpecified, "0px";
    RowRuleInsetJunctionStart           => "row-rule-inset-junction-start",        false, false, AsSpecified, "0px";
    RowRuleStyle                        => "row-rule-style",                       false, false, AsSpecified, "none";
    RowRuleVisibilityItems              => "row-rule-visibility-items",            false, false, AsSpecified, "normal";
    RowRuleWidth                        => "row-rule-width",                       false, false, AsSpecified, "3px";
    RubyAlign                           => "ruby-align",                           true,  false, AsSpecified, "space-around";
    RubyOverhang                        => "ruby-overhang",                        true,  false, AsSpecified, "auto";
    RubyPosition                        => "ruby-position",                        true,  false, AsSpecified, "over";
    RuleOverlap                         => "rule-overlap",                         false, false, AsSpecified, "row-over-column";
    Rx                                  => "rx",                                   false, false, AsSpecified, "auto";
    Ry                                  => "ry",                                   false, false, AsSpecified, "auto";
    Scale                               => "scale",                                false, false, AsSpecified, "none";
    ScrollBehavior                      => "scroll-behavior",                      false, false, AsSpecified, "auto";
    ScrollInitialTarget                 => "scroll-initial-target",                false, false, AsSpecified, "none";
    ScrollMarginBlockEnd                => "scroll-margin-block-end",              false, false, AsSpecified, "0px";
    ScrollMarginBlockStart              => "scroll-margin-block-start",            false, false, AsSpecified, "0px";
    ScrollMarginBottom                  => "scroll-margin-bottom",                 false, false, AsSpecified, "0px";
    ScrollMarginInlineEnd               => "scroll-margin-inline-end",             false, false, AsSpecified, "0px";
    ScrollMarginInlineStart             => "scroll-margin-inline-start",           false, false, AsSpecified, "0px";
    ScrollMarginLeft                    => "scroll-margin-left",                   false, false, AsSpecified, "0px";
    ScrollMarginRight                   => "scroll-margin-right",                  false, false, AsSpecified, "0px";
    ScrollMarginTop                     => "scroll-margin-top",                    false, false, AsSpecified, "0px";
    ScrollMarkerGroup                   => "scroll-marker-group",                  false, false, AsSpecified, "none";
    ScrollPaddingBlockEnd               => "scroll-padding-block-end",             false, false, AsSpecified, "auto";
    ScrollPaddingBlockStart             => "scroll-padding-block-start",           false, false, AsSpecified, "auto";
    ScrollPaddingBottom                 => "scroll-padding-bottom",                false, false, AsSpecified, "auto";
    ScrollPaddingInlineEnd              => "scroll-padding-inline-end",            false, false, AsSpecified, "auto";
    ScrollPaddingInlineStart            => "scroll-padding-inline-start",          false, false, AsSpecified, "auto";
    ScrollPaddingLeft                   => "scroll-padding-left",                  false, false, AsSpecified, "auto";
    ScrollPaddingRight                  => "scroll-padding-right",                 false, false, AsSpecified, "auto";
    ScrollPaddingTop                    => "scroll-padding-top",                   false, false, AsSpecified, "auto";
    ScrollSnapAlign                     => "scroll-snap-align",                    false, false, AsSpecified, "none";
    ScrollSnapStop                      => "scroll-snap-stop",                     false, false, AsSpecified, "normal";
    ScrollSnapType                      => "scroll-snap-type",                     false, false, AsSpecified, "none";
    ScrollTargetGroup                   => "scroll-target-group",                  false, false, AsSpecified, "none";
    ScrollTimelineAxis                  => "scroll-timeline-axis",                 false, false, AsSpecified, "block";
    ScrollTimelineName                  => "scroll-timeline-name",                 false, false, AsSpecified, "none";
    ScrollbarColor                      => "scrollbar-color",                      true,  false, AsSpecified, "auto";
    ScrollbarGutter                     => "scrollbar-gutter",                     false, false, AsSpecified, "auto";
    ScrollbarWidth                      => "scrollbar-width",                      false, false, AsSpecified, "auto";
    ShapeImageThreshold                 => "shape-image-threshold",                false, false, AsSpecified, "0";
    ShapeMargin                         => "shape-margin",                         false, false, AsSpecified, "0px";
    ShapeOutside                        => "shape-outside",                        false, false, AsSpecified, "none";
    ShapeRendering                      => "shape-rendering",                      true,  false, AsSpecified, "auto";
    Speak                               => "speak",                                true,  false, AsSpecified, "normal";
    StopColor                           => "stop-color",                           false, false, Color,       "rgb(0, 0, 0)";
    StopOpacity                         => "stop-opacity",                         false, false, AsSpecified, "1";
    Stroke                              => "stroke",                               true,  false, Color,       "none";
    StrokeDasharray                     => "stroke-dasharray",                     true,  false, AsSpecified, "none";
    StrokeDashoffset                    => "stroke-dashoffset",                    true,  false, AsSpecified, "0px";
    StrokeLinecap                       => "stroke-linecap",                       true,  false, AsSpecified, "butt";
    StrokeLinejoin                      => "stroke-linejoin",                      true,  false, AsSpecified, "miter";
    StrokeMiterlimit                    => "stroke-miterlimit",                    true,  false, AsSpecified, "4";
    StrokeOpacity                       => "stroke-opacity",                       true,  false, AsSpecified, "1";
    StrokeWidth                         => "stroke-width",                         true,  false, AsSpecified, "1px";
    TabSize                             => "tab-size",                             true,  true,  AsSpecified, "8";
    TableLayout                         => "table-layout",                         false, true,  Keyword,     "auto";
    TextAlign                           => "text-align",                           true,  true,  Keyword,     "start";
    TextAlignLast                       => "text-align-last",                      true,  false, AsSpecified, "auto";
    TextAnchor                          => "text-anchor",                          true,  false, AsSpecified, "start";
    TextAutospace                       => "text-autospace",                       true,  false, AsSpecified, "no-autospace";
    TextBoxEdge                         => "text-box-edge",                        true,  false, AsSpecified, "auto";
    TextBoxTrim                         => "text-box-trim",                        false, false, AsSpecified, "none";
    TextCombineUpright                  => "text-combine-upright",                 true,  false, AsSpecified, "none";
    TextDecoration                      => "text-decoration",                      false, false, AsSpecified, "none";
    TextDecorationColor                 => "text-decoration-color",                false, false, Color,       "currentcolor";
    TextDecorationLine                  => "text-decoration-line",                 false, false, Keyword,     "none";
    TextDecorationSkipInk               => "text-decoration-skip-ink",             true,  false, AsSpecified, "auto";
    TextDecorationStyle                 => "text-decoration-style",                false, false, Keyword,     "solid";
    TextDecorationThickness             => "text-decoration-thickness",            false, false, Lengthy,     "auto";
    TextEmphasisColor                   => "text-emphasis-color",                  true,  false, Color,       "currentcolor";
    TextEmphasisPosition                => "text-emphasis-position",               true,  false, AsSpecified, "over";
    TextEmphasisStyle                   => "text-emphasis-style",                  true,  false, AsSpecified, "none";
    TextFit                             => "text-fit",                             true,  false, AsSpecified, "none";
    TextIndent                          => "text-indent",                          true,  true,  Lengthy,     "0px";
    TextJustify                         => "text-justify",                         true,  false, AsSpecified, "auto";
    TextOrientation                     => "text-orientation",                     true,  false, AsSpecified, "mixed";
    TextOverflow                        => "text-overflow",                        false, false, AsSpecified, "clip";
    TextRendering                       => "text-rendering",                       true,  false, AsSpecified, "auto";
    TextShadow                          => "text-shadow",                          true,  false, AsSpecified, "none";
    TextSizeAdjust                      => "text-size-adjust",                     true,  false, AsSpecified, "auto";
    TextSpacingTrim                     => "text-spacing-trim",                    true,  false, AsSpecified, "normal";
    TextTransform                       => "text-transform",                       true,  true,  Keyword,     "none";
    TextUnderlineOffset                 => "text-underline-offset",                true,  false, AsSpecified, "auto";
    TextUnderlinePosition               => "text-underline-position",              true,  false, AsSpecified, "auto";
    TextWrapMode                        => "text-wrap-mode",                       true,  true,  Keyword,     "wrap";
    TextWrapStyle                       => "text-wrap-style",                      true,  false, AsSpecified, "auto";
    TimelineScope                       => "timeline-scope",                       false, false, AsSpecified, "none";
    TimelineTriggerActivationRangeEnd   => "timeline-trigger-activation-range-end",false, false, AsSpecified, "normal";
    TimelineTriggerActivationRangeStart => "timeline-trigger-activation-range-start",false, false, AsSpecified, "normal";
    TimelineTriggerActiveRangeEnd       => "timeline-trigger-active-range-end",    false, false, AsSpecified, "auto";
    TimelineTriggerActiveRangeStart     => "timeline-trigger-active-range-start",  false, false, AsSpecified, "auto";
    TimelineTriggerName                 => "timeline-trigger-name",                false, false, AsSpecified, "none";
    TimelineTriggerSource               => "timeline-trigger-source",              false, false, AsSpecified, "auto";
    Top                                 => "top",                                  false, true,  Lengthy,     "auto";
    TouchAction                         => "touch-action",                         false, false, AsSpecified, "auto";
    Transform                           => "transform",                            false, true,  AsSpecified, "none";
    TransformBox                        => "transform-box",                        false, false, AsSpecified, "view-box";
    TransformOrigin                     => "transform-origin",                     false, false, AsSpecified, "50% 50%";
    TransformStyle                      => "transform-style",                      false, false, AsSpecified, "flat";
    TransitionBehavior                  => "transition-behavior",                  false, false, AsSpecified, "normal";
    TransitionDelay                     => "transition-delay",                     false, false, AsSpecified, "0s";
    TransitionDuration                  => "transition-duration",                  false, false, AsSpecified, "0s";
    TransitionProperty                  => "transition-property",                  false, false, AsSpecified, "all";
    TransitionTimingFunction            => "transition-timing-function",           false, false, AsSpecified, "ease";
    Translate                           => "translate",                            false, false, AsSpecified, "none";
    TriggerScope                        => "trigger-scope",                        false, false, AsSpecified, "none";
    UnicodeBidi                         => "unicode-bidi",                         false, false, AsSpecified, "normal";
    UserSelect                          => "user-select",                          true,  false, Keyword,     "auto";
    VectorEffect                        => "vector-effect",                        false, false, AsSpecified, "none";
    VerticalAlign                       => "vertical-align",                       false, true,  Lengthy,     "baseline";
    ViewTimelineAxis                    => "view-timeline-axis",                   false, false, AsSpecified, "block";
    ViewTimelineInset                   => "view-timeline-inset",                  false, false, Lengthy,     "auto";
    ViewTimelineName                    => "view-timeline-name",                   false, false, AsSpecified, "none";
    ViewTransitionClass                 => "view-transition-class",                false, false, AsSpecified, "none";
    ViewTransitionGroup                 => "view-transition-group",                false, false, AsSpecified, "normal";
    ViewTransitionName                  => "view-transition-name",                 false, false, AsSpecified, "none";
    ViewTransitionScope                 => "view-transition-scope",                false, false, AsSpecified, "none";
    Visibility                          => "visibility",                           true,  false, Keyword,     "visible";
    WhiteSpaceCollapse                  => "white-space-collapse",                 true,  true,  Keyword,     "collapse";
    Widows                              => "widows",                               true,  false, AsSpecified, "2";
    Width                               => "width",                                false, true,  Lengthy,     "auto";
    WillChange                          => "will-change",                          false, false, AsSpecified, "auto";
    // Chrome 152 (`css_properties.json5`, `window-drag`): inherited, `none` or `move`.
    WindowDrag                          => "window-drag",                          true,  false, AsSpecified, "none";
    WordBreak                           => "word-break",                           true,  true,  Keyword,     "normal";
    WordSpacing                         => "word-spacing",                         true,  true,  Lengthy,     "0px";
    WritingMode                         => "writing-mode",                         true,  true,  Keyword,     "horizontal-tb";
    X                                   => "x",                                    false, false, AsSpecified, "0px";
    Y                                   => "y",                                    false, false, AsSpecified, "0px";
    ZIndex                              => "z-index",                              false, false, AsSpecified, "auto";
    Zoom                                => "zoom",                                 false, false, AsSpecified, "1";
    WebkitBorderHorizontalSpacing       => "-webkit-border-horizontal-spacing",    true,  false, AsSpecified, "0px";
    WebkitBorderImage                   => "-webkit-border-image",                 false, false, AsSpecified, "none";
    WebkitBorderVerticalSpacing         => "-webkit-border-vertical-spacing",      true,  false, AsSpecified, "0px";
    WebkitBoxAlign                      => "-webkit-box-align",                    false, false, AsSpecified, "stretch";
    WebkitBoxDecorationBreak            => "-webkit-box-decoration-break",         false, false, AsSpecified, "slice";
    WebkitBoxDirection                  => "-webkit-box-direction",                false, false, AsSpecified, "normal";
    WebkitBoxFlex                       => "-webkit-box-flex",                     false, false, AsSpecified, "0";
    WebkitBoxOrdinalGroup               => "-webkit-box-ordinal-group",            false, false, AsSpecified, "1";
    WebkitBoxOrient                     => "-webkit-box-orient",                   false, false, AsSpecified, "horizontal";
    WebkitBoxPack                       => "-webkit-box-pack",                     false, false, AsSpecified, "start";
    WebkitBoxReflect                    => "-webkit-box-reflect",                  false, false, AsSpecified, "none";
    WebkitFontSmoothing                 => "-webkit-font-smoothing",               true,  false, AsSpecified, "auto";
    WebkitLineBreak                     => "-webkit-line-break",                   false, false, AsSpecified, "auto";
    WebkitLineClamp                     => "-webkit-line-clamp",                   false, false, AsSpecified, "none";
    WebkitLocale                        => "-webkit-locale",                       true,  false, AsSpecified, "auto";
    WebkitMaskBoxImage                  => "-webkit-mask-box-image",               false, false, AsSpecified, "none";
    WebkitMaskBoxImageOutset            => "-webkit-mask-box-image-outset",        false, false, AsSpecified, "0";
    WebkitMaskBoxImageRepeat            => "-webkit-mask-box-image-repeat",        false, false, AsSpecified, "stretch";
    WebkitMaskBoxImageSlice             => "-webkit-mask-box-image-slice",         false, false, AsSpecified, "0 fill";
    WebkitMaskBoxImageSource            => "-webkit-mask-box-image-source",        false, false, AsSpecified, "none";
    WebkitMaskBoxImageWidth             => "-webkit-mask-box-image-width",         false, false, AsSpecified, "auto";
    WebkitMaskPositionX                 => "-webkit-mask-position-x",              false, false, Lengthy,     "0%";
    WebkitMaskPositionY                 => "-webkit-mask-position-y",              false, false, Lengthy,     "0%";
    WebkitRtlOrdering                   => "-webkit-rtl-ordering",                 true,  false, AsSpecified, "logical";
    WebkitRubyPosition                  => "-webkit-ruby-position",                true,  false, AsSpecified, "before";
    WebkitTapHighlightColor             => "-webkit-tap-highlight-color",          true,  false, Color,       "rgba(0, 0, 0, 0.18)";
    WebkitTextCombine                   => "-webkit-text-combine",                 true,  false, AsSpecified, "none";
    WebkitTextDecorationsInEffect       => "-webkit-text-decorations-in-effect",   true,  false, AsSpecified, "none";
    WebkitTextFillColor                 => "-webkit-text-fill-color",              true,  false, Color,       "currentcolor";
    WebkitTextOrientation               => "-webkit-text-orientation",             true,  false, AsSpecified, "vertical-right";
    WebkitTextSecurity                  => "-webkit-text-security",                true,  false, AsSpecified, "none";
    WebkitTextStrokeColor               => "-webkit-text-stroke-color",            true,  false, Color,       "currentcolor";
    WebkitTextStrokeWidth               => "-webkit-text-stroke-width",            true,  false, Lengthy,     "0px";
    WebkitUserDrag                      => "-webkit-user-drag",                    false, false, AsSpecified, "auto";
    WebkitUserModify                    => "-webkit-user-modify",                  true,  false, AsSpecified, "read-only";
    WebkitWritingMode                   => "-webkit-writing-mode",                 true,  false, AsSpecified, "horizontal-tb";

    // ── past this point: properties the cascade knows and `getComputedStyle` does NOT enumerate ──
    //
    // Chrome 152 enumerates 476 properties and these four are not among them. `border-spacing` is a shorthand there
    // over two private longhands (both of which ARE enumerated, above); `background-position-x`/`-y` are the
    // longhands its `background-position` shorthand writes, and here too (`style::shorthand`: the cascade keeps
    // the halves, and `BackgroundPosition` is never set -- `getComputedStyle` writes it from them); `page` Blink
    // leaves out on purpose. They stay here because the CASCADE needs them, and `ENUMERATED` keeps them out of
    // what a page lists -- though `getComputedStyle` answers each of them by name (`border-spacing` as Blink's shorthand).
    BorderSpacing                       => "border-spacing",                       true,  true,  Lengthy,     "0px 0px";
    BackgroundPositionX                 => "background-position-x",                false, false, Lengthy,     "0%";
    BackgroundPositionY                 => "background-position-y",                false, false, Lengthy,     "0%";
    // `page` (CSS Paged Media): Blink computes it and does not enumerate it either (`kIntentionallyNotComputable`);
    // Chrome 150 answers `auto` for an unset one. Nothing here paginates.
    Page                                => "page",                                 false, false, AsSpecified, "auto";
}

/// Does this computed [`PropertyId::Float`] value take the box OUT OF FLOW?
///
/// A WHITELIST, and deliberately not `value != "none"`. The `Keyword` computed kind
/// lowercases a value and does not validate it, so this engine's computed style carries
/// `float: bogus` through where Chrome 150 drops the declaration and computes `none`. Every reader
/// of `float` that used `!= "none"` would then treat a typo as a float -- measured: with `!= "none"`,
/// `<span style="float:bogus">` blockified to `display: block` here and stayed `inline` in Chrome.
/// Naming the four values that mean "floated" makes every such reader agree with Chrome on an
/// invalid one without the cascade having to learn to validate keywords, which is a much larger job
/// (`Computed::Keyword` covers ~90 properties) and is not this unit's.
///
/// The four are measured, not assumed: Chrome 150 on 2026-09-14 computes `left`, `right`,
/// `inline-start` and `inline-end` through unchanged, blockifies the box for all four, and
/// shrink-to-fits all four to the same 27px that `float: left` gets.
pub fn is_floated(value: &str) -> bool {
    matches!(value.trim(), "left" | "right" | "inline-start" | "inline-end")
}

impl PropertyId {
    /// Whether a transition runs on this property without `transition-behavior: allow-discrete`: Blink can
    /// interpolate its values, itself or through the property it resolves to (`blink_names::transitionable`, generated
    /// from `css_properties.json5`). Every other property is DISCRETE for every pair of values -- `float`, `display`,
    /// `position` -- and Chrome 150 starts no transition on it: `transition: all` leaves it out, and naming it starts
    /// nothing.
    pub fn is_interpolable(self) -> bool {
        crate::style::blink_names::transitionable(self.name())
    }
}


/// A property name as the cascade knows it: lowercase, and an ALIAS's target in place of the alias
/// (`-webkit-transform` -> `transform`), read off Blink's alias table (`blink_names`). A name Blink does
/// not list keeps its own spelling.
pub fn canonical_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let names = crate::style::blink_names::BLINK_CSS_NAMES;
    match names.binary_search_by(|(n, _)| (*n).cmp(lower.as_str())) {
        Ok(i) => names[i].1.unwrap_or(names[i].0).to_string(),
        Err(_) => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_discrete_property_is_not_interpolable_and_a_logical_one_follows_its_physical_one() {
        for p in [PropertyId::Opacity, PropertyId::Width, PropertyId::Color, PropertyId::Visibility, PropertyId::Transform, PropertyId::BlockSize, PropertyId::BackgroundPosition] {
            assert!(p.is_interpolable(), "{}", p.name());
        }
        for p in [PropertyId::Float, PropertyId::Display, PropertyId::Position, PropertyId::BorderBlockStartStyle] {
            assert!(!p.is_interpolable(), "{}", p.name());
        }
        let list = crate::style::blink_names::BLINK_TRANSITIONABLE;
        assert!(list.windows(2).all(|w| w[0] < w[1]), "BLINK_TRANSITIONABLE must stay sorted for its binary search");
    }

    #[test]
    fn the_index_is_dense_and_matches_the_table() {
        assert_eq!(COUNT, ALL.len());
        for (i, &p) in ALL.iter().enumerate() {
            assert_eq!(p.index(), i, "{} is not at its own index", p.name());
        }
    }

    #[test]
    fn names_are_unique_and_round_trip() {
        let mut seen = HashSet::new();
        for &p in ALL {
            assert!(seen.insert(p.name()), "duplicate property name {:?}", p.name());
            assert_eq!(PropertyId::from_name(p.name()), Some(p));
        }
        assert_eq!(seen.len(), ALL.len());
    }

    #[test]
    fn lookup_is_ascii_case_insensitive() {
        assert_eq!(PropertyId::from_name("COLOR"), Some(PropertyId::Color));
        assert_eq!(PropertyId::from_name("Background-Color"), Some(PropertyId::BackgroundColor));
        assert_eq!(PropertyId::from_name("color"), Some(PropertyId::Color));
    }

    #[test]
    fn an_unknown_property_is_absent_not_an_error() {
        assert_eq!(PropertyId::from_name("-moz-box-sizing"), None);
        assert_eq!(PropertyId::from_name("--custom"), None, "custom properties are v1.5 (D12)");
        assert_eq!(PropertyId::from_name(""), None);
        // Shorthands are a parse-time expansion, never a stored property.
        assert_eq!(PropertyId::from_name("margin"), None);
        assert_eq!(PropertyId::from_name("font"), None);
    }

    // Every name is lowercase and hyphenated; anything else would never be found by `from_name`,
    // which lowercases before it looks.
    #[test]
    fn every_name_is_in_the_form_the_lookup_can_find() {
        for &p in ALL {
            let n = p.name();
            assert!(!n.is_empty());
            assert!(
                n.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
                "{n:?} is not a lowercase hyphenated name"
            );
        }
    }

    // Spot-checks of the inheritance flag, which is the fact that fails silently. These are the
    // cases where getting it backwards produces a plausible-looking page that is subtly wrong.
    #[test]
    fn inheritance_follows_whether_a_property_describes_text_or_a_box() {
        for p in [PropertyId::Color, PropertyId::FontSize, PropertyId::LineHeight, PropertyId::Visibility] {
            assert!(p.is_inherited(), "{} describes text or is explicitly inherited", p.name());
        }
        for p in [PropertyId::Display, PropertyId::Width, PropertyId::MarginTop, PropertyId::BackgroundColor] {
            assert!(!p.is_inherited(), "{} describes this box only", p.name());
        }
    }

    // The initial *computed* value, not the spec's initial: the initial border-style is `none`, and
    // a border with no style has no width, so Chrome reports 0px rather than medium's 3px.
    //
    // These are RECORDED from the Brave on this machine, one property at a time with
    // `style.setProperty(name, "initial")`. `outline-width` is the one that does NOT follow
    // border-width's rule: `medium` stays 3px even with `outline-style: none`.
    //
    // `min-width` and `min-height` are where the recording misled: it read `0px`, this assertion said
    // so, and every flex item lost its automatic minimum size to it. The probe
    // box was not a flex item, and `getComputedStyle` answers `0px` for exactly those -- Blink's
    // `MinWidthOrMinHeightAuto` -- while the COMPUTED value is `auto` (its `kComputedValue` phase says
    // so, and a flex item reports `auto`, measured on Chrome 150). The table holds the computed value;
    // the resolved one is `Page::resolve_at_read`'s.
    #[test]
    fn initial_values_are_the_computed_ones_chrome_reports() {
        // `medium`, which reads `0px` while the side's style is `none` (`StyleArena::computed_text`)
        assert_eq!(PropertyId::BorderTopWidth.initial(), "3px");
        assert_eq!(PropertyId::Color.initial(), "rgb(0, 0, 0)");
        // computed as itself, read as the element's `color` (`StyleArena::value`)
        assert_eq!(PropertyId::BorderTopColor.initial(), "currentcolor");
        assert_eq!(PropertyId::BackgroundColor.initial(), "rgba(0, 0, 0, 0)");
        assert_eq!(PropertyId::Display.initial(), "inline", "the UA sheet is what makes divs block");
        for p in [PropertyId::MinWidth, PropertyId::MinHeight, PropertyId::MinInlineSize, PropertyId::MinBlockSize] {
            assert_eq!(p.initial(), "auto", "{}: the computed initial; `0px` is a non-item's RESOLVED value", p.name());
        }
        assert_eq!(PropertyId::OutlineWidth.initial(), "3px", "outline keeps medium; border does not");
        // ...and the ones the recording could not answer, because their resolved value is the used
        // value and the probe box answered for them.
        assert_eq!(PropertyId::InlineSize.initial(), "auto");
        // The control answers `auto` for an element with no used value -- a `display: none` one, or
        // an inline. The `0px` that used to stand here was a probe box's height, not an initial.
        assert_eq!(PropertyId::BlockSize.initial(), "auto");
        assert_eq!(PropertyId::TransformOrigin.initial(), "50% 50%");
    }

    // Not a value: a placeholder with an owner. An empty computed font-family is itself a tell, so
    // this test exists to fail loudly if P1 ships without filling it from the persona.
    #[test]
    fn font_family_has_no_initial_because_the_persona_owns_it() {
        assert_eq!(PropertyId::FontFamily.initial(), "");
        assert_eq!(PropertyId::FontSize.initial(), "16px", "Chrome's default, and a user preference");
    }

    // D7: the split exists so layout touches a compact struct instead of a property bag. If a
    // property layout reads were mis-flagged, it would end up in the string tail and layout would
    // parse it back out of a string on every pass.
    #[test]
    fn the_layout_split_puts_geometry_on_one_side_and_paint_on_the_other() {
        for p in [PropertyId::Display, PropertyId::Width, PropertyId::FontSize, PropertyId::FlexGrow] {
            assert!(p.affects_layout(), "{} changes where boxes go", p.name());
        }
        for p in [PropertyId::Color, PropertyId::BackgroundColor, PropertyId::Cursor, PropertyId::BoxShadow] {
            assert!(!p.affects_layout(), "{} never moves a box", p.name());
        }
    }
}
