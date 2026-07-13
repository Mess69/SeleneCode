/**
 * TS-side count dumper for the Phase 2 extraction parity gate (Task 19).
 *
 * Runs the REAL CodeGraph TypeScript extractor over the shared fixture corpus
 * (`crates/selene-extract/tests/fixtures/parity/<lang>/…`) and writes per-file
 * node/edge/ref counts to `expected.json`. The Rust gate (`tests/parity_gate.rs`)
 * then asserts its own counts against that baseline, tolerance 0.
 *
 * ## How to run
 *
 *   cd ../codegraph && npx vite-node <abs path to this script> <abs fixtures dir> <abs out.json>
 *
 * Run from codegraph's root so its deps resolve.
 *
 * ## Loader: vite-node, NOT `npx tsx` (deviation from the task brief — deliberate)
 *
 * The brief specifies `npx tsx`. It does not work here, for a reason that is a
 * property of the dependency, not of this script: `web-tree-sitter` (0.25.10)
 * ships an Emscripten artifact whose CJS build Node's ESM lexer cannot
 * statically analyze. Under tsx, `import('web-tree-sitter')` yields a namespace
 * with a single opaque `default` key and `Parser === undefined`, so
 * `grammars.ts:247`'s `await Parser.init()` throws
 * `TypeError: Cannot read properties of undefined (reading 'init')`.
 * Both import spellings the brief suggests (the `.js` specifier and the direct
 * `.ts` source path) fail identically — the failure is in resolving
 * web-tree-sitter, not in resolving codegraph.
 *
 * Vite's resolver handles the interop correctly, which is why codegraph's own
 * vitest suite runs the extractor fine. `vite-node` is vitest's standalone
 * runner (already a codegraph dependency), so it gives us Vite's resolution for
 * a plain script — the same loader codegraph itself trusts, with no new deps and
 * no changes to the codegraph repo.
 *
 * ## Grammar init is mandatory and must come FIRST
 *
 * `extractFromSource` with an unloaded grammar does NOT throw — it returns an
 * EMPTY result plus an `unsupported_language` / `parser_error` error
 * (tree-sitter.ts:427-450). A baseline generated without `initGrammars()` +
 * `loadGrammarsForLanguages()` would therefore be all zeros, and the gate would
 * pass vacuously against a Rust side that emits real counts only if IT were also
 * broken. That is the single worst failure mode for this gate, so this script
 * REFUSES to write the baseline (exit 1) if any fixture yields zero nodes or any
 * extraction error.
 */
import { readdirSync, statSync, readFileSync, writeFileSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

const [, , FIXTURES_ARG, OUT_ARG] = process.argv;
if (!FIXTURES_ARG || !OUT_ARG) {
  console.error('usage: vite-node dump-ts-extraction.mjs <fixtures-dir> <out.json>');
  process.exit(2);
}
const FIXTURES = resolve(FIXTURES_ARG);
const OUT = resolve(OUT_ARG);

// codegraph root = cwd (the script is invoked from there so deps resolve).
const CG = process.cwd();
const { extractFromSource } = await import(`${CG}/src/extraction/tree-sitter.ts`);
const { initGrammars, loadGrammarsForLanguages, detectLanguage } = await import(
  `${CG}/src/extraction/grammars.ts`
);

/** Every fixture file, as paths relative to the fixtures root, sorted. */
function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir).sort()) {
    const abs = join(dir, entry);
    if (statSync(abs).isDirectory()) out.push(...walk(abs));
    else out.push(abs);
  }
  return out;
}

const files = walk(FIXTURES)
  .filter((f) => !f.endsWith('.json') && !f.endsWith('.toml'))
  .map((f) => relative(FIXTURES, f))
  .sort();

if (files.length === 0) {
  console.error(`FATAL: no fixtures found under ${FIXTURES}`);
  process.exit(1);
}

// Grammar init FIRST. `cpp` whenever `c` is present: the C fixtures can be
// re-detected as C++ by the extractor's own heuristics, and loading a grammar
// that goes unused is free.
const langs = new Set(files.map((f) => detectLanguage(f)));
if (langs.has('c')) langs.add('cpp');
const needed = [...langs].filter((l) => l && l !== 'unknown').sort();

await initGrammars();
await loadGrammarsForLanguages(needed);
console.error(`[parity] grammars loaded: ${needed.join(', ')}`);

/** Count occurrences of `key` across `items`, as a sorted-key object. */
function countBy(items, key) {
  const counts = {};
  for (const it of items) {
    const k = it[key];
    counts[k] = (counts[k] ?? 0) + 1;
  }
  return Object.fromEntries(Object.entries(counts).sort(([a], [b]) => a.localeCompare(b)));
}

const results = {};
const broken = [];

for (const rel of files) {
  const source = readFileSync(join(FIXTURES, rel), 'utf8');
  const language = detectLanguage(rel);
  const r = extractFromSource(rel, source, language);

  const errors = (r.errors ?? []).filter((e) => e.severity === 'error');
  if (r.nodes.length === 0) broken.push(`${rel}: 0 nodes (language=${language})`);
  for (const e of errors) broken.push(`${rel}: ${e.code}: ${e.message}`);

  results[rel] = {
    language,
    nodesByKind: countBy(r.nodes, 'kind'),
    edgesByKind: countBy(r.edges, 'kind'),
    refsByKind: countBy(r.unresolvedReferences, 'referenceKind'),
    nodeCount: r.nodes.length,
    edgeCount: r.edges.length,
    refCount: r.unresolvedReferences.length,
  };
}

// REFUSE to write a broken baseline. A zero-node fixture or a parser error means
// the harness is broken, not that parity is zero — writing it would bake a
// vacuously-passing gate into the repo forever.
if (broken.length > 0) {
  console.error('\nFATAL: refusing to write a baseline — these fixtures did not extract:');
  for (const b of broken) console.error(`  - ${b}`);
  console.error('\nMost likely cause: grammars not initialized/loaded before extraction.');
  process.exit(1);
}

const payload = {
  _comment:
    'GENERATED by tools/parity/dump-ts-extraction.mjs from the CodeGraph TS extractor. Do not hand-edit — regenerate.',
  codegraphCommit: process.env.CODEGRAPH_COMMIT ?? 'unknown',
  fileCount: files.length,
  totals: {
    nodes: Object.values(results).reduce((a, r) => a + r.nodeCount, 0),
    edges: Object.values(results).reduce((a, r) => a + r.edgeCount, 0),
    refs: Object.values(results).reduce((a, r) => a + r.refCount, 0),
  },
  files: results,
};

writeFileSync(OUT, `${JSON.stringify(payload, null, 2)}\n`);
console.error(
  `[parity] wrote ${OUT}: ${files.length} files, ${payload.totals.nodes} nodes, ` +
    `${payload.totals.edges} edges, ${payload.totals.refs} refs`
);
