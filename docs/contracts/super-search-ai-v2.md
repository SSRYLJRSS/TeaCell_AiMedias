# 超级搜索 AI 查询协议（V2 输入兼容 / V3 执行桥接）

> 状态：Frozen
>
> 更新日期：2026-09-28
>
> 机器协议，不可随意变更。变更必须同步 Rust schema、TypeScript 类型和测试矩阵。

本文档定义 AI 输入侧的 V2 兼容协议，以及它到 V3 执行计划的桥接规则：
`SearchIntentV2`/`SearchIntentV3` 结构（包括概念和元数据软条件）、JSON Schema enum 收窄、三层降级、部分剔除规则、
`facet_has_any` / `facet_missing` 语义。列表、总数、全选、排序、warning 和诊断的唯一事实源
以 [SearchPlanV3 执行契约](search-plan-v3.md) 为准。

---

## 1. 两层查询对象（V2）

```
SearchIntentV2 / V3      ← AI 生成（V3 额外包含 preferred/preferredMetadata/evidence/termMatch）
   ↓ 后端解析 + 三层降级 + 校验
QueryExpr               ← plan.filter 的兼容视图（供旧 UI/链路使用）
SearchPlanV3             ← 唯一执行事实源（filter/mustNot/should/ranking）
```

- AI 只生成受 schema 限制的查询意图，不生成 tagId / assetId / SQL / 分页；V3 在 V2 条件结构上增加软偏好字段。
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
        { "text": "包装品牌名", "role": "", "facetHint": "brand_info", "confidence": 0.9 }
      ],
      "textTerms": [ { "text": "IMG_1097", "scope": "fileName" } ],
      "metadata": [ { "key": "file_size", "op": "between", "min": 10485760, "max": 110100480 } ]
    }
  ],
  "exclusions": [ { "text": "模糊", "role": "", "facetHint": null, "confidence": 0.9 } ],
  "sortBy": "created_at",            // 或 null
  "sortDir": "desc"                  // asc | desc | null
}
```

语义：组内 AND、组间 OR；exclusions 全局 NOT；metadata 用规范 key/op/单位。

超级搜索系统提示词由应用内置并维护，用户不可用自由文本替换其 SearchIntentV2 机器协议。用户可编辑的分类说明随请求提供，并决定分类语义；分类 key 不隐含业务含义。旧版 `systemPromptSearch` 设置不参与提示词构造。

示例中的 `brand_info` 假设为用户自建分类。实际请求必须根据同一请求中随 user prompt 发送的分类说明选择 `facetHint`；`key` 名称和历史默认分类不定义业务语义。无法根据说明判断时允许使用 `null`。

### 2.1 JSON Schema enum 收窄

- `facetHint`：enum = **实时分面 key**（新建分面后自动包含；支持 json_schema 的服务商在服务端拒绝非法 key）。
- `metadata.key`：enum = METADATA_KEYS 白名单常量。
- `metadata.op`：enum = METADATA_OPS（eq/in/gt/gte/lt/lte/between/contains）。
- `textTerms.scope`：enum = all/content/description/fileName。
- `assetType`：enum = all/image/video。
- `sortDir`：enum = asc/desc。

### 2.2 V3 软条件

V3 每个 group 在 V2 的 `metadata` 之外增加 `preferredMetadata`。两者都使用同一
`MetadataFilter` 字段形状（`key`、`op`、`value`、`values`、`min`、`max`）；软条件另外包含
`evidence`（原文依据）和 `weight`（0.5、1.0 或 2.0）。缺失 `preferredMetadata` 的旧响应按空数组兼容。

- `metadata` 是硬条件：进入 `SearchPlanV3.filter`，不满足即不返回素材。
- `preferredMetadata` 是软条件：进入 `SearchPlanV3.should`，满足只加权排序，不满足仍保留素材。
- 算法色相、饱和度和明度沿用元数据白名单。诸如“最好主要是绿色”的色彩要求必须进
  `preferredMetadata`，不能成为硬过滤；用户明确要求“绿色”时才进 `metadata`。
- 若模型把明确位于偏好短语中的标准色相区间误放入 `metadata`，后端仅在该颜色没有在偏好短语外再次出现时纠正为软条件；跨短语重复或无法精确匹配的条件保留原语义。
- 软元数据依据必须是用户原句子串；非法条件或无效证据剔除并产生 warning，不得升级为硬条件。

示例：

```json
{
  "metadata": [],
  "preferredMetadata": [{
    "key": "dominant_hue", "op": "between", "value": null, "values": null,
    "min": 70, "max": 155, "evidence": "最好主要是绿色", "weight": 1.0
  }]
}
```

## 3. 模型内容降级与请求错误

只有 HTTP 成功并取得完整模型内容后，解析/校验失败才使用下表进行内容降级。服务返回 `finish_reason=length` 或 Anthropic `stop_reason=max_tokens` 表示模型输出被截断，必须返回 `AI_OUTPUT_TRUNCATED`，不得把半截 JSON 按整句关键词执行。**“永不红字”不适用于服务请求失败或输出截断。**
结构化输出协议兼容是独立的请求层行为：Structured、JsonObject、Plain 每级最多请求一次；仅当服务端在 HTTP 400/404/422 响应中明确指出 `response_format`、`json_schema`、`json_object`、`tool_choice`、`input_schema` 或同类格式字段不受支持时，才尝试下一层。普通 400、上下文超限和其他未识别错误立即返回。

| 层 | 触发 | 结果 |
|---|---|---|
| ① strict | AI 返回合法 JSON + 全部条件合规 | 正常解析执行 |
| ② lenient | 部分条件非法（未知 facetHint / 非法 op / 非法值 / 空组） | 剔除非法项保留其余 + warning |
| ③ keyword | HTTP 成功但模型内容非 JSON / 为空 / groups 无效（即使存在 exclusions）/ 剔除后全空 / 结构校验失败 | `QueryExpr::Leaf{Search{原句}}` + explanation「按关键词搜索」 |

`finish_reason=length`、Anthropic `stop_reason=max_tokens` 或空的最终输出因输出上限终止时，属于截断而非可用的完整模型内容：返回 `AI_OUTPUT_TRUNCATED`，保持当前搜索条件和结果不变，并提示用户缩短查询或更换模型。日志只记录结束原因、响应字符数、模型标识和耗时，不写密钥、原始响应或完整查询。仅当服务确认完整结束但内容本身为空/非法时，才按表中第 ③ 层处理。

**服务请求错误**：401/403、429、超时、连接失败、5xx、普通 400 和未知请求错误均返回失败，不转成关键词搜索。保留 `UNAUTHORIZED`、`AI_RATE_LIMITED`、`TIMEOUT` 等机器码；其他 HTTP 服务失败保留 `INTERNAL`。前端应说明本次解析失败，不得把失败结果表现为本次关键词搜索成功。错误处理不得依赖解析本地化错误文本。

## 3.2 AI 服务解析策略

- 存在 `super_search` 显式绑定时只解析该连接；连接停用、配置不完整或密钥不可用时失败并提示，不静默换用其他连接。
- 未显式绑定时，在已启用的云端连接中自动选择配置完整且凭据可用的一条；旧版 `settings.ai.activeProfile` 不参与超级搜索服务选择。
- 未显式绑定且找不到可用在线连接时返回可操作的配置提示，不自动回退到本地 Ollama 或其他本地模型。用户可以在设置中明确绑定本地连接。
- 设置页的“当前实际使用”由后端解析结果提供，状态响应不包含 API Key 或服务地址。

### 3.1 部分剔除规则（原则「能救一条算一条」）

| 输入 | 处理 |
|---|---|
| `sortBy` 非法 | → 默认（created_at）+ warning |
| `sortDir` 非法 | → 默认（desc）+ warning |
| `assetType` 非法 | → all + warning |
| 单条 `metadata` 非法 | → 剔除该条保留其他 + warning |
| 单条 `preferredMetadata` 非法、编译失败或 evidence 不在原文 | → 剔除该软条件 + warning，不改变硬筛选 |
| `concept.facetHint` 未知 | → 清空 hint（降级全分面搜索）+ warning |
| preferred evidence 缺失、编造或经规范名/可搜索别名复核后与 concept 无关 | → 忽略该 preferred + warning，**不得升级为 required** |
| 原文偏好短语中的概念被模型放入 `concepts` | → 用本轮标签词典的规范名/可搜索别名复核；仅在该概念没有偏好短语外的明确硬条件时移到 `preferred` + warning |
| 原文明确包含文件大小、时长或横竖构图单位/词，但对应 metadata 缺失 | → 保留已解析条件 + warning，不静默声称条件已生效 |
| 空 concept / 空 textTerm | → 剔除 |
| 某 group 全空（且没有任何概念或元数据软偏好） | → 剔除该 group + warning |
| 只有 preferred / preferredMetadata 的 group | → 保留空 filter 占位，使软条件继续进入 `SearchPlanV3.should`，允许全库软排序 |
| 全部 group 被剔除 / 结构校验失败 | → 落第 3 层 |

## 4. 执行对象补充（facet_has_any / facet_missing）

除精确标签筛选外，V2 支持两类「缺 / 有」条件：

- `facet_has_any(facetKey)`：该分面下**至少有一个**标签的素材（例如用户自建的 `brand_info` 分面）。
- `facet_missing(facetKey)`：该分面下**没有任何**标签的素材；分面业务含义由当前用户配置决定。

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
