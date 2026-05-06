import { init as initCode } from "@proxima/flavor-code";
import { init as initMcp } from "@proxima/flavor-mcp";

export function initFlavors(): void {
  initCode();
  initMcp();
}
