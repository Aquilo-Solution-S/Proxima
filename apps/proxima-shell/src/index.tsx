import { render } from "solid-js/web";
import App from "./App";
import { installPerf, record } from "./perf";

const bootStart = performance.now();
installPerf();

window.addEventListener("unhandledrejection", (event) => {
  const reason = event.reason as unknown;
  const cause =
    reason instanceof Error
      ? (reason as Error & { cause?: unknown }).cause
      : undefined;
  console.error("unhandled rejection:", reason, cause ? { cause } : "");
});

window.addEventListener("error", (event) => {
  console.error("uncaught error:", event.error ?? event.message);
});

render(() => <App />, document.getElementById("root") as HTMLElement);

queueMicrotask(() => record("render", "boot_to_first_render", performance.now() - bootStart));
