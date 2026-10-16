import process from 'node:process';
import { setInterval } from 'node:timers';
import { PrototypeHost } from '../../supervisor.mjs';

const host = new PrototypeHost(process.argv[2]);
await host.ready();
process.stdout.write(JSON.stringify({ pid: host.child.pid, instance: host.instance }) + '\n');
// Deliberately keep the owner alive until its supervising test terminates it.
setInterval(() => {}, 1000);
