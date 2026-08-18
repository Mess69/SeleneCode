# PRD — Plateforme graphe : combler les écarts (export, analytics agent, temporel, requêtes, mémoire, confiance, documents)

**Date :** 2026-08-18
**Statut :** Cible d'architecture (le « quoi », pas l'ordre d'exécution) — **avec ordre
d'implémentation recommandé en §11** (demande explicite : implémentation à suivre).
**Auteur :** analyse d'écarts vs Semantica/Graphify (2026-08-18) + recherche crates Rust
(workflow `rust-crate-research`, 4 chercheurs, sources docs.rs/crates.io/lib.rs/GitHub).
**Portée :** sept features (F1–F7) qui transforment le graphe d'un moteur de requêtes
fermé en une plateforme : interopérabilité, analytics pour l'agent, axe temporel,
mémoire, requêtes libres, confiance graduée, documents. **F1 (ingestion de documents)
est régie par son propre PRD** — `docs/specs/2026-08-14-document-ingestion-design.md` —
intégré ici par référence, avec un amendement de recherche (§9).

> Ce document décrit l'**état cible**. Les plans d'exécution détaillés (bite-sized TDD)
> vivent dans `docs/plans/`. Chaque choix de crate ci-dessous est justifié par une
> recherche datée du 2026-08-18 avec versions et sources ; chaque décision est mesurée
> contre les trois contraintes non négociables du produit : **binaire statique unique,
> zéro dépendance C, déterminisme**.

---

## 1. Contexte & motivation

### 1.1 D'où viennent ces features

L'analyse d'écarts du 2026-08-18 contre **Semantica** (infra KG généraliste, 8,8k ★,
Python — ne parse PAS le code) et **Graphify** (code+docs, LLM-dépendant) a identifié
les capacités qui transfèrent à la mission SeleneCode : le graphe existe et il est
bon ; il est **enfermé**. Pas d'export, pas d'analytics accessibles à l'agent, pas
d'axe temporel alors que git le donne, pas de mémoire entre sessions, pas de requêtes
libres pour l'humain, pas de confiance graduée sur les arêtes inférées.

### 1.2 Le levier Rust

Chaque feature ici se construit **sans dépendance C et sans service** : gix est du git
pur Rust, Brandes se code en ~120 lignes déterministes, GraphML est du XML qu'on écrit
soi-même, le PDF a une voie pure-Rust (avec isolation). La stack Rust n'est pas une
contrainte — c'est ce qui permet de livrer ce que Semantica fait en Python+Neo4j+FAISS
dans **un seul binaire local**.

---

## 2. Inventaire des features

| # | feature | valeur | coût | nouvelles deps |
|---|---|---|---|---|
| F2 | `selene export` (json/jsonl/graphml/dot) | interop (Gephi/yEd/Neo4j/scripts) — Semantica et Graphify l'ont | S | quick-xml |
| F3 | analytics agent (MCP) + betweenness | l'agent obtient communautés/hubs/cycles/centralité sans Read — notre propre invariant l'exige | M | aucune |
| F1-A | documents vague A (md/txt/rst) | la rationale dans le graphe (PRD 14/08) | M | pulldown-cmark |
| F6 | `selene query --raw` (SurrealQL lecture seule) | puissance humaine sans toucher la surface agent | S | aucune |
| F4 | `selene diff <rev>` (graphe entre révisions) | l'axe temporel — le différenciateur que personne n'a pour le code | L | gix |
| F5 | mémoire de session (journal d'explorations) | positionnement « memory layer » | S | aucune |
| F7 | confiance sur les arêtes synthétisées | l'agent pondère les sauts inférés | S | aucune |
| F1-B | documents vague B (pdf/docx) | parité Graphify sans extra ni LLM | M | zip, pdf-extract, lopdf |

---

## 3. F2 — `selene export`

### 3.1 Décision : écrire les formats à la main, sur quick-xml

| critère | petgraph-graphml 5.0 | crate gexf 0.1.1 | **maison (quick-xml 0.41)** |
|---|---|---|---|
| attributs typés (line:int, kind:string…) | non — « only string attribute types » | `<attributes>` non implémenté (Gephi typé impossible) | oui — `<key attr.type>` complet |
| conversion du graphe requise | oui (adapter petgraph + xml-rs) | oui | non (itère nos Node/Edge) |
| dépendances | petgraph + xml-rs | quick-xml 0.36 figé | quick-xml (déjà décidé pour F1-B docx) |
| **Verdict** | | | **maison** |

DOT : maison aussi (~50 lignes) — `petgraph::dot` insère les attributs custom **sans
échappement** (vérifié dans sa source) et impose des ids numériques. GEXF : **non** en
v1 — GraphML couvre déjà Gephi ET yEd (c'est leur format d'échange documenté).

### 3.2 La forme

`selene export [--format json|jsonl|graphml|dot] [--out FILE] [-p DIR]`
- **json** (défaut) : `{nodes, edges, meta{version, extraction_version, root, counts}}`,
  **tri canonique** (nodes par id, edges par (source,target,kind)) — le dump est aussi
  l'outil des gates G1/G2 du PRD documents et du diff F4 : une seule sérialisation
  canonique, trois usages.
- **jsonl** : une ligne `{"type":"node"|"edge",…}` — streaming/jq-friendly.
- **graphml** : `<key>` typées : kind, name, file (string), line (int), provenance,
  language ; arêtes : kind, provenance. Namespace standard graphdrawing.org.
- **dot** : labels échappés (`"` `\` `\n`), couleur par kind optionnelle plus tard.
- Ouverture du store : même chorégraphie que `viz`/`report` (`query_root_direct`).

**Gate F2** : export→ré-export byte-identique ; GraphML validé par re-parse quick-xml ;
un nom de symbole contenant `"` `<` `&` et un retour ligne survit aux trois formats
(test d'injection, même esprit que le `</script>` de la viz).

---

## 4. F3 — analytics pour l'agent + betweenness

### 4.1 Décision : Brandes fait maison, déterministe — pas rustworkx-core

| critère | rustworkx-core 0.18.1 | **maison (analysis.rs)** |
|---|---|---|
| betweenness dispo | oui (+9 autres centralités) | ~120 lignes (Brandes 2001) |
| **déterminisme en parallèle** | **NON** — vérifié dans sa source : accumulation float sous RwLock en ordre d'arrivée des threads | oui — chunks de sources figés, réduction séquentielle en ordre fixe |
| déterministe en séquentiel | oui (`parallel_threshold = usize::MAX`) — mais on perd le parallélisme, la seule raison de prendre le crate | — |
| deps | petgraph+ndarray+rand+rayon-cond+… | zéro |
| **Verdict** | | **maison** (revisiter le crate si Katz/eigenvector deviennent nécessaires) |

Échelle : Brandes exact = O(V·E) — exact jusqu'à ~50k nœuds ; au-delà, **échantillonnage
de pivots Brandes-Pich avec graine fixe** (déterministe par construction, sources triées
par id) et le résultat est marqué `approx: true`.

### 4.2 Où vit le code — déplacement d'`analysis.rs`

`selene-cli/src/analysis.rs` (Louvain + Tarjan, construits le 10/08) **déménage dans
`selene-graph::analysis`** : le MCP doit y accéder et sentrux l'autorise (mcp ordre 30 →
graph ordre 60 ; cli 10 → graph 60). Cohérence doctrinale : SurrealQL-max couvre la
*traversée à la requête* ; l'analyse plein-graphe en RAM est la même espèce que la table
de symboles du résolveur (précédent argumenté dans le module doc d'analysis.rs).
`selene-cli` ré-exporte pour la viz/report — zéro changement de comportement (gate : les
21 tests d'analysis passent inchangés après déplacement).

### 4.3 La surface MCP : un outil `graph_insights`

Un seul outil (pas quatre) : `graph_insights {scope?: "overview"|"symbol:<name>"|"module:<path>"}` →
- overview : communautés nommées par hub (le travail du 12/08), top betweenness (les
  vrais goulots — le degré favorise la plomberie, l'intermédiarité trouve les ponts),
  cycles de modules (imports), ponts rares, modules orphelins.
- symbol : centralité du symbole, sa communauté, ses ponts.
Success-shaped toujours (« pas indexé » = guidance), budgets ≤ `MAX_OUTPUT_LENGTH`,
sortie stable (tri déterministe) — et `server-instructions` (source unique) apprend à
l'agent quand l'appeler. Le `report` bascule ses god-nodes sur la betweenness.

**Gate F3** : même graphe ⇒ mêmes scores aux 6 décimales sur 3 runs (le gate qui aurait
disqualifié rustworkx-core) ; l'outil répond sur le dogfood avec les mêmes clusters que
la viz (une seule vérité) ; zéro `isError`.

---

## 5. F4 — `selene diff <rev>` : l'axe temporel

### 5.1 Décision : gix, épinglé exact

| critère | git2 0.21 | **gix 0.86.0** |
|---|---|---|
| pureté Rust / binaire statique | **non** — libgit2-sys 0.18.4 = C vendored + toolchain C | oui — zlib-rs, SHA-1 propre, « just Rust » |
| API lecture-à-révision sans checkout | oui | oui — `rev_parse_single → find_commit → find_tree → traverse → find_blob` |
| maturité | 1.0-stable, rust-lang | pré-1.0, 49 breaking releases — **pin exact `=0.86.0`**, confiné dans un module |
| **Verdict** | | **gix**, `default-features = false` + features minimales (revision + objets) |

### 5.2 La mécanique — réutiliser tout ce qui existe

1. gix lit l'arbre à `<rev>` (aucun checkout, aucun fichier écrit) → corpus en mémoire
   (chemins + contenus, filtrés par `is_source_file` — les mêmes règles que le scan).
2. Le **pipeline existant** extract→resolve tourne sur ce corpus vers un **store
   éphémère `kv-mem`** (déjà une feature par défaut de selene-db — zéro DDL nouveau).
3. Les deux graphes (rev et courant) sortent par la **sérialisation canonique de F2**,
   et le diff est une comparaison de multisets triés : nœuds ajoutés/supprimés, arêtes
   ajoutées/supprimées, groupées par fichier/module.
4. Sortie : markdown lisible (« +12 fonctions, −3 ; le cycle X⇄Y est apparu ;
   `resolve_one` a 4 nouveaux appelants ») + `--json` (le format F2).

Coût assumé : une indexation complète de l'état à `<rev>` (django ≈ 11 s) — honnête et
documenté ; l'incrémental de diff est une optimisation future (Annexe A).

**Gate F4** : `selene diff HEAD` ⇒ diff vide, byte-déterministe sur 3 runs ; un commit
synthétique ajoutant une fonction appelée ⇒ le diff nomme exactement le nœud et l'arête ;
le worktree n'est **jamais** modifié (assert : fingerprint du répertoire avant/après).

---

## 6. F5 — mémoire de session

Journal local `.selene/memory.jsonl` : à chaque `explore`, une ligne
`{ts, question, roots, files_shown, flow_head}` (métadonnées, pas la réponse entière —
compacte, ~300 o/ligne). Surfaces :
- outil MCP **`recall`** `{query?}` : les explorations passées pertinentes (match
  lexical sur les questions ; sémantique plus tard si `semantic-search`) — « tu as déjà
  exploré ça le 12/08, les roots étaient X, Y ».
- `selene memory` (CLI) : liste/purge.
**Décision** : `explore` lui-même n'est **pas** modifié en v1 (ses sorties restent
déterministes et comparables aux goldens) — le rappel est un outil séparé que l'agent
appelle. `purge` efface le journal (audit purge existant). Opt-out `SELENE_NO_MEMORY=1`.
Local uniquement — l'invariant « rien ne sort de la machine » couvre le journal.

**Gate F5** : le journal n'altère aucun octet des réponses explore (goldens inchangés) ;
`recall` répond success-shaped sur journal vide ; purge le supprime.

---

## 7. F6 — `selene query --raw`

CLI **seulement** (la surface MCP reste les outils curés — un agent qui improvise du
SurrealQL est exactement ce que l'anti-Read interdit). Garde-fous, dans l'ordre :
1. parse par `surrealdb::sql::parse` ; **toute** déclaration non-`SELECT` ⇒ refus exit 1
   (le seul cas d'erreur avec le path invalide — la discipline exit-code existante) ;
2. `LIMIT 1000` injecté si absent ; timeout 5 s ;
3. sortie JSON (lignes brutes), stderr = stats (n lignes, durée).
Ouverture du store : `query_root_direct` (respect du daemon, message existant).

**Gate F6** : `UPDATE`/`DELETE`/`DEFINE`/`REMOVE`/transactions refusés (table de cas) ;
un `SELECT` sur `node` renvoie du JSON parsable ; le refus n'écrit rien (fingerprint
`.selene/` inchangé).

---

## 8. F7 — confiance graduée sur les arêtes inférées

`Edge.confidence: Option<f32>` — **additif sur le wire** (`skip_serializing_if None` :
les graphes existants se relisent inchangés ; l'identité d'arête des gates de parité —
(source,target,kind,provenance)+synthesizedBy — ne contient pas ce champ, tolérance 0
préservée). Émetteurs : les synthétiseurs de dispatch assignent un palier par canal
selon la force de la preuve (route exacte 0.9, name-match multi-candidats 0.5…) — les
paliers par canal sont **documentés dans le ledger de déviations**, pas improvisés ;
plus tard les mentions `Embedding` portent leur cosine (PRD documents). Rendu explore :
`*(dynamic)*` devient `*(dynamic, 0.9)*` uniquement quand `Some` — le corpus golden du
gate Phase 4 est mis à jour en conséquence, une fois, avec la raison consignée.
DDL : champ optionnel sur les tables d'arêtes (`DEFINE FIELD IF NOT EXISTS`).

**Gate F7** : un graphe pré-F7 se relit sans erreur (test de migration) ; parité 6/6 et
dispatch 5/5 inchangés ; chaque canal de synthèse a un palier consigné.

---

## 9. F1 — documents : intégration et amendement de recherche

Le PRD `2026-08-14-document-ingestion-design.md` reste **l'autorité** pour F1 (couches,
modèle de données, seams, gates G1–G5). La recherche du 2026-08-18 **amende son spike
§5.5 (PDF)** — il est partiellement tranché :

- **pdf-extract 0.12.0** (MIT, pur Rust, ~903k dl/mois) en primaire — **avec isolation
  obligatoire** : casier documenté de panics sur PDF malformés (issue #141 : « ~50
  panic/crash fixes for untrusted PDF input », #108, #134, #147) ⇒ extraction dans un
  thread dédié + `catch_unwind`, tout panic = « fichier inextractible » success-shaped
  dans `FileRecord.errors`. Le `catch_unwind` du PRD documents passe de précaution à
  **exigence prouvée**.
- **Plan B consigné : lopdf 0.44.0** (`extract_text`, même famille de parse, surface
  d'échec plus simple, fidélité moindre) — le spike ne compare plus que ces deux-là
  sur le corpus de 10 PDF réels.
- **Disqualifiés** (binaire statique) : extractous (Tika/GraalVM natif + Tesseract,
  figé depuis 2024), pdfium-render (dylib C++ à fournir soi-même).
- **DOCX — décision renforcée** : bokuweb/docx-rs est un **writer-only** (vérifié) —
  la voie maison zip+quick-xml sur `word/document.xml` est confirmée ;
  docx-rust 0.1.11 (fork lecteur) est le Plan B consigné.
- **pulldown-cmark 0.13.4 confirmé** : `into_offset_iter()` donne les plages d'octets
  par événement ⇒ plages de lignes exactes des `Section` (l'exigence Read-parité).

---

## 10. Risques & invariants

### 10.1 Audit des nouvelles dépendances (toutes MIT ou MIT/Apache-2.0, zéro C)

| crate | version épinglée | pour | risque consigné |
|---|---|---|---|
| quick-xml | 0.41.x | F2 GraphML, F1-B docx | aucun notable (35M dl/mois, memchr seul) |
| pulldown-cmark | 0.13.x | F1-A | aucun notable (no-unsafe, 14M dl/mois) |
| gix | **=0.86.0 exact** | F4 | pré-1.0, 49 breaking releases ⇒ pin exact + confiné dans un module unique |
| zip | 4.x | F1-B docx | flate2 en rust-backend |
| pdf-extract / lopdf | 0.12.x / 0.44.x | F1-B pdf | panics ⇒ thread+catch_unwind obligatoire (§9) |

Budget : Δ binaire par défaut < 5 Mo toutes features confondues (mesuré à chaque gate).

### 10.2 Invariants (tous réaffirmés, deux étendus)

Déterminisme (chaque feature a son gate de reproductibilité — y compris la betweenness
parallèle, là où le crate du commerce échoue) · anti-Read (F3 ferme une violation en
creux ; F6 est CLI-only pour ne pas ouvrir une échappatoire agent) · `isError` réservé ·
budgets monotones · binaire statique unique sans C · `server-instructions` source
unique · **rien ne sort de la machine, jamais** (étendu à F5 : le journal est local ;
F4 : gix ne fait aucun accès réseau — lecture d'objets locaux uniquement) · **le
worktree de l'utilisateur n'est jamais modifié** (nouveau, F4 le grave : un outil de
lecture qui écrit dans le repo est une faute).

---

## 11. Ordre d'implémentation (recommandé) et critère de fin

**F2 → F3 → F1-A → F6 → F4 → F5 → F7 → F1-B**, chaque feature : plan TDD bite-sized →
implémentation → gate au vrai binaire → commit. Rationale de l'ordre : F2 d'abord parce
que sa sérialisation canonique est l'outil de mesure de F4 et des gates F1 ; F3 avant
F1-A parce qu'il déplace `analysis.rs` (déménager avant de construire dessus) ; F1-B en
dernier parce que son spike PDF est le seul travail à issue incertaine.

**Critère de fin (« prêt pour la prod »)** : les 8 features gate-vertes au vrai
binaire + `cargo test --workspace` 0 FAILED (compté, jamais `| head`) + clippy 0 +
parité/dispatch verts + ask.sh 3/3 + README/RESUME à jour + binaire redéployé
(`rm+cp ~/.local/bin/selene`) + `du -sh target` < 30 Go.

---

## Annexe A — Décisions ouvertes

- **F4 incrémental** (diff sans ré-index complet, via `content_hash` des deux états) :
  optimisation, après que le diff exact fonctionne.
- **F3 approximation** : seuil exact→pivots (50k ?) à calibrer sur VS Code.
- **F5 rappel sémantique** (embed des questions passées) : après F1 couche 3.
- **GEXF, Cypher export** : sur demande réelle uniquement.
- **`graph_insights` en CLI** (`selene insights`) : probable, trivial une fois le MCP fait.
- **Paliers de confiance par canal (F7)** : valeurs exactes arbitrées au plan, dans le ledger.
