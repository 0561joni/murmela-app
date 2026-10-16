import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { WordAlternative } from "../lib/textTranslation";

export type AlternativesState =
  | { status: "loading" }
  | { status: "ready"; items: WordAlternative[] }
  | { status: "rewriting"; choice: string }
  | { status: "error"; message: string };

interface Props {
  word: string;
  /** The highlighted word in the translation; the menu opens below it. */
  anchor: HTMLElement | null;
  state: AlternativesState;
  onPick: (text: string) => void;
  onClose: () => void;
}

const MENU_WIDTH = 240;

/** DeepL-style menu of other words for one place in a translation. */
export function AlternativesMenu({ word, anchor, state, onPick, onClose }: Props) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number; above: boolean } | null>(null);

  // Follow the word while the pane scrolls or the window resizes.
  useLayoutEffect(() => {
    if (!anchor) return;
    const place = () => {
      const rect = anchor.getBoundingClientRect();
      const height = menuRef.current?.offsetHeight ?? 200;
      const above = rect.bottom + 6 + height > window.innerHeight - 8 && rect.top - 6 - height > 8;
      setPosition({
        left: Math.max(8, Math.min(rect.left - 10, window.innerWidth - MENU_WIDTH - 8)),
        top: above ? rect.top - 6 - height : rect.bottom + 6,
        above,
      });
    };
    place();
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      window.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [anchor, state.status]);

  // A click anywhere else, or Escape, closes the menu.
  useEffect(() => {
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Node;
      if (menuRef.current?.contains(target) || anchor?.contains(target)) return;
      onClose();
    };
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("pointerdown", onPointer, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [anchor, onClose]);

  // Keyboard users land on the first option as soon as there is one.
  useEffect(() => {
    if (state.status === "ready") menuRef.current?.querySelector<HTMLButtonElement>("[role=menuitem]")?.focus();
  }, [state.status]);

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const items = [...(menuRef.current?.querySelectorAll<HTMLButtonElement>("[role=menuitem]") ?? [])];
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "ArrowDown" ? (at + 1) % items.length : (at - 1 + items.length) % items.length;
    items[next]?.focus();
  }

  return (
    <div
      ref={menuRef}
      className={"alternatives-menu" + (position?.above ? " is-above" : "")}
      style={position ? { left: position.left, top: position.top, width: MENU_WIDTH } : { visibility: "hidden" }}
      role="menu"
      aria-label={`Alternatives for ${word}`}
      onKeyDown={onKeyDown}
    >
      <p className="alternatives-menu-label">Instead of <strong>{word}</strong></p>
      {state.status === "loading" ? (
        <div className="alternatives-menu-loading" role="status">
          <span className="icon-button-spinner" aria-hidden="true" />
          Finding other words…
        </div>
      ) : state.status === "rewriting" ? (
        <div className="alternatives-menu-loading" role="status">
          <span className="icon-button-spinner" aria-hidden="true" />
          Rewriting with “{state.choice}”…
        </div>
      ) : state.status === "error" ? (
        <p className="alternatives-menu-message" role="alert">{state.message}</p>
      ) : state.items.length === 0 ? (
        <p className="alternatives-menu-message" role="status">No other words fit here.</p>
      ) : (
        <ul>
          {state.items.map((item) => (
            <li key={item.text}>
              <button type="button" role="menuitem" onClick={() => onPick(item.text)}>
                {item.text}
              </button>
            </li>
          ))}
        </ul>
      )}
      {state.status === "ready" && state.items.length > 0 ? (
        <p className="alternatives-menu-hint">The rest of the paragraph adapts to your choice.</p>
      ) : null}
    </div>
  );
}
