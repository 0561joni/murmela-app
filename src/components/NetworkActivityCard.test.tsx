import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { NetworkActivity } from "../types/domain";

const apiMocks = vi.hoisted(() => ({
  getNetworkActivity: vi.fn(),
  clearNetworkActivity: vi.fn(),
  exportNetworkActivity: vi.fn(),
}));

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  ...apiMocks,
}));

import { NetworkActivityCard, networkActivityText } from "./NetworkActivityCard";

const activity: NetworkActivity = {
  destinations: [
    { id: "hugging_face", name: "Hugging Face", host: "huggingface.co", reason: "Downloads the models you choose." },
    { id: "github", name: "GitHub", host: "github.com", reason: "Checks for a new version of Murmela." },
    { id: "lemon_squeezy", name: "Lemon Squeezy", host: "api.lemonsqueezy.com", reason: "Activates Murmela Pro once." },
  ],
  entries: [
    {
      id: 3,
      startedAt: "2026-10-14T09:12:00Z",
      destination: "github",
      host: "github.com",
      purpose: "Update check",
      bytes: null,
      outcome: "failed",
    },
    {
      id: 2,
      startedAt: "2026-10-13T18:40:00Z",
      destination: "lemon_squeezy",
      host: "api.lemonsqueezy.com",
      purpose: "Activate Murmela Pro",
      bytes: 1_284,
      outcome: "completed",
    },
    {
      id: 1,
      startedAt: "2026-10-13T18:02:00Z",
      destination: "hugging_face",
      host: "huggingface.co",
      purpose: "Download Whisper Large v3 Turbo",
      bytes: 1_624_555_275,
      outcome: "completed",
    },
  ],
};

beforeEach(() => {
  apiMocks.getNetworkActivity.mockReset().mockResolvedValue(activity);
  apiMocks.clearNetworkActivity.mockReset().mockResolvedValue(undefined);
  apiMocks.exportNetworkActivity.mockReset().mockResolvedValue("/Users/ada/Desktop/log.txt");
});

describe("NetworkActivityCard", () => {
  it("writes the log as plain text, one connection per line", () => {
    const text = networkActivityText(activity, new Date("2026-10-16T12:00:00Z"));
    const lines = text.trimEnd().split("\n");
    expect(lines[0]).toBe("Murmela network activity");
    expect(lines).toContain("- GitHub (github.com): Checks for a new version of Murmela.");
    expect(lines).toContain("Connections made (3):");
    const connections = lines.slice(lines.indexOf("Connections made (3):") + 1);
    expect(connections).toHaveLength(3);
    expect(connections[0]).toMatch(/ · GitHub \(github\.com\) · Update check \[Failed\] · size not reported$/);
    expect(connections[2]).toMatch(/ · Hugging Face \(huggingface\.co\) · Download Whisper Large v3 Turbo · 1\.6 GB$/);
    expect(networkActivityText({ ...activity, entries: [] })).toContain("Connections made (0):\nNone since");
  });

  it("exports the log and says where it was saved", async () => {
    render(<NetworkActivityCard />);
    fireEvent.click(await screen.findByRole("button", { name: "Export…" }));
    await waitFor(() => expect(apiMocks.exportNetworkActivity).toHaveBeenCalledTimes(1));
    const [fileName, contents] = apiMocks.exportNetworkActivity.mock.calls[0];
    expect(fileName).toMatch(/^Murmela network activity \d{4}-\d{2}-\d{2}\.txt$/);
    expect(contents).toContain("Update check [Failed]");
    expect(await screen.findByText("Saved to /Users/ada/Desktop/log.txt")).toBeTruthy();
  });

  it("says nothing when the save is canceled", async () => {
    apiMocks.exportNetworkActivity.mockResolvedValue(null);
    render(<NetworkActivityCard />);
    fireEvent.click(await screen.findByRole("button", { name: "Export…" }));
    await waitFor(() => expect(apiMocks.exportNetworkActivity).toHaveBeenCalled());
    expect(screen.queryByText(/^Saved to/)).toBeNull();
  });

  it("lists the only three places Murmela can contact", async () => {
    render(<NetworkActivityCard />);
    const list = await screen.findByRole("list", { name: "Places Murmela can contact" });
    const items = within(list).getAllByRole("listitem");
    expect(items).toHaveLength(3);
    expect(items.map((item) => item.querySelector("code")?.textContent)).toEqual([
      "huggingface.co",
      "github.com",
      "api.lemonsqueezy.com",
    ]);
    expect(screen.getByText("No analytics, no crash reporting, no telemetry.", { exact: false })).toBeTruthy();
  });

  it("logs each connection with destination, purpose, size and outcome", async () => {
    render(<NetworkActivityCard />);
    const log = await screen.findByRole("region", { name: "Connection log" });
    const rows = within(log).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(3);
    expect(rows[0].textContent).toContain("GitHub");
    expect(rows[0].textContent).toContain("Update check · Failed");
    expect(rows[0].textContent).toContain("—");
    expect(rows[1].textContent).toContain("Lemon Squeezy");
    expect(rows[1].textContent).toContain("1 KB");
    expect(rows[2].textContent).toContain("Download Whisper Large v3 Turbo");
    expect(rows[2].textContent).toContain("1.6 GB");
    expect(rows[2].textContent).not.toContain("Failed");
  });

  it("clears the log", async () => {
    render(<NetworkActivityCard />);
    await screen.findByRole("region", { name: "Connection log" });
    fireEvent.click(screen.getByRole("button", { name: "Clear log" }));

    expect(await screen.findByText(/No connections since the log started/)).toBeTruthy();
    expect(apiMocks.clearNetworkActivity).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("region", { name: "Connection log" })).toBeNull();
  });

  it("disables Clear log while the log is empty", async () => {
    apiMocks.getNetworkActivity.mockResolvedValue({ ...activity, entries: [] });
    render(<NetworkActivityCard />);
    await screen.findByText(/No connections since the log started/);
    await waitFor(() =>
      expect((screen.getByRole("button", { name: "Clear log" }) as HTMLButtonElement).disabled).toBe(true),
    );
  });

  it("explains when the log can't be read", async () => {
    apiMocks.getNetworkActivity.mockRejectedValue("database is locked");
    render(<NetworkActivityCard />);
    expect(await screen.findByText(/database is locked|could not be read/)).toBeTruthy();
  });
});
