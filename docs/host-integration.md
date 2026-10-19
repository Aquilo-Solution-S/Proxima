# Host integration
> Status: current.

An embedded host composes Proxima with its own authentication, HTTP router,
extra stores, publication intake, and lifecycle policy. The `proxima` facade
is the host boundary. A flavor receives typed services; it does not receive
raw core tables or resolve owners itself. See [concepts](concepts.md) for the
boundary and [configuration](configuration.md) for the settings inventory.

## Boot a composed host

Implement `FlavorApp` for the application bundle. Defaults are applied first;
environment and explicit builder calls then overlay them.

```rust
use proxima::{AppInfo, FlavorApp, RuntimeBuilder};
use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use std::net::SocketAddr;

struct MyApp;

impl FlavorBundle for MyApp {
    fn register(_: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> { Ok(()) }
    fn migrators() -> Vec<NamedMigrator> { Vec::new() }
}

impl FlavorApp for MyApp {
    fn app_info() -> AppInfo {
        AppInfo { id: "my-app", title: "My App", version: env!("CARGO_PKG_VERSION") }
    }

    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder.store_number(3).mcp_bind(SocketAddr::from(([127, 0, 0, 1], 31415)))
    }
}

#[tokio::main]
async fn main() -> Result<(), proxima::ProximaError> {
    proxima::run::<MyApp>().await?;
    Ok(())
}
```

The host normally implements `FlavorBundle` and links one or more flavors;
the tuple `FlavorApp` implementation folds configuration, services, and HTTP
mounts left to right. `build()` gives a booted runtime for embedding; `run()`
also starts configured listeners and background workers. Authenticated MCP
requests carry a server-resolved `AuthzContext`; callers never supply a
resolved owner or role.

With an object store configured, the runtime sweeps expired uploads and drains
object-purge debt itself (`RuntimeBuilder::object_purge_interval`, default 60
seconds, zero turns it off); a host built on the runtime writes no code for it.
A library host that composes `PgStorage` and `CitedBlobStore` without the
runtime calls `proxima::object_maintenance_pass` on a cadence no longer than
the upload TTL. The pass takes the same storage maintenance lock as the
runtime and `maintain-storage`, and skips when another pass holds it. Both
the runtime's boot and the pass refuse while the purge queue holds debt for
another object store; see [operate](operate.md).

Migration ledgers are checked before any schema change. A recorded version
must be shipped by a composed source using that same ledger; sources sharing
a ledger are checked as one set. Separate ledgers are never populated from
another ledger.
Databases with old copies recorded on both shared and separate ledgers also
require a reset; there is no copy-cutover compatibility path.

A host that deliberately shares a database between releases can explicitly set
`RuntimeBuilder::missing_migration_policy(MissingMigrationPolicy::Allow)`.
The deployable host and its maintenance commands read the same choice from
`PROXIMA_MISSING_MIGRATION_POLICY`, as does `tools/dev-migrate`. The same
policy applies when boot skips migrations. Standalone migration and
planning callers use the corresponding `_with_policy` functions; the ordinary
functions reject missing versions. Allowing missing versions still rejects
dirty ledgers, changed checksums, and migration lanes that would replay or
reorder previously applied work.

Read-only boot and planning share the runner's migration lock so they cannot
validate between source commits. They fail after a five-second lock wait.
Complete the out-of-band migration job before starting pods that skip
migrations; allowing missing versions does not bypass this serialization.

## Authentication and owner scope

Provide an `Authenticator` through `RuntimeBuilder::authenticator`, or use the
OIDC adapter from `auth-oidc`. The authenticator validates the bearer, maps
`(iss, sub)` to a `UserId`, and produces owner access for the request. The
`X-Proxima-Owner` value is a raw owner UUID selected from that authorized
scope. It is not an authorization proof.

The scope gate judges the caller's role on the owner the call's `space`
resolves to.

Tool admission combines two distinct policies. `ServedSurface` configures a
deployment through `RuntimeBuilder::served_surface`; every declared read-only
tool/action and core resource remains served. `ToolScope` is the authenticated
caller's exact key set, including reads and resources. `CapabilitySet` applies
both policies before the independent owner-role gate. Neither policy supplies
owner authority.

A dispatcher requires its declared action key, such as `core_upload:read_url`.
A resource requires its template or static key, such as `resource:memory`;
`resource:how-to` controls the guide too. A bare dispatcher does not grant its
actions. Listings and calls use the same actual declaration, including dynamic
host tools. Custom role shaping may narrow each policy without converting one
into the other. Replace deployment `.tool_scope(...)` calls with
`.served_surface(ServedSurface::...)` and `tool_palette_excluding` with
`served_surface_excluding`; retain `ToolScope` for caller scopes.

An embedded host may install an `OwnerAccessPort` and an optional forwarder
policy. Reads are set-based and owner-scoped; writes pass through the engine's
authorization and command ports. Do not recreate owner predicates in a flavor
or add a caller-controlled ACL/share set.

For subjects in `PROXIMA_FORWARDER_SUBJECTS`, the signed token must carry an
`owner` UUID claim, a `by` principal UUID and an explicitly present `via`
claim containing a session UUID or `null` for a direct principal call.
Missing or malformed writer claims refuse authentication. The forwarder
remains the authenticated subject; its
`PROXIMA_FORWARDER_ROLE` applies only to that registered personal or group
owner. An absent owner header uses the claim. A present `X-Proxima-Owner`
header and any live session must agree with it before the target is resolved.
An unknown owner grants no role. Ordinary subjects ignore the claim.

With the PostgreSQL registry, a personal owner is registered by its first
ordinary authenticated write. Complete that write before forwarding to a new
person, or supply an authoritative `OwnerRegistry` for provisioned owners.
Forwarding does not register an unknown owner.

Forwarding never unions ordinary memberships into the request, grants group
management, or permits delegation. `ForwarderRole` is the closed choice
`Viewer`, `Ingest`, or `Editor`; none writes Goals or manages groups.
`OwnerRegistry` probes both owner kinds. Custom authenticators use
`AuthzContext::from_verified_forwarded_owner` only after verifying the
credential. That pending context grants no roles until the edge resolves its
claim through the configured policy and registry. Host role shaping must
preserve the owner and writer bindings. Replace implementations of the retired `GroupRegistry`
with `OwnerRegistry::is_registered_owner` when upgrading.

Each memory or Goal revision records the writer captured in its sealed
`OwnerScope`, separately from its owner, credential subject, payload author
or grounding sources. A custom authenticator binds `WriterAttribution` with
`AuthzContext::with_attested_writer` before `proxima::authenticate` seals the
context. For a forwarder it supplies the verified pair to
`AuthzContext::from_verified_forwarded_owner(subject, owner, writer)`.
Ordinary OIDC calls record their mapped principal with `via: null`.
There are no author headers or caller-supplied author arguments. A host role
shaper must preserve the attested pair, and a sealed context cannot replace it.

Attaching a subgroup or changing its relation requires manage authority on
both groups. Detaching it requires manage authority on either endpoint;
removing a person requires manage authority on the parent group. PostgreSQL
write policies enforce these boundaries. Lock requests retain both endpoints
for tree serialization; their read and write policies require manage authority
on either endpoint.

## Other stores and HTTP

Use `FlavorApp::services` to expose a typed service, and
`FlavorApp::mount_http` to add host-owned routes. The host accessor provides a
pool and the resolved query policy:

```rust
fn services(ctx: &AppContext) -> Result<FlavorServices, FlavorServiceError> {
    let store = MyStore::new(ctx.host().clone_pool_for_host());
    Ok(FlavorServices::with(store))
}
```

Every memory handle is a bare UUID. The host assigns each store a unique
16-bit number among the stores it routes between; configure it with
`RuntimeBuilder::store_number` or `PROXIMA_STORE_NUMBER`. First boot persists
the number; a later mismatch refuses startup. UUIDv8 handles carry that number
and sort by timestamp and counter. Start fresh stores for this release.

For a store accessed by flavor workers, register an `OtherStores` port with
`RuntimeBuilder::other_stores`; the facade wraps it as a typed service with the
configured timeout. The extra-store transaction is separate from a Proxima
`UnitOfWork`; use host-state participation when the change must be atomic with
a Fact write. A flavor never queries `proxima_core` directly.

The store's vector index implements the `VectorIndex` port.
The PostgreSQL adapter writes complete chunk versions in the store transaction
and ranks each memory by its best chunk. Index hits are candidates: the store
reads their memory rows under owner RLS before using their IDs or scores.

Derivation reads all foreign origins within one call-wide deadline and refuses
before writing if they cannot all be read. Lineage deduplicates foreign origins
across the page, bounds distinct reads, and renders unread origins as unavailable
with a reason. The limits and concurrency contract live in
[`other_stores` rustdoc](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/storage_ports/other_stores.rs).

## Fact publication outbox

Implement `DurableIntake` in the host and register it with
`RuntimeBuilder::outbox_intake`, or register named consumers with
`outbox_consumer`. A captured listenable Fact is committed to the outbox in
the write transaction. A consumer receives it at least once; deduplicate on
the event id. `Ok(Accepted)` and `Ok(Rejected)` mark a record published, while
an intake error releases it for retry. No consumer means Facts are still
captured until the configured pending bound is reached.

Erase and lease takeover are checked twice: just before `accept`, and again
when the record is marked published, under the lane lock and in the marking
transaction. A record erased or reclaimed by another process before `accept`
is not handed over. If that happens while `accept` is running, your intake
already has the event: Proxima cannot take it back, and it does not record the
record as published. Treat that as at-least-once delivery.

```rust
let builder = RuntimeBuilder::default()
    .outbox_intake(std::sync::Arc::new(MyIntake::new()))
    .published_retention(std::time::Duration::from_secs(86_400));
```

Bind publication source, model provenance, and host-bound extensions once on
the authenticated context. The host owns consumer ledgers, retention, and
actor attribution. The typed `PublicationRecord`, `DurableIntake`, and delivery outcomes own the
CloudEvents envelope and retry contract in rustdoc. Scheduling, execution, and
call logging belong to the host; the outbox is its change channel.

The built-in HTTP sink reads `PROXIMA_OUTBOX_HTTP_URL` and delivers each event
to that receiver without following redirects. Plaintext HTTP requires loopback
or an exact host entry in `PROXIMA_PLAINTEXT_HOSTS`, whether or not a token is
configured. See [configuration](configuration.md) for the token-file and CA
settings.

## Host-state participant

Register one `PgHostStateParticipant` with
`RuntimeBuilder::host_state_participant`. Boot binds it to the actual
participant descriptor and declared tables. A `UnitOfWork::apply_host_state`
command carries one owner and a duplicate-free subset of those tables; the
private maintenance permit checks participant, table, and owner before the
handler runs. A dispatcher can route several typed command payloads without
creating another authority path.

This capability only maintains the host's registered state. It does not grant
Fact, Abstraction, Perspective, or Goal writes and does not bypass ordinary
owner authorization. The callback is transaction-local and must honor the
stamped owner and declared table scope. The participant and transaction-scoped capability types own this contract in
rustdoc; lifecycle behavior is verified by the facade and storage PG tests.

## Erase and export callbacks

Register lifecycle callbacks on the same boot-frozen participant when its
declared tables need owner, source, or exact-Fact maintenance. Core performs
authorization and acquires the lifecycle fence; the callback receives a typed
selection and returns receipts for the declared tables. Whole-owner, source,
flavor-scoped, and exact-physical-Fact scopes remain distinct. Export is a
read path under the caller's authorization; it does not grant erase authority.

An erase commits core inverse work and the participant callback atomically. A
connection loss after commit may leave the caller's observed result unknown;
retry using the operation's idempotency boundary. For hard deletion rules see
[data and erasure](data-and-erasure.md).

## Verify an embedded host

```sh
cargo check -p my-host
cargo test -p my-host
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
```

Start the host against a local database, inspect `tools/list` and the resource
catalog, and exercise one authorized read, one Fact write, one outbox intake,
and one lifecycle callback. Run the full workspace gates before publishing;
see [contributing](contributing.md).
