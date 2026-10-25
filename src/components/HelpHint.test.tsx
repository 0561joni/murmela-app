import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { HelpHint } from "./HelpHint";

describe("HelpHint", () => {
  it("opens an explanation on click and closes it with Escape", () => {
    render(
      <HelpHint title="Speaker labels">
        <p>Marks who said what.</p>
      </HelpHint>,
    );
    const button = screen.getByRole("button", { name: "About speaker labels" });
    expect(button.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(button);
    expect(screen.getByRole("dialog", { name: "Speaker labels" }).textContent).toContain("Marks who said what.");
    expect(button.getAttribute("aria-expanded")).toBe("true");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(button);
  });

  it("closes on a click elsewhere and does not toggle a surrounding label's control", () => {
    const onChange = vi.fn();
    render(
      <label>
        <input type="checkbox" onChange={onChange} />
        Option
        <HelpHint title="Option">
          <p>Details.</p>
        </HelpHint>
      </label>,
    );
    fireEvent.click(screen.getByRole("button", { name: "About option" }));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeTruthy();
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
