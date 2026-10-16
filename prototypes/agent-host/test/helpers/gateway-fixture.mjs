import http from 'node:http';
import https from 'node:https';
import process from 'node:process';
import { StringDecoder } from 'node:string_decoder';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { GatewayTransport } from '../../gateway-transport.mjs';
export const executable = fileURLToPath(new URL(`../../../model-gateway/target/debug/teacell-model-gateway-poc${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));

export function openai(response, { text = '网关回复', tool = false, inputTokens = 20, finish = 'stop' } = {}) {
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const chunk = (delta, finish_reason = null) => response.write(`data: ${JSON.stringify({ id: 'fixture', object: 'chat.completion.chunk', created: 1, model: 'mock', choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
  if (tool) chunk({ role: 'assistant', tool_calls: [{ index: 0, id: 'fixture-call', type: 'function', function: { name: typeof tool === 'object' ? tool.name : 'lookup_fixture', arguments: JSON.stringify(typeof tool === 'object' ? tool.args : {}) } }] });
  else { chunk({ role: 'assistant', content: text.slice(0, 2) }); chunk({ content: text.slice(2) }); }
  chunk({}, tool ? 'tool_calls' : finish);
  response.write(`data: ${JSON.stringify({ id: 'fixture', choices: [], usage: { prompt_tokens: inputTokens, completion_tokens: 5, total_tokens: inputTokens + 5 } })}\n\n`);
  response.end('data: [DONE]\n\n');
}
export function anthropic(response) {
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const event = data => response.write(`event: ${data.type}\ndata: ${JSON.stringify(data)}\n\n`);
  event({ type: 'message_start', message: { id: 'fixture', type: 'message', role: 'assistant', model: 'mock', content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 20, output_tokens: 0 } } });
  event({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
  event({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: '网关回复' } });
  event({ type: 'content_block_stop', index: 0 });
  event({ type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 5 } });
  event({ type: 'message_stop' });
  response.end();
}

export async function fixture(t, respond = (_request, response) => openai(response), { governed = false, revision = 1, fixtureProxyUrl, tls = false, trustTls = true } = {}) {
  if (!Number.isSafeInteger(revision) || revision < 1 || revision > 0xffffffff) throw new Error('INVALID_FIXTURE_REVISION');
  const received = [];
  const handle = async (incoming, response) => {
    let body = '';
    const decoder = new StringDecoder('utf8');
    for await (const chunk of incoming) body += decoder.write(chunk);
    body += decoder.end();
    received.push({ url: incoming.url, headers: incoming.headers, body: JSON.parse(body), secure: incoming.socket.encrypted === true });
    respond(incoming, response, received.length);
  };
  const fixtureDirectory = fileURLToPath(new URL('../fixtures/', import.meta.url));
  const server = tls
    ? https.createServer({
      key: await readFile(join(fixtureDirectory, 'gateway-test-key.pem')),
      cert: await readFile(join(fixtureDirectory, 'gateway-test-cert.pem')),
    }, handle)
    : http.createServer(handle);
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const directory = await mkdtemp(join(tmpdir(), 'teacell-gateway-'));
  const policyPath = join(directory, 'fixture-policy.json');
  if (governed) await writeFile(policyPath, JSON.stringify({ revision, enabled: true }));
  const bridge = new GatewayTransport(executable, server.address().port, {
    fixturePolicyPath: governed ? policyPath : undefined,
    fixtureProxyUrl,
    fixtureTlsCaPath: tls && trustTls ? join(fixtureDirectory, 'gateway-test-trust.pem') : undefined,
    fixtureScheme: tls ? 'https' : 'http',
    fixtureWorkingDirectory: directory,
    fixtureRevision: revision,
  });
  t.after(async () => {
    await bridge.close();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  });
  await bridge.ready;
  return { bridge, directory, received, policyPath, port: server.address().port, server, secure: tls };
}

