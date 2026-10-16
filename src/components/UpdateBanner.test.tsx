import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const apiMocks = vi.hoisted(() => ({
  installUpdateAndRestart: vi.fn(),
  downloadUpdateAsFree: vi.fn(),
  openProPage: vi.fn(),
}));

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  ...apiMocks,
}));

import { UpdateBanner } from "./UpdateBanner";

const ready = {
  state: "ready" as const,
  currentVersion: "0.1.0",
  version: "0.2.0",
  notes: "Faster live dictation.",
  message: null,
  lastCheckedMs: 1,
  proUpdatesUntil: null,
};

describe("UpdateBanner", () => {
  it("stays hidden until a release is downloaded", () => {
    render(<UpdateBanner status={{ ...ready, state: "downloading" }} />);
    expect(screen.queryByText(/is ready/)).toBeNull();
  });

  it("restarts into the downloaded release", async () => {
    apiMocks.installUpdateAndRestart.mockResolvedValue(undefined);
    render(<UpdateBanner status={ready} />);
    expect(screen.getByText("Murmela 0.2.0 is ready")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Restart to update" }));
    await waitFor(() => expect(apiMocks.installUpdateAndRestart).toHaveBeenCalled());
  });

  it("hides on Later and explains that the update installs on quit", () => {
    render(<UpdateBanner status={ready} />);
    expect(screen.getByText(/installs when you restart or quit Murmela/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Later" }));
    expect(screen.queryByText("Murmela 0.2.0 is ready")).toBeNull();
  });

  describe("with a release the Pro key doesn't cover", () => {
    const renewal = { ...ready, state: "needs_renewal" as const, proUpdatesUntil: "2027-10-16" };

    it("explains why it wasn't downloaded and offers renewing", () => {
      apiMocks.openProPage.mockResolvedValue(undefined);
      render(<UpdateBanner status={renewal} />);
      expect(screen.getByText("Murmela 0.2.0 is out")).toBeTruthy();
      expect(screen.getByText(/ended on October 16, 2027/)).toBeTruthy();
      expect(screen.getByText(/version you have keeps Pro for good/)).toBeTruthy();
      expect(screen.queryByRole("button", { name: "Restart to update" })).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "Renew" }));
      expect(apiMocks.openProPage).toHaveBeenCalled();
    });

    it("downloads it anyway as Free on request", async () => {
      apiMocks.downloadUpdateAsFree.mockResolvedValue({ ...ready, proUpdatesUntil: null });
      render(<UpdateBanner status={renewal} />);
      fireEvent.click(screen.getByRole("button", { name: "Install as Free" }));
      await waitFor(() => expect(apiMocks.downloadUpdateAsFree).toHaveBeenCalledTimes(1));
    });

    it("hides on Later", () => {
      render(<UpdateBanner status={renewal} />);
      fireEvent.click(screen.getByRole("button", { name: "Later" }));
      expect(screen.queryByText("Murmela 0.2.0 is out")).toBeNull();
    });
  });
});
