# 超级搜索 AI 查询协议（V2 输入兼容 / V3 执行桥接）

> 状态：Frozen
>
> 更新日期：2026-09-19
>
> 机器协议，不可随意变更。变更必须同步 Rust schema、TypeScript 类型和测试矩阵。

本文档定义 AI 输入侧的 V2 兼容协议，以及它到 V3 执行计划的桥接规则：
`SearchIntentV2`/`SearchIntentV3` 结构、JSON Schema enum 收窄、三层降级、部分剔除规则、
`facet_has_any` / `facet_missing` 语义。列表、总数、全选、排序、warning 和诊断的唯一事实源
以 [SearchPlanV3 执行契约](search-plan-v3.md) 为准。

---

## 1. 两层查询对象（V2）

```
SearchIntentV2 / V3      ← AI 生成（V3 额外包含 preferred/evidence/termMatch）
   ↓ 后端解析 + 三层降级 + 校验
QueryExpr               ← plan.filter 的兼容视图（供旧 UI/链路使用）
SearchPlanV3             ← 唯一执行事实源（filter/mustNot/should/ranking）
```

- AI 只生成 `SearchIntentV2`，不生成 tagId / assetId / SQL / 分页。
- 后端 `build_expr_from_v2` / `build_plan_from_v3` 把 intent 解析成兼容视图与执行计划；
  前端的 `expr` 只是 `plan.filter` 的派生视图。
- 排序单独返回（sortBy / sortDir），与 expr 并列。

## 2. SearchIntentV2（AI 允许输出的形状）

```json
{
  "groups": [
    {
      "assetType": "all",            // all | image | video
      "concepts": [
        { "text": "海边", "role": "scene", "facetHint": "scene", "confidence": 0.9 }
      ],
      "textTerms": [ { "text": "IMG_1097", "scope": "fileName" } ],
      "metadata": [ { "key": "file_size", "op": "between", "min": 10485760, "max": 110100480 } ]
    }
  ],
  "exclusions": [ { "text": "夜景", "role": "lighting", "facetHint": "lighting", "confidence": 0.9 } ],
  "sortBy": "created_at",            // 或 null
  "sortDir": "desc"                  // asc | desc | null
}
```

语义：组内 AND、组间 OR；exclusions 全局 NOT；metadata 用规范 key/op/单位。

### 2.1 JSON Schema enum 收窄

- `facetHint`：enum = **实时分面 key**（新建分面后自动包含；支持 json_schema 的服务商在服务端拒绝非法 key）。
- `metadata.key`：enum = METADATA_KEYS 白名单常量。
- `metadata.op`：enum = METADATA_OPS（eq/in/gt/gte/lt/lte/between/contains）。
- `textTerms.scope`：enum = all/content/description/fileName。
- `assetType`：enum = all/image/video。
- `sortDir`：enum = asc/desc。

## 3. 三层降级（永不红字报错）

| 层 | 触发 | 结果 |
|---|---|---|
| ① strict | AI 返回合法 JSON + 全部条件合规 | 正常解析执行 |
| ② lenient | 部分条件非法（未知 facetHint / 非法 op / 非法值 / 空组） | 剔除非法项保留其余 + warning |
| ③ keyword | 非 JSON / 空 groups（即使存在 exclusions）/ 剔除后全空 / 结构校验失败 | `QueryExpr::Leaf{Search{原句}}` + explanation「按关键词搜索」，**永不失败** |

**例外**：`is_config_error`（鉴权 / 连不上 / 超时）仍真报错 —— 配置问题必须让用户知道。

### 3.1 部分剔除规则（原则「能救一条算一条」）

| 输入 | 处理 |
|---|---|
| `sortBy` 非法 | → 默认（created_at）+ warning |
| `sortDir` 非法 | → 默认（desc）+ warning |
| `assetType` 非法 | → all + warning |
| 单条 `metadata` 非法 | → 剔除该条保留其他 + warning |
| `concept.facetHint` 未知 | → 清空 hint（降级全分面搜索）+ warning |
| preferred evidence 缺失、编造或经规范名/可搜索别名复核后与 concept 无关 | → 忽略该 preferred + warning，**不得升级为 required** |
| 原文偏好短语中的概念被模型放入 `concepts` | → 用本轮标签词典的规范名/可搜索别名复核；仅在该概念没有偏好短语外的明确硬条件时移到 `preferred` + warning |
| 原文明确包含文件大小、时长或横竖构图单位/词，但对应 metadata 缺失 | → 保留已解析条件 + warning，不静默声称条件已生效 |
| 空 concept / 空 textTerm | → 剔除 |
| 某 group 全空（且没有 preferred） | → 剔除该 group + warning |
| 只有 preferred 的 group | → 保留空 filter 占位，使 preferred 继续进入 `SearchPlanV3.should`，允许全库软排序 |
| 全部 group 被剔除 / 结构校验失败 | → 落第 3 层 |

## 4. 执行对象补充（facet_has_any / facet_missing）

除精确标签筛选外，V2 支持两类「缺 / 有」条件：

- `facet_has_any(facetKey)`：该分面下**至少有一个**标签的素材（如「有主体标签的图片」）。
- `facet_missing(facetKey)`：该分面下**没有任何**标签的素材（如「没有场景标签的图片」）。

两者进 `QueryExpr` 作为 leaf 条件，前端与精确标签条件混排，后端 `query_expr.rs` 编译。

## 5. 关键不变量

1. `SearchPlanV3` 是唯一执行事实源；`expr` 只是 `plan.filter` 的兼容视图，
   前端不得从扁平 query 覆盖 AI 生成的复杂计划树。
2. AI 结果经本地 `guard_intent` 确定性守卫（OR 合并 / assetType 纠偏 / concept 清洗 / confidence 钳制）后才执行。
3. 解析状态三态：`full`（完全理解）/ `partial`（部分理解 + warning）/ `keyword`（按关键词搜索），前端据此渲染黄字而非红字。
4. metadata 条件校验以 `search_query::compile_metadata` 为唯一判定（sanitize 与执行层同一校验，绝不漂移）。

## 6. 相关文件

- 协议结构：`src-tauri/src/services/super_search_ai.rs`（SearchIntentV2/V3 / intent_schema / degrade_parse / sanitize_all / guard_intent）
- 执行计划：`src-tauri/src/db/search_plan.rs`（SearchPlanV3 校验、编译、分页、诊断）
- 执行编译：`src-tauri/src/db/query_expr.rs` + `src-tauri/src/db/search_query.rs`
- 命令壳：`src-tauri/src/commands/super_search_cmd.rs`
- 前端：`src/stores/superSearchStore.ts` + `src/components/supersearch/AiSearchBar.tsx`
