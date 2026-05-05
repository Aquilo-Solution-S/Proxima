import { For, Show, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import type { Hub, RegisteredSettingsPanel } from "../hub";

export const SettingsView: Component<{ hub: Hub }> = (props) => {
  const activePanel = (): RegisteredSettingsPanel | undefined =>
    props.hub.settingsPanels().find(
      (p) => p.id === props.hub.currentSettingsPanel(),
    );

  return (
    <section class="proxima-view proxima-view-settings">
      <h1>Settings</h1>

      <Show
        when={props.hub.settingsPanels().length > 0}
        fallback={
          <p class="proxima-dim">No settings panels registered.</p>
        }
      >
        <div class="proxima-settings-host">
          <nav class="proxima-settings-rail">
            <For each={props.hub.settingsPanels()}>
              {(panel) => (
                <button
                  type="button"
                  classList={{
                    "proxima-settings-tab": true,
                    active: panel.id === props.hub.currentSettingsPanel(),
                  }}
                  onClick={() => props.hub.setCurrentSettingsPanel(panel.id)}
                >
                  {panel.label}
                  <Show when={panel.flavor}>
                    <span class="proxima-dim proxima-mono">
                      {" "}
                      ({panel.flavor})
                    </span>
                  </Show>
                </button>
              )}
            </For>
          </nav>
          <div class="proxima-settings-pane">
            <Show
              when={activePanel()}
              fallback={
                <p class="proxima-dim">No panel selected.</p>
              }
            >
              {(p) => <Dynamic component={p().component} />}
            </Show>
          </div>
        </div>
      </Show>
    </section>
  );
};
