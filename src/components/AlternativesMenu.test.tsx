import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AlternativesMenu } from "./AlternativesMenu";

const anchor = () => {
  const element = document.createElement("mark");
  document.body.append(element);
  return element;
};

describe("AlternativesMenu", () => {
  it("lists the alternatives, focuses the first and picks one", () => {
    const onPick = vi.fn();
    render(
      <AlternativesMenu
        word="report"
        anchor={anchor()}
        state={{ status: "ready", items: [{ text: "document", probability: 0 }, { text: "summary", probability: 0 }] }}
        onPick={onPick}
        onClose={vi.fn()}
      />,
    );
    const items = screen.getAllByRole("menuitem");
    expect(items.map((item) => item.textContent)).toEqual(["document", "summary"]);
    expect(document.activeElement).toBe(items[0]);
    fireEvent.keyDown(items[0], { key: "ArrowDown" });
    expect(document.activeElement).toBe(items[1]);
    fireEvent.click(items[1]);
    expect(onPick).toHaveBeenCalledWith("summary");
  });

  it("closes on Escape and on a click elsewhere", () => {
    const onClose = vi.fn();
    render(<AlternativesMenu word="report" anchor={anchor()} state={{ status: "loading" }} onPick={vi.fn()} onClose={onClose} />);
    expect(screen.getByRole("status").textContent).toContain("Finding other words");
    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.pointerDown(document.body);
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("explains when nothing fits or the lookup failed", () => {
    const view = render(<AlternativesMenu word="the" anchor={anchor()} state={{ status: "ready", items: [] }} onPick={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByRole("status").textContent).toBe("No other words fit here.");
    view.rerender(<AlternativesMenu word="the" anchor={anchor()} state={{ status: "error", message: "This paragraph is too long." }} onPick={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByRole("alert").textContent).toBe("This paragraph is too long.");
  });
});
