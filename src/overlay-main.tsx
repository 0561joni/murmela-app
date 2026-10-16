import { AppearanceProvider } from "./lib/appearance";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { OverlayApp } from "./OverlayApp";
import "./styles.css";

document.documentElement.classList.add("overlay-window");
document.body.classList.add("overlay-window");

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <AppearanceProvider>
      <OverlayApp />
    </AppearanceProvider>
  </StrictMode>,
);
