import test from 'node:test';
import { Buffer } from 'node:buffer';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CAPABILITY_ERROR_CODES, createCapabilityRpc } from '../src/capability-rpc.mjs';
import { createPiTools, PI_TOOL_NAMES } from '../src/tools.mjs';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const contract = JSON.parse(readFileSync(
  join(repositoryRoot, 'docs', 'contracts', 'agent-v1.json'),
  'utf8',
));
const schema = JSON.parse(readFileSync(
  join(repositoryRoot, 'docs', 'contracts', 'agent-v1.schema.json'),
  'utf8',
));
const instanceId = `instance_${randomUUID().replaceAll('-', '')}`;

function harness() {
  const sent = [];
  const rpc = createCapabilityRpc({
    instanceId,
    sendFrame: frame => sent.push(frame),
  });
  return { sent, rpc };
}

test('capability_error_codes_mirror_the_frozen_contract', () => {
  const declared = contract.enums.AgentErrorCode.map(entry => entry.id ?? entry);
  assert.deepEqual([...CAPABILITY_ERROR_CODES].sort(), [...declared].sort());
});

test('exchange_sends_a_bounded_request_and_resolves_the_result', async () => {
  const { sent, rpc } = harness();
  const pending = rpc.exchange('assets.search', { text: '森林' });
  assert.equal(sent.length, 1);
  const [frame] = sent;
  assert.equal(frame.type, 'capability.request');
  assert.equal(frame.protocol, 1);
  assert.equal(frame.instance, instanceId);
  assert.match(frame.requestId, /^[A-Za-z0-9_-]{16,128}$/u);
  assert.equal(frame.capabilityId, 'assets.search');
  assert.deepEqual(JSON.parse(Buffer.from(frame.payload, 'base64').toString('utf8')), { text: '森林' });

  const result = { schemaVersion: 1, total: 0, truncated: false, items: [] };
  rpc.receive({
    protocol: 1,
    instance: instanceId,
    type: 'capability.result',
    requestId: frame.requestId,
    capabilityId: 'assets.search',
    ok: true,
    payload: Buffer.from(JSON.stringify(result), 'utf8').toString('base64'),
  });
  assert.deepEqual(await pending, result);
});

test('fixed_failure_codes_are_preserved_and_unknown_ones_degrade_to_internal', async () => {
  const { sent, rpc } = harness();
  const failed = rpc.exchange('assets.search', { text: 'x' });
  rpc.receive({
    protocol: 1,
    instance: instanceId,
    type: 'capability.result',
    requestId: sent[0].requestId,
    capabilityId: 'assets.search',
    ok: false,
    code: 'LIBRARY_CHANGED',
  });
  await assert.rejects(failed, error => error.code === 'LIBRARY_CHANGED');

  const unknown = rpc.exchange('assets.search', { text: 'x' });
  rpc.receive({
    protocol: 1,
    instance: instanceId,
    type: 'capability.result',
    requestId: sent[1].requestId,
    capabilityId: 'assets.search',
    ok: false,
    code: 'NOT_A_CONTRACT_CODE',
  });
  await assert.rejects(unknown, error => error.code === 'INTERNAL');
});

test('invalid_requests_fail_before_anything_is_sent', async () => {
  const { sent, rpc } = harness();
  await assert.rejects(rpc.exchange('../assets.search', {}), error => error.code === 'UNSUPPORTED_CAPABILITY');
  await assert.rejects(rpc.exchange('assets.search', { text: 'x'.repeat(40 * 1024) }),
    error => error.code === 'INVALID_INPUT');
  assert.equal(sent.length, 0);
});

test('result_frames_must_match_the_request_identity', async () => {
  const { sent, rpc } = harness();
  const pending = rpc.exchange('assets.search', { text: 'x' });
  assert.throws(() => rpc.receive({
    protocol: 1,
    instance: instanceId,
    type: 'capability.result',
    requestId: sent[0].requestId,
    capabilityId: 'assets.list',
    ok: false,
    code: 'INTERNAL',
  }), /CAPABILITY_INVALID_FRAME/u);
  rpc.receive({
    protocol: 1,
    instance: instanceId,
    type: 'capability.result',
    requestId: sent[0].requestId,
    capabilityId: 'assets.search',
    ok: false,
    code: 'CANCELLED',
  });
  await assert.rejects(pending, error => error.code === 'CANCELLED');
});

test('aborting_a_tool_call_fails_closed_and_ignores_the_late_result', async () => {
  const { sent, rpc } = harness();
  const controller = new globalThis.AbortController();
  const pending = rpc.exchange('assets.search', { text: 'x' }, { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, error => error.code === 'CANCELLED');
  assert.equal(sent.length, 1);
  rpc.receive({
    protocol: 1,
    instance: instanceId,
    type: 'capability.result',
    requestId: sent[0].requestId,
    capabilityId: 'assets.search',
    ok: false,
    code: 'CANCELLED',
  });
});

test('tool_definitions_declare_only_contract_backed_names_and_parameters', async () => {
  const { rpc } = harness();
  const tools = createPiTools({ capabilityRpc: rpc });
  // PI_TOOL_NAMES is the canonical (sorted) declaration that the Rust host
  // validates on ready; the tools array keeps a reading order for humans.
  assert.deepEqual(tools.map(tool => tool.name).sort(), [...PI_TOOL_NAMES].sort());
  const byName = new Map(tools.map(tool => [tool.name, tool]));
  const search = byName.get('tea_assets_search');
  assert.equal(search.parameters.type, 'object');
  assert.deepEqual(Object.keys(search.parameters.properties), ['text']);
  assert.equal(search.parameters.properties.text.maxLength,
    schema.$defs.Instructions.maxLength);
  assert.equal(search.parameters.additionalProperties, false);
  assert.ok(contract.capabilities['assets.search'].callers.includes('pi'));

  const trashPrepare = byName.get('tea_assets_trash_prepare');
  assert.deepEqual(Object.keys(trashPrepare.parameters.properties), ['selectionRef']);
  assert.equal(trashPrepare.parameters.properties.selectionRef.maxLength, 128);
  assert.equal(trashPrepare.parameters.additionalProperties, false);
  assert.ok(contract.capabilities['assets.trash.prepare'].callers.includes('pi'));

  const trashSubmit = byName.get('tea_assets_trash_submit');
  assert.deepEqual(Object.keys(trashSubmit.parameters.properties),
    ['planId', 'idempotencyKey']);
  assert.equal(trashSubmit.parameters.properties.planId.maxLength, 128);
  assert.equal(trashSubmit.parameters.properties.idempotencyKey.maxLength, 128);
  assert.equal(trashSubmit.parameters.additionalProperties, false);
  assert.ok(contract.capabilities['assets.trash.submit'].callers.includes('pi'));

  const operationTools = [
    byName.get('tea_operations_cancel'),
    byName.get('tea_operations_get'),
  ];
  for (const operationTool of operationTools) {
    assert.equal(operationTool.parameters.type, 'object');
    assert.deepEqual(Object.keys(operationTool.parameters.properties), ['operationId']);
    assert.equal(operationTool.parameters.properties.operationId.maxLength,
      schema.$defs.ResourceRef.maxLength);
    assert.equal(operationTool.parameters.additionalProperties, false);
  }
  assert.ok(contract.capabilities['operations.get'].callers.includes('pi'));
  assert.ok(contract.capabilities['operations.cancel'].callers.includes('pi'));

  // Every declared Pi tool must map back to a frozen contract capability that
  // lists `pi` as a caller, and its parameter names must be exactly that
  // capability's input record fields minus `schemaVersion` (which the Rust host
  // injects and re-validates). This is what stops the Node registry from
  // drifting away from the frozen contract as adapters are added.
  //
  // `assets.search.cursor` is the one documented omission: pagination cursors
  // are not implemented, so `search_assets` fails closed on a non-null cursor
  // and the tool must not invite the model to pass one.
  const documentedOmissions = new Map([['assets.search', ['cursor']]]);
  for (const tool of tools) {
    const capabilityId = tool.name.slice('tea_'.length).replaceAll('_', '.');
    const capability = contract.capabilities[capabilityId];
    assert.ok(capability, `${tool.name} has no contract capability`);
    assert.ok(capability.callers.includes('pi'), `${capabilityId} is not Pi-callable`);
    const omitted = documentedOmissions.get(capabilityId) ?? [];
    const expected = Object.keys(schema.$defs[capability.input].properties)
      .filter(key => key !== 'schemaVersion' && !omitted.includes(key));
    assert.deepEqual(Object.keys(tool.parameters.properties).sort(), expected.sort(),
      `${tool.name} parameters must match ${capability.input} minus schemaVersion`);
    assert.equal(tool.parameters.additionalProperties, false);
    // The Rust gateway validates the declared schema root against a frozen
    // four-key allowlist (type/properties/required/additionalProperties) and
    // answers REQUEST_INVALID for any extra key. TypeBox merges the second
    // `Type.Object(props, options)` argument INTO the schema root, so a root
    // `description` silently breaks every model request instead of failing
    // here. Assert the exact root shape so that regression is caught at the
    // tool registry instead of at runtime.
    assert.deepEqual(
      Object.keys(tool.parameters).sort(),
      ['additionalProperties', 'properties', 'required', 'type'],
      `${tool.name} schema root may only carry the four keys the gateway allows`,
    );
  }

  // GUI-only entries must never appear as model tools, and imports.* stays an
  // explicit documented gap (A3.md item 4) rather than a tool that only fails.
  for (const forbidden of [
    'tea_plans_approve',
    'tea_operations_resume',
    'tea_trash_empty_prepare',
    'tea_trash_empty_submit',
    'tea_imports_prepare',
    'tea_imports_submit',
  ]) {
    assert.ok(!PI_TOOL_NAMES.includes(forbidden), `${forbidden} must never be a Pi tool`);
  }
  assert.equal(PI_TOOL_NAMES.length, 16);
});

test('tool_execute_returns_the_encoded_record_and_reports_fixed_errors', async () => {
  const record = { schemaVersion: 1, total: 2, truncated: false, items: [] };
  const tools = createPiTools({
    capabilityRpc: { exchange: async () => record },
  });
  const ok = await tools[0].execute('call_1', { text: 'x' });
  assert.equal(ok.isError, undefined);
  assert.deepEqual(JSON.parse(ok.content[0].text), record);
  assert.deepEqual(ok.details, record);

  const failing = createPiTools({
    capabilityRpc: {
      exchange: async () => {
        const error = new Error('failed');
        error.code = 'LIBRARY_CHANGED';
        throw error;
      },
    },
  });
  const failed = await failing[0].execute('call_2', { text: 'x' });
  assert.equal(failed.isError, true);
  assert.deepEqual(failed.details, { code: 'LIBRARY_CHANGED' });
});
