/**
 * DirectoryGrantsPanel 测试（A3-06）。
 *
 * 验收口径：
 * - 只展示「记住的」目录授权，并明确说出授权范围（导入来源只有读取）；
 * - 撤销后必须刷新列表并提示「下一次会重新询问」，不能在前端假装成功；
 * - 目录已不可用时如实标注，不隐藏也不能撤销失败；
 * - 撤销全部需要二次确认，且不能删除素材。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import DirectoryGrantsPanel from "@/components/settings/DirectoryGrantsPanel";
import type { DirectoryGrant } from "@/api/authorization";

const mocks = vi.hoisted(() => ({
  listDirectoryGrants: vi.fn(),
  revokeDirectoryGrant: vi.fn(),
  revokeAllDirectoryGrants: vi.fn(),
}));

vi.mock("@/api/authorization", () => ({
  listDirectoryGrants: mocks.listDirectoryGrants,
  revokeDirectoryGrant: mocks.revokeDirectoryGrant,
  revokeAllDirectoryGrants: mocks.revokeAllDirectoryGrants,
}));

const importGrant: DirectoryGrant = {
  grantRef: "grant_import_aaaaaaaaaaaaaaaa",
  displayName: "下载素材",
  displayPath: "D:\\素材\\下载",
  purpose: "import_source",
  recursive: true,
  remembered: true,
  available: true,
  access: { read: true, write: false, export: false },
};

const exportGrant: DirectoryGrant = {
  grantRef: "grant_export_bbbbbbbbbbbbbbbb",
  displayName: "导出目标",
  displayPath: "E:\\导出",
  purpose: "export_destination",
  recursive: false,
  remembered: true,
  expiresAtUnixSeconds: 1893456000,
  available: false,
  access: { read: true, write: true, export: true },
};

beforeEach(() => {
  vi.clearAllMocks();
  mocks.listDirectoryGrants.mockResolvedValue([]);
  mocks.revokeDirectoryGrant.mockResolvedValue(undefined);
  mocks.revokeAllDirectoryGrants.mockResolvedValue(undefined);
});

describe("DirectoryGrantsPanel", () => {
  it("空列表说明只有勾选「记住」才会出现，不误导为权限已清空", async () => {
    render(<DirectoryGrantsPanel />);
    expect(await screen.findByText(/还没有记住的目录/)).toBeInTheDocument();
    expect(screen.getByText(/记住读取不等于入库、写入或外发给 AI 服务的许可/)).toBeInTheDocument();
  });

  it("导入来源只显示读取；导出目标显示写入与导出，并标注目录已不可用", async () => {
    mocks.listDirectoryGrants.mockResolvedValue([importGrant, exportGrant]);
    render(<DirectoryGrantsPanel />);

    expect(await screen.findByText("下载素材")).toBeInTheDocument();
    expect(screen.getByText("导入来源（只读）")).toBeInTheDocument();
    // 导入来源的 access 里没有写入/导出，文案就不能出现这两个词。
    const importRow = screen.getByText("下载素材").closest("li");
    expect(importRow).not.toBeNull();
    expect(importRow).toHaveTextContent("权限：读取（含子目录）");
    expect(importRow).toHaveTextContent("长期有效，直到你撤销");
    expect(importRow).not.toHaveTextContent("写入");

    const exportRow = screen.getByText("导出目标").closest("li");
    expect(exportRow).toHaveTextContent("权限：读取 · 写入 · 导出（仅当前目录）");
    expect(exportRow).toHaveTextContent("本次会话授权");
    expect(screen.getByText("目录已不可用")).toBeInTheDocument();
  });

  it("撤销单个授权后刷新列表，并说明下一次会重新询问", async () => {
    mocks.listDirectoryGrants.mockResolvedValueOnce([importGrant]).mockResolvedValueOnce([]);
    render(<DirectoryGrantsPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "撤销" }));
    await waitFor(() =>
      expect(mocks.revokeDirectoryGrant).toHaveBeenCalledWith("grant_import_aaaaaaaaaaaaaaaa"),
    );
    expect(await screen.findByRole("status")).toHaveTextContent(
      "已撤销「下载素材」的访问权限；下一次请求会重新询问。",
    );
    await waitFor(() => expect(screen.getByText(/还没有记住的目录/)).toBeInTheDocument());
    expect(mocks.listDirectoryGrants).toHaveBeenCalledTimes(2);
  });

  it("撤销失败如实报错，不把条目从列表里抹掉", async () => {
    mocks.listDirectoryGrants.mockResolvedValue([importGrant]);
    mocks.revokeDirectoryGrant.mockRejectedValueOnce(new Error("权限库被占用"));
    render(<DirectoryGrantsPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "撤销" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("撤销失败：权限库被占用");
    expect(screen.getByText("下载素材")).toBeInTheDocument();
  });

  it("撤销全部必须二次确认，确认后说明不会删除素材", async () => {
    mocks.listDirectoryGrants.mockResolvedValueOnce([importGrant, exportGrant]).mockResolvedValueOnce([]);
    render(<DirectoryGrantsPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "撤销全部" }));
    const dialog = await screen.findByRole("dialog", { name: "撤销全部目录授权" });
    expect(dialog).toHaveTextContent("不会删除任何素材或已导入的文件");
    expect(mocks.revokeAllDirectoryGrants).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "确认撤销" }));
    await waitFor(() => expect(mocks.revokeAllDirectoryGrants).toHaveBeenCalledTimes(1));
    expect(await screen.findByRole("status")).toHaveTextContent("已撤销全部记住的目录授权");
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "撤销全部目录授权" })).not.toBeInTheDocument(),
    );
  });

  it("读取失败时展示错误且保留刷新入口", async () => {
    mocks.listDirectoryGrants.mockRejectedValueOnce(new Error("权限库不可读"));
    render(<DirectoryGrantsPanel />);

    expect(await screen.findByRole("alert")).toHaveTextContent("读取目录授权失败：权限库不可读");
    expect(screen.getByRole("button", { name: "刷新" })).toBeEnabled();
  });
});
