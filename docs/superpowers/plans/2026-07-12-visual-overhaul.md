# Visual Overhaul (Stitch fidelity) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A polished, Stitch-like macOS look across all six pages, with an Auto/Light/Dark theme toggle, themed dropdowns, and a rebuilt scan live-status panel.

**Architecture:** All work is in `src/web_ui.rs` (the shared `STYLE`, `SHARED_JS`, `shell`, `icon`, and the six page functions). Presentation-only — no endpoint/handler/behavior change. CSS-variable design tokens gain explicit `[data-theme]` overrides so an in-app toggle can force a theme.

**Tech Stack:** self-contained HTML/CSS/JS (no external requests, no web/icon fonts), system font stack, hand-authored inline SVG icons.

## Global Constraints

- **Self-contained:** no `http(s)://` in any rendered page. No CDN/fonts/icon-fonts. Every page keeps passing that assertion.
- **Behavior-preserving:** every page still fetches the same endpoints and keeps the DOM ids/hooks its script and tests rely on (`id="q"`, `id="results"`, `id="drives"`, `id="running"`, `/api/*` strings, etc.). This is a restyle — do not change what any page *does*.
- **XSS-safe:** all DB-derived text via `esc()`/`textContent`.
- **No SHARED_JS const collision** in page scripts.
- **Conventional Commits**, scope `web`; body ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- **Windows/PowerShell dev env:** `cargo build`, `cargo test`. Run `cargo test -p cleanupstorages --lib web` while iterating; full suite once before each commit. After each task confirm `grep -c "http://\|https://" src/web_ui.rs` is 0.

## Task order
1 (theme tokens + toggle + form controls) → 2 (component/shell/icon refresh) → 3 (scan panel) → 4 (Overview + Drives).

---

### Task 1: Theme system — tokens, Auto/Light/Dark toggle, themed form controls

**Files:** Modify `src/web_ui.rs` (`STYLE`, `SHARED_JS`, `shell`)

**Interfaces:** the page shell gains a working theme toggle (persisted in `localStorage.theme`, applied before paint via `data-theme` on `<html>`); `select`/`input`/`option` follow the theme.

- [ ] **Step 1: Replace the token block at the top of `STYLE`.** Replace the current `:root{…}` + `@media (prefers-color-scheme:dark){…}` lines (the first ~6 lines of `STYLE`) with the four-variant token architecture below. The default `:root` and `:root[data-theme="light"]` hold the light values; `@media(dark)` (Auto) and `:root[data-theme="dark"]` hold the dark values. The attribute selectors win over `@media` (higher specificity, outside the media query), so a forced theme always overrides the OS.

```css
:root{color-scheme:light dark;
 --bg:#ececee;--content:#ffffff;--elev:#ffffff;--fg:#1d1d1f;--mut:#6e6e73;
 --line:#00000014;--line-strong:#00000022;--accent:#0071e3;--accent-weak:#0071e314;
 --amber:#b25000;--amber-bg:#ff9f0a24;--red:#c9382b;--red-bg:#ff453a1f;
 --green:#1a7f37;--green-bg:#30d15824;--gray:#8a8a8e;
 --r-sm:8px;--r:12px;--r-lg:16px;
 --sh-sm:0 1px 2px #0000000f,0 1px 1px #0000000a;
 --sh-md:0 6px 18px #00000016,0 1px 3px #0000000f;--sh-lg:0 14px 36px #0000001f;
 --sidebar:248px;--topbar:56px;}
:root[data-theme="light"]{
 --bg:#ececee;--content:#ffffff;--elev:#ffffff;--fg:#1d1d1f;--mut:#6e6e73;
 --line:#00000014;--line-strong:#00000022;--accent:#0071e3;--accent-weak:#0071e314;
 --amber:#b25000;--amber-bg:#ff9f0a24;--red:#c9382b;--red-bg:#ff453a1f;
 --green:#1a7f37;--green-bg:#30d15824;--gray:#8a8a8e;}
@media (prefers-color-scheme:dark){:root{
 --bg:#161618;--content:#1f1f22;--elev:#2a2a2e;--fg:#f5f5f7;--mut:#9a9aa0;
 --line:#ffffff14;--line-strong:#ffffff28;--accent:#0a84ff;--accent-weak:#0a84ff26;
 --amber:#ff9f0a;--amber-bg:#ff9f0a26;--red:#ff453a;--red-bg:#ff453a26;
 --green:#30d158;--green-bg:#30d15826;--gray:#98989d;}}
:root[data-theme="dark"]{
 --bg:#161618;--content:#1f1f22;--elev:#2a2a2e;--fg:#f5f5f7;--mut:#9a9aa0;
 --line:#ffffff14;--line-strong:#ffffff28;--accent:#0a84ff;--accent-weak:#0a84ff26;
 --amber:#ff9f0a;--amber-bg:#ff9f0a26;--red:#ff453a;--red-bg:#ff453a26;
 --green:#30d158;--green-bg:#30d15826;--gray:#98989d;}
```

- [ ] **Step 2: Add form-control + theme-toggle CSS** to the end of `STYLE` (before the closing `"##`):

```css
select,input[type=text],input[type=search],textarea{background:var(--content);color:var(--fg);
 border:1px solid var(--line-strong);border-radius:var(--r-sm);padding:8px 10px;font:inherit;color-scheme:inherit;}
select:focus,input:focus,textarea:focus{outline:none;border-color:var(--accent);
 box-shadow:0 0 0 3px var(--accent-weak);}
option{background:var(--content);color:var(--fg);}
.seg{display:inline-flex;background:var(--line);border-radius:999px;padding:2px;gap:2px;}
.seg button{border:0;background:transparent;color:var(--mut);border-radius:999px;padding:4px 8px;
 cursor:pointer;display:flex;align-items:center;gap:4px;font:inherit;font-size:12px;}
.seg button.on{background:var(--content);color:var(--fg);box-shadow:var(--sh-sm);}
.seg button svg{width:14px;height:14px;}
.themebar{margin-top:auto;padding-top:14px;border-top:1px solid var(--line);display:flex;
 align-items:center;justify-content:space-between;gap:8px;}
.themebar .lbl{font-size:12px;color:var(--mut);}
```

- [ ] **Step 3: Add the theme JS** to the end of `SHARED_JS` (before the closing `"##`):

```js
function applyTheme(t){ if(t==='auto'){localStorage.removeItem('theme');delete document.documentElement.dataset.theme;}
  else{localStorage.setItem('theme',t);document.documentElement.dataset.theme=t;}
  for(const b of document.querySelectorAll('.themebar .seg button')) b.classList.toggle('on', b.dataset.theme===(t||'auto')); }
(function initTheme(){ const cur=localStorage.getItem('theme')||'auto';
  document.addEventListener('DOMContentLoaded',()=>{
    for(const b of document.querySelectorAll('.themebar .seg button')){ b.onclick=()=>applyTheme(b.dataset.theme); }
    applyTheme(cur);
  });})();
```

- [ ] **Step 4: Pre-paint theme + the toggle control in `shell`.** In `shell()`, add a tiny inline script at the very start of `<head>` (before `<style>`) so the saved theme is applied before first paint (no flash):

```html
<script>(function(){var t=localStorage.getItem('theme');if(t&&t!=='auto')document.documentElement.dataset.theme=t;})();</script>
```

And add a theme control to the sidebar. Change the `<aside>` in `shell` from
`<aside class="side"><h1>CleanUpStorages</h1><p class="tagline">Storage cleanup</p><nav>{nav}</nav></aside>`
to include a flex column + the theme bar (the `{icons}` are three inline SVGs — reuse `icon("auto")`, `icon("light")`, `icon("dark")`, added in Task 2; for THIS task use simple text labels A / ☀ / ☾ or the placeholder glyphs, and Task 2 swaps in the SVGs):

```rust
    let themebar = r##"<div class="themebar"><span class="lbl">Theme</span>
<div class="seg" role="group" aria-label="Theme">
<button data-theme="auto" title="Follow system">Auto</button>
<button data-theme="light" title="Light">Light</button>
<button data-theme="dark" title="Dark">Dark</button></div></div>"##;
```

Make the sidebar a flex column so the theme bar pins to the bottom: give `<aside class="side">` its content in `display:flex;flex-direction:column` (add to the `aside.side` rule in STYLE: `display:flex;flex-direction:column;`) and append `{themebar}` after `<nav>{nav}</nav>`.

- [ ] **Step 5: Add the page test** (in `src/web.rs` tests) — a small check that the shell renders the toggle and stays self-contained. Extend an existing page test (e.g. `root_is_overview_and_self_contained`) or add:

```rust
    #[tokio::test]
    async fn shell_has_theme_toggle() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains(r#"data-theme="dark""#) && body.contains("themebar"), "theme toggle present");
        assert!(body.contains("applyTheme"), "theme JS present");
        assert!(!body.contains("http://") && !body.contains("https://"), "self-contained");
    }
```

- [ ] **Step 6: Build + test.** `cargo build`; `cargo test -p cleanupstorages --lib web`; confirm `grep -c "http://\|https://" src/web_ui.rs` == 0.

- [ ] **Step 7: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "feat(web): Auto/Light/Dark theme toggle + themed form controls"
```

---

### Task 2: Core design-system refresh — components, shell, richer icons

**Files:** Modify `src/web_ui.rs` (`STYLE` component rules, `shell` shell markup, `icon`)

**Interfaces:** unchanged class names/DOM structure; purely a visual refresh so every page (which uses `.card`/`.btn`/`.pill`/tree/etc.) looks better. Adds new icon keys used by later tasks.

- [ ] **Step 1: Rework the component section of `STYLE`** (everything after the token/theme/form blocks from Task 1). Replace the existing component rules with this refined set (keep the Task 1 token + form + `.seg`/`.themebar` blocks intact; this replaces the `*{box-sizing…}` line through the end):

```css
*{box-sizing:border-box;}
body{margin:0;font:14px/1.5 -apple-system,"Segoe UI",Roboto,system-ui,sans-serif;
 background:var(--bg);color:var(--fg);-webkit-font-smoothing:antialiased;}
.mono{font-family:ui-monospace,"Cascadia Code","SF Mono",Consolas,monospace;font-variant-numeric:tabular-nums;}
h1,h2,h3{letter-spacing:-.01em;}
aside.side{position:fixed;left:0;top:0;bottom:0;width:var(--sidebar);display:flex;flex-direction:column;
 background:var(--panel,color-mix(in srgb,var(--content) 78%,transparent));
 backdrop-filter:blur(24px) saturate(180%);-webkit-backdrop-filter:blur(24px) saturate(180%);
 border-right:1px solid var(--line);padding:18px 12px;overflow-y:auto;}
aside.side h1{font-size:16px;margin:2px 8px 0;font-weight:650;}
aside.side .tagline{margin:0 8px 18px;font-size:11px;color:var(--mut);text-transform:uppercase;letter-spacing:.06em;}
nav a{display:flex;gap:11px;align-items:center;padding:8px 10px;margin:1px 0;border-radius:var(--r-sm);
 color:var(--mut);text-decoration:none;font-weight:500;font-size:13.5px;transition:background .12s,color .12s;}
nav a.active{background:var(--accent-weak);color:var(--accent);}
nav a:hover:not(.active){background:var(--line);color:var(--fg);}
nav a svg{width:18px;height:18px;flex:none;}
header.top{position:fixed;top:0;left:var(--sidebar);right:0;height:var(--topbar);display:flex;align-items:center;
 gap:12px;padding:0 24px;background:color-mix(in srgb,var(--bg) 72%,transparent);
 backdrop-filter:blur(24px) saturate(180%);-webkit-backdrop-filter:blur(24px) saturate(180%);
 border-bottom:1px solid var(--line);z-index:5;}
header.top strong{font-size:15px;font-weight:600;letter-spacing:-.01em;}
header.top .spacer{flex:1;}
main{margin-left:var(--sidebar);padding:calc(var(--topbar) + 24px) 28px 48px;max-width:1120px;}
.card{background:var(--content);border:1px solid var(--line);border-radius:var(--r-lg);padding:20px;
 margin:0 0 16px;box-shadow:var(--sh-sm);}
.card.hover{transition:box-shadow .16s,transform .16s,border-color .16s;}
.card.hover:hover{box-shadow:var(--sh-md);transform:translateY(-1px);border-color:var(--line-strong);}
.grid{display:grid;grid-template-columns:repeat(12,1fr);gap:16px;}
.mut{color:var(--mut);} .row{display:flex;align-items:center;gap:8px;}
.btn{font:inherit;font-weight:500;padding:8px 14px;border-radius:var(--r-sm);border:1px solid var(--line-strong);
 background:var(--content);color:var(--fg);cursor:pointer;display:inline-flex;align-items:center;gap:7px;
 transition:background .12s,box-shadow .12s,transform .06s;box-shadow:var(--sh-sm);}
.btn:hover{background:var(--line);} .btn:active{transform:translateY(.5px);}
.btn svg{width:15px;height:15px;}
.btn-primary{background:linear-gradient(var(--accent),color-mix(in srgb,var(--accent) 88%,#000));
 border-color:transparent;color:#fff;box-shadow:inset 0 1px 0 #ffffff30,var(--sh-sm);}
.btn-primary:hover{filter:brightness(1.05);background:var(--accent);}
.btn-danger{background:var(--content);border-color:color-mix(in srgb,var(--red) 45%,var(--line-strong));color:var(--red);}
.btn-danger:hover{background:var(--red-bg);}
.icon-btn{border:0;background:transparent;color:var(--mut);border-radius:999px;width:30px;height:30px;
 display:inline-flex;align-items:center;justify-content:center;cursor:pointer;box-shadow:none;padding:0;}
.icon-btn:hover{background:var(--line);color:var(--fg);}
.pill{font-size:11px;padding:2px 9px;border-radius:999px;font-weight:600;letter-spacing:.01em;
 display:inline-flex;align-items:center;gap:5px;}
.pill::before{content:"";width:6px;height:6px;border-radius:50%;background:currentColor;}
.pill.quarantined{color:var(--amber);background:var(--amber-bg);}
.pill.missing{color:var(--red);background:var(--red-bg);}
.pill.active{color:var(--green);background:var(--green-bg);}
.pill.purged,.pill.offline{color:var(--gray);background:var(--line);}
table{width:100%;border-collapse:collapse;}
th,td{text-align:left;padding:9px 10px;border-bottom:1px solid var(--line);vertical-align:top;}
th{color:var(--mut);font-weight:600;font-size:11px;text-transform:uppercase;letter-spacing:.05em;}
.progressbar{height:8px;background:var(--line);border-radius:999px;overflow:hidden;}
.progressbar>span{display:block;height:100%;background:linear-gradient(90deg,var(--accent),color-mix(in srgb,var(--accent) 70%,#fff));border-radius:999px;}
.dot{width:10px;height:10px;border-radius:50%;flex:none;box-shadow:inset 0 0 0 1px #00000018;}
.console-out{font-family:ui-monospace,Consolas,monospace;white-space:pre-wrap;background:var(--content);
 border:1px solid var(--line);border-radius:var(--r);padding:14px;min-height:320px;max-height:60vh;overflow:auto;box-shadow:var(--sh-sm);}
.console-in{width:100%;font-family:ui-monospace,Consolas,monospace;padding:11px 12px;border-radius:var(--r-sm);
 border:1px solid var(--line-strong);background:var(--content);color:var(--fg);}
.cards{display:flex;flex-wrap:wrap;gap:16px;}
.cards .card{width:250px;margin:0;}
.cards .card.keep{border-color:var(--accent);box-shadow:0 0 0 2px var(--accent) inset,var(--sh-md);}
.thumb{width:100%;height:150px;object-fit:contain;border-radius:var(--r-sm);background:var(--line);display:block;}
.noimg{width:100%;height:150px;display:flex;align-items:center;justify-content:center;color:var(--mut);
 background:var(--line);border-radius:var(--r-sm);font-size:12px;text-align:center;padding:8px;}
.badge{font-size:11px;color:var(--accent);font-weight:600;margin-top:8px;display:block;}
.arch{color:var(--mut);font-size:11px;margin-top:8px;}
.branch{margin-left:14px;border-left:1px solid var(--line);padding-left:8px;}
details.drive>summary,details.folder>summary{cursor:pointer;padding:5px 7px;border-radius:var(--r-sm);
 list-style:none;display:flex;align-items:center;gap:8px;user-select:none;}
details>summary::-webkit-details-marker{display:none;}
details>summary::before{content:"\25B8";color:var(--mut);font-size:11px;width:10px;transition:transform .12s;}
details[open]>summary::before{transform:rotate(90deg);}
details.drive>summary:hover,details.folder>summary:hover{background:var(--line);}
.leaf{display:flex;align-items:center;gap:8px;padding:4px 7px 4px 24px;border-radius:var(--r-sm);cursor:default;}
.leaf:hover{background:var(--line);}
.leaf .fname{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.leaf .meta{font-size:12px;white-space:nowrap;}
.leaf.dup{background:color-mix(in srgb,var(--dup) 9%,transparent);cursor:pointer;}
.leaf.hl{background:color-mix(in srgb,var(--dup) 24%,transparent);box-shadow:inset 0 0 0 1px var(--dup);}
.diamond{font-size:11px;font-weight:700;color:var(--dup);flex:none;}
.stat{font-size:28px;font-weight:680;letter-spacing:-.02em;}
.tiles{display:flex;gap:12px;flex-wrap:wrap;}
.tile{flex:1;min-width:120px;background:var(--content);border:1px solid var(--line);border-radius:var(--r);padding:12px 14px;box-shadow:var(--sh-sm);}
.tile .k{font-size:11px;color:var(--mut);text-transform:uppercase;letter-spacing:.05em;}
.tile .v{font-size:22px;font-weight:650;margin-top:2px;}
.switch{position:relative;display:inline-block;width:38px;height:22px;}
.switch input{opacity:0;width:0;height:0;}
.switch .sl{position:absolute;inset:0;background:var(--line-strong);border-radius:999px;transition:.15s;}
.switch .sl::before{content:"";position:absolute;width:16px;height:16px;left:3px;top:3px;background:#fff;border-radius:50%;transition:.15s;box-shadow:var(--sh-sm);}
.switch input:checked + .sl{background:var(--accent);}
.switch input:checked + .sl::before{transform:translateX(16px);}
```

Note: `var(--panel, …)` — the `--panel` token was removed; the fallback `color-mix(...)` handles it. If any page referenced `--panel` directly, it still resolves via the fallback.

- [ ] **Step 2: Expand `icon()`** with the glyphs later tasks + the theme bar use. Add these arms before the `_ =>` fallback (keep the existing nav icons):

```rust
        "auto" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 3v18"/></svg>"#,
        "light" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="4.5"/><path d="M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5 19 19M19 5l-1.5 1.5M6.5 17.5 5 19"/></svg>"#,
        "dark" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12.8A8 8 0 1 1 11.2 3a6.5 6.5 0 0 0 9.8 9.8Z"/></svg>"#,
        "edit" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>"#,
        "trash" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14"/></svg>"#,
        "refresh" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>"#,
        "folder" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 7h5l2 2h11v9a2 2 0 0 1-2 2H3z"/></svg>"#,
        "check" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M20 6 9 17l-5-5"/></svg>"#,
        "warn" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 3 2 20h20L12 3Z"/><path d="M12 9v5M12 17h.01"/></svg>"#,
        "plus" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>"#,
```

Then make the theme-bar buttons (added in Task 1) use icons: in `shell`'s `themebar`, replace the button text with `{icon}<span>Auto</span>` etc. — i.e. `format!` the themebar so each button is `<button data-theme="auto">{}<span>Auto</span></button>` with `icon("auto")`, `icon("light")`, `icon("dark")`. (Keep the label text for clarity; the `.seg button svg` rule sizes the icon.)

- [ ] **Step 3: Build + test.** `cargo build` (warning-clean); `cargo test -p cleanupstorages --lib web` — all existing page tests still pass (class names/ids unchanged; only styling changed). Confirm no `http(s)://`.

- [ ] **Step 4: Commit**

```bash
git add src/web_ui.rs
git commit -m "feat(web): refined macOS-style design system + richer icon set"
```

---

### Task 3: Rebuild the Scan live-status panel

**Files:** Modify `src/web_ui.rs` (`scan_page`)

**Interfaces:** same endpoints (`/api/detected-drives`, `/api/pick-folder`, `/api/scan`, `/api/scan/status`), same polling; only the presentation changes. Keep the DOM ids the existing test asserts and the script updates (`#drives`, `#path`, `#force`, `#scan`, `#browse`, `#running`, `#recent`, `#queued`).

- [ ] **Step 1: Read the current `scan_page`** to preserve its script's data flow, then restyle its `main` and enrich the status rendering. Keep the search/detected-drives/path/force/scan controls but use the refreshed components: detected drives as `.cards`, a real `.switch` for force, and a **status panel** with a progress bar + `.tiles` count tiles + a styled recent list. Set `main` to:

```rust
    let main = r##"
<div class="mut" style="margin-bottom:16px">Scan a drive to catalog its files and find duplicates. Nothing is modified except a tiny hidden marker used to recognise the drive next time.</div>
<h3 style="margin:0 0 10px;font-size:13px;color:var(--mut);text-transform:uppercase;letter-spacing:.05em">Detected drives</h3>
<div id="drives" class="cards" style="margin-bottom:20px"><span class="mut">Looking for connected drives…</span></div>
<div class="card">
  <h3 style="margin-top:0">Or enter a path</h3>
  <div class="row" style="margin-bottom:12px">
    <input id="path" type="text" placeholder="e.g. D:\ or a folder to scan" style="flex:1">
    <button class="btn" id="browse">Browse…</button>
  </div>
  <label class="row" style="font-size:13px;color:var(--mut);margin-bottom:14px">
    <span class="switch"><input id="force" type="checkbox"><span class="sl"></span></span> Force full rescan (re-hash every file, slower)
  </label>
  <button class="btn btn-primary" id="scan">Scan</button>
</div>
<div class="card" id="status-card">
  <div class="row" style="justify-content:space-between"><h3 style="margin:0" id="status-title">Live status</h3><span class="mut" id="status-sub"></span></div>
  <div class="progressbar" id="pbar" style="margin:12px 0;display:none"><span style="width:40%"></span></div>
  <div class="tiles" id="tiles" style="display:none">
    <div class="tile"><div class="k">Hashed</div><div class="v mono" id="t-hashed">0</div></div>
    <div class="tile"><div class="k">Unchanged</div><div class="v mono" id="t-skip">0</div></div>
    <div class="tile"><div class="k">Errors</div><div class="v mono" id="t-err">0</div></div>
    <div class="tile"><div class="k">Archive entries</div><div class="v mono" id="t-arch">0</div></div>
  </div>
  <div class="mut" id="running" style="margin-top:8px">No scan running.</div>
  <div class="mut" id="queued" style="margin-top:4px"></div>
</div>
<div class="card">
  <h3 style="margin-top:0">Recent scans</h3>
  <div id="recent" class="mut">None yet.</div>
</div>"##;
```

Add an indeterminate progress animation to `STYLE` (append in this task): `@keyframes indet{0%{margin-left:-40%}100%{margin-left:100%}} #pbar.run>span{animation:indet 1.2s infinite ease-in-out;}`.

- [ ] **Step 2: Update `scan_page`'s script** so `poll()` drives the new elements: when a scan is running, show `#pbar` (add class `run`) + `#tiles`, set the four tile values from `s.running.{hashed,skipped,errors,archive_entries}`, set `#status-title`="Scanning" and `#status-sub`=the drive/path; when idle hide `#pbar`/`#tiles`, `#running`="No scan running.". Render `#recent` rows with a status icon (`icon` isn't available in JS — use a Unicode ✓/⚠ or a small inline `<svg>` string), the counts, and the error note in `.pill.missing` style when `error_message`. Keep the existing `loadDrives()`, `#browse`→`/api/pick-folder`, `#scan`→`/api/scan`, and the poll loop timing. Render detected drives as `.card`-style clickable cards (fill `#path` on click) showing the mount path + a "new / catalogued" tag.

Reuse the existing script's structure; only the DOM-update lines change. Every interpolation of server data stays `esc()`-wrapped.

- [ ] **Step 3: Keep the page test green.** `scan_page_is_self_contained_and_wired` asserts `name="csrf"`, `/api/scan`, `/api/detected-drives`, `/api/pick-folder`, no `http(s)://` — all preserved. Optionally add `assert!(body.contains("id=\"tiles\""))`.

- [ ] **Step 4: Build + test.** `cargo build`; `cargo test -p cleanupstorages --lib web`; no `http(s)://`.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "feat(web): rebuilt scan live-status panel (progress + count tiles + recent list)"
```

---

### Task 4: Overview hero/bento + Drives cards + inline edit form

**Files:** Modify `src/web_ui.rs` (`overview_page`, `drives_page`)

- [ ] **Step 1: Overview hero + bento.** In `overview_page`, wrap the hero stat in a card with a subtle accent glow and use the refreshed `.stat`/`.tiles`. Replace the hero `<section class="card">` with:

```html
<section class="card" style="position:relative;overflow:hidden">
  <div style="position:absolute;inset:-40% -20% auto auto;width:340px;height:340px;border-radius:50%;
    background:radial-gradient(closest-side,var(--accent-weak),transparent);pointer-events:none"></div>
  <div class="mut" style="font-size:11px;text-transform:uppercase;letter-spacing:.08em">System health</div>
  <h2 id="hero" class="stat" style="margin:8px 0 2px">…</h2>
  <div class="mut" id="hero-sub"></div>
</section>
```

Keep the three cards below in the `.grid`; give each `class="card hover"` for the lift, and render the `#dupe-count` with `.stat`. The script is unchanged (same ids/endpoints).

- [ ] **Step 2: Drives inline edit form** (replaces the `window.prompt` pair). Read the current `drives_page` first. In each card, keep the effective-name title + description + the Rescan/Forget buttons, but change the **Edit…** button to reveal an inline form inside the card instead of prompting. Add a hidden form block to each card:

```js
`<div class="edit-form" style="display:none;margin-top:12px;border-top:1px solid var(--line);padding-top:12px">
   <input class="ef-name" type="text" placeholder="Custom name (blank = detected)" style="width:100%;margin-bottom:8px" value="${esc(d.display_name||'')}">
   <input class="ef-desc" type="text" placeholder="Short description" style="width:100%;margin-bottom:8px" value="${esc(d.description||'')}">
   <div class="row"><button class="btn btn-primary ef-save">Save</button><button class="btn ef-cancel">Cancel</button></div>
 </div>`
```

Wire it in the per-card loop: the `.edit` button toggles `.edit-form` visibility; `.ef-cancel` hides it; `.ef-save` calls `apiPost("/api/rename-drive",{volume_id:c.dataset.vid, name:c.querySelector('.ef-name').value, description:c.querySelector('.ef-desc').value})` then `load()`. Give the Edit/Rescan/Forget buttons their icons (`icon` is server-side; in JS embed the small inline `<svg>` strings or keep text — text is fine). Remove the old `window.prompt` handler.

- [ ] **Step 3: Keep tests green.** `drives_page_is_wired_and_self_contained` still asserts `/api/drives`, `/api/forget-drive`, `/api/purge-all`, `/api/rename-drive`, self-contained — all preserved. Overview test unchanged.

- [ ] **Step 4: Build + full test.** `cargo build`; `cargo test -p cleanupstorages` (full). No `http(s)://`.

- [ ] **Step 5: Update the testing guide** (`docs/TESTING-GUIDE.md`): a short "what to look at" note — the theme toggle (sidebar, Auto/Light/Dark), that dropdowns now follow the theme, the scan panel's live tiles, and the Drives inline edit.

- [ ] **Step 6: Commit**

```bash
git add src/web_ui.rs docs/TESTING-GUIDE.md
git commit -m "feat(web): Overview hero/bento + Drives inline edit form"
```

---

## Self-review notes
- **Spec coverage:** theme toggle + form controls (T1); design-system refresh + icons + shell (T2); scan panel (T3); Overview + Drives + inline edit (T4). Browse tree / Review / Console are covered by T2's shared-class refresh (no per-page rewrite needed). All spec items mapped.
- **Behavior-preserving:** no endpoint/handler change; every page keeps its ids/hooks and endpoint fetches; page tests' assertion strings preserved. Only Task 1 adds a behavior (theme switching) and Task 3/4 restructure presentation.
- **Consistency:** tokens defined once (T1) and consumed by components (T2+); `.tiles`/`.tile`/`.switch`/`.seg` classes defined in T1/T2 and used in T3/T1; icon keys added in T2 used in T2/T3/T4.
- **Self-contained:** every task ends by confirming `grep -c "http://\|https://" src/web_ui.rs` == 0.
- **No placeholders:** concrete CSS/markup given; the "read current markup first" spots (T3 script, T4 drives) are called out with the exact ids/behavior to preserve.
