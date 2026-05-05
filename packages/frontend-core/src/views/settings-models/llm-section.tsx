import { For, Show, createSignal, type Component, type Resource } from "solid-js";
import { commands, type Dialect, type LlmModelRecord } from "../../bindings";
import { formatCommandError as formatError } from "../../format-error";
import { LoadingSurface } from "../../primitives";
import { LlmCapsBadges } from "./badges";
import { DIALECTS } from "./constants";

// LLM Section
export const LlmSection: Component<{
  llmModels: Resource<LlmModelRecord[]>;
  onChange: () => void;
}> = (props) => {
  const [error, setError] = createSignal<string | null>(null);
  const [formData, setFormData] = createSignal({
    vendor: "",
    model_id: "",
    dialect: "anthropic" as Dialect,
    base_url: "",
    tool_use: false,
    json_mode: false,
    long_context: false,
    vision: false,
    secret_ref: "",
  });

  const handleSubmit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const data = formData();
    const record: LlmModelRecord = {
      vendor: data.vendor,
      model_id: data.model_id,
      dialect: data.dialect,
      base_url: data.base_url,
      caps: {
        tool_use: data.tool_use,
        json_mode: data.json_mode,
        long_context: data.long_context,
        vision: data.vision,
      },
      secret_ref: data.secret_ref || null,
    };
    const r = await commands.modelsRegisterLlm(record);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      setFormData({
        vendor: "",
        model_id: "",
        dialect: "anthropic",
        base_url: "",
        tool_use: false,
        json_mode: false,
        long_context: false,
        vision: false,
        secret_ref: "",
      });
      props.onChange();
    }
  };

  const handleDelete = async (vendor: string, modelId: string) => {
    const r = await commands.modelsDeleteLlm(vendor, modelId);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onChange();
    }
  };

  return (
    <section>
      <h2>LLM models</h2>
      <Show when={props.llmModels.error}>
        <p class="proxima-error">Error: {formatError(props.llmModels.error!)}</p>
      </Show>
      <Show when={props.llmModels.loading}>
        <LoadingSurface mode="inline" label="Loading" size={36} />
      </Show>
      <Show when={props.llmModels()}>
        {(models) => (
          <>
            <Show when={models().length === 0}>
              <p class="proxima-dim">No LLM models registered.</p>
            </Show>
            <Show when={models().length > 0}>
              <table class="proxima-models-table">
                <thead>
                  <tr>
                    <th>Vendor</th>
                    <th>Model ID</th>
                    <th>Dialect</th>
                    <th>Base URL</th>
                    <th>Caps</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  <For each={models()}>
                    {(m) => (
                      <tr>
                        <td>
                          <span class="proxima-mono">{m.vendor}</span>
                        </td>
                        <td>
                          <span class="proxima-mono">{m.model_id}</span>
                        </td>
                        <td>
                          <span class="proxima-mono">{m.dialect}</span>
                        </td>
                        <td>
                          <span class="proxima-mono">{m.base_url}</span>
                        </td>
                        <td>
                          <LlmCapsBadges caps={m.caps} />
                        </td>
                        <td>
                          <button
                            class="proxima-btn proxima-btn-danger"
                            onClick={() => handleDelete(m.vendor, m.model_id)}
                          >
                            Delete
                          </button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </Show>
          </>
        )}
      </Show>

      <details class="proxima-models-form">
        <summary>Register model</summary>
        <form onSubmit={handleSubmit}>
          <Show when={error()}>
            <p class="proxima-error">{error()}</p>
          </Show>
          <div class="proxima-models-form-grid">
            <label for="llm-vendor">Vendor</label>
            <input
              id="llm-vendor"
              type="text"
              value={formData().vendor}
              onInput={(e) =>
                setFormData({ ...formData(), vendor: e.target.value })
              }
              placeholder="anthropic"
            />

            <label for="llm-model-id">Model ID</label>
            <input
              id="llm-model-id"
              type="text"
              value={formData().model_id}
              onInput={(e) =>
                setFormData({ ...formData(), model_id: e.target.value })
              }
              placeholder="claude-3-7-sonnet-20250219"
            />

            <label for="llm-dialect">Dialect</label>
            <select
              id="llm-dialect"
              value={formData().dialect}
              onChange={(e) =>
                setFormData({ ...formData(), dialect: e.target.value as Dialect })
              }
            >
              <For each={DIALECTS}>
                {(d) => <option value={d}>{d}</option>}
              </For>
            </select>

            <label for="llm-base-url">Base URL</label>
            <input
              id="llm-base-url"
              type="text"
              value={formData().base_url}
              onInput={(e) =>
                setFormData({ ...formData(), base_url: e.target.value })
              }
              placeholder="https://api.anthropic.com"
            />

            <label for="llm-secret-ref">Secret Ref</label>
            <input
              id="llm-secret-ref"
              type="text"
              value={formData().secret_ref}
              onInput={(e) =>
                setFormData({ ...formData(), secret_ref: e.target.value })
              }
              placeholder="keychain://anthropic_api_key"
            />

            <label>Capabilities</label>
            <div class="checkbox-row">
              <label>
                <input
                  type="checkbox"
                  checked={formData().tool_use}
                  onChange={(e) =>
                    setFormData({ ...formData(), tool_use: e.target.checked })
                  }
                />
                tool_use
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={formData().json_mode}
                  onChange={(e) =>
                    setFormData({ ...formData(), json_mode: e.target.checked })
                  }
                />
                json_mode
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={formData().long_context}
                  onChange={(e) =>
                    setFormData({ ...formData(), long_context: e.target.checked })
                  }
                />
                long_context
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={formData().vision}
                  onChange={(e) =>
                    setFormData({ ...formData(), vision: e.target.checked })
                  }
                />
                vision
              </label>
            </div>
          </div>
          <div class="proxima-models-form-actions">
            <button type="submit" class="proxima-btn">
              Register
            </button>
          </div>
        </form>
      </details>
    </section>
  );
};
