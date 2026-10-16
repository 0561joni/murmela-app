import { act, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppearanceProvider, applyAppearance } from "./appearance";
import { settingsFixture } from "../test/fixtures";
vi.mock("./api", () => ({
  getSettings: () =>
    Promise.resolve({ appearance: "system", motionPreference: "system" }),
}));

describe("Appearance across windows", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
  });
  it("resolves explicit themes and respects system reduced motion", () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query.includes("reduced-motion"),
    }));
    applyAppearance({ ...settingsFixture, appearance: "dark" });
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.dataset.motion).toBe("reduced");
    applyAppearance({ ...settingsFixture, appearance: "light" });
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(
      JSON.parse(localStorage.getItem("murmela-appearance")!).appearance,
    ).toBe("light");
  });
  it("reacts to system changes and cleans up listeners", async () => {
    let dark = false;
    const callbacks = new Map<string, () => void>();
    const remove = vi.fn();
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query.includes("color-scheme") && dark,
      addEventListener: (_: string, callback: () => void) =>
        callbacks.set(query, callback),
      removeEventListener: remove,
    }));
    const view = render(
      <AppearanceProvider>
        <span>Workspace</span>
      </AppearanceProvider>,
    );
    await act(async () => Promise.resolve());
    expect(document.documentElement.dataset.theme).toBe("light");
    act(() => {
      dark = true;
      callbacks.get("(prefers-color-scheme: dark)")?.();
    });
    expect(document.documentElement.dataset.theme).toBe("dark");
    view.unmount();
    expect(remove).toHaveBeenCalledTimes(2);
  });
  it("accepts cross-window changes without requiring a reload", async () => {
    const view = render(
      <AppearanceProvider>
        <span>Workspace</span>
      </AppearanceProvider>,
    );
    await act(async () => Promise.resolve());
    act(() =>
      window.dispatchEvent(
        new StorageEvent("storage", {
          key: "murmela-appearance",
          newValue: JSON.stringify({
            appearance: "dark",
            motionPreference: "reduced",
          }),
        }),
      ),
    );
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.dataset.motion).toBe("reduced");
    view.unmount();
  });
});
