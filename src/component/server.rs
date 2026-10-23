//! Graphics component server: serves one bounded decode request at a time
//! over the v1 stdio protocol.
//!
//! The binary entry point (`bitty-graphics` with no arguments) calls
//! [`serve_stdio`]. Tests call [`serve`] with explicit streams, or drive
//! [`Session`] directly with an injected clock.
//!
//! # What the server is
//!
//! Core owns the trust decision: bounded protocol intake, placement policy,
//! the aggregate image budget, pre-allocation validation, and pre-upload
//! validation all stay in Core. This process is mechanism only:
//!
//! - It decodes an opaque payload handed to it through
//!   [`Message::DecodeRequest`], using the crate's existing bounded decoder
//!   ([`decode_kitty_payload_owned`]). It never parses a path, argv,
//!   environment, URL, socket, or command from the wire.
//! - Every request is validated against the advertised caps before any pixel
//!   buffer is allocated, and every response is checked for the exact
//!   normalized shape (`stride == width * 4`, `len == width * height * 4`).
//! - It keeps no cross-request image state: one request produces one bitmap,
//!   and the bitmap is dropped after its frame is written.
//! - It never persists content, uploads a texture, or places an image.
//!
//! # Concurrency and ordering
//!
//! The loop is single-threaded and handles one decode at a time
//! ([`MAX_CONCURRENT_DECODES`]). Requests are processed in arrival order and
//! answered in the same order. A host that stops reading its responses
//! applies ordinary pipe backpressure; the component never queues frames
//! without bound.
//!
//! # Shutdown, EOF, and crashes
//!
//! `Shutdown` (or stdin EOF) writes `ShutdownAck` for an explicit shutdown
//! and exits 0. A framing, decode-schema, or handshake violation writes a
//! connection-level `Error` and exits 3. An internal write failure exits 4.
//! A decode failure of the *payload bytes* (malformed PNG, over-cap
//! dimensions, length mismatch) is a per-request typed `Error`; the
//! connection stays open and the next request is served.
//!
//! A component crash cannot be cleaned from inside; the Core host client must
//! place it in its own process group and treat EOF as `component_lost`
//! (documented remaining host work). The server starts no threads, so it
//! cannot leave background work behind.

use std::io::{self, BufWriter, Read, Write};
use std::time::{Duration, Instant};

use crate::kitty_decode::{
    KITTY_DECODE_MAX_BYTES, KittyDecodeError, KittyTransmitFormat, decode_kitty_payload_owned,
};

use super::protocol::{
    CAPABILITY_MASK, COMPONENT_NAME, CONNECTION_ID, ComponentCaps, ErrorKind, FrameReader,
    HELLO_CAPABILITY_GRANTED, HELLO_MODE_MASK, HELLO_SAFE_MODE, MAX_DEADLINE_MS, MAX_DECODED_BYTES,
    MAX_REQUEST_FRAME_BYTES, Message, PIXEL_FORMAT_RGBA8, RATE_CAPACITY, RATE_PER_SECOND,
    decode_message, negotiate, write_frame,
};

/// Version reported in `HelloAck` and by `--version` (the crate version).
pub use super::COMPONENT_VERSION;

/// Clean shutdown (explicit `Shutdown` or stdin EOF).
pub const SERVER_EXIT_OK: u8 = 0;
/// Usage refusal (the operator asked for something the binary does not
/// serve); mirrors the other component binaries.
pub const SERVER_EXIT_REFUSED: u8 = 2;
/// Handshake or protocol violation; the peer is told why first when the
/// stream still permits a write.
pub const SERVER_EXIT_PROTOCOL: u8 = 3;
/// The server could not write its output stream.
pub const SERVER_EXIT_INTERNAL: u8 = 4;

/// Consecutive rate violations before the connection is closed.
pub const RATE_MAX_VIOLATIONS: u32 = 16;

const ERR_HANDSHAKE_REQUIRED: &str = "handshake required";
const ERR_PROTOCOL_VERSION: &str = "protocol version unsupported";
const ERR_COMPONENT_MISMATCH: &str = "component name mismatch";
const ERR_UNKNOWN_HELLO_MODE: &str = "unknown hello mode";
const ERR_DUPLICATE_HANDSHAKE: &str = "duplicate handshake";
const ERR_DENIED: &str = "graphics decode denied by host policy";
const ERR_RATE_LIMITED: &str = "request rate exceeded";
const ERR_UNEXPECTED: &str = "unexpected message";

const ERR_UNSUPPORTED_FORMAT: &str = "unsupported transmit format";
const ERR_EMPTY_PAYLOAD: &str = "empty image payload";
const ERR_MISSING_DIMENSIONS: &str = "raw format needs width and height";
const ERR_ZERO_DIMENSION: &str = "image dimension is zero";
const ERR_DIMENSIONS_BUDGET: &str = "image dimensions exceed the cap";
const ERR_PIXELS_BUDGET: &str = "image area exceeds the cap";
const ERR_DECODED_BUDGET: &str = "decoded bitmap exceeds the cap";
const ERR_LENGTH_MISMATCH: &str = "payload length does not match declared dimensions";
const ERR_MALFORMED_IMAGE: &str = "image payload is malformed";
const ERR_OUTPUT_BUDGET: &str = "decoded bitmap exceeds the declared output budget";
const ERR_DEADLINE: &str = "request deadline elapsed";
const ERR_OUTPUT_SHAPE: &str = "decoded bitmap has an inconsistent shape";

// Keep the server's decode ceiling tied to the protocol cap.
const _: () = assert!(KITTY_DECODE_MAX_BYTES == MAX_DECODED_BYTES as usize);

/// Serves the protocol on stdin/stdout.
#[must_use]
pub fn serve_stdio() -> u8 {
    serve(io::stdin(), io::stdout())
}

/// Serves the protocol on explicit streams (tests and embedders).
///
/// Returns [`SERVER_EXIT_OK`], [`SERVER_EXIT_PROTOCOL`], or
/// [`SERVER_EXIT_INTERNAL`].
pub fn serve<R, W>(input: R, output: W) -> u8
where
    R: Read,
    W: Write,
{
    let mut reader = FrameReader::with_limit(input, MAX_REQUEST_FRAME_BYTES);
    let mut writer = BufWriter::with_capacity(64 * 1024, output);
    let mut session = Session::new();
    loop {
        let payload = match reader.read_frame() {
            Ok(None) => return SERVER_EXIT_OK,
            Ok(Some(payload)) => payload,
            Err(error) => {
                let _ = write_connection_error(&mut writer, &error.to_string());
                return SERVER_EXIT_PROTOCOL;
            }
        };
        let message = match decode_message(&payload) {
            Ok(message) => message,
            Err(error) => {
                let _ = write_connection_error(&mut writer, &error.to_string());
                return SERVER_EXIT_PROTOCOL;
            }
        };
        let outcome = session.handle(message, Instant::now());
        for reply in &outcome.replies {
            if write_frame(&mut writer, reply).is_err() {
                return SERVER_EXIT_INTERNAL;
            }
        }
        if writer.flush().is_err() {
            return SERVER_EXIT_INTERNAL;
        }
        if let Some(code) = outcome.close {
            return code;
        }
    }
}

fn write_connection_error<W: Write>(writer: &mut W, detail: &str) -> Result<(), ()> {
    let message = Message::Error {
        request: CONNECTION_ID,
        kind: ErrorKind::Protocol,
        message: bounded_diagnostic(detail),
    };
    write_frame(writer, &message).map_err(|_| ())?;
    writer.flush().map_err(|_| ())
}

/// One bounded static diagnostic.
///
/// Wire and decode errors never carry peer content (strings are validated
/// before use and the formatted error is a fixed shape plus numbers), but the
/// result is still clamped so a future detail cannot grow the frame.
fn bounded_diagnostic(detail: &str) -> String {
    const FALLBACK: &str = "invalid protocol frame";
    if detail.is_empty() || detail.len() > super::protocol::MAX_ERROR_MESSAGE_BYTES {
        return FALLBACK.to_owned();
    }
    let filtered: String = detail
        .chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .collect();
    if filtered.is_empty() {
        FALLBACK.to_owned()
    } else {
        filtered
    }
}

/// Result of one handled message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionOutcome {
    /// Frames to write, in order.
    pub replies: Vec<Message>,
    /// Exit code to use after writing `replies`, when the session ends.
    pub close: Option<u8>,
}

/// One connection's protocol state.
///
/// The session is single-threaded and owns the handshake phase, the
/// safe-mode/deprivation decision, and the request rate gate. Tests inject
/// [`Instant`] into [`Session::handle`] so rate policy is deterministic.
pub struct Session {
    phase: Phase,
    deprived: bool,
    rate: RateGate,
    rate_violations: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    AwaitingHello,
    Ready,
    Closed,
}

struct RateGate {
    tokens: u32,
    last: Option<Instant>,
}

impl RateGate {
    const fn new() -> Self {
        Self {
            tokens: RATE_CAPACITY,
            last: None,
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last {
            let elapsed = now.saturating_duration_since(last);
            let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            let refill = u32::try_from(millis.saturating_mul(u64::from(RATE_PER_SECOND)) / 1000)
                .unwrap_or(u32::MAX);
            if refill > 0 {
                self.tokens = self.tokens.saturating_add(refill).min(RATE_CAPACITY);
                self.last = Some(now);
            }
        } else {
            self.last = Some(now);
        }
        if self.tokens == 0 {
            false
        } else {
            self.tokens -= 1;
            true
        }
    }
}

impl Session {
    /// Starts a session awaiting `Hello`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: Phase::AwaitingHello,
            deprived: false,
            rate: RateGate::new(),
            rate_violations: 0,
        }
    }

    /// Handles one decoded message.
    #[must_use]
    pub fn handle(&mut self, message: Message, now: Instant) -> SessionOutcome {
        let mut outcome = SessionOutcome::default();
        if self.phase == Phase::Closed {
            outcome.close = Some(SERVER_EXIT_PROTOCOL);
            return outcome;
        }

        match message {
            Message::Hello {
                min,
                max,
                component,
                version: _,
                mode,
            } => {
                self.handle_hello(min, max, &component, mode, &mut outcome);
                return outcome;
            }
            Message::Shutdown => {
                outcome.replies.push(Message::ShutdownAck);
                outcome.close = Some(SERVER_EXIT_OK);
                self.phase = Phase::Closed;
                return outcome;
            }
            _ => {}
        }

        if self.phase != Phase::Ready {
            outcome.replies.push(Message::Error {
                request: CONNECTION_ID,
                kind: ErrorKind::Protocol,
                message: ERR_HANDSHAKE_REQUIRED.to_owned(),
            });
            outcome.close = Some(SERVER_EXIT_PROTOCOL);
            self.phase = Phase::Closed;
            return outcome;
        }

        if self.consumes_rate(&message) && !self.rate.allow(now) {
            self.rate_violations = self.rate_violations.saturating_add(1);
            outcome.replies.push(Message::Error {
                request: message.request_id().unwrap_or(CONNECTION_ID),
                kind: ErrorKind::RateLimited,
                message: ERR_RATE_LIMITED.to_owned(),
            });
            if self.rate_violations >= RATE_MAX_VIOLATIONS {
                outcome.close = Some(SERVER_EXIT_PROTOCOL);
                self.phase = Phase::Closed;
            }
            return outcome;
        }
        let request_id = message.request_id().unwrap_or(CONNECTION_ID);

        match message {
            Message::DecodeRequest {
                request,
                format,
                width,
                height,
                budget_bytes,
                deadline_ms,
                payload,
            } => {
                if self.deprived {
                    outcome.replies.push(denied(request));
                } else {
                    outcome.replies.push(self.decode(
                        request,
                        format,
                        width,
                        height,
                        budget_bytes,
                        deadline_ms,
                        payload,
                        now,
                    ));
                }
            }
            Message::Hello { .. }
            | Message::HelloAck { .. }
            | Message::DecodeResponse { .. }
            | Message::Error { .. }
            | Message::Shutdown
            | Message::ShutdownAck => {
                outcome.replies.push(Message::Error {
                    request: request_id,
                    kind: ErrorKind::Protocol,
                    message: ERR_UNEXPECTED.to_owned(),
                });
                outcome.close = Some(SERVER_EXIT_PROTOCOL);
                self.phase = Phase::Closed;
            }
        }
        outcome
    }

    fn handle_hello(
        &mut self,
        min: u16,
        max: u16,
        component: &str,
        mode: u8,
        outcome: &mut SessionOutcome,
    ) {
        if self.phase == Phase::Ready {
            outcome.replies.push(Message::Error {
                request: CONNECTION_ID,
                kind: ErrorKind::Protocol,
                message: ERR_DUPLICATE_HANDSHAKE.to_owned(),
            });
            outcome.close = Some(SERVER_EXIT_PROTOCOL);
            self.phase = Phase::Closed;
            return;
        }
        if mode & !HELLO_MODE_MASK != 0 {
            outcome.replies.push(Message::Error {
                request: CONNECTION_ID,
                kind: ErrorKind::Protocol,
                message: ERR_UNKNOWN_HELLO_MODE.to_owned(),
            });
            outcome.close = Some(SERVER_EXIT_PROTOCOL);
            self.phase = Phase::Closed;
            return;
        }
        if component != COMPONENT_NAME {
            outcome.replies.push(Message::Error {
                request: CONNECTION_ID,
                kind: ErrorKind::Protocol,
                message: ERR_COMPONENT_MISMATCH.to_owned(),
            });
            outcome.close = Some(SERVER_EXIT_PROTOCOL);
            self.phase = Phase::Closed;
            return;
        }
        let Some(protocol) = negotiate(min, max) else {
            outcome.replies.push(Message::Error {
                request: CONNECTION_ID,
                kind: ErrorKind::Protocol,
                message: ERR_PROTOCOL_VERSION.to_owned(),
            });
            outcome.close = Some(SERVER_EXIT_PROTOCOL);
            self.phase = Phase::Closed;
            return;
        };
        self.deprived = mode & HELLO_SAFE_MODE != 0 || mode & HELLO_CAPABILITY_GRANTED == 0;
        let capabilities = if self.deprived { 0 } else { CAPABILITY_MASK };
        self.phase = Phase::Ready;
        outcome.replies.push(Message::HelloAck {
            protocol,
            component: COMPONENT_NAME.to_owned(),
            version: COMPONENT_VERSION.to_owned(),
            capabilities,
            caps: ComponentCaps::v1(),
        });
    }

    fn consumes_rate(&self, message: &Message) -> bool {
        matches!(message, Message::DecodeRequest { .. })
    }

    #[allow(clippy::too_many_arguments)]
    fn decode(
        &mut self,
        request: u64,
        format: u8,
        width: Option<u32>,
        height: Option<u32>,
        budget_bytes: u32,
        deadline_ms: u32,
        payload: Vec<u8>,
        started: Instant,
    ) -> Message {
        let Some(transmit) = KittyTransmitFormat::from_f(u32::from(format)) else {
            return refusal(request, ErrorKind::Unsupported, ERR_UNSUPPORTED_FORMAT);
        };
        // Defensive re-check: the wire decoder already bounds both fields.
        if deadline_ms == 0 || deadline_ms > MAX_DEADLINE_MS {
            return refusal(request, ErrorKind::InvalidRequest, ERR_DEADLINE);
        }
        let effective_budget = u64::from(budget_bytes).min(u64::from(MAX_DECODED_BYTES));
        if effective_budget == 0 {
            return refusal(request, ErrorKind::Budget, ERR_OUTPUT_BUDGET);
        }

        let image =
            match decode_kitty_payload_owned(transmit, width, height, payload.into_boxed_slice()) {
                Ok(image) => image,
                Err(error) => {
                    let (kind, message) = decode_error(&error);
                    return refusal(request, kind, message);
                }
            };

        // The deadline is a host-owned ceiling. v1 cannot interrupt a decode
        // already in progress without a watchdog thread, so it checks after
        // the fact and withholds an over-deadline bitmap; Core's supervisor
        // still owns hard wall-clock enforcement (it can kill the process).
        let deadline = Duration::from_millis(u64::from(deadline_ms));
        if started.elapsed() > deadline {
            return refusal(request, ErrorKind::Timeout, ERR_DEADLINE);
        }

        let (pixel_width, pixel_height) = image.dimensions();
        let rgba = image.into_rgba();
        let expected = u64::from(pixel_width)
            .checked_mul(u64::from(pixel_height))
            .and_then(|pixels| pixels.checked_mul(4));
        let Some(expected) = expected else {
            return refusal(request, ErrorKind::Internal, ERR_OUTPUT_SHAPE);
        };
        if u64::try_from(rgba.len()).ok() != Some(expected) {
            return refusal(request, ErrorKind::Internal, ERR_OUTPUT_SHAPE);
        }
        if expected > effective_budget {
            return refusal(request, ErrorKind::Budget, ERR_OUTPUT_BUDGET);
        }
        let Some(stride) = pixel_width.checked_mul(4) else {
            return refusal(request, ErrorKind::Internal, ERR_OUTPUT_SHAPE);
        };
        Message::DecodeResponse {
            request,
            format: PIXEL_FORMAT_RGBA8,
            width: pixel_width,
            height: pixel_height,
            stride,
            rgba,
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

fn denied(request: u64) -> Message {
    Message::Error {
        request,
        kind: ErrorKind::Denied,
        message: ERR_DENIED.to_owned(),
    }
}

fn refusal(request: u64, kind: ErrorKind, message: &'static str) -> Message {
    Message::Error {
        request,
        kind,
        message: message.to_owned(),
    }
}

fn decode_error(error: &KittyDecodeError) -> (ErrorKind, &'static str) {
    match error {
        KittyDecodeError::EmptyPayload => (ErrorKind::InvalidRequest, ERR_EMPTY_PAYLOAD),
        KittyDecodeError::MissingDimensions => (ErrorKind::InvalidRequest, ERR_MISSING_DIMENSIONS),
        KittyDecodeError::ZeroDimension => (ErrorKind::InvalidRequest, ERR_ZERO_DIMENSION),
        KittyDecodeError::DimensionsTooLarge { .. } => (ErrorKind::Budget, ERR_DIMENSIONS_BUDGET),
        KittyDecodeError::TooManyPixels { .. } => (ErrorKind::Budget, ERR_PIXELS_BUDGET),
        KittyDecodeError::DecodedTooLarge { .. } => (ErrorKind::Budget, ERR_DECODED_BUDGET),
        KittyDecodeError::LengthMismatch { .. } => (ErrorKind::InvalidRequest, ERR_LENGTH_MISMATCH),
        KittyDecodeError::MalformedPng(_) => (ErrorKind::MalformedImage, ERR_MALFORMED_IMAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{MAX_ERROR_MESSAGE_BYTES, PROTOCOL_VERSION};
    use super::*;

    /// 1x1 RGBA PNG, single red opaque pixel (same fixture the decoder
    /// tests pin; generated with Python stdlib `zlib`).
    const PNG_1X1_RGBA_RED: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 86, 199, 47, 13, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    fn now() -> Instant {
        Instant::now()
    }

    fn handshake(session: &mut Session) -> SessionOutcome {
        session.handle(
            Message::Hello {
                min: PROTOCOL_VERSION,
                max: PROTOCOL_VERSION,
                component: COMPONENT_NAME.to_owned(),
                version: "test-host".to_owned(),
                mode: HELLO_CAPABILITY_GRANTED,
            },
            now(),
        )
    }

    fn png_request(request: u64) -> Message {
        Message::DecodeRequest {
            request,
            format: 100,
            width: None,
            height: None,
            budget_bytes: 4096,
            deadline_ms: 5_000,
            payload: PNG_1X1_RGBA_RED.to_vec(),
        }
    }

    #[test]
    fn handshake_reports_caps() {
        let mut session = Session::new();
        let outcome = handshake(&mut session);
        assert_eq!(outcome.close, None);
        let [
            Message::HelloAck {
                protocol,
                component,
                version,
                capabilities,
                caps,
            },
        ] = outcome.replies.as_slice()
        else {
            panic!("expected one HelloAck, got {:?}", outcome.replies);
        };
        assert_eq!(*protocol, PROTOCOL_VERSION);
        assert_eq!(component, COMPONENT_NAME);
        assert_eq!(version, COMPONENT_VERSION);
        assert_eq!(*capabilities, CAPABILITY_MASK);
        assert_eq!(*caps, ComponentCaps::v1());
    }

    #[test]
    fn handshake_is_required_first() {
        let mut session = Session::new();
        let outcome = session.handle(png_request(1), now());
        assert_eq!(outcome.close, Some(SERVER_EXIT_PROTOCOL));
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                kind: ErrorKind::Protocol,
                ..
            }]
        ));
    }

    #[test]
    fn duplicate_handshake_closes() {
        let mut session = Session::new();
        handshake(&mut session);
        let outcome = handshake(&mut session);
        assert_eq!(outcome.close, Some(SERVER_EXIT_PROTOCOL));
    }

    #[test]
    fn component_and_version_mismatch_close() {
        for (component, min, max) in [
            ("other", 1, 1),
            (COMPONENT_NAME, 2, 3),
            (COMPONENT_NAME, 0, 0),
        ] {
            let mut session = Session::new();
            let outcome = session.handle(
                Message::Hello {
                    min,
                    max,
                    component: component.to_owned(),
                    version: "test".to_owned(),
                    mode: HELLO_CAPABILITY_GRANTED,
                },
                now(),
            );
            assert_eq!(outcome.close, Some(SERVER_EXIT_PROTOCOL));
        }
    }

    #[test]
    fn unknown_hello_mode_closes() {
        let mut session = Session::new();
        let outcome = session.handle(
            Message::Hello {
                min: 1,
                max: 1,
                component: COMPONENT_NAME.to_owned(),
                version: "test".to_owned(),
                mode: 0x80,
            },
            now(),
        );
        assert_eq!(outcome.close, Some(SERVER_EXIT_PROTOCOL));
    }

    #[test]
    fn safe_mode_is_deprived_but_shutdown_works() {
        let mut session = Session::new();
        let outcome = session.handle(
            Message::Hello {
                min: 1,
                max: 1,
                component: COMPONENT_NAME.to_owned(),
                version: "test".to_owned(),
                mode: HELLO_SAFE_MODE | HELLO_CAPABILITY_GRANTED,
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::HelloAck {
                capabilities: 0,
                ..
            }]
        ));
        let outcome = session.handle(png_request(1), now());
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                kind: ErrorKind::Denied,
                request: 1,
                ..
            }]
        ));
        let outcome = session.handle(Message::Shutdown, now());
        assert_eq!(outcome.replies, vec![Message::ShutdownAck]);
        assert_eq!(outcome.close, Some(SERVER_EXIT_OK));
    }

    #[test]
    fn missing_capability_is_deprived() {
        let mut session = Session::new();
        let outcome = session.handle(
            Message::Hello {
                min: 1,
                max: 1,
                component: COMPONENT_NAME.to_owned(),
                version: "test".to_owned(),
                mode: 0,
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::HelloAck {
                capabilities: 0,
                ..
            }]
        ));
    }

    #[test]
    fn png_request_returns_normalized_rgba() {
        let mut session = Session::new();
        handshake(&mut session);
        let outcome = session.handle(png_request(9), now());
        let [
            Message::DecodeResponse {
                request,
                format,
                width,
                height,
                stride,
                rgba,
            },
        ] = outcome.replies.as_slice()
        else {
            panic!("expected one DecodeResponse, got {:?}", outcome.replies);
        };
        assert_eq!(*request, 9);
        assert_eq!(*format, PIXEL_FORMAT_RGBA8);
        assert_eq!((*width, *height, *stride), (1, 1, 4));
        assert_eq!(rgba, &[0xFF, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn raw_rgba_and_rgb_decode() {
        let mut session = Session::new();
        handshake(&mut session);
        let outcome = session.handle(
            Message::DecodeRequest {
                request: 1,
                format: 32,
                width: Some(2),
                height: Some(1),
                budget_bytes: 64,
                deadline_ms: 1_000,
                payload: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::DecodeResponse { rgba, .. }] if rgba == &[1, 2, 3, 4, 5, 6, 7, 8]
        ));
        let outcome = session.handle(
            Message::DecodeRequest {
                request: 2,
                format: 24,
                width: Some(1),
                height: Some(1),
                budget_bytes: 64,
                deadline_ms: 1_000,
                payload: vec![10, 20, 30],
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::DecodeResponse { rgba, .. }] if rgba == &[10, 20, 30, 0xFF]
        ));
    }

    #[test]
    fn malformed_payload_is_typed_and_connection_survives() {
        let mut session = Session::new();
        handshake(&mut session);
        let outcome = session.handle(
            Message::DecodeRequest {
                request: 4,
                format: 100,
                width: None,
                height: None,
                budget_bytes: 4096,
                deadline_ms: 1_000,
                payload: b"not a png".to_vec(),
            },
            now(),
        );
        assert_eq!(outcome.close, None);
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                request: 4,
                kind: ErrorKind::MalformedImage,
                ..
            }]
        ));
        // The next request still decodes.
        let outcome = session.handle(png_request(5), now());
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::DecodeResponse { request: 5, .. }]
        ));
    }

    #[test]
    fn decode_bounds_map_to_typed_errors() {
        let mut session = Session::new();
        handshake(&mut session);
        // 100_000 x 100_000 raw RGBA would need far beyond IMG-3.
        let outcome = session.handle(
            Message::DecodeRequest {
                request: 1,
                format: 32,
                width: Some(100_000),
                height: Some(100_000),
                budget_bytes: MAX_DECODED_BYTES,
                deadline_ms: 1_000,
                payload: vec![0; 8],
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                kind: ErrorKind::Budget,
                ..
            }]
        ));
        // Missing raw dimensions.
        let outcome = session.handle(
            Message::DecodeRequest {
                request: 2,
                format: 24,
                width: None,
                height: Some(1),
                budget_bytes: 64,
                deadline_ms: 1_000,
                payload: vec![0; 3],
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                kind: ErrorKind::InvalidRequest,
                ..
            }]
        ));
        // Unknown format is unsupported, not a connection failure.
        let outcome = session.handle(
            Message::DecodeRequest {
                request: 3,
                format: 7,
                width: Some(1),
                height: Some(1),
                budget_bytes: 64,
                deadline_ms: 1_000,
                payload: vec![0; 4],
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                kind: ErrorKind::Unsupported,
                ..
            }]
        ));
    }

    #[test]
    fn output_budget_smaller_than_bitmap_is_refused() {
        let mut session = Session::new();
        handshake(&mut session);
        let outcome = session.handle(
            Message::DecodeRequest {
                request: 1,
                format: 32,
                width: Some(2),
                height: Some(1),
                budget_bytes: 4,
                deadline_ms: 1_000,
                payload: vec![1; 8],
            },
            now(),
        );
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                kind: ErrorKind::Budget,
                ..
            }]
        ));
    }

    #[test]
    fn rate_gate_sheds_burst_then_closes() {
        let mut session = Session::new();
        handshake(&mut session);
        let fixed = now();
        for _ in 0..RATE_CAPACITY {
            let outcome = session.handle(png_request(1), fixed);
            assert_eq!(outcome.close, None);
        }
        let mut close = None;
        let mut refused = 0;
        for _ in 0..RATE_MAX_VIOLATIONS {
            let outcome = session.handle(png_request(1), fixed);
            if matches!(
                outcome.replies.as_slice(),
                [Message::Error {
                    kind: ErrorKind::RateLimited,
                    ..
                }]
            ) {
                refused += 1;
            }
            close = outcome.close;
        }
        assert_eq!(refused, RATE_MAX_VIOLATIONS);
        assert_eq!(close, Some(SERVER_EXIT_PROTOCOL));
    }

    #[test]
    fn server_to_host_messages_are_unexpected() {
        let mut session = Session::new();
        handshake(&mut session);
        let outcome = session.handle(Message::ShutdownAck, now());
        assert_eq!(outcome.close, Some(SERVER_EXIT_PROTOCOL));
        assert!(matches!(
            outcome.replies.as_slice(),
            [Message::Error {
                kind: ErrorKind::Protocol,
                ..
            }]
        ));
    }

    #[test]
    fn shutdown_is_always_available() {
        let mut session = Session::new();
        // Before the handshake too.
        let outcome = session.handle(Message::Shutdown, now());
        assert_eq!(outcome.replies, vec![Message::ShutdownAck]);
        assert_eq!(outcome.close, Some(SERVER_EXIT_OK));
    }

    #[test]
    fn diagnostics_are_bounded_and_control_free() {
        assert_eq!(bounded_diagnostic(""), "invalid protocol frame");
        assert_eq!(bounded_diagnostic("bad\nline"), "bad?line");
        let long = "x".repeat(MAX_ERROR_MESSAGE_BYTES + 1);
        assert_eq!(bounded_diagnostic(&long), "invalid protocol frame");
    }
}
