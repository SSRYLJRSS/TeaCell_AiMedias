import { spawn } from 'node:child_process';
import { clearTimeout, setTimeout } from 'node:timers';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import process from 'node:process';

const directory = path.dirname(fileURLToPath(import.meta.url));
const repository = path.resolve(directory, '../..');
const serverPath = path.join(repository, 'prototypes/assistant-ui/serve.mjs');
const manifestPath = path.join(directory, 'Cargo.toml');
const port = 14321;

const server = spawn(process.execPath, [serverPath, '--port', String(port)], {
  cwd: repository,
  stdio: ['ignore', 'pipe', 'inherit'],
});
let serverOutput = '';
let shuttingDown = false;

function stopServer() {
  if (!shuttingDown) {
    shuttingDown = true;
    server.kill();
  }
}

const ready = new Promise((resolve, reject) => {
  const timeout = setTimeout(() => reject(new Error('prototype server did not start within 10 seconds')), 10_000);
  server.stdout.on('data', (chunk) => {
    serverOutput += chunk.toString();
    process.stdout.write(chunk);
    if (serverOutput.includes(`http://127.0.0.1:${port}/`)) {
      clearTimeout(timeout);
      resolve();
    }
  });
  server.once('error', (error) => {
    clearTimeout(timeout);
    reject(error);
  });
  server.once('exit', (code) => {
    if (!shuttingDown) {
      clearTimeout(timeout);
      reject(new Error(`prototype server exited before becoming ready (${code})`));
    }
  });
});

let app;
try {
  await ready;
  app = spawn(process.env.CARGO ?? 'cargo', ['run', '--locked', '--manifest-path', manifestPath], {
    cwd: repository,
    stdio: 'inherit',
    env: process.env,
  });
  const exitCode = await new Promise((resolve, reject) => {
    app.once('error', reject);
    app.once('exit', (code, signal) => resolve(code ?? (signal ? 1 : 0)));
  });
  process.exitCode = exitCode;
} finally {
  stopServer();
}
