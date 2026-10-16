import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const apiMocks = vi.hoisted(() => ({
  getHealthCheck: vi.fn(),
  listLicenceDocuments: vi.fn(),
  readLicenceDocument: vi.fn(),
  getUpdateStatus: vi.fn(),
  checkForUpdates: vi.fn(),
}));

const upToDate = {
  state: "up_to_date" as const,
  currentVersion: "1.2.3",
  version: null,
  notes: null,
  message: null,
  lastCheckedMs: 1,
  proUpdatesUntil: null,
};
const aboutProps = { autoUpdateEnabled: true, saving: false, onToggleAutoUpdate: vi.fn() };

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  ...apiMocks,
}));

import { AboutSection } from "./AboutSection";

describe("AboutSection", () => {
  it("credits every engine and loads a licence text only when it is opened", async () => {
    apiMocks.getHealthCheck.mockResolvedValue({ appName: "Murmela", appVersion: "1.2.3" });
    apiMocks.listLicenceDocuments.mockResolvedValue([{ name: "CC-BY-4.0.txt", sizeBytes: 19_000 }]);
    apiMocks.readLicenceDocument.mockResolvedValue("Attribution 4.0 International");
    apiMocks.getUpdateStatus.mockResolvedValue(upToDate);
    render(<AboutSection {...aboutProps} />);

    expect(await screen.findByText(/Version 1\.2\.3 · /)).toBeTruthy();
    expect(screen.getByRole("link", { name: "Parakeet TDT 0.6B v3" })).toBeTruthy();
    expect(screen.getByText("CC BY 4.0")).toBeTruthy();
    expect(screen.getByText(/Gemma is provided under and subject to the Gemma Terms of Use/)).toBeTruthy();

    const details = (await screen.findByText("CC-BY-4.0.txt")).closest("details")!;
    expect(apiMocks.readLicenceDocument).not.toHaveBeenCalled();
    details.open = true;
    fireEvent(details, new Event("toggle"));
    await waitFor(() => expect(apiMocks.readLicenceDocument).toHaveBeenCalledWith("CC-BY-4.0.txt"));
    expect(await screen.findByText("Attribution 4.0 International")).toBeTruthy();
  });

  it("toggles automatic updates and checks on request", async () => {
    apiMocks.getHealthCheck.mockResolvedValue({ appName: "Murmela", appVersion: "1.2.3" });
    apiMocks.listLicenceDocuments.mockResolvedValue([]);
    apiMocks.getUpdateStatus.mockResolvedValue(upToDate);
    apiMocks.checkForUpdates.mockResolvedValue({ ...upToDate, state: "ready", version: "1.3.0" });
    const onToggleAutoUpdate = vi.fn();
    render(<AboutSection {...aboutProps} onToggleAutoUpdate={onToggleAutoUpdate} />);

    expect(await screen.findByText("Murmela is up to date.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Check for updates automatically" }));
    expect(onToggleAutoUpdate).toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Check now" }));
    expect(await screen.findByText(/Murmela 1\.3\.0 is downloaded/)).toBeTruthy();
  });
});
