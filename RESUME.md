# RESUME — reprendre SeleneCode après un redémarrage

> **MISE À JOUR 2026-08-10 — la couche sémantique « beat Graphify » est CONSTRUITE**
> (plan : `docs/plans/2026-08-10-semantic-viz-and-report.md`) : détection de communautés
> Louvain déterministe (`selene-cli/src/analysis.rs`) + bouton **Clusters** dans la viz,
> god-nodes + ponts rares dans le HUD, reçu **token economy** mesuré au pied de chaque
> réponse `explore` (le « 52× » vs lecture brute), et `selene report` → `GRAPH_REPORT.md`
> (hubs, clusters, cycles de modules, modules orphelins, questions suggérées ; purge le
> retire sur son marqueur). La 3D/2,5D est ABANDONNÉE (décision utilisateur : les 4
> features sémantiques suffisent).

> ⚠️ **MISE À JOUR 2026-07-17 — plusieurs sections ci-dessous sont PÉRIMÉES.** L'état courant est
> dans `README.md` (usage, install 1-commande, viz) et `docs/plans/2026-07-16-optimization-roadmap.md`
> (perf : plus rapide que CodeGraph sur les 3 corpus — 0,77×/0,77×/0,96× ; RAM et VS Code = les
> chantiers restants). En particulier : les phases 6+7 SONT construites (CLI ~22 commandes, hooks
> git, daemon, installer 8 agents), Task 19/20 SONT faites, le benchmark vitesse vs TS EST fait,
> et les variables `SURREAL_*` embarquées MARCHENT désormais (patch vendored, `vendor/surrealdb`).


**Écrit le 2026-07-13, mis à jour le 2026-07-15 (`e580537` — Phase 7 installer + Phase 6 daemon
CONSTRUITS et testés contre le vrai binaire).** Ce fichier est la **seule chose à lire** pour
repartir. Il suppose que tu as tout oublié — c'est voulu.

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
| Phase 5 (MCP + binaire) | ✅ **Task 19 faite**, **Task 20 gate construit + lancé** (§5bis), latence gros-dépôt fixée |
| Binaire | ✅ `index` · `serve --mcp` · **`status`** (nouveau) · README réécrit, quick-start vérifiée |
| Perf | ✅ **SeleneCode BAT CodeGraph sur 2/3 dépôts** : TS 0,69× · Rust 0,37× (plus rapide) · django 1,10× (plancher FTS). Voir `docs/benchmarks/2026-07-14-rust-vs-ts-speed.md` §Follow-up 6 |
| Phase 6 (CLI + daemon + sync) | ✅ **CLI 22 cmds · daemon warm-store + proxy · sync incrémental · sync routé par le daemon** — testés contre le vrai binaire (§5ter) |
| Phase 7 (installer) | ✅ **`install`/`uninstall` pour les 5 agents JSON** (claude/cursor/gemini/kiro/antigravity), écriture chirurgicale, chemin absolu (§7) |
| Phases 8, 9 (langages wave-2, parité, v1) | ⬜ roadmap seulement |

**Branche de travail : `feat/phase45-graph-context-mcp`** (PAS mergée).
`main` est à `ba29336` (fin de Phase 3).

**Toute la suite : ~1 100 tests, 0 échec.** Parity 6/6, dispatch 5/5, phase4 7/7.

## 5ter. Phase 6 daemon — le spike Task 1 est TRANCHÉ (2026-07-15) : le daemon EST nécessaire

Le plan disait : *« Task 1's spike must RATIFY the premise. If concurrent reads work, the
daemon-as-arbiter ruling is VOID and Phase 6 shrinks by ~5 tasks. »* **Mesuré : les lectures
concurrentes ne marchent PAS.** Deux process qui tentent de tenir le même store RocksDB embarqué en
même temps : le second **bloque** (verrou exclusif ; `connect_disk_with_lock_retry` boucle jusqu'au
timeout). Donc :
- **On ne peut pas cacher le store ouvert dans `serve`** sans bloquer un `selene status`/`index`
  concurrent. L'ouverture par appel actuelle est CORRECTE.
- **Le daemon-as-arbiter n'est pas annulé — il est confirmé.** C'est lui (un seul process tient le
  store, les autres lui parlent par socket) qui permet à la fois le store chaud (warm-up une seule
  fois) ET l'accès concurrent CLI.

### CONSTRUIT (2026-07-15, `0c28b56`/`e580537`) — testé contre le vrai binaire

`crates/selene-mcp/src/daemon/` (POSIX) + `crates/selene/tests/daemon.rs` (4 tests, vrai binaire) :

- **Élection** : pidfile-comme-verrou par hard-link atomique (`O_EXCL` en fallback), record JSON
  `{pid,version,socket,started_at}`, nettoyage compare-and-delete d'un cadavre mort (`lock.rs`).
- **Trois modes de `serve --mcp`** (`serve.rs`) : *direct* (in-process, `SELENE_NO_DAEMON=1`),
  *daemon* (`SELENE_DAEMON_INTERNAL=1` : élit, bind, store chaud, refcount + idle-timeout, SIGTERM),
  *proxy* (le cas courant : connecte le daemon vivant même-version et `copy_bidirectional` ; sinon
  spawn un daemon détaché puis connecte ; sinon fallback direct).
- **Store chaud** : `handlers::open` consulte un cache process-global par-racine (tokio `OnceCell`,
  aucun lock tenu à travers un `.await`). AVANT : chaque appel MCP rouvrait RocksDB. **Mesuré** :
  requête `explore` à froid (CLI one-shot) ~800 ms *à chaque appel* ; en daemon chaud, les appels
  répétés tombent à ~180–450 ms (**~2–4×**).
- **Régression corrigée** : le daemon tient le verrou exclusif en continu → `selene sync` échouait
  (`LOCK: Resource temporarily unavailable`). Fix : `sync` route une frame de contrôle
  (`{"selene_control":"sync"}`) au daemon, qui ré-indexe sur SON store chaud → pas de bagarre de
  verrou, et le symbole ajouté est **immédiatement** interrogeable (mesuré). `index` complet pendant
  un daemon vivant → guidage propre (`kill <pid>` ou `selene sync`), plus d'erreur cryptique.
- **FileWatcher** (`watch.rs`, `1a85978`) : le daemon auto-sync sur changement de fichier (`notify`
  récursif, debounce 2 s, ré-index sur le store chaud). **Piège = la boucle de rétroaction** : un
  sync ÉCRIT dans `.selene/`, donc ces events relanceraient un sync à l'infini. Deux gardes,
  mesurées : `relevant()` jette tout event sous `.selene/`/`.git/` (un sync ne peut jamais *démarrer*
  un burst), et le debounce est à *deadline* (seul un event pertinent l'étend). Vérifié : 1 seul
  auto-sync par changement, CPU retombe à 0 % (une version antérieure à reset-sur-tout-event
  tournait en boucle). Opt-out `SELENE_NO_WATCH=1`.
- **`selene daemon`** liste les daemons vivants (registre `~/.selene/daemons/`, records morts élagués).
- **Cleanup orphelin** : déjà correct sans watchdog — un proxy mort ferme sa socket → la session du
  daemon voit EOF → refcount tombe → idle-reap. Pas de gap de correction.

### Long-tail Phase 6/7 — FAIT (2026-07-15)

- **git-hooks** (`selene-sync/hooks.rs`) : `selene init` installe post-commit/merge/checkout qui
  lancent `selene sync` en fond ; `uninit` les retire ; `--no-hooks` opt-out. Bloc marqué, préserve
  le hook existant de l'utilisateur, idempotent, chemin absolu. Testé en vrai dépôt git.
- **worktree-mismatch** (`selene-sync/worktree.rs`) : `selene status` avertit si le worktree git
  courant diffère de celui indexé. Conservateur (le moindre doute → pas d'avertissement). PAS sur le
  chemin chaud MCP (git par requête régresserait la latence). Testé avec un vrai `git worktree add`.
- **watchdog liveness** (`selene-mcp/daemon/watchdog.rs`) : un thread OS abort le daemon si le
  runtime tokio se fige (heartbeat via task tokio, déféré sur progrès disque, cap 10×, opt-out
  `SELENE_NO_WATCHDOG`). Prouvé : un daemon sain survit 3× le timeout.
- **Fichiers d'instructions installer** : chaque agent qui les lit reçoit un bloc `## SeleneCode`
  (utilise `explore`, ne lis pas les fichiers) — bloc marqué dans CLAUDE.md/AGENTS.md/GEMINI.md, ou
  fichier possédé (cursor `.mdc`, kiro steering). Préserve la prose de l'utilisateur.

**Reste (niche, non bloquant)** : `telemetry`/`upgrade` sont **Phase 8** (pas 6/7). Sweep %APPDATA%
opencode + migration antigravity : déférés (documentés). Le port des ~97 tests TS à la ligne : non
fait (consigne « features pas mimic »).

## 7. Phase 7 installer — COMPLET, 8 agents / 4 formats (2026-07-15, `d953ee6`)

`crates/selene-installer/` + `selene install`/`uninstall`/`--print-config`. Les **8 agents** dans
leur **propre format**, chacun avec préservation **octet-pour-octet** des voisins :
- `format.rs` — 3 writers préservant le format, chacun avec un garde `round_trips` (parse → ré-émet
  → compare les octets ; **refuse** de toucher un fichier qui ne round-trip pas plutôt que le
  reformater) : **json** (jsonc-parser CST — JSON *et* JSONC, commentaires + ordre des clés + virgules
  traînantes survivent ; remplace l'ancien writer serde_json qui reformatait), **toml** (toml_edit —
  codex `[mcp_servers.selene]`), **yaml** (line-based — hermes `mcp_servers.selene` + l'entrée liste
  `platform_toolsets.cli: - mcp-selene`).
- `targets.rs` — registre des 8 cibles (ordre gelé claude, cursor, codex, opencode, hermes, gemini,
  antigravity, kiro), injection `Ctx{home,cwd,env}` (pas de globals ; les tests fakent HOME), chemins
  + formes d'entrée par agent (opencode conteneur `mcp` avec `{type,command[],enabled}` ; codex /
  hermes / antigravity global-only), et `--target auto|all|none|<csv>`. Les **2 seuls** cas exit-1 :
  target inconnu, location invalide. Tout le reste success-shaped (created/updated/unchanged/removed/
  not-found/kept/unsupported).

Vérifié contre le **vrai binaire** sur les 4 formats (commentaire codex/opencode + voisins préservés,
hermes toolset, ré-install `unchanged`, uninstall ne retire que selene). 9 tests format + 6 tests
cibles (Ctx sur tempdirs) + 2 bout-en-bout (exit codes).

**Déféré** (documenté dans `targets.rs`) : les fichiers secondaires par-agent (blocs d'instructions
AGENTS.md/CLAUDE.md/GEMINI.md, cleanup .cursor/rules + kiro steering, sweep %APPDATA% opencode,
migration antigravity). Le port des ~97 tests de contrat TS à la ligne n'est pas fait (consigne
« features pas mimic »). L'enregistrement du serveur MCP — la feature — l'est.

## 5bis. Task 20 — le gate du jalon : ce qu'il a RÉVÉLÉ (2026-07-15)

Le gate est construit (`crates/selene-mcp/tests/dogfood_gate.rs`, `#[ignore]`) + `scripts/dogfood.sh`
(Half B). Il pilote le **vrai binaire** en MCP stdio, sur 3 dépôts réels. Résultat mesuré :

| repo | nœuds | latence | réponse |
|---|---:|---:|---|
| SeleneCode | 5k | 1,2 s | ✅ |
| codegraph | 5k | 1,4 s | ✅ |
| **VS Code** | **349k** | **35,6 → 6,5 s** (fixé) | ❌ **fausse** (vocabulaire) |

**Le produit marche sur petits/moyens dépôts, pas encore sur les gros.** Deux problèmes indépendants,
tous deux mesurés (`docs/benchmarks/2026-07-phase5-dogfood.md`) :

1. **Latence — ✅ FIXÉE.** `explore` faisait **35,6 s** sur VS Code : quatre passes de pertinence
   non-indexées, O(taille du graphe). La seule passe rapide était l'index FTS. Fix : au-dessus de
   `LARGE_REPO_FILES = 3000`, on saute pass0 / pass6-7 (`CONTAINS`) / pass12 / dominant_file et on
   s'appuie sur FTS → **6,5 s** (dont ~2,4 s de requête ; le reste = startup `serve`, nul avec le
   daemon Phase 6). **Petits dépôts byte-identiques** (sha vérifiés).
2. **Vocabulaire — ⬜ RESTE, et la voie est tracée.** La question dit « key**press** », le code dit
   « key**binding** ». **Tenté : l'analyseur `edgengram(3,15)` de SurrealDB** (le mécanisme natif de
   partial-match, remplaçant la table SQLite `name_segment_vocab` du build TS). **Mesuré sur VS Code
   (re-index 11 min) : ça ne suffit pas** — « keypress » et « keybinding » ne partagent que « key »
   (3 car.), noyé sous les matches de « command »/« executed » ; et les vrais symboles ne contiennent
   NI « command » NI « executed » — ils ne sont atteignables que par le SENS. Reverté (byte-identique
   petits dépôts, zéro bénéfice sur la cible ; garder un changement de schéma inefficace = le piège
   du seam inerte). **La vraie réponse SurrealDB-native : la recherche vectorielle** (HNSW + cosine
   KNN) — embedder chaque symbole + la requête, KNN sur la similarité. C'est le superpouvoir
   graphe+vecteur que le SQLite du build TS ne pouvait pas offrir, et le prochain gros levier
   « max out SurrealDB ». Scopé, pas construit (il faut un pipeline d'embeddings). Détail :
   `docs/benchmarks/2026-07-phase5-dogfood.md`.

---

## 1 bis. « Est-ce que c'est prêt ? Est-ce que c'est CodeGraph en Rust ? »

**Non.** Ça **marche**, ce n'est pas le **produit**. Distinction qui coûte cher si on la rate.

### Ce qui existe vraiment

Un binaire unique, SurrealDB embarqué (RocksDB), qui **indexe** et **sert du MCP**. Vérifié en vrai,
pas déduit : `selene index` + `selene serve --mcp` + `explore` répond (§2).
**11–12 langages** (c, cpp, go, java, js, kotlin, php, python, ruby, rust, ts).

### Ce qui EXISTE maintenant (mis à jour 2026-07-15)

- **Binaire** : `index` · `serve --mcp` · **`status`**. README réécrit avec quick-start vérifiée.
- **Task 19 (discipline `isError` + caps d'entrée)** : ✅ faite, testée via le vrai serveur.
- **Task 20 (gate du jalon)** : ✅ construit (`dogfood_gate.rs` + `dogfood.sh`) et **lancé** — voir
  §5bis. Il a prouvé que le produit marche sur petits/moyens dépôts et révélé le gap gros-dépôt
  (latence fixée, vocabulaire à faire via vector search).

### Ce qui n'existe TOUJOURS PAS — stubs de 3 lignes

| crate | conséquence concrète |
|---|---|
| `selene-sync` | **Réindex À LA MAIN quand le code change.** Pas de watch, pas d'incrémental branché. |
| `selene-installer` | Pas de `selene install`. La config MCP s'écrit **à la main** (documenté au README). |
| `selene-cli` | Le binaire `selene` porte `index`/`serve`/`status` en dur ; les 22 sous-commandes du plan Phase 6 (dont `sync`, le daemon) ne sont pas construites. |

*(`selene-resolve` (17 k lignes) et `selene-graph` sont **implémentés** — « stub » traîne dans leur
doc de module et trompe un `grep`.)*

### Ce qui reste pour « ça tient sa promesse à TOUTE échelle »

1. **Vector search** — le gate VS Code échoue sur le vocabulaire (§5bis). La voie native est tracée.
2. **`explore` prouvé surtout sur TS/Rust.** Le gate Phase 4 tourne sur 2 projets TS ; Python/Django,
   Go, Java/Spring : le code existe, peu prouvé (le dogfood gate ajoute django/VS Code côté latence).

### Utilisable dès maintenant

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

## 3. Perf — de 10,7× derrière TS à **1,4–1,9×** (2026-07-14, une journée)

| corpus | ce matin | **maintenant** | codegraph TS | écart |
|---|---:|---:|---:|---|
| codegraph/src (162 f.) | 18,4 s | **3,6 s** | 2,4 s | **TS 1,5×** |
| SeleneCode (328 f.) | 22,8 s | **4,0 s** | 2,8 s | **TS 1,4×** |
| django (931 f.) | 61,1 s | **10,9 s** | 5,7 s | **TS 1,9×** |

Déterministe. Couverture intacte (+4 références que l'ancien code **jetait**). Tous les gates verts.
`explore` répond toujours 3/3.

**L'arc de la journée** (chaque ligne = un commit, mesuré, graphe identique) :
```
matin (séquentiel, file-par-disque, 32k lookups bloquants)   django 61,1s   TS 10,7x
+ commit groupé (931 allers-retours -> 4)                            51,2s
+ bug du delete par clé corrigé (il JETAIT des refs) + réécriture    40,0s
+ fetch unique (START offset = O(n²))                                36,2s
+ index morts retirés                                                33,5s
+ feature `allocator` de SurrealDB (mimalloc)                        28,9s
+ index de nœuds en mémoire (32 524 lookups bloquants -> 48)         23,7s
+ file de refs gardée en mémoire (plus d'aller-retour disque)        16,4s
+ écritures CONCURRENTES + FTS en parallèle du resolve               10,9s   TS 1,9x
```

### ⚠ LA LEÇON, en une phrase — lis-la avant d'optimiser quoi que ce soit

> **La base de données n'a JAMAIS été le problème.** Elle avale tout le graphe django en < 1,5 s, et
> son propre benchmark la donne à **3,5× Redis** en CRUD. **Elle n'était pas lente. On était bruyants.**

Les deux trouvailles qui ont tout fait sont **la même**, à un étage d'écart :

1. **Le « ladder » n'était pas CPU-bound. C'était 32 524 lookups bloquants** (69 % de son temps) —
   dont **14 674 `get_node`**, un *point lookup par clé primaire*. On reconstruisait, requête par
   requête, une table de 19 061 lignes qui tient dans **8 Mo de RAM**.
   ⚠ **Le cache LRU n'y pouvait rien, et sa taille n'était pas la solution** : le passer de 5 000 à
   200 000 n'a retiré que **8 %** des lectures. Ce sont des **miss froids** — 12 279 noms distincts,
   chacun cherché une fois. *Un cache paresseux paie un aller-retour par clé distincte, pour
   toujours.* **Un seul scan (127 ms) les a tous remplacés.** Ladder : 6 839 → **1 889 ms**.
2. **La file de références était un tampon de handoff qui transitait par le DISQUE** — 52 358 lignes
   écrites, relues, effacées, **entre deux phases du même processus**. Passées en mémoire.
   Persist : 7,3 → **3,4 s**. (Et ça supprime un **bug de déterminisme** : l'ordre de résolution
   dépendait d'un **record-id généré par SurrealDB**.)

**La résolution n'est pas un problème de graphe. C'est une table de symboles** — *« quels nœuds
s'appellent `foo` ? »*. Le bon endroit pour un dictionnaire qu'on interroge 32 524 fois, c'est la
**RAM**. Le moteur graphe sert à **stocker** le résultat et à le **traverser** (`explore`, callers,
impact) — **jamais** à la phase de construction.

### ⛔ Mesuré et REJETÉ — ne les retente pas

| idée | verdict |
|---|---|
| `SURREAL_DATASTORE_SYNC` / fsync | **inerte** — le SDK n'appelle jamais `Builder::with_config()` : **toutes** les variables `SURREAL_*` sont mortes en embarqué. Valait 12 % de toute façon. |
| `WHERE [a,b,c] IN [...]` (**recommandé par nos propres docs**) | **RÉGRESSION 64×** — une expression sur tableau ne peut pas utiliser l'index composite |
| réglage de `CHUNK` (100/250/500/1000) | **aucun effet** — leurs chiffres mesurent des allers-retours **réseau** ; on est en process |
| différer les index de `node` au bulk load | **nul** — le `DEFINE INDEX` en masse coûte **plus** que les 19 061 maintenances qu'il évite |
| `lto = true` (fat) | **nul**, +10 min de compilation |
| tokio pile 10 MiB, multi_thread explicite | **nul** |
| ⛔ `panic = 'abort'` (**recommandé par SurrealDB**) | **JAMAIS** — `selene-resolve` enveloppe les détecteurs de framework et les synthétiseurs dans `catch_unwind` : un résolveur qui panique est une **erreur collectée**, pas un index mort |
| feature `allocator` de SurrealDB (mimalloc) | ✅ **31,5 → 28,9 s** — pas activée par défaut, et un `default-features = false` ne l'a jamais |

### Ce qui reste sur les 10,9 s de django, et le SEUL levier qui vaut encore le coup

```
écriture bulk  ~4,5 s (nœuds 0,8 · arêtes 0,6 · sérialisation + fichiers)
synthèse        2,8 s   ← mono-thread                persist  ~2 s
ladder          1,9 s                                FTS      0 s (recouvert)
```
**Plus aucun poste ne domine, et le FTS est gratuit** (recouvert par le resolve). Pour passer
**DEVANT** TS (5,7 s), il faut soit réduire le **volume écrit** — la table `node` est `SCHEMAFULL`,
~25 champs, 9 index — soit revoir la **synthèse** (2,8 s, mono-thread). **C'est un changement de
modèle de données, pas du réglage** — le seul levier restant qui vaille plus qu'une fraction de
seconde. ⚠ **À ne PAS entreprendre sans arbitrage** : ça touche la forme du graphe.

Détail complet : `docs/benchmarks/2026-07-14-rust-vs-ts-speed.md`.

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
