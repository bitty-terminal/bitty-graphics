//! `bitty-graphics` executable component entry point.
//!
//! A bare invocation serves the v1 stdio decode protocol
//! ([`bitty_graphics::component`]); `--version` reports the component version
//! used by the descriptor and release tag. The binary carries no installer,
//! reads no configuration, and never publishes itself.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use bitty_graphics::component::server::{COMPONENT_VERSION, SERVER_EXIT_REFUSED, serve_stdio};

fn usage() {
    eprintln!(
        "usage: bitty-graphics [--version | --help]\n\
         \n\
         With no arguments, serves the graphics component protocol v1 on\n\
         stdin/stdout. stderr carries diagnostics only."
    );
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (None, _) => ExitCode::from(serve_stdio()),
        (Some(flag), None) if flag == "--version" || flag == "-V" => {
            println!("bitty-graphics {COMPONENT_VERSION}");
            ExitCode::SUCCESS
        }
        (Some(flag), None) if flag == "--help" || flag == "-h" => {
            usage();
            ExitCode::SUCCESS
        }
        _ => {
            usage();
            ExitCode::from(SERVER_EXIT_REFUSED)
        }
    }
}
