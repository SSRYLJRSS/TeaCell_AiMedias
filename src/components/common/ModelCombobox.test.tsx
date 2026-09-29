/**
 * ModelCombobox 测试（FB5-04 §3.6/§13.5）：
 *  - 点击「读取模型列表」显示模型；选择写入 value；
 *  - 手动输入始终可用；当前自定义值不被返回列表清空并标记「自定义」；
 *  - 旧请求不覆盖新结果（代际守卫）；获取失败仍可手填；
 *  - RefreshCw 图标 + tooltip；列表最大高度 240px。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import ModelCombobox from "@/components/common/ModelCombobox";

afterEach(() => {
  vi.restoreAllMocks();
});

const noop = () => undefined;

describe("ModelCombobox（FB5-04 §3.6）", () => {
  it("点击读取模型列表 → 显示模型；选择模型写入 onChange", async () => {
    const onChange = vi.fn();
    const discover = vi.fn().mockResolvedValue(["gpt-4.1-mini", "qwen-vl-max", "claude-sonnet-4"]);
    render(<ModelCombobox value="" onChange={onChange} onDiscover={discover} />);
    const input = screen.getByRole("combobox", { name: "模型" }) as HTMLInputElement;
    // 手动输入始终可用
    fireEvent.change(input, { target: { value: "my-custom" } });
    expect(onChange).toHaveBeenCalledWith("my-custom");

    // 读取列表
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    await waitFor(() => expect(screen.getByRole("listbox", { name: "模型列表" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("option", { name: "qwen-vl-max" }));
    expect(onChange).toHaveBeenCalledWith("qwen-vl-max");
  });

  it("手填值不在列表中：保留并标记「自定义」，不被返回列表清空", async () => {
    const onChange = vi.fn();
    const discover = vi.fn().mockResolvedValue(["gpt-4.1-mini"]);
    const { rerender } = render(<ModelCombobox value="my-model" onChange={onChange} onDiscover={discover} />);
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    await waitFor(() => expect(screen.getByRole("listbox", { name: "模型列表" })).toBeInTheDocument());
    // 自定义项存在
    const custom = screen.getByRole("option", { name: "自定义：my-model" });
    expect(custom).toBeInTheDocument();
    // 选择自定义项 → 写回原值（不覆盖）
    fireEvent.click(custom);
    expect(onChange).toHaveBeenCalledWith("my-model");
    // 列表返回后 value 未被清空
    rerender(<ModelCombobox value="my-model" onChange={onChange} onDiscover={discover} />);
    expect((screen.getByRole("combobox", { name: "模型" }) as HTMLInputElement).value).toBe("my-model");
  });

  it("旧请求不覆盖新结果（两次发现，后发先至/先发后至都被代际守卫）", async () => {
    let resolveOld!: (v: string[]) => void;
    const oldPromise = new Promise<string[]>((r) => {
      resolveOld = r;
    });
    const discover = vi
      .fn()
      .mockReturnValueOnce(oldPromise) // 第一次（旧 baseUrl）：挂起
      .mockResolvedValueOnce(["new-model"]); // 第二次（新 baseUrl）：立即返回
    render(<ModelCombobox value="" onChange={noop} onDiscover={discover} />);
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    // 换 baseUrl 后再点一次（触发第二次发现）
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    await waitFor(() => expect(screen.getByRole("option", { name: "new-model" })).toBeInTheDocument());
    // 旧的挂起请求此刻才返回：不得覆盖新列表
    resolveOld(["stale-model"]);
    await waitFor(() => {
      expect(screen.queryByRole("option", { name: "stale-model" })).not.toBeInTheDocument();
    });
    expect(screen.getByRole("option", { name: "new-model" })).toBeInTheDocument();
  });

  it("连接来源变化时清空旧列表并忽略旧连接的在途响应", async () => {
    let resolveOld!: (v: string[]) => void;
    const oldPromise = new Promise<string[]>((resolve) => {
      resolveOld = resolve;
    });
    const firstDiscover = vi.fn().mockReturnValue(oldPromise);
    const nextDiscover = vi.fn().mockResolvedValue(["model-for-new-provider"]);
    const props = { value: "", onChange: noop };
    const { rerender } = render(
      <ModelCombobox {...props} sourceRevision={1} onDiscover={firstDiscover} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));

    rerender(<ModelCombobox {...props} sourceRevision={2} onDiscover={nextDiscover} />);
    expect(screen.queryByRole("listbox", { name: "模型列表" })).not.toBeInTheDocument();
    resolveOld(["stale-provider-model"]);
    await waitFor(() => expect(screen.queryByText(/stale-provider-model/)).not.toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    await waitFor(() =>
      expect(screen.getByRole("option", { name: "model-for-new-provider" })).toBeInTheDocument(),
    );
  });

  it("远端发现被禁用时仍允许手动输入模型名称", () => {
    const onChange = vi.fn();
    render(
      <ModelCombobox value="" onChange={onChange} onDiscover={vi.fn()} discoverDisabled />,
    );
    const input = screen.getByRole("combobox", { name: "模型" });
    expect(input).toBeEnabled();
    expect(screen.getByRole("button", { name: "读取模型列表" })).toBeDisabled();
    fireEvent.change(input, { target: { value: "manual-model" } });
    expect(onChange).toHaveBeenCalledWith("manual-model");
  });

  it("获取失败显示可读原因，输入框仍可手填", async () => {
    const onChange = vi.fn();
    const discover = vi.fn().mockRejectedValue(new Error("该服务未提供模型列表，请手动输入"));
    render(<ModelCombobox value="" onChange={onChange} onDiscover={discover} />);
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    await waitFor(() => expect(screen.getByText("该服务未提供模型列表，请手动输入")).toBeInTheDocument());
    // 输入框仍可用
    const input = screen.getByRole("combobox", { name: "模型" }) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "qwen-vl-plus" } });
    expect(onChange).toHaveBeenCalledWith("qwen-vl-plus");
  });

  it("刷新按钮使用 RefreshCw 图标 + tooltip；列表最大高度 240px", async () => {
    const discover = vi.fn().mockResolvedValue(["a", "b", "c"]);
    const { container } = render(<ModelCombobox value="" onChange={noop} onDiscover={discover} />);
    const refreshBtn = screen.getByRole("button", { name: "读取模型列表" });
    // tooltip title
    expect(refreshBtn.getAttribute("title")).toBe("读取模型列表");
    expect(container.querySelector(".lucide-refresh-cw")).not.toBeNull();
    fireEvent.click(refreshBtn);
    await waitFor(() => expect(screen.getByRole("listbox", { name: "模型列表" })).toBeInTheDocument());
    // 已有列表后按钮变为「刷新模型列表」
    expect(screen.getByRole("button", { name: "刷新模型列表" })).toBeInTheDocument();
    const listbox = screen.getByRole("listbox", { name: "模型列表" }) as HTMLElement;
    expect(listbox.style.maxHeight).toBe("240px");
  });

  it("键盘：↓ 展开/导航，Enter 选中，Esc 关闭", async () => {
    const onChange = vi.fn();
    const discover = vi.fn().mockResolvedValue(["m1", "m2"]);
    render(<ModelCombobox value="" onChange={onChange} onDiscover={discover} />);
    const input = screen.getByRole("combobox", { name: "模型" }) as HTMLInputElement;
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    await waitFor(() => expect(screen.getByRole("listbox", { name: "模型列表" })).toBeInTheDocument());
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("m2");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByRole("listbox", { name: "模型列表" })).not.toBeInTheDocument();
  });
});
