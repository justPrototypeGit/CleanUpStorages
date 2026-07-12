//! Shared, self-contained UI shell: one design-system stylesheet, an inline SVG icon set, a
//! sidebar/toolbar renderer, and shared client helpers. Every page in `web.rs` renders through
//! `shell()` so the six screens stay visually identical and no markup is duplicated.

pub const STYLE: &str = r##"
@font-face{font-family:'Inter';src:url(/assets/InterVariable.woff2) format('woff2');
 font-weight:100 900;font-style:normal;font-display:swap;}
:root{color-scheme:light dark;--font-ui:'Inter',-apple-system,"Segoe UI",Roboto,system-ui,sans-serif;
 --bg:#ececee;--panel:#ffffffc4;--content:#ffffff;--elev:#ffffff;--fg:#1d1d1f;--mut:#6e6e73;
 --line:#00000014;--line-strong:#00000022;--accent:#0071e3;--accent-weak:#0071e314;
 --amber:#b25000;--amber-bg:#ff9f0a24;--red:#c9382b;--red-bg:#ff453a1f;
 --green:#1a7f37;--green-bg:#30d15824;--gray:#8a8a8e;
 --r-sm:6px;--r:9px;--r-lg:11px;
 --sh-sm:0 1px 1px #0000000a,0 1px 3px #0000000d;
 --sh-md:0 4px 14px #00000012,0 1px 3px #0000000d;--sh-lg:0 12px 30px #0000001a;
 --sidebar:240px;--topbar:52px;}
:root[data-theme="light"]{
 --bg:#ececee;--panel:#ffffffc4;--content:#ffffff;--elev:#ffffff;--fg:#1d1d1f;--mut:#6e6e73;
 --line:#00000014;--line-strong:#00000022;--accent:#0071e3;--accent-weak:#0071e314;
 --amber:#b25000;--amber-bg:#ff9f0a24;--red:#c9382b;--red-bg:#ff453a1f;
 --green:#1a7f37;--green-bg:#30d15824;--gray:#8a8a8e;}
@media (prefers-color-scheme:dark){:root{
 --bg:#161618;--panel:#1f1f22c4;--content:#232327;--elev:#2e2e33;--fg:#f5f5f7;--mut:#9a9aa0;
 --line:#ffffff14;--line-strong:#ffffff28;--accent:#0a84ff;--accent-weak:#0a84ff26;
 --amber:#ff9f0a;--amber-bg:#ff9f0a26;--red:#ff453a;--red-bg:#ff453a26;
 --green:#30d158;--green-bg:#30d15826;--gray:#98989d;}}
:root[data-theme="dark"]{
 --bg:#161618;--panel:#1f1f22c4;--content:#232327;--elev:#2e2e33;--fg:#f5f5f7;--mut:#9a9aa0;
 --line:#ffffff14;--line-strong:#ffffff28;--accent:#0a84ff;--accent-weak:#0a84ff26;
 --amber:#ff9f0a;--amber-bg:#ff9f0a26;--red:#ff453a;--red-bg:#ff453a26;
 --green:#30d158;--green-bg:#30d15826;--gray:#98989d;}
*{box-sizing:border-box;}
body{margin:0;font:13px/1.5 var(--font-ui);letter-spacing:-.003em;
 background:var(--bg);color:var(--fg);-webkit-font-smoothing:antialiased;text-rendering:optimizeLegibility;}
.mono{font-family:ui-monospace,"SF Mono","Cascadia Code",Consolas,monospace;font-variant-numeric:tabular-nums;font-size:.92em;}
h1,h2,h3{letter-spacing:-.014em;font-weight:600;margin-top:0;}
h3{font-size:14px;} h2{font-size:19px;}
aside.side{position:fixed;left:0;top:0;bottom:0;width:var(--sidebar);display:flex;flex-direction:column;
 background:var(--panel);backdrop-filter:blur(24px) saturate(180%);-webkit-backdrop-filter:blur(24px) saturate(180%);
 border-right:1px solid var(--line);padding:18px 12px;overflow-y:auto;}
aside.side h1{font-size:16px;margin:2px 8px 0;font-weight:650;}
aside.side .tagline{margin:0 8px 18px;font-size:11px;color:var(--mut);text-transform:uppercase;letter-spacing:.06em;}
nav a{display:flex;gap:10px;align-items:center;padding:6px 9px;margin:1px 0;border-radius:var(--r-sm);
 color:var(--fg);text-decoration:none;font-weight:500;font-size:13px;transition:background .12s,color .12s;}
nav a.active{background:var(--accent-weak);color:var(--accent);}
nav a:hover:not(.active){background:var(--line);color:var(--fg);}
nav a svg{width:18px;height:18px;flex:none;}
header.top{position:fixed;top:0;left:var(--sidebar);right:0;height:var(--topbar);display:flex;align-items:center;
 gap:12px;padding:0 24px;background:color-mix(in srgb,var(--bg) 72%,transparent);
 backdrop-filter:blur(24px) saturate(180%);-webkit-backdrop-filter:blur(24px) saturate(180%);
 border-bottom:1px solid var(--line);z-index:5;}
header.top strong{font-size:13.5px;font-weight:600;letter-spacing:-.01em;}
header.top .spacer{flex:1;}
main{margin-left:var(--sidebar);padding:calc(var(--topbar) + 28px) 40px 64px;}
main>*{max-width:1180px;margin-left:auto;margin-right:auto;}
.card{background:var(--content);border:1px solid var(--line-strong);border-radius:var(--r-lg);padding:18px;
 margin:0 0 14px;box-shadow:var(--sh-sm);}
.card.hover{transition:box-shadow .16s,transform .16s,border-color .16s;}
.card.hover:hover{box-shadow:var(--sh-md);transform:translateY(-1px);border-color:var(--line-strong);}
.grid{display:grid;grid-template-columns:repeat(12,1fr);gap:16px;}
.mut{color:var(--mut);} .row{display:flex;align-items:center;gap:8px;}
.btn{font:inherit;font-weight:500;padding:5px 12px;border-radius:var(--r-sm);border:1px solid var(--line-strong);
 background:var(--content);color:var(--fg);cursor:pointer;display:inline-flex;align-items:center;gap:6px;
 transition:background .12s,box-shadow .12s;box-shadow:var(--sh-sm);}
.btn:hover{background:var(--line);} .btn:active{background:var(--line-strong);}
.btn svg{width:14px;height:14px;}
.btn-primary{background:linear-gradient(color-mix(in srgb,var(--accent) 94%,#fff),var(--accent));
 border-color:color-mix(in srgb,var(--accent) 60%,#000);color:#fff;box-shadow:inset 0 1px 0 #ffffff2e,var(--sh-sm);}
.btn-primary:hover{filter:brightness(1.04);}
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
.progressbar>span{display:block;height:100%;border-radius:999px;
 background:linear-gradient(90deg,var(--accent),color-mix(in srgb,var(--accent) 70%,#fff));}
.console-out{font-family:ui-monospace,Consolas,monospace;white-space:pre-wrap;background:var(--content);
 border:1px solid var(--line);border-radius:var(--r);padding:14px;min-height:320px;max-height:60vh;overflow:auto;box-shadow:var(--sh-sm);}
.console-in{width:100%;font-family:ui-monospace,Consolas,monospace;padding:11px 12px;border-radius:var(--r-sm);
 border:1px solid var(--line-strong);background:var(--content);color:var(--fg);}
.cards{display:flex;flex-wrap:wrap;gap:16px;}
.cards .card{width:250px;margin:0;}
#group .cards{justify-content:center;}
#group .card{width:300px;}
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
.dot{width:10px;height:10px;border-radius:50%;flex:none;box-shadow:inset 0 0 0 1px #00000018;}
.leaf{display:flex;align-items:center;gap:8px;padding:4px 7px 4px 24px;border-radius:var(--r-sm);cursor:default;}
.leaf:hover{background:var(--line);}
.leaf .fname{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.leaf .meta{font-size:12px;white-space:nowrap;}
.leaf.dup{background:color-mix(in srgb,var(--dup) 9%,transparent);cursor:pointer;}
.leaf.hl{background:color-mix(in srgb,var(--dup) 24%,transparent);box-shadow:inset 0 0 0 1px var(--dup);}
.diamond{font-size:11px;font-weight:700;color:var(--dup);flex:none;}
.stat{font-size:23px;font-weight:600;letter-spacing:-.02em;}
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
#pbar.run>span{animation:indet 1.2s infinite ease-in-out;}
@keyframes indet{0%{margin-left:-40%}100%{margin-left:100%}}
select,input[type=text],input[type=search],textarea{background:var(--content);color:var(--fg);
 border:1px solid var(--line-strong);border-radius:var(--r-sm);padding:8px 10px;font:inherit;color-scheme:inherit;}
select:focus,input:focus,textarea:focus{outline:none;border-color:var(--accent);box-shadow:0 0 0 3px var(--accent-weak);}
option{background:var(--content);color:var(--fg);}
.seg{display:inline-flex;background:var(--line);border-radius:999px;padding:2px;gap:2px;}
.seg button{border:0;background:transparent;color:var(--mut);border-radius:999px;padding:4px 9px;
 cursor:pointer;display:flex;align-items:center;gap:5px;font:inherit;font-size:12px;}
.seg button.on{background:var(--content);color:var(--fg);box-shadow:var(--sh-sm);}
.seg button svg{width:14px;height:14px;}
.themebar{margin-top:auto;padding-top:14px;border-top:1px solid var(--line);}
.themebar .lbl{font-size:11px;color:var(--mut);text-transform:uppercase;letter-spacing:.05em;display:block;margin:0 4px 6px;}
"##;

pub const SHARED_JS: &str = r##"
const $=s=>document.querySelector(s);
const CSRF=(document.querySelector('meta[name="csrf"]')||{}).content||"";
function esc(s){return (s==null?"":String(s)).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));}
function fmtSize(n){if(n==null)return"—";const u=["B","KB","MB","GB","TB"];let i=0,x=Number(n);while(x>=1024&&i<u.length-1){x/=1024;i++;}return (i?x.toFixed(1):x)+" "+u[i];}
function fmtDate(t){return t?new Date(t*1000).toISOString().slice(0,10):"—";}
function hueOf(s){let h=0;for(let i=0;i<String(s).length;i++)h=(h*31+String(s).charCodeAt(i))>>>0;return h%360;}
function driveColor(id){return `hsl(${hueOf(id)},46%,52%)`;}
function dupColor(hash){return `hsl(${hueOf(hash)},42%,56%)`;}
async function apiGet(u){const r=await fetch(u);if(!r.ok)throw new Error(await r.text());return r.json();}
async function apiPost(u,body){const r=await fetch(u,{method:"POST",headers:{"content-type":"application/json","x-cleanup-token":CSRF},body:JSON.stringify(body||{})});if(!r.ok)throw new Error(await r.text());return r.json();}
function applyTheme(t){ if(t==='auto'){localStorage.removeItem('theme');delete document.documentElement.dataset.theme;}
  else{localStorage.setItem('theme',t);document.documentElement.dataset.theme=t;}
  for(const b of document.querySelectorAll('.themebar .seg button')) b.classList.toggle('on', b.dataset.theme===(t||'auto')); }
(function initTheme(){ const q=new URLSearchParams(location.search).get('theme');
  const cur=q||localStorage.getItem('theme')||'auto';
  document.addEventListener('DOMContentLoaded',()=>{
    for(const b of document.querySelectorAll('.themebar .seg button')){ b.onclick=()=>applyTheme(b.dataset.theme); }
    applyTheme(cur);
  });})();
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
    let themebar = format!(r##"<div class="themebar"><span class="lbl">Theme</span>
<div class="seg" role="group" aria-label="Theme">
<button data-theme="auto" title="Follow system">{}<span>Auto</span></button>
<button data-theme="light" title="Light">{}<span>Light</span></button>
<button data-theme="dark" title="Dark">{}<span>Dark</span></button></div></div>"##,
        icon("auto"), icon("light"), icon("dark"));
    format!(r##"<!doctype html><html lang="en"><head>
<script>(function(){{var u=new URLSearchParams(location.search).get('theme');var t=u||localStorage.getItem('theme');if(t&&t!=='auto')document.documentElement.dataset.theme=t;}})();</script>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="csrf" content="{csrf}"><title>CleanUpStorages — {title}</title>
<style>{style}</style></head><body>
<aside class="side"><h1>CleanUpStorages</h1><p class="tagline">Storage cleanup</p><nav>{nav}</nav>{themebar}</aside>
<header class="top"><strong>{title}</strong></header>
<main>{main_html}</main>
<script>{shared}</script><script>{page_script}</script>
</body></html>"##,
        csrf = csrf, title = title, style = STYLE, nav = nav, themebar = themebar, main_html = main_html,
        shared = SHARED_JS, page_script = page_script)
}

pub fn overview_page(csrf: &str) -> String {
    let main = r##"
<section class="card" style="padding:20px 22px">
  <div class="mut" style="font-size:10.5px;text-transform:uppercase;letter-spacing:.07em;font-weight:600">System health</div>
  <h2 id="hero" class="stat" style="margin:7px 0 3px">…</h2>
  <div class="mut" id="hero-sub" style="font-size:12.5px"></div>
</section>
<div class="grid">
  <div class="card hover" style="grid-column:span 5"><h3 style="margin-top:0">Duplicate groups</h3>
    <div class="stat" id="dupe-count">…</div>
    <div class="mut" id="dupe-reclaim"></div>
    <a class="btn btn-primary" href="/review" style="margin-top:14px;text-decoration:none">Review duplicates</a></div>
  <div class="card hover" style="grid-column:span 7"><h3 style="margin-top:0">Reclaimable space</h3>
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
     <div style="display:flex;justify-content:space-between"><span><span class="dot" style="background:${driveColor(d.volume_id)};display:inline-block;margin-right:6px"></span>${esc(d.display_name||d.label)}</span><span class="mono">${fmtSize(d.reclaimable_bytes)}</span></div>
     <div class="progressbar"><span style="width:${Math.round(100*(d.reclaimable_bytes||0)/max)}%"></span></div></div>`).join("")||'<span class="mut">Nothing to reclaim.</span>';
  const acts=await apiGet("/api/activity");
  $("#activity").innerHTML=acts.length?acts.map(a=>`<div style="padding:6px 0;border-bottom:1px solid var(--line)">
     <span>${esc(a.summary)}</span> <span class="mut mono" style="float:right">${fmtDate(a.occurred_at)}</span></div>`).join(""):"No activity yet.";
}
init().catch(e=>{$("#activity").textContent="Error: "+e;});"##;
    shell("overview", csrf, "Overview", main, script)
}

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
  for(const d of drives.values()) reconcile(d);
  return drives;
}
// A .zip catalogued both as a loose file AND via its entries would otherwise show twice. Fold the
// loose archive file's own size onto its archive node and drop the duplicate leaf.
function reconcile(node){
  const keep=[];
  for(const f of node.files){ const a=node.children.get(f.name); if(a&&a.archive){ a.selfHit=f.hit; } else keep.push(f); }
  node.files=keep;
  for(const c of node.children.values()) reconcile(c);
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
    const ico=child.archive?'<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" style="opacity:.55;margin-right:5px;vertical-align:-1px"><rect x="3" y="4" width="18" height="4" rx="1"/><path d="M5 8v11a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8M10 12h4"/></svg>':'';
    const sz=child.archive&&child.selfHit?` <span class="mut mono" style="font-size:11px">${fmtSize(child.selfHit.size_bytes)}</span>`:'';
    html+=`<details class="folder"><summary>${ico}${esc(child.name)}${badge}${sz}</summary>${renderFolder(child)}</details>`;
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
  for(const v of vs){ const o=document.createElement("option"); o.value=v.volume_id; o.textContent=v.display_name||v.label; sel.appendChild(o); }
  $("#q").addEventListener("input",debounced);
  for(const k of ["volume","category","status"]) $("#"+k).addEventListener("change",run);
  run();
}
init();"##;
    shell("browse", csrf, "Browse", main, script)
}

pub fn review_page(csrf: &str) -> String {
    let main = r##"
<div style="max-width:900px;margin:0 auto">
  <div class="mut" id="progress" style="margin-bottom:16px;text-align:center"></div>
  <div id="group"></div>
  <div style="display:flex;gap:12px;margin-top:24px;align-items:center;justify-content:center">
    <button class="btn" id="skip">Skip this group</button>
    <button class="btn btn-primary" id="confirm">Keep selected, quarantine the rest</button>
  </div>
  <p class="mut" style="font-size:11px;text-align:center;margin-top:14px">Nothing is deleted — copies move to a recoverable <span class="mono">_ToDelete</span> folder until you purge.</p>
  <div class="mut" id="msg" style="margin-top:10px;min-height:1.4em;text-align:center"></div>
</div>"##;
    let script = r##"
let groups=[],idx=0,keepId=null;
async function load(){
  try{ groups=await apiGet("/api/duplicates"); }catch(e){ $("#msg").textContent="Load error: "+e; return; }
  idx=0; render();
}
function render(){
  if(idx>=groups.length){ $("#progress").textContent=""; $("#group").innerHTML="<p>All duplicate groups reviewed. 🎉</p>"; $("#confirm").style.display="none"; $("#skip").style.display="none"; return; }
  const g=groups[idx]; keepId=g.suggested_keep_id;
  $("#progress").textContent=`Group ${idx+1} of ${groups.length} · ${g.members.length} copies`;
  $("#group").innerHTML=`<div class="cards">${g.members.map(m=>card(m)).join("")}</div>`;
  for(const el of document.querySelectorAll(".cards .card")) el.addEventListener("click",()=>{ keepId=Number(el.dataset.id); paint(); });
  paint();
  for(const b of document.querySelectorAll(".repack")) b.addEventListener("click", async (ev)=>{
    ev.stopPropagation();
    const id=Number(b.dataset.id);
    b.disabled=true; $("#msg").textContent="Repacking archive…";
    try{
      const j=await apiPost("/api/repack",{entry_id:id});
      $("#msg").textContent=`Removed '${j.removed_entry}' from its archive (${j.retained_entries} kept). Original saved in _ToDelete.`; idx++; render();
    }catch(e){ $("#msg").textContent="Repack error: "+e; b.disabled=false; }
  });
}
function card(m){
  const img=(m.category==="photo"&&m.mounted)?`<img class="thumb" loading="lazy" src="/api/preview/${m.id}" onerror="this.replaceWith(Object.assign(document.createElement('div'),{className:'noimg',textContent:'no preview'}))">`:`<div class="noimg">${m.mounted?"no preview":"drive not connected"}</div>`;
  const arch = m.is_loose ? "" :
    (m.id===keepId ? `<div class="arch">inside archive</div>`
     : m.mounted ? `<button class="btn btn-danger repack" data-id="${m.id}">Remove from archive</button>`
                 : `<div class="arch">drive not connected</div>`);
  return `<div class="card" data-id="${m.id}">${img}
    <div class="mono" style="word-break:break-all;font-size:12px;margin:10px 0 6px">${esc(m.location)}</div>
    <div class="mut" style="font-size:12px"><b style="color:var(--fg)">${esc(m.volume_label||m.volume_id)}</b></div>
    <div class="mut" style="font-size:12px">${fmtSize(m.size_bytes)} · created ${fmtDate(m.created_time)}</div>
    <div class="mut" style="font-size:12px">status: ${esc(m.status)}</div>${arch}
    <div class="badge kept-badge" style="visibility:hidden">✓ keep this</div></div>`;
}
function paint(){
  for(const el of document.querySelectorAll(".cards .card")){
    const on=Number(el.dataset.id)===keepId;
    el.classList.toggle("keep",on);
    el.querySelector(".kept-badge").style.visibility=on?"visible":"hidden";
  }
}
$("#confirm").addEventListener("click",async()=>{
  const g=groups[idx]; if(!g)return;
  const victims=g.members.filter(m=>m.id!==keepId&&m.is_loose).map(m=>m.id);
  if(victims.length===0){ $("#msg").textContent="Nothing to quarantine (the other copies are inside archives)."; return; }
  $("#confirm").disabled=true; $("#msg").textContent="Quarantining…";
  try{
    const j=await apiPost("/api/quarantine",{quarantine_ids:victims});
    let m=`Quarantined ${j.quarantined}, skipped ${j.skipped}.`; if(j.unmounted_volumes&&j.unmounted_volumes.length) m+=" Some drives not connected."; if(j.errors&&j.errors.length) m+=" Errors: "+j.errors.join("; "); $("#msg").textContent=m; idx++; render();
  }catch(e){ $("#msg").textContent="Error: "+e; }
  $("#confirm").disabled=false;
});
$("#skip").addEventListener("click",()=>{ idx++; $("#msg").textContent=""; render(); });
load();"##;
    shell("duplicates", csrf, "Review duplicates", main, script)
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
  $("#drives").innerHTML = drives.length? drives.map(d=>`<div class="card" data-vid="${esc(d.volume_id)}" data-path="${esc(d.mount_path||'')}" data-desc="${esc(d.description||'')}">
    <div style="display:flex;justify-content:space-between;align-items:start">
      <div><h3 style="margin:0;display:flex;align-items:center;gap:8px"><span class="dot" style="background:${driveColor(d.volume_id)}"></span>${esc(d.display_name||d.label)}</h3>
        ${d.description?`<div class="mut" style="font-size:12px;margin-top:2px">${esc(d.description)}</div>`:""}
        <div class="mut" style="font-size:12px">${d.connected?'<span class="pill active">connected</span>':'<span class="pill purged">offline</span>'}
          · ${d.active_files.toLocaleString()} files · last scan ${fmtDate(d.last_seen_at)}
          ${d.has_errors?' · <span class="pill missing">had scan errors</span>':''}</div></div>
      <div class="mut mono">${fmtSize(d.reclaimable_bytes)} reclaimable</div></div>
    ${bar(d)}
    <div class="row" style="margin-top:12px">
      <button class="btn rescan" ${d.connected?'':'disabled'}>${d.has_errors?'Repair (rescan)':'Rescan'}</button>
      <button class="btn edit">Edit…</button>
      <button class="btn btn-danger forget">Forget…</button></div>
    <div class="edit-form" style="display:none;margin-top:12px;border-top:1px solid var(--line);padding-top:12px">
      <input class="ef-name" type="text" placeholder="Custom name (blank = detected)" style="width:100%;margin-bottom:8px" value="${esc(d.display_name||'')}">
      <input class="ef-desc" type="text" placeholder="Short description" style="width:100%;margin-bottom:8px" value="${esc(d.description||'')}">
      <div class="row"><button class="btn btn-primary ef-save">Save</button><button class="btn ef-cancel">Cancel</button></div>
    </div></div>`).join("")
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
    const form=c.querySelector(".edit-form");
    c.querySelector(".edit").onclick=()=>{ form.style.display = form.style.display==="none"?"block":"none"; };
    c.querySelector(".ef-cancel").onclick=()=>{ form.style.display="none"; };
    c.querySelector(".ef-save").onclick=async()=>{
      try{
        await apiPost("/api/rename-drive",{volume_id:c.dataset.vid,
          name:c.querySelector(".ef-name").value, description:c.querySelector(".ef-desc").value});
        $("#msg").textContent="Drive updated."; load();
      }catch(e){ $("#msg").textContent="Error: "+e; }
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

pub fn scan_page(csrf: &str) -> String {
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
    let script = r##"
async function loadDrives(){
  try{
    const ds=await apiGet("/api/detected-drives");
    if(!ds.length){ $("#drives").innerHTML='<span class="mut">No drives detected. Type a path below.</span>'; return; }
    $("#drives").innerHTML=ds.map(d=>`<div class="card" data-path="${esc(d.mount_path)}" style="cursor:pointer">
      <div style="font-weight:600">${esc(d.mount_path)}</div>
      <div class="mut" style="font-size:12px;margin-top:4px">${d.catalogued?("Catalogued as "+esc(d.volume_label||"—")+" · rescan"):"New drive"}</div>
      </div>`).join("");
    for(const el of document.querySelectorAll("#drives [data-path]")) el.addEventListener("click",()=>{ $("#path").value=el.dataset.path; });
  }catch(e){ $("#drives").innerHTML='<span class="mut">Could not list drives: '+esc(String(e))+'</span>'; }
}
$("#browse").addEventListener("click",async()=>{
  try{
    const j=await apiPost("/api/pick-folder");
    if(j.path) $("#path").value=j.path;
  }catch(e){ $("#running").textContent="Folder picker error: "+e; }
});
$("#scan").addEventListener("click",async()=>{
  const path=$("#path").value.trim(); if(!path){ $("#running").textContent="Enter a path first."; return; }
  const force=$("#force").checked;
  try{
    await apiPost("/api/scan",{path,force});
    poll();
  }catch(e){ $("#running").innerHTML='<span style="color:var(--red)">Scan error: '+esc(String(e))+'</span>'; }
});
function setTiles(on){ $("#tiles").style.display=on?"flex":"none"; const p=$("#pbar"); p.style.display=on?"block":"none"; p.classList.toggle("run",on); }
async function poll(){
  try{
    const s=await apiGet("/api/scan/status");
    if(s.running){ const r=s.running;
      $("#status-title").textContent="Scanning…"; $("#status-sub").textContent=r.path;
      $("#t-hashed").textContent=r.hashed.toLocaleString(); $("#t-skip").textContent=r.skipped.toLocaleString();
      $("#t-err").textContent=r.errors.toLocaleString(); $("#t-arch").textContent=r.archive_entries.toLocaleString();
      setTiles(true); $("#running").textContent="";
    } else { $("#status-title").textContent="Live status"; $("#status-sub").textContent=""; setTiles(false); $("#running").textContent="No scan running."; }
    $("#queued").textContent = s.queued.length ? ("Queued: "+s.queued.join(", ")) : "";
    $("#recent").innerHTML = s.recent.length ? s.recent.map(r=>{
      const mark = r.error_message ? '<span class="pill missing">error</span>' : '<span class="pill active">done</span>';
      const msg = r.error_message ? `<span style="color:var(--red)">${esc(r.error_message)}</span>` : `${r.hashed} hashed · ${r.skipped} unchanged · ${r.errors} errors · ${r.archive_entries} archive entries · ${r.marked_missing} newly missing`;
      return `<div class="row" style="padding:8px 0;border-bottom:1px solid var(--line);gap:10px">${mark}<span class="mono" style="flex:none">${esc(r.path)}</span><span class="mut" style="font-size:12px">${msg}</span></div>`;
    }).join("") : "None yet.";
    if(s.running || s.queued.length) setTimeout(poll, 1500);
  }catch(e){ /* stop polling on error */ }
}
loadDrives(); poll();"##;
    shell("scan", csrf, "Scan a drive", main, script)
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
