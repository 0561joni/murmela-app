import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const apiMocks = vi.hoisted(() => ({
  installUpdateAndRestart: vi.fn(),
  downloadUpdate: vi.fn(),
  installUpdateInHomeApplications: vi.fn(),
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
  progress: null,
  proUpdatesUntil: null,
};

describe("UpdateBanner", () => {
  it("announces a new release and downloads it only on request", async () => {
    apiMocks.downloadUpdate.mockResolvedValue({ ...ready });
    render(<UpdateBanner status={{ ...ready, state: "available" }} />);
    expect(screen.getByText("Murmela 0.2.0 is available")).toBeTruthy();
    expect(apiMocks.downloadUpdate).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Download update" }));
    await waitFor(() => expect(apiMocks.downloadUpdate).toHaveBeenCalledTimes(1));
  });

  it("shows download progress without buttons", () => {
    render(<UpdateBanner status={{ ...ready, state: "downloading", progress: 42 }} />);
    expect(screen.getByText("Downloading Murmela 0.2.0 · 42 %")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("stays visible and explains the choices when the update can't be installed", async () => {
    apiMocks.installUpdateInHomeApplications.mockResolvedValue(undefined);
    apiMocks.installUpdateAndRestart.mockRejectedValue("Failed to move the new app into place");
    render(<UpdateBanner status={{ ...ready, state: "cannot_install" }} />);
    expect(screen.getByText("Murmela 0.2.0 couldn't be installed")).toBeTruthy();
    expect(screen.getByText(/isn't allowed to change apps in the Applications folder/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText(/Failed to move the new app into place/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Install in my Applications folder" }));
    await waitFor(() => expect(apiMocks.installUpdateInHomeApplications).toHaveBeenCalledTimes(1));
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
