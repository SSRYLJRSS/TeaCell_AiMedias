export const RUNNER_BY_TARGET = Object.freeze({
  "x86_64-pc-windows-msvc": { os: "win32", arch: "x64" },
  "aarch64-apple-darwin": { os: "darwin", arch: "arm64" },
  "x86_64-unknown-linux-gnu": { os: "linux", arch: "x64" },
});

export const REQUIRED_PACKAGE_EXTENSIONS = Object.freeze({
  "x86_64-pc-windows-msvc": [".exe", ".msi"],
  "aarch64-apple-darwin": [".dmg"],
  "x86_64-unknown-linux-gnu": [".appimage", ".deb"],
});

export function comparePackageNames(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

export function filterPackageFilesByVersion(files, version) {
  if (!Array.isArray(files) || typeof version !== "string" || !/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) {
    return [];
  }
  const escapedVersion = version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const versionInFilename = new RegExp(`(?:^|[^0-9])${escapedVersion}(?:$|[^0-9])`);
  return files.filter((file) => typeof file === "string" && versionInFilename.test(file));
}

export function assertRunnerMatchesTarget(target, runner) {
  const expected = RUNNER_BY_TARGET[target];
  if (!expected) throw new Error(`未知候选目标：${target}`);
  if (runner?.os !== expected.os || runner?.arch !== expected.arch) {
    throw new Error(
      `runner 与 target 不匹配：${target} 需要 ${expected.os}/${expected.arch}，实际 ${runner?.os ?? "?"}/${runner?.arch ?? "?"}`,
    );
  }
}

export function assertCandidateCheckout(expectedCommit, actualCommit, porcelainStatus) {
  if (!/^[a-f0-9]{40,64}$/i.test(expectedCommit ?? "")) {
    throw new Error("候选必须使用完整 Git commit SHA");
  }
  if (!/^[a-f0-9]{40,64}$/i.test(actualCommit ?? "") || actualCommit.toLowerCase() !== expectedCommit.toLowerCase()) {
    throw new Error(`候选 SHA 与当前 HEAD 不一致：预期 ${expectedCommit}，实际 ${actualCommit || "未知"}`);
  }
  if (typeof porcelainStatus !== "string" || porcelainStatus.length > 0) {
    throw new Error("候选源码工作树不干净；拒绝把未提交改动标记为该 Git SHA 的产物");
  }
}

export function hasRequiredPackageExtensions(target, packageNames) {
  const required = REQUIRED_PACKAGE_EXTENSIONS[target];
  if (!required || !Array.isArray(packageNames)) return false;
  const extensions = new Set(
    packageNames.map((name) => {
      if (typeof name !== "string") return "";
      const dot = name.lastIndexOf(".");
      return dot < 0 ? "" : name.slice(dot).toLowerCase();
    }),
  );
  return required.every((extension) => extensions.has(extension));
}

function normalizePackageMetadata(rows) {
  if (!Array.isArray(rows)) return null;
  const names = new Set();
  const normalized = [];
  for (const row of rows) {
    if (
      !row || typeof row.name !== "string" || !row.name || /[\\/]/.test(row.name) ||
      !Number.isSafeInteger(row.sizeBytes) || row.sizeBytes <= 0 ||
      typeof row.sha256 !== "string" || !/^[a-f0-9]{64}$/i.test(row.sha256) || names.has(row.name)
    ) return null;
    names.add(row.name);
    normalized.push({ name: row.name, sizeBytes: row.sizeBytes, sha256: row.sha256.toLowerCase() });
  }
  return normalized.sort((a, b) => comparePackageNames(a.name, b.name));
}

export function packageMetadataMatches(expected, actual) {
  const left = normalizePackageMetadata(expected);
  const right = normalizePackageMetadata(actual);
  return left !== null && right !== null && JSON.stringify(left) === JSON.stringify(right);
}
