// Test-only child process. All paths come from the trusted fixture launcher;
// this is not a GUI/Pi path-selection API or a production session index.
import process from 'node:process';
import { join, resolve, dirname } from 'node:path';
import { SessionManager } from '@earendil-works/pi-coding-agent';
import { GatewayTransport } from '../gateway-transport.mjs';
import { createGatewaySession } from '../gateway-session.mjs';
import { executable } from '../test/helpers/gateway-fixture.mjs';
import { preflightSession } from '../session-preflight.mjs';

const [port, directory, protocol, mode, file] = process.argv.slice(2);
if (!['create', 'resume', 'inspect'].includes(mode)) throw new Error('INVALID_FIXTURE_MODE');
const sessions = join(directory, 'sessions');
if (mode !== 'create' && (!file || dirname(resolve(file)) !== resolve(sessions))) throw new Error('INVALID_FIXTURE_SESSION_PATH');
if (mode !== 'create') await preflightSession(file, sessions);
const manager = mode === 'create' ? SessionManager.create(directory, sessions) : SessionManager.open(file, sessions, directory);
const bridge = new GatewayTransport(executable, Number(port), { fixtureWorkingDirectory: directory });
let session;
let calls = 0;
try {
  await bridge.ready;
  session = await createGatewaySession(directory, bridge, { protocol, fixtureSessionManager: manager, tools: [{
    name: 'lookup_fixture', label: 'Fixture lookup', description: 'Fixture only',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: async () => { calls++; return { content: [{ type: 'text', text: 'persisted-fixture-tool-receipt' }], details: {} }; },
  }] });
  const restored = { messageCount: session.agent.state.messages.length, entryCount: manager.getEntryCount(), calls, attempts: bridge.attempts };
  if (mode !== 'inspect') {
    await session.prompt(mode === 'create' ? '持久对话的第一轮' : '用户明确继续后的新一轮');
    if (session.agent.state.messages.at(-1).stopReason !== 'stop') throw new Error('FIXTURE_PROMPT_FAILED');
    if (mode === 'create') await session.compact('保留已经完成的工具记录');
  }
  process.stdout.write(`${JSON.stringify({ file: session.sessionFile, id: session.sessionId, restored, calls, attempts: bridge.attempts,
    tools: session.getActiveToolNames(), messageCount: session.agent.state.messages.length, entryCount: manager.getEntryCount(),
    compactions: manager.getEntries().filter(entry => entry.type === 'compaction').length,
    toolResults: manager.getEntries().filter(entry => entry.type === 'message' && entry.message.role === 'toolResult').length })}\n`);
} finally {
  session?.dispose();
  await bridge.close();
}
