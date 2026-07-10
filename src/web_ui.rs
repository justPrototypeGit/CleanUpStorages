//! Shared, self-contained UI shell: one design-system stylesheet, an inline SVG icon set, a
//! sidebar/toolbar renderer, and shared client helpers. Every page in `web.rs` renders through
//! `shell()` so the six screens stay visually identical and no markup is duplicated.

pub const STYLE: &str = r##"
:root{color-scheme:light dark;--bg:#f5f5f7;--panel:#ffffffcc;--content:#ffffff;--fg:#1d1d1f;
 --mut:#6e6e73;--line:#1d1d1f1a;--accent:#0071e3;--amber:#b45309;--amber-bg:#f59e0b26;
 --red:#c0392b;--red-bg:#e74c3c22;--green:#1a7f37;--green-bg:#2ecc7122;--gray:#6e6e73;}
@media (prefers-color-scheme:dark){:root{--bg:#000;--panel:#1d1d1fcc;--content:#1d1d1f;
 --fg:#f5f5f7;--mut:#98989d;--line:#ffffff1a;--accent:#0a84ff;}}
*{box-sizing:border-box;}
body{margin:0;font:14px/1.45 -apple-system,"Segoe UI",Roboto,sans-serif;background:var(--bg);color:var(--fg);}
.mono{font-family:ui-monospace,"Cascadia Code","SF Mono",Consolas,monospace;font-variant-numeric:tabular-nums;}
aside.side{position:fixed;left:0;top:0;bottom:0;width:260px;background:var(--panel);
 backdrop-filter:blur(20px) saturate(180%);border-right:1px solid var(--line);padding:20px 12px;overflow-y:auto;}
aside.side h1{font-size:18px;margin:0 8px 2px;letter-spacing:-.3px;}
aside.side .tagline{margin:0 8px 20px;font-size:12px;color:var(--mut);}
nav a{display:flex;gap:10px;align-items:center;padding:7px 10px;margin:2px 0;border-radius:6px;
 color:var(--mut);text-decoration:none;font-weight:500;}
nav a.active{background:color-mix(in srgb,var(--accent) 12%,transparent);color:var(--accent);}
nav a:hover:not(.active){background:var(--line);}
nav a svg{width:16px;height:16px;flex:none;}
header.top{position:fixed;top:0;left:260px;right:0;height:52px;display:flex;align-items:center;
 gap:12px;padding:0 20px;background:var(--panel);backdrop-filter:blur(20px) saturate(150%);
 border-bottom:1px solid var(--line);z-index:5;}
header.top strong{font-size:14px;letter-spacing:-.1px;}
main{margin-left:260px;padding:76px 24px 40px;max-width:1100px;}
.card{background:var(--content);border:1px solid var(--line);border-radius:14px;padding:20px;margin:0 0 16px;
 box-shadow:0 1px 2px #00000010;}
.grid{display:grid;grid-template-columns:repeat(12,1fr);gap:16px;}
.btn{font:inherit;padding:8px 14px;border-radius:8px;border:1px solid var(--line);
 background:transparent;color:var(--fg);cursor:pointer;}
.btn:hover{background:var(--line);}
.btn-primary{background:var(--accent);border-color:var(--accent);color:#fff;}
.btn-primary:hover{filter:brightness(1.06);}
.btn-danger{border-color:var(--red);color:var(--red);}
.btn-danger:hover{background:var(--red-bg);}
.pill{font-size:11px;padding:2px 8px;border-radius:999px;font-weight:500;}
.pill.quarantined{color:var(--amber);background:var(--amber-bg);}
.pill.missing{color:var(--red);background:var(--red-bg);}
.pill.active{color:var(--green);background:var(--green-bg);}
.pill.purged{color:var(--gray);background:var(--line);}
.mut{color:var(--mut);}
table{width:100%;border-collapse:collapse;}
th,td{text-align:left;padding:8px;border-bottom:1px solid var(--line);vertical-align:top;}
th{color:var(--mut);font-weight:600;font-size:12px;text-transform:uppercase;letter-spacing:.04em;}
.progressbar{height:6px;background:var(--line);border-radius:999px;overflow:hidden;}
.progressbar>span{display:block;height:100%;background:var(--accent);}
.console-out{font-family:ui-monospace,Consolas,monospace;white-space:pre-wrap;background:var(--content);
 border:1px solid var(--line);border-radius:10px;padding:12px;min-height:300px;max-height:60vh;overflow:auto;}
.console-in{width:100%;font-family:ui-monospace,Consolas,monospace;padding:10px;border-radius:8px;
 border:1px solid var(--line);background:var(--content);color:var(--fg);}
"##;

pub const SHARED_JS: &str = r##"
const $=s=>document.querySelector(s);
const CSRF=(document.querySelector('meta[name="csrf"]')||{}).content||"";
function esc(s){return (s==null?"":String(s)).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));}
function fmtSize(n){if(n==null)return"—";const u=["B","KB","MB","GB","TB"];let i=0,x=Number(n);while(x>=1024&&i<u.length-1){x/=1024;i++;}return (i?x.toFixed(1):x)+" "+u[i];}
function fmtDate(t){return t?new Date(t*1000).toISOString().slice(0,10):"—";}
async function apiGet(u){const r=await fetch(u);if(!r.ok)throw new Error(await r.text());return r.json();}
async function apiPost(u,body){const r=await fetch(u,{method:"POST",headers:{"content-type":"application/json","x-cleanup-token":CSRF},body:JSON.stringify(body||{})});if(!r.ok)throw new Error(await r.text());return r.json();}
"##;

/// Inline SVG for a nav glyph (stroke-based, currentColor). Unknown keys get a generic dot.
fn icon(name: &str) -> &'static str {
    match name {
        "overview" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>"#,
        "browse" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 7h5l2 2h11v9a2 2 0 0 1-2 2H3z"/></svg>"#,
        "duplicates" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>"#,
        "drives" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><circle cx="7" cy="7.5" r="1"/><circle cx="7" cy="16.5" r="1"/></svg>"#,
        "scan" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/></svg>"#,
        "console" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3M13 15h4"/></svg>"#,
        _ => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/></svg>"#,
    }
}

struct NavItem { key: &'static str, href: &'static str, label: &'static str }
const NAV: &[NavItem] = &[
    NavItem{key:"overview",href:"/",label:"Overview"},
    NavItem{key:"browse",href:"/browse",label:"Browse"},
    NavItem{key:"duplicates",href:"/review",label:"Duplicates"},
    NavItem{key:"drives",href:"/drives",label:"Drives"},
    NavItem{key:"scan",href:"/scan",label:"Scan"},
    NavItem{key:"console",href:"/console",label:"Console"},
];

/// Render a full self-contained page. `active` is a NAV key.
pub fn shell(active: &str, csrf: &str, title: &str, main_html: &str, page_script: &str) -> String {
    let nav = NAV.iter().map(|n| {
        let cls = if n.key == active { "active" } else { "" };
        let current = if n.key == active { r#" aria-current="page""# } else { "" };
        format!(r#"<a class="{cls}"{current} href="{}">{}<span>{}</span></a>"#, n.href, icon(n.key), n.label)
    }).collect::<String>();
    format!(r##"<!doctype html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="csrf" content="{csrf}"><title>CleanUpStorages — {title}</title>
<style>{style}</style></head><body>
<aside class="side"><h1>CleanUpStorages</h1><p class="tagline">Storage cleanup</p><nav>{nav}</nav></aside>
<header class="top"><strong>{title}</strong></header>
<main>{main_html}</main>
<script>{shared}</script><script>{page_script}</script>
</body></html>"##,
        csrf = csrf, title = title, style = STYLE, nav = nav, main_html = main_html,
        shared = SHARED_JS, page_script = page_script)
}

pub fn overview_page(csrf: &str) -> String {
    let main = r##"
<section class="card"><div class="mut" style="font-size:11px;text-transform:uppercase;letter-spacing:.08em">System health</div>
  <h2 id="hero" style="margin:6px 0 2px;font-size:26px">…</h2>
  <div class="mut" id="hero-sub"></div></section>
<div class="grid">
  <div class="card" style="grid-column:span 5"><h3 style="margin-top:0">Duplicate groups</h3>
    <div style="font-size:22px" id="dupe-count">…</div>
    <div class="mut" id="dupe-reclaim"></div>
    <a class="btn btn-primary" href="/review" style="display:inline-block;margin-top:12px;text-decoration:none">Review duplicates</a></div>
  <div class="card" style="grid-column:span 7"><h3 style="margin-top:0">Reclaimable space</h3>
    <div id="reclaim-bars"></div></div>
  <div class="card" style="grid-column:span 12"><h3 style="margin-top:0">Recent activity</h3>
    <div id="activity" class="mut">Loading…</div></div>
</div>"##;
    let script = r##"
async function init(){
  const st=await apiGet("/api/stats");
  const totalFiles=st.volumes.reduce((a,v)=>a+v.active_files,0);
  $("#hero").textContent=totalFiles.toLocaleString()+" files catalogued";
  $("#hero-sub").textContent="across "+st.volumes.length+" drive"+(st.volumes.length===1?"":"s")+" · catalog stored on this computer";
  $("#dupe-count").textContent=st.duplicate_groups+" group"+(st.duplicate_groups===1?"":"s");
  const drives=await apiGet("/api/drives");
  const totalReclaim=drives.reduce((a,d)=>a+(d.reclaimable_bytes||0),0);
  $("#dupe-reclaim").textContent="~"+fmtSize(totalReclaim)+" reclaimable";
  const max=Math.max(1,...drives.map(d=>d.reclaimable_bytes||0));
  $("#reclaim-bars").innerHTML=drives.map(d=>`<div style="margin:10px 0">
     <div style="display:flex;justify-content:space-between"><span>${esc(d.label)}</span><span class="mono">${fmtSize(d.reclaimable_bytes)}</span></div>
     <div class="progressbar"><span style="width:${Math.round(100*(d.reclaimable_bytes||0)/max)}%"></span></div></div>`).join("")||'<span class="mut">Nothing to reclaim.</span>';
  const acts=await apiGet("/api/activity");
  $("#activity").innerHTML=acts.length?acts.map(a=>`<div style="padding:6px 0;border-bottom:1px solid var(--line)">
     <span>${esc(a.summary)}</span> <span class="mut mono" style="float:right">${fmtDate(a.occurred_at)}</span></div>`).join(""):"No activity yet.";
}
init().catch(e=>{$("#activity").textContent="Error: "+e;});"##;
    shell("overview", csrf, "Overview", main, script)
}

pub fn drives_page(csrf: &str) -> String {
    let main = r##"
<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:12px">
  <div class="mut">Manage catalogued drives. Nothing here deletes files on your drives.</div>
  <button class="btn btn-danger" id="purge-all">Purge all quarantines</button></div>
<div id="drives" class="mut">Loading drives…</div>
<div class="mut" id="msg" style="margin-top:12px;min-height:1.4em"></div>"##;
    let script = r##"
function bar(d){ if(d.total_bytes==null) return "";
  const used=d.total_bytes-d.free_bytes, pct=Math.round(100*used/d.total_bytes);
  return `<div style="margin:10px 0"><div style="display:flex;justify-content:space-between">
    <span class="mono">${fmtSize(used)} of ${fmtSize(d.total_bytes)} used</span><span class="mut">${pct}% full</span></div>
    <div class="progressbar"><span style="width:${pct}%"></span></div></div>`; }
async function load(){
  const drives=await apiGet("/api/drives");
  $("#drives").innerHTML = drives.length? drives.map(d=>`<div class="card" data-vid="${esc(d.volume_id)}" data-path="${esc(d.mount_path||'')}">
    <div style="display:flex;justify-content:space-between;align-items:start">
      <div><h3 style="margin:0">${esc(d.label)}</h3>
        <div class="mut" style="font-size:12px">${d.connected?'<span class="pill active">connected</span>':'<span class="pill purged">offline</span>'}
          · ${d.active_files.toLocaleString()} files · last scan ${fmtDate(d.last_seen_at)}
          ${d.has_errors?' · <span class="pill missing">had scan errors</span>':''}</div></div>
      <div class="mut mono">${fmtSize(d.reclaimable_bytes)} reclaimable</div></div>
    ${bar(d)}
    <div style="display:flex;gap:8px;margin-top:12px">
      <button class="btn rescan" ${d.connected?'':'disabled'}>${d.has_errors?'Repair (rescan)':'Rescan'}</button>
      <button class="btn btn-danger forget">Forget…</button></div></div>`).join("")
    : '<div class="mut">No drives catalogued yet. Scan one from the Scan page.</div>';
  for(const c of document.querySelectorAll("[data-vid]")){
    c.querySelector(".forget").onclick=async()=>{
      const vid=c.dataset.vid, label=c.querySelector("h3").textContent;
      if(!window.confirm(`Forget "${label}"? This removes it from the catalog only — files on the drive are NOT deleted. You can rescan to re-add it.`))return;
      try{ const r=await apiPost("/api/forget-drive",{volume_id:vid}); $("#msg").textContent=`Forgot ${label} (${r.removed_files} entries removed).`; load(); }
      catch(e){ $("#msg").textContent="Error: "+e; }
    };
    c.querySelector(".rescan").onclick=async()=>{
      const path=c.dataset.path; if(!path){ $("#msg").textContent="Drive not connected."; return; }
      try{ await apiPost("/api/scan",{path,force:false}); $("#msg").textContent="Rescan queued for "+path+". Watch progress on the Scan page."; }
      catch(e){ $("#msg").textContent="Error: "+e; }
    };
  }
}
$("#purge-all").onclick=async()=>{
  if(!window.confirm("Permanently delete every drive's _ToDelete quarantine? This is the only real delete and cannot be undone."))return;
  try{ const r=await apiPost("/api/purge-all",{});
    let m=`Purged ${r.purged_volumes} volume(s), reclaimed ${fmtSize(r.bytes_reclaimed)}.`;
    if(r.skipped_unmounted.length)m+=" Skipped (offline): "+r.skipped_unmounted.join(", ")+".";
    if(r.errors.length)m+=" Errors: "+r.errors.join("; ");
    $("#msg").textContent=m; load(); }
  catch(e){ $("#msg").textContent="Error: "+e; }
};
load().catch(e=>{$("#drives").textContent="Error: "+e;});"##;
    shell("drives", csrf, "Drives", main, script)
}

/// Client-side-only REPL: parses a typed line into one existing `/api/*` call (the same
/// endpoints the buttons use, via SHARED_JS `apiGet`/`apiPost`) and prints the JSON response as
/// scrollback text. No new backend, no subprocess, no shell execution — unrecognized input
/// prints a usage hint and makes no request.
pub fn console_page(csrf: &str) -> String {
    let main = r##"
<div class="mut" style="margin-bottom:8px">Runs this app's own commands only — the same safe actions as the buttons. Type <span class="mono">help</span>.</div>
<div id="out" class="console-out" aria-live="polite"></div>
<input id="cmd" class="console-in" style="margin-top:10px" placeholder="e.g. status  ·  search thesis  ·  scan D:\ --force" autofocus>"##;
    let script = r##"
const out=$("#out");
function print(s,cls){ const d=document.createElement("div"); if(cls)d.className=cls; d.textContent=s; out.appendChild(d); out.scrollTop=out.scrollHeight; }
function printJSON(o){ print(JSON.stringify(o,null,2)); }
const HELP=`Commands:
  status                         catalog summary
  duplicates                     list duplicate groups
  search <query> [--category c] [--status s]
  scan <path> [--force]          queue a scan
  quarantine <id> [id ...]       quarantine file ids
  repack <id>                    remove an in-archive duplicate
  forget <volumeId>              remove a drive from the catalog
  purge --all                    purge every mounted quarantine
  drives                         list drives
  help, clear`;
// naive shell-ish tokenizer: splits on whitespace, honours "double quotes".
function tokenize(line){ const m=line.match(/"[^"]*"|\S+/g)||[]; return m.map(t=>t.replace(/^"|"$/g,"")); }
function flag(toks,name){ const i=toks.indexOf("--"+name); if(i<0)return null; const v=toks[i+1]; toks.splice(i, v&&!v.startsWith("--")?2:1); return v||true; }
async function exec(line){
  print("$ "+line);
  const toks=tokenize(line); const cmd=(toks.shift()||"").toLowerCase();
  try{
    if(cmd==="help"||cmd===""){ print(HELP); return; }
    if(cmd==="clear"){ out.innerHTML=""; return; }
    if(cmd==="status"){ printJSON(await apiGet("/api/stats")); return; }
    if(cmd==="drives"){ printJSON(await apiGet("/api/drives")); return; }
    if(cmd==="duplicates"){ printJSON(await apiGet("/api/duplicates")); return; }
    if(cmd==="search"){ const cat=flag(toks,"category"), st=flag(toks,"status");
      const p=new URLSearchParams(); if(toks.length)p.set("q",toks.join(" ")); if(cat)p.set("category",cat); if(st)p.set("status",st);
      printJSON(await apiGet("/api/search?"+p.toString())); return; }
    if(cmd==="scan"){ const force=!!flag(toks,"force"); const path=toks.join(" ");
      if(!path){ print("usage: scan <path> [--force]","mut"); return; }
      printJSON(await apiPost("/api/scan",{path,force})); return; }
    if(cmd==="quarantine"){ const ids=toks.map(Number).filter(n=>!isNaN(n));
      if(!ids.length){ print("usage: quarantine <id> [id ...]","mut"); return; }
      printJSON(await apiPost("/api/quarantine",{quarantine_ids:ids})); return; }
    if(cmd==="repack"){ const id=Number(toks[0]); if(isNaN(id)){ print("usage: repack <id>","mut"); return; }
      printJSON(await apiPost("/api/repack",{entry_id:id})); return; }
    if(cmd==="forget"){ if(!toks[0]){ print("usage: forget <volumeId>","mut"); return; }
      printJSON(await apiPost("/api/forget-drive",{volume_id:toks[0]})); return; }
    if(cmd==="purge"){ if(flag(toks,"all")){ printJSON(await apiPost("/api/purge-all",{})); return; }
      print("only 'purge --all' is supported from the console; use the Drives page for a single drive.","mut"); return; }
    print("unknown command: "+cmd+" (try 'help')","mut");
  }catch(e){ print("error: "+e,"mut"); }
}
$("#cmd").addEventListener("keydown",e=>{ if(e.key==="Enter"){ const v=e.target.value; e.target.value=""; if(v.trim())exec(v.trim()); }});
print(HELP);"##;
    shell("console", csrf, "Console", main, script)
}
