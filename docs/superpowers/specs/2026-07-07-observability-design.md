# Observability (logging + request tracing) — Design Spec

**Date:** 2026-07-07
**Status:** Approved design (pre-implementation)

## 1. Problem & goal

There is no visibility into the connection between the browser UI and the backend, nor consistent
diagnostic logging for the CLI. When something misbehaves (a slow endpoint, a request that 500s, a scan that
errors), the only signal is the HTTP status in the browser or a `println!` line.

**Goal:** structured logging + per-request tracing for the UI↔API connection (and light, consistent CLI
tracing), using the standard Rust `tracing` stack. Each web request produces a span with fields and
received/sent events (status + latency); a request id correlates any deeper log lines back to the request.

## 2. Constraints

- **Console (stderr) only.** Nothing is written to disk; nothing leaves the machine. The server is local,
  single-user, `127.0.0.1`.
- **User-facing output is unchanged.** Existing `println!` (scan summaries, "Quarantined N files", the browse
  URL, etc.) is program *output* and stays exactly as is. The tracing layer emits diagnostic *logs* separately.
- **Additive and non-breaking:** the tracing layers and subscriber init are pure side-effects; if logging fails
  it never affects request handling or a command's result. All existing tests keep passing.
- **Paths and query params are logged in full** (e.g. `/api/search?q=thesis`) — most useful for debugging on a
  local single-user machine; no redaction.

## 3. Architecture

New `src/observability.rs` owns `init()`, which installs a global `tracing` subscriber: an `EnvFilter`
(driven by `RUST_LOG`, default `info`) plus a compact `tracing_subscriber::fmt` layer writing to **stderr**.
`init()` is idempotent — it uses `try_init` and is a silent no-op if a subscriber is already installed (safe
for tests and for `browse`, which shares the CLI init).

Both entry points call it once at startup:
- **CLI** (`src/main.rs`) calls `observability::init(verbose)` before dispatching a command.
- **Web** (`web::serve`) runs under the same subscriber and additionally wraps the axum router in
  `tower_http::trace::TraceLayer`.

New dependencies: `tracing`, `tracing-subscriber` (features `env-filter`, `fmt`), and `tower-http` with the
`trace` feature (version-matched to axum 0.7 — `tower-http` 0.6.x pairs with axum 0.7 / http 1).

## 4. Per-request web span

The router is wrapped in a `TraceLayer` (via `build_router_with`, so tests and production share it) configured
so each request runs inside a span named `request` carrying:
- **`id`** — a short per-request id from a process-global `AtomicU64` counter (monotonic, distinct), formatted
  compactly, so every log line from that request is correlatable.
- **`method`** — the HTTP method.
- **`uri`** — the full path + query string.

Within that span the layer emits:
- **on_request** → "started processing request" at **DEBUG** (quiet unless opted in).
- **on_response** → "finished processing request" at **INFO** with **status** and **latency**.

Because handler logs nest under the current span, any `tracing::warn!`/`error!` a handler emits mid-request
prints with the same `id`. The mutating/fallible handlers (`api_quarantine`, `api_repack`, `api_scan`,
`api_pick_folder`, and the read handlers' `err500` path) additionally emit a `tracing::warn!`/`error!` with the
failure reason, so failures appear in the trace, not only as an HTTP status.

Example:
```
INFO request{id=42 method=POST uri=/api/quarantine}: started processing request
WARN request{id=42 method=POST uri=/api/quarantine}: quarantine skipped: no surviving copy
INFO request{id=42 method=POST uri=/api/quarantine}: finished 200 in 42ms
```

## 5. CLI tracing + verbosity

- **CLI tracing:** after `init()`, the CLI runs inside the same tracing system. Existing `println!` user output
  stays. Meaningful internal steps gain `tracing` events without changing normal output — e.g. `run_scan`
  emits `info!` on volume resolution and `debug!` for the snapshot; the quarantine/repack/purge engines emit an
  `info!` per action (mirroring what already goes to `actions_log`, but visible live). Each CLI command runs
  inside a small span (e.g. `command{name=scan}`) so its events are grouped.
- **Verbosity:** controlled by `RUST_LOG` the standard way (e.g. `RUST_LOG=debug`), defaulting to `info` when
  unset. A global `-v/--verbose` CLI flag is a convenience that bumps the default to `debug`; `RUST_LOG`, if
  set, always wins over the flag.

## 6. Error handling

`init()` uses `try_init`, never panics on a double-init (silent no-op). The tracing layers are side-effects
only; a logging failure never affects request handling or a command's result.

## 7. Testing

Tests install a *local* capturing subscriber (in-memory buffer, scoped to the test via
`tracing::subscriber::with_default`) and assert on it:
- A web test builds the router **with** the `TraceLayer`, sends a `oneshot` request under a captured
  subscriber, and asserts the buffer contains a completion line with the method, the `200` status, and a
  request-`id` field.
- `observability::init()` is idempotent — a second call does not panic/error (unit test).
- The request-id generator is monotonic and produces distinct ids (unit test).
- Existing tests are unaffected — the layer is additive; the subscriber init is a no-op when one is already set.

## 8. File structure

- `Cargo.toml` — add `tracing`, `tracing-subscriber` (env-filter, fmt), `tower-http` (trace).
- `src/observability.rs` — **new**: `init(verbose)`, the request-id counter, and a `make_request_span` helper.
- `src/lib.rs` — `pub mod observability;`.
- `src/main.rs` — global `-v/--verbose` flag; call `observability::init(verbose)` before dispatch.
- `src/web.rs` — `build_router_with` applies the `TraceLayer`; mutating/error handlers emit `warn!`/`error!`.
- `src/scanner.rs` / `src/quarantine.rs` / `src/repack.rs` / `src/purge.rs` — a few `info!`/`debug!` events at
  the meaningful action sites (light).

## 9. Out of scope (deferred)

- Log files on disk / rotation / retention (console only for now).
- A log-viewer panel in the web UI (surfacing logs in the browser).
- Metrics/telemetry export (Prometheus, OpenTelemetry).
- Redaction modes (full logging chosen for a local single-user tool).
- Per-request timing histograms / percentiles.
