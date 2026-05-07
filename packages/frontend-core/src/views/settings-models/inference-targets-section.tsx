import { For, Show, createResource, createSignal, type Component } from "solid-js";
import type { InferenceTargetConfigTs, InferenceTargetTs, Owner } from "../../bindings";
import type { EngineClient } from "../../client";
import { sentinelOwner } from "../../graph-store";

interface Props {
  client: Pick<
    EngineClient,
    "listInferenceTargets" | "registerInferenceTarget" | "removeInferenceTarget"
  >;
  owner?: Owner;
}

const errorMessage = (err: unknown): string => {
  if (err && typeof err === "object") {
    if ("code" in err && "message" in err) {
      const code = (err as { code: unknown }).code;
      const message = (err as { message: unknown }).message;
      return `${String(code)}: ${String(message)}`;
    }
    if ("message" in err) {
      const message = (err as { message: unknown }).message;
      if (typeof message === "string") return message;
    }
  }
  return String(err);
};

const configSummary = (target: InferenceTargetTs): string =>
  JSON.stringify(target.config);

export const InferenceTargetsSection: Component<Props> = (props) => {
  const owner = () => props.owner ?? sentinelOwner();
  const [targets, { refetch }] = createResource(async () =>
    props.client.listInferenceTargets({ owner: owner() }),
  );
  const [targetRef, setTargetRef] = createSignal("");
  const [kind, setKind] = createSignal<InferenceTargetConfigTs["kind"]>("local_cli");
  const [command, setCommand] = createSignal("");
  const [vendor, setVendor] = createSignal("");
  const [dialect, setDialect] = createSignal("");
  const [modelId, setModelId] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);

  const clearForm = () => {
    setTargetRef("");
    setCommand("");
    setVendor("");
    setDialect("");
    setModelId("");
  };

  const submit = async (event: Event) => {
    event.preventDefault();
    setError(null);
    const config: InferenceTargetConfigTs =
      kind() === "local_cli"
        ? {
            kind: "local_cli",
            command: command(),
            profile: null,
            env_overrides: [],
          }
        : {
            kind: "remote_model",
            vendor: vendor(),
            dialect: dialect(),
            model_id: modelId(),
            credentials_ref: null,
          };
    try {
      await props.client.registerInferenceTarget({
        owner: owner(),
        target_ref: targetRef(),
        config,
      });
      clearForm();
      refetch();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const remove = async (targetRefToRemove: string) => {
    setError(null);
    try {
      await props.client.removeInferenceTarget({
        owner: owner(),
        target_ref: targetRefToRemove,
      });
      refetch();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <section>
      <h2>InferenceTargets</h2>
      <Show when={error()}>
        {(message) => <p class="proxima-error" role="alert">{message()}</p>}
      </Show>
      <Show when={(targets() ?? []).length === 0}>
        <p class="proxima-dim">No inference targets registered.</p>
      </Show>
      <Show when={(targets() ?? []).length > 0}>
        <table class="proxima-models-table">
          <thead>
            <tr>
              <th>target_ref</th>
              <th>kind</th>
              <th>config</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <For each={targets() ?? []}>
              {(target) => (
                <tr>
                  <td>
                    <span class="proxima-mono">{target.target_ref}</span>
                  </td>
                  <td>
                    <span class="proxima-mono">{target.config.kind}</span>
                  </td>
                  <td>
                    <code>{configSummary(target)}</code>
                  </td>
                  <td>
                    <button
                      type="button"
                      class="proxima-btn proxima-btn-danger"
                      onClick={() => void remove(target.target_ref)}
                    >
                      remove
                    </button>
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>

      <details class="proxima-models-form">
        <summary>Register target</summary>
        <form onSubmit={(event) => void submit(event)}>
          <div class="proxima-models-form-grid">
            <label for="inference-target-ref">target_ref</label>
            <input
              id="inference-target-ref"
              type="text"
              value={targetRef()}
              onInput={(event) => setTargetRef(event.currentTarget.value)}
            />

            <label for="inference-target-kind">kind</label>
            <select
              id="inference-target-kind"
              value={kind()}
              onChange={(event) =>
                setKind(event.currentTarget.value as InferenceTargetConfigTs["kind"])
              }
            >
              <option value="local_cli">local_cli</option>
              <option value="remote_model">remote_model</option>
            </select>

            <Show when={kind() === "local_cli"}>
              <label for="inference-target-command">command</label>
              <input
                id="inference-target-command"
                type="text"
                value={command()}
                onInput={(event) => setCommand(event.currentTarget.value)}
              />
            </Show>

            <Show when={kind() === "remote_model"}>
              <label for="inference-target-vendor">vendor</label>
              <input
                id="inference-target-vendor"
                type="text"
                value={vendor()}
                onInput={(event) => setVendor(event.currentTarget.value)}
              />

              <label for="inference-target-dialect">dialect</label>
              <input
                id="inference-target-dialect"
                type="text"
                value={dialect()}
                onInput={(event) => setDialect(event.currentTarget.value)}
              />

              <label for="inference-target-model-id">model_id</label>
              <input
                id="inference-target-model-id"
                type="text"
                value={modelId()}
                onInput={(event) => setModelId(event.currentTarget.value)}
              />
            </Show>
          </div>
          <div class="proxima-models-form-actions">
            <button type="submit" class="proxima-btn">
              register
            </button>
          </div>
        </form>
      </details>
    </section>
  );
};
