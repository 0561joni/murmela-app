import { act, fireEvent, render, screen } from "@testing-library/react";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import { VirtualPassages, type PassageListHandle } from "./VirtualPassages";
import { reviewFixture } from "../test/reviewFixture";
import { speakerMap } from "../lib/speakerLabels";
describe("Long transcript window", () => {
  it("returns to visible passages after filtering from the end of a long transcript", () => {
    const data = reviewFixture(10000),
      ref = createRef<PassageListHandle>();
    const props = {
      speakers: speakerMap(data.detail.speakers),
      manual: new Set<string>(),
      selected: new Set<string>(),
      activeId: null,
      onSelect: vi.fn(),
      onAssign: vi.fn(),
      onSeek: vi.fn(),
      onManualScroll: vi.fn(),
    };
    const view = render(
      <VirtualPassages {...props} ref={ref} segments={data.detail.segments} />,
    );
    act(() => ref.current?.reveal("passage-9999"));
    view.rerender(
      <VirtualPassages
        {...props}
        ref={ref}
        segments={[data.detail.segments[1]]}
      />,
    );
    expect(screen.getByText(/Passage 2:/)).toBeTruthy();
    expect(screen.getByRole("region")).toHaveProperty("scrollTop", 0);
  });
  it("renders a bounded window for 10,000 passages and seeks to distant passages", () => {
    const data = reviewFixture(10000);
    const ref = createRef<PassageListHandle>();
    const scroll = vi.fn();
    render(
      <VirtualPassages
        ref={ref}
        segments={data.detail.segments}
        speakers={speakerMap(data.detail.speakers)}
        manual={new Set()}
        selected={new Set()}
        activeId={null}
        onSelect={vi.fn()}
        onAssign={vi.fn()}
        onSeek={vi.fn()}
        onManualScroll={scroll}
      />,
    );
    expect(screen.getAllByRole("article").length).toBeLessThan(30);
    expect(screen.queryByText(/Passage 10000:/)).toBeNull();
    act(() => ref.current?.reveal("passage-9999"));
    expect(screen.getByText(/Passage 10000:/)).toBeTruthy();
    expect(screen.getAllByRole("article").length).toBeLessThan(30);
    fireEvent.wheel(
      screen.getByRole("region", { name: "Transcript passages" }),
    );
    expect(scroll).toHaveBeenCalledOnce();
  });
});
