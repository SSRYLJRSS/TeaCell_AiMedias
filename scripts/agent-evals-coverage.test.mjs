import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  collectRustReferences,
  checkCoverage,
  readCaseIds,
  buildRustNameIndex,
} from "./agent-evals-coverage.mjs";

/** 最小工作树：一个 Rust 用例 + 一份 cases.json + 一份覆盖说明。 */
function makeTree({ cases = "E01", evidence = "", rust = "" } = {}) {
  const root = mkdtempSync(join(tmpdir(), "teacell-evals-"));
  mkdirSync(join(root, "tests", "agent-evals"), { recursive: true });
  mkdirSync(join(root, "src-tauri", "src"), { recursive: true });
  writeFileSync(
    join(root, "tests", "agent-evals", "cases.json"),
    JSON.stringify({
      schemaVersion: 1,
      adapter: "test",
      cases: cases.split(",").map((caseId) => ({ caseId, caseVersion: 1 })),
    }),
    "utf8",
  );
  writeFileSync(join(root, "tests", "agent-evals", "README.md"), evidence, "utf8");
  writeFileSync(join(root, "src-tauri", "src", "agent_tools.rs"), rust, "utf8");
  return root;
}

const RUST_MODULE = `#[cfg(test)]\nmod tests {\n    #[test]\n    fn a_real_test_in_the_fixture() {}\n}\n`;

test("真实工作树通过：42 条用例都有覆盖行，引用的 Rust 用例都存在", () => {
  assert.deepEqual(checkCoverage(), []);
  assert.equal(readCaseIds().length, 42);
});

test("用例缺覆盖行时报错", () => {
  const root = makeTree({
    cases: "E01,E02",
    evidence: "| E01 只预览不写入 | `services::agent_tools::tests::a_real_test_in_the_fixture` |\n",
    rust: RUST_MODULE,
  });
  try {
    const problems = checkCoverage({ repositoryRoot: root });
    assert.equal(problems.length, 1);
    assert.equal(problems[0].kind, "missing-case-row");
    assert.match(problems[0].detail, /E02/u);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("覆盖说明引用了不存在的用例号时报错", () => {
  const root = makeTree({
    cases: "E01",
    evidence: "| E01 只预览不写入 | 已覆盖 |\n| E99 幽灵用例 | 已覆盖 |\n",
    rust: RUST_MODULE,
  });
  try {
    const problems = checkCoverage({ repositoryRoot: root });
    assert.equal(problems.length, 1);
    assert.equal(problems[0].kind, "unknown-case-reference");
    assert.match(problems[0].detail, /E99/u);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("引用的 Rust 用例被改名或删除时报错", () => {
  const root = makeTree({
    cases: "E01",
    // 这条用例在源码里不存在 —— 这正是「覆盖表写着已覆盖、实际没有」的那种腐烂。
    evidence: "| E01 只预览不写入 | `services::agent_tools::tests::a_test_that_was_renamed_away` |\n",
    rust: RUST_MODULE,
  });
  try {
    const problems = checkCoverage({ repositoryRoot: root });
    assert.equal(problems.length, 1);
    assert.equal(problems[0].kind, "dangling-rust-reference");
    assert.match(problems[0].detail, /a_test_that_was_renamed_away/u);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("常量与模块也算有效引用，不误报", () => {
  const root = makeTree({
    cases: "E01",
    evidence:
      "| E01 只预览不写入 | `services::agent_runtime::capabilities::SUPPORTED_PI_CAPABILITIES` 与 `services::agent_tools::tests` |\n",
    rust: `${RUST_MODULE}\npub const SUPPORTED_PI_CAPABILITIES: &[u8] = &[];\n`,
  });
  try {
    assert.deepEqual(checkCoverage({ repositoryRoot: root }), []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("只收集三段及以上的 Rust 路径，避免把 `E01` 之类的普通词当引用", () => {
  const references = collectRustReferences("`a::b` 与 `a::b::c` 与 plain_word");
  assert.deepEqual(references, ["a::b::c"]);
});

test("名称索引能读到底层真实文件里的 fn 与 mod", () => {
  const { functions, modules } = buildRustNameIndex();
  assert.ok(functions.has("assets_search_returns_a_page_and_persists_the_exact_selection"));
  assert.ok(modules.has("tests"));
});
