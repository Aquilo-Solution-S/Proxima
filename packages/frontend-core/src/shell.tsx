import { For, Suspense, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import type { Hub, RegisteredView } from "./hub";
import { LoadingSurface } from "./primitives";

const EmptyView: Component = () => (
  <div class="shell-empty">No view selected</div>
);

export const Shell: Component<{ hub: Hub }> = (props) => {
  const activeView = (): RegisteredView | undefined =>
    props.hub.views().find((v) => v.id === props.hub.currentView());

  return (
    <div class="proxima-shell">
      <header class="chrome-top">
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
      </header>
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
