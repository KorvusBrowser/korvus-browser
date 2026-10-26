//! `core/media/` — the media OBJECTS a page can hold: streams, tracks and the recorder over them.
//!
//! Korvus decodes and encodes nothing, and that does not make these interfaces unimplementable,
//! because almost all of them describe an object graph that exists BEFORE a byte moves: a canvas
//! capture track really is live, enabled and unmuted; a recorder really does have a state machine that
//! runs without an encoder behind it. What is genuinely missing is said out loud rather than faked —
//! see each file's header.

pub mod demux;
pub mod video_pipe;
pub mod media_device_info;
pub mod media_recorder;
pub mod media_session;
pub mod media_source;
pub mod mse;
pub mod media_stream;
pub mod video_frame;
