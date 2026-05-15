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

Each crate publishes a single `FlavorDescriptor` (docs/08 §Flavor
Metadata) — the structured form of "Flavor crate" carrying
`flavor_id`, `display_name`, `package_version`, `author`, and a
`FlavorProvenance` discriminator. The substrate threads it through
every `PersonalityInstance` so the hub renders the flavor label
authoritatively. `FlavorProvenance::Marketplace { source_url }` is
the marketplace hook reserved for post-v1 out-of-process loading; v1
flavors all ship as `Builtin`.

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
| Goal payloads | schemas registered against core `GoalPayload` |
| Relations | registered `RelationDescriptor` ids |
| Tools | build-time tool types |
| Personalities | `PersonalityFlavor` impls |
| Wake entries | recipe refs, model tiers, execution modes, tool palettes |
| Frontend | views, settings panels, renderers, codecs |

Flavor authors may not define runtime registration endpoints, feature flags,
or similarity-authored edges.

Goal is a core entity. A flavor may ship GoalPayload schemas, sidecars,
renderers, and schema-aware tools; it does not redefine Goal lifecycle or
approval state (see [06 §Goal Assignment](06-goals-and-self.md#goal-assignment)).

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
