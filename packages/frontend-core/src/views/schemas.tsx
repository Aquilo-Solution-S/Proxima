import { For, Show, createResource, type Component } from "solid-js";
import { commands, type SchemaInfo } from "../bindings";
import { LoadingSurface, SchemaTag } from "../primitives";
import type { Hub } from "../hub";

export const SchemasView: Component<{ hub: Hub }> = (props) => {
  const [schemaResp] = createResource(async () => {
    const r = await commands.schema();
    if (r.status === "error") throw r.error;
    return r.data;
  });

  return (
    <section class="proxima-view proxima-view-schemas">
      <h1>Schemas</h1>
      <p class="proxima-dim">
        All payload schemas registered with the engine. Each row links to the
        flavor that registered it (if any) and the renderer override (if one
        exists).
      </p>

      <Show when={schemaResp.error}>
        <p class="proxima-error">
          Engine error: {String(schemaResp.error)}
        </p>
      </Show>
      <Show when={schemaResp.loading}>
        <LoadingSurface label="Loading schemas" />
      </Show>
      <Show when={schemaResp()}>
        {(resp) => (
          <ul class="proxima-schema-list">
            <For each={resp().schemas}>
              {(s) => <SchemaRow info={s} hub={props.hub} />}
            </For>
          </ul>
        )}
      </Show>
    </section>
  );
};

const SchemaRow: Component<{ info: SchemaInfo; hub: Hub }> = (props) => {
  const renderer = () =>
    props.hub.rendererFor(props.info.schema_id, props.info.schema_version);
  const owningFlavor = () =>
    props.hub.registeredRenderers().find(
      (r) =>
        r.schemaId === props.info.schema_id &&
        r.schemaVersion === props.info.schema_version,
    )?.flavor ?? null;

  return (
    <li>
      <SchemaTag
        id={props.info.schema_id}
        version={props.info.schema_version}
      />
      <span class="proxima-dim">({props.info.kind})</span>
      <Show when={owningFlavor()}>
        <span class="proxima-dim">— {owningFlavor()}</span>
      </Show>
      <Show when={renderer()}>
        <span class="proxima-dim">— renderer registered</span>
      </Show>
    </li>
  );
};
