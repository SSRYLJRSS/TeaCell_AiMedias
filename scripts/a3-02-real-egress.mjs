// A3-02 真实外发端到端验收驱动（脱敏）。
//
// 该脚本本身不直连模型、不持有密钥：它通过环境变量把真实 provider 参数
// 传给 Rust 侧被 `#[ignore]` 的四个真实外发测试，并只把
// “是否通过、耗时、脱敏后的端点主机/模型名”写入 artifacts 证据文件。
//
// 密钥只出现在本进程的子进程环境变量里，绝不写入证据文件、绝不打印。
//
// 用法（负责人显式授权时）：
//   TEACELL_A302_E2E_BASE_URL=http://127.0.0.1:3425 \
//   TEACELL_A302_E2E_KEY=*** \
//   TEACELL_A302_E2E_MODEL=opencode-zen-free/mimo-v2.6-flash-free \
//   node scripts/a3-02-real-egress.mjs
//
// 未提供上述三个环境变量时，脚本以退出码 2 明确表示“未获真实外发授权”，
// 不尝试连接，也绝不伪造通过结果。

import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..");

const baseUrl = process.env.TEACELL_A302_E2E_BASE_URL;
const key = process.env.TEACELL_A302_E2E_KEY;
const model = process.env.TEACELL_A302_E2E_MODEL;

if (!baseUrl || !key || !model) {
  console.error(
    "A3-02 真实外发未获授权：缺少 TEACELL_A302_E2E_BASE_URL / TEACELL_A302_E2E_KEY / TEACELL_A302_E2E_MODEL。不连接、不伪造结果。",
  );
  process.exit(2);
}

// 脱敏：只保留 scheme + host + port，不保留任何路径/查询；密钥从不进入证据。
function redactEndpoint(raw) {
  try {
    const url = new URL(raw);
    return `${url.protocol}//${url.host}`;
  } catch {
    return "<invalid-endpoint>";
  }
}

// 四个真实外发用例，逐个串行执行，避免并发争用同一 provider 额度。
const cases = [
  {
    name: "real_provider_egress_streams_and_keeps_endpoint_and_key_off_stdio",
    assertions: [
      "real HttpGatewayTransport returned 200 and streamed non-empty SSE data frames",
      "neither endpoint nor key appeared in any frame returned to the Host (privacy boundary)",
      "credentials backend was read exactly once per attempt (no per-chunk key reread)",
      "DisabledGatewayTransport rejects the same connection with UPSTREAM_UNAVAILABLE (default-safe)",
    ],
  },
  {
    name: "real_provider_error_is_structured_and_keeps_secrets_off_stdio",
    assertions: [
      "upstream failure surfaces as a structured gateway.response.error frame, not a panic or silent stall",
      "the error frame carries no endpoint and no key (secrets never cross the Host stdio boundary)",
      "no partial/duplicated stream frames were emitted after the error",
    ],
  },
  {
    name: "real_provider_stream_can_be_cancelled_mid_flight",
    assertions: [
      "an in-flight stream can be cancelled and the cancellation is honoured promptly",
      "cancellation releases the concurrency/credit permit so the connection is reusable afterwards",
      "the cancel path leaks neither endpoint nor key",
    ],
  },
  {
    name: "real_provider_requests_are_gated_by_our_own_rate_limit",
    assertions: [
      "the second request is not silently forwarded upstream while the first holds concurrency/credits",
      "the credit gate is wait-based (acquire_until_owned), so exhaustion blocks instead of leaking through",
      "any resulting error is structured and free of endpoint/key",
    ],
  },
];

const startedAt = new Date().toISOString();
const results = [];

for (const testCase of cases) {
  const result = spawnSync(
    "cargo",
    [
      "test",
      "--lib",
      "--all-features",
      "--",
      "--ignored",
      "--test-threads=1",
      "--nocapture",
      testCase.name,
    ],
    {
      cwd: resolve(repoRoot, "src-tauri"),
      env: {
        ...process.env,
        TEACELL_A302_E2E_BASE_URL: baseUrl,
        TEACELL_A302_E2E_KEY: key,
        TEACELL_A302_E2E_MODEL: model,
      },
      encoding: "utf8",
    },
  );

  const stdout = result.stdout ?? "";
  const sanitize = (text) => (key ? text.split(key).join("***REDACTED***") : text);
  const passed =
    result.status === 0 && /test result: ok\.\s*1 passed/.test(stdout);

  results.push({
    testName: testCase.name,
    passed,
    exitCode: result.status,
    assertions: testCase.assertions,
    transcriptTail: sanitize(
      `${stdout}\n${result.stderr ?? ""}`
        .split("\n")
        .filter((line) =>
          /test result:|test services::|panicked|assert|A3-02/.test(line),
        )
        .join("\n"),
    ),
  });

  console.log(
    `A3-02 ${passed ? "PASS" : "FAIL"} ${testCase.name} (exit=${result.status})`,
  );
}

const allPassed = results.every((entry) => entry.passed);

const evidence = {
  card: "A3-02",
  kind: "real-provider-egress",
  status: allPassed ? "passed" : "failed",
  startedAt,
  finishedAt: new Date().toISOString(),
  endpoint: redactEndpoint(baseUrl),
  model,
  command:
    "cargo test --lib --all-features -- --ignored --test-threads=1 --nocapture <testName>",
  cases: results,
};

const outDir = resolve(repoRoot, "artifacts/agent-runtime/a3-02-real-egress");
mkdirSync(outDir, { recursive: true });
const outPath = resolve(outDir, "real-egress-result.json");
writeFileSync(outPath, JSON.stringify(evidence, null, 2) + "\n", "utf8");
console.log(`A3-02 真实外发证据写入：${outPath}（status=${evidence.status}）`);

process.exit(allPassed ? 0 : 1);
