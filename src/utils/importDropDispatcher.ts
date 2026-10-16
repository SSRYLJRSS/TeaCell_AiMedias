import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";

type ImportDropPayload = { type: "over" | "leave" } | { type: "drop"; paths: string[] };
type ImportDropHandler = (payload: ImportDropPayload) => void;

let activeHandler: ImportDropHandler | null = null;
let dispatcher: Promise<UnlistenFn | null> | null = null;
let dispatcherGeneration = 0;

/** Install one app-level listener and release it when the app root unmounts. */
export function startImportDropDispatcher(): Promise<void> {
  if (!dispatcher) {
    const generation = dispatcherGeneration;
    const startup = Promise.resolve()
      .then(() => getCurrentWebview()
        .onDragDropEvent((event) => activeHandler?.(event.payload as ImportDropPayload)))
      .then((unlisten) => {
        if (generation !== dispatcherGeneration) {
          unlisten();
          return null;
        }
        return unlisten;
      })
      .catch((error: unknown) => {
        if (dispatcher === startup) dispatcher = null;
        throw error;
      });
    dispatcher = startup;
  }
  return dispatcher.then(() => undefined);
}

/** Stop stale WebView listeners, including one whose registration is still pending. */
export function stopImportDropDispatcher(): void {
  dispatcherGeneration += 1;
  activeHandler = null;
  const current = dispatcher;
  dispatcher = null;
  if (current) void current.then((unlisten) => unlisten?.()).catch(() => undefined);
}

/** The mounted import page is the only active recipient; remounting replaces the callback. */
export function registerImportDropHandler(handler: ImportDropHandler): () => void {
  activeHandler = handler;
  return () => {
    if (activeHandler === handler) activeHandler = null;
  };
}
