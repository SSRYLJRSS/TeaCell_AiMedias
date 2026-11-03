#!/usr/bin/env node
/* global process, console */
// A6-01 dependency: requirement -> evidence matrix validator.
//
// The matrix at tests/acceptance/requirements.json is the only place that says
// which real test (or which documented manual procedure) backs each confirmed
// requirement R01-R60. This script exists because the A6-01 card forbids the
// cheap answer ("the component exists, therefore the requirement is done"):
// every reference must resolve to a test case that actually exists in the
// working tree, or the check fails.
//
// The index is rebuilt from source on every run. build-cache/ holds a copy of
// the same lists for humans, but nothing here reads it: build-cache/ is pruned
// on a schedule, so a gate that depended on it would silently go vacuous after
// the first cleanup.
//
// Usage:
//   node scripts/acceptance-evidence.mjs check            # structure + references
//   node scripts/acceptance-evidence.mjs check --strict    # also require no 2.0 gap
//   node scripts/acceptance-evidence.mjs summary          # per-version counts

import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";

import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPOSITORY_ROOT = dirname(HERE);
export const MATRIX_RELATIVE_PATH = "tests/acceptance/requirements.json";

/** Versions a requirement can be scoped to. */
export const REQUIREMENT_VERSIONS = ["2.0", "2.5", "engineering"];

/** `covered` needs real evidence; `gap` needs a reason; `deferred` needs a card. */
export const EVIDENCE_STATUSES = ["covered", "gap", "deferred"];

const REQUIREMENT_ID_PATTERN = /^R(\d\d)$/u;
const RUST_SOURCE_ROOTS = ["src-tauri/src", "src-tauri/tests"];
const FRONTEND_SOURCE_ROOT = "src";
const PLAYBOOK_RELATIVE_PATH = "docs/QA_PLAYBOOK.md";

function walk(directory, predicate, collected = []) {
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
      walk(absolute, predicate, collected);
    } else if (entry.isFile() && predicate(entry.name)) {
      collected.push(absolute);
    }
  }
  return collected;
}

/**
 * Collects every Rust test name in the crate.
 *
 * Attributes are separated from the `fn` by other attributes often enough
 * (`#[test]` then `#[ignore = "..."]`) that the gap is matched rather than
 * assumed to be empty.
 */
export function collectRustTestNames(root = REPOSITORY_ROOT) {
  const names = new Map();
  for (const sourceRoot of RUST_SOURCE_ROOTS) {
    for (const file of walk(join(root, sourceRoot), (name) => name.endsWith(".rs"))) {
      const text = readFileSync(file, "utf8");
      const pattern = /#\[(?:test|tokio::test[^\]]*)\][\s\S]{0,240}?\bfn\s+([A-Za-z0-9_]+)\s*\(/gu;
      for (const match of text.matchAll(pattern)) {
        const name = match[1];
        const key = relative(root, file).replaceAll("\\", "/");
        if (!names.has(name)) names.set(name, key);
      }
    }
  }
  return names;
}

/**
 * Collects frontend case titles per test file.
 *
 * Only the literal form is read: this repository writes `it("...", ...)` or
 * `test("...", ...)` everywhere. A table-driven `it.each` still has a literal
 * template, so the titles remain greppable.
 */
export function collectFrontendCases(root = REPOSITORY_ROOT) {
  const cases = new Map();
  for (const file of walk(join(root, FRONTEND_SOURCE_ROOT), (name) => /\.test\.tsx?$/u.test(name))) {
    const text = readFileSync(file, "utf8");
    const key = relative(root, file).replaceAll("\\", "/");
    const titles = [];
    const pattern = /\b(?:it|test)(?:\.\w+)*\(\s*(["'`])([\s\S]{0,400}?)\1\s*,/gu;
    for (const match of text.matchAll(pattern)) {
      titles.push(match[2].replaceAll("\\n", "\n"));
    }
    cases.set(key, titles);
  }
  return cases;
}

/** Collects the playbook row ids (`A-01`, `I-36`, `H-09c`, ...). */
export function collectPlaybookIds(root = REPOSITORY_ROOT) {
  const ids = new Set();
  let text;
  try {
    text = readFileSync(join(root, PLAYBOOK_RELATIVE_PATH), "utf8");
  } catch {
    return ids;
  }
  for (const line of text.split("\n")) {
    // 手册里两种行格式并存：`| A-01 | 名称 |` 与 `| X-01 安装与启动 | …`。
    // 只认第一种会让 X-01~X-06 这些平台条目在索引里整体消失（A6-01 实测踩到），
    // 于是矩阵写对了条目却被判「不存在」。
    const match = /^\|\s*([A-Z])-(\d\d[a-z]?)(?:\s*\||\s)/u.exec(line);
    if (match) ids.add(`${match[1]}-${match[2]}`);
  }
  return ids;
}

export function loadMatrix(root = REPOSITORY_ROOT) {
  const path = join(root, MATRIX_RELATIVE_PATH);
  const raw = readFileSync(path, "utf8");
  return JSON.parse(raw);
}

/** Shortest edit-ish hint so a renamed test can be fixed without a search. */
function suggest(name, candidates) {
  const needle = name.toLowerCase();
  const scored = [];
  for (const candidate of candidates) {
    const haystack = candidate.toLowerCase();
    let score = 0;
    if (haystack === needle) score = 1000;
    else if (haystack.includes(needle) || needle.includes(haystack)) score = 500 + Math.min(haystack.length, needle.length);
    else {
      const parts = needle.split(/[_\-. ]+/u).filter(Boolean);
      for (const part of parts) if (part.length > 3 && haystack.includes(part)) score += 20;
    }
    if (score > 0) scored.push([score, candidate]);
  }
  scored.sort((left, right) => right[0] - left[0]);
  return scored.slice(0, 3).map(([, candidate]) => candidate);
}

function resolveReference(reference, names, label, errors, scope) {
  if (typeof reference !== "string" || reference.trim() === "") {
    errors.push(`${label}: 证据引用必须是非空字符串`);
    return null;
  }
  if (names.has(reference)) return reference;
  const matches = [...names.keys()].filter((candidate) => candidate.includes(reference));
  if (matches.length === 1) return matches[0];
  if (matches.length > 1) {
    errors.push(`${label}: 「${reference}」匹配到多个${scope}用例（${matches.slice(0, 4).join(", ")}），请写更精确的名字`);
    return null;
  }
  const hints = suggest(reference, names.keys());
  errors.push(`${label}: 「${reference}」不是真实存在的${scope}用例${hints.length ? `；相近的有 ${hints.join(", ")}` : ""}`);
  return null;
}

/**
 * Validates the matrix against a live index of the working tree.
 *
 * Returns `{ errors, warnings, resolved }`. `resolved` is the matrix with every
 * reference replaced by the concrete name it matched, so callers can report the
 * exact evidence rather than the pattern.
 */
export function validateMatrix(matrix, index) {
  const errors = [];
  const warnings = [];
  const resolved = [];

  if (!Array.isArray(matrix?.requirements)) {
    errors.push("requirements.json 缺少 requirements 数组");
    return { errors, warnings, resolved };
  }
  if (typeof matrix.contractVersion !== "string" || matrix.contractVersion === "") {
    errors.push("requirements.json 缺少 contractVersion");
  }

  const seen = new Set();
  for (const entry of matrix.requirements) {
    const label = entry?.id ?? "<缺少 id>";
    if (typeof entry?.id !== "string" || !REQUIREMENT_ID_PATTERN.test(entry.id)) {
      errors.push(`${label}: id 必须是 R01–R60 形式`);
      continue;
    }
    if (seen.has(entry.id)) errors.push(`${label}: id 重复`);
    seen.add(entry.id);

    if (typeof entry.requirement !== "string" || entry.requirement.trim() === "") {
      errors.push(`${label}: requirement 不能为空（要写需求本身，不能只写「组件已存在」）`);
    }
    if (!REQUIREMENT_VERSIONS.includes(entry.version)) {
      errors.push(`${label}: version 必须是 ${REQUIREMENT_VERSIONS.join(" / ")} 之一`);
    }
    if (!EVIDENCE_STATUSES.includes(entry.status)) {
      errors.push(`${label}: status 必须是 ${EVIDENCE_STATUSES.join(" / ")} 之一`);
    }
    if (!Array.isArray(entry.cards) || entry.cards.length === 0) {
      errors.push(`${label}: cards 不能为空（承接该需求的任务卡）`);
    } else {
      for (const card of entry.cards) {
        if (typeof card !== "string" || !/^[AB]\d-\d\d$/u.test(card)) {
          errors.push(`${label}: 任务卡「${card}」不是 A0-01/B4-02 这种形式`);
        }
      }
    }

    const automated = entry.automated ?? {};
    const rustRefs = Array.isArray(automated.rust) ? automated.rust : [];
    const frontendRefs = Array.isArray(automated.frontend) ? automated.frontend : [];
    const resolvedRust = [];
    for (const reference of rustRefs) {
      const hit = resolveReference(reference, index.rust, label, errors, " Rust ");
      if (hit) resolvedRust.push(hit);
    }
    const resolvedFrontend = [];
    for (const reference of frontendRefs) {
      if (typeof reference?.file !== "string") {
        errors.push(`${label}: frontend 证据必须带 file`);
        continue;
      }
      const cases = index.frontend.get(reference.file);
      if (!cases) {
        errors.push(`${label}: 前端测试文件「${reference.file}」不存在`);
        continue;
      }
      const patterns = Array.isArray(reference.contains) ? reference.contains : [];
      const minimum = Number.isInteger(reference.minCases) ? reference.minCases : 0;
      if (patterns.length === 0 && minimum === 0) {
        errors.push(`${label}: frontend 证据「${reference.file}」要写 contains 或 minCases，否则只证明文件存在`);
        continue;
      }
      if (minimum > 0 && cases.length < minimum) {
        errors.push(`${label}: 「${reference.file}」只有 ${cases.length} 个用例，少于声明的 ${minimum}`);
      }
      let missing = false;
      for (const pattern of patterns) {
        const matched = cases.filter((title) => title.includes(pattern));
        if (matched.length === 0) {
          missing = true;
          errors.push(`${label}: 「${reference.file}」里没有包含「${pattern}」的用例`);
        }
      }
      if (!missing) resolvedFrontend.push({ file: reference.file, cases: cases.length, contains: patterns });
    }

    const manualRefs = Array.isArray(entry.manual) ? entry.manual : [];
    const resolvedManual = [];
    for (const reference of manualRefs) {
      if (typeof reference?.id !== "string") {
        errors.push(`${label}: manual 证据必须带 id`);
        continue;
      }
      if (!index.playbook.has(reference.id)) {
        errors.push(`${label}: QA 手册里没有条目「${reference.id}」`);
        continue;
      }
      if (!["executed", "pending"].includes(reference.status)) {
        errors.push(`${label}: manual「${reference.id}」的 status 必须是 executed / pending`);
        continue;
      }
      resolvedManual.push({ id: reference.id, status: reference.status });
    }

    const automatedCount = resolvedRust.length + resolvedFrontend.length;
    if (entry.status === "covered" && automatedCount === 0) {
      errors.push(`${label}: status=covered 但没有任何自动化证据。真机手工条目应记为 gap + reason，并在 manual 里标 pending`);
    }
    if (entry.status !== "covered" && (typeof entry.reason !== "string" || entry.reason.trim() === "")) {
      errors.push(`${label}: status=${entry.status} 必须写 reason（说明缺什么、由哪张卡补齐）`);
    }
    if (entry.version === "2.5" && resolvedManual.some((item) => item.status === "executed")) {
      warnings.push(`${label}: 2.5 条目标了已执行的真机证据，确认不是在 2.0 阶段提前宣称`);
    }

    resolved.push({
      id: entry.id,
      requirement: entry.requirement,
      version: entry.version,
      status: entry.status,
      cards: entry.cards,
      reason: entry.reason ?? null,
      rust: resolvedRust,
      frontend: resolvedFrontend,
      manual: resolvedManual,
    });
  }

  for (let number = 1; number <= 61; number += 1) {
    const id = `R${String(number).padStart(2, "0")}`;
    if (!seen.has(id)) errors.push(`${id}: 覆盖表里缺这一条（R01–R60 必须逐条映射）`);
  }

  return { errors, warnings, resolved };
}

/** Counts by version and status so a report can quote exact numbers. */
export function summarize(matrix) {
  const byVersion = new Map();
  for (const entry of matrix.requirements ?? []) {
    const bucket = byVersion.get(entry.version) ?? { covered: 0, gap: 0, deferred: 0, total: 0 };
    bucket.total += 1;
    if (entry.status === "covered") bucket.covered += 1;
    else if (entry.status === "gap") bucket.gap += 1;
    else if (entry.status === "deferred") bucket.deferred += 1;
    byVersion.set(entry.version, bucket);
  }
  const rust = new Set();
  let frontend = 0;
  for (const entry of matrix.requirements ?? []) {
    for (const name of entry.automated?.rust ?? []) rust.add(name);
    frontend += (entry.automated?.frontend ?? []).length;
  }
  return {
    byVersion: Object.fromEntries(byVersion),
    distinctRustTests: rust.size,
    frontendFiles: frontend,
  };
}

/** Requirements that are scope 2.0 but carry no automated evidence yet. */
export function blockingGaps(matrix) {
  return (matrix.requirements ?? [])
    .filter((entry) => entry.version === "2.0" && entry.status !== "covered")
    .map((entry) => `${entry.id}（${entry.status}）：${entry.reason ?? "缺少理由"}`);
}

// R59 asks for 「未被跳过的必要测试」清点: a skipped-but-required test must not
// quietly count as passing. Reading the matrix alone cannot see this, because an
// `#[ignore]` test still exists by name and therefore still satisfies a reference.
// So the audit walks the Rust sources and demands that every skip states why.
//
// A blank `#[ignore]` is rejected outright: an unexplained skip is exactly the
// shape of a necessary test being switched off to get a green run. A reason is
// accepted at two levels — the attribute itself, or the immediately preceding
// comment block — because the real repo documents several of these next to the
// item instead of inline.
const IGNORE_ATTRIBUTE = /^#\[ignore\s*(?:=\s*"([^"]*)")?\s*\]/;
/** Reasons that describe an environment or safety precondition rather than a shortcut. */
const IGNORE_REASON_KEYWORDS = ["需", "manual", "手动", "破坏性", "真机", "real", "requires", "网络", "凭据"];

/**
 * An affirmative "this is done" claim in a specification file.
 *
 * Requirement ids are covered by the `^\| R59` and `\[x\]` forms; the bare
 * phrases are kept narrow so that ordinary prose like "识别已完成的任务"
 * (a product behaviour, not a delivery claim) does not trip the gate.
 */
const PROGRESS_CLAIM_PATTERN =
  /已验收：|本卡已验收|验收通过（|已交付：|\[\s*[xX]\s*\]|^\|\s*`?R\d\d`?\s*\|[^|]*\|[^|]*\|\s*(?:已完成|covered|通过)\s*\|/u;
/**
 * A line that denies being a progress source is not a claim.
 *
 * The spec files legitimately say "不被当作已交付功能", "不等于任务验收通过",
 * "不是已实现功能" — those are the rule being restated, not a violation.
 */
const PROGRESS_DISCLAIMER_PATTERN =
  /不是|不等于|不被|不得|禁|不算|不写|不累计|不记录|不承载|不维护|不表示|未|待|非/u;

function walkRustSources(dir, files = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const absolute = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "target" || entry.name === "node_modules") continue;
      walkRustSources(absolute, files);
    } else if (entry.name.endsWith(".rs")) {
      files.push(absolute);
    }
  }
  return files;
}

/**
 * Documents that are allowed to talk about delivery state.
 *
 * R60 says "待实现规格与已交付事实分开，进度只在 PROJECT_PLAN 维护". That
 * sentence was only a convention until this gate existed: nothing stopped a
 * plan file from growing a "已验收" table that then drifted away from the real
 * state, and a reader would have had two contradicting progress sources.
 *
 * PROJECT_PLAN.md owns progress. PRD/ARCHITECTURE/DEVELOPMENT/PLATFORM and the
 * rest own their own subject and may legitimately say a capability "已完成";
 * plans/ and execution cards are specifications and must keep saying "要做什么",
 * not "已经做到哪". AGENTS.md and docs/README.md are navigation and repeat the
 * rule itself, so they are excluded as owners of the rule.
 */
export const PROGRESS_OWNER_PATHS = ["docs/PROJECT_PLAN.md"];

/** Directories whose Markdown is specification, not delivery state. */
const SPECIFICATION_DIRECTORIES = ["plans"];

/**
 * PROJECT_PLAN.md must say so itself; otherwise "唯一进度事实源" is a claim
 * only AGENTS.md makes, and the owning document could drift away from it.
 */
const PROGRESS_OWNER_DECLARATIONS = [
  /唯一(?:的)?进度(?:事实源|来源|来源地)/,
  /进度(?:只|仅)(?:在|由)(?:本文档|本文件)/,
  /进度(?:只|仅)(?:在|由)\s*\[?PROJECT_PLAN/i,
];

/**
 * Finds Markdown under plans/ that claims a card or requirement is already done.
 *
 * The negative form is deliberately accepted (`这是开发次序与验收边界，不是已
 * 完成进度`) because the execution index has to state that it is *not* a
 * progress source. Only an affirmative claim is a second progress source.
 */
export function collectSpecificationProgressClaims(root = REPOSITORY_ROOT) {
  const claims = [];
  const missing = [];
  for (const name of PROGRESS_OWNER_PATHS) {
    const absolute = join(root, name);
    if (!existsSync(absolute)) {
      missing.push(`${name} 不存在，无法证明它是唯一进度事实源`);
      continue;
    }
    const text = readFileSync(absolute, "utf8");
    if (!PROGRESS_OWNER_DECLARATIONS.some((pattern) => pattern.test(text))) {
      missing.push(`${name} 没有声明自己是唯一进度事实源（R60 要求的权威边界）`);
    }
  }
  for (const directory of SPECIFICATION_DIRECTORIES) {
    const absolute = join(root, directory);
    if (!existsSync(absolute)) continue;
    for (const file of walk(absolute, (name) => name.endsWith(".md"))) {
      const relativePath = relative(root, file).replaceAll("\\", "/");
      const lines = readFileSync(file, "utf8").split("\n");
      lines.forEach((raw, index) => {
        const line = raw.trim();
        if (!PROGRESS_CLAIM_PATTERN.test(line)) return;
        if (PROGRESS_DISCLAIMER_PATTERN.test(line)) return;
        claims.push(`${relativePath}:${index + 1} ${line.slice(0, 120)}`);
      });
    }
  }
  return { claims, missing };
}

/**
 * Names that legitimately appear in docs but are not defined in this repository.
 *
 * `on_web_resource_request` is a Tauri API the plugin contract has to name;
 * renaming it here would make the document wrong about the framework instead of
 * wrong about our code. The list is deliberately tiny: every other identifier in
 * backticks is expected to resolve to a definition or a call site in the tree,
 * because a contract that tells an implementer to look at a function that never
 * existed is indistinguishable from one describing code that was deleted.
 */
const EXTERNAL_IDENTIFIERS = new Set(["on_web_resource_request"]);

/** Backticked identifiers worth resolving: snake_case names, at least 8 chars. */
const SYMBOL_PATTERN = /`([a-z][a-z0-9]*(?:_[a-z0-9]+){1,})`/g;

/**
 * Checks that identifier names cited in the documentation actually exist in code.
 *
 * The motivating failure is a real one from A6-04: the traceability list cited
 * nine test names that had been renamed, and TROUBLESHOOTING told a reader to
 * inspect `is_config_error`, a function that never existed. Nothing caught
 * either, because prose is not compiled.
 */
export function collectOrphanSymbols(root = REPOSITORY_ROOT) {
  const documentFiles = [];
  for (const directory of ["docs", "plans"]) {
    const absolute = join(root, directory);
    if (!existsSync(absolute)) continue;
    documentFiles.push(...walk(absolute, (name) => name.endsWith(".md")));
  }
  const cited = new Map();
  for (const file of documentFiles) {
    readFileSync(file, "utf8")
      .split("\n")
      .forEach((line, index) => {
        for (const match of line.matchAll(SYMBOL_PATTERN)) {
          const name = match[1];
          if (EXTERNAL_IDENTIFIERS.has(name)) continue;
          if (!cited.has(name)) cited.set(name, `${relative(root, file).replaceAll("\\", "/")}:${index + 1}`);
        }
      });
  }
  const sources = [
    ...walk(join(root, "src"), () => true),
    ...walk(join(root, "src-tauri", "src"), () => true),
    ...(existsSync(join(root, "src-tauri", "tests")) ? walk(join(root, "src-tauri", "tests"), () => true) : []),
    ...walk(join(root, "packages"), () => true),
    ...walk(join(root, "scripts"), () => true),
  ].filter((file) => /\.(rs|ts|tsx|mjs|cjs|js)$/.test(file));
  const haystack = sources.map((file) => readFileSync(file, "utf8")).join("\n");
  const orphans = [];
  for (const [name, location] of cited) {
    if (!haystack.includes(name)) orphans.push(`${location} 引用的 \`${name}\` 在代码里不存在`);
  }
  return { cited: cited.size, orphans };
}

export function collectIgnoredTests(root = REPOSITORY_ROOT) {
  const files = walkRustSources(join(root, "src-tauri", "src")).concat(
    existsSync(join(root, "src-tauri", "tests")) ? walkRustSources(join(root, "src-tauri", "tests")) : [],
  );
  const ignored = [];
  const problems = [];
  for (const file of files) {
    const lines = readFileSync(file, "utf8").split("\n");
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index].trim();
      const match = IGNORE_ATTRIBUTE.exec(line);
      if (!match) continue;
      const location = `${relative(root, file).replaceAll("\\", "/")}:${index + 1}`;
      const inline = (match[1] ?? "").trim();
      // Walk back over attributes/doc comments to the comment block that explains the skip.
      const context = [];
      for (let cursor = index - 1; cursor >= 0; cursor -= 1) {
        const previous = lines[cursor].trim();
        if (previous.startsWith("//") || previous.startsWith("#[")) {
          context.push(previous);
          continue;
        }
        break;
      }
      const reason = `${inline}\n${context.join("\n")}`.trim();
      const nearby = lines.slice(index + 1, index + 4).join(" ");
      const functionName = /fn\s+([A-Za-z0-9_]+)/.exec(nearby)?.[1] ?? "(未找到紧邻的 fn)";
      if (inline.length === 0 && context.length === 0) {
        problems.push(`${location} ${functionName}：#[ignore] 没有任何理由，无法证明它不是被关掉的必要测试`);
        continue;
      }
      const hasKeyword = IGNORE_REASON_KEYWORDS.some((keyword) => reason.toLowerCase().includes(keyword.toLowerCase()));
      if (!hasKeyword) {
        problems.push(
          `${location} ${functionName}：#[ignore] 的理由「${reason.replaceAll("\n", " ").slice(0, 80)}」未说明环境或安全前置，无法判断是否为被关掉的必要测试`,
        );
        continue;
      }
      ignored.push({ location, function: functionName, reason: reason.replaceAll("\n", " ").slice(0, 160) });
    }
  }
  return { ignored, problems };
}

export function checkRepository(root = REPOSITORY_ROOT, { strict = false } = {}) {
  const matrix = loadMatrix(root);
  const index = {
    rust: collectRustTestNames(root),
    frontend: collectFrontendCases(root),
    playbook: collectPlaybookIds(root),
  };
  const result = validateMatrix(matrix, index);
  const gaps = blockingGaps(matrix);
  const skips = collectIgnoredTests(root);
  const progress = collectSpecificationProgressClaims(root);
  result.errors.push(...skips.problems);
  result.errors.push(...progress.missing);
  result.errors.push(
    ...progress.claims.map(
      (claim) => `规格文件在宣称交付状态（进度只允许写在 ${PROGRESS_OWNER_PATHS.join(" / ")}）— ${claim}`,
    ),
  );
  if (strict && gaps.length > 0) {
    result.errors.push(...gaps.map((gap) => `严格模式：2.0 需求仍未覆盖 — ${gap}`));
  }
  return { matrix, index, result, gaps, skips, progress };
}

function formatSummary(matrix) {
  const summary = summarize(matrix);
  const lines = [`需求覆盖矩阵 ${MATRIX_RELATIVE_PATH}（契约 ${matrix.contractVersion}）`];
  for (const version of REQUIREMENT_VERSIONS) {
    const bucket = summary.byVersion[version];
    if (!bucket) continue;
    lines.push(
      `  ${version}: 共 ${bucket.total}，covered ${bucket.covered}，gap ${bucket.gap}，deferred ${bucket.deferred}`,
    );
  }
  lines.push(`  引用的不同 Rust 用例：${summary.distinctRustTests}`);
  lines.push(`  引用的前端测试文件：${summary.frontendFiles}`);
  return lines.join("\n");
}

function main(argv) {
  const [command = "check", ...rest] = argv;
  const strict = rest.includes("--strict");
  if (command === "summary") {
    console.log(formatSummary(loadMatrix()));
    return 0;
  }
  if (command !== "check") {
    console.error(`未知命令：${command}（可用 check / summary）`);
    return 2;
  }
  const { matrix, result, gaps, skips, progress } = checkRepository(REPOSITORY_ROOT, { strict });
  if (result.warnings.length > 0) {
    for (const warning of result.warnings) console.warn(`警告：${warning}`);
  }
  if (result.errors.length > 0) {
    for (const error of result.errors) console.error(`错误：${error}`);
    console.error(`\n共 ${result.errors.length} 处不一致。`);
    return 1;
  }
  console.log(formatSummary(matrix));
  if (skips.ignored.length > 0) {
    console.log(`\n跳过的用例 ${skips.ignored.length} 处（均写明环境或安全前置，不计为通过）：`);
    for (const skip of skips.ignored) console.log(`  - ${skip.location} ${skip.function}：${skip.reason}`);
  }
  if (gaps.length > 0) {
    console.log(`\n2.0 未覆盖条目 ${gaps.length} 条（非严格模式放行，交付门禁必须清零）：`);
    for (const gap of gaps) console.log(`  - ${gap}`);
  }
  console.log("\n引用校验通过：每条证据都指向工作树里真实存在的用例。");
  console.log(
    `进度来源校验通过：plans/ 下 ${progress.claims.length} 处交付状态宣称，进度只由 ${PROGRESS_OWNER_PATHS.join(" / ")} 拥有。`,
  );
  return 0;
}

// The test file imports the pure functions below without running the CLI, so
// the entry point is guarded by comparing resolved paths.
function invokedDirectly() {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    if (!statSync(entry).isFile()) return false;
    return realpathSync(entry) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (invokedDirectly()) {
  process.exitCode = main(process.argv.slice(2));
}
