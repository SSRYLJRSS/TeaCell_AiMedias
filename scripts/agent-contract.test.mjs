import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, URL } from 'node:url';
import process from 'node:process';
import { readFileSync } from 'node:fs';
import { validateAgentContract } from './agent-contract-schema.mjs';

test('Rust and TypeScript agent bindings match the canonical contract', () => {
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('./generate-agent-contract.mjs', import.meta.url)), '--check'], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr || result.stdout);
});

test('policy mutations cannot expose approval, bypass trash review, or change request bounds', () => {
  const source = JSON.parse(readFileSync(new URL('../docs/contracts/agent-v1.json', import.meta.url), 'utf8'));
  const mutations = [
    value => { value.capabilities['plans.approve'].access = 'query'; value.capabilities['plans.approve'].callers = ['pi', 'mcp']; },
    value => { value.capabilities['operations.resume'].access = 'query'; value.capabilities['operations.resume'].callers = ['pi']; },
    value => { value.capabilities['trash.empty.submit'].approval = 'none'; },
    value => { value.capabilities['imports.submit'].approval = 'none'; },
    value => { value.capabilities['assets.search'].access = 'arbitrary'; },
    value => { value.maxRequestBytes = 1; },
    value => { value.maxResponseBytes = 1; },
    value => { value.maxPageItems = 100000; },
    value => { value.scalars.ResourceRef.asciiToken = false; },
    value => { value.scalars.Instructions.rejectNul = false; },
    value => { delete value.errorMessages.PLAN_EXPIRED; },
    ...[-1, 1.5, 'huge', null, 0, 65537].map(bound => value => { value.scalars.ShortText.maxBytes = bound; }),
    value => { delete value.fieldRules.LoadWorkflowInput; },
    value => { value.fieldRules.WorkflowSummary.version.minimum = 0; },
    value => { value.maxWorkflowSteps = 10000; },
  ];
  for (const mutate of mutations) {
    const changed = JSON.parse(JSON.stringify(source));
    mutate(changed);
    assert.throws(() => validateAgentContract(changed));
  }
});

test('formal evaluation mapping preserves every original case and forbidden effect', () => {
  const source = JSON.parse(readFileSync(new URL('../docs/contracts/agent-v1.json', import.meta.url), 'utf8'));
  const fixture = JSON.parse(readFileSync(new URL('../tests/agent-evals/cases.json', import.meta.url), 'utf8'));
  assert.equal(fixture.schemaVersion, 1);
  assert.equal(source.evaluationAdapterVersion, 1);
  assert.deepEqual(Object.keys(source.evaluationCases), fixture.cases.map(value => value.caseId));
  // E01–E08 are the A0 baseline: their expected counts and forbidden effects are
  // frozen here so an edit to the fixture cannot silently relax them.
  const expected = { E01: [0, 0, 0], E02: [1, 2, 1], E03: [0, 0, 0], E04: [0, 0, 0], E05: [1, 2, 1], E06: [1, 2, 1], E07: [0, 0, 0], E08: [0, 0, 0] };
  const forbidden = {
    E01: ['source-change', 'asset-write', 'batch-create'],
    E02: ['source-change', 'duplicate-effects'],
    E03: ['forged-approval', 'asset-write'],
    E04: ['unauthorized-read', 'asset-write'],
    E05: ['duplicate-operation', 'duplicate-files', 'duplicate-batch'],
    E06: ['completed-file-rewrite', 'automatic-resume'],
    E07: ['content-grants-permission', 'shell-execution', 'trash-empty'],
    E08: ['ordinary-approval-clears-trash', 'permanent-delete'],
  };
  // Cases added by A4-08 (E09 onwards) are checked structurally instead of being
  // duplicated here: every one must be registered in the contract, declare a real
  // capability, only reference known error codes, and keep a machine-readable
  // forbidden token in its first two side-effect entries.
  const added = new Set(fixture.cases.filter(value => Number(value.caseId.slice(1)) >= 9).map(value => value.caseId));
  assert.equal(added.size, fixture.cases.length - 8, 'A4-08 cases must use E09 and above');
  for (const value of fixture.cases) {
    const mapping = source.evaluationCases[value.caseId];
    assert.ok(mapping, `missing evaluation mapping for ${value.caseId}`);
    assert.equal(value.caseVersion, mapping.caseVersion);
    assert.equal(value.capability, mapping.prototypeCapability);
    assert.ok(source.enums.CapabilityId.includes(mapping.capability));
    assert.ok(mapping.expectedErrors.every(code => source.enums.AgentErrorCode.includes(code)));
    assert.equal(value.allowedResults.length, 3);
    assert.deepEqual([value.assertions.operations, value.assertions.assets, value.assertions.batches], value.allowedResults);
    assert.equal(value.assertions.sourcePreserved, true);
    assert.ok(Array.isArray(value.prohibitedSideEffects) && value.prohibitedSideEffects.length >= 2);
    assert.ok(value.prohibitedSideEffects.every(effect => typeof effect === 'string' && effect.length > 0));
    if (added.has(value.caseId)) {
      // The first entry is always a machine-readable token so a future adapter can
      // assert on it; at least one later entry must carry the Chinese policy reason
      // (the fixture documents its own rationale inside this existing field).
      assert.match(value.prohibitedSideEffects[0], /^[a-z][a-z-]*$/);
      assert.ok(value.prohibitedSideEffects.some(effect => /[\u4e00-\u9fff]/.test(effect)), `${value.caseId} needs a Chinese policy reason`);
      assert.ok(Array.isArray(value.requiredConfirmations));
      assert.ok(Array.isArray(value.fakeModelResponses) && value.fakeModelResponses.length > 0);
      assert.equal(typeof value.userUtterance, 'string');
    } else {
      assert.deepEqual(value.allowedResults, expected[value.caseId]);
      assert.deepEqual(value.prohibitedSideEffects, forbidden[value.caseId]);
    }
  }
});

test('tool schemas retain strict fields, scalar limits and registered input/output references', () => {
  const source = JSON.parse(readFileSync(new URL('../docs/contracts/agent-v1.json', import.meta.url), 'utf8'));
  const schema = JSON.parse(readFileSync(new URL('../docs/contracts/agent-v1.schema.json', import.meta.url), 'utf8'));
  for (const [name, fields] of Object.entries(source.records)) {
    const definition = schema.$defs[name];
    assert.equal(definition.additionalProperties, false);
    assert.deepEqual(Object.keys(definition.properties), Object.keys(fields));
    assert.deepEqual(definition.required, Object.entries(fields).filter(([, type]) => !type.endsWith('?')).map(([key]) => key));
  }
  for (const [id, descriptor] of Object.entries(source.capabilities)) {
    assert.equal(schema['x-capabilities'][id].input.$ref, `#/$defs/${descriptor.input}`);
    assert.equal(schema['x-capabilities'][id].output.$ref, `#/$defs/${source.resultTypes[descriptor.input]}`);
  }
  assert.equal(schema.$defs.Instructions['x-maxUtf8Bytes'], source.scalars.Instructions.maxBytes);
  assert.equal(schema.$defs.u32.maximum, 4294967295);
  assert.equal(schema.$defs.LoadWorkflowInput.properties.version.allOf[1].minimum, 1);
  assert.equal(schema.$defs.WorkflowSummary.properties.parameters.maxItems, source.maxWorkflowParameters);
  assert.equal(schema.$defs.WorkflowSummary.properties.steps.maxItems, source.maxWorkflowSteps);
  const tokenPattern = new RegExp(schema.$defs.ResourceRef.pattern);
  assert.equal(tokenPattern.test('resource_0123456789abcdef'), true);
  for (const bad of ['resource_0123456789abcdef\n', '../../outside', 'C:/private/photo.jpg']) assert.equal(tokenPattern.test(bad), false);
});
