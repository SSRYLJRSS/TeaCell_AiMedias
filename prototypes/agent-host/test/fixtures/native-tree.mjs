import process from 'node:process';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { setInterval } from 'node:timers';

const input = createInterface({ input: process.stdin });
input.once('line', line => {
  if (line !== 'start') process.exit(2);
  const descendant = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
    windowsHide: true, stdio: 'ignore',
  });
  descendant.once('spawn', () => {
    process.stdout.write(JSON.stringify({ child: process.pid, descendant: descendant.pid }) + '\n');
  });
  setInterval(() => {}, 1000);
});
