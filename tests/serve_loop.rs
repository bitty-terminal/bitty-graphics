//! Live component-server integration tests.
//!
//! The real `bitty-graphics` binary is spawned as a stdio coprocess and
//! driven with the public protocol codec. Set
//! `BITTY_GRAPHICS_COMPONENT_BIN` to run the same suite against a packed
//! release executable (the CI `component-dist` job does this after packing).
//!
//! Covered acceptance paths: handshake and advertised caps, PNG and raw
//! decode, normalized response shape, typed decode failures that keep the
//! connection alive, safe-mode deprivation, zero-length/oversized/unknown/
//! truncated/trailing frames that fail closed, clean `Shutdown`, clean stdin
//! EOF, and the CLI surface.

use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use bitty_graphics::component::protocol::{
    CAPABILITY_MASK, COMPONENT_NAME, ComponentCaps, ErrorKind, FrameReader,
    HELLO_CAPABILITY_GRANTED, HELLO_SAFE_MODE, MAX_DECODED_BYTES, MAX_REQUEST_FRAME_BYTES,
    MAX_RESPONSE_FRAME_BYTES, Message, PIXEL_FORMAT_RGBA8, PROTOCOL_VERSION, decode_message,
    encode_message,
};
use bitty_graphics::component::server::{
    COMPONENT_VERSION, SERVER_EXIT_OK, SERVER_EXIT_PROTOCOL, SERVER_EXIT_REFUSED,
};

/// Upper bound for one protocol message or one process observation.
const WAIT: Duration = Duration::from_secs(20);

/// 1x1 RGBA PNG, single red opaque pixel.
const PNG_1X1_RGBA_RED: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 248, 207, 192, 240, 31, 0,
    5, 0, 1, 255, 86, 199, 47, 13, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// One spawned component process plus its decoded message stream.
struct Component {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Result<Message, String>>,
    capabilities: u8,
}

impl Component {
    fn binary() -> PathBuf {
        std::env::var_os("BITTY_GRAPHICS_COMPONENT_BIN")
            .map_or_else(|| env!("CARGO_BIN_EXE_bitty-graphics").into(), Into::into)
    }

    fn spawn() -> Self {
        let mut child = Command::new(Self::binary())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bitty-graphics");
        let stdin = child.stdin.take().expect("component stdin");
        let stdout = child.stdout.take().expect("component stdout");
        let (sender, messages) = mpsc::channel();
        std::thread::Builder::new()
            .name("component-reader".to_owned())
            .spawn(move || {
                let mut reader =
                    FrameReader::with_limit(BufReader::new(stdout), MAX_RESPONSE_FRAME_BYTES);
                loop {
                    match reader.read_message() {
                        Ok(Some(message)) => {
                            if sender.send(Ok(message)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(Err(error.to_string()));
                            break;
                        }
                    }
                }
            })
            .expect("reader thread");
        Self {
            child,
            stdin: Some(stdin),
            messages,
            capabilities: 0,
        }
    }

    fn start() -> Self {
        Self::start_with_mode(HELLO_CAPABILITY_GRANTED)
    }

    fn start_with_mode(mode: u8) -> Self {
        let mut component = Self::spawn();
        component.handshake(mode);
        component
    }

    fn handshake(&mut self, mode: u8) {
        self.send(&Message::Hello {
            min: PROTOCOL_VERSION,
            max: PROTOCOL_VERSION,
            component: COMPONENT_NAME.to_owned(),
            version: "test-host".to_owned(),
            mode,
        });
        match self.recv() {
            Message::HelloAck {
                protocol,
                component,
                version,
                capabilities,
                caps,
            } => {
                assert_eq!(protocol, PROTOCOL_VERSION);
                assert_eq!(component, COMPONENT_NAME);
                assert_eq!(version, COMPONENT_VERSION);
                assert_eq!(caps, ComponentCaps::v1());
                self.capabilities = capabilities;
            }
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    fn send(&mut self, message: &Message) {
        let payload = encode_message(message).expect("encode message");
        self.send_raw(&frame(&payload));
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("component stdin");
        stdin.write_all(bytes).expect("write raw bytes");
        stdin.flush().expect("flush raw bytes");
    }

    fn drop_stdin(&mut self) {
        drop(self.stdin.take());
    }

    fn recv(&mut self) -> Message {
        match self.messages.recv_timeout(WAIT) {
            Ok(Ok(message)) => message,
            Ok(Err(error)) => panic!("component decode error: {error}"),
            Err(error) => panic!("component message timeout: {error}"),
        }
    }

    fn expect_error(&mut self, kind: ErrorKind) -> Message {
        match self.recv() {
            message @ Message::Error { kind: got, .. } if got == kind => message,
            other => panic!("expected {kind:?}, got {other:?}"),
        }
    }

    fn expect_connection_error(&mut self) -> Message {
        match self.recv() {
            message @ Message::Error { request: 0, .. } => message,
            other => panic!("expected a connection-level Error, got {other:?}"),
        }
    }

    fn wait_code(&mut self) -> i32 {
        self.child
            .wait()
            .expect("wait for component")
            .code()
            .expect("component exited without a code")
    }

    fn shutdown(mut self) {
        self.send(&Message::Shutdown);
        assert_eq!(self.recv(), Message::ShutdownAck);
        // Dropping stdin after the ack proves the server does not wait for an
        // EOF that will never come.
        self.drop_stdin();
        assert_eq!(self.wait_code(), i32::from(SERVER_EXIT_OK));
    }
}

/// Wraps one payload in the v1 length-prefixed frame.
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut framed = u32::try_from(payload.len())
        .expect("test payload fits u32")
        .to_be_bytes()
        .to_vec();
    framed.extend_from_slice(payload);
    framed
}

fn decode_request(request: u64, format: u8, payload: &[u8]) -> Message {
    Message::DecodeRequest {
        request,
        format,
        width: None,
        height: None,
        budget_bytes: MAX_DECODED_BYTES,
        deadline_ms: 5_000,
        payload: payload.to_vec(),
    }
}

#[test]
fn handshake_advertises_caps_and_png_decodes() {
    let mut component = Component::start();
    assert_eq!(component.capabilities, CAPABILITY_MASK);

    component.send(&decode_request(1, 100, PNG_1X1_RGBA_RED));
    match component.recv() {
        Message::DecodeResponse {
            request,
            format,
            width,
            height,
            stride,
            rgba,
        } => {
            assert_eq!(request, 1);
            assert_eq!(format, PIXEL_FORMAT_RGBA8);
            assert_eq!((width, height, stride), (1, 1, 4));
            assert_eq!(rgba, [0xFF, 0x00, 0x00, 0xFF]);
        }
        other => panic!("expected DecodeResponse, got {other:?}"),
    }

    component.shutdown();
}

#[test]
fn raw_rgb_and_rgba_decode_with_normalized_shape() {
    let mut component = Component::start();
    component.send(&Message::DecodeRequest {
        request: 2,
        format: 32,
        width: Some(2),
        height: Some(1),
        budget_bytes: 64,
        deadline_ms: 1_000,
        payload: vec![1, 2, 3, 4, 5, 6, 7, 8],
    });
    assert!(matches!(
        component.recv(),
        Message::DecodeResponse { width: 2, height: 1, stride: 8, rgba, .. }
            if rgba == vec![1, 2, 3, 4, 5, 6, 7, 8]
    ));
    component.send(&Message::DecodeRequest {
        request: 3,
        format: 24,
        width: Some(1),
        height: Some(1),
        budget_bytes: 64,
        deadline_ms: 1_000,
        payload: vec![10, 20, 30],
    });
    assert!(matches!(
        component.recv(),
        Message::DecodeResponse { rgba, .. } if rgba == vec![10, 20, 30, 0xFF]
    ));
    component.shutdown();
}

#[test]
fn malformed_image_is_typed_and_connection_survives() {
    let mut component = Component::start();
    component.send(&decode_request(4, 100, b"not a png"));
    assert!(matches!(
        component.expect_error(ErrorKind::MalformedImage),
        Message::Error { request: 4, .. }
    ));
    component.send(&decode_request(5, 100, PNG_1X1_RGBA_RED));
    assert!(matches!(
        component.recv(),
        Message::DecodeResponse { request: 5, .. }
    ));
    component.shutdown();
}

#[test]
fn over_cap_dimensions_are_refused_without_killing_the_connection() {
    let mut component = Component::start();
    component.send(&Message::DecodeRequest {
        request: 6,
        format: 32,
        width: Some(5000),
        height: Some(5000),
        budget_bytes: MAX_DECODED_BYTES,
        deadline_ms: 1_000,
        payload: vec![0; 8],
    });
    assert!(matches!(
        component.expect_error(ErrorKind::Budget),
        Message::Error { request: 6, .. }
    ));
    component.send(&decode_request(7, 100, PNG_1X1_RGBA_RED));
    assert!(matches!(
        component.recv(),
        Message::DecodeResponse { request: 7, .. }
    ));
    component.shutdown();
}

#[test]
fn safe_mode_handshake_is_deprived_but_shuts_down() {
    let mut component = Component::start_with_mode(HELLO_SAFE_MODE);
    assert_eq!(component.capabilities, 0);
    component.send(&decode_request(1, 100, PNG_1X1_RGBA_RED));
    assert!(matches!(
        component.expect_error(ErrorKind::Denied),
        Message::Error { request: 1, .. }
    ));
    component.shutdown();
}

#[test]
fn zero_length_frame_fails_closed() {
    let mut component = Component::spawn();
    component.send_raw(&[0x00, 0x00, 0x00, 0x00]);
    component.expect_connection_error();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn oversized_frame_fails_closed_from_the_header() {
    let mut component = Component::spawn();
    // The 4-byte length alone is larger than any request frame; the server
    // must refuse without waiting for the (never-sent) payload.
    component.send_raw(&u32::MAX.to_be_bytes());
    component.expect_connection_error();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn request_frame_just_over_the_cap_fails_closed() {
    let mut component = Component::spawn();
    let len = u32::try_from(MAX_REQUEST_FRAME_BYTES + 1).expect("cap fits u32");
    component.send_raw(&len.to_be_bytes());
    component.expect_connection_error();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn unknown_tag_fails_closed() {
    let mut component = Component::spawn();
    component.send_raw(&frame(&[0x7F]));
    component.expect_connection_error();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn truncated_payload_fails_closed() {
    let mut component = Component::spawn();
    // Header promises 8 bytes, then stdin ends after 1.
    component.send_raw(&[0x00, 0x00, 0x00, 0x08, 0x01]);
    component.drop_stdin();
    component.expect_connection_error();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn trailing_bytes_fail_closed() {
    let mut component = Component::spawn();
    let mut payload = encode_message(&Message::Hello {
        min: 1,
        max: 1,
        component: COMPONENT_NAME.to_owned(),
        version: "test-host".to_owned(),
        mode: HELLO_CAPABILITY_GRANTED,
    })
    .expect("encode hello");
    payload.push(0x00);
    component.send_raw(&frame(&payload));
    component.expect_connection_error();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn malformed_decoded_request_length_fails_closed() {
    let mut component = Component::spawn();
    // A hand-built DecodeRequest whose declared payload length (8) exceeds
    // the bytes actually present (4).
    let mut payload = vec![0x10];
    payload.extend_from_slice(&1_u64.to_be_bytes());
    payload.push(100);
    payload.push(0x00);
    payload.push(0x00);
    payload.extend_from_slice(&64_u32.to_be_bytes());
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&8_u32.to_be_bytes());
    payload.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47]);
    component.send_raw(&frame(&payload));
    component.expect_connection_error();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn decode_before_handshake_fails_closed() {
    let mut component = Component::spawn();
    component.send(&decode_request(1, 100, PNG_1X1_RGBA_RED));
    component.expect_connection_error();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_PROTOCOL));
}

#[test]
fn clean_eof_exits_zero_without_a_reply() {
    let mut component = Component::start();
    component.drop_stdin();
    assert_eq!(component.wait_code(), i32::from(SERVER_EXIT_OK));
    // The reader saw EOF; no queued message may remain.
    assert!(component.messages.try_recv().is_err());
}

#[test]
fn version_help_and_refusal_are_stable() {
    let version = Command::new(Component::binary())
        .arg("--version")
        .output()
        .expect("run --version");
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("bitty-graphics {COMPONENT_VERSION}")
    );

    let help = Command::new(Component::binary())
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stderr).contains("protocol v1"));

    let refused = Command::new(Component::binary())
        .arg("--unknown")
        .output()
        .expect("run with an unknown flag");
    assert_eq!(refused.status.code(), Some(i32::from(SERVER_EXIT_REFUSED)));
}

#[test]
fn codec_rejects_a_response_frame_above_the_limit_before_decoding() {
    // Host-side guard: a hostile component cannot make the host allocate a
    // frame larger than the advertised response cap.
    let header = u32::MAX.to_be_bytes();
    let mut reader = FrameReader::with_limit(&header[..], MAX_RESPONSE_FRAME_BYTES);
    assert!(reader.read_frame().is_err());
    assert_eq!(
        decode_message(&[0x00]),
        Err(bitty_graphics::component::protocol::WireError::UnknownTag(
            0
        ))
    );
}
