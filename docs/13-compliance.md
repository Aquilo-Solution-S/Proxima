# 13 — Compliance Primitives

> **Status:** design intent plus current primitive inventory. Deferred enforcement rows are not implementation claims.

Architecture contract for substrate-level compliance operations.
Controller policy decides when to invoke them; the substrate defines
bounded primitives and metadata vocabulary.

## Contract boundary

| Area | Contract |
|---|---|
| cognitive lifecycle | append-only; Facts immutable; A/P/Goals supersede (see [02 §Re-derivation and supersession](02-memory.md#re-derivation-and-supersession)) |
| compliance lifecycle | out-of-band admin operation; may hard-delete substrate rows |
| scope | one `Owner` or one Owner-scoped source object |
| authorship | admin/controller principal; never operator-authored |
| operator visibility | diminished graph only; no compliance audit reads |
| protocol | surfaced through admin surface in [14](14-protocol-surface.md); not a Memory mutation |

`v1 intent` below means design contract, not implemented-code claim.

## Operations

| Operation | Status | Scope | Contract |
|---|---|---|---|
| `delete_owner` | current Host API for erase; transport RPC deferred | one abandoned group `Owner`, verified dropped personal `Owner`, or refused World owner | remove owner-scoped memories, goals, edges, sidecars, embeddings, source-batch payloads, and invocation caches only after abandonment/drop proof; live owners refuse; retain suppression and audit rows |
| `delete_source_scope` | current Host API for erase; transport RPC deferred | one source object inside one abandoned/dropped `Owner` | erase rows attributable to the scope only under the same abandonment/drop proof as owner erase; live owners refuse; flavor resolves scope, substrate executes compliance deletion |
| `pause_owner` | v1 intent | one `Owner` | stop future operator dispatch and wake execution; reads and export remain available |
| `resume_owner` | v1 intent | one `Owner` | clear pause state for future dispatch |
| `export_owner` | current Host API for export; transport RPC deferred | one personal/group `Owner` | deterministic owner-scoped bundle: memories, goals, edges, fact entities, receipts, source batches, citations, cited objects/blob refs, source cursors, registered sidecars, and matching compliance audit rows |
| per-memory cascade delete | deferred | one Memory and derived closure | requires partial-graph repair and invocation-cache invalidation |
| tool-recipient export from calls | deferred | external-effect calls | waits for per-call recipient storage (see [12 §Compliance Metadata](12-tool-manifest.md#compliance-metadata)) |
| legal-consequence runtime blocking | deferred | tool invocation | `legal_consequence` remains design intent; human approval remains required pattern (see [05 §Human approval](05-actions.md#human-approval), [12 §Compliance Metadata](12-tool-manifest.md#compliance-metadata)) |

The World-owner refusal above has a publish-side consequence — rows published to World permanently leave personal/group erase reach; see [Consumer Projector Guidance](reference/public-api.md#consumer-projector-guidance).

Current export bundle:

| Section | Rows |
|---|---|
| substrate | `memories`, `goals`, `edges`, `fact_entities`, `fact_receipts`, `source_batches`, `citation_mappings`, `cited_objects`, `source_cursors` |
| sidecars | registered memory/goal/edge/citation/cited-object sidecar rows for the target owner |
| blob refs | `cited_uploaded_blob_v1` / other registered cited-object sidecars; object bytes remain external |
| audit | matching `compliance_audit_log` rows by owner digest |
| excluded | persona/self rows, caller-supplied auth path, caller-supplied audit context |
| gate | system auth path or `ComplianceAdminPort::may_perform_compliance_export` |
| legal hold | no effect on export; hold blocks physical destruction only |

## Outcomes

| Outcome | Meaning | Contract |
|---|---|---|
| `completed` | operation applied | receipt records ids, timestamps, scope, requester, counts; never deleted payloads |
| `refused` | lawful/retention hold blocks operation | receipt records refusal class and controller citation |
| `not-found` | scoped owner/source absent | no mutation; auditable response |
| `unauthorized` | requester lacks admin right | no mutation; auditable response |

Refusal is a valid compliance result, not a substrate failure.

## Legal/security hold

| Field | Contract |
|---|---|
| scope | one `OwnerRef` |
| active state | present owner hold row; set/clear require compliance-erase operator approval plus owner `Admin` write authority; get requires owner `Admin` |
| gated paths | substantive owner-memory physical destruction: current `erase_*` compliance family (`delete_owner`, `delete_source_scope`) |
| refusal | typed `ComplianceEraseRefusal::LegalHoldActive`; no destructive statement runs |
| non-effects | no change to abandonment law, drop proof, reads, ordinary writes, embedding work-queue consumption, suppression checks, export, or audit retention |
| race boundary | checked inside the storage compliance-erase transaction under the owner legal-hold lock before deletion |

Forward rule: any future physical-destruction path must inherit the
same in-transaction owner hold gate before it can exist.
Exception: transient work-queue rows (`proxima_core.embedding_jobs`) are
consumed by ordinary embedding-pipeline operation; legal holds do not
suspend that pipeline.

Operator rule: the controller/operator owns the legal judgment
(litigation hold, GDPR erasure duty, regulator instruction). Proxima
guarantees only the mechanical suspension of physical destruction.

## Suppression list — re-ingest rejection

Hard deletion must not reopen ingest.

| Rule | Contract |
|---|---|
| retained key | opaque idempotency key only |
| retained metadata | deletion timestamp and operation id |
| rejected path | source ingest checks suppression before dedup and rejects matching batches |
| rejection shape | no-op `Suppressed`; no retry pressure |
| retention | indefinite; deleting suppression would permit silent re-ingest |
| PII guard | idempotency keys must be content-derived or otherwise opaque, never natural identifiers (see [01 §Compliance metadata](01-event-source.md#compliance-metadata)) |

## Audit log

| Field class | Contract |
|---|---|
| operation identity | uuidv7 operation id, owner/scope, requester |
| timing | requested/completed timestamps |
| outcome | `completed`, `refused`, `not-found`, `unauthorized` |
| owner roles | group membership/role administration and authorization denials are audit-worthy controller events; personal-memory MCP calls should be logged metadata-only or redacted by host/admin policy, not copied into a shared audit payload |
| counts | affected-row counts only |
| refusal | structured reason and retention/legal citation |
| forbidden content | deleted payloads, payload diffs, natural-person identifiers, decision trees |
| visibility | admin protocol only; not queryable by operators |
| retention | indefinite controller evidence |

Audit survives `delete_owner` for the same Owner.

Owner remains the storage and graph isolation primitive. Access is server-resolved `OwnerRoles` over concrete `OwnerRef`s; Core enforces those roles at verb/tool entry and never adds org/share-set semantics. Edge rows are source-owned; descriptor policy and target gates control cross-owner target admission. Compliance export/delete redacts or omits unreadable targets independently from source-readable edge rows.

## External side effects

Compliance operations are substrate-local.

| External state | Contract |
|---|---|
| already-sent email / message / PR / transfer / notice | not rolled back by substrate deletion |
| downstream cleanup | controller/Ops obligation |
| recipient notification inventory | deferred until per-call recipients exist (see [12](12-tool-manifest.md#compliance-metadata)) |
| legally significant tool calls | require human approval flow; automatic blocking deferred |
| embedding provider egress | Fact/derived text sent to the configured embedding endpoint is disclosure to an external processor — document it as an AVV/DPA recipient. Non-loopback plaintext HTTP is rejected. |
| uploaded cited blobs (S3) | owner erasure removes the owner's canonical objects (`objects/<owner_hash>/…`) in-band; abandoned `pending/` uploads are reclaimed by the required S3 lifecycle rule (see [15 §Blob storage lifecycle](15-deployment.md#blob-storage-lifecycle)). |

## Compliance vocabulary

Shared metadata vocabulary used by sources, schemas, tools, and
owner policy.

| Vocabulary | Values / shape | Used by |
|---|---|---|
| lawful basis | `not-applicable`, `consent`, `contract`, `legitimate-interest`, `legal-obligation`, `vital-interest`, `public-task` | source metadata |
| retention policy | `indefinite(reason)`, `retain-for(duration)` | source metadata, owner override |
| region | `eu`, `uk`, `us`, `ch`, `br`, `in`, `cn`, `unrestricted`, extensible deployment values | source/tool residency checks |
| recipient id | opaque controller-defined string | tool metadata, future per-call export |
| special-category flag | boolean per payload schema | schema metadata; heightened audit/reporting |

Trivial values make compliance enforcement a no-op for deployments
that do not need regime-specific behavior.

## Required metadata

| Metadata | Home | Contract |
|---|---|---|
| `lawful_basis` | [01](01-event-source.md#compliance-metadata) | per-source processing basis |
| `collection_purpose` | [01](01-event-source.md#compliance-metadata) | controller-authored purpose |
| `retention_policy` | [01](01-event-source.md#compliance-metadata) | source default; owner override allowed |
| `data_residency` | [01](01-event-source.md#compliance-metadata), [12](12-tool-manifest.md#compliance-metadata) | source payload region; future tool-call region check |
| `special_category` | [03](03-schema-registry.md#special-category-declaration) | per-schema heightened-protection marker |
| `recipients` | [12](12-tool-manifest.md#compliance-metadata) | deferred external-recipient inventory |
| `legal_consequence` | [12](12-tool-manifest.md#compliance-metadata) | deferred automatic wake/tool blocking |

## Owner policy

Per-Owner runtime overlay. Enforcement is a design primitive unless a
code path explicitly implements it.

| Policy field | Default | Contract |
|---|---|---|
| pause flag | `false` | paused owners skip future operator dispatch and wake execution |
| residency allowlist | empty | empty means unrestricted; non-empty constrains future residency checks |
| retention override | absent | absent inherits source retention policy |
| legal/security hold | absent | active row suspends substantive owner-memory physical destruction for the owner-scoped `erase_*` family only; transient `proxima_core.embedding_jobs` may still be consumed |
| consent state | empty opaque value | controller-managed; substrate stores, controller interprets |
| legal-consequence override | `false` | future override for automated legal-consequence blocking |

Policy updates are admin-only except legal-hold set/clear, which require
compliance-erase operator authority. Policy state is audited and not visible
to operators; owner Admins may read legal-hold state.
Fresh Owners with no policy row use the all-permissive defaults.

## Deferred enforcement

| Enforcement path | Status | Reason |
|---|---|---|
| tool descriptor compliance fields | deferred in [12](12-tool-manifest.md#compliance-metadata) | current tool descriptors do not carry the fields |
| startup failure for missing tool fields | deferred in [12](12-tool-manifest.md#compliance-metadata) | depends on descriptor placement |
| owner residency allowlist for tool calls | deferred in [12](12-tool-manifest.md#compliance-metadata) | depends on tool `data_residency` |
| recipient export from tool-call records | deferred in [12](12-tool-manifest.md#compliance-metadata) | no per-tool invocation recipient table |
| automatic `legal_consequence` blocking | deferred in [12](12-tool-manifest.md#compliance-metadata) | human-approval Fact flow remains required |

## Legal mappings

| Primitive | GDPR-style mapping |
|---|---|
| `delete_owner`, `delete_source_scope` | erasure / deletion request |
| `pause_owner` | restriction of processing |
| `export_owner` | access and portability |
| suppression list | accountability and re-ingest prevention |
| recipient id | future recipient notification support |
| special-category flag | heightened-protection handling |

Mappings explain primitive purpose only. DPIA, breach notification,
DPO appointment, SCCs, privacy-policy text, consent UX, and backup
operations remain controller/Ops concerns.

## Anchors

- `contract-boundary`
- `operations`
- `outcomes`
- `suppression-list--re-ingest-rejection`
- `audit-log`
- `external-side-effects`
- `compliance-vocabulary`
- `required-metadata`
- `owner-policy`
- `deferred-enforcement`
- `legal-mappings`
