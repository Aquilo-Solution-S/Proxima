import { For, Show, type Component } from "solid-js";
import type { Hub } from "../hub";
import { SchemaTag } from "../primitives";

export const FlavorsView: Component<{ hub: Hub }> = (props) => {
  return (
    <section class="proxima-view proxima-view-flavors">
      <h1>Flavors</h1>
      <p class="proxima-dim">
        Build-time flavor crates compiled into this binary.
      </p>

      <Show
        when={props.hub.registeredFlavors().length > 0}
        fallback={
          <p class="proxima-dim" style={{ "margin-top": "16px" }}>
            No flavors registered. Bare substrate.
          </p>
        }
      >
        <ul class="proxima-flavor-list">
          <For each={props.hub.registeredFlavors()}>
            {(name) => <FlavorCard name={name} hub={props.hub} />}
          </For>
        </ul>
      </Show>
    </section>
  );
};

const FlavorCard: Component<{ name: string; hub: Hub }> = (props) => {
  const renderers = () =>
    props.hub.registeredRenderers().filter((r) => r.flavor === props.name);
  const views = () =>
    props.hub.views().filter((v) => v.flavor === props.name);

  return (
    <li class="proxima-flavor-card">
      <h2>{props.name}</h2>
      <div>
        <h3>Renderers</h3>
        <Show
          when={renderers().length > 0}
          fallback={<p class="proxima-dim">none</p>}
        >
          <ul class="proxima-schema-list">
            <For each={renderers()}>
              {(r) => (
                <li>
                  <SchemaTag id={r.schemaId} version={r.schemaVersion} />
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
      <div>
        <h3>Views</h3>
        <Show
          when={views().length > 0}
          fallback={<p class="proxima-dim">none</p>}
        >
          <ul class="proxima-flavor-views">
            <For each={views()}>
              {(v) => (
                <li>
                  {v.label}{" "}
                  <span class="proxima-dim proxima-mono">({v.id})</span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
    </li>
  );
};
