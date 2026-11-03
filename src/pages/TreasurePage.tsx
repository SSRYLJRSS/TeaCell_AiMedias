/**
 * 百宝箱说明页（PRD 第 8 节 / A3-05）。
 *
 * 2.0 只提供占位说明：可见、可进入，但不加载任何插件、不读取任意 URL 或可执行文件。
 * 真正的插件宿主属于 2.5，届时另行冻结契约；这里刻意不建立任何扩展点，
 * 避免提前把「可选入口」演变成任意代码执行面。
 */
import Button from "@/components/common/Button";

export default function TreasurePage() {
  return (
    <section className="flex h-full flex-col items-center justify-center gap-4 px-6 py-10">
      <h1 className="text-lg font-medium text-[var(--color-text)]">百宝箱尚未开放</h1>
      <p className="max-w-[440px] text-center text-sm leading-relaxed text-[var(--color-text-secondary)]">
        这里将用于收纳后续版本提供的扩展能力。2.0 版本暂不加载任何插件，也不会从网络或本地读取可执行扩展。
      </p>
      <p className="max-w-[440px] text-center text-xs leading-relaxed text-[var(--color-text-tertiary)]">
        你可以在「设置 → 外观与浏览 → 底栏入口」中调整这一入口是否显示；隐藏它不会删除任何功能或数据。
      </p>
      <Button onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }))}>
        前往设置
      </Button>
    </section>
  );
}
