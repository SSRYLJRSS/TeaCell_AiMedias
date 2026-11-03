#!/usr/bin/env node
/* global process, console */
/**
 * 构建缓存（固定目录）的用量审计与清理入口。
 *
 * 位置由 Cargo 自己决定，本脚本不重新实现优先级规则：以 `cargo metadata` 的
 * `target_directory` 为唯一事实，避免 CARGO_TARGET_DIR、config `target-dir`
 * 与默认 `target` 三者之间的相对路径基准差异被脚本猜错。
 */
import { existsSync, readdirSync, rmSync, statSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** 缓存总量上限。超过后 `status` 以非零退出，提示先清理再全量构建。
 * 全量 `cargo test --all-features` 的 debug 目标实测约 24 GiB，加 release/交叉目标可到 ~30 GiB；
 * 24 GiB 会在正常全量构建时必然超限，故上限按用户授权的 50 GiB 设定。 */
export const DEFAULT_BUDGET_BYTES = 50 * 1024 ** 3;
/** 未被触碰超过该天数的顶层条目在默认清理中被删除。 */
export const DEFAULT_RETENTION_DAYS = 7;

export function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) return "unknown";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

/** Cargo 的最终构建目录。cargo 不可用时返回 null，由调用方决定如何降级。 */
export function resolveTargetDirectory(cwd = root) {
  // 仓库根没有 Cargo.toml（crate 在 src-tauri），必须显式给出清单；
  // 同时保持 cwd 在仓库根，使 .cargo/config.toml 的 target-dir 相对基准仍然是仓库根。
  const result = spawnSync(
    "cargo",
    ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", join(cwd, "src-tauri", "Cargo.toml")],
    {
      cwd,
      encoding: "utf8",
      windowsHide: true,
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  if (result.error || result.status !== 0) return null;
  try {
    const parsed = JSON.parse(result.stdout ?? "");
    return typeof parsed.target_directory === "string" && parsed.target_directory
      ? resolve(parsed.target_directory)
      : null;
  } catch {
    return null;
  }
}

/** 拒绝把清理范围指向仓库之外，避免误删用户目录。 */
export function assertInsideRepository(targetDirectory, repositoryRoot = root) {
  const normalizedRoot = resolve(repositoryRoot);
  const normalizedTarget = resolve(targetDirectory);
  if (normalizedTarget === normalizedRoot) {
    throw new Error(`构建缓存目录不能是仓库根：${normalizedTarget}`);
  }
  if (!normalizedTarget.startsWith(`${normalizedRoot}${sep}`)) {
    throw new Error(`构建缓存目录必须位于仓库内，实际解析为：${normalizedTarget}`);
  }
  return normalizedTarget;
}

function entrySize(path) {
  let total = 0;
  const stack = [path];
  while (stack.length) {
    const current = stack.pop();
    let stats;
    try {
      stats = statSync(current);
    } catch {
      continue;
    }
    if (stats.isDirectory()) {
      let children;
      try {
        children = readdirSync(current);
      } catch {
        continue;
      }
      for (const child of children) stack.push(join(current, child));
    } else if (stats.isFile()) {
      total += stats.size;
    }
  }
  return total;
}

/** 顶层条目及其大小、最近修改时间。只统计一层，够用于预算判断。 */
export function inspectCache(targetDirectory) {
  if (!existsSync(targetDirectory)) return { exists: false, path: targetDirectory, totalBytes: 0, entries: [] };
  const entries = readdirSync(targetDirectory).map((name) => {
    const path = join(targetDirectory, name);
    const stats = statSync(path);
    return {
      name,
      path,
      isDirectory: stats.isDirectory(),
      mtimeMs: stats.mtimeMs,
      bytes: entrySize(path),
    };
  });
  entries.sort((left, right) => right.bytes - left.bytes);
  const totalBytes = entries.reduce((sum, entry) => sum + entry.bytes, 0);
  return { exists: true, path: targetDirectory, totalBytes, entries };
}

/**
 * 选出应删除的顶层条目。`incremental` 单独处理：它只影响增量编译速度，
 * 与保留期无关，因此总是可清理。
 */
export function selectPrunable(entries, { now = Date.now(), retentionDays = DEFAULT_RETENTION_DAYS } = {}) {
  const cutoff = now - retentionDays * 24 * 60 * 60 * 1000;
  return entries.filter((entry) => entry.name === "incremental" || entry.mtimeMs < cutoff);
}

export function parseArgs(argv) {
  const [mode, ...rest] = argv;
  const options = { mode, dryRun: false, all: false, retentionDays: DEFAULT_RETENTION_DAYS };
  for (let index = 0; index < rest.length; index += 1) {
    const arg = rest[index];
    if (arg === "--dry-run") options.dryRun = true;
    else if (arg === "--all") options.all = true;
    else if (arg === "--retention-days" && rest[index + 1]) {
      const parsed = Number(rest[++index]);
      if (!Number.isFinite(parsed) || parsed < 0) throw new Error(`--retention-days 需要非负数字：${rest[index]}`);
      options.retentionDays = parsed;
    } else throw new Error(`未知参数：${arg}`);
  }
  return options;
}

function report(targetDirectory, budgetBytes) {
  const cache = inspectCache(targetDirectory);
  if (!cache.exists) {
    console.log(`[build-cache] ${targetDirectory} 不存在（尚未编译）`);
    return 0;
  }
  console.log(`[build-cache] 位置：${cache.path}`);
  console.log(`[build-cache] 总量：${formatBytes(cache.totalBytes)} / 预算 ${formatBytes(budgetBytes)}`);
  for (const entry of cache.entries) {
    console.log(`  ${entry.name.padEnd(28)} ${formatBytes(entry.bytes).padStart(10)}`);
  }
  if (cache.totalBytes > budgetBytes) {
    console.log(`[build-cache] 超出预算 ${formatBytes(cache.totalBytes - budgetBytes)}；运行 npm run build:cache:clean`);
    return 1;
  }
  return 0;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const budgetBytes = DEFAULT_BUDGET_BYTES;

  const resolved = resolveTargetDirectory();
  if (!resolved) throw new Error("无法通过 cargo metadata 确定构建目录；请先确认 cargo 可用");
  const targetDirectory = assertInsideRepository(resolved);

  if (options.mode === "status") {
    process.exitCode = report(targetDirectory, budgetBytes);
    return;
  }
  if (options.mode !== "clean") throw new Error("用法：node scripts/build-cache.mjs <status|clean> [--dry-run] [--all] [--retention-days N]");

  const cache = inspectCache(targetDirectory);
  if (!cache.exists) {
    console.log(`[build-cache] ${targetDirectory} 不存在，无需清理`);
    return;
  }

  if (options.all) {
    // 整个缓存目录是本仓库生成物的唯一固定位置，删除后下次构建会重新填充。
    if (options.dryRun) {
      console.log(`[build-cache] 将删除整个缓存目录：${targetDirectory}（${formatBytes(cache.totalBytes)}）`);
      return;
    }
    rmSync(targetDirectory, { recursive: true, force: true });
    console.log(`[build-cache] 已删除缓存目录，释放 ${formatBytes(cache.totalBytes)}`);
    return;
  }

  const prunable = selectPrunable(cache.entries, { retentionDays: options.retentionDays });
  if (prunable.length === 0) {
    console.log(`[build-cache] 没有超过 ${options.retentionDays} 天的条目；当前 ${formatBytes(cache.totalBytes)}`);
    return;
  }
  const freed = prunable.reduce((sum, entry) => sum + entry.bytes, 0);
  for (const entry of prunable) {
    if (options.dryRun) {
      console.log(`[build-cache] 将删除 ${entry.name}（${formatBytes(entry.bytes)}）`);
      continue;
    }
    rmSync(entry.path, { recursive: true, force: true });
    console.log(`[build-cache] 已删除 ${entry.name}（${formatBytes(entry.bytes)}）`);
  }
  if (options.dryRun) console.log(`[build-cache] 预演：可释放 ${formatBytes(freed)}`);
  else console.log(`[build-cache] 共释放 ${formatBytes(freed)}；剩余 ${formatBytes(inspectCache(targetDirectory).totalBytes)}`);
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    main();
  } catch (error) {
    console.error(`[build-cache] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
