//! The viewer channel (D88 step 3): what a person's client may do over a ticket.
//!
//! The automation channel is all of CDP -- what Playwright and a server hold -- and a web page never opens it
//! (`access.rs`). A client in a page (`client`, or one embedded in another site) opens the viewer
//! channel instead, with a ticket the automation channel minted (`Korvus.createViewer`) or the one `korvus serve`
//! prints for its own client. The ticket is the key: one browser context (or, the endpoint's own, any route), an
//! expiry, view-only or interactive, revocable (`Korvus.revokeViewer`). Over it a connection may do what a person
//! at a browser does -- see the screen, type and click, open and close tabs in its context, move through the
//! address bar and the history, answer a dialog, size the window -- and nothing a program does: no script, no
//! cookies or storage, no network log, no other context. That is how Browserless's live URL, Browserbase's and
//! Steel's live views and Hyperbeam's embed URL hand a page a session without the API key, enforced here rather
//! than in the page.

/// A ticket's grant: whose context, what it may do, until when, from which pages.
#[derive(Clone, Debug)]
pub struct Ticket {
    /// The browser context its connections live in; `None` for the endpoint's own ticket, whose connection takes
    /// the route it opens (`/` or `/persona/<name>`).
    pub context: Option<String>,
    /// Input, navigation, tabs, dialogs, the window -- else the screen alone.
    pub interactive: bool,
    /// When it stops opening connections and ends the open ones; `None`: with the endpoint.
    pub expires: Option<std::time::Instant>,
    /// The page origins it may be opened from (lower-cased); empty: any, the ticket being the key.
    pub origins: Vec<String>,
    /// The tenant of the connection that minted it (plugin ABI 3, G1), which its connections belong to.
    pub tenant: i64,
}

impl Ticket {
    pub fn expired(&self) -> bool {
        self.expires.is_some_and(|e| std::time::Instant::now() >= e)
    }

    /// May a page of `origin` (none: not a page) open it?
    pub fn admits(&self, origin: Option<&str>) -> bool {
        match origin.map(|o| o.trim().trim_end_matches('/').to_ascii_lowercase()).filter(|o| !o.is_empty()) {
            None => true,
            Some(o) => self.origins.is_empty() || self.origins.iter().any(|x| *x == o),
        }
    }
}

/// A fresh ticket string: 32 hex digits from the OS's randomness (`ids::hex32`).
pub fn mint() -> String {
    crate::ids::hex32().to_ascii_lowercase()
}

/// May a viewer connection send `method`? `on_page`: on a page's session (or a page socket), else on the browser.
/// `interactive`: the ticket's grant.
pub fn allows(method: &str, on_page: bool, interactive: bool) -> bool {
    const SEE_BROWSER: &[&str] = &[
        "Browser.getVersion",
        "Browser.getWindowForTarget",
        "Target.setDiscoverTargets",
        "Target.getTargets",
        "Target.getTargetInfo",
        "Target.attachToTarget",
        "Target.detachFromTarget",
        "Target.activateTarget",
        "Target.setAutoAttach",
    ];
    const DRIVE_BROWSER: &[&str] = &["Target.createTarget", "Target.closeTarget", "Browser.setWindowBounds"];
    const SEE_PAGE: &[&str] = &[
        "Page.enable",
        "Page.disable",
        "Page.getFrameTree",
        "Page.getLayoutMetrics",
        "Page.getNavigationHistory",
        "Page.setLifecycleEventsEnabled",
        "Page.startScreencast",
        "Page.stopScreencast",
        "Page.screencastFrameAck",
        "Page.captureScreenshot",
        "Runtime.runIfWaitingForDebugger",
    ];
    const DRIVE_PAGE: &[&str] = &[
        "Page.navigate",
        "Page.reload",
        "Page.navigateToHistoryEntry",
        "Page.handleJavaScriptDialog",
        "Page.bringToFront",
        "Page.stopLoading",
        "Input.dispatchMouseEvent",
        "Input.dispatchKeyEvent",
        "Input.dispatchTouchEvent",
        "Input.insertText",
        "Input.imeSetComposition",
    ];
    let (see, drive) = if on_page { (SEE_PAGE, DRIVE_PAGE) } else { (SEE_BROWSER, DRIVE_BROWSER) };
    see.contains(&method) || (interactive && drive.contains(&method))
}

/// May a viewer connection hear `method`? What a person's client shows -- the page's navigations, dialogs, frames
/// and the targets -- and never another client's network log, console or script contexts.
pub fn hears(method: &str) -> bool {
    method.starts_with("Page.") || method.starts_with("Target.") || method.starts_with("Inspector.")
}

/// May a viewer open `url`? A web page, or a blank one: never `javascript:` (script), `file:` (the endpoint's own
/// disk) or anything else a person's address bar has no business reaching on the endpoint's machine.
pub fn opens(url: &str) -> bool {
    let u = url.trim();
    u.is_empty() || u.eq_ignore_ascii_case("about:blank") || url::Url::parse(u).is_ok_and(|p| matches!(p.scheme(), "http" | "https"))
}

/// The refusal a viewer connection gets for a method its ticket does not allow.
pub fn refusal(method: &str) -> serde_json::Value {
    serde_json::json!({ "code": -32000, "message": format!("{method} is not allowed on a viewer connection") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_viewer_sees_and_drives_as_a_person() {
        assert!(allows("Page.startScreencast", true, false));
        assert!(allows("Target.getTargets", false, false));
        assert!(!allows("Input.dispatchMouseEvent", true, false), "view-only");
        assert!(allows("Input.dispatchMouseEvent", true, true));
        assert!(allows("Target.createTarget", false, true));
        assert!(!allows("Target.createTarget", false, false));
        // a page method is not a browser one
        assert!(!allows("Page.navigate", false, true));
    }

    #[test]
    fn a_viewer_runs_no_script_and_reads_nothing_private() {
        for m in [
            "Runtime.evaluate",
            "Runtime.callFunctionOn",
            "Network.getCookies",
            "Network.enable",
            "Storage.getCookies",
            "DOM.getDocument",
            "Fetch.enable",
            "Page.addScriptToEvaluateOnNewDocument",
            "Target.createBrowserContext",
            "Target.exposeDevToolsProtocol",
            "Browser.close",
            "Korvus.createViewer",
        ] {
            assert!(!allows(m, true, true) && !allows(m, false, true), "{m}");
        }
        assert!(!hears("Network.requestWillBeSent") && !hears("Runtime.consoleAPICalled") && hears("Page.frameNavigated"));
    }

    #[test]
    fn a_viewer_opens_web_pages_only() {
        assert!(opens("https://accounts.google.com/") && opens("http://127.0.0.1:8000/a") && opens("about:blank") && opens(""));
        assert!(!opens("javascript:alert(1)") && !opens("file:///etc/passwd") && !opens("data:text/html,x") && !opens("chrome://settings"));
    }

    #[test]
    fn a_ticket_admits_its_origins() {
        let t = Ticket { context: None, interactive: true, expires: None, origins: vec!["https://app.example".into()], tenant: -1 };
        assert!(t.admits(None) && t.admits(Some("https://APP.example/")) && !t.admits(Some("https://evil.example")));
        let any = Ticket { origins: vec![], ..t.clone() };
        assert!(any.admits(Some("https://evil.example")));
        let gone = Ticket { expires: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)), ..t };
        assert!(gone.expired());
        assert_eq!(mint().len(), 32);
        assert_ne!(mint(), mint());
    }
}
