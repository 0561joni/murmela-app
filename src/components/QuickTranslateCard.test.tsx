import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { settingsFixture } from "../test/fixtures";

vi.mock("../lib/quickTranslateApi", async () => {
  const actual = await vi.importActual<typeof import("../lib/quickTranslateApi")>("../lib/quickTranslateApi");
  return {
    ...actual,
    systemTranslationLanguage: vi.fn(async () => "de"),
    describeApps: vi.fn(async (ids: string[]) => ids.map((bundleId) => ({ bundleId, name: bundleId === "com.apple.keychainaccess" ? "Keychain Access" : null }))),
    pickApp: vi.fn(async () => ({ bundleId: "com.apple.Terminal", name: "Terminal" })),
  };
});

import { QuickTranslateCard } from "./QuickTranslateCard";

function renderCard(overrides = {}) {
  const onChange = vi.fn(async () => undefined);
  render(
    <QuickTranslateCard
      settings={{ ...settingsFixture, ...overrides }}
      isSaving={false}
      hasEngine
      capturing={false}
      onChange={onChange}
      onCaptureShortcut={vi.fn(async () => undefined)}
      onCancelCapture={vi.fn(async () => undefined)}
      feedback={() => null}
    />,
  );
  return onChange;
}

describe("QuickTranslateCard", () => {
  it("defaults to ⌘C twice and switches to a key combination", () => {
    const onChange = renderCard();
    expect(screen.getByRole("button", { name: "⌘C twice" }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Key combination" }));
    expect(onChange).toHaveBeenCalledWith("quickTranslateShortcut", "Ctrl+Alt+T");
  });

  it("shows the macOS language and names the ignored apps", async () => {
    const onChange = renderCard();
    expect(await screen.findByRole("option", { name: "Automatic (German, from macOS)" })).toBeTruthy();
    expect(await screen.findByText("Keychain Access")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Stop ignoring Keychain Access" }));
    expect(onChange).toHaveBeenCalledWith(
      "quickTranslateIgnoredApps",
      settingsFixture.quickTranslateIgnoredApps.filter((id) => id !== "com.apple.keychainaccess"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Add app…" }));
    await waitFor(() =>
      expect(onChange).toHaveBeenCalledWith("quickTranslateIgnoredApps", [...settingsFixture.quickTranslateIgnoredApps, "com.apple.Terminal"]),
    );
  });

  it("chooses your language and where the translation shows", () => {
    const onChange = renderCard();
    fireEvent.change(screen.getByLabelText("Your language"), { target: { value: "es-AR" } });
    expect(onChange).toHaveBeenCalledWith("quickTranslateMyLanguage", "es-AR");
    fireEvent.change(screen.getByLabelText("Show the translation in"), { target: { value: "app" } });
    expect(onChange).toHaveBeenCalledWith("quickTranslateDisplay", "app");
  });
});
