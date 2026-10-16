import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LicenseStatus } from "../types/domain";

const apiMocks = vi.hoisted(() => ({
  getLicenseStatus: vi.fn(),
  activateLicense: vi.fn(),
  deactivateLicense: vi.fn(),
}));

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  ...apiMocks,
}));

import { LicenseCard } from "./LicenseCard";
import { activateFromLink, resetLicenseCache } from "../hooks/useLicense";

const free: LicenseStatus = {
  pro: false,
  tier: null,
  updatesUntil: null,
  releaseDate: "2026-10-16",
  keyHint: null,
  licensedTo: null,
  activatedAt: null,
};
const pro: LicenseStatus = {
  pro: true,
  tier: "pro",
  updatesUntil: "2099-10-14",
  releaseDate: "2026-10-16",
  keyHint: "••••4D51",
  licensedTo: "Ada Lovelace",
  activatedAt: "2026-10-14T12:00:00Z",
};
const lifetime: LicenseStatus = { ...pro, tier: "lifetime", updatesUntil: null };

beforeEach(() => {
  resetLicenseCache();
  apiMocks.getLicenseStatus.mockReset().mockResolvedValue(free);
  apiMocks.activateLicense.mockReset().mockResolvedValue(pro);
  apiMocks.deactivateLicense.mockReset().mockResolvedValue(free);
});

describe("LicenseCard", () => {
  it("activates a pasted key and shows who it is licensed to", async () => {
    render(<LicenseCard />);
    const input = await screen.findByLabelText("License key");
    const activate = screen.getByRole("button", { name: "Activate" }) as HTMLButtonElement;
    expect(activate.disabled).toBe(true);

    fireEvent.change(input, { target: { value: " 38B1460A-5104-4067-A91D-77B872934D51 " } });
    fireEvent.click(activate);

    expect(await screen.findByText("Licensed to Ada Lovelace")).toBeTruthy();
    expect(apiMocks.activateLicense).toHaveBeenCalledWith(" 38B1460A-5104-4067-A91D-77B872934D51 ");
    expect(screen.getByText(/Key ••••4D51/)).toBeTruthy();
    expect(screen.getByRole("heading", { name: /Murmela Pro/ })).toBeTruthy();
    expect(screen.getByText("Updates until October 14, 2099")).toBeTruthy();
  });

  it("shows Lifetime keys with updates for life and no renewal form", async () => {
    apiMocks.getLicenseStatus.mockResolvedValue(lifetime);
    render(<LicenseCard />);
    expect(await screen.findByRole("heading", { name: /Murmela Lifetime/ })).toBeTruthy();
    expect(screen.getByText("Updates for life")).toBeTruthy();
    expect(screen.queryByLabelText("New license key")).toBeNull();
  });

  it("says when a Pro key's updates have ended but this version keeps Pro", async () => {
    apiMocks.getLicenseStatus.mockResolvedValue({ ...pro, updatesUntil: "2026-10-20", releaseDate: "2026-10-16" });
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date("2027-01-05T12:00:00Z"));
    try {
      render(<LicenseCard />);
      expect(await screen.findByText("Updates ended on October 20, 2026")).toBeTruthy();
      expect(screen.getByText(/This version keeps Pro for good/)).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("explains a version newer than the Pro key's updates and takes a renewal key", async () => {
    const ended: LicenseStatus = { ...pro, pro: false, updatesUntil: "2027-10-16", releaseDate: "2027-11-02" };
    apiMocks.getLicenseStatus.mockResolvedValue(ended);
    apiMocks.activateLicense.mockResolvedValue({ ...pro, updatesUntil: "2028-11-20", releaseDate: "2027-11-02" });
    render(<LicenseCard />);
    expect(await screen.findByText("This version is newer than your Pro updates")).toBeTruthy();
    expect(screen.getByText(/ended on October 16, 2027, and this version came out on November 2, 2027/)).toBeTruthy();

    fireEvent.change(screen.getByLabelText("New license key"), { target: { value: "renewal-key-0002" } });
    fireEvent.click(screen.getByRole("button", { name: "Activate" }));
    expect(await screen.findByText("Updates until November 20, 2028")).toBeTruthy();
    expect(apiMocks.activateLicense).toHaveBeenCalledWith("renewal-key-0002");
  });

  it("shows the server's explanation when a key is refused", async () => {
    apiMocks.activateLicense.mockRejectedValue(
      "We couldn't find this license key. Copy the whole key from your receipt email and try again.",
    );
    render(<LicenseCard />);
    fireEvent.change(await screen.findByLabelText("License key"), { target: { value: "typo" } });
    fireEvent.click(screen.getByRole("button", { name: "Activate" }));

    expect((await screen.findByRole("alert")).textContent).toContain("couldn't find this license key");
    expect(screen.getByLabelText("License key")).toBeTruthy();
  });

  it("asks before deactivating this Mac, then returns to the free version", async () => {
    apiMocks.getLicenseStatus.mockResolvedValue(pro);
    render(<LicenseCard />);
    fireEvent.click(await screen.findByRole("button", { name: "Deactivate this Mac" }));
    expect(apiMocks.deactivateLicense).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Keep Pro" }));
    fireEvent.click(screen.getByRole("button", { name: "Deactivate this Mac" }));
    fireEvent.click(screen.getByRole("button", { name: "Deactivate" }));

    await waitFor(() => expect(apiMocks.deactivateLicense).toHaveBeenCalledTimes(1));
    expect(await screen.findByLabelText("License key")).toBeTruthy();
  });

  it("activates from an activation link without typing", async () => {
    render(<LicenseCard />);
    await screen.findByLabelText("License key");
    await act(() => activateFromLink("38B1460A-5104-4067-A91D-77B872934D51"));

    expect(apiMocks.activateLicense).toHaveBeenCalledWith("38B1460A-5104-4067-A91D-77B872934D51");
    expect(await screen.findByText("Licensed to Ada Lovelace")).toBeTruthy();
  });

  it("keeps a link's key in the field when activation fails, ready to retry", async () => {
    apiMocks.activateLicense.mockRejectedValue(
      "Couldn't reach the license server. Activation needs the internet once; after that Murmela Pro works offline.",
    );
    render(<LicenseCard />);
    await screen.findByLabelText("License key");
    await act(() => activateFromLink("38B1460A-5104-4067-A91D-77B872934D51"));

    expect((await screen.findByRole("alert")).textContent).toContain("needs the internet once");
    expect((screen.getByLabelText("License key") as HTMLInputElement).value).toBe(
      "38B1460A-5104-4067-A91D-77B872934D51",
    );
    expect((screen.getByRole("button", { name: "Activate" }) as HTMLButtonElement).disabled).toBe(false);
  });
});
