import { lstat, mkdir, readdir, realpath } from 'node:fs/promises';
import { basename, join, relative } from 'node:path';

export async function verifyAgentDirectory(directory) {
  let metadata;
  try {
    metadata = await lstat(directory);
  } catch {
    throw new Error('AGENT_DIRECTORY_UNAVAILABLE');
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error('AGENT_DIRECTORY_INVALID');
  }
  try {
    return await realpath(directory);
  } catch {
    throw new Error('AGENT_DIRECTORY_UNAVAILABLE');
  }
}

export async function ensureSessionsDirectory(agentDirectory) {
  const root = await verifyAgentDirectory(agentDirectory);
  const sessionsDirectory = join(root, 'sessions');
  try {
    await mkdir(sessionsDirectory);
  } catch (error) {
    if (error.code !== 'EEXIST') throw new Error('SESSION_DIRECTORY_UNAVAILABLE');
  }

  let metadata;
  try {
    metadata = await lstat(sessionsDirectory);
  } catch {
    throw new Error('SESSION_DIRECTORY_UNAVAILABLE');
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error('SESSION_DIRECTORY_INVALID');
  }
  return sessionsDirectory;
}

export async function validateSessionFiles(sessionsDirectory) {
  let files;
  try {
    files = await readdir(sessionsDirectory);
  } catch {
    throw new Error('SESSION_DIRECTORY_UNAVAILABLE');
  }
  for (const file of files) {
    if (!file.endsWith('.jsonl')) continue;
    let metadata;
    try {
      metadata = await lstat(join(sessionsDirectory, file));
    } catch {
      throw new Error('SESSION_FILE_UNAVAILABLE');
    }
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      throw new Error('SESSION_FILE_INVALID');
    }
  }
}

export async function verifySessionFile(sessionsDirectory, filePath) {
  const expectedDirectory = await realpath(sessionsDirectory).catch(() => {
    throw new Error('SESSION_DIRECTORY_UNAVAILABLE');
  });
  const resolvedFile = await realpath(filePath).catch(() => {
    throw new Error('SESSION_FILE_UNAVAILABLE');
  });
  if (relative(expectedDirectory, resolvedFile) !== basename(filePath)) {
    throw new Error('SESSION_FILE_INVALID');
  }
  let metadata;
  try {
    metadata = await lstat(filePath);
  } catch {
    throw new Error('SESSION_FILE_UNAVAILABLE');
  }
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error('SESSION_FILE_INVALID');
  }
  return resolvedFile;
}
