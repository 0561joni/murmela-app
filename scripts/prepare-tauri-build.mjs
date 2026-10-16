import { spawnSync } from "node:child_process";
import { readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const bundleRoot = join(process.cwd(), "src-tauri", "target", "release", "bundle");
const cleanupDirs = [join(bundleRoot, "dmg"), join(bundleRoot, "macos")];

for (const directory of cleanupDirs) {
  let entries = [];
  try {
    entries = readdirSync(directory, { withFileTypes: true });
  } catch {
    continue;
  }

  for (const entry of entries) {
    const fullPath = join(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name.startsWith("dmg-stage-")) {
        rmSync(fullPath, { force: true, recursive: true });
      }
      continue;
    }

    const shouldRemove =
      entry.name.endsWith(".dmg") ||
      entry.name.endsWith(".tmp.dmg") ||
      (entry.name.startsWith("rw.") && entry.name.endsWith(".dmg"));
    if (shouldRemove) {
      rmSync(fullPath, { force: true });
    }
  }
}


// The bundled helpers and workers, then the licence texts of exactly the libraries in this build.
for (const [script, label] of [
  ["build-translation-worker.mjs", "Translation runtime"],
  ["build-vibevoice-worker.mjs", "VibeVoice worker"],
  ["build-moss-worker.mjs", "MOSS worker"],
  ["build-fluid-worker.mjs", "Live-pair helper"],
  ["generate-third-party-licenses.mjs", "Third-party licence notices"],
]) {
  const result = spawnSync(process.execPath, [join("scripts", script)], { stdio: "inherit" });
  if (result.status !== 0) throw new Error(`${label} build failed.`);
}
