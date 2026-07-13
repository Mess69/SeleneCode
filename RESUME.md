# RESUME — reprendre SeleneCode après un redémarrage

**Écrit le 2026-07-13.** Ce fichier est la **seule chose à lire** pour repartir. Il suppose que
tu as tout oublié — c'est voulu.

---

## 0. La commande à donner à Claude au redémarrage

> « Lis `RESUME.md` et reprends. »

Tout le reste de ce fichier est là pour que cette phrase suffise.

---

## 1. Où on en est, en une phrase

**Le binaire tourne, indexe un vrai dépôt, sert du MCP — mais `explore` ne répond pas encore
correctement aux questions de flux.** C'est le seul vrai blocage. Tout le reste (perf, plans,
infra) est réglé ou planifié.

| | état |
|---|---|
| Phases 1, 2, 3 (db, extract, resolve) | ✅ **mergées sur `main`** (`ba29336`), gates verts |
| Phase 4 (graph + context) | ✅ code fini, gate vert — **mais le gate ment, voir §4** |
| Phase 5 (MCP + binaire) | 🟡 écrit et commité, **jamais testé/finalisé** (Tasks 19–20 restent) |
| Perf | ✅ **6× + 2,5×** — voir §3 |
| Phases 6, 7 (CLI/daemon, installer) | ✅ **plans écrits et arbitrés**, 35 tâches prêtes à exécuter |
| Phases 8, 9 (langages wave-2, parité, v1) | ⬜ roadmap seulement |

**Branche de travail : `feat/phase45-graph-context-mcp`** (PAS mergée).
`main` est à `ba29336` (fin de Phase 3).

---

## 2. LE BLOCAGE — à attaquer en premier

### Le produit tourne et ne répond pas

J'ai lancé le vrai binaire, sur du vrai MCP, contre un vrai dépôt (SeleneCode : 328 fichiers,
5 035 nœuds, 17 216 arêtes, 11 875 références résolues — **le graphe est bon, ce n'est PAS un bug
d'extraction ni de résolution**).

Question posée (exactement celle du gate final Task 20) :

> **« how does an unresolved reference become a graph edge »**

Réponse : **9 737 caractères de contexte confiant, bien formaté, et FAUX.**
- symboles de départ : `graph_outcome`, `match_reference`, `unresolved_content`
  — `graph_outcome` est un **helper d'erreur MCP**. Il sort parce que la requête contient les
  mots « graph » et « unresolved ».
- **0 des 4** symboles requis (`resolve_and_persist_batched`, `resolve_one`, `create_edges`,
  `insert_edges`)
- **0 des 2** fichiers requis (`crates/selene-resolve/src/batch.rs`, `resolver.rs`)
- **aucune section Flow**

Un agent recevrait `handlers.rs`, n'apprendrait rien, et ouvrirait `batch.rs` — **exactement le
Read que ce produit existe pour empêcher** (invariant « sufficiency / anti-Read », CLAUDE.md).

### Les 3 sondes qui localisent le bug

Reproduire : `/tmp/ask.sh "<requête>"` (script recréé en §6 s'il a disparu ; il pilote le vrai
binaire release en MCP contre `/tmp/dogfood-selene`).

| requête | symboles de départ | Flow ? | trouve batch.rs ? |
|---|---|---|---|
| « how does an unresolved reference become a graph edge » | graph_outcome, match_reference, unresolved_content | ❌ | ❌ |
| « resolve_and_persist_batched » (nom exact) | resolve_project, index_and_drive, **resolve_and_persist_batched** | ❌ | ✅ |
| « how are edges created during resolution » | insert_edges, insert_edges, insert_edges | ❌ | ❌ |

Ce que ça prouve :
1. **Le graphe a la donnée** — nommer le symbole le trouve. Ne cherche pas un bug de store.
2. **La pertinence fait du matching lexical** mot-de-requête ↔ nom-de-symbole. Elle n'a aucune
   notion qu'une question de *flux* veut une *chaîne connectée*.
3. **Ligne 3 : le même symbole 3 fois** → bug de dédup des seeds.
4. **La section Flow ne s'affiche JAMAIS** — même ligne 2, où le bon symbole était pourtant là.

### Diagnostic (à vérifier, pas à croire sur parole)

`ContextBuilder::render_flow_section` (`crates/selene-context/src/builder.rs:111-133`) est
**correctement câblé** — il EST appelé en prod, et il refuse honnêtement d'inventer une chaîne
qu'il ne peut pas prouver (*« A fabricated spine is worse than none »*). **Garde cet instinct.**

Il échoue à cause de ce qu'on lui **donne à manger** :
- chemin 1 : besoin de `extract_search_terms(query).len() >= 2` — pour une question en prose, les
  « termes » sont des mots anglais, pas des symboles → aucune chaîne.
- chemin 2 : repli sur `ctx.roots` — mais les roots sont les symboles lexicalement proches
  ci-dessus, **non connectés entre eux** → `build_flow_from_named_symbols` renvoie `None`, à juste
  titre.

⇒ **La cause racine est la sélection des seeds (relevance). Flow est la victime.** Si les roots
étaient `resolve_and_persist_batched → resolve_one → create_edges → insert_edges`, la chaîne
existe et Flow s'affiche tout seul.

**Piste la plus probable** (à confirmer contre `docs/reference/from-codegraph/maps/mcp-context.md`,
qui est l'autorité — le build TS a résolu ce problème) : on score la **similarité de nom** mais pas
la **connectivité dans le graphe**. Un ensemble de seeds qui forment une **chaîne d'appels
connectée** doit écraser trois correspondances lexicales isolées.

### ⚠ Le piège : le gate de la Phase 4 PASSE pendant que c'est cassé

`crates/selene-context/tests/phase4_gate.rs` est vert (7/7). Il passe parce qu'il tourne sur de
**petites fixtures plantées** (2 projets, tous deux de forme TypeScript). Sur un vrai dépôt de
328 fichiers, la pertinence s'effondre.

**Son succès est la chose dont il faut se méfier, pas celle à laquelle se fier.** Le reviewer
`rev13` l'avait dit (« le corpus du gate fait 2 projets, pas les ≥6 spécifiés ») — c'est maintenant
prouvé dans le produit, pas argumenté dans une revue. **Il avait raison.**

### État EXACT à la pause — MESURÉ contre le vrai binaire (commit `e879fba`)

Un agent (`relevance`) a produit ~750 lignes, commitées en WIP (`faf9b54` + `e879fba`). **Ça
compile, et je l'ai TESTÉ** — pas une promesse, des chiffres. Reproduis-les en une commande :
`./scripts/ask.sh "<requête>"` (script versionné dans le repo).

| requête | AVANT (début de session) | **MAINTENANT (`e879fba`)** |
|---|---|---|
| **Q1 — celle du gate** | `graph_outcome`(!), match_reference, unresolved_content · Flow ❌ · symboles requis 0/4 | seeds `UnresolvedReference`, `unresolved`, `GraphStore` · **Flow ✅ (4 étapes)** · **symboles requis 0/4 ❌** |
| Q2 — nom exact | resolve_project, index_and_drive, resolve_and_persist_batched · Flow ❌ | **`resolve_and_persist_batched` en 1er** ✅ · **Flow ✅ (4 étapes)** · batch.rs ✅ |
| Q3 — prose | **insert_edges ×3** (bug dédup) · Flow ❌ · batch.rs ❌ | dédup **corrigé** ✅ · batch.rs ✅ · resolve_one ✅ · Flow ❌ |

**Acquis (réels) :** bug de déduplication **corrigé** ; seeds nettement meilleurs ; **la section
Flow s'affiche maintenant** (elle ne s'affichait sur *aucune* requête au début).

### ✅ Question tranchée : il n'y a PAS de second bug dans `build_flow_from_named_symbols`

J'avais prévu une expérience pour le déterminer. **Elle n'est plus nécessaire** : Flow s'affiche
parfaitement (Q1, Q2) dès qu'on lui donne des seeds connectés. **Le flow builder marche.**

### ⚠ LE BUG RESTANT, isolé : la sélection des seeds sur une question en prose

Sur **Q1 — la question exacte du gate** — `explore` renvoie encore **0 des 4 symboles requis** et
**0 des 2 fichiers requis**. La cause est maintenant nette :

> **Les seeds sont des TYPES, pas des FONCTIONS.** `UnresolvedReference`, `GraphStore` sont des
> types. **On n'appelle pas un type** — donc aucune chaîne d'appels ne peut relier ces seeds au
> vrai flux (`resolve_and_persist_batched → resolve_one → create_edges → insert_edges`). Le Flow
> qui s'affiche est un flux **plausible mais hors-sujet**, ce qui est *dangereux* : c'est un
> contexte confiant et faux, exactement ce que l'invariant anti-Read interdit.

**Piste :** le scoring doit préférer des seeds qui (a) sont des **fonctions/méthodes** quand la
question est une question de *flux* (« how does X become Y »), et (b) forment une **chaîne
connectée** dans le graphe — pas trois symboles pertinents pris isolément. L'autorité est
`docs/reference/from-codegraph/maps/mcp-context.md` : **le build TS a résolu ce problème** —
regarder ce qu'il fait qu'on ne fait pas.

**Critère de réussite, non négociable :** `./scripts/ask.sh "how does an unresolved reference
become a graph edge"` doit afficher `batch.rs: True | resolve_one: True |
resolve_and_persist_batched: True`. **Pas un test unitaire** — `phase4_gate.rs` est vert (7/7)
pendant que tout ceci est cassé.

---

## 3. Perf — RÉGLÉ (garder le contexte, ne pas refaire)

`selene index` sur codegraph (162 fichiers) : **52,4 s → 20,6 s**. Sur SeleneCode : **8 m 52 s →
1 m 29 s**. Sortie **identique** à chaque fois (aucune arête perdue), gates Phase 3 verts.

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
| `selene-context/tests/phase4_gate.rs` | ✅ 7/7 | ⚠ **MENT** — voir §2. Corpus = 2 projets TS plantés. |
| Task 20 (le gate du jalon) | ⬜ **pas écrit** | C'est LUI qui prouve le produit. §5. |

**À faire aussi (findings du reviewer `rev13`, non traités) :**
- élargir le corpus du gate Phase 4 à **≥6 projets** couvrant TS/React, **Python/Django, Go, Rust,
  Java/Spring** + un synth. Aujourd'hui : 2, tous TS. Donc chaque assertion est **non prouvée pour
  4 des 5 familles de langages** qu'on livre.
- `get_dominant_file()` n'a pas de primitive store → la passe 4 du scoring est un **no-op silencieux
  qui a l'air de marcher**. L'implémenter ou l'enregistrer comme déviation explicite.
- snapshots `insta` (Task 13 half 2) + table de budgets dans `docs/benchmarks/` : jamais faits.

---

## 5. Ce qui reste — dans l'ordre

### A. Débloquer `explore` (§2) ← **PRIORITÉ ABSOLUE**
Sans ça, rien d'autre n'a de valeur.

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
