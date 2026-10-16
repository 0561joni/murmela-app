import { ShutdownBoundary } from "./components/ShutdownBoundary";
import { AppearanceProvider } from "./lib/appearance";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";

// On macOS the desktop app's main window is transparent with the native sidebar
// material behind it (tauri.conf.json). Let the stylesheet paint only the
// content plane there, so the system material shows through the sidebar glass.
if ("__TAURI_INTERNALS__" in window && /Mac OS X/.test(navigator.userAgent)) {
  document.documentElement.classList.add("native-vibrancy");
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <AppearanceProvider>
      <ShutdownBoundary>
        <App />
      </ShutdownBoundary>
    </AppearanceProvider>
  </StrictMode>,
);
