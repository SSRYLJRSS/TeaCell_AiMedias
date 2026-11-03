/**
 * Diagnostics for the stdio bridge.
 *
 * stdout belongs to the MCP transport, so every diagnostic goes to stderr. The
 * contract also forbids secrets and file paths in logs, so anything written here is
 * scrubbed: the pipe key, tokens, connection ids and filesystem paths never survive.
 */

const SENSITIVE_FIELD = /^(key|token|secret|password|authorization|endpoint|endpointpath|path|paths|pipelinename|pipename|connectionid|principalid)$/iu;

const TOKEN_PATTERN = /\b[0-9a-fA-F]{64}\b/gu;
const B64URL_PATTERN = /\b[A-Za-z0-9_-]{43,}={0,2}/gu;
const WINDOWS_PATH_PATTERN = /\b[A-Za-z]:[\\/][^\s"'`)\]]*/gu;
const POSIX_PATH_PATTERN = /(?:^|[\s"'(=])((?:\/[^\s"'`)\]]+){2,})/gu;

/** Removes secrets and paths from one line of text. */
export function scrub(text) {
  return String(text)
    .replace(TOKEN_PATTERN, '[redacted-token]')
    .replace(B64URL_PATTERN, '[redacted]')
    .replace(WINDOWS_PATH_PATTERN, '[redacted-path]')
    .replace(POSIX_PATH_PATTERN, (match, path) => match.replace(path, '[redacted-path]'));
}

function scrubValue(value) {
  if (value === null || value === undefined) return value;
  if (typeof value === 'string') return scrub(value);
  if (typeof value === 'number' || typeof value === 'boolean') return value;
  return '[redacted]';
}

function formatFields(fields) {
  if (!fields || typeof fields !== 'object') return '';
  const parts = [];
  for (const [key, value] of Object.entries(fields)) {
    if (SENSITIVE_FIELD.test(key)) {
      parts.push(`${key}=[redacted]`);
      continue;
    }
    if (value === undefined) continue;
    const safe = scrubValue(value);
    parts.push(`${key}=${typeof safe === 'string' ? safe : String(safe)}`);
  }
  return parts.length > 0 ? ` ${parts.join(' ')}` : '';
}

export const LOG_LEVELS = Object.freeze({ error: 0, warn: 1, info: 2, debug: 3 });

/**
 * @param {{ write?: (line: string) => void, level?: keyof LOG_LEVELS, name?: string }} [options]
 */
export function createLogger(options = {}) {
  const write = typeof options.write === 'function'
    ? options.write
    : (line) => {
        try {
          process.stderr.write(line);
        } catch {
          // stderr can be a closed pipe when the parent is gone; diagnostics are optional.
        }
      };
  const threshold = LOG_LEVELS[options.level ?? 'info'] ?? LOG_LEVELS.info;
  const name = options.name ?? 'mcp-bridge';

  const emit = (level, message, fields) => {
    if (LOG_LEVELS[level] > threshold) return;
    write(`[${name}] ${level} ${scrub(message)}${formatFields(fields)}\n`);
  };

  return {
    error: (message, fields) => emit('error', message, fields),
    warn: (message, fields) => emit('warn', message, fields),
    info: (message, fields) => emit('info', message, fields),
    debug: (message, fields) => emit('debug', message, fields),
  };
}

/** A logger that drops everything; handy as a default injection point. */
export const silentLogger = Object.freeze({
  error: () => {},
  warn: () => {},
  info: () => {},
  debug: () => {},
});
