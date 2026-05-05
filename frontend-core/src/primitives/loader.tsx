import {
  Show,
  createEffect,
  onCleanup,
  type Component,
  type JSX,
} from "solid-js";
import "./proxima-loader.js";

export type ProximaLoaderTheme = "dark" | "light";
export type ProximaLoaderStars = "on" | "off";
export type ProximaLoaderMode = "inline" | "panel" | "overlay";

export type ProximaLoaderElement = HTMLElement & {
  progress: number | null;
  label: string;
  complete: (opts?: { then?: () => void }) => void;
  reset: () => void;
};

type LoaderProps = {
  size?: number;
  theme?: ProximaLoaderTheme;
  label?: string;
  progress?: number | null;
  speed?: number;
  stars?: ProximaLoaderStars;
  class?: string;
};

export const ProximaLoader: Component<LoaderProps> = (props) => {
  let mount!: HTMLDivElement;

  createEffect(() => {
    const loader = document.createElement("proxima-loader") as ProximaLoaderElement;
    mount.replaceChildren(loader);

    onCleanup(() => {
      loader.remove();
    });
  });

  createEffect(() => {
    const loader = mount.firstElementChild as ProximaLoaderElement | null;
    if (!loader) return;

    loader.setAttribute("size", String(props.size ?? 72));
    loader.setAttribute("theme", props.theme ?? "dark");
    loader.setAttribute("speed", String(props.speed ?? 1));
    loader.setAttribute("stars", props.stars ?? "off");

    if (props.label) {
      loader.setAttribute("label", props.label);
    } else {
      loader.removeAttribute("label");
    }

    if (props.progress == null) {
      loader.progress = null;
    } else {
      loader.progress = props.progress;
    }
  });

  return (
    <div
      ref={mount}
      classList={{
        "proxima-loader-mount": true,
        [props.class ?? ""]: Boolean(props.class),
      }}
    />
  );
};

type LoadingSurfaceProps = LoaderProps & {
  mode?: ProximaLoaderMode;
  blocking?: boolean;
  active?: boolean;
  children?: JSX.Element;
};

export const LoadingSurface: Component<LoadingSurfaceProps> = (props) => {
  const mode = () => props.mode ?? "panel";
  const active = () => props.active ?? true;
  const blocking = () => props.blocking ?? mode() === "overlay";
  const size = () => props.size ?? (mode() === "inline" ? 32 : 112);
  const stars = () => props.stars ?? (mode() === "inline" ? "off" : "on");

  return (
    <div
      classList={{
        "proxima-loading-surface": true,
        "is-inline": mode() === "inline",
        "is-panel": mode() === "panel",
        "is-overlay": mode() === "overlay",
        "is-blocking": blocking(),
      }}
      aria-busy={active() ? "true" : "false"}
      aria-live={blocking() ? "assertive" : "polite"}
    >
      <Show when={props.children}>{props.children}</Show>
      <Show when={active()}>
        <div class="proxima-loading-layer">
          <ProximaLoader
            size={size()}
            theme={props.theme ?? "dark"}
            label={props.label}
            progress={props.progress}
            speed={props.speed}
            stars={stars()}
          />
        </div>
      </Show>
    </div>
  );
};
