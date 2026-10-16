import { describeError } from "../lib/formatting";
import {
  useEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type ReactNode,
} from "react";
import { AppIcon, type AppIconName } from "./IconButton";

export function Button({
  children,
  icon,
  busy,
  variant = "secondary",
  size = "default",
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  icon?: AppIconName;
  busy?: boolean;
  /** primary: the one main action per view · secondary: outlined · ghost: quiet text · danger: destructive or stop */
  variant?: "primary" | "secondary" | "danger" | "ghost";
  size?: "default" | "large";
}) {
  return (
    <button
      type="button"
      {...props}
      aria-busy={busy || undefined}
      disabled={props.disabled || busy}
      className={["button", "button-" + variant, size === "large" ? "button-large" : "", className].filter(Boolean).join(" ")}
    >
      {busy ? (
        <span className="icon-button-spinner" aria-hidden="true" />
      ) : icon ? (
        <AppIcon name={icon} />
      ) : null}
      {children}
    </button>
  );
}

export function ActionButton({
  action,
  children,
  success = "Done",
  ...props
}: Omit<Parameters<typeof Button>[0], "onClick"> & {
  action: () => Promise<unknown>;
  success?: string;
}) {
  const [state, setState] = useState<"idle" | "busy" | "success" | "error">(
    "idle",
  );
  const [error, setError] = useState("");
  const inFlight = useRef(false);
  useEffect(() => {
    if (state !== "success") return;
    const timer = window.setTimeout(() => setState("idle"), 2200);
    return () => window.clearTimeout(timer);
  }, [state]);
  return (
    <span className="action-feedback">
      <Button
        {...props}
        busy={state === "busy"}
        icon={state === "success" ? "check" : props.icon}
        onClick={async () => {
          if (inFlight.current) return;
          inFlight.current = true;
          setState("busy");
          try {
            await action();
            setState(success ? "success" : "idle");
          } catch (reason) {
            setError(
              describeError(reason, "Please try again."),
            );
            setState("error");
          } finally {
            inFlight.current = false;
          }
        }}
      >
        {state === "success" ? success : children}
      </Button>
      <span
        className={state === "error" ? "error-text action-message" : "sr-only"}
        role={state === "error" ? "alert" : "status"}
      >
        {state === "error" ? error : state === "success" ? success : ""}
      </span>
    </span>
  );
}

/** Screen toolbar: the title on the left, screen-level controls on the right. */
export function PageHeader({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children?: ReactNode;
}) {
  return (
    <header className="page-header" data-tauri-drag-region>
      <div className="page-header-title" data-tauri-drag-region>
        <h1>{title}</h1>
        {description ? <p className="page-header-description">{description}</p> : null}
      </div>
      {children ? <div className="page-header-actions">{children}</div> : null}
    </header>
  );
}

export function Progress({
  value,
  label,
}: {
  value?: number | null;
  label: string;
}) {
  return (
    <div
      className="progress-track"
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={
        value == null
          ? undefined
          : Math.round(Math.max(0, Math.min(100, value)))
      }
    >
      <div
        className={"progress-fill" + (value == null ? " indeterminate" : "")}
        style={
          value == null
            ? undefined
            : { width: Math.max(0, Math.min(100, value)) + "%" }
        }
      />
    </div>
  );
}

/** Shows a shortcut as separate keycaps, e.g. ⌘ ⇧ Space. Expects the text from formatShortcutForDisplay. */
export function ShortcutKeys({ shortcut }: { shortcut: string }) {
  const keys = shortcut.split("+").filter(Boolean);
  return (
    <span className="shortcut-keys" aria-label={keys.join(" ")}>
      {keys.map((key, index) => (
        <kbd key={index} aria-hidden="true">{key}</kbd>
      ))}
    </span>
  );
}
