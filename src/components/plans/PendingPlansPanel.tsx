/** 待审计划面板（A5-02）。
 *
 * 为什么需要它：外部客户端（MCP）只能「准备计划」，不能批准。但那些计划不
 * 属于任何本地会话，`linkedPlans` 里永远看不到它们，于是「外部准备 → 茶馆
 * 确认」这条链在界面上没有落点。这个面板是那条链的可信端点：
 *
 * - 列表来自后端 `list_pending_plans`（一次只列当前库世代内仍 pending 的行，
 *   不按来源过滤，所以 gui/pi/mcp 都会出现，且必须如实显示发起方）；
 * - 确认/拒绝只调 `approve_plan`/`reject_plan`，后端再用
 *   `check_capability_entry(PlansApprove)` 校验调用者是可信界面；
 * - **`reviewKind === "permanent_trash"` 一律只读**：清空回收站有自己的专属
 *   确认交互（删除对话框里的永久清空），在通用面板上给一个「确认此计划」
 *   按钮就等于绕开那条 N2 专属流程；
 * - 不在本地缓存计划副本、不做任何「已批准」推断，每次打开都向后端重读。
 *
 * 呈现复用 `AgentCards` 已导出的三个纯函数（能力名/发起方/目标），不复制标签表；
 * 但不复用 `PlanCard`，因为它提交时走 `useAgentSessionStore.approvePlanCard`，
 * 那个 action 要靠 `linkedPlans` 反查所属会话才刷新——独立面板总是找不到会话。
 */
import { useCallback, useEffect, useState, type ReactNode } from "react";
import Modal from "@/components/common/Modal";
import Button from "@/components/common/Button";
import {
  capabilityLabel,
  planOriginLabel,
  planTargetLabel,
} from "@/components/assistant/AgentCards";
import { approvePlan, rejectPlan } from "@/api/plans";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import type { PlanPreview } from "@/types/plans";

function expiresIn(expiresAtUnixSeconds: number, now: number): string {
  const remain = expiresAtUnixSeconds - Math.floor(now / 1000);
  if (remain <= 0) return "已过期";
  if (remain < 60) return `${remain} 秒后过期`;
  return `${Math.floor(remain / 60)} 分钟后过期`;
}

/** 这个面板能不能替用户确认这份计划。 */
function canReviewHere(plan: PlanPreview): boolean {
  return plan.reviewStatus === "pending" && plan.reviewKind === "ordinary";
}

interface PendingPlansPanelProps {
  /** 作为助手右侧栏的一个选项卡渲染：不渲染 Modal，关闭动作交给父级外壳。 */
  embedded?: boolean;
}

/** 面板外壳：默认仍是模态框（行为与改动前一致）；`embedded` 时换成侧栏里的普通容器。
 *
 *  定义在模块作用域而不是渲染函数里，避免每次渲染生成新组件类型导致列表整体重挂载
 *  （会额外触发一次回读，也会丢滚动位置）。
 */
function PanelFrame({
  embedded,
  open,
  onClose,
  footer,
  children,
}: {
  embedded: boolean;
  open: boolean;
  onClose: () => void;
  footer: ReactNode;
  children: ReactNode;
}) {
  if (!embedded) {
    return (
      <Modal open={open} wide title="待审计划" onClose={onClose} footer={footer}>
        {children}
      </Modal>
    );
  }
  // 嵌入时不再自带标题栏：外壳的选项卡已经写着「待审计划」。
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">{children}</div>
      <div className="flex shrink-0 justify-end gap-2 border-t border-[var(--color-border)] px-3 py-2">
        {footer}
      </div>
    </div>
  );
}

export default function PendingPlansPanel({ embedded = false }: PendingPlansPanelProps) {
  const open = usePendingPlansStore((s) => s.open);
  const plans = usePendingPlansStore((s) => s.plans);
  const loading = usePendingPlansStore((s) => s.loading);
  const loadError = usePendingPlansStore((s) => s.error);
  const closePanel = usePendingPlansStore((s) => s.closePanel);
  const reload = usePendingPlansStore((s) => s.reload);

  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  /** 嵌进助手侧栏时，「选项卡在前台」就等于打开；store.open 只对模态路径有意义。 */
  const active = embedded || open;

  useEffect(() => {
    // 面板只在需要时挂载（App 层 `{pendingPlansOpen && <PendingPlansPanel/>}`，
    // 助手页则是选项卡切过来时才挂载），所以「挂载即向后端重读一次」，
    // 不缓存任何计划副本。
    void reload();
  }, [reload]);

  useEffect(() => {
    if (!active) return;
    // 过期时间只在展示层刷新，真实过期仍由后端拒绝。
    const timer = setInterval(() => setNow(Date.now()), 15_000);
    return () => clearInterval(timer);
  }, [active]);

  useEffect(() => {
    if (active) setActionError(null);
  }, [active]);

  const decide = useCallback(
    async (plan: PlanPreview, verdict: "approve" | "reject") => {
      setBusy(plan.planId);
      setActionError(null);
      try {
        if (verdict === "approve") {
          await approvePlan(plan.planId, plan.reviewRef);
        } else {
          await rejectPlan(plan.planId, plan.reviewRef);
        }
      } catch (error) {
        // 审批失败可能是「已过期/已被别处处理」，必须回读真实列表而不是
        // 就地改本地状态——否则界面会显示一个后端并不同意的结论。
        setActionError(error instanceof Error ? error.message : String(error));
      } finally {
        setBusy(null);
        await reload();
      }
    },
    [reload],
  );

  // 嵌入侧栏时关闭由外壳负责，这里不再放第二个「关闭」按钮（否则同一屏两个同名控件）。
  const footer = (
    <>
      <Button onClick={() => void reload()} disabled={loading || busy !== null}>
        {loading ? "正在读取…" : "刷新"}
      </Button>
      {!embedded && (
        <Button variant="primary" onClick={closePanel}>
          关闭
        </Button>
      )}
    </>
  );

  return (
    <PanelFrame embedded={embedded} open={open} onClose={closePanel} footer={footer}>
      <div className="flex flex-col gap-3">
        <p className="text-[11px] leading-relaxed text-[var(--color-text-tertiary)]">
          外部客户端与内置助手都只能「准备」计划，批准与拒绝只在你这里发生。
          确认只代表「范围可以」，执行仍需在任务卡里由你明确开始；拒绝不产生任何写入。
        </p>

        {actionError && (
          <p role="alert" className="text-xs text-[var(--color-danger)]">
            {actionError}
          </p>
        )}
        {loadError && (
          <p role="alert" className="text-xs text-[var(--color-danger)]">
            读取待审计划失败：{loadError}
          </p>
        )}

        {!loadError && !loading && plans.length === 0 && (
          <p data-testid="pending-plans-empty" className="py-6 text-center text-xs text-[var(--color-text-tertiary)]">
            现在没有等待你确认的计划。
          </p>
        )}

        <ul data-testid="pending-plans-list" className="flex flex-col gap-2">
          {plans.map((plan) => {
            const reviewable = canReviewHere(plan);
            // 过期只在展示层禁用按钮；真正的过期判定仍由后端 approve_plan 负责。
            const expired = plan.expiresAtUnixSeconds <= Math.floor(now / 1000);
            return (
              <li
                key={plan.planId}
                data-plan-id={plan.planId}
                data-plan-origin={plan.originKind}
                className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2"
              >
                <div className="flex items-baseline gap-2">
                  <span className="text-xs font-medium text-[var(--color-text)]">
                    {capabilityLabel(plan.capability)} · {plan.itemCount} 项
                  </span>
                  <span className="ml-auto shrink-0 text-[11px] text-[var(--color-text-tertiary)]">
                    {expiresIn(plan.expiresAtUnixSeconds, now)}
                  </span>
                </div>
                <p className="mt-1 text-xs text-[var(--color-text)]">{plan.summary}</p>
                {/* 审核者必须看得见「谁提的、动哪些、动到哪」三件事；
                    ref 本身是不透明令牌，可读路径由后端写进 summary，这里不假装它是路径。 */}
                <dl className="mt-1 flex flex-col gap-0.5 text-[11px] text-[var(--color-text-tertiary)]">
                  <div>发起方：{planOriginLabel(plan)}</div>
                  {plan.scope.selectionRef && <div>范围：已选素材集合</div>}
                  {plan.scope.sourceRef && <div>来源：已授权目录</div>}
                  {plan.scope.targetRef && <div>{planTargetLabel(plan.capability)}</div>}
                  <div>预算上限：{plan.budget.maxItems} 项</div>
                </dl>

                {plan.reviewKind === "permanent_trash" ? (
                  <p className="mt-2 text-[11px] text-[var(--color-status)]">
                    永久清空回收站必须在删除对话框里单独确认，这里只显示，不能代为批准。
                  </p>
                ) : (
                  <div className="mt-2 flex items-center gap-2">
                    <Button
                      variant="primary"
                      className="min-h-0 px-2 py-0.5 text-[11px]"
                      disabled={!reviewable || busy !== null || expired}
                      onClick={() => void decide(plan, "approve")}
                    >
                      {busy === plan.planId ? "正在处理…" : expired ? "计划已过期" : "确认此计划"}
                    </Button>
                    <Button
                      className="min-h-0 px-2 py-0.5 text-[11px]"
                      disabled={!reviewable || busy !== null || expired}
                      onClick={() => void decide(plan, "reject")}
                    >
                      拒绝
                    </Button>
                    <span className="text-[11px] text-[var(--color-text-tertiary)]">仅确认范围，不代替执行</span>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      </div>
    </PanelFrame>
  );
}
