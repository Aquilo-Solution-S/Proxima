import { Show, createSignal, type Component } from "solid-js";

interface Props {
  busy: boolean;
  error: () => string | null;
  onClose: () => void;
  onCreate: (displayName: string, purpose: string) => void;
}

export const CreatePersonalityDialog: Component<Props> = (props) => {
  const [displayName, setDisplayName] = createSignal("");
  const [purpose, setPurpose] = createSignal("");

  const canSubmit = () =>
    !props.busy &&
    displayName().trim() !== "" &&
    purpose().trim() !== "";

  const create = () => {
    if (!canSubmit()) return;
    props.onCreate(displayName().trim(), purpose().trim());
  };

  return (
    <div class="personality-dialog-backdrop" role="dialog" aria-modal="true">
      <div class="personality-dialog">
        <div class="personality-dialog-head">
          <div>
            <h2>Create new Personality</h2>
            <p>
              Purpose frames every wake as the system-prompt baseline.
              Per-trigger behavior is configured later on WakeEntries.
            </p>
          </div>
          <button type="button" class="hub-nav-item" onClick={props.onClose}>
            Close
          </button>
        </div>

        <Show when={props.error()}>
          {(message) => (
            <p class="proxima-error personality-dialog-error" role="alert">
              {message()}
            </p>
          )}
        </Show>

        <div class="personality-dialog-body">
          <section class="personality-detail-form">
            <label>
              Display name
              <input
                value={displayName()}
                placeholder="Name this instance"
                onInput={(event) => setDisplayName(event.currentTarget.value)}
              />
            </label>
            <div class="personality-field-group">
              <label>
                Purpose
                <textarea
                  rows="3"
                  value={purpose()}
                  placeholder="e.g. Develop perspectives on code changes"
                  onInput={(event) => setPurpose(event.currentTarget.value)}
                />
              </label>
              <span class="personality-field-hint">
                Short intent statement — used verbatim as the system-prompt
                baseline for every wake. Wake entries layer per-trigger
                context on top.
              </span>
            </div>
          </section>
        </div>

        <div class="personality-dialog-actions">
          <button
            type="button"
            class="hub-nav-item"
            disabled={!canSubmit()}
            onClick={create}
          >
            Create
          </button>
        </div>
      </div>
    </div>
  );
};
