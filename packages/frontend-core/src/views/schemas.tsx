import "./schemas.css";

import { For, Show, createMemo, createSignal, type Component } from "solid-js";
import type { SchemaInfo } from "../bindings";
import { useGraph } from "../graph-store";
import { LoadingSurface, Mono, SchemaTag } from "../primitives";
import type { Hub } from "../hub";

export const SchemasView: Component<{ hub: Hub }> = (props) => {
  const graph = useGraph();
  const schemas = () => graph.state().schemas;
  const groups = createMemo(() => groupSchemasByFlavor(schemas(), props.hub));

  return (
    <section class="proxima-view proxima-view-schemas">
      <div class="schema-view-head">
        <div>
          <h1>Schemas</h1>
          <p class="proxima-dim">
            Build-time payload registry grouped by flavor.
          </p>
        </div>
        <Mono style={{ "font-size": "10px", color: "var(--ink-50)" }}>
          {schemas().length} registered
        </Mono>
      </div>

      <Show when={graph.state().streamStatus === "connecting"}>
        <LoadingSurface label="Loading schemas" />
      </Show>
      <Show
        when={schemas().length > 0}
        fallback={
          <p class="proxima-dim">
            Schema registry is available in the Tauri shell runtime.
          </p>
        }
      >
        <div class="schema-flavor-stack">
          <For each={groups()}>
            {(group) => <SchemaFlavorGroup group={group} hub={props.hub} />}
          </For>
        </div>
      </Show>
    </section>
  );
};

type SchemaFlavorGroupModel = {
  flavor: string;
  schemas: SchemaInfo[];
};

const groupSchemasByFlavor = (
  schemas: SchemaInfo[],
  hub: Hub,
): SchemaFlavorGroupModel[] => {
  const byFlavor = new Map<string, SchemaInfo[]>();
  for (const schema of [...schemas].sort(compareSchemaInfo)) {
    const flavor = schemaFlavor(schema, hub);
    byFlavor.set(flavor, [...(byFlavor.get(flavor) ?? []), schema]);
  }

  return [...byFlavor.entries()]
    .map(([flavor, entries]) => ({ flavor, schemas: entries }))
    .sort((a, b) => a.flavor.localeCompare(b.flavor));
};

const compareSchemaInfo = (a: SchemaInfo, b: SchemaInfo): number =>
  kindRank(a.kind) - kindRank(b.kind) ||
  a.schema_id.localeCompare(b.schema_id) ||
  a.schema_version - b.schema_version;

const kindRank = (kind: SchemaInfo["kind"]): number =>
  ({
    Fact: 0,
    Abstraction: 1,
    Perspective: 2,
    Goal: 3,
    Edge: 4,
    CitedObject: 5,
    CitationMapping: 6,
  })[kind] ?? 99;

const schemaFlavor = (schema: SchemaInfo, hub: Hub): string => {
  const registered = registrationFor(schema, hub)?.flavor;
  if (registered !== undefined) return registered;

  const namespace = schema.schema_id.split("/")[0];
  if (hub.registeredFlavors().includes(namespace)) return namespace;

  return "substrate";
};

/// Display name for a flavor key. Production flavor keys mirror the
/// schema-id namespace (`proxima-code`, `proxima-agent-memory`); the
/// `proxima-` prefix is signal-free at the UI layer, so strip it for
/// headings while keeping the canonical key everywhere else.
const formatFlavorLabel = (flavor: string): string =>
  flavor.startsWith("proxima-") ? flavor.slice("proxima-".length) : flavor;

const registrationFor = (schema: SchemaInfo, hub: Hub) =>
  hub.registeredCodecs().find(
    (r) =>
      r.schemaId === schema.schema_id &&
      r.schemaVersion === schema.schema_version,
  ) ??
  hub.registeredRenderers().find(
    (r) =>
      r.schemaId === schema.schema_id &&
      r.schemaVersion === schema.schema_version,
  );

const SchemaFlavorGroup: Component<{
  group: SchemaFlavorGroupModel;
  hub: Hub;
}> = (props) => {
  const [collapsed, setCollapsed] = createSignal(
    props.group.flavor === "substrate",
  );
  const [kindFilter, setKindFilter] =
    createSignal<SchemaInfo["kind"] | null>(null);
  const counts = () => countByKind(props.group.schemas);
  const visibleSchemas = () => {
    const filter = kindFilter();
    return filter === null
      ? props.group.schemas
      : props.group.schemas.filter((schema) => schema.kind === filter);
  };

  return (
    <section
      classList={{
        "schema-flavor-group": true,
        "is-collapsed": collapsed(),
      }}
    >
      <header class="schema-flavor-head">
        <button
          type="button"
          class="schema-flavor-toggle"
          aria-expanded={!collapsed()}
          aria-controls={`schema-flavor-${props.group.flavor}`}
          onClick={() => setCollapsed((value) => !value)}
        >
          <span class="schema-flavor-title">
            <span
              classList={{
                "schema-flavor-chevron": true,
                "is-collapsed": collapsed(),
              }}
              aria-hidden="true"
            />
            <span>
              <h2>{formatFlavorLabel(props.group.flavor)}</h2>
              <Mono style={{ "font-size": "10px", color: "var(--ink-60)" }}>
                {visibleSchemas().length} / {props.group.schemas.length} schemas
              </Mono>
            </span>
          </span>
        </button>
        <div class="schema-kind-summary">
          <button
            type="button"
            classList={{
              "schema-kind-chip": true,
              "is-active": kindFilter() === null,
            }}
            onClick={() => setKindFilter(null)}
          >
            All <b>{props.group.schemas.length}</b>
          </button>
          <For each={counts()}>
            {(count) => (
              <button
                type="button"
                classList={{
                  "schema-kind-chip": true,
                  "is-active": kindFilter() === count.kind,
                }}
                onClick={() => setKindFilter(count.kind)}
              >
                {count.kind} <b>{count.count}</b>
              </button>
            )}
          </For>
        </div>
      </header>
      <Show when={!collapsed()}>
        <div id={`schema-flavor-${props.group.flavor}`} class="schema-table">
          <For each={visibleSchemas()}>
            {(schema) => <SchemaRow info={schema} hub={props.hub} />}
          </For>
        </div>
      </Show>
    </section>
  );
};

const countByKind = (schemas: SchemaInfo[]) => {
  const counts = new Map<SchemaInfo["kind"], number>();
  for (const schema of schemas) {
    counts.set(schema.kind, (counts.get(schema.kind) ?? 0) + 1);
  }
  return [...counts.entries()]
    .map(([kind, count]) => ({ kind, count }))
    .sort((a, b) => kindRank(a.kind) - kindRank(b.kind));
};

const SchemaRow: Component<{ info: SchemaInfo; hub: Hub }> = (props) => {
  const hasCodec = () =>
    props.hub.codecFor(props.info.schema_id, props.info.schema_version) !== null;
  const hasRenderer = () =>
    props.hub.rendererFor(props.info.schema_id, props.info.schema_version) !== null;
  const registryCode = () => schemaRegistryCode(props.info);

  return (
    <article class={`schema-row schema-row-${schemaKindClass(props.info.kind)}`}>
      <div class="schema-kind-marker" aria-hidden="true">
        {schemaKindGlyph(props.info.kind)}
      </div>
      <div class="schema-row-main">
        <div class="schema-row-id">
          <SchemaTag
            id={props.info.schema_id}
            version={props.info.schema_version}
            color="var(--ink-70)"
          />
          <span class="schema-kind">{props.info.kind}</span>
        </div>
        <div class="schema-row-meta">
          <span>{props.info.sidecar_table ?? "no sidecar"}</span>
          <Show when={props.info.natural_key_columns.length > 0}>
            <span>natural key: {props.info.natural_key_columns.join(", ")}</span>
          </Show>
          <Show when={props.info.filter_keys.length > 0}>
            <span>filters: {props.info.filter_keys.join(", ")}</span>
          </Show>
        </div>
      </div>
      <div class="schema-runtime">
        <span classList={{ active: hasCodec() }}>codec</span>
        <span classList={{ active: hasRenderer() }}>renderer</span>
      </div>
      <details class="schema-code">
        <summary>registry shape</summary>
        <pre>{registryCode()}</pre>
      </details>
    </article>
  );
};

const schemaKindClass = (kind: SchemaInfo["kind"]): string =>
  kind.replace(/([a-z])([A-Z])/g, "$1-$2").toLowerCase();

const schemaKindGlyph = (kind: SchemaInfo["kind"]): string =>
  ({
    Fact: "F",
    Abstraction: "A",
    Perspective: "P",
    Goal: "G",
    Edge: "E",
    CitedObject: "C",
    CitationMapping: "M",
  })[kind];

const schemaRegistryCode = (schema: SchemaInfo): string =>
  JSON.stringify(
    {
      schema_id: schema.schema_id,
      schema_version: schema.schema_version,
      kind: schema.kind,
      sidecar_table: schema.sidecar_table,
      filter_keys: schema.filter_keys,
      natural_key_columns: schema.natural_key_columns,
      source_code: "not exposed by Schema response",
    },
    null,
    2,
  );
