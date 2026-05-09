import { createResource, createSignal, Show, type Component } from "solid-js";
import { commands, type McpConnectionTs } from "../bindings";
import { LoadingSurface } from "../primitives";

async function loadMcpConnection(): Promise<McpConnectionTs> {
  const result = await commands.mcpConnectionGet();
  if (result.status === "error") throw result.error;
  return result.data;
}

export const SettingsMcpPanel: Component = () => {
  const [connection, { refetch }] = createResource(loadMcpConnection);
  const [copied, setCopied] = createSignal<string | null>(null);
  const [confirmingRotate, setConfirmingRotate] = createSignal(false);
  const [rotating, setRotating] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const copyValue = async (label: string, value: string | null | undefined) => {
    if (!value) return;
    await navigator.clipboard.writeText(value);
    setCopied(label);
    window.setTimeout(() => setCopied((current) => (current === label ? null : current)), 1400);
  };

  const rotate = async () => {
    setRotating(true);
    setError(null);
    try {
      const result = await commands.mcpMasterTokenRotate();
      if (result.status === "error") throw result.error;
      setConfirmingRotate(false);
      refetch();
    } catch (err) {
      setError(String(err));
    } finally {
      setRotating(false);
    }
  };

  return (
    <div class="proxima-settings-panel">
      <h2>MCP</h2>
      <Show when={connection.loading}>
        <LoadingSurface mode="inline" label="Loading" size={36} />
      </Show>
      <Show when={connection.error || error()}>
        <p class="proxima-error">{String(connection.error ?? error())}</p>
      </Show>
      <Show when={connection()}>
        {(conn) => (
          <>
            <div class="proxima-mcp-fields">
              <McpField
                label="Endpoint"
                value={conn().url ?? "(listener unavailable)"}
                canCopy={conn().url !== null}
                copied={copied() === "endpoint"}
                onCopy={() => copyValue("endpoint", conn().url)}
              />
              <McpField
                label="Token"
                value={conn().token}
                copied={copied() === "token"}
                onCopy={() => copyValue("token", conn().token)}
              />
              <McpField
                label="Authorization"
                value={conn().authorization_header}
                copied={copied() === "authorization"}
                onCopy={() => copyValue("authorization", conn().authorization_header)}
              />
            </div>
            <div class="proxima-mcp-actions">
              <Show
                when={confirmingRotate()}
                fallback={
                  <button
                    type="button"
                    class="hub-nav-item"
                    onClick={() => setConfirmingRotate(true)}
                  >
                    Rotate token
                  </button>
                }
              >
                <span class="proxima-dim">Replace current token?</span>
                <button
                  type="button"
                  class="hub-nav-item danger"
                  disabled={rotating()}
                  onClick={rotate}
                >
                  Confirm
                </button>
                <button
                  type="button"
                  class="hub-nav-item"
                  disabled={rotating()}
                  onClick={() => setConfirmingRotate(false)}
                >
                  Cancel
                </button>
              </Show>
            </div>
          </>
        )}
      </Show>
    </div>
  );
};

const McpField: Component<{
  label: string;
  value: string;
  canCopy?: boolean;
  copied: boolean;
  onCopy: () => void;
}> = (props) => (
  <div class="proxima-mcp-field">
    <div>
      <div class="proxima-mcp-label">{props.label}</div>
      <code>{props.value}</code>
    </div>
    <button
      type="button"
      class="hub-nav-item"
      disabled={props.canCopy === false}
      onClick={props.onCopy}
    >
      {props.copied ? "Copied" : "Copy"}
    </button>
  </div>
);
