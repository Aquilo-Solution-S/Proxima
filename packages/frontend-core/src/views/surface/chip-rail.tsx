import { For, Show, type Component } from "solid-js";
import { GRAPH_LAYERS, useGraphFilter } from "../../graph-filter-store";

interface Chip {
  facet: "schema" | "flavor" | "author" | "time" | "size" | "pillar";
  label: string;
  remove: () => void;
}

const formatTime = (range: { fromMs: number; toMs: number }): string => {
  const fmt = (ms: number) => new Date(ms).toISOString().slice(0, 16);
  return `${fmt(range.fromMs)} → ${fmt(range.toMs)}`;
};

const formatSize = (range: { minBytes: number; maxBytes: number }): string =>
  `${range.minBytes}–${range.maxBytes} B`;

export const ChipRail: Component<{ flavors: string[] }> = (_props) => {
  const filter = useGraphFilter();

  const chips = (): Chip[] => {
    const s = filter.state();
    const out: Chip[] = [];
    for (const flavorId of s.hiddenFlavorIds) {
      out.push({
        facet: "flavor",
        label: `flavor != ${flavorId}`,
        remove: () => filter.setFlavor(flavorId, true),
      });
    }
    for (const sid of s.schemaIds) {
      out.push({
        facet: "schema",
        label: `schema: ${sid}`,
        remove: () => filter.setSchema(sid, false),
      });
    }
    for (const author of s.authoredBy) {
      out.push({
        facet: "author",
        label: `author: ${author}`,
        remove: () => filter.setAuthor(author, false),
      });
    }
    if (s.timeRange) {
      out.push({
        facet: "time",
        label: `time: ${formatTime(s.timeRange)}`,
        remove: () => filter.setTimeRange(null),
      });
    }
    if (s.sizeRange) {
      out.push({
        facet: "size",
        label: `size: ${formatSize(s.sizeRange)}`,
        remove: () => filter.setSizeRange(null),
      });
    }
    if (s.layers.size !== GRAPH_LAYERS.length) {
      const active = Array.from(s.layers).join(", ");
      out.push({
        facet: "pillar",
        label: `pillar: ${active}`,
        remove: () => {
          for (const l of GRAPH_LAYERS) filter.setLayer(l, true);
        },
      });
    }
    return out;
  };

  return (
    <Show when={chips().length > 0}>
      <ul class="surface-chip-rail" role="list">
        <For each={chips()}>
          {(chip) => (
            <li class={`surface-chip surface-chip--${chip.facet}`} role="listitem">
              <span class="surface-chip__label">{chip.label}</span>
              <button
                type="button"
                class="surface-chip__remove"
                aria-label={`remove ${chip.facet} chip`}
                onClick={chip.remove}
              >
                ✕
              </button>
            </li>
          )}
        </For>
      </ul>
    </Show>
  );
};
