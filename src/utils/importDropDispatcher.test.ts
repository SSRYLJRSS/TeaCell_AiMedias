import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ onDragDropEvent: vi.fn() }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: mocks.onDragDropEvent }),
}));

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  mocks.onDragDropEvent.mockResolvedValue(() => undefined);
});

describe("importDropDispatcher", () => {
  it("installs one native listener and routes only to the currently mounted page", async () => {
    const dispatcher = await import("@/utils/importDropDispatcher");
    const first = vi.fn();
    const second = vi.fn();
    const unlisten = vi.fn();
    mocks.onDragDropEvent.mockResolvedValueOnce(unlisten);
    await dispatcher.startImportDropDispatcher();
    const nativeListener = mocks.onDragDropEvent.mock.calls[0][0] as (event: unknown) => void;
    const unregisterFirst = dispatcher.registerImportDropHandler(first);
    const unregisterSecond = dispatcher.registerImportDropHandler(second);

    await dispatcher.startImportDropDispatcher();
    nativeListener({ payload: { type: "drop", paths: ["D:/one.jpg"] } });
    expect(mocks.onDragDropEvent).toHaveBeenCalledTimes(1);
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledWith({ type: "drop", paths: ["D:/one.jpg"] });

    unregisterFirst();
    nativeListener({ payload: { type: "drop", paths: ["D:/two.jpg"] } });
    expect(second).toHaveBeenCalledTimes(2);
    unregisterSecond();
    nativeListener({ payload: { type: "drop", paths: ["D:/three.jpg"] } });
    expect(second).toHaveBeenCalledTimes(2);
    dispatcher.stopImportDropDispatcher();
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalledTimes(1);

    await dispatcher.startImportDropDispatcher();
    expect(mocks.onDragDropEvent).toHaveBeenCalledTimes(2);
  });

  it("turns synchronous WebView lookup failures into retryable rejected promises", async () => {
    const dispatcher = await import("@/utils/importDropDispatcher");
    mocks.onDragDropEvent.mockImplementationOnce(() => { throw new Error("WebView unavailable"); });

    await expect(dispatcher.startImportDropDispatcher()).rejects.toThrow("WebView unavailable");
    await expect(dispatcher.startImportDropDispatcher()).resolves.toBeUndefined();
    expect(mocks.onDragDropEvent).toHaveBeenCalledTimes(2);
  });
});
