import { For, type Component } from "solid-js";
import type { ActiveTab } from "./row-list";

const TABS: { key: ActiveTab; label: string }[] = [
  { key: "All", label: "All" },
  { key: "Perspective", label: "P" },
  { key: "Abstraction", label: "A" },
  { key: "Fact", label: "F" },
  { key: "Goal", label: "G" },
];

export const TabStrip: Component<{
  active: ActiveTab;
  counts: Record<ActiveTab, number>;
  onChange: (tab: ActiveTab) => void;
  onToggleFilters: () => void;
}> = (props) => (
  <div class="surface-tab-strip" role="tablist">
    <For each={TABS}>
      {(tab) => (
        <button
          role="tab"
          aria-selected={props.active === tab.key}
          class="surface-tab"
          classList={{ "surface-tab--active": props.active === tab.key }}
          onClick={() => props.onChange(tab.key)}
        >
          {tab.label} {props.counts[tab.key]}
        </button>
      )}
    </For>
    <span class="surface-tab-strip__spacer" />
    <button
      type="button"
      class="surface-tab-strip__filters"
      aria-label="Filters"
      onClick={props.onToggleFilters}
    >
      ⚙ Filters
    </button>
  </div>
);
