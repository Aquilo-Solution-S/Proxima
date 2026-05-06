import { For, Suspense, createSignal, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import { FilterDialog } from "./filter-dialog";
import { useGraph } from "./graph-store";
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
        flavors={props.hub.registeredFlavors()}
        onClose={() => setFilterOpen(false)}
      />
      <main class="shell-main">
        <Suspense
          fallback={
            <LoadingSurface mode="panel" label="Loading view" stars="on" />
          }
        >
          <Dynamic component={activeView()?.component ?? EmptyView} />
        </Suspense>
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
