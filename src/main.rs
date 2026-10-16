//! `bitty-graphics` executable: component binary packed into the R2
//! prebuilt distribution (`extensions/graphics/<version>/<target>.tar.gz`,
//! CTX-0008 pilot).
//!
//! Pilot scope: reports `--version` and refuses anything else. The stdio
//! serve loop lands with the Core-owned host binding (graphics#14
//! follow-up); until then a bare invocation fails closed instead of
//! pretending to serve the wire protocol.

#![forbid(unsafe_code)]

use std::process::ExitCode;

/// Component version (tracks `Cargo.toml`; the release workflow refuses to
/// publish when the tag disagrees with it).
pub const COMPONENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit code for a refused invocation. Mirrors the `bitty-net`
/// `EXIT_PROTOCOL` pattern: the operator asked for something this binary
/// does not serve.
pub const EXIT_REFUSED: u8 = 2;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (None, _) => {
            eprintln!(
                "bitty-graphics: component serve loop is not yet wired (Core binding pending); refusing to serve"
            );
            ExitCode::from(EXIT_REFUSED)
        }
        (Some(flag), None) if flag == "--version" || flag == "-V" => {
            println!("bitty-graphics {COMPONENT_VERSION}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: bitty-graphics [--version]");
            ExitCode::from(EXIT_REFUSED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{COMPONENT_VERSION, EXIT_REFUSED};

    #[test]
    fn version_tracks_package() {
        assert_eq!(COMPONENT_VERSION, env!("CARGO_PKG_VERSION"));
        assert!(!COMPONENT_VERSION.is_empty());
    }

    #[test]
    fn refusal_code_is_nonzero() {
        assert_ne!(EXIT_REFUSED, 0);
    }
}
