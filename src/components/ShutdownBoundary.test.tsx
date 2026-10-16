import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
import { ShutdownBoundary } from "./ShutdownBoundary";

describe("ShutdownBoundary", () => {
  let event: (() => void) | undefined;
  const cleanup = vi.fn();
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {},
      configurable: true,
    });
    cleanup.mockClear();
    mocks.invoke.mockReset().mockResolvedValue(false);
    mocks.listen.mockReset().mockImplementation(async (_name, handler) => {
      event = handler;
      return cleanup;
    });
  });
  afterEach(() => {
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  });
  it("blocks new interaction and announces cleanup without unmounting ongoing work", async () => {
    const { unmount } = render(
      <ShutdownBoundary>
        <button>Start recording</button>
      </ShutdownBoundary>,
    );
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalled());
    const button = screen.getByRole("button", { name: "Start recording" });
    act(() => event?.());
    const dialog = screen.getByRole("dialog", {
      name: "Quitting Murmela…",
    });
    expect(document.activeElement).toBe(dialog);
    expect(button.isConnected).toBe(true);
    expect(button.parentElement?.hasAttribute("inert")).toBe(true);
    unmount();
    expect(cleanup).toHaveBeenCalledTimes(1);
  });
  it("does not let a stale idle snapshot hide an accepted quit", async () => {
    let resolve!: (active: boolean) => void;
    mocks.invoke.mockImplementation(
      () =>
        new Promise<boolean>((r) => {
          resolve = r;
        }),
    );
    render(<ShutdownBoundary>Workspace</ShutdownBoundary>);
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalled());
    act(() => event?.());
    await act(async () => resolve(false));
    expect(screen.getByRole("dialog")).toBeTruthy();
  });
});
