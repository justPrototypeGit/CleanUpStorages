# Browse Tree + Duplicate Highlighting + Theme Colors — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn Browse into a drive→folder tree with duplicate files color-highlighted (click to highlight copies), auto-color each drive consistently across pages, and fix the low-contrast dark-mode status colors.

**Architecture:** One bounded catalog query (`duplicate_counts`) + three new fields on the search DTO feed a client-side tree (`buildTree`/`renderTree`) built from the existing `/api/search` data — no new endpoint, no schema change. Drive/duplicate colors come from two pure `SHARED_JS` helpers; all theme tokens stay in the one `STYLE` block.

**Tech Stack:** Rust, rusqlite, axum. Self-contained HTML/CSS/JS in `web_ui.rs`. Existing suite is the guard.

## Global Constraints

- **Self-contained pages.** No `http://`/`https://` in any rendered page — no CDN/fonts/icons. Tree uses CSS + inline markup only.
- **XSS-safe.** All DB-derived text via `esc()`/`textContent`; never raw `innerHTML` of server strings. (Building an HTML string from `esc()`-ed pieces and assigning to `innerHTML` is the established pattern here and is safe because every interpolation is escaped.)
- **No SHARED_JS const collision.** The browse page script runs after `SHARED_JS` (which declares `$`, `esc`, `CSRF`, `apiGet`, `apiPost`, `fmtSize`, `fmtDate`, and — after Task 3 — `driveColor`, `dupColor`, `hueOf`). Never re-declare these in a page script.
- **Reads use `Catalog::open_readonly`.** `api_search` stays read-only.
- **Behavior preservation elsewhere.** The existing suite stays green; only the Browse page's own test is updated (kept meaningful) to match the tree.
- **Conventional Commits**, scope `catalog`/`web`; body ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- **Windows/PowerShell dev env:** `cargo build`, `cargo test`.

## File structure

- `src/catalog/store.rs` — MODIFY (Task 1): add `duplicate_counts`.
- `src/web.rs` — MODIFY (Task 2): enrich `HitDto`, fill it in `api_search`.
- `src/web_ui.rs` — MODIFY (Tasks 3, 4, 5): theme tokens + tree CSS + `driveColor`/`dupColor` (T3); `browse_page` tree (T4); drive-color dots on Drives/Overview (T5).

---

### Task 1: `Catalog::duplicate_counts`

**Files:** Modify `src/catalog/store.rs`

**Interfaces:**
- Produces: `Catalog::duplicate_counts(&self, hashes: &[String]) -> anyhow::Result<std::collections::HashMap<String, i64>>` — for the given content hashes, returns those that have **>1 active copy in the whole catalog**, mapped to their active copy count. Hashes not passed in, or with a single active copy, are absent from the map.

- [ ] **Step 1: Write the failing test** (in `src/catalog/store.rs` tests; use the module's real `open_tmp()` helper)

```rust
    #[test]
    fn duplicate_counts_reports_only_multi_active_hashes() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(), label: "V".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let mut f = crate::catalog::models::NewFile {
            volume_id: "v".into(), relative_path: "a".into(), filename: "a".into(),
            extension: "".into(), size_bytes: 1, content_hash: "dup".into(),
            created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Other, container_chain: None };
        cat.upsert_file(&f, 1).unwrap();                       // dup copy 1
        f.relative_path = "b".into(); f.filename = "b".into();
        cat.upsert_file(&f, 1).unwrap();                       // dup copy 2
        f.relative_path = "u".into(); f.filename = "u".into(); f.content_hash = "uniq".into();
        cat.upsert_file(&f, 1).unwrap();                       // unique
        let m = cat.duplicate_counts(&["dup".to_string(), "uniq".to_string(), "absent".to_string()]).unwrap();
        assert_eq!(m.get("dup").copied(), Some(2));
        assert_eq!(m.get("uniq"), None);   // single copy -> not duplicated
        assert_eq!(m.get("absent"), None); // not in catalog
    }
```

- [ ] **Step 2: Run it — expect FAIL** (`cargo test -p cleanupstorages duplicate_counts_reports_only_multi_active_hashes`) → method missing.

- [ ] **Step 3: Implement** (in `src/catalog/store.rs`, near `duplicate_group_count`):

```rust
    /// For the given content hashes, those with >1 active copy in the catalog, mapped to their
    /// active copy count. Bounded by the passed hashes (indexed on content_hash).
    pub fn duplicate_counts(&self, hashes: &[String])
        -> anyhow::Result<std::collections::HashMap<String, i64>>
    {
        let mut out = std::collections::HashMap::new();
        if hashes.is_empty() { return Ok(out); }
        // Deduplicate the input so the IN-list stays small.
        let uniq: std::collections::HashSet<&String> = hashes.iter().collect();
        let placeholders = std::iter::repeat("?").take(uniq.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT content_hash, count(*) FROM files
             WHERE status='active' AND content_hash IN ({placeholders})
             GROUP BY content_hash HAVING count(*) > 1");
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            uniq.iter().map(|h| *h as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows { let (h, n) = row?; out.insert(h, n); }
        Ok(out)
    }
```

- [ ] **Step 4: Run it — expect PASS.** Then full `cargo test -p cleanupstorages`.

- [ ] **Step 5: Commit**

```bash
git add src/catalog/store.rs
git commit -m "feat(catalog): duplicate_counts — active copy counts for given hashes"
```

---

### Task 2: Enrich `HitDto` + fill it in `api_search`

**Files:** Modify `src/web.rs`

**Interfaces:**
- Consumes: `Catalog::duplicate_counts` (Task 1), `Catalog::volume_stats` (existing).
- Produces: `/api/search` hits now include `volume_label: String`, `content_hash: String`, `copies: Option<i64>` (present only when the file is duplicated).

- [ ] **Step 1: Add the failing test** (in `src/web.rs` tests; `seed_dupes` seeds vol-1 "Photos HDD" with two `content_hash:"DUP"` files)

```rust
    #[tokio::test]
    async fn api_search_enriches_label_hash_and_copies() {
        let (_t, db, _state) = seed_dupes();
        let v = get_json(&db, "/api/search").await; // empty query -> all files
        let arr = v.as_array().unwrap();
        assert!(arr.len() >= 2);
        for h in arr {
            assert_eq!(h["volume_label"], "Photos HDD");     // friendly name, not the id
            assert_eq!(h["content_hash"], "DUP");
            assert_eq!(h["copies"], 2);                       // both are duplicated (2 active copies)
        }
    }

    #[tokio::test]
    async fn api_search_copies_null_for_unique_file() {
        let (_t, db) = seed_catalog(); // thesis.pdf (h1) + archived inner.jpg (h2): both unique
        let v = get_json(&db, "/api/search?q=thesis").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["volume_label"], "Test HDD");
        assert!(arr[0]["copies"].is_null());
    }
```

- [ ] **Step 2: Run — expect FAIL** (fields missing / assertion mismatch).

- [ ] **Step 3: Add the three fields to `HitDto`** (find `struct HitDto` in `src/web.rs`):

```rust
    volume_label: String,
    content_hash: String,
    copies: Option<i64>,
```

The existing `impl From<FileRecord> for HitDto` can't know the label or copies (they need the catalog), so give `From` sensible defaults and let the handler fill them: in `From`, set `content_hash: f.content_hash.clone()` (add `.clone()` where `content_hash` is read, or move it last), `volume_label: String::new()`, `copies: None`. Then the handler overwrites `volume_label`/`copies`. (Keep `content_hash` filled by `From` since the record already has it.)

- [ ] **Step 4: Fill label + copies in `api_search`** (replace the final map/collect at ~`src/web.rs:244-245`):

```rust
    let hits = cat.search_filtered(&filters, limit).map_err(err500)?;
    // Friendly drive names + which results are duplicated (global active-copy count).
    let labels: std::collections::HashMap<String, String> = cat.volume_stats().map_err(err500)?
        .into_iter().map(|(id, label, _, _)| (id, label)).collect();
    let hashes: Vec<String> = hits.iter().map(|f| f.content_hash.clone()).collect();
    let dupes = cat.duplicate_counts(&hashes).map_err(err500)?;
    let out: Vec<HitDto> = hits.into_iter().map(|f| {
        let mut dto = HitDto::from(f);
        dto.volume_label = labels.get(&dto.volume_id).cloned().unwrap_or_else(|| dto.volume_id.clone());
        dto.copies = dupes.get(&dto.content_hash).copied();
        dto
    }).collect();
    Ok(Json(out))
```

- [ ] **Step 5: Run — expect PASS.** Then full `cargo test -p cleanupstorages` (the existing `api_search_returns_hits_with_location` / `_shows_archive_chain_in_location` still pass — they only assert `location`/`volume_id`/`category`, all unchanged).

- [ ] **Step 6: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): enrich search hits with drive name, content hash, copy count"
```

---

### Task 3: Theme colors + tree CSS + `driveColor`/`dupColor` helpers

**Files:** Modify `src/web_ui.rs` (`STYLE`, `SHARED_JS`)

**Interfaces:**
- Produces (in `SHARED_JS`): `hueOf(s)`, `driveColor(id)`, `dupColor(hash)` — pure functions returning stable HSL strings.
- Produces (in `STYLE`): dark-mode status color overrides, softened dark bg, and tree classes (`.drive`, `.folder`, `.branch`, `.leaf`, `.leaf.dup`, `.leaf.hl`, `.dot`, `.diamond`).

- [ ] **Step 1: Add the dark-mode status colors + softened bg.** In `STYLE`'s `@media (prefers-color-scheme:dark)` `:root` block (currently overrides `--bg/--panel/--content/--fg/--mut/--line/--accent`), add:

```css
--bg:#1c1c1e;--content:#2c2c2e;
--amber:#ff9f0a;--amber-bg:#ff9f0a26;--red:#ff453a;--red-bg:#ff453a26;--green:#30d158;--green-bg:#30d15826;--gray:#98989d;
```

(Replace the existing dark `--bg:#000` and `--content:#1d1d1f` with the above; keep the other dark overrides. Light mode `:root` is unchanged.)

- [ ] **Step 2: Add the tree CSS** to the end of `STYLE`:

```css
.branch{margin-left:14px;border-left:1px solid var(--line);padding-left:8px;}
details.drive>summary,details.folder>summary{cursor:pointer;padding:4px 6px;border-radius:6px;
 list-style:none;display:flex;align-items:center;gap:8px;user-select:none;}
details>summary::-webkit-details-marker{display:none;}
details>summary::before{content:"\25B8";color:var(--mut);font-size:11px;width:10px;transition:transform .12s;}
details[open]>summary::before{transform:rotate(90deg);}
details.drive>summary:hover,details.folder>summary:hover{background:var(--line);}
.dot{width:10px;height:10px;border-radius:50%;flex:none;}
.leaf{display:flex;align-items:center;gap:8px;padding:3px 6px 3px 24px;border-radius:6px;cursor:default;}
.leaf:hover{background:var(--line);}
.leaf .fname{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.leaf .meta{font-size:12px;white-space:nowrap;}
.leaf.dup{background:color-mix(in srgb,var(--dup) 9%,transparent);cursor:pointer;}
.leaf.hl{background:color-mix(in srgb,var(--dup) 24%,transparent);box-shadow:inset 0 0 0 1px var(--dup);}
.diamond{font-size:11px;font-weight:600;color:var(--dup);flex:none;}
```

- [ ] **Step 3: Add the color helpers** to `SHARED_JS` (after `fmtDate`):

```js
function hueOf(s){let h=0;for(let i=0;i<String(s).length;i++)h=(h*31+String(s).charCodeAt(i))>>>0;return h%360;}
function driveColor(id){return `hsl(${hueOf(id)},60%,55%)`;}
function dupColor(hash){return `hsl(${hueOf(hash)},72%,52%)`;}
```

- [ ] **Step 4: Build + run** (`cargo build` warning-clean; `cargo test -p cleanupstorages --lib web`). No page uses the new classes yet, so all existing page tests (self-contained, etc.) stay green — this task only adds CSS/JS.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs
git commit -m "feat(web): dark-mode status colors, softened dark bg, tree CSS + drive/dup color helpers"
```

---

### Task 4: Browse page becomes a tree

**Files:** Modify `src/web_ui.rs` (`browse_page`)

**Interfaces:**
- Consumes: `/api/search` (enriched, Task 2), `/api/volumes`, and `driveColor`/`dupColor` (Task 3).
- Produces: `browse_page` renders the search box + 3 filters (unchanged) and a collapsible drive→folder→file tree with duplicate highlighting.

**Preserve for the test:** the page must still contain `id="q"` (search input), `id="results"` (tree container), and fetch `/api/search`; and stay self-contained.

- [ ] **Step 1: Replace `browse_page`'s `main` + `script`.** Keep the top controls exactly; swap the `<table>` for a tree container `<div id="results" class="tree"></div>`, and replace the row-rendering script with the tree builder/renderer. Full replacement:

```rust
pub fn browse_page(csrf: &str) -> String {
    let main = r##"
<div style="display:flex;gap:10px;flex-wrap:wrap;align-items:center;margin-bottom:12px">
  <input id="q" type="search" placeholder="Search filename or path…" style="flex:1;min-width:220px;padding:8px 12px;border-radius:999px;border:1px solid var(--line);background:var(--content);color:var(--fg)" autofocus>
  <select id="volume" class="btn"><option value="">All drives</option></select>
  <select id="category" class="btn"><option value="">All types</option>
    <option value="photo">Photo</option><option value="video">Video</option>
    <option value="document">Document</option><option value="academic">Academic</option><option value="other">Other</option></select>
  <select id="status" class="btn"><option value="">Any status</option>
    <option value="active">Active</option><option value="missing">Missing</option>
    <option value="quarantined">Quarantined</option><option value="purged">Purged</option></select>
</div>
<div class="mut" id="count" style="margin-bottom:8px"></div>
<div class="mut" style="font-size:12px;margin-bottom:8px">Files sharing content share a <span class="diamond" style="--dup:hsl(280,72%,52%)">◆</span> color — click one to highlight its copies.</div>
<div class="card" style="padding:10px"><div id="results" class="tree"></div></div>"##;
    let script = r##"
let timer=null;
// --- data -> tree model (pure) ---
function buildTree(hits){
  const drives=new Map();
  for(const h of hits){
    if(!drives.has(h.volume_id)) drives.set(h.volume_id,{id:h.volume_id,label:h.volume_label||h.volume_id,size:0,children:new Map(),files:[]});
    const drive=drives.get(h.volume_id); drive.size+=(h.size_bytes||0);
    const segs=String(h.relative_path||"").split('/').filter(Boolean);
    const last=segs.pop()||h.filename||"(file)";
    let node=drive;
    for(const seg of segs){ if(!node.children.has(seg)) node.children.set(seg,{name:seg,children:new Map(),files:[]}); node=node.children.get(seg); }
    if(h.container_chain){
      if(!node.children.has(last)) node.children.set(last,{name:last,archive:true,children:new Map(),files:[]});
      node.children.get(last).files.push({name:h.container_chain,hit:h});
    } else { node.files.push({name:last,hit:h}); }
  }
  return drives;
}
function countDups(node){ let n=node.files.filter(f=>f.hit.copies).length; for(const c of node.children.values()) n+=countDups(c); return n; }
// --- model -> HTML (pure; every interpolation esc()'d) ---
function renderLeaf(f){
  const h=f.hit;
  const dia=h.copies?`<span class="diamond" style="--dup:${dupColor(h.content_hash)}" title="${h.copies} copies">◆${h.copies}</span>`:"";
  const pill=h.status!=="active"?`<span class="pill ${esc(h.status)}">${esc(h.status)}</span>`:"";
  const cls=h.copies?"leaf dup":"leaf";
  const style=h.copies?`style="--dup:${dupColor(h.content_hash)}"`:"";
  return `<div class="${cls}" data-hash="${esc(h.content_hash)}" ${style}><span class="fname">${esc(f.name)}</span>${dia}<span class="meta mut">${fmtSize(h.size_bytes)}</span>${pill}</div>`;
}
function renderFolder(node){
  let html='<div class="branch">';
  for(const child of node.children.values()){
    const d=countDups(child); const badge=d?` <span class="mut" style="font-size:11px">${d} dup</span>`:"";
    html+=`<details class="folder"><summary>${child.archive?"🗜 ":""}${esc(child.name)}${badge}</summary>${renderFolder(child)}</details>`;
  }
  for(const f of node.files) html+=renderLeaf(f);
  return html+'</div>';
}
function renderTree(drives){
  if(!drives.size) return '<div class="mut" style="padding:12px">No files match.</div>';
  let html="";
  for(const drive of drives.values()){
    html+=`<details class="drive" open><summary><span class="dot" style="background:${driveColor(drive.id)}"></span><b>${esc(drive.label)}</b> <span class="mut">${fmtSize(drive.size)}</span></summary>${renderFolder(drive)}</details>`;
  }
  return html;
}
async function run(){ try{
  const p=new URLSearchParams(); const q=$("#q").value.trim(); if(q)p.set("q",q);
  for(const k of ["volume","category","status"]){ const v=$("#"+k).value; if(v)p.set(k,v); }
  p.set("limit","3000");
  const hits=await apiGet("/api/search?"+p.toString());
  $("#count").textContent=hits.length+" result"+(hits.length===1?"":"s")+(hits.length>=3000?" (showing first 3000 — refine your search)":"");
  $("#results").innerHTML=renderTree(buildTree(hits));
}catch(e){ $("#count").textContent="Search error: "+e; } }
// click a duplicated leaf -> highlight every visible copy (same content hash)
$("#results").addEventListener("click",e=>{
  const leaf=e.target.closest(".leaf.dup"); if(!leaf)return;
  const hash=leaf.dataset.hash, was=leaf.classList.contains("hl");
  document.querySelectorAll(".leaf.hl").forEach(el=>el.classList.remove("hl"));
  if(!was) document.querySelectorAll('.leaf[data-hash="'+CSS.escape(hash)+'"]').forEach(el=>el.classList.add("hl"));
});
function debounced(){ clearTimeout(timer); timer=setTimeout(run,180); }
async function init(){
  const vs=await apiGet("/api/volumes"); const sel=$("#volume");
  for(const v of vs){ const o=document.createElement("option"); o.value=v.volume_id; o.textContent=v.label; sel.appendChild(o); }
  $("#q").addEventListener("input",debounced);
  for(const k of ["volume","category","status"]) $("#"+k).addEventListener("change",run);
  run();
}
init();"##;
    shell("browse", csrf, "Browse", main, script)
}
```

Note: 🗜 (compression) is a plain Unicode character — self-contained, no external request. If it renders inconsistently you may drop it, but keep the archive folder distinguishable (the `archive:true` node still nests its entries).

- [ ] **Step 2: Update the browse page test** (`index_page_has_search_ui_and_calls_api`, ~`src/web.rs:1147`). It still asserts `id="q"`, `id="results"`, `/api/search`, self-contained — all preserved. Add one assertion that the tree is wired:

```rust
        assert!(body.contains("buildTree") && body.contains("renderTree"), "renders a tree");
        assert!(body.contains("class=\"tree\""), "tree container present");
```

- [ ] **Step 3: Build + run** (`cargo build`; `cargo test -p cleanupstorages --lib web`). Confirm `index_page_has_search_ui_and_calls_api` passes and no `http(s)://` leaked.

- [ ] **Step 4: Manual smoke (recommended):** build release, run `browse` against the sandbox, confirm drives expand, duplicates show a colored ◆ and click-highlights copies, and dark mode reads well.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "feat(web): Browse becomes a drive/folder tree with duplicate highlighting"
```

---

### Task 5: Consistent drive colors on Drives + Overview

**Files:** Modify `src/web_ui.rs` (`drives_page`, `overview_page`)

**Interfaces:** Consumes `driveColor` (Task 3). Uses each drive's `volume_id` (already present in `/api/drives` and `/api/stats` volume rows).

- [ ] **Step 1: Drives page dot.** In `drives_page`'s card rendering, prepend a `driveColor` dot to each drive card's title. Find where the card title (`<h3>${esc(d.label)}</h3>` or similar) is built and change it to:

```js
`<h3 style="display:flex;align-items:center;gap:8px"><span class="dot" style="background:${driveColor(d.volume_id)}"></span>${esc(d.label)}</h3>`
```

(Read the actual current markup first; keep everything else. `d.volume_id` is in the `/api/drives` DTO.)

- [ ] **Step 2: Overview reclaim bars dot.** In `overview_page`'s reclaim-bar rendering, prepend the same dot before each drive's name. Find the reclaim-bar row (it maps `drives` with `esc(d.label)`); change the label span to include `<span class="dot" style="background:${driveColor(d.volume_id)};display:inline-block;margin-right:6px"></span>` before `esc(d.label)`. (`/api/drives` supplies `volume_id`.)

- [ ] **Step 3: Build + run** (`cargo build`; `cargo test -p cleanupstorages --lib web`). The drives/overview page tests still assert their endpoints + self-containment — unchanged. Confirm green.

- [ ] **Step 4: Commit**

```bash
git add src/web_ui.rs
git commit -m "feat(web): show each drive's auto color on Drives and Overview too"
```

---

## Self-review notes

- **Spec coverage:** duplicate_counts (T1); enriched hits volume_label/content_hash/copies (T2); theme dark status colors + softened bg + tree CSS + driveColor/dupColor (T3); Browse tree + duplicate highlight + click-to-highlight (T4); consistent drive colors on Drives/Overview (T5). All spec items mapped.
- **Type consistency:** `duplicate_counts(&[String]) -> HashMap<String,i64>` used by `api_search`; `HitDto.{volume_label,content_hash,copies}` consumed by `buildTree`/`renderLeaf`; `driveColor(id)`/`dupColor(hash)` defined in T3, used in T4/T5. `copies: Option<i64>` → JSON `null`/number, client tests `h.copies` truthiness.
- **No SHARED_JS collision:** the browse script uses `$`,`esc`,`fmtSize`,`apiGet`,`driveColor`,`dupColor` by reference; declares only `timer`,`buildTree`,`countDups`,`renderLeaf`,`renderFolder`,`renderTree`,`run`,`debounced`,`init` — none of which are SHARED_JS names.
- **Ordering:** T1 (query) → T2 (API) → T3 (CSS/helpers) → T4 (tree, needs T2+T3) → T5 (reuse color on other pages). Each independently revertible.
- **Self-contained:** tree uses CSS `content:"\25B8"`, Unicode ◆/🗜, `color-mix`, and inline styles — no external references; the browse test re-asserts no `http(s)://`.
