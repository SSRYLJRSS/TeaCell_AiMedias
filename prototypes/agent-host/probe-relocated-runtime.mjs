// Run exclusively from the copied resource layout; no user assets or services.
import process from 'node:process';
import { readFile, mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { createPrototypeSession } from './sdk-session.mjs';

const resources = fileURLToPath(new URL('.', import.meta.url));
if (process.platform !== 'win32' || process.arch !== 'x64' || process.version !== 'v24.19.0') throw new Error('UNSUPPORTED_PROBE_RUNTIME');
const expectedExecutable = resolve(resources, '../runtime/node-v24.19.0-win-x64/node.exe');
if (resolve(process.execPath).toLowerCase() !== expectedExecutable.toLowerCase()) throw new Error('NOT_PACKAGED_RUNTIME');
if (process.env.PATH !== '') throw new Error('AMBIENT_PATH');
const data = resolve(resources, '../probe-data');
await mkdir(data, { recursive: true });
const session = await createPrototypeSession(data);
try {
  await session.prompt('独立资源目录 SDK 验证');
  if (session.agent.state.messages.at(-1).content[0].text !== '原型流式回复' || session.getActiveToolNames().length) throw new Error('SDK_PROBE_FAILED');
  const { processImage } = await import(new URL('./node_modules/@earendil-works/pi-coding-agent/dist/utils/image-process.js', import.meta.url));
  const png = await readFile(join(resources, 'probe-preview.png'));
  const image = await processImage(png, 'image/png', { autoResizeImages: true });
  if (!image.ok || image.mimeType !== 'image/png' || !image.data) throw new Error('IMAGE_ASSET_FAILED');
  const require = createRequire(import.meta.url);
  const native = require('./node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-tui/native/win32/prebuilds/win32-x64/win32-platform.node');
  process.stdout.write(`${JSON.stringify({ ok: true, runtime: process.version, executable: process.execPath, sdk: '1.0.4', imageBytes: png.length,
    imageSha256: createHash('sha256').update(png).digest('hex'), nativeExports: Object.keys(native).sort(), tools: session.getActiveToolNames() })}\n`);
} finally { session.dispose(); }
