import { ensureSessionsDirectory, validateSessionFiles, verifySessionFile } from './paths.mjs';
import { Buffer } from 'node:buffer';
import { open, readdir, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';
import {
  createAgentSession,
  createExtensionRuntime,
  ModelRuntime,
  SessionManager,
  SettingsManager,
  VERSION,
} from '@earendil-works/pi-coding-agent';
import { createAssistantMessageEventStream } from '@earendil-works/pi-ai';
import { streamSimple as openAiStreamSimple } from '@earendil-works/pi-ai/api/openai-completions';
import { streamSimple as anthropicStreamSimple } from '@earendil-works/pi-ai/api/anthropic-messages';
import { setTimeout as delay } from 'node:timers/promises';
import { SDK_VERSION } from './protocol.mjs';
import { createGatewayFetch } from './gateway-fetch.mjs';
import { createPiTools, normalizeSessionToolNames } from './tools.mjs';

const GATEWAY_API_KEY = 'gateway-placeholder-only';
const GATEWAY_MODELS = Object.freeze({
  openai: Object.freeze({
    protocol: 'openai_chat',
    api: 'openai-completions',
    providerId: 'teacell-agent-openai',
    baseUrl: 'https://gateway.invalid/v1',
    stream: openAiStreamSimple,
  }),
  anthropic: Object.freeze({
    protocol: 'anthropic_messages',
    api: 'anthropic-messages',
    providerId: 'teacell-agent-anthropic',
    baseUrl: 'https://gateway.invalid',
    stream: anthropicStreamSimple,
  }),
});
const GATEWAY_MODEL_COMPAT = Object.freeze({
  supportsStore: false,
  supportsDeveloperRole: false,
  supportsReasoningEffort: false,
  supportsUsageInStreaming: true,
  sendSessionAffinityHeaders: false,
  maxTokensField: 'max_tokens',
});

// Fallback declarations used when the lease does not carry a real model window
// (offline/mock sessions, or an older Rust gateway that has not yet expanded
// `wire_payload`). A leased session overrides both from the lease:
//   * `lease.contextWindow` — the model's real window, and
//   * `lease.maxOutputTokens` — the adaptive reply budget (default 4096, Rust
//     ceiling MAX_OUTPUT_TOKENS = 8192).
// pi-ai subtracts a fixed CONTEXT_SAFETY_TOKENS = 4096 from the window before
// clamping `max_tokens`, so a too-small declared window silently shrank the
// reply budget once the full tool surface (18 Pi tools ≈ 2.5k tokens) was
// registered. The 32768/4096 fallback keeps `max_tokens` at the full adaptive
// budget for a healthy window while staying a conservative floor for tests.
const GATEWAY_CONTEXT_WINDOW = 32768;
const GATEWAY_MAX_OUTPUT_TOKENS = 4096;
// Ceilings mirror the Rust gateway (`MAX_OUTPUT_TOKENS = 8192`, and a sanity
// bound for the model window). Anything outside these → fall back to defaults.
const GATEWAY_MAX_OUTPUT_BUDGET = 8192;
const GATEWAY_MAX_CONTEXT_WINDOW = 4_000_000;
const THINKING_LEVELS = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh'];

function mockStream(model, _context, options) {
  const stream = createAssistantMessageEventStream();
  const message = {
    role: 'assistant', content: [], api: model.api, provider: model.provider,
    model: model.id, timestamp: Date.now(), stopReason: 'stop',
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  };
  void (async () => {
    try {
      stream.push({ type: 'start', partial: message });
      message.content.push({ type: 'text', text: '' });
      stream.push({ type: 'text_start', contentIndex: 0, partial: message });
      for (const text of ['离线', ' Host', ' 已连接。']) {
        await delay(35, undefined, { signal: options?.signal });
        message.content[0].text += text;
        stream.push({ type: 'text_delta', contentIndex: 0, delta: text, partial: message });
      }
      stream.push({ type: 'text_end', contentIndex: 0, content: message.content[0].text, partial: message });
      stream.push({ type: 'done', reason: 'stop', message });
    } catch {
      message.stopReason = 'aborted';
      stream.push({ type: 'error', reason: 'aborted', error: message });
    }
  })();
  return stream;
}

function trustedLoader() {
  const extensions = { extensions: [], errors: [], runtime: createExtensionRuntime() };
  return {
    getExtensions: () => extensions,
    getSkills: () => ({ skills: [], diagnostics: [] }),
    getPrompts: () => ({ prompts: [], diagnostics: [] }),
    getThemes: () => ({ themes: [], diagnostics: [] }),
    getAgentsFiles: () => ({ agentsFiles: [] }),
    getSystemPrompt: () => '茶馆本地 Agent。模型请求由本机 Rust 网关转发到你已经选择的 AI 服务，你无法自行更改服务、密钥或联网开关。你只能调用已注册的素材工具（搜索、回收站计划准备与提交）；提交类工具必须等待界面的用户批准，不得自行批准、不得绕开工具直接改库，也不得声称拥有其他能力。',
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [],
    getAppendSystemPromptSources: () => [],
    extendResources: () => { throw new Error('RESOURCE_EXTENSION_DISABLED'); },
    reload: async () => {},
  };
}

export async function createOfflineSession(agentDir, conversationId, mode, options = {}) {
  const { gatewayRpc, capabilityRpc, pinSessionFile, unpinSessionFile } = options;
  if (VERSION !== SDK_VERSION) throw new Error('SDK_VERSION_MISMATCH');
  if (typeof pinSessionFile !== 'function' || typeof unpinSessionFile !== 'function') {
    throw new Error('SESSION_FILE_PINNER_UNAVAILABLE');
  }
  // A3-10：会话档位由 Rust 决定并下发「本次会话应注册的工具集合」。Host 不复制
  // 档位表——只校验它确实是注册集合的子集、无重复、非空，然后照单注册。
  const sessionTools = normalizeSessionToolNames(options.tools);
  const sessionsDir = await ensureSessionsDirectory(agentDir);
  await validateSessionFiles(sessionsDir);
  const found = await findPinnedSession(sessionsDir, conversationId, pinSessionFile, unpinSessionFile);
  if (mode === 'new' && found) {
    if (found.pinnedFile && unpinSessionFile) await unpinSessionFile(found.pinnedFile);
    throw new Error('CONVERSATION_EXISTS');
  }
  if (mode !== 'new' && !found) throw new Error('CONVERSATION_NOT_FOUND');

  const credentials = {
    async read() { return undefined; },
    async list() { return []; },
    async modify(_providerId, update) { return update(undefined); },
    async delete() {},
  };
  const runtime = await ModelRuntime.create({
    credentials,
    modelsPath: null,
    refreshOnCreate: false,
    allowModelNetwork: false,
  });
  runtime.registerProvider('teacell-offline', {
    baseUrl: 'https://offline.invalid',
    api: 'openai-completions',
    apiKey: 'offline-only-placeholder',
    streamSimple: mockStream,
    models: [{ id: 'offline-mock', name: 'Offline mock', api: 'openai-completions',
      reasoning: false, input: ['text'], contextWindow: 4096, maxTokens: 256,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
  });
  let sessionManager;
  let pinnedSessionFile = found?.pinnedFile;
  let activeGatewayRun;
  try {
    if (found) {
      await verifySessionFile(sessionsDir, found.path);
      sessionManager = SessionManager.open(found.path, sessionsDir, agentDir);
    } else {
      const created = SessionManager.create(agentDir, sessionsDir, { id: conversationId });
      const path = created.getSessionFile();
      pinnedSessionFile = basename(path);
      await writeFile(path, `${JSON.stringify(created.fileEntries[0])}\n`, { flag: 'wx' });
      await pinSessionFile(pinnedSessionFile);
      const pinnedHeader = await readSessionHeader(path);
      if (pinnedHeader?.id !== conversationId) throw new Error('SESSION_FILE_INVALID');
      sessionManager = SessionManager.open(path, sessionsDir, agentDir);
    }
    const { session } = await createAgentSession({
      cwd: agentDir,
      agentDir,
      modelRuntime: runtime,
      model: runtime.getModel('teacell-offline', 'offline-mock'),
      tools: [...sessionTools],
      customTools: createPiTools({ capabilityRpc }),
      resourceLoader: trustedLoader(),
      sessionManager,
      // Pi 自带上下文自动压缩（阈值/overflow/length 三路）。默认开启，复用而非自研；
      // `autoCompaction: false` 仅供需要断言「不隐式重试」的测试/诊断场景。
      // reserve/keep 取 8192：对 32768 窗口在 ~24576 tokens 触发压缩，对 4096 的
      // offline-mock 因 window-reserve<=0 永不触发，保证既有离线用例不受影响。
      settingsManager: SettingsManager.inMemory({
        compaction: {
          enabled: options.autoCompaction !== false,
          reserveTokens: 8192,
          keepRecentTokens: 8192,
        },
        retry: { enabled: false },
      }),
    });
    const activeTools = session.getActiveToolNames();
    if (activeTools.length !== sessionTools.length ||
        !sessionTools.every(name => activeTools.includes(name))) {
      session.dispose();
      throw new Error('UNEXPECTED_TOOL');
    }
    async function setGatewayLease(lease, identity) {
      const requiredLeaseKeys = ['leaseId', 'model', 'protocol', 'provider'];
      const optionalLeaseKeys = ['contextWindow', 'maxOutputTokens', 'reasoning', 'thinkingLevel'];
      if (!gatewayRpc || !lease || !identity ||
          !requiredLeaseKeys.every(key => Object.hasOwn(lease, key)) ||
          !Object.keys(lease).every(key => requiredLeaseKeys.includes(key) || optionalLeaseKeys.includes(key)) ||
          Object.keys(identity).sort().join(',') !== 'conversationId,runId' ||
          typeof lease.leaseId !== 'string' || !/^[A-Za-z0-9_-]{16,128}$/u.test(lease.leaseId) ||
          identity.conversationId !== conversationId ||
          typeof identity.runId !== 'string' || !/^[A-Za-z0-9_-]{16,128}$/u.test(identity.runId) ||
          typeof lease.model !== 'string' || lease.model.length === 0 || lease.model.length > 256 ||
          lease.model.includes('\0')) {
        throw new Error('INVALID_MODEL_LEASE');
      }
      if (typeof lease.provider !== 'string' || !Object.hasOwn(GATEWAY_MODELS, lease.provider) ||
          typeof lease.protocol !== 'string') {
        throw new Error('INVALID_MODEL_LEASE');
      }
      const gateway = GATEWAY_MODELS[lease.provider];
      if (!gateway || lease.protocol !== gateway.protocol) throw new Error('INVALID_MODEL_LEASE');

      // 真实模型窗口/自适应输出预算由 Rust lease 携带；缺省或越界回落到保守常量。
      const contextWindow = Number.isSafeInteger(lease.contextWindow) &&
        lease.contextWindow >= 1024 && lease.contextWindow <= GATEWAY_MAX_CONTEXT_WINDOW
        ? lease.contextWindow : GATEWAY_CONTEXT_WINDOW;
      const maxTokens = Number.isSafeInteger(lease.maxOutputTokens) &&
        lease.maxOutputTokens >= 1 && lease.maxOutputTokens <= GATEWAY_MAX_OUTPUT_BUDGET
        ? lease.maxOutputTokens : GATEWAY_MAX_OUTPUT_TOKENS;
      const reasoning = lease.reasoning === true;

      clearGatewayLease();
      const gatewayFetch = createGatewayFetch(gatewayRpc.exchange);
      runtime.registerProvider(gateway.providerId, {
        baseUrl: gateway.baseUrl,
        api: gateway.api,
        apiKey: GATEWAY_API_KEY,
        authHeader: true,
        streamSimple: (model, context, streamOptions) => gateway.stream(model, context, {
          ...streamOptions,
          apiKey: GATEWAY_API_KEY,
          cacheRetention: 'none',
          fetch: gatewayFetch,
          maxRetries: 0,
          transport: 'sse',
        }),
        models: [{
          id: lease.model,
          name: lease.model,
          api: gateway.api,
          reasoning,
          input: ['text'],
          contextWindow,
          maxTokens,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
          ...(lease.provider === 'openai'
            ? { compat: reasoning ? { ...GATEWAY_MODEL_COMPAT, supportsReasoningEffort: true } : GATEWAY_MODEL_COMPAT }
            : {}),
        }],
      });
      const model = runtime.getModel(gateway.providerId, lease.model);
      if (!model) throw new Error('GATEWAY_MODEL_UNAVAILABLE');
      try {
        await session.setModel(model);
        // 思考档位：仅在模型声明支持 reasoning 且租约显式给出合法档位时应用。
        if (reasoning && typeof lease.thinkingLevel === 'string' &&
            THINKING_LEVELS.includes(lease.thinkingLevel) &&
            typeof session.setThinkingLevel === 'function') {
          try { session.setThinkingLevel(lease.thinkingLevel); } catch { /* 不支持的档位忽略，不阻断租约 */ }
        }
        gatewayRpc.setLease({
          conversationId,
          runId: identity.runId,
          leaseId: lease.leaseId,
        });
        activeGatewayRun = identity.runId;
      } catch {
        gatewayRpc.clearLease();
        throw new Error('GATEWAY_MODEL_UNAVAILABLE');
      }
    }
    function clearGatewayLease(runId) {
      if (runId && activeGatewayRun && runId !== activeGatewayRun) return;
      activeGatewayRun = undefined;
      gatewayRpc?.clearLease();
    }
    return { session, pinnedSessionFile, tools: sessionTools, setGatewayLease, clearGatewayLease };
  } catch (error) {
    if (pinnedSessionFile) await unpinSessionFile(conversationId, pinnedSessionFile).catch(() => {});
    throw error;
  }
}

// 只读投影：把 Pi JSONL 会话（唯一事实源）解析成 compaction-aware 的消息投影，
// 供 UI 恢复历史。全程只读取，不写会话文件、不伪造「收据」，也不改变任何状态。
// 若会话不在内存中打开，用 pin 机制安全打开文件后读投影，读毕立即 unpin。
// 返回值是 SessionProjection（{ messages, thinkingLevel, model, entries }）。
export async function readConversationProjection(agentDir, conversationId, options = {}) {
  const { pinSessionFile, unpinSessionFile } = options;
  if (typeof pinSessionFile !== 'function' || typeof unpinSessionFile !== 'function') {
    throw new Error('SESSION_FILE_PINNER_UNAVAILABLE');
  }
  const sessionsDir = await ensureSessionsDirectory(agentDir);
  await validateSessionFiles(sessionsDir);
  const found = await findPinnedSession(sessionsDir, conversationId, pinSessionFile, unpinSessionFile);
  if (!found) throw new Error('CONVERSATION_NOT_FOUND');
  try {
    await verifySessionFile(sessionsDir, found.path);
    const sessionManager = SessionManager.open(found.path, sessionsDir, agentDir);
    return sessionManager.buildSessionProjection();
  } finally {
    await unpinSessionFile(found.pinnedFile).catch(() => {});
  }
}

async function findPinnedSession(sessionsDir, conversationId, pinSessionFile, unpinSessionFile) {
  const expectedSuffix = `_${conversationId}.jsonl`;
  for (const file of await readdir(sessionsDir)) {
    if (!file.endsWith(expectedSuffix)) continue;
    await pinSessionFile(file);
    const path = join(sessionsDir, file);
    const header = await readSessionHeader(path).catch(() => undefined);
    if (header?.id === conversationId) return { path, pinnedFile: basename(path) };
    await unpinSessionFile(file);
  }
  return undefined;
}

async function readSessionHeader(path) {
  const handle = await open(path, 'r');
  try {
    const chunk = Buffer.alloc(64 * 1024);
    let text = '';
    while (text.length < chunk.length) {
      const { bytesRead } = await handle.read(chunk, 0, chunk.length, null);
      if (bytesRead === 0) break;
      text += chunk.toString('utf8', 0, bytesRead);
      const newline = text.indexOf('\n');
      if (newline >= 0) text = text.slice(0, newline);
      else if (bytesRead < chunk.length) break;
      else throw new Error('SESSION_HEADER_TOO_LARGE');
      try {
        const header = JSON.parse(text);
        return header?.type === 'session' && typeof header.id === 'string' ? header : undefined;
      } catch {
        return undefined;
      }
    }
    if (!text) return undefined;
    try {
      const header = JSON.parse(text);
      return header?.type === 'session' && typeof header.id === 'string' ? header : undefined;
    } catch {
      return undefined;
    }
  } finally {
    await handle.close();
  }
}
