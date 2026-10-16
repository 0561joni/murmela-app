import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const apiMocks = vi.hoisted(() => ({
  getLicenseStatus: vi.fn(),
  openProPage: vi.fn(),
}));

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  ...apiMocks,
}));

import { ProBanner } from "./ProBanner";
import { resetLicenseCache } from "../hooks/useLicense";

beforeEach(() => {
  resetLicenseCache();
  apiMocks.getLicenseStatus.mockReset().mockResolvedValue({ pro: false, tier: null, updatesUntil: null, releaseDate: "2026-10-16", keyHint: null, licensedTo: null, activatedAt: null });
  apiMocks.openProPage.mockReset().mockResolvedValue(undefined);
});

describe("ProBanner", () => {
  it("invites free users to Pro and opens the pricing page", async () => {
    render(<ProBanner />);
    fireEvent.click(await screen.findByRole("button", { name: "Get Pro" }));
    expect(apiMocks.openProPage).toHaveBeenCalledTimes(1);
  });

  it("leads to the License screen for people who already have a key", async () => {
    const opened = vi.fn();
    window.addEventListener("murmela:open-license", opened);
    render(<ProBanner />);
    fireEvent.click(await screen.findByRole("button", { name: "I have a key" }));
    expect(opened).toHaveBeenCalledTimes(1);
    window.removeEventListener("murmela:open-license", opened);
  });

  it("can be closed until the next launch", async () => {
    render(<ProBanner />);
    fireEvent.click(await screen.findByRole("button", { name: "Hide until next launch" }));
    expect(screen.queryByRole("region", { name: "Murmela Pro" })).toBeNull();
  });

  it("never shows for Pro, nor before the license status is known", async () => {
    let resolve: (value: unknown) => void = () => undefined;
    apiMocks.getLicenseStatus.mockReturnValue(new Promise((done) => (resolve = done)));
    render(<ProBanner />);
    expect(screen.queryByRole("region", { name: "Murmela Pro" })).toBeNull();
    resolve({ pro: true, keyHint: "••••4D51", licensedTo: "Ada", activatedAt: null });
    await Promise.resolve();
    expect(screen.queryByRole("region", { name: "Murmela Pro" })).toBeNull();
  });

  it("asks for a renewal when this version is newer than the Pro key's updates", async () => {
    apiMocks.getLicenseStatus.mockResolvedValue({
      pro: false,
      tier: "pro",
      updatesUntil: "2027-10-16",
      releaseDate: "2027-11-02",
      keyHint: "••••4D51",
      licensedTo: "Ada",
      activatedAt: null,
    });
    render(<ProBanner />);
    expect(await screen.findByText("Pro is off in this version.")).toBeTruthy();
    expect(screen.getByText(/ended on October 16, 2027/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Renew" }));
    expect(apiMocks.openProPage).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Enter a new key" })).toBeTruthy();
  });
});
