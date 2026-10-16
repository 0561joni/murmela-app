// Writes src-tauri/licenses/THIRD-PARTY-LIBRARIES.txt: the licence texts of every
// Rust crate and npm package that ships inside Murmela. Development and build-only
// dependencies are left out. Crates that share an identical licence text are listed
// together under one copy of it.
//
// Run with `npm run licenses`; the Tauri build runs it too (prepare-tauri-build.mjs).
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const output = join(root, "src-tauri/licenses/THIRD-PARTY-LIBRARIES.txt");
const LICENSE_FILE = /^(licen[cs]e|copying|notice|unlicense)([-._].*)?$/i;

const MIT_TEMPLATE = (holders) => `Copyright (c) ${holders}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`;

const BSD_3_TEMPLATE = (holders) => `Copyright (c) ${holders}. All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice, this
   list of conditions and the following disclaimer in the documentation and/or
   other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors may
   be used to endorse or promote products derived from this software without
   specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT,
INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.`;

/** A licence text bundled in src-tauri/licenses, without its "Applies to" note. */
function bundledText(file, marker) {
  const bundled = readFileSync(join(root, "src-tauri/licenses", file), "utf8");
  return bundled.slice(bundled.indexOf(marker)).trim();
}

/** For "A OR B" licences we may pick one; MIT is the shortest to reproduce. */
function preferredFiles(names, spdx) {
  if (!/\bOR\b/.test(spdx ?? "") || !/\bMIT\b/.test(spdx)) return names;
  const mit = names.filter((name) => /mit/i.test(name));
  return mit.length > 0 ? mit : names;
}

function licenceFiles(directory, spdx) {
  if (!existsSync(directory)) return [];
  const names = readdirSync(directory)
    .filter((name) => LICENSE_FILE.test(name) && statSync(join(directory, name)).isFile())
    .sort();
  return preferredFiles(names, spdx)
    .map((name) => readFileSync(join(directory, name), "utf8").replace(/\r\n/g, "\n").trim())
    .filter(Boolean);
}

/** The package's own licence files, or the standard text for its SPDX licence. */
function licenceText(directory, licenseFile, spdx, holders) {
  const texts = licenseFile && existsSync(join(directory, licenseFile))
    ? [readFileSync(join(directory, licenseFile), "utf8").trim()]
    : licenceFiles(directory, spdx);
  if (texts.length > 0) return texts.join("\n\n" + "-".repeat(40) + "\n\n");
  if (/\bMIT\b/.test(spdx ?? "")) return MIT_TEMPLATE(holders || "the package authors");
  if (/Apache-2\.0/.test(spdx ?? "")) return bundledText("Apache-2.0.txt", "Apache License");
  if (/MPL-2\.0/.test(spdx ?? "")) return bundledText("MPL-2.0.txt", "Mozilla Public License");
  if (/BSD-3-Clause/.test(spdx ?? "")) return BSD_3_TEMPLATE(holders || "the package authors");
  if (/^Unlicense$/.test(spdx ?? "")) return "Released into the public domain under the Unlicense (https://unlicense.org).";
  return `Licence: ${spdx ?? "not declared"}. The package ships no licence file; see https://spdx.org/licenses/${encodeURIComponent((spdx ?? "").split(/\s+/)[0])}.html`;
}

function rustPackages() {
  // The notices describe the build being made, so only this platform's crates count.
  const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)[1];
  const metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--filter-platform", host, "--manifest-path", join(root, "src-tauri/Cargo.toml")], {
      maxBuffer: 256 * 1024 * 1024,
      encoding: "utf8",
    }),
  );
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const packages = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  // Follow only normal dependencies from the app: build scripts, proc-macros run at
  // compile time and dev-dependencies never reach the user, so they need no notice.
  const shipped = new Set();
  const queue = [metadata.resolve.root];
  while (queue.length > 0) {
    const id = queue.pop();
    if (shipped.has(id)) continue;
    shipped.add(id);
    for (const dep of nodes.get(id)?.deps ?? []) {
      const pkg = packages.get(dep.pkg);
      const procMacro = pkg?.targets.some((target) => target.kind.includes("proc-macro"));
      if (!procMacro && dep.dep_kinds.some((kind) => kind.kind === null)) queue.push(dep.pkg);
    }
  }
  shipped.delete(metadata.resolve.root);
  return [...shipped]
    .map((id) => packages.get(id))
    .filter((pkg) => pkg && pkg.source) // local path crates are Murmela's own code
    .map((pkg) => {
      const directory = dirname(pkg.manifest_path);
      const holders = (pkg.authors ?? []).map((author) => author.replace(/\s*<.*>/, "")).join(", ");
      return {
        label: `${pkg.name} ${pkg.version} (${pkg.license ?? "licence not declared"})`,
        text: licenceText(directory, pkg.license_file, pkg.license, holders),
      };
    });
}

function npmPackages() {
  const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const seen = new Map();
  const queue = Object.keys(manifest.dependencies ?? {});
  while (queue.length > 0) {
    const name = queue.pop();
    if (seen.has(name)) continue;
    const directory = join(root, "node_modules", name);
    const packageJson = join(directory, "package.json");
    if (!existsSync(packageJson)) {
      throw new Error(`${name} is not installed; run npm install first.`);
    }
    const pkg = JSON.parse(readFileSync(packageJson, "utf8"));
    const license = typeof pkg.license === "string" ? pkg.license : pkg.license?.type;
    const author = typeof pkg.author === "string" ? pkg.author : pkg.author?.name;
    seen.set(name, {
      label: `${pkg.name} ${pkg.version} (${license ?? "licence not declared"})`,
      text: licenceText(directory, null, license, author?.replace(/\s*<.*>/, "")),
    });
    queue.push(...Object.keys(pkg.dependencies ?? {}));
  }
  return [...seen.values()];
}

/** Licence files of the Python packages PyInstaller froze into the model workers. */
function pythonPackages() {
  const bundles = join(root, "src-tauri/bundle");
  if (!existsSync(bundles)) return [];
  const found = new Map();
  const walk = (directory, depth) => {
    if (depth > 4 || !existsSync(directory)) return;
    for (const name of readdirSync(directory)) {
      const path = join(directory, name);
      if (!statSync(path).isDirectory()) continue;
      if (name.endsWith(".dist-info")) {
        const metadataPath = join(path, "METADATA");
        if (!existsSync(metadataPath)) continue;
        const metadata = readFileSync(metadataPath, "utf8").split("\n\n")[0];
        const field = (key) => metadata.match(new RegExp(`^${key}: (.+)$`, "m"))?.[1]?.trim();
        const packageName = field("Name") ?? name.replace(/-[^-]+\.dist-info$/, "");
        const version = field("Version") ?? "";
        const classifiers = { "Apache Software License": "Apache-2.0", "MIT License": "MIT", "BSD License": "BSD-3-Clause" };
        const classifierName = metadata.match(/^Classifier: License :: OSI Approved :: (.+)$/m)?.[1];
        const classifier = classifiers[classifierName] ?? classifierName;
        const license = field("License-Expression") ?? (field("License")?.length < 40 ? field("License") : undefined) ?? classifier;
        const licenceDir = join(path, "licenses");
        const texts = [...licenceFiles(path, license), ...collectAll(licenceDir)];
        const author = field("Author") ?? field("Author-email")?.replace(/\s*<.*>/, "");
        found.set(`${packageName} ${version}`, {
          label: `${packageName} ${version} (${license ?? "licence not declared"})`,
          text: texts.length > 0 ? texts.join("\n\n" + "-".repeat(40) + "\n\n") : licenceText(path, null, license, author),
        });
      } else {
        walk(path, depth + 1);
      }
    }
  };
  walk(bundles, 0);
  return [...found.values()];
}

/** Every licence-like file below a dist-info `licenses/` folder. */
function collectAll(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { recursive: true })
    .map((name) => join(directory, String(name)))
    .filter((path) => statSync(path).isFile())
    .sort()
    .map((path) => readFileSync(path, "utf8").replace(/\r\n/g, "\n").trim())
    .filter(Boolean);
}

function section(title, entries) {
  const groups = new Map();
  for (const entry of entries) {
    // Identical up to whitespace counts as identical.
    const key = entry.text.replace(/\s+/g, " ").trim();
    const group = groups.get(key) ?? { text: entry.text.trim(), labels: [] };
    group.labels.push(entry.label);
    groups.set(key, group);
  }
  const blocks = [...groups.values()]
    .map(({ text, labels }) => ({ labels: labels.sort(), text }))
    .sort((a, b) => a.labels[0].localeCompare(b.labels[0]));
  const rule = "=".repeat(78);
  return [
    `${rule}\n${title} (${entries.length} packages)\n${rule}`,
    ...blocks.map(({ labels, text }) => `${labels.map((label) => `* ${label}`).join("\n")}\n\n${text}\n\n${"-".repeat(78)}`),
  ].join("\n\n");
}

const rust = rustPackages();
const npm = npmPackages();
const python = pythonPackages();
const header = `Murmela third-party libraries

Licence texts of the open-source libraries compiled into Murmela or shipped with it,
including the Python packages frozen into the model workers.
Models and model runtimes are described in THIRD_PARTY_NOTICES.md and the other files in
this folder. Generated by scripts/generate-third-party-licenses.mjs; do not edit by hand.

Where a library is offered under a choice of licences (for example "MIT OR Apache-2.0"),
Murmela uses it under the MIT licence. The MPL-2.0 libraries are used unmodified; their
source code is available from crates.io.
`;
const sections = [header, section("Rust crates", rust), section("JavaScript packages", npm)];
if (python.length > 0) sections.push(section("Python packages in the model workers", python));
writeFileSync(output, sections.join("\n\n") + "\n");
console.log(`Wrote ${output}: ${rust.length} Rust crates, ${npm.length} npm packages, ${python.length} Python packages.`);
