import {
  createSignal,
  type Accessor,
  type Component,
  type JSX,
  type Setter,
} from "solid-js";
import type { MemoryRow } from "./bindings";

export interface Renderer<T = unknown> {
  render: (props: { memory: MemoryRow; payload: T }) => JSX.Element;
}

export interface RegisteredView {
  id: string;
  label: string;
  component: Component;
  /** `null` for substrate-owned views; flavor name for flavor-registered. */
  flavor: string | null;
}

export interface FlavorScope {
  registerRenderer<T>(
    schemaId: string,
    schemaVersion: number,
    renderer: Renderer<T>,
  ): void;
  registerView(view: {
    id: string;
    label: string;
    component: Component;
  }): void;
}

export interface Hub {
  registerFlavor(
    name: string,
    register: (scope: FlavorScope) => void,
  ): void;
  rendererFor(
    schemaId: string,
    schemaVersion: number,
  ): Renderer<unknown> | null;
  views: Accessor<RegisteredView[]>;
  currentView: Accessor<string>;
  setCurrentView: Setter<string>;
  registeredFlavors: Accessor<string[]>;
}

const rendererKey = (id: string, v: number): string => `${id}@${v}`;

export function createHub(substrateViews: RegisteredView[]): Hub {
  const renderers = new Map<string, Renderer<unknown>>();
  const [views, setViews] = createSignal<RegisteredView[]>(substrateViews);
  const [flavors, setFlavors] = createSignal<string[]>([]);
  const [currentView, setCurrentView] = createSignal<string>(
    substrateViews[0]?.id ?? "",
  );

  return {
    registerFlavor(name, register) {
      setFlavors((prev) => [...prev, name]);
      register({
        registerRenderer: (sid, sver, r) => {
          renderers.set(rendererKey(sid, sver), r as Renderer<unknown>);
        },
        registerView: (view) => {
          setViews((prev) => [...prev, { ...view, flavor: name }]);
        },
      });
    },
    rendererFor: (sid, sver) =>
      renderers.get(rendererKey(sid, sver)) ?? null,
    views,
    currentView,
    setCurrentView,
    registeredFlavors: flavors,
  };
}
