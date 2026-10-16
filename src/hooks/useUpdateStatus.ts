import { useCallback, useEffect, useState } from "react";
import { checkForUpdates, getUpdateStatus, listenUpdateStatus } from "../lib/api";
import type { UpdateStatus } from "../types/domain";

/** The background updater's status, kept current through its events. */
export function useUpdateStatus() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);

  useEffect(() => {
    let current = true;
    let unlisten: (() => void) | undefined;
    void listenUpdateStatus((next) => current && setStatus(next)).then((stop) => {
      if (current) unlisten = stop;
      else stop();
    });
    getUpdateStatus()
      .then((next) => current && setStatus(next))
      .catch(() => undefined);
    return () => {
      current = false;
      unlisten?.();
    };
  }, []);

  const checkNow = useCallback(async () => {
    setStatus((previous) => (previous ? { ...previous, state: "checking", message: null } : previous));
    setStatus(await checkForUpdates());
  }, []);

  return { status, checkNow };
}
