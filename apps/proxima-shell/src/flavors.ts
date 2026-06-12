import { init as initCode } from "@proxima/flavor-code";
import { init as initGoal } from "@proxima/flavor-goal";
import { init as initAgentMemory } from "@proxima/flavor-agent-memory";

export function initFlavors(): void {
  initCode();
  initGoal();
  initAgentMemory();
}
