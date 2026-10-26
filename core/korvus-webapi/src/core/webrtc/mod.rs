//! `core/webrtc/` — the WebRTC interfaces, one file per group.
//!
//! The capture was taken on a machine with no camera, no microphone and no peer, so the state these
//! objects are in is genuinely the un-negotiated one: `signalingState: "stable"`, zero senders, a data
//! channel `connecting` with a null `id`. None of that is a workaround — it is the state the recording
//! records, and the tables here answer it.

pub mod config; // RTCConfiguration, converted and checked as Chrome does
pub mod gather; // ICE gathering over real sockets and STUN
pub mod ice_event; // RTCPeerConnectionIceEvent
pub mod peer_connection;
pub mod sdp; // An offer's m-sections, Chrome 150's
pub mod rtc_cert; // The self-signed DTLS certificate the send path presents (goal A)
pub mod rtc_values;
pub mod rtp;
