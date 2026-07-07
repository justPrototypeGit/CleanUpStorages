//! Console (stderr) tracing: one global subscriber + a per-request span helper.

use std::sync::atomic::{AtomicU64, Ordering};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// A distinct, monotonically increasing id for correlating a request's log lines.
pub fn next_request_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// Install the global stderr tracing subscriber. Idempotent: a second call is a silent no-op
/// (so the CLI and `serve` can both call it, and tests never conflict). `RUST_LOG` wins when set;
/// otherwise the default level is `debug` when `verbose`, else `info`.
pub fn init(verbose: bool) {
    let default = if verbose { "debug" } else { "info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_target(false)
        .compact()
        .try_init();
}

/// The span every HTTP request runs inside; its fields appear on the request/response log lines.
pub fn make_request_span(req: &axum::http::Request<axum::body::Body>) -> tracing::Span {
    tracing::info_span!(
        "request",
        id = next_request_id(),
        method = %req.method(),
        uri = %req.uri(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_monotonic_and_distinct() {
        let a = next_request_id();
        let b = next_request_id();
        let c = next_request_id();
        assert!(b > a && c > b);
        assert_ne!(a, b);
    }

    #[test]
    fn init_is_idempotent() {
        // calling twice must not panic (try_init is a no-op the second time)
        init(false);
        init(true);
    }
}
