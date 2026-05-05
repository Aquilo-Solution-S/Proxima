import { For, Show, createResource, type Component } from "solid-js";
import { commands, type QueryRequest } from "../bindings";
import { useQuery } from "../queries";
import { SchemaTag } from "../primitives";

const NIL_UUID = "00000000-0000-0000-0000-000000000000";

const NOOP_QUERY: QueryRequest = {
  owner: {
    principal: { User: NIL_UUID },
    org_id: NIL_UUID,
  },
  entity_kind: null,
  schema_id: null,
  supersession: "HeadsOnly",
  limit: 0,
};

export const SettingsView: Component = () => {
  const queryResp = useQuery(() => NOOP_QUERY);

  const [schemaResp] = createResource(async () => {
    const r = await commands.schema();
    if (r.status === "error") throw r.error;
    return r.data;
  });

  return (
    <section class="proxima-view proxima-view-settings">
      <h1>Settings</h1>

      <h2>Engine</h2>
      <Show when={queryResp.error}>
        <p class="proxima-error">
          Engine error: {String(queryResp.error)}
        </p>
      </Show>
      <Show when={queryResp.loading}>
        <p class="proxima-dim">Loading…</p>
      </Show>
      <Show when={queryResp()}>
        {(resp) => (
          <p class="proxima-dim">
            seq high-water:{" "}
            {resp().seq_high_water ?? "(no events yet)"}
          </p>
        )}
      </Show>

      <h2>Registered schemas</h2>
      <Show
        when={schemaResp()}
        fallback={<p class="proxima-dim">Loading…</p>}
      >
        <ul class="proxima-schema-list">
          <For each={schemaResp()!.schemas}>
            {(s) => (
              <li>
                <SchemaTag id={s.schema_id} version={s.schema_version} />{" "}
                <span class="proxima-dim">({s.kind})</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
};
