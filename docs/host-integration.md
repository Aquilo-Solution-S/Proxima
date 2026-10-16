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
        builder.mcp_bind(SocketAddr::from(([127, 0, 0, 1], 31415)))
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

## Authentication and owner scope

Provide an `Authenticator` through `RuntimeBuilder::authenticator`, or use the
OIDC adapter from `auth-oidc`. The authenticator validates the bearer, maps
`(iss, sub)` to a `UserId`, and produces owner access for the request. The
`X-Proxima-Owner` value is a raw owner UUID selected from that authorized
scope. It is not an authorization proof.

An embedded host may install an `OwnerAccessPort` and an optional forwarder
policy. Reads are set-based and owner-scoped; writes pass through the engine's
authorization and command ports. Do not recreate owner predicates in a flavor
or add a caller-controlled ACL/share set.

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

For a store accessed by flavor workers, register an `OtherStores` port with
`RuntimeBuilder::other_stores`; the facade wraps it as a typed service with the
configured timeout. The extra-store transaction is separate from a Proxima
`UnitOfWork`; use host-state participation when the change must be atomic with
a Fact write. A flavor never queries `proxima_core` directly.

## Fact publication outbox

Implement `DurableIntake` in the host and register it with
`RuntimeBuilder::outbox_intake`, or register named consumers with
`outbox_consumer`. A captured listenable Fact is committed to the outbox in
the write transaction. A consumer receives it at least once; deduplicate on
the event id. `Ok(Accepted)` and `Ok(Rejected)` mark a record published, while
an intake error releases it for retry. No consumer means Facts are still
captured until the configured pending bound is reached.

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
