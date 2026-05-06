import { render } from "solid-js/web";
import App from "./App";
import { installPerf, record } from "./perf";

const bootStart = performance.now();
installPerf();

render(() => <App />, document.getElementById("root") as HTMLElement);

queueMicrotask(() => record("render", "boot_to_first_render", performance.now() - bootStart));
