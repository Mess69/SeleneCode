# RESUME — reprendre SeleneCode après un redémarrage

**Écrit le 2026-07-13, mis à jour le 2026-07-14 (`c0c7143` — le blocage est levé).** Ce fichier est
la **seule chose à lire** pour repartir. Il suppose que tu as tout oublié — c'est voulu.

---

## 0. La commande à donner à Claude au redémarrage

> « Lis `RESUME.md` et reprends. »

Tout le reste de ce fichier est là pour que cette phrase suffise.

---

## 1. Où on en est, en une phrase

**`explore` RÉPOND.** Le blocage unique de la session précédente — *« le produit tourne et ne
répond pas »* — est **levé et mesuré contre le vrai binaire** (`c0c7143`). La question exacte du
gate Task 20 renvoie maintenant **3/3** avec une section Flow juste. Ce qui reste est du travail
planifié, plus un blocage.

| | état |
|---|---|
| Phases 1, 2, 3 (db, extract, resolve) | ✅ **mergées sur `main`** (`ba29336`), gates verts |
| Phase 4 (graph + context) | ✅ code fini — **et `explore` répond enfin** (§2) |
| Phase 5 (MCP + binaire) | 🟡 écrit et commité, **Tasks 19–20 restent** |
| Perf | ✅ **6× + 2,5×** — voir §3 |
| Phases 6, 7 (CLI/daemon, installer) | ✅ **plans écrits et arbitrés**, 35 tâches prêtes |
| Phases 8, 9 (langages wave-2, parité, v1) | ⬜ roadmap seulement |

**Branche de travail : `feat/phase45-graph-context-mcp`** (PAS mergée).
`main` est à `ba29336` (fin de Phase 3).

**Toute la suite : 1 089 tests, 0 échec.** Parity 6/6, dispatch 5/5, phase4 7/7.

---

## 1 bis. « Est-ce que c'est prêt ? Est-ce que c'est CodeGraph en Rust ? »

**Non.** Ça **marche**, ce n'est pas le **produit**. Distinction qui coûte cher si on la rate.

### Ce qui existe vraiment

Un binaire unique, SurrealDB embarqué (RocksDB), qui **indexe** et **sert du MCP**. Vérifié en vrai,
pas déduit : `selene index` + `selene serve --mcp` + `explore` répond (§2).
**11–12 langages** (c, cpp, go, java, js, kotlin, php, python, ruby, rust, ts).

### Ce qui n'existe PAS — et ce sont des stubs de 3 lignes, pas des « presque finis »

| crate | lignes | conséquence concrète |
|---|---|---|
| `selene-cli` | **3** | **2 commandes en tout** : `index`, `serve`. Pas de `status`, rien d'autre. |
| `selene-sync` | **3** | **Tu réindexes À LA MAIN quand ton code change.** Pas de watch, pas d'incrémental branché. |
| `selene-installer` | **3** | Pas de `selene install`. La config MCP s'écrit **à la main**. |

*(`selene-resolve` (17 k lignes) et `selene-graph` sont **implémentés** — le mot « stub » traîne dans
leur doc de module et trompe un `grep`. Ne te fais pas avoir.)*

### Les trois trous qui séparent « ça tourne » de « ça tient sa promesse »

1. **Task 20 — le gate du jalon — n'est pas écrit.** Personne n'a **prouvé** qu'un agent répond avec
   **zéro Read/Grep** sur un gros dépôt (VS Code, 11 938 fichiers). C'est *la* promesse du produit.
   Elle est **plausible, pas démontrée**. Tout le reste est décoration tant que ce n'est pas mesuré.
2. **Task 19 — discipline `isError`** pas faite. Un `?` qui s'échappe d'un handler et l'agent
   abandonne l'outil **pour toujours** (§5.B).
3. **`explore` n'est prouvé que sur TS et Rust.** Le gate Phase 4 tourne sur **2 projets, tous les
   deux TS**. Python/Django, Go, Java/Spring : le code existe, **rien ne le prouve**.

### Utilisable dès maintenant, avec ces réserves

```bash
cargo build --release -p selene
./target/release/selene index /chemin/du/repo
# puis pointer la config MCP sur :
#   ./target/release/selene serve --mcp --path /chemin/du/repo
```

---

## 2. LE BLOCAGE EST LEVÉ (`c0c7143`) — lire ceci avant de retoucher la pertinence

### Ce que fait `explore` maintenant

`./scripts/ask.sh "how does an unresolved reference become a graph edge"` — la question **exacte**
du gate Task 20 :

```
  batch.rs SHOWN: True | resolve_one: True | resolve_and_persist_batched: True  => 3/3
  Flow: True (3 étapes)

    1. resolve_and_persist_batched  (selene-resolve/src/batch.rs:113)
    2. insert_edges                 (selene-db/src/store_impl.rs:105)
    3. Edge                         (selene-core/src/lib.rs:346)
```

Avant : **0/3, aucun Flow**, et des seeds (`ReferenceResolver`, `UnresolvedReference`) qui sont des
**types** — on n'appelle pas un type.

### L'instrument était la DIRECTION, pas le poids. Ne recommence pas par les poids.

La session précédente avait prouvé qu'**aucun repondération ne pouvait marcher**, et elle avait
raison : la réponse score ≈12 après le ×0,6 de la passe 5, `ReferenceResolver` score ≈143 pour avoir
*épelé* deux mots de la requête, et la passe 12 est additive et plafonnée à 30. On ne monte pas de
12 à 143. Deux correctifs *dans* le multiplicateur ont été mesurés et annulés (ils sont consignés
dans `term_groups()` — **ne les retente pas**).

Ce qu'elle a manqué : **sa propre contre-preuve était directionnelle.** Injecter la connectivité
dans le score promouvait `file_node_id`, `hash_content`, `node_id` — *« la couche utilitaire touche
tous les concepts »*. Vrai. Et le signe distinctif est dans les degrés :

```
  classement des 1 460 callables non-test par concepts couverts via les appels SORTANTS :
    #2  resolve_and_persist_batched  {EDGE, RESOLVE}  out=29 in=6     <- la réponse
    #3  resolve_one                  {REFER,RESOLVE}  out=24 in=27

  témoin — le MÊME dépôt classé par DEGRÉ BRUT (ce que récompense un score non orienté) :
    collect out=15 in=527 · get_node_text out=0 in=205 · as_str out=0 in=177 · default out=0 in=95
```

**Toute la plomberie qui avait fait échouer la tentative précédente a `out=0` et un `in` énorme.**
Un utilitaire est *appelé par* tout ; un orchestrateur *appelle* tout. La passe 12 score
`deg_out + deg_in` : elle ne peut pas distinguer un pilote d'un utilitaire. Restreindre aux appels
sortants ne *dévalue* pas la plomberie — elle la rend **structurellement inéligible** (une fonction
qui n'appelle rien ne couvre rien). Aucun amortissement nécessaire.

⇒ **passe 14 — réservation d'orchestrateur** (`relevance.rs`). Un callable dont les appels sortants
couvrent ≥2 concepts de la requête prend 1 des 2 slots de root, **derrière le root 1**, en
remplaçant les plus faibles (le budget de roots reste fixe — la passe 11 divise `max_nodes` entre
les roots, donc un root *ajouté* amincit tous les autres : c'est l'échec consigné dans
`pick_diverse_roots`).

### Le Flow : « la plus longue chaîne gagne » était faux depuis toujours

Un petit ensemble `named` le masquait. Sur un vrai sous-graphe, « finit sur un symbole nommé »
devient **vide de sens** (tout est nommé) et « la plus longue » dégénère en « la plus profonde » :

```
  1. resolve_and_persist_batched  2. resolve_all  3. resolve_one     <- juste
  4. resolve_via_import ... 8. WorkspacePackages                     <- rien à voir avec « edge »
```

Chaque saut est une vraie arête d'appel, et l'ensemble est **hors-sujet**. Exiger « finir sur
quelque chose de pertinent » ne suffit pas non plus : `resolve_workspace_import` **est** pertinent —
tout le crate s'appelle `resolve_*`. Ce qui manquait, c'est **l'ARRIVÉE** : *« comment X devient
Y »* se répond par une chaîne qui **part d'un pôle et finit sur l'AUTRE**. Une chaîne de `resolve`
à `resolve` n'explique rien, quelle que soit sa longueur. Parmi celles qui arrivent, **la plus
serrée gagne**.

### ⚠ Ce qu'il ne faut PAS refaire (mesuré, dans cette session)

- **Ancrer sur les types et chercher un plus court chemin entre leurs « handlers ».** Testé :
  `references` est une lance à incendie (2 666 arêtes) → 155 handlers d'un côté, 96 de l'autre →
  le plus court chemin entre deux ensembles aussi gros trouve la paire de **déchets** la plus
  proche (`visit_node → create_node`). Le plus-court-chemin est le mauvais objectif.
- **Élargir la liste des kinds interdits en sink** au-delà de celle de TS
  (`{constant, variable, field, property}`). Ajouter import/export/parameter *paraît* raisonnable
  et **supprime le Flow** d'un projet à 2 fichiers dont la seule épine à 3 nœuds passe par un nœud
  d'import.

### Non sur-ajusté — vérifié contre un binaire construit depuis HEAD, sur 4 requêtes jamais réglées

| requête | avant | après |
|---|---|---|
| how does a file get indexed | `… → pool → build_pool → build → map` | `index → index_all → run_pipeline → get_file` |
| how are nodes stored | `fresh_mem`(un **bench**) `→ in_memory → DATABASE`(une **const**) | `bulk_load → load → Node` |
| what happens when a file is deleted | *(inchangé)* | *(inchangé)* |
| how does the mcp server handle a tool call | *(inchangé)* | *(inchangé)* |

**4 améliorées, 2 inchangées, 0 régressée.**

### ⚠ La sonde elle-même mentait

`scripts/ask.sh` faisait un `in` sur **tout** le texte : un fichier simplement **nommé** dans la
liste du blast-radius comptait comme un succès. Il annonçait 2/3 alors que la vérité était **1/3**.
Un fichier *nommé mais non affiché* est le **pire** résultat possible : il désigne à l'agent un
fichier qu'il doit ensuite ouvrir. La sonde teste maintenant les **sections de fichier rendues**.

---

## 2 bis. CE QUI RESTE OUVERT sur `explore` (pré-existant, reproduit, NON corrigé)

1. **Les tests dans `src/` pilotent les flows.** `is_test_file` teste le **chemin**, or Rust met ses
   tests unitaires dans `#[cfg(test)] mod tests` **à l'intérieur du fichier source** — donc
   `explore_is_the_only_default_visible_tool` **amorce un flow**. La règle « un test ne peut pas
   être un ROOT » existe déjà ; c'est son implémentation qui suppose la convention TS
   (`.test.ts` séparés). Piste : le `qualified_name` commence par `tests::`.
2. **5 `EdgeKind` sont à ZÉRO dans un vrai index Rust** : `type_of`, `returns`, `overrides`,
   `decorates`, `exports`. `decorates`/`exports` : normal sur du Rust. **`type_of` et `returns` :
   PAS normal** — Rust est plein de paramètres typés et de types de retour. Ils semblent émis comme
   `references`. Déclarés dans l'enum, jamais peuplés : exactement la forme du « seam inerte » que
   ce projet paie en boucle (§9). À trancher : les peupler, ou les consigner comme déviation.

## 3. Perf — ⛔ **ON EST 8 À 11× PLUS LENTS QUE LE BUILD TS.** (mesuré le 2026-07-14)

### Le benchmark a été fait. Le résultat est mauvais.

| corpus | fichiers | **selene (Rust)** | **codegraph (TS)** | écart | nœuds S / TS | arêtes S / TS |
|---|---:|---:|---:|---|---|---|
| codegraph/src (TS) | 162 | 18,4 s | **2,4 s** | **TS 7,7×** | 3 803 / 3 803 | 14 081 / 14 078 |
| SeleneCode/crates (Rust) | 328 | 22,8 s | **2,8 s** | **TS 8,2×** | 5 086 / 5 090 | 17 192 / 17 255 |
| django (Python) | 931 | 61,1 s | **5,7 s** | **TS 10,7×** | 19 061 / 19 063 | 46 942 / 46 488 |

**Le graphe produit est le MÊME** (nœuds à 4 près, arêtes à ~1 %). Donc « plus rapide » ne veut
**pas** dire « en fait moins » : TS fait le même travail, en un dixième du temps. **Et l'écart se
creuse avec la taille** (7,7× → 10,7×) : on ne part pas seulement derrière, on **scale moins bien**.

**Ce n'est PAS Rust qui est lent — c'est LA BASE.** Sur django (61 s) :

```
    ladder (le VRAI travail de résolution)    8,4 s   ← 14 %
    persist (l'écriture)                     29,5 s   ← ~48 % du TOTAL
```
```
    log RocksDB : « Sync mode: every transaction commit »   ← fsync à CHAQUE commit
                  inline-blocking granted = 2 464 643
```

CodeGraph écrit dans **`node-sqlite` en mode WAL** (qui groupe et ne fsync pas par transaction).
Nous, dans **SurrealDB/RocksDB qui fsync à chaque commit**. Le ladder que tout le monde croyait cher
fait **14 %** du temps. La base possède les cinq sixièmes restants.

⚠ **Ça contredit la décision d'archi centrale du projet** (« SurrealQL-max », PRD §5.2/§5.4) : elle a
été prise sur des arguments de **lecture** (traversée récursive dans le moteur) et n'a **jamais été
chiffrée côté ÉCRITURE** — or c'est là que le produit passe son temps. Le gate DB Phase 1 qui a l'air
d'avoir tranché comparait **SurrealKV à RocksDB**, *deux backends à nous*. Jamais SurrealDB à SQLite.
Ça ne veut pas dire « arrache SurrealDB » : ça veut dire que la décision **repose désormais sur une
mesure qui la contredit** et doit être ré-argumentée, pas héritée.

**Détail complet, méthode et prochaines expériences :
`docs/benchmarks/2026-07-14-rust-vs-ts-speed.md`.**

### Les deux régressions à NOUS, corrigées (contexte, ne pas refaire)

`selene index` sur codegraph (162 fichiers) : **52,4 s → 20,6 s**. Sur SeleneCode : **8 m 52 s →
1 m 29 s**. Sortie **identique** à chaque fois (aucune arête perdue), gates Phase 3 verts.

**Ces chiffres sont Rust contre SON PROPRE PASSÉ** — on a corrigé deux bugs à nous. Ils ne disaient
rien sur TS, et on le sait maintenant : *« on était mauvais, on l'est moins, et on reste 8× derrière »*.

**L'argument qu'on se racontait** (et qui était faux) : le port supprime la couche WASM (pool de
workers, resets de parser, retries OOM) et lie tree-sitter nativement, donc il doit être devant.
C'est probablement **vrai pour l'extraction** — et **hors sujet**, parce que l'extraction n'est pas
le goulot. On a remplacé un SQLite embarqué rapide par une base multi-modèle généraliste, et on le
paie **à chaque écriture**.

**Deux bugs, tous deux instructifs :**

1. **Le binaire tournait sur le backend que son propre benchmark avait rejeté.** (`81c2437`)
   La Phase 1 avait mesuré SurrealKV à **46 nœuds/s vs RocksDB 706** (« pathologically slow ») et
   fait de RocksDB le défaut. Mais `selene-mcp/Cargo.toml` demandait `kv-surrealkv`, et **Cargo
   unifie les features sur tout le graphe de dépendances** : cette ligne, à trois crates de
   distance, écrasait le défaut pour tout le produit. `SurrealStore::open` préfère SurrealKV dès
   qu'il est **compilé** (surreal.rs:93 vs :106) — **le compiler, c'est le choisir**.
   *Personne ne l'avait vu parce que personne n'avait jamais EXÉCUTÉ le binaire.*

2. **Persist = 82 % du temps d'indexation.** (`899aea6`)
   Mesuré via les spans par phase (le ladder ne fait que **5 %**) :
   ```
   ladder     2 578 ms    ← le vrai travail de résolution :  5 %
   persist   42 779 ms    ← l'écriture :                    82 %
   synthesis    243 ms
   ```
   `delete_resolved`/`mark_failed` filtrent sur la clé en 3 parties
   `(fromNodeId, referenceName, referenceKind)` — **aucun index ne la couvrait**
   (`referenceKind` n'en avait aucun). Un `DEFINE INDEX` composite : **42,8 s → 11,0 s**.

**Levier restant (NON fait, optionnel) :** persist est encore ~54 %, car
`run_keyed_statements` émet **une requête DELETE par clé** (22 462 requêtes). Une seule requête
par chunk les effondrerait.
⚠ **MAIS** : la clé doit rester le **tuple exact à 3 champs**. Une clé concaténée/hashée peut
**collisionner**, et ce projet a **déjà perdu des données** à cause d'un delete par clé qui matchait
la mauvaise ligne (l'incident #760, clé à 2 champs). Ne prends pas ce raccourci.

⛔ **Ne parallélise PAS le resolve ladder.** J'avais écrit ça dans un commit avant de mesurer :
c'est **faux**, le ladder fait 2,6 s. Le paralléliser parfaitement ne gagnerait presque rien.

---

## 4. Gates — lesquels croire

| gate | état | à savoir |
|---|---|---|
| `selene-resolve` parity + dispatch | ✅ 11/11 | **Fiables.** Comparent l'*identité* des arêtes vs le build TS, tolérance 0. |
| `scripts/ask.sh` (le vrai binaire) | ✅ 3/3 | **LE seul qui prouve `explore`.** Vrai MCP, vrai dépôt. La sonde a été corrigée (§2) — elle mentait. |
| `selene-context/tests/phase4_gate.rs` | ✅ 7/7 | ⚠ Corpus = **2 projets, tous TS**. Il était vert pendant que `explore` ne répondait pas : ne t'y fie **jamais** seul. |
| Task 20 (le gate du jalon) | ⬜ **pas écrit** | C'est LUI qui prouve le produit de bout en bout. §5. |

⚠ **`cargo test --workspace | head` t'a déjà menti dans cette session** : la troncature a caché un
test en échec et j'ai annoncé « tout vert » à tort. **Compte les échecs, ne les regarde pas défiler :**
```bash
cargo test --workspace 2>&1 | grep -c 'test result: FAILED'   # doit afficher 0
```

**À faire aussi (findings du reviewer `rev13`, non traités) :**
- élargir le corpus du gate Phase 4 à **≥6 projets** couvrant TS/React, **Python/Django, Go, Rust,
  Java/Spring** + un synth. Aujourd'hui : 2, tous TS. Donc chaque assertion est **non prouvée pour
  4 des 5 familles de langages** qu'on livre.
- `get_dominant_file()` n'a pas de primitive store → la passe 4 du scoring est un **no-op silencieux
  qui a l'air de marcher**. L'implémenter ou l'enregistrer comme déviation explicite.
- snapshots `insta` (Task 13 half 2) + table de budgets dans `docs/benchmarks/` : jamais faits.
- ⬜ **AUCUN benchmark de VITESSE vs le build TS.** Les trois docs de `docs/benchmarks/` mesurent la
  *justesse* (identité des arêtes, tolérance 0), jamais le temps. On **ne sait pas** si on est plus
  rapide que CodeGraph. `../codegraph` est là : c'est une demi-heure de travail (§5.A bis).

---

## 5. Ce qui reste — dans l'ordre

### A. ~~Débloquer `explore`~~ ✅ **FAIT** (`c0c7143`, §2)
La question du gate renvoie 3/3 avec un Flow juste. **Avant de retoucher la pertinence, lis §2** —
en particulier la liste « ce qu'il ne faut PAS refaire » : trois approches y sont déjà mesurées et
mortes.

### A bis. Les deux défauts pré-existants d'`explore` (§2 bis)
Ni l'un ni l'autre ne bloque Task 20, mais le premier laisse un **test piloter une réponse** :
tests dans `src/` (Rust met ses tests DANS le fichier) ; et `type_of`/`returns` à zéro dans l'index.

### A ter. Le benchmark qu'on n'a jamais fait : **Rust vs TS, en vitesse** (~30 min)
On **ne sait pas** si on bat CodeGraph. Tous nos chiffres de perf sont *Rust contre son propre passé*
(§3). `../codegraph` est sur le disque — les deux builds peuvent indexer le **même dépôt**.

Protocole, et **le second point n'est pas optionnel** :
1. Même corpus, même machine, plusieurs tailles (petit / moyen / VS Code), chronomètre.
2. **Comparer aussi les nœuds/arêtes produits** — sinon « plus rapide » peut simplement vouloir dire
   « il en fait moins ». C'est exactement l'erreur que ce projet répète (§9).
3. Écrire le résultat dans `docs/benchmarks/`, **même s'il est mauvais.**

### B. Finir la Phase 5
- **Task 19 — la discipline `isError`.** Le piège est documenté par le spike du projet lui-même :
  **rmcp 2.2 a TROIS issues** — `Ok(success)` → `isError:false` ; `Ok(CallToolResult::error(..))`
  → `isError:true` ; **`Err(ErrorData)` → échec de transport JSON-RPC -32603, PAS `isError:true`.**
  ⇒ **ne jamais laisser un `?` sur une erreur de store s'échapper d'un handler.**
  Invariant : `isError` est **réservé** (PathRefusal + vrai bug). Tout le reste (« pas indexé »,
  « symbole introuvable », « rien trouvé ») renvoie une **guidance success-shaped**.
  *Une seule `isError` prématurée et l'agent abandonne l'outil pour toujours.*
- **Task 20 — LE GATE DU JALON.** Faits déjà mesurés dans
  `.superpowers/sdd/task-20-facts.md` — **le dépôt large est VS Code (11 938 fichiers), PAS
  Django** (2 926 fichiers, sous la barre des 5 000 ; le plan dit explicitement *« ne pas assouplir
  la ligne pour l'adapter au dépôt »*). Les deux sont clonés en frères : `../vscode`, `../django`.
  La question de flux VS Code est **tracée à la main et vérifiée** (voir le fichier de faits) :
  `_doDispatch → _commandService.executeCommand → CommandsRegistry.getCommand`. Elle est choisie
  **parce que** le saut passe par une **interface** (`ICommandService`) — donc il n'existe dans notre
  graphe que si la synthèse de dispatch de la Phase 3 l'a bien pontée.
  Le gate mesure **mécaniquement** que l'agent fait **zéro Read/Grep** (compter les blocs
  `tool_use` dans `claude -p --output-format stream-json`), avec un **contrôle négatif** et une
  règle 2-sur-3 **par dépôt**.

### C. Merger Phases 4+5 sur `main` (après revue de branche complète)

### D. Phase 6 — CLI + daemon + sync (22 tâches)
`docs/plans/2026-07-13-phase6-cli-daemon-sync.md` — **plan écrit, 10 questions arbitrées.**
⚠ **La meilleure trouvaille du plan, à ne pas perdre : la raison d'être du daemon CHANGE en Rust.**
La map dit « les commandes CLI ouvrent la DB directement » — **c'est un fait SQLite, pas portable.**
SQLite-WAL admet plusieurs processus ; **SurrealDB embarqué prend un verrou EXCLUSIF.** Porté
littéralement, `selene status` **ne peut pas tourner pendant qu'un éditeur est attaché**.
→ Décision : **daemon-as-arbiter**. **MAIS le spike de la Task 1 doit RATIFIER la prémisse** en
mesurant ce qu'obtient réellement un second processus. **Si les lectures concurrentes marchent, la
décision est ANNULÉE** et on suit la map (Phase 6 perd ~5 tâches).

### E. Phase 7 — installer (13 tâches)
`docs/plans/2026-07-13-phase7-installer.md` — **plan écrit, 10 questions arbitrées.**
Décisions clés déjà prises : marqueurs `SELENE_*` **+ strip unique du legacy `CODEGRAPH_*`** (ton
choix) ; **tout le JSON écrit chirurgicalement** (la map disait de re-sérialiser — ça ferait échouer
le gate « neighbor preservation » par construction) ; on écrit le **chemin absolu** de
`current_exe()` dans la config MCP, pas le nom nu `selene` (un binaire statique n'est pas garanti
dans le `PATH`, et une config qui pointe vers un exécutable introuvable échoue **silencieusement**).

### F. Phases 8, 9 — langages wave-2, parité complète, polish v1
Roadmap seulement : `docs/plans/2026-07-12-selenecode-roadmap.md`.

---

## 6. Environnement — ce qui a cassé, et comment l'éviter

### ⚠ `target/` avait atteint **149 Go** et rempli le disque à 100 %

Cargo ne fait **jamais** de GC des artefacts périmés ; cette session a rebuild des dizaines de fois
avec des combinaisons de features différentes, et tout s'accumule (111 Go rien que dans
`debug/deps`).

**Conséquence en cascade, très trompeuse :** macOS met son **swap sur le disque** → disque plein →
le swap ne peut plus grandir → la mémoire *semble* épuisée → **`fork()` échoue** (« Device not
configured ») → les agents ne se lancent plus, un build meurt en ENOSPC. **J'ai diagnostiqué ça
comme un problème de mémoire et j'ai failli tuer des processus pour réparer un problème de disque.**

**Ça reviendra.** Guet :
```bash
du -sh target                                  # si > 20 Go, nettoyer
cargo clean                                    # ou: rm -rf target/debug target/doc
df -h /System/Volumes/Data                     # surveiller l'espace libre
```

Aussi : 26 agents morts des Phases 1–3 traînaient encore en mémoire (2,7 Go). Tués. Tes sessions
Claude dans **d'autres dépôts** (signing-api, ntcc, cricut-svg…) ont été **laissées intactes**.

### Recréer `/tmp/ask.sh` (la sonde MCP) s'il a disparu au reboot

```bash
cat > /tmp/ask.sh <<'SH'
#!/bin/bash
q="$1"
printf '%s\n%s\n%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"p","version":"1"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"explore\",\"arguments\":{\"query\":\"$q\"}}}" \
 | ./target/release/selene serve --mcp --path /tmp/dogfood-selene 2>/dev/null | sed -n '2p' \
 | python3 -c "
import sys,json,re
d=json.load(sys.stdin); txt=''.join(c.get('text','') for c in d['result']['content'])
s=re.search(r'Starting from: (.*)', txt)
print('  seeds:', (s.group(1)[:85] if s else '(none)'))
print('  Flow:', '### Flow' in txt, '| steps:', len(re.findall(r'(?m)^\d+\.\s+\`', txt)), '| batch.rs:', 'selene-resolve/src/batch.rs' in txt, '| resolve_one:', 'resolve_one' in txt)
"
SH
chmod +x /tmp/ask.sh
```

---

## 7. Commandes utiles

```bash
# construire et LANCER (release — le debug est 2,4× plus lent)
cargo build --release -p selene
./target/release/selene index /chemin/du/repo
RUST_LOG=info ./target/release/selene index /chemin   # timings par phase (stderr)

# recréer le corpus de dogfood
rm -rf /tmp/dogfood-selene && mkdir -p /tmp/dogfood-selene
cp -R crates docs Cargo.toml /tmp/dogfood-selene/
./target/release/selene index /tmp/dogfood-selene

# les gates auxquels on peut se fier
cargo test -p selene-resolve --test resolution_parity_gate --test dispatch_coverage_gate

# le gate dont il faut se méfier (passe alors que le produit est cassé)
cargo test -p selene-context --test phase4_gate

# la sonde de perf (longue, ignorée par défaut)
cargo test -p selene-context --test perf_phase_probe -- --ignored --nocapture
```

---

## 8. Les invariants — ne jamais les régresser (CLAUDE.md §8.2)

- **Sufficiency / anti-Read.** L'agent répond avec **zéro Read/Grep**. C'est le produit. Tout
  changement se juge là-dessus : *est-ce que ça empêche l'agent d'ouvrir un fichier ?*
- **`isError` est RÉSERVÉ.** Seulement `PathRefusal` + vrai bug. Tout le reste = guidance
  success-shaped. *Une seule `isError` prématurée et l'agent abandonne l'outil.*
- **Le dispatch dynamique doit être bout-en-bout.** Une couverture **partielle est PIRE que rien** —
  un flux à moitié ponté révèle un saut que l'agent va alors lire.
- **Extraction déterministe.** Dérivée de l'AST, jamais résumée par un LLM.
- **Une seule source de guidance agent** : les `server-instructions` du MCP.

## 9. La leçon la plus chère du projet

> **« Un seam qui renvoie "rien trouvé" est indiscernable d'un seam qui marche et n'a rien trouvé. »**

**Quatre "inert seams"** ont été livrés ici avec des tests unitaires verts et **zéro appelant en
production**. Et ce soir, un cinquième d'une autre espèce : **le binaire tournait sur le mauvais
backend de base de données pendant trois phases, parce que personne ne l'avait jamais exécuté.**

⇒ **Un test vert ne prouve rien. Lance le vrai binaire.**
