//! `selene viz` — turn the whole code graph into ONE self-contained, offline
//! HTML page: a dark "galaxy" of nodes + links rendered by a dependency-free 2D
//! force-directed graph on `<canvas>`.
//!
//! This module is the pure transform + template half (no I/O): given the graph's
//! [`Node`]s and [`Edge`]s it (1) drops low-signal kinds, (2) ranks the rest by
//! graph degree and caps to `max_nodes` so a browser can actually draw it, (3)
//! serializes a compact `{nodes, links, meta}` JSON, and (4) inlines it into a
//! template that carries all of its CSS/JS inline — no CDN, no `src=`, no fetch.
//! `cmd::viz` owns opening the store and writing the file.
//!
//! **Why a cap.** A large repo is ~350k nodes; no browser force-sim survives that.
//! The default keeps the most-connected ~2000 nodes (the structural backbone) and
//! the page states "showing N of M". `--all-kinds`/`--max-nodes` widen it.

use std::collections::{HashMap, HashSet};

use selene_core::{Edge, Node, NodeKind};

/// Options controlling the export (mirrors the `viz` subcommand flags).
pub struct VizOptions {
    /// Hard cap on rendered nodes (most-connected first). Always ≥ 1.
    pub max_nodes: usize,
    /// Keep the low-signal kinds ([`is_low_signal`]) that are dropped by default.
    pub all_kinds: bool,
    /// Human label for the page header (the project root path).
    pub root_label: String,
}

/// The rendered page plus the counts `cmd::viz` echoes to the user.
pub struct VizDoc {
    pub html: String,
    pub shown_nodes: usize,
    pub total_nodes: usize,
    pub shown_edges: usize,
    pub total_edges: usize,
}

/// Kinds dropped from the default view: high-count, low-signal structural noise.
/// A file, an import, a local variable, or a parameter rarely carries the *flow*
/// a galaxy is meant to show, and there are a lot of them. `--all-kinds` keeps them.
fn is_low_signal(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File | NodeKind::Import | NodeKind::Variable | NodeKind::Parameter
    )
}

/// Build the self-contained HTML page from the full graph.
///
/// Selection strategy: degree is counted over **all** edges (so a hub stays a hub
/// even if some of its neighbors get dropped), kinds are filtered, the survivors
/// are sorted by degree (desc; name then id break ties, so the output is
/// deterministic) and truncated to `max_nodes`. Links are kept only when *both*
/// endpoints survived, self-loops dropped, and each `(source, target, kind)`
/// de-duplicated.
/// Is this path test/vendored/generated noise — code a first map should not
/// show? (The consensus default across dependency-cruiser/madge/NDepend/
/// typescript-graph: third-party and test scaffolding are excluded up front,
/// with the hidden count surfaced so the map is trusted.) Segment and
/// filename checks, no regex needed.
fn is_noise_path(path: &str) -> bool {
    const NOISE_DIRS: [&str; 22] = [
        "node_modules",
        "vendor",
        "third_party",
        "dist",
        "build",
        "target",
        "generated",
        "__generated__",
        "__tests__",
        "tests",
        "test",
        "spec",
        "specs",
        "__mocks__",
        "mocks",
        "fixtures",
        "fixture",
        "e2e",
        "examples",
        "example",
        "docs",
        "doc",
    ];
    let file = path.rsplit('/').next().unwrap_or(path);
    if path
        .split('/')
        .take(path.split('/').count().saturating_sub(1))
        .any(|seg| NOISE_DIRS.contains(&seg))
    {
        return true;
    }
    for pat in [".test.", ".spec.", ".mock.", ".stories.", "_test.", "_spec."] {
        if file.contains(pat) {
            return true;
        }
    }
    file.ends_with(".d.ts") || file.ends_with(".min.js")
}

/// The module (directory-prefix group) of a path at `depth` segments.
fn module_of(path: &str, depth: usize) -> String {
    let dir_end = path.rfind('/').unwrap_or(0);
    let dir = &path[..dir_end];
    if dir.is_empty() {
        return "(root)".to_string();
    }
    let segs: Vec<&str> = dir.split('/').collect();
    segs[..depth.min(segs.len())].join("/")
}

pub fn build_html(nodes: &[Node], edges: &[Edge], opts: &VizOptions) -> VizDoc {
    let total_nodes = nodes.len();
    let total_edges = edges.len();
    let max_nodes = opts.max_nodes.max(1);

    // Degree over the full edge set — the importance signal for the cap.
    let mut degree: HashMap<&str, u32> = HashMap::new();
    for e in edges {
        *degree.entry(e.source.as_str()).or_default() += 1;
        *degree.entry(e.target.as_str()).or_default() += 1;
    }

    // --- the noise pass -----------------------------------------------------
    // Test/vendored/generated code never makes the first map; the count is
    // shipped so the page can say "N hidden" instead of silently lying.
    let noise_hidden = nodes
        .iter()
        .filter(|n| !is_low_signal(n.kind) && is_noise_path(&n.file_path))
        .count();

    let mut kept: Vec<&Node> = nodes
        .iter()
        .filter(|n| opts.all_kinds || !is_low_signal(n.kind))
        .filter(|n| !is_noise_path(&n.file_path))
        .collect();
    kept.sort_by(|a, b| {
        let da = degree.get(a.id.as_str()).copied().unwrap_or(0);
        let db = degree.get(b.id.as_str()).copied().unwrap_or(0);
        db.cmp(&da)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    kept.truncate(max_nodes);

    // --- the module map (the DEFAULT view) ----------------------------------
    // Aggregate the FULL app graph (all kept-quality nodes, not just the
    // capped symbol set) into directory-prefix modules — the unit every
    // surviving code-map tool defaults to. Depth auto-tunes: the deepest
    // prefix that still lands at a readable module count.
    let app_nodes: Vec<&Node> = nodes
        .iter()
        .filter(|n| !is_low_signal(n.kind) && !is_noise_path(&n.file_path))
        .collect();
    let mut mod_depth = 1usize;
    for d in (1..=4).rev() {
        let count = app_nodes
            .iter()
            .map(|n| module_of(&n.file_path, d))
            .collect::<HashSet<_>>()
            .len();
        if count <= 36 {
            mod_depth = d;
            break;
        }
    }
    let mut mod_index: HashMap<String, usize> = HashMap::new();
    let mut mod_members: Vec<u32> = Vec::new();
    let mut node_mod: HashMap<&str, usize> = HashMap::new();
    for n in &app_nodes {
        let m = module_of(&n.file_path, mod_depth);
        let idx = *mod_index.entry(m).or_insert_with(|| {
            mod_members.push(0);
            mod_members.len() - 1
        });
        mod_members[idx] += 1;
        node_mod.insert(n.id.as_str(), idx);
    }
    // Cross-module edge counts (directed) + intra counts, over the FULL edges.
    let mut pair_counts: HashMap<(usize, usize), u32> = HashMap::new();
    let mut intra_counts: Vec<u32> = vec![0; mod_members.len()];
    for e in edges {
        if let (Some(&sm), Some(&tm)) = (
            node_mod.get(e.source.as_str()),
            node_mod.get(e.target.as_str()),
        ) {
            if sm == tm {
                intra_counts[sm] += 1;
            } else {
                *pair_counts.entry((sm, tm)).or_default() += 1;
            }
        }
    }
    let mut mod_labels: Vec<String> = vec![String::new(); mod_members.len()];
    for (label, idx) in &mod_index {
        mod_labels[*idx] = label.clone();
    }
    let modules_json: Vec<serde_json::Value> = mod_labels
        .iter()
        .enumerate()
        .map(|(i, p)| {
            serde_json::json!({ "p": p, "n": mod_members[i], "e": intra_counts[i] })
        })
        .collect();
    let mut mod_links_sorted: Vec<((usize, usize), u32)> = pair_counts.into_iter().collect();
    mod_links_sorted.sort_by_key(|((s, t), _)| (*s, *t)); // deterministic output
    let mod_links_json: Vec<serde_json::Value> = mod_links_sorted
        .iter()
        .map(|((s, t), c)| serde_json::json!({ "s": s, "t": t, "c": c }))
        .collect();

    // id -> dense index into the emitted node array.
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(kept.len());
    for (i, n) in kept.iter().enumerate() {
        index.insert(n.id.as_str(), i);
    }

    let nodes_json: Vec<serde_json::Value> = kept
        .iter()
        .map(|n| {
            serde_json::json!({
                "n": n.name,
                "k": n.kind.as_str(),
                "f": n.file_path,
                "l": n.start_line,
                "d": degree.get(n.id.as_str()).copied().unwrap_or(0),
                "m": node_mod.get(n.id.as_str()).map(|i| *i as i64).unwrap_or(-1),
            })
        })
        .collect();

    let mut seen: HashSet<(usize, usize, &str)> = HashSet::new();
    let mut links_json: Vec<serde_json::Value> = Vec::new();
    for e in edges {
        if let (Some(&s), Some(&t)) = (index.get(e.source.as_str()), index.get(e.target.as_str())) {
            if s == t {
                continue; // self-loops add clutter, not signal
            }
            if seen.insert((s, t, e.kind.as_str())) {
                links_json.push(serde_json::json!({ "s": s, "t": t, "k": e.kind.as_str() }));
            }
        }
    }

    let shown_nodes = nodes_json.len();
    let shown_edges = links_json.len();

    let data = serde_json::json!({
        "nodes": nodes_json,
        "links": links_json,
        "modules": modules_json,
        "modLinks": mod_links_json,
        "meta": {
            "shown": shown_nodes,
            "total": total_nodes,
            "edges": shown_edges,
            "totalEdges": total_edges,
            "root": opts.root_label,
            "maxNodes": max_nodes,
            "allKinds": opts.all_kinds,
            "noiseHidden": noise_hidden,
        }
    });

    // `to_string` on an owned `Value` only fails on a non-string map key, which
    // this shape never has — fall back to an empty graph rather than unwrap.
    let data_str = serde_json::to_string(&data)
        .unwrap_or_else(|_| r#"{"nodes":[],"links":[],"meta":{}}"#.to_string());
    // The JSON is embedded as a JS object literal inside <script>. Escaping every
    // '<' to its < form (valid JS-in-string) makes a "</script>" breakout
    // impossible regardless of what a symbol name or file path contains.
    let data_str = data_str.replace('<', "\\u003c");

    let html = TEMPLATE
        .replace("__DATA__", &data_str)
        .replace("__TITLE__", &html_escape(&opts.root_label));

    VizDoc {
        html,
        shown_nodes,
        total_nodes,
        shown_edges,
        total_edges,
    }
}

/// Minimal HTML-text escape for the one interpolated value (the title).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The whole page: one file, everything inline. `__DATA__` (a JS object literal)
/// and `__TITLE__` (HTML-escaped text) are the only substitution points.
const TEMPLATE: &str = r####"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>selene galaxy — __TITLE__</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { margin: 0; height: 100%; overflow: hidden; }
  body {
    font: 13px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    color: #e6e8ef;
    background:
      radial-gradient(1200px 800px at 30% 20%, #171a2b 0%, #0a0b12 55%, #05060a 100%);
  }
  #stage { position: fixed; inset: 0; }
  canvas { display: block; width: 100%; height: 100%; cursor: grab; }
  canvas.grabbing { cursor: grabbing; }

  .panel {
    position: fixed;
    background: rgba(15,17,28,0.82);
    border: 1px solid rgba(255,255,255,0.08);
    border-radius: 10px;
    backdrop-filter: blur(8px);
    box-shadow: 0 8px 30px rgba(0,0,0,0.45);
  }

  #hud { top: 12px; left: 12px; padding: 10px 12px; max-width: 46vw; }
  #hud h1 { margin: 0 0 2px; font-size: 13px; font-weight: 650; letter-spacing: .2px; }
  #hud .sub { color: #97a0b8; font-size: 11px; word-break: break-all; }
  #hud .counts { margin-top: 6px; color: #c6ccdd; font-size: 11px; }
  #hud .counts b { color: #fff; font-weight: 650; }
  #hud .hint { margin-top: 6px; color: #6d7690; font-size: 10.5px; }

  #controls { top: 12px; right: 12px; padding: 8px; display: flex; gap: 6px; align-items: center; }
  #controls input {
    background: rgba(0,0,0,0.35); border: 1px solid rgba(255,255,255,0.12);
    color: #e6e8ef; border-radius: 7px; padding: 6px 9px; width: 180px; outline: none;
  }
  #controls input::placeholder { color: #6d7690; }
  #controls button {
    background: rgba(255,255,255,0.06); border: 1px solid rgba(255,255,255,0.12);
    color: #cdd3e3; border-radius: 7px; padding: 6px 9px; cursor: pointer; font-size: 12px;
  }
  #controls button:hover { background: rgba(255,255,255,0.12); }
  #controls button.on { background: rgba(110,168,254,0.25); border-color: rgba(110,168,254,0.6); color: #fff; }

  #legend { bottom: 12px; left: 12px; padding: 10px 12px; max-height: 46vh; overflow: auto; min-width: 130px; }
  #legend .lt { color: #97a0b8; font-size: 10.5px; text-transform: uppercase; letter-spacing: .6px; margin-bottom: 6px; }
  .lrow { display: flex; align-items: center; gap: 8px; padding: 2px 0; cursor: pointer; user-select: none; }
  .lrow.off { opacity: .38; }
  .sw { width: 10px; height: 10px; border-radius: 3px; flex: none; }
  .lrow .ln { color: #d6dbea; }
  .lrow .lc { margin-left: auto; color: #7c86a1; font-variant-numeric: tabular-nums; }

  #details { bottom: 12px; right: 12px; padding: 12px 14px; width: 300px; display: none; }
  #details.show { display: block; }
  #details .dk { display: inline-block; font-size: 10px; text-transform: uppercase; letter-spacing: .5px;
    padding: 2px 7px; border-radius: 999px; color: #05060a; font-weight: 700; }
  #details .dn { margin: 8px 0 2px; font-size: 15px; font-weight: 650; word-break: break-word; }
  #details .df { color: #97a0b8; font-size: 11px; word-break: break-all; }
  #details .dm { margin-top: 8px; color: #c6ccdd; font-size: 11px; }
  #details .dm b { color: #fff; }
  #details .close { position: absolute; top: 8px; right: 10px; cursor: pointer; color: #7c86a1; font-size: 16px; }
  #details .close:hover { color: #fff; }
</style>
</head>
<body>
  <div id="stage"><canvas id="c"></canvas></div>

  <div id="hud" class="panel">
    <h1>selene galaxy</h1>
    <div class="sub" id="root"></div>
    <div class="counts" id="counts"></div>
    <div class="hint" id="hint">click a module = drill in · scroll = zoom · drag = pan/move</div>
  </div>

  <div id="controls" class="panel">
    <button id="back" title="Back to the module map" style="display:none">← map</button>
    <button id="mode-map" class="on" title="Architecture map (modules)">Map</button>
    <button id="mode-sym" title="Every symbol">Symbols</button>
    <input id="search" type="search" placeholder="search…" autocomplete="off" spellcheck="false">
    <button id="fit" title="Fit graph to view">Fit</button>
  </div>

  <div id="legend" class="panel">
    <div class="lt" id="legend-title">Modules (click to drill in)</div>
    <div id="legend-rows"></div>
  </div>

  <div id="details" class="panel">
    <span class="close" id="details-close">×</span>
    <span class="dk" id="d-kind"></span>
    <div class="dn" id="d-name"></div>
    <div class="df" id="d-file"></div>
    <div class="dm" id="d-meta"></div>
  </div>

<script>
const DATA = __DATA__;

// ---- palette: node fill per kind ------------------------------------------
const COLORS = {
  function: "#6ea8fe", method: "#63e6be", class: "#ffd43b", struct: "#ffa94d",
  interface: "#da77f2", trait: "#f783ac", protocol: "#e599f7", enum: "#a9e34b",
  enum_member: "#c0eb75", constant: "#ffe066", type_alias: "#66d9e8",
  module: "#74c0fc", namespace: "#4dabf7", component: "#ff8787", route: "#69db7c",
  property: "#b2f2bb", field: "#96f2d7", export: "#ffec99", import: "#ced4da",
  variable: "#adb5bd", parameter: "#868e96", file: "#7a8296"
};
const colorFor = k => COLORS[k] || "#dee2e6";
const MODULE_PALETTE = ["#6ea8fe","#63e6be","#ffd43b","#da77f2","#ffa94d","#69db7c",
  "#f783ac","#66d9e8","#a9e34b","#ff8787","#ffe066","#4dabf7","#e599f7","#96f2d7"];

// ---- model ----------------------------------------------------------------
const symbols = DATA.nodes.map((d, i) => ({
  n: d.n, k: d.k, f: d.f, l: d.l, deg: d.d || 0, m: d.m,
  i, x: 0, y: 0, vx: 0, vy: 0, match: false, kind: "sym"
}));
const symLinks = DATA.links.map(l => ({ s: l.s, t: l.t, k: l.k }));

const modules = (DATA.modules || []).map((d, i) => ({
  n: d.p, k: "module", mem: d.n, intra: d.e, deg: 0,
  i, x: 0, y: 0, vx: 0, vy: 0, match: false, kind: "mod",
  color: MODULE_PALETTE[i % MODULE_PALETTE.length]
}));
const modLinks = (DATA.modLinks || []).map(l => ({ s: l.s, t: l.t, c: l.c }));
for (const l of modLinks) { modules[l.s].deg += l.c; modules[l.t].deg += l.c; }

// ---- the current view -----------------------------------------------------
// "map"      : the architecture map — one node per module, edge width = count.
// "symbols"  : symbol dots (all, or scoped to one module after a drill-down).
let view = "map";
let scopeMod = -1; // module index when drilled in, else -1
let curNodes = [], curLinks = [];
let PHYS = {};

function seedDisk(list, spread) {
  const R0 = spread * Math.sqrt(Math.max(1, list.length));
  for (const nd of list) {
    const a = Math.random() * Math.PI * 2, r = R0 * Math.sqrt(Math.random());
    nd.x = Math.cos(a) * r; nd.y = Math.sin(a) * r; nd.vx = 0; nd.vy = 0;
  }
}

function setView(v, modIdx) {
  view = v; scopeMod = (modIdx === undefined ? -1 : modIdx);
  selected = null; hovered = null; document.getElementById("details").classList.remove("show");
  if (view === "map") {
    curNodes = modules; curLinks = modLinks;
    PHYS = { REPULSE: 5200, RANGE: 620, SPRING: 0.02, REST: 170, GRAVITY: 0.03 };
    seedDisk(curNodes, 60);
  } else {
    const scoped = scopeMod >= 0 ? symbols.filter(nd => nd.m === scopeMod) : symbols;
    const keep = new Set(scoped.map(nd => nd.i));
    curNodes = scoped;
    curLinks = symLinks.filter(l => keep.has(l.s) && keep.has(l.t))
      .map(l => ({ s: l.s, t: l.t, k: l.k }));
    PHYS = { REPULSE: 620, RANGE: 220, SPRING: 0.012, REST: 46, GRAVITY: 0.012 };
    seedDisk(curNodes, 32);
  }
  byIndex = new Map(curNodes.map(nd => [nd.i, nd]));
  alpha = 1;
  for (let i = 0; i < 90; i++) tick();
  fit();
  syncChrome();
}

let byIndex = new Map();
const nodeAt = i => byIndex.get(i);

// ---- physics --------------------------------------------------------------
const VELOCITY_DECAY = 0.82, ALPHA_DECAY = 0.018, ALPHA_MIN = 0.004;
let alpha = 1;

// ---- view / interaction state --------------------------------------------
const canvas = document.getElementById("c");
const ctx = canvas.getContext("2d");
let dpr = 1, cw = 0, ch = 0;
let scale = 1, tx = 0, ty = 0;
let hovered = null, selected = null;
const hiddenKinds = new Set();
let searchQ = "";
let dragNode = null, panning = false, downX = 0, downY = 0, moved = false;

function resize() {
  dpr = window.devicePixelRatio || 1;
  cw = canvas.clientWidth; ch = canvas.clientHeight;
  canvas.width = Math.round(cw * dpr);
  canvas.height = Math.round(ch * dpr);
}
window.addEventListener("resize", resize);
resize();

const toWorldX = sx => (sx - tx) / scale;
const toWorldY = sy => (sy - ty) / scale;
function radiusOf(nd) {
  if (nd.kind === "mod") return Math.min(46, 10 + Math.sqrt(nd.mem) * 1.9);
  return Math.min(22, 3 + Math.sqrt(nd.deg) * 1.5);
}

function fit() {
  if (!curNodes.length) return;
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (const nd of curNodes) {
    if (nd.x < minX) minX = nd.x; if (nd.y < minY) minY = nd.y;
    if (nd.x > maxX) maxX = nd.x; if (nd.y > maxY) maxY = nd.y;
  }
  const w = Math.max(1, maxX - minX), h = Math.max(1, maxY - minY);
  const pad = view === "map" ? 150 : 60;
  scale = Math.min((cw - pad) / w, (ch - pad) / h);
  scale = Math.max(0.02, Math.min(6, scale));
  tx = cw / 2 - (minX + maxX) / 2 * scale;
  ty = ch / 2 - (minY + maxY) / 2 * scale;
}

// ---- simulation -----------------------------------------------------------
function tick() {
  alpha += (0 - alpha) * ALPHA_DECAY;
  const { REPULSE, RANGE, SPRING, REST, GRAVITY } = PHYS;
  const RANGE2 = RANGE * RANGE;

  for (const nd of curNodes) {
    nd.vx += -nd.x * GRAVITY * alpha;
    nd.vy += -nd.y * GRAVITY * alpha;
  }

  const grid = new Map();
  const key = (cx, cy) => cx + "," + cy;
  for (const nd of curNodes) {
    const kk = key(Math.floor(nd.x / RANGE), Math.floor(nd.y / RANGE));
    let bucket = grid.get(kk);
    if (!bucket) { bucket = []; grid.set(kk, bucket); }
    bucket.push(nd);
  }
  for (const nd of curNodes) {
    const cx = Math.floor(nd.x / RANGE), cy = Math.floor(nd.y / RANGE);
    let seen = 0;
    for (let gx = cx - 1; gx <= cx + 1 && seen < 80; gx++) {
      for (let gy = cy - 1; gy <= cy + 1 && seen < 80; gy++) {
        const bucket = grid.get(key(gx, gy));
        if (!bucket) continue;
        for (const m of bucket) {
          if (m === nd) continue;
          let dx = nd.x - m.x, dy = nd.y - m.y;
          let d2 = dx * dx + dy * dy;
          if (d2 > RANGE2) continue;
          if (d2 < 0.01) { dx = (Math.random() - 0.5); dy = (Math.random() - 0.5); d2 = 0.01; }
          const f = REPULSE * alpha / d2;
          nd.vx += dx * f; nd.vy += dy * f;
          if (++seen >= 80) break;
        }
      }
    }
  }

  for (const l of curLinks) {
    const a = nodeAt(l.s), b = nodeAt(l.t);
    if (!a || !b) continue;
    let dx = b.x - a.x, dy = b.y - a.y;
    let d = Math.sqrt(dx * dx + dy * dy) || 0.01;
    const rest = view === "map" ? PHYS.REST + radiusOf(a) + radiusOf(b) : PHYS.REST;
    const f = (d - rest) * PHYS.SPRING * alpha / d;
    a.vx += dx * f; a.vy += dy * f;
    b.vx -= dx * f; b.vy -= dy * f;
  }

  for (const nd of curNodes) {
    if (nd === dragNode) continue;
    nd.vx *= VELOCITY_DECAY; nd.vy *= VELOCITY_DECAY;
    nd.x += nd.vx; nd.y += nd.vy;
  }
}

// ---- rendering ------------------------------------------------------------
function visible(nd) { return nd.kind === "mod" || !hiddenKinds.has(nd.k); }

function shortLabel(p) {
  // Keep as many TRAILING segments as fit — "svgenie-website/src/components"
  // and "src/components" must never collapse to the same "…/src/components".
  if (p.length <= 32) return p;
  const segs = p.split("/");
  let out = segs[segs.length - 1];
  for (let i = segs.length - 2; i >= 0; i--) {
    if (segs[i].length + 1 + out.length > 30) break;
    out = segs[i] + "/" + out;
  }
  return out.length < p.length ? "…/" + out : p;
}

function draw() {
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, cw, ch);
  const isMap = view === "map";

  // edges — quadratic bow; on the map, width/opacity carry the pair count.
  for (const l of curLinks) {
    const a = nodeAt(l.s), b = nodeAt(l.t);
    if (!a || !b || !visible(a) || !visible(b)) continue;
    const ax = a.x * scale + tx, ay = a.y * scale + ty;
    const bx = b.x * scale + tx, by = b.y * scale + ty;
    const hot = (selected && (a === selected || b === selected)) ||
                (hovered && (a === hovered || b === hovered));
    if (isMap) {
      const w = Math.min(8, 0.8 + Math.sqrt(l.c) * 0.55) * Math.min(1.4, Math.max(0.5, scale));
      ctx.lineWidth = w;
      ctx.strokeStyle = hot ? "rgba(150,180,255,0.8)"
        : "rgba(140,152,190," + Math.min(0.55, 0.10 + Math.log(1 + l.c) * 0.07) + ")";
    } else {
      ctx.lineWidth = Math.max(0.4, 0.6 * scale);
      ctx.strokeStyle = hot ? "rgba(150,180,255,0.55)" : "rgba(140,152,190,0.12)";
    }
    const mx = (ax + bx) / 2 - (by - ay) * 0.08;
    const my = (ay + by) / 2 + (bx - ax) * 0.08;
    ctx.beginPath();
    ctx.moveTo(ax, ay);
    ctx.quadraticCurveTo(mx, my, bx, by);
    ctx.stroke();
  }

  const dim = searchQ.length > 0;
  for (const nd of curNodes) {
    if (!visible(nd)) continue;
    const sx = nd.x * scale + tx, sy = nd.y * scale + ty;
    const r = Math.max(1.4, radiusOf(nd) * scale);
    const active = nd === selected || nd === hovered || nd.match;
    ctx.globalAlpha = dim && !nd.match && nd !== selected ? 0.18 : 1;
    ctx.fillStyle = nd.kind === "mod" ? nd.color : colorFor(nd.k);
    ctx.beginPath();
    ctx.arc(sx, sy, r, 0, Math.PI * 2);
    ctx.fill();
    if (nd.kind === "mod") {
      ctx.globalAlpha *= 0.35;
      ctx.lineWidth = Math.max(1.5, r * 0.16);
      ctx.strokeStyle = nd.color;
      ctx.beginPath();
      ctx.arc(sx, sy, r + Math.max(2, r * 0.18), 0, Math.PI * 2);
      ctx.stroke();
      ctx.globalAlpha = dim && !nd.match && nd !== selected ? 0.18 : 1;
    }
    if (active) {
      ctx.globalAlpha = 1;
      ctx.lineWidth = 2;
      ctx.strokeStyle = nd === selected ? "#ffffff" : "rgba(255,255,255,0.7)";
      ctx.beginPath();
      ctx.arc(sx, sy, r, 0, Math.PI * 2);
      ctx.stroke();
    }
    // labels: always on the map; on symbols only when big on screen or active.
    if (nd.kind === "mod") {
      ctx.globalAlpha = 1;
      ctx.fillStyle = "rgba(240,243,250,0.95)";
      ctx.font = "600 " + Math.max(10, Math.min(14, 11 * Math.sqrt(scale))) +
        "px -apple-system, Segoe UI, Roboto, sans-serif";
      const label = shortLabel(nd.n);
      const tw = ctx.measureText(label).width;
      ctx.fillText(label, sx - tw / 2, sy + r + 14);
      ctx.fillStyle = "rgba(151,160,184,0.9)";
      ctx.font = "10px -apple-system, Segoe UI, Roboto, sans-serif";
      const sub = nd.mem + " symbols";
      const sw = ctx.measureText(sub).width;
      ctx.fillText(sub, sx - sw / 2, sy + r + 26);
    } else if (r > 9 || active) {
      ctx.globalAlpha = 1;
      ctx.fillStyle = "rgba(232,235,243,0.92)";
      ctx.font = "11px -apple-system, Segoe UI, Roboto, sans-serif";
      ctx.fillText(nd.n, sx + r + 3, sy + 3.5);
    }
  }
  ctx.globalAlpha = 1;
}

function frame() {
  if (alpha > ALPHA_MIN) tick();
  draw();
  requestAnimationFrame(frame);
}
function reheat(v) { alpha = Math.max(alpha, v || 0.3); }

// ---- picking --------------------------------------------------------------
function pick(sx, sy) {
  let best = null, bestD = Infinity;
  for (const nd of curNodes) {
    if (!visible(nd)) continue;
    const nx = nd.x * scale + tx, ny = nd.y * scale + ty;
    const r = Math.max(5, radiusOf(nd) * scale + 3);
    const dx = sx - nx, dy = sy - ny, d = dx * dx + dy * dy;
    if (d <= r * r && d < bestD) { best = nd; bestD = d; }
  }
  return best;
}

// ---- interaction ----------------------------------------------------------
canvas.addEventListener("mousedown", e => {
  const sx = e.offsetX, sy = e.offsetY;
  downX = sx; downY = sy; moved = false;
  const hit = pick(sx, sy);
  if (hit) { dragNode = hit; } else { panning = true; canvas.classList.add("grabbing"); }
});
window.addEventListener("mousemove", e => {
  const rect = canvas.getBoundingClientRect();
  const sx = e.clientX - rect.left, sy = e.clientY - rect.top;
  if (Math.abs(sx - downX) + Math.abs(sy - downY) > 3) moved = true;
  if (dragNode) {
    dragNode.x = toWorldX(sx); dragNode.y = toWorldY(sy);
    dragNode.vx = 0; dragNode.vy = 0; reheat(0.25);
  } else if (panning) {
    tx += (sx - downX); ty += (sy - downY); downX = sx; downY = sy;
  } else {
    const h = pick(sx, sy);
    if (h !== hovered) { hovered = h; canvas.style.cursor = h ? "pointer" : "grab"; }
  }
});
window.addEventListener("mouseup", e => {
  if (dragNode && !moved) {
    if (dragNode.kind === "mod") { setView("symbols", dragNode.i); }
    else select(dragNode);
  } else if (!moved && !dragNode) select(null);
  dragNode = null; panning = false; canvas.classList.remove("grabbing");
});
canvas.addEventListener("wheel", e => {
  e.preventDefault();
  const sx = e.offsetX, sy = e.offsetY;
  const wx = toWorldX(sx), wy = toWorldY(sy);
  const factor = Math.exp(-e.deltaY * 0.0016);
  scale = Math.max(0.02, Math.min(8, scale * factor));
  tx = sx - wx * scale; ty = sy - wy * scale;
}, { passive: false });
canvas.addEventListener("dblclick", () => fit());

// ---- details panel --------------------------------------------------------
const dEl = document.getElementById("details");
function select(nd) {
  selected = nd;
  if (!nd) { dEl.classList.remove("show"); return; }
  const kEl = document.getElementById("d-kind");
  kEl.textContent = nd.k; kEl.style.background = nd.kind === "mod" ? nd.color : colorFor(nd.k);
  document.getElementById("d-name").textContent = nd.n;
  document.getElementById("d-file").textContent = nd.kind === "mod" ? "" : nd.f + ":" + nd.l;
  document.getElementById("d-meta").innerHTML = nd.kind === "mod"
    ? "<b>" + nd.mem + "</b> symbols · <b>" + nd.intra + "</b> internal edges"
    : "degree <b>" + nd.deg + "</b> · " +
      curLinks.filter(l => nodeAt(l.s) === nd).length + " out · " +
      curLinks.filter(l => nodeAt(l.t) === nd).length + " in";
  dEl.classList.add("show");
  reheat(0.15);
}
document.getElementById("details-close").addEventListener("click", () => select(null));

// ---- legend / chrome ------------------------------------------------------
function syncChrome() {
  const rows = document.getElementById("legend-rows");
  rows.innerHTML = "";
  const title = document.getElementById("legend-title");
  const back = document.getElementById("back");
  const meta = DATA.meta || {};
  const noise = meta.noiseHidden
    ? " · <b>" + meta.noiseHidden + "</b> test/vendored hidden" : "";

  document.getElementById("mode-map").classList.toggle("on", view === "map");
  document.getElementById("mode-sym").classList.toggle("on", view === "symbols" && scopeMod < 0);
  back.style.display = scopeMod >= 0 ? "" : "none";

  if (view === "map") {
    title.textContent = "Modules (click to drill in)";
    document.getElementById("hint").textContent =
      "click a module = drill in · scroll = zoom · drag = pan/move";
    document.getElementById("counts").innerHTML =
      "<b>" + modules.length + "</b> modules · <b>" + modLinks.length + "</b> dependencies · <b>" +
      (meta.total || symbols.length) + "</b> symbols" + noise;
    const sorted = modules.slice().sort((a, b) => b.mem - a.mem);
    for (const m of sorted) {
      const row = document.createElement("div");
      row.className = "lrow";
      row.innerHTML =
        '<span class="sw" style="background:' + m.color + '"></span>' +
        '<span class="ln">' + shortLabel(m.n) + '</span><span class="lc">' + m.mem + '</span>';
      row.addEventListener("click", () => setView("symbols", m.i));
      rows.appendChild(row);
    }
  } else {
    title.textContent = "Kinds (click to toggle)";
    document.getElementById("hint").textContent =
      "click = inspect · scroll = zoom · drag = pan/move";
    const scopeLabel = scopeMod >= 0 ? modules[scopeMod].n + " · " : "";
    document.getElementById("counts").innerHTML =
      scopeLabel + "showing <b>" + curNodes.length + "</b> symbols · <b>" +
      curLinks.length + "</b> edges" + noise;
    const counts = {};
    for (const nd of curNodes) counts[nd.k] = (counts[nd.k] || 0) + 1;
    const kinds = Object.keys(counts).sort((a, b) => counts[b] - counts[a]);
    for (const k of kinds) {
      const row = document.createElement("div");
      row.className = "lrow";
      if (hiddenKinds.has(k)) row.classList.add("off");
      row.innerHTML =
        '<span class="sw" style="background:' + colorFor(k) + '"></span>' +
        '<span class="ln">' + k + '</span><span class="lc">' + counts[k] + '</span>';
      row.addEventListener("click", () => {
        if (hiddenKinds.has(k)) hiddenKinds.delete(k); else hiddenKinds.add(k);
        row.classList.toggle("off");
      });
      rows.appendChild(row);
    }
  }
}

// ---- search ---------------------------------------------------------------
document.getElementById("search").addEventListener("input", e => {
  searchQ = e.target.value.trim().toLowerCase();
  for (const nd of curNodes) nd.match = searchQ.length > 0 && nd.n.toLowerCase().includes(searchQ);
});
document.getElementById("fit").addEventListener("click", () => fit());
document.getElementById("mode-map").addEventListener("click", () => setView("map"));
document.getElementById("mode-sym").addEventListener("click", () => setView("symbols"));
document.getElementById("back").addEventListener("click", () => setView("map"));

// ---- HUD ------------------------------------------------------------------
document.getElementById("root").textContent = (DATA.meta || {}).root || "";

setView(modules.length > 1 ? "map" : "symbols");
frame();
</script>
</body>
</html>
"####;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use selene_core::{EdgeKind, Language};

    fn node(id: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/lib.rs".to_string(),
            language: Language::Rust,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: None,
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: vec![],
            type_parameters: vec![],
            return_type: None,
            route_method: None,
            route_path: None,
            framework: None,
            updated_at: 0,
        }
    }

    fn edge(s: &str, t: &str) -> Edge {
        Edge {
            source: s.to_string(),
            target: t.to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: None,
            column: None,
            provenance: None,
        }
    }

    fn opts(max: usize, all: bool) -> VizOptions {
        VizOptions {
            max_nodes: max,
            all_kinds: all,
            root_label: "/tmp/demo".to_string(),
        }
    }

    #[test]
    fn low_signal_kinds_dropped_by_default() {
        let nodes = vec![
            node("function:a", "a", NodeKind::Function),
            node("variable:v", "v", NodeKind::Variable),
            node("import:i", "i", NodeKind::Import),
        ];
        let doc = build_html(&nodes, &[], &opts(2000, false));
        assert_eq!(
            doc.shown_nodes, 1,
            "only the function survives the default filter"
        );
        assert_eq!(doc.total_nodes, 3);
        // ...and are kept when --all-kinds is set
        let doc_all = build_html(&nodes, &[], &opts(2000, true));
        assert_eq!(doc_all.shown_nodes, 3);
    }

    #[test]
    fn cap_keeps_the_highest_degree_nodes() {
        // b is the hub (degree 2); a and c have degree 1 each.
        let nodes = vec![
            node("function:a", "a", NodeKind::Function),
            node("function:b", "b", NodeKind::Function),
            node("function:c", "c", NodeKind::Function),
        ];
        let edges = vec![
            edge("function:b", "function:a"),
            edge("function:b", "function:c"),
        ];
        let doc = build_html(&nodes, &edges, &opts(1, false));
        assert_eq!(doc.shown_nodes, 1);
        // the surviving node must be the hub — its data is embedded in the page
        assert!(
            doc.html.contains("\"n\":\"b\""),
            "the hub 'b' should be the kept node"
        );
    }

    #[test]
    fn links_kept_only_when_both_endpoints_survive_and_are_deduped() {
        let nodes = vec![
            node("function:a", "a", NodeKind::Function),
            node("function:b", "b", NodeKind::Function),
        ];
        // one real edge, plus a duplicate, plus a dangling edge to a dropped node
        let edges = vec![
            edge("function:a", "function:b"),
            edge("function:a", "function:b"),
            edge("function:a", "variable:gone"),
        ];
        let doc = build_html(&nodes, &edges, &opts(2000, false));
        assert_eq!(doc.shown_nodes, 2);
        assert_eq!(doc.shown_edges, 1, "dupe collapsed, dangling edge dropped");
    }

    #[test]
    fn page_is_self_contained_and_carries_the_data() {
        let nodes = vec![node("function:main", "main", NodeKind::Function)];
        let doc = build_html(&nodes, &[], &opts(2000, false));
        let h = &doc.html;
        assert!(h.starts_with("<!doctype html>"));
        // no external resources of any kind
        assert!(
            !h.contains("http://") && !h.contains("https://"),
            "no network URLs"
        );
        assert!(!h.contains("src="), "no external script/style src");
        assert!(
            !h.contains("__DATA__") && !h.contains("__TITLE__"),
            "placeholders substituted"
        );
        assert!(h.contains("\"n\":\"main\""), "node data embedded inline");
    }

    #[test]
    fn script_close_tag_in_a_name_cannot_break_out() {
        let nodes = vec![node("function:x", "</script><b>x", NodeKind::Function)];
        let doc = build_html(&nodes, &[], &opts(2000, false));
        // the raw sequence must not appear literally inside the embedded data
        assert!(!doc.html.contains("</script><b>x"));
        assert!(
            doc.html.contains("\\u003c/script>"),
            "the '<' was escaped to \\u003c"
        );
    }
}
