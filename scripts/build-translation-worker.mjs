import { createHash } from "node:crypto";
import { createReadStream, existsSync, mkdirSync, copyFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { resolveWorkerSigningIdentity } from "./local-signing.mjs";

// Reproducible native dependency, fetched only while building the application.
const revision = "972d2313bc0bf0a45f634f77d95c9fb03aeab12c";
const archiveSha = "1eea355e60e4898c7121764170fc23dd5a6735be2dc84e9cd45eab7ff4760775";
const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const target = join(root, "src-tauri/target/translation-runtime");
const archive = join(target, "llama.tar.gz");
const source = join(target, `llama.cpp-${revision}`);
const build = join(target, "build");
const bundle = join(root, "src-tauri/bundle/translation");

if (process.platform !== "darwin" || process.arch !== "arm64") process.exit(0);
const env = { ...process.env };
// Use the separately installed, working CLT toolchain when no developer override exists.
if (!env.DEVELOPER_DIR && existsSync("/Library/Developer/CommandLineTools/usr/bin/clang"))
  env.DEVELOPER_DIR = "/Library/Developer/CommandLineTools";
function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, env, stdio: "inherit" });
  if (result.status !== 0) throw new Error(`${command} failed (${result.status ?? result.error})`);
}
mkdirSync(target, { recursive: true });
if (!existsSync(archive)) run("curl", ["-fL", "--retry", "3", `https://codeload.github.com/ggml-org/llama.cpp/tar.gz/${revision}`, "-o", archive]);
const hash = createHash("sha256");
for await (const chunk of createReadStream(archive)) hash.update(chunk);
if (hash.digest("hex") !== archiveSha) throw new Error("llama.cpp source archive checksum mismatch; remove the cached archive and rebuild.");
if (!existsSync(join(source, "CMakeLists.txt"))) run("tar", ["-xzf", archive, "-C", target]);
run("cmake", ["-S", "workers/translation", "-B", build, `-DLLAMA_SOURCE=${source}`, "-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_OSX_DEPLOYMENT_TARGET=11.0", "-DGGML_METAL=ON", "-DGGML_OPENMP=OFF"]);
run("cmake", ["--build", build, "--config", "Release", "--target", "murmela-translation-worker", "-j", "6"]);
mkdirSync(bundle, { recursive: true });
copyFileSync(join(build, "murmela-translation-worker"), join(bundle, "murmela-translation-worker"));
run("codesign", ["--force", "--sign", resolveWorkerSigningIdentity(), join(bundle, "murmela-translation-worker")]);
copyFileSync(join(source, "LICENSE"), join(root, "src-tauri/licenses/llama.cpp-MIT.txt"));
