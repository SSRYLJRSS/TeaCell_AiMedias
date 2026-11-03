/** 基础模态的键盘焦点闭环（M-14）。
 *
 *  M-14 原文点名「Esc 可退出浮层且**焦点归还**」「焦点顺序合理、危险按钮不是默认焦点」。
 *  `docs/UI_DESIGN_SYSTEM.md` §9 也写着「Dialog 打开后焦点进入，关闭后返回触发点」。
 *  Modal 是全仓唯一的基础模态（18 处业务弹窗都基于它），所以这三件事在这里测一次
 *  就等于覆盖了全部弹窗。
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import Modal from "@/components/common/Modal";
import Button from "@/components/common/Button";

function renderDialog(props: { open: boolean; onClose: () => void; autoFocusInput?: boolean }) {
  const { open, onClose, autoFocusInput = false } = props;
  return render(
    <>
      <button type="button" data-testid="opener">
        打开
      </button>
      <Modal
        open={open}
        title="删除 2 项素材"
        onClose={onClose}
        footer={
          <>
            <Button onClick={onClose}>取消</Button>
            <Button variant="danger" onClick={() => undefined}>
              确认删除
            </Button>
          </>
        }
      >
        <p>正文</p>
        <input aria-label="备注" autoFocus={autoFocusInput} />
      </Modal>
    </>,
  );
}

describe("Modal 键盘焦点闭环（M-14）", () => {
  it("打开后焦点进入弹窗，并落在容器上而不是危险按钮上", () => {
    const { rerender } = render(<button type="button">打开</button>);
    // 模拟「用户用键盘/鼠标点了触发按钮」：真实场景里 activeElement 就是它。
    const opener = screen.getByRole("button", { name: "打开" });
    opener.focus();
    expect(document.activeElement).toBe(opener);

    rerender(
      <>
        <button type="button">打开</button>
        <Modal open title="删除 2 项素材" onClose={() => undefined} footer={<Button variant="danger">确认删除</Button>}>
          <p>正文</p>
        </Modal>
      </>,
    );

    const dialog = screen.getByRole("dialog");
    // 焦点必须进弹窗：留在底层页面上，键盘用户按 Tab 会走到看不见的地方。
    expect(document.activeElement).toBe(dialog);
    // 危险按钮不能是默认焦点：M-14 明确要求。
    expect(document.activeElement).not.toBe(screen.getByRole("button", { name: "确认删除" }));
  });

  it("关闭后焦点归还给打开它的触发按钮，而不是掉到 body", () => {
    const onClose = vi.fn();
    const { rerender } = renderDialog({ open: false, onClose });

    const opener = screen.getByTestId("opener");
    opener.focus();

    rerender(
      <>
        <button type="button" data-testid="opener">
          打开
        </button>
        <Modal open title="删除 2 项素材" onClose={onClose} footer={<Button onClick={onClose}>取消</Button>}>
          <p>正文</p>
        </Modal>
      </>,
    );
    expect(document.activeElement).toBe(screen.getByRole("dialog"));

    // Esc 关闭（Modal 的 Esc 走 window 捕获阶段，不依赖弹窗内部焦点）
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);

    rerender(
      <>
        <button type="button" data-testid="opener">
          打开
        </button>
        <Modal open={false} title="删除 2 项素材" onClose={onClose} footer={<Button onClick={onClose}>取消</Button>}>
          <p>正文</p>
        </Modal>
      </>,
    );

    // 焦点必须回到触发点：否则键盘用户失去位置，只能从头 Tab 一遍。
    expect(document.activeElement).toBe(screen.getByTestId("opener"));
  });

  it("Tab 在弹窗内循环，不会跑到底层页面（Shift+Tab 反向同理）", () => {
    renderDialog({ open: true, onClose: () => undefined });

    const dialog = screen.getByRole("dialog");
    // DOM 顺序：正文里的输入框在 footer 按钮之前。
    const first = screen.getByLabelText("备注");
    const last = screen.getByRole("button", { name: "确认删除" });

    // 正序：焦点在最后一个可聚焦元素时再 Tab 应折回第一个
    last.focus();
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(first);

    // 逆序：焦点在第一个时 Shift+Tab 应折到最后一个
    first.focus();
    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("弹窗内部已有 autoFocus 元素时不抢走它的焦点", () => {
    renderDialog({ open: true, onClose: () => undefined, autoFocusInput: true });

    // 输入框自己声明了 autoFocus（如标签管理的新建输入），那是有意的输入起点，
    // 把它抢到容器上会让用户多点一次。
    expect(document.activeElement).toBe(screen.getByLabelText("备注"));
  });

  // P0 回归：960×640 的最小窗口下，长内容弹窗（迁移向导/导出失败清单）曾把底部按钮
  // 顶出视口且正文不可滚动，用户只能强行关窗口。
  it("限高并在正文内部滚动，页头页脚不被顶出视口", () => {
    renderDialog({ open: true, onClose: () => undefined });
    const dialog = screen.getByRole("dialog");
    expect(dialog.className).toContain("flex-col");
    expect(dialog.className).toContain("max-h-[calc(100vh-2rem)]");
    const body = screen.getByTestId("modal-body");
    expect(body.className).toContain("overflow-y-auto");
    expect(body.className).toContain("min-h-0");
    const footer = screen.getByRole("button", { name: "确认删除" }).parentElement;
    expect(footer?.className).toContain("shrink-0");
  });
});
