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
| `components/ai/` | Filmstrip、Workbench、进度和分面标签输入 |
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
| `platform.rs` | 编译目标静态能力 DTO；不做运行时健康探测 |
| `export_local.rs` | 复制/移动、目录布局、CSV 和任务状态 |
| `dedup.rs` / `perceptual.rs` | 文件哈希去重、dHash、相似图和同源关系 |
| `palette.rs` | 主色提取、色板状态和回填 |
| `kinship.rs` | 同源/连拍关系判定 |
| `media_refill.rs` | 存量媒体元数据、GPS、尺寸和色板回填 |
| `ai_cloud.rs` | OpenAI 兼容/Anthropic 请求、解析、批次执行、取消和重试 |
| `super_search_ai.rs` | SearchIntentV2、Schema、降级、守卫和解析 |
| `ollama_setup.rs` / `ollama_installer.rs` / `ollama_runtime.rs` | 检测、推荐、拉取、安装和服务生命周期 |
| `credentials.rs` | 系统凭据读写 |
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
| `ai.rs` / `ai_connections.rs` | AI 批次、建议和连接绑定 |
| `settings.rs` | 设置 JSON、normalize 和兼容迁移 |
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
- 回收站通过 `assets.deleted_at` 软删实现。
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
- 打标工作台不区分 AI/手动模式；批次创建后可立即手工填写，也可启动 AI 生成建议。
- 批次创建和执行都以 `ai_usage_bindings.tagging` 当前绑定的连接为准；允许建批后切换服务，开始/续跑时按新连接执行，不回退旧 `settings.ai` 激活档案。

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
- move 导出：文件移动成功后同步素材路径。
- 所有批处理都要区分成功、失败、重复和取消。
- 失败必须保留可恢复信息，不允许 UI 假成功。

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

项目使用 MIT 许可证，但以下依赖需要对外分发前复核：

- `rawler` 及其 LGPL/GPL 许可条件。
- `heif-rs`、libheif、libde265、x265 等静态链接组件。三目标原生归档及源码/许可证 URL、SHA256 见 [`src-tauri/native/heif-manifest.json`](../src-tauri/native/heif-manifest.json)；x265 使用 GPL-2.0-or-later，任何对外交付前必须由负责人独立审查许可义务和再分发条件。
- 其他图像、视频或模型依赖的再分发条款。

内部自用风险与对外商业分发不同。发布安装包前必须核对许可证，并保留动态链接、替换库或更换依赖的备选方案。
