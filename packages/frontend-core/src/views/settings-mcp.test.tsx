import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { commands } from "../bindings";
import { SettingsMcpPanel } from "./settings-mcp";

vi.mock("../bindings", () => ({
  commands: {
    mcpConnectionGet: vi.fn(),
    mcpMasterTokenRotate: vi.fn(),
  },
}));

const connection = (token: string) => ({
  url: "http://127.0.0.1:31415/mcp",
  token,
  authorization_header: `Bearer ${token}`,
  listening: true,
});

describe("SettingsMcpPanel", () => {
  beforeEach(() => {
    Object.defineProperty(globalThis, "matchMedia", {
      configurable: true,
      value: vi.fn(() => ({
        matches: false,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      })),
    });
    HTMLCanvasElement.prototype.getContext = vi.fn(() => null) as never;
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders connection values and copies the token", async () => {
    vi.mocked(commands.mcpConnectionGet).mockResolvedValue({
      status: "ok",
      data: connection("token-1"),
    });
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    render(() => <SettingsMcpPanel />);

    expect(await screen.findByText("http://127.0.0.1:31415/mcp")).toBeTruthy();
    fireEvent.click(screen.getAllByRole("button", { name: "Copy" })[1]);
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("token-1"));
  });

  it("rotates after inline confirmation", async () => {
    vi.mocked(commands.mcpConnectionGet).mockResolvedValue({
      status: "ok",
      data: connection("token-1"),
    });
    vi.mocked(commands.mcpMasterTokenRotate).mockResolvedValue({
      status: "ok",
      data: connection("token-2"),
    });

    render(() => <SettingsMcpPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Rotate token" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    await waitFor(() => expect(commands.mcpMasterTokenRotate).toHaveBeenCalledTimes(1));
  });
});
