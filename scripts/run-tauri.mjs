import { existsSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const rootDir = process.cwd();
const tauriBinary = join(rootDir, "node_modules", ".bin", "tauri");
const args = process.argv.slice(2);

function nativeBuildEnv() {
  const env = { ...process.env };
  if (!env.DEVELOPER_DIR && existsSync("/Library/Developer/CommandLineTools/usr/bin/clang")) {
    env.DEVELOPER_DIR = "/Library/Developer/CommandLineTools";
  }
  return env;
}

function run(command, commandArgs) {
  const result = spawnSync(command, commandArgs, {
    cwd: rootDir,
    env: nativeBuildEnv(),
    stdio: "inherit",
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

if (args[0] === "dev") {
  run(process.execPath, [join(rootDir, "scripts", "build-translation-worker.mjs")]);
  run(process.execPath, [join(rootDir, "scripts", "build-vibevoice-worker.mjs")]);
  run(process.execPath, [join(rootDir, "scripts", "build-moss-worker.mjs")]);
}

run(tauriBinary, args);
// Tauri builds the .app; the DMG is assembled and signed by our own script.
if (args[0] === "build" && !args.includes("--bundles")) {
  run(process.execPath, [join(rootDir, "scripts", "build-macos-dmg.mjs")]);
}
