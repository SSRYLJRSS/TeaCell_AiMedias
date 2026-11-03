#!/usr/bin/env node
// A6-03 打包/平台/性能预算判定器。
//
// 目的：把 docs/PERFORMANCE.md §9 冻结的 Windows x64 预算与
// artifacts/agent-runtime/a6-03-packaging-budget.json 的实测值做机器判定，
// 避免“人工读数字、口径算错分母”的假通过（曾把总包/基线当增量百分比）。
//
// 用法：
//   node scripts/a6-packaging-budget.mjs check
//   node scripts/a6-packaging-budget.mjs summary
//
// 退出码：全部 pass → 0；任一项 fail 或数据缺失 → 1。

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MIB = 1024 * 1024;

// 冻结预算（docs/PERFORMANCE.md §9，2026-10-09 独立复核）。改这里必须同时改文档。
export const FROZEN_BUDGETS = Object.freeze({
  nsisDeltaBytes: 50 * MIB,
  nsisDeltaPercent: 160,
  agentUnpackedBytes: 250 * MIB,
  hostIdleWorkingSetBytes: 192 * MIB,
  hostSingleStreamBytes: 256 * MIB,
  hostReadyMs: 5000,
  hostExitMs: 1000,
  rustGatewayDeltaBytes: 32 * MIB,
});

export const DEFAULT_BUDGET_PATH = join(
  "artifacts",
  "agent-runtime",
  "a6-03-packaging-budget.json",
);

function round(value, digits = 2) {
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}

export function toMiB(bytes) {
  return bytes / MIB;
}

/**
 * 从原始测量值计算“增量”与“增量占基线比例”。
 * 分母必须是**无 Agent 基线包体**，不是总包体。
 */
export function computeDelta(withAgentBytes, baselineBytes) {
  if (!Number.isFinite(withAgentBytes) || !Number.isFinite(baselineBytes)) {
    throw new Error("包体积必须是数字");
  }
  if (baselineBytes <= 0) {
    throw new Error("无 Agent 基线包体必须大于 0");
  }
  const deltaBytes = withAgentBytes - baselineBytes;
  return {
    deltaBytes,
    deltaMiB: round(toMiB(deltaBytes)),
    baselineMiB: round(toMiB(baselineBytes)),
    percentOfBaseline: round((deltaBytes / baselineBytes) * 100),
  };
}

/** 判定单个带 limit/measured 的预算项；返回 pass/fail 与原因。 */
export function judgeBudget(entry) {
  if (!entry || typeof entry !== "object") {
    return { id: "?", verdict: "fail", reason: "缺少预算条目" };
  }
  const verdict = entry.verdict;
  if (verdict !== "pass" && verdict !== "fail") {
    return { id: entry.id ?? "?", verdict: "fail", reason: "verdict 必须是 pass 或 fail" };
  }
  return {
    id: entry.id ?? "?",
    verdict,
    limit: entry.limit,
    measured: entry.measured,
    reason: verdict === "fail" ? entry.reason ?? "实测超限" : "",
  };
}

/** 用冻结预算重新核算交付 JSON 的关键数字，防止手填的 verdict 与实测不符。 */
export function recomputeBudgets(report) {
  const results = [];
  const artifacts = report?.artifacts;
  if (!artifacts?.withAgentInstaller || !artifacts?.baselineNoAgentInstaller) {
    return [{ id: "package-size", verdict: "fail", reason: "缺少 withAgent/baseline 包体记录" }];
  }
  const withBytes = artifacts.withAgentInstaller.bytes;
  const baseBytes = artifacts.baselineNoAgentInstaller.bytes;
  const delta = computeDelta(withBytes, baseBytes);

  results.push({
    id: "nsis-delta-mib",
    verdict: delta.deltaBytes <= FROZEN_BUDGETS.nsisDeltaBytes ? "pass" : "fail",
    measured: `${delta.deltaMiB} MiB`,
    limit: `<= ${FROZEN_BUDGETS.nsisDeltaBytes / MIB} MiB`,
    reason:
      delta.deltaBytes <= FROZEN_BUDGETS.nsisDeltaBytes
        ? ""
        : `增量 ${delta.deltaMiB} MiB 超出 ${FROZEN_BUDGETS.nsisDeltaBytes / MIB} MiB`,
  });
  results.push({
    id: "nsis-delta-percent",
    verdict: delta.percentOfBaseline <= FROZEN_BUDGETS.nsisDeltaPercent ? "pass" : "fail",
    measured: `${delta.percentOfBaseline}%`,
    limit: `<= ${FROZEN_BUDGETS.nsisDeltaPercent}%`,
    reason:
      delta.percentOfBaseline <= FROZEN_BUDGETS.nsisDeltaPercent
        ? ""
        : `增量占基线 ${delta.percentOfBaseline}% 超出 ${FROZEN_BUDGETS.nsisDeltaPercent}%`,
  });

  const overlayBytes = report?.overlay?.totalUnpackedBytes;
  if (!Number.isFinite(overlayBytes)) {
    results.push({ id: "agent-unpacked-mib", verdict: "fail", reason: "缺少 overlay 解包总量" });
  } else {
    results.push({
      id: "agent-unpacked-mib",
      verdict: overlayBytes <= FROZEN_BUDGETS.agentUnpackedBytes ? "pass" : "fail",
      measured: `${round(toMiB(overlayBytes))} MiB`,
      limit: `<= ${FROZEN_BUDGETS.agentUnpackedBytes / MIB} MiB`,
      reason:
        overlayBytes <= FROZEN_BUDGETS.agentUnpackedBytes
          ? ""
          : `解包总量 ${round(toMiB(overlayBytes))} MiB 超出 ${FROZEN_BUDGETS.agentUnpackedBytes / MIB} MiB`,
    });
  }

  // 剩余条目从 JSON 的 budgets 复制，但用冻结上限复核 measured 数值。
  // 注意：host-single-stream-mib（单流 working set ≤256MiB）与 rust-gateway-delta-mib
  // （独立 Rust 网关增量 ≤32MiB）同样在 docs/PERFORMANCE.md §9 冻结，必须一并判定。
  // 之前只在 FROZEN_BUDGETS 里定义了它们、却没在这里检查，导致把 9999 塞进去也判通过
  // 的“假绿色”；缺失实测记录必须判 fail，不能默认放过。
  const byId = new Map((report?.budgets ?? []).map((item) => [item.id, item]));
  const numericChecks = [
    { id: "host-idle-workingset-mib", limit: FROZEN_BUDGETS.hostIdleWorkingSetBytes / MIB },
    { id: "host-single-stream-mib", limit: FROZEN_BUDGETS.hostSingleStreamBytes / MIB },
    { id: "rust-gateway-delta-mib", limit: FROZEN_BUDGETS.rustGatewayDeltaBytes / MIB },
    { id: "host-ready-ms", limit: FROZEN_BUDGETS.hostReadyMs },
    { id: "host-exit-ms", limit: FROZEN_BUDGETS.hostExitMs },
  ];
  for (const check of numericChecks) {
    const item = byId.get(check.id);
    if (!item) {
      results.push({ id: check.id, verdict: "fail", reason: "缺少实测记录" });
      continue;
    }
    const measuredValue = Number.parseFloat(String(item.measured));
    if (!Number.isFinite(measuredValue)) {
      results.push({ id: check.id, verdict: "fail", reason: `无法解析实测值「${item.measured}」` });
      continue;
    }
    results.push({
      id: check.id,
      verdict: measuredValue <= check.limit ? "pass" : "fail",
      measured: item.measured,
      limit: item.limit,
      reason:
        measuredValue <= check.limit
          ? ""
          : `实测 ${item.measured} 超出上限 ${check.limit}`,
    });
  }
  return results;
}

export function summarize(results) {
  const failed = results.filter((r) => r.verdict !== "pass");
  return {
    total: results.length,
    passed: results.length - failed.length,
    failed: failed.length,
    allPass: failed.length === 0,
  };
}

function readReport(path) {
  const text = readFileSync(path, "utf8");
  return JSON.parse(text);
}

function main(argv) {
  const here = dirname(fileURLToPath(import.meta.url));
  const repoRoot = resolve(here, "..");
  const mode = argv[0] ?? "check";
  const reportPath = resolve(repoRoot, DEFAULT_BUDGET_PATH);

  let report;
  try {
    report = readReport(reportPath);
  } catch (error) {
    console.error(`无法读取预算测量文件 ${reportPath}：${error.message}`);
    return 1;
  }

  const results = recomputeBudgets(report);
  const summary = summarize(results);

  if (mode === "summary") {
    console.log(JSON.stringify({ reportPath, results, summary }, null, 2));
  } else {
    for (const item of results) {
      const mark = item.verdict === "pass" ? "PASS" : "FAIL";
      const line = `${mark}  ${item.id.padEnd(26)} ${String(item.measured).padStart(12)}  (${item.limit})`;
      console.log(item.reason ? `${line}  — ${item.reason}` : line);
    }
    console.log(`\n${summary.passed}/${summary.total} 通过`);
  }
  return summary.allPass ? 0 : 1;
}

const invokedDirectly = process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  process.exit(main(process.argv.slice(2)));
}
