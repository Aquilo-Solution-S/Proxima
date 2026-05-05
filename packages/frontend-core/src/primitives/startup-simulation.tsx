import { createSignal, onCleanup, onMount, type Component } from "solid-js";
import type { ProximaLoaderElement } from "./loader";
import "./proxima-loader.js";

type StartupSimulationProps = {
  onComplete: () => void;
};

const clamp = (min: number, value: number, max: number): number =>
  Math.max(min, Math.min(value, max));

export const StartupSimulation: Component<StartupSimulationProps> = (props) => {
  let root!: HTMLDivElement;
  let mount!: HTMLDivElement;
  const [exiting, setExiting] = createSignal(false);

  onMount(() => {
    const loader = document.createElement("proxima-loader") as ProximaLoaderElement;
    loader.setAttribute("theme", "dark");
    loader.setAttribute("stars", "on");
    loader.setAttribute("speed", "0.92");
    loader.setAttribute("aria-label", "Starting Proxima");
    mount.replaceChildren(loader);

    let finished = false;
    let progressRaf = 0;
    const timers: number[] = [];

    const finish = () => {
      if (finished) return;
      finished = true;
      setExiting(true);
      timers.push(window.setTimeout(() => props.onComplete(), 180));
    };

    const setLoaderSize = () => {
      const bounds = root.getBoundingClientRect();
      const basis = Math.min(bounds.width, bounds.height || window.innerHeight);
      loader.setAttribute("size", String(Math.round(clamp(220, basis * 0.44, 360))));
    };

    setLoaderSize();
    const observer =
      "ResizeObserver" in window ? new ResizeObserver(setLoaderSize) : null;
    observer?.observe(root);

    const reducedMotion = window.matchMedia?.(
      "(prefers-reduced-motion: reduce)",
    ).matches ?? false;

    if (reducedMotion) {
      loader.progress = 1;
      timers.push(window.setTimeout(finish, 600));
    } else {
      const startedAt = performance.now();
      const progressDuration = 520;
      const tickProgress = (now: number) => {
        const raw = clamp(0, (now - startedAt) / progressDuration, 1);
        const eased = 1 - Math.pow(1 - raw, 3);
        loader.progress = 0.04 + eased * 0.72;
        if (raw < 1 && !finished) {
          progressRaf = requestAnimationFrame(tickProgress);
        }
      };

      progressRaf = requestAnimationFrame(tickProgress);
      loader.addEventListener("complete", finish, { once: true });
      timers.push(
        window.setTimeout(() => {
          cancelAnimationFrame(progressRaf);
          loader.complete();
        }, 180),
        window.setTimeout(finish, 1850),
      );
    }

    onCleanup(() => {
      finished = true;
      cancelAnimationFrame(progressRaf);
      observer?.disconnect();
      timers.forEach((timer) => window.clearTimeout(timer));
      loader.remove();
    });
  });

  return (
    <div
      ref={root}
      classList={{
        "startup-simulation": true,
        "is-exiting": exiting(),
      }}
      aria-busy="true"
    >
      <div ref={mount} class="startup-simulation-loader" />
    </div>
  );
};
