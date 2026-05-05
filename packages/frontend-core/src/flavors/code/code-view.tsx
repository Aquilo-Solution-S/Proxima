import { For, Show, createSignal, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import { ReposPanel } from "./repos-panel";

interface SubTab {
  id: string;
  label: string;
  component: Component;
}

const SUB_TABS: SubTab[] = [
  { id: "repos", label: "Repos", component: ReposPanel },
];

export const CodeView: Component = () => {
  const [active, setActive] = createSignal<string>(SUB_TABS[0]!.id);
  const activeTab = (): SubTab | undefined =>
    SUB_TABS.find((t) => t.id === active());

  return (
    <section class="proxima-view proxima-view-code">
      <h1>Code</h1>
      <div class="proxima-code-host">
        <nav class="proxima-code-rail">
          <For each={SUB_TABS}>
            {(tab) => (
              <button
                type="button"
                classList={{
                  "proxima-code-tab": true,
                  active: tab.id === active(),
                }}
                onClick={() => setActive(tab.id)}
              >
                {tab.label}
              </button>
            )}
          </For>
        </nav>
        <div class="proxima-code-pane">
          <Show
            when={activeTab()}
            fallback={<p class="proxima-dim">No sub-panel selected.</p>}
          >
            {(t) => <Dynamic component={t().component} />}
          </Show>
        </div>
      </div>
    </section>
  );
};
