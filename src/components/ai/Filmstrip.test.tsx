import { describe, expect, it, vi } from "vitest";
import {  } from "@testing-library/react";
import Filmstrip from "@/components/ai/Filmstrip";
import type { AiSuggestion } from "@/types/ai";

import { actRender } from "@/test/render";
// ── jsdom 补齐：虚拟滚动需要尺寸 + ResizeObserver ──
class MockResizeObserver {
  cb: ResizeObserverCallback;
  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
  }
  observe() {
    this.cb(
      [{ contentRect: { width: 800, height: 600 } } as ResizeObserverEntry],
      this as unknown as ResizeObserver,
    );
  }
  unobserve() {}
  disconnect() {}
}
vi.stubGlobal("ResizeObserver", MockResizeObserver);
Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, value: 800 });
Object.defineProperty(HTMLElement.prototype, "offsetWidth", { configurable: true, value: 800 });

vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue("asset://thumb.webp"),
  toFileUrl: (p: string) => `asset://${p}`,
}));

function mk(i: number): AiSuggestion {
  return {
    id: i,
    batchId: 1,
    assetId: i,
    assetPath: `d:/p/${i}.jpg`,
    mimeType: "image/jpeg",
    suggestedTags: {},
    status: "pending",
    confirmedTags: {},
    lastError: null,
    createdAt: 0,
  };
}

describe("Filmstrip（§8.4 虚拟化）", () => {
  it("渲染胶片条；数千素材不创建常驻重型 DOM（按钮数远小于总数）", async () => {
    const suggestions = Array.from({ length: 1000 }, (_, i) => mk(i));
    const { container } = await actRender(
      <Filmstrip suggestions={suggestions} currentId={1} selectedIds={new Set()} onPick={() => {}} />,
    );
    const buttons = container.querySelectorAll("button");
    expect(buttons.length).toBeGreaterThan(0);
    expect(buttons.length).toBeLessThan(100);
  });

  it("空批次返回空", async () => {
    const { container } = await actRender(
      <Filmstrip suggestions={[]} currentId={null} selectedIds={new Set()} onPick={() => {}} />,
    );
    expect(container.firstChild).toBeNull();
  });
});
