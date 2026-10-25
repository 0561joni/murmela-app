import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

const POPOVER_WIDTH = 300;

/** A small "?" button next to a label that opens a short explanation.
 * Click or Enter opens it; a click elsewhere or Escape closes it. `topic`
 * names the button ("About <topic>") when the title is a question. */
export function HelpHint({ title, topic, children }: { title: string; topic?: string; children: ReactNode }) {
  const buttonRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const popoverId = useId();
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);

  // Below the button, or above it when there is no room; follows scrolling.
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const rect = buttonRef.current?.getBoundingClientRect();
      if (!rect) return;
      const height = popoverRef.current?.offsetHeight ?? 160;
      const above = rect.bottom + 8 + height > window.innerHeight - 8 && rect.top - 8 - height > 8;
      setPosition({
        left: Math.max(8, Math.min(rect.left - 12, window.innerWidth - POPOVER_WIDTH - 8)),
        top: above ? rect.top - 8 - height : rect.bottom + 8,
      });
    };
    place();
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      window.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Node;
      if (popoverRef.current?.contains(target) || buttonRef.current?.contains(target)) return;
      setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      setOpen(false);
      buttonRef.current?.focus();
    };
    document.addEventListener("pointerdown", onPointer, true);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onPointer, true);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className={open ? "help-hint is-open" : "help-hint"}
        aria-label={`About ${topic ?? title.toLowerCase()}`}
        aria-expanded={open}
        aria-controls={open ? popoverId : undefined}
        onClick={(event) => {
          // Inside a <label>, a click would otherwise also toggle its control.
          event.preventDefault();
          event.stopPropagation();
          setOpen((current) => !current);
        }}
      >
        <span aria-hidden="true">?</span>
      </button>
      {open
        ? createPortal(
            <div
              ref={popoverRef}
              id={popoverId}
              className="help-popover"
              role="dialog"
              aria-label={title}
              style={position ? { left: position.left, top: position.top, width: POPOVER_WIDTH } : { visibility: "hidden" }}
            >
              <p className="help-popover-title">{title}</p>
              <div className="help-popover-body">{children}</div>
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
