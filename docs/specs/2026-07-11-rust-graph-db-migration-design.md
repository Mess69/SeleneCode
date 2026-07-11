# PRD — Portage de CodeGraph vers Rust + base graphe embarquée

**Date :** 2026-07-11
**Statut :** Cible d'architecture (le « quoi », pas l'ordre d'exécution)
**Auteur :** brainstorming CodeGraph
**Portée :** Réécriture de CodeGraph (aujourd'hui TypeScript + `node:sqlite`) en **Rust**, avec la couche de stockage passant de **SQLite/FTS5** à une **base graphe embarquée** (SurrealDB en mode embarqué, abstraite derrière un trait `GraphStore`).

> Ce document décrit l'**état cible**. Il n'impose pas de séquence de phases. Un plan d'exécution séparé (via `writing-plans`) décidera de l'ordre de construction.

---

## 1. Contexte & motivation

### 1.1 Ce qu'est CodeGraph aujourd'hui

Bibliothèque + CLI + serveur MCP de *code intelligence* **local-first**. Il parse n'importe quelle base de code avec tree-sitter, stocke symboles/arêtes/fichiers dans SQLite (FTS5), et expose un graphe de connaissances aux agents IA via MCP. Les données par projet vivent dans `.codegraph/`. L'extraction est **déterministe** (dérivée de l'AST, pas résumée par un LLM). Distribué comme `@colbymchenry/codegraph` sur npm ; le même binaire sert d'installeur, d'indexeur et de serveur MCP.

**Invariant produit à préserver coûte que coûte : local-first, zéro serveur, gratuit en local.** C'est la raison d'être du choix `node:sqlite` (aucune étape de build native, aucun serveur). Toute la valeur repose sur : un agent répond à une question **structurelle/de flux** (« comment X atteint Y », impact, callers) avec quelques appels codegraph **rapides** et **zéro Read/Grep**.

### 1.2 Pourquoi changer de stack (décision du mainteneur)

Trois moteurs retenus, par ordre d'importance :

1. **Performance & passage à l'échelle** — indexation et requêtes plus rapides sur gros monorepos ; parallélisme réel.
2. **Robustesse & correction** — sûreté mémoire/type de Rust, moins de bugs runtime, concurrence sans data-races, maintenabilité long terme.
3. **Distribution / binaire natif** — se débarrasser de la dépendance runtime Node, livrer **un seul binaire statique**.

Décision explicite sur la DB : **pas d'attachement à Neo4j**. Objectif = DB **plus performante, orientée graphe (nodes + links), embarquée, locale et gratuite**, quitte à ce qu'elle soit écrite en Rust. Un modèle graphe (nœuds + relations typées) est jugé plus pertinent que le relationnel pour ce produit.

> **Neo4j est écarté** : c'est un serveur JVM séparé à installer et faire tourner — collision frontale avec l'invariant local-first / zéro-install. On le remplace par une **base graphe embarquée** qui conserve cet invariant.

### 1.3 Rust vs TypeScript — gains et contreparties

**Gains**

- **Vitesse** : code natif, pas de warmup V8, abstractions à coût zéro.
- **Parallélisme** : parsing data-parallèle via **Rayon** au lieu de `worker_threads` + `parse-worker.ts`.
- **Mémoire** : pas de pauses GC, empreinte prévisible sur très gros monorepos.
- **Sûreté** : ownership/borrow → pas de data-races dans le daemon (lectures/écritures concurrentes) ; `match` exhaustif sur les enums `NodeKind`/`EdgeKind` **vérifié par le compilateur** (vs unions de chaînes en TS).
- **tree-sitter natif** : tree-sitter est C/Rust — on supprime la couche wasm actuelle (parse plus rapide, chaîne plus simple).
- **Distribution** : un binaire statique, aucune dépendance au runtime Node. Tout `node-version-check.ts` (bug Node 25.x, plancher Node 20) **disparaît**.

**Contreparties (assumées, à consigner)**

- Perte de l'itération rapide et de l'immense écosystème npm.
- Temps de compilation Rust ≫ `tsc`.
- Certaines dépendances TS (SDK MCP, `jsonc-parser`) ont des équivalents Rust **plus jeunes** → risque de maturité (voir §8).
- Toutes les grammaires tree-sitter n'ont pas de crate Rust maintenue de première classe (voir §8).

### 1.4 Base graphe embarquée vs SQLite — gains et contreparties

**Gains**

- **Modèle natif** : nœuds + arêtes typées de première classe ; plus de gymnastique de jointures.
- **Traversées à profondeur variable** : `getImpactRadius`, path-finding, callers/callees profonds deviennent des **traversées natives** au lieu de CTE récursives + BFS applicatif (`GraphTraverser`).
- **Adjacence sans index** : les bases graphe évitent les lookups d'index répétés que SQL paie sur les chemins profonds.
- **Toujours local-first** : SurrealDB/Kùzu s'embarquent (un fichier/dossier, comme SQLite) → l'invariant zéro-serveur est conservé.

**Contreparties honnêtes**

- SQLite est d'une maturité et d'une robustesse difficiles à égaler ; **FTS5 est excellent**.
- Les requêtes actuelles de CodeGraph sont **majoritairement 1–3 sauts**, que SQLite traite très bien. Le gain graphe est **concentré** sur les traversées profondes (impact radius, path-finding) et la clarté du modèle — **pas** un gain uniforme. Le gain objectif le plus large vient de **Rust** (perf, binaire, sûreté), pas du changement de DB en soi.

---

## 2. Inventaire des features à porter (parité cible)

| Domaine | Contenu actuel (TS) |
|---|---|
| **Extraction** | `ExtractionOrchestrator`, wrappers tree-sitter, **28 extracteurs de langage** (`languages/*.ts`), + extracteurs autonomes non-tree-sitter : `svelte`, `vue`, `liquid`, `dfm` (Delphi). `parse-worker.ts` (parsing hors thread principal). |
| **Résolution** | `ReferenceResolver` orchestre `import-resolver` (+ `path-aliases` : alias tsconfig, globs de membres cargo workspace), `name-matcher`, et `frameworks/` : **Express, Laravel, Rails, FastAPI, Django, Flask, Spring, Gin, Axum, ASP.NET, Vapor, React Router, SvelteKit, Vue/Nuxt, Cargo workspaces**. Émet des nœuds `route` + arêtes `references`. |
| **Dynamic-dispatch (synthétiseurs)** | callback/observer, EventEmitter, **React re-render** (`setState`→`render`), **JSX child** (`render`→composant enfant), descripteur ORM Django. Arêtes `provenance:'heuristic'` + `metadata.synthesizedBy`/`registeredAt`. |
| **Graphe** | `GraphTraverser` (BFS/DFS, impact radius, path-finding) + `GraphQueryManager`. |
| **Contexte** | `ContextBuilder` + formatter (markdown/JSON). |
| **Recherche** | Parseur de requête plein-texte + helpers pour **FTS5**. |
| **Sync** | `FileWatcher` (FSEvents/inotify/RDCW) avec debounce + filtre ; helpers git-hooks. |
| **MCP** | `MCPServer`, `tools.ts`, `transport.ts`, `server-instructions.ts`. Outils clés : `codegraph_explore` (PRIMAIRE), `codegraph_node` (SECONDAIRE), search, callers/callees/impact, files. Budgets d'explore scalés au nombre de fichiers. |
| **Installeur** | **8 cibles** : claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro. `registry` (`ALL_TARGETS`, flag `--target auto\|all\|none\|<id>`), sérialiseur TOML, éditions JSONC chirurgicales préservant commentaires, strip par marqueurs. |
| **CLI** | commander, ~22 sous-commandes : `install`/`uninstall`, `init`/`uninit`, `index`, `sync`, `status`, `query`, `explore`, `node`, `callers`/`callees`/`impact`, `files`, `affected`, `daemon`, `serve --mcp`, `unlock`, `prompt-hook`, `telemetry`, `upgrade`, `version`. |
| **Daemon** | Serveur persistant, socket `.codegraph/daemon.sock`, idle-timeout (`CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`), watchdog PPID (#277). |
| **DB** | `DatabaseConnection`, `QueryBuilder` (prepared statements), `schema.sql`, adaptateur `node:sqlite` (WAL + FTS5). |
| **Transverse** | `telemetry`, `upgrade`, `project-config`, `directory`, erreurs (`PathRefusalError`, `NotIndexedError`), UI terminal (progress shimmer). |
| **Types** | `NodeKind` (22 valeurs), `EdgeKind` (12 valeurs) — chaînes exactes partagées extracteurs/resolvers. |

---

## 3. Architecture cible — workspace de crates Rust

Frontières nettes : chaque crate a **un but clair**, communique par interfaces bien définies, se comprend et se teste indépendamment.

```
codegraph-core        Types partagés: NodeKind(22)/EdgeKind(12) → enums exhaustifs,
                      Node, Edge, Provenance{Static,Heuristic}, erreurs (PathRefusal, NotIndexed)
codegraph-db          trait GraphStore (open/index/query/fts) + backend SurrealDB embarqué + FTS
                      ← backend swappable derrière le trait (fallback permissif: IndraDB+Tantivy ou redb+Tantivy)
codegraph-extract     tree-sitter NATIF (crate `tree-sitter` + grammaires) + extracteurs autonomes
                      (svelte/vue/liquid/dfm) ; parallélisme Rayon (remplace parse-worker)
codegraph-resolve     ReferenceResolver, import-resolver (+ path-aliases), name-matcher,
                      frameworks/*, synthétiseurs dynamic-dispatch
codegraph-graph       Traverser (BFS/DFS, impact, path) + QueryManager ; traversées simples
                      poussées DANS la DB (SurrealQL), logique complexe en Rust
codegraph-context     ContextBuilder + formatter (markdown/JSON)
codegraph-mcp         Serveur MCP (crate rmcp), tools, transport, server-instructions
                      (source unique de la guidance agent — cf. issue #529)
codegraph-sync        FileWatcher via crate `notify` (FSEvents/inotify/RDCW), debounce, git-hooks
codegraph-installer   8 cibles + registry + writers TOML/JSON/JSONC (préservation format)
codegraph-cli         clap (remplace commander), daemon, telemetry, upgrade
codegraph (bin)       câble toutes les couches → un seul binaire statique
```

**Mapping des couches (identique à la pipeline actuelle) :**

```
fichiers → codegraph-extract (tree-sitter) → codegraph-db (nodes/edges/files)
                 ↓
          codegraph-resolve (imports, name-matching, frameworks, synthèse dyn-dispatch)
                 ↓
          codegraph-graph (callers, callees, impact, path)
                 ↓
          codegraph-context (markdown/JSON pour consommation IA)
                 ↓
          codegraph-mcp / codegraph-cli (surfaces)
```

L'équivalent Rust de `src/index.ts` (la classe `CodeGraph` qui câble tout et ré-exporte les types) devient l'API publique de la crate `codegraph` (façade `struct CodeGraph`).

---

## 4. Modèle de données graphe + stratégie FTS

### 4.1 Nœuds & relations

Le mapping vers un **property-graph** est quasi 1-pour-1 :

- Chaque **Node** (les 22 `NodeKind` : `file`, `module`, `class`, `struct`, `interface`, `trait`, `protocol`, `function`, `method`, `property`, `field`, `variable`, `constant`, `enum`, `enum_member`, `type_alias`, `namespace`, `parameter`, `import`, `export`, `route`, `component`) → **un nœud** portant `kind`, `name`, `file`, `line`/`span`, `metadata`.
- Chaque **Edge** (les 12 `EdgeKind` : `contains`, `calls`, `imports`, `exports`, `extends`, `implements`, `references`, `type_of`, `returns`, `instantiates`, `overrides`, `decorates`) → **une relation typée** portant `provenance` (`static` | `heuristic`) et, pour les arêtes synthétisées, `synthesizedBy` + `registeredAt`.
- Les arêtes **synthétisées** (callback, EventEmitter, react-render, jsx-child, ORM Django) restent des relations `heuristic`, surfacées inline dans `codegraph_explore` (section Flow) et le trail de `codegraph_node`.

**C'est là que le graphe bat SQL** : `getImpactRadius` et le path-finding deviennent des **traversées natives à profondeur variable** (ex. SurrealQL `->calls->...`) au lieu de CTE récursives + BFS applicatif.

### 4.2 Recherche plein-texte (FTS)

Remplacement de FTS5. **Stratégie retenue : FTS natif de SurrealDB** (index full-text intégrés) → **une seule dépendance** de stockage, pas de moteur de recherche séparé.
**Plan B consigné :** si le FTS natif est insuffisant (analyzers, ranking), intégrer **Tantivy** (le « Lucene de Rust ») comme index FTS dédié à côté du `GraphStore`. Le trait `GraphStore` isole ce choix du reste du code.

---

## 5. Choix de la DB

### 5.1 Décision : SurrealDB en mode embarqué

Requête reine = **traversée à profondeur variable** + **FTS sur les symboles**. Contrainte dure : **embarqué, local, gratuit, Rust-natif**. Paysage vérifié en juillet 2026 (les licences/statuts évoluent — à re-vérifier avant implémentation) :

| Critère | **SurrealDB** | IndraDB | Cozo | ~~KùzuDB~~ |
|---|---|---|---|---|
| Langage | Rust | **Rust** | **Rust** | C++ |
| Modèle | Multi-modèle, `RELATE` | Graphe typé, multi-hop | Relationnel+graphe (Datalog) | Property-graph |
| FTS intégré | ✅ natif | ❌ (→ Tantivy) | ✅ natif | ✅ |
| Perf traversée profonde | Bonne | Bonne | Bonne (récursion Datalog) | Excellente |
| Licence | ⚠️ **BSL 1.1** (pas OSI) | ✅ **MPL-2.0** | ✅ **MPL-2.0** | MIT |
| Activité | ✅ Très active | ✅ v5.0.0 (août 2025) | ❌ dernier commit déc. 2024 | ❌ **archivé oct. 2025 (Apple)** |
| Verdict | Complet, licence non-OSI | Permissif+vivant, sans FTS | Riche mais dormant + pré-1.0 | **Mort en OSS** |

**Retenu : SurrealDB embarqué** (crate `surrealdb`, backend `kv-rocksdb` ou `kv-surrealkv`). Raisons : (a) préférence explicite du mainteneur ; (b) **batteries incluses** — graphe *et* FTS *et* stockage dans une seule crate, architecture plus simple ; (c) Rust-natif, gratuit en local ; (d) le plus actif/mature du lot.

**Licence — analyse (pas un avis juridique) :** cœur SurrealDB en BSL 1.1 (composants en Apache 2.0). L'**Additional Use Grant autorise gratuitement l'embarquement** dans une application distribuée aux utilisateurs ou exploitée comme service — la seule interdiction est d'offrir *SurrealDB lui-même* en DBaaS concurrent, ce que CodeGraph ne fait pas. Chaque version passe en **Apache 2.0 quatre ans après sa sortie** (v3.0 → 2030-01-01). **Donc : usage légal et gratuit, y compris pour un projet open source.** Réserve résiduelle : la BSL n'étant **pas OSI-approuvée**, la distribution agrégée de CodeGraph n'est plus 100 % OSI-open, ce qui peut freiner le packaging (Debian/Fedora) et l'adoption en entreprise à politique OSS stricte. Non-copyleft → n'oblige pas à relicencier le code de CodeGraph.

### 5.2 Abstraction `GraphStore` (obligatoire)

Toute la DB est derrière un **trait `GraphStore`** dans `codegraph-db`. Le reste du code ne connaît jamais SurrealDB directement. Bénéfice : porte de sortie garantie. **Fallback permissif (100 % OSI)** si la BSL devient bloquante — Kùzu (l'ancien plan B MIT) étant désormais **archivé/mort** (acqui-hire Apple, oct. 2025), le repli est :

- **IndraDB (MPL-2.0) + Tantivy (MIT)** — store graphe typé multi-hop prêt à l'emploi + FTS dédié ; ou
- **redb (MIT/Apache) + Tantivy (MIT)** — store KV pur-Rust + traversal maison dans `codegraph-graph` (que CodeGraph implémente déjà) + FTS dédié. Voie la plus contrôlée, zéro risque d'abandon d'une DB-produit de niche.

### 5.3 Réserves à lever par benchmark (gate)

1. **Perf de traversée profonde** — Bench requis : temps d'`impact radius` / path-finding profond sur gros repo. Si SurrealDB déçoit → bascule vers un fallback via le trait `GraphStore`.
2. **Licence BSL 1.1** — usage/embarquement gratuit et légal confirmé (§5.1), mais **non-OSI** : réserve de packaging/adoption, pas de blocage juridique. Mitigée par le trait `GraphStore` + le fallback permissif ci-dessus.
3. **Écritures bulk** — l'indexation initiale d'un gros monorepo doit rester rapide (transactions batch, WAL-like).

### 5.4 Travail de recherche à mener — quelle logique porter du *code* vers la *DB* ? (spike)

> **Ce n'est pas encore une décision d'architecture, c'est un travail de recherche à faire.** Le PRD acte le principe ; le spike tranche la ligne de partage.

Constat (et avantage réel du passage au graphe) : aujourd'hui la traversée est faite en **code applicatif** (`GraphTraverser` BFS/DFS, `GraphQueryManager`). Avec une DB graphe, **une partie de cette logique peut descendre dans la DB** sous forme de requêtes déclaratives (SurrealQL `->calls->...`, chemins à profondeur variable, filtres par `provenance`/`kind`). Bénéfices attendus : moins de code Rust, adjacence sans index, traversées profondes potentiellement plus rapides.

Ce qu'il faut **rechercher/prototyper** avant de figer l'API du trait `GraphStore` :

1. **Cartographier ce qui peut descendre dans la DB** — callers/callees (1 hop), impact radius (profondeur variable), path-finding, filtres par kind/provenance. **Vs ce qui reste forcément en Rust** : les heuristiques complexes de `buildFlowFromNamedSymbols` (désambiguïsation segment/co-naming, biais d'overload, ≤ 1 pont non-nommé), la synthèse dynamic-dispatch, les budgets d'explore.
2. **Benchmark in-DB vs applicatif** — un `impact radius` profond exprimé en SurrealQL est-il réellement plus rapide que le BFS Rust ? À partir de quelle profondeur / taille de repo le gain apparaît-il ?
3. **⚠️ Tension avec la portabilité (`GraphStore`)** — plus on pousse de logique dans SurrealQL, plus le **fallback permissif** (IndraDB/redb, qui sont des *stores sans langage de requête*) coûte cher : cette logique devrait être **réécrite en Rust** en cas de bascule. → Deux stratégies possibles, à trancher :
   - **(a) Primitives portables** : le trait `GraphStore` expose des primitives bas-niveau (ex. `neighbors(node, edge_kind, provenance, depth)`) implémentables par *tous* les backends ; la logique de haut niveau reste en Rust. Portabilité maximale, on exploite moins SurrealQL.
   - **(b) Couplage assumé** : on pousse un maximum de traversée dans SurrealQL (moins de code, potentiellement plus rapide) en acceptant qu'un fallback OSI soit plus coûteux à réimplémenter.
4. **Livrable du spike** : une note fixant la **ligne de partage code/DB** retenue + les résultats de benchmark, **produite avant de geler l'API du trait `GraphStore`** (dont dépendent `codegraph-graph`, `codegraph-mcp`, `codegraph-cli`).

---

## 6. Mapping feature-par-feature TS → Rust

| Feature TS | Équivalent Rust cible |
|---|---|
| tree-sitter (wasm) | crate `tree-sitter` + crates de grammaire par langage (natif, pas de wasm) |
| `parse-worker.ts` (worker_threads) | **Rayon** (parsing data-parallèle) |
| `node:sqlite` + `schema.sql` + FTS5 | crate `surrealdb` embarquée derrière `trait GraphStore` + FTS natif (plan B : Tantivy) |
| `QueryBuilder` (prepared statements) | requêtes SurrealQL paramétrées dans `codegraph-db` |
| `GraphTraverser` (BFS/DFS applicatif) | traversées natives SurrealQL + logique résiduelle en Rust |
| commander (CLI) | **clap** |
| SDK MCP (TS) | crate **rmcp** (SDK MCP Rust officiel) |
| `FileWatcher` (FSEvents/inotify/RDCW) | crate **notify** (unifie les 3 backends OS) |
| `toml.ts` (sérialiseur maison) | **toml_edit** (édition préservant le format) |
| `jsonc-parser` (éditions chirurgicales) | crate **jsonc-parser** (Rust, dprint) |
| UI terminal (shimmer) | **indicatif** + **crossterm** |
| `fs.mkdtempSync` (tests) | **tempfile** ; snapshots via **insta** ; `cargo test` |
| unions de chaînes `NodeKind`/`EdgeKind` | **enums** Rust exhaustifs (match vérifié compilateur) |
| erreurs `PathRefusalError`/`NotIndexedError` | enum d'erreurs `thiserror` ; sémantique préservée (cf. §8) |

---

## 7. Distribution du binaire

- **Un binaire statique** cross-compilé : darwin (arm64/x64), linux (x64/arm64, gnu + musl), windows (x64/arm64).
- **Shim npm** (modèle esbuild / @biomejs) : un paquet léger `@colbymchenry/codegraph` détecte la plateforme et télécharge le binaire pré-compilé. → **`npx @colbymchenry/codegraph` continue de fonctionner** ; les utilisateurs npm actuels ne cassent pas.
- Canaux additionnels : `cargo install`, Homebrew, GitHub Releases (avec `SHA256SUMS`).
- **Disparaît** : toute la logique `node-version-check.ts` (bug Node 25, plancher Node 20, engines).

---

## 8. Risques & invariants à ne pas régresser

### 8.1 Risques du portage

- **Couverture des grammaires** : toutes les 28 grammaires tree-sitter n'ont pas de crate Rust maintenue de 1re classe → certaines à vendre/vendoriser. Les 4 extracteurs autonomes (svelte/vue/liquid/dfm) sont faits main et **doivent être réimplémentés**.
- **Maturité SurrealDB embarqué** + perf traversée → **gate benchmark** (§5.3).
- **Licence BSL non-OSI** de SurrealDB → réserve de packaging/adoption (§5.1) ; fallback permissif prêt derrière `GraphStore` (§5.2).
- **Volatilité de l'écosystème DB graphe** : Kùzu (MIT, mature) est mort en 2025 (Apple), Cozo est dormant → re-vérifier statut/licence des dépendances DB avant de committer l'implémentation.
- **Maturité du SDK MCP Rust (`rmcp`)** plus jeune que le SDK TS.
- **Parité installeur** : 8 cibles + éditions préservant commentaires/format ; c'est là que les régressions cassent silencieusement chaque nouvelle install.
- **Temps de compilation Rust** ; perte de l'écosystème npm.

### 8.2 Invariants produit (règle d'or : ne pas régresser)

- **Sufficiency / anti-Read** : une question de flux se résout en **1 appel explore sur petit repo, 3–5 sur gros**, avec **Read/Grep = 0**. Le budget explore doit rester **monotone avec la taille du repo** (`getExploreBudget`, `getExploreOutputBudget` ; invariant : un tier plus gros n'a jamais un `maxCharsPerFile` plus petit).
- **`isError` réservé** : uniquement pour les vrais « arrête d'essayer » (refus sécurité `PathRefusalError`, vrais dysfonctionnements). Toute condition attendue/récupérable (non indexé, symbole introuvable) renvoie une réponse **shape succès** portant la guidance (`NotIndexedError` → `textResult`). Une ou deux réponses `isError` tôt = l'agent abandonne codegraph.
- **Surface d'outils toujours exposée**, même à une racine non indexée (monorepos).
- **Couverture dynamic-dispatch end-to-end** : ne jamais livrer un flow à moitié bridgé (partiel = pire que rien). Fermer le flow de bout en bout et re-mesurer.
- **`server-instructions` = source unique** de la guidance agent (issue #529).
- **Extraction déterministe** (dérivée AST, jamais LLM).

---

## 9. Critères de succès

**Parité fonctionnelle**

- Les 28 langages + 4 extracteurs autonomes extraient les mêmes symboles (comparaison node/edge count vs build TS, à tolérance près).
- Les 15 frameworks + 5 synthétiseurs dyn-dispatch produisent les mêmes `route`/`references`/arêtes heuristiques.
- Les 8 cibles d'installeur passent les ~97 tests de contrat (idempotence, préservation des voisins, uninstall réversible, re-run byte-égal `unchanged`).
- Surface MCP + CLI équivalente ; budgets explore identiques.

**Performance (mesurée, pas supposée)**

- Temps d'indexation d'un gros monorepo **< build TS**.
- Latence de requête (callers/callees/impact/explore) **≤ build TS**, avantage attendu sur les traversées profondes.
- Cold-start du binaire **< startup Node+MCP actuel** (~2–3 s).
- Taille binaire raisonnable ; empreinte mémoire stable sur gros repos.
- Validation A/B sur la matrice de couverture (méthodologie `docs/design/dynamic-dispatch-coverage-playbook.md`) : une question de flux atteint **~0 Read/Grep** dans le budget explore du repo, **plus vite** que sans codegraph, **sans régression** sur un repo témoin.

---

## Annexe A — Décisions ouvertes (à trancher au plan d'exécution)

- **Licence** : SurrealDB retenu (embarquement BSL gratuit et légal). Décision résiduelle : tolérer une dépendance non-OSI dans la distribution, ou basculer sur le fallback 100 % OSI (IndraDB+Tantivy / redb+Tantivy) — arbitrage packaging/adoption, réversible via le trait `GraphStore`.
- **Répartition traversée** : quelle part de la logique graphe pousser *dans* la DB (SurrealQL) vs garder en Rust (`codegraph-graph`) ? → **spike de recherche dédié en §5.4** (à mener avant de figer l'API `GraphStore`).
- **Ordre de construction** : hors périmètre de ce PRD (cible d'architecture). À décider via `writing-plans` si/quand l'exécution démarre.
