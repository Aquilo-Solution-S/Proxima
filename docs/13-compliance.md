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
| `delete_owner` | current Host API for erase; transport RPC deferred | one abandoned group `Owner` or verified dropped personal `Owner` | remove owner-scoped memories, goals, wake configs, edges, sidecars, cited blobs and their upload records, embeddings, source-batch payloads, invocation caches, and delegated-authority grants only after abandonment/drop proof; group erase deletes grants for that exact owner; personal erase also deletes cross-owner grants issued by the dropped subject; live owners refuse; retain suppression and compliance-audit rows |
| `delete_source_scope` | current Host API for erase; transport RPC deferred | one source object inside one abandoned/dropped `Owner` | erase rows attributable to the scope only under the same abandonment/drop proof as owner erase; delegated-authority grants are owner-level and remain; live owners refuse; flavor resolves scope, substrate executes compliance deletion |
| `pause_owner` | v1 intent | one `Owner` | stop future operator dispatch and wake execution; reads and export remain available |
| `resume_owner` | v1 intent | one `Owner` | clear pause state for future dispatch |
| `export_owner` | current Host API for export; transport RPC deferred | one personal/group `Owner` | deterministic owner-scoped bundle: memories, goals, edges, fact entities, receipts, source batches, citations, cited objects/blob refs, source cursors, delegated-authority grants, registered sidecars, and matching compliance audit rows |
| per-memory cascade delete | deferred | one Memory and derived closure | requires partial-graph repair and invocation-cache invalidation |
| tool-recipient export from calls | deferred | external-effect calls | waits for per-call recipient storage (see [12 §Compliance Metadata](12-tool-manifest.md#compliance-metadata)) |
| legal-consequence runtime blocking | deferred | tool invocation | `legal_consequence` remains design intent; human approval remains required pattern (see [05 §Human approval](05-actions.md#human-approval), [12 §Compliance Metadata](12-tool-manifest.md#compliance-metadata)) |

Every owner is personal or group, so every row sits inside some owner's erase
reach. An owner-to-owner transfer moves that reach rather than escaping it:
the destination owner can erase the transferred rows and the source owner no
longer can. Consent for that handover is admin on both sides of the transfer,
including group-manage on the destination — the receiving operators accept the
erase responsibility by holding it. See [Consumer Projector
Guidance](reference/public-api.md#consumer-projector-guidance).

Erase row inventory — rows the `erase_*` family destroys that the operation
tables above do not name column-by-column:

| Rows | Contract |
|---|---|
| `wake_config` | owner-authored `prompt` / `hard_memory_t` / `tool_ids`; nothing else collects it (`goal.wake_id` is `ON DELETE RESTRICT` and erase never deletes the `owners` row). Owner erase destroys every wake row of the owner. Source-scope erase destroys none because the rows carry no source attribution. Counted as `wake_configs_count`. |
| external objects | each exact `cooled.object_key` or erased `blob_uploads.object_key` is marked in `cold_purge_pending` inside the erase transaction and destroyed only after commit. Compliance rows carry `compliance_operation_id`; standalone `erase_memory` debts do not. `cold_object_purge_pending` remains true until every attributed key is reconciled. |
| `blob`, `blob_uploads`, registered citation sidecars | content hashes, and upload `bucket` / `object_key` / `filename` / `mime` / `sha256` / `etag` / `error_message`, plus whatever a flavor's citation payload carries. Deleted in FK order after the memory deletions: citation sidecar rows (keyed on `blob_id`, both families), then `blob_uploads`, then `blob`. Counted as `blobs_count`, `blob_uploads_count`, and `sidecar_rows_count` (all four sidecar families). |
| blobs under source scope | candidates are the selected hot/cooled admissions' `blob_id` values captured before deletion; any candidate still cited by a surviving hot or cooled admission remains. Exact upload object keys for deleted candidates enter `cold_purge_pending`; NULL-`blob_id` uploads remain owner-level. |

Current export bundle:

| Section | Rows |
|---|---|
| substrate | `memory`, `goal`, `ingest_keys`, `source_cursors`, `cooled`, `sketch` |
| cooled | row metadata only, including the `object_key` that locates the dumped payload — a manifest, not the bytes; the bundle is a database export and hydration recovers the payload |
| sketch | derived one-liners minus the generated `search_tsv` lexical index |
| delegated authority | exact-owner `delegated_authority_grants` only; explicit stable JSON field allowlist excludes redeemable `delegation_id` and credentials; a personal export does not pull group-owned grants merely because the same subject issued them |
| blobs | exact-owner `proxima_core.blob` rows with stable `blob_id`, `schema_id`, and `content_hash` allowlist; covers opaque CitedObject schemas without sidecars; excludes `blob_uploads`, storage coordinates, and object bytes |
| sidecars | registered memory/goal sidecar rows joined on the entity id; registered cited-object and citation-mapping sidecar rows joined on `blob_id` and owner-filtered by `blob.owner_id` |
| blob refs | `memory.blob_id` resolves through the exported authoritative blob row; typed citation content also lives in registered cited-object sidecars, while opaque CitedObject schemas require no sidecar; object bytes remain external |
| audit | matching `compliance_audit_log` rows by owner digest |
| excluded | persona/self rows, caller-supplied auth path, caller-supplied audit context, `announce` (the bundle's `source_batches` section is present but reads no rows) |
| gate | system auth path or `ComplianceAdminPort::may_perform_compliance_export` |
| legal hold | no effect on export; hold blocks physical destruction only |

## Outcomes

| Outcome | Meaning | Contract |
|---|---|---|
| `completed` | database operation applied | receipt records ids, timestamps, scope, requester, counts; `cold_object_purge_pending` and `cited_object_purge_pending` independently report outstanding external destruction |
| `refused` | lawful/retention hold blocks operation | receipt records refusal class and controller citation |
| `not-found` | scoped owner/source absent | no mutation; auditable response |
| `unauthorized` | requester lacks admin right | no mutation; auditable response |

Refusal is a valid compliance result, not a substrate failure.

## Legal/security hold

| Field | Contract |
|---|---|
| scope | one `OwnerRef` |
| active state | present owner hold row; set/clear require compliance-erase operator approval plus owner `Admin` write authority; get requires owner `Admin` |
| gated paths | substantive owner-memory selection for physical destruction: the `erase_*` compliance family (`delete_owner`, `delete_source_scope`) and the owner-data `maintain-retention` actions (`announce` pruning; Fact forget/cool) |
| refusal | typed `ComplianceEraseRefusal::LegalHoldActive`; no destructive statement runs |
| non-effects | no change to abandonment law, drop proof, reads, ordinary writes, embedding work-queue consumption, suppression checks, export, or audit retention |
| race boundary | checked inside the storage compliance-erase transaction under the owner legal-hold lock before deletion |

Forward rule: any future physical-destruction path must inherit the
same in-transaction owner hold gate before it can exist.
The `maintain-retention` pass inherits it: every per-owner transaction
takes the owner hold lock, re-checks the hold, and skips held owners —
for `announce` pruning (destruction) and, conservatively, for the
Fact forget/cool pass as well (a hold means "freeze this owner's state").
Exception: transient work-queue rows (`proxima_core.embedding_jobs`) are
consumed by ordinary embedding-pipeline operation; legal holds do not
suspend that pipeline.
`--retry-cold-object-purges` selects no owner data: it completes exact-key
debts already committed by an erase that passed the hold gate.

Operator rule: the controller/operator owns the legal judgment
(litigation hold, GDPR erasure duty, regulator instruction). Proxima
guarantees only the mechanical suspension of physical destruction.

## Retention enforcement — `maintain-retention` pass

The owner Fact-retention window (`owner_fact_retention.retention_seconds`,
surfaced as `fact_retention_seconds` on `proxima://graph`) and
`announce` growth are enforced by an operator-scheduled CLI pass
(`proxima-mcp maintain-retention`), not by an in-process scheduler — the
same external-clock doctrine as `maintain-embeddings`.

| Rule | Contract |
|---|---|
| enforcement action | expired Facts are forgotten (cool), never hard-deleted except erase; physical destruction stays exclusive to the `erase_*` family |
| cold store | a live pass requires the serving host's `PROXIMA_S3_*` block; enforcement fails before database access when it is absent or invalid; dry run needs no object store |
| scope | Fact rows of owners with a configured window; owners without a window are untouched; Abstractions/Perspectives derived from expired Facts persist |
| audit exclusion | `core/mcp-call-logged-v1` Facts are never aged out — indefinite controller evidence (see Audit log) |
| change feed | each forget batch commits its `announce.forget` events atomically with the cooling transaction |
| `announce` pruning | rows older than an explicit operator-supplied age horizon are deleted per owner; there is deliberately no default horizon — destruction requires an explicit flag |
| purge retry | `--retry-cold-object-purges` retries at most `--batch-size` durable exact-key debts; each S3 deletion runs without an open database transaction, then a short transaction clears the debt and its audit flag after the operation's last key |
| legal hold | both halves take the per-owner hold lock in every transaction and skip held owners (forward-rule inheritance above) |
| serialization | one pass at a time via a process-global advisory lock; an overlapping cron fire prints a skip notice and exits 0 |
| cursor safety | pruning creates an undetectable gap for forward pollers whose `since` cursor predates the horizon; choose a horizon comfortably larger than the slowest consumer's lag, or have lagging consumers re-baseline with a fresh full read |
| dry run | `--dry-run` reports would-be forgotten/pruned/purge-retry counts without mutating anything or requiring S3 |

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
| counts | affected-row counts only, including `delegated_authority_grants_count`, `wake_configs_count`, `blobs_count`, `blob_uploads_count` and `sidecar_rows_count` for owner erasure |
| external purge state | independent `cold_object_purge_pending` (exact queued keys) and `cited_object_purge_pending` (owner-prefix purge); either may remain true after database erasure completes |
| refusal | structured reason and retention/legal citation |
| forbidden content | deleted payloads, payload diffs, natural-person identifiers, decision trees |
| visibility | admin protocol only; not queryable by operators |
| retention | indefinite controller evidence |

Audit survives `delete_owner` for the same Owner.

Expired and revoked delegation grants remain durable audit evidence until an
owner erase selects them. Source-scope erase never selects a grant.

Owner remains the storage and graph isolation primitive. Access is server-resolved `OwnerRoles` over concrete `OwnerRef`s; Core enforces those roles at verb/tool entry and never adds org/share-set semantics. Pins live on the Memory admission (`origins[]` / `refs[]`). Compliance export/delete redacts unreadable pin targets independently of the source admission.

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
| retention override | absent | absent inherits source retention policy; a configured `owner_fact_retention` window is enforced by the `maintain-retention` forget/cool pass, which leaves cold stubs rather than a tombstone flag |
| legal/security hold | absent | active row suspends substantive owner-memory physical destruction for the owner-scoped `erase_*` family and the `maintain-retention` pass; transient `proxima_core.embedding_jobs` may still be consumed |
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
