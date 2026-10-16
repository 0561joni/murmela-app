import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AccessibilityExplainer } from "./AccessibilityExplainer";

describe("AccessibilityExplainer", () => {
  it("explains Apple's wording before continuing to the macOS prompt", async () => {
    const onContinue = vi.fn().mockResolvedValue(undefined);
    render(<AccessibilityExplainer onContinue={onContinue} onCancel={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "Why Murmela asks for Accessibility" })).toBeTruthy();
    expect(screen.getByText(/“control this computer”/)).toBeTruthy();
    const continueButton = screen.getByRole("button", { name: "Continue" });
    expect(document.activeElement).toBe(continueButton);
    fireEvent.click(continueButton);
    await waitFor(() => expect(onContinue).toHaveBeenCalledTimes(1));
  });

  it("closes with Not now or Escape without asking macOS", () => {
    const onContinue = vi.fn();
    const onCancel = vi.fn();
    render(<AccessibilityExplainer onContinue={onContinue} onCancel={onCancel} />);

    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(2);
    expect(onContinue).not.toHaveBeenCalled();
  });
});
