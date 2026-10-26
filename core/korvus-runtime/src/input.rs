//! INPUT — the behaviour layer's dispatch: where a synthetic mouse, key, touch or wheel event comes
//! from and when it is delivered.
//!
//! Moved out of `lib.rs` VERBATIM. Nothing here was edited in the move.
//!
//! The engine does not "fire a click". It asks the behaviour plugin
//! ([`behavior::BehaviorService`], default = the neuromotor model) for a plan — a list of
//! [`Timed`] emissions at µs offsets — and then PLAYS that plan in real time, stamping each event
//! with the page clock. That indirection is the whole point: the timing between two events a real
//! finger produces is the signal a site's instrumentation reads, and it is not something a caller should be
//! able to pass in.

use crate::Engine;

/// One scheduled input event for the real-time scheduler: what to emit, at a relative µs offset from the
/// action's start. `Engine::play` sleeps the offsets in real time and stamps each with the page clock.
struct Timed {
    at_us: f64,
    emit: Emit,
}

fn touch_kind(p: behavior::TouchPhase) -> &'static str {
    match p {
        behavior::TouchPhase::Start => "touchstart",
        behavior::TouchPhase::Move => "touchmove",
        behavior::TouchPhase::End => "touchend",
    }
}

/// CDP's four key event types (`input_handler.cc:1200-1213`); anything else is none of them.
fn key_stroke(ty: &str) -> Option<webapi::input::Stroke> {
    Some(match ty {
        "keyDown" => webapi::input::Stroke::Down,
        "rawKeyDown" => webapi::input::Stroke::RawDown,
        "char" => webapi::input::Stroke::Char,
        "keyUp" => webapi::input::Stroke::Up,
        _ => return None,
    })
}

enum Emit {
    /// FIXER C6: the pointer arriving at (x, y) -- ONE platform move through Blink's pipeline
    /// (`webapi::input::platform_move`): `pointerrawupdate` per raw sample when a listener wants it, `:hover`,
    /// the pointer and mouse boundary events, `pointermove`, `mousemove`, into a frame when the point is on one.
    /// `samples` are the raw samples of one display frame merged into this move, `(x, y, µs offset)`, oldest
    /// first, the move itself last -- Chrome delivers continuous input once per frame with the rest as
    /// `getCoalescedEvents()` (measured ~120 mousemoves/s here against Chrome's <= 60). `hover` false is the
    /// synthetic-coordinate path of `click_selector`: a bare `pointermove` + `mousemove` at the point, no
    /// boundary events, because that point is not where the element is.
    Move { x: i32, y: i32, samples: Vec<(i32, i32, f64)>, hover: bool },
    /// A press / release of `button` at (x, y) as the platform reports it (`webapi::input::platform_button`):
    /// the pointer event, its compatibility mouse event, the focus on a press, the click on a release, the
    /// `dblclick` of a second click, the `contextmenu` of a right press.
    Press { x: i32, y: i32, button: i32 },
    Release { x: i32, y: i32, button: i32 },
    MouseSel { sel: String, kind: &'static str, x: i32, y: i32, buttons: i32, button: i32 },
    PointerSel { sel: String, kind: &'static str, x: i32, y: i32, buttons: i32, button: i32, pressure: f64 },
    /// One keystroke AT THE FOCUS -- the only place a browser types. `Stroke` says
    /// which of CDP's four key-event types this is, and the keydown form carries its own default
    /// action (`keypress` + the editing command) rather than the caller spelling it out.
    ///
    /// A PLATFORM key -- the keyboard the engine stands in for -- whose `KeyboardEvent`
    /// fields (virtual key, location, modifier state, what it types) are the platform's, resolved at
    /// dispatch time by `webapi::input::platform_key_input` so a held Shift reaches every later event.
    Key { stroke: webapi::input::Stroke, key: String, code: String, text: String },
    /// A key event whose every field the CALLER gave -- CDP `Input.dispatchKeyEvent`, its
    /// parameters verbatim. It moves no platform keyboard state.
    KeyEvent { stroke: webapi::input::Stroke, input: webapi::input::KeyInput },
    /// CDP `Input.insertText` -- one edit at the focus, no key events. Chrome's own
    /// answer for a string that did not come from a key press.
    Insert { text: String },
    /// CDP `Input.imeSetComposition` -- the IME's composition text at the focus (D88 step 2).
    Compose { text: String },
    FocusSel { sel: String },
    /// A wheel step at (x, y): a `WheelEvent` at the element there (it went to the document).
    Wheel { x: i32, y: i32, dx: f64, dy: f64 },
    Touch { kind: &'static str, x: i32, y: i32, force: f64, radius_x: f64, radius_y: f64 },
}

impl Engine {
    /// Read the page clock (`performance.now()`), so behavioral `event.timeStamp`s anchor to the SAME
    /// DOMHighResTimeStamp origin the detector reads — the two must agree. Read in Rust (`webapi::clock_now`): the
    /// page's `performance.now` is the page's to replace, and an engine step never calls what a page can replace.
    pub(crate) fn perf_now(&mut self) -> f64 {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::clock_now(scope)
    }

    /// Real-time scheduler (realtime): emit each `Timed` event at its wall-clock offset —
    /// sleeping the generated Δt between events — with `timeStamp` anchored to the page clock at dispatch,
    /// so `performance.now()` at a handler tracks `event.timeStamp` (no burst-dispatch tell). Uses the
    /// tokio clock, so `#[tokio::test(start_paused)]` advances it instantly (fast tests, real code path).
    ///
    /// Returns the number of EVENTS DISPATCHED, which is no longer the number of emissions: six
    /// variants answer their own tally and `continue`. A `Move`, `Press` or `Release` (FIXER C6) fires
    /// whatever Blink's pipeline makes of it -- boundary events of both families where the pointer
    /// crossed an edge, the pointer and mouse events, the focus, the click; the
    /// `Key` fires one to four (`keydown`, its `keypress`, and the `beforeinput`/`input` pair of
    /// the editing command), `Insert` zero or two, and the focus variant zero, two or four.
    /// The difference matters because `tests/behavior.rs` asserts that every event the engine
    /// reports emitting reached a listener — an invariant that catches a silently dropped event,
    /// and that a scheduled-emission count would quietly stop checking the moment one emission
    /// stopped meaning one event.
    async fn play(&mut self, timed: Vec<Timed>) -> anyhow::Result<usize> {
        use deno_core::v8;
        let t0 = tokio::time::Instant::now();
        // The schedule's start as an absolute instant: each event happens at it plus the schedule's own offset (the
        // tokio clock, so a paused test advances it) and is stamped as Blink stamps a platform event -- the absolute
        // time clamped (`webapi::clock_at`), not a clamped reading plus a raw offset, which was off the tick grid.
        let t0_std = std::time::Instant::now();
        let mut n = 0usize;
        // The synthetic-coordinate moves' own previous point, for their movement.
        let mut last_move = (self.cursor.x.round() as i32, self.cursor.y.round() as i32);
        for item in &timed {
            let target = std::time::Duration::from_micros(item.at_us.max(0.0) as u64);
            let elapsed = t0.elapsed();
            if target > elapsed {
                tokio::time::sleep(target - elapsed).await;
            }
            let at_now = t0_std + t0.elapsed();
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            let ts_ms = webapi::clock_at(scope, at_now);
            match &item.emit {
                Emit::Move { x, y, samples, hover } => {
                    // Each raw sample keeps its own time; the move is stamped with the last one's.
                    let mut raw: Vec<(f64, f64, f64)> = Vec::with_capacity(samples.len());
                    for (sx, sy, us) in samples.iter() {
                        let at = t0_std + std::time::Duration::from_micros(us.max(0.0) as u64);
                        raw.push((*sx as f64, *sy as f64, webapi::clock_at(scope, at)));
                    }
                    let ts = raw.last().map(|r| r.2).unwrap_or(ts_ms);
                    if *hover {
                        n += webapi::input::platform_move(scope, *x as f64, *y as f64, raw, ts);
                    } else {
                        let (mx, my) = (*x - last_move.0, *y - last_move.1);
                        webapi::input::dispatch_trusted_pointer(scope, "pointermove", *x as f64, *y as f64, mx as f64, my as f64, 0, -1, 0.0, ts);
                        webapi::input::dispatch_trusted_mouse(scope, "mousemove", *x as f64, *y as f64, mx as f64, my as f64, 0, -1, ts);
                        n += 2;
                    }
                    last_move = (*x, *y);
                    continue;
                }
                Emit::Press { x, y, button } => {
                    n += webapi::input::platform_button(scope, webapi::input::MouseKind::Down, *x as f64, *y as f64, *button, ts_ms);
                    continue;
                }
                Emit::Release { x, y, button } => {
                    n += webapi::input::platform_button(scope, webapi::input::MouseKind::Up, *x as f64, *y as f64, *button, ts_ms);
                    continue;
                }
                Emit::MouseSel { sel, kind, x, y, buttons, button } => {
                    webapi::input::dispatch_trusted_mouse_on_selector(scope, sel, kind, *x as f64, *y as f64, *buttons, *button, ts_ms);
                }
                Emit::PointerSel { sel, kind, x, y, buttons, button, pressure } => {
                    webapi::input::dispatch_trusted_pointer_on_selector(scope, sel, kind, *x as f64, *y as f64, *buttons, *button, *pressure, ts_ms);
                }
                Emit::Key { stroke, key, code, text } => {
                    let input = webapi::input::platform_key_input(scope, *stroke, key, code, text);
                    n += webapi::input::key_at_focus(scope, *stroke, &input, ts_ms);
                    continue;
                }
                Emit::KeyEvent { stroke, input } => {
                    n += webapi::input::key_at_focus(scope, *stroke, input, ts_ms);
                    continue;
                }
                Emit::Insert { text } => {
                    n += webapi::input::insert_text_at_focus(scope, text, ts_ms);
                    continue;
                }
                Emit::Compose { text } => {
                    n += webapi::input::ime_set_composition_at_focus(scope, text, ts_ms);
                    continue;
                }
                Emit::FocusSel { sel } => {
                    n += webapi::input::focus_selector(scope, sel, ts_ms).unwrap_or(0);
                    continue;
                }
                Emit::Wheel { x, y, dx, dy } => {
                    webapi::input::dispatch_trusted_wheel(scope, *x as f64, *y as f64, *dx, *dy, ts_ms);
                }
                Emit::Touch { kind, x, y, force, radius_x, radius_y } => {
                    webapi::input::dispatch_trusted_touch(scope, kind, *x as f64, *y as f64, *force, *radius_x, *radius_y, ts_ms);
                }
            }
            // Every variant that reaches here is exactly one event; the others `continue` above with
            // their own tally.
            n += 1;
        }
        Ok(n)
    }

    /// The display's frame interval in µs (the persona's refresh rate): what continuous input is delivered on.
    fn frame_interval_us(&mut self) -> f64 {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let ms = webapi::input::frame_interval_ms(scope);
        if ms.is_finite() && ms > 0.0 { ms * 1000.0 } else { 16_666.7 }
    }

    /// The behaviour model's path from `from` to `to` (a target `target_w` wide): what a CDP client's move becomes
    /// when korvus humanizes input (`korvus serve --humanize`, goal 1020 step 5).
    pub fn plan_move(&self, from: spec::Point, to: spec::Point, target_w: f64) -> Vec<spec::MotionSample> {
        self.behavior.plan_move(from, to, target_w)
    }

    /// The move stream for `from`→`to`: the behaviour model's samples, delivered as Chrome delivers a mouse --
    /// ONE move per display frame, at the frame's end, carrying the frame's samples as its coalesced events
    /// (FIXER C6: Blink's main-thread event queue aligns continuous input to `requestAnimationFrame`; korvus
    /// fired every ~8 ms sample as its own pair, ~120 mousemoves/s against Chrome's <= 60). Each delivered move
    /// is a whole platform move (`Emit::Move`): `:hover`, the boundary events of both families wherever the
    /// trajectory crosses an element edge, `pointermove`, `mousemove`. Returns the last sample's µs offset.
    ///
    /// `hover` is false for ONE caller, [`Engine::click_selector`]'s fallback for an element with no box in
    /// the viewport, which moves to a SYNTHETIC coordinate derived from the selector string
    /// (`behavior::synth_coord`) and hand-rolls its own `pointerover`/`mouseover` ON the selector.
    fn move_timed(&mut self, from: spec::Point, to: spec::Point, target_w: f64, hover: bool, out: &mut Vec<Timed>) -> f64 {
        let samples = self.behavior.plan_move(from, to, target_w);
        let frame = self.frame_interval_us();
        let mut i = 0;
        while i < samples.len() {
            let slot = (samples[i].t_us / frame).floor();
            let mut j = i;
            while j + 1 < samples.len() && (samples[j + 1].t_us / frame).floor() == slot {
                j += 1;
            }
            let group: Vec<(i32, i32, f64)> = samples[i..=j].iter().map(|s| (s.x, s.y, s.t_us)).collect();
            let last = &samples[j];
            // Delivered when the frame the samples fell in begins its main-frame work: the end of that slot.
            let at_us = ((slot + 1.0) * frame).max(last.t_us);
            out.push(Timed { at_us, emit: Emit::Move { x: last.x, y: last.y, samples: group, hover } });
            i = j + 1;
        }
        samples.last().map(|s| s.t_us).unwrap_or(0.0)
    }

    /// Dispatch a single genuinely-trusted mouse event into the page (a low-level primitive; timeStamp is
    /// the page clock now) -- at the element under (x, y), with Blink's fields for everything not given (FIXER
    /// C6: it went to the document). Multi-event human actions use the real-time scheduler instead.
    pub fn dispatch_trusted_mouse(&mut self, kind: &str, x: f64, y: f64, movement_x: f64, movement_y: f64, buttons: i32, button: i32) -> anyhow::Result<()> {
        use deno_core::v8;
        let ts = self.perf_now();
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::input::dispatch_trusted_mouse(scope, kind, x, y, movement_x, movement_y, buttons, button, ts);
        Ok(())
    }

    /// Dispatch a single genuinely-trusted `TouchEvent` into the page (a low-level primitive; timeStamp is
    /// the page clock now) -- the one-finger RAW path B6's `inject_touch` takes (step 12), the touch analogue of
    /// [`Engine::dispatch_trusted_mouse`]. `kind` is "touchstart"|"touchmove"|"touchend"; `force` and the
    /// elliptical `radius_x`/`radius_y` are the contact the caller supplies (CDP's touch-point defaults for an
    /// injected finger). A MOBILE persona's humanised `tap`/`swipe` use the real-time scheduler instead.
    pub fn dispatch_trusted_touch(&mut self, kind: &str, x: f64, y: f64, force: f64, radius_x: f64, radius_y: f64) -> anyhow::Result<()> {
        use deno_core::v8;
        let ts = self.perf_now();
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::input::dispatch_trusted_touch(scope, kind, x, y, force, radius_x, radius_y, ts);
        Ok(())
    }

    /// Move the cursor to `(to_x, to_y)` as a human would — WindMouse + Fitts + tremor — dispatched in
    /// REAL TIME (each `mousemove` at its human Δt, timeStamp tracking `performance.now()`).
    pub async fn move_mouse(&mut self, to_x: f64, to_y: f64, target_w: f64) -> anyhow::Result<usize> {
        let from = self.cursor;
        let to = spec::Point::new(to_x, to_y);
        let mut timed = Vec::new();
        self.move_timed(from, to, target_w, true, &mut timed);
        self.cursor = to;
        self.play(timed).await
    }

    /// The behaviour model's humanised keystroke stream as a schedule (dwell/flight/digraph/rollover
    /// + ~6% backspace-corrected typos), every emission aimed at THE FOCUSED ELEMENT.
    ///
    /// The keystrokes used to be dispatched on `document` and the field edit lived in a
    /// separate, selector-keyed path. One schedule now, and `Emit::Key`'s keydown form runs the
    /// whole default action — `keypress`, then `beforeinput` → the value change → `input` when the
    /// focused element is an editable control. `text` per keystroke is what the key TYPES, which is
    /// what Chrome inserts; a non-character key (Backspace) carries an empty one and still edits,
    /// because the editing command comes from the key.
    fn keystrokes(&mut self, text: &str) -> Vec<Timed> {
        self.behavior
            .type_text(text)
            .iter()
            .map(|e| {
                let (stroke, typed) = match e.kind {
                    behavior::KeyEventKind::KeyDown => (
                        webapi::input::Stroke::Down,
                        if e.key.chars().count() == 1 { e.key.clone() } else { String::new() },
                    ),
                    behavior::KeyEventKind::KeyUp => (webapi::input::Stroke::Up, String::new()),
                };
                Timed {
                    at_us: e.t_us,
                    emit: Emit::Key { stroke, key: e.key.clone(), code: e.code.clone(), text: typed },
                }
            })
            .collect()
    }

    /// Type `text` as a human would (dwell/flight/digraph/rollover + ~6% backspace-corrected typos),
    /// dispatched in real time as trusted keystrokes AT THE FOCUSED ELEMENT.
    ///
    /// With nothing focused the keystrokes land on `<body>` and change nothing, which is what Chrome
    /// does — and is why this is not the entry point CDP's `Input.insertText` uses.
    pub async fn type_text(&mut self, text: &str) -> anyhow::Result<usize> {
        let timed = self.keystrokes(text);
        self.play(timed).await
    }

    /// Type `text` INTO the field matched by `selector`: focus it first (which moves the focus ring
    /// and fires `blur`/`focusout`/`focus`/`focusin`), then run the same humanised keystroke stream
    /// `type_text` runs. `None` if the selector matches nothing.
    ///
    /// This used to be the ONLY path that could change a field's value, and it took a
    /// selector, so nothing arriving over CDP could reach it. It is now `focus` + `type_text`'s own
    /// body — the difference between the two is the focus step and nothing else, which is exactly
    /// the difference a browser has between typing and clicking-then-typing.
    pub async fn type_into(&mut self, selector: &str, text: &str) -> anyhow::Result<Option<usize>> {
        use deno_core::v8;
        let ts = self.perf_now();
        let focused = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            webapi::input::focus_selector(scope, selector, ts)
        };
        let Some(focus_events) = focused else { return Ok(None) };
        let timed = self.keystrokes(text);
        let n = self.play(timed).await?;
        Ok(Some(n + focus_events))
    }

    /// CDP `Input.insertText`: put `text` into the focused element in ONE edit, with no key events.
    ///
    /// Measured on stock Chrome 150: `beforeinput` → the value change → `input`, all three carrying
    /// the WHOLE string as `data` and `inputType: "insertText"`, and not one `keydown` among them.
    /// With nothing editable focused it fires nothing at all and returns 0 — also measured, on a
    /// freshly navigated page where nothing had ever been focused.
    pub async fn insert_text(&mut self, text: &str) -> anyhow::Result<usize> {
        self.play(vec![Timed { at_us: 0.0, emit: Emit::Insert { text: text.to_string() } }]).await
    }

    /// CDP `Input.imeSetComposition`: the IME's composition `text` at the focus, which `insert_text` commits (D88
    /// step 2; `webapi::input::ime_set_composition_at_focus`).
    pub async fn ime_set_composition(&mut self, text: &str) -> anyhow::Result<usize> {
        self.play(vec![Timed { at_us: 0.0, emit: Emit::Compose { text: text.to_string() } }]).await
    }

    /// One PLATFORM key event at the focused element -- a person pressing `key` on the physical key
    /// `code` -- of the type named in CDP's vocabulary. `text` is what the key types; empty lets the
    /// platform keyboard decide (a printable key types itself, Enter `"\r"`, nothing under Control).
    ///
    /// The four types are CDP's and the mapping was measured rather than read: `keyDown` carries
    /// its default action (`keypress` + the editing command), `rawKeyDown` is the bare `keydown`
    /// because the client means to send the character itself as a `char`, and `rawKeyDown` + `char`
    /// + `keyUp` produced exactly the same six events as `keyDown{text}` + `keyUp`.
    ///
    /// The event's virtual key, location and modifier state are the platform keyboard's
    /// (`webapi::input::platform_key_input`) -- a `keyDown` of Shift holds Shift for every later event
    /// until its `keyUp`, as the recorded X11 keys do. For a caller that supplies every field itself,
    /// which is what CDP is, see [`Engine::dispatch_key_event`].
    pub async fn dispatch_key(&mut self, ty: &str, key: &str, code: &str, text: &str) -> anyhow::Result<usize> {
        let Some(stroke) = key_stroke(ty) else { return Ok(0) };
        self.play(vec![Timed {
            at_us: 0.0,
            emit: Emit::Key { stroke, key: key.to_string(), code: code.to_string(), text: text.to_string() },
        }])
        .await
    }

    /// CDP `Input.dispatchKeyEvent`: one key event at the focused element whose every field is the
    /// caller's (`webapi::input::KeyInput::from_cdp`) -- `modifiers`, `windowsVirtualKeyCode`,
    /// `location`, `autoRepeat` -- the way `InputHandler::DispatchKeyEvent` hands them to the renderer.
    /// `None` for a `type` that is none of CDP's four (Chrome refuses it before anything happens).
    pub async fn dispatch_key_event(&mut self, ty: &str, input: webapi::input::KeyInput) -> anyhow::Result<Option<usize>> {
        let Some(stroke) = key_stroke(ty) else { return Ok(None) };
        self.play(vec![Timed { at_us: 0.0, emit: Emit::KeyEvent { stroke, input } }]).await.map(Some)
    }

    /// Scroll ~`total_dy` px as one human wheel gesture (persona-coherent), dispatched in real time as
    /// trusted `wheel` events.
    pub async fn scroll(&mut self, total_dy: f64) -> anyhow::Result<usize> {
        let events = self.behavior.scroll(total_dy);
        // At the cursor: a wheel goes to the element under the pointer.
        let (x, y) = (self.cursor.x.round() as i32, self.cursor.y.round() as i32);
        let timed: Vec<Timed> = events
            .iter()
            .map(|e| Timed { at_us: e.t_us, emit: Emit::Wheel { x, y, dx: e.delta_x, dy: e.delta_y } })
            .collect();
        self.play(timed).await
    }

    /// Tap at (x,y) as a finger would — touchstart (force + elliptical contact) → touchend after a dwell,
    /// dispatched in real time. For a MOBILE persona (touch).
    pub async fn tap(&mut self, x: f64, y: f64) -> anyhow::Result<usize> {
        let samples = self.behavior.tap(x, y);
        let timed: Vec<Timed> = samples
            .iter()
            .map(|s| Timed { at_us: s.t_us, emit: Emit::Touch { kind: touch_kind(s.phase), x: s.x, y: s.y, force: s.force, radius_x: s.radius_x, radius_y: s.radius_y } })
            .collect();
        self.play(timed).await
    }

    /// Swipe from (fx,fy) to (tx,ty) — touchstart → touchmove* (with a fling) → touchend.
    pub async fn swipe(&mut self, fx: f64, fy: f64, tx: f64, ty: f64) -> anyhow::Result<usize> {
        let samples = self.behavior.swipe(spec::Point::new(fx, fy), spec::Point::new(tx, ty));
        let timed: Vec<Timed> = samples
            .iter()
            .map(|s| Timed { at_us: s.t_us, emit: Emit::Touch { kind: touch_kind(s.phase), x: s.x, y: s.y, force: s.force, radius_x: s.radius_x, radius_y: s.radius_y } })
            .collect();
        self.play(timed).await
    }

    /// Move to (x,y) with a human trajectory, then (after a brief settle + press-hold) a trusted click
    /// graph THERE — one real-time schedule, never a teleport-to-click.
    ///
    /// FIXER C6: the press and the release are two platform events (`Emit::Press`, `Emit::Release`), and
    /// everything a browser makes of them comes from Blink's pipeline at the element under the point: the
    /// `pointerdown` / `mousedown` (`detail` = the click count), the focus (mousedown's default action),
    /// then `pointerup` / `mouseup` and the `click` on the nearest common ancestor of the two
    /// targets -- a `PointerEvent` -- and `dblclick` after a second click within 500 ms and 2 px.
    pub async fn click_at(&mut self, x: f64, y: f64, target_w: f64) -> anyhow::Result<usize> {
        self.button_click_at(x, y, target_w, 0).await
    }

    /// [`Engine::click_at`] with any button: 0 left, 1 middle, 2 right (a right press also sends
    /// `contextmenu`, as on Linux; the release then sends `auxclick`).
    pub async fn button_click_at(&mut self, x: f64, y: f64, target_w: f64, button: i32) -> anyhow::Result<usize> {
        let from = self.cursor;
        let to = spec::Point::new(x, y);
        let mut timed = Vec::new();
        // A real viewport coordinate, so the hover chain follows the trajectory -- the cursor
        // arriving is what makes `mouseover`/`mouseenter` fire here, as it does in a browser.
        let end = self.move_timed(from, to, target_w, true, &mut timed);
        let press_us = self.behavior.press_dwell_us();
        let (ix, iy) = (x.round() as i32, y.round() as i32);
        let down = timed.last().map(|t| t.at_us).unwrap_or(end).max(end) + 20_000.0;
        timed.push(Timed { at_us: down, emit: Emit::Press { x: ix, y: iy, button } });
        timed.push(Timed { at_us: down + press_us, emit: Emit::Release { x: ix, y: iy, button } });
        self.cursor = to;
        self.play(timed).await
    }

    /// CDP `Input.dispatchMouseEvent` for `mouseMoved` / `mousePressed` / `mouseReleased`: EXACTLY the event
    /// the client sent, as Chrome dispatches it (`content/browser/devtools/protocol/input_handler.cc`
    /// `CreateWebMouseEvent`) -- no trajectory, no added click: a press is a press and a release is a release,
    /// so a 12 s hold is a 12 s hold (a bot-detection script's press-and-hold). `buttons` and `click_count` are the
    /// client's own (Chrome sends what it is given: a press without `clickCount` is a `mousedown` with
    /// `detail` 0 and no `click`). Returns the DOM events dispatched.
    /// Where the pointer is: the point the last mouse event left it at.
    pub fn cursor(&self) -> (f64, f64) {
        (self.cursor.x, self.cursor.y)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mouse_event(
        &mut self,
        kind: webapi::input::MouseKind,
        x: f64,
        y: f64,
        button: i32,
        buttons: u16,
        click_count: i32,
        modifiers: webapi::input::Modifiers,
    ) -> anyhow::Result<usize> {
        use deno_core::v8;
        let ts = self.perf_now();
        if kind == webapi::input::MouseKind::Move {
            self.cursor = spec::Point::new(x, y);
        }
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let input = webapi::input::MouseInput { kind, x, y, button, buttons, click_count, modifiers, timestamp_ms: ts, coalesced: Vec::new() };
        Ok(webapi::input::mouse_input(scope, &input))
    }

    /// CDP `Input.dispatchDragEvent`: a DevTools client moving a drag to (x, y) -- `dragEnter`, `dragOver`, `drop`,
    /// `dragCancel` -- with the data it carries (`webapi::input::dispatch_drag`).
    pub fn dispatch_drag(&mut self, kind: &str, x: f64, y: f64, items: Vec<(String, String)>, mask: u32, modifiers: webapi::input::Modifiers) -> usize {
        use deno_core::v8;
        let ts = self.perf_now();
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::input::dispatch_drag(scope, kind, x, y, items, mask, modifiers, ts)
    }

    /// CDP `Input.dispatchMouseEvent` `mouseWheel`: one `WheelEvent` at the element under (x, y), with the
    /// client's deltas (`deltaMode` 0) and `wheelDelta*` = the notch direction times 120.
    pub fn mouse_wheel(&mut self, x: f64, y: f64, delta_x: f64, delta_y: f64, buttons: u16, modifiers: webapi::input::Modifiers) -> anyhow::Result<usize> {
        use deno_core::v8;
        let ts = self.perf_now();
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let notch = |d: f64| if d == 0.0 { 0.0 } else { -d.signum() * 120.0 };
        let input = webapi::input::WheelInput {
            x,
            y,
            delta_x,
            delta_y,
            delta_mode: 0,
            wheel_delta_x: notch(delta_x),
            wheel_delta_y: notch(delta_y),
            buttons,
            modifiers,
            timestamp_ms: ts,
        };
        Ok(webapi::input::wheel_input(scope, &input))
    }

    /// This window's `(innerWidth, innerHeight)` as whole CSS pixels — the same numbers
    /// `window.innerWidth` answers, from the same fallback chain.
    ///
    /// That chain was `window_geometry` in this crate, beside the op that served the JS.
    /// Both are gone; `webapi::window_geometry_of` is the one definition, and the `Window` table's
    /// `innerWidth` row reads it too, so "the same numbers" is now true by construction rather than
    /// by two functions agreeing.
    fn viewport(&self) -> (i32, i32) {
        let g = webapi::window_geometry_of(&self.persona().screen());
        (g[0] as i32, g[1] as i32)
    }

    /// Interaction coupling (P5): assign the element matched by `selector` a stable synthetic viewport
    /// coordinate (Korvus has no layout), move the cursor there in real time, then a trusted click graph
    /// ON that element. `None` if the selector matches nothing.
    pub async fn click_selector(&mut self, selector: &str, target_w: f64) -> anyhow::Result<Option<usize>> {
        use deno_core::v8;
        let exists = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            webapi::input::selector_exists(scope, selector)
        };
        if !exists {
            return Ok(None);
        }
        // FIXER C6: an element with a box in the viewport is clicked WHERE IT IS -- its centre, through the same
        // pipeline as any click (`click_at`). The synthetic coordinate below predates layout, and stays only for an
        // element with no box on screen.
        let center = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            webapi::input::element_center(scope, selector)
        };
        if let Some((cx, cy)) = center {
            return Ok(Some(self.click_at(cx, cy, target_w).await?));
        }
        let (vw, vh) = self.viewport();
        let (sxi, syi) = behavior::synth_coord(selector, self.behavior.motor_seed(), vw, vh);
        let from = self.cursor;
        let to = spec::Point::new(sxi as f64, syi as f64);
        let mut timed = Vec::new();
        // `false`: (sxi, syi) is SYNTHETIC -- derived from the selector string, not from the
        // element's box -- so hit-testing it would fire a second hover pair at whatever is really
        // there, contradicting the hand-rolled one below. See `move_timed`'s note.
        let end = self.move_timed(from, to, target_w, false, &mut timed);
        let end = timed.last().map(|t| t.at_us).unwrap_or(end).max(end);
        let press_us = self.behavior.press_dwell_us();
        let down = end + 20_000.0;
        let sel = selector.to_string();
        // Hover onset at arrival: pointerover/enter + mouseover/enter (the cursor entering the element).
        timed.push(Timed { at_us: end, emit: Emit::PointerSel { sel: sel.clone(), kind: "pointerover", x: sxi, y: syi, buttons: 0, button: 0, pressure: 0.0 } });
        timed.push(Timed { at_us: end, emit: Emit::MouseSel { sel: sel.clone(), kind: "mouseover", x: sxi, y: syi, buttons: 0, button: 0 } });
        timed.push(Timed { at_us: end, emit: Emit::PointerSel { sel: sel.clone(), kind: "pointerenter", x: sxi, y: syi, buttons: 0, button: 0, pressure: 0.0 } });
        timed.push(Timed { at_us: end, emit: Emit::MouseSel { sel: sel.clone(), kind: "mouseenter", x: sxi, y: syi, buttons: 0, button: 0 } });
        // Full pointer graph ON the element: pointerdown → mousedown → focus → pointerup → mouseup → click.
        timed.push(Timed { at_us: down, emit: Emit::PointerSel { sel: sel.clone(), kind: "pointerdown", x: sxi, y: syi, buttons: 1, button: 0, pressure: 0.5 } });
        timed.push(Timed { at_us: down, emit: Emit::MouseSel { sel: sel.clone(), kind: "mousedown", x: sxi, y: syi, buttons: 1, button: 0 } });
        timed.push(Timed { at_us: down, emit: Emit::FocusSel { sel: sel.clone() } });
        timed.push(Timed { at_us: down + press_us, emit: Emit::PointerSel { sel: sel.clone(), kind: "pointerup", x: sxi, y: syi, buttons: 0, button: 0, pressure: 0.0 } });
        timed.push(Timed { at_us: down + press_us, emit: Emit::MouseSel { sel: sel.clone(), kind: "mouseup", x: sxi, y: syi, buttons: 0, button: 0 } });
        timed.push(Timed { at_us: down + press_us, emit: Emit::MouseSel { sel, kind: "click", x: sxi, y: syi, buttons: 0, button: 0 } });
        self.cursor = to;
        let n = self.play(timed).await?;
        Ok(Some(n))
    }

    /// Emit plausible AMBIENT activity over `rounds` — small idle cursor drifts interleaved with occasional
    /// reading scrolls — so a session is never zero-behaviour (a positive bot signal). Built as ONE
    /// continuous real-time schedule (a single reading dwell): one page-clock anchor, one relative
    /// timeline with reading pauses between actions, so timeStamps stay monotonic across both channels.
    /// All events genuinely trusted.
    pub async fn ambient(&mut self, rounds: usize) -> anyhow::Result<usize> {
        let mut timed: Vec<Timed> = Vec::new();
        let mut clock = 0.0_f64; // relative µs offset accumulator across the whole dwell
        let mut cursor = self.cursor;
        for i in 0..rounds {
            let (ox, oy) = self.behavior.idle_offset(60.0);
            let to = spec::Point::new((cursor.x + ox).max(0.0), (cursor.y + oy).max(0.0));
            let mut seg = Vec::new();
            let end = self.move_timed(cursor, to, 200.0, true, &mut seg);
            for mut t in seg {
                t.at_us += clock;
                // The raw samples a move carries are on the same timeline as the move itself.
                if let Emit::Move { samples, .. } = &mut t.emit {
                    for sample in samples.iter_mut() {
                        sample.2 += clock;
                    }
                }
                timed.push(t);
            }
            clock += end + 250_000.0; // reading pause (~250 ms) before the next action
            cursor = to;
            if i % 3 == 2 {
                let events = self.behavior.scroll(300.0);
                let mut wend = 0.0_f64;
                for e in &events {
                    let (x, y) = (cursor.x.round() as i32, cursor.y.round() as i32);
                    timed.push(Timed { at_us: clock + e.t_us, emit: Emit::Wheel { x, y, dx: e.delta_x, dy: e.delta_y } });
                    wend = wend.max(e.t_us);
                }
                clock += wend + 250_000.0;
            }
        }
        self.cursor = cursor;
        self.play(timed).await
    }
}
