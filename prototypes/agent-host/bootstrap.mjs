import process from 'node:process';
import { setTimeout, clearTimeout } from 'node:timers';

// No SDK or third-party import before the owner establishes process ownership.
async function waitForOwner() {
  return new Promise((resolve, reject) => {
    const finish = error => {
      clearTimeout(timer);
      process.stdin.off('readable', readable);
      process.stdin.off('end', ended);
      process.stdin.off('error', finish);
      process.stdin.pause();
      if (error) reject(error); else resolve();
    };
    const readable = () => {
      const token = process.stdin.read(6);
      if (token !== null) finish(token.toString('ascii') === 'start\n' ? undefined : new Error('START_DENIED'));
    };
    const ended = () => finish(new Error('OWNER_DISCONNECTED'));
    const timer = setTimeout(() => finish(new Error('START_TIMEOUT')), 5000);
    process.stdin.on('readable', readable);
    process.stdin.once('end', ended);
    process.stdin.once('error', finish);
    readable();
  });
}

try {
  await waitForOwner();
  await import('./host.mjs');
} catch {
  process.exitCode = 2;
  process.stdin.destroy();
}
