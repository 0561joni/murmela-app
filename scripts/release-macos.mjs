// Publishes a macOS release to the public releases repository.
//
//   npm run release:mac            build, notarize, upload
//   npm run release:mac -- --dry-run      everything except the upload
//   npm run release:mac -- --skip-build   reuse the last build (e.g. after a failed upload)
//
// Steps: check the version and git state, build and notarize (via
// build-macos-dmg.mjs), pack the app for the in-app updater and sign it with
// the updater key, write latest.json, and create the GitHub release with the
// DMG (versioned and as Murmela.dmg for the website's download link), the
// update archive and latest.json. Installed copies read latest.json from
// `releases/latest/download/`, so publishing is what ships the update.
//
// Needs: the Developer ID certificate and notarytool profile (see
// build-macos-dmg.mjs), the updater key at ~/.tauri/murmela-updater.key with
// its password in the keychain item "murmela-updater-key-password", and an
// authenticated GitHub CLI (`gh auth login`).
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const RELEASES_REPO = "0561joni/murmela-releases";
const rootDir = process.cwd();
const args = new Set(process.argv.slice(2));
const dryRun = args.has("--dry-run");
const skipBuild = args.has("--skip-build");

const tauriConfig = JSON.parse(readFileSync(join(rootDir, "src-tauri", "tauri.conf.json"), "utf8"));
const { productName, version } = tauriConfig;
const tag = `v${version}`;
const bundleDir = join(rootDir, "src-tauri", "target", "release", "bundle");
const appPath = join(bundleDir, "macos", `${productName}.app`);
const dmgPath = join(bundleDir, "dmg", `${productName}_${version}_aarch64.dmg`);
const outDir = join(bundleDir, "release");
const archiveName = `${productName}_${version}_aarch64.app.tar.gz`;
const archivePath = join(outDir, archiveName);
const updaterKeyPath = join(homedir(), ".tauri", "murmela-updater.key");
// Built into the app and written to latest.json: Pro keys unlock releases from
// before their update year ends (see src-tauri/src/license.rs).
const releaseDate = new Date().toISOString().slice(0, 10);

function fail(message) {
  console.error(`\n${message}`);
  process.exit(1);
}

function run(command, commandArgs, options = {}) {
  console.log(`> ${[command, ...commandArgs].join(" ")}`);
  const result = spawnSync(command, commandArgs, { cwd: rootDir, stdio: "inherit", ...options });
  if (result.status !== 0) fail(`${command} failed (exit ${result.status ?? result.error}).`);
}

function capture(command, commandArgs, options = {}) {
  const result = spawnSync(command, commandArgs, { cwd: rootDir, encoding: "utf8", ...options });
  return { ok: result.status === 0, stdout: (result.stdout ?? "").trim(), stderr: (result.stderr ?? "").trim() };
}

/** The newest section of RELEASE_NOTES.md: its heading and its body. */
function releaseNotes() {
  const text = readFileSync(join(rootDir, "RELEASE_NOTES.md"), "utf8");
  const match = text.match(/^## (.+)\n([\s\S]*?)(?=^## |(?![\s\S]))/m);
  if (!match) fail("RELEASE_NOTES.md needs a '## Heading' section describing this release.");
  return { headline: match[1].trim(), body: match[2].trim() };
}

function checkPreconditions() {
  if (process.platform !== "darwin") fail("macOS releases must be built on macOS.");
  const packageVersion = JSON.parse(readFileSync(join(rootDir, "package.json"), "utf8")).version;
  const cargoVersion = readFileSync(join(rootDir, "src-tauri", "Cargo.toml"), "utf8").match(/^version = "(.+)"/m)?.[1];
  if (packageVersion !== version || cargoVersion !== version) {
    fail(
      `Versions disagree: tauri.conf.json ${version}, package.json ${packageVersion}, Cargo.toml ${cargoVersion}.\n` +
        "Set the same new version in all three before releasing.",
    );
  }
  if (!dryRun && capture("git", ["status", "--porcelain"]).stdout) {
    fail("Commit or stash your changes first, so the release matches a commit.");
  }
  if (!existsSync(updaterKeyPath)) fail(`Updater signing key not found at ${updaterKeyPath}.`);
  if (!capture("security", ["find-generic-password", "-s", "murmela-updater-key-password"]).ok) {
    fail('The updater key password is missing from the keychain item "murmela-updater-key-password".');
  }
  if (!dryRun) {
    if (!capture("gh", ["--version"]).ok) fail("Install the GitHub CLI (brew install gh) and run gh auth login.");
    if (!capture("gh", ["auth", "status"]).ok) fail("Run gh auth login first.");
    if (capture("gh", ["release", "view", tag, "--repo", RELEASES_REPO]).ok) {
      fail(`${tag} is already published. Raise the version for a new release.`);
    }
  }
}

function checkNotarized() {
  const app = capture("spctl", ["-a", "-vv", "-t", "exec", appPath]);
  if (!`${app.stdout}\n${app.stderr}`.includes("Notarized Developer ID")) {
    fail(`${productName}.app is not notarized:\n${app.stderr || app.stdout}`);
  }
  if (!capture("xcrun", ["stapler", "validate", dmgPath]).ok) fail("The DMG has no notarization ticket stapled.");
}

function buildUpdaterArchive() {
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  // COPYFILE_DISABLE keeps macOS metadata (._ files) out of the archive.
  run("tar", ["-czf", archivePath, "-C", join(bundleDir, "macos"), `${productName}.app`], {
    env: { ...process.env, COPYFILE_DISABLE: "1" },
  });
  const password = capture("security", ["find-generic-password", "-s", "murmela-updater-key-password", "-w"]).stdout;
  run(join(rootDir, "node_modules", ".bin", "tauri"), ["signer", "sign", archivePath], {
    env: {
      ...process.env,
      TAURI_SIGNING_PRIVATE_KEY_PATH: updaterKeyPath,
      TAURI_SIGNING_PRIVATE_KEY_PASSWORD: password,
    },
  });
  return readFileSync(`${archivePath}.sig`, "utf8").trim();
}

function writeLatestJson(signature, headline) {
  const latest = {
    version,
    notes: headline,
    pub_date: skipBuild ? new Date().toISOString() : `${releaseDate}T00:00:00Z`,
    platforms: {
      "darwin-aarch64": {
        signature,
        url: `https://github.com/${RELEASES_REPO}/releases/download/${tag}/${archiveName}`,
      },
    },
  };
  const path = join(outDir, "latest.json");
  writeFileSync(path, `${JSON.stringify(latest, null, 2)}\n`);
  return path;
}

checkPreconditions();
const notes = releaseNotes();
console.log(`Releasing ${productName} ${version}: ${notes.headline}${dryRun ? " (dry run)" : ""}`);

if (!skipBuild) {
  run("npm", ["run", "tauri", "build"], { env: { ...process.env, MURMELA_RELEASE_DATE: releaseDate } });
}
if (!existsSync(appPath) || !existsSync(dmgPath)) fail(`Build output missing: ${appPath} or ${dmgPath}.`);
checkNotarized();

const signature = buildUpdaterArchive();
const latestJsonPath = writeLatestJson(signature, notes.headline);
const versionedDmg = join(outDir, `${productName}_${version}_aarch64.dmg`);
const stableDmg = join(outDir, `${productName}.dmg`);
copyFileSync(dmgPath, versionedDmg);
copyFileSync(dmgPath, stableDmg);
const notesPath = join(outDir, "notes.md");
writeFileSync(notesPath, `${notes.body}\n`);
const assets = [versionedDmg, stableDmg, archivePath, `${archivePath}.sig`, latestJsonPath];

if (dryRun) {
  console.log(`\nDry run complete. Release files in ${outDir}:`);
  for (const asset of assets) console.log(`  ${asset}`);
  process.exit(0);
}

run("gh", [
  "release", "create", tag, ...assets,
  "--repo", RELEASES_REPO,
  "--title", `${productName} ${version}`,
  "--notes-file", notesPath,
  "--latest",
]);
if (!capture("git", ["rev-parse", "-q", "--verify", `refs/tags/${tag}`]).ok) {
  run("git", ["tag", "-a", tag, "-m", `${productName} ${version}`]);
}
console.log(`\nPublished ${productName} ${version}: https://github.com/${RELEASES_REPO}/releases/tag/${tag}`);
console.log(`Push the tag to the code repository with: git push origin ${tag}`);
