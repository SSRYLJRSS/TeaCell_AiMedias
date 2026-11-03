/**
 * API 封装层统一入口（架构 §3.3）：
 * - 页面禁止直接 invoke，统一走 src/api/*；
 * - 后端 AppError { code, message } → 前端 AppError；
 * - on() 订阅后端进度事件，返回取消订阅函数。
 */
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen, type Event, type UnlistenFn } from "@tauri-apps/api/event";
import { createLogCorrelationId, logger } from "@/utils/logger";

export class AppError extends Error {
  public readonly cause?: string;

  constructor(
    public code: string,
    message: string,
    cause?: string,
  ) {
    super(message);
    this.name = "AppError";
    this.cause = cause;
  }
}

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const startedAt = performance.now();
  const requestId = createLogCorrelationId(cmd);
  try {
    return await tauriInvoke<T>(cmd, args);
  } catch (e) {
    const err = e as { code?: string; message?: string; cause?: string } | undefined;
    const appError = new AppError(err?.code ?? "UNKNOWN", err?.message ?? String(e), err?.cause);
    logger.error(`invoke failed: ${cmd} [${appError.code}] ${appError.message}`, {
      requestId,
      command: cmd,
      durationMs: Math.round(performance.now() - startedAt),
      cause: appError.cause,
    });
    throw appError;
  }
}

/** 事件订阅：on<T>("import://progress", handler) → 返回 unsubscribe */
export function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e: Event<T>) => handler(e.payload));
}

/** 供各 api 模块标注取消订阅函数类型，避免直接依赖 @tauri-apps/api。 */
export type { UnlistenFn };
