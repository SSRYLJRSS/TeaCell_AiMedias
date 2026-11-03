# 架构说明

> 更新日期：2026-09-23
>
> 本文档描述代码当前实际结构和不可违反的设计约束。数据库、搜索、分面和 AI 协议变化时，必须同步更新本文及相关契约。

## 1. 总览

```text
React 19 + TypeScript
pages -> stores -> api -> Tauri invoke/events
                              |
                              v
Rust commands -> services -> db
                     |
                     +-> filesystem / image / video / HTTP / Ollama
```

目标不是“薄应用”，而是让每一层只承担一种责任：

- `pages/` 负责页面编排和组合。
- `stores/` 负责跨组件状态和动作。
- `api/` 是所有 Tauri 调用的唯一前端入口。
- `commands/` 负责 IPC 参数、锁、事件和异步边界。
- `services/` 负责可测试的业务逻辑。
- `db/` 负责 SQL、事务和迁移。

当前新增 `src-tauri/src/contracts/` 保存 Agent 边界类型，机器枚举由 `docs/contracts/agent-v1.json` 同源生成 Rust 与 TypeScript，并由 tooling 检查漂移。该模块目前不提供业务执行或授权；实现范围与未完成项见 [Agent v1契约](contracts/agent-v1.md)。

## 2. 前端结构

### 2.1 页面

| 页面 | 文件 | 职责 |
|---|---|---|
| 入库 | `src/pages/ImportPage.tsx` | 待入库清单、改名、托管选项、导入任务 |
| 素材库 | `src/pages/LibraryPage.tsx` | 网格、筛选、批量操作、对话框和查看器入口 |
| 超级搜索 | `src/pages/SuperSearchPage.tsx` | 三区条件、AI 搜索、警告、诊断和结果网格 |
| 打标 | `src/pages/AiTaggingPage.tsx` | 批次、主图/胶片条、建议确认、标签编辑 |
| 设置 | `src/pages/SettingsPage.tsx` | AI 连接、本地模型、入库、外观、数据与恢复 |

全局导航由 `src/App.tsx` 和 `src/components/layout/BottomBar.tsx` 管理。查看器打开时隐藏底栏。

### 2.2 状态层

`src/stores/` 当前包含：

- `libraryStore`：素材列表、筛选、分页、查看器上下文和刷新。
- `selectionStore`：选择集合、范围选择、截断信息。
- `tagStore`：标签树、分面和治理状态。
- `aiStore`：批次、建议、进度和确认状态。
- `settingsStore`：设置加载、串行持久化、外观预览、主题和错误状态；设置页只在用户实际改值后安排自动保存。
- `taskStore`：导入、导出、AI 等全局任务进度。
- `superSearchStore`：SearchPlanV3、AI 合并、持久化和诊断。
- `metadataStore`：元数据筛选项和派生值。
- `numericDomainStore`：数值分面范围和类型信息。
- `platformStore`：后端静态平台能力的单一前端消费入口。

跨页核心状态不得放在页面局部 state 中，也不得复制到第二个 store。

### 2.3 API 层

`src/api/` 按域拆分：

- `client.ts`：统一 invoke 和错误封装。
- `assets.ts` / `thumbnail.ts` / `preview.ts`
- `import.ts` / `export.ts` / `video.ts`
- `ai.ts` / `connections.ts` / `ollama.ts`
- `platform.ts`：静态平台能力 IPC。
- `tags.ts` / `settings.ts` / `superSearch.ts`

约束：

1. 页面和组件不直接 import `invoke`。
2. Rust serde 字段变化必须同步 `src/types/`。
3. 事件监听集中在 hooks/api 封装，组件负责卸载和清理。
4. `src/utils/logger.ts` 是唯一允许直接调用 `invoke` 的例外：日志必须使用原始
   `@tauri-apps/api/core`，不能经过 `src/api/client.ts`，否则 IPC 失败会递归触发日志。
   它只承载前端日志回传，不承载业务命令。

### 2.4 主要组件域

| 目录 | 内容 |
|---|---|
| `components/common/` | Modal、ContextMenu、Button、ModelCombobox、错误边界、进度条 |
| `components/layout/` | TitleBar、BottomBar |
| `components/library/` | 网格、卡片、侧栏、标签树、元数据、颜色条、查看器入口 |
| `components/viewer/` | 全屏查看器、媒体视口、胶片条、信息栏、标签栏 |
| `components/supersearch/` | AI 搜索、QueryBuilder、条件 chips |
| `components/ai/` | SuggestionReview（独立打标页与助手右栏共用的审核实现）、Filmstrip、Workbench、进度和分面标签输入 |
| `components/settings/` | AI 连接、分面、治理、本地模型和服务管理 |
| `components/media/` | 视频播放器和播放控件 |

复杂组件应按职责拆分。拆分必须先移动、后改行为，不与功能修复混在同一批。

## 3. Rust 后端结构

### 3.1 commands 层

`src-tauri/src/commands/` 按域组织：

- `assets_cmd`、`thumbnail_cmd`、`media_cmd`
- `import_cmd`、`export_cmd`
- `tags_cmd`、`super_search_cmd`
- `ai_cmd`、`ai_connections_cmd`
- `ollama_cmd`、`settings_cmd`
- `observability_cmd`：前端日志回传和脱敏诊断包导出

commands 只允许：

- 解析和校验 IPC 参数。
- 获取 `AppState`、短锁读状态。
- 把耗时任务放到 `spawn_blocking` 或后台线程。
- 发 Tauri 事件。
- 调用 services/db 并映射错误。

commands 不允许：

- 直接拼业务 SQL。
- 在数据库锁内执行网络、解码或大文件 IO。
- 承载可独立测试的复杂业务逻辑。

### 3.2 services 层

| 模块 | 职责 |
|---|---|
| `imaging.rs` | 全局图片解码入口、策略链、尺寸探测、并发许可 |
| `thumbnail.rs` / `preview.rs` | 双层缩略图和待入库预览瘦壳 |
| `importer.rs` | 扫描、哈希、改名、托管、元数据和落库管线 |
| `exif_meta.rs` | EXIF 提取与 RAW 元数据兜底 |
| `video.rs` / `video_proxy.rs` | 视频元数据、抽帧、兼容代理 |
| `backup_restore.rs` | 备份暂存、验证、主库交换、旧库保底和恢复失败回滚编排 |
| `authorization.rs` | 可信主体构造、目录授权、会话/授权/库世代校验和受控文件/素材句柄 |
| `plans.rs` | 准备计划、可信 GUI 批准与原子 submit：校验主体/库/世代并在短事务消费批准登记操作 |
| `operations.rs` | 操作队列声明与释放、逐项收据、取消/恢复、子任务绑定和启动对账 |
| `conversations.rs` | 会话索引、持久草稿、附件可用性与显式 plan/operation/batch 交接投影 |
| `import_prepare.rs` | 导入准备：把扫描/检查固化为带指纹、分类与改名预览的计划，不做副作用 |
| `import_execute.rs` | 按已受理 operation 可靠执行导入：逐项收据、重复映射、取消与恢复跳过 |
| `platform.rs` | 编译目标静态能力 DTO；不做运行时健康探测 |
| `export_local.rs` | 只复制导出、目录布局、逐文件结果、CSV 和任务状态；库内原文件与原记录保留 |
| `library_move.rs` | 总仓内分库移动：冻结逐文件目标、移动日志、路径与核验证据同事务切换、重启对账 |
| `workflows.rs` | 个人常用流程：受控步骤目录与参数白名单、类型化保存门、只追加版本、草稿从操作收据派生 |
| `workflow_run.rs` | 个人流程实例化：本次输入解析成冻结快照 → 新计划 → GUI 确认后提交 → 逐步骤进 Operations，需单独确认的步骤永不执行 |
| `mcp_bridge.rs` | 外部 MCP 本机接入：命名管道监听、6 步握手、能力范围裁剪、调用复跑 `capabilities::dispatch`、撤销即时生效、端点文件生命周期 |
| `dedup.rs` / `perceptual.rs` | 文件哈希去重、dHash、相似图和同源关系 |
| `palette.rs` | 主色提取、色板状态和回填 |
| `kinship.rs` | 同源/连拍关系判定 |
| `media_refill.rs` | 存量媒体元数据、GPS、尺寸和色板回填 |
| `ai_cloud.rs` | OpenAI 兼容/Anthropic 请求、解析、批次执行、取消和重试 |
| `super_search_ai.rs` | SearchIntentV2、Schema、降级、守卫和解析 |
| `ollama_setup.rs` / `ollama_installer.rs` / `ollama_runtime.rs` | 检测、推荐、拉取、安装和服务生命周期 |
| `credentials.rs` | 系统凭据读写 |
| `agent_runtime/` | 应用拥有的 Pi Host 进程、受控 framing、版本握手、精确事件校验、会话归属与关闭/恢复；首期仅 Windows x64，运行资源离线且无业务工具 |
| `heic_decode.rs` / `raw_decode.rs` | imaging 的专用解码下游 |

后台外部进程统一通过 `utils/process.rs` 构造；Windows 使用无控制台标志，其他平台保持原生命令行为。版本/GPU健康探针使用有限时的双管道读取，超时终止并回收子进程；媒体抽帧与转码保留各自取消/超时控制。
维护页通过 `ollama_installer_cache_info` 只读安装包文件元数据，不复用包含版本子进程与本机 HTTP 请求的 `ollama_install_status`。缓存读取失败不返回空缓存假成功。

`error.rs` 是命令错误的统一序列化边界：`AppError` 返回 `{ code, message, cause? }`。
`code` 使用稳定的机器码，业务校验优先使用 `invalid_arg` / `not_found` / `conflict` /
`cancelled` / `timeout` / `file_locked` / `ai_rate_limited` / `unauthorized` /
`unsupported` / `internal`；`cause` 保留 thiserror 的底层 source，前端 `AppError`
会同时保留 `code`、`message` 和 `cause`，用于差异化提示和排障。

根模块 `src-tauri/src/observability.rs` 负责统一日志初始化和安全边界：

- 同时输出到 stdout 和 `data_dir/logs/app.log.*`，文件 writer 使用非阻塞队列；启动后清理
  过期日志并把 `app.log*` 总量压到 50 MB 以内，始终保留最新文件。
- 安装 panic hook；同步写入 `fatal.log`，保证启动期和异步队列未刷盘时仍有证据。
- 提供 `info` / `debug` / `trace` 运行时级别切换，设置值落库后热生效。
- 对前端回传的 message/context 做长度限制和高置信度凭据脱敏。
- 导出诊断包时只读取摘要和日志文件，不读取素材原文件、提示词或完整请求体；日志总量上限
  20 MB、单文件上限 4 MB，超限时保留最新文件的尾部并在摘要中标记 `truncatedLogs`。

### 3.3 db 层

| 模块 | 职责 |
|---|---|
| `migrations.rs` | 版本迁移、幂等修复、schema 初始化 |
| `schema_features.rs` | 可选约束能力登记和实际结构自检 |
| `assets.rs` | 素材 CRUD、筛选、排序、分页 |
| `search.rs` | FTS5/LIKE 原子谓词 |
| `search_query.rs` | 元数据字段和运算符白名单编译 |
| `query_expr.rs` | QueryExpr 布尔树编译 |
| `search_plan.rs` | SearchPlanV3 校验、编译、评分和诊断 |
| `tags.rs` / `tag_facets.rs` / `facet_numbers.rs` | 层级标签、分面和数值分面 |
| `asset_tags.rs` / `tag_ops.rs` | 标签关联和撤销流水 |
| `ai.rs` / `ai_connections.rs` | AI 批次、建议和独立用途连接绑定；Agent 用途规则见 [实现契约 §6](../plans/v2/12-implementation-contracts.md#6-模型与个人流程) |
| `settings.rs` | 设置 JSON、normalize 和兼容迁移 |
| `authorization.rs` | 主体授权世代、目录授权和 opaque handle 的幂等持久结构 |
| `plans.rs` | 计划/批准/操作的幂等持久结构与原子批准消费 |
| `operations.rs` | 操作步骤/项目/收据/子任务绑定/恢复摘要的幂等持久结构与原子状态推进 |
| `conversations.rs` | 会话/草稿/附件/交接链接的幂等持久结构；不存第二份 transcript |
| `import_plans.rs` | 导入准备计划与逐项候选（指纹/分类/改名预览/剔除）的幂等持久结构 |
| `export.rs` / `dedup.rs` / `backup.rs` / `video_proxy.rs` | 任务和持久化模型 |

### 3.4 AppState

`src-tauri/src/state.rs` 统一持有：

- SQLite 连接。
- 取消注册表。
- 缩略图/媒体相关共享状态。
- Ollama runtime 状态。
- schema capability 缓存。

服务层尽量接收纯参数或 `Connection`，避免与 `AppHandle` 耦合；事件发送留在 commands。

## 4. 数据库与迁移

### 4.1 版本模型

- `PRAGMA user_version` 当前推进到 V22。
- V23 色板关系表、V24 数值分面、V25 在线连接限额字段和 V26 视频代理指纹采用无条件幂等修复，不推进 `user_version`，存量库每次启动可自愈补齐；V26 只追加列，不修改已发布的 V14 代理表迁移。
- 已发布迁移禁止修改，只能追加新迁移或幂等修复。
- 新列只追加到表尾，读取使用列名映射，禁止依赖物理位置。

### 4.2 关键数据原则

- 设置表只有一个业务 key：`app_settings`，内容为完整 JSON。
- `tag_facets` 是分面唯一事实源，旧 JSON 配置只作为历史迁移输入。
- `tag_terms` 与 `tag_aliases` 的唯一约束由 `schema_features` 控制，禁止双写。
- 回收站通过 `assets.deleted_at` 软删实现。移入计划摘要必须写明具体素材与按 `trash::expires_at_ms`（与到期清理同一公式）算出的清理日；恢复必须先证明文件仍在原位置、身份未变且仍属当前总仓（`services/trash.rs` 的 `FILE_MISSING`/`FILE_CHANGED`/`OUTSIDE_LIBRARY`/`PERMISSION_DENIED`），只清 `deleted_at` 不得宣称恢复成功。永久删除仍只在可信 GUI 的独立确认里发生，不暴露成 Agent 工具。
- 永久删除有两类发起方，共用同一删除服务 `services/trash.rs::delete_target`：手动清空由 `empty_trash` 走「`prepare_empty` → GUI 自批准（`review_kind` 必须是 `permanent_trash`，普通计划确认不能替代）→ `submit_empty` → `trash::empty`」；到期清理由 `lib.rs` 维护线程以受信内部调用直接执行 `trash::purge_expired`，不构造 `CallerContext`、不进能力目录，因此模型无法伪造维护身份（`trash.empty.*` 的 callers 已收紧为 `["gui"]`）。
- 逐文件删除效果落 `db::deletion_journal`（`intent → file_deleted → committed`，另有 `failed`/`missing`/`unknown`）。`remove_file` 前必须比对 `source_identity`、拒绝链接/目录；`assets::delete` 与 `record_committed` 必须同事务。状态迁移用 `changed != 1 → conflict("OUTCOME_UNKNOWN")` 保证只有迁移赢家能拥有后续 DB 更新。文件已删而提交前崩溃时，`operations::reconcile_startup` 里的 `trash::reconcile_interrupted_deletions` 依据日志把操作收敛为真实删除或 `outcome_unknown`，不重新扩大删除范围；有不明项时操作状态必须是 `outcome_unknown`，不能报 `partially_succeeded`。
- 到期清理只能在启动对账（含 `trash::reconcile_interrupted_restores` 与 `reconcile_interrupted_deletions`）之后运行，且总仓非 `Ready` 时整体跳过并记录告警——无法核验归属就不删。
- 标签操作流水用于批次撤销；手工覆盖应清理批次来源，避免误撤销。
- RAW/非 RAW 同源组只有在整组恰好包含一个 RAW 和一个非 RAW 时才互相折叠；有缺失、重复或第三个成员的歧义组不自动扩散标签、数值或去重结果。
- 视频代理只有在源素材指纹、编码器版本、工具指纹与源路径均匹配时才可复用；旧记录的指纹为空时视为陈旧缓存并重建，不影响原始素材。
- 运行时 keyring IO 由 `services/credentials.rs` 协调：先拿凭据操作锁，再短暂读写 DB 元数据，释放 DB guard 后才触碰系统凭据；不持有该锁跨网络请求。更新/删除与 DB 失败时必须恢复旧凭据或明确报告补偿失败。
- V15 旧设置凭据迁移只在 `Database`/`AppState` 创建前的启动 bootstrap 执行；凭据写入失败时保留旧 JSON，运行时 commands 不得复用此例外。
- 外部工具直接改库前必须了解 FTS 触发器和自定义 `cjk_bigram` 依赖。普通 SQLite 客户端只适合 SELECT。

### 4.3 备份与恢复

- 备份使用 SQLite `VACUUM INTO` 生成单文件快照。
- 恢复前校验来源并在数据目录同卷暂存、复验；暂存失败时活动库保持不变。
- `Database` 普通访问先取得 lifecycle 读门再短暂锁连接；恢复取得独占 lifecycle 写门，阻止其他命令访问半交换状态。连接 mutex 只用于 checkpoint/close、短 SQL 守卫和替换连接，不能跨复制、rename 或迁移文件 IO。
- 将现库保留为 `library.db.old` 后再安装恢复副本。若该路径已有内容则拒绝恢复，不覆盖、不删除；成功恢复后仍保留 `.old`，需人工确认后才能为下一次恢复腾出该名称。
- 运行中的导入、导出或打标任务存在时拒绝恢复。
- 获得独占门后再次检查任务状态，再执行文件交换与 `db::init` 迁移/自检。
- 替换或重新打开失败时先保留失败副本并从 `.old` 复制回主库；若不能证明活动库有效，封锁后续 DB 命令。若主库缺失，启动时只从校验通过的 `.old` 原样复制恢复，绝不静默创建空库，也不删除 `.old`。

## 5. 关键机制

### 5.1 图像与缩略图

所有通用图片解码经 `services/imaging.rs`：

1. 读取尺寸或内嵌预览。
2. 尝试 TIFF/CR3 等容器预览。
3. 必要时进入 `heic_decode` 或 `raw_decode`。
4. 使用全局许可限制并发。

占位层优先快速预览，禁止对 RAW 做昂贵全解码。高清层按需生成并缓存。dev 和 release 的图像依赖必须保持合理优化配置，新增图像依赖要核对 `Cargo.toml`。

平台能力由 `services/platform.rs` 单一计算，前端只消费 `platformStore`，不直接探测操作系统。
FFmpeg/FFprobe 发布构建只从主程序相邻且通过版本探测的 Tauri sidecar 启动，缺失/不可用时失败关闭，不回退任意系统 `PATH`；仅开发构建允许 `PATH` 回退。代理复用必须核对源素材指纹、源路径、编码器策略版本和工具指纹。源素材指纹在数据库锁外基于导入 hash（如有）、文件大小、修改时间及首尾固定大小样本计算；它用于缓存失效判断，不等同于完整内容校验和。代理容器由后端
能力选择（Windows/macOS H.264/MP4，Linux VP8/WebM）。来源、目标二进制和许可证材料摘要由
`src-tauri/binaries/manifest.json` 锁定，打包入口统一为 `scripts/desktop.mjs`。

### 5.2 普通搜索

普通素材库查询由 `AssetFilter` 表达：

1. `search.rs` 生成 FTS5/LIKE 原子谓词。
2. `search_query.rs` 编译元数据条件。
3. `assets.rs` 组合筛选、排序和分页。

FTS5 使用独立内容表和触发器同步，自注册 `cjk_bigram` 处理中文。查询必须参数化，不能把所有命中 ID 拉回 Rust 再拼长 IN。

### 5.3 超级搜索

超级搜索的机器协议由 `SearchPlanV3` 统一：

- `filter`：必须满足。
- `should`：软排序，数组顺序就是优先级。
- `mustNot`：只存正向条件，由计划层统一取反。
- `ranking`：字段排序或 relevance。

列表、总数、全选 ID 和诊断必须走同一计划编译器。详情见 [contracts/search-plan-v3.md](contracts/search-plan-v3.md)。

AI 解析先形成 `SearchIntentV2`，再由后端转为受限查询结构。AI 不生成 SQL、tagId、分页或物理执行计划。详情见 [contracts/super-search-ai-v2.md](contracts/super-search-ai-v2.md)。

超级搜索显式用途绑定优先且只使用绑定连接；无绑定时由 `services/credentials.rs` 自动解析可用在线连接，不读取旧版默认档案，也不自动回退本地模型。解析失败会保留为可操作错误，不切换到另一条服务。设置页通过无凭据的状态命令展示后端实际解析结果；用户仍可显式绑定本地服务。

- 标签词典在每次 AI 搜索请求时从活动标签和可搜索别名实时读取，并附带父类路径；人工确认后的新词无需重启即可被后续搜索识别。
- 精确规范名/别名直接解析为标签；无法可靠映射但置信度达到阈值时降级为内容搜索，低置信度概念给出未采用 warning。

### 5.4 标签与分面

- 标签父子关系使用递归逻辑和安全检查，禁止环。
- 分面 key 是机器协议，显示名只用于 UI 和提示词。
- `input_mode` 决定分面是否进入 AI 提示词。
- 新库、重置标签和空标签恢复库会播种 `subject` / `scene` / `people` 的默认层级；父节点用于浏览和宽泛筛选，AI 输出叶子。
- AI 确认的新词按已选择分面创建；系统不再把核心分面新词自动归入「其他」，需要层级整理时由用户通过现有标签治理入口调整。
- 创建与编辑通过 `FacetSaveInput` 一次提交；`services::facets` 校验业务字段，`db::tag_facets` 在一个 SQLite 事务中保存配置。
- 手工覆盖标签时清理批次来源，保证撤销安全。
- 删除分面按契约顺序级联清理标签、关联、建议和流水。

详情见 [contracts/facets-v2.md](contracts/facets-v2.md)。

### 5.5 AI 打标

批次状态：

```text
pending -> processing -> done
                  |
                  +-> cancelled
                  +-> interrupted（重启后修复）
```

规则：

- done/cancelled/interrupted 可续跑剩余 pending。
- processing 不允许重复启动。
- 单条失败只标记该项，不阻塞整批。
- 空解析视为失败，不写成空标签成功。
- 视觉输出使用 `description`、`tags`、`numbers` 协议；`peoplePresence` 只作为旧结果的可选兼容字段读取，不再要求模型提供或参与新结果语义。
- 分面 key 只承担结构路由；业务说明来自设置界面可编辑的全局打标说明和各分类“给 AI 的分类说明”。数量、范围等机器约束由同一界面的单选/多选、上限和数值配置动态生成。
- 解析层校验 JSON 结构、可用性与当前可见的分类配置，不按固定主体/场景/人物语义补标签、删标签、截断描述或重写模型输出。
- `confidenceMinSuggest` 控制建议进入待确认列表的最低置信度；待确认建议经用户确认后才写入素材。
- 请求层优先使用结构化输出：本地 Ollama 原生 JSON Schema、OpenAI 兼容 `json_schema`、Anthropic tool use；不可用时按批次缓存并降级为 `json_object` 或纯文本。
- 无效 JSON、必需结构字段缺失或完全没有可用内容时最多做一次协议修复；描述长度、主体是否存在和人物标签一致性不作为后台隐式规则。
- 低于 `confidenceMinSuggest` 的标签直接拦截，其余建议全部保持 pending，确认后才写正式标签。
- 续跑以是否已有分析结果为判断依据，不把低置信度全被拦截或纯描述结果误判为未处理。
- 批量确认按页提交并在页间释放数据库锁，避免整批建议形成长时间单一写事务。
- 视频多帧结果按证据合并，保留合并标签的置信度；失败帧记录 warning，单帧噪声不直接进入建议。
- 打标工作台支持手工复核和计划驱动的 AI 执行。`services::tagging` 负责将计划操作接入 AI batch，`services::ai_cloud` 执行供应商请求；AI batch 是建议与执行状态的事实源，operation 保存编排关系。冻结范围、批准、预算、取消和并发回写规则见 [实现契约 §4.4](../plans/v2/12-implementation-contracts.md#44-ai-打标计划与外发)。
- Tauri 启动命令只接收 operation/step 标识，由 `services::tagging` 校验调用者与绑定关系后交给执行服务；新旧入口和计划新鲜度约束见 [实现契约 §4.4](../plans/v2/12-implementation-contracts.md#44-ai-打标计划与外发)。
- 批次、tagging link、operation step 和 operation 的中断/取消及启动恢复状态在数据库事务内同步；失效审批重计划时旧操作明确取消、未完成批次回到 pending，再创建需重新批准的新范围。终态写入失败会整体回滚，避免只改批次状态。

### 5.6 日志、前端异常与诊断

- 后端业务代码只记录结构化事实，不在模块内自行创建文件 appender、过滤器或第二套日志器。
- AI 打标批次日志使用 `operation = "ai_tagging"`，并带 `batch_id`、`asset_id`、`stage`、
  `error_code` 与可确认的 `http_status`；不记录完整响应体，避免把模型原文或凭据带入日志。
- 前端统一通过 `src/utils/logger.ts` 记录 `debug` / `info` / `warn` / `error`；生产 WebView
  控制台通常不可见，异常通过 `log_frontend` 回传 Rust。
- 每条前端日志由 logger 写入不可被业务上下文覆盖的 `sessionId` 和递增 `sequence`；每次
  `src/api/client.ts` invoke 生成唯一 `requestId`，失败日志同时带 `command`、`durationMs`
  和 `cause`，用于把一次 IPC 失败与前后端上下文关联起来。
- 全局捕获 `window.error` 和 `unhandledrejection`。回传失败不得反向制造业务失败。
- 日志可包含路径、命令名、任务 ID、阶段和错误摘要；不得记录完整 API Key、Authorization、
  用户提示词、完整请求体或素材内容。
- 日志按天滚动，保留 30 个文件，并在启动时清理过期文件、把总量控制在 50 MB 内。
  `fatal.log` 是启动失败和 panic 的同步兜底证据。
- 诊断包包含 `diagnostics.json`、`logs/app.log*`、`fatal.log` 和存在的 `panic.log`；摘要仅含
  版本、系统、schema、数量、日志级别和最近 AI 批次状态；日志收集上限为总量 20 MB、
  单文件 4 MB，超限尾部截断并在摘要中记录。数据库锁只在读取摘要时短暂持有。

### 5.7 删除、导出和恢复

- 软删：保留文件，写入 `deleted_at`。
- 彻底删除：删除文件成功后才移除或更新数据库记录。
- 设置页“原始素材文件”重置复用彻底删除流程，覆盖在库与回收站；删除失败的文件保留素材记录并回报失败数量。
- 复制导出（A4-01）：`services/export_local.rs` 只做复制，库内原文件与原记录始终保留。目标目录必须来自 `pick_directory_grant("export_destination", …)` 的授权句柄，命令层用 `authorization::resolve_export_directory` 重新解析成 canonical root，**不接受前端传来的裸路径**。写入用 `create_new(true)`，同名自动加 `(1)(2)…`（`unique_dest`），复制后比对字节数，任何一步失败都清掉半截文件并只把该文件记为失败，任务继续处理下一个。
- 目标目录在复制过程中逐项复验（A4-01）：静态名字检查（`is_single_normal_component`）只证明拼出来的字符串合法，不证明路径实体没被替换。`guard_target_directory` 在任务开始时对授权根目录做 `symlink_metadata`（不是 `metadata`，后者跟随链接）+ `is_link_or_reparse` 检查并返回 `canonicalize()` 的根；`resolve_target_dir` 对每个派生出的子目录（按标签/按月份）重新 `create_dir_all` + 链接检查 + `canonical.parent() == dest_root` 比对。根目录不安全是任务级硬失败（没有可信目标）；子目录不安全**只让该文件失败**（`PATH_REJECTED`），其余照常复制——逐文件结果是本卡的验收要求，整批中断会把「一个标签名异常」放大成「整次导出失败」。
- 导出的终态必须落库（A4-01）：`export_local` 是 `copy_assets` 的薄包装，任何 `?` 上的硬失败都会补一次 `export::finish_task(..., "failed", None, Some(原因))`。此前直接在 `?` 返回，`export_tasks.status` 永久停在 `running`，而 `db/export.rs` 没有中断回收、`operations::reconcile_startup` 只处理 operations 表，没有任何执行者会推进它——界面永远转圈，且无法判断文件复制了几个。文件效果不回滚（`create_new` 已落盘的副本保留），只保证状态与原因如实可见。
- 导出也走计划与收据（A4-01）：`agent_tools::prepare_export` 冻结选择、目标句柄与布局成待审计划（准备阶段不写任何文件）；`exports.submit` 在事务内只登记每素材一条 operation item，磁盘复制在事务之后执行，逐文件结果写 `export_items`，进度经 `operations::record_item_state` 落成 `copied`→`succeeded/published`、`failed`→`failed/none`，取消释放的条目由 `finish_cancelled` 统一结算为 `cancelled`。因此部分成功、空间不足、权限拒绝、目标已存在和中途取消都返回真实分项结果，不会被单点失败或 `done/total` 计数掩盖。
- 库外移动已停用：`export_cmd::export_local_files` 对 `mode="move"` 直接返回 `AppError::unsupported`（`OUT_OF_LIBRARY_MOVE_REJECTED`），保留参数与明确错误而不是靠隐藏界面按钮。库内移动由 A4-02 的 `assets.move.prepare/submit` 承担，入口是主素材库与超级搜索页的「移动到…」→ `commands/move_cmd.rs`（`list_move_targets` / `move_assets`），不再复用导出命令的 `mode` 参数。
- 「有哪些分库可选」由磁盘现状决定，而不是数据库历史上出现过什么：`services/collection_targets.rs::list` 用 `managed_library::verified_root_path` 取核验过的根，深度优先遍历真实目录（跳过非目录、链接与保留目录，深度上限 4、总量上限 200，名字排序），再逐个 `db::collections::ensure` 登记引用（幂等，引用由总仓代次派生）。命令层只做「取当前总仓 → 转发」，遍历与上限都是业务规则，不写在 `commands/`。总仓根不可用时返回 `INVALID_LIBRARY_ROOT`，不返回空列表——空列表会让用户以为自己的分类全丢了。
- 完成摘要同时给目标位置与失败项：`commands/export_report(task_id)` 在一个锁内返回 `ExportReport { task, counts, items }`，前端不再用 `done/total` 反推成败。
- 审核前就要看得见目标位置（A4-01）：`prepare_export` 在准备期调用 `authorization::resolve_export_directory` 快速校验目标句柄，同时把解析出的路径经 `utils::path::display_native_path` 收缩 `\\?\`/`\\?\UNC\` 前缀后写进计划摘要（「复制导出：将选中的 N 项素材复制到 …，库内原文件与记录保留、不移动」）。超过 240 字符时保留末段并注明「路径过长，仅显示末尾」，避免撑破 `Instructions` 字节预算后变成不可读的内部错误。前端 `AgentCards` 的 `CAPABILITY_LABEL` 对 `exports.prepare/submit` 写「准备复制导出 / 复制导出」，并单独渲染 `scope.targetRef` 一行（`planTargetLabel`：导出「复制到」、导入「入库到」、移动「移动到」），此前这一行完全不渲染，审核者只能看到不透明令牌。
- 库内分库移动（A4-02）：分库不是拥有素材的实体，而是总仓内的相对目录；`db/collections.rs` 用 `SHA-256("teacell-collection-v1\0" + library_id + 0 + epoch + 0 + relative_path)` 的前 16 字节派生 `collection_ref`，所以总仓重新核验（`library_epoch` 变化）后旧引用自然失效，而不是悄悄解析到别的目录。`library_move::resolve_target` 按当前总仓代次解析目标，`plan_items` 在锁外做源文件探活与 `unique_target` 同名避让，把逐文件目标**冻结进计划**——执行阶段必须用 `agent_tools::move_plan_items` 读回冻结值，不能重新推导 `(1)(2)` 后缀，否则计划与实际目标不一致。
- 移动的文件与数据库效果：`db/move_journal.rs` 的五态梯形（`intent → moved → committed`，旁支 `failed` / `rolled_back`）保证「同一文件只有一个执行者提交」；先写 `intent`，`fs::rename` 成功后再 `record_moved`，然后在一个事务内写 `assets::update_file_location`（同时改 `file_path`/`file_name`/`modified_at`）+ `libraries::verification_candidate` 重读 `location_revision` + `record_verified` + `record_committed`。`trg_managed_asset_location_changed` 会在 `file_path` 变更时把定位置为 `unknown`，因此**必须**在同一事务里补回核验证据，否则素材会从 `VERIFIED_ASSET_SCOPE` 里掉出去。禁止假装事务能回滚文件系统：rename 成功而事务失败时日志停在 `moved`，交给启动对账判定唯一真实位置。
- 移动的越界与取消：`managed_library::validate_import_target` + `verified_root_path` 拒绝库外、保留目录（`.teacell-library-id` 等）与链接/目录联接逃逸；取消有两个来源——进程内原子标志与数据库里 `operations::cancel_operation` 置的 `cancelling` 状态（跨 WebView 重载后只有数据库权威），未开始的文件由 `finish_cancelled` 统一结算，不谎报成功。
- 移动的重启对账：`library_move::reconcile_interrupted_moves` 挂在 `services::operations::reconcile_startup` 的 `import_execute::reconcile_interrupted_imports` 之后，逐个读 `move_journal::unfinished`（`intent`/`moved`）在锁外判定真实位置，再映射成 `NoEffect`（回退为待执行）/`Moved`（补完提交）/`Unknown`（`outcome_unknown`），绝不把未知文件效果当成成功。
- 所有批处理都要区分成功、失败、重复和取消。
- 失败必须保留可恢复信息，不允许 UI 假成功。
- 总仓迁移与重新定位（A4-05）：两条互不替代的命令——`migrate`（软件复制并切换）与 `relocate`（用户已搬好，只核验并切换）。清单从 `managed_asset_locations JOIN assets` 派生而不是整树扫描，因此旧根下**未登记**的夹杂文件既不复制也不删除；`preflight_migration` 依次拒绝未知类型、已存在的未结束迁移、非 `Ready` 总仓、目标在源内/源在目标内、目录联接、`relocate` 时目标缺文件，然后按 5% 余量核对剩余空间（Windows 走 `windows-sys` 的 `GetDiskFreeSpaceExW`；其他平台返回 `None` 并降级为警告，不假装通过）。
- 迁移的持久阶段：`db::library_migration` 的 `preparing → copying → switched → cleaning → completed`（旁支 `failed`/`aborted`，逐项 `pending → copied → verified`）。`switched` 是分水岭：定位已换、源仍完整。复制写入 `{target}.partial-{uuid}` 并 `sync_all`，双向摘要一致后才 `rename`；目标已有同内容文件直接复用（断点重试），内容不同则报冲突而不覆盖。**清单冻结时一并冻结每个条目的 `content_sha256`（来自 `managed_asset_locations`）**，`require_frozen_digest` 是唯一判据：复制结果、重新定位的就地核验、目标完整性检查都必须对上冻结摘要，缺失同样拒绝——只比尺寸会把同路径同尺寸的陌生文件当成原库。任一项未通过核验就 `record_failure` 且**不切换定位**。文件 IO 全部在 DB 锁外，定位切换是单个短事务。
- 维护门：不用 `Database::block_access()`（那会连读一起掐掉），而是 `services::library_migration::LibraryMaintenanceGate`（排他租约 + 原因）挂在 `AppState`，写命令入口显式调 `ensure_writable`；`switch_locator` 另取 `db.maintenance()` 写 lease，因此长复制阶段不持数据库 lease。到期清理线程在 `reconcile_startup` 之后显式读该门，维护中跳过。**`enter` 返回 `AppResult<MaintenanceLease>` 且排他**：已有持有者时返回 `Conflict`、不覆盖理由与原状态；租约带 token，`Drop` 只释放自己那一份，所以第二个失败/取消的请求不会把仍在进行中的迁仓的门提前关掉。持有者 panic 时随 unwind 释放，不会永久锁死。**门立在共用执行边界上，不是各入口各写一遍**：判定收进 `services::agent_runtime::capabilities::dispatch`（Host 与 MCP 桥共用的唯一咽喉），由 `capability_descriptor(capability).access` 决定——`Query`/`Cancel` 放行、其余（含描述表查不到的）一律 `MAINTENANCE_ACTIVE`。手写「写能力清单」会在下一个能力加入时静默失效；描述表是唯一来源。MCP 桥不再自检维护门（那会让不经 MCP 的 Host 路径绕过同一道门），只负责把门接线进去。GUI 侧的 `start_operation`/`resume_operation` 也各自 `ensure_writable`——排队与批准发生在更早时刻，不能代表执行这一刻允许写入。
- 切换必然轮换 `library_epoch`（`trg_library_locator_requires_new_epoch` 强制），旧计划/句柄/目录授权/分库引用因此全部自动失效，不需要额外清理。`managed_library::prepare_root_for_migration(snapshot, root, create_marker)` 是「拒绝把任意空文件夹当原库」的落点：重新定位要求目标已带同一 `library_id` 的 marker，绝不现场补写。标记的创建被收进迁移路径的 `ensure_target_marker`（迁移是唯一会创建新总仓目录的路径，用 `create_new` 写、绝不覆盖），`switch_locator` 只核验不写。
- 迁移的重启对账：`reconcile_interrupted_migration` 挂在 `operations::reconcile_startup` 链末端——`preparing` → `aborted`；`copying` → `failed(INTERRUPTED)` 且定位不动；`switched`/`cleaning` → 重跑 `rebuild_projections` 后 `completed`（幂等）。源目录处置按 A0-05 只报告残留，本应用从不自动删除搬运来源。
- 迁仓的任务影响（A4-06）：`db::plans::in_flight_operations` 只数**当前代次**的 `queued/running/interrupted/outcome_unknown`（旧代次行已废弃，算进去会让用户永远无法迁移），命令层 `task_impact` 再合并进程内标志与 `db::ai::count_pending_or_processing`——前者覆盖未落库的短任务，后者覆盖 WebView 重载后标志丢失、任务仍在库里的情形。维护期**不静默取消任何任务**：`blocking` 为真只禁用「开始迁移」，由用户自己结束或取消；`start_library_migration` 在 `ensure_writable` 之外再查一次影响面。
- 旧库外记录的批量处置（A4-06）：`prepare_legacy_unlink_batch`/`prepare_legacy_copy_back_batch`/`confirm_legacy_resolutions_batch` 逐项独立处置，**永不聚合成一个结果**——每项自带 `state` 与错误码，界面用 `batchEntriesNeedingAttention` 单独列出需要人工看的项。批量复制回仓仍要求每个文件由可信原生选择器重新选中（源路径不进 IPC），因此一次点击的批量只对解除关联成立；同哈希不同元数据按契约只展示候选与「保留为独立记录」，**不提供合并**。
- 个人常用流程（A4-07）：模板保存的是**步骤与参数定义**，不是执行审批、原路径凭证或历史密钥。`services::workflows::prepare_draft` 是唯一保存门：名称/说明有界，步骤非空且 ≤ `MAX_WORKFLOW_STEPS`（8），每步先 `decode_capability` 再命中 `STEP_CATALOG` 白名单（只含 7 个 submit 能力），`depends_on` 只能指向前序步，参数必须被某步骤消费且 ≤ `MAX_WORKFLOW_PARAMETERS`（7）。封闭性来自类型——`WorkflowStepInput`/`WorkflowParameterInput`/`WorkflowDraftInput` 与七键 `WorkflowParameterKey` 全是 `deny_unknown_fields`，脚本、任意命令与凭据结构上无法表达，不是靠黑名单；`validate_steps` 另显式拒绝 `workflows.submit` 自身，防止流程嵌套流程。
- 版本与实例化记录：`db::workflows` 用三表——`workflows`（header + `current_version`）、`workflow_versions`（**只追加**，`add_version` 以 `current_version` 做 CAS，陈旧编辑报冲突）、`workflow_instantiations`（`plan_id → (workflow_ref, version)`，`ON CONFLICT DO NOTHING` 让重放保留首次版本）。编辑必产新版本，`rename` 只改 header，`set_archived` 是删除语义且保留历史版本，因此运行中任务记下的版本 N 永远解析到 N，不存在「热改运行中步骤」。
- 草稿只来自已理解的业务步骤：`draft_from_operation` 要求 GUI 身份并按 `operations_db::steps` 顺序生成链式 `depends_on`；合法步骤进 `steps`，无法表达为受控能力的步骤进 `excluded` 并带原因交给界面摊开，全部不可表达时直接 `invalid_arg`——不会把历史操作偷偷翻译成脚本。`WorkflowDraft` 的 `requires`/`runtimeInputs`/`notes` 让「保存前告知需要的能力与运行时仍需现场选择的输入」成为可渲染的数据。管理面板 `WorkflowPanel` 全应用只渲染一次（导航顺序按 UI 规范冻结为七组，不为它新增顶级组），详情区固定展示能力链、运行时输入、参数定义与 `needsAttention`。
- 个人流程的「再次使用」在 A4-07 只把流程名、版本与运行时待填输入注入到目标会话的输入框（`src/stores/composerInjectStore.ts`，取走即清空且不覆盖用户已写内容）；真正重新解析资源并生成新计划的实例化由 A4-08 的 `services::workflow_run` 承担。
- 流程实例化（A4-08）：`prepare_instance` 只读地把每一步的本次输入解析成 `WorkflowRunSnapshot`（存进 `plans.details_json`）并 `prepare_plan`，**不创建 operation、不碰文件**；`submit_instance` 在用户于对话框确认后显式 `approve_plan`（GUI 可信确认，与 `export_cmd` 自批准同一条路径），再用**同一 draft 构造器**重建 `current` 提交——`plan_hash` 才能一致，快照/版本/目录授权任一变化都会变成 `PlanChanged`。快照里只有本次解析出的 `selection_ref`/`asset_ids`/`destination_ref`/`collection_ref`，没有任何旧 plan/operation/收据引用，因此「上一次的批准」结构上无法被复用；同一流程连调两次必得两份独立计划。
- 实例化的能力分类（A4-08）：`imports.submit`/`tagging.submit`/`trash.empty.submit` 的 `requires_separate_action=true`——流程计划的 Ordinary 批准**不等于**这三类步骤的批准（入库要 GUI 亲自选来源目录、正式标签要逐条审核 + 显式 `start_tagging`、永久清空要 `permanent_trash` 专属确认），所以 `submit_instance` 的 effect 直接跳过它们，执行阶段只记 `skipped` 并点名去哪个界面。可直接执行的只有 `exports.submit`/`assets.move.submit`/`assets.trash.submit`/`assets.restore.submit`；`item_id_for` 对其余能力返回 `UnsupportedCapability`，不存在猜 id 硬塞的路径。
- 实例化的 Operations 接入：`consume_and_create_operation_with` 已自动插入 ordinal 0 的 `workflows.submit` step，所以流程步骤从 **ordinal 1** 起 `insert_step`；effect 内只 `insert_step`/`insert_item`（`target_ref` 对导出取 `destination_ref`、对移动取 `collection_relative_path`），**不 settle**——真实文件操作逐项完成才终态。任何中途 `Err` 必须先 `finish_operation(Failed)` 再返回，否则 operation 永停 `running` 会挂死恢复链。`workflows::contract_summary` 在 `needs_attention` 非空时返回 `conflict`，准备阶段原样上抛并指向「重新编辑流程」这一可修复入口。
- 执行期的依赖门与真实结算（A4-08）：`run_steps` 每执行完一步就用 `step_verdict` **从 `operation_items` 的当前状态反推**这一步的真实结论（有 `outcome_unknown` 即「结果未知」、有 `failed` 即「未成功」、有 `cancelled` 即「被取消」、仍有 `pending`/`running` 即「没有结算」，全部结算才是 `Completed`）——四个 `run_*_step` 自己返回的 `Ok(())` 只说明「函数没炸」，与「文件效果已达成」是两件事，不能拿来门控下游。下一步开跑前先过 `dependency_block`：`depends_on` 为空即独立步骤照常执行；有依赖时目标 ordinal 为 `depends_on + 1`（`workflows` 侧生成的是 0 基前序下标，而 `FrozenStep.ordinal` 是 1 基），前置 `Degraded` 时**整步跳过而不是按子集继续**——按子集继续会让用户在计划里批准过的范围被静默收窄，比明确跳过更危险。跳过由 `skip_step_for_dependency` 把该步 `pending`/`running` 的 item 逐条结为 `cancelled`/`effect_state=none`/`DEPENDENCY_UNMET` 并留收据：`operation_items.state` 的 CHECK 没有 `skipped`，而留在 `pending` 会被 `refresh_progress` 算进 `total` 却不计完成，摘要就会出现「应做 N 项」的错觉；`collect_step_results` 再按「整步 item 全是 `cancelled` + `DEPENDENCY_UNMET`」把它如实报成 `skipped` 并说明前置未完成。前置是 `requires_separate_action` 时**不阻断**：流程永不自执行这类步骤，把它当未完成会让「先导入再导出」永久跑不起来（该缺口属 A3-03）。
- 受理即返回与运行期取消（A4-08）：`run_workflow_instance` 只做「提交 + 抢占队列租约 + 起后台线程」就返回 `WorkflowRunStart{plan_id, operation_id, operation_state, replayed, can_cancel}`，**不再 `await` 整次执行**——否则前端在拿到 `operationId` 之前物理上无法取消。后续进度由 `workflow_run_instance_report`（只需 `planId`）读出稳定 operation 快照。同计划重试的幂等键由 plan 派生（`wfidem_*`，只含 `[A-Za-z0-9_-]` 且满足 `ResourceRef` 长度约束），但**先查 `operations_for_plan` 再决定是否 `approve_plan`**：计划一旦被消费，`review_status` 就是 `consumed`，再批准只会得到 `PlanChanged`——这正是「同计划重试产生孤儿任务或假报错」的来源，仅把幂等键改成派生并不够；命中已有 operation 时直接返回 `replayed: true`，已终态则不再抢租约（`is_queued_for_dispatch` 对终态必然为假，硬抢只会把「重试查看结果」报成「队列忙」）。`run_export_step` 的本地取消标志照 `operation_dispatch::run_export` 的范式在 `on_item` 里轮询 `plans_db::operation` 的 `phase == "cancelling" || state == "cancelled"` 后置位；`move`/`restore` 的 `cancelled` 本就只是进程内补充，数据库侧取消由它们各自的 `cancel_requested` 轮询承接，不再重复造一套。
- 外部 MCP 接入（A5-01）：行为事实只有 [`docs/contracts/mcp-bridge-v1.md`](contracts/mcp-bridge-v1.md)。宿主侧 `services::mcp_bridge` 在 Windows x64 上开命名管道 `\\.\pipe\teacell-mcp-v1-<instance>`（其他平台两端一致回 `UNSUPPORTED`），每连接一线程、最多 8 个会话，超出回 `close{code:"BUSY"}`；端点文件 `<app_data>/mcp/endpoint.json` 只含 `schemaVersion/protocol/instance/pipeName/key`，`key` 每次宿主启动由 `Uuid` 重新生成、退出即失效，**令牌本身从不落盘**。
- 握手判定严格按契约 6 步顺序：①帧字段与类型完全匹配且 `protocol == 1` → ②端点文件 `protocol`/`instance` 与当前进程一致（旧实例或缺失即 `PROTOCOL_UNSUPPORTED`）→ ③`connectionId` 语法合法、`mcp_connections` 中存在且 `revoked_at IS NULL` → ④`token` 与 `HMAC-SHA256(key, connectionId)` **常量时间**比较 → ⑤连接能力范围 ∩ 可发布集合非空 → ⑥总仓 Ready 否则 `LIBRARY_OFFLINE`。先校验形状再触碰数据库，避免伪造帧探测库状态。
- 令牌与常量时间比较都手写：`HMAC-SHA256` 用 `sha2` 按 `H(k^opad ‖ H(k^ipad ‖ msg))` 两轮实现（RFC 4231 test case 2 已断言），比较用 XOR 累积而不新增 `hmac`/`subtle` 依赖——与「不扩大依赖面」的仓库约束一致。
- 调用复用**同一条领域路径**：`capabilities::dispatch` 只是把 `CallerContext` 换成 `TrustedAdapterIdentity::Mcp{principal_id, instance_id}`（`session_id` 为 `None`），所以 MCP 与内置 Agent 走的是同一批 `agent_tools::*`，不存在「只给 MCP 打开的旁路」。载荷解码强制丢弃调用方 `schemaVersion` 并注入 `1`，`deny_unknown_fields` 天然拒掉 `principal`/`approved`/`libraryId`/`grantEpoch` 这类授权字段。
- 撤销即时生效：每个 `call` 帧都重新读 `mcp_pairings::get`，`revoked_at` 非空即回 `PAIRING_REVOKED` 并断开；已入队的写任务不回滚不重放，取消只走 `operations.cancel`。`plans.approve`、`operations.resume`、`trash.empty.*` 结构上不在可发布集合里，`publishable_set_never_contains_approval_or_resume_entries` 直接断言这一点。
- 前端 `src/components/settings/McpBridgePanel.tsx` 在「数据与隐私」组末尾管理配对：默认只勾 `assets.search` + `operations.get`，写类能力必须用户显式勾选；`assets.trash.submit` 的说明明确写「外部调用永远返回 APPROVAL_REQUIRED，本协议没有批准帧，永久删除不在可发布范围」。关闭开关会同步停监听并删端点文件。
- 计划的来源与拒绝（A5-02）：`PlanPreview` 携带 `originKind`（必填）与 `originLabel`（可选），审核者能直接看出这是外部客户端在申请写入；名称不持久化，回读时现查 `mcp_pairings::find_by_principal`，所以改名与撤销立刻反映。`plans` 表的 `origin_kind` 是**可空**列，`None` 只表示「A5-02 之前写入、来源未知」，读取按 `gui` 呈现而不猜。拒绝复用既有 `invalidated` 状态（不改 CHECK、不加迁移），`db::plans::reject_plan` 用 `WHERE review_status='pending'` 并**返回是否命中**；**已批准计划不能被拒绝**（`PLAN_CHANGED`），因为撤回批准会与在途 submit 赛跑，撤销执行只能走取消任务。

## 6. 状态流

| 链路 | 流程 |
|---|---|
| 入库 | ImportPage -> import API -> command -> importer -> db/预览 -> 事件 -> taskStore |
| 缩略图 | Thumbnail -> api -> command -> thumbnail -> imaging -> 缓存 |
| 普通搜索 | LibraryPage -> libraryStore -> assets API -> AssetFilter -> db |
| 超级搜索 | SuperSearchPage -> superSearchStore -> SearchPlanV3 -> command -> db |
| AI 搜索 | AiSearchBar -> ai_parse_search_query -> SearchIntentV2 -> QueryExpr/SearchPlan |
| AI 打标 | 选图 -> 建批 -> 执行 -> 进度事件 -> 建议确认 -> 标签/FTS |
| 本地模型 | SettingsPage -> ollama API -> command -> installer/runtime/setup |
| 恢复 | SettingsPage -> backup API -> command -> 校验/替换/迁移/重启 |
| 分类重置 | SettingsPage -> settings API -> command ->（可选）原文件逐项删除 -> DB 事务/缓存清理 |
| 日志回传 | 前端 logger -> raw invoke -> observability -> 文件日志 |
| 诊断包 | SettingsPage -> exportDiagnostics -> command -> zip 摘要/日志文件 |

## 7. 设计约束

1. 前端不直连 invoke，commands 不写业务和业务 SQL。
2. 图片解码只有一个统一入口，专用解码器不能成为平行主链。
3. 数据库锁内不执行阻塞任务。
4. 所有输入路径必须校验，所有 SQL 必须绑定参数。
5. 所有破坏性操作都必须明确结果，禁止静默覆盖和假删除。
6. 已发布迁移不回改，schema 变化必须有恢复路径。
7. 搜索计划、分面 key 和 AI 意图是机器协议，改协议必须同步 Rust、TypeScript 和测试。
8. UI 只使用语义主题变量，状态不能只靠颜色表达。
9. 性能路径不引入第二个缓存或第二个事实源，除非契约明确。
10. 文档只保留当前事实，过程历史交给 Git。
11. 日志只能在统一可观测性边界内落盘；前端日志例外使用 raw invoke，且不得传递敏感原文。

## 8. 分发与许可

项目和组合安装包使用 GPL-3.0-or-later，许可全文见根目录 `LICENSE`，版权及第三方声明见 `NOTICE`。此前以 MIT 发布的代码保留原授权。公开分发必须随安装包提供对应源码与构建材料：

- `rawler` 及其 LGPL/GPL 许可条件。
- `heif-rs`、libheif、libde265、x265 等静态链接组件。三目标原生归档及源码/许可证 URL、SHA256 见 [`src-tauri/native/heif-manifest.json`](../src-tauri/native/heif-manifest.json)；x265 使用 GPL-2.0-or-later，在本组合分发中适用 GPLv3。
- 其他图像、视频或模型依赖的再分发条款。

源码归档必须包含本程序、静态库与独立 FFmpeg 程序的实际源码、所需补丁、构建脚本和依赖锁。仅有许可证文本和上游主页链接不能替代对应源码。商业使用允许；再分发程序或修改版本时须遵守 GPL 源码与许可义务。

## 9. 2.0目标架构（待原型验证与实现）

第9.1–9.6节描述目标结构，不能据此判断功能已接通；第9.7节起描述已落地的模块机制，实现缺口和验收状态只见 [项目计划](PROJECT_PLAN.md#9-20-agent开发状态)。已批准产品行为由 [PRD第8节](PRD.md#8-20-agent-与25插件的已批准目标待实现) 定义；精确字段与协议由对应冻结契约定义。标为2.5的Module/运行空间仅为后续设计，不在2.0创建；当前实现描述不构成放宽任务卡或冻结契约的依据。

### 9.1 系统拓扑

```mermaid
flowchart TD
    GUI[可信 React 主界面] --> API[前端 API Adapter]
    Chat[聊天页面] <--> API
    API <--> Runtime[Rust ConversationRuntime / 受控 IPC]
    Runtime <--> Host[完整 Pi SDK Host / Node sidecar]
    Host --> Tool[领域工具 Adapter]
    Ext[外部助手] --> MCP[MCP Adapter / stdio bridge]
    Web[2.5 低权限插件 WebView] --> SDK[2.5 Plugin SDK / 受控桥]
    API --> Domain[Rust 领域能力 Module]
    Tool --> Domain
    MCP --> Domain
    SDK --> Domain
    Domain --> Policy[授权和操作计划]
    Policy --> Ops[持久操作执行]
    Ops --> Existing[现有 services / db / imaging]
    Ops --> Connectors[模型 Adapter；2.5 扩展生图与 RAW]
    Connectors --> Artifact[产物校验与入库]
    Artifact --> Existing
```

聊天消息、事件和控制均沿 React → src/api → Rust ConversationRuntime → Pi Host 传递；Pi 的业务工具回到同一 Rust 领域能力。图中的 Runtime 与 Domain 都在同一个 Rust 业务宿主内，前端不直连 Node。

### 9.2 Module 与 Interface

| Module | 稳定 Interface（示意） | 隐藏的复杂度 |
|---|---|---|
| ConversationRuntime | prompt / subscribe / stopReasoning / restore | Pi 类型、队列、会话切换、压缩、版本兼容 |
| DomainCapabilities | 按域定义的 query / prepare / submit | 参数、业务约束、身份、分页和错误 |
| Authorization | evaluate / requestReview / consumeApproval | 权限范围、配置变化、撤权和审计 |
| Operations | get / list / cancel / resumeEligible | 持久步骤、幂等、资源冲突、恢复核实 |
| ManagedLibrary | inspect / prepareMigration / relocate / reconcileLegacy | 单仓身份、托管范围、离线、迁仓与旧库外记录治理；维护入口不向 Agent 开放 |
| PersonalWorkflows | save / edit / instantiate / retire | 可编辑参数模板、版本、能力兼容与每次调用重新授权；不运行用户脚本 |
| PluginHost（2.5） | install / enable / openView / disable | 包验证、版本、WebView、工作区和撤权 |
| Artifacts（2.5 媒体扩展） | create / validate / register | 暂存、格式、来源关系；2.0 仅先做实际导入所需收据 |
| ConnectorRegistry（分期） | describe / invokeApproved / getHealth | 2.0 仅落实模型实际所需能力；媒体及通用扩展后续增加 |

采用小 Interface 封装复杂 Implementation；真正变化的 Seam 是 Pi/MCP/插件入口、模型提供方及媒体引擎。不要创建“任意名称+任意 JSON 执行一切”的万能业务接口。底层 IPC 可以统一封包，业务仍有类型和明确动作。

### 9.3 一份能力定义，多个 Adapter

每个能力定义稳定 ID、契约版本、输入/输出 schema、所需权限、资源范围、副作用类别、大小限制、取消/重试语义和可见入口。Rust 执行层拥有最终校验权；2.0 对齐 TypeScript 类型、Pi 工具描述和 MCP schema，2.5 再增加插件 SDK 包装，契约测试防止漂移。表中统一使用 prepare→planId→submit，preview 只是计划的展示，不是另一种受理阶段。

首批能力示例：

| 能力 | 输入 | 输出 |
|---|---|---|
| assets.search | 经校验搜索意图/计划、分页 | 有界摘要、计数、selectionRef |
| imports.prepare | directoryHandle、collectionRef、选项 | planId、摘要、需要确认内容 |
| imports.submit | planId、提交幂等键 | operationId |
| tagging.prepare | selectionRef、连接/分类设置 | planId |
| tagging.submit | 已批准计划 | operationId、batchRef |
| exports.prepare/submit | selectionRef、目的目录句柄、复制布局 | planId / operationId |
| assets.move.prepare/submit | selectionRef、当前总仓内目标分库 | planId / operationId |
| assets.trash.prepare/submit | 精确素材集合 | planId / operationId；摘要含具体素材与预计清理日 |
| assets.restore.prepare/submit | 当前回收站中的精确集合 | planId / operationId；执行前逐项核验文件仍在原位且归属未变 |
| trash.empty.prepare/submit | 当前回收站中可安全清理的固定集合 | 独立 planId / operationId；不接受任意路径永久删除 |
| artifacts.register（2.5） | 已校验产物引用、目标分库 | 逐项资产收据 |
| operations.get/cancel | operationId | 真实状态/取消请求结果 |

句柄由宿主签发，引用不等于授权。审批绑定和执行语义统一见 [06](../plans/v2/06-permissions-and-data-safety.md)。正式标签写入不得借通用标签接口绕开 AI 建议确认。

### 9.4 数据所有权

| 数据 | 所有者 | 其他部分如何使用 |
|---|---|---|
| 素材、标签、FTS、配方与来源关系 | Rust/SQLite 业务内核 | 使用业务 Interface |
| 已批准计划、操作编排状态与关联 | Rust 持久操作记录 | 通过 ID 查询，不相信聊天文字 |
| 总仓身份、定位、托管凭据与旧记录待处理状态 | Rust 管理的持久库目录和维护记录 | 路径展示不作为所有权证据；迁移细则见 06 |
| 导入草稿、会话附件引用和工作台关联 | 宿主持久记录，前端投影 | 同任务多视图共享；页面卸载不丢失，不同对话不共用单一清单 |
| 个人流程定义、参数 schema 与版本 | 应用管理的流程注册表 | Pi 按版本加载；不携带上次的操作批准 |
| 既有 AI batch/export 的领域执行状态 | 各域持久任务记录 | operation 读取投影并编排后续步骤，不双写独立执行进度 |
| Pi 原生 transcript 和压缩结构 | Pi SessionManager | 应用保存索引与格式版本，不镜像第二套规范上下文 |
| 插件工程/草稿 | 插件命名空间存储，由宿主管理 | 专属存储 API 和导出 |
| 缓存、临时产物 | 宿主空间管理 | 可恢复任务占用明确；不能清理仍在使用的产物 |
| 凭据、客户端配对 | 系统凭据库与宿主授权记录 | 只给连接引用，不给普通插件密钥 |

同一素材库只设一个业务宿主。MCP bridge/Pi/插件不另开主库；应用侧新增单实例及库所有权校验。不会因 stdio 启动多个 bridge 就创建多个数据库业务进程。

### 9.5 运行空间

目标逻辑布局如下，物理目录由平台路径模块决定，不硬编码用户路径。2.0 只创建实际使用的 agent/operations 空间，plugins 各目录留待 2.5，不创建空目录占位：

    应用数据/
      agent/sessions/                 Pi 会话
      plugins/packages/<id>/<version>/ 只读已安装代码
      plugins/data/<id>/              持久工程与设置
      plugins/cache/<id>/             可清理缓存
      operations/<operationId>/       输入引用、暂存和产物

权限并非由目录名字保证；普通插件经宿主访问，调用时验证身份/句柄/配额。可信原生扩展的真实 OS 权限另见 05/06。素材库根目录、主库、凭据目录与插件工作区分离。

### 9.6 并发与故障

- 模型流、搜索、缩略图和重计算设独立并发预算，不能因一个显影批次饿死浏览。
- DB 短事务；复制、解码、网络、keyring 与进程等待在 DB 锁外。
- 会话一次推理循环由其状态机控制；2.0 长业务任务默认排队，查询、浏览和聊天保有独立资源预算。任务内部逐图并发仍受所属域配置限制，不能把排队等同全应用单线程；所有入口共用冲突与维护门。
- 插件页关闭只结束交互实例；具有有效任务授权的宿主任务可继续。退出主应用时停止接新请求、请求取消并记录中断；不是默认后台常驻服务。
- Pi 崩溃恢复会话并查询已有操作，不能重放全部工具调用。插件崩溃不应丢失已接受的宿主任务。
- 进度通知可以合并/丢帧，终态必须可查询；有 requestId、operationId、sequence、schemaVersion；过期事件不能覆盖新状态。

### 9.7 总仓保护数据边界（A1-02）

`db/libraries.rs`保存单当前总仓定位与资产保护映射，追加V27幂等结构，不改旧assets主键或标签/历史关系。`libraries`以singleton=1约束唯一当前定位，library_id是稳定身份，library_epoch是不可复用随机令牌；普通启动不旋转世代，备份恢复安装和维护切换必须显式生成新世代。旧settings.library_root只承接初始候选，最终运行定位以此表为准；常规设置保存不能清空或替换已有根，首次候选经锁外检查、短事务CAS后建立带新世代的身份，已有根的更换与重新定位只走后续维护流程。

`managed_asset_locations`以asset_id关联原记录，保存观察到的路径/modified_at、当前库与世代、相对定位、文件身份、摘要和托管收据。缺映射也按未核验处理；结构迁移不遍历assets回填，新记录默认unknown。路径或观察版本改变撤销证据并刷新不可复用location_revision；分类成功也刷新修订，旧分类结果不能覆盖新结果。verified要求完整证据字段，但数据库字段存在不证明文件归属；只有受控服务经锁外IO和身份核验后可登记。根健康独立于资产分类：unconfigured/unverified/ready/offline/wrong_root；失联不批量归类为库外，缺文件不删元数据。

共享`VERIFIED_ASSET_SCOPE`固定使用SQL别名a，在分页/计数之前约束当前健康库、核验状态、世代和观察版本。分批核验用keyset取最多100项，包含缺映射旧记录与对当前库无效的旧verified证据，IO在锁外；回写pending分类同时比较库身份/世代/根身份、资产路径/版本与location_revision，单语句有条件登记/更新映射并刷新修订，拒绝过时快照及路径ABA。该接口不能将记录标为verified，拒绝改写当前有效verified；旧世代证据可在当前ready根快照下CAS降级并清掉旧证明，再由正式核验服务登记。恢复安装通过db::init_for_restore在Database独占生命周期边界内完成迁移与随机世代刷新，撤销根身份/ready后才replace_connection；成功恢复及重新开库的回滚路径均使用该初始化。普通init不旋转；启动时从library.db.old自动救回也在开放连接前刷新世代，后续普通重开保持该新世代。数据库trigger拒绝修改root_path/root_identity而不刷新epoch。

普通列表/搜索/分面、去重与ID解析、缩略图和视频代理、导出与AI取图、媒体回填/色板/哈希等批量写入、标签/数值/AI审核写回、设置根保存、备份恢复世代失效以及启动回收站GC均已接入有效范围或在受控维护边界内执行；受控导入在写入资产行的同一事务登记托管证据，失败回滚。工程校验不清除库外/未知记录的原元数据，离线根保留证据并失败关闭。当前边界仍是数据库级保护：文件身份核验到实际打开/删除窗口的OS句柄策略由A1-03提供受控打开和打开后复核；敌对同账户TOCTOU、平台专属无跟随句柄和三端证据仍按后续A6/PLATFORM验收，不能把路径字符串、无链接检查或本卡测试声称为OS沙箱。

### 9.8 目录授权与受控资源句柄（A1-03）

`services/authorization.rs` 统一从已验证 Adapter 身份构造 `CallerContext`；GUI 身份取自 `AppState` 本运行实例和 WebView 会话，Pi/MCP 未来 Adapter 只能从各自握手/配对结果构造，不能由工具输入提供 principal。`commands/authorization_cmd.rs` 只允许 GUI 调用 Rust 宿主原生目录选择器，前端和外部 Agent都不能提交任意目录路径来创建授权。

`db/authorization.rs` 在V27追加幂等结构：每个 principal 有可替换的 `grantEpoch`；`directory_grants` 记录用途、真实目录身份、本次/记住、有效期、授权世代和库世代；`resource_handles` 记录主体、session/instance、目录/文件/素材类型、用途、读写/外发 access mask、过期和身份证据。撤销单条或全部授权在同一短事务中标记 grant/handle 并提升 principal 世代，旧句柄随后失败关闭。

读取区分 `import_source`（read）与 `export_destination`（write + export）。目录分页、文件打开和托管资产打开都在短锁外执行 IO：先核验句柄主体/会话/用途/世代/库世代，再检查规范根、逐级链接/reparse、相对路径、打开后的文件身份/大小/观察时间和根内位置；长读取可在有界块间复核撤权。素材句柄还要求当前 `VERIFIED_ASSET_SCOPE` 的有效托管定位。`ResourceRef` 本身从不授予权限，服务不提供通用路径读写命令，也不把本地路径写入授权错误日志。

### 9.9 准备计划、批准与原子提交（A1-04）

`services/plans.rs` 与 `db/plans.rs` 把“等待审核的计划”和“已受理的执行任务”分开：`prepare_plan` 只由领域能力处理器调用，写入计划的确切能力、范围引用、主体/会话/实例、库身份与世代、授权世代、条目数、预算、期限和由这些字段派生的 `planHash`，返回 `planId` 与一次性的 `reviewRef`；等待审核期间不创建 operation、不持有数据库锁、不占用资源独占锁。计划一旦写入即为不可变事实，输入或配置变化只能生成新计划，旧计划与新范围的摘要不同。

只有可信 GUI 能批准：`PlansApprove` 的能力入口表限定 `Gui`，批准时必须同时给出对应计划的 `planId` 和 `reviewRef`，服务再次校验当前库就绪、库世代、授权世代与 `planHash`，并把计划从 `pending` 转 `approved` 且写入一条 `plan_approvals`。GUI 审核的是计划本身，不需要冒充创建计划的 Pi/MCP/GUI principal；计划保留原创建主体、session/instance，提交时仍由原主体且同一运行实例完成。模型文本、`approved:true`、外部客户端提示都不能创建这条记录；契约解码层已拒绝这些伪造字段。`trash.empty.submit`（`permanent_trash`）与普通整理（`ordinary`）的审核类别在计划里固定，不能互换或借一次普通批准清空。

`submit_plan` 接收当前领域快照并在单个短事务内完成：先校验主体、原 session/instance、当前库就绪与世代一致、计划未过期、授权世代未变；从当前能力、范围、服务修订、模型、条目数、摘要和预算重建 `planHash`，与准备时保存的 hash 不一致就使计划失效并返回 `PLAN_CHANGED`；随后按 `(principalId, libraryEpoch, capability, idempotencyKey)` 查询幂等键；消费唯一未消费且摘要匹配、且计划仍为 `approved` 的批准；登记 operation 并写回 `consumed_operation_id`。同键同计划返回原 `operationId`（`replayed=true`），同键异计划返回 `IDEMPOTENCY_CONFLICT` 且不消费第二个批准；缺少或已消费批准返回 `approval_required`，此时尚无 `operationId`。过期的计划或批准在判定时标记 `expired`；库世代变更、根切换或授权撤销（`grantEpoch` 提升）都会让旧计划无法消费旧批准。

`commands/plans_cmd.rs` 只暴露 GUI 的计划列表、审核批准与操作查询；submit 是领域接口，不作为外部 Agent 原始命令注册。真实导入、打标、导出与移动由各自领域服务消费计划，外部 Adapter 只能经已授权的 capability handler 进入，不能直接调用批准后执行的 GUI 命令。

### 9.10 可恢复操作队列与逐项收据（A1-05）

`db/operations.rs` 在 `db/plans.rs` 建表之后追加幂等结构：`operation_steps` 以 `(operationId, stepId)` 固定步骤身份，`operation_items` 以 `(operationId, stepId, itemId)` 固定逐项身份并保存状态/effect_state/输入指纹/目标，`operation_receipts` 以 receiptId 幂等记录每次修订的事件、文件/子任务引用和错误，`operation_child_tasks` 以 `(operationId, stepId)` 唯一绑定子任务并以 `(taskKind, taskId)` 防止同一子任务被跨步骤复用，`operation_recovery` 保存启动对账摘要。operations 表通过幂等 `ALTER TABLE` 追加 `finished_at`，不改已发布迁移。

`db/tagging.rs` 追加 `tagging_batch_links`，以唯一 operation/step、唯一导入来源 operation/step、已批准范围指纹及持久请求计数/截止点绑定 AI 批次；同一批次可由多个顺序完成、各自独立审批的素材范围共同处理。批准消费、operation/step、AI pending batch 与范围化子任务映射共用一笔数据库事务；链接状态与 operation 状态在启动、完成、中断及启动恢复中同步。每个获准素材在发送前同事务递增预算、把对应 operation item 标为 `running/unknown` 并写入发送授权收据；已发送请求在结果不明或重启后不会被自动重放。若模型建议及分析已落库、但成功收据尚未提交，启动会按冻结建议 ID 和素材 ID 核验后补齐成功收据；全部素材因此已知完成时同步终结 step、operation 和 batch。若没有待继续素材而仍有未知请求，则 operation 终结为 `outcome_unknown` 并保留不可继续的恢复摘要；显式继续剩余素材后，既有未知项仍保留在最终 operation 状态和恢复摘要中。仅旧记录只有批次级尝试数、且完全没有逐项记录时才作为一个未知项呈现。

`services/operations.rs` 提供操作层编排：`OperationQueue` 单槽按全局 FIFO 领取可自动派发的 `queued/ready`，并把队首推进到 `running/executing`；需要 GUI 显式启动的 AI `tagging.submit` 等待项不占槽，也不会挡住导入派发。用户显式启动打标时仍须持有同一队列槽位，并服从更早可派发工作的 FIFO。未声明时只读与聊天不被长任务阻塞；`register_item`、`record_item_state`、`bind_child_task`、`finish_operation`/`finish_cancelled` 只由持有该操作的主体会话调用，跨 principal/session/instance 与跨库世代失败关闭；`bind_child_task` 同键同任务幂等、异键返回 `RESOURCE_CONFLICT`，保证恢复时不重复创建 AI 批次/导出任务。`reconcile_startup` 把未完成操作置为 `interrupted/reconciling`、统计未知 effect 并写入恢复摘要，自动对账只呈现事实，`resume_operation` 需用户在可信界面显式继续后才回到 `queued`；取消只标记 `cancelling`，`finish_cancelled` 为每个终结项目修订写收据并保留已成功项，已产生副作用但尚不能确认完成的项目进入 `outcome_unknown`。逐项状态以收据与真实文件/DB 效果为准，不能用 processed 计数或事件代替。结算判据与入口无关，只与「这个动作本身能不能产生不可证实效果」有关：移动、恢复、清空都会出现「文件已动而记录没同步」，所以 GUI 命令（`move_cmd`/`trash_cmd` 的三个 `finish_*` helper）与 dispatcher 都是同一顺序——`unknown > 0` 先落 `outcome_unknown`，再判成功、失败、部分成功；导出只是复制，`ExportSummary` 结构上没有 `unknown` 字段，因此三态结算是正确答案而不是漏判。新增写动作时先回答「它能不能出现不可证实效果」，再决定要不要抄这个优先级。

`commands/operations_cmd.rs` 暴露 GUI 的取消、恢复和恢复摘要查询；`AppState` 持有进程内 `OperationQueue`，`lib.rs` 启动维护任务调用一次启动对账。导入由 A2 dispatcher 处理逐项恢复；AI 打标由 `services::tagging` 管理单一 batch 子任务，在启动对账后将已中断的子任务同步为可显式恢复状态。队列抽象测试不能代替各领域 worker 的真实副作用验证。

### 9.11 会话索引与持久工作台草稿（A1-06）

`db/conversations.rs` 追加 `conversations`、`conversation_drafts`、`conversation_attachments`、`conversation_links` 四张幂等结构表，不推进既有 `user_version`。Pi 原生 JSONL 仍是唯一上下文源：本层只保存可重建投影——标题、状态、草稿载荷、附件引用与显式的 plan/operation/batch 交接链接，绝不另存一份参与模型推理的 transcript。会话按 `principalId` 归属（GUI 主体稳定，不随每次运行的 WebView session 重建），因此重开应用后仍可读回；草稿以 `(conversationId, draftKind)` 为唯一键并带 `revision`。

`services/conversations.rs` 覆盖卡内行为：`create/list/get/rename/set_archived/delete` 都先校验主体与当前库健康并只暴露本人会话；`list` 的筛选只搜索可重建的标题投影，不搜索对话内容；`save_draft` 校验草稿种类与 JSON 对象载荷字节上限；`attach_asset` 只记录素材引用和当时显示名，读取时用 `VERIFIED_ASSET_SCOPE` 计算 `available`，缺失或未核验就显式不可用，绝不静默重新授权；`link` 是唯一的交接入口，`(linkKind, linkRef)` 唯一键保证同一任务只归属一个会话，手动任务不传会话也照常成立。`delete_conversation` 先查询该会话是否仍有未结束 operation，有则返回 `RESOURCE_CONFLICT`；否则只做软删除（`deleted_at`），素材、正式标签、计划、操作与收据一律保留。

`commands/conversations_cmd.rs` 暴露 GUI 的创建/列表/读取/改名/归档/删除/存草稿/加附件/交接命令；前端只经 `src/api/conversations.ts` 调用，`src/stores/conversationStore.ts` 按稳定 `conversationRef` 分桶保存草稿（切页/交替编辑两个会话不串清单），并在选中或重开时按该 ID 重新拉取投影重建，而不是依赖“最近一个”全局 ID。A1-06 只建立宿主会话索引与草稿基础；Pi transcript 关联、草稿字段的业务级校验和附件可访问性的真实素材 UAT 由 A3/A4 继续接入。

### 9.12 导入准备计划（A2-01）

准备、确认前复验、执行源文件复验及目录恢复重绑共用 `utils/hash.rs` 的有界哈希读取检查点（1 MiB）。外部来源在块边界重新核验授权/主体/库世代；准备和执行还复核各自的准确取消状态。执行已经发布的持久目标时保留跨会话恢复语义，不要求旧来源会话句柄继续有效。恢复重绑只复核新目录授权及库世代，不据此声明启动对账或全部恢复哈希支持逐块取消。

`services/import_prepare.rs` 与 `db/import_plans.rs` 把“扫描/检查”和“正式入库”分开：`prepare_import` 只枚举候选、分类预览、计算改名预览并写入计划，绝不复制文件、不落正式资产、不外发；候选来源要么是 A1-03 的 `import_source` 授权目录句柄（经 `locate_authorized_file` 逐个核验主体/授权/链接/打开后身份），要么是拖拽的显式文件路径（保持既有信任级别）。每个候选项持久化 `fileRef`/路径、`size`、`modified_at`、文件身份与完整内容摘要、`ready/limited/unsupported` 分类、提示文案与后端同源 `renamePreview`，计划另存目标分库、改名模板、库身份与世代和期限。

`verify_import_plan` 复检整份计划：逐项重新打开核验指纹，返回已变化（`staleItemRefs`）的条目以及库是否已切换；变化项必须重新准备，submit（A2-02）不得沿用旧指纹。重新准备时请求携带 `basePlanRef` 与 `excludedItemRefs`，服务把被剔除条目的路径并入排除集，因此用户移除的不可处理项不会在下一版计划里被重新塞回；改名预览用候选文件真实 mtime 调用与正式入库同一个 `render_name`，保证显示规则与实际计算一致。非法分库名、未知/越权目录来源、库离线等返回结构化错误码。准备任务以独立 `taskId` 注册取消，枚举、分类和计划事务提交前均检查取消；取消回滚准备记录，后台线程结束清理登记。`commands/import_cmd.rs` 暴露 GUI 的 `prepare_import`/`cancel_prepare_import`/`get_import_plan`/`verify_import_plan`，`src/api/import.ts` 提供对应封装；正式导入执行、逐项收据与恢复由 A2-02 接入，本卡不把准备计划当作已入库。

### 9.13 可靠导入受理、派发与恢复（A2-02）

`services/import_submission.rs` 是可信 GUI 的确认边界：受理前校验当前主体、授权世代、库健康、准备期限和输入；同主体/库世代/能力/幂等键重试返回原 operation，不新建计划或再批准。异范围同键报冲突。`confirm_import` 在线程池受理后立即返回统一 `SubmissionResult`，受理不是执行完成。独立入库页面的生产接线见 [A2-05 工作台](#915-独立入库工作台a2-05)。

`services/import_dispatch.rs` 经 `OperationQueue` 单槽领取 `imports.submit`，按持久领域计划恢复执行上下文，并协调现有 `import_running` 互斥。领取后任何映射或执行错误都形成可查询结果，释放槽位后继续后续任务；未知效果不转写为成功。排队取消直接结束，运行中取消由 worker 读取该 operation 的请求；其他任务的取消标志不影响本操作。`resume_operation` 先检查导入映射，再由可信 GUI 显式恢复、重新绑定本人会话并唤醒同一派发器。启动对账只展示待继续状态，不自动执行。

`services/import_execute.rs` 校验 operation 与准备计划的真实绑定；受理事务先为完整批准清单建立逐项身份，worker 开始前还会兼容回填旧操作。每项执行复验准备输入、使用授权解析后的真实路径和全量摘要，复用既有 importer/imaging 管线。复制按 1 MiB 分块，在块边界及进度回调后复核取消和来源授权。内容先写入总仓保留目录 `.import-stage`，持久化暂存对象身份、同步文件并复算完整 SHA-256；通过同卷硬链接以不覆盖方式发布到正式路径，journal 分别记录暂存与发布身份。`publication_verified` 后再次复核授权，撤权时保留已发布文件和 journal、阻止资产登记并将 operation 转为可显式恢复的 `interrupted`。成功项从收据跳过，不要求源文件继续存在。应用启动对账在数据库锁外检查未完成导入的目标/暂存路径、对象身份和完整摘要，并与托管资产路径及摘要登记核对；已发布但未登记的项保持可显式恢复，身份冲突或已发布文件缺失则写为 `outcome_unknown`，已登记但操作收据缺失的项补齐成功收据。运行中取消也先核对文件 journal，避免把发布后的文件效果误记为未发生。应用重启后，目录来源须由可信 GUI 重新选择原目录；服务核对目录身份以及每个未完成文件的相对位置、身份、大小、时间与摘要，再原子重绑当前会话句柄并恢复 operation，错误目录或已变化文件不会继续执行。正式发布收据固定最终目标；恢复时按该目标的发布对象身份和完整摘要验证后登记，不再次套用改名模板。目录重绑及全量摘要复核由 GUI 命令的 `spawn_blocking` 工作线程执行。文件 IO 和摘要计算在数据库锁外。当前自动化证明 Windows 本机路径；其他平台硬链接/并发路径安全、真实素材与断电 UAT 仍按平台和最终验收关口单独核实。

`operations` 的逐项状态、收据和计数同事务提交，插入收据失败完整回滚；operation 总量由受理时冻结的 step 总量计算，不随逐项延迟登记而缩小。授权世代在运行中失效时，只中断对应 operation 并保留可恢复快照。取消保留已完成效果，结果未知优先于普通失败/取消。跨进程断点、真实媒体、系统掉电与三端验证的实际证据和缺口只在 [PROJECT_PLAN](PROJECT_PLAN.md#9-20-agent开发状态) 登记。

### 9.14 历史库外记录处置（A2-03）

`db/legacy_assets.rs` 只把未被当前总仓所有权收据证明的资产投影到待处理页；持久收据区分准备、复制、发布、完成、解除关联和结果未知。总仓离线或无法重新核验时，列表仍保留待处理记录，但页面禁用逐项操作。列表查询在释放数据库锁后核验总仓标记；标记缺失、内容不符/无效 UTF-8 或根目录条目本身是链接时报告 `wrong_root`，标记或路径因权限/普通 IO 无法读取时报告 `offline`；每个处置入口仍重新核验当前总仓身份与世代；ready 总仓与活动收据绑定不符时将收据转为 `outcome_unknown`，offline/wrong_root 时保留收据并禁用操作。

`services/legacy_assets.rs` 仅允许可信 GUI 准备或确认操作。库外待处理记录可以重新选择对应原文件并比较文件身份、大小、修改时间和完整 SHA-256；复制准备还必须把当前大小、修改时间与旧资产行快照对比，变化时降为 `unknown` 且不创建处置收据。处置收据另存操作系统可报告的完整修改时间精度，并在确认、复制、发布和登记前后复核；旧活动收据缺少此字段时迁为 `outcome_unknown`，不以旧毫秒快照继续执行。哈希相同的托管资产只作冲突预览，不自动合并。回仓先持久化目标路径，再在数据库锁外把内容写入 `.legacy-stage` 下与收据绑定的暂存对象；复制后重开暂存路径核对对象身份，并复核源句柄/路径的身份、大小、完整修改时间和摘要及暂存内容，再通过同卷硬链接原子且不覆盖地创建正式目标。正式目标永不用于续写，只读打开；Windows 登记期间拒绝写/删共享。目标登记前再次核验原源快照，变化时不登记，收据保留为 `outcome_unknown`；身份仍匹配的暂存对象可显式重试。已发布目标缺失、身份/内容无法证明、正式目标被占用、根/源快照变化时进入 `outcome_unknown`，绝不覆盖或认领目标文件。登记时核对目标身份、摘要与收据一致，数据库资产修改时间取固定句柄的最终快照；资产条件或事务 CAS 不符时回滚资产登记并将收据转为 `outcome_unknown`。硬链接能力与同账户并发改写边界见 [PLATFORM 第9节](PLATFORM.md#9-20-agent平台增量)。发布后再在同一数据库事务内更新原资产行、登记托管收据并完成处置。

解除关联只写处置收据并隐藏待处理投影，保留资产行、标签、评分、关系及库外文件。准备和确认都会核验根仍可用、原路径仍是库外普通文件且快照未变；文件缺失或变化会把定位状态降为 `unknown`，不把该行隐藏。缓存状态为 `ready` 但根标记或身份不匹配时，列表显示 `wrong_root`；路径不可访问显示 `offline`，两者都禁用逐项操作。解除后的记录可以在总仓重新核验后重新打开。`unknown`、`missing` 仅展示，处置服务不注册 Agent/MCP 入口。

### 9.15 独立入库工作台（A2-05）

工作台异步动作在发起时固定会话及扫描选项，完成后只合并到该会话的草稿；只有活动会话与投影仍匹配时才更新当前 UI。会话选择及工作台投影分别以请求修订丢弃迟到响应；当前加载被新选择打断时释放加载/初始化门，让页面跟进新会话。`conversationStore` 按会话和草稿类别记录本地成功写入世代，旧读取不得覆盖读取期间已经成功持久化的新草稿。切换会话不重新受理入库或改变既有幂等键。

`src/stores/importWorkbenchStore.ts` 以 A1-06 会话草稿保存固定 `planRef`、`operationRef`、计划提交的幂等键和必要的可恢复选项；草稿不保存源路径或未经净化的扫描错误，持久警告只保存路径无关的分类计数。授权目录通过 `directoryRef` 准备计划；拖入或文件选择的原始路径只在当前 GUI 内存中保留，路径预览由 GUI-only `get_import_plan_preview_paths` 按当前主体和自有计划重新解析，不改变共享草稿/API 协议。计划来源丢失、变更或授权失效时返回路径无关的结构化错误，工作台保留计划引用并阻止提交，要求重新选择或准备。重开页面从草稿引用恢复计划和操作快照。入库页订阅当前 `activeRef`，初始化及会话变化后与工作台投影同步；若会话在加载中再次变化，加载结束后跟进最新选择。投影查询目标已经是活动会话时不重复调用会话选择，避免慢查询反写较旧的 `activeRef`；活动引用与投影未同步时，工作台按加载中处理并禁用提交、编辑、清理、交接及操作控制；store 也拒绝写入不匹配的会话草稿。

入库前先固定候选范围，再由用户显式提交。准备扫描/分类可按准确 `taskId` 取消；运行中的导入按准确 `operationRef` 取消或恢复。提交前持久化幂等键，超时/响应丢失后沿用同一个计划和键重试；若计划预览或来源已不可恢复，只要幂等键仍在，页面也保留不依赖路径的“重试并核对受理”入口，后端先查同键 operation 再决定是否需要当前计划。仅在确认请求本身返回、且尚未获得 operation 的明确未受理错误时释放该键，以便重新准备。受理后的会话关联或草稿保存失败仍保留原键并以其重试对账；提交结果不确定时保留该引用并阻止清除或换键，直到重新查询并收敛到原 operation。移除异常项生成排除项的新计划；计划已进入提交确认状态后不允许重建。操作进度从 operation 快照读取，`taskStore` 将最新快照与短暂任务卡分开保存，旧 operation 卡被新任务清理后仍不会回退到工作台最初的 queued 投影；同一 operation 每次终态修订都会重新触发素材/分面刷新和逐项结果查询。终态的新增、重复映射、失败计数及失败明细从逐项状态和最新持久收据重建，重启后仍可查看。结果查询在阻塞工作线程中执行；数据库锁仅覆盖必要快照读取，之后以 item ID 索引计划显示名，按线性时间重建结果，避免大计划下二次方扫描或锁内 CPU 遍历。终态结果读取失败时，工作台按 `operationRef` 显示可重试错误；同一 operation、同一 revision 的并发读取合并，新 revision 会发起新的结果查询，较旧请求的迟到结果不能覆盖新终态；重试时清除先前缓存，成功后显示新结果并清除错误。结果视图只返回计数、`itemRef`、显示名、状态和错误码，不暴露本地路径。

交接只为未提交计划或已受理操作追加 A1-06 关联，不触发计划重建、提交、重新扫描或重新导入；选项变化只影响后续显式提交前的重新准备。缺少 AI 不阻断手动入库。旧 `import_files` 直写 IPC 不注册为 Tauri 命令，正式写入只能从已批准计划进入操作队列；完成的导入继续触发原有色板补算和更新事件。`src/utils/importDropDispatcher.ts` 在 WebView 生命周期安装唯一原生拖拽监听，页面挂载只替换活动回调，卸载清除回调，应用根卸载时解除原生监听。计划中的源路径仍由受控计划服务用于核验和执行，不能把“草稿不含路径”误解为计划数据库不存储路径。

### 9.16 Pi Agent Host 进程与协议（A3-01）

`AppState.agent_runtime` 是单一 Rust 进程管理器；Windows x64 setup 注册事件适配器，Tauri 退出时调用 `shutdown`。`commands/agent_host_cmd.rs` 只暴露 GUI IPC，打开、发送、取消和关闭会话前按当前 GUI 身份验证其会话归属；外部 Agent/MCP 没有启动 Host 或任意指定本地路径的入口。会话索引/草稿仍属于 A1-06，只读消息投影由 `conversation.history`（Pi JSONL 唯一事实源）提供，历史 UI 交互和业务工具由后续 A3 卡接入，不能把本卡 IPC 入口说成完整助手体验。

`services/agent_runtime/` 只在 Windows x64 解析运行资源。开发模式使用固定 Node 与仓库 `packages/agent-host`，正式模式使用 Tauri resource overlay 中的 Node 和 Pi Host。`scripts/prepare-agent-host.mjs` 验证版本、官方归档/二进制/许可证摘要与 250 MiB 解包预算；`scripts/desktop.mjs` 的目标检查、开发和打包均生成/使用相应资源配置。Host 子进程从空环境启动、`PATH` 为空、只保留 Windows 必需运行变量，资源目录和可写会话目录分离；Host 保持 `modelNetwork=false`，只注册经 Rust 握手确认的只读能力工具，不读取全局凭据或模型缓存。Pi 可经受限 stdio frames 请求 Rust 网关。外发准入是**按配置推导**，没有用户可见的「允许联网」开关（2026-11-01 裁决，口径见[实现契约 §6.1](../plans/v2/12-implementation-contracts.md#61-a3-02-agent-模型网关接线)）：绑定服务、连接启用且密钥可用即视为就绪，`SwitchableGatewayTransport` 常驻装配（内部 `HttpGatewayTransport`：关闭重定向、关闭自动重试、默认 `no_proxy()`，仅当用户显式填写 `settings.agent_model_proxy` 时使用该代理——不继承系统/环境代理，TLS 校验开启，连接与流式读取都有截止时间并响应取消），代理变化经 `apply_model_proxy` 就地重建内部 transport，不必重启应用；`DisabledGatewayTransport` 保留为显式失败关闭的接缝（测试与将来降级装配用）。发送前 `GatewayBroker::preflight` 先判三件事并以固定错误码返回可行动原因：网络出口未就绪 → `AGENT_MODEL_NETWORK_DISABLED`（历史码名，语义为配置未就绪）、未绑定服务 → `AGENT_SERVICE_UNBOUND`、云端服务缺密钥 → `AGENT_CREDENTIAL_MISSING`，避免先起 run 再在网关深处失败。这条链只决定**能否外发**；模型能用哪层工具由会话权限档位决定，两者正交，档位定义只在[实现契约 §6.2](../plans/v2/12-implementation-contracts.md#62-agent-会话权限档位a3-10)。用户在服务地址里填的路径前缀**原样**拼接接口路径（`/chat/completions`、`/messages`），不再自动补 `/v1`，因此智谱 `/api/paas/v4`、火山 `/api/v3` 这类前缀不会被改写成 404。

Rust 与 Node 共用冻结的[Agent Host 协议 v1](contracts/agent-host-protocol-v1.md)：有界 newline-JSON 帧、schema v6 精确握手与精确工具声明、单调 Host sequence、单次 requestId、会话/run 引用和迟到事件过滤。schema v6 增加 `settled` 的可选 `stopReason`/`rawStopReason`/`usage`（区分正常结束/长度截断/失败并透出真实用量）与只读 `conversation.history` 投影；Host 复用 Pi 内置上下文自动压缩（阈值/overflow/可恢复 length，不自研），Rust 通过 `gatewayLease` 下发真实 `contextWindow`/`maxOutputTokens`/`reasoning`/`thinkingLevel`。schema v3 的运行错误可携带固定网关类别与安全 HTTP 状态，Rust 校验完整字段和取值后才转发给 UI；schema v4 增加双向能力调用帧，Host 只能声明并调用 Rust 已注册的工具；schema v5 让 `conversation.open` 携带会话权限档位和该档位裁剪后的工具集合，Host 不做档位判断、只照单注册并在 ack 里回读，由 Rust 断言裁剪确实生效。ACK 必须与请求 command、conversationId/runId 一致，否则按结果未知处理。Node stdout 以 FIFO 队列写帧，背压时等待 drain。每个数据目录仅允许一个租约所有者；恢复锁记录 PID 与实例 ID，进程意外退出后可由后续明确恢复回收；恢复只处理可确认的过期租约，不重放提示词或 operation。Rust 验证规范化后的数据根仍是应用数据目录的直接子目录，并以不共享删除的 no-follow 句柄固定数据根和 `sessions`。Node 对一次目录快照只发现名称以目标 conversationId 结尾的候选会话；每个候选都先通过 `session.pin_request` 由 Rust no-follow 固定，Rust 只接受对应 conversationId 命名的单链接普通文件。之后 Node 才读取会话头，SDK 只接收已固定的直接普通文件。新会话先独占写头并固定，再传给 SDK；关闭会话时按 conversationId 释放文件句柄，进程退出时清理剩余句柄。Rust 也约束 pin 文件名归属，避免 Host 释放其他会话的 pin。AgentRuntime 只在共享状态锁内复制 Host 引用，协议等待在锁外串行执行；关停可中断挂起请求并走独立的 Host shutdown 通道。发送 ACK 未确认时保留原 runId 并标为结果未知，禁止重发。Rust 拥有子进程和 Windows Job Object，异常或退出只回收自有进程树；正常退出预算为 1 秒，超时强制清理不计为正常退出成功。A3-02 已把 Agent run 租约、连接版本复核、凭据隔离、共享限流、上传 credit、按需流拉取和取消接到生产 Host/Rust stdio 链路；本机集成测试只注入内存 fixture；生产 transport 按 2026-11-01 裁决常驻装配（`SwitchableGatewayTransport`，无用户可见开关，仅代理可热切换），外发准入由绑定+启用+密钥推导而非开关。精确帧和安全不变量见[实现契约 §6.1](../plans/v2/12-implementation-contracts.md#61-a3-02-agent-模型网关接线)，卡状态与剩余验收见[项目计划](PROJECT_PLAN.md#9-20-agent开发状态)。聊天 UI、持久 transcript 投影、真实素材 UAT 与其他平台仍按对应卡验收。

### 9.17 Agent 选择快照与能力执行（A3-03）

Pi 工具面由 `src-tauri/src/contracts/tools.rs` 从冻结的能力契约派生：只有 caller 含 `pi` 的能力生成工具（**派生 18 项**，由 `contracts/tools.rs` 的测试硬编码断言），`plans.approve` 与 `operations.resume` 不生成工具名，工具名固定为 `tea_` + 能力 ID。派生面与实际注册面**不是同一件事**：`services/agent_runtime/capabilities.rs` 的 `SUPPORTED_PI_CAPABILITIES` 当前注册 **16 项**（`assets.search`、`tagging.prepare/submit`、`exports.prepare/submit`、`assets.move.prepare/submit`、`assets.trash.prepare/submit`、`assets.restore.prepare/submit`、`operations.get/cancel`、`workflows.load/prepare/submit`），差的 2 项正是 `imports.prepare`/`imports.submit`——契约声明了 `pi` caller，但 A5-03 已裁决 2.0 不发布（见 §9.17 下文与 `docs/contracts/mcp-bridge-v1.md` §4.1.1）。任何调用都必须由宿主构造 `CallerContext` 后再经 `services/agent_tools.rs` 执行；模型传入的主体、批准、路径和 SQL 一律不作为输入。

`assets.search` 是首个接通的领域能力：它复用 `db/assets` 的搜索与全量 ID 查询，把返回页映射为契约 `SearchPage`，并用 `services/selections.rs` 持久化**确切结果集**到 `agent_selections`（`db/selections.rs`）。选择引用只在创建它的主体、同一 `library_epoch` 和 1 小时有效期内可用，解引用时重新校验；快照上限 10,000 项，超出即标记 `truncated`。分页 `cursor` 当前显式返回 `UNSUPPORTED_CAPABILITY`，不重复返回首页。

工具调用链路：Node 在 `packages/agent-host/src/tools.mjs` 的 `PI_TOOL_NAMES` 里注册与 `SUPPORTED_PI_CAPABILITIES` **精确相等**的 16 个 `tea_*` 工具（`declared_tools_match` 在握手时逐名校验：多一个、少一个、重名都拒绝），模型发起的调用经 `capability.request`（能力 ID + 有界 Base64 参数）交给 Rust；Rust 只用可信 Host 身份和当前活动 run 构造 `CallerContext`，丢弃模型传入的 `schemaVersion` 后校验契约输入，在独立线程执行并把 `capability.result` 写回（成功回传记录，失败只回固定 `AgentErrorCode`）。没有活动 run、能力未注册、载荷超限、会话权限档位不允许该能力或 schema 不符均失败关闭，不伪造成功结果。档位在 `conversation.open` 时决定 Node 注册哪些工具（`read` 4 项、`standard` 12 项、`full` 16 项，语义见[实现契约 §6.2](../plans/v2/12-implementation-contracts.md#62-agent-会话权限档位a3-10)），Rust 侧另有一道按 `capability_descriptor.access` 推导的独立能力门，因此裁剪工具表只是体验优化，越档调用一定被拒。`assets.trash.prepare` 只把 search 产生的选择集整理成待审计划并核对素材仍全部活动；`assets.trash.submit` 必须消费 GUI 批准，在同一事务内软删精确选择集、写入步骤并结算操作，未批准返回 `APPROVAL_REQUIRED`、集合漂移返回 `PLAN_CHANGED`。`exports.prepare/submit`、`assets.move.prepare/submit`、`assets.restore.prepare/submit`、`tagging.prepare/submit`、`workflows.load/prepare/submit` 已按同一形态在 `services/agent_tools.rs` 实现（含各自的冻结快照与 `target_ref`），并**已加入 `SUPPORTED_PI_CAPABILITIES` 与 Node 工具表**；任何一项的增删都要三处同时改（`SUPPORTED_PI_CAPABILITIES`、`tools.mjs` 的工具定义与 `PI_TOOL_NAMES`、契约能力表），否则握手失败。操作查询与取消复用领域服务的归属校验和取消语义，只作用于当前调用方自己的操作。`trash.empty.*` 已实现但 callers 收紧为 `["gui"]`（A4-04），因此既不派生 Pi 工具也没有模型入口——「模型不能伪造维护身份」不靠校验，而是根本没有入口；`imports.*` 的 Pi/MCP 入口按 A5-03 裁决在 2.0 不发布。

### 9.18 助手会话界面与消息流（A3-04，进行中）

助手页面（`src/pages/AssistantPage.tsx` + `src/components/assistant/`）只经 `src/api/agent.ts` 调用 `agent_host_start`、`agent_host_open_conversation`、`agent_host_send`、`agent_host_cancel_run`、`agent_host_close_conversation`，并订阅 `agent-host-event` 原始 Host 帧，不直接 `invoke`。会话索引、标题、草稿、附件与计划/任务链接仍由 A1-06 的 `services/conversations.rs` 与 `conversationStore` 提供，助手层不自建第二套会话或业务进度。

消息流归 `src/stores/agentSessionStore.ts`，按稳定 `conversationRef` 分桶。`applyFrame` 以 `(hostInstanceId|runId, sequence)` 单调去重：同序号重放返回原状态对象，不重复追加文本；终态之后到达的旧序号不得把状态退回流式；旧 `hostInstanceId` 的迟到帧只提示一次异常、不修改当前会话状态。协议事件不充当持久状态，重新进入会话时计划卡与任务卡一律经 `getConversation` / `listPendingPlans` / `getOperation` 回读真实快照。

计划归属不依赖"最近一个全局 ID"：`hydrate` 以「已通过 `conversation_links` 链接到该会话的待审计划」∪「本次发送后新增的计划」判定，确认后把计划引用写回该会话；`agent_host_send` 返回 `outcomeUnknown` 时单列为待核对异常并保留 runId，供用户发起精确取消，绝不重放提示词。界面把「停止回答」（取消当前 run）与「取消任务」（`cancel_operation`）呈现为两个独立动作，浮层与完整页共享同一会话 store（A3-09 承接）。

底栏入口按 PRD 第 8 节排为「入库｜素材库｜中央 Logo（助手）｜打标」，中央 Logo 只负责导航，首次进入新建对话、有历史恢复最近对话；快捷项增删隐藏排序与百宝箱说明页属 A3-05（见 §9.19）。

### 9.19 底栏入口、工作状态区与设置入口（A3-05，进行中）

底栏的可配置入口只有一份受控内置目录：Rust 侧 `src-tauri/src/db/settings.rs` 的 `NAVIGATION_SHORTCUT_IDS = ["import","library","ai","treasure"]` 是唯一允许值的来源，前端 `src/types/settings.ts` 的同名常量与它逐字对齐。`Navigation { shortcuts: NavigationShortcut[], settingsPinned }` 与 `Settings` 一起持久化，`get_settings` 的归一链调用 `normalize_navigation`：丢弃未知 id（任意 URL、命令、路径或未来插件标识都不可能进入底栏）、丢弃重复 id、保留首次出现的相对顺序、最多保留目录长度；`had_entries && normalized.is_empty()` 时回落默认目录，因此**只有"全部无法映射"才恢复默认，显式空数组代表用户主动隐藏了全部入口**。隐藏入口只影响渲染，功能页、路由与数据都保留。`src/utils/normalizeSettings.ts` 的 `normalizeNavigation` 保持同一口径，并对"旧库完全没有 `navigation` 字段"直接返回默认目录——这与"显式空数组"是两种不同输入，不能合并处理。

`BottomBar` 用 `grid-cols-[1fr_auto_1fr]` 三列栅格：左右两列渲染按用户顺序过滤后的快捷项，中央列固定是 Logo 助手入口，因此入口数量增减都不会移动 Logo 的窗口正中位置。「设置」不是快捷项而是一个独立固定项，只在 `settingsPinned` 为真时追加在可配置入口之后。`src/pages/TreasurePage.tsx` 只声明"百宝箱尚未开放"并指回设置，不建立任何插件宿主或扩展点（属 2.5），避免提前引入可执行扩展面。

左上角 `src/components/layout/WorkStatus.tsx` 的状态只由 `taskStore` 的真实任务投影（`projectWorkStatus` 按 进行中→未完成→已完成 排序），不设任何假计时器或估算百分比：多任务时按 `WORK_STATUS_ROTATE_MS = 6000` 缓慢轮播，鼠标悬停或键盘聚焦即暂停，滚轮与 ArrowUp/Down、PageUp/PageDown 可手动切换并在 `WORK_STATUS_MANUAL_HOLD_MS = 8000` 内不被自动轮播抢走；`prefers-reduced-motion: reduce` 时默认不自动轮播，但仍保留手动切换。`TitleBar` 左侧因此是"品牌标志（纯装饰）→ 工作状态区 → 文字「设置」按钮"，桌面端窗口控制仍是三个原生按钮。

设置页「外观与浏览 → 底栏入口」只提供上移/下移、显隐、恢复默认入口与固定「设置」四项操作，全部落到同一个 `navigation` 字段；它不提供自由文本、URL 或命令输入，也不新增功能页面。

### 9.20 首次引导、服务选择与目录授权（A3-06，已收口）

`src/components/onboarding/OnboardingWizard.tsx` 是基于共享 `Modal`（`wide`）的三步向导：总库位置、AI 服务、基本操作。三步都可跳过，且"不配置 AI"不是错误状态——文案明确写入库、素材库和手动标签在没有 AI 服务时仍可用，向导不设"必须先配 AI 才能导入"的门槛。选择总库时只调 `@tauri-apps/plugin-dialog` 的 `open` 拿原始路径，再交给 `settingsStore.save`，因此 `save_settings` 的总仓 CAS 采纳与 `managed_library::prepare_initial_root`/`commit_initial_root` 仍全部发生在后端，向导不自建第二套总仓切换路径。向导完成的持久化标记仍是 `Settings.tutorial_prompt_handled`，写回失败时停在末态并给出「继续使用」（`continueAfterTutorialSaveFailure`），不假装保存成功。

目录授权只有一条信任链：`authorization::grant_selected_directory` 由原生选择器拿到目录，`remembered=false` 时签发 12 小时会话授权并绑定 `owner_session_id`，`remembered=true` 才跨会话保留且在 `list_remembered_grants` 中可见。`DirectoryPurpose::access_mask()` 决定权限位：`ImportSource` 只给 `ACCESS_READ`，只有 `ExportDestination` 才带 `WRITE | EXPORT`，所以"记住读取"从数据层就不可能外扩成写入或上传。导入页的「记住这个目录」checkbox 默认关闭（`pickDirectoryGrant("import_source", false)` 只授权本次读取），勾选后才传 `true`。`src/components/settings/DirectoryGrantsPanel.tsx` 是 `listDirectoryGrants`/`revokeDirectoryGrant`/`revokeAllDirectoryGrants` 的 GUI 消费者，逐条展示用途、权限位、是否含子目录、失效时间与 `available`（被移动/删除/换总库后重新核验为不可用），并声明"记住读取不等于入库、写入或外发给 AI 服务的许可"。

输入区的服务信息来自 `getAiUsageBindings()` 与 `listAiConnections()`：`Composer` 展示当前 Agent 绑定的服务与模型，并允许通过 `setAiUsageBinding("agent", …)` 切换——只作用于后续调用，已批准任务按计划快照执行，从不回显密钥值。

**"本机/在线"必须由地址推出，不能采信声明。** `src/utils/endpointScope.ts` 的 `classifyEndpoint(baseUrl)` 解析 http(s) 主机，返回 `loopback`（`localhost`、`127.0.0.0/8`、`::1`）/ `remote` / `invalid`；`Composer` 的 `describeModel`、下拉标注与 `agent-outbound-summary` 全部使用它，`deployment` 只用来判断声明与地址是否矛盾并显式提示（`deploymentMatchesEndpoint`）。理由是 `AiConnectionManager` 的地址是自由文本、`deployment` 是表单字段，二者没有一致性约束——若按声明输出"不会离开这台电脑"，就等于把用户填的标签当成地址事实，正是 A3-06 停下条件禁止的形态。

**故障文案按部署方式分流。** 云服务缺密钥是真故障（网关 `gateway.rs` 只对 `deployment == "cloud"` 强制凭据）；本机服务（例如 `ServiceManagement.syncLocalModel` 写入的 `apiKey: null` 档案）缺密钥是正常状态，因此 `Composer` 的 `credentialProblem` 只在云服务上判 `configured`、对本机只把"密钥环不可用"当故障。被停用或缺凭据时以 `role="alert"` 说明发送会失败、已写内容不会丢，且"本应用不会自动改用其他服务"，不静默回退到本机或云端。

**外发摘要只说真实发生的外发。** Agent 通道当前只发文字：`agent_runtime::ManagedHost::dispatch_run` 的 payload 只有 `{conversationId, runId, text}`，网关 `validate_text_blocks` 只接受 `type == "text"`，冻结契约（实现契约 §4.4 / `12-implementation-contracts.md:124`）明写"消息只接受文本：不支持图片、音频、视频、文件"。所以 `agent-outbound-summary` 说"本次只发送你输入的文字和必要的素材名称等信息，不发送图片内容本身"，并指向"需要图片参与处理（例如自动打标）请走对应的独立计划"——真正会压缩发图的是打标链路（§9.14），由它自己的计划摘要写明 `imaging::AI_IMAGE_MAX_PX` 与接收方。给出"发送压缩 JPEG（最长边 1024px）""助手会先请求原始清晰度"这类承诺会虚构一项并不存在的能力。

**"记住目录"必须有对得上的复用入口。** `remembered=true` 的授权只持久化 grant 不持久化句柄（句柄绑定会话/实例），所以复用必须经 `reissueDirectoryGrant(grantRef)` → `authorization::reissue_directory_handle` 换发新句柄。`useImportWorkbench` 的 `rememberedGrants`（只取 `purpose == "import_source"`）与 `chooseRememberedFolder` 把这条链路接到入库页与助手右侧工作台：可用项一键复用并走同一条 `prepareDirectory`，`available === false` 的项如实标注"目录已不可用"且不可点，绝不再弹一次原生选择器冒充"记住了"。

**引导向导第二步要报服务可用状态。** `OnboardingWizard` 的 `serviceIssues` 按绑定服务的存在性、`enabled`、部署方式与 `credentialStatus` 给出"已停用/密钥不可用/本机未启动"等可执行说明，并始终附带"入库、素材库和手动标签不受影响"——未配置或配置坏掉 AI 都不阻断手动流程，这正是"不配置 AI 也能用"的落点。

### 9.21 助手右侧导入与标签审核工作台（A3-07，进行中）

助手页右栏（`src/components/assistant/AssistantWorkbench.tsx`）不复制领域状态：入库侧一切读写经 `src/hooks/useImportWorkbench.ts`，该 hook 就是 `src/pages/ImportPage.tsx` 使用的同一行为层（同一个 `importWorkbenchStore` 投影，`prepareFiles`/`prepareDirectory`/`submitPlan`/`loadOperationResult` 都只有一份实现）。因此同一批文件在独立入库页与右栏看到的是同一份草稿、同一组待确认计划与同一组 operation 结果，「开始入库」在两处都不会各自受理一次。标签审核侧经 `src/components/ai/SuggestionReview.tsx`，与 `src/pages/AiTaggingPage.tsx` 共用切图、标签编辑、数值项决策、`Filmstrip` 多选套用与"全部确认"。这两个共享组件都不调用 `invoke`：确认走 `src/api/ai.ts`，入库走 `src/api/import.ts`。

布局：`wide` 时 `w-[620px]`、否则 `w-[380px]`，可整体收起（默认不占宽度）。宽窗加宽时收起左侧历史但聊天列始终保留；窄窗（`NARROW_ASSISTANT_QUERY = "(max-width: 1099px)"`）默认收起历史并提供可开合浮层（遮罩 + `absolute inset-y-0 left-0`），避免三列在 960×640 下互相挤没。`useMediaQuery` 只决定展示，不参与任何权限判断。

对话绑定：面板比较 `workbench.conversationRef` 与传入的 `conversationRef`，不一致时只渲染"正在切换到当前对话的入库草稿与打标批次"的 `role="status"` 说明并不提供任何提交入口——后台对话的任务照旧继续，但不会把草稿或提交动作抢到当前右栏。附件只是引用（`ConversationAttachment`），只有用户在工作台里显式选文件/目录才会进待入库清单，附件本身不触发入库。

收据到打标的接线（契约 §4.4 的 GUI 入口）：`ImportResultView` 带 `source_step_ref` 与 `duplicate_items`，因为打标准备按 `(operationId, stepId)` 寻址。`aiStore.prepareFromImport` 调 `aiPrepareImportBatch`，范围只来自逐项成功收据；`duplicate_mapped` 项在面板里逐条 checkbox 且默认不勾选，勾选后才把 `itemRef` 传入，后端 `import_selection` 再校验传入 id 必须全部命中真实重复项（否则 `PLAN_CHANGED`），所以不存在从数量或目录推断对象。准备只产出待审批计划：`window.confirm` 展示计划摘要 → `approvePlan` → `aiSubmitTaggingPlan` → 用户点「开始打标」才 `aiStartTagging`。审批与启动保持两个独立动作，未启动不联网。

### 9.22 显式交接与素材库本次结果视图（A3-08，进行中）

交接的对象是**不可变 id 快照**，不是"当前选择"这个活引用。`src/api/selections.ts` 暴露 `snapshotSelection(query, assetIds, total)`、`summarySelection(selectionRef)`、`pageSelection(selectionRef, offset, limit)`，对应 `src-tauri/src/commands/selections_cmd.rs` 的三个 GUI-only 命令（`snapshot_selection`/`summary_selection`/`page_selection`，全部先 `gui_caller`）。快照落在 `agent_selections` 表：`SELECTION_TTL_SECONDS = 3_600`、`MAX_SELECTION_ASSETS = 10_000`，记录 `principal_id`/`library_id`/`library_epoch`/`query`/`asset_ids`/`total`/`truncated`。`selections::resolve` 复用既有校验：principal 不符 → `Unauthorized`，过期或不存在 → `ReferenceExpired`，库世代改变 → `LibraryChanged`。因此素材库之后再改筛选、改选择、甚至删除素材，都不会改写这份集合——这正是"固定交接"的含义。

数量口径由后端统一算出，UI 不自行推导：`SelectionSummary { snapshot, available, missing }` 只查 `count_active`，`page` 的 `available`/`missing` 也**描述整个快照**而不是当前分页窗口，否则结果视图头部的"多少已不可用"会随滚动漂移。`db::assets::by_ids_ordered_active` 按传入 id 顺序保序（`pos: HashMap<i64, usize>` + `sort_by_key`）并额外过滤 `a.deleted_at IS NULL`——`VERIFIED_ASSET_SCOPE` 本身不含软删过滤，这一步漏了就会把已删项当成可用。缺失成员不静默收缩：`missing` 单独计数并在界面明说"N 项已被删除或移出总库，不会显示"。

会话侧新增 `selection` 链接种类：`conversation_links.link_kind` 的 CHECK 从 `('plan','operation','batch')` 扩为四项，`migrate_structure` 走幂等重建（查 `sqlite_master` 原文里是否已含 `'selection'`，缺则 `ALTER TABLE … RENAME` + 重建 + `INSERT … SELECT`），不改已发布迁移。`conversations::link` 的白名单同步放宽，前端 `ConversationLinkKind` 增加 `"selection"`。链接只记录"这次交接了哪份快照"，永远不重新绑定或重新准备它。

前端接线：`src/components/library/HandoffDialog.tsx` 在固定之前先说明交的是什么（条数、截断、只读），`confirm()` 顺序为 `snapshotSelection` → 复用或新建会话 → `link(ref, "selection", selectionRef)` → `select(ref)` → 回报 `onHandedOff`。`src/stores/selectionResultStore.ts` 只存"看哪份快照"（`selectionRef`/`title`/`query`），刻意不存素材行与游标，避免与 `libraryStore` 的领域状态互相污染。`src/components/library/SelectionResultView.tsx` 按 200 一页只读分页、用 `revision` ref 丢弃迟到分页，`AssetCard` 传空的 `onSelect`/`onContextMenu`（只读，仅双击可进 Viewer），底部"加载更多"/"已显示全部"。`src/pages/LibraryPage.tsx` 的判定顺序是 `preview` → `result` → 正常浏览，结果视图与 Viewer 一样整体替换库页，退出后筛选、选择与滚动位置原样恢复，不创建分库。

助手侧读同一份摘要：`agentSessionStore.linkedSelections` 在 `hydrate` 时从 `view.links` 过滤出 `kind === "selection"`，`SelectionCard` 与右栏 `SelectionPreview` 各自回读 `summarySelection` 后渲染数量与「查看本次结果」。点按钮时**先** `useSelectionResultStore.open(...)`、**再** `app:navigate` 到 `library`，避免先切页闪一帧空视图。入库页的"交给助手"走 `handoffPlan`/`handoffOperation`（只 `linkConversation` + `select`，不重新准备或提交），并显式说明交出去的是计划引用。

### 9.23 应用内悬浮助手（A3-09，已收口）

悬浮助手是**应用内**的一层浮层，不是系统桌面常驻窗口（PRD 第 8 节）。实现见 `src/components/assistant/FloatingAssistant.tsx`：默认只渲染一个 48px 的圆形 Logo（`aria-label="打开助手浮窗"`，与底栏中央 Logo 的 `aria-label="助手"` 区分，避免同名入口歧义），点开后展开 `PANEL_MAX_WIDTH = 380` / `PANEL_MAX_HEIGHT = 520` 的面板。

位置与吸附：`defaultPosition()` 落在右下角；`clampPosition` 在每次渲染与 `resize` 时把坐标夹到 `[EDGE_MARGIN, width - LOGO_SIZE - EDGE_MARGIN]`，因此缩窗不会把入口留到窗口外（A3-09 验收）；`snapPosition` 在松手后吸附到最近的水平边缘、垂直位置保持；`readPosition`/`writePosition` 只记忆位置，本地存储损坏时退回默认值而不影响助手可用。`panelRect` 依 Logo 位于左半/右半决定面板开在右侧/左侧，并整体夹紧到窗口内。

不维护第三套聊天数据：消息来自 `agentSessionStore.runtimes`（`runtimeOf`），会话来自 `conversationStore`；`subscribe()` 内部已有模块级幂等守卫，浮层与完整助手页共享同一份事件订阅，浮层只在自己的 `useEffect` 里调用、不新建订阅。「新建」走 `create("新对话")` + `select` + `open(ref, "new")`；点开入口时 `continueLatest()` 先 `refresh()` 取 `activeRef ?? 最近的活动会话`，找不到则提示「还没有对话。点「新建」开始。」而不是伪造会话。

审批边界：浮层对 `linkedPlans[activeRef]` 只渲染「能力名 · N 项 · 摘要」与「查看并确认」，**不提供任何批准/全部确认/清空按钮**（A3-09 验收：浮层不能直接批准复杂或清空计划）。列表**只保留 `reviewStatus === "pending"` 的计划**（区块名就叫「待确认计划」，把已批准/已过期的计划列进去会让用户以为还有东西要审），能力名复用 `AgentCards.capabilityLabel`，让「移入回收站」「执行导入」这类计划在浮层里可分辨。「查看并确认」不只是裸导航：先 `useAssistantFocusStore.focusPlan(planId)` 写下要定位的计划，再 `openConversation(activeRef)` + `app:navigate` 到 `assistant`；完整页读这个目标后滚到对应 `[data-plan-id]` 卡片、加 4 秒高亮并**消费**目标（读取即清除，下次进页面不会凭空高亮），连续 3 次找不到才放弃，避免计划仍在回读时过早丢目标。「完整页面」按钮不带目标，只切页。

可达性与遮挡：`open` 变 true 时焦点移进面板（`tabIndex={-1}` + `focus()`），变 false 时焦点还给 Logo（`focus({ preventScroll: true })`），**首次挂载只记录状态不移动焦点**——浮层在每次离开助手页时都会重新挂载，挂载就 focus 会把焦点从搜索框抢走（`openedOnce` 守卫）。面板打开时 `Escape` 在捕获阶段监听并 `stopPropagation`，但**先检查 `document.querySelector('[aria-modal="true"]')`**：真正打开的模态弹窗（`src/components/common/Modal.tsx`，`z-50`）才拥有 Esc，浮层是 `z-40` 的 `aria-modal="false"` 非模态层，不能连带收起。窗口宽度小于 `PANEL_MAX_WIDTH + 2×EDGE_MARGIN` 时 `panelRect` 先把可用空间算出来再取小（旧实现用 `Math.max(260, …)` 作最小宽度，在 292px 以下会宽出窗口）。`src/App.tsx` 的条件为 `!viewerOpen && page !== "assistant" && settings?.floatingAssistant !== false`：大图浏览时隐藏（不遮挡全屏预览）、完整助手页内不重复显示。

设置开关：`Settings.floating_assistant` 新增于 `src-tauri/src/db/settings.rs`，`#[serde(default = "default_true")]` 且 `impl Default` 为 `true`——旧设置缺字段时按开启处理，所以消费方必须写 `!== false` 而不是 `=== true`。前端 `src/types/settings.ts` 的 `floatingAssistant?: boolean` **必须是可选**：设成必填会让 11 个测试文件的 `Settings` 字面量报 `TS2741`；归一化在 `src/utils/normalizeSettings.ts` 用 `asBool(r.floatingAssistant, true)`。设置页在「外观与浏览 → 素材框」之后新增「悬浮助手」组，文案明说「关掉只隐藏入口，正在执行的任务不受影响」。

### 9.24 外部 MCP 本机接入（A5-01，进行中）

外部客户端只能通过两段链路进入：`MCP 客户端 ──stdio(JSON-RPC/MCP 2025-11-25)──▶ packages/mcp-bridge ──Windows 命名管道──▶ Rust 宿主`。行为事实只有 [契约 `mcp-bridge-v1.md`](contracts/mcp-bridge-v1.md)，本节只记录实现落点。

桥接进程（`packages/mcp-bridge/`，零依赖，11 个文件 50 个 Node 测试）**没有数据库连接、没有文件写执行权、没有网络访问**，`dependencies` 为空对象；它把 MCP 的 5 个方法翻成本机 IPC 帧，并把宿主错误码映射到契约的 8 个端到端码（`HOST_CODE_MAP`，未知码回落 `HOST_UNAVAILABLE`）。宿主未运行时 `tools/list` 返回空表、`tools/call` 返回 `HOST_UNAVAILABLE` 且 `isError`，**不静默另起一个写库宿主**（PRD/A0-02 生命周期）。第 8 个码 `INVALID_INPUT` 是 A5-03 用真实客户端发现的缺口：载荷不合规与宿主不可用必须可区分，否则客户端会去重启一个健康的应用。

宿主侧 `src-tauri/src/services/mcp_bridge.rs` 负责监听与握手。分帧与内部 Host 协议一致（UTF-8、每行一个 JSON、LF 分隔、单帧 65,536 字节）；`frame_decoder_respects_the_contract_limits` 覆盖超限、空帧、非对象与截断。握手 6 步顺序见 §5.7「外部 MCP 接入（A5-01）」：**先校验帧形状再触碰数据库**，因此伪造帧无法用来探测库状态。

能力范围在宿主侧裁剪：可发布集合恰 5 项（`assets.search`/`assets.trash.prepare`/`assets.trash.submit`/`operations.get`/`operations.cancel`），`welcome.tools` 只含「连接范围 ∩ 可发布集合」并按 name 升序，`inputSchema` 从 `docs/contracts/agent-v1.schema.json` 的 `x-capabilities[cap].input` 运行时裁剪出传递闭包（与 `contracts/tools.rs` 同一做法），避免把整份 schema 塞进每一帧。

**为什么这 5 项里永远不会有 `imports.*`/`exports.*`**（A5-03 设计结论）：这两类都要求一个 `import_source`/`export_destination` 目录句柄，而 `authorization::grant_selected_directory` 只接受 `CallerKind::Gui`，`validate_handle_mask` 又逐项比对 principal+instance+session —— MCP caller 的 `session_id` 恒为 `None`（`TrustedAdapterIdentity::Mcp` 只带 principal 与 instance），所以为 GUI 会话签发的句柄在 MCP 侧解析必然失败。`validate_grant` 对 `remembered=true` 只校 principal、确实能让外部连接通过，但那要求用户手工把 `directoryRef` 抄进客户端配置——把「会话隔离」换成「用户别抄错」，所以不做。`import_prepare` 的 `paths` 通道（无授权裸路径）也**不得**被任何 Adapter 复用：它只服务 GUI 工作台的重新准备，评估用例 E32 已把「裸路径变成目录授权」列为禁止副作用。结论冻结在 [MCP 桥接契约 §4.1.1](contracts/mcp-bridge-v1.md#411-为什么-20-不发布-imports设计结论不是待办)。

配对数据落在 `src-tauri/src/db/mcp_pairings.rs` 的 `mcp_connections` 表（`capabilities` 是 JSON 数组，损坏时降级空数组不 panic）；`revoke` 用 `revoked_at = COALESCE(revoked_at, ?2)` 让首次撤销时间获胜，重复撤销幂等。设置页 `McpBridgePanel` 是唯一管理入口：创建时默认只勾查询类两项，写类能力必须用户显式勾选，撤销按 `connectionId` 立即断开在途会话。

`Settings.mcp_bridge_enabled`（`#[serde(default)]`，默认 `false`）是唯一开关；`lib.rs` 的 `mcp_cmd::start_from_settings` 在 setup 阶段读取它，关闭时删端点文件并关闭全部会话。开关与端点文件的关系是：**有端点文件只说明宿主这一进程曾启用过，不代表任何连接仍被授权**——授权只看 `mcp_connections` 的 `revoked_at`。

### 9.25 外部计划的可审查来源与拒绝（A5-02，进行中）

外部客户端准备的计划会出现在可信界面的待审列表里（`plans::pending_plans` 的 SQL 只按 `library_id`/`library_epoch` 过滤，本来就不按 principal 分栏），但审核者原先无法从 `PlanPreview` 判断「这是谁在申请写入」。A5-02 补齐两件事，都不新增业务实现。

**来源归属**：`docs/contracts/agent-v1.json` 的 `records.PlanPreview` 增加必填 `originKind`（`CallerKind`）与可选 `originLabel`（`ShortText?`），重新生成四件绑定。`plans` 表新增可空列 `origin_kind`（幂等 `ALTER TABLE`，**不回填**）：`None` 表示 A5-02 之前写入的行，读取时按 `gui` 呈现——猜测会把编造的来源摆给审核者，缺字段才是诚实的「未知」。名称**不持久化**，每次回读走 `db::mcp_pairings::find_by_principal`（未撤销连接优先，其次最新），所以客户端改名后所有界面立刻一致、配对被删后显示「外部客户端（已不在配对列表中）」而不是过期名字。前端唯一文案出口是 `AgentCards.tsx` 的 `planOriginLabel()`。

**拒绝路径**：`PlanReviewStatus` 五值里没有 `rejected`，因为既有语义里 `invalidated` 就是「此计划不再有效」，A5-02 复用它（不改 CHECK、不加迁移）。新增 `db::plans::reject_plan`（`WHERE review_status='pending'`，**返回是否命中**——`invalidate_plan` 返回 `()` 无法告知，用它会导致并发审批时骗审核者）、`services::plans::reject_plan`、Tauri 命令 `reject_plan` 与 `src/api/plans.ts::rejectPlan`。判定与批准对称：`PlansApprove` 入口 → `validate_caller` → `load_plan_for_review`（不校 principal，因此 GUI 能拒绝 MCP 的计划）→ `review_ref` 必须匹配 → 过期转 `expired` 回 `PLAN_EXPIRED` → 仅 `pending` 可拒。**已批准计划拒绝回 `PLAN_CHANGED`**：撤回批准会与在途 submit 赛跑，撤销执行只能走取消任务。拒绝不创建 operation，重复拒绝幂等。

**句柄边界说明**：`imports.*`/`exports.*`/`assets.move.*` 的 MCP 路径架构性不可行——`grant_selected_directory` 只接受 `CallerKind::Gui`，`validate_handle_mask` 要求 `handle.session_id == caller.session_id`，而 MCP caller 的 `session_id` 恒为 `None`；因此外部客户端能到的最远一步是「受控查询 + 准备计划」，执行仍由可信界面驱动。这是「客户端不得与本地会话混淆权限」的实施方式，不是缺口。

**待审入口不依赖会话**：外部计划进不了助手页不是权限问题，而是归属问题——`conversations::ensure_plan_linkable` 对 `origin_kind=="mcp"` 一律 `Unauthorized`（只有 `gui`/`None` 与 `pi` 两种来源能挂进会话），所以助手页与浮层读的 `linkedPlans[activeRef]` 结构上永远不含它们。入口因此放在**应用层**：`src/components/plans/PendingPlansPanel.tsx`（照 `WorkflowPanel` 的单例面板范式挂在 `src/App.tsx` 外壳，与页面切换无关），由 `src/stores/pendingPlansStore.ts` 只持 `open/plans/loading/error` 四个界面字段，**不缓存计划副本**——每次打开重新挂载即向后端 `list_pending_plans` 重读，每次批准/拒绝后无论成败都再读一次。回读而非就地改本地状态的理由：动作失败的原因可能是「已过期」或「已被别处处理」，只有后端列表是真实结论。面板刻意**不复用 `AgentCards.tsx::PlanCard`**：`PlanCard` 经 `useAgentSessionStore` 的 `approvePlanCard` 提交，而那个 action 靠 `linkedPlans` 反查所属会话、找不到就完全不刷新（对无会话归属的外部计划恒为真）。复用限于三个已导出的纯函数 `capabilityLabel`/`planOriginLabel`/`planTargetLabel`。**`reviewKind==="permanent_trash"` 在通用面板只读**：清空回收站有专属确认交互（`trash_cmd::empty_trash` 自己 `prepare → approve → submit` 全程 GUI 自批，前端入口在删除对话框），通用面板放按钮等于绕过专属确认，所以那一类只渲染一句说明、不给任何按钮。此入口不新增任何权限：后端 `plans::pending_plans` 本来就按库世代 + 未过期 + `pending` 筛选、不按来源过滤，`PlansApprove` 入口仍只接受 GUI。

### 9.26 真实客户端互操作与写队列统一（A5-03，进行中）

A5-01/A5-02 的全部证据停在「直接构造 `BridgeHost` 调 `handle_hello`/`handle_frame`」，`McpBridge::start()` 起监听器这条路径从未被测过。A5-03 补上真实链路，立刻暴露三处宿主缺陷（详见[契约 §6](contracts/mcp-bridge-v1.md) 与 `docs/PROJECT_PLAN.md` 的 A5-03 段）。

**两个真实客户端放在独立包**。`packages/mcp-interop/`（`private`，唯一 `devDependency` 为官方 SDK）含 `client-node.mjs`（官方 `@modelcontextprotocol/sdk` 1.32.1）与 `client-python.py`（PyPI `mcp` 1.28.1），两者**不共用一行实现代码**。刻意不装进 `packages/mcp-bridge/`：桥接有一条门禁断言其 `dependencies` 与 `devDependencies` **都为空**（「零依赖所以可审计出无 DB、无写权」是 A5-01 冻结契约的一部分），为装测试依赖放宽它等于拆掉可审计性。代价是 `npm run test:mcp-interop` 成为独立门禁。

**客户端只做一步**。两条客户端共用 `--mode discover|prepare|submit|submit-unapproved|status`，每次调用完成一件事并向 stdout 写单行 JSON。剧本由 `src-tauri/tests/mcp_interop.rs` 驱动，因为 GUI 审批必须插在两次调用之间，而批准只有可信 GUI 能做——客户端永远拿不到批准能力，这一点由结构保证而非约定。

**链路**：客户端 → stdio(JSON-RPC/MCP) → `packages/mcp-bridge` → Windows 命名管道 → 本进程 `McpBridge` → `dispatch_capability`。测试里没有任何 mock。

**三处被测出的宿主缺陷**（都是真 bug，不是测试写法）：①`listener` 把端点文件里的**裸** `pipeName` 直接传给 `ServerOptions::create`，Win32 当相对路径处理报 `os error 123`；修法是只在调用处拼 `\\.\pipe\` 前缀，端点文件格式与 Node 侧 `pipePath` 都不动（裸名是对的设计，拼接责任在调用方）。②工具 `inputSchema` 缺根 `"type": "object"`，官方 SDK 在 `tools/list` 上整份判无效，`--mode discover` 直接失败。③契约 §5 会回 `INVALID_INPUT`、§6 的端到端码表却没有它，桥接兜底成 `HOST_UNAVAILABLE`——客户端因此会去重启一个健康的茶馆；修法是把它补成第**八**个端到端码，并让客户端知道「改请求，别重启，也不是权限问题」。

**写队列统一**：`gui_pi_and_mcp_writes_share_one_queue_in_arrival_order` 交错发起三个来源的写任务，验证共用同一条 `OperationQueue`。两条真实约束被写成测试而不是绕开：`plans.created_at` 精度是**秒**，同秒到达只能按 `operation_id` 排（`same_second_arrivals_are_ordered_without_regard_to_source` 显式断言「同秒不区分来源」）；`claim_next` 会把槽位占给返回的任务，**任务结束不自动释放**，必须显式释放否则后续任务永远排队。

**选择集合快照冻结**：`an_external_plan_only_acts_on_the_selection_it_froze` 在计划批准后、提交前往库里加素材，断言提交只作用于冻结的 N 项、新素材仍活动、再准备新计划才含它。

**连接生命周期的诚实性**：宿主的端点文件只说明「这一进程曾启用过」，不说明任何连接仍被授权（授权只看 `mcp_connections.revoked_at`）。三个测试分别覆盖：崩溃残留的过期端点如实报连接失败、干净关停后工具表为空且写调用回 `HOST_UNAVAILABLE`、重启后同一客户端恢复服务；任务完成后客户端凭稳定 operation id 读回收据而不重放写入。

### 9.27 需求证据矩阵与真机旅程条目（A6-01，进行中）

A6-01 的要求是「R01–R60 逐条关联真实用例，不能只写『相关组件已存在』」。把这句话做成人的纪律注定失效——下一轮改个测试名，整张表就静默失真。因此落成三件机器可校验的东西。

**`tests/acceptance/requirements.json`（契约 `acceptance-evidence-v1`）**：只维护「需求 → 证据引用」的关系，不写完成勾选、不累计进度（进度仍只归 `docs/PROJECT_PLAN.md`）。每条含 `id`/`requirement`/`version`/`status`/`cards`/`automated.{rust,frontend}`/`manual`/`reason`。`status` 三态：`covered`（有自动化证据）/`gap`（2.0 必需但证据未齐）/`deferred`（属 2.5）。

**`scripts/acceptance-evidence.mjs`**：每次运行**从源码重建索引**——`collectRustTestNames` 扫 `src-tauri/src` 与 `src-tauri/tests` 的 `#[test]`/`#[tokio::test]`；`collectFrontendCases` 扫 `src/**/*.test.tsx?` 的 `it(`/`test(` 字面标题；`collectPlaybookIds` 读 `docs/QA_PLAYBOOK.md` 的条目 ID。刻意**不读 `build-cache/*.tsv`**：那个目录会被 `build:cache:clean` 定期清掉，门禁依赖它等于自带一个静默失效开关。引用必须精确：名字不存在直接失败并给 3 个相近候选，子串匹配到多个也失败，不允许猜。前端证据只写文件路径不算证据（必须 `contains` 或 `minCases`），否则「文件存在」会被当成「行为被覆盖」。命令 `npm run test:acceptance`（结构+引用）与 `--strict`（2.0 不允许留 gap）。

**`docs/QA_PLAYBOOK.md` §14.3 的 M-01~M-14**：把卡要求②③的可执行定义写成真机条目（引导含跳过、拖图预检不自动入库、确认导入与完成摘要、打标审核、本次结果视图与退出、离开页面继续聊、纯手动无 AI 全链路、独立入库页↔助手交接、旧会话恢复、个人流程路线、最小窗口 960×640、手动改宽与收起历史、长中文与多任务轮播、键盘焦点/误点/异常）。矩阵里这些条目一律 `manual.status = "pending"`，真跑过才改 `executed`——自动化通过不替代 L6。

**两处真实覆盖缺口按卡要求补实现**：R13「列表/单图切换」原先只有 `list`/`grid` 两档（`grid` 是缩略图网格，不是单图），补 `"single"` 档并让「切回单图仍停在同一张」成为可断言事实；R09 百宝箱说明页原先无专属测试（只有 `BottomBar` 断言入口转发），补页面测试并断言页内不存在 `iframe`/`script`/`a[href]`，把「2.0 不留扩展点」从注释变成断言。

**校验器自身也有门禁**：`scripts/acceptance-evidence.test.mjs` 用临时工作树验证索引与判定（引用不存在要失败、子串多义要拒绝、前端证据必须有 `contains`/`minCases`、`covered` 必须有自动化证据、非 `covered` 必须有 `reason`、少写 R01–R60 任一条要报错）。本轮据此修掉一个真实漏检：手册里 `| A-01 | 名称 |` 与 `| X-01 安装与启动 |` 两种行格式并存，旧正则只认第一种，`X-01`~`X-06` 平台条目在索引里整体消失，导致矩阵写对了反而被判「不存在」。
