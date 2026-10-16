import { ShutdownBoundary } from "./components/ShutdownBoundary";
import { AppearanceProvider } from "./lib/appearance";
import { createRoot } from "react-dom/client";
import { SplashApp } from "./SplashApp";
import "./splash.css";

// On macOS the splash window carries the native popover material behind its
// rounded card (tauri.conf.json), so the card can be translucent glass.
if ("__TAURI_INTERNALS__" in window && /Mac OS X/.test(navigator.userAgent)) {
  document.documentElement.classList.add("native-vibrancy");
}

createRoot(document.getElementById("root")!).render(
  <AppearanceProvider>
    <ShutdownBoundary>
      <SplashApp />
    </ShutdownBoundary>
  </AppearanceProvider>,
);
