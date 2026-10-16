import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, writeFile, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fixture, openai } from './helpers/gateway-fixture.mjs';
import { createGatewaySession } from '../gateway-session.mjs';
import { invokeWorkflowFixture, workflowFixtureTools } from '../workflow-bridge.mjs';

const parameters = collection => ({ collection, tagging_instructions: '识别场景，保持建议待审核', rename: true });
const definition = { schema: 1, capability_schema: 1, id: 'import-and-tag', version: 1,
  name: '导入并生成建议', defaults: parameters('旅行素材'), steps: [{ kind: 'import' }, { kind: 'suggest_tags', input_step: 0 }] };

test('Pi explicitly loads a version and prepares fresh plans without read/bash/approval tools', { timeout: 20000 }, async t => {
  const f = await fixture(t, (_request, response, count) => {
    const round = (count - 1) % 3;
    if (round === 0) openai(response, { tool: { name: 'load_personal_workflow', args: { id: 'import-and-tag', version: 1 } } });
    else if (round === 1) openai(response, { tool: { name: 'prepare_personal_workflow', args: { id: 'import-and-tag', version: 1, parameters: parameters(count < 4 ? '旅行素材' : '产品素材') } } });
    else openai(response, { text: '本次计划等待你确认。' });
  });
  const source = join(f.directory, 'fixture-source');
  await mkdir(source);
  await writeFile(join(source, 'first.jpg'), 'first-fixture');
  await writeFile(join(source, 'second.jpg'), 'second-fixture');
  const context = { db: join(f.directory, 'workflows.sqlite'), source, inputNames: ['first.jpg'] };
  await invokeWorkflowFixture(context, { action: 'save', definition, expected_version: 0 }, { admin: true });
  const plans = [];
  for (const input of ['first.jpg', 'second.jpg']) {
    const tools = workflowFixtureTools({ ...context, inputNames: [input] });
    const session = await createGatewaySession(f.directory, f.bridge, { tools });
    try {
      assert.deepEqual(session.getActiveToolNames().sort(), ['load_personal_workflow', 'prepare_personal_workflow']);
      await session.prompt('显式加载 import-and-tag 的第1版，为当前素材生成新的计划。');
      const results = session.agent.state.messages.filter(message => message.role === 'toolResult');
      assert.equal(results.length, 2);
      assert.ok(results.every(result => !result.isError), JSON.stringify(results));
      assert.match(results[0].details.skill, /每次仍需新的用户确认/);
      plans.push(results[1].details);
      assert.equal(session.agent.state.messages.at(-1).content[0].text, '本次计划等待你确认。');
    } finally { session.dispose(); }
  }
  assert.notEqual(plans[0].plan_id, plans[1].plan_id);
  assert.notDeepEqual(plans[0].inputs, plans[1].inputs);
  assert.equal(plans[1].parameters.collection, '产品素材');
  assert.equal(plans[1].state, 'awaiting_confirmation');
  assert.equal(f.bridge.attempts, 6);
  assert.equal(await readFile(join(source, 'first.jpg'), 'utf8'), 'first-fixture');
  assert.equal(await readFile(join(source, 'second.jpg'), 'utf8'), 'second-fixture');
});

test('agent fixture rejects admin edits and old approval fields', { timeout: 10000 }, async t => {
  const f = await fixture(t);
  const source = join(f.directory, 'fixture-source'); await mkdir(source);
  const context = { db: join(f.directory, 'workflows.sqlite'), source, inputNames: [] };
  await invokeWorkflowFixture(context, { action: 'save', definition, expected_version: 0 }, { admin: true });
  await assert.rejects(invokeWorkflowFixture(context, { action: 'save', definition: { ...definition, version: 2 }, expected_version: 1 }), /TRUSTED_EDIT_REQUIRED/);
  await assert.rejects(invokeWorkflowFixture(context, { action: 'load', id: definition.id, version: 1, approval: 'old-approved' }), /unknown field/);
  const loaded = await invokeWorkflowFixture(context, { action: 'load', id: definition.id, version: 1 });
  assert.equal(loaded.definition.version, 1);
  assert.equal(f.received.length, 0);
});
