/** 重要异常单列区（A3-04）。
 *
 * 协议规定 Host 事件不充当持久状态，出错时更容易被后续正常消息淹没。
 * 这里把「发送结果未知」「旧实例迟到事件」「运行失败」单独列出，
 * 要求用户显式关闭，而不是随消息滚动消失。
 */
import clsx from "clsx";
import Button from "@/components/common/Button";
import { useAgentSessionStore } from "@/stores/agentSessionStore";

export default function AnomalyNotice({ conversationRef }: { conversationRef: string }) {
  const anomalies = useAgentSessionStore((s) => s.anomalies);
  const dismiss = useAgentSessionStore((s) => s.dismissAnomaly);
  const mine = anomalies.filter((item) => item.conversationRef === conversationRef);
  if (mine.length === 0) return null;

  return (
    <div className="border-b border-[var(--color-border)] bg-[var(--color-status-soft)] px-4 py-2">
      <ul className="mx-auto flex max-w-[760px] flex-col gap-1">
        {mine.map((item) => (
          <li key={item.id} className="flex items-start gap-2">
            <span
              className={clsx(
                "mt-0.5 shrink-0 text-[11px] font-medium",
                item.code === "OUTCOME_UNKNOWN" ||
                  item.code === "STALE_HOST_INSTANCE" ||
                  item.code === "HOST_INTERRUPTED"
                  ? "text-[var(--color-status)]"
                  : "text-[var(--color-danger)]",
              )}
            >
              {item.code === "OUTCOME_UNKNOWN"
                ? "结果待核对"
                : item.code === "STALE_HOST_INSTANCE"
                  ? "已忽略旧事件"
                  : item.code === "HOST_INTERRUPTED"
                    ? "回答被中断"
                    : "运行异常"}
            </span>
            <span className="min-w-0 flex-1 text-[11px] leading-5 text-[var(--color-text-secondary)]">
              {item.message}
            </span>
            <Button className="min-h-0 shrink-0 px-2 py-0.5 text-[11px]" onClick={() => dismiss(item.id)}>
              知道了
            </Button>
          </li>
        ))}
      </ul>
    </div>
  );
}
