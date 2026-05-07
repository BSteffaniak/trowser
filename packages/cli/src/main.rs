#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match trowser_cli::run().await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("trowser error: {error:#}");
            ExitCode::from(1)
        }
    }
}
