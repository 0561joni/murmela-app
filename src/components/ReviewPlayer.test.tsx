import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { createRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReviewAudio, ReviewRef } from "../types/domain";
const mocks = vi.hoisted(() => ({
  resolve: vi.fn(),
  release: vi.fn(),
  pick: vi.fn(),
}));
vi.mock("../lib/reviewApi", async (original) => ({
  ...(await original<typeof import("../lib/reviewApi")>()),
  resolveReviewAudio: mocks.resolve,
  releaseReviewAudio: mocks.release,
}));
vi.mock("../lib/api", () => ({ pickAudioFiles: mocks.pick }));
import { ReviewPlayer, type ReviewPlayerHandle } from "./ReviewPlayer";
import { ReviewApiError } from "../lib/reviewApi";
const reference: ReviewRef = { kind: "saved", id: "recording" };
const resource = (token = "original"): ReviewAudio => ({
  token,
  url: `http://127.0.0.1:4000/audio/${token}`,
  durationMs: 120000,
});
beforeEach(() => {
  mocks.resolve.mockReset().mockResolvedValue(resource());
  mocks.release.mockReset().mockResolvedValue(undefined);
  mocks.pick.mockReset();
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(function (
    this: HTMLMediaElement,
  ) {
    this.dispatchEvent(new Event("play"));
    return Promise.resolve();
  });
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(function (
    this: HTMLMediaElement,
  ) {
    this.dispatchEvent(new Event("pause"));
  });
});
afterEach(() => vi.restoreAllMocks());
async function player() {
  const ref = createRef<ReviewPlayerHandle>();
  const onTime = vi.fn();
  const view = render(
    <ReviewPlayer
      ref={ref}
      reference={reference}
      durationMs={120000}
      onTime={onTime}
      onResolved={vi.fn()}
    />,
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Play" }).hasAttribute("disabled"),
    ).toBe(false),
  );
  const audio = view.container.querySelector("audio")!;
  Object.defineProperty(audio, "duration", { configurable: true, value: 120 });
  Object.defineProperty(audio, "readyState", { configurable: true, value: 1 });
  fireEvent.loadedMetadata(audio);
  return { ...view, ref, audio, onTime };
}
describe("Original audio player", () => {
  it("plays timestamps, seeks in both directions, changes speed and reports playback position", async () => {
    const { audio, ref, onTime } = await player();
    await act(async () => ref.current?.seek(42000));
    expect(audio.currentTime).toBe(42);
    expect(screen.getByRole("button", { name: "Pause" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Back 10 seconds" }));
    expect(audio.currentTime).toBe(32);
    fireEvent.click(screen.getByRole("button", { name: "Forward 10 seconds" }));
    expect(audio.currentTime).toBe(42);
    fireEvent.change(screen.getByLabelText("Playback speed"), {
      target: { value: "1.5" },
    });
    expect(audio.playbackRate).toBe(1.5);
    fireEvent.change(screen.getByLabelText("Seek audio"), {
      target: { value: "90" },
    });
    expect(audio.currentTime).toBe(90);
    fireEvent.timeUpdate(audio);
    expect(onTime).toHaveBeenLastCalledWith(90000);
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(screen.getByRole("button", { name: "Play" })).toBeTruthy();
  });
  it("converts unsupported playback codecs once and keeps the playback position", async () => {
    const { audio, unmount } = await player();
    audio.currentTime = 37;
    mocks.resolve.mockResolvedValueOnce(resource("fallback"));
    fireEvent.error(audio);
    await waitFor(() => expect(audio.src).toContain("fallback"));
    expect(mocks.resolve).toHaveBeenLastCalledWith(reference, null, true);
    fireEvent.loadedMetadata(audio);
    expect(audio.currentTime).toBe(37);
    expect(mocks.release).toHaveBeenCalledWith("original");
    fireEvent.error(audio);
    expect(await screen.findByText(/including after conversion/)).toBeTruthy();
    expect(mocks.resolve).toHaveBeenCalledTimes(2);
    unmount();
    expect(mocks.release).toHaveBeenCalledWith("fallback");
  });
  it("offers relinking after a missing or mismatched source and preserves reading", async () => {
    mocks.resolve.mockRejectedValueOnce(
      new ReviewApiError(
        "SOURCE_FILE_REQUIRED",
        "Locate the original recording.",
      ),
    );
    render(
      <ReviewPlayer
        reference={reference}
        durationMs={120000}
        onTime={vi.fn()}
        onResolved={vi.fn()}
      />,
    );
    await screen.findByRole("button", { name: "Locate original audio" });
    mocks.pick.mockResolvedValue([{ filePath: "/moved/recording.mp3" }]);
    mocks.resolve.mockRejectedValueOnce(
      new ReviewApiError(
        "SOURCE_FILE_MISMATCH",
        "This is a different recording.",
      ),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Locate original audio" }),
    );
    await screen.findByText("This is a different recording.");
    expect(mocks.resolve).toHaveBeenLastCalledWith(
      reference,
      "/moved/recording.mp3",
      false,
    );
    expect(
      screen.getByRole("button", { name: "Locate original audio" }),
    ).toBeTruthy();
  });
  it("releases a resolution that completes after navigation", async () => {
    let resolve!: (value: ReviewAudio) => void;
    mocks.resolve.mockReturnValueOnce(
      new Promise<ReviewAudio>((r) => {
        resolve = r;
      }),
    );
    const view = render(
      <ReviewPlayer
        reference={reference}
        durationMs={120000}
        onTime={vi.fn()}
        onResolved={vi.fn()}
      />,
    );
    view.unmount();
    await act(async () => resolve(resource("late")));
    expect(mocks.release).toHaveBeenCalledWith("late");
  });
});
