/** 超级搜索唯一主输入框：普通关键词与自然语言条件统一从这里进入。
 *  FB5-05（§9.7）：AI 解析失败只显示 aiError，不自动把整句执行为全文搜索；
 *  提供明确命令「按原文搜索」，只有用户点击后才生成全文条件。
 *  FB6 需求五：搜索框上方显示「超级搜索」页面标识（唯一可见标题，字号克制，非营销式 hero）。
 *  W6-5（§W6-5）：warning 一律黄字列表；解释按 parseStatus 区分三态 ——
 *  full = 蓝字解释；partial = 黄字「部分条件无法理解」；keyword = 黄字「按关键词搜索」。 */
import { FormEvent, useEffect } from "react";
import { LoaderCircle } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { onAiSearchProgress } from "@/api/superSearch";
import { useSuperSearchStore } from "@/stores/superSearchStore";

const EXAMPLES = ["2025 年拍的横图海边素材，大于 5MB", "Sony 拍的视频，时长 10 秒以上", "清新的竖版人像，不要夜景"];

interface AiSearchBarProps { onSubmit: (text: string) => void; }

export default function AiSearchBar({ onSubmit }: AiSearchBarProps) {
  const { aiInput, setAiInput, aiLoading, aiError, aiExplanation, warnings, parseStatus, setQuery, aiRequestId, aiPhase, aiElapsedMs, aiCancelPending, handleAiProgress, cancelAiSearch, tickAiElapsed } = useSuperSearchStore(
    useShallow((s) => ({
      aiInput: s.aiInput,
      setAiInput: s.setAiInput,
      aiLoading: s.aiLoading,
      aiError: s.aiError,
      aiExplanation: s.aiExplanation,
      warnings: s.warnings,
      parseStatus: s.parseStatus,
      setQuery: s.setQuery,
      aiRequestId: s.aiRequestId,
      aiPhase: s.aiPhase,
      aiElapsedMs: s.aiElapsedMs,
      aiCancelPending: s.aiCancelPending,
      handleAiProgress: s.handleAiProgress,
      cancelAiSearch: s.cancelAiSearch,
      tickAiElapsed: s.tickAiElapsed,
    })),
  );
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void onAiSearchProgress(handleAiProgress).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    }).catch(() => undefined);
    return () => { active = false; unlisten?.(); };
  }, [handleAiProgress]);
  useEffect(() => {
    if (!aiRequestId || (!aiLoading && !aiCancelPending)) return;
    const timer = window.setInterval(tickAiElapsed, 1000);
    return () => window.clearInterval(timer);
  }, [aiRequestId, aiLoading, aiCancelPending, tickAiElapsed]);
  const submit = (event?: FormEvent) => { event?.preventDefault(); const text = aiInput.trim(); if (text) onSubmit(text); };
  // §9.7：只有用户点击「按原文搜索」才生成全文条件（scope=all 整句）
  const runRawSearch = () => { const text = aiInput.trim(); if (text) setQuery({ search: text }); };
  const keywordMode = parseStatus === "keyword";
  const partialMode = parseStatus === "partial" || (!keywordMode && warnings.length > 0);
  const phaseLabel = aiPhase === "queued" ? "等待 AI 服务" : aiPhase === "requesting" ? "正在请求并等待模型响应" : aiPhase === "validating" ? "正在校验搜索条件" : aiPhase === "cancelling" ? "已停止接收结果，正在结束当前请求" : aiPhase === "cancelled" ? "已取消，本次解析结果未应用" : aiPhase === "completed" ? "解析完成" : "正在解析";
  const elapsedLabel = `${Math.floor(aiElapsedMs / 1000)} 秒`;
  return <div className="flex flex-col gap-2">
    <h1 className="text-center text-sm font-medium tracking-wide text-[var(--color-text)]">超级搜索</h1>
    <form onSubmit={submit} className="relative">
      <input type="search" value={aiInput} onChange={(e) => setAiInput(e.target.value)} placeholder="搜索文件名、标签，或描述你想找的素材" className="ui-control h-12 w-full pl-4 pr-24 text-sm placeholder:text-[var(--color-text-secondary)]" aria-label="超级搜索" />
      <button type="submit" disabled={aiLoading || !aiInput.trim()} className="absolute inset-y-1.5 right-1.5 min-w-20 rounded bg-[var(--color-text)] px-3 text-xs font-medium text-[var(--color-bg)] transition-opacity hover:opacity-85 disabled:opacity-45">{aiLoading ? "解析中" : "搜索"}</button>
    </form>
    {(aiLoading || aiCancelPending || aiPhase === "cancelled") && (
      <div className="flex min-w-0 items-center gap-2 text-[11px] text-[var(--color-text-secondary)]" role="status" aria-live="polite">
        {aiLoading && <LoaderCircle className="size-3.5 shrink-0 animate-spin" aria-hidden="true" />}
        <span className="min-w-0 flex-1">{phaseLabel} · 已用时 {elapsedLabel}</span>
        {(aiLoading || aiCancelPending) && <button type="button" onClick={() => void cancelAiSearch()} disabled={aiCancelPending} className="shrink-0 rounded border border-[var(--color-border)] px-2 py-0.5 text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] disabled:opacity-50">{aiCancelPending ? "正在停止" : "取消解析"}</button>}
      </div>
    )}
    <div className="flex min-w-0 items-center gap-2 overflow-x-auto whitespace-nowrap text-[11px] text-[var(--color-text-tertiary)]">
      <span className="shrink-0">试试</span>
      {EXAMPLES.map((example) => <button key={example} type="button" onClick={() => setAiInput(example)} className="shrink-0 text-[var(--color-text-secondary)] hover:text-[var(--color-text)]">{example}</button>)}
    </div>
    {aiError ? (
      // 服务请求错误如实失败；上次有效条件与结果保持不变。
      <div className="flex flex-wrap items-center gap-2 border-l-2 border-[var(--color-danger)] pl-2 text-[11px] leading-5 text-[var(--color-danger)]">
        <p className="min-w-0 flex-1">AI 解析失败，本次未应用；当前条件与结果保持不变：{aiError}</p>
        <button type="button" onClick={runRawSearch} className="shrink-0 rounded border border-[var(--color-border)] px-2 py-0.5 text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]">
          按原文搜索
        </button>
      </div>
    ) : (aiExplanation || warnings.length > 0) && (
      // W6-5：黄字 warning 列表；三态解释文案
      <div className="border-l-2 pl-2 text-[11px] leading-5"
        style={{ borderColor: "var(--color-status)", color: "var(--color-text-secondary)" }}
      >
        {keywordMode ? (
          <p className="text-[var(--color-status)]">未能理解搜索条件，已按关键词搜索：{aiInput.trim()}</p>
        ) : partialMode ? (
          <p className="text-[var(--color-status)]">部分条件未能准确理解，已按能识别的部分搜索：{aiExplanation}</p>
        ) : aiExplanation ? (
          <p>AI 已转换为下方条件：{aiExplanation}</p>
        ) : null}
        {warnings.map((warning) => (
          <p key={warning} className="text-[var(--color-status)]">{warning}</p>
        ))}
      </div>
    )}
  </div>;
}

