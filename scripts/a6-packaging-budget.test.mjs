import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  computeDelta,
  FROZEN_BUDGETS,
  judgeBudget,
  recomputeBudgets,
  summarize,
  toMiB,
} from "./a6-packaging-budget.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..");
const shippedReportPath = resolve(repoRoot, "artifacts/agent-runtime/a6-03-packaging-budget.json");

// 实测制品 JSON 在干净 CI 里不存在（artifacts/ 被 .gitignore 忽略）。
// 依赖它的断言只在制品存在时运行；缺失则 skip，避免“干净环境 8 项中 1 项失败”的假红。
// 真正的打包门禁（a6-packaging-budget.mjs check）在制品生成后单独执行，不受影响。
function loadShippedReport() {
  if (!existsSync(shippedReportPath)) return null;
  return JSON.parse(readFileSync(shippedReportPath, "utf8"));
}

test("computeDelta uses the no-agent baseline (not the total package) as denominator", () => {
  // 曾把 72986181/31994459 = 228% 误当增量百分比；真实增量比例应约为 128%。
  const delta = computeDelta(72986181, 31994459);
  assert.ok(delta.deltaBytes > 0);
  assert.ok(delta.percentOfBaseline > 100 && delta.percentOfBaseline < 200);
  assert.equal(delta.percentOfBaseline, Math.round((delta.deltaBytes / 31994459) * 10000) / 100);
});

test("computeDelta rejects non-positive baselines and non-numeric bytes", () => {
  assert.throws(() => computeDelta(100, 0), /必须大于 0/);
  assert.throws(() => computeDelta(Number.NaN, 10), /必须是数字/);
});

test("toMiB converts bytes to mebibytes", () => {
  assert.equal(toMiB(1024 * 1024), 1);
  assert.equal(toMiB(2 * 1024 * 1024), 2);
});

test("judgeBudget accepts only explicit pass/fail verdicts", () => {
  assert.equal(judgeBudget({ id: "x", verdict: "pass" }).verdict, "pass");
  assert.equal(judgeBudget({ id: "x", verdict: "fail" }).verdict, "fail");
  assert.equal(judgeBudget({ id: "x", verdict: "maybe" }).verdict, "fail");
  assert.equal(judgeBudget(null).verdict, "fail");
});

test("recomputeBudgets flags an over-budget installer even if the JSON claims pass", () => {
  const report = {
    artifacts: {
      withAgentInstaller: { bytes: 60 * 1024 * 1024 + 31 * 1024 * 1024 + 1024 * 1024 },
      baselineNoAgentInstaller: { bytes: 31 * 1024 * 1024 },
    },
    overlay: { totalUnpackedBytes: 100 * 1024 * 1024 },
    budgets: [
      { id: "host-idle-workingset-mib", measured: "130.45 MiB", limit: "<= 192 MiB", verdict: "pass" },
      { id: "host-ready-ms", measured: "1046 ms", limit: "<= 5000 ms", verdict: "pass" },
      { id: "host-exit-ms", measured: "24 ms", limit: "<= 1000 ms", verdict: "pass" },
    ],
  };
  const results = recomputeBudgets(report);
  const delta = results.find((r) => r.id === "nsis-delta-mib");
  assert.equal(delta.verdict, "fail", "超过 50 MiB 的增量必须判 fail");
});

test("recomputeBudgets passes the shipped A6-03 measurement", (t) => {
  const report = loadShippedReport();
  if (!report) {
    t.skip("artifacts/...a6-03-packaging-budget.json 不存在（干净 CI 无实测制品），跳过实测通过断言");
    return;
  }
  const results = recomputeBudgets(report);
  const summary = summarize(results);
  assert.equal(summary.allPass, true, `预算应全部通过，失败项：${JSON.stringify(results.filter((r) => r.verdict !== 'pass'))}`);
  assert.equal(summary.total, 8);
});

test("recomputeBudgets 真正判定单流与网关增量：塞入 9999 必须 fail，不能假绿色", (t) => {
  // 复现审计发现的假绿色：FROZEN_BUDGETS 定义了 hostSingleStreamBytes(256MiB) 与
  // rustGatewayDeltaBytes(32MiB)，但旧 numericChecks 只查 idle/ready/exit，塞 9999 仍 6/6 通过。
  const report = loadShippedReport();
  if (!report) {
    t.skip("artifacts/...a6-03-packaging-budget.json 不存在（干净 CI 无实测制品），跳过超限判 fail 断言");
    return;
  }
  // 把两条真实测量都改成离谱超限值。
  for (const item of report.budgets) {
    if (item.id === "host-single-stream-mib") item.measured = "9999 MiB";
    if (item.id === "rust-gateway-delta-mib") item.measured = "9999 MiB";
  }
  const results = recomputeBudgets(report);
  const single = results.find((r) => r.id === "host-single-stream-mib");
  const gateway = results.find((r) => r.id === "rust-gateway-delta-mib");
  assert.equal(single?.verdict, "fail", "单流 working set 超 256MiB 必须判 fail");
  assert.equal(gateway?.verdict, "fail", "网关增量超 32MiB 必须判 fail");
  // 缺失实测记录也必须 fail，不能默认放过。
  delete report.budgets.find((b) => b.id === "host-single-stream-mib");
  const missing = recomputeBudgets(report).find((r) => r.id === "host-single-stream-mib");
  assert.equal(missing?.verdict, "fail", "缺少单流实测记录必须判 fail");
});

test("summarize counts passes and failures", () => {
  const summary = summarize([
    { verdict: "pass" },
    { verdict: "pass" },
    { verdict: "fail" },
  ]);
  assert.deepEqual(summary, { total: 3, passed: 2, failed: 1, allPass: false });
});

test("frozen budgets match docs/PERFORMANCE.md §9 values", () => {
  assert.equal(FROZEN_BUDGETS.nsisDeltaBytes, 50 * 1024 * 1024);
  assert.equal(FROZEN_BUDGETS.nsisDeltaPercent, 160);
  assert.equal(FROZEN_BUDGETS.agentUnpackedBytes, 250 * 1024 * 1024);
  assert.equal(FROZEN_BUDGETS.hostIdleWorkingSetBytes, 192 * 1024 * 1024);
  assert.equal(FROZEN_BUDGETS.hostReadyMs, 5000);
  assert.equal(FROZEN_BUDGETS.hostExitMs, 1000);
});
