import { init as initCode } from "@proxima/flavor-code";
import { init as initGoal } from "@proxima/flavor-goal";
import { init as initMcp } from "@proxima/flavor-mcp";

export function initFlavors(): void {
  initCode();
  initGoal();
  initMcp();
}
