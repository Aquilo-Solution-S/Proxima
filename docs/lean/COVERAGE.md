# Kernel Coverage Matrix — docs → axioms

Every invariant extracted from the source docs (inventories of 2026-06-11) maps
to exactly one row: a kernel construct, a structural carrier ("the shape IS the
rule"), or an explicit exclusion with reason. Axiom names are in
`docs/lean/Foundations/*.lean`; namespace `Proxima`.

Carrier kinds: `axiom` | `def` | `inductive`/`structural` (the type shape
carries the rule) | `comment` (recorded as design commentary at the named
location) | `excluded` (engine/app concern, not ontology).

Baseline axiom count after transcription: **134** (per-file: Composition 23,
Memory 22, Identity 19, Edges 16, Goals 13, Operators 11, Citations 11,
Owner 9, Compliance 7, Prelude 3). The minimization pass updates this section
with before/after counts.

## universe.md (U)

| ID | Invariant | Carrier |
|---|---|---|
| U-1 | Layering strict & irreversible; no lower-layer memory from higher layer | axiom `fact_iff_event` + `facts_only_from_sources` + `legalClasses` upward-False cells; header comments Memory.lean |
| U-2 | Causal claims perspective-relative; no semantic/causal Fact→Fact; Perspective is locus of causal claims | `legalClasses` Fact→Fact cell (no Causal/Interpretive) + `operatorEdgeShape` PerspectiveLink arm; Edges.lean header |
| U-3 | Dreaming = flavor-declared consolidation; no Dream entity/relation/pipeline | structural absence; Operators.lean header comment |
| U-4 | Reality enters only through Event Sources | axiom `fact_iff_event` (with ME-1) |

## 01 — Event Source (ES)

| ID | Invariant | Carrier |
|---|---|---|
| ES-1 | Group org → billing annotation on group-owned rows | excluded: org demoted to engine — decision `2026-06-11-org-out-of-kernel.md`; `owner_org_id` is engine billing metadata with no kernel face (was THEOREM `owner_org_denormalized` when Owner carried org) |
| ES-2 | Visibility rule (principal-only) | def `visible` |
| ES-3 | org never enters access or identity | structural: org absent from the kernel entirely (`Owner := Principal`) — decision `2026-06-11-org-out-of-kernel.md` |
| ES-4 | event_id deterministic content hash of (source, owner, payload) | axiom `event_id_payload_determined` (kernel-visible projection) |
| ES-5 | Batch id unique within (source, owner) | excluded: per-scope engine validation with no kernel-observable face; F→A gate carries owner+personality dims (`ftoa_batch_exclusive`) — decision `2026-06-11-batch-id-scope.md` |
| ES-6 | Every Fact traces to an Event Source, no exceptions | axiom `fact_iff_event` |
| ES-7 | 1:1 event→Fact at engine boundary; no source-side aggregation | comment Identity.lean Event docstring; enforcement is membrane-contract (engine) |
| ES-8 | Source must not abstract/interpret/cross-join/relevance-filter/persist | excluded: EventSource impl contract (doc 01 §must-not list); kernel carries the consequence via `facts_only_from_sources` |
| ES-9 | Compliance metadata: every source declares 4 fields; Facts inherit | excluded: engine registration totality (CO-39..45 rows); kernel keeps SourceId opaque |
| ES-10 | Idempotency keys content-derived/opaque, never natural-person identifiers | comment Compliance.lean suppression docstring (CO-20) |
| ES-11 | Facts' typing frozen at insert; engine does not migrate Facts across schema versions | `Immutable Memory`-stance via `AppendOnly Memory` + accessor totality (`memory_schema` fixed per row); migration mechanics excluded (SR-50..55 exclusion block) |
| ES-12 | No `Principal.Org` variant; org-wide = `<org>-everyone` group | inductive `Principal` two-constructor shape |
| ES-13 | Per-memory ACL (AccessGrant) is v2+, not v1 | structural absence + Owner.lean header comment |
| ES-14 | Push vs pull is source-side implementation detail | excluded: engine |
| ES-15 | Bootstrap/founding-goal is flavor onboarding | excluded: app-layer |

## 02 — Memory (ME)

| ID | Invariant | Carrier |
|---|---|---|
| ME-1 | Fact ⟷ carries source event | axiom `fact_iff_event` |
| ME-2 | Fact owner = event owner | axiom `fact_event_owner` |
| ME-3 | Text iff derived (Facts none; A/P always, immutable, authored) | axiom `text_iff_derived`; immutability of Text per `AppendOnly Memory` + comment |
| ME-4 | Facts never supersede / never superseded | THEOREM `facts_never_supersede` (Edges.lean, via bridge + matrix) |
| ME-5a | Supersession same kind | THEOREM `supersession_same_kind` (Edges.lean) |
| ME-5b | Supersession same owner | THEOREM `supersession_same_owner` (Edges.lean, via edge scope) |
| ME-6 | Authoring personality shares memory owner | axiom `authoring_personality_owner` |
| ME-7 | Facts below read-scope matrix; A/P/Goal gated | defs `personality_may_read` (Memory) + `personality_may_read_goal` (Goals) + axiom `read_scope_diagonal` |
| ME-8 | Matrix asymmetry valid; future-reads-only; direct retrieval only | structural absence of symmetry axiom + comment |
| ME-9 | Edge scope single-owner | axiom `edge_scope_single_owner` |
| ME-10 | ℓ(source) ≥ ℓ(target) for memory edges | THEOREM `edge_layer_rule` (from the matrix's empty upward cells) |
| ME-11 | Class-legality matrix (9 cells) | def `legalClasses` + THEOREM `edge_class_legal` (from mask axioms `edge_respects_mask` + `descriptor_masks_tighten_only`) + bridge axiom `supersession_pointer_is_edge` |
| ME-12 | Supersession same endpoint shape (incl. Goal→Goal) | axiom `supersession_same_endpoint_shape` |
| ME-13 | Edges immutable v1 | instances `Immutable Edge`, `AppendOnly Edge` |
| ME-14 | Descriptor masks tighten, never relax | axioms `relation_endpoint_admitted`, `descriptor_masks_tighten_only`, `edge_respects_mask` |
| ME-15 | Causal chain is a query, not an entity; materialized = cache only | structural absence + Edges.lean header |
| ME-16 | Memory id is identity | axiom `memory_id_injective` |
| ME-17 | Personality: parallel lineages per instance; substrate stores instances/wake | comment Memory.lean §Personality; runtime tables excluded (engine) |
| ME-18 | Cross-personality supersession = explicit editorial gesture, never operator | excluded: write-path authorization (engine); lineage-scope default comment |
| ME-19 | Relation registry: unregistered relations invalid | `entities_use_registered_vocabulary` (Composition) |
| ME-20 | Core relations table (derived-from/supersedes/inspires/authored) | excluded: vocabulary content, not law — flavors/core register ids; classes pinned by `RelationClass` |

## 04 — Consolidation (CN)

| ID | Invariant | Carrier |
|---|---|---|
| CN-1 | F→A edge shape | def `operatorEdgeShape` + axiom `operator_edges_shaped`; full shape THEOREM `ftoa_edge_shape` |
| CN-2 | A→P edge shape | same merged carrier; full shape THEOREM `atop_edge_shape` |
| CN-3 | A→Goal evidence shape (Structural class) | `operatorEdgeShape` OperatorAtoGoal arm + `operator_edges_shaped` |
| CN-4 | frame/PerspectiveLink shape | `operatorEdgeShape` PerspectiveLink arm + `operator_edges_shaped` |
| CN-5 | No downward writes | axiom `facts_only_from_sources` (+ ME-1) |
| CN-6 | Derived memories have provenance | axiom `derived_has_provenance` (merged); per-kind shapes THEOREMs `abstraction_has_provenance`, `perspective_has_provenance` |
| CN-7 | Cross-domain join is typed Abstraction | comment (shape carried by CN-6 + U-2 matrix) |
| CN-8 | F→A batch-gate exclusivity per (owner, personality, batch, input contract, operator, output schema) | axiom `ftoa_batch_exclusive`; personality dim per decision `2026-06-11-ftoa-gate-personality-scope.md` |
| CN-9 | Atomic invocation (all-or-nothing outputs) | excluded: storage-layer transaction contract (same stance as WH event/projection atomicity) |
| CN-10 | Retry/changed-prompt = new derivation, never mutation | `AppendOnly Memory` + comment Operators.lean |
| CN-11 | Wake dispatcher loop, cursors, depth bound, runtime tables | excluded: engine runtime |
| CN-12 | Prompt locality (core ships no domain prompts) | excluded: engine/flavor split mechanics; spirit carried by CF-A |

## 06 — Goals (GO)

| ID | Invariant | Carrier |
|---|---|---|
| GO-1 | Supersession same owner | axiom `goal_supersession_constraints` (merged); projection THEOREM `goal_supersession_same_owner` |
| GO-2 | Valid lifecycle transition | def `goalTransitionAdmitted` + merged axiom; projection THEOREM `goal_supersession_admitted` |
| GO-3 | Goal DAG acyclic | inductive `goalAncestor` + axiom `goal_parents_acyclic` |
| GO-4 | Parents same owner | axiom `goal_parents_same_owner` |
| GO-5 | Every transition new row; no in-place mutation | `AppendOnly Goal` + `Supersedable Goal` |
| GO-6 | Goal is not Memory | structural: distinct Types |
| GO-7 | Self is a query, never an entity/cache | structural absence + Goals.lean header |
| GO-8 | Active set definition (heads, state=Active) | defs `goalIsHead`, `activeGoals` |
| GO-9 | Goal id is identity | axiom `goal_id_injective` |
| GO-10 | Authorship vocabulary | inductive `GoalAuthorship` |
| GO-11 | GoalWrite protocol (request_id idempotency, conflict detection, stream visibility) | excluded: protocol surface (doc 14 out of scope per spec) |
| GO-12 | Assignment = core/inspires edge Goal→Self-Perspective; instance-scoped active_goals query | shape via descriptor masks (ME-14); traversal query engine-level per decision `2026-06-11-active-goals-two-queries.md` |
| GO-13 | Goal-scoped wake policy; planner-first | excluded: engine runtime |
| GO-14 | Cross-owner assignment/evidence rejected | axioms `edge_scope_single_owner` + `goal_parents_same_owner` |

## 03 — Schema Registry (SR) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| SR-1 | Registry frozen at startup, no runtime registration | axiom `registry_composition` (the law); THEOREM `registry_determined` |
| SR-2 | Every memory payload schema-typed | accessor totality + `entities_use_registered_vocabulary` (merged) |
| SR-8 | Schema ids flavor-qualified | axiom `schema_namespace` + THEOREM `registry_namespace_discipline` |
| SR-11/16 | Fact no text / A-P text required | axiom `text_iff_derived` |
| SR-13 | Fact identity ≠ payload hash; UUIDv7 | axiom `memory_id_injective` + ST-22 comment |
| SR-14/44 | Fact has no supersedes | THEOREM `facts_never_supersede` |
| SR-24 | relation_class closed, not flavor-extensible | inductive `RelationClass` (note: doc 03 does NOT enumerate classes — doc 02's five are authoritative; no contradiction, verified 2026-06-11 against doc 03 §EdgePayload verbatim) |
| SR-25 | Edges immutable v1 | `Immutable Edge` |
| SR-30..33 | special_category per schema, author-declared | axiom `schema_special_category : SchemaId → Bool` (domain shape) |
| SR-43 | Stateful Fact: each observation a new Fact | `Immutable`-stance + ST-14 row |
| SR-46 | Tombstone is a Fact (deletion is observed state) | comment Identity.lean / excluded detail: stateful-schema mechanics |
| SR-49 | Memory row stores schema id+version+kind | accessors `memory_schema`, `memory_kind` |
| SR-56/57 | Layer/kind change in migration forbidden | excluded: migration mechanics; spirit = kind is fixed per memory (accessor totality, immutability) |

**SR exclusions (engine/SQL/Rust mechanics):** SR-3..7, SR-9, SR-10, SR-12,
SR-15, SR-17..23, SR-26..29, SR-34..42, SR-45, SR-47, SR-48, SR-50..55,
SR-58..72 — sidecar tables, validators, renderers, migration discipline,
opaque-schema rules, natural-key queries. Reason: typed-payload mechanics are
engine contracts beneath the opacity boundary (CF-G); the kernel deliberately
cannot see payloads.

## 08 — Core & Flavors (CF) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| CF-1 | Core owns substrate; flavors contribute build-time vocabulary | axioms `core_vocabulary`/`flavor_schemas`/`registry_composition`; THEOREM `core_always_present` (CF-A) |
| CF-2/3 | Build-time choice; no runtime registration tier | `registry_composition`; THEOREM `registry_determined` (CF-D) |
| CF-20..24 | Namespace prefix discipline | axiom `contributions_namespaced`; THEOREM `registry_namespace_discipline` (CF-B) |
| CF-49/50 essence | No cross-flavor collision | structural (identity carries namespace, CF-C comment) |
| CF-54/55 | Composite binary = build artifact, not plugin host | axiom `active_registry` (one ambient registry) + comment |
| CF-56 | No feature flags; linkage + register() | excluded: Rust mechanics; spirit = CF-D |
| CF-57/58 | Registry ownership per flavor; prefixes kept in composite | CF-B + CF-C |
| CF-60 | Cross-flavor reads obey owner/read-scope | axioms `edge_scope_single_owner`, def `personality_may_read` (already universal) |
| CF-61 | Cross-flavor edges use registered descriptors | THEOREM `edges_use_registered_relations` (projection of `entities_use_registered_vocabulary`) |
| CF-43..46 | Goal entity core-owned; flavor owns payload/tools | structural: Goal axioms live in kernel; payloads opaque |

**CF exclusions:** CF-4..19, CF-25..42, CF-47..53, CF-59, CF-62 — macro
surface, Cargo metadata, freeze-guard panics, tool packs, and wake-entry
validation. Reason: Rust composition mechanics; their ontology content is
CF-A/B/C/D above.

## 07 — Storage (ST) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| ST-1..4 | Fresh ids; immutable identity; supersession = new row | `memory_id_injective`, `goal_id_injective`, classes `Immutable`/`Supersedable` |
| ST-5 | Edges insert-only | `Immutable Edge`, `AppendOnly Edge` |
| ST-6 | EventId deterministic; duplicate = replay | axiom `event_id_payload_determined` |
| ST-7/8 | CitedObject/CitationMapping ids, insert-only, one mapping per Fact | `cited_object_id_injective`, `citation_mapping_id_injective`, `Immutable` instances + theorem `citation_unique_per_fact` |
| ST-9 | Owner identity columns (principal kind + id); `owner_org_id` billing annotation | `Owner := Principal` + `owner_principal`; org column is engine-only — decision `2026-06-11-org-out-of-kernel.md` |
| ST-10 | Cross-owner edges/evidence rejected | axiom `edge_scope_single_owner` |
| ST-11 | INSERT-only cognitive lifecycle | class `AppendOnly` + instances |
| ST-13 | Only compliance erasure deletes | Compliance.lean (`erased`, `erasure_removes_cognitive`) |
| ST-14 | Stateful current-state = head query, never replacement | comment + SR-43 row |
| ST-15..17 | Vector-store independence (targets F/A/P AND Goals) | `EmbeddingTarget` sum + `embedding_target` + `Immutable Embedding`; absence of entity→Embedding accessor |
| ST-22/23 | Content hash = dedup key not identity; collision semantics | comments on `EventId` + `event_id_payload_determined` |
| ST-26 | Supersession logical; current state = query | defs `goalIsHead`/`activeGoals` pattern + comment |

**ST exclusions:** ST-12 (sidecar migration), ST-18..21 (core/flavor SQL
ownership — spirit in CF-A), ST-24/25 (partitioning physical). Reason:
storage-layout mechanics.

## 11 — Citations (CI)

| ID | Invariant | Carrier |
|---|---|---|
| CI-1 | Fact-only citation (citation ⇒ Fact; OPTIONAL on Facts since 2026-06-13) | axiom `citation_fact_is_fact`; THEOREM `citation_implies_fact` over the choice-def `memory_citation` |
| CI-2 | Exactly one mapping per Fact; target is Fact; no orphans | axiom `citation_fact_injective`; THEOREMs `citation_points_back`, `citation_reverse_total`, `citation_unique_per_fact` |
| CI-3 | A/P cite transitively via provenance | comment + CN-6 axioms |
| CI-7/8 | Owner scoping; Fact owner = object owner | axiom `citation_owner_match` |
| CI-9 | One object ↔ N mappings | structural absence of object-side restriction |
| CI-12/13 | Edges do not cite | structural absence of citation accessor on Edge |
| CI-14 | Operator provenance = row metadata, not citation | comment Operators.lean |
| CI-4/5/6/10/11 | content-hash idempotency mechanics | excluded: engine (hash invisible below opacity boundary) |
| CI-15/16 | S3 bytes, presigned URLs | excluded: engine storage |

## 13 — Compliance (CO)

| ID | Invariant | Carrier |
|---|---|---|
| CO-1 | Cognitive lifecycle append-only | classes + instances (ST-11 row) |
| CO-2/6 | Compliance out-of-band, never a Memory mutation | inductive `ComplianceOp` + header comment |
| CO-3 | Scope = one Owner or Owner-scoped source object | constructor shapes of `ComplianceOp` |
| CO-4/5 | Admin-authored; operator visibility diminished | excluded: authorization surface (engine/protocol) |
| CO-7 | delete_owner removes cognitive, retains suppression+audit | axioms `erased`, `erasure_removes_cognitive` (Memory+Goal); THEOREM `erasure_removes_edges`; suppression survival structural |
| CO-8 | delete_source_scope | constructor `DeleteSourceScope` (semantics engine-resolved per flavor — comment) |
| CO-9/10 | Pause/resume semantics | axiom `paused` + comment (dispatch gate engine-enforced) |
| CO-12/13/14 | Outcomes incl. refusal-is-valid | inductive `ComplianceOutcome` |
| CO-15/16/20 | Suppression retains opaque key only | accessor shape `suppression_key : … → EventId` |
| CO-17/18 | Suppression blocks re-ingest | axiom `suppression_blocks_reingest` |
| CO-19/29 | Suppression/audit survive erasure indefinitely | structural (unconditional quantification; Compliance.lean comment) |
| CO-11, CO-21..28, CO-30..58 | Export, audit content, side effects, vocabulary fields, owner policy, GDPR mappings | excluded: controller/engine obligations and legal commentary; ES-9/ES-10 rows carry the kernel-relevant faces |

### r1 additions (codex review, 2026-06-11)

| ID | Invariant | Carrier |
|---|---|---|
| GO-2b | Stale prior cannot be lifecycle head | axiom `goal_supersession_prior_is_head` |
| GO-15 | Goal title/text core retrieval text | axioms `goal_title`, `goal_text` |
| GO-16 | Operator-authored Goals carry authoring personality, owner-matched | axioms `goal_authoring_personality`, `goal_authoring_personality_owner` |
| CI-17 | Cited objects / mappings schema-registered | `entities_use_registered_vocabulary` arms 4+5 |
| ST-EdgeId | EventSource edges content-hash id vs UUIDv7 (AGENTS.md inv. 17) | inductive `EdgeId` sum + axiom `edge_id_authorship_split` (Edges.lean) — exclusion reversed after both reviewers + AGENTS.md elevated it |

## Minimization log

**Pass 1 (2026-06-11, workflow: 9 analyzers + adversarial verify, 25
proposals, 23 verified):** post-r1 count 151 axioms → **132 axioms + 23
theorems**, lake-build green. Of the 132, ~60 are primitive declarations
(opaque Types + accessors); the rest are invariants.

Implemented reductions:
- Owner → constructive subtype def (`Owner`, `owner_principal`, `owner_org`
  defs); ES-1 `owner_org_denormalized` PROVED. −4 axioms.
  *(Superseded 2026-06-11 morning review: org demoted to engine entirely,
  `Owner := Principal`, `OrgId`/`group_org` removed, ES-1 dissolves —
  130 axioms; decision `2026-06-11-org-out-of-kernel.md`.)*
- Edges: descriptor masks are now the primitive layer; `edge_class_legal`
  (ME-11) and `edge_layer_rule` (ME-10) PROVED from them.
- Memory supersession: `supersession_pointer_is_edge` bridge axiom (doc 02
  verbatim: the pointer IS the core/supersedes edge); ME-4, ME-5a, ME-5b
  PROVED from the matrix + edge scope. Net −2.
- Operators: CN-1..4 merged into `operatorEdgeShape` def + ONE axiom; CN-1/2
  full shapes (target kinds) PROVED via `provenance_pins_target`. CN-6 merged
  to `derived_has_provenance`; per-kind shapes PROVED. Net −5.
- Citations: Fact-side pointer `memory_citation` is now a choice-based DEF;
  primitives are `citation_fact_is_fact` + `citation_fact_injective`;
  CI-1 (citation ⇒ Fact; `fact_has_citation` retired 2026-06-13 — citations
  optional on Facts), CI-2a, CI-2c PROVED.
- Goals: GO-1+GO-2 merged into `goal_supersession_constraints`; projections
  PROVED. −1.
- Compliance: `suppression_owner` REMOVED (doc 13 retains the opaque key
  only — entry carries no Owner; ES-4 makes key-matching scope-matching);
  erasure Edge conjunct PROVED (`erasure_removes_edges`). −2.
- Composition: registry restructured around the composition law
  (`registry_composition` + `flavor_schemas`/`flavor_relations` +
  `contributions_namespaced`); `core_always_present`,
  `registry_namespace_discipline`, `registry_determined` (pointwise) PROVED.
  Six registration axioms merged into `entities_use_registered_vocabulary`.
  Net −5, and the over-strong cross-binary determination axiom is gone.

Over-strength corrections (doc-fidelity, flagged in decisions/):
- `batch_unique_within_source_owner` REMOVED (global injectivity exceeded the
  docs' scoped uniqueness); `ftoa_batch_exclusive` owner-conditioned →
  `decisions/2026-06-11-batch-id-scope.md`.
- `read_scope_diagonal` scoped to the Owner's own instances.
- `goal_parents_same_owner` KEPT, re-cited to doc 04 §Isolation →
  `decisions/2026-06-11-goal-parents-owner-scope.md`.

Additions forced by review (both reviewers + AGENTS.md invariant 17):
- `EdgeId` is now the ContentHash/UUIDv7 SUM with `edge_id_authorship_split`
  (Edges.lean) — supersedes the r1 ST-EdgeId exclusion row.

Rejected by adversarial verify: 2 proposals (one stale, one inverse of an
already-landed r1 fix).
