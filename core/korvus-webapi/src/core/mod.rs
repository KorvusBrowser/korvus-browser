//! `core/` — the always-on Web-API interfaces, one folder per domain (Chromium's `core/` tier).
//!
//! Blueprint layout: each domain is a folder of per-interface
//! files. `geometry` is the first slice and the template every later domain copies.

pub mod animation;
pub mod clipboard;
pub mod geometry;
pub mod encoding;
pub mod streams;
pub mod timing;
pub mod resize_observer;
pub mod intersection_observer;
pub mod fonts;
/// The document's permissions policy and the `FeaturePolicy` object that reports it.
pub mod feature_policy;
/// Trusted Types -- `window.trustedTypes` and the five interfaces behind it, plus the
/// one measured type map `getTypeMapping`, `getAttributeType` and `getPropertyType` all three read.
pub mod trusted_types;
pub mod fileapi;
pub mod xmlhttprequest;
pub mod dom;
pub mod navigator;
pub mod messaging;
pub mod css;
pub mod events;
pub mod editing;
pub mod html;
pub mod xml;
pub mod speech;
pub mod media;
pub mod webrtc;
/// WebXR without a device: inline sessions, XRRigidTransform, XRRay.
pub mod xr;
pub mod misc_ifaces;
/// The built-in AI APIs' `availability()` / `create()`.
pub mod ai;
/// `Worker` and `SharedWorker` -- the worker family as tables with native constructors,
/// their receive pumps included. Replaces `host.js`'s two `class X extends EventTarget` declarations.
pub mod workers;
