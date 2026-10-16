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
    getSystemPrompt: () => '茶馆隔离 Host。当前运行仅使用离线测试模型，不提供业务工具。',
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [],
    getAppendSystemPromptSources: () => [],
    extendResources: () => { throw new Error('RESOURCE_EXTENSION_DISABLED'); },
    reload: async () => {},
  };
}

export async function createOfflineSession(agentDir, conversationId, mode, options = {}) {
  const { gatewayRpc, pinSessionFile, unpinSessionFile } = options;
  if (VERSION !== SDK_VERSION) throw new Error('SDK_VERSION_MISMATCH');
  if (typeof pinSessionFile !== 'function' || typeof unpinSessionFile !== 'function') {
    throw new Error('SESSION_FILE_PINNER_UNAVAILABLE');
  }
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
      tools: [],
      customTools: [],
      resourceLoader: trustedLoader(),
      sessionManager,
      settingsManager: SettingsManager.inMemory({
        compaction: { enabled: false },
        retry: { enabled: false },
      }),
    });
    if (session.getActiveToolNames().length !== 0) {
      session.dispose();
      throw new Error('UNEXPECTED_TOOL');
    }
    async function setGatewayLease(lease, identity) {
      if (!gatewayRpc || !lease || !identity ||
          Object.keys(lease).sort().join(',') !== 'leaseId,model,protocol,provider' ||
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
          reasoning: false,
          input: ['text'],
          contextWindow: 8192,
          maxTokens: 2048,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
          ...(lease.provider === 'openai' ? { compat: GATEWAY_MODEL_COMPAT } : {}),
        }],
      });
      const model = runtime.getModel(gateway.providerId, lease.model);
      if (!model) throw new Error('GATEWAY_MODEL_UNAVAILABLE');
      try {
        await session.setModel(model);
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
    return { session, pinnedSessionFile, setGatewayLease, clearGatewayLease };
  } catch (error) {
    if (pinnedSessionFile) await unpinSessionFile(conversationId, pinnedSessionFile).catch(() => {});
    throw error;
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
