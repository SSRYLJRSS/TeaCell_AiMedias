import { join } from 'node:path';
import {
  createAgentSession, createExtensionRuntime, ModelRuntime,
  SessionManager, SettingsManager, VERSION,
} from '@earendil-works/pi-coding-agent';
import { createAssistantMessageEventStream } from '@earendil-works/pi-ai';
import { setTimeout as delay } from 'node:timers/promises';
import { SDK_VERSION } from './protocol.mjs';

export { SDK_VERSION } from './protocol.mjs';

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
      for (const text of ['原型', '流式', '回复']) {
        await delay(40, undefined, { signal: options?.signal });
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

// Explicit loader: no discovery from cwd, user's home, skills, packages or MCP.
export function trustedLoader() {
  const extensions = { extensions: [], errors: [], runtime: createExtensionRuntime() };
  return {
    getExtensions: () => extensions,
    getSkills: () => ({ skills: [], diagnostics: [] }),
    getPrompts: () => ({ prompts: [], diagnostics: [] }),
    getThemes: () => ({ themes: [], diagnostics: [] }),
    getAgentsFiles: () => ({ agentsFiles: [] }),
    getSystemPrompt: () => '素材助手隔离原型。没有业务写入工具。',
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [], getAppendSystemPromptSources: () => [],
    extendResources: () => { throw new Error('RESOURCE_EXTENSION_DISABLED'); },
    reload: async () => {},
  };
}

export async function createPrototypeSession(directory) {
  if (VERSION !== SDK_VERSION) throw new Error('SDK_VERSION_MISMATCH');
  const runtime = await ModelRuntime.create({
    authPath: join(directory, 'auth.json'), modelsPath: null,
    modelsStorePath: join(directory, 'models-cache.json'),
    refreshOnCreate: false, allowModelNetwork: false,
  });
  runtime.registerProvider('teacell-poc', {
    baseUrl: 'https://teacell-poc.invalid',
    api: 'openai-completions', apiKey: 'public-mock-value-not-a-credential',
    streamSimple: mockStream,
    models: [{ id: 'mock', name: 'Local mock', api: 'openai-completions',
      reasoning: false, input: ['text'], contextWindow: 4096, maxTokens: 256,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
  });
  const { session } = await createAgentSession({
    cwd: directory, agentDir: directory, modelRuntime: runtime,
    model: runtime.getModel('teacell-poc', 'mock'), tools: [], customTools: [],
    resourceLoader: trustedLoader(), sessionManager: SessionManager.inMemory(directory),
    settingsManager: SettingsManager.inMemory({
      compaction: { enabled: false }, retry: { enabled: false },
    }),
  });
  if (session.getActiveToolNames().length !== 0) {
    session.dispose();
    throw new Error('UNEXPECTED_TOOL');
  }
  return session;
}
