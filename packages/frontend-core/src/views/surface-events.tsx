import { Show, createSignal, type Component, type JSX } from "solid-js";
import type { ChangeEvent, EdgeRow, EntityRef, GoalRow } from "../bindings";
import { useGraph, type DecodedMemory } from "../graph-store";
import type { Hub } from "../hub";
import { Mono, SchemaTag } from "../primitives";
import { VirtualList } from "./virtual-list";

const shortId = (id: string): string => id.slice(0, 8);

const entityRefId = (ref: EntityRef): string =>
  ref.Memory !== undefined ? ref.Memory : ref.Goal!;

const entityRefKind = (ref: EntityRef): "Memory" | "Goal" =>
  ref.Memory !== undefined ? "Memory" : "Goal";

const ownerLabel = (event: ChangeEvent): string => {
  const principal = event.owner.principal.User ?? event.owner.principal.Group!;
  const kind = event.owner.principal.User !== undefined ? "User" : "Group";
  return `${kind} ${shortId(principal)}`;
};

const renderMemoryPayload = (
  memory: DecodedMemory,
  hub: Hub,
): JSX.Element | null => {
  const renderer = hub.rendererFor(
    memory.row.schema_id,
    memory.row.schema_version,
    memory.row.kind,
  );
  return (
    renderer?.render({
      memory: memory.row,
      payload: memory.payload,
    }) ?? null
  );
};

const EventDetailField: Component<{ label: string; children: JSX.Element }> = (
  props,
) => (
  <div class="event-detail-field">
    <span>{props.label}</span>
    <span>{props.children}</span>
  </div>
);

const EventEntityPreview: Component<{
  event: ChangeEvent;
  hub: Hub;
  memoriesById: ReadonlyMap<string, DecodedMemory>;
  goalsById: ReadonlyMap<string, GoalRow>;
  edgesById: ReadonlyMap<string, EdgeRow>;
}> = (props) => {
  const append = () => props.event.kind.EntityAppend;
  const edge = () => props.event.kind.EdgeAppend;
  const memory = () => {
    const entity = append()?.entity;
    if (entity?.Memory === undefined) return null;
    return props.memoriesById.get(entity.Memory) ?? null;
  };
  const goal = () => {
    const entity = append()?.entity;
    if (entity?.Goal === undefined) return null;
    return props.goalsById.get(entity.Goal) ?? null;
  };
  const edgeRow = () => {
    const id = edge()?.edge_id;
    return id === undefined ? null : (props.edgesById.get(id) ?? null);
  };
  const rendered = (): JSX.Element | null => {
    const row = memory();
    return row === null ? null : renderMemoryPayload(row, props.hub);
  };

  return (
    <>
      <Show keyed when={memory()}>
        {(row) => (
          <div class="event-hydrated">
            <div class="event-hydrated-head">
              <SchemaTag
                id={row.row.schema_id}
                version={row.row.schema_version}
              />
              <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
                {row.row.payload.length} bytes
              </Mono>
            </div>
            <Show
              keyed
              when={rendered()}
              fallback={
                <p class="prose prose-small">
                  {row.decodeError?.message ?? "Payload renderer unavailable"}
                </p>
              }
            >
              {(node) => <div class="event-payload-preview">{node}</div>}
            </Show>
          </div>
        )}
      </Show>
      <Show keyed when={goal()}>
        {(row) => (
          <div class="event-hydrated">
            <EventDetailField label="Goal state">{row.state}</EventDetailField>
            <p class="prose prose-small"><strong>{row.title}</strong></p>
            <p class="prose prose-small">{row.text}</p>
          </div>
        )}
      </Show>
      <Show keyed when={edgeRow()}>
        {(row) => (
          <div class="event-hydrated">
            <EventDetailField label="Relation">
              {row.relation}
            </EventDetailField>
            <EventDetailField label="Class">
              {row.relation_class}
            </EventDetailField>
            <EventDetailField label="Payload">
              {row.payload.length} bytes
            </EventDetailField>
          </div>
        )}
      </Show>
      <Show when={memory() === null && goal() === null && edgeRow() === null}>
        <p class="proxima-dim event-hydration-note">Hydration pending</p>
      </Show>
    </>
  );
};

const EventRow: Component<{
  event: ChangeEvent;
  expanded: boolean;
  onToggle: () => void;
  hub: Hub;
  memoriesById: ReadonlyMap<string, DecodedMemory>;
  goalsById: ReadonlyMap<string, GoalRow>;
  edgesById: ReadonlyMap<string, EdgeRow>;
}> = (props) => {
  const append = () => props.event.kind.EntityAppend;
  const edge = () => props.event.kind.EdgeAppend;
  const eventKind = () => append()?.entity_kind ?? "Edge";
  const detailId = () => `event-detail-${props.event.seq}`;

  return (
    <div
      classList={{
        "fact-row": true,
        "event-row": true,
        "is-expanded": props.expanded,
      }}
    >
      <div class="fact-gutter">
        <span class="fact-glyph">CE</span>
      </div>
      <div class="fact-body">
        <button
          type="button"
          class="event-row-toggle"
          aria-label={`${props.expanded ? "Collapse" : "Expand"} event ${shortId(
            props.event.seq,
          )} ${eventKind()}`}
          aria-expanded={props.expanded}
          aria-controls={detailId()}
          onClick={props.onToggle}
        >
          <span class="fact-row-head">
            <Mono style={{ "font-size": "10px" }}>
              {shortId(props.event.seq)}
            </Mono>
            <span class="proxima-dim">{eventKind()}</span>
          </span>
          <span
            classList={{
              "lane-collapse-icon": true,
              "is-collapsed": !props.expanded,
            }}
            aria-hidden="true"
          />
        </button>
        <Show when={props.expanded}>
          <div id={detailId()} class="event-detail">
            <EventDetailField label="Owner">
              {ownerLabel(props.event)}
            </EventDetailField>
            <EventDetailField label="Sequence">
              <Mono>{props.event.seq}</Mono>
            </EventDetailField>
            <Show keyed when={append()}>
              {(body) => (
                <>
                  <EventDetailField label="Schema">
                    {body.schema_id} v{body.schema_version}
                  </EventDetailField>
                  <EventDetailField label={entityRefKind(body.entity)}>
                    <Mono>{entityRefId(body.entity)}</Mono>
                  </EventDetailField>
                  <Show keyed when={body.supersedes}>
                    {(supersedes) => (
                      <EventDetailField label="Supersedes">
                        <Mono>{entityRefId(supersedes)}</Mono>
                      </EventDetailField>
                    )}
                  </Show>
                </>
              )}
            </Show>
            <Show keyed when={edge()}>
              {(body) => (
                <>
                  <EventDetailField label="Relation">
                    {body.relation}
                  </EventDetailField>
                  <EventDetailField label="Edge">
                    <Mono>{body.edge_id}</Mono>
                  </EventDetailField>
                  <EventDetailField label="Source">
                    <Mono>{entityRefId(body.source)}</Mono>
                  </EventDetailField>
                  <EventDetailField label="Target">
                    <Mono>{entityRefId(body.target)}</Mono>
                  </EventDetailField>
                </>
              )}
            </Show>
            <EventEntityPreview
              event={props.event}
              hub={props.hub}
              memoriesById={props.memoriesById}
              goalsById={props.goalsById}
              edgesById={props.edgesById}
            />
          </div>
        </Show>
      </div>
    </div>
  );
};

export const EventStream: Component<{
  collapsed: boolean;
  onToggle: () => void;
  width: number;
  onResizeStart: JSX.EventHandlerUnion<HTMLDivElement, PointerEvent>;
  onResizeKeyDown: JSX.EventHandlerUnion<HTMLDivElement, KeyboardEvent>;
  events: readonly ChangeEvent[];
  hub: Hub;
}> = (props) => {
  const graph = useGraph();
  const [expandedSeq, setExpandedSeq] = createSignal<string | null>(null);

  return (
    <aside
      classList={{
        "event-stream": true,
        "is-collapsed": props.collapsed,
      }}
    >
      <Show
        when={!props.collapsed}
        fallback={
          <button
            type="button"
            class="rail-collapsed-trigger"
            aria-label="Expand Event stream"
            aria-expanded="false"
            onClick={props.onToggle}
          >
            <span class="rail-collapse-icon is-open" aria-hidden="true" />
            <span class="rail-collapsed-title">Event stream</span>
          </button>
        }
      >
        <div
          class="event-stream-resize-handle"
          role="separator"
          aria-label="Resize Event stream"
          aria-orientation="vertical"
          aria-valuemin={220}
          aria-valuemax={560}
          aria-valuenow={props.width}
          tabIndex={0}
          onPointerDown={props.onResizeStart}
          onKeyDown={props.onResizeKeyDown}
        />
        <div class="stream-head">
          <div class="rail-head-copy">
            <span class="rail-title">Event stream</span>
            <Mono style={{ "font-size": "9px", color: "var(--ink-50)" }}>
              append-only
            </Mono>
          </div>
          <button
            type="button"
            class="rail-toggle"
            aria-label="Collapse Event stream"
            aria-expanded="true"
            onClick={props.onToggle}
          >
            <span class="rail-collapse-icon is-right" aria-hidden="true" />
          </button>
        </div>
        <Show
          when={props.events.length > 0}
          fallback={<p class="proxima-dim surface-empty">No events</p>}
        >
          <VirtualList
            class="stream-list"
            items={props.events}
            itemKey={(event) => event.seq}
            estimateSize={56}
            overscan={14}
          >
            {(event) => (
              <EventRow
                event={event}
                expanded={expandedSeq() === event.seq}
                onToggle={() =>
                  setExpandedSeq((current) =>
                    current === event.seq ? null : event.seq,
                  )
                }
                hub={props.hub}
                memoriesById={graph.state().memoriesById}
                goalsById={graph.state().goalsById}
                edgesById={graph.state().edgesById}
              />
            )}
          </VirtualList>
        </Show>
      </Show>
    </aside>
  );
};
