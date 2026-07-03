# P-R1 Multi-Owner MCP Serving

Status: accepted for v0.0.6 slice P-R1.

## Contract

| Surface | Rule |
|---|---|
| Serving path | one resolver-backed path only |
| Bearer identity | host `Authenticator` resolves a `UserId` |
| Owner authority | `OwnerAccessPort::resolve_roles_for_subject(UserId)` |
| Session owner | selected once at MCP `initialize`; HTTP transport uses `X-Proxima-Owner` |
| Session state | `Mcp-Session-Id -> OwnerRef` server binding |
| Tool calls | no owner argument; bound owner rechecked on every authenticated request |
| Palette | frozen registry, deployment `ToolScope`, then bound-owner role filter |
| Revocation | membership removal denies the next request |

Owner key wire forms:

```text
world
personal:<uuid>
group:<uuid>
```

## Boundaries

| Boundary | Rule |
|---|---|
| MCP serving | resolver-only; no process-pinned owner |
| Embedded host boot | may still carry a boot owner; host territory |
| Insecure single-owner | embedded/no-MCP only |
| Storage writes | engine-minted permits or backend-owned unit of work only |

## Rejected

| Alternative | Reason |
|---|---|
| `--owner-user` serving | fixed-owner serving path; second owner axis |
| `--master-token` serving | static local bearer asserts `UserId` outside host identity proof |
| dual `--serving-mode` | compatibility mode for retired surface |
| per-call owner parameter | caller-supplied owner authority |
| dynamic tool registry | violates frozen build-time registry |
| one process per owner | operational workaround; not an authorization contract |

## Checks

| Invariant | Check |
|---|---|
| no fixed-owner serving | no `--owner-user`, no `OidcAuthenticator::single_owner`, no `FixedOwner` |
| owner selection fail-closed | invalid owner header/session owner mismatch denied |
| per-request recheck | `multi_owner_e2e` revocation lane |
| no local static bearer | no `--master-token`, no `PROXIMA_MCP_MASTER_TOKEN`, stale `pxm_` credentials fail closed |
| docs surface | `MIGRATING.md`, `docs/10-configuration.md`, `docs/15-deployment.md`, env reference |

Cross-project: Aquilo #4095 / Proxima P-R1.
