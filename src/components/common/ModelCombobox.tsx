/**
 * ModelCombobox（FB5-04 §3.6）：可输入模型 combobox，替代旧 select/ModelSelect。
 *  - 输入框始终可编辑：支持列表之外的自定义模型名，手填值不在列表时保留并标记「自定义」；
 *  - 下拉列表去重 + 不区分大小写排序（后端 discover_models 已做，前端再兜底去重）；
 *  - 列表最大高度 240px，超出内部滚动；
 *  - 键盘：↑/↓ 导航、Enter 选中、Esc 关闭；点击外部关闭；
 *  - 刷新按钮使用 lucide RefreshCw + tooltip（不带「刷新」文字）；
 *  - 不在每次键盘输入时请求网络；请求中 loading 不改变当前 model；
 *  - 代际守卫：旧请求（如旧 baseUrl）的返回不覆盖新结果；
 *  - 获取失败显示可读原因，输入框仍可用。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { Check, ChevronDown, RefreshCw } from "lucide-react";

interface ModelComboboxProps {
  value: string;
  onChange: (v: string) => void;
  /** 拉取模型列表（失败抛错，message 为可读分类文案） */
  onDiscover: () => Promise<string[]>;
  placeholder?: string;
  /** 未获取过列表时刷新按钮的 aria-label 前缀（如「读取模型列表」）；已有列表时「刷新模型列表」 */
  label?: string;
  disabled?: boolean;
  /** 禁用远端发现时仍允许用户手动输入模型名称。 */
  discoverDisabled?: boolean;
  /** 服务协议、地址或密钥变化时递增，旧模型列表和在途请求随之失效。 */
  sourceRevision?: number | string;
}

export default function ModelCombobox({ value, onChange, onDiscover, placeholder = "例如 gpt-4.1-mini", label = "模型", disabled, discoverDisabled = false, sourceRevision }: ModelComboboxProps) {
  const [models, setModels] = useState<string[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const loadSeq = useRef(0);
  const sourceRevisionRef = useRef(sourceRevision);

  if (sourceRevisionRef.current !== sourceRevision) {
    sourceRevisionRef.current = sourceRevision;
    // 同步使当前在途请求立即过期，不等 effect 执行后才作废。
    loadSeq.current += 1;
  }

  useEffect(() => {
    setModels([]);
    setLoaded(false);
    setLoading(false);
    setError(null);
    setOpen(false);
    setHighlight(-1);
  }, [sourceRevision]);

  // 列表项 = 去重后的模型；手填值不在列表中时附加「自定义：xxx」
  const list = useCallback((): string[] => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const m of models) {
      const k = m.trim();
      if (!k || seen.has(k.toLowerCase())) continue;
      seen.add(k.toLowerCase());
      out.push(k);
    }
    return out;
  }, [models]);
  const trimmed = value.trim();
  const isCustom = trimmed !== "" && !list().some((m) => m === trimmed);
  const items = isCustom ? [...list(), `自定义：${trimmed}`] : list();

  const load = useCallback(async () => {
    if (disabled || discoverDisabled) return;
    setError(null);
    setLoading(true);
    const seq = ++loadSeq.current;
    try {
      const m = await onDiscover();
      // 代际守卫：期间又发起了新请求（如改了 baseUrl）→ 旧结果不应用（§13.5）
      if (seq !== loadSeq.current) return;
      setModels(m);
      setLoaded(true);
      setOpen(true);
      setHighlight(m.findIndex((x) => x === trimmed));
    } catch (e) {
      if (seq !== loadSeq.current) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  }, [onDiscover, disabled, discoverDisabled, trimmed]);

  // 点击外部关闭
  useEffect(() => {
    if (!open) return;
    const onDocDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDocDown);
    return () => document.removeEventListener("mousedown", onDocDown);
  }, [open]);

  const select = (m: string) => {
    onChange(m);
    setOpen(false);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) setOpen(true);
      setHighlight((h) => Math.min(h + 1, items.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlight((h) => Math.max(h - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (open && highlight >= 0 && items[highlight]) {
        select(items[highlight].startsWith("自定义：") ? trimmed : items[highlight]);
      } else {
        setOpen(false);
      }
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  };

  const inputCls =
    "ui-control h-9 w-full min-w-0 rounded-md pr-7 text-sm outline-none focus:border-[var(--color-accent)]";

  return (
    <div ref={rootRef} className="relative flex min-w-0 flex-1 flex-col">
      <div className="flex min-w-0 items-center gap-1.5">
        <div className="relative min-w-0 flex-1">
          <input
            ref={inputRef}
            className={inputCls}
            value={value}
            disabled={disabled}
            placeholder={placeholder}
            onChange={(e) => onChange(e.target.value)}
            onFocus={() => setOpen(true)}
            onKeyDown={onKeyDown}
            aria-label={label}
            aria-expanded={open}
            role="combobox"
            autoComplete="off"
            spellCheck={false}
          />
          <ChevronDown
            size={14}
            strokeWidth={1.75}
            aria-hidden="true"
            className="pointer-events-none absolute top-1/2 right-2 -translate-y-1/2 text-[var(--color-text-tertiary)]"
          />
        </div>
        <button
          type="button"
          onClick={() => void load()}
          disabled={disabled || discoverDisabled}
          aria-label={loaded ? "刷新模型列表" : "读取模型列表"}
          title={loaded ? "刷新模型列表" : "读取模型列表"}
          className={clsx(
            "flex size-7 shrink-0 items-center justify-center rounded-md text-[var(--color-text-secondary)] transition-colors",
            "hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:opacity-50",
          )}
        >
          <RefreshCw size={14} strokeWidth={1.75} aria-hidden="true" className={clsx(loading && "animate-spin")} />
        </button>
      </div>

      {error && <span className="mt-1 text-xs text-[var(--color-danger)]">{error}</span>}

      {open && !loading && items.length > 0 && (
        <div
          role="listbox"
          aria-label={`${label}列表`}
          className="absolute top-full right-0 left-0 z-20 mt-1 overflow-y-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface-raised)] py-1 shadow-lg"
          style={{ maxHeight: 240 }}
        >
          {items.map((m, i) => {
            const isSel = m === value || (m.startsWith("自定义：") && trimmed === value);
            const isCustomItem = m.startsWith("自定义：");
            return (
              <button
                key={m}
                type="button"
                role="option"
                aria-selected={isSel}
                onMouseEnter={() => setHighlight(i)}
                onClick={() => select(isCustomItem ? trimmed : m)}
                className={clsx(
                  "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm transition-colors",
                  highlight === i ? "bg-[var(--color-surface-hover)] text-[var(--color-text)]" : "text-[var(--color-text)]",
                  isCustomItem && "text-[var(--color-text-secondary)] italic",
                )}
              >
                <span className="w-4 shrink-0">{isSel && <Check size={13} strokeWidth={2} aria-hidden="true" />}</span>
                <span className="min-w-0 flex-1 truncate">{m}</span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
