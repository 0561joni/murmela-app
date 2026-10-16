import { useEffect, useState } from "react";
import { getDictationOutputState, listenDictationOutput } from "../lib/translationApi";
import type { DictationOutputState } from "../types/domain";

export function useDictationTranslation(enabled: boolean | undefined) {
  const [state, setState] = useState<DictationOutputState | null>(null);
  useEffect(() => {
    let disposed = false;
    let received = false;
    let cleanup: (() => void) | undefined;
    void listenDictationOutput((next) => {
      received = true;
      if (!disposed) setState(next);
    }).then(async (unlisten) => {
      if (disposed) { unlisten(); return; }
      cleanup = unlisten;
      const snapshot = await getDictationOutputState();
      if (!disposed && !received) setState(snapshot);
    }).catch(() => undefined);
    return () => { disposed = true; cleanup?.(); };
  }, [enabled]);
  return [state, setState] as const;
}
