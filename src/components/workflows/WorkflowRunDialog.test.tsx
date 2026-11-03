/**
 * A4-08：把常用流程实例化成新计划的验收测试。
 *
 * `@/api/workflowRun` 只把四个命令函数替换成 vi.fn()，纯函数（stepStateLabel /
 * runSummaryLines / requiresSeparateNotice / confirmBlockedReason / isRunFinished /
 * runPhaseLabel）仍走真实实现 —— 要验的正是这些判定与文案本身，mock 掉就什么都没测到。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import WorkflowRunDialog from "@/components/workflows/WorkflowRunDialog";
import {
  requiresSeparateNotice,
  runSummaryLines,
  stepStateLabel,
  type WorkflowInstancePreviewView,
  type WorkflowRunReport,
  type WorkflowRunStart,
} from "@/api/workflowRun";
import type { WorkflowView } from "@/api/workflows";
import { AppError } from "@/api/client";

vi.mock("@/api/workflowRun", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  prepareWorkflowInstance: vi.fn(),
  runWorkflowInstance: vi.fn(),
  fetchWorkflowRunReport: vi.fn(),
  cancelWorkflowInstance: vi.fn(),
}));

// 面板侧的接线挂在常用流程面板里，只替掉它的列表命令，纯函数仍走真实实现。
vi.mock("@/api/workflows", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  listWorkflows: vi.fn(),
  workflowStepCatalog: vi.fn(),
}));

vi.mock("@/api/selections", () => ({
  snapshotSelection: vi.fn(),
}));

vi.mock("@/api/move", () => ({
  listMoveTargets: vi.fn(),
}));

vi.mock("@/api/connections", () => ({
  listAiConnections: vi.fn(),
}));

vi.mock("@/api/authorization", () => ({
  pickDirectoryGrant: vi.fn(),
}));

import {
  cancelWorkflowInstance,
  fetchWorkflowRunReport,
  prepareWorkflowInstance,
  runWorkflowInstance,
} from "@/api/workflowRun";
import { snapshotSelection } from "@/api/selections";
import { listMoveTargets } from "@/api/move";
import { listAiConnections } from "@/api/connections";
import { pickDirectoryGrant } from "@/api/authorization";
import { useSelectionStore } from "@/stores/selectionStore";
import WorkflowPanel from "@/components/workflows/WorkflowPanel";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { useConversationStore } from "@/stores/conversationStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";
import { listWorkflows, workflowStepCatalog } from "@/api/workflows";

function viewOf(over: Partial<WorkflowView> = {}): WorkflowView {
  return {
    workflowRef: "workflow_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    version: 3,
    name: "旅行照片入库并导出",
    description: "先导入再复制导出",
    archived: false,
    updatedAt: 1_700_000_000,
    parameters: [],
    steps: [
      {
        capability: "exports.submit",
        label: "复制导出",
        dependsOn: null,
        reviewKind: "ordinary",
        warning: null,
      },
    ],
    requires: ["复制导出"],
    runtimeInputs: [
      { key: "selectionRef", label: "素材集合", required: true, hint: "在素材库选中要导出的素材" },
    ],
    needsAttention: null,
    ...over,
  };
}

function previewOf(over: Partial<WorkflowInstancePreviewView> = {}): WorkflowInstancePreviewView {
  return {
    planId: "plan_1",
    reviewRef: "review_1",
    workflowRef: "workflow_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    version: 3,
    workflowName: "旅行照片入库并导出",
    summary: "把 2 项素材复制导出到已授权目录",
    expiresAtUnixSeconds: 1_800_000_000,
    steps: [
      {
        ordinal: 1,
        capability: "exports.submit",
        label: "复制导出",
        requiresSeparateAction: false,
        dependsOn: null,
        itemCount: 2,
        note: null,
      },
    ],
    warnings: [],
    ...over,
  };
}

function reportOf(over: Partial<WorkflowRunReport> = {}): WorkflowRunReport {
  return {
    planId: "plan_1",
    operationId: "operation_1",
    operationState: "succeeded",
    steps: [
      {
        ordinal: 1,
        capability: "exports.submit",
        label: "复制导出",
        state: "succeeded",
        message: null,
        done: 2,
        failed: 0,
        total: 2,
      },
    ],
    summary: "已完成 1 个步骤",
    ...over,
  };
}

function startOf(over: Partial<WorkflowRunStart> = {}): WorkflowRunStart {
  return {
    planId: "plan_1",
    operationId: "operation_1",
    operationState: "queued",
    replayed: false,
    canCancel: true,
    ...over,
  };
}

const ROOT_TARGET = { collectionRef: "col_root", relativePath: "", label: "总仓根目录" };
const SUB_TARGET = { collectionRef: "col_travel", relativePath: "旅行", label: "旅行" };

beforeEach(() => {
  vi.mocked(prepareWorkflowInstance).mockReset().mockResolvedValue(previewOf());
  vi.mocked(runWorkflowInstance).mockReset().mockResolvedValue(startOf());
  vi.mocked(fetchWorkflowRunReport).mockReset().mockResolvedValue(reportOf());
  vi.mocked(cancelWorkflowInstance).mockReset().mockResolvedValue(undefined);
  vi.mocked(snapshotSelection)
    .mockReset()
    .mockResolvedValue({ selectionRef: "selection_1", total: 2, truncated: false });
  vi.mocked(listMoveTargets).mockReset().mockResolvedValue([ROOT_TARGET, SUB_TARGET]);
  vi.mocked(listAiConnections).mockReset().mockResolvedValue([]);
  vi.mocked(pickDirectoryGrant).mockReset().mockResolvedValue(null);
  useSelectionStore.setState({
    selected: new Set<number>([11, 12]),
    truncated: false,
    selectionTotal: 2,
    anchorIndex: 1,
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

function renderDialog(over: Partial<WorkflowView> = {}, open = true) {
  const view = viewOf(over);
  const onClose = vi.fn();
  const utils = render(<WorkflowRunDialog open={open} onClose={onClose} view={view} />);
  const rerender = (nextOpen: boolean) =>
    utils.rerender(<WorkflowRunDialog open={nextOpen} onClose={onClose} view={view} />);
  return { ...utils, view, onClose, rerender };
}

describe("WorkflowRunDialog 本次输入", () => {
  it("标题写明流程名与版本，并说明这些输入只属于这一次", () => {
    renderDialog();
    const dialog = screen.getByRole("dialog", {
      name: "使用常用流程：旅行照片入库并导出（第 3 版）",
    });
    expect(within(dialog).getByText(/每次都要现场重选/)).toBeTruthy();
  });

  it("素材库没有选中时给出「先去素材库选择素材」且不能确认", () => {
    useSelectionStore.setState({ selected: new Set<number>(), truncated: false, selectionTotal: 0 });
    renderDialog();

    expect(screen.getByText("先去素材库选择素材")).toBeTruthy();
    expect(screen.getByText("先去素材库选择素材")).toBeTruthy();
    expect(screen.getByRole("button", { name: "确认并开始" })).toHaveProperty("disabled", true);
    // 没有选择就不该去打快照
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    expect(snapshotSelection).not.toHaveBeenCalled();
  });

  it("准备计划时才把当前选择固定成快照，并把 selectionRef 交给后端", async () => {
    renderDialog();
    expect(screen.getByText(/已在素材库选中 2 项/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));

    await waitFor(() => expect(snapshotSelection).toHaveBeenCalledTimes(1));
    expect(vi.mocked(snapshotSelection).mock.calls[0][1]).toEqual([11, 12]);
    await waitFor(() => expect(prepareWorkflowInstance).toHaveBeenCalledTimes(1));
    expect(prepareWorkflowInstance).toHaveBeenCalledWith(
      "workflow_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      3,
      { selectionRef: "selection_1" },
    );
  });

  it("每次重新打开都从空白开始，上一份预览与旧输入都不沿用", async () => {
    const { rerender } = renderDialog({
      runtimeInputs: [
        { key: "renamePattern", label: "重命名规则", required: false, hint: "可留空" },
      ],
    });

    fireEvent.change(screen.getByLabelText("重命名规则"), { target: { value: "旅行_{序号}" } });
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await waitFor(() => expect(screen.getByTestId("workflow-run-preview")).toBeTruthy());

    rerender(false);
    rerender(true);

    expect(screen.getByLabelText("重命名规则")).toHaveProperty("value", "");
    expect(screen.queryByTestId("workflow-run-preview")).toBeNull();
  });

  it("改了输入就作废上一份预览，避免批到用旧输入准备的计划", async () => {
    renderDialog({
      runtimeInputs: [
        { key: "renamePattern", label: "重命名规则", required: false, hint: "可留空" },
      ],
    });
    fireEvent.change(screen.getByLabelText("重命名规则"), { target: { value: "a" } });
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await waitFor(() => expect(screen.getByTestId("workflow-run-preview")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("重命名规则"), { target: { value: "b" } });
    expect(screen.queryByTestId("workflow-run-preview")).toBeNull();
    expect(screen.getByRole("button", { name: "确认并开始" })).toHaveProperty("disabled", true);
  });

  it("导出目标必须现场选目录授权，选完显示为只读文本", async () => {
    vi.mocked(pickDirectoryGrant).mockResolvedValue({
      grantRef: "grant_1",
      directoryRef: "directory_1",
      displayName: "导出",
      displayPath: "D:\\导出",
      purpose: "export_destination",
      recursive: false,
      remembered: false,
      available: true,
      access: { read: true, write: true, export: true },
    });
    renderDialog({
      runtimeInputs: [
        { key: "destinationRef", label: "导出目标", required: true, hint: "每次单独授权" },
      ],
    });

    fireEvent.click(screen.getByRole("button", { name: "选择导出目标目录" }));
    await waitFor(() => expect(pickDirectoryGrant).toHaveBeenCalledWith("export_destination", false));

    const shown = screen.getByLabelText("导出目标目录");
    expect(shown).toHaveProperty("value", "D:\\导出");
    expect(shown).toHaveProperty("readOnly", true);

    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await waitFor(() => expect(prepareWorkflowInstance).toHaveBeenCalledTimes(1));
    expect(vi.mocked(prepareWorkflowInstance).mock.calls[0][2]).toEqual({
      destinationRef: "directory_1",
    });
  });

  it("目标分库下拉默认选中总仓根目录", async () => {
    renderDialog({
      runtimeInputs: [
        { key: "collectionRef", label: "目标分库", required: true, hint: "总仓内的分库" },
      ],
    });

    await waitFor(() => expect(listMoveTargets).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.getByLabelText("目标分库")).toHaveProperty("value", "col_root"));
    const options = Array.from(screen.getByLabelText("目标分库").querySelectorAll("option")).map(
      (option) => option.textContent,
    );
    expect(options).toContain("总仓根目录");
    expect(options).toContain("旅行");
  });

  it("AI 打标缺少可用服务时提示先去设置配置，并禁止确认", async () => {
    renderDialog({
      steps: [
        {
          capability: "tagging.submit",
          label: "AI 打标",
          dependsOn: null,
          reviewKind: "ordinary",
          warning: null,
        },
      ],
      runtimeInputs: [
        { key: "serviceRef", label: "AI 服务", required: true, hint: "按当前绑定核实" },
        { key: "taggingInstructions", label: "打标要求", required: true, hint: "例如只标风景" },
      ],
    });

    await waitFor(() => expect(listAiConnections).toHaveBeenCalledTimes(1));
    expect(screen.getByText("需要先在设置里配置 AI 服务")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("打标要求"), { target: { value: "只标风景" } });
    expect(screen.getByRole("button", { name: "确认并开始" })).toHaveProperty("disabled", true);
    expect(screen.getByText("需要先在设置里配置 AI 服务")).toBeTruthy();
  });

  it("来源目录只读提示并禁止准备，流程不代选来源", async () => {
    renderDialog({
      runtimeInputs: [
        { key: "sourceRef", label: "导入来源", required: true, hint: "每次都要重新选择" },
      ],
    });

    expect(screen.getByText("请先在入库页选择来源目录")).toBeTruthy();
    // 没有任何可填控件，也不许拿旧目录兜底
    expect(screen.queryByLabelText("导入来源")).toBeNull();
    expect(screen.getByRole("button", { name: "准备计划" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "确认并开始" })).toHaveProperty("disabled", true);
  });
});

describe("WorkflowRunDialog 预览与执行", () => {
  it("预览区展示摘要、逐步骤与必须单独确认的提示", async () => {
    vi.mocked(prepareWorkflowInstance).mockResolvedValue(
      previewOf({
        summary: "先入库 3 项，再等您确认标签",
        steps: [
          {
            ordinal: 1,
            capability: "imports.submit",
            label: "入库",
            requiresSeparateAction: false,
            dependsOn: null,
            itemCount: 3,
            note: null,
          },
          {
            ordinal: 2,
            capability: "tagging.submit",
            label: "AI 打标",
            requiresSeparateAction: true,
            dependsOn: 0,
            itemCount: 3,
            note: "正式标签仍要逐条确认",
          },
        ],
        warnings: ["导出只复制，不会移动库内原文件"],
      }),
    );
    renderDialog();

    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));

    const preview = await screen.findByTestId("workflow-run-preview");
    expect(within(preview).getByText("先入库 3 项，再等您确认标签")).toBeTruthy();
    expect(within(preview).getByText(/第 1 步「入库」（3 项）/)).toBeTruthy();
    expect(
      within(preview).getByText(/第 2 步「AI 打标」（3 项）（需要您单独确认）/, {
        normalizer: (text) => text,
      }),
    ).toBeTruthy();
    expect(
      within(preview).getByText(
        "第 2 步「AI 打标」需要您在打标界面单独确认，流程不会替您批准。",
      ),
    ).toBeTruthy();
    expect(within(preview).getByText("导出只复制，不会移动库内原文件")).toBeTruthy();
  });

  it("确认后只拿到受理回执，靠轮询读出真实进度，结束后逐行展示结果", async () => {
    // 受理回执是 queued，不是终态：旧实现要等整次执行才会返回，这里必须证明
    // 「受理」与「结束」是两件事。
    vi.mocked(runWorkflowInstance).mockResolvedValue(startOf({ operationState: "queued" }));
    let resolveRunning: (value: WorkflowRunReport) => void = () => undefined;
    vi.mocked(fetchWorkflowRunReport)
      .mockImplementationOnce(
        () =>
          new Promise<WorkflowRunReport>((resolve) => {
            resolveRunning = resolve;
          }),
      )
      .mockResolvedValue(reportOf());

    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await screen.findByTestId("workflow-run-preview");

    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));
    expect(runWorkflowInstance).toHaveBeenCalledWith("plan_1");

    // 还没结束：必须能看到「正在运行」区块，而不是一个空对话框。
    const progress = await screen.findByTestId("workflow-run-progress");
    expect(within(progress).getByText(/已受理，正在排队等待执行/)).toBeTruthy();
    expect(screen.queryByTestId("workflow-run-summary")).toBeNull();

    resolveRunning(reportOf({ operationState: "running" }));
    // 轮询间隔 800ms，接近 waitFor 默认的 1000ms：并发跑全量时容易踩线，这里给足 3s。
    await waitFor(() => expect(screen.getByText(/正在执行，请稍候/)).toBeTruthy(), { timeout: 3000 });

    const summary = await screen.findByTestId("workflow-run-summary", {}, { timeout: 3000 });
    expect(within(summary).getByText("第 1 步「复制导出」：已成功")).toBeTruthy();
    expect(screen.queryByText(/部分步骤未完成/)).toBeNull();
    // 结束后不再显示运行区块，避免「已完成」与「正在运行」同时出现。
    await waitFor(() => expect(screen.queryByTestId("workflow-run-progress")).toBeNull(), {
      timeout: 3000,
    });
  });

  it("运行期就能取消，不必等出结果", async () => {
    vi.mocked(runWorkflowInstance).mockResolvedValue(startOf({ operationState: "queued" }));
    // 轮询一直停在 running：这正是旧实现根本走不到的窗口。
    vi.mocked(fetchWorkflowRunReport).mockResolvedValue(reportOf({ operationState: "running" }));
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await screen.findByTestId("workflow-run-preview");
    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));

    const progress = await screen.findByTestId("workflow-run-progress");
    fireEvent.click(within(progress).getByRole("button", { name: "取消任务" }));
    await waitFor(() => expect(cancelWorkflowInstance).toHaveBeenCalledWith("operation_1"));
    expect(await screen.findByRole("button", { name: "已请求取消" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("幂等命中已结束的任务时直接读回上次结果，不再起第二个任务", async () => {
    vi.mocked(runWorkflowInstance).mockResolvedValue(
      startOf({ operationState: "succeeded", replayed: true, canCancel: false }),
    );
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await screen.findByTestId("workflow-run-preview");
    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));

    const summary = await screen.findByTestId("workflow-run-summary");
    expect(within(summary).getByText("第 1 步「复制导出」：已成功")).toBeTruthy();
    expect(screen.queryByTestId("workflow-run-progress")).toBeNull();
  });

  it("部分步骤未完成时明确说不会自动重试", async () => {
    vi.mocked(runWorkflowInstance).mockResolvedValue(startOf({ operationState: "running" }));
    vi.mocked(fetchWorkflowRunReport).mockResolvedValue(
      reportOf({
        operationState: "partially_succeeded",
        steps: [
          {
            ordinal: 1,
            capability: "exports.submit",
            label: "复制导出",
            state: "failed",
            message: "目标目录拒绝写入",
            done: 0,
            failed: 2,
            total: 2,
          },
        ],
      }),
    );
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await screen.findByTestId("workflow-run-preview");
    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));

    const summary = await screen.findByTestId("workflow-run-summary");
    expect(
      within(summary).getByText("第 1 步「复制导出」：未成功：目标目录拒绝写入"),
    ).toBeTruthy();
    expect(screen.getByText("部分步骤未完成，未完成的步骤保持原样，不会自动重试。")).toBeTruthy();
  });

  it("结果未知时提示去任务记录核对", async () => {
    vi.mocked(runWorkflowInstance).mockResolvedValue(startOf({ operationState: "running" }));
    vi.mocked(fetchWorkflowRunReport).mockResolvedValue(
      reportOf({
        operationState: "outcome_unknown",
        steps: [
          {
            ordinal: 1,
            capability: "tagging.submit",
            label: "AI 打标",
            state: "outcome_unknown",
            message: null,
            done: 1,
            failed: 0,
            total: 2,
          },
        ],
      }),
    );
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await screen.findByTestId("workflow-run-preview");
    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));

    expect(
      await screen.findByText("部分步骤结果未知，请到任务记录里核对后再决定是否重试。"),
    ).toBeTruthy();
  });

  it("执行失败时用 AppError 的 message 报错，不假装成功", async () => {
    vi.mocked(runWorkflowInstance).mockRejectedValue(
      new AppError("PLAN_EXPIRED", "计划已过期，请重新准备"),
    );
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await screen.findByTestId("workflow-run-preview");
    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));

    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "执行失败：计划已过期，请重新准备",
    );
    expect(screen.queryByTestId("workflow-run-summary")).toBeNull();
    expect(screen.queryByTestId("workflow-run-progress")).toBeNull();
  });

  it("轮询单次失败不改写结论，任务仍在运行", async () => {
    vi.mocked(runWorkflowInstance).mockResolvedValue(startOf({ operationState: "queued" }));
    vi.mocked(fetchWorkflowRunReport).mockRejectedValue(
      new AppError("INTERNAL", "读取进度失败"),
    );
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));
    await screen.findByTestId("workflow-run-preview");
    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));

    const progress = await screen.findByTestId("workflow-run-progress");
    expect(await within(progress).findByText(/读取进度失败/)).toBeTruthy();
    expect(within(progress).getByText(/任务本身仍在进行/)).toBeTruthy();
    expect(screen.queryByTestId("workflow-run-summary")).toBeNull();
  });

  it("准备失败时给出可读原因，且不进入执行", async () => {
    vi.mocked(prepareWorkflowInstance).mockRejectedValue(
      new AppError("WORKFLOW_ARCHIVED", "这个常用流程已停用"),
    );
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "准备计划" }));

    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "准备计划失败：这个常用流程已停用",
    );
    expect(screen.queryByTestId("workflow-run-preview")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "确认并开始" }));
    expect(runWorkflowInstance).not.toHaveBeenCalled();
  });

  it("关闭按钮把控制权交回面板", () => {
    const { onClose } = renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("常用流程面板的接线", () => {
  beforeEach(() => {
    vi.mocked(listWorkflows).mockReset().mockResolvedValue([viewOf({ archived: false })]);
    vi.mocked(workflowStepCatalog).mockReset().mockResolvedValue([]);
    useConversationStore.setState({ activeRef: "conv_a" });
    useComposerInjectStore.setState({ pending: {} });
    useWorkflowPanelStore.setState({ open: true, draft: null, draftingFrom: null, error: null });
  });

  it("「用本次输入开始」打开实例化弹窗，不注入助手输入框", async () => {
    render(<WorkflowPanel />);
    await waitFor(() => expect(listWorkflows).toHaveBeenCalled());

    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    fireEvent.click(screen.getByRole("button", { name: "用本次输入开始" }));

    const dialog = await screen.findByRole("dialog", {
      name: "使用常用流程：旅行照片入库并导出（第 3 版）",
    });
    expect(dialog).toBeTruthy();
    // 这条路径是「现场收集本次输入」，不是把旧说明塞回对话。
    expect(useComposerInjectStore.getState().pending["conv_a"]).toBeUndefined();
    // 打开时还没准备计划：用户得先把本次输入填上。
    expect(prepareWorkflowInstance).not.toHaveBeenCalled();
  });

  it("已停用的流程不给开新实例，只如实说明历史仍可读", async () => {
    vi.mocked(listWorkflows).mockResolvedValue([viewOf({ archived: true })]);
    render(<WorkflowPanel />);
    await waitFor(() => expect(listWorkflows).toHaveBeenCalled());

    fireEvent.click(await screen.findByText("旅行照片入库并导出"));
    const start = screen.getByRole("button", { name: "用本次输入开始" });
    expect(start).toHaveProperty("disabled", true);
  });
});

describe("实例化相关的纯函数", () => {
  it("步骤状态一律翻成中文，未知取值原样回显", () => {
    expect(stepStateLabel("succeeded")).toBe("已成功");
    expect(stepStateLabel("failed")).toBe("未成功");
    expect(stepStateLabel("skipped")).toBe("已跳过");
    expect(stepStateLabel("cancelled")).toBe("已取消");
    expect(stepStateLabel("outcome_unknown")).toBe("结果未知");
    expect(stepStateLabel("weird" as never)).toBe("weird");
  });

  it("逐步骤结果每步一行，有 message 才追加冒号", () => {
    const lines = runSummaryLines(
      reportOf({
        steps: [
          {
            ordinal: 1,
            capability: "imports.submit",
            label: "入库",
            state: "succeeded",
            message: null,
            done: 3,
            failed: 0,
            total: 3,
          },
          {
            ordinal: 2,
            capability: "tagging.submit",
            label: "AI 打标",
            state: "failed",
            message: "服务未配置",
            done: 0,
            failed: 3,
            total: 3,
          },
        ],
      }),
    );
    expect(lines).toEqual([
      "第 1 步「入库」：已成功",
      "第 2 步「AI 打标」：未成功：服务未配置",
    ]);
  });

  it("没有需要单独确认的步骤时不硬凑一句提示", () => {
    expect(requiresSeparateNotice(previewOf())).toBeNull();
  });

  it("需要单独确认的步骤点名是第几步、去哪个界面", () => {
    const notice = requiresSeparateNotice(
      previewOf({
        steps: [
          {
            ordinal: 1,
            capability: "imports.submit",
            label: "入库",
            requiresSeparateAction: false,
            dependsOn: null,
            itemCount: 1,
            note: null,
          },
          {
            ordinal: 2,
            capability: "tagging.submit",
            label: "AI 打标",
            requiresSeparateAction: true,
            dependsOn: 0,
            itemCount: 1,
            note: null,
          },
        ],
      }),
    );
    expect(notice).toBe(
      "第 2 步「AI 打标」需要您在打标界面单独确认，流程不会替您批准。",
    );
  });
});
