import { Show, type Component } from "solid-js";
import { useQuery } from "../queries";
import type { QueryRequest } from "../bindings";

const NIL_UUID = "00000000-0000-0000-0000-000000000000";

const NOOP_QUERY: QueryRequest = {
  owner: { principal: { User: NIL_UUID }, org_id: NIL_UUID },
  entity_kind: null,
  schema_id: null,
  supersession: "HeadsOnly",
  limit: 0,
};

export const SettingsGeneralPanel: Component = () => {
  const queryResp = useQuery(() => NOOP_QUERY);

  return (
    <div class="proxima-settings-panel">
      <h2>Engine</h2>
      <Show when={queryResp.error}>
        <p class="proxima-error">Engine error: {String(queryResp.error)}</p>
      </Show>
      <Show when={queryResp.loading}>
        <p class="proxima-dim">Loading…</p>
      </Show>
      <Show when={queryResp()}>
        {(resp) => (
          <p class="proxima-dim">
            seq high-water: {resp().seq_high_water ?? "(no events yet)"}
          </p>
        )}
      </Show>

      <h2>Owner</h2>
      <p class="proxima-dim">
        principal: User({NIL_UUID})
        <br />
        org_id: {NIL_UUID}
      </p>
      <p class="proxima-dim">
        Editable owner config lands when a dedicated configuration verb does.
      </p>

      <h2>Theme</h2>
      <p class="proxima-dim">dark (only theme in v1)</p>
    </div>
  );
};
