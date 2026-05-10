import { For, Show, type Component } from "solid-js";
import type { DecodedMemory, MemoryProvenance } from "../../graph-store";
import type { OneHopLineage } from "../../graph-selectors";
import type { Hub } from "../../hub";

const formatRelative = (ms: number): string => {
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60_000);
  if (min < 1) return "now";
  if (min < 60) return `${min}m ago`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr}h ago`;
  return `${Math.round(hr / 24)}d ago`;
};

const FallbackPayload: Component<{ payload: unknown }> = (props) => {
  if (props.payload === null || typeof props.payload !== "object") {
    return <div class="detail-pane__fallback-scalar">{String(props.payload)}</div>;
  }
  const entries = Object.entries(props.payload as Record<string, unknown>);
  return (
    <dl class="detail-pane__kv">
      <For each={entries}>
        {([k, v]) => (
          <>
            <dt>{k}</dt>
            <dd>{typeof v === "object" ? JSON.stringify(v) : String(v)}</dd>
          </>
        )}
      </For>
    </dl>
  );
};

export const DetailPane: Component<{
  memory: DecodedMemory;
  provenance: MemoryProvenance | undefined;
  lineage: OneHopLineage;
  flavor: string | null;
  hub: Hub;
}> = (props) => {
  const renderer = () =>
    props.hub.rendererFor(
      props.memory.row.schema_id,
      props.memory.row.schema_version,
      props.memory.row.kind,
    );

  return (
    <section class="detail-pane">
      <header class="detail-pane__header">
        <div class="detail-pane__title">
          {props.memory.row.schema_id} v{props.memory.row.schema_version}
        </div>
        <div class="detail-pane__meta-line">
          {props.memory.row.id.slice(0, 8)} ·
          {" "}{props.memory.row.payload.length} bytes ·
          {" "}{props.provenance?.authoring_personality_instance_id ?? "—"}
        </div>
      </header>

      <section class="detail-pane__block">
        <h3>PAYLOAD</h3>
        <Show when={renderer()} fallback={<FallbackPayload payload={props.memory.payload} />}>
          {renderer()!.render({ memory: props.memory.row, payload: props.memory.payload })}
        </Show>
      </section>

      <section class="detail-pane__block">
        <h3>LINEAGE (1-hop)</h3>
        <Show
          when={props.lineage.outbound.length > 0 || props.lineage.inbound.length > 0}
          fallback={<div class="detail-pane__empty">no incident edges</div>}
        >
          <ul>
            <For each={props.lineage.outbound}>
              {(group) => (
                <li>→ {group.relation} {group.target_kind} {group.target_schema_id} ×{group.count}</li>
              )}
            </For>
            <For each={props.lineage.inbound}>
              {(group) => (
                <li>← {group.relation} {group.target_kind} {group.target_schema_id} ×{group.count}</li>
              )}
            </For>
          </ul>
        </Show>
      </section>

      <section class="detail-pane__block">
        <h3>METADATA</h3>
        <dl class="detail-pane__kv">
          <dt>schema_id</dt><dd>{props.memory.row.schema_id}</dd>
          <dt>schema_version</dt><dd>{props.memory.row.schema_version}</dd>
          <dt>flavor</dt><dd>{props.flavor ?? "core"}</dd>
          <dt>pillar</dt><dd>{props.memory.row.kind}</dd>
          <dt>authored_by</dt>
          <dd>{props.provenance?.authoring_personality_instance_id ?? "—"}</dd>
          <dt>written_at</dt>
          <dd>{props.provenance ? formatRelative(props.provenance.written_at_ms) : "—"}</dd>
          <dt>byte_size</dt>
          <dd>{props.memory.row.payload.length}</dd>
        </dl>
      </section>
    </section>
  );
};
