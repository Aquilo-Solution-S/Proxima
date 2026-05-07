import "./personalities.css";

import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  type Component,
} from "solid-js";
import {
  commands,
  type InstantiatePersonalityOutcomeTs,
  type InstantiatePersonalityTs,
  type ListPersonalityInstancesTs,
  type Owner,
  type PersonalityInstanceTs,
  type ProtocolError,
  type SetWakeConfigOutcomeTs,
  type SetWakeConfigTs,
  type TombstonePersonalityOutcomeTs,
  type TombstonePersonalityTs,
  type WakeFilterTs,
} from "../bindings";
import { sentinelOwner } from "../graph-store";
import { Mono } from "../primitives";
import {
  registeredPayloadRenderers,
  registeredPersonalityTypes,
  type RegisteredPersonalityType,
} from "../registry";

type CommandResult<T> = Promise<
  { status: "ok"; data: T } | { status: "error"; error: ProtocolError }
>;

export type PersonalityCommandClient = {
  provisionOwner: (owner: Owner) => CommandResult<null>;
  listPersonalityInstances: (
    req: ListPersonalityInstancesTs,
  ) => CommandResult<PersonalityInstanceTs[]>;
  instantiatePersonality: (
    req: InstantiatePersonalityTs,
  ) => CommandResult<InstantiatePersonalityOutcomeTs>;
  setWakeConfig: (req: SetWakeConfigTs) => CommandResult<SetWakeConfigOutcomeTs>;
  tombstonePersonality: (
    req: TombstonePersonalityTs,
  ) => CommandResult<TombstonePersonalityOutcomeTs>;
};

const emptyOnMemory = (): WakeFilterTs => ({
  kind: "on_memory",
  version: 1,
  schema_id: "proxima-code/commit-summary-v1",
  authored_by: { kind: "any" },
  probability: 1,
});

const emptyOnEdge = (): WakeFilterTs => ({
  kind: "on_edge",
  version: 1,
  relation_id: "core/inspires",
  source: { kind: "any" },
  target: { kind: "self_perspective" },
  probability: 1,
});

const registeredSchemaOptions = () => {
  const seen = new Set<string>();
  return registeredPayloadRenderers()
    .map((registration) => ({
      schemaId: registration.schemaId,
      flavor: registration.flavor,
    }))
    .filter((option) => {
      if (seen.has(option.schemaId)) return false;
      seen.add(option.schemaId);
      return true;
    })
    .sort((a, b) =>
      `${a.flavor}:${a.schemaId}`.localeCompare(`${b.flavor}:${b.schemaId}`),
    );
};

const schemaOptionsFor = (schemaId: string) => {
  const options = registeredSchemaOptions();
  if (options.some((option) => option.schemaId === schemaId)) return options;
  return [{ schemaId, flavor: "stored config" }, ...options];
};

export const PersonalitiesView: Component<{
  client?: PersonalityCommandClient;
  owner?: Owner;
}> = (props) => {
  const owner = props.owner ?? sentinelOwner();
  const client = props.client ?? commands;
  const [instances, setInstances] = createSignal<PersonalityInstanceTs[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal<PersonalityInstanceTs | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [confirmingTombstone, setConfirmingTombstone] = createSignal<string | null>(
    null,
  );
  const [tombstoning, setTombstoning] = createSignal<string | null>(null);
  const personalityTypes = registeredPersonalityTypes();

  const tombstoneInstance = async (instance: PersonalityInstanceTs) => {
    setTombstoning(instance.personality_instance_id);
    setError(null);
    try {
      await unwrap(
        client.tombstonePersonality({
          owner: instance.owner,
          personality_type_id: instance.personality_type_id,
          personality_instance_id: instance.personality_instance_id,
        }),
      );
      setInstances((prev) =>
        prev.filter(
          (row) => row.personality_instance_id !== instance.personality_instance_id,
        ),
      );
      setConfirmingTombstone(null);
      void refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setTombstoning(null);
    }
  };

  const visibleInstances = createMemo(() =>
    [...instances()].sort((a, b) =>
      `${a.flavor.display_name}:${a.display_name}`.localeCompare(
        `${b.flavor.display_name}:${b.display_name}`,
      ),
    ),
  );

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      await unwrap(client.provisionOwner(owner));
      const rows = await unwrap(
        client.listPersonalityInstances({
          owner,
          personality_type_id: null,
          include_tombstoned: false,
        }),
      );
      setInstances(rows);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  };

  createEffect(() => {
    void refresh();
  });

  const createPersonality = async (
    type: RegisteredPersonalityType,
    displayName: string,
    purpose: string,
  ) => {
    setLoading(true);
    setError(null);
    try {
      await unwrap(
        client.instantiatePersonality({
          owner,
          personality_type_id: type.typeId,
          payload_overrides: JSON.stringify({
            display_name: displayName,
            purpose,
          }),
        }),
      );
      setCreating(false);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
      setLoading(false);
    }
  };

  return (
    <section class="personality-view">
      <div class="personality-toolbar">
        <div>
          <h1>Personalities</h1>
          <Show when={error()}>
            {(message) => <p class="proxima-error">{message()}</p>}
          </Show>
        </div>
        <div class="personality-actions">
          <button
            type="button"
            class="hub-nav-item"
            disabled={loading() || personalityTypes.length === 0}
            onClick={() => setCreating(true)}
          >
            Create new Personality
          </button>
          <button type="button" class="hub-nav-item" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
      </div>

      <div class="personality-grid" aria-busy={loading()}>
        <For each={visibleInstances()}>
          {(instance) => (
            <PersonalityCard
              instance={instance}
              onEdit={() => setEditing(instance)}
              confirming={
                confirmingTombstone() === instance.personality_instance_id
              }
              busy={tombstoning() === instance.personality_instance_id}
              onTombstone={() =>
                setConfirmingTombstone(instance.personality_instance_id)
              }
              onCancelTombstone={() => setConfirmingTombstone(null)}
              onConfirmTombstone={() => void tombstoneInstance(instance)}
            />
          )}
        </For>
        <Show when={visibleInstances().length === 0}>
          <p class="personality-empty">No personalities configured.</p>
        </Show>
      </div>

      <Show when={creating()}>
        <CreatePersonalityDialog
          types={personalityTypes}
          busy={loading()}
          onClose={() => setCreating(false)}
          onCreate={(type, displayName, purpose) =>
            void createPersonality(type, displayName, purpose)
          }
        />
      </Show>

      <Show when={editing()}>
        {(instance) => (
          <WakeEditor
            instance={instance()}
            onClose={() => setEditing(null)}
            onSaved={() => {
              setEditing(null);
              void refresh();
            }}
            client={client}
          />
        )}
      </Show>
    </section>
  );
};

export const EngineerInstancesPanel = PersonalitiesView;

const CreatePersonalityDialog: Component<{
  types: RegisteredPersonalityType[];
  busy: boolean;
  onClose: () => void;
  onCreate: (
    type: RegisteredPersonalityType,
    displayName: string,
    purpose: string,
  ) => void;
}> = (props) => {
  const [selectedTypeId, setSelectedTypeId] = createSignal(
    props.types[0]?.typeId ?? "",
  );
  const selectedType = createMemo(
    () => props.types.find((type) => type.typeId === selectedTypeId()) ?? null,
  );
  const [displayName, setDisplayName] = createSignal(
    props.types[0]?.defaultDisplayName ?? "",
  );
  const [purpose, setPurpose] = createSignal(props.types[0]?.defaultPurpose ?? "");

  const chooseType = (type: RegisteredPersonalityType) => {
    setSelectedTypeId(type.typeId);
    setDisplayName(type.defaultDisplayName);
    setPurpose(type.defaultPurpose);
  };

  const create = () => {
    const type = selectedType();
    if (type === null) return;
    props.onCreate(type, displayName().trim(), purpose().trim());
  };

  return (
    <div class="personality-dialog-backdrop" role="dialog" aria-modal="true">
      <div class="personality-dialog">
        <div class="personality-dialog-head">
          <div>
            <h2>Create new Personality</h2>
            <p>Choose a flavor-provided type, then set its instance details.</p>
          </div>
          <button type="button" class="hub-nav-item" onClick={props.onClose}>
            Close
          </button>
        </div>

        <div class="personality-dialog-body">
          <section class="personality-type-picker" aria-label="Personality type">
            <For each={props.types}>
              {(type) => (
                <button
                  type="button"
                  classList={{
                    "personality-type-option": true,
                    selected: selectedTypeId() === type.typeId,
                  }}
                  onClick={() => chooseType(type)}
                >
                  <span>{type.label}</span>
                  <small>{type.flavor}</small>
                  <em>{type.purpose}</em>
                </button>
              )}
            </For>
          </section>

          <section class="personality-detail-form">
            <label>
              Display name
              <input
                value={displayName()}
                onInput={(event) => setDisplayName(event.currentTarget.value)}
              />
            </label>
            <label>
              Purpose
              <textarea
                rows="4"
                value={purpose()}
                onInput={(event) => setPurpose(event.currentTarget.value)}
              />
            </label>
          </section>
        </div>

        <div class="personality-dialog-actions">
          <button
            type="button"
            class="hub-nav-item"
            disabled={props.busy || displayName().trim() === ""}
            onClick={create}
          >
            Create
          </button>
        </div>
      </div>
    </div>
  );
};

const PersonalityCard: Component<{
  instance: PersonalityInstanceTs;
  onEdit: () => void;
  confirming: boolean;
  busy: boolean;
  onTombstone: () => void;
  onCancelTombstone: () => void;
  onConfirmTombstone: () => void;
}> = (props) => {
  const statusClass = () => props.instance.status.replace(/-/g, "_");
  const truncatedName = () => truncateName(props.instance.display_name);
  return (
    <article class="personality-row" data-testid="personality-card">
      <div class="personality-row-head">
        <div class="personality-title">
          <strong>{props.instance.display_name}</strong>
          <div class="personality-meta">
            <span class="personality-flavor-chip" data-testid="personality-flavor-chip">
              Flavor {props.instance.flavor.display_name}
            </span>
            <span>{props.instance.personality_type_id}</span>
            <Mono>{shortId(props.instance.personality_instance_id)}</Mono>
          </div>
        </div>
        <span class={`personality-status ${statusClass()}`}>
          {props.instance.status}
        </span>
      </div>
      <div class="self-perspective-ref">
        <span>Self</span>
        <button
          type="button"
          class="self-perspective-button"
          title={props.instance.current_self_perspective_memory_id}
          onClick={() => void copyText(props.instance.current_self_perspective_memory_id)}
        >
          <Mono>{shortId(props.instance.current_self_perspective_memory_id)}</Mono>
        </button>
      </div>
      <Show when={props.instance.status === "needs_repair"}>
        <div class="repair-banner">
          <span>
            Wake config needs repair - saved filters could not be loaded after a
            recent update.
          </span>
          <button type="button" class="hub-nav-item" onClick={props.onEdit}>
            Re-edit
          </button>
        </div>
      </Show>
      <ul class="wake-list">
        <For each={props.instance.wake_filters}>
          {(filter) => <li class="wake-item">{wakeSummary(filter)}</li>}
        </For>
      </ul>
      <div class="personality-card-actions">
        <button type="button" class="hub-nav-item" onClick={props.onEdit}>
          Edit wake config
        </button>
        <button
          type="button"
          class="hub-nav-item danger"
          disabled={props.confirming || props.busy}
          onClick={props.onTombstone}
        >
          Tombstone
        </button>
      </div>
      <Show when={props.confirming}>
        <div class="tombstone-confirm" data-testid="tombstone-confirm">
          <span>{`Tombstone ${truncatedName()}? Wakes stop; memories remain.`}</span>
          <button
            type="button"
            class="hub-nav-item danger"
            disabled={props.busy}
            onClick={props.onConfirmTombstone}
          >
            Confirm tombstone
          </button>
          <button
            type="button"
            class="hub-nav-item"
            disabled={props.busy}
            onClick={props.onCancelTombstone}
          >
            Cancel
          </button>
        </div>
      </Show>
    </article>
  );
};

const truncateName = (name: string): string =>
  name.length > 48 ? `${name.slice(0, 45)}...` : name;

const WakeEditor: Component<{
  instance: PersonalityInstanceTs;
  onClose: () => void;
  onSaved: () => void;
  client: PersonalityCommandClient;
}> = (props) => {
  const [filters, setFilters] = createSignal<WakeFilterTs[]>(
    props.instance.status === "needs_repair"
      ? []
      : props.instance.wake_filters.map(cloneFilter),
  );
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);

  const updateFilter = (index: number, next: WakeFilterTs) => {
    setFilters((prev) => prev.map((filter, i) => (i === index ? next : filter)));
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await unwrap(
        props.client.setWakeConfig({
          owner: props.instance.owner,
          personality_type_id: props.instance.personality_type_id,
          personality_instance_id: props.instance.personality_instance_id,
          wake_filters: filters(),
        }),
      );
      props.onSaved();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="wake-editor-backdrop" role="dialog" aria-modal="true">
      <div class="wake-editor">
        <div class="wake-editor-head">
          <div>
            <h2>{props.instance.display_name}</h2>
            <Mono>{props.instance.personality_instance_id}</Mono>
            <Show when={props.instance.status === "needs_repair"}>
              <p class="proxima-error">wake config needs repair</p>
            </Show>
            <Show when={error()}>
              {(message) => <p class="proxima-error">{message()}</p>}
            </Show>
          </div>
          <button type="button" class="hub-nav-item" onClick={props.onClose}>
            Close
          </button>
        </div>

        <div class="wake-editor-list" data-testid="wake-filters-list">
          <For each={filters()}>
            {(filter, index) => (
              <WakeFilterEditor
                filter={filter}
                onChange={(next) => updateFilter(index(), next)}
                onRemove={() =>
                  setFilters((prev) => prev.filter((_, i) => i !== index()))
                }
              />
            )}
          </For>
        </div>

        <div class="wake-editor-actions">
          <div class="personality-actions">
            <button
              type="button"
              class="hub-nav-item"
              onClick={() => setFilters((prev) => [...prev, emptyOnMemory()])}
            >
              Add OnMemory
            </button>
            <button
              type="button"
              class="hub-nav-item"
              onClick={() => setFilters((prev) => [...prev, emptyOnEdge()])}
            >
              Add OnEdge
            </button>
          </div>
          <button
            type="button"
            class="hub-nav-item"
            disabled={saving()}
            onClick={() => void save()}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  );
};

const WakeFilterEditor: Component<{
  filter: WakeFilterTs;
  onChange: (next: WakeFilterTs) => void;
  onRemove: () => void;
}> = (props) => {
  const probabilityPromille = () =>
    String(Math.round(props.filter.probability * 1000));
  const changeProbabilityPromille = (value: string) => {
    const promille = Number(value);
    if (Number.isFinite(promille)) {
      const probability = Math.max(0, Math.min(1000, promille)) / 1000;
      props.onChange({ ...props.filter, probability });
    }
  };

  return (
    <div class="wake-editor-row">
      <label>
        Kind
        <select
          value={props.filter.kind}
          onChange={(event) =>
            props.onChange(
              event.currentTarget.value === "on_edge"
                ? emptyOnEdge()
                : emptyOnMemory(),
            )
          }
        >
          <option value="on_memory">OnMemory</option>
          <option value="on_edge">OnEdge</option>
        </select>
      </label>
      <Show
        when={props.filter.kind === "on_memory"}
        fallback={<OnEdgeFields filter={props.filter} onChange={props.onChange} />}
      >
        <OnMemoryFields filter={props.filter} onChange={props.onChange} />
      </Show>
      <label>
        Probability (promille)
        <input
          type="number"
          min="0"
          max="1000"
          step="1"
          value={probabilityPromille()}
          onInput={(event) =>
            changeProbabilityPromille(event.currentTarget.value)
          }
        />
      </label>
      <button type="button" class="hub-nav-item" onClick={props.onRemove}>
        Remove
      </button>
    </div>
  );
};

const OnMemoryFields: Component<{
  filter: WakeFilterTs;
  onChange: (next: WakeFilterTs) => void;
}> = (props) => {
  const filter = props.filter;
  if (filter.kind !== "on_memory") return null;
  return (
    <>
      <label>
        Schema
        <select
          value={filter.schema_id}
          onChange={(event) =>
            props.onChange({ ...filter, schema_id: event.currentTarget.value })
          }
        >
          <For each={schemaOptionsFor(filter.schema_id)}>
            {(option) => (
              <option value={option.schemaId}>
                {option.schemaId} ({option.flavor})
              </option>
            )}
          </For>
        </select>
      </label>
      <label>
        Author
        <select
          value={filter.authored_by.kind}
          onChange={(event) =>
            props.onChange({
              ...filter,
              authored_by:
                event.currentTarget.value === "external"
                  ? { kind: "external" }
                  : { kind: "any" },
            })
          }
        >
          <option value="any">Any</option>
          <option value="external">External</option>
        </select>
      </label>
    </>
  );
};

const OnEdgeFields: Component<{
  filter: WakeFilterTs;
  onChange: (next: WakeFilterTs) => void;
}> = (props) => {
  const filter = props.filter;
  if (filter.kind !== "on_edge") return null;
  return (
    <>
      <label>
        Relation
        <input
          value={filter.relation_id}
          onInput={(event) =>
            props.onChange({
              ...filter,
              relation_id: event.currentTarget.value,
            })
          }
        />
      </label>
      <label>
        Target
        <select
          value={filter.target.kind}
          onChange={(event) =>
            props.onChange({
              ...filter,
              target:
                event.currentTarget.value === "self_perspective"
                  ? { kind: "self_perspective" }
                  : { kind: "any" },
            })
          }
        >
          <option value="self_perspective">Self</option>
          <option value="any">Any</option>
        </select>
      </label>
    </>
  );
};

const cloneFilter = (filter: WakeFilterTs): WakeFilterTs =>
  JSON.parse(JSON.stringify(filter)) as WakeFilterTs;

const shortId = (value: string): string => value.slice(0, 8);

const copyText = async (value: string): Promise<void> => {
  await navigator.clipboard?.writeText(value);
};

const wakeSummary = (filter: WakeFilterTs) => {
  switch (filter.kind) {
    case "on_memory":
      return (
        <>
          <span>OnMemory</span>
          <Mono>{filter.schema_id}</Mono>
        </>
      );
    case "on_edge":
      return (
        <>
          <span>OnEdge</span>
          <Mono>{filter.relation_id}</Mono>
        </>
      );
    case "custom":
      return (
        <>
          <span>Custom</span>
          <Mono>{filter.kind_id}</Mono>
        </>
      );
  }
};

const unwrap = async <T, E>(
  result: Promise<{ status: "ok"; data: T } | { status: "error"; error: E }>,
): Promise<T> => {
  const value = await result;
  if (value.status === "error") throw value.error;
  return value.data;
};

const errorMessage = (err: unknown): string => {
  if (err && typeof err === "object" && "message" in err) {
    const message = (err as { message: unknown }).message;
    if (typeof message === "string") return message;
  }
  return String(err);
};
