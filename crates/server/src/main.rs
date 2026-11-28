//! `constellation-server --bundle <path> --port <port>`: one room, compatible with Archipelago
//! clients. See the `server` library for the parts.
#![allow(
    unused_crate_dependencies,
    reason = "the library uses the package's dependencies; this is a thin entry point"
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    // The child side of a manifest fill or check: no logging, no runtime, one job from stdin.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("fill-job") {
        return match server::fill_job(&args.next().unwrap_or_default()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    serve()
}

#[tokio::main]
async fn serve() -> ExitCode {
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
    let generation = match config::load_generation() {
        Ok(g) => g,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(2);
        }
    };
    let limits = match config::load() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(2);
        }
    };
    match server::run(args, limits, secrets, generation).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::from(e.exit_code())
        }
    }
}
