#!/usr/bin/env node
/* global process, console */
// A3-03/A4-08 dependency: eval-case -> coverage-table validator.
//
// `tests/agent-evals/cases.json` defines E01-E40, and
// `tests/agent-evals/README.md` claims which real Rust test backs each case.
// That claim is worth nothing if the named test was renamed or deleted — the
// reader would see "已覆盖" next to a function that no longer exists. The same
// class of rot already happened once in the requirement matrix: R13 pointed at
// three PendingList cases that had never been written, and only a machine check
// caught it.
//
// So this script rebuilds the source index on every run and fails when:
//   1. a case in cases.json has no coverage row in the README;
//   2. the README still cites a case id that cases.json no longer defines;
//   3. a Rust path cited by the README ends in a name that is neither a `fn`
//      nor a `mod` in `src-tauri/`.
//
// Limitation, stated on purpose: rule 3 resolves the *last* path segment only.
// It catches renamed/deleted tests (the rot that actually happens) but it does
// not prove that a fully-qualified module path is nested where it claims to be.
// It also cannot tell whether the cited test asserts what the row says — that
// stays a human review item in A6-04.
//
// Usage:
//   node scripts/agent-evals-coverage.mjs check

import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPOSITORY_ROOT = dirname(HERE);
export const CASES_RELATIVE_PATH = "tests/agent-evals/cases.json";
export const EVIDENCE_RELATIVE_PATH = "tests/agent-evals/README.md";
export const RUST_SOURCE_ROOTS = ["src-tauri/src", "src-tauri/tests"];

/** Case ids look like E01..E40 and are the only stable handle in the eval set. */
const CASE_ID_PATTERN = /^E\d\d$/u;
/** `a::b::c` with at least two separators — the shape Rust test paths are cited in. */
const RUST_REFERENCE_PATTERN = /(?:[A-Za-z0-9_]+::)+[A-Za-z0-9_]+/gu;

function walk(directory, collected = []) {
  let entries;
  try {
    entries = readdirSync(directory, { withFileTypes: true });
  } catch {
    return collected;
  }
  for (const entry of entries) {
    const absolute = join(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "target") continue;
      walk(absolute, collected);
    } else if (entry.isFile() && entry.name.endsWith(".rs")) {
      collected.push(absolute);
    }
  }
  return collected;
}

/** Every item name declared under the Rust source roots (fn/const/static/struct/enum/trait/type/mod). */
export function buildRustNameIndex(repositoryRoot = REPOSITORY_ROOT) {
  const functions = new Set();
  const modules = new Set();
  const itemPattern = /\b(?:fn|const|static|struct|enum|trait|type)\s+([A-Za-z0-9_]+)/gu;
  for (const root of RUST_SOURCE_ROOTS) {
    for (const file of walk(join(repositoryRoot, root))) {
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(itemPattern)) functions.add(match[1]);
      for (const match of text.matchAll(/\bmod\s+([A-Za-z0-9_]+)\s*[;{]/gu)) modules.add(match[1]);
    }
  }
  return { functions, modules };
}

/** Case ids declared by the eval fixture, in file order. */
export function readCaseIds(repositoryRoot = REPOSITORY_ROOT) {
  const fixture = JSON.parse(readFileSync(join(repositoryRoot, CASES_RELATIVE_PATH), "utf8"));
  const cases = Array.isArray(fixture) ? fixture : fixture.cases;
  if (!Array.isArray(cases)) throw new Error(`${CASES_RELATIVE_PATH} 里没有 cases 数组`);
  return cases.map((entry) => {
    const id = entry?.caseId;
    if (typeof id !== "string" || !CASE_ID_PATTERN.test(id)) {
      throw new Error(`${CASES_RELATIVE_PATH} 含不可识别的 caseId：${JSON.stringify(id)}`);
    }
    return id;
  });
}

/** Rust paths cited by the coverage document. */
export function collectRustReferences(text) {
  const found = new Set();
  for (const match of text.matchAll(RUST_REFERENCE_PATTERN)) {
    const token = match[0];
    if (token.split("::").length >= 3) found.add(token);
  }
  return [...found].sort();
}

/**
 * Every problem found, as `{ kind, detail }`. Empty array means the coverage
 * table and the fixture agree and every cited test still exists.
 */
export function checkCoverage({ repositoryRoot = REPOSITORY_ROOT } = {}) {
  const problems = [];
  const caseIds = readCaseIds(repositoryRoot);
  const evidence = readFileSync(join(repositoryRoot, EVIDENCE_RELATIVE_PATH), "utf8");
  const { functions, modules } = buildRustNameIndex(repositoryRoot);

  for (const id of caseIds) {
    // 用词边界匹配，避免 E01 命中 E010 之类的前缀假阳性。
    const declared = new RegExp(`(^|[^A-Za-z0-9_])${id}([^0-9]|$)`, "u").test(evidence);
    if (!declared) problems.push({ kind: "missing-case-row", detail: `${id} 在 ${EVIDENCE_RELATIVE_PATH} 里没有覆盖行` });
  }

  const known = new Set(caseIds);
  for (const match of evidence.matchAll(/(?:^|[^A-Za-z0-9_])(E\d\d)(?![0-9])/gu)) {
    if (!known.has(match[1])) {
      problems.push({
        kind: "unknown-case-reference",
        detail: `${EVIDENCE_RELATIVE_PATH} 引用了 ${CASES_RELATIVE_PATH} 未定义的 ${match[1]}`,
      });
    }
  }

  for (const reference of collectRustReferences(evidence)) {
    const last = reference.split("::").at(-1);
    if (!functions.has(last) && !modules.has(last)) {
      problems.push({ kind: "dangling-rust-reference", detail: `${reference} 的末段 ${last} 在 src-tauri/ 里既不是 fn 也不是 mod` });
    }
  }

  return problems;
}

function main(argv) {
  const command = argv[2] ?? "check";
  if (command !== "check") {
    console.error(`未知子命令：${command}。用法：node scripts/agent-evals-coverage.mjs check`);
    return 2;
  }
  const problems = checkCoverage();
  if (problems.length === 0) {
    console.log(`评估覆盖表校验通过：${readCaseIds().length} 条用例都有覆盖行，引用的 Rust 用例都存在。`);
    return 0;
  }
  for (const problem of problems) console.error(`错误：${problem.kind} —— ${problem.detail}`);
  console.error(`\n共 ${problems.length} 处不一致。`);
  return 1;
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1].replaceAll("\\", "/")}`).href) {
  process.exit(main(process.argv));
}
