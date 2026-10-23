//! Graphics component wire protocol v1 (Core host <-> `bitty-graphics`).
//!
//! The component is a stdio coprocess: the host writes requests to its stdin
//! and reads framed messages from its stdout. stderr stays diagnostic and is
//! never protocol. This module is the authoritative schema and codec; it is
//! hand-written (`std` only), forbids panics on hostile input, and fails
//! closed on every bound.
//!
//! # Framing
//!
//! ```text
//! frame   = length:u32be payload[length]
//! payload = tag:u8 fields...
//! ```
//!
//! `length` is 1..=[`MAX_REQUEST_FRAME_BYTES`] host-to-component and
//! 1..=[`MAX_RESPONSE_FRAME_BYTES`] component-to-host. A larger or zero
//! length is rejected from the four-byte header alone; no payload byte is
//! read or allocated. The two ceilings differ because a bounded request
//! carries at most one `IMG-1` payload while a successful decode response
//! carries the decoded `IMG-3` bitmap exactly once.
//!
//! # Field encoding
//!
//! - Integers are big-endian fixed width (`u8`, `u16`, `u32`, `u64`).
//! - `bool` is one byte, `0x00` or `0x01`; any other value is rejected.
//! - `str` is `len:u32be` + UTF-8 bytes, every length bounded per field.
//! - `bytes` is `len:u32be` + raw bytes, bounded per field.
//! - An optional value is `0x00` (absent) or `0x01` followed by the value.
//!
//! # Messages
//!
//! | Tag    | Message         | Fields (in order)                                                                                          | Direction  |
//! |--------|-----------------|------------------------------------------------------------------------------------------------------------|------------|
//! | `0x01` | `Hello`         | `min:u16 max:u16 component:str version:str mode:u8`                                                        | host->comp |
//! | `0x02` | `HelloAck`      | `protocol:u16 component:str version:str capabilities:u8 caps...`                                           | comp->host |
//! | `0x10` | `DecodeRequest` | `request:u64 format:u8 width:opt<u32> height:opt<u32> budget:u32 deadline_ms:u32 payload:bytes`            | host->comp |
//! | `0x11` | `DecodeResponse`| `request:u64 format:u8 width:u32 height:u32 stride:u32 rgba:bytes`                                          | comp->host |
//! | `0x2F` | `Error`         | `request:u64 kind:u8 message:str`                                                                          | comp->host |
//! | `0x3E` | `ShutdownAck`   | (none)                                                                                                     | comp->host |
//! | `0x3F` | `Shutdown`      | (none)                                                                                                     | host->comp |
//!
//! `HelloAck.caps` is the explicit capability block, in this fixed order:
//! `max_request_frame_bytes:u32 max_response_frame_bytes:u32
//! max_payload_bytes:u32 max_image_dimension:u32 max_image_pixels:u64
//! max_decoded_bytes:u32 max_concurrent_decodes:u32 rate_capacity:u32
//! rate_per_second:u32`. [`ComponentCaps::v1`] is the only value this
//! component emits; the decoder accepts any well-formed block so a host can
//! parse a future component without guessing.
//!
//! `request` ids are non-zero and host-assigned. [`CONNECTION_ID`] (`0`)
//! marks a connection-level [`Message::Error`] (handshake, framing, or
//! protocol failure). A `DecodeResponse` always carries normalized RGBA8
//! pixels ([`PIXEL_FORMAT_RGBA8`]), row-major, with `stride == width * 4` and
//! `rgba.len() == width * height * 4`; any other shape is refused on both
//! encode and decode.
//!
//! # Authority and capability boundary
//!
//! This protocol carries **no file operations, no descriptor passing, no
//! environment, no argv, no URL, no socket, and no ambient command
//! language**. The only strings on the wire are the component name and
//! version in the handshake and a bounded static diagnostic in `Error`. Image
//! bytes travel as opaque fields; the component never interprets them as a
//! path, command, or identifier. The host owns installation, digest
//! verification, capability grants, consent, safe-mode denial, placement,
//! the aggregate budget, and renderer upload; the component decodes one
//! bounded payload and returns one bounded bitmap.
//!
//! `Hello.mode` bit `0x01` declares safe mode and bit `0x02` declares that the
//! host granted the graphics decode capability. Safe mode, a missing grant,
//! or both make the session **deprived**: the handshake completes with zero
//! capabilities, decode requests fail closed with `denied`, and shutdown still
//! works.
//!
//! # Bounds
//!
//! - [`MAX_REQUEST_FRAME_BYTES`] (4 MiB + 1 KiB) per request frame and
//!   [`MAX_RESPONSE_FRAME_BYTES`] (64 MiB + 1 KiB) per response frame,
//!   checked before allocation.
//! - [`MAX_PAYLOAD_BYTES`] (4 MiB, `IMG-1`) of image bytes per request.
//! - [`MAX_IMAGE_DIMENSION`] (8192 px/side), [`MAX_IMAGE_PIXELS`]
//!   (4096 x 4096) and [`MAX_DECODED_BYTES`] (64 MiB, `IMG-3`) enforced by
//!   the decode path before allocation.
//! - [`MAX_CONCURRENT_DECODES`] (`1`): the server decodes one request at a
//!   time; a second request is pipelined behind the first, never decoded
//!   concurrently.
//! - [`MAX_DEADLINE_MS`] (30 s) per request deadline declaration.
//! - A request rate gate in [`server`](crate::component::server).
//!
//! All bounds are checked on both encode and decode, so a peer can never emit
//! a frame the other side would refuse.

use std::fmt;
use std::io::{self, Read, Write};

/// Wire protocol version implemented by this crate.
pub const PROTOCOL_VERSION: u16 = 1;

/// Component name on the wire and in the descriptor.
pub const COMPONENT_NAME: &str = "graphics";

/// Size of the big-endian `u32` length prefix in front of every payload.
pub const FRAME_HEADER_BYTES: usize = 4;

/// Maximum image payload bytes per request (`IMG-1`, 4 MiB).
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Maximum host-to-component frame payload (image payload plus fixed fields).
pub const MAX_REQUEST_FRAME_BYTES: usize = MAX_PAYLOAD_BYTES + 1024;

/// Maximum decoded RGBA8 bytes per response (`IMG-3`, 64 MiB).
pub const MAX_DECODED_BYTES: u32 = 64 * 1024 * 1024;

/// Maximum component-to-host frame payload (bitmap plus fixed fields).
pub const MAX_RESPONSE_FRAME_BYTES: usize = MAX_DECODED_BYTES as usize + 1024;

/// Maximum decoded image width or height in pixels.
pub const MAX_IMAGE_DIMENSION: u32 = 8192;

/// Maximum decoded image pixels (4096 x 4096 area).
pub const MAX_IMAGE_PIXELS: u64 = 4096 * 4096;

/// Decodes one request at a time.
pub const MAX_CONCURRENT_DECODES: u32 = 1;

/// Maximum declared request deadline (30 s).
pub const MAX_DEADLINE_MS: u32 = 30_000;

/// Maximum component name bytes (`[a-z][a-z0-9-]{0,31}`).
pub const MAX_COMPONENT_NAME_BYTES: usize = 32;

/// Maximum version string bytes in `Hello` / `HelloAck`.
pub const MAX_VERSION_BYTES: usize = 64;

/// Maximum `Error.message` bytes.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 256;

/// Requests allowed in one burst before the rate gate refuses.
pub const RATE_CAPACITY: u32 = 120;

/// Sustained request rate per second.
pub const RATE_PER_SECOND: u32 = 60;

/// Normalized response pixel format: straight-alpha RGBA8, row-major.
pub const PIXEL_FORMAT_RGBA8: u8 = 1;

// Keep the framing and decode ceilings tied to the accepted IMG caps without
// a lossy cast.
const _: () = assert!(MAX_REQUEST_FRAME_BYTES > MAX_PAYLOAD_BYTES);
const _: () = assert!(MAX_RESPONSE_FRAME_BYTES > MAX_DECODED_BYTES as usize);
const _: () = assert!(MAX_IMAGE_PIXELS * 4 == MAX_DECODED_BYTES as u64);
const _: () = assert!(MAX_IMAGE_DIMENSION as u64 * MAX_IMAGE_DIMENSION as u64 > MAX_IMAGE_PIXELS);
const _: () = assert!(MAX_CONCURRENT_DECODES == 1);

// ── tags ────────────────────────────────────────────────────────────────────

/// `Hello` tag.
pub const TAG_HELLO: u8 = 0x01;
/// `HelloAck` tag.
pub const TAG_HELLO_ACK: u8 = 0x02;
/// `DecodeRequest` tag.
pub const TAG_DECODE_REQUEST: u8 = 0x10;
/// `DecodeResponse` tag.
pub const TAG_DECODE_RESPONSE: u8 = 0x11;
/// `Error` tag.
pub const TAG_ERROR: u8 = 0x2F;
/// `ShutdownAck` tag.
pub const TAG_SHUTDOWN_ACK: u8 = 0x3E;
/// `Shutdown` tag.
pub const TAG_SHUTDOWN: u8 = 0x3F;

/// Connection-level `Error` frame request id.
pub const CONNECTION_ID: u64 = 0;

// ── handshake vocabulary ────────────────────────────────────────────────────

/// `Hello.mode` bit: the host is in safe mode.
pub const HELLO_SAFE_MODE: u8 = 0x01;

/// `Hello.mode` bit: the host granted the graphics decode capability.
pub const HELLO_CAPABILITY_GRANTED: u8 = 0x02;

/// Defined `Hello.mode` bits; every other bit is refused.
pub const HELLO_MODE_MASK: u8 = HELLO_SAFE_MODE | HELLO_CAPABILITY_GRANTED;

/// `HelloAck.capabilities` bit: PNG decode (`f=100`).
pub const CAPABILITY_PNG: u8 = 0x01;

/// `HelloAck.capabilities` bit: raw 24-bit RGB decode (`f=24`).
pub const CAPABILITY_RGB: u8 = 0x02;

/// `HelloAck.capabilities` bit: raw 32-bit RGBA decode (`f=32`).
pub const CAPABILITY_RGBA: u8 = 0x04;

/// Every capability bit defined in v1.
pub const CAPABILITY_MASK: u8 = CAPABILITY_PNG | CAPABILITY_RGB | CAPABILITY_RGBA;

/// Explicit bounds advertised by [`Message::HelloAck`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentCaps {
    /// Largest accepted host-to-component frame payload.
    pub max_request_frame_bytes: u32,
    /// Largest emitted component-to-host frame payload.
    pub max_response_frame_bytes: u32,
    /// Largest accepted image payload in one request.
    pub max_payload_bytes: u32,
    /// Largest decoded image side in pixels.
    pub max_image_dimension: u32,
    /// Largest decoded image area in pixels.
    pub max_image_pixels: u64,
    /// Largest decoded RGBA8 bitmap in bytes.
    pub max_decoded_bytes: u32,
    /// Decodes the server runs concurrently.
    pub max_concurrent_decodes: u32,
    /// Burst capacity of the request rate gate.
    pub rate_capacity: u32,
    /// Sustained requests per second.
    pub rate_per_second: u32,
}

impl ComponentCaps {
    /// The v1 capability block this component emits.
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            max_request_frame_bytes: MAX_REQUEST_FRAME_BYTES as u32,
            max_response_frame_bytes: MAX_RESPONSE_FRAME_BYTES as u32,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u32,
            max_image_dimension: MAX_IMAGE_DIMENSION,
            max_image_pixels: MAX_IMAGE_PIXELS,
            max_decoded_bytes: MAX_DECODED_BYTES,
            max_concurrent_decodes: MAX_CONCURRENT_DECODES,
            rate_capacity: RATE_CAPACITY,
            rate_per_second: RATE_PER_SECOND,
        }
    }
}

/// Negotiates the highest protocol version both peers support.
#[must_use]
pub const fn negotiate(min: u16, max: u16) -> Option<u16> {
    if min <= PROTOCOL_VERSION && PROTOCOL_VERSION <= max {
        Some(PROTOCOL_VERSION)
    } else {
        None
    }
}

// ── errors ──────────────────────────────────────────────────────────────────

/// Typed error category carried by [`Message::Error`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The host withheld the capability or declared safe mode (code 1).
    Denied,
    /// A request was malformed or violated a field bound (code 2).
    InvalidRequest,
    /// A declared or decoded size exceeds its cap (code 3).
    Budget,
    /// The transmit format is not supported in v1 (code 4).
    Unsupported,
    /// The peer violated the framing or message protocol (code 6).
    Protocol,
    /// The declared request deadline elapsed; no bitmap is returned (code 8).
    Timeout,
    /// Internal component failure (code 9).
    Internal,
    /// The request rate gate rejected the exchange (code 10).
    RateLimited,
    /// The compressed image stream is corrupt or truncated (code 12).
    MalformedImage,
}

impl ErrorKind {
    /// Wire code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Denied => 1,
            Self::InvalidRequest => 2,
            Self::Budget => 3,
            Self::Unsupported => 4,
            Self::Protocol => 6,
            Self::Timeout => 8,
            Self::Internal => 9,
            Self::RateLimited => 10,
            Self::MalformedImage => 12,
        }
    }

    /// Kind for a wire code, or `None` for an unknown code.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Denied),
            2 => Some(Self::InvalidRequest),
            3 => Some(Self::Budget),
            4 => Some(Self::Unsupported),
            6 => Some(Self::Protocol),
            8 => Some(Self::Timeout),
            9 => Some(Self::Internal),
            10 => Some(Self::RateLimited),
            12 => Some(Self::MalformedImage),
            _ => None,
        }
    }

    /// Stable `snake_case` name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::InvalidRequest => "invalid_request",
            Self::Budget => "budget",
            Self::Unsupported => "unsupported",
            Self::Protocol => "protocol",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
            Self::RateLimited => "rate_limited",
            Self::MalformedImage => "malformed_image",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Wire codec failure (owned; no `io::Error` escapes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// The underlying stream failed.
    Io(io::ErrorKind),
    /// A frame exceeds the limit for its direction (or an encoder field
    /// exceeds its bound).
    TooLarge {
        /// Observed length.
        len: usize,
        /// Limit that refused it.
        limit: usize,
    },
    /// A zero-length frame.
    EmptyFrame,
    /// A frame ended before a field was complete.
    Truncated,
    /// A frame carried bytes after the last field.
    TrailingBytes,
    /// The tag is not assigned in protocol v1.
    UnknownTag(u8),
    /// A string field is not UTF-8.
    InvalidUtf8,
    /// A field violates its grammar or bound.
    InvalidField(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "stream i/o error: {kind}"),
            Self::TooLarge { len, limit } => {
                write!(f, "frame is {len} bytes (limit {limit})")
            }
            Self::EmptyFrame => f.write_str("empty frame"),
            Self::Truncated => f.write_str("truncated frame"),
            Self::TrailingBytes => f.write_str("trailing bytes after the last field"),
            Self::UnknownTag(tag) => write!(f, "unknown tag 0x{tag:02x}"),
            Self::InvalidUtf8 => f.write_str("string field is not UTF-8"),
            Self::InvalidField(reason) => write!(f, "invalid field: {reason}"),
        }
    }
}

impl std::error::Error for WireError {}

// ── messages ────────────────────────────────────────────────────────────────

/// One protocol v1 message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Handshake offer; must be the first frame (host -> component).
    Hello {
        /// Lowest protocol version the host supports.
        min: u16,
        /// Highest protocol version the host supports.
        max: u16,
        /// Component name the host expects.
        component: String,
        /// Host version string.
        version: String,
        /// Safe-mode/capability declaration bits.
        mode: u8,
    },
    /// Handshake answer (component -> host).
    HelloAck {
        /// Negotiated protocol version.
        protocol: u16,
        /// Component name.
        component: String,
        /// Component version.
        version: String,
        /// Capability bits available for this session; zero when deprived.
        capabilities: u8,
        /// Explicit bounds for this session.
        caps: ComponentCaps,
    },
    /// One bounded decode request (host -> component).
    DecodeRequest {
        /// Non-zero request id.
        request: u64,
        /// Wire transmit format (`f=` value).
        format: u8,
        /// Declared width for raw formats; ignored for PNG.
        width: Option<u32>,
        /// Declared height for raw formats; ignored for PNG.
        height: Option<u32>,
        /// Caller-declared output byte budget.
        budget_bytes: u32,
        /// Caller-declared deadline in milliseconds.
        deadline_ms: u32,
        /// Opaque compressed or raw image bytes.
        payload: Vec<u8>,
    },
    /// One decoded RGBA8 bitmap (component -> host).
    DecodeResponse {
        /// Request id from `DecodeRequest`.
        request: u64,
        /// Normalized pixel format ([`PIXEL_FORMAT_RGBA8`]).
        format: u8,
        /// Decoded pixel width.
        width: u32,
        /// Decoded pixel height.
        height: u32,
        /// Row stride in bytes (`width * 4`).
        stride: u32,
        /// Straight-alpha RGBA8 pixels, row-major.
        rgba: Vec<u8>,
    },
    /// Typed error (component -> host).
    Error {
        /// Request id, or [`CONNECTION_ID`] for a connection-level failure.
        request: u64,
        /// Error category.
        kind: ErrorKind,
        /// Bounded static diagnostic; never echoes payload bytes.
        message: String,
    },
    /// Stop serving and exit (host -> component).
    Shutdown,
    /// Shutdown completed; the component exits after this frame
    /// (component -> host).
    ShutdownAck,
}

impl Message {
    /// Wire tag of this message.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Hello { .. } => TAG_HELLO,
            Self::HelloAck { .. } => TAG_HELLO_ACK,
            Self::DecodeRequest { .. } => TAG_DECODE_REQUEST,
            Self::DecodeResponse { .. } => TAG_DECODE_RESPONSE,
            Self::Error { .. } => TAG_ERROR,
            Self::Shutdown => TAG_SHUTDOWN,
            Self::ShutdownAck => TAG_SHUTDOWN_ACK,
        }
    }

    /// Request id this message carries, when it carries one.
    #[must_use]
    pub const fn request_id(&self) -> Option<u64> {
        match self {
            Self::DecodeRequest { request, .. }
            | Self::DecodeResponse { request, .. }
            | Self::Error { request, .. } => Some(*request),
            Self::Hello { .. } | Self::HelloAck { .. } | Self::Shutdown | Self::ShutdownAck => None,
        }
    }
}

// ── framing ─────────────────────────────────────────────────────────────────

/// Reads length-prefixed frames from a stream.
pub struct FrameReader<R> {
    reader: R,
    max_frame_bytes: usize,
}

impl<R: Read> FrameReader<R> {
    /// Wraps `reader` with the request-frame limit.
    pub const fn new(reader: R) -> Self {
        Self {
            reader,
            max_frame_bytes: MAX_REQUEST_FRAME_BYTES,
        }
    }

    /// Wraps `reader` with an explicit frame limit (host-side response reads).
    pub const fn with_limit(reader: R, max_frame_bytes: usize) -> Self {
        Self {
            reader,
            max_frame_bytes,
        }
    }

    /// Reads one frame payload, or `None` at a clean EOF between frames.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::Io`] for a stream failure,
    /// [`WireError::Truncated`] for EOF inside a header or payload,
    /// [`WireError::EmptyFrame`] for a zero length, and
    /// [`WireError::TooLarge`] for a length above the configured limit
    /// (detected before the payload is read or allocated).
    pub fn read_frame(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        let Some(len) = read_length(&mut self.reader)? else {
            return Ok(None);
        };
        if len == 0 {
            return Err(WireError::EmptyFrame);
        }
        if len > self.max_frame_bytes {
            return Err(WireError::TooLarge {
                len,
                limit: self.max_frame_bytes,
            });
        }
        let mut payload = vec![0_u8; len];
        self.reader
            .read_exact(&mut payload)
            .map_err(|error| match error.kind() {
                io::ErrorKind::UnexpectedEof => WireError::Truncated,
                kind => WireError::Io(kind),
            })?;
        Ok(Some(payload))
    }

    /// Reads and decodes one message, or `None` at a clean EOF.
    ///
    /// # Errors
    ///
    /// Returns the framing failures of [`Self::read_frame`] and the decode
    /// failures of [`decode_message`].
    pub fn read_message(&mut self) -> Result<Option<Message>, WireError> {
        match self.read_frame()? {
            Some(payload) => decode_message(&payload).map(Some),
            None => Ok(None),
        }
    }
}

fn read_length<R: Read>(reader: &mut R) -> Result<Option<usize>, WireError> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    let mut filled = 0;
    while filled < FRAME_HEADER_BYTES {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(WireError::Truncated),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(WireError::Io(error.kind())),
        }
    }
    Ok(Some(
        usize::try_from(u32::from_be_bytes(header)).unwrap_or(usize::MAX),
    ))
}

/// Frame-payload limit for a message direction.
#[must_use]
pub const fn frame_limit(message: &Message) -> usize {
    match message {
        Message::DecodeResponse { .. } => MAX_RESPONSE_FRAME_BYTES,
        _ => MAX_REQUEST_FRAME_BYTES,
    }
}

/// Encodes a message into one length-prefixed frame.
///
/// # Errors
///
/// Returns [`WireError::TooLarge`] when the payload would exceed the limit
/// for this message's direction, or any field bound failure from
/// [`encode_message`].
pub fn encode_frame(message: &Message) -> Result<Vec<u8>, WireError> {
    let payload = encode_message(message)?;
    let limit = frame_limit(message);
    if payload.is_empty() || payload.len() > limit {
        return Err(WireError::TooLarge {
            len: payload.len(),
            limit,
        });
    }
    let length = u32::try_from(payload.len()).map_err(|_| WireError::TooLarge {
        len: payload.len(),
        limit,
    })?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Encodes one frame and writes it to `writer`.
///
/// A `DecodeResponse` is streamed field by field so the bitmap is never
/// copied into a second buffer; every other message encodes first.
///
/// # Errors
///
/// Returns [`WireError::TooLarge`] for an over-bound message,
/// [`WireError::InvalidField`] for an inconsistent response shape, and
/// [`WireError::Io`] for a stream failure.
pub fn write_frame<W: Write>(writer: &mut W, message: &Message) -> Result<(), WireError> {
    if let Message::DecodeResponse {
        request,
        format,
        width,
        height,
        stride,
        rgba,
    } = message
    {
        return write_decode_response(writer, *request, *format, *width, *height, *stride, rgba);
    }
    let frame = encode_frame(message)?;
    writer
        .write_all(&frame)
        .map_err(|error| WireError::Io(error.kind()))
}

fn write_decode_response<W: Write>(
    writer: &mut W,
    request: u64,
    format: u8,
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
) -> Result<(), WireError> {
    validate_response(format, width, height, stride, rgba.len())?;
    // tag + request + format + width + height + stride + byte length.
    let payload_len = 1_usize
        .checked_add(8)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(rgba.len()))
        .ok_or(WireError::TooLarge {
            len: usize::MAX,
            limit: MAX_RESPONSE_FRAME_BYTES,
        })?;
    if payload_len > MAX_RESPONSE_FRAME_BYTES {
        return Err(WireError::TooLarge {
            len: payload_len,
            limit: MAX_RESPONSE_FRAME_BYTES,
        });
    }
    let length = u32::try_from(payload_len).map_err(|_| WireError::TooLarge {
        len: payload_len,
        limit: MAX_RESPONSE_FRAME_BYTES,
    })?;
    let byte_len = u32::try_from(rgba.len()).map_err(|_| WireError::TooLarge {
        len: rgba.len(),
        limit: MAX_RESPONSE_FRAME_BYTES,
    })?;
    let mut header = Vec::with_capacity(FRAME_HEADER_BYTES + payload_len - rgba.len());
    header.extend_from_slice(&length.to_be_bytes());
    header.push(TAG_DECODE_RESPONSE);
    header.extend_from_slice(&request.to_be_bytes());
    header.push(format);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&stride.to_be_bytes());
    header.extend_from_slice(&byte_len.to_be_bytes());
    let io = |error: io::Error| WireError::Io(error.kind());
    writer.write_all(&header).map_err(io)?;
    writer.write_all(rgba).map_err(io)
}

/// Encodes a message payload (tag and fields, no length prefix).
///
/// # Errors
///
/// Returns [`WireError::TooLarge`] when the payload would exceed the limit
/// for this message's direction and [`WireError::InvalidField`] when a field
/// violates its bound.
pub fn encode_message(message: &Message) -> Result<Vec<u8>, WireError> {
    let mut enc = Encoder::new();
    match message {
        Message::Hello {
            min,
            max,
            component,
            version,
            mode,
        } => {
            enc.u8(TAG_HELLO);
            enc.u16(*min);
            enc.u16(*max);
            enc.component_name(component)?;
            enc.version(version)?;
            if mode & !HELLO_MODE_MASK != 0 {
                return Err(WireError::InvalidField("hello mode bits"));
            }
            enc.u8(*mode);
        }
        Message::HelloAck {
            protocol,
            component,
            version,
            capabilities,
            caps,
        } => {
            enc.u8(TAG_HELLO_ACK);
            enc.u16(*protocol);
            enc.component_name(component)?;
            enc.version(version)?;
            if capabilities & !CAPABILITY_MASK != 0 {
                return Err(WireError::InvalidField("capability bits"));
            }
            enc.u8(*capabilities);
            enc.u32(caps.max_request_frame_bytes);
            enc.u32(caps.max_response_frame_bytes);
            enc.u32(caps.max_payload_bytes);
            enc.u32(caps.max_image_dimension);
            enc.u64(caps.max_image_pixels);
            enc.u32(caps.max_decoded_bytes);
            enc.u32(caps.max_concurrent_decodes);
            enc.u32(caps.rate_capacity);
            enc.u32(caps.rate_per_second);
        }
        Message::DecodeRequest {
            request,
            format,
            width,
            height,
            budget_bytes,
            deadline_ms,
            payload,
        } => {
            require_request(*request)?;
            check_deadline(*deadline_ms)?;
            if *budget_bytes == 0 {
                return Err(WireError::InvalidField("zero output budget"));
            }
            if payload.is_empty() {
                return Err(WireError::InvalidField("empty image payload"));
            }
            if payload.len() > MAX_PAYLOAD_BYTES {
                return Err(WireError::TooLarge {
                    len: payload.len(),
                    limit: MAX_PAYLOAD_BYTES,
                });
            }
            enc.u8(TAG_DECODE_REQUEST);
            enc.u64(*request);
            enc.u8(*format);
            enc.opt_u32(*width);
            enc.opt_u32(*height);
            enc.u32(*budget_bytes);
            enc.u32(*deadline_ms);
            enc.bytes(payload)?;
        }
        Message::DecodeResponse {
            request,
            format,
            width,
            height,
            stride,
            rgba,
        } => {
            require_request(*request)?;
            validate_response(*format, *width, *height, *stride, rgba.len())?;
            enc.u8(TAG_DECODE_RESPONSE);
            enc.u64(*request);
            enc.u8(*format);
            enc.u32(*width);
            enc.u32(*height);
            enc.u32(*stride);
            enc.bytes(rgba)?;
        }
        Message::Error {
            request,
            kind,
            message,
        } => {
            enc.u8(TAG_ERROR);
            enc.u64(*request);
            enc.u8(kind.code());
            enc.error_message(message)?;
        }
        Message::Shutdown => enc.u8(TAG_SHUTDOWN),
        Message::ShutdownAck => enc.u8(TAG_SHUTDOWN_ACK),
    }
    let limit = frame_limit(message);
    if enc.buf.len() > limit {
        return Err(WireError::TooLarge {
            len: enc.buf.len(),
            limit,
        });
    }
    Ok(enc.buf)
}

/// Decodes one payload (tag and fields, no length prefix).
///
/// # Errors
///
/// Returns [`WireError`] for an empty or oversize payload, unknown tags,
/// truncated fields, trailing bytes, invalid UTF-8, invalid booleans, or
/// any violated field bound.
pub fn decode_message(payload: &[u8]) -> Result<Message, WireError> {
    if payload.is_empty() {
        return Err(WireError::EmptyFrame);
    }
    if payload.len() > MAX_RESPONSE_FRAME_BYTES {
        return Err(WireError::TooLarge {
            len: payload.len(),
            limit: MAX_RESPONSE_FRAME_BYTES,
        });
    }
    let mut dec = Decoder::new(payload);
    let tag = dec.u8()?;
    let message = match tag {
        TAG_HELLO => Message::Hello {
            min: dec.u16()?,
            max: dec.u16()?,
            component: dec.component_name()?,
            version: dec.version()?,
            mode: dec.u8()?,
        },
        TAG_HELLO_ACK => Message::HelloAck {
            protocol: dec.u16()?,
            component: dec.component_name()?,
            version: dec.version()?,
            capabilities: dec.u8()?,
            caps: ComponentCaps {
                max_request_frame_bytes: dec.u32()?,
                max_response_frame_bytes: dec.u32()?,
                max_payload_bytes: dec.u32()?,
                max_image_dimension: dec.u32()?,
                max_image_pixels: dec.u64()?,
                max_decoded_bytes: dec.u32()?,
                max_concurrent_decodes: dec.u32()?,
                rate_capacity: dec.u32()?,
                rate_per_second: dec.u32()?,
            },
        },
        TAG_DECODE_REQUEST => Message::DecodeRequest {
            request: dec.request()?,
            format: dec.u8()?,
            width: dec.opt_u32()?,
            height: dec.opt_u32()?,
            budget_bytes: dec.u32()?,
            deadline_ms: dec.u32()?,
            payload: dec.payload()?,
        },
        TAG_DECODE_RESPONSE => {
            let request = dec.request()?;
            let format = dec.u8()?;
            let width = dec.u32()?;
            let height = dec.u32()?;
            let stride = dec.u32()?;
            let rgba = dec.response_bytes()?;
            validate_response(format, width, height, stride, rgba.len())?;
            Message::DecodeResponse {
                request,
                format,
                width,
                height,
                stride,
                rgba: rgba.to_vec(),
            }
        }
        TAG_ERROR => Message::Error {
            request: dec.u64()?,
            kind: ErrorKind::from_code(dec.u8()?).ok_or(WireError::InvalidField("error kind"))?,
            message: dec.error_message()?,
        },
        TAG_SHUTDOWN => Message::Shutdown,
        TAG_SHUTDOWN_ACK => Message::ShutdownAck,
        other => return Err(WireError::UnknownTag(other)),
    };
    dec.finish()?;
    message.validate()?;
    Ok(message)
}

impl Message {
    fn validate(&self) -> Result<(), WireError> {
        match self {
            Self::Hello { mode, .. } => {
                if mode & !HELLO_MODE_MASK != 0 {
                    return Err(WireError::InvalidField("hello mode bits"));
                }
            }
            Self::HelloAck { capabilities, .. } => {
                if capabilities & !CAPABILITY_MASK != 0 {
                    return Err(WireError::InvalidField("capability bits"));
                }
            }
            Self::DecodeRequest {
                budget_bytes,
                deadline_ms,
                payload,
                ..
            } => {
                if *budget_bytes == 0 {
                    return Err(WireError::InvalidField("zero output budget"));
                }
                check_deadline(*deadline_ms)?;
                if payload.is_empty() {
                    return Err(WireError::InvalidField("empty image payload"));
                }
            }
            Self::Error { message, .. } => {
                if message.len() > MAX_ERROR_MESSAGE_BYTES || has_control(message) {
                    return Err(WireError::InvalidField("error message"));
                }
            }
            Self::DecodeResponse { .. } | Self::Shutdown | Self::ShutdownAck => {}
        }
        Ok(())
    }
}

fn require_request(request: u64) -> Result<(), WireError> {
    if request == CONNECTION_ID {
        return Err(WireError::InvalidField("zero request id"));
    }
    Ok(())
}

fn check_deadline(deadline_ms: u32) -> Result<(), WireError> {
    if deadline_ms == 0 || deadline_ms > MAX_DEADLINE_MS {
        return Err(WireError::InvalidField("request deadline"));
    }
    Ok(())
}

fn validate_response(
    format: u8,
    width: u32,
    height: u32,
    stride: u32,
    len: usize,
) -> Result<(), WireError> {
    if format != PIXEL_FORMAT_RGBA8 {
        return Err(WireError::InvalidField("response pixel format"));
    }
    if width == 0 || height == 0 {
        return Err(WireError::InvalidField("response dimensions"));
    }
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(WireError::InvalidField("response dimensions"))?;
    let stride_expected = u64::from(width)
        .checked_mul(4)
        .ok_or(WireError::InvalidField("response stride"))?;
    if u64::from(stride) != stride_expected {
        return Err(WireError::InvalidField("response stride"));
    }
    if u64::try_from(len).ok() != Some(expected) {
        return Err(WireError::InvalidField("response length"));
    }
    if len > MAX_DECODED_BYTES as usize {
        return Err(WireError::TooLarge {
            len,
            limit: MAX_RESPONSE_FRAME_BYTES,
        });
    }
    Ok(())
}

fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

fn is_component_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

// ── encoder ─────────────────────────────────────────────────────────────────

struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), WireError> {
        let len = u32::try_from(value.len()).map_err(|_| WireError::TooLarge {
            len: value.len(),
            limit: MAX_REQUEST_FRAME_BYTES,
        })?;
        self.u32(len);
        self.buf.extend_from_slice(value);
        Ok(())
    }

    fn opt_u32(&mut self, value: Option<u32>) {
        match value {
            None => self.u8(0),
            Some(value) => {
                self.u8(1);
                self.u32(value);
            }
        }
    }

    fn component_name(&mut self, value: &str) -> Result<(), WireError> {
        if !is_component_name(value) || value.len() > MAX_COMPONENT_NAME_BYTES {
            return Err(WireError::InvalidField("component name"));
        }
        self.bounded_str(value)
    }

    fn version(&mut self, value: &str) -> Result<(), WireError> {
        if value.is_empty() || value.len() > MAX_VERSION_BYTES || has_control(value) {
            return Err(WireError::InvalidField("version"));
        }
        self.bounded_str(value)
    }

    fn error_message(&mut self, value: &str) -> Result<(), WireError> {
        if value.len() > MAX_ERROR_MESSAGE_BYTES || has_control(value) {
            return Err(WireError::InvalidField("error message"));
        }
        self.bounded_str(value)
    }

    fn bounded_str(&mut self, value: &str) -> Result<(), WireError> {
        let len = u32::try_from(value.len()).map_err(|_| WireError::TooLarge {
            len: value.len(),
            limit: MAX_REQUEST_FRAME_BYTES,
        })?;
        self.u32(len);
        self.buf.extend_from_slice(value.as_bytes());
        Ok(())
    }
}

// ── decoder ─────────────────────────────────────────────────────────────────

struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|end| *end <= self.buf.len())
            .ok_or(WireError::Truncated)?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        let bytes = self.take(1)?;
        let [value] = bytes else {
            return Err(WireError::Truncated);
        };
        Ok(*value)
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        let bytes: [u8; 2] = self.take(2)?.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn request(&mut self) -> Result<u64, WireError> {
        let request = self.u64()?;
        require_request(request)?;
        Ok(request)
    }

    fn opt_u32(&mut self) -> Result<Option<u32>, WireError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u32()?)),
            _ => Err(WireError::InvalidField("optional marker")),
        }
    }

    fn bounded_str(&mut self, max: usize, label: &'static str) -> Result<String, WireError> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if len > max {
            return Err(WireError::InvalidField(label));
        }
        let bytes = self.take(len)?;
        let text = std::str::from_utf8(bytes).map_err(|_| WireError::InvalidUtf8)?;
        Ok(text.to_owned())
    }

    fn component_name(&mut self) -> Result<String, WireError> {
        let text = self.bounded_str(MAX_COMPONENT_NAME_BYTES, "component name")?;
        if !is_component_name(&text) {
            return Err(WireError::InvalidField("component name"));
        }
        Ok(text)
    }

    fn version(&mut self) -> Result<String, WireError> {
        let text = self.bounded_str(MAX_VERSION_BYTES, "version")?;
        if text.is_empty() || has_control(&text) {
            return Err(WireError::InvalidField("version"));
        }
        Ok(text)
    }

    fn error_message(&mut self) -> Result<String, WireError> {
        let text = self.bounded_str(MAX_ERROR_MESSAGE_BYTES, "error message")?;
        if has_control(&text) {
            return Err(WireError::InvalidField("error message"));
        }
        Ok(text)
    }

    fn payload(&mut self) -> Result<Vec<u8>, WireError> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if len == 0 {
            return Err(WireError::InvalidField("empty image payload"));
        }
        if len > MAX_PAYLOAD_BYTES {
            return Err(WireError::TooLarge {
                len,
                limit: MAX_PAYLOAD_BYTES,
            });
        }
        Ok(self.take(len)?.to_vec())
    }

    fn response_bytes(&mut self) -> Result<&'a [u8], WireError> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if len > MAX_DECODED_BYTES as usize {
            return Err(WireError::TooLarge {
                len,
                limit: MAX_RESPONSE_FRAME_BYTES,
            });
        }
        if len > self.remaining() {
            return Err(WireError::Truncated);
        }
        self.take(len)
    }

    fn finish(&self) -> Result<(), WireError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(WireError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Message {
        Message::Hello {
            min: 1,
            max: 1,
            component: COMPONENT_NAME.to_owned(),
            version: "test-host".to_owned(),
            mode: HELLO_CAPABILITY_GRANTED,
        }
    }

    fn ack() -> Message {
        Message::HelloAck {
            protocol: PROTOCOL_VERSION,
            component: COMPONENT_NAME.to_owned(),
            version: super::super::COMPONENT_VERSION.to_owned(),
            capabilities: CAPABILITY_MASK,
            caps: ComponentCaps::v1(),
        }
    }

    fn request() -> Message {
        Message::DecodeRequest {
            request: 7,
            format: 32,
            width: Some(2),
            height: Some(1),
            budget_bytes: 4096,
            deadline_ms: 500,
            payload: vec![1, 2, 3, 4, 5, 6, 7, 8],
        }
    }

    fn response() -> Message {
        Message::DecodeResponse {
            request: 7,
            format: PIXEL_FORMAT_RGBA8,
            width: 1,
            height: 1,
            stride: 4,
            rgba: vec![1, 2, 3, 4],
        }
    }

    fn round_trip(message: Message) {
        let frame = encode_frame(&message).expect("encode");
        let mut reader = FrameReader::with_limit(frame.as_slice(), MAX_RESPONSE_FRAME_BYTES);
        let payload = reader.read_frame().expect("frame").expect("payload");
        assert_eq!(decode_message(&payload).expect("decode"), message);
        assert!(reader.read_frame().expect("eof").is_none());
    }

    #[test]
    fn handshake_round_trips() {
        round_trip(hello());
        round_trip(ack());
    }

    #[test]
    fn request_and_response_round_trip() {
        round_trip(request());
        round_trip(response());
    }

    #[test]
    fn shutdown_and_error_round_trip() {
        round_trip(Message::Shutdown);
        round_trip(Message::ShutdownAck);
        round_trip(Message::Error {
            request: CONNECTION_ID,
            kind: ErrorKind::Protocol,
            message: "handshake required".to_owned(),
        });
    }

    #[test]
    fn caps_are_the_pinned_values() {
        let caps = ComponentCaps::v1();
        assert_eq!(caps.max_request_frame_bytes, 4 * 1024 * 1024 + 1024);
        assert_eq!(caps.max_response_frame_bytes, 64 * 1024 * 1024 + 1024);
        assert_eq!(caps.max_payload_bytes, 4 * 1024 * 1024);
        assert_eq!(caps.max_image_dimension, 8192);
        assert_eq!(caps.max_image_pixels, 4096 * 4096);
        assert_eq!(caps.max_decoded_bytes, 64 * 1024 * 1024);
        assert_eq!(caps.max_concurrent_decodes, 1);
        assert_eq!(caps.rate_capacity, 120);
        assert_eq!(caps.rate_per_second, 60);
    }

    #[test]
    fn negotiation_is_exact() {
        assert_eq!(negotiate(1, 1), Some(1));
        assert_eq!(negotiate(0, 1), Some(1));
        assert_eq!(negotiate(1, 4), Some(1));
        assert_eq!(negotiate(2, 3), None);
        assert_eq!(negotiate(0, 0), None);
    }

    #[test]
    fn empty_and_zero_length_frames_fail_closed() {
        assert_eq!(decode_message(&[]), Err(WireError::EmptyFrame));
        let mut reader = FrameReader::new(&[0_u8; 4][..]);
        assert_eq!(reader.read_frame(), Err(WireError::EmptyFrame));
    }

    #[test]
    fn oversized_length_is_refused_from_the_header() {
        let mut reader = FrameReader::with_limit(&[0x00, 0x00, 0x00, 0x05][..], 4);
        assert_eq!(
            reader.read_frame(),
            Err(WireError::TooLarge { len: 5, limit: 4 })
        );
        // No payload bytes were read; the stream is now at a clean EOF.
        assert_eq!(reader.read_frame(), Ok(None));
    }

    #[test]
    fn truncated_header_and_payload_fail_closed() {
        let mut header = FrameReader::new(&[0x00, 0x00][..]);
        assert_eq!(header.read_frame(), Err(WireError::Truncated));
        let mut payload = FrameReader::new(&[0x00, 0x00, 0x00, 0x04, 0x01][..]);
        assert_eq!(payload.read_frame(), Err(WireError::Truncated));
    }

    #[test]
    fn unknown_tag_fails_closed() {
        assert_eq!(decode_message(&[0x7F]), Err(WireError::UnknownTag(0x7F)));
    }

    #[test]
    fn trailing_bytes_fail_closed() {
        let mut payload = encode_message(&hello()).unwrap();
        payload.push(0);
        assert_eq!(decode_message(&payload), Err(WireError::TrailingBytes));
    }

    #[test]
    fn zero_request_id_is_refused() {
        let mut payload = encode_message(&request()).unwrap();
        // tag(1) + request(8): zero the request id.
        payload[1..9].fill(0);
        assert_eq!(
            decode_message(&payload),
            Err(WireError::InvalidField("zero request id"))
        );
    }

    #[test]
    fn optional_dimensions_are_explicit() {
        let png = Message::DecodeRequest {
            request: 1,
            format: 100,
            width: None,
            height: None,
            budget_bytes: 1024,
            deadline_ms: 1,
            payload: vec![0x89],
        };
        round_trip(png);
        let odd = [
            TAG_DECODE_REQUEST,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            100,
            0x02,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
            1,
        ];
        assert_eq!(
            decode_message(&odd),
            Err(WireError::InvalidField("optional marker"))
        );
    }

    #[test]
    fn payload_bounds_are_enforced() {
        let too_big = Message::DecodeRequest {
            request: 1,
            format: 100,
            width: None,
            height: None,
            budget_bytes: 1024,
            deadline_ms: 1,
            payload: vec![0; MAX_PAYLOAD_BYTES + 1],
        };
        assert!(matches!(
            encode_message(&too_big),
            Err(WireError::TooLarge { .. })
        ));
        let empty = Message::DecodeRequest {
            request: 1,
            format: 100,
            width: None,
            height: None,
            budget_bytes: 1024,
            deadline_ms: 1,
            payload: Vec::new(),
        };
        assert_eq!(
            encode_message(&empty),
            Err(WireError::InvalidField("empty image payload"))
        );
    }

    #[test]
    fn deadline_and_budget_bounds_are_enforced() {
        for deadline in [0, MAX_DEADLINE_MS + 1] {
            let message = Message::DecodeRequest {
                request: 1,
                format: 100,
                width: None,
                height: None,
                budget_bytes: 1024,
                deadline_ms: deadline,
                payload: vec![1],
            };
            assert_eq!(
                encode_message(&message),
                Err(WireError::InvalidField("request deadline"))
            );
        }
        let zero_budget = Message::DecodeRequest {
            request: 1,
            format: 100,
            width: None,
            height: None,
            budget_bytes: 0,
            deadline_ms: 1,
            payload: vec![1],
        };
        assert_eq!(
            encode_message(&zero_budget),
            Err(WireError::InvalidField("zero output budget"))
        );
    }

    #[test]
    fn response_shape_is_exact() {
        for (format, width, height, stride, len) in [
            (2, 1, 1, 4, 4),
            (PIXEL_FORMAT_RGBA8, 0, 1, 0, 0),
            (PIXEL_FORMAT_RGBA8, 1, 1, 8, 4),
            (PIXEL_FORMAT_RGBA8, 1, 1, 4, 3),
        ] {
            let message = Message::DecodeResponse {
                request: 1,
                format,
                width,
                height,
                stride,
                rgba: vec![0; len],
            };
            assert!(encode_message(&message).is_err());
        }
    }

    #[test]
    fn hello_grammar_is_closed() {
        for component in ["", "Graphics", "graphics!", "-graphics", "a_b"] {
            let message = Message::Hello {
                min: 1,
                max: 1,
                component: component.to_owned(),
                version: "test".to_owned(),
                mode: 0,
            };
            assert!(
                encode_message(&message).is_err(),
                "component {component:?} must be refused"
            );
        }
        let bad_mode = Message::Hello {
            min: 1,
            max: 1,
            component: COMPONENT_NAME.to_owned(),
            version: "test".to_owned(),
            mode: 0x80,
        };
        assert_eq!(
            encode_message(&bad_mode),
            Err(WireError::InvalidField("hello mode bits"))
        );
    }

    #[test]
    fn error_messages_are_bounded_and_control_free() {
        let long = Message::Error {
            request: 1,
            kind: ErrorKind::Internal,
            message: "x".repeat(MAX_ERROR_MESSAGE_BYTES + 1),
        };
        assert_eq!(
            encode_message(&long),
            Err(WireError::InvalidField("error message"))
        );
        let control = Message::Error {
            request: 1,
            kind: ErrorKind::Internal,
            message: "bad\nline".to_owned(),
        };
        assert_eq!(
            encode_message(&control),
            Err(WireError::InvalidField("error message"))
        );
    }

    #[test]
    fn error_kind_codes_are_stable() {
        for kind in [
            ErrorKind::Denied,
            ErrorKind::InvalidRequest,
            ErrorKind::Budget,
            ErrorKind::Unsupported,
            ErrorKind::Protocol,
            ErrorKind::Timeout,
            ErrorKind::Internal,
            ErrorKind::RateLimited,
            ErrorKind::MalformedImage,
        ] {
            assert_eq!(ErrorKind::from_code(kind.code()), Some(kind));
        }
        assert_eq!(ErrorKind::from_code(0), None);
        assert_eq!(ErrorKind::from_code(5), None);
        assert_eq!(ErrorKind::from_code(11), None);
    }

    #[test]
    fn wire_error_display_is_stable() {
        assert_eq!(
            WireError::TooLarge { len: 9, limit: 4 }.to_string(),
            "frame is 9 bytes (limit 4)"
        );
        assert_eq!(WireError::EmptyFrame.to_string(), "empty frame");
        assert_eq!(WireError::UnknownTag(0x7F).to_string(), "unknown tag 0x7f");
    }
}
