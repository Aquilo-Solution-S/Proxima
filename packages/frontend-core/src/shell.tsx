import {
  ErrorBoundary,
  For,
  Suspense,
  createSignal,
  type Component,
} from "solid-js";
import { Dynamic } from "solid-js/web";
import { FilterDialog } from "./filter-dialog";
import { useGraph } from "./graph-store";
import { flavorFilterId } from "./graph-filter-store";
import { schemaFlavor } from "./graph-selectors";
import type { Hub, RegisteredView } from "./hub";
import { LoadingSurface, ProximaSeal } from "./primitives";

const EmptyView: Component = () => (
  <div class="shell-empty">No view selected</div>
);

export const Shell: Component<{ hub: Hub }> = (props) => {
  const graph = useGraph();
  const [filterOpen, setFilterOpen] = createSignal(false);
  const activeView = (): RegisteredView | undefined =>
    props.hub.views().find((v) => v.id === props.hub.currentView());
  const schemaKey = (schemaId: string, schemaVersion: number): string =>
    `${schemaId}@${schemaVersion}`;
  const filterOrigins = (): string[] => {
    const snapshot = graph.state();
    const origins = new Set(props.hub.registeredFlavors());
    const schemasByKey = new Map(
      snapshot.schemas.map((schema) => [
        schemaKey(schema.schema_id, schema.schema_version),
        schema,
      ]),
    );
    for (const schema of snapshot.schemas) {
      origins.add(flavorFilterId(schemaFlavor(schema, props.hub)));
    }
    for (const memory of snapshot.memoriesById.values()) {
      const schema = schemasByKey.get(
        schemaKey(memory.row.schema_id, memory.row.schema_version),
      );
      const flavor =
        schema === undefined
          ? props.hub.flavorFor(memory.row.schema_id, memory.row.schema_version)
          : schemaFlavor(schema, props.hub);
      origins.add(flavorFilterId(flavor));
    }
    for (const goal of snapshot.goalsById.values()) {
      const schema = schemasByKey.get(schemaKey(goal.schema_id, goal.schema_version));
      const flavor =
        schema === undefined
          ? props.hub.flavorFor(goal.schema_id, goal.schema_version)
          : schemaFlavor(schema, props.hub);
      origins.add(flavorFilterId(flavor));
    }
    return [...origins];
  };

  return (
    <div class="proxima-shell">
      <header class="chrome-top">
        <div class="chrome-left">
          <div class="shell-brand" aria-label="Proxima Shell">
            <ProximaSeal size={22} theme="dark" mode="favicon" />
            <span class="shell-brand-wordmark">Proxima</span>
            <span class="shell-brand-divider" aria-hidden="true" />
            <span class="shell-brand-product">Shell</span>
          </div>
          <nav class="hub-nav">
            <For each={props.hub.views()}>
              {(view) => (
                <button
                  type="button"
                  classList={{
                    "hub-nav-item": true,
                    active: view.id === props.hub.currentView(),
                  }}
                  onClick={() => props.hub.setCurrentView(view.id)}
                >
                  {view.label}
                </button>
              )}
            </For>
          </nav>
        </div>
        <button
          type="button"
          class="hub-nav-item"
          aria-haspopup="dialog"
          aria-expanded={filterOpen()}
          onClick={() => setFilterOpen((v) => !v)}
        >
          Filters
        </button>
      </header>
      <FilterDialog
        open={filterOpen()}
        schemas={graph.state().schemas}
        flavors={filterOrigins()}
        onClose={() => setFilterOpen(false)}
      />
      <main class="shell-main">
        <ErrorBoundary
          fallback={(err, reset) => {
            const causeOf = (e: unknown): unknown =>
              e instanceof Error
                ? (e as Error & { cause?: unknown }).cause
                : undefined;
            console.error("shell view crashed:", err, "cause:", causeOf(err));
            const message = err instanceof Error ? err.message : String(err);
            const cause = causeOf(err);
            const causeStr =
              cause === undefined
                ? ""
                : `\ncause: ${
                    typeof cause === "object"
                      ? JSON.stringify(cause)
                      : String(cause)
                  }`;
            return (
              <div class="shell-error">
                <h2>View crashed</h2>
                <pre class="shell-error-message">{message}{causeStr}</pre>
                <button type="button" class="hub-nav-item" onClick={reset}>
                  Retry
                </button>
              </div>
            );
          }}
        >
          <Suspense
            fallback={
              <LoadingSurface mode="panel" label="Loading view" stars="on" />
            }
          >
            <Dynamic component={activeView()?.component ?? EmptyView} />
          </Suspense>
        </ErrorBoundary>
      </main>
      <footer class="status-foot">
        <span class="rail-title">
          {props.hub.registeredFlavors().length} flavor
          {props.hub.registeredFlavors().length === 1 ? "" : "s"}
        </span>
      </footer>
    </div>
  );
};
