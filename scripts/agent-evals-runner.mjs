#!/usr/bin/env node
/* global process, console */
// A4-08 offline runnable-eval runner entry.
//
// `scripts/agent-evals-coverage.mjs` (`npm run test:evals`) only proves the
// coverage table does not rot — it never executes a case. This script is the
// executable half: it drives the Rust host-based runner family
// (`services::agent_runtime::windows_integration_tests::agent_eval_*`), which
// runs the faithfully-drivable cases through the real Node Host + a
// deterministic scripted provider (no network, no real credentials) and prints
// a per-case PASS/FAIL/SKIP ledger. It never touches a real model.
//
// Each case runs in its OWN `cargo test` process. Repeated Host launches inside
// a single test process are flaky (the third launch can hang on the settlement
// channel), so one process per case keeps the runner deterministic. The pure
// ledger test (no Host) runs once at the end.
//
// It runs only where the pinned Node Host can run (Windows x64) and the HEIF
// native libraries are staged; anywhere else it prints an explicit SKIP and
// exits 0, so the gate stays stable across platforms instead of pretending the
// cases passed. `npm run test:evals` keeps the cross-platform mapping check.
//
// Usage:
//   node scripts/agent-evals-runner.mjs          # run (or SKIP) the offline runner

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPOSITORY_ROOT = dirname(HERE);
export const RUNTIME_NODE = join(
  REPOSITORY_ROOT,
  "artifacts",
  "agent-runtime",
  "node-v24.19.0-win-x64",
  "node.exe",
);
export const HEIF_DIR = join(
  REPOSITORY_ROOT,
  "src-tauri",
  "native",
  "heif",
  "x86_64-pc-windows-msvc",
);

const MODULE = "services::agent_runtime::windows_integration_tests";

// The faithfully-drivable cases. Each entry is one `#[ignore]`d Rust test that
// is driven in its own process (see the header note on flakiness).
export const EVAL_CASES = [
  { caseId: "E09", test: `${MODULE}::agent_eval_case_e09_search_only` },
  { caseId: "E10", test: `${MODULE}::agent_eval_case_e10_search_only` },
  { caseId: "E13", test: `${MODULE}::agent_eval_case_e13_trash_requires_gui_approval` },
  { caseId: "E12", test: `${MODULE}::agent_eval_case_e12_prepare_only` },
  { caseId: "E23", test: `${MODULE}::agent_eval_case_e23_repeated_search_creates_no_plan` },
  { caseId: "E11", test: `${MODULE}::agent_eval_case_e11_ambiguous_prepare_stays_pending` },
  { caseId: "E20", test: `${MODULE}::agent_eval_case_e20_replay_submit_creates_single_operation` },
  { caseId: "E27", test: `${MODULE}::agent_eval_case_e27_cancel_after_success_is_rejected` },
  { caseId: "E16", test: `${MODULE}::agent_eval_case_e16_selective_submit_trashes_only_the_chosen_asset` },
  { caseId: "E15", test: `${MODULE}::agent_eval_case_e15_retract_then_reprepare_stays_pending` },
  { caseId: "E17", test: `${MODULE}::agent_eval_case_e17_hesitate_after_prepare_keeps_plan_pending` },
  { caseId: "E19", test: `${MODULE}::agent_eval_case_e19_forged_approval_is_rejected` },
  { caseId: "E32", test: `${MODULE}::agent_eval_case_e32_prepare_requires_authorized_destination` },
  { caseId: "E33", test: `${MODULE}::agent_eval_case_e33_move_prepare_outside_library_is_rejected` },
  { caseId: "E41", test: `${MODULE}::agent_eval_case_e41_move_prepare_within_library_creates_pending_plan` },
  { caseId: "E42", test: `${MODULE}::agent_eval_case_e42_restore_prepare_creates_pending_plan` },
];

// Pure ledger test (no Host), asserts all 42 cases are classified.
export const LEDGER_TEST = `${MODULE}::agent_eval_runner_ledger_enumerates_every_case_without_launching_hosts`;

/**
 * Decide whether the offline runner can execute here. Pure and injectable so the
 * "degrade to SKIP instead of failing" behaviour is testable without a Host.
 * @returns {{ run: boolean, reason: string }}
 */
export function planRunner({
  platform = process.platform,
  runtimeNodeExists = existsSync(RUNTIME_NODE),
  heifDirExists = existsSync(HEIF_DIR),
} = {}) {
  if (platform !== "win32") {
    return {
      run: false,
      reason: `platform ${platform} is not Windows x64; the pinned Node Host only runs on Windows x64`,
    };
  }
  if (!runtimeNodeExists) {
    return {
      run: false,
      reason: `pinned runtime missing: ${RUNTIME_NODE} (run \`npm run prepare-agent-host\`)`,
    };
  }
  if (!heifDirExists) {
    return {
      run: false,
      reason: `HEIF libraries missing: ${HEIF_DIR} (run \`npm run prepare-heif-libraries\`)`,
    };
  }
  return { run: true, reason: "Windows x64 with pinned runtime and HEIF libraries present" };
}

function runOne(testName, { ignored }) {
  const libtest = ["--exact"];
  if (ignored) libtest.push("--ignored");
  libtest.push(testName, "--test-threads=1", "--nocapture");
  return spawnSync(
    "cargo",
    [
      "test",
      "--manifest-path",
      join(REPOSITORY_ROOT, "src-tauri", "Cargo.toml"),
      "--lib",
      "--all-features",
      "--",
      ...libtest,
    ],
    {
      cwd: REPOSITORY_ROOT,
      stdio: "inherit",
      env: { ...process.env, HEIF_BINARIES_DIR: HEIF_DIR },
    },
  );
}

function main() {
  const plan = planRunner();
  if (!plan.run) {
    console.log(`[agent-evals-runner] SKIP: ${plan.reason}`);
    return 0;
  }
  console.log(`[agent-evals-runner] executing ${EVAL_CASES.length} offline cases on Windows x64.`);

  let failures = 0;
  for (const { caseId, test } of EVAL_CASES) {
    console.log(`[agent-evals-runner] ${caseId}\tRUN\t${test}`);
    const result = runOne(test, { ignored: true });
    if (result.error) {
      console.error(`[agent-evals-runner] ${caseId}\tFAIL\tcargo launch error: ${result.error.message}`);
      failures += 1;
    } else if (result.status !== 0) {
      console.error(`[agent-evals-runner] ${caseId}\tFAIL\tcargo exited ${result.status}`);
      failures += 1;
    } else {
      console.log(`[agent-evals-runner] ${caseId}\tPASS`);
    }
  }

  console.log(`[agent-evals-runner] ledger\tRUN\t${LEDGER_TEST}`);
  const ledger = runOne(LEDGER_TEST, { ignored: false });
  if (ledger.error || ledger.status !== 0) {
    console.error(`[agent-evals-runner] ledger\tFAIL`);
    failures += 1;
  } else {
    console.log(`[agent-evals-runner] ledger\tPASS`);
  }

  if (failures > 0) {
    console.error(`[agent-evals-runner] ${failures} failure(s); the offline eval runner did not pass.`);
    return 1;
  }
  console.log(`[agent-evals-runner] all ${EVAL_CASES.length} cases + ledger passed (offline).`);
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exit(main());
}
