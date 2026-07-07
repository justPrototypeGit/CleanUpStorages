# Observability (logging + request tracing) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add structured `tracing` logging with a per-request span (id + method + uri + status + latency) for the web API, plus light consistent CLI tracing — console (stderr) only, level-controlled by `RUST_LOG`/`-v`.

**Architecture:** A new `src/observability.rs` installs one global `tracing` subscriber (`EnvFilter` + `fmt` to stderr, idempotent `try_init`). `web::build_router_with` wraps the router in `tower_http::trace::TraceLayer` whose span carries a per-request id/method/uri; handler failures emit `warn!`/`error!` nested under that span. The CLI calls the same `init` and adds a command span + a few action events.

**Tech Stack:** Rust 1.88, existing deps, plus `tracing`, `tracing-subscriber` (env-filter + fmt), `tower-http` (trace feature, version-matched to axum 0.7 / http 1 → tower-http 0.6.x).

## Global Constraints

- **Console (stderr) only** — nothing to disk, nothing off-machine.
- **User-facing `println!` output is unchanged** — tracing logs are separate diagnostics; do NOT convert existing prints.
- **Additive / non-breaking** — the tracing layer + subscriber init are pure side-effects; with no subscriber set, tracing is a no-op, so all existing tests keep passing unchanged. `init` never panics on double-init.
- **Full paths + query params logged** (e.g. `/api/search?q=thesis`) — no redaction (local single-user tool).
- **Verbosity:** `RUST_LOG` (default `info` when unset) controls levels; `-v/--verbose` bumps the default to `debug`; `RUST_LOG` wins when set.
- **Git:** branch `feat/observability` off `main`. Conventional Commits, scopes `web`/`cli`/`scanner`. Each commit ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

**Depends on (merged):** the axum web server (`build_router_with`, `serve`, `AppState`, the handlers, `err500`), `scanner::run_scan`, the quarantine/repack/purge engines, `src/main.rs` (clap `Cli`). **Out of scope:** log files/rotation; a UI log panel; metrics export; redaction modes.

---

## File Structure

- `Cargo.toml` — add `tracing`, `tracing-subscriber` (features `env-filter`), `tower-http` (feature `trace`).
- `src/observability.rs` — **new**: `init(verbose)`, `next_request_id()`, `make_request_span(&Request)`. Registered in `lib.rs`.
- `src/lib.rs` — `pub mod observability;`.
- `src/main.rs` — global `-v/--verbose` flag; `observability::init(verbose)` before dispatch; a `command` span.
- `src/web.rs` — `build_router_with` applies `TraceLayer`; `err500` logs `error!`; CSRF-403 sites log `warn!`.
- `src/scanner.rs` — an `info!` in `run_scan` at volume resolution.

---

### Task 1: Observability module + subscriber init + CLI `-v` flag

**Files:**
- Modify: `Cargo.toml`
- Create: `src/observability.rs`
- Modify: `src/lib.rs`, `src/main.rs`
- Test: inline `#[cfg(test)]` in `src/observability.rs`

**Interfaces:**
- Produces:
  - `pub fn init(verbose: bool)` — install the global subscriber (idempotent; `RUST_LOG` wins, else `debug` if `verbose` else `info`).
  - `pub fn next_request_id() -> u64` — process-global monotonic counter (starts at 1).
  - `pub fn make_request_span(req: &axum::http::Request<axum::body::Body>) -> tracing::Span` — an INFO span `request` with fields `id`, `method`, `uri`.

- [ ] **Step 1: Add dependencies**

In `Cargo.toml` `[dependencies]`:

```toml
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tower-http = { version = "0.6", features = ["trace"] }
```

(If `tower-http` 0.6 fails to resolve against the locked axum 0.7.x, use the version cargo selects for axum 0.7's `http = 1` — adapt the minor and note it; do NOT downgrade axum.)

- [ ] **Step 2: Write failing tests**

Create `src/observability.rs`:

```rust
//! Console (stderr) tracing: one global subscriber + a per-request span helper.

use std::sync::atomic::{AtomicU64, Ordering};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// A distinct, monotonically increasing id for correlating a request's log lines.
pub fn next_request_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::Relaxed)
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
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test --lib observability`
Expected: FAIL — `init` not found.

- [ ] **Step 4: Implement `init` + `make_request_span`; register module**

Add to `src/observability.rs`:

```rust
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
```

Add `pub mod observability;` to `src/lib.rs`.

- [ ] **Step 5: Add the `-v` flag + init call in `main.rs`**

In `src/main.rs`, add the flag to `Cli` and init before dispatch:

```rust
#[derive(Parser)]
#[command(name = "cleanupstorages", version, about)]
struct Cli {
    /// Verbose logging (debug level). RUST_LOG, if set, overrides this.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}
```

And at the top of `fn main`:

```rust
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    cleanupstorages::observability::init(cli.verbose);
    match cli.command {
        // ... unchanged ...
    }
}
```

- [ ] **Step 6: Run tests + build**

Run: `cargo test --lib observability` then `cargo build`
Expected: PASS; builds (first build pulls tracing/tower-http).

- [ ] **Step 7: Commit**

```bash
git checkout -b feat/observability   # only if not already on it
git add Cargo.toml Cargo.lock src/observability.rs src/lib.rs src/main.rs
git commit -m "feat(cli): tracing subscriber init, request-id, -v flag

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Per-request TraceLayer on the router

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `build_router_with(state)` applies `tower_http::trace::TraceLayer` using `observability::make_request_span`, an on_request DEBUG event and an on_response INFO event with status + millisecond latency. No signature change.

- [ ] **Step 1: Write the failing test (capturing subscriber)**

Add to `web.rs` `mod tests` (a `CaptureWriter` + a test that a request produces a completion line with method/status/id):

```rust
    #[derive(Clone)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf); Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer { self.clone() }
    }

    #[tokio::test]
    async fn request_is_traced_with_method_status_and_id() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, db) = seed_catalog();
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(CaptureWriter(buf.clone()))
            .finish();
        let _guard = tracing::subscriber::set_default(sub); // held across the await (current-thread test)

        let app = build_router(db.clone());
        let res = app.oneshot(Request::builder().uri("/api/search?q=thesis").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("GET"), "log: {logged}");
        assert!(logged.contains("200"), "log: {logged}");
        assert!(logged.contains("id="), "request-id field present: {logged}");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib web::tests::request_is_traced_with_method_status_and_id`
Expected: FAIL — no layer, so nothing is logged (buffer empty; assertions fail).

- [ ] **Step 3: Apply the TraceLayer**

In `src/web.rs`, add imports and wrap the router in `build_router_with`. Add near the top:

```rust
use tower_http::trace::{TraceLayer, DefaultOnRequest, DefaultOnResponse};
use tower_http::LatencyUnit;
```

In `build_router_with`, apply the layer to the `Router` before `.with_state(state)`:

```rust
pub fn build_router_with(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        // ... all existing routes unchanged ...
        .route("/scan", get(scan_page))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<axum::body::Body>| {
                    crate::observability::make_request_span(req)
                })
                .on_request(DefaultOnRequest::new().level(tracing::Level::DEBUG))
                .on_response(
                    DefaultOnResponse::new()
                        .level(tracing::Level::INFO)
                        .latency_unit(LatencyUnit::Millis),
                ),
        )
        .with_state(state)
}
```

(Keep the exact existing `.route(...)` lines; only add the `.layer(...)` before `.with_state(state)`.)

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS — the trace test passes (buffer contains the GET/200/id line); all existing tests still pass (the layer is a no-op when no subscriber is set).

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): per-request TraceLayer span (id, method, uri, status, latency)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Handler failure logging (err500 + CSRF rejections)

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `err500` logs a `tracing::error!` with the error before returning the 500 tuple.
- Each CSRF token-rejection site (`api_quarantine`, `api_repack`, `api_scan`, `api_pick_folder`) logs a `tracing::warn!` before returning 403.

- [ ] **Step 1: Write the failing test**

Add to `web.rs` `mod tests` (reuses `CaptureWriter` from Task 2; a CSRF-403 request must log a warn):

```rust
    #[tokio::test]
    async fn csrf_rejection_is_logged() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(CaptureWriter(buf.clone()))
            .finish();
        let _guard = tracing::subscriber::set_default(sub);

        let app = build_router_with(state);
        // POST /api/quarantine with NO token -> 403 and a warn line
        let res = app.oneshot(Request::builder().method("POST").uri("/api/quarantine")
            .header("content-type", "application/json")
            .body(Body::from("{\"quarantine_ids\":[1]}")).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);

        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("WARN"), "expected a warn line: {logged}");
        assert!(logged.to_lowercase().contains("token"), "reason mentions token: {logged}");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib web::tests::csrf_rejection_is_logged`
Expected: FAIL — no warn is emitted yet.

- [ ] **Step 3: Implement the logging**

In `src/web.rs`:

1. `err500` — add an error log:

```rust
fn err500<E: std::fmt::Display>(e: E) -> (axum::http::StatusCode, String) {
    tracing::error!(error = %e, "request failed");
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
```

2. At each CSRF token check (in `api_quarantine`, `api_repack`, `api_scan`, `api_pick_folder`), add a `warn!` in the reject branch. The current pattern is:

```rust
    if !ok { return Err((StatusCode::FORBIDDEN, "missing or bad token".into())); }
```

Change each to:

```rust
    if !ok {
        tracing::warn!("rejected request: missing or bad CSRF token");
        return Err((StatusCode::FORBIDDEN, "missing or bad token".into()));
    }
```

(Apply to all four handlers that have this check.)

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS (the CSRF-403 warn test passes; all existing tests still pass).

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): log request failures (err500 error) and CSRF rejections (warn)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: CLI command span + a scan-level trace event

**Files:**
- Modify: `src/main.rs` (command span), `src/scanner.rs` (info event)
- Test: inline `#[cfg(test)]` in `src/scanner.rs`

**Interfaces:**
- `main` wraps command dispatch in an INFO span `command` with a `name` field.
- `run_scan` emits `tracing::info!` on a successful volume resolution (volume id + label).

- [ ] **Step 1: Write the failing test**

Add to `src/scanner.rs` `mod tests` (a capturing subscriber asserts `run_scan` logs an info event):

```rust
    #[derive(Clone)]
    struct CaptureW(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureW {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().extend_from_slice(b); Ok(b.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureW {
        type Writer = CaptureW;
        fn make_writer(&'a self) -> Self::Writer { self.clone() }
    }

    #[test]
    fn run_scan_logs_volume_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.txt"), b"hi").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();

        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(CaptureW(buf.clone())).finish();
        let _guard = tracing::subscriber::set_default(sub);

        run_scan(&cat, &root, false, crate::volume::ReadonlyMode::Fingerprint, 100, None).unwrap();
        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.to_lowercase().contains("volume"), "expected a volume info line: {logged}");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib scanner::tests::run_scan_logs_volume_resolution`
Expected: FAIL — no info event yet.

- [ ] **Step 3: Implement**

1. In `src/scanner.rs` `run_scan`, after the volume is resolved and upserted (right before/after `upsert_volume`), add:

```rust
    tracing::info!(volume = %identity.volume_id, label = %identity.label,
        identified_by = %identity.identified_by, "scanning volume");
```

(Place it after `let identity = ...` resolves to `Some` and before the scan runs.)

2. In `src/main.rs`, wrap the dispatch in a `command` span. Give each arm a name. Simplest: create the span from the parsed command before the match:

```rust
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    cleanupstorages::observability::init(cli.verbose);
    let name = match &cli.command {
        Command::Scan { .. } => "scan",
        Command::Search { .. } => "search",
        Command::Status => "status",
        Command::Browse { .. } => "browse",
        Command::Duplicates => "duplicates",
        Command::Quarantine { .. } => "quarantine",
        Command::Purge { .. } => "purge",
        Command::Repack { .. } => "repack",
    };
    let _span = tracing::info_span!("command", name).entered();
    match cli.command {
        // ... unchanged dispatch ...
    }
}
```

(Match the exact `Command` variant names in the current `main.rs`. `.entered()` keeps the span active for the whole command.)

- [ ] **Step 4: Run tests + full suite + release build**

Run: `cargo test --lib scanner` then `cargo test` then `cargo build --release`
Expected: PASS; release builds.

- [ ] **Step 5: Manual smoke**

Run:
```bash
cargo run -- status
RUST_LOG=info cargo run -- browse --no-open   # then curl an endpoint in another shell, observe the request line, Ctrl+C
```
Expected: `status` runs (a `command{name=status}` span may show at info); with `browse`, hitting an endpoint prints a `request{...}: finished 200 in Nms` line to stderr. Report what you observed.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/scanner.rs
git commit -m "feat(cli): command span + scan volume-resolution trace event

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- `tracing` + `tracing-subscriber` global subscriber, stderr, EnvFilter (§3) → Task 1 ✓
- `init` idempotent via `try_init` (§3, §6) → Task 1 ✓
- CLI `-v/--verbose` + `RUST_LOG` default info/debug (§5) → Task 1 ✓
- Per-request span `request` with id/method/uri; on_request DEBUG, on_response INFO + status + latency (§4) → Task 2 ✓
- Request-id counter, monotonic/distinct (§4) → Task 1 ✓
- Handler failures emit warn/error nested under the span (§4) → Task 3 (err500 error, CSRF warn) ✓
- CLI command span + light action events (§5) → Task 4 (command span, run_scan info) ✓
- Console only, user prints unchanged, additive/non-breaking (§2) → all tasks (layer/init are no-ops without a subscriber; no prints touched) ✓
- Testing via a local capturing subscriber (§7) → Tasks 1 (idempotent/id), 2 (request line), 3 (csrf warn), 4 (scan info) ✓
- `tower-http` trace feature, axum-0.7-matched (§8) → Task 1 ✓

**Placeholder scan:** No TBD/TODO; every step has runnable code + commands. The one conditional (Task 1 tower-http version) gives a concrete fallback rule + a compile check.

**Type consistency:** `init(verbose: bool)`, `next_request_id() -> u64`, `make_request_span(&Request<Body>) -> Span` consistent across Task 1 (def), Task 2 (used in TraceLayer). `CaptureWriter`/`CaptureW` `MakeWriter` impl identical shape in Tasks 2/4. `err500` signature unchanged (Task 3 only adds a log line). `Command` variant names in Task 4 must match `src/main.rs` (implementer copies the exact arms).

**Deferred (logged):** log files/rotation; UI log panel; metrics; redaction — all §9, unchanged.
