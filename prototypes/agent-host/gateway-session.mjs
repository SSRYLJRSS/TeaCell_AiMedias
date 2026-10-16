import { join } from 'node:path';
import { createAgentSession, ModelRuntime, SessionManager, SettingsManager } from '@earendil-works/pi-coding-agent';
import { streamSimple as openaiStream } from '@earendil-works/pi-ai/api/openai-completions';
import { streamSimple as anthropicStream } from '@earendil-works/pi-ai/api/anthropic-messages';
import { trustedLoader } from './sdk-session.mjs';

export async function createGatewaySession(directory, gateway, { protocol = 'openai-completions', tools = [], settings = {}, fixtureImageCapability = false, fixtureSessionManager } = {}) {
  if (!['openai-completions', 'anthropic-messages'].includes(protocol)) throw new Error('UNSUPPORTED_PROTOCOL');
  const runtime = await ModelRuntime.create({
    authPath: join(directory, 'auth.json'), modelsPath: null,
    modelsStorePath: join(directory, 'models-cache.json'), refreshOnCreate: false, allowModelNetwork: false,
  });
  const providerStream = protocol === 'openai-completions' ? openaiStream : anthropicStream;
  runtime.registerProvider('teacell-gateway', {
    baseUrl: `https://teacell-gateway.invalid${protocol === 'openai-completions' ? '/v1' : ''}`, api: protocol,
    apiKey: 'public-placeholder-for-configuration-only',
    streamSimple: (model, context, options) => providerStream(model, context, {
      ...options, fetch: gateway.fetch, transport: 'sse', maxRetries: 0,
      apiKey: 'public-placeholder-for-configuration-only',
    }),
    models: [{ id: 'mock', name: 'Gateway fixture', api: protocol, reasoning: false,
      input: fixtureImageCapability ? ['text', 'image'] : ['text'], contextWindow: 4096, maxTokens: 256,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
  });
  const { session } = await createAgentSession({
    cwd: directory, agentDir: directory, modelRuntime: runtime,
    model: runtime.getModel('teacell-gateway', 'mock'), tools: tools.map(tool => tool.name), customTools: tools,
    resourceLoader: trustedLoader(), sessionManager: fixtureSessionManager ?? SessionManager.inMemory(directory),
    settingsManager: SettingsManager.inMemory({ compaction: { enabled: false, keepRecentTokens: 0, reserveTokens: 256 }, retry: { enabled: false }, ...settings, cacheWarming: 'off' }),
  });
  return session;
}
