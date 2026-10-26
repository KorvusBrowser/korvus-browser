//! Extensions specific to the HTTP/2 protocol.

use crate::hpack::BytesStr;

use bytes::Bytes;
use std::fmt;

/// KORVUS PATCH: a request's priority as Chromium's network stack gives it to HTTP/2 -- its SPDY
/// priority, 0 (highest) to 7, the urgency of its RFC 9218 `priority` field (`u=N`, 3 when it carries none). In a
/// request's extensions it replaces the connection's fixed `headers_stream_dependency` for that request's HEADERS
/// frame with Chromium's own (`net/spdy/http2_priority_dependencies.cc`, `spdy_session.cc` `CreateHeaders`): an
/// exclusive dependency on the newest open stream of the same or a higher priority, and the weight
/// `Spdy3PriorityToHttp2Weight` (`spdy_protocol.cc`) gives the priority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChromiumPriority(pub u8);

/// Represents the `:protocol` pseudo-header used by
/// the [Extended CONNECT Protocol].
///
/// [Extended CONNECT Protocol]: https://datatracker.ietf.org/doc/html/rfc8441#section-4
#[derive(Clone, Eq, PartialEq)]
pub struct Protocol {
    value: BytesStr,
}

impl Protocol {
    /// Converts a static string to a protocol name.
    pub const fn from_static(value: &'static str) -> Self {
        Self {
            value: BytesStr::from_static(value),
        }
    }

    /// Returns a str representation of the header.
    pub fn as_str(&self) -> &str {
        self.value.as_str()
    }

    pub(crate) fn try_from(bytes: Bytes) -> Result<Self, std::str::Utf8Error> {
        Ok(Self {
            value: BytesStr::try_from(bytes)?,
        })
    }
}

impl<'a> From<&'a str> for Protocol {
    fn from(value: &'a str) -> Self {
        Self {
            value: BytesStr::from(value),
        }
    }
}

impl AsRef<[u8]> for Protocol {
    fn as_ref(&self) -> &[u8] {
        self.value.as_ref()
    }
}

impl fmt::Debug for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.value.fmt(f)
    }
}
