import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  blockingGaps,
  collectFrontendCases,
  collectIgnoredTests,
  collectPlaybookIds,
  collectSpecificationProgressClaims,
  collectRustTestNames,
  summarize,
  validateMatrix,
} from "./acceptance-evidence.mjs";

/** 最小工作树：一处 Rust 用例、一个前端测试文件、一行手册条目。 */
function makeTree({ rustName = "assets_search_binds_the_exact_selection" } = {}) {
  const root = mkdtempSync(join(tmpdir(), "teacell-acceptance-"));
  mkdirSync(join(root, "src-tauri", "src"), { recursive: true });
  mkdirSync(join(root, "src", "pages"), { recursive: true });
  mkdirSync(join(root, "docs"), { recursive: true });
  writeFileSync(
    join(root, "src-tauri", "src", "agent_tools.rs"),
    `#[cfg(test)]\nmod tests {\n    #[test]\n    fn ${rustName}() {\n        assert!(true);\n    }\n}\n`,
    "utf8",
  );
  writeFileSync(
    join(root, "src", "pages", "Demo.test.tsx"),
    `describe("演示", () => {\n  it("表格里能看到本次选中项", () => {});\n  it("清空后不再保留选择", () => {});\n});\n`,
    "utf8",
  );
  writeFileSync(
    join(root, "docs", "QA_PLAYBOOK.md"),
    ["| A-01 | 空库首次引导 |", "| X-01 安装与启动 | 安装候选包并启动 |", "| I-36b | 真实客户端 |"].join("\n"),
    "utf8",
  );
  return root;
}

function indexOf(root) {
  return {
    rust: collectRustTestNames(root),
    frontend: collectFrontendCases(root),
    playbook: collectPlaybookIds(root),
  };
}

/** 造一份结构合法、只含一条需求的矩阵；缺的 R02–R60 由校验器报出来。 */
function matrixWith(overrides = {}) {
  return {
    contractVersion: "acceptance-evidence-v1",
    requirements: [
      {
        id: "R01",
        requirement: "示例需求",
        version: "2.0",
        status: "covered",
        cards: ["A6-01"],
        automated: { rust: ["assets_search_binds_the_exact_selection"] },
        ...overrides,
      },
    ],
  };
}

test("collectRustTestNames indexes both `#[test]` and `#[tokio::test]` bodies", () => {
  const root = makeTree();
  try {
    const names = collectRustTestNames(root);
    assert.ok(names.has("assets_search_binds_the_exact_selection"));
    assert.equal(names.get("assets_search_binds_the_exact_selection"), "src-tauri/src/agent_tools.rs");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("collectFrontendCases reads every it/test title in a *.test.tsx file", () => {
  const root = makeTree();
  try {
    const cases = collectFrontendCases(root);
    const titles = cases.get("src/pages/Demo.test.tsx");
    assert.deepEqual(titles, ["表格里能看到本次选中项", "清空后不再保留选择"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("collectPlaybookIds accepts both playbook row shapes including X-01", () => {
  const root = makeTree();
  try {
    const ids = collectPlaybookIds(root);
    // `| A-01 |` 与 `| X-01 安装与启动 |` 是手册里并存的两种写法。
    // 只认第一种会让平台条目在索引里整体消失（A6-01 实测踩到）。
    // 带字母后缀的行（H-09b / I-36b）也必须原样保留，否则矩阵无法引用它们。
    assert.deepEqual([...ids].sort(), ["A-01", "I-36b", "X-01"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a reference that does not exist fails instead of silently passing", () => {
  const root = makeTree();
  try {
    const { errors } = validateMatrix(
      matrixWith({ automated: { rust: ["assets_search_binds_the_wrong_selection"] } }),
      indexOf(root),
    );
    assert.ok(errors.some((error) => error.includes("不是真实存在的")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an ambiguous substring reference is refused rather than guessed", () => {
  const root = makeTree();
  try {
    writeFileSync(
      join(root, "src-tauri", "src", "agent_tools.rs"),
      `mod tests {\n#[test]\nfn assets_search_one() {}\n#[test]\nfn assets_search_two() {}\n}\n`,
      "utf8",
    );
    const { errors } = validateMatrix(matrixWith({ automated: { rust: ["assets_search"] } }), indexOf(root));
    assert.ok(errors.some((error) => error.includes("匹配到多个")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a frontend reference needs contains or minCases, not just a file path", () => {
  const root = makeTree();
  try {
    const file = "src/pages/Demo.test.tsx";
    const bare = validateMatrix(matrixWith({ automated: { frontend: [{ file }] } }), indexOf(root));
    assert.ok(bare.errors.some((error) => error.includes("否则只证明文件存在")));

    const missing = validateMatrix(
      matrixWith({ automated: { frontend: [{ file, contains: ["根本不存在的用例"] }] } }),
      indexOf(root),
    );
    assert.ok(missing.errors.some((error) => error.includes("里没有包含")));

    const enough = validateMatrix(
      matrixWith({ automated: { frontend: [{ file, minCases: 2 }] } }),
      indexOf(root),
    );
    assert.ok(!enough.errors.some((error) => error.includes("Demo.test.tsx")));

    const tooFew = validateMatrix(
      matrixWith({ automated: { frontend: [{ file, minCases: 9 }] } }),
      indexOf(root),
    );
    assert.ok(tooFew.errors.some((error) => error.includes("少于声明的 9")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("covered needs automated evidence and every gap needs a reason", () => {
  const root = makeTree();
  try {
    const coveredWithoutEvidence = validateMatrix(
      matrixWith({ automated: {} }),
      indexOf(root),
    );
    assert.ok(coveredWithoutEvidence.errors.some((error) => error.includes("status=covered 但没有任何自动化证据")));

    const gapWithoutReason = validateMatrix(
      matrixWith({ status: "gap", automated: {} }),
      indexOf(root),
    );
    assert.ok(gapWithoutReason.errors.some((error) => error.includes("必须写 reason")));

    const gapWithReason = validateMatrix(
      matrixWith({ status: "gap", automated: {}, reason: "真机安装证据属 A6-03" }),
      indexOf(root),
    );
    assert.ok(!gapWithReason.errors.some((error) => error.includes("必须写 reason")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("manual evidence must name a real playbook row and an honest status", () => {
  const root = makeTree();
  try {
    const unknown = validateMatrix(matrixWith({ manual: [{ id: "Z-99", status: "executed" }] }), indexOf(root));
    assert.ok(unknown.errors.some((error) => error.includes("QA 手册里没有条目")));

    const badStatus = validateMatrix(matrixWith({ manual: [{ id: "A-01", status: "maybe" }] }), indexOf(root));
    assert.ok(badStatus.errors.some((error) => error.includes("status 必须是 executed / pending")));

    const ok = validateMatrix(matrixWith({ manual: [{ id: "X-01", status: "pending" }] }), indexOf(root));
    assert.ok(!ok.errors.some((error) => error.includes("QA 手册里没有条目")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a matrix missing R02–R60 is rejected, so the table cannot shrink silently", () => {
  const root = makeTree();
  try {
    const { errors } = validateMatrix(matrixWith(), indexOf(root));
    assert.ok(errors.some((error) => error.includes("R60: 覆盖表里缺这一条")));
    assert.ok(!errors.some((error) => error.startsWith("R01: 覆盖表里缺这一条")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("blockingGaps only reports 2.0 scope, and summarize counts by version", () => {
  const matrix = {
    requirements: [
      { id: "R01", version: "2.0", status: "covered", automated: { rust: ["a"] } },
      { id: "R02", version: "2.0", status: "gap", reason: "缺真机证据", automated: {} },
      { id: "R03", version: "2.5", status: "deferred", reason: "B 卡范围", automated: {} },
    ],
  };
  assert.deepEqual(blockingGaps(matrix), ["R02（gap）：缺真机证据"]);
  const summary = summarize(matrix);
  assert.deepEqual(summary.byVersion["2.0"], { total: 2, covered: 1, gap: 1, deferred: 0 });
  assert.equal(summary.distinctRustTests, 1);
  assert.equal(summary.frontendFiles, 0);
});

// R59 要求清点「未被跳过的必要测试」。只读矩阵看不出这件事：一个被 `#[ignore]`
// 关掉的用例仍然有名字，引用依然能解析成功，于是门禁全绿而测试其实没跑。
// 下面三条锁住「跳过必须写明环境或安全前置」这条线。
test("an ignored test with no stated reason is reported, not silently accepted", () => {
  const root = mkdtempSync(join(tmpdir(), "teacell-ignore-"));
  try {
    mkdirSync(join(root, "src-tauri", "src"), { recursive: true });
    writeFileSync(
      join(root, "src-tauri", "src", "lib.rs"),
      "mod inner {\n    #[test]\n    #[ignore]\n    fn quietly_skipped() {}\n}\n",
      "utf8",
    );
    const { ignored, problems } = collectIgnoredTests(root);
    assert.equal(ignored.length, 0);
    assert.equal(problems.length, 1);
    // 上方紧邻的 `#[test]` 属性会被当作上下文收进来，于是走「理由不足」这条，
    // 而不是「完全没有理由」。两条路径都必须拒绝，这里只锁「拒绝且点名文件与函数」。
    assert.match(problems[0], /quietly_skipped/);
    assert.match(problems[0], /src-tauri\/src\/lib\.rs:3/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an ignored test with no attributes or comments above it at all is reported as unexplained", () => {
  const root = mkdtempSync(join(tmpdir(), "teacell-ignore-"));
  try {
    mkdirSync(join(root, "src-tauri", "src"), { recursive: true });
    writeFileSync(join(root, "src-tauri", "src", "lib.rs"), "#[ignore]\nfn bare_ignore() {}\n", "utf8");
    const { ignored, problems } = collectIgnoredTests(root);
    assert.equal(ignored.length, 0);
    assert.equal(problems.length, 1);
    assert.match(problems[0], /没有任何理由/);
    assert.match(problems[0], /bare_ignore/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an ignored test whose stated reason names no environment or safety precondition is reported", () => {
  const root = mkdtempSync(join(tmpdir(), "teacell-ignore-"));
  try {
    mkdirSync(join(root, "src-tauri", "src"), { recursive: true });
    writeFileSync(
      join(root, "src-tauri", "src", "lib.rs"),
      'mod inner {\n    #[test]\n    #[ignore = "先这样"]\n    fn postponed() {}\n}\n',
      "utf8",
    );
    const { ignored, problems } = collectIgnoredTests(root);
    assert.equal(ignored.length, 0);
    assert.equal(problems.length, 1);
    assert.match(problems[0], /未说明环境或安全前置/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an ignored test with a real precondition is counted and explained instead of failing the gate", () => {
  const root = mkdtempSync(join(tmpdir(), "teacell-ignore-"));
  try {
    mkdirSync(join(root, "src-tauri", "src"), { recursive: true });
    writeFileSync(
      join(root, "src-tauri", "src", "lib.rs"),
      'mod inner {\n    #[test]\n    #[ignore = "需真实外发凭据与网络"]\n    fn needs_real_egress() {}\n}\n',
      "utf8",
    );
    const { ignored, problems } = collectIgnoredTests(root);
    assert.deepEqual(problems, []);
    assert.equal(ignored.length, 1);
    assert.equal(ignored[0].function, "needs_real_egress");
    assert.match(ignored[0].reason, /需真实外发凭据与网络/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// R60: 进度只在 PROJECT_PLAN 维护。这三条把「唯一进度事实源」从文档承诺变成门禁：
// 规格文件长出第二份进度要失败，PROJECT_PLAN 自己不说清归属也要失败。
function makeAuthorityTree({ projectPlan = "# 项目计划\n\n> **进度只在本文档维护。**\n" } = {}, plans = {}) {
  const root = mkdtempSync(join(tmpdir(), "teacell-authority-"));
  mkdirSync(join(root, "docs"), { recursive: true });
  writeFileSync(join(root, "docs", "PROJECT_PLAN.md"), projectPlan, "utf8");
  if (plans !== null) {
    mkdirSync(join(root, "plans", "v2", "execution"), { recursive: true });
    for (const [name, content] of Object.entries(plans)) {
      writeFileSync(join(root, "plans", "v2", name), content, "utf8");
    }
  }
  return root;
}

test("a specification file that claims a card is accepted is rejected as a second progress source", () => {
  const root = makeAuthorityTree({}, {
    "execution/A6.md": "# A6\n\n**验收与交付：** 本卡已验收，证据见矩阵。\n",
  });
  try {
    const { claims, missing } = collectSpecificationProgressClaims(root);
    assert.deepEqual(missing, []);
    assert.equal(claims.length, 1);
    assert.match(claims[0], /plans\/v2\/execution\/A6\.md:3/);
    assert.match(claims[0], /本卡已验收/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a specification file restating that it is not a progress source is not a claim", () => {
  const root = makeAuthorityTree({}, {
    "execution/README.md":
      "# 任务索引\n\n这不是已完成进度；实际交付状态只在 PROJECT_PLAN。\n\n- 原型不被当作已交付功能。\n- 代码生成完毕不等于任务验收通过。\n",
  });
  try {
    const { claims, missing } = collectSpecificationProgressClaims(root);
    assert.deepEqual(missing, []);
    assert.deepEqual(claims, []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("PROJECT_PLAN must declare that it owns progress, otherwise the gate has no authority to point at", () => {
  const root = makeAuthorityTree({ projectPlan: "# 项目计划\n\n随便写点状态。\n" }, {});
  try {
    const { missing } = collectSpecificationProgressClaims(root);
    assert.equal(missing.length, 1);
    assert.match(missing[0], /没有声明自己是唯一进度事实源/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
