// Shared helpers for building Murmela's Python-based model workers.
//
// Workers are frozen with PyInstaller into self-contained executables, so the
// finished app never depends on the Python installed on the Mac. A Python
// interpreter is only needed at build time; this module finds a suitable one
// and keeps a private virtual environment per worker under src-tauri/target/.
import { existsSync, readdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

export function run(command, args, options = {}) {
  const result = spawnSync(command, args, { stdio: "inherit", ...options });
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed (${result.status ?? result.error})`);
}

export function capture(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : null;
}

const PREFERRED = ["3.12", "3.13", "3.11", "3.10", "3.14"];

function describe(python) {
  const out = capture(python, ["-c", "import platform,sys; print(platform.machine(), '%d.%d' % sys.version_info[:2])"]);
  if (!out) return null;
  const [arch, version] = out.split(" ");
  return { python, arch, version };
}

// Find a native arm64 Python 3.10+ (MLX wheels are arm64-only; mlx-audio needs 3.10+).
export function findPython() {
  const candidates = [process.env.MURMELA_PYTHON, process.env.PYTHON312];
  const dirs = ["/opt/homebrew/bin", "/usr/local/bin", join(homedir(), ".pyenv/shims")];
  const frameworks = "/Library/Frameworks/Python.framework/Versions";
  if (existsSync(frameworks)) {
    for (const version of readdirSync(frameworks)) dirs.push(join(frameworks, version, "bin"));
  }
  for (const version of PREFERRED) {
    for (const dir of dirs) candidates.push(join(dir, `python${version}`));
    candidates.push(capture("/usr/bin/which", [`python${version}`]));
  }
  for (const dir of dirs) candidates.push(join(dir, "python3"));
  candidates.push(capture("/usr/bin/which", ["python3"]));
  if (capture("/usr/bin/which", ["uv"])) candidates.push(capture("uv", ["python", "find", ">=3.10"]));

  const found = [];
  for (const candidate of new Set(candidates.filter(Boolean))) {
    if (!existsSync(candidate) || !statSync(candidate).isFile()) continue;
    const info = describe(candidate);
    if (info?.arch === "arm64" && PREFERRED.includes(info.version)) found.push(info);
  }
  found.sort((a, b) => PREFERRED.indexOf(a.version) - PREFERRED.indexOf(b.version));
  if (found.length > 0) return found[0].python;

  if (capture("/usr/bin/which", ["uv"])) {
    run("uv", ["python", "install", "3.12"]);
    const managed = capture("uv", ["python", "find", "3.12"]);
    if (managed) return managed;
  }
  throw new Error(
    "An arm64 Python 3.10 or newer is required to build Murmela's model workers (the macOS system Python 3.9 is too old). " +
      "Install one with `brew install python@3.12`, or set MURMELA_PYTHON to its path, then build again.",
  );
}

// Create (once) a virtual environment and install the locked requirements.
export function ensureVenv(venv, requirements, { cwd } = {}) {
  const python = join(venv, "bin/python");
  if (!existsSync(python) || !describe(python)) {
    const base = findPython();
    console.log(`Creating build environment ${venv} with ${base}`);
    run(base, ["-m", "venv", "--clear", venv], { cwd });
  }
  run(python, ["-m", "pip", "install", "--disable-pip-version-check", "--quiet", "--upgrade", "pip"], { cwd });
  run(python, ["-m", "pip", "install", "--disable-pip-version-check", "-r", requirements], { cwd });
  return python;
}

// Sign every Mach-O file below `directory`, the entry executable last.
export function signMachO(directory, entryExecutable, identity) {
  const machO = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir)) {
      const path = join(dir, entry);
      const info = statSync(path);
      if (info.isDirectory()) walk(path);
      else if (/Mach-O/.test(capture("/usr/bin/file", ["-b", path]) ?? "")) machO.push(path);
    }
  };
  walk(directory);
  machO.sort((a, b) => (a === entryExecutable) - (b === entryExecutable));
  for (const path of machO) run("codesign", ["--force", "--sign", identity, path], { stdio: "ignore" });
}
