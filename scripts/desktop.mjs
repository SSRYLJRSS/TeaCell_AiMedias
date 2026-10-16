#!/usr/bin/env node
/* global process, console */
/** Shared desktop entry point: target-specific native dependencies and bundled media tools. */
import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyHeifCache } from "./heif-contract.mjs";
import { assertProductIdentity } from "./product-identity.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const tauri = join(root, "src-tauri");
const manifestPath = join(tauri, "binaries", "manifest.json");
const heifManifestPath = join(tauri, "native", "heif-manifest.json");
const targets = {
  "x86_64-pc-windows-msvc": { os: "windows", ext: ".exe", heif: ["native", "heif", "x86_64-pc-windows-msvc"] },
  "aarch64-apple-darwin": { os: "macos", ext: "", heif: ["native", "heif", "aarch64-apple-darwin"] },
  "x86_64-unknown-linux-gnu": { os: "linux", ext: "", heif: ["native", "heif", "x86_64-unknown-linux-gnu"] },
};

const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const die = (message) => { console.error(`[desktop] ${message}`); process.exit(1); };

function defaultTarget() {
  if (process.platform === "win32" && process.arch === "x64") return "x86_64-pc-windows-msvc";
  if (process.platform === "darwin" && process.arch === "arm64") return "aarch64-apple-darwin";
  if (process.platform === "linux" && process.arch === "x64") return "x86_64-unknown-linux-gnu";
  return null;
}

function parseArgs(args) {
  const [mode, ...rest] = args;
  let target;
  let strict = false;
  const passthrough = [];
  for (let i = 0; i < rest.length; i += 1) {
    if (rest[i] === "--target" && rest[i + 1]) target = rest[++i];
    else if (rest[i] === "--strict-release") strict = true;
    else passthrough.push(rest[i]);
  }
  return { mode, target: target ?? defaultTarget(), strict, passthrough };
}

function validateVersions() {
  const packageJson = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const tauriConfig = JSON.parse(readFileSync(join(tauri, "tauri.conf.json"), "utf8"));
  const cargoToml = readFileSync(join(tauri, "Cargo.toml"), "utf8");
  try {
    assertProductIdentity(packageJson, tauriConfig, cargoToml);
  } catch (error) {
    die(`产品名称校验失败：${error.message}`);
  }
  const cargoVersion = cargoToml.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const versions = [packageJson.version, tauriConfig.version, cargoVersion];
  if (versions.some((version) => !version) || new Set(versions).size !== 1) {
    die(`package.json、tauri.conf.json、Cargo.toml 版本必须一致（当前：${versions.join(", ")}）`);
  }
  return versions[0];
}

function readManifest() {
  try {
    return JSON.parse(readFileSync(manifestPath, "utf8"));
  } catch (error) {
    die(`媒体工具 manifest 不存在或无效：${error.message}`);
  }
}

function checkSidecars(target, meta, strict, mediaManifest) {
  const targetManifest = mediaManifest.targets?.[target];
  const results = [];
  for (const name of ["ffmpeg", "ffprobe"]) {
    const file = join(tauri, "binaries", `${name}-${target}${meta.ext}`);
    const present = existsSync(file) && statSync(file).isFile();
    const problems = [];
    if (strict) {
      const expected = targetManifest?.[`${name}Sha256`];
      if (!present) problems.push("文件缺失");
      if (!/^[a-f0-9]{64}$/i.test(expected ?? "")) problems.push("manifest SHA256 缺失");
      else if (present && sha256(file) !== expected) problems.push("SHA256 不匹配");
      if (present && meta.os !== "windows" && (statSync(file).mode & 0o111) === 0) problems.push("缺少可执行位");
    }
    results.push({ name, file, present, problems });
  }

  const licenseName = targetManifest?.licenseFileName ?? "LICENSE.txt";
  const safeLicenseName = typeof licenseName === "string" && licenseName.length > 0 && !/[\\/]/.test(licenseName) && licenseName !== "." && licenseName !== "..";
  const license = safeLicenseName ? join(tauri, "binaries", "licenses", target, licenseName) : "";
  const licensePresent = Boolean(license) && existsSync(license) && statSync(license).isFile();
  const licenseProblems = [];
  if (strict) {
    if (!safeLicenseName) licenseProblems.push("许可证文件名无效");
    if (!/^[a-f0-9]{64}$/i.test(targetManifest?.licenseSha256 ?? "")) licenseProblems.push("manifest 许可证 SHA256 缺失");
    else if (!licensePresent) licenseProblems.push("许可证文件缺失");
    else if (sha256(license) !== targetManifest.licenseSha256) licenseProblems.push("许可证 SHA256 不匹配");
  }
  results.push({ name: "license", file: license, present: licensePresent, problems: licenseProblems });

  const ffmpeg = results.find((item) => item.name === "ffmpeg");
  if (strict && ffmpeg?.present && ffmpeg.problems.length === 0 && Array.isArray(targetManifest?.requiredEncoders)) {
    const probe = spawnSync(ffmpeg.file, ["-hide_banner", "-encoders"], { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
    const output = `${probe.stdout ?? ""}\n${probe.stderr ?? ""}`;
    const missing = targetManifest.requiredEncoders.filter((encoder) => !output.includes(encoder));
    if (probe.status !== 0 || missing.length) ffmpeg.problems.push(`编码器缺失：${missing.join(", ")}`);
  }
  return results;
}

function configureTargetEnv(target) {
  const env = { ...process.env };
  env.HEIF_BINARIES_DIR = join(tauri, "native", "heif", target);
  return env;
}

function readHeifManifest() {
  try {
    return JSON.parse(readFileSync(heifManifestPath, "utf8"));
  } catch (error) {
    die(`HEIF 来源 manifest 不存在或无效：${error.message}`);
  }
}

function prepareHeifLibraries(target) {
  const script = join(root, "scripts", "prepare-heif-libraries.mjs");
  const result = spawnSync(process.execPath, [script, "--target", target], { cwd: root, stdio: "inherit", env: process.env });
  if (result.error || result.status !== 0) {
    die(`HEIF 原生依赖准备失败；目标 ${target}，命令：npm run prepare-heif-libraries -- --target ${target}`);
  }
}

function prepareAgentHost(target) {
  if (target !== "x86_64-pc-windows-msvc") return null;
  const script = join(root, "scripts", "prepare-agent-host.mjs");
  const result = spawnSync(process.execPath, [script, "--target", target], {
    cwd: root,
    encoding: "utf8",
    env: process.env,
  });
  if (result.error || result.status !== 0) {
    die(`Agent Host资源准备失败；请核对固定Node/Pi版本、Node归档、许可证和依赖：${(result.stderr ?? "").trim()}`);
  }
  let prepared;
  try { prepared = JSON.parse((result.stdout ?? "").trim()); }
  catch { die("Agent Host资源准备结果无效"); }
  if (!prepared.overlay || !existsSync(prepared.overlay)) die("Agent Host Tauri资源overlay未生成");
  console.log(`[desktop] agent-host resources: ${prepared.agentFileCount} files, ${prepared.agentResourceBytes} bytes`);
  console.log(`[desktop] Visual C++ runtime: ${prepared.vcRuntimeVersion}, ${prepared.vcRuntimeFileCount} files, ${prepared.vcRuntimeBytes} bytes`);
  console.log(`[desktop] agent-host overlay: ${prepared.overlay}`);
  return prepared.overlay;
}

function runTauri(mode, target, passthrough, env, agentHostOverlay) {
  const cli = join(root, "node_modules", "@tauri-apps", "cli", "tauri.js");
  if (!existsSync(cli)) die("Tauri CLI 缺失，请先运行 npm ci");
  const args = [mode, "--target", target];
  if (mode === "build") args.push("--config", agentHostOverlay ?? "src-tauri/tauri.sidecar.conf.json");
  args.push(...passthrough);
  const buildLogPath = mode === "build" ? process.env.CANDIDATE_BUILD_LOG : null;
  const captureOutput = Boolean(buildLogPath);
  const buildRecordPath = join(root, "artifacts", "agent-host-build", "last-successful-desktop-build.json");
  if (mode === "build" && agentHostOverlay) rmSync(buildRecordPath, { force: true });
  const result = spawnSync(process.execPath, [cli, ...args], {
    cwd: root,
    stdio: captureOutput ? ["inherit", "pipe", "pipe"] : "inherit",
    ...(captureOutput ? { maxBuffer: 256 * 1024 * 1024 } : {}),
    env,
  });
  if (captureOutput) {
    const stdout = result.stdout ?? Buffer.alloc(0);
    const stderr = result.stderr ?? Buffer.alloc(0);
    const header = Buffer.from([
      `target: ${target}`,
      `command: ${process.execPath} ${[cli, ...args].join(" ")}`,
      "",
      "",
    ].join("\n"), "utf8");
    const log = Buffer.concat([header, stdout, Buffer.from("\n--- stderr ---\n", "utf8"), stderr]);
    mkdirSync(dirname(buildLogPath), { recursive: true });
    writeFileSync(buildLogPath, log);
    if (stdout) process.stdout.write(stdout);
    if (stderr) process.stderr.write(stderr);
  }
  if (result.error || result.status !== 0) die(`tauri ${mode} 失败（${result.error?.message ?? result.status ?? "signal"}）`);
  if (mode === "build" && agentHostOverlay) {
    mkdirSync(dirname(buildRecordPath), { recursive: true });
    writeFileSync(buildRecordPath, `${JSON.stringify({ schemaVersion: 1, target, overlay: agentHostOverlay }, null, 2)}\n`, { flag: "wx" });
  }
}

function main() {
  const { mode, target, strict, passthrough } = parseArgs(process.argv.slice(2));
  if (!["dev", "build", "check"].includes(mode)) die("用法：npm run desktop:<dev|build|check> -- --target <triple>");
  if (!target || !targets[target]) die(`不支持或无法推断 target：${target ?? "(none)"}`);

  const version = validateVersions();
  const meta = targets[target];
  const mediaManifest = readManifest();
  const heifManifest = readHeifManifest();
  if (!mediaManifest.targets?.[target]) die(`媒体工具 manifest 缺少目标：${target}`);
  const expectedRunner = mediaManifest.targets[target].runner;
  if (!expectedRunner || process.platform !== expectedRunner.os || process.arch !== expectedRunner.arch) {
    die(`目标必须由原生 runner 构建：${target} 需要 ${expectedRunner?.os ?? "?"}/${expectedRunner?.arch ?? "?"}，当前 ${process.platform}/${process.arch}`);
  }
  const effectiveStrict = strict || mode === "build";
  console.log(`[desktop] mode=${mode} target=${target} version=${version} os=${meta.os} host=${process.platform}/${process.arch}`);

  if (mode !== "check") prepareHeifLibraries(target);
  const heifResult = verifyHeifCache(join(tauri, "native", "heif", target), heifManifest, target);
  const heifCurrent = heifResult.ok && !heifResult.legacy;
  const heifStatus = heifCurrent
    ? "verified"
    : heifResult.ok
      ? "legacy marker; re-run prepare-heif-libraries to upgrade"
      : `not prepared (${heifResult.reason})`;
  console.log(`[desktop] heif: ${heifStatus}`);

  const sidecars = checkSidecars(target, meta, effectiveStrict, mediaManifest);
  for (const item of sidecars) {
    const problems = item.problems.length ? ` (${item.problems.join("；")})` : "";
    console.log(`[desktop] ${item.name}: ${item.present ? item.file : "missing"}${problems}`);
  }
  if (effectiveStrict && !heifCurrent) {
    die(`严格校验失败；先运行 npm run prepare-heif-libraries -- --target ${target}`);
  }
  if (effectiveStrict && sidecars.some((item) => item.problems.length)) {
    die(`严格校验失败；先运行 npm run prepare-media-tools -- --target ${target}`);
  }
  const agentHostOverlay = prepareAgentHost(target);
  if (mode === "check") return;
  if (!heifCurrent) die(`HEIF 原生依赖校验失败；先运行 npm run prepare-heif-libraries -- --target ${target}`);
  runTauri(mode, target, passthrough, configureTargetEnv(target), agentHostOverlay);
}

main();
