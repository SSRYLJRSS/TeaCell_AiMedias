import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  assertInsideRepository,
  formatBytes,
  inspectCache,
  parseArgs,
  selectPrunable,
} from "./build-cache.mjs";

const DAY_MS = 24 * 60 * 60 * 1000;

test("formatBytes renders human readable sizes with stable precision", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(999), "999 B");
  assert.equal(formatBytes(1024), "1.0 KiB");
  assert.equal(formatBytes(1024 ** 3), "1.0 GiB");
  assert.equal(formatBytes(-1), "unknown");
});

test("assertInsideRepository rejects the repository root and any out-of-tree path", () => {
  const root = "F:\\vibecode\\demo";
  assert.throws(() => assertInsideRepository(root, root), /不能是仓库根/);
  assert.throws(() => assertInsideRepository("F:\\vibecode\\elsewhere\\target", root), /必须位于仓库内/);
  assert.equal(
    assertInsideRepository(join(root, "build-cache", "target"), root),
    join(root, "build-cache", "target"),
  );
});

test("selectPrunable always reclaims incremental and expires only stale entries", () => {
  const now = Date.UTC(2026, 9, 18);
  const entries = [
    { name: "incremental", mtimeMs: now, bytes: 10 },
    { name: "debug", mtimeMs: now, bytes: 20 },
    { name: "release", mtimeMs: now - 30 * DAY_MS, bytes: 30 },
  ];
  const prunable = selectPrunable(entries, { now, retentionDays: 7 });
  assert.deepEqual(
    prunable.map((entry) => entry.name),
    ["incremental", "release"],
  );
});

test("selectPrunable keeps everything inside the retention window", () => {
  const now = Date.UTC(2026, 9, 18);
  const entries = [{ name: "debug", mtimeMs: now - DAY_MS, bytes: 1 }];
  assert.deepEqual(selectPrunable(entries, { now, retentionDays: 7 }), []);
});

test("parseArgs accepts documented flags and rejects unknown ones", () => {
  assert.deepEqual(parseArgs(["status"]), { mode: "status", dryRun: false, all: false, retentionDays: 7 });
  assert.deepEqual(parseArgs(["clean", "--dry-run", "--retention-days", "3"]), {
    mode: "clean",
    dryRun: true,
    all: false,
    retentionDays: 3,
  });
  assert.equal(parseArgs(["clean", "--all"]).all, true);
  assert.throws(() => parseArgs(["clean", "--nope"]), /未知参数/);
  assert.throws(() => parseArgs(["clean", "--retention-days", "-2"]), /非负数字/);
  assert.throws(() => parseArgs(["clean", "--retention-days", "abc"]), /非负数字/);
});

test("inspectCache reports per-entry bytes and sorts largest first", () => {
  const directory = mkdtempSync(join(tmpdir(), "build-cache-test-"));
  try {
    mkdirSync(join(directory, "debug"));
    writeFileSync(join(directory, "debug", "big.bin"), "x".repeat(4096));
    writeFileSync(join(directory, "small.txt"), "hello");
    const cache = inspectCache(directory);
    assert.equal(cache.exists, true);
    assert.equal(cache.totalBytes, 4096 + 5);
    assert.deepEqual(
      cache.entries.map((entry) => entry.name),
      ["debug", "small.txt"],
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("inspectCache reports a missing directory instead of throwing", () => {
  const cache = inspectCache(join(tmpdir(), "build-cache-definitely-missing"));
  assert.equal(cache.exists, false);
  assert.equal(cache.totalBytes, 0);
  assert.deepEqual(cache.entries, []);
});
