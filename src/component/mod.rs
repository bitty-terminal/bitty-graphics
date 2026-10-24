//! Executable-component boundary for the graphics decoder.
//!
//! The crate still exposes the in-process decode and raster seam for Core's
//! linked use; this module adds the installable stdio form required by the
//! component split (graphics#17): a bounded v1 wire protocol and a server
//! that decodes one request at a time and returns one bounded RGBA8 bitmap.
//!
//! It carries no installer, download source, capability policy, placement
//! policy, aggregate budget, or packaging logic beyond the repository
//! scripts. Core remains the trust decision owner; this component is
//! mechanism only.
//!
//! ```text
//! Core host client  ──stdio frames──▶  bitty-graphics  ──▶  bounded decode
//!   (remaining work)                    (this module)        (kitty_decode)
//!                                          │
//!                                          └──▶ one bounded RGBA8 bitmap
//! ```
//!
//! # Boundaries fixed here
//!
//! - The wire is the only input. [`DecodeRequest`](protocol::Message::DecodeRequest)
//!   carries the transmit format, optional declared dimensions, a declared
//!   output budget and deadline, and opaque image bytes. It carries no path,
//!   argv, environment, file descriptor, URL, or command.
//! - The server validates every request against the advertised caps before
//!   allocation, decodes through the same bounded decoder the linked crate
//!   uses, and returns the exact normalized RGBA8 shape or a typed refusal.
//! - Every response is bounded by [`MAX_RESPONSE_FRAME_BYTES`](protocol::MAX_RESPONSE_FRAME_BYTES);
//!   request frames, payloads, image dimensions, decoded bytes, deadline, and
//!   request rate are capped.
//! - Safe mode and a withheld capability are host declarations; the component
//!   refuses decode with a typed `denied` error while still completing the
//!   handshake and shutdown. It never self-grants.

pub mod protocol;
pub mod server;

/// Component version reported by `HelloAck`, `--version`, and the descriptor.
pub const COMPONENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub use protocol::{
    CAPABILITY_MASK, CAPABILITY_PNG, CAPABILITY_RGB, CAPABILITY_RGBA, COMPONENT_NAME,
    CONNECTION_ID, ComponentCaps, ErrorKind, FrameReader, HELLO_CAPABILITY_GRANTED,
    HELLO_MODE_MASK, HELLO_SAFE_MODE, MAX_CONCURRENT_DECODES, MAX_DEADLINE_MS, MAX_DECODED_BYTES,
    MAX_IMAGE_DIMENSION, MAX_IMAGE_PIXELS, MAX_PAYLOAD_BYTES, MAX_REQUEST_FRAME_BYTES,
    MAX_RESPONSE_FRAME_BYTES, Message, PIXEL_FORMAT_RGBA8, PROTOCOL_VERSION, WireError,
    decode_message, encode_frame, encode_message, negotiate, write_frame,
};
pub use server::{
    RATE_MAX_VIOLATIONS, SERVER_EXIT_INTERNAL, SERVER_EXIT_OK, SERVER_EXIT_PROTOCOL,
    SERVER_EXIT_REFUSED, Session, SessionOutcome, serve, serve_stdio,
};
