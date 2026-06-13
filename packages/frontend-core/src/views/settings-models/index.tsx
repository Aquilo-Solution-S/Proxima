import { createResource, createSignal, Show, type Component } from "solid-js";
import { createTauriEngineClient } from "../../tauri-client";
import { sentinelOwner } from "../../graph-store";
import { EmbeddingSection } from "./embedding-section";
import { ModelsTable } from "./models-table";
import { RegisterModelModal } from "./register-model-modal";
import { loadActive, loadEmb } from "./loaders";

type SubTab = "chat" | "embedding";

export const SettingsModelsPanel: Component = () => {
  const client = createTauriEngineClient();
  const owner = sentinelOwner();
  const [subTab, setSubTab] = createSignal<SubTab>("chat");
  const [registerOpen, setRegisterOpen] = createSignal(false);

  const [embeddingModels, { refetch: refetchEmb }] = createResource(loadEmb);
  const [active, { refetch: refetchActive }] = createResource(loadActive);
  const [targets, { refetch: refetchTargets }] = createResource(async () =>
    client.listInferenceTargets({ principal: owner.principal }),
  );
  const [bindings, { refetch: refetchBindings }] = createResource(async () =>
    client.listInferenceTierBindings({ principal: owner.principal }),
  );

  const existingRefs = () =>
    (targets() ?? []).map((target) => target.target_ref);

  return (
    <div class="proxima-settings-panel">
      <nav class="proxima-models-subtabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={subTab() === "chat"}
          classList={{
            "proxima-models-subtab": true,
            active: subTab() === "chat",
          }}
          onClick={() => setSubTab("chat")}
        >
          Chat
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={subTab() === "embedding"}
          classList={{
            "proxima-models-subtab": true,
            active: subTab() === "embedding",
          }}
          onClick={() => setSubTab("embedding")}
        >
          Embedding
        </button>
      </nav>

      <Show when={subTab() === "chat"}>
        <section>
          <header class="proxima-models-section-head">
            <h2>Chat models</h2>
            <button
              type="button"
              class="proxima-btn proxima-btn-primary"
              onClick={() => setRegisterOpen(true)}
            >
              + Register model
            </button>
          </header>

          <ModelsTable
            client={client}
            owner={owner}
            targets={targets}
            bindings={bindings}
            refetchTargets={refetchTargets}
            refetchBindings={refetchBindings}
          />

          <Show when={registerOpen()}>
            <RegisterModelModal
              client={client}
              owner={owner}
              existingRefs={existingRefs()}
              onClose={() => setRegisterOpen(false)}
              onRegistered={() => refetchTargets()}
            />
          </Show>
        </section>
      </Show>

      <Show when={subTab() === "embedding"}>
        <EmbeddingSection
          embeddingModels={embeddingModels}
          active={active}
          onModelsChange={refetchEmb}
          onActiveChange={refetchActive}
        />
      </Show>
    </div>
  );
};
