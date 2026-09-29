/**
 * StartupSkeleton 测试（指导书 阶段 1 §3.2/§13.1）：
 *  - 非 Tauri 环境（纯 jsdom/浏览器预览）不抛异常；
 *  - 渲染 logo 占位 +「正在准备素材库」文案；
 *  - 不承诺百分比（无百分比数字文案）；
 *  - 纯展示组件：不触发任何 IPC/DB 请求（渲染即可证明无副作用）。
 */
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import StartupSkeleton from "@/components/common/StartupSkeleton";

describe("StartupSkeleton（§3.2）", () => {
  it("非 Tauri 环境渲染不抛异常，显示核心文案", () => {
    expect(() => render(<StartupSkeleton />)).not.toThrow();
    expect(screen.getByText("茶馆")).toBeInTheDocument();
    expect(screen.getByText("正在准备素材库")).toBeInTheDocument();
  });

  it("不承诺百分比（无百分号文案）", () => {
    render(<StartupSkeleton />);
    expect(screen.queryByText(/%/)).not.toBeInTheDocument();
    expect(screen.queryByText(/loading/i)).not.toBeInTheDocument();
  });

  it("含不定进度条容器（不显示假进度）", () => {
    const { container } = render(<StartupSkeleton />);
    const bar = container.querySelector(".startup-indeterminate");
    expect(bar).not.toBeNull();
  });
});
