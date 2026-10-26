//! Generate behavior-case SKELETONS from WebIDL — `{iface, member, expr}` with no `expected`.
//!
//! The agent-authored corpus has a coverage hole that no amount of agent effort closes: the agent
//! chooses which expressions to write, so a member it did not think to exercise passes by not being
//! tested. the capture rig took away its power to decide the ANSWER; this takes away its
//! power to decide the QUESTION.
//!
//! The key simplification is that the answer comes from a real browser. A generated argument does not
//! have to be *correct* — if `new DOMRect("x")` throws in Chrome, "it throws `TypeError`" is a perfectly
//! good recorded expectation, and one an implementer is unusually likely to get wrong. All the
//! generator owes is an expression that is DETERMINISTIC; the capture tool enforces that empirically by
//! evaluating each one twice and discarding any that disagrees with itself.
//!
//! Reaching a receiver is the whole difficulty. Requiring a declared `constructor` covered the
//! geometry and fetch corners and almost nothing else: `Element` had 1 of its 154 members questioned,
//! `HTMLInputElement` 1 of 61, `Navigator` 1 of 69 -- because `new Element()` throws, so every
//! instance case was dropped and the surface passed by not being asked. Those are not edge cases;
//! they are the DOM. [`receiver_expr`] closes that with recipes: the element tables generated from
//! Blink give `document.createElement("input")`, and a short vetted list gives the objects a page
//! hands you rather than builds (`document`, `navigator`, `document.createRange()`).
//!
//! A recipe is a claim about the browser, and a wrong one is worse than a missing one -- it records
//! a stable `throws TypeError` as the expected answer and then demands Korvus reproduce it. So the
//! capture tool probes each receiver against the real browser (`o instanceof <Iface>`) before
//! recording anything filed under it. A skeleton goes into `tests/fixtures/behavior-cases/` as it
//! is -- no `expected` -- and the next capture answers it: `emit.py --role oracle` fills every entry
//! that has a question and no answer, whoever wrote the question.

use crate::spec::{Arg, InterfaceSpec, MemberSpec};

/// A generated case with no expectation yet — the next capture supplies that (see the module note).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CaseSkeleton {
    pub iface: String,
    pub member: String,
    pub expr: String,
    /// The receiver expression `expr` reaches this member through, for instance members only.
    ///
    /// Carried separately rather than parsed back out of `expr`, because a recipe can itself contain
    /// the delimiters a parser would key on (`Attr`'s is a whole IIFE with its own `; return`). The
    /// capture tool probes it against the real browser -- `<recv> instanceof <Iface>` -- and files no
    /// answer under a receiver that did not check out. Dropped on the way to the corpus: it exists to
    /// be verified, not to be recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recv: Option<String>,
    /// The answer arrives ASYNCHRONOUSLY: `expr` evaluates to a promise and the recorded answer is
    /// what it SETTLES to. Honoured by `behavior-run.js`, which awaits only the cases carrying it.
    ///
    /// Written as `await` on disk so the corpus and the skeleton spell the flag the same way; see
    /// `oracle::BehaviorCase::awaited` for why it is opt-in per case and never global.
    #[serde(rename = "await", skip_serializing_if = "std::ops::Not::not")]
    pub awaited: bool,
    /// The RECEIVER itself arrives as a promise, so the capture tool's `instanceof` probe has to
    /// await it before it can mean anything.
    ///
    /// Carried separately from `awaited` because the two are independent and conflating them is a
    /// real bug: `navigator.getBattery()` is an awaited case on a receiver (`navigator`) that is not
    /// a promise, while `XRSession.visibilityState` is a plain attribute read on a receiver that is.
    /// A probe that awaits when it should not still answers correctly (`await` on a non-promise is a
    /// no-op); one that does NOT await when it should answers `false` and silently drops every case
    /// filed under the recipe -- which is exactly how ~46 XR members came to look unimplementable.
    #[serde(rename = "recvAwait", skip_serializing_if = "std::ops::Not::not")]
    pub recv_await: bool,
}

/// A JS literal that a WebIDL type will accept, or `None` when the type needs a value we cannot spell.
///
/// Deliberately ONE literal per type rather than a matrix: the point is to reach the member at all.
/// Edge-case coercion (a string where a double is declared, a missing required argument) is generated
/// separately by [`coercion_probes`], where being wrong is the whole idea.
fn literal_for(ty: &str) -> Option<&'static str> {
    let t = ty.trim().trim_end_matches('?').trim();
    if t.starts_with("sequence<") || t.starts_with("FrozenArray<") || t.starts_with("ObservableArray<") {
        return Some("[]");
    }
    if t.starts_with("record<") {
        return Some("{}");
    }
    Some(match t {
        "DOMString" | "USVString" | "ByteString" | "CSSOMString" => "\"x\"",
        "boolean" => "true",
        "byte" | "octet" | "short" | "unsigned short" | "long" | "unsigned long" | "long long"
        | "unsigned long long" => "1",
        "float" | "unrestricted float" | "double" | "unrestricted double" => "1",
        "any" => "1",
        "object" => "{}",
        // A dictionary is spelled as a bare interface-ish name; an empty object satisfies one whose
        // members are all optional, and throws otherwise -- which is itself a recordable answer.
        _ if t.ends_with("Init") || t.ends_with("Options") || t.ends_with("Dict") => "{}",
        _ => return None,
    })
}

/// The arguments a call needs: every required argument, and nothing more. Returns `None` when any
/// required argument has a type we cannot spell.
fn required_args(args: &[Arg]) -> Option<String> {
    let mut out = Vec::new();
    for a in args {
        if a.optional || a.variadic {
            break; // optional arguments are exercised by their own probe below
        }
        out.push(literal_for(&a.ty)?);
    }
    Some(out.join(", "))
}

/// `new Iface(<required args>)`, when the interface can be constructed at all.
fn constructor_expr(spec: &InterfaceSpec) -> Option<String> {
    let ctor = spec.members.iter().find_map(|m| match m {
        // `[HTMLConstructor]` is declared by every HTML element interface and constructs nothing:
        // outside a custom-element upgrade it throws "Illegal constructor". Treating it as a receiver
        // gave `HTMLInputElement.value` the expectation "throws TypeError" -- a statement about the
        // constructor filed under a member it never reached.
        MemberSpec::Constructor { args, html_constructor: false } => Some(args),
        _ => None,
    })?;
    Some(format!("new {}({})", spec.name, required_args(ctor)?))
}

/// Objects a page HANDS you rather than lets you build: no constructor, no element tag.
///
/// Every entry is a claim that this expression yields an instance of that interface in a real
/// browser, and the capture tool checks each one before recording an answer under it -- so a wrong
/// line here costs an interface its cases, never a false expectation. Entries are deliberately the
/// plainest expression that works, because the string becomes the corpus key an answer is filed
/// under and a fussier one would churn the corpus for no gain.
///
/// Disjoint from the element tables by construction (nothing here is an element interface); a test
/// below holds that, so the resolution order in [`receiver_expr`] never has to be argued about.
static SINGLETONS: &[(&str, &str)] = &[
    // The globals and the objects hanging off them.
    ("Window", "window"),
    ("Document", "document"),
    ("Navigator", "navigator"),
    ("Location", "location"),
    ("History", "history"),
    ("Screen", "screen"),
    ("Performance", "performance"),
    ("Crypto", "crypto"),
    ("Console", "console"),
    ("CustomElementRegistry", "customElements"),
    ("Storage", "localStorage"),
    // The node types with a factory instead of a constructor. `Element`/`Node`/`CharacterData` are
    // abstract: any concrete instance is one, and a div is the least surprising choice.
    ("Element", "document.createElement(\"div\")"),
    ("Node", "document.createElement(\"div\")"),
    ("EventTarget", "document.createElement(\"div\")"),
    ("CharacterData", "document.createTextNode(\"x\")"),
    ("Text", "document.createTextNode(\"x\")"),
    ("Comment", "document.createComment(\"x\")"),
    ("DocumentFragment", "document.createDocumentFragment()"),
    ("DocumentType", "document.doctype"),
    ("DOMImplementation", "document.implementation"),
    ("ShadowRoot", "document.createElement(\"div\").attachShadow({ mode: \"open\" })"),
    // Live views and lists. These have no constructor by design -- the DOM owns them.
    // Each of these BUILDS ITS OWN subtree rather than querying the document.
    // `document.querySelectorAll(\"*\")` reaches an instance just as well, but then `length`
    // answers "how many elements were on the page the capture ran against" -- a fact about that
    // page, recorded as though it were a fact about the member. A detached two-element div gives
    // the same interface and an answer that is the same everywhere.
    ("NodeList", "(() => { const d = document.createElement(\"div\"); d.innerHTML = \"<i></i><b></b>\"; return d.querySelectorAll(\"*\"); })()"),
    ("HTMLCollection", "(() => { const d = document.createElement(\"div\"); d.innerHTML = \"<i></i><b></b>\"; return d.getElementsByTagName(\"*\"); })()"),
    ("NamedNodeMap", "document.createElement(\"div\").attributes"),
    ("DOMStringMap", "document.createElement(\"div\").dataset"),
    ("DOMTokenList", "document.createElement(\"div\").classList"),
    ("CSSStyleDeclaration", "document.createElement(\"div\").style"),
    // Empty on any bare document, which is at least the same answer everywhere --
    // unlike a COUNT of the capture page's sheets would be.
    ("StyleSheetList", "document.styleSheets"),
    // An Attr is only reachable THROUGH an element that already has the attribute set.
    (
        "Attr",
        "(() => { const e = document.createElement(\"div\"); e.setAttribute(\"a\", \"b\"); return e.getAttributeNode(\"a\"); })()",
    ),
    // Reachable only as a property of something else, which is exactly why no author writes them.
    ("FontFaceSet", "document.fonts"),
    ("Navigation", "navigation"),
    ("PerformanceTiming", "performance.timing"),
    ("PerformanceNavigation", "performance.navigation"),
    ("VisualViewport", "visualViewport"),
    ("StylePropertyMap", "document.createElement(\"div\").attributeStyleMap"),
    ("MediaQueryList", "matchMedia(\"(min-width: 1px)\")"),
    ("CSSStyleSheet", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"a{color:red}\"); return s; })()"),
    ("CSSRuleList", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"a{color:red}\"); return s.cssRules; })()"),
    ("CSSStyleRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"a{color:red}\"); return s.cssRules[0]; })()"),
    // Traversal: constructed by the document, never with `new`.
    ("Range", "document.createRange()"),
    ("NodeIterator", "document.createNodeIterator(document.documentElement)"),
    ("TreeWalker", "document.createTreeWalker(document.documentElement)"),
    ("Selection", "getSelection()"),

    // ── recipes added when the corpus was measured and 343 reachable interfaces had NO behavior
    // evidence at all. Almost none of them was broken: the generator simply could not reach a
    // receiver, so every instance member passed by never being asked. Each line below was checked
    // against the Brave 151 on this machine before it was written down -- the probe evaluated it and
    // asked `o instanceof <Iface>` -- and the seven candidates that came back `null` or as some other
    // interface were dropped rather than guessed at.
    // Objects a page reaches through another object, never with `new`.
    ("ValidityState", "document.createElement(\"input\").validity"),
    ("TextMetrics", "document.createElement(\"canvas\").getContext(\"2d\").measureText(\"x\")"),
    ("SubtleCrypto", "crypto.subtle"),
    ("ElementInternals", "(() => { const n = \"x-\" + \"probe\"; if (!customElements.get(n)) customElements.define(n, class extends HTMLElement { constructor() { super(); this.i = this.attachInternals(); } }); return document.createElement(n).i; })()"),
    ("CustomStateSet", "(() => { const n = \"x-\" + \"states\"; if (!customElements.get(n)) customElements.define(n, class extends HTMLElement { constructor() { super(); this.i = this.attachInternals(); } }); return document.createElement(n).i.states; })()"),
    ("StorageManager", "navigator.storage"),
    ("Permissions", "navigator.permissions"),
    ("Clipboard", "navigator.clipboard"),
    ("Scheduling", "navigator.scheduling"),
    ("UserActivation", "navigator.userActivation"),
    ("NavigatorUAData", "navigator.userAgentData"),
    ("PluginArray", "navigator.plugins"),
    ("MimeTypeArray", "navigator.mimeTypes"),
    ("Plugin", "navigator.plugins[0]"),
    ("MimeType", "navigator.mimeTypes[0]"),
    ("CookieStore", "cookieStore"),
    ("CacheStorage", "caches"),
    ("BarProp", "locationbar"),
    ("DOMRectList", "(() => { const d = document.createElement(\"div\"); return d.getClientRects(); })()"),
    ("DataTransferItemList", "new DataTransfer().items"),
    ("MessagePort", "new MessageChannel().port1"),
    ("AbstractRange", "document.createRange()"),
    // Constructible, but with an argument `literal_for` cannot spell -- a callback, a dictionary, a node.
    ("IntersectionObserver", "new IntersectionObserver(() => {})"),
    ("ResizeObserver", "new ResizeObserver(() => {})"),
    ("MutationObserver", "new MutationObserver(() => {})"),
    ("PerformanceObserver", "new PerformanceObserver(() => {})"),
    ("Path2D", "new Path2D()"),
    ("URLPattern", "new URLPattern({ pathname: \"/a\" })"),
    ("FontFace", "new FontFace(\"probe\", \"url(data:,)\")"),
    ("XPathEvaluator", "new XPathEvaluator()"),
    ("Request", "new Request(\"http://x/a\")"),
    ("Response", "new Response(\"x\")"),
    ("ReadableStream", "new ReadableStream()"),
    ("WritableStream", "new WritableStream()"),
    ("TransformStream", "new TransformStream()"),
    ("Headers", "new Headers()"),
    ("DataTransfer", "new DataTransfer()"),
    ("Touch", "new Touch({ identifier: 1, target: document.documentElement })"),
    ("StaticRange", "(() => { const t = document.createTextNode(\"ab\"); return new StaticRange({ startContainer: t, startOffset: 0, endContainer: t, endOffset: 1 }); })()"),
    ("TextDecoderStream", "new TextDecoderStream()"),
    ("TextEncoderStream", "new TextEncoderStream()"),
    ("CompressionStream", "new CompressionStream(\"gzip\")"),
    ("DecompressionStream", "new DecompressionStream(\"gzip\")"),
    ("MessageChannel", "new MessageChannel()"),
    ("DOMRectReadOnly", "new DOMRectReadOnly(1, 2, 3, 4)"),
    ("CSSKeywordValue", "new CSSKeywordValue(\"auto\")"),
    ("IDBKeyRange", "IDBKeyRange.only(1)"),
    ("IDBFactory", "indexedDB"),
    ("AbortSignal", "new AbortController().signal"),
    ("ReadableStreamDefaultReader", "new ReadableStream().getReader()"),
    // The CSSOM rule types. Each one needs the at-rule that produces it, parsed into a sheet.
    ("CSSMediaRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@media screen { a { color: red } }\"); return s.cssRules[0]; })()"),
    ("CSSSupportsRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@supports (color: red) { a { color: red } }\"); return s.cssRules[0]; })()"),
    ("CSSKeyframesRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@keyframes k { from { color: red } }\"); return s.cssRules[0]; })()"),
    ("CSSKeyframeRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@keyframes k { from { color: red } }\"); return s.cssRules[0].cssRules[0]; })()"),
    ("CSSFontFaceRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@font-face { font-family: a; src: url(data:,) }\"); return s.cssRules[0]; })()"),
    ("CSSCounterStyleRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@counter-style c { system: cyclic; symbols: x; suffix: '.' }\"); return s.cssRules[0]; })()"),
    ("CSSPageRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@page { margin: 1px }\"); return s.cssRules[0]; })()"),
    ("CSSPropertyRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@property --p { syntax: '*'; inherits: false; }\"); return s.cssRules[0]; })()"),
    ("CSSLayerBlockRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@layer a { b { color: red } }\"); return s.cssRules[0]; })()"),
    ("CSSContainerRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@container (min-width: 1px) { a { color: red } }\"); return s.cssRules[0]; })()"),
    ("CSSPositionTryRule", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@position-try --p { top: 0px }\"); return s.cssRules[0]; })()"),
    ("CSSPositionTryDescriptors", "(() => { const s = new CSSStyleSheet(); s.replaceSync(\"@position-try --p { top: 0px }\"); return s.cssRules[0].style; })()"),
    ("StylePropertyMapReadOnly", "document.createElement(\"div\").computedStyleMap()"),
    ("CSSUnitValue", "CSS.px(1)"),
    // Web Animations: the objects `element.animate()` hands back.
    ("Animation", "document.createElement(\"div\").animate([{ opacity: 0 }], 1)"),
    ("KeyframeEffect", "document.createElement(\"div\").animate([{ opacity: 0 }], 1).effect"),
    ("AnimationEffect", "document.createElement(\"div\").animate([{ opacity: 0 }], 1).effect"),
    ("AnimationTimeline", "document.timeline"),
    ("DocumentTimeline", "document.timeline"),
    // Timing entries. `navigation` is the one entry type every document has.
    ("PerformanceNavigationTiming", "performance.getEntriesByType(\"navigation\")[0]"),
    ("PerformanceEntry", "performance.getEntriesByType(\"navigation\")[0]"),
    // XPath: an evaluator, a compiled expression, and a result.
    ("XPathExpression", "document.createExpression(\"//a\")"),
    ("XPathResult", "document.evaluate(\"//a\", document, null, 0, null)"),
    // SVG's value types. None is constructible; each is reached through an element that owns one, and the
    // `createSVG*` factories on `<svg>` are the spec's own way to get the free-standing ones.
    ("SVGTransformList", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").transform.baseVal"),
    ("SVGTransform", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").createSVGTransform()"),
    ("SVGLength", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").createSVGLength()"),
    ("SVGAngle", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").createSVGAngle()"),
    ("SVGNumber", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").createSVGNumber()"),
    ("SVGPoint", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").createSVGPoint()"),
    ("SVGRect", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").createSVGRect()"),
    ("SVGMatrix", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").createSVGMatrix()"),
    ("SVGAnimatedLength", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"rect\").width"),
    ("SVGAnimatedString", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"a\").href"),
    ("SVGAnimatedNumber", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"feGaussianBlur\").stdDeviationX"),
    ("SVGLengthList", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"text\").x.baseVal"),
    ("SVGAnimatedLengthList", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"text\").x"),
    ("SVGStringList", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").requiredExtensions"),
    ("SVGPointList", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"polygon\").points"),
    ("SVGAnimatedRect", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").viewBox"),
    ("SVGAnimatedEnumeration", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"feBlend\").mode"),
    ("SVGAnimatedTransformList", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"g\").transform"),
    ("SVGAnimatedPreserveAspectRatio", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").preserveAspectRatio"),
    ("SVGPreserveAspectRatio", "document.createElementNS(\"http://www.w3.org/2000/svg\", \"svg\").preserveAspectRatio.baseVal"),
    ("TouchList", "new TouchEvent(\"touchstart\", { touches: [new Touch({ identifier: 1, target: document.documentElement })] }).touches"),

    // ── device-family entry points (goal-webapi-max Track 1.1). None of these has a WebIDL
    // constructor; a page reaches them only through `navigator.<member>` or a bare global. Each line
    // below was checked against Brave 151 on this machine -- `o instanceof <Iface>` evaluated true --
    // before being written down. `navigator.bluetooth` and `Gamepad`/`MIDIAccess` are deliberately NOT
    // here: the recorded browser answers `navigator.bluetooth` `undefined` (Web Bluetooth ships off on
    // Linux) and there is no device to make a `Gamepad` or a granted `MIDIAccess` real; see the brief's
    // NOTES.
    ("MediaDevices", "navigator.mediaDevices"),
    ("XRSystem", "navigator.xr"),
    ("USB", "navigator.usb"),
    ("HID", "navigator.hid"),
    ("Serial", "navigator.serial"),
    ("GPU", "navigator.gpu"),
    ("WakeLock", "navigator.wakeLock"),
    ("Geolocation", "navigator.geolocation"),
    ("CredentialsContainer", "navigator.credentials"),
    ("MediaSession", "navigator.mediaSession"),
    ("ServiceWorkerContainer", "navigator.serviceWorker"),
    // `[SameObject] readonly attribute SpeechSynthesis speechSynthesis` on Window -- an own property of
    // the global, not reached through `navigator`.
    ("SpeechSynthesis", "speechSynthesis"),
    // WebRTC objects a peer connection hands back rather than lets a page construct: no device, no
    // remote peer and no signaling needed for any of the four -- `addTransceiver`/`createDataChannel`
    // only touch the connection's own local state.
    ("RTCRtpTransceiver", "new RTCPeerConnection().addTransceiver(\"audio\")"),
    ("RTCRtpSender", "new RTCPeerConnection().addTransceiver(\"audio\").sender"),
    ("RTCRtpReceiver", "new RTCPeerConnection().addTransceiver(\"audio\").receiver"),
    ("RTCDataChannel", "new RTCPeerConnection().createDataChannel(\"x\")"),

    // ── the device families' SECOND pass (goal-webapi-max Track 1.1, re-run). The first pass took the
    // eleven `navigator.<x>` entry points; this one takes the objects those families hand back, which
    // is where the members actually live. Measured before starting: 149 device-family interfaces with
    // own instance members still had NO receiver at all, 789 members behind them, and `gencases` was
    // emitting one constructor probe apiece and stopping.
    //
    // Every line below was evaluated in a real headless Brave 151 on this machine BEFORE it was
    // written down -- one batch probe, `o instanceof <Iface>` per candidate -- and the sixteen
    // candidates that came back `null`, `undefined`, a different interface or a bare ReferenceError
    // are NOT here. They are recorded in the brief's NOTES instead, because each is a finding about
    // what this control browser can be honestly asked, not a gap to be papered over.
    //
    // Note what the vetting caught: `document.createElement("canvas").captureStream()
    // .getVideoTracks()[0]` is NOT a `BrowserCaptureMediaStreamTrack` (it is a
    // `CanvasCaptureMediaStreamTrack`), and writing it down for the former -- which reads entirely
    // plausible -- would have recorded 3 answers about the wrong object.

    // Web Audio's graph. An OfflineAudioContext needs no sound card, no output device and no user
    // gesture, so the whole node/param graph is reachable on a headless box with no audio hardware --
    // which is the same reason the brief gives for `enumerateDevices()` answering `[]`.
    ("BaseAudioContext", "new OfflineAudioContext(1, 1, 44100)"),
    ("AudioNode", "new OfflineAudioContext(1, 1, 44100).destination"),
    ("AudioDestinationNode", "new OfflineAudioContext(1, 1, 44100).destination"),
    ("AudioListener", "new OfflineAudioContext(1, 1, 44100).listener"),
    ("AudioParam", "new GainNode(new OfflineAudioContext(1, 1, 44100)).gain"),
    ("AudioScheduledSourceNode", "new OscillatorNode(new OfflineAudioContext(1, 1, 44100))"),
    ("AudioWorklet", "new OfflineAudioContext(1, 1, 44100).audioWorklet"),
    // Deprecated, factory-only: `ScriptProcessorNode` has no constructor in any spec version.
    ("ScriptProcessorNode", "new OfflineAudioContext(1, 1, 44100).createScriptProcessor(256, 1, 1)"),

    // A media track without a camera or a microphone: a canvas is a capture source, and the track it
    // yields is a real `MediaStreamTrack` with a real `kind`, `readyState` and `getSettings()`.
    ("MediaStreamTrack", "document.createElement(\"canvas\").captureStream().getVideoTracks()[0]"),
    ("CanvasCaptureMediaStreamTrack", "document.createElement(\"canvas\").captureStream().getVideoTracks()[0]"),
    // The text-track graph hangs off any <video>; none of it needs a loaded media resource.
    ("TextTrack", "document.createElement(\"video\").addTextTrack(\"subtitles\")"),
    ("TextTrackList", "document.createElement(\"video\").textTracks"),
    ("TextTrackCueList", "document.createElement(\"video\").addTextTrack(\"subtitles\").cues"),
    ("TextTrackCue", "new VTTCue(0, 1, \"a\")"),
    ("VideoPlaybackQuality", "document.createElement(\"video\").getVideoPlaybackQuality()"),
    ("SourceBufferList", "new MediaSource().sourceBuffers"),
    ("ChapterInformation", "new MediaMetadata({ chapterInfo: [{ title: \"a\", startTime: 0 }] }).chapterInfo[0]"),
    ("ImageTrackList", "new ImageDecoder({ data: new Uint8Array([137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,1,0,0,0,1,8,6,0,0,0,31,21,196,137,0,0,0,10,73,68,65,84,120,156,99,0,1,0,0,5,0,1,13,10,45,180,0,0,0,0,73,69,78,68,174,66,96,130]), type: \"image/png\" }).tracks"),

    // The abstract sensor bases: reachable through any concrete sensor, and neither declares a
    // constructor of its own.
    ("Sensor", "new Accelerometer()"),
    ("OrientationSensor", "new RelativeOrientationSensor()"),

    // More `navigator.<x>` entry points, found by sweeping the families rather than the brief's list.
    ("MediaCapabilities", "navigator.mediaCapabilities"),
    ("Presentation", "navigator.presentation"),
    ("DevicePosture", "navigator.devicePosture"),
    ("NavigatorManagedData", "navigator.managed"),

    // A DTMF sender exists on any local audio sender; no remote peer and no signaling.
    ("RTCDTMFSender", "new RTCPeerConnection().addTransceiver(\"audio\").sender.dtmf"),

    // A DeviceMotionEvent's init dictionary builds both of its sub-objects with no motion sensor.
    ("DeviceMotionEventAcceleration", "new DeviceMotionEvent(\"x\", { acceleration: { x: 1, y: 2, z: 3 } }).acceleration"),
    ("DeviceMotionEventRotationRate", "new DeviceMotionEvent(\"x\", { rotationRate: { alpha: 1, beta: 2, gamma: 3 } }).rotationRate"),

    // WebGPU without an adapter. `requestAdapter()` is a promise and no expression can await it, so
    // the adapter-side of the API stays correctly unasked -- but the canvas context, the error
    // hierarchy and the language-feature set are all synchronous and all real here.
    ("GPUCanvasContext", "document.createElement(\"canvas\").getContext(\"webgpu\")"),
    ("GPUError", "new GPUValidationError(\"m\")"),
    ("WGSLLanguageFeatures", "navigator.gpu.wgslLanguageFeatures"),

    // Speech recognition's grammar list is buildable with no microphone and no recognition session.
    ("SpeechGrammar", "(() => { const l = new webkitSpeechGrammarList(); l.addFromString(\"#JSGF V1.0;\", 1); return l[0]; })()"),
];

/// Hand-vetted constructor calls for device-family interfaces whose declared constructor takes an
/// argument `literal_for`'s generic `{}`/`[]` cannot actually satisfy -- a required callback member, a
/// dictionary whose OWN required members are invisible at the `Arg` level, or (`OfflineAudioContext`) an
/// overload order where the first declared constructor takes the dictionary that `{}` fails and the
/// second takes three plain numbers `literal_for` could have spelled all along.
///
/// Checked BEFORE `constructor_expr` in [`receiver_expr`], because that generic path would otherwise
/// win with a `{}` the real browser rejects for a missing required member -- which does not lie (the
/// resulting case is a correctly recorded `throws TypeError`), it just means the interface's OWN
/// members are never reached through it, exactly the hole `receiver_expr` exists to close. Each line
/// was checked against Brave 151 on this machine -- `o instanceof <Iface>` evaluated true -- before
/// being written down.
static CTOR_RECIPES: &[(&str, &str)] = &[
    ("OfflineAudioContext", "new OfflineAudioContext(1, 1, 44100)"),
    ("RTCIceCandidate", "new RTCIceCandidate({ candidate: \"candidate:1 1 UDP 1 127.0.0.1 1 typ host\", sdpMid: \"0\" })"),
    ("VideoFrame", "new VideoFrame(document.createElement(\"canvas\"), { timestamp: 0 })"),
    (
        "PaymentRequest",
        "new PaymentRequest([{ supportedMethods: \"basic-card\" }], { total: { label: \"total\", amount: { currency: \"USD\", value: \"1.00\" } } })",
    ),
    (
        "AudioData",
        "new AudioData({ format: \"f32\", sampleRate: 44100, numberOfFrames: 1, numberOfChannels: 1, timestamp: 0, data: new Float32Array(1) })",
    ),
    ("VideoDecoder", "new VideoDecoder({ output: () => {}, error: () => {} })"),
    ("VideoEncoder", "new VideoEncoder({ output: () => {}, error: () => {} })"),
    ("AudioDecoder", "new AudioDecoder({ output: () => {}, error: () => {} })"),
    ("AudioEncoder", "new AudioEncoder({ output: () => {}, error: () => {} })"),
    // `new URL("x")` is what `literal_for` synthesises from the declared `USVString`, and it THROWS in
    // a browser: "x" is not an absolute URL and there is no base. The oracle reported the receiver
    // rejected and recorded 5 statics where the interface has 17 members. The recipe carries every
    // component so each accessor has something to report -- user, password, port, path, query, hash.
    ("URL", "new URL(\"http://user:pw@example.com:8080/a/b?q=1#h\")"),

    // ── the 31 receivers the corpus-wide sweep reported REJECTED BY THE BROWSER ────────────────────
    //
    // Each was a declared constructor whose generic argument `literal_for` synthesises -- `{}`, `"x"`,
    // `[]` -- is refused, so the interface recorded its statics and nothing else. Almost all are Event
    // subclasses whose init dictionary has a REQUIRED member: `new BlobEvent("x", {})` throws because
    // the dictionary needs `data`, and no amount of argument-type cleverness invents a Blob.
    //
    // All 31 were verified in one batch against a real headless Brave 151 on this machine -- the probe
    // evaluated each and asked `o instanceof <Iface>` -- which is how 39 candidates became 31. The
    // eight that did not survive are NOT here, and each is a finding rather than a gap:
    //
    //   SnapEvent, AnimationTrigger        "Illegal constructor" -- webref declares a constructor the
    //                                      browser does not actually expose
    //   BarcodeDetector                    not defined in Brave on Linux AT ALL, though the macOS
    //                                      capture lists it as a global. Shape Detection is platform-
    //                                      gated; same class as `navigator.bluetooth`, and it needs a
    //                                      macOS capture rather than a guess from this machine
    //   Profiler                           NotAllowedError -- JS profiling is off in this build
    //   IntersectionObserverEntry          `takeRecords()` is empty until an observation is delivered,
    //                                      so there is no synchronous receiver
    //   RTCIceTransport                    reachable only through a live transport
    //   ClipboardChangeEvent, WindowControlsOverlayGeometryChangeEvent   init members we cannot build
    //
    // `Touch` was ALREADY in the singleton table above and was still rejected, which is a finding about
    // PRECEDENCE, not about the recipe: `receiver_expr` tries CTOR_RECIPES, then the declared
    // constructor, then the singletons -- so for any interface that declares a constructor the
    // singleton entry is unreachable. An override belongs here or nowhere.
    ("AudioBuffer", "new AudioBuffer({ length: 1, sampleRate: 44100 })"),
    ("ByteLengthQueuingStrategy", "new ByteLengthQueuingStrategy({ highWaterMark: 1 })"),
    ("CountQueuingStrategy", "new CountQueuingStrategy({ highWaterMark: 1 })"),
    ("CSSMathSum", "new CSSMathSum(CSS.px(1), CSS.px(2))"),
    ("CSSMathMin", "new CSSMathMin(CSS.px(1), CSS.px(2))"),
    ("CSSMathMax", "new CSSMathMax(CSS.px(1), CSS.px(2))"),
    ("CSSMathProduct", "new CSSMathProduct(CSS.px(1), CSS.number(2))"),
    ("CSSTransformValue", "new CSSTransformValue([new CSSTranslate(CSS.px(1), CSS.px(2))])"),
    ("CSSVariableReferenceValue", "new CSSVariableReferenceValue(\"--x\")"),
    ("EncodedAudioChunk", "new EncodedAudioChunk({ type: \"key\", timestamp: 0, data: new Uint8Array(1) })"),
    ("EncodedVideoChunk", "new EncodedVideoChunk({ type: \"key\", timestamp: 0, data: new Uint8Array(1) })"),
    ("FederatedCredential", "new FederatedCredential({ id: \"i\", provider: \"https://p.example/\" })"),
    ("RTCError", "new RTCError({ errorDetail: \"data-channel-failure\" })"),
    ("ProcessingInstruction", "document.createProcessingInstruction(\"t\", \"d\")"),
    ("BlobEvent", "new BlobEvent(\"x\", { data: new Blob([]) })"),
    ("FormDataEvent", "new FormDataEvent(\"x\", { formData: new FormData() })"),
    ("PromiseRejectionEvent", "new PromiseRejectionEvent(\"x\", { promise: Promise.resolve(), reason: 1 })"),
    ("TaskPriorityChangeEvent", "new TaskPriorityChangeEvent(\"x\", { previousPriority: \"user-blocking\" })"),
    ("SpeechSynthesisEvent", "new SpeechSynthesisEvent(\"x\", { utterance: new SpeechSynthesisUtterance(\"a\") })"),
    ("SpeechSynthesisErrorEvent", "new SpeechSynthesisErrorEvent(\"x\", { utterance: new SpeechSynthesisUtterance(\"a\"), error: \"canceled\" })"),
    ("SensorErrorEvent", "new SensorErrorEvent(\"x\", { error: new DOMException(\"m\", \"NotAllowedError\") })"),
    ("RTCErrorEvent", "new RTCErrorEvent(\"x\", { error: new RTCError({ errorDetail: \"data-channel-failure\" }) })"),
    ("RTCPeerConnectionIceErrorEvent", "new RTCPeerConnectionIceErrorEvent(\"x\", { errorCode: 701 })"),
    ("RTCDTMFToneChangeEvent", "new RTCDTMFToneChangeEvent(\"x\", { tone: \"1\" })"),
    ("RTCDataChannelEvent", "new RTCDataChannelEvent(\"x\", { channel: new RTCPeerConnection().createDataChannel(\"c\") })"),
    ("MediaKeyMessageEvent", "new MediaKeyMessageEvent(\"x\", { messageType: \"license-request\", message: new ArrayBuffer(1) })"),
    ("GPUPipelineError", "new GPUPipelineError(\"m\", { reason: \"validation\" })"),
    ("OfflineAudioCompletionEvent", "new OfflineAudioCompletionEvent(\"x\", { renderedBuffer: new AudioBuffer({ length: 1, sampleRate: 44100 }) })"),
    ("AudioProcessingEvent", "new AudioProcessingEvent(\"x\", { playbackTime: 0, inputBuffer: new AudioBuffer({ length: 1, sampleRate: 44100 }), outputBuffer: new AudioBuffer({ length: 1, sampleRate: 44100 }) })"),
    ("WebTransport", "new WebTransport(\"https://web-platform.test:8443/x\")"),
    ("Touch", "new Touch({ identifier: 1, target: document.documentElement, clientX: 0, clientY: 0, screenX: 0, screenY: 0, pageX: 0, pageY: 0 })"),

    // ── Web Audio's node constructors ─────────────────────────────────────────────────────────────
    //
    // Every one of these declares `constructor(BaseAudioContext, optional <X>Options)`. `literal_for`
    // cannot spell a `BaseAudioContext` -- no literal is one -- so `constructor_expr` returned None
    // for the whole family and each node recorded a single `new GainNode()` probe and nothing else.
    // An OfflineAudioContext supplies the missing argument without a sound card: it renders to a
    // buffer, so the graph is fully real on a box with no audio hardware at all.
    //
    // The three that take a live `AudioContext` instead are the three whose OPTIONS need one -- a
    // media element source, a stream source and a stream destination are defined only on the online
    // context -- and each was checked with `instanceof` in real Brave like the rest.
    ("AnalyserNode", "new AnalyserNode(new OfflineAudioContext(1, 1, 44100))"),
    ("AudioBufferSourceNode", "new AudioBufferSourceNode(new OfflineAudioContext(1, 1, 44100))"),
    ("BiquadFilterNode", "new BiquadFilterNode(new OfflineAudioContext(1, 1, 44100))"),
    ("ConstantSourceNode", "new ConstantSourceNode(new OfflineAudioContext(1, 1, 44100))"),
    ("ConvolverNode", "new ConvolverNode(new OfflineAudioContext(1, 1, 44100))"),
    ("DelayNode", "new DelayNode(new OfflineAudioContext(1, 1, 44100))"),
    ("DynamicsCompressorNode", "new DynamicsCompressorNode(new OfflineAudioContext(1, 1, 44100))"),
    ("GainNode", "new GainNode(new OfflineAudioContext(1, 1, 44100))"),
    // `IIRFilterOptions` has two REQUIRED members, so even a spellable context would not be enough.
    ("IIRFilterNode", "new IIRFilterNode(new OfflineAudioContext(1, 1, 44100), { feedforward: [1], feedback: [1] })"),
    ("OscillatorNode", "new OscillatorNode(new OfflineAudioContext(1, 1, 44100))"),
    ("PannerNode", "new PannerNode(new OfflineAudioContext(1, 1, 44100))"),
    ("StereoPannerNode", "new StereoPannerNode(new OfflineAudioContext(1, 1, 44100))"),
    ("WaveShaperNode", "new WaveShaperNode(new OfflineAudioContext(1, 1, 44100))"),
    ("MediaElementAudioSourceNode", "new MediaElementAudioSourceNode(new AudioContext(), { mediaElement: document.createElement(\"audio\") })"),
    ("MediaStreamAudioDestinationNode", "new MediaStreamAudioDestinationNode(new AudioContext())"),
    ("MediaStreamAudioSourceNode", "(() => { const c = new AudioContext(); return new MediaStreamAudioSourceNode(c, { mediaStream: new MediaStreamAudioDestinationNode(c).stream }); })()"),

    // `PasswordCredentialData` requires `id` AND `password`; the generic `{}` throws.
    ("PasswordCredential", "new PasswordCredential({ id: \"i\", password: \"p\" })"),
];

/// Objects a page can only reach by WAITING for one: the receiver is a promise, and until the runner
/// could await, every case filed under one of these was dropped because `(a Promise) instanceof
/// XRSession` is stably, uselessly `false`.
///
/// Same contract as [`SINGLETONS`] -- each line is a claim about the browser that the capture tool
/// probes (`(await (<recipe>)) instanceof <Iface>`) before recording anything under it, so a wrong
/// line costs an interface its cases and can never invent an expectation. The difference is only that
/// the probe awaits.
///
/// Checked against Brave 151 on this machine before being written down. `requestSession("inline")`
/// needs no user gesture, no headset and no GPU: six sequential calls returned six live `XRSession`s
/// in 2 ms headless, which is why the recipes below simply ask again rather than memoising one
/// session in a global the page would then carry.
static ASYNC_RECIPES: &[(&str, &str)] = &[
    ("XRSession", "navigator.xr.requestSession(\"inline\")"),
    ("XRRenderState", "navigator.xr.requestSession(\"inline\").then((s) => s.renderState)"),
    ("XRInputSourceArray", "navigator.xr.requestSession(\"inline\").then((s) => s.inputSources)"),
    (
        "XRReferenceSpace",
        "navigator.xr.requestSession(\"inline\").then((s) => s.requestReferenceSpace(\"viewer\"))",
    ),
];

/// Arguments the generic [`literal_for`] cannot spell, for members worth reaching anyway.
///
/// The corpus this generator reads models interfaces but not ENUMS, so a `XRSessionMode` argument
/// has no literal and `required_args` gives up -- which is why `XRSystem.requestSession` and
/// `XRSystem.isSessionSupported`, the two doors to the entire XR family, had never been asked a single
/// question. A guessed `"x"` would not have helped: WebIDL says an invalid enum value throws
/// `TypeError`, so the recorded answer would have been a fact about enum validation and nothing else.
///
/// This is a QUESTION table, which is the half of a case the generator is allowed to author. Every
/// answer still comes from the capture. Keep it small: the right fix is to teach the corpus about
/// enums, and each line here is an admission that has not happened yet.
static ARG_RECIPES: &[(&str, &str, &str)] = &[
    ("XRSystem", "isSessionSupported", "\"inline\""),
    ("XRSystem", "requestSession", "\"inline\""),
];

/// The declared arguments for one member, preferring a vetted recipe over the generic literals.
fn args_for(iface: &str, member: &str, args: &[Arg]) -> Option<String> {
    if let Some((_, _, a)) = ARG_RECIPES.iter().find(|(i, m, _)| *i == iface && *m == member) {
        return Some((*a).to_string());
    }
    required_args(args)
}

/// The settled type behind `Promise<T>`, or `None` for a member that does not return one.
///
/// String surgery rather than a type parser because that is all the corpus gives us: `ret` is the IDL
/// text. `Promise<undefined>` is a real and common declaration (`session.end()`), and its settled
/// value -- `undefined` -- is a perfectly recordable answer, so it is NOT filtered out here.
fn promise_inner(ret: &str) -> Option<&str> {
    let t = ret.trim().trim_end_matches('?').trim();
    t.strip_prefix("Promise<")?.strip_suffix('>').map(str::trim)
}

/// How to obtain an instance of `iface` to ask questions of, or `None` when nothing here reaches it.
///
/// Order: `CTOR_RECIPES` first -- a small override for the few declared constructors the generic path
/// cannot actually drive to a live instance -- then the declared constructor itself (the spec's own
/// answer, and the only receiver that also exercises argument handling), then the hand-vetted
/// singletons, then the element tables generated from Blink. The last two are disjoint, so the order
/// between them is arbitrary.
///
/// `ASYNC_RECIPES` goes LAST, after every synchronous source, because an interface reachable without
/// waiting should be reached without waiting: an awaited case costs a task boundary, an event-loop
/// pump on the Korvus side and a deadline on both, and none of that buys anything for an object you
/// can just build. Nothing in that table is reachable any other way today, so the order is a
/// statement of intent rather than a tie-break -- and `the_receiver_sources_do_not_overlap` holds it.
fn receiver_expr(spec: &InterfaceSpec) -> Option<Receiver> {
    if let Some((_, e)) = CTOR_RECIPES.iter().find(|(n, _)| *n == spec.name) {
        return Some(Receiver::sync(e));
    }
    if let Some(c) = constructor_expr(spec) {
        return Some(Receiver { expr: c, awaited: false });
    }
    if let Some((_, e)) = SINGLETONS.iter().find(|(n, _)| *n == spec.name) {
        return Some(Receiver::sync(e));
    }
    if let Some(e) = crate::html_tags::create_expr_for(&spec.name) {
        return Some(Receiver { expr: e, awaited: false });
    }
    ASYNC_RECIPES
        .iter()
        .find(|(n, _)| *n == spec.name)
        .map(|(_, e)| Receiver { expr: (*e).to_string(), awaited: true })
}

/// A receiver recipe and whether reaching it means waiting for a promise.
#[derive(Debug, Clone)]
struct Receiver {
    expr: String,
    awaited: bool,
}

impl Receiver {
    fn sync(e: &str) -> Self {
        Receiver { expr: e.to_string(), awaited: false }
    }
}

/// Wrap a body so the whole case is one expression with a receiver in scope.
fn with_receiver(ctor: &str, body: &str) -> String {
    format!("(() => {{ const o = {ctor}; return {body}; }})()")
}

/// The same, for a case that has to WAIT -- either for the receiver, or for what the member returns,
/// or for both.
///
/// The receiver is awaited only when it actually is a promise. `await` on a plain object is harmless
/// but it is not free: it moves the whole case into the runner's second pass, past the task boundary
/// the synchronous corpus is careful to stay inside, and a reader of the corpus should be able to tell
/// from the expression which of the two things is being waited for.
fn with_receiver_awaited(recv: &Receiver, body: &str) -> String {
    let o = if recv.awaited { format!("await ({})", recv.expr) } else { recv.expr.clone() };
    format!("(async () => {{ const o = {o}; return {body}; }})()")
}

/// Members with no answer a recorded oracle can take, because their effect is on the browsing
/// context or on the user rather than on a returned value.
///
/// Two capture runs died here, in two different ways, and both are the same mistake:
///
///   * NAVIGATION. `history.back()` does not return a value -- it destroys the document the question
///     was asked in, and over CDP that takes the execution context with it. The run reached 527 files
///     and then every later evaluate targeted a context that no longer existed.
///   * MODALS AND PROMPTS. `alert()`, `confirm()`, `showOpenFilePicker()` block the renderer waiting
///     for a person. Nobody answers, so the capture hangs rather than failing. The second run reached
///     `Window.json` and stopped there.
///
/// There is nothing to record in either case: a recorded oracle asks what an expression evaluates to,
/// and these do not evaluate to anything. They keep their surface axes -- present, native, shape and
/// spec all still check them -- and stay out of the behavior corpus.
///
/// Match by interface first, then by member. Spelling out only the pairs one capture happened to trip
/// over is how the first version shipped without `Window.focus` and `Window.moveTo`, which are not
/// navigation but do take over the desktop of whoever is running the capture.
fn unrecordable(iface: &str, member: &str) -> bool {
    match iface {
        "History" | "Navigation" => matches!(
            member,
            "back" | "forward" | "go" | "pushState" | "replaceState" | "navigate" | "traverseTo" | "reload"
        ),
        "Location" => matches!(member, "assign" | "replace" | "reload" | "href"),
        "VisualViewport" => matches!(member, "width" | "height"),
        "Document" | "XMLDocument" => matches!(
            member,
            "open" | "close" | "write" | "writeln" | "execCommand"
            // `lastModified` IS the capture time, formatted. It reads as stable because all three
            // passes finish inside its one-second resolution, so the determinism filter cannot catch
            // it, and what gets written down is a date the engine must then reproduce forever.
            | "lastModified"
        ),
        "Window" => matches!(
            member,
            // The operator's WINDOW SIZE, not the engine's. A capture records whatever size the
            // browser happened to be, and no implementation can reproduce that; the screen's own
            // dimensions and colour depth are different -- those belong to the fingerprint persona
            // and are worth asking about.
            "innerWidth" | "innerHeight" | "outerWidth" | "outerHeight"
            | "screenX" | "screenY" | "screenLeft" | "screenTop"
            |
            // navigation and lifetime
            "open" | "close" | "stop" | "location" | "navigate"
            // modal: blocks the renderer on a person who is not there
            | "alert" | "confirm" | "prompt" | "print" | "showModalDialog"
            // permission / picker prompts: the same, with a chooser
            | "showOpenFilePicker" | "showSaveFilePicker" | "showDirectoryPicker" | "queryLocalFonts"
            // takes over the machine the capture is running on
            | "moveTo" | "moveBy" | "resizeTo" | "resizeBy" | "focus" | "blur"
        ),
        _ => false,
    }
}

/// Is this declared type one whose VALUE the shared runner can carry back?
///
/// The runner serialises booleans, numbers, strings and flat arrays of those; everything else comes
/// back `unsupported`. That is a transport limit, not a judgement -- but it meant an object-returning
/// member could never be asked ANYTHING. `document.implementation`, `element.getBoundingClientRect()`,
/// `element.getClientRects()`, every Promise-returning method: 775 answers were dropped corpus-wide as
/// "not representable", and the member behind each was left with no evidence at all.
fn scalar_typed(ty: &str) -> bool {
    matches!(
        ty.trim().trim_end_matches('?').trim(),
        "boolean" | "byte" | "octet" | "short" | "unsigned short" | "long" | "unsigned long"
            | "long long" | "unsigned long long" | "float" | "unrestricted float" | "double"
            | "unrestricted double" | "DOMString" | "USVString" | "ByteString" | "CSSOMString"
            | "undefined" | "void" | "any"
    )
}

/// The BRAND of what a member evaluates to -- `"[object DOMRect]"`, `"[object Promise]"`,
/// `"[object Null]"`.
///
/// A string, so the runner can carry it; and a real fact about the object, so it is worth carrying.
/// It does not prove a DOMRect holds the right numbers, but it proves the member returns a DOMRect at
/// all, which is most of what "is this implemented" means and is exactly what a shell returning
/// `undefined` fails. The constructor probe above has used this projection since it was written; this
/// applies it to the members.
///
/// Emitted for any non-scalar declared type. An enum-valued member gets one too and answers
/// `"[object String]"` -- correct, deterministic, and low-value; the alternative is a name-based guess
/// at which identifiers are enums and which are interfaces, which this file has no corpus to make.
fn brand_of(expr: &str) -> String {
    format!("Object.prototype.toString.call({expr})")
}

/// Every case worth generating for one interface.
///
/// Ordered so the corpus reads top-down: constants, statics, the constructor itself, then instance
/// members. A member appears more than once when there is more than one honest question to ask of it
/// (a setter is read back; an operation is called both with its declared arguments and with none).
pub fn generate(spec: &InterfaceSpec) -> Vec<CaseSkeleton> {
    let mut out = Vec::new();
    let name = &spec.name;
    let mut push = |member: &str, expr: String, recv: Option<&Receiver>| {
        if unrecordable(name, member) {
            return;
        }
        out.push(CaseSkeleton {
            iface: name.clone(),
            member: member.to_string(),
            // An expression that opens `(async ` is exactly the one the runner has to await. Deriving
            // the flag from the expression, rather than threading a bool through a dozen call sites,
            // keeps the two from ever disagreeing -- and a disagreement here is not a compile error,
            // it is a Promise's brand quietly recorded as if it were a settled value.
            awaited: expr.starts_with("(async "),
            expr,
            recv: recv.map(|r| r.expr.clone()),
            recv_await: recv.map(|r| r.awaited).unwrap_or(false),
        })
    };

    for m in &spec.members {
        match m {
            // A constant's value is fixed by the IDL, so this asks the one thing the IDL cannot:
            // whether it is actually THERE, on the interface object, at the declared value.
            MemberSpec::Const { name: n, .. } => push(n, format!("{name}.{n}"), None),

            MemberSpec::Attribute { name: n, is_static: true, ty, .. } => {
                push(n, format!("{name}.{n}"), None);
                if !scalar_typed(ty) {
                    push(n, brand_of(&format!("{name}.{n}")), None);
                }
            }
            MemberSpec::Operation { name: n, args, ret, is_static: true } => {
                if let Some(a) = args_for(name, n, args) {
                    push(n, format!("{name}.{n}({a})"), None);
                    if !scalar_typed(ret) {
                        push(n, brand_of(&format!("{name}.{n}({a})")), None);
                    }
                    // A static that returns a promise gets the same second question the instance
                    // members below get: not what the wrapper is, but what it SETTLES to.
                    if let Some(inner) = promise_inner(ret) {
                        let call = format!("await {name}.{n}({a})");
                        push(n, format!("(async () => {{ return {call}; }})()"), None);
                        if !scalar_typed(inner) {
                            push(n, format!("(async () => {{ return {}; }})()", brand_of(&call)), None);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Constructing it IS behavior: arity enforcement, argument coercion, and the "Illegal constructor"
    // of an interface that declares none are all things a shell gets wrong while looking perfect.
    let ctor = constructor_expr(spec);
    match &ctor {
        Some(c) => {
            push("constructor", format!("(() => {{ const o = {c}; return Object.prototype.toString.call(o); }})()"), None);
            // No arguments at all, whatever the IDL requires. Chrome answers either with an object or
            // with the TypeError an implementation that forgot arity checking will not produce.
            push("constructor", format!("(() => {{ const o = new {name}(); return Object.prototype.toString.call(o); }})()"), None);
        }
        // An interface with no `constructor` must reject `new`. Shells get this right by accident and
        // hand-written interfaces get it wrong, so it is worth asking every time.
        None => push("constructor", format!("new {name}()"), None),
    }

    // The receiver need not be the constructor: most of the DOM has none, and a recipe
    // (`document.createElement("input")`) reaches exactly the members that used to go unasked.
    let Some(ctor) = receiver_expr(spec) else {
        let mut seen = std::collections::HashSet::new();
        out.retain(|c| seen.insert((c.member.clone(), c.expr.clone())));
        return out;
    };
    // A receiver that only exists after a promise settles makes EVERY case behind it an awaited one,
    // even a plain attribute read: `o` cannot be bound at all without waiting. A synchronous receiver
    // keeps the synchronous form, and only the individual promise-returning members below opt in.
    let wrap = |body: &str| {
        if ctor.awaited { with_receiver_awaited(&ctor, body) } else { with_receiver(&ctor.expr, body) }
    };
    for m in &spec.members {
        match m {
            MemberSpec::Attribute { name: n, readonly, is_static: false, ty, .. } => {
                push(n, wrap(&format!("o.{n}")), Some(&ctor));
                if !scalar_typed(ty) {
                    push(n, wrap(&brand_of(&format!("o.{n}"))), Some(&ctor));
                }
                if !readonly {
                    // Round-trip: what a setter STORES, which is where coercion lives. `o.x = "1"` on a
                    // `double` attribute is the exact shape of the DOMRect bug that started all this.
                    if let Some(v) = literal_for(ty) {
                        push(n, wrap(&format!("(o.{n} = {v}, o.{n})")), Some(&ctor));
                    }
                }
            }
            MemberSpec::Operation { name: n, args, ret, is_static: false } => {
                // A CALL THAT RETURNS A PROMISE OUT OF AN ALREADY-ASYNC CASE FLATTENS, and the
                // resulting case lies about what it asks. `wrap` on an awaited receiver builds
                // `(async () => { const o = await (...); return o.end(); })()`; awaiting THAT unwraps
                // `o.end()` too, so the recorded answer is the settled value under an expression that
                // reads as the wrapper question. It is the same question as the explicit `await
                // o.end()` below, spelled worse -- so on an awaited receiver the plain call is
                // dropped and the wrapper is asked through its brand, which is a string and cannot
                // flatten.
                let flattens = ctor.awaited && promise_inner(ret).is_some();
                if let Some(a) = args_for(name, n, args) {
                    if !flattens {
                        push(n, wrap(&format!("o.{n}({a})")), Some(&ctor));
                    }
                    if !scalar_typed(ret) {
                        push(n, wrap(&brand_of(&format!("o.{n}({a})"))), Some(&ctor));
                    }
                    // THE 137. A promise-returning member has, until now, only ever been asked what
                    // its WRAPPER is -- `"[object Promise]"` for every one of them, which catches a
                    // shell that returns `undefined` and nothing else. These two ask what it settles
                    // to, and the brand of that when the settled type is not a scalar.
                    //
                    // The `"[object Promise]"` question above is deliberately KEPT. It is what ~137
                    // recorded cases already assert, it is cheap, and it is a different claim: that
                    // the member returns a promise at all, which an implementation that resolves
                    // synchronously would fail while still settling to the right value.
                    if let Some(inner) = promise_inner(ret) {
                        push(n, with_receiver_awaited(&ctor, &format!("await o.{n}({a})")), Some(&ctor));
                        if !scalar_typed(inner) {
                            push(
                                n,
                                with_receiver_awaited(&ctor, &brand_of(&format!("await o.{n}({a})"))),
                                Some(&ctor),
                            );
                        }
                    }
                }
                if !args.is_empty() && args.iter().any(|a| !a.optional) {
                    // Called with nothing, to pin arity enforcement. A promise-returning member
                    // called wrong REJECTS rather than throwing, so the arity probe has to be awaited
                    // too or it records `"[object Promise]"` where the answer is a TypeError.
                    if !flattens {
                        push(n, wrap(&format!("o.{n}()")), Some(&ctor));
                    }
                    if promise_inner(ret).is_some() {
                        push(n, with_receiver_awaited(&ctor, &format!("await o.{n}()")), Some(&ctor));
                    }
                }
            }
            _ => {}
        }
    }
    // An all-optional constructor makes the "with arguments" and "with none" probes the same string,
    // and a member declared twice across partials can repeat. Identical expressions ask one question.
    out.dedup_by(|a, b| a.expr == b.expr && a.member == b.member);
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert((c.member.clone(), c.expr.clone())));
    out
}

/// Type-coercion probes: pass a STRING where a number is declared, and a number where a string is.
///
/// Separate from [`generate`] because the intent is inverted — these arguments are deliberately of the
/// wrong type, to record whether Chrome coerces or rejects. WebIDL says `unrestricted double`, which
/// requires coercing `"10"`; an implementation that pattern-matches on "is it a number" ships a member
/// that passes every case its author thought to write. That is not hypothetical: it is what `DOMRect`
/// did, and every one of its 17 hand-written cases passed.
pub fn coercion_probes(spec: &InterfaceSpec) -> Vec<CaseSkeleton> {
    let mut out = Vec::new();
    let ctor_args = spec.members.iter().find_map(|m| match m {
        MemberSpec::Constructor { args, html_constructor: false } => Some(args),
        _ => None,
    });
    let numeric = |t: &str| {
        matches!(t.trim().trim_end_matches('?').trim(),
            "byte" | "octet" | "short" | "unsigned short" | "long" | "unsigned long" | "long long"
            | "unsigned long long" | "float" | "unrestricted float" | "double" | "unrestricted double")
    };
    if ctor_args.is_some_and(|a| a.iter().any(|a| numeric(&a.ty))) {
        let ctor_args = ctor_args.expect("guarded above");
        let swapped: Option<Vec<&str>> = ctor_args
            .iter()
            .take_while(|a| !a.optional && !a.variadic)
            .map(|a| if numeric(&a.ty) { Some("\"1\"") } else { literal_for(&a.ty) })
            .collect();
        if let Some(v) = swapped {
            if !v.is_empty() {
                out.push(CaseSkeleton {
                    iface: spec.name.clone(),
                    member: "constructor".into(),
                    recv: None,
                    awaited: false,
                    recv_await: false,
                    expr: format!(
                        "(() => {{ const o = new {}({}); return Object.prototype.toString.call(o); }})()",
                        spec.name,
                        v.join(", ")
                    ),
                });
            }
        }
    }
    for m in &spec.members {
        if let MemberSpec::Attribute { name: n, readonly: false, is_static: false, ty, .. } = m {
            if numeric(ty) && !unrecordable(&spec.name, n) {
                // Any receiver will do: the question is what the SETTER stores, and a factory-built
                // one asks it of the interfaces that never had a constructor to ask it through.
                if let Some(c) = receiver_expr(spec) {
                    let body = format!("(o.{n} = \"1\", o.{n})");
                    let expr =
                        if c.awaited { with_receiver_awaited(&c, &body) } else { with_receiver(&c.expr, &body) };
                    out.push(CaseSkeleton {
                        iface: spec.name.clone(),
                        member: n.clone(),
                        awaited: c.awaited,
                        expr,
                        recv: Some(c.expr),
                        recv_await: c.awaited,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Arg, InterfaceSpec, MemberSpec};

    fn arg(name: &str, ty: &str, optional: bool) -> Arg {
        Arg { name: name.into(), ty: ty.into(), optional, variadic: false }
    }

    fn domrect() -> InterfaceSpec {
        InterfaceSpec {
            name: "DOMRect".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Constructor {
                    html_constructor: false,
                    args: vec![
                        arg("x", "unrestricted double", true),
                        arg("y", "unrestricted double", true),
                    ],
                },
                MemberSpec::Attribute {
                    name: "x".into(),
                    ty: "unrestricted double".into(),
                    readonly: false,
                    is_static: false,
                    inherited: false,
                put_forwards: false,
                },
            ],
        }
    }

    #[test]
    fn identical_questions_are_asked_once() {
        // DOMRect's constructor is all-optional, so "with required args" and "with none" are the same
        // expression. The corpus should not carry it twice.
        let cases = generate(&domrect());
        let mut keys: Vec<_> = cases.iter().map(|c| (&c.member, &c.expr)).collect();
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n, "duplicate case generated");
    }

    #[test]
    fn an_instance_member_is_reached_through_the_declared_constructor() {
        let cases = generate(&domrect());
        assert!(cases.iter().any(|c| c.member == "x" && c.expr.contains("new DOMRect()") && c.expr.ends_with("o.x; })()")));
        // and the setter is read back, because storing is where coercion happens
        assert!(cases.iter().any(|c| c.member == "x" && c.expr.contains("(o.x = 1, o.x)")));
    }

    #[test]
    fn an_interface_with_no_constructor_is_asked_to_reject_new() {
        // `Frob` is reachable no other way either, so the rejection probe is the only honest question.
        let spec = InterfaceSpec { name: "Frob".into(), inherits: None, exposed: vec![], members: vec![] };
        let cases = generate(&spec);
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].expr, "new Frob()");
        assert_eq!(cases[0].member, "constructor");
    }

    /// The hole this closes: before recipes, an interface with no constructor yielded exactly one
    /// case -- "does `new` throw" -- and every one of its members passed by never being asked.
    #[test]
    fn a_factory_built_interface_still_reaches_its_instance_members() {
        let spec = InterfaceSpec {
            name: "HTMLInputElement".into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Attribute {
                name: "value".into(),
                ty: "DOMString".into(),
                readonly: false,
                is_static: false,
                inherited: false,
            put_forwards: false,
            }],
        };
        let cases = generate(&spec);
        assert!(
            cases.iter().any(|c| c.member == "value" && c.expr.contains(r#"document.createElement("input")"#)),
            "the tag table must supply the receiver: {cases:#?}"
        );
        // The setter round-trip is where coercion lives, and it is now asked here too.
        assert!(cases.iter().any(|c| c.expr.contains(r#"(o.value = "x", o.value)"#)));
        // `new HTMLInputElement()` must still be asked to throw -- the recipe does not excuse it.
        assert!(cases.iter().any(|c| c.member == "constructor" && c.expr == "new HTMLInputElement()"));
    }

    /// There is no answer to record for a member whose whole effect is on the browsing context or on
    /// the user, and asking anyway killed two real captures -- History destroying the execution
    /// context, then Window.alert blocking the renderer on a person who was not there.
    #[test]
    fn an_unrecordable_member_is_never_asked() {
        let spec = InterfaceSpec {
            name: "History".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Operation { name: "back".into(), ret: "undefined".into(), args: vec![], is_static: false },
                MemberSpec::Attribute {
                    name: "length".into(), ty: "unsigned long".into(),
                    readonly: true, is_static: false, inherited: false,
                put_forwards: false,
                },
            ],
        };
        let cases = generate(&spec);
        assert!(cases.iter().all(|c| c.member != "back"), "back() navigates: {cases:#?}");
        assert!(cases.iter().any(|c| c.member == "length"), "the readable surface is still asked");

        let win = InterfaceSpec {
            name: "Window".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Operation { name: "alert".into(), ret: "undefined".into(), args: vec![], is_static: false },
                MemberSpec::Operation { name: "focus".into(), ret: "undefined".into(), args: vec![], is_static: false },
                MemberSpec::Attribute {
                    name: "devicePixelRatio".into(), ty: "double".into(),
                    readonly: true, is_static: false, inherited: false,
                put_forwards: false,
                },
            ],
        };
        let cases = generate(&win);
        assert!(cases.iter().all(|c| c.member != "alert"), "alert() blocks the renderer");
        assert!(cases.iter().all(|c| c.member != "focus"), "focus() grabs the capture machine's desktop");
        assert!(cases.iter().any(|c| c.member == "devicePixelRatio"));
        // The window's SIZE is the operator's, not the engine's, and no implementation reproduces
        // the dimensions a capture happened to be run at.
        let sized = InterfaceSpec {
            name: "Window".into(), inherits: None, exposed: vec![],
            members: vec![MemberSpec::Attribute {
                name: "innerWidth".into(), ty: "long".into(),
                readonly: true, is_static: false, inherited: false,
            put_forwards: false,
            }],
        };
        assert!(generate(&sized).iter().all(|c| c.member != "innerWidth"));
    }

    #[test]
    fn a_singleton_is_reached_through_its_vetted_recipe() {
        let spec = InterfaceSpec {
            name: "Navigator".into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Attribute {
                name: "userAgent".into(),
                ty: "DOMString".into(),
                readonly: true,
                is_static: false,
                inherited: false,
            put_forwards: false,
            }],
        };
        assert!(generate(&spec).iter().any(|c| c.member == "userAgent" && c.expr.contains("const o = navigator;")));
    }

    /// A `SINGLETONS` entry for an interface that ALSO declares a spellable constructor is dead code:
    /// `receiver_expr` returns the constructor first and the recipe is never used, silently.
    ///
    /// This is not hypothetical. It is how the `URL` and `Touch` recipes were lost -- both sat in the
    /// singleton table, both were shadowed, and both were only found when a corpus-wide sweep reported
    /// the browser REJECTING a receiver nobody had written. The failure leaves no trace: the table
    /// still reads correct, the interface still gets cases, and the cases are about the wrong object.
    ///
    /// So the table itself is checked against the real corpus. An override belongs in `CTOR_RECIPES`,
    /// which is consulted first, or nowhere.
    ///
    /// A RATCHET, not a clean sheet: running this for the first time found TWENTY-FIVE entries already
    /// shadowed, listed below. They are not all bugs -- `Range`'s recipe and `new Range()` reach the
    /// same object, so the dead entry costs nothing -- but several are: `StaticRange`'s
    /// `new StaticRange({})` THROWS in a browser while its recipe works, and `CSSStyleSheet`'s recipe
    /// deliberately calls `replaceSync` so `cssRules` is non-empty where the bare constructor's is
    /// empty. Sorting the harmless from the harmful means re-recording those interfaces, which is a
    /// different unit's work. What this test buys today is that the number cannot grow in silence.
    #[test]
    fn no_singleton_is_shadowed_by_a_declared_constructor() {
        /// Entries already shadowed when this check was written. Shrinking this list is progress;
        /// adding to it means a recipe someone wrote is being silently ignored.
        const KNOWN_DEAD: &[&str] = &[
            "Animation", "CSSKeywordValue", "CSSStyleSheet", "CSSUnitValue", "Comment",
            "CustomElementRegistry", "DOMRectReadOnly", "DataTransfer", "Document",
            "DocumentFragment", "DocumentTimeline", "EventTarget", "Headers", "MessageChannel",
            "Path2D", "Range", "ReadableStream", "Response", "StaticRange", "Text",
            "TextDecoderStream", "TextEncoderStream", "TransformStream", "WritableStream",
            "XPathEvaluator",
        ];
        let corpus = crate::merge::parse_corpus(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webref"));
        assert!(!corpus.is_empty(), "the vendored webref corpus is missing");
        let mut shadowed = Vec::new();
        for (name, _) in SINGLETONS {
            if CTOR_RECIPES.iter().any(|(n, _)| n == name) || KNOWN_DEAD.contains(name) {
                continue;
            }
            let Some(spec) = corpus.get(*name) else { continue };
            if let Some(c) = constructor_expr(spec) {
                shadowed.push(format!("{name} (constructor_expr yields `{c}`)"));
            }
        }
        assert!(
            shadowed.is_empty(),
            "these singleton recipes are unreachable -- move them to CTOR_RECIPES: {shadowed:#?}"
        );
    }

    /// The two receiver sources are disjoint, which is what makes their order in `receiver_expr`
    /// arbitrary rather than a decision anyone has to remember.
    #[test]
    fn the_recipe_sources_do_not_overlap() {
        for (name, _) in SINGLETONS {
            assert!(
                crate::html_tags::create_expr_for(name).is_none(),
                "{name} is reachable from BOTH the singleton list and an element tag"
            );
        }
        // `ASYNC_RECIPES` is tried LAST, so an entry that any synchronous source also reaches is dead
        // code -- and dead in the expensive direction: the interface would keep its cheap synchronous
        // receiver while the line here reads as though the family needed to be awaited.
        for (name, _) in ASYNC_RECIPES {
            assert!(
                !SINGLETONS.iter().any(|(n, _)| n == name)
                    && !CTOR_RECIPES.iter().any(|(n, _)| n == name)
                    && crate::html_tags::create_expr_for(name).is_none(),
                "{name} has a synchronous receiver too; its ASYNC_RECIPES line is never used"
            );
        }
    }

    /// An interface reachable only by waiting: every case behind it is awaited, and the receiver is
    /// flagged so the capture tool's `instanceof` probe awaits it too.
    #[test]
    fn an_awaited_receiver_makes_every_case_behind_it_awaited() {
        let spec = InterfaceSpec {
            name: "XRSession".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Attribute {
                    name: "visibilityState".into(), ty: "XRVisibilityState".into(),
                    readonly: true, is_static: false, inherited: false, put_forwards: false,
                },
                MemberSpec::Operation {
                    name: "end".into(), ret: "Promise<undefined>".into(), args: vec![], is_static: false,
                },
            ],
        };
        let cases = generate(&spec);
        let inst: Vec<_> = cases.iter().filter(|c| c.member != "constructor").collect();
        assert!(!inst.is_empty(), "the async recipe did not reach the instance members");
        for c in &inst {
            assert!(c.awaited, "not awaited: {}", c.expr);
            assert!(c.recv_await, "receiver not flagged awaited: {}", c.expr);
            assert_eq!(c.recv.as_deref(), Some("navigator.xr.requestSession(\"inline\")"));
            assert!(c.expr.starts_with("(async () => { const o = await ("), "{}", c.expr);
        }
        // The constructor probe is about the interface object, not an instance, and stays synchronous.
        let ctor: Vec<_> = cases.iter().filter(|c| c.member == "constructor").collect();
        assert!(ctor.iter().all(|c| !c.awaited), "the constructor probe needs no receiver: {ctor:#?}");

        // `end()` returns a promise, and the case is already an async IIFE, so a plain `return
        // o.end();` would flatten and record the SETTLED value under an expression that reads as the
        // wrapper question. Exactly one value question survives, and it says `await`; the wrapper is
        // asked through its brand, which is a string and cannot flatten.
        let end: Vec<&str> = cases.iter().filter(|c| c.member == "end").map(|c| c.expr.as_str()).collect();
        assert!(
            end.iter().any(|e| e.contains("return await o.end();")),
            "the settled question is missing: {end:#?}"
        );
        assert!(
            end.iter().all(|e| !e.contains("return o.end();")),
            "a plain promise-returning call on an awaited receiver flattens and must not be asked: {end:#?}"
        );
        assert!(
            end.iter().any(|e| e.contains("toString.call(o.end())")),
            "the wrapper is still asked, through its brand: {end:#?}"
        );
    }

    /// A promise-returning member keeps its wrapper question AND gains a settled-value one.
    ///
    /// Both matter and they are different claims: the wrapper question fails an implementation that
    /// returns a plain value where a promise is declared, the settled question fails one that returns
    /// a promise resolving to nothing.
    #[test]
    fn a_promise_returning_member_is_asked_what_it_settles_to() {
        let spec = InterfaceSpec {
            name: "StorageManager".into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Operation {
                name: "persisted".into(), ret: "Promise<boolean>".into(), args: vec![], is_static: false,
            }],
        };
        let cases = generate(&spec);
        let p: Vec<_> = cases.iter().filter(|c| c.member == "persisted").collect();
        assert!(
            p.iter().any(|c| !c.awaited && c.expr.contains("Object.prototype.toString.call(o.persisted())")),
            "the \"[object Promise]\" question must survive: {p:#?}"
        );
        let settled: Vec<_> = p.iter().filter(|c| c.awaited).collect();
        assert_eq!(settled.len(), 1, "exactly one settled question for a scalar-settling member: {p:#?}");
        assert!(settled[0].expr.contains("await o.persisted()"), "{}", settled[0].expr);
        // `boolean` is carriable on its own, so no brand question is generated for it.
        assert!(
            !settled[0].expr.contains("Object.prototype.toString"),
            "a scalar-settling promise needs no brand probe: {}",
            settled[0].expr
        );
        // The receiver (`navigator.storage`) is NOT a promise, so the probe must not await it.
        assert!(settled.iter().all(|c| !c.recv_await), "navigator.storage is not a promise: {settled:#?}");
    }

    /// An argument the generic literal table cannot spell, supplied by a vetted QUESTION recipe.
    #[test]
    fn a_vetted_argument_recipe_opens_a_member_the_generic_path_gives_up_on() {
        let spec = InterfaceSpec {
            name: "XRSystem".into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Operation {
                name: "isSessionSupported".into(),
                ret: "Promise<boolean>".into(),
                // XRSessionMode is an enum; the corpus does not model enums, so `literal_for` fails.
                args: vec![arg("mode", "XRSessionMode", false)],
                is_static: false,
            }],
        };
        let cases = generate(&spec);
        assert!(
            cases.iter().any(|c| c.member == "isSessionSupported" && c.expr.contains("\"inline\"")),
            "the vetted argument recipe was not used: {cases:#?}"
        );
        assert!(
            cases.iter().any(|c| c.member == "isSessionSupported" && c.awaited),
            "the settled answer is what this member is worth asking about: {cases:#?}"
        );
    }

    /// `Promise<T>` is unwrapped to `T`, and only to `T`.
    #[test]
    fn the_settled_type_is_read_out_of_the_declaration() {
        assert_eq!(promise_inner("Promise<boolean>"), Some("boolean"));
        assert_eq!(promise_inner("Promise<undefined>"), Some("undefined"));
        assert_eq!(promise_inner("Promise<sequence<DOMString>>"), Some("sequence<DOMString>"));
        assert_eq!(promise_inner("boolean"), None);
        assert_eq!(promise_inner("PromiseLike"), None);
    }

    #[test]
    fn a_numeric_setter_gets_a_string_because_webidl_says_coerce_it() {
        // The DOMRect bug in one test: `o.x = "1"` must coerce, and nothing an author writes by hand
        // covers it unless they thought of it.
        let probes = coercion_probes(&domrect());
        assert!(probes.iter().any(|c| c.member == "x" && c.expr.contains("o.x = \"1\"")));
    }

    #[test]
    fn a_member_whose_argument_we_cannot_spell_is_skipped_not_guessed() {
        let spec = InterfaceSpec {
            name: "I".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Constructor { args: vec![], html_constructor: false },
                MemberSpec::Operation {
                    name: "f".into(),
                    ret: "undefined".into(),
                    args: vec![arg("n", "SomeOtherInterface", false)],
                    is_static: false,
                },
            ],
        };
        let cases = generate(&spec);
        // `f` is still asked to enforce its arity, but never called with a made-up receiver-typed arg.
        assert!(cases.iter().all(|c| !c.expr.contains("o.f(\"")));
        assert!(cases.iter().any(|c| c.member == "f" && c.expr.contains("o.f()")));
    }
}
