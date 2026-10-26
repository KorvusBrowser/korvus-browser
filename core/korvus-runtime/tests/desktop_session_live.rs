//! LIVE gate: the Chrome persona is the DESKTOP SESSION's Chrome.
//!
//! This desktop's session is Wayland (`XDG_SESSION_TYPE=wayland`), and the stock Chrome 150 that
//! passes a challenge provider here runs in it. Two facts follow, both measured on that Chrome and re-emitted into
//! the bundle from a capture taken in the session (`reference/captures/chrome-150-linux-raw-
//! 20261008T122334.json`, `capture/emit.py --role surface/worker-surface/oracle`):
//!
//! * no `EyeDropper` -- Chromium's `chrome_browser_field_trials.cc` disables `features::kEyeDropper`
//!   whenever the session type is wayland (Edge, Opera and Brave on this desktop agree); the 2026-09-12
//!   capture ran under X11 and recorded it;
//! * `screen.availHeight === screen.height` (1080) and `availTop === 0` -- on Wayland Chrome cannot see
//!   the panel's work area (the X11 capture saw the GNOME strut: 1052 / 28).

#[tokio::test]
async fn the_chrome_persona_is_the_wayland_desktop_chrome() {
    let got = crate::keyboard_live::answers(
        crate::keyboard_live::CHROME_BUNDLE,
        "[typeof window.EyeDropper, 'EyeDropper' in window, screen.availHeight, screen.availTop, screen.height]",
    )
    .await;
    assert_eq!(got, vec![serde_json::json!("undefined"), false.into(), 1080.into(), 0.into(), 1080.into()]);
}
