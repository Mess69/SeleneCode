//! Deterministic synthetic-graph generator for the PRD §5.3 benchmark gate.
//!
//! Feature-gated behind `bench-support` (off by default — this never ships in a
//! normal build). Its whole job is to hand the criterion benches
//! (`benches/bulk_and_traverse.rs`) and their determinism test a graph with a
//! *realistic shape* — long call-chain backbones, hub fan-in, a shared name
//! vocabulary for FTS overlap, four languages, ~5 edges/node — while being
//! **fully deterministic from a `u64` seed**: same `(seed, nodes)` in ⇒
//! byte-identical `(Vec<Node>, Vec<Edge>)` out.
//!
//! No external `rand`, no `Instant::now`/entropy: randomness is a seeded
//! [`SplitMix64`] stream, and every node's stable `id` is derived from
//! `(seed, global_index)` alone (independent of draw order), so the id set is
//! reproducible even if the edge logic changes.
//!
//! # Shape guarantees (what the benches rely on)
//!
//! - **Files:** ~1 file node per [`SYMS_PER_FILE`] symbols (`contains` edges
//!   wire each file to its symbols).
//! - **Clean deep corridor:** symbol indices `[0, CHAIN_LEN)` form one
//!   isolated call chain (`sym0 -> sym1 -> … -> sym{CHAIN_LEN-1}`, [`CHAIN_LEN`]
//!   ≥ 12) with **no other edges into or out of them** except their file's
//!   `contains`. That makes [`Landmarks::deep_head_id`]→[`Landmarks::deep_tail_id`]
//!   a guaranteed ≥ `CHAIN_LEN-1`-hop `find_path`, and `impact_radius` on the
//!   tail a clean N-level backward walk — no random shortcut can shorten either.
//! - **Backbone chains everywhere else:** the remaining symbols are chained in
//!   [`CHAIN_LEN`]-sized groups, so deep paths are common, not a single fixture.
//! - **Hub fan-in:** [`Landmarks::hub_id`] gets ≥ [`HUB_FAN_IN`] direct callers.
//! - **~5 edges/node** across `contains`/`calls`/`references`/`imports`/
//!   `instantiates`/`extends` (non-`contains` edges carry a unique `line` so the
//!   store's `(source,target,kind,line,col)` identity dedup never collapses
//!   them).
//! - **Docstrings** on ~30% of symbols; **languages** cycled over [`LANGS`].

use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance};

/// Symbols per file node (`~1 file per 25 symbols`, PRD-flavored repo shape).
pub const SYMS_PER_FILE: usize = 25;

/// Length of every call-chain backbone (≥ 12 so `impact_radius`/`find_path`
/// have genuinely deep paths). Group 0 is the reserved clean corridor.
pub const CHAIN_LEN: usize = 16;

/// Direct callers wired onto the hub node (≥ 100 per the "some hub nodes with
/// 100+ callers" fan-out requirement), capped down on tiny graphs.
pub const HUB_FAN_IN: usize = 150;

/// The four languages cycled across files (a file's symbols inherit its
/// language, as in a real repo).
pub const LANGS: [&str; 4] = ["rust", "typescript", "python", "go"];

/// Fixed `updated_at` stamp for every node — a constant, never `now()`, so the
/// output stays byte-deterministic.
const EPOCH_MILLIS: i64 = 1_720_000_000_000;

/// Shared identifier vocabulary. Names are camelCased combinations of these, so
/// the FTS index sees heavy term overlap (e.g. many names contain `user`,
/// [`FTS_TERM`]). Kept small on purpose — overlap is the point.
const VOCAB: [&str; 40] = [
    "get", "set", "create", "update", "delete", "fetch", "load", "save", "parse", "render",
    "handle", "process", "validate", "compute", "build", "resolve", "user", "order", "item",
    "node", "graph", "token", "cache", "config", "request", "response", "service", "manager",
    "handler", "context", "session", "buffer", "stream", "index", "query", "result", "value",
    "entry", "record", "worker",
];

/// A vocabulary root guaranteed to be frequent across generated names — the
/// term the FTS bench searches for (matches any name containing `User`/`user`
/// once the `camel` + `lowercase` analyzer splits humps).
pub const FTS_TERM: &str = "user";

/// Landmark node ids a bench needs to target specific structures, captured
/// during generation so no post-hoc scan is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landmarks {
    /// A node with ≥ [`HUB_FAN_IN`] direct callers — the `callers` fan-out
    /// target.
    pub hub_id: String,
    /// Head of the reserved clean corridor — the `find_path` source.
    pub deep_head_id: String,
    /// Tail of the reserved clean corridor, `CHAIN_LEN-1` hops from the head —
    /// the `find_path` target and the `impact_radius` root.
    pub deep_tail_id: String,
    /// A term present in many node names — the `search_fts` query term.
    pub fts_term: String,
}

/// A minimal, fast, seedable PRNG (SplitMix64). Deterministic and dependency
/// free — the whole point of the generator.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform-ish index in `0..n`. `n` must be non-zero (callers guarantee
    /// it; a zero `n` folds to `0` rather than dividing by zero).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }

    /// `true` with probability ~`p` (`p` in `0.0..=1.0`).
    fn chance(&mut self, p: f64) -> bool {
        // 53-bit mantissa's worth of uniform [0,1).
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        unit < p
    }
}

/// Stable node id from `(seed, global_index)`, formatted `kind:hash32hex`.
///
/// The low 16 hex digits are the global index, so ids are **unique by
/// construction** (no collision set needed); the high 16 are a SplitMix64
/// scramble of the index for a hash-like look. Independent of the RNG draw
/// stream, so the id set is stable even if edge generation changes.
fn make_id(seed: u64, kind: NodeKind, global_index: usize) -> String {
    let mut mix = SplitMix64::new(seed ^ (global_index as u64).wrapping_mul(0x2545_F491_4F6C_DD1D));
    let high = mix.next_u64();
    format!(
        "{}:{:016x}{:016x}",
        kind.as_str(),
        high,
        global_index as u64
    )
}

/// A camelCase identifier built from 2–3 [`VOCAB`] roots (first lowercased,
/// the rest capitalized): `get` + `User` + `Order` ⇒ `getUserOrder`.
fn camel_name(rng: &mut SplitMix64) -> String {
    let parts = 2 + rng.below(2); // 2 or 3 roots
    let mut name = String::with_capacity(24);
    for i in 0..parts {
        let word = VOCAB[rng.below(VOCAB.len())];
        if i == 0 {
            name.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                name.extend(first.to_uppercase());
                name.push_str(chars.as_str());
            }
        }
    }
    name
}

/// File extension for a language (falls back to `txt` for anything unexpected).
fn lang_ext(lang: &str) -> &'static str {
    match lang {
        "rust" => "rs",
        "typescript" => "ts",
        "python" => "py",
        "go" => "go",
        _ => "txt",
    }
}

/// Symbol node-kind mix (mostly callable; enough class-likes to hang
/// `extends`/`instantiates`/`implements` off of).
fn pick_symbol_kind(rng: &mut SplitMix64) -> NodeKind {
    match rng.below(100) {
        0..=44 => NodeKind::Function,
        45..=64 => NodeKind::Method,
        65..=74 => NodeKind::Class,
        75..=79 => NodeKind::Struct,
        80..=84 => NodeKind::Interface,
        85..=88 => NodeKind::Trait,
        89..=91 => NodeKind::Variable,
        92..=94 => NodeKind::Constant,
        95..=96 => NodeKind::Enum,
        97 => NodeKind::TypeAlias,
        98 => NodeKind::Property,
        _ => NodeKind::Field,
    }
}

/// `true` for the class-like kinds that can be an `extends`/`implements`/
/// `instantiates` target.
fn is_class_like(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Interface | NodeKind::Trait | NodeKind::Enum
    )
}

/// The synthetic graph generator. Zero-sized — all state is local to
/// [`generate`](Self::generate).
pub struct SyntheticGraph;

impl SyntheticGraph {
    /// Generate a deterministic `(nodes, edges)` graph of ~`nodes` nodes from
    /// `seed`. Same inputs ⇒ identical output. See the module docs for the
    /// shape guarantees; use [`generate_with_landmarks`](Self::generate_with_landmarks)
    /// when the benches need the hub / deep-chain / FTS landmark ids.
    pub fn generate(seed: u64, nodes: usize) -> (Vec<Node>, Vec<Edge>) {
        let (nodes, edges, _) = Self::generate_with_landmarks(seed, nodes);
        (nodes, edges)
    }

    /// Like [`generate`](Self::generate) but also returns the [`Landmarks`] the
    /// per-query benches target (hub, deep-chain head/tail, FTS term).
    pub fn generate_with_landmarks(
        seed: u64,
        total_nodes: usize,
    ) -> (Vec<Node>, Vec<Edge>, Landmarks) {
        let total_nodes = total_nodes.max(CHAIN_LEN + 4);
        let num_files = (total_nodes / (SYMS_PER_FILE + 1)).max(1);
        let num_syms = total_nodes - num_files;
        let syms_per_file = num_syms.div_ceil(num_files).max(1);

        let mut rng = SplitMix64::new(seed);
        let mut nodes: Vec<Node> = Vec::with_capacity(total_nodes);
        let mut ids: Vec<String> = Vec::with_capacity(total_nodes);
        let mut file_paths: Vec<String> = Vec::with_capacity(num_files);

        // --- File nodes: global indices [0, num_files) ---
        for f in 0..num_files {
            let lang = LANGS[f % LANGS.len()];
            let ext = lang_ext(lang);
            let path = format!("src/pkg{}/module{}.{}", f % 32, f, ext);
            let id = make_id(seed, NodeKind::File, f);
            nodes.push(Node {
                id: id.clone(),
                kind: NodeKind::File,
                name: format!("module{f}.{ext}"),
                qualified_name: path.clone(),
                file_path: path.clone(),
                language: lang.to_string(),
                start_line: 1,
                end_line: 1,
                start_column: 0,
                end_column: 0,
                docstring: None,
                signature: None,
                visibility: None,
                is_exported: None,
                is_async: None,
                is_static: None,
                is_abstract: None,
                decorators: Vec::new(),
                type_parameters: Vec::new(),
                return_type: None,
                route_method: None,
                route_path: None,
                framework: None,
                updated_at: EPOCH_MILLIS,
            });
            ids.push(id);
            file_paths.push(path);
        }

        // --- Symbol nodes: global indices [num_files, total_nodes) ---
        // Corridor symbols (index < CHAIN_LEN) and the hub are forced callable.
        let hub_sym = CHAIN_LEN; // first symbol of chain group 1
        let mut class_like: Vec<usize> = Vec::new(); // symbol indices, for extends/instantiates targets
        let mut kinds: Vec<NodeKind> = Vec::with_capacity(num_syms);

        for s in 0..num_syms {
            let file = (s / syms_per_file).min(num_files - 1);
            let lang = LANGS[file % LANGS.len()];
            let global = num_files + s;

            let kind = if s < CHAIN_LEN || s == hub_sym {
                NodeKind::Function
            } else {
                pick_symbol_kind(&mut rng)
            };
            kinds.push(kind);
            if is_class_like(kind) {
                class_like.push(s);
            }

            let name = camel_name(&mut rng);
            let file_path = file_paths[file].clone();
            let qualified_name = format!("{file_path}::{name}");
            let docstring = if rng.chance(0.30) {
                Some(format!(
                    "Handles the {name} operation for the {} subsystem.",
                    VOCAB[rng.below(VOCAB.len())]
                ))
            } else {
                None
            };
            let signature = match kind {
                NodeKind::Function | NodeKind::Method => {
                    Some(format!("fn {name}(request: Request) -> Result<Response>"))
                }
                _ => None,
            };
            let id = make_id(seed, kind, global);
            nodes.push(Node {
                id: id.clone(),
                kind,
                name,
                qualified_name,
                file_path,
                language: lang.to_string(),
                start_line: (s as u32 % 4000) + 2,
                end_line: (s as u32 % 4000) + 8,
                start_column: 0,
                end_column: 4,
                docstring,
                signature,
                visibility: None,
                is_exported: Some(s % 3 == 0),
                is_async: Some(matches!(kind, NodeKind::Function | NodeKind::Method) && s % 5 == 0),
                is_static: None,
                is_abstract: None,
                decorators: Vec::new(),
                type_parameters: Vec::new(),
                return_type: None,
                route_method: None,
                route_path: None,
                framework: None,
                updated_at: EPOCH_MILLIS,
            });
            ids.push(id);
        }

        // Helper: global id of a symbol by its symbol-index.
        let sym_id = |s: usize| ids[num_files + s].clone();

        let mut edges: Vec<Edge> = Vec::with_capacity(num_syms * 6);
        // Monotonic line counter → every non-`contains` edge is identity-unique
        // in the store (never collapsed by `(source,target,kind,line,col)` dedup).
        let mut line: u32 = 0;
        let mut next_line = || {
            line = line.wrapping_add(1);
            Some(line)
        };

        // --- contains: file -> each of its symbols ---
        for s in 0..num_syms {
            let file = (s / syms_per_file).min(num_files - 1);
            edges.push(mk_edge(&ids[file], &sym_id(s), EdgeKind::Contains, None));
        }

        // --- call-chain backbones, in CHAIN_LEN-sized groups ---
        // Group 0 == the reserved clean corridor (no other edges touch it).
        let mut g = 0;
        while g * CHAIN_LEN < num_syms {
            let start = g * CHAIN_LEN;
            let end = (start + CHAIN_LEN).min(num_syms);
            for s in start..end.saturating_sub(1) {
                edges.push(mk_edge(
                    &sym_id(s),
                    &sym_id(s + 1),
                    EdgeKind::Calls,
                    next_line(),
                ));
            }
            g += 1;
        }

        // --- hub fan-in: >= HUB_FAN_IN distinct callers -> hub ---
        let fan_in = HUB_FAN_IN
            .min(num_syms.saturating_sub(CHAIN_LEN + 1) / 2)
            .max(0);
        let mut wired = 0;
        // Draw sources from the tail end of the symbol range (never the
        // corridor, never the hub itself), stepping to keep them distinct.
        let mut src = num_syms.saturating_sub(1);
        while wired < fan_in && src > hub_sym {
            if src != hub_sym && src >= CHAIN_LEN {
                edges.push(mk_edge(
                    &sym_id(src),
                    &sym_id(hub_sym),
                    EdgeKind::Calls,
                    next_line(),
                ));
                wired += 1;
            }
            src -= 1;
        }

        // --- per-symbol filler edges to reach ~5/node (skip the corridor) ---
        for s in CHAIN_LEN..num_syms {
            // 0..2 extra calls, biased toward the hub for extra fan-in.
            let extra_calls = rng.below(3);
            for _ in 0..extra_calls {
                let target = if rng.chance(0.10) {
                    hub_sym
                } else {
                    pick_non_corridor(&mut rng, num_syms)
                };
                if target != s {
                    edges.push(mk_edge(
                        &sym_id(s),
                        &sym_id(target),
                        EdgeKind::Calls,
                        next_line(),
                    ));
                }
            }
            // ~1.5 references.
            let refs = 1 + usize::from(rng.chance(0.5));
            for _ in 0..refs {
                let target = pick_non_corridor(&mut rng, num_syms);
                if target != s {
                    edges.push(mk_edge(
                        &sym_id(s),
                        &sym_id(target),
                        EdgeKind::References,
                        next_line(),
                    ));
                }
            }
            // ~0.6 cross-file imports.
            if rng.chance(0.6) {
                let target = pick_non_corridor(&mut rng, num_syms);
                if target != s {
                    edges.push(mk_edge(
                        &sym_id(s),
                        &sym_id(target),
                        EdgeKind::Imports,
                        next_line(),
                    ));
                }
            }
            // ~0.15 instantiations of a class-like symbol.
            if rng.chance(0.15) && !class_like.is_empty() {
                let target = class_like[rng.below(class_like.len())];
                if target != s {
                    edges.push(mk_edge(
                        &sym_id(s),
                        &sym_id(target),
                        EdgeKind::Instantiates,
                        next_line(),
                    ));
                }
            }
            // Class-likes extend another class-like ~40% of the time.
            if is_class_like(kinds[s]) && rng.chance(0.4) && class_like.len() > 1 {
                let target = class_like[rng.below(class_like.len())];
                if target != s {
                    edges.push(mk_edge(
                        &sym_id(s),
                        &sym_id(target),
                        EdgeKind::Extends,
                        next_line(),
                    ));
                }
            }
        }

        let landmarks = Landmarks {
            hub_id: sym_id(hub_sym),
            deep_head_id: sym_id(0),
            deep_tail_id: sym_id(CHAIN_LEN - 1),
            fts_term: FTS_TERM.to_string(),
        };

        (nodes, edges, landmarks)
    }
}

/// Pick a symbol index outside the reserved corridor (`>= CHAIN_LEN`). Keeps
/// random edges from ever touching the clean deep chain.
fn pick_non_corridor(rng: &mut SplitMix64, num_syms: usize) -> usize {
    let span = num_syms.saturating_sub(CHAIN_LEN);
    if span == 0 {
        return CHAIN_LEN.min(num_syms.saturating_sub(1));
    }
    CHAIN_LEN + rng.below(span)
}

/// Build one edge with the bench's fixed defaults (tree-sitter provenance, no
/// column/metadata).
fn mk_edge(source: &str, target: &str, kind: EdgeKind, line: Option<u32>) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: None,
        line,
        column: None,
        provenance: Some(Provenance::TreeSitter),
    }
}
