import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const EXPECTED_REMOTE = "git@github.com:0561joni/Murmela.git";
const rootDir = process.cwd();

function usage() {
  return [
    "Usage:",
    '  npm run deploy:mac -- -m "Your commit message"',
    "",
    "Options:",
    "  -m, --message <message>  Commit message used when local changes exist",
    "  -h, --help               Show this help",
    "",
    "Environment:",
    "  DEPLOY_COMMIT_MESSAGE    Alternative commit message source",
  ].join("\n");
}

function parseArgs(argv) {
  const parsed = {
    message: process.env.DEPLOY_COMMIT_MESSAGE?.trim() || "",
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "-h" || arg === "--help") {
      console.log(usage());
      process.exit(0);
    }

    if (arg === "-m" || arg === "--message") {
      const value = argv[index + 1];
      if (!value) {
        fail(`${arg} requires a commit message.\n\n${usage()}`);
      }
      parsed.message = value.trim();
      index += 1;
      continue;
    }

    if (arg.startsWith("--message=")) {
      parsed.message = arg.slice("--message=".length).trim();
      continue;
    }

    fail(`Unknown argument: ${arg}\n\n${usage()}`);
  }

  return parsed;
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function run(command, args, options = {}) {
  console.log(`> ${[command, ...args].join(" ")}`);
  const result = spawnSync(command, args, {
    cwd: rootDir,
    stdio: "inherit",
    ...options,
  });

  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function capture(command, args) {
  const result = spawnSync(command, args, {
    cwd: rootDir,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

  if (result.status !== 0) {
    const detail = result.stderr.trim() || result.stdout.trim();
    fail(detail || `Command failed: ${[command, ...args].join(" ")}`);
  }

  return result.stdout.trim();
}

function assertMacOS() {
  if (process.platform !== "darwin") {
    fail("macOS deploy must run on macOS because DMG creation requires hdiutil.");
  }
}

function assertExpectedRemote() {
  const remote = capture("git", ["remote", "get-url", "--push", "origin"]);
  if (remote !== EXPECTED_REMOTE) {
    fail(`Expected origin push URL ${EXPECTED_REMOTE}, but found ${remote}.`);
  }
}

function currentBranch() {
  const result = spawnSync("git", ["symbolic-ref", "--short", "HEAD"], {
    cwd: rootDir,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

  if (result.status !== 0) {
    fail("Cannot deploy from a detached HEAD. Check out a branch first.");
  }

  return result.stdout.trim();
}

function hasLocalChanges() {
  return capture("git", ["status", "--porcelain"]).length > 0;
}

function expectedDmgPath() {
  const tauriConfigPath = join(rootDir, "src-tauri", "tauri.conf.json");
  const tauriConfig = JSON.parse(readFileSync(tauriConfigPath, "utf8"));
  const productName = tauriConfig.productName;
  const version = tauriConfig.version;
  const architecture =
    process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : process.arch;
  return join(
    rootDir,
    "src-tauri",
    "target",
    "release",
    "bundle",
    "dmg",
    `${productName}_${version}_${architecture}.dmg`,
  );
}

function assertDmgExists(dmgPath) {
  if (existsSync(dmgPath)) {
    return;
  }

  const dmgDir = join(rootDir, "src-tauri", "target", "release", "bundle", "dmg");
  const found = existsSync(dmgDir)
    ? readdirSync(dmgDir).filter((name) => name.endsWith(".dmg"))
    : [];
  const foundText = found.length > 0 ? ` Found: ${found.join(", ")}` : "";
  fail(`Expected DMG was not created at ${dmgPath}.${foundText}`);
}

const args = parseArgs(process.argv.slice(2));

assertMacOS();
assertExpectedRemote();
const branch = currentBranch();

run("npm", ["run", "tauri", "build"]);

const dmgPath = expectedDmgPath();
assertDmgExists(dmgPath);

if (hasLocalChanges()) {
  if (!args.message) {
    fail(`Local changes exist. Provide a commit message.\n\n${usage()}`);
  }

  run("git", ["add", "-A"]);

  const staged = spawnSync("git", ["diff", "--cached", "--quiet"], {
    cwd: rootDir,
    stdio: "ignore",
  });
  if (staged.status !== 0) {
    run("git", ["commit", "-m", args.message]);
  } else {
    console.log("No committable changes after staging.");
  }
} else {
  console.log("No local changes to commit.");
}

run("git", ["push", "origin", `HEAD:${branch}`]);

console.log("");
console.log(`Pushed branch: ${branch}`);
console.log(`DMG: ${dmgPath}`);
