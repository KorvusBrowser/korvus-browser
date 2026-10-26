//! KORVUS PATCH: the phases of every connection this client OPENS, as Chromium's
//! connect jobs record them (`net::LoadTimingInfo::ConnectTiming`), and whether a response rode a connection
//! that was already open.
//!
//! Chromium 150, `net/socket/`:
//!  * `TransportConnectJob::DoResolveHost` / `DoResolveHostComplete` (transport_connect_job.cc:269-303): the
//!    host's lookup starts and ends around the resolver -- a literal IP or a cached answer still has both, a
//!    moment apart -- and `connect_start` is overwritten with `domain_lookup_end` ("connections that do not go
//!    through proxies").
//!  * `ConnectJob::LogConnectStart` (connect_job.cc:217): every other job's `connect_start` is when the job
//!    began. Behind a proxy that is the whole proxy connect -- its lookup, its TCP, the `CONNECT` tunnel -- and
//!    the lookup is NULL: "DNS and SSL times are both times for the host, not the proxy" (load_timing_info.h).
//!  * `SSLConnectJob::DoSSLConnect` (ssl_connect_job.cc:388): `ssl_start` when the handshake WITH THE
//!    DESTINATION begins -- never a TLS handshake with an HTTPS proxy.
//!  * `ConnectJob::StopTimerAndLogConnectCompletion` (connect_job.cc:228): `connect_end` when the job is done.
//!  * `StreamSocketHandle::GetLoadTimingInfo` (stream_socket_handle.cc:27): a reused socket reports none of
//!    them; an HTTP/1 socket is reused when it served a request before OR sat idle in the pool
//!    (http_basic_state.cc:86), and an HTTP/2 session is fresh only for its first stream (spdy_session.cc:1513).
//!
//! The recorder rides the connect future as a scoped thread-local (set only while [`Scoped`] polls it), so the
//! resolver, the TCP connector and the TLS connector mark it without a change to their service signatures.

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};

/// The phases of the connection a response came over, set in the extensions of every response it carries.
/// `None` is Chromium's null `TimeTicks`: a phase that did not happen (a lookup behind a proxy, a handshake on
/// a plain `http:` connection).
#[derive(Debug, Clone, Copy)]
pub struct ConnectTiming {
    /// When the host lookup began.
    pub domain_lookup_start: Option<Instant>,
    /// When the host lookup ended.
    pub domain_lookup_end: Option<Instant>,
    /// When the connect job began (direct: the lookup's end).
    pub connect_start: Option<Instant>,
    /// When the TLS handshake with the destination began.
    pub ssl_start: Option<Instant>,
    /// When the connection became usable.
    pub connect_end: Instant,
}

/// Whether a response rode a connection the pool already held (an idle HTTP/1 connection, or an HTTP/2 session
/// another request opened): Chromium's `LoadTimingInfo::socket_reused`, under which none of [`ConnectTiming`]
/// is reported.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionReused(pub bool);

#[derive(Default)]
struct Marks {
    job_start: Option<Instant>,
    proxied: bool,
    domain_lookup_start: Option<Instant>,
    domain_lookup_end: Option<Instant>,
    ssl_start: Option<Instant>,
}

/// One connection attempt's marks.
#[derive(Default)]
pub(crate) struct Recorder(Mutex<Marks>);

thread_local! {
    static CURRENT: RefCell<Option<Arc<Recorder>>> = const { RefCell::new(None) };
}

fn with(f: impl FnOnce(&mut Marks)) {
    CURRENT.with(|c| {
        if let Some(rec) = c.borrow().as_ref() {
            if let Ok(mut m) = rec.0.lock() {
                f(&mut m);
            }
        }
    });
}

/// The connection goes through a proxy: its lookups are the proxy's, not the host's.
pub(crate) fn proxied() {
    with(|m| m.proxied = true);
}

/// The host's lookup begins (`TransportConnectJob::DoResolveHost`).
pub(crate) fn lookup_start() {
    let now = Instant::now();
    with(|m| {
        if !m.proxied && m.domain_lookup_start.is_none() {
            m.domain_lookup_start = Some(now);
        }
    });
}

/// The host's lookup ended (`DoResolveHostComplete`).
pub(crate) fn lookup_end() {
    let now = Instant::now();
    with(|m| {
        if !m.proxied && m.domain_lookup_start.is_some() && m.domain_lookup_end.is_none() {
            m.domain_lookup_end = Some(now);
        }
    });
}

/// A TLS handshake begins: the destination's (`SSLConnectJob::DoSSLConnect`) when it runs over a proxy's
/// tunnel (`tunneled`) or over a direct connection, never the one an HTTPS proxy's own connection makes.
pub(crate) fn ssl_start(tunneled: bool) {
    let now = Instant::now();
    with(|m| {
        if (tunneled || !m.proxied) && m.ssl_start.is_none() {
            m.ssl_start = Some(now);
        }
    });
}

/// The connection is usable now: its phases, as the connect job reports them.
pub(crate) fn finish() -> ConnectTiming {
    let connect_end = Instant::now();
    let mut out = ConnectTiming {
        domain_lookup_start: None,
        domain_lookup_end: None,
        connect_start: None,
        ssl_start: None,
        connect_end,
    };
    with(|m| {
        out.domain_lookup_start = m.domain_lookup_start;
        out.domain_lookup_end = m.domain_lookup_end;
        // A direct connection's connect starts when its lookup ended; any other when its job began.
        out.connect_start = match (m.proxied, m.domain_lookup_end) {
            (false, Some(end)) => Some(end),
            _ => m.job_start,
        };
        out.ssl_start = m.ssl_start;
    });
    out
}

pin_project_lite::pin_project! {
    /// A connect future with its [`Recorder`] current while it is polled.
    pub(crate) struct Scoped<F> {
        rec: Arc<Recorder>,
        #[pin]
        fut: F,
    }
}

impl<F> Scoped<F> {
    /// The job begins now (`ConnectJob::LogConnectStart`).
    pub(crate) fn new(fut: F) -> Self {
        let rec = Recorder::default();
        if let Ok(mut m) = rec.0.lock() {
            m.job_start = Some(Instant::now());
        }
        Scoped { rec: Arc::new(rec), fut }
    }
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        /// Puts the previous recorder back even if the inner poll unwinds.
        struct Restore(Option<Arc<Recorder>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                let prev = self.0.take();
                CURRENT.with(|c| *c.borrow_mut() = prev);
            }
        }
        let this = self.project();
        let _restore = Restore(CURRENT.with(|c| c.replace(Some(this.rec.clone()))));
        this.fut.poll(cx)
    }
}
