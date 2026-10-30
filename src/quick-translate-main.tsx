import { AppearanceProvider } from "./lib/appearance";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QuickTranslateApp } from "./QuickTranslateApp";
import "./styles.css";
import "./quick-translate.css";

document.documentElement.classList.add("quick-window");

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <AppearanceProvider>
      <QuickTranslateApp />
    </AppearanceProvider>
  </StrictMode>,
);
