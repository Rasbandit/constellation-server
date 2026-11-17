//! `constellation-server --bundle <path> --port <port>`: one room, compatible with Archipelago
//! clients. See the `server` library for the parts.
#![allow(
    unused_crate_dependencies,
    reason = "the library uses the package's dependencies; this is a thin entry point"
)]

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    server::init_logging();
    let args = match server::Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("{e}; {}", server::USAGE);
            return ExitCode::from(2);
        }
    };
    let secrets = match config::load_secrets() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(2);
        }
    };
    match server::run(args, config::load(), secrets).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::from(e.exit_code())
        }
    }
}
