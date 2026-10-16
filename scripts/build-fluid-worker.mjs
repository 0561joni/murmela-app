import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { resolveWorkerSigningIdentity } from "./local-signing.mjs";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
if (process.platform !== "darwin" || process.arch !== "arm64") {
  throw new Error("The live-pair helper requires Apple Silicon macOS.");
}
const manifest = JSON.parse(readFileSync(join(root, "workers/fluid/manifest.json"), "utf8"));
const packagePath = join(root, "workers/fluid");
const buildPath = join(root, "src-tauri/target/fluid-build");
const bundle = join(root, "src-tauri/bundle/fluid");
function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: root, stdio: "inherit", ...options });
  if (result.status !== 0) throw new Error(`${command} failed (${result.status ?? result.error})`);
  return result;
}
// The package pins FluidAudio by exact revision; refuse a drifted manifest.
const packageSource = readFileSync(join(packagePath, "Package.swift"), "utf8");
if (!packageSource.includes(`revision: "${manifest.fluidAudio.revision}"`)) {
  throw new Error("workers/fluid/Package.swift and manifest.json pin different FluidAudio revisions.");
}
const swift = ["build", "-c", "release", "--arch", "arm64", "--package-path", packagePath, "--build-path", buildPath];
run("swift", swift);
const binPath = run("swift", [...swift, "--show-bin-path"], { stdio: ["ignore", "pipe", "inherit"] })
  .stdout.toString()
  .trim();
mkdirSync(bundle, { recursive: true });
const helper = join(bundle, "murmela-fluid-worker");
copyFileSync(join(binPath, "MurmelaFluidWorker"), helper);
// Ad-hoc or local signature; no paid developer account. CoreML keeps its
// compile cache in ~/Library/Caches/murmela-fluid-worker, which stays writable.
run("codesign", ["--force", "--sign", resolveWorkerSigningIdentity(), helper]);
const checkout = join(buildPath, "checkouts/FluidAudio");
// SwiftPM checkouts are read-only; replace rather than overwrite.
const license = join(bundle, "FluidAudio-LICENSE");
rmSync(license, { force: true });
copyFileSync(join(checkout, "LICENSE"), license);
chmodSync(license, 0o644);
copyFileSync(join(root, "workers/fluid/manifest.json"), join(bundle, "manifest.json"));
