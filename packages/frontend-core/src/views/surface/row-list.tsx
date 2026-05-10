import { For, Show, type Component } from "solid-js";
import type { DecodedMemory, MemoryProvenance } from "../../graph-store";

export type ActiveTab = "All" | "Fact" | "Abstraction" | "Perspective" | "Goal";

const KIND_BADGE: Record<DecodedMemory["row"]["kind"], string> = {
  Fact: "F",
  Abstraction: "A",
  Perspective: "P",
  Goal: "G",
};

const formatRelative = (ms: number | undefined): string => {
  if (ms === undefined) return "—";
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60_000);
  if (min < 1) return "now";
  if (min < 60) return `${min}m`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr}h`;
  return `${Math.round(hr / 24)}d`;
};

export const RowList: Component<{
  rows: DecodedMemory[];
  provenance: ReadonlyMap<string, MemoryProvenance>;
  activeTab: ActiveTab;
  selectedId: string | null;
  onSelect: (id: string) => void;
}> = (props) => (
  <div class="surface-row-list" role="grid">
    <For each={props.rows}>
      {(row) => {
        const prov = props.provenance.get(row.row.id);
        const isSelected = props.selectedId === row.row.id;
        return (
          <div
            role="row"
            class="surface-row"
            classList={{ "surface-row--selected": isSelected }}
            onClick={() => props.onSelect(row.row.id)}
          >
            <Show when={props.activeTab === "All"}>
              <span class={`surface-row__pillar surface-row__pillar--${row.row.kind}`}>
                {KIND_BADGE[row.row.kind]}
              </span>
            </Show>
            <span class="surface-row__schema">{row.row.schema_id}</span>
            <span class="surface-row__author">
              {prov?.authoring_personality_instance_id ?? "—"}
            </span>
            <span class="surface-row__size">{row.row.payload.length} B</span>
            <span class="surface-row__time">{formatRelative(prov?.written_at_ms)}</span>
          </div>
        );
      }}
    </For>
  </div>
);
