/**
 * ViewerInfoSidebar（指导书 §2.3/§4.1）：查看器左属性栏。
 *  只保留 属性（通用 / 图片 / 视频分组）。标签区 FB2-04 已收敛到 ViewerViewport 下方的
 *  ViewerTagBar（同一页只保留一个编辑入口，避免双入口状态分叉，§10.3）。
 *
 * M-14b（L-01 主链路 / PRD R-16 / QA_PLAYBOOK F-05、J-01）：恢复「收藏 + 1–5 星评级」入口。
 *  `de1fdb9`、`683628a` 两次大重构把前端入口整体删掉了（后端 `set_rating` 命令与
 *  `assets.rating/favorite` 列一直保留），导致 L-01 的「查看器评级」这一步无路可走。
 *
 *  写入统一由 ViewerPage 负责：那里同时承载键盘 1–5/0/F 与乐观更新、失败回滚与播报。
 *  本组件只做展示与回调，避免「点星」和「按快捷键」走两套写库逻辑而状态分叉。
 */
import clsx from "clsx";
import AssetInfoPanel from "@/components/library/AssetInfoPanel";
import type { Asset } from "@/types/asset";

interface ViewerInfoSidebarProps {
  asset: Asset;
  onRefreshed: (asset: Asset) => void;
  /** 设评级（0 = 清除） */
  onRate: (rating: number) => void;
  /** 切换收藏 */
  onToggleFavorite: () => void;
  /** 写入进行中（键盘与点星共用同一 busy 语义） */
  busy?: boolean;
}

const STARS = [1, 2, 3, 4, 5] as const;

export default function ViewerInfoSidebar({ asset, onRefreshed, onRate, onToggleFavorite, busy = false }: ViewerInfoSidebarProps) {
  const rating = asset.rating ?? 0;
  const favorited = asset.favorite === 1;

  return (
    <div className="flex flex-col gap-4">
      {/* 收藏 + 评级（PRD R-16）：点当前星标可清零，与超级搜索的 0 = 未评级语义一致 */}
      <section>
        <h4 className="mb-1.5 text-[10px] font-medium tracking-wide text-[var(--color-text-secondary)] uppercase">
          评级
        </h4>
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-0.5" role="group" aria-label="评级">
            {STARS.map((star) => {
              const on = rating >= star;
              return (
                <button
                  key={star}
                  type="button"
                  disabled={busy}
                  aria-label={`设为 ${star} 星`}
                  aria-pressed={on}
                  title={on && rating === star ? `清除评级（当前 ${star} 星）` : `设为 ${star} 星`}
                  onClick={() => onRate(rating === star ? 0 : star)}
                  className={clsx(
                    "rounded px-0.5 text-sm leading-none transition-colors disabled:opacity-50",
                    on ? "text-[var(--color-accent)]" : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
                  )}
                >
                  {on ? "★" : "☆"}
                </button>
              );
            })}
          </div>
          <button
            type="button"
            disabled={busy}
            aria-label={favorited ? "取消收藏" : "收藏"}
            aria-pressed={favorited}
            onClick={onToggleFavorite}
            className={clsx(
              "rounded px-1 text-sm leading-none transition-colors disabled:opacity-50",
              favorited ? "text-[var(--color-accent)]" : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
            )}
            title={favorited ? "取消收藏" : "收藏"}
          >
            {favorited ? "★" : "☆"}
          </button>
        </div>
      </section>

      {/* 属性（通用/图片/视频分组） */}
      <section>
        <h4 className="mb-1.5 text-[10px] font-medium tracking-wide text-[var(--color-text-secondary)] uppercase">属性</h4>
        <AssetInfoPanel asset={asset} onRefreshed={onRefreshed} />
      </section>
    </div>
  );
}
