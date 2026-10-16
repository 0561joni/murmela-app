import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { IconButton } from "./IconButton";

describe("IconButton", () => {
  it("exposes an accessible name and tooltip without visible label text", () => {
    render(<IconButton icon="trash" label="Delete transcript" />);

    const button = screen.getByRole("button", { name: "Delete transcript" });
    const tooltip = screen.getByRole("tooltip", { name: "Delete transcript" });
    expect(button.getAttribute("aria-describedby")).toBe(tooltip.id);
    expect(button.querySelector(".app-icon")?.getAttribute("aria-hidden")).toBeNull();
    expect(button.querySelector(".app-icon-wrap")?.getAttribute("aria-hidden")).toBe("true");
    expect(tooltip.parentElement).toBe(button);
    expect(button.childNodes).toHaveLength(2);
  });

  it("can receive keyboard focus and preserves button semantics", () => {
    render(<IconButton icon="pencil" label="Rename transcript" />);
    const button = screen.getByRole("button", { name: "Rename transcript" });
    button.focus();
    expect(document.activeElement).toBe(button);
    expect(button.classList.contains("icon-button")).toBe(true);
  });

  it("does not invoke disabled actions", () => {
    const onClick = vi.fn();
    render(<IconButton icon="download" label="Download model" disabled onClick={onClick} />);
    const button = screen.getByRole("button", { name: "Download model" });
    fireEvent.click(button);
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(onClick).not.toHaveBeenCalled();
  });

  it("renders compact, destructive, selected, busy, success, and error states", () => {
    const { rerender } = render(<IconButton icon="trash" label="Delete" size="compact" tone="danger" state="selected" />);
    const button = screen.getByRole("button", { name: "Delete" });
    expect(button.className).toContain("icon-button--compact");
    expect(button.className).toContain("icon-button--danger");
    expect(button.className).toContain("icon-button--selected");

    rerender(<IconButton icon="retry" label="Retry" state="busy" />);
    expect(screen.getByRole("button", { name: "Retry" }).querySelector(".icon-button-spinner")).toBeTruthy();
    rerender(<IconButton icon="check" label="Saved" state="success" />);
    expect(screen.getByRole("button", { name: "Saved" }).className).toContain("icon-button--success");
    rerender(<IconButton icon="xmark" label="Failed" state="error" />);
    expect(screen.getByRole("button", { name: "Failed" }).className).toContain("icon-button--error");
  });
});
