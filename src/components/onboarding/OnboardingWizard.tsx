/** 首次引导向导（A3-06）。
 *
 * 三条硬要求（任务卡 A3-06）：
 * 1. 引导「总库位置 / AI 服务 / 基本操作」，全程可跳过；
 * 2. 未配置 AI 时手动流程仍然可用，且不能把「没有 AI 服务」说成素材导入失败；
 * 3. 首次引导不得强迫用户配置 AI 才能导入。
 *
 * 因此本向导不阻塞页面：它只提供入口与说明，用户在任意一步都能跳过。
 */
import { useEffect, useState } from "react";
import { open as pickDir } from "@tauri-apps/plugin-dialog";
import Button from "@/components/common/Button";
import { formatPathForDisplay } from "@/utils/pathDisplay";
import Modal from "@/components/common/Modal";
import {
  getAiUsageBindings,
  listAiConnections,
  type AiConnection,
} from "@/api/connections";
import { useSettingsStore } from "@/stores/settingsStore";
import { deploymentMatchesEndpoint } from "@/utils/endpointScope";
import { logger } from "@/utils/logger";

const STEP_COUNT = 3;

export interface OnboardingWizardProps {
  open: boolean;
  /** 是否需要显示「引导已完成但保存失败」的错误。 */
  persistError: string | null;
  /** 已完成或跳过：调用方负责把「已处理」写回设置。 */
  onFinish: () => Promise<void> | void;
  /** 保存失败时仍允许继续使用。 */
  onContinueAnyway: () => void;
  /** 打开新手教程（沿用既有 openHelpPage 流程）。 */
  onOpenTutorial: () => Promise<void> | void;
  tutorialBusy: boolean;
  tutorialError: string | null;
  tutorialUrl: string;
  tutorialCopied: boolean;
  onCopyTutorialUrl: () => Promise<void> | void;
  /** 前往设置页继续配置。 */
  onOpenSettings: () => void;
}

export default function OnboardingWizard({
  open,
  persistError,
  onFinish,
  onContinueAnyway,
  onOpenTutorial,
  tutorialBusy,
  tutorialError,
  tutorialUrl,
  tutorialCopied,
  onCopyTutorialUrl,
  onOpenSettings,
}: OnboardingWizardProps) {
  const settings = useSettingsStore((s) => s.settings);
  const save = useSettingsStore((s) => s.save);
  const [step, setStep] = useState(0);
  const [connections, setConnections] = useState<AiConnection[]>([]);
  const [agentBound, setAgentBound] = useState<string | null>(null);
  const [bindingLoaded, setBindingLoaded] = useState(false);
  const [picking, setPicking] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) setStep(0);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    void (async () => {
      try {
        const [bindings, list] = await Promise.all([getAiUsageBindings(), listAiConnections()]);
        if (cancelled) return;
        setConnections(list);
        setAgentBound(bindings.agent ?? null);
      } catch (cause) {
        if (cancelled) return;
        logger.warn("引导读取 AI 服务失败", {
          error: cause instanceof Error ? cause.message : String(cause),
        });
      } finally {
        if (!cancelled) setBindingLoaded(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open]);

  const libraryRoot = settings?.libraryRoot ?? "";

  /** 绑定服务的可用性问题：停用 / 云端缺密钥 / 地址与部署方式矛盾。
   *  本机服务不需要密钥，`missing` 不是问题；未绑定也不算问题（手动流程可用）。
   */
  const serviceIssues = (() => {
    if (!agentBound) return [];
    const bound = connections.find((item) => item.id === agentBound);
    if (!bound) return ["助手绑定的服务已不存在，请到设置里重新选择。"];
    const issues: string[] = [];
    if (!bound.enabled) {
      issues.push(`「${bound.name}」已停用，入库、素材库和手动标签不受影响，但助手与自动打标会失败。`);
    }
    if (bound.deployment === "cloud" && bound.credentialStatus !== "configured") {
      issues.push(`「${bound.name}」的密钥不可用，请在设置里重新填写；界面不会回显密钥内容。`);
    }
    if (!deploymentMatchesEndpoint(bound.deployment, bound.baseUrl)) {
      issues.push(
        `「${bound.name}」声明的部署方式（${bound.deployment === "local" ? "本机" : "在线"}）与接口地址不一致，请到设置里核对地址。`,
      );
    }
    return issues;
  })();

  const chooseLibraryRoot = async () => {
    setPicking(true);
    setLocalError(null);
    try {
      const dir = await pickDir({ directory: true });
      const current = useSettingsStore.getState().settings;
      if (typeof dir !== "string" || !current) return;
      await save({ ...current, libraryRoot: dir });
    } catch (cause) {
      setLocalError(
        `保存总库位置失败：${cause instanceof Error ? cause.message : String(cause)}。可以稍后在设置里重试。`,
      );
    } finally {
      setPicking(false);
    }
  };

  const footer =
    step < STEP_COUNT - 1 && !persistError ? (
      <>
        <Button disabled={picking} onClick={() => void onFinish()}>
          跳过引导
        </Button>
        <Button variant="primary" onClick={() => setStep((value) => value + 1)}>
          下一步
        </Button>
      </>
    ) : (
      <>
        <Button disabled={tutorialBusy} onClick={() => void onFinish()}>
          以后再看
        </Button>
        {persistError ? (
          <Button variant="primary" onClick={onContinueAnyway}>
            继续使用
          </Button>
        ) : tutorialError ? (
          <Button variant="primary" disabled={tutorialBusy} onClick={() => void onFinish()}>
            {tutorialCopied ? "已复制，关闭" : "关闭"}
          </Button>
        ) : (
          <Button variant="primary" disabled={tutorialBusy} onClick={() => void onOpenTutorial()}>
            {tutorialBusy ? "正在打开…" : "查看新手教程"}
          </Button>
        )}
      </>
    );

  return (
    <Modal open={open} title="欢迎使用茶馆" onClose={() => void onFinish()} footer={footer} wide>
      <div className="flex flex-col gap-4">
        <p className="text-[11px] text-[var(--color-text-tertiary)]" aria-live="polite">
          第 {step + 1} / {STEP_COUNT} 步 · 任意一步都可以跳过，之后在设置里补
        </p>

        {/* 保存失败必须立即可见：不管用户停在哪一步，都不能让它看起来像「已经记住」。 */}
        {(persistError || tutorialError) && (
          <div role="alert" className="rounded-[var(--radius-control)] border border-[var(--color-danger)] px-3 py-2 text-xs text-[var(--color-danger)]">
            {persistError ?? tutorialError}
          </div>
        )}

        {step === 0 && (
          <section className="flex flex-col gap-2">
            <h2 className="text-sm font-medium text-[var(--color-text)]">总库位置</h2>
            <p className="text-sm leading-6 text-[var(--color-text-secondary)]">
              茶馆把导入的素材统一复制到「总库」里管理。现在不设置也能浏览已有素材，
              但导入前必须先选择总库位置。
            </p>
            <div className="flex flex-wrap items-center gap-2">
              <input
                readOnly
                aria-label="总库位置"
                value={formatPathForDisplay(libraryRoot)}
                placeholder="未配置"
                className="ui-control w-64 rounded-md px-2 py-1.5 text-sm outline-none"
              />
              <Button disabled={picking} onClick={() => void chooseLibraryRoot()}>
                {picking ? "选择中…" : "选择…"}
              </Button>
            </div>
            {!libraryRoot && (
              <p className="text-xs text-[var(--color-status)]">
                还没有配置总库位置。跳过向导后，入库页会提示你先到设置里选择。
              </p>
            )}
            {localError && (
              <p role="alert" className="text-xs text-[var(--color-danger)]">
                {localError}
              </p>
            )}
          </section>
        )}

        {step === 1 && (
          <section className="flex flex-col gap-2">
            <h2 className="text-sm font-medium text-[var(--color-text)]">AI 服务</h2>
            <p className="text-sm leading-6 text-[var(--color-text-secondary)]">
              AI 服务用来做自动打标、超级搜索和助手对话。不配置也能用入库、素材库和手动标签；
              这些功能不会因为没有 AI 服务而失败。服务在设置里管理，密钥只保存在系统凭据中，界面不会回显。
            </p>
            <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-control)] border border-[var(--color-border)]">
              <li className="flex items-center gap-3 px-3 py-2 text-sm">
                <span className="text-[var(--color-text-secondary)]">已配置服务</span>
                <span className="ml-auto text-[var(--color-text)]">
                  {!bindingLoaded ? "正在读取…" : `${connections.length} 个`}
                </span>
              </li>
              <li className="flex items-center gap-3 px-3 py-2 text-sm">
                <span className="text-[var(--color-text-secondary)]">助手使用的服务</span>
                <span className="ml-auto text-[var(--color-text)]">
                  {!bindingLoaded
                    ? "正在读取…"
                    : agentBound
                      ? (connections.find((item) => item.id === agentBound)?.name ?? "已绑定（服务已不存在）")
                      : "尚未绑定"}
                </span>
              </li>
            </ul>
            {/* A3-06：服务无效 / 密钥不可用 / 本机离线都要在这里就说清楚，
                不能等到用户发送第一条消息才发现；未配置 AI 也不影响手动流程。 */}
            {bindingLoaded && serviceIssues.length > 0 && (
              <ul className="flex flex-col gap-1 rounded-[var(--radius-control)] border border-[var(--color-status)] px-3 py-2 text-xs leading-5 text-[var(--color-text-secondary)]">
                {serviceIssues.map((issue) => (
                  <li key={issue}>{issue}</li>
                ))}
              </ul>
            )}
            <div>
              <Button
                disabled={picking}
                onClick={() => {
                  onOpenSettings();
                  void onFinish();
                }}
              >
                前往设置配置服务
              </Button>
            </div>
          </section>
        )}

        {step === 2 && (
          <section className="flex flex-col gap-3">
            <h2 className="text-sm font-medium text-[var(--color-text)]">基本操作</h2>
            <p className="text-sm leading-6 text-[var(--color-text-secondary)]">
              欢迎使用茶馆，是否查看使用教程？
            </p>
            <ul className="list-disc pl-5 text-xs leading-6 text-[var(--color-text-secondary)]">
              <li>入库：选择或拖入文件，确认计划后才会复制进总库。</li>
              <li>素材库：双击进入大图浏览，右键可打标、导出、移动。</li>
              <li>中央 Logo：进入助手。助手只能准备计划，执行必须由你在卡片上确认。</li>
            </ul>
            {tutorialUrl && (
              <div className="flex flex-col gap-2">
                <label htmlFor="tutorial-help-url" className="text-xs text-[var(--color-text-secondary)]">
                  自动打开失败时，可复制此地址到浏览器：
                </label>
                <textarea
                  id="tutorial-help-url"
                  readOnly
                  rows={3}
                  value={tutorialUrl}
                  onFocus={(event) => event.currentTarget.select()}
                  className="ui-control w-full resize-y rounded-md px-2 py-1.5 text-xs"
                />
                <Button onClick={() => void onCopyTutorialUrl()}>
                  {tutorialCopied ? "已复制文档地址" : "复制文档地址"}
                </Button>
              </div>
            )}
          </section>
        )}
      </div>
    </Modal>
  );
}
