// Isolated negative fixture: launched with a poisoned project directory and a
// sanitized environment. Public sentinels are not real service credentials.
import process from 'node:process';
import { GatewayTransport } from '../gateway-transport.mjs';
import { executable } from '../test/helpers/gateway-fixture.mjs';

const [port, directory] = process.argv.slice(2);
const forbiddenNames = /^(node_options|node_path|path|http_proxy|https_proxy|all_proxy|openai_api_key|anthropic_api_key|pi_coding_agent_dir)$/i;
const forbiddenEnvironment = () => Object.keys(process.env).filter(name => forbiddenNames.test(name) && !(name.toLowerCase() === 'path' && process.env[name] === ''));
const bootstrapForbidden = forbiddenEnvironment();
const { createGatewaySession } = await import('../gateway-session.mjs');
const bridge = new GatewayTransport(executable, Number(port), { fixtureWorkingDirectory: directory });
let session;
try {
  await bridge.ready;
  session = await createGatewaySession(directory, bridge);
  await session.prompt('公开环境哨兵实验');
  const result = session.agent.state.messages.at(-1);
  if (result.stopReason !== 'stop') throw new Error('FIXTURE_RESPONSE_FAILED');
  const forbidden = forbiddenEnvironment();
  process.stdout.write(`${JSON.stringify({ ok: true, bootstrapForbidden, forbidden, emptyPath: process.env.PATH === '', tools: session.getActiveToolNames(), attempts: bridge.attempts })}\n`);
} finally {
  session?.dispose();
  await bridge.close();
}
