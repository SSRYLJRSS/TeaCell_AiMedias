import { spawn } from 'node:child_process';
import { Buffer } from 'node:buffer';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import { childEnvironment } from './supervisor.mjs';
import { FrameDecoder } from './protocol.mjs';
import { setTimeout, clearTimeout } from 'node:timers';

const executable = fileURLToPath(new URL(`../operation-recovery/target/debug/workflow_fixture${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));

export function invokeWorkflowFixture(context, payload, { admin = false } = {}) {
  return new Promise((resolve, reject) => {
    const input = Buffer.from(JSON.stringify(payload));
    if (input.length > 65536) { reject(new Error('FRAME_LIMIT')); return; }
    const child = spawn(executable, [admin ? 'admin' : 'agent', context.db, context.source], {
      env: childEnvironment(process.env), windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
    });
    const decoder = new FrameDecoder(); const frames = [];
    const deadline = setTimeout(() => { child.kill(); reject(new Error('WORKFLOW_TIMEOUT')); }, 5000);
    child.once('error', reject);
    child.stdin.on('error', reject);
    child.stdout.on('data', chunk => {
      try {
        frames.push(...decoder.push(chunk));
        if (frames.length > 1) throw new Error('UNEXPECTED_FRAME');
      } catch (error) { child.kill(); reject(error); }
    });
    child.stderr.resume();
    child.once('close', code => {
      clearTimeout(deadline);
      try {
        decoder.end();
        const frame = frames[0];
        if (!frame) throw new Error('MISSING_FRAME');
        if (code !== 0 || frame.ok !== true) reject(new Error(frame.error ?? 'WORKFLOW_FIXTURE_FAILED'));
        else resolve(frame.data);
      } catch { reject(new Error('WORKFLOW_FIXTURE_FAILED')); }
    });
    child.stdin.end(input);
  });
}

const identity = { id: { type: 'string', minLength: 1, maxLength: 64 }, version: { type: 'integer', minimum: 1 } };
function toolResult(data) { return { content: [{ type: 'text', text: JSON.stringify(data) }], details: data }; }
export function workflowFixtureTools(context) {
  return [{
    name: 'load_personal_workflow', label: '加载个人流程',
    description: '按指定ID和不可变版本读取个人流程数据，不授予任何执行权限。',
    parameters: { type: 'object', properties: identity, required: ['id', 'version'], additionalProperties: false },
    execute: async (_callId, args) => toolResult(await invokeWorkflowFixture(context, { action: 'load', id: args.id, version: args.version })),
  }, {
    name: 'prepare_personal_workflow', label: '准备个人流程计划',
    description: '使用宿主当前授权的选择和目录产生新的待审核计划；不执行素材修改。',
    parameters: { type: 'object', properties: { ...identity, parameters: {
      type: 'object', properties: { collection: { type: 'string' }, tagging_instructions: { type: 'string' }, rename: { type: 'boolean' } },
      required: ['collection', 'tagging_instructions', 'rename'], additionalProperties: false,
    } }, required: ['id', 'version', 'parameters'], additionalProperties: false },
    execute: async (_callId, args) => toolResult(await invokeWorkflowFixture(context, {
      action: 'prepare', id: args.id, version: args.version, parameters: args.parameters,
      input_names: context.inputNames,
    })),
  }];
}
