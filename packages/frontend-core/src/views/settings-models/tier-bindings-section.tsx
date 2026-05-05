import { For, Show, createResource, createSignal, type Component, type Resource } from "solid-js";
import { commands, type LlmCaps, type LlmModelRecord, type ModelTier, type TierBindings } from "../../bindings";
import { formatCommandError as formatError } from "../../format-error";
import { TIERS } from "./constants";
import { loadTierRequires } from "./loaders";

// Tier Bindings Section
export const TierBindingsSection: Component<{
  bindings: Resource<TierBindings>;
  llmModels: Resource<LlmModelRecord[]>;
  onChange: () => void;
}> = (props) => {
  const [error, setError] = createSignal<string | null>(null);

  // Create resources for tier requirements
  const [fastReq] = createResource(() => loadTierRequires("fast"));
  const [standardReq] = createResource(() => loadTierRequires("standard"));
  const [deepReq] = createResource(() => loadTierRequires("deep"));

  const handleBind = async (tier: ModelTier, vendor: string, modelId: string) => {
    const r = await commands.tierBind(tier, vendor, modelId);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onChange();
    }
  };

  const handleUnbind = async (tier: ModelTier) => {
    const r = await commands.tierUnbind(tier);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onChange();
    }
  };

  const formatCaps = (caps: LlmCaps): string => {
    const parts: string[] = [];
    if (caps.tool_use) parts.push("tool_use");
    if (caps.json_mode) parts.push("json_mode");
    if (caps.long_context) parts.push("long_context");
    if (caps.vision) parts.push("vision");
    return parts.join(", ");
  };

  return (
    <section>
      <h2>Tier bindings</h2>
      <Show when={error()}>
        <p class="proxima-error">{error()}</p>
      </Show>
      <Show when={props.bindings()}>
        {(bindings) => (
          <For each={TIERS}>
            {(tier) => {
              const binding = () => bindings()?.[tier];
              const models = () => props.llmModels() ?? [];
              const reqResource =
                tier === "fast"
                  ? fastReq
                  : tier === "standard"
                    ? standardReq
                    : deepReq;

              return (
                <div>
                  <Show when={reqResource()}>
                    {(reqCaps) => (
                      <p class="proxima-tier-requires">
                        requires: {formatCaps(reqCaps())}
                      </p>
                    )}
                  </Show>
                  <div class="proxima-tier-row">
                    <span class="proxima-tier-label">{tier}</span>
                    <select
                      value={binding() ? `${binding()!.vendor}|${binding()!.model_id}` : ""}
                      onChange={(e) => {
                        if (e.target.value) {
                          const [vendor, modelId] = e.target.value.split("|");
                          void handleBind(tier, vendor, modelId);
                        }
                      }}
                    >
                      <option value="">— Select model —</option>
                      <For each={models()}>
                        {(m) => (
                          <option value={`${m.vendor}|${m.model_id}`}>
                            {m.vendor} / {m.model_id}
                          </option>
                        )}
                      </For>
                    </select>
                    <button
                      class="proxima-btn proxima-btn-danger"
                      onClick={() => void handleUnbind(tier)}
                      disabled={!binding()}
                    >
                      Unbind
                    </button>
                  </div>
                </div>
              );
            }}
          </For>
        )}
      </Show>
    </section>
  );
};
