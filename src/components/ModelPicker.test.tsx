import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { InstalledModel } from "../types/domain";
import { ModelInfoButton, ModelPicker } from "./ModelPicker";

const models: InstalledModel[] = [
  {
    id: "ggml-medium-bin",
    engine: "whisper.cpp",
    modelName: "ggml-medium.bin",
    variant: "accurate",
    localPath: "/models/ggml-medium.bin",
    sizeBytes: 1_533_763_059,
    isDefault: true,
    profile: "accurate",
  },
  {
    id: "qwen3-asr-1.7b-bf16",
    engine: "qwen3_asr_c",
    modelName: "Qwen3-ASR-1.7B",
    variant: "1.7B BF16",
    localPath: "/models/qwen3-asr-1.7b-bf16",
    sizeBytes: 4_703_041_355,
    isDefault: false,
    profile: "accurate",
  },
];

const nativeModels: InstalledModel[] = [
  {
    ...models[0],
    id: "moss-transcribe-diarize-0.9b-f16",
    engine: "moss-transcribe-cpp",
    modelName: "MOSS Transcribe + Diarize 0.9B F16",
    capabilities: {
      supportedContexts: ["shortcut_dictation", "quick_dictate", "file_transcription"],
      nativeDiarization: true,
      timestampedSegments: true,
      contextSupport: true,
      languageControl: "automatic_only",
      maximumAudioDurationMs: 5_400_000,
    },
  },
  {
    ...models[0],
    id: "test-file-engine",
    engine: "test-file",
    modelName: "File-only engine",
    capabilities: {
      supportedContexts: ["file_transcription"],
      nativeDiarization: true,
      timestampedSegments: true,
      contextSupport: true,
      languageControl: "automatic_only",
      maximumAudioDurationMs: 3_600_000,
    },
  },
];

describe("ModelPicker", () => {
  it("shows a two-line selected value and context-specific recommendation", () => {
    render(
      <ModelPicker
        label="Quick Dictate model"
        value={models[0].id}
        models={models}
        context="quick_dictate"
        onChange={vi.fn()}
      />,
    );
    const trigger = screen.getByRole("button", { name: "Quick Dictate model" });
    expect(within(trigger).getByText("Whisper Medium")).toBeTruthy();
    expect(within(trigger).getByLabelText(/Speed .* Accuracy/)).toBeTruthy();

    fireEvent.click(trigger);
    const listbox = screen.getByRole("listbox", { name: "Quick Dictate model" });
    expect(within(listbox).getByRole("option", { name: /Qwen.*Recommended/ })).toBeTruthy();
    expect(within(listbox).getAllByRole("option")).toHaveLength(2);
  });

  it("selects with mouse and supports arrow and Escape navigation", async () => {
    const onChange = vi.fn();
    render(
      <ModelPicker
        label="Quick Dictate model"
        value={models[0].id}
        models={models}
        context="quick_dictate"
        onChange={onChange}
      />,
    );
    const trigger = screen.getByRole("button", { name: "Quick Dictate model" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const options = screen.getAllByRole("option");
    await waitFor(() => expect(document.activeElement).toBe(options[0]));
    fireEvent.keyDown(options[0], { key: "ArrowDown" });
    expect(document.activeElement).toBe(options[1]);
    fireEvent.click(options[1]);
    expect(onChange).toHaveBeenCalledWith(models[1].id);
    await waitFor(() => expect(document.activeElement).toBe(trigger));

    fireEvent.click(trigger);
    fireEvent.keyDown(screen.getByRole("listbox"), { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("opens model information without selecting and restores focus", async () => {
    const onChange = vi.fn();
    render(
      <ModelPicker
        label="File Transcription model"
        value={models[0].id}
        models={models}
        context="file_transcription"
        onChange={onChange}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "File Transcription model" }));
    const infoButton = screen.getByRole("button", { name: "About Qwen" });
    fireEvent.click(infoButton);
    expect(onChange).not.toHaveBeenCalled();
    const dialog = screen.getByRole("dialog", { name: "Qwen" });
    expect(within(dialog).getByText("Qwen3-ASR-1.7B")).toBeTruthy();
    expect(within(dialog).getByText("4.7 GB")).toBeTruthy();
    expect(within(dialog).getByText("Recommended for the record button and files")).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(document.activeElement).toBe(infoButton);
    });
  });

  it("shows a disabled empty state", () => {
    render(
      <ModelPicker
        label="Shortcut Dictation model"
        value={null}
        models={[]}
        context="shortcut_dictation"
        onChange={vi.fn()}
      />,
    );
    const trigger = screen.getByRole("button", { name: "Shortcut Dictation model" });
    expect((trigger as HTMLButtonElement).disabled).toBe(true);
    expect(within(trigger).getByText("No engine installed")).toBeTruthy();
  });

  it("does not pretend a model is selected when the saved choice cannot run this workflow", () => {
    render(
      <ModelPicker
        label="Shortcut Dictation model"
        value={nativeModels[1].id}
        models={[...models, nativeModels[1]]}
        context="shortcut_dictation"
        onChange={vi.fn()}
      />,
    );
    const trigger = screen.getByRole("button", { name: "Shortcut Dictation model" });
    expect((trigger as HTMLButtonElement).disabled).toBe(false);
    expect(within(trigger).getByText("Choose an engine")).toBeTruthy();
  });

  it("filters native models by supported use context", () => {
    const { rerender } = render(
      <ModelPicker label="Shortcut Dictation model" value={nativeModels[0].id} models={nativeModels} context="shortcut_dictation" onChange={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Shortcut Dictation model" }));
    expect(screen.getAllByRole("option")).toHaveLength(1);
    expect(screen.queryByText("File-only engine")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Shortcut Dictation model" }));

    rerender(<ModelPicker label="File Transcription model" value={nativeModels[0].id} models={nativeModels} context="file_transcription" onChange={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "File Transcription model" }));
    expect(screen.getAllByRole("option")).toHaveLength(2);
  });

  it("explains built-in speakers and automatic language control", () => {
    render(<ModelInfoButton model={nativeModels[0]} />);
    fireEvent.click(screen.getByRole("button", { name: "About MOSS with speakers" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText(/Built-in speaker identification/)).toBeTruthy();
    expect(within(dialog).getByText(/fixed-language choice is not applied/)).toBeTruthy();
  });
});

describe("ModelInfoButton", () => {
  it("opens from a download card and closes on backdrop click", () => {
    render(<ModelInfoButton model={models[0]} />);
    const infoButton = screen.getByRole("button", { name: "About Whisper Medium" });
    fireEvent.click(infoButton);
    const dialog = screen.getByRole("dialog", { name: "Whisper Medium" });
    fireEvent.mouseDown(dialog.parentElement as HTMLElement);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
