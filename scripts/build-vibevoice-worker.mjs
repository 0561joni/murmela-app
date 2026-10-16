// Builds the self-contained VibeVoice-ASR (MLX) worker that Murmela bundles.
//
// VibeVoice runs through the Python package `mlx-audio`. The app must never
// depend on whatever `python3` happens to be on the PATH (it will not have
// mlx-audio installed), so this script creates a private virtual environment,
// installs the locked requirements, freezes the worker with PyInstaller
// (one-folder mode) and stages the result in src-tauri/bundle/vibevoice/, which
// tauri.conf.json packages into Murmela.app/Contents/Resources/workers/vibevoice/.
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { resolveWorkerSigningIdentity } from "./local-signing.mjs";
import { ensureVenv, run, signMachO } from "./python-runtime.mjs";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
if (process.platform !== "darwin" || process.arch !== "arm64") {
  throw new Error("The VibeVoice MLX worker requires Apple Silicon macOS.");
}

const workerDir = join(root, "workers/vibevoice");
const target = join(root, "src-tauri/target/vibevoice-runtime");
const bundle = join(root, "src-tauri/bundle/vibevoice");
const executableName = "murmela-vibevoice-worker";
const bundledExecutable = join(bundle, executableName, executableName);
const stampPath = join(bundle, ".murmela-build");

// mlx-community/VibeVoice-ASR-8bit ships no tokenizer; mlx-audio uses the
// Qwen2.5-7B one. Bundle it so transcription never needs the network. Files are
// verified by their Git blob ids (content hashes) from the Hugging Face repo.
const TOKENIZER_REPOSITORY = "https://huggingface.co/Qwen/Qwen2.5-7B/resolve/main";
const TOKENIZER_FILES = {
  "tokenizer.json": { size: 7_031_645, gitOid: "443909a61d429dff23010e5bddd28ff530edda00" },
  "tokenizer_config.json": { size: 7_228, gitOid: "ba7e4c5637b9732dadcd66286ce48334e8b31e9e" },
  "vocab.json": { size: 2_776_833, gitOid: "4783fe10ac3adce15ac8f358ef5462739852c569" },
  "merges.txt": { size: 1_671_839, gitOid: "20024bfe7c83998e9aeaf98a0cd6a2ce6306c2f0" },
  "LICENSE": { size: 11_343, gitOid: "6634c8cc3133b3848ec74b9f275acaaa1ea618ab" },
};

function gitBlobOid(bytes) {
  return createHash("sha1").update(`blob ${bytes.length}\0`).update(bytes).digest("hex");
}

async function ensureTokenizer(directory) {
  mkdirSync(directory, { recursive: true });
  for (const [name, expected] of Object.entries(TOKENIZER_FILES)) {
    const path = join(directory, name);
    if (existsSync(path) && gitBlobOid(readFileSync(path)) === expected.gitOid) continue;
    console.log(`Downloading Qwen2.5 tokenizer file ${name}`);
    const response = await fetch(`${TOKENIZER_REPOSITORY}/${name}?download=true`);
    if (!response.ok) throw new Error(`Downloading ${name} failed: HTTP ${response.status}`);
    const bytes = Buffer.from(await response.arrayBuffer());
    const oid = gitBlobOid(bytes);
    if (bytes.length !== expected.size || oid !== expected.gitOid) {
      throw new Error(`Tokenizer file ${name} failed verification (size ${bytes.length}, git oid ${oid}; expected ${expected.size}, ${expected.gitOid}).`);
    }
    writeFileSync(path, bytes);
  }
}

const stamp = createHash("sha256")
  .update(readFileSync(join(workerDir, "requirements.lock")))
  .update(readFileSync(join(workerDir, "murmela_vibevoice_worker.py")))
  .update(readFileSync(join(workerDir, "build.sh")))
  .update(JSON.stringify(TOKENIZER_FILES))
  .update(readFileSync(fileURLToPath(import.meta.url)))
  .digest("hex");
if (existsSync(bundledExecutable) && existsSync(stampPath) && readFileSync(stampPath, "utf8") === stamp) {
  console.log("VibeVoice worker is up to date.");
  process.exit(0);
}

mkdirSync(target, { recursive: true });
const tokenizerDir = join(target, "qwen2.5-7b-tokenizer");
await ensureTokenizer(tokenizerDir);
const venvPython = ensureVenv(join(target, "venv"), join(workerDir, "requirements.lock"), { cwd: root });
// Fail fast here instead of at transcription time.
run(venvPython, ["-c", "import mlx.core, mlx_audio.stt.generate, mlx_audio.stt.utils; print('mlx-audio import OK')"]);

const distDir = join(target, "dist");
rmSync(distDir, { recursive: true, force: true });
run("bash", [join(workerDir, "build.sh"), distDir, join(target, "build")], {
  cwd: root,
  env: { ...process.env, PYTHON312: venvPython, MURMELA_CODESIGN_IDENTITY: "", MURMELA_VIBEVOICE_TOKENIZER: tokenizerDir },
});

// Stage the one-folder bundle. Symlinks are dereferenced because Tauri's
// resource copy does not reproduce them inside the app bundle.
rmSync(join(bundle, executableName), { recursive: true, force: true });
mkdirSync(bundle, { recursive: true });
cpSync(join(distDir, executableName), join(bundle, executableName), { recursive: true, dereference: true });
colocateMetalLibraries(join(bundle, executableName, "_internal"));
signMachO(join(bundle, executableName), bundledExecutable, resolveWorkerSigningIdentity());

// MLX loads its Metal kernels (mlx.metallib) from the directory of the
// libmlx.dylib that was actually loaded. PyInstaller loads libmlx.dylib through
// a top-level link in _internal/ that points to mlx/lib/, and dyld reports the
// link's location — so the kernels next to the real file are never found
// ("Failed to load the default metallib"). The staged copy has no links, so
// keep a single copy of each such library at the top level, next to its
// metallib, and drop the unreferenced duplicates in the package folder.
function colocateMetalLibraries(internal) {
  const walk = (dir) =>
    readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
      entry.isDirectory() ? walk(join(dir, entry.name)) : [join(dir, entry.name)],
    );
  for (const metallib of walk(internal).filter((path) => path.endsWith(".metallib"))) {
    const directory = dirname(metallib);
    if (directory === internal) continue;
    const topLevelLibraries = readdirSync(directory).filter(
      (name) => name.endsWith(".dylib") && existsSync(join(internal, name)),
    );
    if (topLevelLibraries.length === 0) continue;
    for (const name of topLevelLibraries) rmSync(join(directory, name));
    renameSync(metallib, join(internal, basename(metallib)));
  }
  if (!existsSync(join(internal, "mlx.metallib"))) {
    throw new Error("mlx.metallib is not next to the libmlx.dylib the worker loads.");
  }
}

// Smoke test: the frozen worker must import its runtime and run on the GPU.
const smoke = spawnSync(bundledExecutable, ["--self-test"], {
  encoding: "utf8",
  // Same offline, minimal environment the app gives the worker.
  env: { PATH: "/usr/bin:/bin", HOME: process.env.HOME, HF_HUB_OFFLINE: "1", TRANSFORMERS_OFFLINE: "1" },
});
if (smoke.status !== 0 || !/"selfTest"/.test(smoke.stdout ?? "")) {
  throw new Error(`VibeVoice worker smoke test failed:\n${smoke.stdout}\n${smoke.stderr}`);
}
writeFileSync(stampPath, stamp);
console.log(`VibeVoice worker ready: ${bundledExecutable}`);
