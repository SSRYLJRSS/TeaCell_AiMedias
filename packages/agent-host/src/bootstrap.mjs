import process from 'node:process';
import { setTimeout as delay, clearTimeout } from 'node:timers';

// The Rust owner assigns the child to its OS process group before releasing this barrier.
async function waitForOwner() {
  return new Promise((resolve, reject) => {
    const finish = error => {
      clearTimeout(timer);
      process.stdin.off('readable', readable);
      process.stdin.off('end', ended);
      process.stdin.off('error', onError);
      process.stdin.pause();
      if (error) reject(error); else resolve();
    };
    const onError = () => finish(new Error('OWNER_DISCONNECTED'));
    const readable = () => {
      const token = process.stdin.read(6);
      if (token !== null) finish(token.toString('ascii') === 'start\n' ? undefined : new Error('START_DENIED'));
    };
    const ended = () => finish(new Error('OWNER_DISCONNECTED'));
    const timer = delay(() => finish(new Error('START_TIMEOUT')), 5000);
    process.stdin.on('readable', readable);
    process.stdin.once('end', ended);
    process.stdin.once('error', onError);
    readable();
  });
}

try {
  const [agentDir, hostInstanceId, appBuild, recoveryMode] = process.argv.slice(2);
  if (!agentDir || !hostInstanceId || !appBuild || !['fresh', 'recover-stranded'].includes(recoveryMode)) {
    throw new Error('INVALID_START_ARGUMENTS');
  }
  await waitForOwner();
  const { runHost } = await import('./host.mjs');
  const exitCode = await runHost({ agentDir, hostInstanceId, appBuild, recoveryMode });
  if (exitCode !== 0) throw new Error('HOST_START_REJECTED');
} catch (error) {
  // Never serialize exception text: it may contain user input or provider details.
  const code = typeof error?.message === 'string' && /^[A-Z0-9_]{1,48}$/u.test(error.message)
    ? error.message
    : 'BOOTSTRAP_FAILED';
  process.stderr.write(`agent_host_start_failed error_code=${code}\n`);
  process.exitCode = 2;
  process.stdin.destroy();
}
