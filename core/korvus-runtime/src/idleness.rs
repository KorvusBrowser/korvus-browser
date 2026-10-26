//! A document's network-quiet points, Blink's `networkAlmostIdle` and `networkIdle`.
//!
//! Chrome (chromium-150 `third_party/blink/renderer/core/loader/idleness_detector.{h,cc}`): from the
//! document's `DOMContentLoaded`, it is "almost idle" once no more than 2 of its requests have been in
//! flight for 500 ms, and "idle" once none has. Each point is reported once, stamped with the moment its
//! quiet period began. Puppeteer's `waitUntil: 'networkidle2' | 'networkidle0'` waits for exactly these
//! (`Page.lifecycleEvent`), and so does `korvus fetch --wait-until`.
//!
//! The detector is REPLAYED, not fed live: from the transport's request ledger and the page's task timeline
//! (`webapi::loaf_task_intervals`). Every request opens its ledger row in the task that asked for it
//! and closes it in the task its response arrives in -- the two places Blink runs `OnWillSendRequest` and
//! `OnDidLoadResource`. Blink looks for 500 quiet ms at the START of each task (`WillProcessTask`) and only then;
//! the replay looks at each task's start, each request's start and end (each inside a task), and the ticks of
//! Blink's `kNetworkQuietWatchdog` -- the 2 s timer `OnDidLoadResource` arms, which re-arms itself while a quiet
//! period holds, so a page that runs nothing still reaches its points. It does NOT look at the moment it is
//! asked: a point is reached when a task would have seen it.
//!
//! A BUSY THREAD IS NOT QUIET. Blink shifts each quiet base by the whole length of every task that ends while
//! the base is set (`DidProcessTask`), the task that set it included; the reported moment stays where the quiet
//! period began. The replay does the same with the timeline. Its tasks are script calls: work that runs no script
//! (parsing, style, layout) does not shift the base here, where Blink's task observer sees every task.

use std::time::{Duration, Instant};

/// `IdlenessDetector::kNetworkQuietWindow`.
const QUIET_WINDOW: Duration = Duration::from_millis(500);
/// `IdlenessDetector::kNetworkQuietMaximumConnections`.
const ALMOST_IDLE_MAX: usize = 2;
/// `IdlenessDetector::kNetworkQuietWatchdog`.
const WATCHDOG: Duration = Duration::from_secs(2);

/// When a document reached Blink's two network-quiet points. Each is the moment its quiet period
/// began, which is the timestamp Chrome's `Page.lifecycleEvent` carries. `None` means not reached (yet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkIdle {
    /// `networkAlmostIdle`: at most two requests in flight for the quiet window.
    pub almost_idle: Option<Instant>,
    /// `networkIdle`: no request in flight for the quiet window.
    pub idle: Option<Instant>,
}

/// One request of the document: when it started, and when it ended (`None` while in flight).
pub type Span = (Instant, Option<Instant>);

/// One finished task of the page's thread: when it started and ended.
pub type Task = (Instant, Instant);

impl crate::Engine {
    /// The current document's network-quiet points, on Blink's definitions. `default()`
    /// until `DOMContentLoaded`. The page's own requests count, and so do its in-process frames'; a
    /// worker's do not (`Reporter::target` names a worker). See the module documentation.
    pub fn network_idle(&mut self) -> NetworkIdle {
        let Some(dcl) = self.lifecycle_marks().0 else {
            return NetworkIdle::default();
        };
        let tasks = self.with_scope(|scope| webapi::loaf_task_intervals(scope));
        quiet_points(dcl, &transport::ledger().page_spans(), &tasks, Instant::now())
    }
}

/// Replay Blink's detector over `spans` and `tasks`, from `DOMContentLoaded` at `dcl` up to `now`.
pub fn quiet_points(dcl: Instant, spans: &[Span], tasks: &[Task], now: Instant) -> NetworkIdle {
    let in_flight_at_dcl = spans.iter().filter(|(s, e)| *s <= dcl && e.is_none_or(|e| e > dcl)).count();
    let mut d = Detector::start(dcl, in_flight_at_dcl);
    // What happens after `dcl`, in time order. At one instant: a task's end (its shift) before the next task's
    // start (its check), and a request's start before its end, so a request served in no time (a cache hit) still
    // opens before it closes.
    let mut events: Vec<(Instant, Event)> = Vec::with_capacity(spans.len() * 2 + tasks.len() * 2);
    for &(s, e) in spans {
        if s > dcl && s <= now {
            events.push((s, Event::Send));
        }
        if let Some(e) = e.filter(|e| *e > dcl && *e <= now) {
            events.push((e, Event::Load));
        }
    }
    for &(s, e) in tasks {
        if s > dcl && s <= now {
            events.push((s, Event::TaskStart));
        }
        // a task that ends after `dcl` shifts by its WHOLE length, the part before the base was set included
        if e > dcl && e <= now {
            events.push((e, Event::TaskEnd(e.saturating_duration_since(s))));
        }
    }
    events.sort_by_key(|&(at, ev)| (at, ev.rank()));
    for (at, ev) in events {
        d.watchdog_until(at);
        match ev {
            Event::TaskEnd(busy) => d.task_done(busy),
            Event::TaskStart => d.task_at(at),
            Event::Send => {
                d.task_at(at);
                d.will_send();
            }
            Event::Load => {
                d.task_at(at);
                d.finished(at);
            }
        }
    }
    d.watchdog_until(now);
    d.marks
}

/// What the replay meets after `DOMContentLoaded`.
#[derive(Clone, Copy, Debug)]
enum Event {
    /// A task ended, after running this long (`DidProcessTask`).
    TaskEnd(Duration),
    /// A task started (`WillProcessTask`).
    TaskStart,
    /// A request was sent, in a task.
    Send,
    /// A request ended, in a task.
    Load,
}

impl Event {
    fn rank(self) -> u8 {
        match self {
            Event::TaskEnd(_) => 0,
            Event::TaskStart => 1,
            Event::Send => 2,
            Event::Load => 3,
        }
    }
}

/// Blink's `IdlenessDetector`, one document's worth.
struct Detector {
    active: usize,
    in_almost_idle_period: bool,
    in_idle_period: bool,
    /// `network_2_quiet_` / `network_0_quiet_`: the quiet base, which the tasks since shift later.
    almost_idle_base: Option<Instant>,
    idle_base: Option<Instant>,
    /// `network_2_quiet_start_time_` / `network_0_quiet_start_time_`: when the quiet period began, as reported.
    almost_idle_since: Option<Instant>,
    idle_since: Option<Instant>,
    /// When the `kNetworkQuietWatchdog` timer fires next; `None` while it is not running.
    watchdog: Option<Instant>,
    marks: NetworkIdle,
}

impl Detector {
    /// `Start()`, run by `DomContentLoadedEventFired()`: both periods open, then `OnDidLoadResource()`
    /// with the requests in flight at that moment.
    fn start(at: Instant, active: usize) -> Self {
        let mut d = Detector {
            active,
            in_almost_idle_period: true,
            in_idle_period: true,
            almost_idle_base: None,
            idle_base: None,
            almost_idle_since: None,
            idle_since: None,
            watchdog: None,
            marks: NetworkIdle::default(),
        };
        d.did_load_resource(at);
        d
    }

    /// `OnWillSendRequest()`: one more in flight. Above the period's maximum, its quiet time restarts.
    fn will_send(&mut self) {
        self.active += 1;
        if self.in_almost_idle_period && self.active > ALMOST_IDLE_MAX {
            self.almost_idle_base = None;
        }
        if self.in_idle_period && self.active > 0 {
            self.idle_base = None;
        }
    }

    /// A request ended: one fewer in flight, then `OnDidLoadResource()`.
    fn finished(&mut self, at: Instant) {
        self.active = self.active.saturating_sub(1);
        self.did_load_resource(at);
    }

    /// `OnDidLoadResource()`. Reaching exactly the maximum restarts the almost-idle quiet time; going
    /// below it only starts one not already running. Reaching zero restarts the idle quiet time. Then the
    /// watchdog is armed, unless it is running.
    fn did_load_resource(&mut self, at: Instant) {
        if !self.in_almost_idle_period && !self.in_idle_period {
            return; // `HasCompleted()`
        }
        if self.active > ALMOST_IDLE_MAX {
            return;
        }
        if self.active == ALMOST_IDLE_MAX && self.in_almost_idle_period {
            self.almost_idle_base = Some(at);
            self.almost_idle_since = Some(at);
        } else if self.active < ALMOST_IDLE_MAX && self.in_almost_idle_period && self.almost_idle_base.is_none() {
            self.almost_idle_base = Some(at);
            self.almost_idle_since = Some(at);
        }
        if self.active == 0 && self.in_idle_period {
            self.idle_base = Some(at);
            self.idle_since = Some(at);
        }
        if self.watchdog.is_none() {
            self.watchdog = Some(at + WATCHDOG);
        }
    }

    /// `WillProcessTask()`: a task starts at `at`. A quiet time longer than the window -- measured from the
    /// shifted base -- ends its period, and the point is reported with the moment the quiet period began.
    fn task_at(&mut self, at: Instant) {
        if self.in_almost_idle_period && self.almost_idle_base.is_some_and(|b| at.saturating_duration_since(b) > QUIET_WINDOW) {
            self.marks.almost_idle = self.almost_idle_since;
            self.in_almost_idle_period = false;
            self.almost_idle_base = None;
        }
        if self.in_idle_period && self.idle_base.is_some_and(|b| at.saturating_duration_since(b) > QUIET_WINDOW) {
            self.marks.idle = self.idle_since;
            self.in_idle_period = false;
            self.idle_base = None;
        }
        if !self.in_almost_idle_period && !self.in_idle_period {
            self.watchdog = None; // `Stop()`
        }
    }

    /// `DidProcessTask()`: a task that ran `busy` long has ended; a base that is set moves later by all of it.
    fn task_done(&mut self, busy: Duration) {
        if self.in_almost_idle_period {
            if let Some(b) = self.almost_idle_base.as_mut() {
                *b += busy;
            }
        }
        if self.in_idle_period {
            if let Some(b) = self.idle_base.as_mut() {
                *b += busy;
            }
        }
    }

    /// Fire every watchdog tick due by `until`: each is a task of its own (`NetworkQuietTimerFired`), which
    /// re-arms the timer while a quiet period holds.
    fn watchdog_until(&mut self, until: Instant) {
        while let Some(tick) = self.watchdog.filter(|t| *t <= until) {
            self.watchdog = None;
            self.task_at(tick);
            let holds = (self.in_idle_period && self.idle_base.is_some()) || (self.in_almost_idle_period && self.almost_idle_base.is_some());
            if holds {
                self.watchdog = Some(tick + WATCHDOG);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn a_page_with_nothing_in_flight_is_idle_from_its_domcontentloaded() {
        let dcl = Instant::now();
        let spans = [(ms(dcl, 0) - Duration::from_millis(100), Some(ms(dcl, 0) - Duration::from_millis(50)))];
        let task_at_500 = [(ms(dcl, 500), ms(dcl, 500))];
        assert_eq!(quiet_points(dcl, &spans, &task_at_500, ms(dcl, 600)), NetworkIdle::default(), "the window is strict");
        let task_at_501 = [(ms(dcl, 501), ms(dcl, 502))];
        let m = quiet_points(dcl, &spans, &task_at_501, ms(dcl, 600));
        assert_eq!((m.idle, m.almost_idle), (Some(dcl), Some(dcl)), "reached at the first task past the window");
    }

    #[test]
    fn a_page_that_runs_nothing_reaches_its_points_on_the_watchdog() {
        let dcl = Instant::now();
        assert_eq!(quiet_points(dcl, &[], &[], ms(dcl, 1999)), NetworkIdle::default(), "no task has looked yet");
        let m = quiet_points(dcl, &[], &[], ms(dcl, 2000));
        assert_eq!((m.idle, m.almost_idle), (Some(dcl), Some(dcl)), "the 2 s watchdog is a task");
    }

    #[test]
    fn three_in_flight_reach_almost_idle_when_the_third_last_ends() {
        let dcl = Instant::now();
        let early = dcl - Duration::from_millis(10);
        let spans = [(early, Some(ms(dcl, 1000))), (early, Some(ms(dcl, 1200))), (early, Some(ms(dcl, 1400)))];
        // the watchdog was armed when the first of them ended (at DOMContentLoaded three were in flight)
        let m = quiet_points(dcl, &spans, &[], ms(dcl, 2999));
        assert_eq!(m, NetworkIdle::default(), "nothing has looked since 1400 ms");
        let m = quiet_points(dcl, &spans, &[], ms(dcl, 3000));
        assert_eq!(m.almost_idle, Some(ms(dcl, 1000)), "two left in flight from 1000 ms");
        assert_eq!(m.idle, Some(ms(dcl, 1400)));
    }

    #[test]
    fn a_request_inside_the_window_restarts_the_quiet_time() {
        let dcl = Instant::now();
        let spans = [(ms(dcl, 300), Some(ms(dcl, 600)))];
        let m = quiet_points(dcl, &spans, &[], ms(dcl, 1999));
        assert_eq!((m.almost_idle, m.idle), (Some(dcl), None), "one in flight never left almost-idle; the request's end looked at 600 ms");
        assert_eq!(quiet_points(dcl, &spans, &[], ms(dcl, 2000)).idle, Some(ms(dcl, 600)));
    }

    #[test]
    fn a_point_once_reached_stays_reached() {
        let dcl = Instant::now();
        let spans = [(ms(dcl, 700), Some(ms(dcl, 800)))];
        // The request at 700 ms runs in a task that starts after 500 quiet ms: idle was reached first.
        assert_eq!(quiet_points(dcl, &spans, &[], ms(dcl, 900)).idle, Some(dcl));
    }

    #[test]
    fn a_request_that_never_ends_keeps_the_page_from_idle_but_not_from_almost_idle() {
        let dcl = Instant::now();
        let spans = [(dcl - Duration::from_millis(5), None)];
        let m = quiet_points(dcl, &spans, &[], ms(dcl, 10_000));
        assert_eq!(m.idle, None);
        assert_eq!(m.almost_idle, Some(dcl));
    }

    #[test]
    fn a_request_served_in_no_time_still_counts() {
        let dcl = Instant::now();
        let spans = [(ms(dcl, 400), Some(ms(dcl, 400)))];
        assert_eq!(quiet_points(dcl, &spans, &[], ms(dcl, 2000)).idle, Some(ms(dcl, 400)));
    }

    // MAIN's review of A2b, from chromium-150 `idleness_detector.cc`: the base is set at DOMContentLoaded inside a
    // task running -50..+20 ms, so that task's WHOLE 70 ms counts; a 400 ms task then runs 100..500. 900 - 470 =
    // 430 quiet ms at 900; 971 - 470 = 501 at 971.
    #[test]
    fn a_busy_thread_is_not_quiet() {
        let dcl = Instant::now();
        let tasks = [(dcl - Duration::from_millis(50), ms(dcl, 20)), (ms(dcl, 100), ms(dcl, 500))];
        let at = |n: u64| {
            let mut t = tasks.to_vec();
            t.push((ms(dcl, n), ms(dcl, n + 1)));
            quiet_points(dcl, &[], &t, ms(dcl, n + 1))
        };
        assert_eq!(at(900), NetworkIdle::default(), "430 quiet ms");
        assert_eq!(at(971).idle, Some(dcl), "501 quiet ms; reported from where the quiet began");
    }

    #[test]
    fn the_watchdog_rearms_while_a_period_holds() {
        let dcl = Instant::now();
        let spans = [(ms(dcl, 100), Some(ms(dcl, 1900)))];
        // armed at DOMContentLoaded: 2000 ms looks (100 quiet ms since 1900) and re-arms for 4000 ms
        assert_eq!(quiet_points(dcl, &spans, &[], ms(dcl, 3999)).idle, None);
        assert_eq!(quiet_points(dcl, &spans, &[], ms(dcl, 4000)).idle, Some(ms(dcl, 1900)));
    }
}
