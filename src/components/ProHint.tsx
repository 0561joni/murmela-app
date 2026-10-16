const OPEN_LICENSE_EVENT = "murmela:open-license";

/** Takes the user to Settings → License from anywhere in the app. */
export function openLicenseSettings() {
  window.dispatchEvent(new Event(OPEN_LICENSE_EVENT));
}

export function onOpenLicenseSettings(handler: () => void) {
  window.addEventListener(OPEN_LICENSE_EVENT, handler);
  return () => window.removeEventListener(OPEN_LICENSE_EVENT, handler);
}

export function ProTag() {
  return <span className="pro-tag">Pro</span>;
}

/** Quiet note shown in place of a Pro feature on the free version. */
export function ProHint({ children }: { children: React.ReactNode }) {
  return (
    <p className="pro-hint">
      <ProTag /> <span>{children}</span>{" "}
      <button type="button" className="link-button" onClick={openLicenseSettings}>
        Add license key
      </button>
    </p>
  );
}
