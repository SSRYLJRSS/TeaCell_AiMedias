import { useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { Search } from "lucide-react";

interface SearchInputProps {
  placeholder?: string;
  debounceMs?: number;
  onSearch: (keyword: string) => void;
  /** 宽度由调用方决定：工具栏默认 w-64，侧栏等窄容器传 w-full。 */
  className?: string;
}

/** 搜索框：内置防抖（默认 300ms，对应架构 §4 搜索流程） */
export default function SearchInput({
  placeholder = "搜索标签 / 文件名…",
  debounceMs = 300,
  onSearch,
  className,
}: SearchInputProps) {
  const [value, setValue] = useState("");
  // F18：挂载时（或 onSearch 引用变化导致 effect 重启时）不得把初始空值提交上去——
  // 否则页面刚加载/顶栏重渲染后会误发 setFilter({search:""})，触发 B09 清空选中
  const mounted = useRef(false);

  useEffect(() => {
    if (!mounted.current) {
      mounted.current = true;
      return;
    }
    const timer = setTimeout(() => onSearch(value.trim()), debounceMs);
    return () => clearTimeout(timer);
  }, [value, debounceMs, onSearch]);

  return (
    <div className={clsx("relative flex items-center", className ?? "w-64")}>
      <Search
        size={14}
        aria-hidden="true"
        className="pointer-events-none absolute left-2.5 text-[var(--color-text-tertiary)]"
      />
      <input
        type="search"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        placeholder={placeholder}
        className="ui-control w-full min-w-0 py-2 pl-8 pr-3 text-sm placeholder:text-[var(--color-text-secondary)]"
      />
    </div>
  );
}
