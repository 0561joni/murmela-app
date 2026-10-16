import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    include: ["src/**/*.{test,spec}.{ts,tsx}", "browser-helper/test/**/*.test.js"],
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
  },
});
