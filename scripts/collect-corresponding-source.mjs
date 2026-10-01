#!/usr/bin/env node
/* global process, console, fetch, AbortSignal, Buffer */
/** Stage an auditable source release. Run cargo vendor into <stage>/vendor/cargo first. */
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const stage = resolve(process.argv[2] ?? "artifacts/corresponding-staging");
const application = join(stage, "application");
const hash = (bytes, algorithm = "sha256", encoding = "hex") => createHash(algorithm).update(bytes).digest(encoding);
const run = (command, args) => {
  const result = spawnSync(command, args, { encoding: "utf8", windowsHide: true, maxBuffer: 32 * 1024 * 1024 });
  if (result.status !== 0 || result.error) throw new Error(`${command}: ${result.error?.message ?? result.stderr}`);
  return result.stdout.trim();
};
if (!existsSync(join(stage, "vendor/cargo/rawler-0.7.2/LICENSE"))) throw new Error("Run locked cargo vendor before source collection");
if (run("git", ["status", "--porcelain"])) throw new Error("Corresponding application source requires a clean committed checkout");
const commit = run("git", ["rev-parse", "HEAD"]);
mkdirSync(application, { recursive: true });
run("git", ["archive", "--format=tar", "-o", join(stage, "application.tar"), "HEAD"]);
run("tar", ["-xf", join(stage, "application.tar"), "-C", application]);
cpSync("artifacts/dependency-inputs/archives", join(stage, "vendor/native"), { recursive: true });
cpSync("artifacts/frontend-sources", join(stage, "vendor/frontend-upstream"), { recursive: true });
for (const file of ["src-tauri/native/distribution-sources.json", "src-tauri/native/frontend-sources.json"]) {
  const manifest = JSON.parse(readFileSync(file, "utf8"));
  for (const source of manifest.sources) {
    const directory = file.includes("frontend-") ? "frontend-upstream" : "native";
    const bytes = readFileSync(join(stage, "vendor", directory, source.archiveName));
    if (hash(bytes) !== source.archiveSha256 || bytes.length !== source.archiveSizeBytes) throw new Error(`Source mismatch: ${source.name}`);
  }
}
const lock = JSON.parse(readFileSync("package-lock.json", "utf8"));
mkdirSync(join(stage, "vendor/npm"), { recursive: true });
const packages = [];
for (const [path, meta] of Object.entries(lock.packages)) {
  if (!path || meta.dev || meta.devOptional || !meta.resolved) continue;
  const name = path.split("node_modules/").at(-1);
  const file = `${name.replaceAll("/", "_")}-${meta.version}.tgz`;
  const destination = join(stage, "vendor/npm", file);
  let bytes;
  if (existsSync(destination)) bytes = readFileSync(destination);
  else {
    const url = meta.resolved.replace("https://registry.npmjs.org/", "https://registry.npmmirror.com/");
    const response = await fetch(url, { signal: AbortSignal.timeout(120_000) });
    if (!response.ok) throw new Error(`npm source download ${response.status}: ${name}`);
    bytes = Buffer.from(await response.arrayBuffer());
  }
  const [algorithm, digest] = meta.integrity.split("-");
  if (hash(bytes, algorithm, "base64") !== digest) throw new Error(`npm integrity mismatch: ${name}`);
  writeFileSync(destination, bytes);
  packages.push({ name, version: meta.version, file, integrity: meta.integrity, sha256: hash(bytes), license: meta.license });
  console.log(`npm source verified: ${name}@${meta.version}`);
}
const config = join(application, ".cargo/config.toml");
writeFileSync(config, `${readFileSync(config, "utf8")}\n[source.crates-io]\nreplace-with = "vendored-sources"\n[source.vendored-sources]\ndirectory = "../vendor/cargo"\n`);
writeFileSync(join(stage, "SOURCE-RELEASE.json"), `${JSON.stringify({ license: "GPL-3.0-or-later", commit, npm: packages }, null, 2)}\n`);
writeFileSync(join(stage, "BUILD-SOURCE.md"), `# TeaCell corresponding source\n\nApplication commit: ${commit}. License: GPL-3.0-or-later. Original MIT and third-party notices are preserved.\n\nThe application/ directory is the committed source snapshot. vendor/cargo contains every locked Rust crate, including LGPL rawler and bundled SQLite. vendor/npm contains integrity-verified runtime packages; TanStack packages include their TypeScript src directory. vendor/frontend-upstream contains preferred source for React, scheduler, Tauri API/dialog, clsx, Lucide and Zustand. vendor/native contains all ten native/media source archives and their copyright/license files.\n\n## Build\n\nUse the native target and tools specified in application/docs/PLATFORM.md and .github/workflows/distribution-dependencies.yml (CMake 3.31.6, Node 22, Rust 1.98.1, MSVC on Windows, MinGW for Windows FFmpeg). No private credentials or signing keys are needed.\n\nFrom application/: npm ci; prepare media and HEIF with the explicit native target; npm run desktop:build -- --target <target>. Cargo uses the accompanying vendored sources. npm runtime tarballs may be added with npm cache add ../vendor/npm/<file>; package-lock.json retains exact integrity pins. General-purpose build tools and operating-system libraries are obtained from their standard distributions.\n\nTo rebuild native media: copy vendor/native archives into artifacts/dependency-inputs/archives; run node scripts/prepare-distribution-sources.mjs; activate MSVC x64 on Windows; run node scripts/build-distribution-heif.mjs artifacts/dependency-inputs artifacts/dependency-build; run bash scripts/build-distribution-ffmpeg.sh artifacts/dependency-inputs artifacts/dependency-build; run node scripts/collect-distribution-dependencies.mjs <target> artifacts/dependency-inputs artifacts/dependency-build artifacts/dependencies-<target>. Set CMAKE_COMMAND to the native CMake 3.31.6 executable. The recipe enables GPLv3, local-file media, x264, VP8/Vorbis and AV1 decoding. Build configuration and dependency hashes are in the release build-evidence archive. If rebuilt archive hashes differ due to toolchain/time metadata, update the target manifests and regenerate verified caches before building.\n\nUnsigned installers permit installation of modified builds; the application does not require publisher-controlled keys to run them. Do not reuse this release's checksum file for a modified build.\n`);
const files = [];
function walk(directory, prefix = "") {
  for (const name of readdirSync(directory).sort()) {
    const file = join(directory, name), path = `${prefix}${name}`;
    if (statSync(file).isDirectory()) walk(file, `${path}/`);
    else if (path !== "SOURCE-SHA256SUMS.txt" && path !== "application.tar") files.push(`${hash(readFileSync(file))}  ${path}`);
  }
}
walk(stage);
writeFileSync(join(stage, "SOURCE-SHA256SUMS.txt"), `${files.join("\n")}\n`);
console.log(`source complete: ${commit}, ${files.length} files`);
