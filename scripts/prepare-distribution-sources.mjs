#!/usr/bin/env node
/* global process, console, fetch, AbortSignal, Buffer */
/** Materialize the exact, hash-pinned sources used by the distributed native programs. */
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(process.argv[2] ?? join(root, "artifacts", "dependency-inputs"));
const manifest = JSON.parse(readFileSync(join(root, "src-tauri/native/distribution-sources.json"), "utf8"));
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
mkdirSync(join(output, "archives"), { recursive: true });
mkdirSync(join(output, "sources"), { recursive: true });
for (const source of manifest.sources) {
  if (!/^[a-z0-9-]+$/.test(source.name) || !/^[a-f0-9]{40}$/.test(source.sourceCommit) ||
      !/^[a-f0-9]{64}$/.test(source.archiveSha256) || !/^https:\/\/codeload.github.com\//.test(source.archiveUrl) ||
      source.archiveName !== `${source.name}-${source.sourceCommit}.tar.gz`) throw new Error(`Invalid source pin: ${source.name}`);
  const archive = join(output, "archives", source.archiveName);
  const bytes = existsSync(archive) ? readFileSync(archive) : Buffer.from(await (await fetch(source.archiveUrl, {
    signal: AbortSignal.timeout(180_000),
  })).arrayBuffer());
  if (bytes.length !== source.archiveSizeBytes || sha256(bytes) !== source.archiveSha256) {
    throw new Error(`Source SHA256 or size mismatch: ${source.name}`);
  }
  if (!existsSync(archive)) writeFileSync(archive, bytes, { flag: "wx" });
  const list = spawnSync("tar", ["-tzf", archive], { encoding: "utf8", windowsHide: true });
  if (list.status !== 0) throw new Error(`Cannot read source archive: ${source.name}`);
  const entries = list.stdout.split(/\r?\n/).filter(Boolean);
  if (!entries.length || entries.some((entry) => {
    const parts = entry.replaceAll("\\", "/").split("/");
    return parts[0] !== source.sourceDirectory || parts.includes("..") || entry.includes(":");
  })) throw new Error(`Unsafe source archive: ${source.name}`);
  const destination = join(output, "sources", source.sourceDirectory);
  if (!existsSync(destination)) {
    const extraction = spawnSync("tar", ["-xzf", archive, "-C", join(output, "sources")], { stdio: "inherit", windowsHide: true });
    if (extraction.status !== 0) throw new Error(`Source extraction failed: ${source.name}`);
  }
  if (!existsSync(join(destination, source.licensePath))) throw new Error(`Missing upstream license: ${source.name}`);
  console.log(`source verified: ${source.name} ${source.sourceCommit}`);
}
writeFileSync(join(output, "source-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
