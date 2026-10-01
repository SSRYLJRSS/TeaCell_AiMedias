#!/usr/bin/env node
/* global process, console */
import { createHash } from "node:crypto";
import { copyFileSync, cpSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const [target, inputsArg, buildArg, destinationArg] = process.argv.slice(2);
const inputs = resolve(inputsArg), build = resolve(buildArg), destination = resolve(destinationArg);
const manifest = JSON.parse(readFileSync(join(inputs, "source-manifest.json"), "utf8"));
const windows = process.platform === "win32";
const libraries = windows ? ["heif.lib", "x265-static.lib", "libde265.lib"] : ["libheif.a", "libx265.a", "libde265.a"];
const hash = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
mkdirSync(destination, { recursive: true });
function archive(name, directory, paths) {
  const file = join(destination, name);
  const run = windows ? spawnSync("7z", ["a", "-tzip", "-mx=9", file, ...paths], { cwd: directory, stdio: "inherit", windowsHide: true })
    : spawnSync("zip", ["-q", "-r", file, ...paths], { cwd: directory, stdio: "inherit" });
  if (run.status !== 0 || run.error) throw new Error(`Archive failed: ${name}`);
  return { name, sizeBytes: statSync(file).size, sha256: hash(file) };
}
const heif = archive(`TeaCell_HEIF_${target}.zip`, join(build, "heif"), ["include", "lib"]);
if (process.argv.includes("--heif-only")) {
  const librariesSha256 = Object.fromEntries(libraries.map((file) => [file, hash(join(build, "heif", "lib", file))]));
  copyFileSync(join(build, "heif-build-commands.json"), join(destination, "heif-build-commands.json"));
  writeFileSync(join(destination, "dependency-manifest.json"), `${JSON.stringify({ target, commit: process.env.GITHUB_SHA, sources: manifest, heif: { ...heif, libraries, librariesSha256 } }, null, 2)}\n`);
  console.log(`HEIF-only source build collected: ${target}`);
  process.exit(0);
}
const ffmpeg = archive(`TeaCell_FFmpeg_${target}.zip`, join(build, "ffmpeg"), ["bin", "LICENSE.txt"]);
const librariesSha256 = Object.fromEntries(libraries.map((file) => [file, hash(join(build, "heif", "lib", file))]));
const suffix = windows ? ".exe" : "";
const binariesSha256 = Object.fromEntries(["ffmpeg", "ffprobe"].map((name) => [name, hash(join(build, "ffmpeg/bin", `${name}${suffix}`))]));
cpSync(join(inputs, "archives"), join(destination, "sources"), { recursive: true });
copyFileSync(join(inputs, "source-manifest.json"), join(destination, "source-manifest.json"));
for (const file of ["heif-build-commands.json", "ffmpeg-buildconf.txt", "ffmpeg-config.log"]) copyFileSync(join(build, file), join(destination, file));
for (const file of readdirSync(destination).filter((name) => name.endsWith(".log"))) console.log(`build evidence: ${file}`);
writeFileSync(join(destination, "dependency-manifest.json"), `${JSON.stringify({ target, commit: process.env.GITHUB_SHA, sources: manifest,
  heif: { ...heif, libraries, librariesSha256 }, ffmpeg: { ...ffmpeg, version: "7.1.3", binariesSha256, licenseSha256: hash(join(build, "ffmpeg/LICENSE.txt")) } }, null, 2)}\n`);
