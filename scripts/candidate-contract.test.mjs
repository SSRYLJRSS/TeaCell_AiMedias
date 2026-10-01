import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { Buffer } from "node:buffer";
import { URL } from "node:url";
import {
  assertCandidateCheckout,
  assertRunnerMatchesTarget,
  filterPackageFilesByVersion,
  hasRequiredPackageExtensions,
  packageMetadataMatches,
  RUNNER_BY_TARGET,
} from "./candidate-contract.mjs";

test("English package identity preserves the legacy MSI upgrade family", () => {
  const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
  assert.equal(config.productName, "TeaCell AI Media Manager");
  assert.equal(config.identifier, "com.bagertea.aimdeias");
  // Tauri's documented UUID-v5 derivation for the original Windows x64 product.
  const dnsNamespace = Buffer.from("6ba7b8109dad11d180b400c04fd430c8", "hex");
  const digest = createHash("sha1").update(dnsNamespace).update("茶馆.exe.app.x64").digest().subarray(0, 16);
  digest[6] = (digest[6] & 0x0f) | 0x50;
  digest[8] = (digest[8] & 0x3f) | 0x80;
  const hex = digest.toString("hex");
  const legacyUpgradeCode = `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  assert.equal(config.bundle.windows.wix.upgradeCode, legacyUpgradeCode);
  const englishPackages = ["TeaCell AI Media Manager_1.0.2_x64-setup.exe", "TeaCell AI Media Manager_1.0.20_x64-setup.exe"];
  assert.deepEqual(filterPackageFilesByVersion(englishPackages, "1.0.2"), [englishPackages[0]]);
});

test("English install identity uses Chinese desktop and start-menu link names", () => {
  const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
  assert.equal(config.productName, "TeaCell AI Media Manager");
  assert.equal(config.bundle.windows.nsis.template, "nsis/installer.nsi");
  const template = readFileSync(new URL("../src-tauri/nsis/installer.nsi", import.meta.url), "utf8");
  assert.match(template, /!define SHORTCUTNAME "茶馆"/);
  assert.match(template, /CreateShortcut "\$DESKTOP\\\$\{SHORTCUTNAME\}\.lnk"/);
  assert.match(template, /CreateShortcut "\$SMPROGRAMS\\\$\{SHORTCUTNAME\}\.lnk"/);
  assert.doesNotMatch(template, /\$\{PRODUCTNAME\}\.lnk/);
});

test("candidate metadata only accepts the exact clean source commit", () => {
  const commit = "a".repeat(40);
  assert.doesNotThrow(() => assertCandidateCheckout(commit, commit, ""));
  assert.doesNotThrow(() => assertCandidateCheckout(commit.toUpperCase(), commit, ""));
  assert.throws(
    () => assertCandidateCheckout(commit, "b".repeat(40), ""),
    /候选 SHA 与当前 HEAD 不一致/,
  );
  assert.throws(
    () => assertCandidateCheckout(commit, commit, " M src/App.tsx\n"),
    /工作树不干净/,
  );
  assert.throws(
    () => assertCandidateCheckout(commit, commit, "?? docs/untracked-note.md\n"),
    /工作树不干净/,
  );
  assert.throws(() => assertCandidateCheckout("abc", commit, ""), /完整 Git commit SHA/);
});

test("candidate target contract maps to the required native runner architectures", () => {
  for (const [target, runner] of Object.entries(RUNNER_BY_TARGET)) {
    assert.doesNotThrow(() => assertRunnerMatchesTarget(target, runner));
  }
  assert.throws(
    () => assertRunnerMatchesTarget("aarch64-apple-darwin", { os: "darwin", arch: "x64" }),
    /runner 与 target 不匹配/,
  );
  assert.throws(() => assertRunnerMatchesTarget("unknown-target", { os: "linux", arch: "x64" }), /未知候选目标/);
});

test("candidate package contract requires every planned installer type", () => {
  assert.equal(
    hasRequiredPackageExtensions("x86_64-pc-windows-msvc", ["app_x64-setup.exe", "app_x64.msi"]),
    true,
  );
  assert.equal(hasRequiredPackageExtensions("x86_64-pc-windows-msvc", ["app_x64-setup.exe"]), false);
  assert.equal(
    hasRequiredPackageExtensions("x86_64-unknown-linux-gnu", ["app-x86_64.AppImage", "app_amd64.deb"]),
    true,
  );
  assert.equal(hasRequiredPackageExtensions("x86_64-unknown-linux-gnu", ["app_amd64.deb"]), false);
});

test("candidate collection excludes stale bundles from other app versions", () => {
  const packages = [
    "茶馆_1.0.1_x64-setup.exe",
    "茶馆_1.0.1_x64_en-US.msi",
    "茶馆_1.0.2_x64-setup.exe",
    "茶馆_1.0.2_x64_en-US.msi",
    "茶馆_1.0.2_x64_zh-CN.msi",
    "茶馆_1.0.20_x64-setup.exe",
  ];
  assert.deepEqual(filterPackageFilesByVersion(packages, "1.0.2"), [
    "茶馆_1.0.2_x64-setup.exe",
    "茶馆_1.0.2_x64_en-US.msi",
    "茶馆_1.0.2_x64_zh-CN.msi",
  ]);
  assert.deepEqual(filterPackageFilesByVersion(packages, "not-a-version"), []);
});

test("candidate package metadata comparison catches missing, altered, and duplicate records", () => {
  const valid = [
    { name: "app.msi", sizeBytes: 128, sha256: "a".repeat(64) },
    { name: "app.exe", sizeBytes: 256, sha256: "b".repeat(64) },
  ];
  assert.equal(packageMetadataMatches(valid, [...valid].reverse()), true);
  assert.equal(packageMetadataMatches(valid, [{ ...valid[0], sizeBytes: 127 }, valid[1]]), false);
  assert.equal(packageMetadataMatches(valid, [{ ...valid[0], sha256: "c".repeat(64) }, valid[1]]), false);
  assert.equal(packageMetadataMatches(valid, [valid[0]]), false);
  assert.equal(packageMetadataMatches([valid[0], valid[0]], valid), false);
});
