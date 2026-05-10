import { For, Show, type Component } from "solid-js";
import { GRAPH_LAYERS, useGraphFilter } from "../../graph-filter-store";

export interface FilterFacets {
  flavors: string[];
  schemas: { schemaId: string; flavor: string | null }[];
  authors: string[];
}

export const FilterDrawer: Component<{
  open: boolean;
  onClose: () => void;
  facets: FilterFacets;
}> = (props) => {
  const filter = useGraphFilter();

  const onTimeFrom = (e: Event) => {
    const value = (e.currentTarget as HTMLInputElement).value;
    const fromMs = value === "" ? null : new Date(value).getTime();
    const current = filter.state().timeRange;
    if (fromMs === null) {
      filter.setTimeRange(null);
      return;
    }
    filter.setTimeRange({ fromMs, toMs: current?.toMs ?? Date.now() });
  };
  const onTimeTo = (e: Event) => {
    const value = (e.currentTarget as HTMLInputElement).value;
    const toMs = value === "" ? null : new Date(value).getTime();
    const current = filter.state().timeRange;
    if (toMs === null) {
      filter.setTimeRange(null);
      return;
    }
    filter.setTimeRange({ fromMs: current?.fromMs ?? 0, toMs });
  };
  const onSize = (key: "minBytes" | "maxBytes", e: Event) => {
    const value = Number((e.currentTarget as HTMLInputElement).value);
    const current = filter.state().sizeRange ?? { minBytes: 0, maxBytes: Number.MAX_SAFE_INTEGER };
    filter.setSizeRange({ ...current, [key]: value });
  };

  return (
    <Show when={props.open}>
      <aside class="surface-filter-drawer" role="dialog" aria-label="Filters">
        <header class="surface-filter-drawer__header">
          <h2>Filters</h2>
          <button type="button" onClick={props.onClose} aria-label="close">×</button>
        </header>

        <fieldset>
          <legend>Pillar</legend>
          <For each={GRAPH_LAYERS}>
            {(layer) => (
              <label>
                <input
                  type="checkbox"
                  checked={filter.state().layers.has(layer)}
                  onInput={(e) =>
                    filter.setLayer(layer, e.currentTarget.checked)
                  }
                />
                {layer}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Flavor</legend>
          <For each={props.facets.flavors}>
            {(flavor) => (
              <label>
                <input
                  type="checkbox"
                  checked={!filter.state().hiddenFlavorIds.has(flavor)}
                  onInput={(e) => filter.setFlavor(flavor, e.currentTarget.checked)}
                />
                {flavor}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Schema</legend>
          <For each={props.facets.schemas}>
            {(schema) => (
              <label>
                <input
                  type="checkbox"
                  checked={filter.state().schemaIds.has(schema.schemaId)}
                  onInput={(e) =>
                    filter.setSchema(schema.schemaId, e.currentTarget.checked)
                  }
                />
                {schema.schemaId}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Authored by</legend>
          <For each={props.facets.authors}>
            {(author) => (
              <label>
                <input
                  type="checkbox"
                  checked={filter.state().authoredBy.has(author)}
                  onInput={(e) =>
                    filter.setAuthor(author, e.currentTarget.checked)
                  }
                />
                {author}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Time</legend>
          <label>
            from <input type="datetime-local" onInput={onTimeFrom} />
          </label>
          <label>
            to <input type="datetime-local" onInput={onTimeTo} />
          </label>
        </fieldset>

        <fieldset>
          <legend>Size (bytes)</legend>
          <label>
            min <input type="number" min="0" onInput={(e) => onSize("minBytes", e)} />
          </label>
          <label>
            max <input type="number" min="0" onInput={(e) => onSize("maxBytes", e)} />
          </label>
        </fieldset>

        <footer class="surface-filter-drawer__footer">
          <button type="button" onClick={() => filter.reset()}>Reset</button>
          <button type="button" onClick={props.onClose}>Done</button>
        </footer>
      </aside>
    </Show>
  );
};
