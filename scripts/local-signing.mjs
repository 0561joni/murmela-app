// Which identity signs macOS builds, in order of preference:
//
// 1. APPLE_SIGNING_IDENTITY, if set.
// 2. A "Developer ID Application" certificate: the paid Apple identity that
//    public releases need (hardened runtime, notarization).
// 3. "Blabber Local Signing", the self-signed certificate created by
//    `scripts/setup-local-signing.sh`. Ad-hoc signatures change with every
//    build, so macOS privacy settings (Microphone, Accessibility) would treat
//    each build as a new app; a stable identity keeps those permissions.
// 4. Ad-hoc ("-").
import { spawnSync } from "node:child_process";

export const LOCAL_SIGNING_IDENTITY = "Blabber Local Signing";

export function resolveSigningIdentity() {
  if (process.env.APPLE_SIGNING_IDENTITY) return process.env.APPLE_SIGNING_IDENTITY;
  if (process.platform !== "darwin") return "-";
  const developerId = findDeveloperIdIdentity();
  if (developerId) return developerId;
  const found = spawnSync("security", ["find-certificate", "-c", LOCAL_SIGNING_IDENTITY], {
    stdio: "ignore",
  });
  return found.status === 0 ? LOCAL_SIGNING_IDENTITY : "-";
}

// Worker build steps sign their output only so it runs during development.
// Release packaging re-signs everything inside the app, so these skip the
// Developer ID and its slow round trip to Apple's timestamp server.
export function resolveWorkerSigningIdentity() {
  const identity = resolveSigningIdentity();
  if (!isDeveloperIdIdentity(identity)) return identity;
  const local = spawnSync("security", ["find-certificate", "-c", LOCAL_SIGNING_IDENTITY], {
    stdio: "ignore",
  });
  return local.status === 0 ? LOCAL_SIGNING_IDENTITY : "-";
}

export function isDeveloperIdIdentity(identity) {
  return identity.startsWith("Developer ID Application");
}

function findDeveloperIdIdentity() {
  const result = spawnSync("security", ["find-identity", "-v", "-p", "codesigning"], {
    encoding: "utf8",
  });
  if (result.status !== 0) return null;
  const match = result.stdout.match(/"(Developer ID Application: [^"]+)"/);
  return match ? match[1] : null;
}
