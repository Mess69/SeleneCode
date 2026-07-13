/**
 * TS-side EDGE dumper for the Phase 3 resolution parity gate (plan Task 29).
 *
 * Runs the **REAL CodeGraph pipeline** — `CodeGraph.init()` + `indexAll()`, which is
 * extraction → framework emission → the `resolveOne` ladder → the conformance passes
 * → synthesis — over each project in the corpus, and writes the resulting EDGE SET to
 * `expected.json`. The Rust gate asserts its own edge set against that, tolerance 0.
 *
 * ## How to run
 *
 *   cd ../codegraph && npx vite-node <selene>/tools/parity/dump-ts-resolution.mjs \
 *       <selene>/crates/selene-resolve/tests/fixtures/resolve \
 *       <selene>/crates/selene-resolve/tests/fixtures/resolve/expected.json
 *
 * Run from codegraph's root so its deps resolve. `vite-node`, **not** `npx tsx` — see
 * `dump-ts-extraction.mjs`'s header: web-tree-sitter's Emscripten CJS build defeats
 * Node's ESM lexer under tsx, and `Parser.init()` throws on undefined.
 *
 * ## Edges compare on SEMANTIC identity, never on id spelling
 *
 * A TS node id is a literal string (`func:src/a.ts:login:12`); ours is a sha256 hash of
 * the same four components. Comparing ids would therefore diff 100% of edges while
 * telling us nothing. Both engines derive their id from `(file, kind, name, start_line)`,
 * so THAT tuple is the identity, and it is what this dumper writes:
 *
 *     "<kind>:<name>@<file>"                      an ordinary symbol
 *     "route:<framework>:<METHOD>:<path>@<file>:<line>"   a route node
 *     "file:@<path>"                              a file node
 *
 * Routes carry `(framework, method, path, file, line)` because that IS a route's
 * identity — several routes share one file and one line (`get(h).post(h2)`,
 * `resources :articles`), and they are separated only by method+path. Loosening this
 * to the raw id would blind the gate exactly where dispatch bridging lives; loosening
 * it to the path alone would collapse the same-line routes and hide a deleted verb.
 *
 * The line is deliberately NOT part of a symbol's key: extraction already gates
 * per-file node identity (Phase 2), and re-asserting a line here would turn a
 * one-line formatting difference in a fixture into a resolution failure. Two
 * same-named symbols of the same kind in one file (an overload) would collide — the
 * corpus must not contain one, and `baseline_is_not_vacuous` on the Rust side is where
 * that would show up as a count mismatch.
 *
 * ## An edge's identity
 *
 *   (source, target, kind, provenance) + metadata.synthesizedBy
 *
 * `synthesizedBy` is part of the key because two synthesizer channels may bridge the
 * same pair for different reasons, and collapsing them would hide a channel that
 * stopped firing while another still did.
 *
 * ## It REFUSES to write a vacuous baseline
 *
 * Phase 2's cautionary tale: a baseline of zeros is reproduced perfectly by a Rust side
 * that is equally broken, and the gate is then green forever having compared nothing. So
 * this script exits non-zero if any project yields zero edges, zero cross-file edges, or
 * an index error — and it records the codegraph commit, so a stale baseline is
 * detectable rather than merely wrong.
 */
import { cpSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';

const [, , CORPUS_ARG, OUT_ARG] = process.argv;
if (!CORPUS_ARG || !OUT_ARG) {
  console.error('usage: vite-node dump-ts-resolution.mjs <corpus-dir> <out.json>');
  process.exit(2);
}
const CORPUS = resolve(CORPUS_ARG);
const OUT = resolve(OUT_ARG);
const CG = process.cwd(); // codegraph root — the script is invoked from there

function codegraphCommit() {
  if (process.env.CODEGRAPH_COMMIT) return process.env.CODEGRAPH_COMMIT;
  try {
    return execFileSync('git', ['-C', CG, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  } catch {
    return 'unknown'; // not a git checkout — the gate rejects this
  }
}

const { CodeGraph } = await import(`${CG}/src/index.ts`);

/** The project directories of the corpus: every immediate subdirectory. */
function projects() {
  return readdirSync(CORPUS)
    .sort()
    .filter((e) => !e.startsWith('.') && !e.startsWith('_'))
    .filter((e) => statSync(join(CORPUS, e)).isDirectory());
}

/**
 * `(file, kind, name, line)` — the identity both engines derive their id from.
 *
 * ## Why a route's key is its NAME, and why `framework` is not in it
 *
 * A TS route node has **no `framework`, `routeMethod` or `routePath` field at all** —
 * those are SeleneCode's (the Task 11 decision: keep ids opaque, put the semantics in
 * indexed fields). TS keeps a route's semantics in two places only: the id STRING
 * (`route:${file}:${line}:${METHOD}:${path}`) and the `name` (`"GET /articles"`).
 *
 * `name` is the one both engines carry, and it is byte-identical by construction —
 * `routes.rs` documents that spelling as a wire contract precisely so this comparison
 * is possible. So a route's key is `route:<name>@<file>:<line>`, which is exactly
 * `(method, path, file, line)` in the only spelling both sides can produce.
 *
 * `framework` is asserted **separately** (the manifest's `expect_frameworks`, on both
 * engines) rather than folded in here. Folding it in would mean comparing a field we
 * invented against a field TS does not have — i.e. comparing our own output to itself,
 * which is the definition of a gate that gates nothing.
 *
 * The line IS part of a route's key (unlike a symbol's) because several routes legally
 * share one file AND one line — `get(h).post(h2)`, `resources :articles` — and they are
 * separated only by name+line. Drop either and a deleted verb passes unnoticed, which is
 * the failure this gate exists to catch.
 */
function endpointKey(node) {
  if (!node) return null;
  if (node.kind === 'route') return `route:${node.name}@${node.filePath}:${node.startLine}`;
  if (node.kind === 'file') return `file:@${node.filePath}`;
  return `${node.kind}:${node.name}@${node.filePath}`;
}

function parseMetadata(raw) {
  if (!raw) return {};
  if (typeof raw === 'object') return raw;
  try {
    return JSON.parse(raw);
  } catch {
    return {};
  }
}

async function dumpProject(dir) {
  // Copy to a temp dir: indexing writes `.codegraph/` into the project root, and a
  // fixture tree must stay exactly the bytes both engines read.
  const tmp = mkdtempSync(join(tmpdir(), 'p3-parity-'));
  const work = join(tmp, basename(dir));
  cpSync(join(CORPUS, dir), work, { recursive: true });

  try {
    const cg = await CodeGraph.init(work);
    const res = await cg.indexAll({});
    const hardErrors = (res.errors ?? []).filter((e) => e.severity === 'error');
    if (!res.success || hardErrors.length) {
      throw new Error(
        `index failed for ${dir}: ${JSON.stringify(hardErrors.slice(0, 3))}`
      );
    }

    const q = cg.queries;
    const nodes = q.getAllNodes();
    const byId = new Map(nodes.map((n) => [n.id, n]));

    const edges = [];
    for (const n of nodes) {
      for (const e of q.getOutgoingEdges(n.id)) {
        const src = endpointKey(byId.get(e.source));
        const dst = endpointKey(byId.get(e.target));
        if (!src || !dst) continue; // an endpoint we cannot name is one we cannot compare
        const meta = parseMetadata(e.metadata);
        edges.push({
          source: src,
          target: dst,
          kind: e.kind,
          provenance: e.provenance ?? 'tree-sitter',
          synthesizedBy: meta.synthesizedBy ?? null,
        });
      }
    }
    edges.sort((a, b) =>
      JSON.stringify(a) < JSON.stringify(b) ? -1 : JSON.stringify(a) > JSON.stringify(b) ? 1 : 0
    );

    const crossFile = edges.filter((e) => {
      const f = (k) => k.split('@').pop().split(':')[0];
      return f(e.source) !== f(e.target);
    }).length;

    const frameworks = [
      ...new Set(nodes.filter((n) => n.kind === 'route').map((n) => n.framework).filter(Boolean)),
    ].sort();

    return {
      edges,
      nodes: nodes.length,
      crossFileEdges: crossFile,
      frameworks,
      routes: nodes.filter((n) => n.kind === 'route').length,
    };
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

const out = { codegraphCommit: codegraphCommit(), projects: {} };
const failures = [];

for (const dir of projects()) {
  process.stderr.write(`  ${dir} … `);
  try {
    const r = await dumpProject(dir);
    out.projects[dir] = r;
    process.stderr.write(
      `${r.nodes} nodes, ${r.edges.length} edges (${r.crossFileEdges} cross-file), ` +
        `${r.routes} routes [${r.frameworks.join(',') || '-'}]\n`
    );
    // Anti-vacuity, per project. A project that resolves nothing gates nothing.
    if (r.edges.length === 0) failures.push(`${dir}: ZERO edges`);
    if (r.crossFileEdges === 0) failures.push(`${dir}: ZERO cross-file edges`);
  } catch (err) {
    process.stderr.write(`FAILED\n`);
    failures.push(`${dir}: ${err.message}`);
  }
}

if (failures.length) {
  console.error('\nREFUSING TO WRITE A VACUOUS OR BROKEN BASELINE:');
  for (const f of failures) console.error(`  - ${f}`);
  console.error(
    '\nA baseline of zeros is reproduced perfectly by a Rust side that is equally broken,\n' +
      'and the gate is then green forever having compared nothing. Fix the corpus or the\n' +
      'pipeline; do not ship this file.'
  );
  process.exit(1);
}

writeFileSync(OUT, JSON.stringify(out, null, 2) + '\n');

const all = Object.values(out.projects).flatMap((p) => p.edges);
const byKind = {};
const byProv = {};
for (const e of all) {
  byKind[e.kind] = (byKind[e.kind] ?? 0) + 1;
  byProv[e.provenance] = (byProv[e.provenance] ?? 0) + 1;
}
console.error(`\nwrote ${OUT}`);
console.error(`  projects: ${Object.keys(out.projects).length}`);
console.error(`  edges:    ${all.length}`);
console.error(`  by kind:  ${JSON.stringify(byKind)}`);
console.error(`  by prov:  ${JSON.stringify(byProv)}`);
const synth = all.filter((e) => e.synthesizedBy);
console.error(
  `  synthesized: ${synth.length} ` +
    `${JSON.stringify([...new Set(synth.map((e) => e.synthesizedBy))])}`
);
