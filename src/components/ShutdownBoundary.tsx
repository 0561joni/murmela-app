import { useEffect, useRef, useState, type ReactNode } from "react";

/** Keep work screens mounted while native cleanup drains their operations. */
export function ShutdownBoundary({ children }: { children: ReactNode }) {
  const [stopping, setStopping] = useState(false);
  const dialog = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    let eventReceived = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("app://shutdown-started", () => {
          eventReceived = true;
          if (!disposed) setStopping(true);
        }),
      )
      .then(async (cleanup) => {
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
        const { invoke } = await import("@tauri-apps/api/core");
        const active = await invoke<boolean>("is_app_shutting_down");
        if (!disposed && !eventReceived) setStopping(active);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  useEffect(() => {
    if (stopping) dialog.current?.focus();
  }, [stopping]);
  return (
    <>
      <div inert={stopping} aria-hidden={stopping || undefined}>
        {children}
      </div>
      {stopping && (
        <div className="modal-backdrop shutdown-backdrop">
          <div
            ref={dialog}
            tabIndex={-1}
            role="dialog"
            aria-modal="true"
            aria-labelledby="shutdown-title"
            aria-describedby="shutdown-description"
            className="surface modal-panel"
          >
            <h2 id="shutdown-title">Quitting Murmela…</h2>
            <p id="shutdown-description" role="status">
              Stopping running work and freeing memory. This can take a moment.
            </p>
          </div>
        </div>
      )}
    </>
  );
}
