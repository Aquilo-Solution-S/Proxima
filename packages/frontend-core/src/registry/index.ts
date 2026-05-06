import type { Component } from "solid-js";
import type { EntityKind, MemoryRow } from "../bindings";
import type { PayloadCodec, Renderer, RegisteredView } from "../hub";

export interface PayloadRendererComponentProps<T = unknown> {
  memory: MemoryRow;
  payload: T;
}

export type PayloadRendererComponent<T = unknown> = Component<
  PayloadRendererComponentProps<T>
>;

export interface PayloadRendererRegistration<T = unknown> {
  schemaId: string;
  schemaVersion?: number;
  kind?: EntityKind;
  flavor: string;
  codec?: PayloadCodec<T>;
  component?: PayloadRendererComponent<T>;
  renderer?: Renderer<T>;
}

export interface EdgeStyle {
  color?: number;
  highlightColor?: number;
  opacity?: number;
}

export interface EdgeStyleRegistration {
  relationId: string;
  style: EdgeStyle;
}

export interface ShellViewRegistration {
  id: string;
  route: string;
  label: string;
  component: Component;
  flavor: string;
}

export interface RegisteredPayloadRenderer {
  schemaId: string;
  schemaVersion: number;
  kind: EntityKind | null;
  flavor: string;
  codec: PayloadCodec<unknown> | null;
  renderer: Renderer<unknown>;
}

const payloadRenderers = new Map<string, RegisteredPayloadRenderer>();
const edgeStyles = new Map<string, EdgeStyle>();
const shellViews = new Map<string, ShellViewRegistration>();

const payloadKey = (
  kind: EntityKind | null,
  schemaId: string,
  schemaVersion: number,
): string => `${kind ?? "*"}:${schemaId}@${schemaVersion}`;

const rendererFromComponent = <T,>(
  component: PayloadRendererComponent<T>,
): Renderer<T> => ({
  render: (props) => component(props),
});

export function registerPayloadRenderer<T>(
  registration: PayloadRendererRegistration<T>,
): void {
  const schemaVersion = registration.schemaVersion ?? 1;
  const renderer =
    registration.renderer ??
    (registration.component === undefined
      ? null
      : rendererFromComponent(registration.component));
  if (renderer === null) {
    throw new Error(
      `payload renderer ${registration.schemaId}@${schemaVersion} has no renderer`,
    );
  }
  payloadRenderers.set(
    payloadKey(registration.kind ?? null, registration.schemaId, schemaVersion),
    {
      schemaId: registration.schemaId,
      schemaVersion,
      kind: registration.kind ?? null,
      flavor: registration.flavor,
      codec: (registration.codec as PayloadCodec<unknown> | undefined) ?? null,
      renderer: renderer as Renderer<unknown>,
    },
  );
}

export function registeredPayloadRenderers(): RegisteredPayloadRenderer[] {
  return [...payloadRenderers.values()];
}

export function getPayloadRenderer(
  schemaId: string,
  schemaVersion: number,
  kind?: EntityKind,
): RegisteredPayloadRenderer | null {
  if (kind !== undefined) {
    const exact = payloadRenderers.get(payloadKey(kind, schemaId, schemaVersion));
    if (exact !== undefined) return exact;
  }
  return payloadRenderers.get(payloadKey(null, schemaId, schemaVersion)) ?? null;
}

export function registerEdgeStyle(registration: EdgeStyleRegistration): void {
  edgeStyles.set(registration.relationId, registration.style);
}

export function getEdgeStyle(relationId: string): EdgeStyle | null {
  return edgeStyles.get(relationId) ?? null;
}

export function registerShellView(registration: ShellViewRegistration): void {
  shellViews.set(registration.id, registration);
}

export function registeredShellViews(): RegisteredView[] {
  return [...shellViews.values()].map((view) => ({
    id: view.id,
    label: view.label,
    component: view.component,
    flavor: view.flavor,
  }));
}

export function registeredFlavorNames(): string[] {
  const flavors = new Set<string>();
  for (const renderer of payloadRenderers.values()) flavors.add(renderer.flavor);
  for (const view of shellViews.values()) flavors.add(view.flavor);
  return [...flavors];
}

export function clearRegistriesForTests(): void {
  payloadRenderers.clear();
  edgeStyles.clear();
  shellViews.clear();
}
