/**
 * 常用流程管理面板（A4-07）验收测试。
 *
 * 卡片要求的关键行为：
 * 1. 保存的是「允许的步骤 + 参数定义」，界面上不存在能填素材 ID、路径、凭据的输入框；
 * 2. 保存前告知会申请哪些能力、复用时还要现场选什么；
 * 3. 「再次使用」只把「这次还要填什么」放回输入框，不假装能一键重跑；
 * 4. 改名/停用不改在途任务使用的版本快照（后端保证，界面如实说明）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import WorkflowPanel, { draftInputFromView, reusePrompt } from "@/components/workflows/WorkflowPanel";
import type { WorkflowStepCatalogEntry, WorkflowView } from "@/api/workflows";

vi.mock("@/api/workflows", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  listWorkflows: vi.fn(),
  workflowStepCatalog: vi.fn(),
  createWorkflow: vi.fn(),
  updateWorkflow: vi.fn(),
  renameWorkflow: vi.fn(),
  setWorkflowArchived: vi.fn(),
}));

import {
  createWorkflow,
  listWorkflows,
  renameWorkflow,
  setWorkflowArchived,
  updateWorkflow,
  workflowStepCatalog,
} from "@/api/workflows";
import { useConversationStore } from "@/stores/conversationStore";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";

const CATALOG: WorkflowStepCatalogEntry[] = [
  {
    capability: "imports.submit",
    label: "执行导入",
    warning: null,
    parameters: [
      {
        key: "sourceRef",
        label: "导入来源",
        required: true,
        description: "这次要导入的目录",
      },
    ],
  },
  {
    capability: "exports.submit",
    label: "执行导出",
    warning: null,
    parameters: [
      {
        key: "destinationRef",
        label: "导出目标",
        required: true,
        description: "这次要用的导出目录",
      },
    ],
  },
];

function viewOf(over: Partial<WorkflowView> = {}): WorkflowView {
  return {
    workflowRef: "workflow_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    version: 3,
    name: "旅行照片入库并导出",
    description: "先导入再复制导出",
    archived: false,
    updatedAt: 1_700_000_000,
    parameters: [
      { key: "sourceRef", label: "导入来源", required: true, description: "这次要导入的目录" },
    ],
    steps: [
      { capability: "imports.submit", label: "执行导入", dependsOn: null, reviewKind: "ordinary", warning: null },
      {
        capability: "exports.submit",
        label: "执行导出",
        dependsOn: 0,
        reviewKind: "ordinary",
        warning: null,
      },
    ],
    requires: ["执行导入", "执行导出"],
    runtimeInputs: [
      { key: "selectionRef", label: "素材范围", required: true, hint: "在素材库选中要导出的素材" },
    ],
    needsAttention: null,
    ...over,
  };
}

beforeEach(() => {
  vi.mocked(listWorkflows).mockReset().mockResolvedValue([viewOf()]);
  vi.mocked(workflowStepCatalog).mockReset().mockResolvedValue(CATALOG);
  vi.mocked(createWorkflow).mockReset();
  vi.mocked(updateWorkflow).mockReset();
  vi.mocked(renameWorkflow).mockReset();
  vi.mocked(setWorkflowArchived).mockReset();
  useConversationStore.setState({ activeRef: "conv_a", active: null });
  useComposerInjectStore.setState({ pending: {} });
  useWorkflowPanelStore.setState({ open: true, draft: null, draftingFrom: null, error: null });
});

afterEach(() => {
  vi.clearAllMocks();
});

function renderPanel() {
  return render(<WorkflowPanel />);
}

describe("WorkflowPanel", () => {
  it("列出流程并把版本、能力顺序与运行时输入说清楚", async () => {
    renderPanel();
    const dialog = screen.getByRole("dialog", { name: "常用流程" });
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    // 左栏条目
    fireEvent.click(await within(dialog).findByText("旅行照片入库并导出"));

    // 左栏条目里也有版本徽标，所以详情头的版本用 getAllByText（至少两处即证明详情已渲染）
    expect(within(dialog).getAllByText("第 3 版").length).toBeGreaterThanOrEqual(2);
    // 会申请的能力按顺序列出，依赖关系也要可读
    expect(within(dialog).getByText(/执行导出（依赖第 1 步）/)).toBeTruthy();
    expect(within(dialog).getByText("会申请的能力（按顺序）")).toBeTruthy();
    // 复用时仍然要现场选择的输入必须出现
    expect(within(dialog).getByText(/素材范围（必填）/)).toBeTruthy();
    // 保存的只有参数定义，没有本次的素材或凭据
    expect(within(dialog).queryByLabelText(/密钥/)).toBeNull();
    expect(within(dialog).queryByLabelText(/路径/)).toBeNull();
  });

  it("管理入口支持查找并可以包含已停用的流程", async () => {
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.change(screen.getByLabelText("查找常用流程"), { target: { value: "旅行" } });
    fireEvent.click(screen.getByRole("button", { name: "查找" }));
    await waitFor(() =>
      expect(listWorkflows).toHaveBeenLastCalledWith(false, "旅行"),
    );

    fireEvent.click(screen.getByLabelText("包含已停用流程"));
    await waitFor(() => expect(listWorkflows).toHaveBeenLastCalledWith(true, "旅行"));
  });

  it("新建时只能从受控目录选步骤，且不能保存空名称或空步骤", async () => {
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "新建" }));
    const dialog = screen.getByRole("dialog", { name: "常用流程" });

    const nameInput = within(dialog).getByLabelText("流程名称");
    expect(nameInput).toHaveProperty("value", "");
    // 名称与步骤都为空时保存按钮禁用（不靠后端报错兜底）
    const submit = within(dialog).getByRole("button", { name: "保存常用流程" });
    expect(submit).toHaveProperty("disabled", true);

    fireEvent.click(within(dialog).getByRole("button", { name: "添加步骤" }));
    // 步骤下拉的选项只来自 workflow_step_catalog
    const select = within(dialog).getByLabelText("第 1 步动作");
    const options = Array.from(select.querySelectorAll("option")).map((option) => option.textContent);
    expect(options).toEqual(["执行导入", "执行导出"]);

    fireEvent.change(nameInput, { target: { value: "我的流程" } });
    expect(within(dialog).getByRole("button", { name: "保存常用流程" })).toHaveProperty(
      "disabled",
      false,
    );
  });

  it("保存后刷新列表并说明版本与在途任务不受影响", async () => {
    vi.mocked(createWorkflow).mockResolvedValue(viewOf({ version: 1, name: "我的流程" }));
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "新建" }));
    fireEvent.change(screen.getByLabelText("流程名称"), { target: { value: "我的流程" } });
    fireEvent.click(screen.getByRole("button", { name: "添加步骤" }));
    fireEvent.click(screen.getByRole("button", { name: "保存常用流程" }));

    await waitFor(() => expect(createWorkflow).toHaveBeenCalledTimes(1));
    const payload = vi.mocked(createWorkflow).mock.calls[0][0];
    // 载荷里只有定义：没有素材 ID、路径、授权或密钥
    expect(Object.keys(payload).sort()).toEqual(["description", "name", "parameters", "steps"]);
    expect(payload.steps).toEqual([{ capability: "imports.submit", dependsOn: null }]);
    expect(payload.parameters).toEqual([
      { key: "sourceRef", required: true, description: "这次要导入的目录" },
    ]);

    // 保存成功后回到浏览态并刷新列表
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/已保存「我的流程」/)).toBeTruthy();
  });

  it("编辑已有版本时把当前版本号作为 CAS 条件提交", async () => {
    vi.mocked(updateWorkflow).mockResolvedValue(viewOf({ version: 4 }));
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    expect(screen.getByText(/编辑会保存为新版本/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "保存为新版本" }));
    await waitFor(() => expect(updateWorkflow).toHaveBeenCalledTimes(1));
    expect(vi.mocked(updateWorkflow).mock.calls[0][0]).toBe(
      "workflow_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    // 必须带上读到的版本号，否则并发编辑会静默覆盖别人的新版本
    expect(vi.mocked(updateWorkflow).mock.calls[0][1]).toBe(3);
  });

  it("改名与停用都如实说明不改在途任务快照", async () => {
    vi.mocked(renameWorkflow).mockResolvedValue(viewOf({ name: "新名字" }));
    vi.mocked(setWorkflowArchived).mockResolvedValue(viewOf({ archived: true }));
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    fireEvent.click(screen.getByRole("button", { name: "改名" }));
    fireEvent.change(screen.getByLabelText("新的流程名称"), { target: { value: "新名字" } });
    fireEvent.click(screen.getByRole("button", { name: "确认改名" }));
    await waitFor(() => expect(renameWorkflow).toHaveBeenCalledWith(expect.any(String), "新名字"));
    expect(await screen.findByText(/改名只调整列表显示/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "停用" }));
    const confirm = screen.getByRole("dialog", { name: "停用常用流程" });
    expect(within(confirm).getByText(/正在运行的任务保持原样/)).toBeTruthy();
    fireEvent.click(within(confirm).getByRole("button", { name: "停用" }));

    await waitFor(() => expect(setWorkflowArchived).toHaveBeenCalledWith(expect.any(String), true));
    expect(await screen.findByText(/已停用：列表默认不再显示/)).toBeTruthy();
  });

  it("把「再次使用」的输入清单放回输入框，而不是直接重跑", async () => {
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    fireEvent.click(screen.getByRole("button", { name: "再次使用" }));

    const injected = useComposerInjectStore.getState().pending["conv_a"];
    expect(injected).toBeTruthy();
    expect(injected).toContain("旅行照片入库并导出");
    expect(injected).toContain("素材范围（必填）");
    // 明确要求只准备计划、不直接执行
    expect(injected).toContain("不要直接执行");
    // 面板关闭，用户回到输入框
    expect(useWorkflowPanelStore.getState().open).toBe(false);
  });

  it("没有选中会话时「再次使用」给出可解释的提示", async () => {
    useConversationStore.setState({ activeRef: null, active: null });
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    fireEvent.click(screen.getByRole("button", { name: "再次使用" }));

    expect(await screen.findByText(/先在左侧选择一个对话/)).toBeTruthy();
    expect(useComposerInjectStore.getState().pending).toEqual({});
  });

  it("保存失败时保留表单并显示原因，不假装已保存", async () => {
    vi.mocked(createWorkflow).mockRejectedValue(new Error("步骤里有不允许的动作"));
    renderPanel();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "新建" }));
    fireEvent.change(screen.getByLabelText("流程名称"), { target: { value: "X" } });
    fireEvent.click(screen.getByRole("button", { name: "添加步骤" }));
    fireEvent.click(screen.getByRole("button", { name: "保存常用流程" }));

    expect(await screen.findByText(/保存失败：步骤里有不允许的动作/)).toBeTruthy();
    // 表单还在，用户不用重填
    expect(screen.getByLabelText("流程名称")).toHaveProperty("value", "X");
  });

  it("从任务生成草稿时把排除的步骤与原因直接摊开", async () => {
    useWorkflowPanelStore.setState({
      open: true,
      draftingFrom: null,
      error: null,
      draft: {
        sourceOperationRef: "operation_1",
        name: "移入回收站",
        description: "",
        steps: [{ capability: "assets.trash.submit", dependsOn: null }],
        parameters: [],
        requires: ["移入回收站"],
        runtimeInputs: [],
        notes: ["已省略批准引用与素材 ID"],
        excluded: [{ capability: "unknown.capability", reason: "不是受控能力目录里的动作" }],
      },
    });
    renderPanel();

    expect(await screen.findByText(/无法保存为常用流程/)).toBeTruthy();
    expect(screen.getByText(/unknown\.capability：不是受控能力目录里的动作/)).toBeTruthy();
    expect(screen.getByText("已省略批准引用与素材 ID")).toBeTruthy();
  });
});

describe("WorkflowPanel 嵌入助手右侧栏", () => {
  it("默认（不传 embedded）仍是模态框，行为不变", async () => {
    renderPanel();
    expect(screen.getByRole("dialog", { name: "常用流程" })).toBeInTheDocument();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));
    // 模态框里有「新建」等管理动作，说明内容真的挂上了
    expect(screen.getByRole("button", { name: "新建" })).toBeInTheDocument();
  });

  it("embedded 时渲染成侧栏内容而不是对话框，关闭动作交给父级", async () => {
    render(<WorkflowPanel embedded />);

    // 不再是 role="dialog"：嵌入时没有遮罩，也不抢焦点
    expect(screen.queryByRole("dialog")).toBeNull();
    // 标题由外壳的选项卡承担，面板自己不再重复一遍
    expect(screen.queryByRole("heading", { name: "常用流程" })).toBeNull();
    expect(screen.getByRole("button", { name: "新建" })).toBeInTheDocument();
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    // 列表与详情照常可用
    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    expect(screen.getByText("会申请的能力（按顺序）")).toBeTruthy();
    expect(screen.getByText(/执行导出（依赖第 1 步）/)).toBeTruthy();

    // 面板自己不提供关闭类按钮：同一屏只该有外壳那一个「收起」
    expect(screen.queryByRole("button", { name: "关闭" })).toBeNull();
    expect(screen.queryByRole("button", { name: "收起" })).toBeNull();
  });

  it("embedded 时「再次使用」关的是父级侧栏，而不是只关自己", async () => {
    const onClose = vi.fn();
    render(<WorkflowPanel embedded onClose={onClose} />);
    await waitFor(() => expect(listWorkflows).toHaveBeenCalledTimes(1));

    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    fireEvent.click(screen.getByRole("button", { name: "再次使用" }));

    // 提示已放回输入框；同时整个抽屉要收起（否则用户看不到输入框）
    expect(useComposerInjectStore.getState().pending["conv_a"]).toBeTruthy();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("草稿与复用文案的纯函数", () => {
  it("已有版本转成编辑初值时只取定义字段", () => {
    const input = draftInputFromView(viewOf());
    expect(input).toEqual({
      name: "旅行照片入库并导出",
      description: "先导入再复制导出",
      steps: [
        { capability: "imports.submit", dependsOn: null },
        { capability: "exports.submit", dependsOn: 0 },
      ],
      parameters: [
        { key: "sourceRef", required: true, description: "这次要导入的目录" },
      ],
    });
    expect(JSON.stringify(input)).not.toContain("workflow_");
  });

  it("复用文案带着版本、步骤与输入清单", () => {
    const text = reusePrompt(viewOf());
    expect(text).toContain("第 3 版");
    expect(text).toContain("执行导入 → 执行导出");
    expect(text).toContain("在素材库选中要导出的素材");
    expect(text).toContain("按本次重新选择");
  });
});
