/** 百宝箱说明页（R09 / A3-05）。
 *
 *  这一页的价值全在「说了什么、没做什么」：2.0 必须让用户看得见并进得来，
 *  同时不能加载任何插件、不能读任意 URL 或可执行文件。之前的覆盖只在
 *  BottomBar 断言「点击入口把 treasure 交给 onNavigate」，那证明的是入口，
 *  不是页面内容，所以这里单独锁住页面文案与出口。
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import TreasurePage from "@/pages/TreasurePage";

describe("TreasurePage 百宝箱说明页", () => {
  it("明确说明 2.0 尚未开放，且不加载插件或可执行扩展", () => {
    render(<TreasurePage />);
    expect(screen.getByRole("heading", { name: "百宝箱尚未开放" })).toBeInTheDocument();
    expect(screen.getByText(/2\.0 版本暂不加载任何插件/)).toBeInTheDocument();
    expect(screen.getByText(/不会从网络或本地读取可执行扩展/)).toBeInTheDocument();
  });

  it("说明隐藏入口不影响功能与数据，指向底栏入口设置", () => {
    render(<TreasurePage />);
    expect(screen.getByText(/隐藏它不会删除任何功能或数据/)).toBeInTheDocument();
  });

  it("提供前往设置的出口，派发 app:navigate=settings", () => {
    const listener = vi.fn();
    window.addEventListener("app:navigate", listener);
    try {
      render(<TreasurePage />);
      fireEvent.click(screen.getByRole("button", { name: "前往设置" }));
      expect(listener).toHaveBeenCalledTimes(1);
      expect((listener.mock.calls[0][0] as CustomEvent).detail).toBe("settings");
    } finally {
      window.removeEventListener("app:navigate", listener);
    }
  });

  it("本地不渲染任何 iframe / script / 外部链接（2.0 不留扩展点）", () => {
    const { container } = render(<TreasurePage />);
    expect(container.querySelector("iframe")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("a[href]")).toBeNull();
  });
});
