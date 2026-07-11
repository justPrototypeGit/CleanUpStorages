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

/// Test-only: a process-wide lock serializing the handful of tests that install a `tracing`
/// subscriber via `set_default` and assert on its captured output. `tracing`'s callsite
/// interest cache is global, and installing (or tearing down) a subscriber rebuilds it
/// process-wide; letting two such tests overlap under the parallel test runner races that
/// rebuild and can drop the very event a test asserts on. Holding this guard for the test's
/// duration serializes only those tests — every other test still runs fully parallel.
/// Poison-tolerant: a test panicking while holding it must not cascade-fail the others.
#[cfg(test)]
pub(crate) fn tracing_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
