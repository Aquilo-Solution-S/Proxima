# 13. Flavor Marketplace

Binding ADR:
`docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md`.

## Unit

Marketplace shipment unit:

```
Flavor crate {
  payload schemas,
  relation descriptors,
  event sources,
  MCP tools,
  personalities,
  wake filter kinds,
  frontend package,
  migrations,
}
```

Personality shipment unit inside a flavor:

```
Personality {
  personality_type_id,
  self_schema,
  default self payload,
  system prompt,
  tool palette extension,
  writeable schemas,
  writeable relations,
  default wake filters,
  model tier,
  max wake chain depth,
}
```

## Authoring Boundary

Flavor authors may define:

| Surface | Rule |
|---|---|
| Schemas | typed payload traits only |
| Relations | registered `RelationDescriptor` ids |
| Tools | build-time tool types |
| Personalities | `PersonalityFlavor` impls |
| Wake filters | `WakeFilterKind` impls |
| Frontend | views, settings panels, renderers, codecs |

Flavor authors may not define runtime registration endpoints, feature flags,
or similarity-authored edges.

## Composite Discipline

Composite binary owns inclusion:

```
register(core substrate)
register(flavor A)
register(flavor B)
freeze registry
```

Collision checks happen before serving:

- schema id/version duplicate
- relation id duplicate
- MCP tool duplicate
- personality type duplicate
- wake filter kind duplicate
- invalid personality write surface

## Frontend Contract

Frontend packages may ship:

| Frontend asset | Registered through |
|---|---|
| Payload renderer | Hub renderer registry |
| Payload codec | Hub codec registry |
| View | Hub view registry |
| Settings panel | Hub settings registry |

Runtime instance management uses substrate commands:

```
ProvisionOwner
ListPersonalityInstances
InstantiatePersonality
SetWakeConfig
```

## Out Of Scope For v1

- Runtime plugin loading.
- Out-of-process flavor execution.
- Flavor-defined billing contracts.
- Marketplace install UI.
- Tool-palette negotiation across binaries.
- Custom wake-filter editor generation beyond JSON parameters.

Those require a new ADR and do not change the v1 in-process registry.
