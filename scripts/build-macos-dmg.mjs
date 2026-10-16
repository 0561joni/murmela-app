import {
  closeSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdtempSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  readdirSync,
  rmSync,
  symlinkSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, sep } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { isDeveloperIdIdentity, resolveSigningIdentity } from "./local-signing.mjs";

const rootDir = process.cwd();
const tauriConfigPath = join(rootDir, "src-tauri", "tauri.conf.json");
const tauriConfig = JSON.parse(readFileSync(tauriConfigPath, "utf8"));
const productName = tauriConfig.productName;
const version = tauriConfig.version;
const architecture = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : process.arch;
const bundleRoot = join(rootDir, "src-tauri", "target", "release", "bundle");
const macosBundleDir = join(bundleRoot, "macos");
const dmgBundleDir = join(bundleRoot, "dmg");
const appPath = join(macosBundleDir, `${productName}.app`);
const outputDmgName = `${productName}_${version}_${architecture}.dmg`;
const outputDmgPath = join(macosBundleDir, outputDmgName);
const outputDmgCompatPath = join(dmgBundleDir, outputDmgName);
const entitlementsPath = join(rootDir, "src-tauri", "Entitlements.plist");
const workerEntitlementsPath = join(rootDir, "src-tauri", "WorkerEntitlements.plist");
const mainExecutablePath = join(appPath, "Contents", "MacOS", tauriConfig.mainBinaryName ?? "murmela");
// Keychain profile created once with `xcrun notarytool store-credentials`.
const notaryProfile = process.env.MURMELA_NOTARY_PROFILE || "murmela-notary";
const signatureResourcesPath = join(appPath, "Contents", "_CodeSignature", "CodeResources");

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: rootDir,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function runAsync(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: rootDir, stdio: ["ignore", "ignore", "pipe"] });
    let stderr = "";
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) =>
      code === 0 ? resolve() : reject(new Error(`${command} ${args.join(" ")} failed:\n${stderr}`)),
    );
  });
}

function isMachO(path) {
  const header = Buffer.alloc(8);
  const fd = openSync(path, "r");
  try {
    if (readSync(fd, header, 0, 8, 0) < 8) return false;
  } finally {
    closeSync(fd);
  }
  const magic = header.readUInt32BE(0);
  if ([0xfeedface, 0xfeedfacf, 0xcefaedfe, 0xcffaedfe].includes(magic)) return true;
  // Universal binaries share 0xCAFEBABE with Java class files; a universal
  // header's next word is a small architecture count, a class file's is its version.
  return magic === 0xcafebabe && header.readUInt32BE(4) < 20;
}

// Tauri copies resources by following symlinks, so a bundled framework (the
// Python runtime inside the PyInstaller workers) arrives with its
// Versions/Current links replaced by duplicate files. codesign then seals
// a layout the notary service rejects ("The signature of the binary is
// invalid"). Rebuild the standard layout before signing.
function normalizeFrameworks(root) {
  const { frameworks } = collectNestedCode(root);
  for (const framework of frameworks) {
    const versionsDir = join(framework, "Versions");
    if (!existsSync(versionsDir)) continue;
    const versions = readdirSync(versionsDir).filter((name) => name !== "Current");
    if (versions.length !== 1) {
      throw new Error(`Expected one version in ${versionsDir}, found: ${versions.join(", ")}`);
    }
    const [version] = versions;
    rmSync(join(versionsDir, "Current"), { force: true, recursive: true });
    symlinkSync(version, join(versionsDir, "Current"));
    for (const entry of readdirSync(framework)) {
      if (entry !== "Versions") rmSync(join(framework, entry), { force: true, recursive: true });
    }
    for (const entry of readdirSync(join(versionsDir, version))) {
      if (entry !== "_CodeSignature") symlinkSync(join("Versions", "Current", entry), join(framework, entry));
    }
  }
}

function collectNestedCode(root) {
  const files = [];
  const frameworks = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir)) {
      const path = join(dir, entry);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) continue;
      if (info.isDirectory()) {
        if (entry.endsWith(".framework")) frameworks.push(path);
        walk(path);
      } else if (path !== mainExecutablePath && isMachO(path)) {
        files.push({ path, executable: (info.mode & 0o111) !== 0 && !/\.(dylib|so)$/.test(entry) });
      }
    }
  };
  walk(root);
  const depth = (path) => path.split(sep).length;
  files.sort((a, b) => depth(b.path) - depth(a.path));
  frameworks.sort((a, b) => depth(b) - depth(a));
  return { files, frameworks };
}

// Apple's timestamp server is occasionally slow or answers with a skewed
// time ("timestamps differ by N seconds"); a retry usually succeeds.
async function codesignWithRetry(args, attempts = 4) {
  for (let attempt = 1; ; attempt += 1) {
    try {
      return await runAsync("codesign", args);
    } catch (error) {
      if (attempt >= attempts) throw error;
      console.warn(`codesign attempt ${attempt} failed for ${args.at(-1)}; retrying.`);
      await new Promise((resolve) => setTimeout(resolve, 5000 * attempt));
    }
  }
}

// Apple's notary service requires every Mach-O file to carry a Developer ID
// signature with the hardened runtime and a secure timestamp, signed from the
// inside out. `codesign --deep` is not enough for that.
async function signForDistribution(identity) {
  const base = ["--force", "--options", "runtime", "--timestamp", "--sign", identity];
  normalizeFrameworks(join(appPath, "Contents"));
  const { files, frameworks } = collectNestedCode(join(appPath, "Contents"));
  console.log(`Signing ${files.length} nested binaries and ${frameworks.length} frameworks with "${identity}".`);
  const queue = [...files];
  const workers = Array.from({ length: 12 }, async () => {
    for (let file = queue.shift(); file; file = queue.shift()) {
      const entitlements = file.executable ? ["--entitlements", workerEntitlementsPath] : [];
      await codesignWithRetry([...base, ...entitlements, file.path]);
    }
  });
  await Promise.all(workers);
  for (const framework of frameworks) await codesignWithRetry([...base, framework]);
  await codesignWithRetry([...base, "--entitlements", entitlementsPath, appPath]);
}

// A copy of the previous DMG left open in Finder makes `hdiutil create`
// fail with "Resource busy".
function detachMountedCopies(imagePaths) {
  const info = spawnSync("hdiutil", ["info"], { encoding: "utf8" }).stdout ?? "";
  for (const block of info.split(/^=+$/m)) {
    const image = block.match(/^image-path\s*:\s*(.+)$/m)?.[1]?.trim();
    if (!imagePaths.includes(image)) continue;
    for (const [, mountPoint] of block.matchAll(/^\/dev\/\S+\t[^\t]*\t(\/Volumes\/.+)$/gm)) {
      console.log(`Ejecting ${mountPoint}, a mounted copy of the previous DMG.`);
      run("hdiutil", ["detach", mountPoint.trim(), "-quiet"]);
    }
  }
}

function notaryAvailable() {
  if (process.env.MURMELA_SKIP_NOTARIZE === "1") {
    console.log("MURMELA_SKIP_NOTARIZE=1: skipping notarization.");
    return false;
  }
  const check = spawnSync("xcrun", ["notarytool", "history", "--keychain-profile", notaryProfile], {
    stdio: "ignore",
  });
  if (check.status === 0) return true;
  console.warn(
    [
      "",
      `No notarization credentials found in the keychain profile "${notaryProfile}".`,
      "The build is signed but NOT notarized, so other Macs will refuse to open it.",
      "Store credentials once with:",
      `  xcrun notarytool store-credentials ${notaryProfile} --key <AuthKey.p8> --key-id <KEY_ID> --issuer <ISSUER_ID>`,
      "",
    ].join("\n"),
  );
  return false;
}

function notarize(path) {
  console.log(`Notarizing ${path} (usually a few minutes)...`);
  const result = spawnSync(
    "xcrun",
    ["notarytool", "submit", path, "--keychain-profile", notaryProfile, "--wait", "--output-format", "json"],
    { cwd: rootDir, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
  );
  let submission = {};
  try {
    submission = JSON.parse(result.stdout);
  } catch {
    console.error(result.stdout);
  }
  if (submission.status !== "Accepted") {
    console.error(`Notarization failed with status ${submission.status ?? "unknown"}.`);
    if (submission.id) {
      run("xcrun", ["notarytool", "log", submission.id, "--keychain-profile", notaryProfile]);
    }
    process.exit(1);
  }
}

if (process.platform !== "darwin") {
  process.exit(0);
}

mkdirSync(macosBundleDir, { recursive: true });
mkdirSync(dmgBundleDir, { recursive: true });

const signingIdentity = resolveSigningIdentity();
const distribution = isDeveloperIdIdentity(signingIdentity);
if (distribution) {
  await signForDistribution(signingIdentity);
} else if (signingIdentity !== "-") {
  // Same flags as the ad-hoc signature, but with a stable identity so macOS
  // keeps Microphone and Accessibility permissions across rebuilds.
  console.log(`Signing ${productName}.app with "${signingIdentity}".`);
  run("codesign", [
    "--force",
    "--deep",
    "--sign",
    signingIdentity,
    "--entitlements",
    entitlementsPath,
    appPath,
  ]);
} else if (!existsSync(signatureResourcesPath)) {
  console.log("No macOS signing identity detected; applying an ad-hoc bundle signature.");
  console.log("Run scripts/setup-local-signing.sh once to keep permissions across rebuilds.");
  run("codesign", [
    "--force",
    "--deep",
    "--sign",
    "-",
    "--entitlements",
    entitlementsPath,
    appPath,
  ]);
}
run("codesign", ["--verify", "--deep", "--strict", appPath]);

const notarizing = distribution && notaryAvailable();
if (notarizing) {
  // Staple the app itself too, so it opens offline after leaving the DMG.
  const zipDir = mkdtempSync(join(tmpdir(), "murmela-notarize-"));
  const zipPath = join(zipDir, `${productName}.zip`);
  try {
    run("ditto", ["-c", "-k", "--sequesterRsrc", "--keepParent", appPath, zipPath]);
    notarize(zipPath);
  } finally {
    rmSync(zipDir, { force: true, recursive: true });
  }
  run("xcrun", ["stapler", "staple", appPath]);
}

const stageDir = mkdtempSync(join(tmpdir(), "murmela-dmg-stage-"));
const stageRoot = join(stageDir, productName);

try {
  mkdirSync(stageRoot, { recursive: true });
  // Keep framework links relative; Node otherwise rewrites them as absolute paths.
  cpSync(appPath, join(stageRoot, `${productName}.app`), { recursive: true, verbatimSymlinks: true });
  symlinkSync("/Applications", join(stageRoot, "Applications"));

  detachMountedCopies([outputDmgPath, outputDmgCompatPath]);
  rmSync(outputDmgPath, { force: true });
  rmSync(outputDmgCompatPath, { force: true });

  run("hdiutil", [
    "create",
    "-volname",
    productName,
    "-srcfolder",
    stageRoot,
    "-ov",
    "-format",
    "UDZO",
    outputDmgPath,
  ]);

  if (distribution) {
    await codesignWithRetry(["--force", "--timestamp", "--sign", signingIdentity, outputDmgPath]);
  }
  if (notarizing) {
    notarize(outputDmgPath);
    run("xcrun", ["stapler", "staple", outputDmgPath]);
  }

  cpSync(outputDmgPath, outputDmgCompatPath, { force: true });
} finally {
  rmSync(stageDir, { force: true, recursive: true });
}
