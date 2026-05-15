# 09 — Frontend & Client Model

Current contract for the implemented frontend. 14 owns the protocol
semantics; 09 owns the client transport, state, rendering, and bundle
composition facts.

No protocol extension, runtime registration path, or offline replica is
defined here.

<a id="stack"></a>
## Stack

| Layer | Current choice | Boundary |
|---|---|---|
| Shell | Tauri 2 + Solid | `apps/proxima-shell` product composition and desktop host. |
| Engine access | Tauri IPC over embedded `Arc<Engine>` | JS calls generated Tauri command bindings; Rust handlers call engine verbs in-process. |
| Command bindings | `packages/frontend-core/src/bindings.ts` | Generated from the Tauri/Specta Rust command surface; do not edit by hand. |
| Core frontend package | `@proxima/core` | Shell primitives, hub, graph/filter stores, Tauri client, substrate views. |
| Flavor frontend packages | `@proxima/flavor-code`, `@proxima/flavor-goal`, `@proxima/flavor-mcp` | Payload codecs/renderers/editors/styles/views registered at Shell startup. |
| Payload bytes | CBOR | Decoded by registered flavor codecs; unknown payloads surface metadata/decode fallback. |
| State | In-memory `createGraphStore()` | Built from `Schema` + `Query` + `EventHistory` + `Subscribe`; no durable frontend replica. |

Locked implementation choice: Solid + Tauri 2 for the Shell.

<a id="transport"></a>
## Transport

The client-facing protocol surface is the six verbs in
[14](14-protocol-surface.md#the-six-verbs):

| Verb | Current Shell path |
|---|---|
| `Query` | `commands.query(req)` |
| `Subscribe` | `commands.subscribe(req, Channel<ChangeEvent>)` |
| `EventHistory` | `commands.eventHistory(req)` |
| `GoalWrite` | `commands.goalWrite(draft)` |
| `EventIngest` | `commands.eventIngest(draft)` |
| `Schema` | `commands.schema()` |

Desktop Shell path:

1. `apps/proxima-shell/src/App.tsx` builds `TauriEngineClient`.
2. `packages/frontend-core/src/tauri-client.ts` wraps generated
   Tauri commands behind `EngineClient`.
3. `apps/proxima-shell/src-tauri/src/commands/engine.rs` receives
   `State<'_, Arc<Engine>>`.
4. Commands call the matching engine method with `Credentials::None`.
5. `Subscribe` streams identity-only `ChangeEvent`s through
   `tauri::ipc::Channel`.

The Rust engine stays in-process with the Shell. The JS side does not
talk to the MCP listener and does not route through a loopback protocol
adapter on the desktop path.

Wire deployments remain a separate fulfillment of the same 14 contract:
`proto/proxima/v1/engine.proto` and `crates/wire-grpc` expose the
engine service over gRPC for headless/remote clients. The Shell frontend
does not currently consume that path.

Envelope rows cross the JS boundary as generated Tauri/Specta types.
Typed payload fields remain opaque bytes until a flavor codec decodes
them.

<a id="schema-driven-ui-codegen"></a>
## Schema-Driven UI + Codegen

Source of truth:

| Surface | Owner |
|---|---|
| Payload schema identity | Flavor Rust crates, registered at build time per [03](03-schema-registry.md) and [08](08-core-and-flavors.md). |
| Engine command/envelope types | Rust command signatures + Tauri/Specta bindings. |
| Runtime schema list | `Schema` verb response (`SchemaInfo`, filters, relations). |
| Payload decoding/rendering | Flavor frontend package registry calls. |

Current generated artifact:

```
packages/frontend-core/src/bindings.ts
```

Current manual flavor frontend artifact:

```
flavors/<name>/frontend/src/index.ts
```

Payload decode path:

1. `Query` returns row metadata plus optional CBOR payload bytes.
2. `GraphStore` asks `hub.codecFor(schema_id, schema_version)`.
3. Registered codec decodes bytes to a typed JS value.
4. Missing codec records `DecodeError { kind: "missing_codec" }`.
5. Decode failure records `DecodeError { kind: "decode_failed" }`.
6. Views render metadata, byte length, and fallback payload/error text
   when no renderer/codec is available.

No current contract:

- generated per-flavor TypeScript payload bindings;
- generated JSON-Schema renderer as the generic fallback;
- payload `.proto` files mirroring Rust sidecars.

<a id="local-first-replica-offline-queue"></a>
## Local-First Replica + Offline Queue

Deferred. Current frontend state is memory-only.

`createGraphStore(client, hub, owner)` owns the visible graph snapshot:

| Input | Use |
|---|---|
| `schema()` | Populate schema metadata for filters, views, and stateful natural-key handling. |
| `query(snapshotReq(owner))` | Cold snapshot of memories, goals, and edges. |
| `eventHistory({ owner, limit, before: null })` | Recent event rail seed and memory provenance seed. |
| `subscribe({ owner, since })` | Live identity-only append stream. |
| follow-up `query(... include_payloads: true ...)` | Hydrate entities referenced by live events. |

`Subscribe` events are identity-only. The store marks pending
hydration for appended memory/goal/edge IDs, batches follow-up
`Query` calls, dedupes events by `seq`, and marks the stream
`degraded` on seq regression or repeated hydration failure.

No current frontend durable cache, write queue, local vector index, or
offline replay contract.

<a id="ui-bundle-composition"></a>
## UI Bundle Composition

The Shell bundle is composed at build/startup from one substrate package
plus flavor frontend packages.

| Package | Responsibility |
|---|---|
| `@proxima/core` | `Shell`, `createHub`, `createGraphStore`, `createGraphFilterStore`, Tauri client, primitives, substrate views. |
| `apps/proxima-shell` | Product app, substrate view list, settings panels, Tauri host, flavor init call. |
| `@proxima/flavor-code` | Code payload codecs/renderers, code relation styles, code shell view, styles. |
| `@proxima/flavor-goal` | Goal proposal renderers/editors and goal relation styles. |
| `@proxima/flavor-mcp` | MCP substrate renderers and relation styles. |

Startup path:

```
apps/proxima-shell/src/flavors.ts
  initCode()
  initGoal()
  initMcp()

apps/proxima-shell/src/App.tsx
  initFlavors()
  createHub(substrateViews, substrateSettingsPanels)
  createGraphStore(createTauriEngineClient(), hub)
```

`pnpm-workspace.yaml` includes `packages/*` and `flavors/*/frontend`.
The linked JS flavor packages must match the Rust flavor crates linked
into the product binary. This is the frontend analogue of 08 composite
discipline; not a runtime plugin system.

<a id="hub-architecture"></a>
## Hub Architecture

Core surfaces:

| Surface | File |
|---|---|
| `createHub()` | `packages/frontend-core/src/hub.ts` |
| Registry hooks | `packages/frontend-core/src/registry/index.ts` |
| Graph state | `packages/frontend-core/src/graph-store.tsx` |
| Filter state | `packages/frontend-core/src/graph-filter-store.tsx` |
| Tauri client | `packages/frontend-core/src/tauri-client.ts` |

Flavor registration hooks:

| Hook | Key |
|---|---|
| `registerPayloadRenderer` | `(kind?, schemaId, schemaVersion)` |
| `registerEdgeStyle` | `relationId` |
| `registerShellView` | `id` / `route` |
| `registerGoalPayloadEditor` | `(schemaId, schemaVersion)` |
| `registerPersonalityType` | `typeId` |

Legacy hub methods still exist for substrate-local registration
(`registerFlavor`, `registerCodec`, `registerRenderer`,
`registerView`, `registerSettingsPanel`), but current flavor packages
use the registry hooks above.

Substrate views:

| View | Contract |
|---|---|
| Surface | Memory lanes, goal rail, event rail, detail pane. |
| Atlas | Deterministic projection over current graph data; layer axis fixed by entity kind. |
| Schemas | Runtime schema/renderer visibility. |
| Flavors | Compiled-in flavor frontend metadata. |
| Personalities | Runtime personality instances and type metadata. |
| Settings | General, model, MCP panels owned by Shell/core. |

Renderer fallback:

1. Exact registered payload renderer by kind/schema/version.
2. Schema/version renderer without kind.
3. Substrate metadata/payload fallback.
4. Decode errors surfaced from `GraphStore.decodeErrorsByEntity`.

<a id="flavor-endpoints-deferred"></a>
## Flavor Endpoints (Deferred)

Current rule: flavor-specific frontend behavior fits inside the six 14
verbs by way of registered schemas, relations, filters, renderers,
codecs, editors, and views.

No current flavor-specific frontend transport endpoint is part of the
Shell contract. A future flavor service must be specified in the
owning flavor and mounted by a composite binary without adding runtime
registration.

<a id="multi-owner-ui"></a>
## Multi-Owner UI

Protocol stance: one Owner per read/write/subscribe call
([14](14-protocol-surface.md#owner-scoping--the-primary-axis)).

Current Shell state: `createGraphStore()` is constructed for one owner
at a time. Multi-owner switching, background streams per visible Owner,
and cross-owner notification badges are not implemented frontend
contracts yet.

Deferred UI contract:

| Concern | Rule |
|---|---|
| Active Owner | New `GoalWrite` / `EventIngest` uses selected Owner. |
| Owner switch | Recreate or partition graph/filter stores by Owner. |
| Background Owners | One `Subscribe` per visible Owner when implemented. |

<a id="mobile"></a>
## Mobile

Deferred. Current implementation target is the desktop Shell.

Mobile must preserve:

| Concern | Contract |
|---|---|
| Protocol | Same six 14 verbs. |
| Owner scoping | Same per-call Owner. |
| Payloads | Same CBOR payload bytes and flavor codecs. |
| Composition | Same flavor-owned UI/rendering boundary where possible. |

No current contract for platform notification services, background
tasks, capture shortcuts, mobile storage, or biometric unlock.

<a id="embedded-engine-mode-desktop"></a>
## Embedded Engine Mode (Desktop)

Current desktop shape:

| Item | Fact |
|---|---|
| Engine ownership | Shell builds an `Engine` and stores `Arc<Engine>` in Tauri state. |
| Storage | Postgres via `DATABASE_URL`; migrations run during Shell boot. |
| Auth | `NoAuth` resolver, command calls pass `Credentials::None`. |
| Commands | `#[tauri::command]` handlers expose engine operations. |
| Bindings | Tauri/Specta exports TypeScript command types to `@proxima/core`. |
| MCP | Shell also hosts MCP at `127.0.0.1:31415`; frontend does not use it as its client transport. |

Boot path:

```
apps/proxima-shell/src-tauri/src/boot.rs
  PgStorage::connect(DATABASE_URL)
  run core + flavor migrations
  build proxima-code engine
  register mcp-substrate + goal flavors
  host MCP listener
```

The embedded mode preserves the 14 contract at the command boundary.
Moving a future Shell to a remote engine is a client transport change,
not a renderer/state contract change.
