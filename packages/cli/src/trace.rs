//! Optional tracing subscriber for local profiling diagnostics.

/// Initializes tracing when `TROWSER_TRACE=1` and the `profile-trace` feature is enabled.
#[cfg(feature = "profile-trace")]
pub fn init_trace() {
    use tracing_subscriber::{EnvFilter, fmt};

    let enabled = std::env::var("TROWSER_TRACE")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "on" | "yes"));

    if !enabled {
        return;
    }

    let filter = std::env::var("RUST_LOG")
        .ok()
        .map_or_else(|| EnvFilter::new("debug"), EnvFilter::new);

    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

/// No-op when the `profile-trace` feature is disabled.
#[cfg(not(feature = "profile-trace"))]
pub const fn init_trace() {}
