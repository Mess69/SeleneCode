# PRD — Ingestion de documents (Markdown, PDF, DOCX) dans le graphe

**Date :** 2026-08-14
**Statut :** Cible d'architecture (le « quoi », pas l'ordre d'exécution)
**Auteur :** dérivé du brief Graphify + audit du codebase (workflow `doc-prd-groundwork`,
puis revue adversariale 3-lentilles `prd-adversarial-verify`, 2026-08-14)
**Portée :** ingestion de documents non-code dans le graphe SeleneCode — formats, modèle de
données, pipeline, liaison doc↔code, surfaces. **Hors périmètre :** images, vidéo/audio,
Google Workspace, `.html`, `.xlsx`, la passe LLM de concepts (déférées en Annexe A).

> Ce document décrit l'**état cible**. Il n'impose pas de séquence de phases. Un plan
> d'exécution séparé (via `writing-plans`, dans `docs/plans/`) décidera de l'ordre de
> construction. Chaque fait sur le codebase cité ici a été vérifié `fichier:ligne` le
> 2026-08-14, puis contre-vérifié par une passe adversariale — les corrections de cette
> passe sont intégrées (corpus dispatch, filtre de kinds d'explore, seam du résolveur).

---

## 1. Contexte & motivation

### 1.1 Le graphe répond au « quoi / comment », pas au « pourquoi »

Le graphe SeleneCode capture la structure du code (AST) et ses flux (résolution +
synthèse de dispatch). Ce qu'il ne capture pas : la **rationale** — les specs, ADR,
design docs, README qui expliquent *pourquoi* le code a cette forme. Or c'est la
question qu'un agent pose juste après « comment ça marche » ; aujourd'hui la seule
réponse est un `Read` du document — exactement ce que le produit existe pour empêcher.

Aujourd'hui un `.md`/`.pdf`/`.txt` **n'entre jamais dans l'index** : `is_source_file`
(dérivé du même `EXTENSION_MAP` que `detect_language` — « so parser support and
indexing selection can never drift ») renvoie false, et les deux chemins de scan
filtrent dessus (git fast path `scan/mod.rs:160`, FS-walk `scan/mod.rs:883-886`).
Pas de `FileRecord`, pas de nœud, rien.

### 1.2 L'état de l'art — Graphify — et sa limite structurelle

Graphify (v8) ingère `.md .mdx .qmd .html .txt .rst .yaml .yml`, `.docx .xlsx`
(extra `[office]`), `.pdf` (extra `[pdf]`), images et vidéo dans le même graphe que
le code. Leur découpage, verbatim : *« Code is parsed locally with tree-sitter AST:
deterministic, no LLM, nothing leaves your machine. (Docs, PDFs, images and video
use your assistant's model, or a configured API key, for a semantic pass.) »*
Leurs liens doc→code : *« `# NOTE:` / `# WHY:` comments and ADR/RFC citations become
first-class nodes linked to the code »* ; liens markdown et `[[wikilinks]]` →
arêtes `references` ; arêtes taguées `EXTRACTED` vs `INFERRED` ; cache SHA-256
incrémental.

**La limite** : leur passe documents **exige un LLM** et, par défaut, **le contenu
des documents quitte la machine** (modèle de la session IDE ou clé API ; seul
Ollama est local). Le résultat est non-déterministe (deux runs LLM ⇒ deux graphes).

### 1.3 La thèse SeleneCode : plus loin qu'eux sur leur propre terrain annoncé

SeleneCode peut offrir l'ingestion de documents en **battant** Graphify sur l'axe
qu'ils revendiquent (local-first) : **trois couches, toutes locales, les deux
premières byte-déterministes, la troisième reproductible — et cette
reproductibilité est un gate (G1b), pas un slogan** :

1. **Structure** (déterministe) — parse structurel des documents : sections,
   titres, liens, code-spans. Zéro LLM, zéro réseau.
2. **Liaison lexicale** (déterministe) — un ladder de mentions doc→code :
   chemins cités, identifiants en code-spans, liens — **lié par le résolveur**
   (Phase 3), comme toute référence cross-file (§5.4).
3. **Liaison sémantique** (locale, **reproductibilité mesurée par G1b**) —
   `selene-embed` existe déjà : fastembed v5 / ONNX natif, BGE-small-en-v1.5
   384-d, CPU, aucun appel réseau après le téléchargement unique du modèle
   (`selene-embed/src/lib.rs:5-33`). Son API est générique —
   `embed_documents(&[String])` accepte tout texte (`lib.rs:40-56`) ; seule la
   moitié stockage est spécifique aux symboles.

La passe LLM de concepts (le seul étage où Graphify voit des choses que l'ONNX ne
voit pas) est **explicitement hors du chemin par défaut** et déférée (Annexe A).

**Invariant produit renforcé (nouveau, à graver en §8.2) : rien ne sort de la
machine, jamais — y compris pour les documents.** Graphify ne peut pas l'écrire ;
nous si.

---

## 2. Inventaire des formats (parité cible)

| format | vague | couche 1 (structure) | texte source du rendu | précédent codebase |
|---|---|---|---|---|
| `.md` (+ `.mdx` dégradé en md) | **A** | sections par titres, liens, wikilinks, code-spans, front-matter ignoré | le fichier lui-même (`code_of`, numéros de ligne réels) | `is_file_level_only` (yaml/twig/properties) : FileRecord + zéro symbole — `walker/ladder.rs:29-33` |
| `.txt`, `.rst` | **A** | fichier = 1 section (txt) ; sections par titres soulignés (rst, best-effort) | le fichier lui-même | idem — ⚠ voir §8.1 : 5 `requirements.txt` vivent dans le corpus du gate dispatch |
| `.pdf` | **B** | couche texte extraite, sections par heuristique de taille/page | **texte extrait stocké** (§4.4) | aucun — nouveau chemin bytes |
| `.docx` | **B** | XML `word/document.xml` (zip) : styles Heading → sections | **texte extrait stocké** | aucun — nouveau chemin bytes |
| `.qmd`, `.html`, `.xlsx`, `.yaml/.yml` (sections), images, vidéo, `.gdoc/.gsheet` | hors périmètre v1 | — | — | Annexe A ; yaml reste file-level-only (pas de sections) — déficit assumé vs Graphify |

**Contreparties (assumées, à consigner) :** pas de vision/OCR (un PDF scanné sans
couche texte donne un `FileRecord` avec erreur d'extraction non-fatale, pas de
sections) ; pas de tableurs ; `.mdx` parse comme du markdown (le JSX est ignoré).

---

## 3. Les trois couches d'ingestion

### 3.1 Couche 1 — structure déterministe

**Décision : `pulldown-cmark` pour le markdown, pas un grammar tree-sitter.**

| critère | tree-sitter-md (tiers) | pulldown-cmark |
|---|---|---|
| cohérence avec les 12 grammars épinglés | + (même mécanique) | − (2ᵉ mécanique) |
| maturité / conformité CommonMark | moyenne (grammar non officiel) | référence de l'écosystème (rustdoc, mdBook) |
| coût binaire / build | un grammar C de plus | pure Rust, léger |
| positions byte/ligne exactes | oui | oui (offsets → lignes) |
| **Verdict** | | **pulldown-cmark** |

Raisons : (a) un document n'est pas du code — la convention « 12 grammars, tous
des langages » (`Cargo.toml:112-123`) reste intacte ; (b) conformité CommonMark
supérieure ; (c) le walker tree-sitter et son ladder restent purs code — le
doc-parser est une **branche à côté**, pas dedans.

PDF : **spike requis** (§5.5). DOCX : zip + `quick-xml` sur `word/document.xml`
(le format est du XML zippé ; parse déterministe, pas de dépendance lourde).

Ce que la couche 1 émet par fichier (voir §4.3 pour la provenance `parser`) :
un nœud `Document` (le fichier), des nœuds `Section` (un par titre), des arêtes
`contains` Document→Section (intra-fichier), et des **candidats de référence**
pour tout ce qui pointe hors du fichier — liens `[texte](autre.md)`,
`[[wikilinks]]`, chemins cités, code-spans — que le résolveur lie en Phase 3
(§5.4). L'extraction n'émet **aucune arête cross-file** : c'est le contrat gravé
du crate (`selene-extract/src/lib.rs:24-32`), et il tient pour les docs aussi.

### 3.2 Couche 2 — le ladder de liaison doc→code (déterministe, dans le résolveur)

L'extraction émet les candidats (`UnresolvedReference`, canal dédié `doc-mention`,
sur le modèle du canal `fnref` existant) ; **`selene-resolve` les lie** — c'est lui
qui possède la table nom→nœuds construite en un seul scan RAM (le pattern qui a
fait ladder 6 839 → 1 889 ms, RESUME §3 ; `matcher/receiver.rs`), et c'est le seul
étage que `sync` re-exécute pour re-lier le cross-file après un changement
(`selene-sync/src/lib.rs:137-152`) — l'incrémental des mentions est donc acquis
par construction, dans les deux sens (le doc change, ou le code cité change).

Ordre fixe du ladder, tie-breaks déterministes, étage marqué dans `metadata` :

1. **Chemins cités** : segment de texte égal à un `FileRecord.path` connu ⇒
   `mentions` Section→nœud du fichier cible — vers son nœud `File` s'il existe
   (fichiers parsés par grammar), vers son nœud `Document` pour un doc ; un
   fichier file-level-only (yaml…) n'a **aucun nœud** (`walker/ladder.rs:29-33`)
   ⇒ pas d'arête, pas d'invention.
2. **Code-spans** : contenu d'un `` `backtick` `` égal exactement à un `name` ou
   `qualified_name` de symbole ⇒ `mentions` Section→symbole. Ambiguïté (N
   symboles homonymes) : lier si N ≤ K, sinon ne rien émettre — un lien faux
   coûte plus qu'un lien absent. **K est calibré par le corpus G3b, pas décrété**
   (Annexe A ; hypothèse de départ K=3).
3. **Liens doc→doc** : arête `references` Section→Document (parité Graphify),
   liée par le même canal.

### 3.3 Couche 3 — liaison sémantique locale (opt-in, reproductibilité mesurée)

Étend `selene embed` (existant) : embedder aussi les nœuds `Section`, stockés
dans **le même champ `embedding` du node table** — les sections *sont* des
nœuds, l'index HNSW cosine existant les couvre sans DDL nouveau
(`selene-db/src/semantic.rs:47-89`). Effets :

- **`query`/`search` : zéro travail** — ces surfaces passent un filtre de kinds
  vide (`selene-cli/src/cmd/query.rs:64`, `selene-mcp/src/semantic.rs:59`) ;
  `hybrid_search` (BM25 + KNN, fusion RRF k=60 — `semantic.rs:161-213`) renverra
  des sections dès qu'elles existent.
- **`explore` : PAS gratuit** — chaque point d'admission de la relevance filtre
  sur l'allowlist fermée `HIGH_VALUE_NODE_KINDS` (`relevance.rs:70-90`, posée
  par les deux constructeurs d'options `:404`/`:443`, appliquée aux seeds
  `:558`, au FTS `:639`, à la passe 4½ `:677`, à l'admission en traversée
  `:1241`). Sans modification, un nœud `section` est silencieusement invisible
  d'explore. **Le changement requis — et son garde-fou — sont spécifiés en §6.**

En plus : arêtes `mentions` **sémantiques** Section→symbole quand
cosine ≥ seuil (calibré par G3), plafonnées à K par section, marquées
`Provenance::Embedding` — le pendant exact de `Heuristic` pour le dispatch :
« ce lien n'est pas prouvé par le texte, il est inféré — et affiché comme tel ».

Reproductibilité : modèle + version + dimension épinglés ; l'hypothèse « même
entrée ⇒ même vecteur ⇒ mêmes arêtes » est plausible (inférence CPU ONNX) mais
fastembed parallélise ses batches en interne (`selene-embed/src/lib.rs:62-63`)
— **elle se mesure, elle ne se décrète pas : gate G1b.**

### 3.4 Couche LLM — hors défaut, déférée

Pas dans cette cible. Sketch et conditions en Annexe A. Le jour où elle existe :
Ollama d'abord (local), opt-in explicite, provenance dédiée, jamais requise par
aucune surface.

---

## 4. Modèle de données

### 4.1 NodeKind : 22 → 24

Ajouts : `Document` (`"document"`), `Section` (`"section"`).
Sites à mettre à jour (additif mais multi-sites, vérifié) :

- l'enum + `ALL` + `as_str` + le test `kind_counts_match_the_data_model` 22→24
  (`selene-core/src/lib.rs:62-142, 554-584`) ;
- **le filtre d'explore** : `Section` (et `Document` ?) rejoignent — sous
  contrôle, §6 — `HIGH_VALUE_NODE_KINDS` et ses deux constructeurs
  (`selene-context/src/relevance.rs:70-90, 404, 443`) ;
- `is_low_signal` de la viz (les docs hors galaxie par défaut, §6).

Aucun DDL (le `kind` est un champ de la table `node` unique). Un vieux binaire
lisant le nouveau wire string a un comportement défini, pas un crash : erreur
serde « unknown variant » via le chemin de lecture du store
(`selene-db/src/nodes.rs:174`), `Error::UnknownNodeKind` via `FromStr`
(parsing des filtres de query).

### 4.2 EdgeKind : 12 → 13 — et l'audit des traversées

Ajout : `Mentions` (`"mentions"`) — doc→code (couches 2 et 3).
`all_ddl()` itère `EdgeKind::ALL` ⇒ la table relationnelle se crée toute seule,
idempotente (`selene-db/src/schema.rs:236-248`) ; `meta.rs:202-222` (stats) suit.

**Alternative considérée et rejetée : réutiliser `References`.** `references`
est déjà « une lance à incendie » (2 666 arêtes sur le dogfood — RESUME §2) et
les passes de flow/relevance la traversent par listes en dur (`BFS_KINDS`
`flow.rs:42,52`, `boundaries.rs:66`, `node_view.rs:62,68`,
`relevance.rs:345,1060`) ; y verser les mentions polluerait les flows de code.
Une arête `mentions` distincte n'oblige à auditer **aucun** de ces sites — ils
listent leurs kinds explicitement. **Exception** : les liens doc→doc restent
`references` (parité Graphify ; les deux extrémités sont des docs, zéro
pollution des flows de code).

⚠ **Un site traverse par `EdgeKind::ALL` et DOIT être audité** :
`prefetch_impact_adjacency` construit sa liste par `ALL` moins `Contains`
(`selene-db/src/traverse.rs:417-420`) — sans exclusion, chaque `impact` d'un
symbole aspirerait ses sections mentionnantes puis, par les `references`
doc→doc, le sous-graphe documentaire entier. **Décision : `impact` exclut
`Mentions` en v1** (un doc qui mentionne un symbole ne « casse » pas) ; une
surface « docs périmées » pourrait l'exploiter plus tard (Annexe A).

### 4.3 Provenance : 3 → 5, et l'audit obligatoire des consommateurs

Ajouts : **`Parser`** (`"parser"`) — couches 1–2, structurel déterministe — et
**`Embedding`** (`"embedding"`) — couche 3. Précédent : `Scip` est déclaré et
jamais produit (`selene-core/src/lib.rs:247-256`) — l'enum accueille des
producteurs non-tree-sitter par conception.

Pourquoi pas `TreeSitter` pour les couches 1–2 : son doc-comment définit le
variant comme « Extracted directly from the tree-sitter AST » (`lib.rs:249-251`)
— pulldown-cmark et quick-xml n'en sont pas ; l'estampiller serait une
provenance **fausse sur le wire**. `Parser` dit la vérité : « prouvé par un
parse déterministe de la source, hors tree-sitter ».

⚠ **Piège vérifié** : les consommateurs branchent uniquement sur
`== Heuristic` (`flow.rs:428`, `boundaries.rs:70`, `builder.rs:338`,
`report.rs:160`) — tout nouveau variant est silencieusement traité comme
« statiquement prouvé ». Pour `Parser` c'est le rendu correct ; pour
`Embedding` c'est un mensonge. **Le plan d'exécution DOIT auditer ces 4
sites** : `Embedding` se rend `*(inferred)*`, comme `Heuristic` se rend
`*(dynamic)*`.

### 4.4 Identité, positions et texte des nœuds documentaires

- **Un fichier doc émet exactement un nœud `Document`, pas de nœud `File`.**
  L'exception d'id non-hashé `file:<path>` est spécifique au kind `File`
  (`selene-core/src/ids.rs:59-63`) ; `Document` et `Section` utilisent le
  `node_id` standard hashé `{file_path}:{kind}:{name}:{start_line}`
  (`ids.rs:52-57`), comme les nœuds `Route`.
- **Positions.** `start_line`/`end_line` sont des `u32` obligatoires
  (`lib.rs:296-298`) et `start_line` est le discriminant de l'id. md/txt/rst :
  plages de lignes réelles (deux « Introduction » répétés se distinguent par
  leurs lignes). **pdf/docx : `start_line` = `end_line` = ordinal 1-based de la
  section dans le document** — synthétique, stable, et discriminant des titres
  répétés. `Document` : `start_line` = 1, `name` = basename.
- **Texte : un seul régime pour l'indexation, deux pour le rendu.** **Toutes**
  les sections (md compris) stockent leur contenu plafonné (4 KiB) dans
  `Node.docstring` — c'est ce que `embedding_text()` (name + qualified_name +
  signature + docstring, `semantic.rs:32-44`) et le FTS lisent : sans cela, une
  section md serait indexée sur son titre seul. Le **rendu** diverge : md/txt/rst
  rendent via `code_of` (source verbatim numérotée, parité Read — le produit) ;
  pdf/docx rendent leur `docstring` (pas de fichier texte sur disque), avec
  l'en-tête `— extracted from PDF/DOCX`. Alternative side-car
  `.selene/doc-text/` rejetée : un 2ᵉ chemin disque et un 2ᵉ cache à invalider
  pour le même octet rendu.

### 4.5 Language, FileRecord, versionnement

- `Language` : + `Markdown`, `PlainText`, `Rst`, `Pdf`, `Docx` (additif, wire =
  lowercase — `language.rs:36-125`).
- `FileRecord` inchangé dans sa forme : `language` est un `String` libre,
  `content_hash` pilote l'incrémental (`orchestrator.rs:486-492, 638-641`) —
  le SHA-256-cache de Graphify, on l'a déjà. ⚠ pour les formats binaires, le
  hash a besoin d'un contrat bytes (§5.2).
- **`EXTRACTION_VERSION` 2 → 3** : la constante vit dans
  `selene-core/src/ids.rs:38` (avec son test d'épinglage `ids.rs:148-153`, à
  bumper ensemble, raison documentée) ; le doc-comment périmé de
  `selene-extract/src/lib.rs:138` (« currently 1 ») se corrige au passage.

---

## 5. Pipeline — quatre seams, et le chemin binaire

### 5.1 Scan : le routeur d'extensions

Une seule source : ajouter les extensions à `language_for_extension`
(`selene-extract/src/language.rs:41-86`) — `is_source_file` en dérive, les deux
chemins de scan (git + FS) suivent sans autre modification. Les documents
respectent `.gitignore` comme le code.

### 5.2 Lecture : le chemin bytes — aux DEUX endroits qui lisent

Contrainte vérifiée : `read_input` = `std::fs::read_to_string` (UTF-8 strict,
`ParseInput.content: String`) + cap `MAX_FILE_SIZE` 1 MiB
(`orchestrator.rs:249-255, 812-849`). **Et un second seam UTF-8 que le premier
jet de ce PRD avait manqué** : le classifieur de changements de `sync` lit
chaque candidat par `read_to_string` et jette silencieusement un binaire
inconnu (`Err(_) => {}` — `selene-sync/src/lib.rs:96-108`) : un `.pdf` nouveau
ne serait **jamais** indexé par `selene sync` ni par le watcher du daemon.

Requis : (a) lecture `Vec<u8>` pour les formats documentaires **dans
l'orchestrateur ET dans le classifieur de sync** ; (b) un contrat
`hash_bytes`/`hash_content(&[u8])` pour le `content_hash` des binaires (les
golden-byte tests de `ids.rs` épinglent la variante texte — ils restent) ;
(c) caps par format : md 1 MiB inchangé ; pdf/docx 20 MiB de fichier, 512 KiB
de texte extrait — au-delà, sections tronquées + diagnostic non-fatal dans
`FileRecord.errors`.

### 5.3 Extraction : une branche à côté du ladder — couche 1 seulement

`extract_from_source` route : `is_file_level_only` → vide ; grammar → walker
tree-sitter ; **nouveau : `is_document(language)` → doc-parser**. La branche
émet les nœuds `Document`/`Section`, les arêtes `contains` intra-fichier, et
des `UnresolvedReference` (canal `doc-mention`) pour tout lien/chemin/code-span
— **jamais une arête cross-file** : la signature même de la fonction
(`file_path, source, language` — pas de store, `walker/ladder.rs:25`) rend la
liaison impossible ici, et le contrat du crate la proscrit
(`selene-extract/src/lib.rs:24-32`). Échec de parse (PDF chiffré, docx
corrompu) = `FileRecord` + erreur `Severity::Warning` — jamais fatal, jamais
`isError`.

### 5.4 Résolution : la couche 2 vit dans `selene-resolve`

Un canal `doc-mention` dans le résolveur (sur le modèle des canaux existants) :
consomme les candidats de §5.3, les lie avec la table nom→nœuds en RAM
existante, émet les arêtes `mentions`/`references` avec `Provenance::Parser`.
Parce que `sync` ré-exécute la résolution après chaque changement
(`selene-sync/src/lib.rs:137-152`), l'incrémental des liens doc↔code est acquis
sans mécanique nouvelle — le doc change **ou** le code cité change, les
mentions se relient.

### 5.5 (spike) Extraction texte PDF — qualité à mesurer avant de figer

> **Ce n'est pas encore une décision d'architecture, c'est un travail de
> recherche à faire.** Le PRD acte le principe (couche texte déterministe) ;
> le spike tranche la crate.

1. Candidates : `pdf-extract`, `lopdf` (bas niveau), `pdfium-render` (binding
   lourd, probablement disqualifié par le « single static binary »).
2. Mesure : sur ≥ 10 PDF réels (papers arXiv, docs d'archi export-Confluence,
   README exportés) — taux de texte récupéré, ordre de lecture, coût binaire,
   panics (⚠ `catch_unwind` obligatoire, précédent resolveurs — RESUME §3).
3. **Livrable du spike** : une note `docs/benchmarks/` fixant la crate + les
   caps, **produite avant de geler le schéma des nœuds `Section` PDF**.
   **Plan B consigné :** si aucune crate pure-Rust n'est fiable, la v1 livre
   md/txt/rst/docx seulement et le PDF reste en erreur d'extraction propre —
   le pipeline et le modèle de données ne changent pas.

---

## 6. Surfaces

- **explore** — le changement requis (§3.3) et son garde-fou, ensemble :
  `Section` entre dans le filtre de kinds d'explore **via un slot d'admission
  dédié et plafonné** (≤ 2 sections par pool de candidats), pas en concurrence
  libre dans le pool partagé — sinon le cap de rendu « ≤ 2 sections par
  réponse » ne gouvernerait rien : l'éviction du code se joue à l'admission,
  pas au rendu. Les docs enrichissent une réponse code, ils ne l'évincent
  jamais (l'anti-Read porte sur le code d'abord). Rendu selon les deux régimes
  de §4.4. Budgets : tiers et monotonicité intouchés.
- **query / search** : zéro travail (filtre de kinds vide, vérifié §3.3).
- **viz** : `Document`/`Section` rejoignent `is_low_signal` par défaut (la
  galaxie reste une carte du *code*) ; `--all-kinds` les montre.
- **report** : section « Documentation » — docs orphelins (zéro `mentions`
  sortante), top sections par degré de mentions.
- **impact** : exclut `Mentions` (décision §4.2).
- **MCP `server-instructions`** (source unique de guidance, invariant) :
  une ligne — « explore couvre aussi les documents du repo ; cite la section,
  ne lis pas le fichier ».

---

## 7. Mapping Graphify → SeleneCode (parité et dépassement)

| capacité Graphify | équivalent cible SeleneCode | verdict |
|---|---|---|
| `.md .txt .rst` ingérés | couche 1 (vague A) | parité |
| `.mdx .qmd .html .yaml/.yml` (sections) | `.mdx` dégradé en md ; `.qmd`/`.html` hors v1 ; yaml reste file-level-only | déficit assumé (Annexe A) |
| `.pdf` / `.docx` (extras `[pdf]`/`[office]`) | pdf (spike §5.5) + docx natifs dans LE binaire | parité docx sans extra ; **xlsx : déficit assumé** |
| liens md/wikilinks → `references` | idem (couches 1+2, liées en §5.4) | parité |
| `NOTE:`/`WHY:` → nœuds liés au code | déjà couvert côté code (docstrings extraites) + mentions doc→code | parité |
| tags `EXTRACTED`/`INFERRED` | `Provenance` (`parser`/`embedding`) — plus fin, par-arête | dépassement |
| passe sémantique LLM (contenu → API externe par défaut) | embeddings ONNX locaux, reproductibilité **mesurée** (G1b), zéro réseau | **dépassement (la thèse §1.3)** |
| cache SHA-256 incrémental | `content_hash` existant + relien automatique via sync→resolve (§5.4) | parité (déjà là) |
| images / vidéo / gdocs | hors périmètre v1 | déficit assumé (Annexe A) |

---

## 8. Risques & invariants

### 8.1 Risques

- **Le gate dispatch N'EST PAS sans docs.** Contre-vérifié : 5 fichiers
  vague-A vivent dans son corpus —
  `fixtures/dispatch/{flask,fastapi,django,django-orm,django-orm-control}/requirements.txt`
  — et le gate pilote le pipeline de production (`tests/pipeline/mod.rs:318`
  → scan → `is_source_file`) puis compare le multiset d'arêtes sur **tous**
  les kinds (`resolution_parity_gate.rs:230`, `EdgeKind::ALL`) contre une
  baseline TS qui n'a pas ces nœuds, tolérance 0 : ajouter `.txt` au scan la
  casserait telle quelle. **Décision : le gate exclut les kinds documentaires
  (`Document`/`Section`/`Mentions` et leurs `contains`) de son multiset** —
  son objet est la parité de *résolution TS↔Rust*, où les docs n'existent
  pas — consigné dans le ledger de déviations
  (`fixtures/dispatch/deviations.toml`, l'autorité unique). Le corpus de
  parité d'**extraction** (`selene-extract/tests/fixtures/parity/`, 13
  langages) est, lui, vérifié sans docs.
- **Qualité texte PDF** — le spike §5.5 la mesure ; Plan B consigné.
- **Panics de crates tierces** (PDF malformés) — `catch_unwind` autour du
  doc-parser, erreur collectée dans `FileRecord.errors` ; **jamais**
  `panic = 'abort'` (interdit, RESUME §3 ⛔).
- **RAM/temps d'indexation** — caps §5.2 ; gate G4. Le doc-parse entre dans le
  fan-out rayon existant, pas un étage sérialisé de plus.
- **Éviction du code dans explore** — slot d'admission plafonné (§6) ; G3
  vérifie zéro régression sur le corpus de questions code.
- **Secrets dans les docs** — les docs restent locaux et rien ne sort de la
  machine (§8.2) ; le rendu de config reste keys-only (inchangé, #383). Un
  document *prose* cité verbatim est le comportement voulu, comme `Read`.

### 8.2 Invariants (règle d'or : ne pas régresser — et un nouveau)

- **Extraction déterministe** — étendue : couches 1–2 **byte-déterministes**
  (G1) ; couche 3 **reproductible, mesuré** (G1b) ; LLM : jamais dans le
  chemin par défaut, jamais requis par une surface.
- **Zéro arête cross-file en extraction** — tient pour les docs (§5.3/§5.4).
- **Rien ne sort de la machine, jamais** *(nouveau, gravé)* — y compris le
  contenu des documents. Le seul réseau toléré : le téléchargement unique et
  explicite du modèle ONNX par `selene embed` (comportement existant).
- **Sufficiency / anti-Read** — une question de rationale se résout dans
  explore sans `Read` du document, **sur le binaire par défaut** (G3) ; le
  code garde la priorité d'admission et de budget.
- **Budgets monotones** — intouchés.
- **`isError` réservé** — un PDF illisible est un diagnostic, pas une erreur MCP.
- **`server-instructions` = source unique** de la guidance agent.
- **Local-first, zéro serveur, gratuit en local** — inchangé.

---

## 9. Critères de succès (gates)

- **G1 — Déterminisme (build par défaut)** : indexer deux fois un corpus avec
  docs, couches 1–2 ⇒ dumps de graphe byte-identiques (nœuds + arêtes +
  provenance).
- **G1b — Reproductibilité sémantique (build `semantic-search`)** :
  `selene embed` + liaison sémantique deux fois sur la même machine ⇒ ensemble
  d'arêtes `mentions` identique ; les marges cosine proches du seuil sont
  loggées. « Reproductible » se grave en §8.2 **après** ce gate, pas avant.
- **G2 — Zéro régression code** : sur les corpus existants (dogfood, django),
  le sous-graphe code (hors kinds documentaires) est byte-identique
  avant/après ; gates de parité et dispatch verts avec l'exclusion §8.1
  consignée au ledger, tolérance 0 sur le reste.
- **G3 — La question de rationale** (le gate produit) : corpus d'≥ 8 questions
  « pourquoi / où est-ce documenté » sur ≥ 2 dépôts réels ; explore répond avec
  la section pertinente rendue, **zéro Read**, mesuré au vrai binaire
  (protocole `dogfood.sh`). **Barre de passage : le binaire PAR DÉFAUT**
  (FTS seul — la couche 3 ne doit jamais être *requise*, §3.4/§8.2) ; le build
  `semantic-search` est rapporté en delta. Et zéro régression sur le corpus de
  questions code (ask.sh 3/3).
- **G3b — Le ladder de mentions** : corpus de fixtures doc+code assertant les
  arêtes `mentions` attendues à l'exactitude (dont un cas N > K assertant
  **aucune** arête) — le pendant documentaire du gate de dispatch, même
  discipline (fixtures + ledger).
- **G4 — Coût** : surcoût d'indexation < 10 % sur django-avec-docs ; RAM stable
  (pas de nouveau pic dhat) ; binaire par défaut : Δtaille < 3 Mo.
- **G5 — Le vrai binaire** : chaque gate est mesuré via le binaire release,
  jamais déduit des tests (`cargo test … | grep -c FAILED` ⇒ 0, jamais `| head`).

---

## Annexe A — Décisions ouvertes (à trancher au plan d'exécution)

- **Passe LLM de concepts (v2, spike futur)** : Ollama d'abord, opt-in
  (`selene embed --concepts`?), provenance dédiée (`llm`), nœuds « Concept »
  éphémères et reconstruisibles. À n'ouvrir qu'après un G3 vert — si les
  couches 1–3 suffisent aux questions de rationale, cette passe n'existe jamais.
- **K du ladder (ambiguïté des homonymes)** et **seuil cosine + K sémantique** :
  calibrés sur les corpus G3b/G3 — pas de valeur théorique, mesurer
  (hypothèses de départ : K=3 ; cosine à déterminer).
- **`.qmd`, `.html`** : candidats vague C (readability-style pour html) ;
  **`.xlsx`, `.yaml` en sections, images (vision), vidéo (whisper local),
  `.gdoc`** : non planifiés.
- **Granularité des sections** : titre = section (v1) ; sous-découpage des
  sections > 4 KiB à décider quand un cas réel le force.
- **`selene embed` : embed-docs par défaut ou flag ?** — v1 : les sections sont
  embeddées par le même `selene embed` sans flag (une commande, un
  comportement) ; à re-trancher si le coût mesuré le justifie.
- **Surface « docs périmées »** (impact inversé via `Mentions`, §4.2) : idée
  notée, non planifiée.
- **Ordre de construction** : hors périmètre de ce PRD. À décider via
  `writing-plans` (vague A d'abord — md/txt/rst — est l'hypothèse évidente :
  zéro dépendance nouvelle, le gros de la valeur ; ⚠ le plan devra traiter la
  décision §8.1 sur le gate dispatch AVANT d'ajouter `.txt` au scan).
