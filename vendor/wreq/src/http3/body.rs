//! KORVUS PATCH (H3): the body of a response the client layer returns -- an HTTP/1 or HTTP/2 connection's
//! (`wreq_proto::body::Incoming`) or an HTTP/3 stream's.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use tower::BoxError;
use wreq_proto::body::Incoming;

use super::driver::BodyRx;

/// A response body from any of the client's protocols.
pub struct ResBody(Inner);

enum Inner {
    Proto(Incoming),
    // In a mutex only to be `Sync` as `Response` requires; it is only ever reached through `&mut`.
    H3 { rx: std::sync::Mutex<BodyRx>, done: bool },
}

impl ResBody {
    pub(crate) fn proto(body: Incoming) -> ResBody {
        ResBody(Inner::Proto(body))
    }

    pub(crate) fn h3(rx: BodyRx) -> ResBody {
        ResBody(Inner::H3 { rx: std::sync::Mutex::new(rx), done: false })
    }
}

impl std::fmt::Debug for ResBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Inner::Proto(b) => b.fmt(f),
            Inner::H3 { .. } => f.write_str("ResBody(h3)"),
        }
    }
}

impl Body for ResBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        match &mut self.0 {
            Inner::Proto(b) => Pin::new(b).poll_frame(cx).map_err(Into::into),
            Inner::H3 { rx, done } => {
                if *done {
                    return Poll::Ready(None);
                }
                let rx = match rx.get_mut() {
                    Ok(rx) => rx,
                    Err(p) => p.into_inner(),
                };
                match rx.poll_recv(cx) {
                    Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
                    Poll::Ready(Some(Err(e))) => {
                        *done = true;
                        Poll::Ready(Some(Err(e.into())))
                    }
                    Poll::Ready(None) => {
                        *done = true;
                        Poll::Ready(None)
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.0 {
            Inner::Proto(b) => b.is_end_stream(),
            Inner::H3 { done, .. } => *done,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.0 {
            Inner::Proto(b) => b.size_hint(),
            Inner::H3 { .. } => SizeHint::default(),
        }
    }
}
