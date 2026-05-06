import {
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  type Component,
} from "solid-js";
import type { SchemaInfo } from "./bindings";
import {
  GRAPH_LAYERS,
  useGraphFilter,
  type GraphLayer,
} from "./graph-filter-store";

export const FilterDialog: Component<{
  open: boolean;
  schemas: readonly SchemaInfo[];
  flavors: readonly string[];
  onClose: () => void;
}> = (props) => {
  const filters = useGraphFilter();
  const [inputValue, setInputValue] = createSignal(filters.state().search);
  let searchTimer: ReturnType<typeof setTimeout> | null = null;

  createEffect(() => {
    setInputValue(filters.state().search);
  });

  onCleanup(() => {
    if (searchTimer !== null) clearTimeout(searchTimer);
  });

  const setSearchDebounced = (value: string): void => {
    setInputValue(value);
    if (searchTimer !== null) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      filters.setSearch(value);
      searchTimer = null;
    }, 120);
  };

  const handleReset = (): void => {
    if (searchTimer !== null) {
      clearTimeout(searchTimer);
      searchTimer = null;
    }
    filters.reset();
  };

  const sortedSchemas = () =>
    [...props.schemas].sort((a, b) =>
      `${a.schema_id}@${a.schema_version}`.localeCompare(
        `${b.schema_id}@${b.schema_version}`,
      ),
    );
  const sortedFlavors = () => [...props.flavors].sort();

  return (
    <Show when={props.open}>
      <div class="filter-dialog-backdrop" role="presentation">
        <section
          class="filter-dialog"
          role="dialog"
          aria-modal="true"
          aria-label="Filters"
        >
          <div class="filter-dialog-head">
            <h2>Filters</h2>
            <button type="button" class="filter-dialog-close" onClick={props.onClose}>
              Close
            </button>
          </div>

          <label class="filter-field">
            <span>Search</span>
            <input
              aria-label="Search"
              value={inputValue()}
              onInput={(event) => setSearchDebounced(event.currentTarget.value)}
            />
          </label>

          <div class="filter-dialog-section">
            <div class="filter-dialog-section-title">Layer</div>
            <For each={GRAPH_LAYERS}>
              {(layer) => (
                <label class="filter-check">
                  <input
                    type="checkbox"
                    checked={filters.state().layers.has(layer)}
                    onChange={(event) =>
                      filters.setLayer(
                        layer,
                        (event.currentTarget as HTMLInputElement).checked,
                      )
                    }
                  />
                  <span>{layer}</span>
                </label>
              )}
            </For>
          </div>

          <div class="filter-dialog-section">
            <div class="filter-dialog-section-title">Schema</div>
            <Show
              when={sortedSchemas().length > 0}
              fallback={<p class="filter-dialog-empty">No schemas</p>}
            >
              <For each={sortedSchemas()}>
                {(schema) => (
                  <label class="filter-check">
                    <input
                      type="checkbox"
                      checked={filters.state().schemaIds.has(schema.schema_id)}
                      onChange={(event) =>
                        filters.setSchema(
                          schema.schema_id,
                          (event.currentTarget as HTMLInputElement).checked,
                        )
                      }
                    />
                    <span>{schema.schema_id}@{schema.schema_version}</span>
                  </label>
                )}
              </For>
            </Show>
          </div>

          <div class="filter-dialog-section">
            <div class="filter-dialog-section-title">Flavor</div>
            <Show
              when={sortedFlavors().length > 0}
              fallback={<p class="filter-dialog-empty">No flavors</p>}
            >
              <For each={sortedFlavors()}>
                {(flavor) => (
                  <label class="filter-check">
                    <input
                      type="checkbox"
                      checked={!filters.state().hiddenFlavorIds.has(flavor)}
                      onChange={(event) =>
                        filters.setFlavor(
                          flavor,
                          (event.currentTarget as HTMLInputElement).checked,
                        )
                      }
                    />
                    <span>{flavor}</span>
                  </label>
                )}
              </For>
            </Show>
          </div>

          <div class="filter-dialog-actions">
            <button type="button" onClick={handleReset}>
              Reset
            </button>
          </div>
        </section>
      </div>
    </Show>
  );
};

export type { GraphLayer };
