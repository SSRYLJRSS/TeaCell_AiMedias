/** 左侧栏（PRD v2.7）：「类型」区（全部/图片/视频/未打标）+「标签」区（树形导航）
 *  操作区（导入/导出/网盘）已删：导入走入库页，导出走选中操作条
 */
import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import TagTree from "./TagTree";
import MetadataPanel from "./MetadataPanel";
import TagManageDialog from "@/components/dialogs/TagManageDialog";
import { useLibraryStore } from "@/stores/libraryStore";
import type { AssetType } from "@/types/asset";

const TYPE_TABS: { key: string; label: string; assetType: AssetType }[] = [
  { key: "all", label: "全部", assetType: "all" },
  { key: "image", label: "图片", assetType: "image" },
  { key: "video", label: "视频", assetType: "video" },
];

export default function SideBar() {
  const { filter, setFilter } = useLibraryStore(useShallow((s) => ({ filter: s.filter, setFilter: s.setFilter })));
  const [manageOpen, setManageOpen] = useState(false);
  const [tagMode, setTagMode] = useState<"smart" | "metadata">("smart");
  const activeKey = filter.trashOnly ? "trash" : filter.untaggedOnly ? null : filter.assetType;

  return (
    // pb-32 = 底部安全区（M-11 第五次真机补证：占位高度 = 底栏 56 + 入口 48 + 间距 8 = 112，原 pb-32=96 在 1200×800/DPI 下仍差 20px，打标页最后一行实测被压 108×28px²，故抬到 pb-32=128）。收起态悬浮助手入口是 fixed 浮层，固定在左下角，
    // 会盖住左侧栏最底部约 65px 的可点内容。只给滚动内容加内边距不够——默认滚动位置下
    // 末行仍然落在入口下面（打标页批次列表就这样被压住）。把安全区放在 aside 上，
    // 滚动容器底边整体上移，任何滚动位置都不会有内容落到入口底下。
    <aside
      className="flex w-56 shrink-0 flex-col border-r border-[var(--color-border)] bg-[var(--color-bg)] pb-32"
      data-testid="library-side-panel"
    >
      {/* 类型区 */}
      <div className="border-b border-[var(--color-border)] px-3 py-3">
        <h3 className="ui-section-title px-2 pb-2">
          素材筛选
        </h3>
        <div className="flex flex-col gap-0.5">
          {TYPE_TABS.map((t) => (
            <button
              key={t.key}
              onClick={() => setFilter({ assetType: t.assetType, untaggedOnly: false, trashOnly: false })}
              data-active={activeKey === t.key}
              className="ui-nav-item px-2.5 py-2 text-left text-sm text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
            >
              {t.label}
            </button>
          ))}
          {/* R-22 回收站入口 */}
          <div className="my-1 border-t border-[var(--color-border)]" />
          <button
            onClick={() => setFilter({ trashOnly: true, untaggedOnly: false, tagId: null, assetType: "all" })}
            data-active={activeKey === "trash"}
            className="ui-nav-item px-2.5 py-2 text-left text-sm text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
          >
            回收站
          </button>
        </div>
      </div>

      {/* 标签区（FB4-02）：固定头部与滚动正文为兄弟节点，禁止把标题放回滚动层。
          头部不在滚动容器内，正文层只允许纵向滚动（overflow-x-hidden），杜绝顶部漏字与横向滚动条。 */}
      <div className="flex min-h-0 flex-1 flex-col">
        <div className="shrink-0 border-b border-[var(--color-border)] bg-[var(--color-bg)] px-4 py-3">
          <h3 className="text-base font-semibold text-[var(--color-text)]">
            标签
          </h3>
          <div className="mt-2 grid grid-cols-2 rounded-lg bg-[var(--color-surface)] p-0.5" aria-label="标签类型">
            <button
              type="button"
              onClick={() => setTagMode("smart")}
              data-active={tagMode === "smart"}
              className="rounded-md px-2 py-1.5 text-xs text-[var(--color-text-secondary)] transition-colors data-[active=true]:bg-[var(--color-surface-raised)] data-[active=true]:font-semibold data-[active=true]:text-[var(--color-text)] data-[active=true]:shadow-sm"
            >
              智能标签
            </button>
            <button
              type="button"
              onClick={() => setTagMode("metadata")}
              data-active={tagMode === "metadata"}
              className="rounded-md px-2 py-1.5 text-xs text-[var(--color-text-secondary)] transition-colors data-[active=true]:bg-[var(--color-surface-raised)] data-[active=true]:font-semibold data-[active=true]:text-[var(--color-text)] data-[active=true]:shadow-sm"
              title="由文件格式、拍摄时间和 EXIF 信息自动生成"
            >
              文件属性
            </button>
          </div>
        </div>
        {/* 滚动正文自身不再留底部安全区——安全区由 aside（library-side-panel）承担，
            否则安全区会被算在滚动内容里，滚到底时多出一段空白却仍挡不住默认位置的行。 */}
        <div
          className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-2 py-2"
          data-testid="library-tag-scroll"
        >
          {tagMode === "smart"
            ? <TagTree onManage={() => setManageOpen(true)} />
            : <MetadataPanel />}
        </div>
      </div>
      <TagManageDialog open={manageOpen} onClose={() => setManageOpen(false)} />
    </aside>
  );
}
