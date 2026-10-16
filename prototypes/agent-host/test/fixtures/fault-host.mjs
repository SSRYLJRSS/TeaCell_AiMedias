import process from 'node:process';
import { setInterval } from 'node:timers';
import { Buffer } from 'node:buffer';

const [, instance, scenario] = process.argv.slice(2);
process.stdin.resume();
const frame = { protocol: 1, instance, sequence: 1, type: 'ready', sdk: '1.0.4', tools: [] };
if (scenario === 'sdk') frame.sdk = '1.1.0';
if (scenario === 'instance') frame.instance = 'former-instance';
if (scenario === 'protocol') frame.protocol = 2;
if (scenario === 'pollution') process.stdout.write('unexpected log\n');
else if (scenario === 'flood') process.stdout.write(Buffer.alloc(65537, 65));
else if (scenario !== 'silent') {
  process.stdout.write(JSON.stringify(frame) + '\n');
  if (scenario === 'duplicate') process.stdout.write(JSON.stringify(frame) + '\n');
}
setInterval(() => {}, 1000);
