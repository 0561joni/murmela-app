// Builds the self-contained MOSS Transcribe + Diarize worker that Murmela bundles.
//
// The worker is two programs in one folder:
//   - moss-transcribe: the native CPU runtime (moss-transcribe.cpp, pinned and
//     patched with workers/moss/moss-prompt.patch), and
//   - murmela-moss-worker: the NDJSON adapter, frozen with PyInstaller so the
//     app never depends on the Python installed on the Mac.
// The result is staged in src-tauri/bundle/moss/ and packaged by
// tauri.conf.json into Murmela.app/Contents/Resources/workers/moss/.
import { createHash } from "node:crypto";
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { resolveWorkerSigningIdentity } from "./local-signing.mjs";
import { ensureVenv, run, signMachO } from "./python-runtime.mjs";

const REPOSITORY = "https://github.com/localai-org/moss-transcribe.cpp.git";
const REVISION = "190a569c13b4b247450f2fb3b2a431244e84833e";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
if (process.platform !== "darwin" || process.arch !== "arm64") {
  throw new Error("The bundled MOSS worker is built for Apple Silicon macOS.");
}
const env = { ...process.env };
if (!env.DEVELOPER_DIR && existsSync("/Library/Developer/CommandLineTools/usr/bin/clang"))
  env.DEVELOPER_DIR = "/Library/Developer/CommandLineTools";

const workerDir = join(root, "workers/moss");
const target = join(root, "src-tauri/target/moss-runtime");
const source = join(target, "source");
const bundle = join(root, "src-tauri/bundle/moss");
const executableName = "murmela-moss-worker";
const folder = join(bundle, executableName);
const bundledExecutable = join(folder, executableName);
const stampPath = join(bundle, ".murmela-build");
const patch = join(workerDir, "moss-prompt.patch");

const stamp = createHash("sha256")
  .update(REVISION)
  .update(readFileSync(patch))
  .update(readFileSync(join(workerDir, "requirements.lock")))
  .update(readFileSync(join(workerDir, "murmela_moss_worker.py")))
  .update(readFileSync(fileURLToPath(import.meta.url)))
  .digest("hex");
if (existsSync(bundledExecutable) && existsSync(stampPath) && readFileSync(stampPath, "utf8") === stamp) {
  console.log("MOSS worker is up to date.");
  process.exit(0);
}
mkdirSync(target, { recursive: true });

// 1. Native runtime: pinned, patched source kept in an ignored cache.
const sourceStamp = join(source, ".murmela-patch");
const sourceKey = `${REVISION}:${createHash("sha256").update(readFileSync(patch)).digest("hex")}`;
if (!existsSync(sourceStamp) || readFileSync(sourceStamp, "utf8") !== sourceKey) {
  rmSync(source, { recursive: true, force: true });
  run("git", ["clone", "--quiet", REPOSITORY, source], { env });
  run("git", ["-C", source, "checkout", "--quiet", REVISION], { env });
  run("git", ["-C", source, "submodule", "update", "--quiet", "--init", "--recursive"], { env });
  run("git", ["-C", source, "apply", patch], { env });
  writeFileSync(sourceStamp, sourceKey);
}
const build = join(source, "build-murmela");
run("cmake", ["-S", source, "-B", build, "-DCMAKE_BUILD_TYPE=Release", "-DMT_BUILD_TESTS=OFF",
  "-DGGML_NATIVE=OFF", "-DGGML_OPENMP=OFF", "-DCMAKE_OSX_DEPLOYMENT_TARGET=14.0"], { env });
run("cmake", ["--build", build, "--config", "Release", "--parallel", "6"], { env });

// 2. Adapter: frozen with PyInstaller (one-folder).
const venvPython = ensureVenv(join(target, "venv"), join(workerDir, "requirements.lock"), { cwd: root });
const distDir = join(target, "dist");
rmSync(distDir, { recursive: true, force: true });
run(venvPython, ["-m", "PyInstaller", "--noconfirm", "--clean", "--onedir", "--name", executableName,
  "--distpath", distDir, "--workpath", join(target, "work"), "--specpath", join(target, "spec"),
  join(workerDir, "murmela_moss_worker.py")], { cwd: root });

// 3. Stage: adapter folder + native CLI side by side, licenses, signatures.
rmSync(folder, { recursive: true, force: true });
mkdirSync(bundle, { recursive: true });
cpSync(join(distDir, executableName), folder, { recursive: true, dereference: true });
copyFileSync(join(build, "moss-transcribe"), join(folder, "moss-transcribe"));
copyFileSync(join(source, "LICENSE"), join(folder, "moss-transcribe.cpp-LICENSE"));
copyFileSync(join(source, "third_party/ggml/LICENSE"), join(folder, "ggml-LICENSE"));
signMachO(folder, bundledExecutable, resolveWorkerSigningIdentity());

const smoke = spawnSync(bundledExecutable, ["--self-test"], { encoding: "utf8" });
if (smoke.status !== 0 || !/"selfTest"/.test(smoke.stdout ?? "")) {
  throw new Error(`MOSS worker smoke test failed:\n${smoke.stdout}\n${smoke.stderr}`);
}
writeFileSync(stampPath, stamp);
console.log(`MOSS worker ready: ${bundledExecutable}`);
