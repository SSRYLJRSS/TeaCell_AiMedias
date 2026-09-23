# 架构说明

> 更新日期：2026-09-16
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
- `settingsStore`：设置加载、保存、主题和错误状态。
- `taskStore`：导入、导出、AI 等全局任务进度。
- `superSearchStore`：SearchPlanV3、AI 合并、持久化和诊断。
- `metadataStore`：元数据筛选项和派生值。
- `numericDomainStore`：数值分面范围和类型信息。
- `platformStore`：平台能力 DTO（`schemaVersion` / `os` / `arch` / `managedOllama` / `preferredVideoProxy`），由 `App.tsx` 首次挂载调用一次 `loadPlatform()` 载入。

跨页核心状态不得放在页面局部 state 中，也不得复制到第二个 store。

### 2.3 API 层

`src/api/` 按域拆分：

- `client.ts`：统一 invoke 和错误封装。
- `assets.ts` / `thumbnail.ts` / `preview.ts`
- `import.ts` / `export.ts` / `video.ts`
- `ai.ts` / `connections.ts` / `ollama.ts`
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
- V23 色板关系表和 V24 数值分面采用无条件幂等修复，不推进 `user_version`，存量库每次启动可自愈补齐。
- 已发布迁移禁止修改，只能追加新迁移或幂等修复。
- 新列只追加到表尾，读取使用列名映射，禁止依赖物理位置。

### 4.2 关键数据原则

- 设置表只有一个业务 key：`app_settings`，内容为完整 JSON。
- `tag_facets` 是分面唯一事实源，旧 JSON 配置只作为历史迁移输入。
- `tag_terms` 与 `tag_aliases` 的唯一约束由 `schema_features` 控制，禁止双写。
- 回收站通过 `assets.deleted_at` 软删实现。
- 标签操作流水用于批次撤销；手工覆盖应清理批次来源，避免误撤销。
- 外部工具直接改库前必须了解 FTS 触发器和自定义 `cjk_bigram` 依赖。普通 SQLite 客户端只适合 SELECT。

### 4.3 备份与恢复

- 备份使用 SQLite `VACUUM INTO` 生成单文件快照。
- 恢复前必须校验快照，并保留旧库为 `library.db.old`。
- 运行中的导入、导出或打标任务存在时拒绝恢复。
- 恢复后重新执行迁移和启动自检。

## 5. 关键机制

### 5.1 图像与缩略图

所有通用图片解码经 `services/imaging.rs`：

1. 读取尺寸或内嵌预览。
2. 尝试 TIFF/CR3 等容器预览。
3. 必要时进入 `heic_decode` 或 `raw_decode`。
4. 使用全局许可限制并发。

占位层优先快速预览，禁止对 RAW 做昂贵全解码。高清层按需生成并缓存。dev 和 release 的图像依赖必须保持合理优化配置，新增图像依赖要核对 `Cargo.toml`。

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

- 标签词典在每次 AI 搜索请求时从活动标签和可搜索别名实时读取，并附带父类路径；人工确认后的新词无需重启即可被后续搜索识别。
- 精确规范名/别名直接解析为标签；无法可靠映射但置信度达到阈值时降级为内容搜索，低置信度概念给出未采用 warning。

### 5.4 标签与分面

- 标签父子关系使用递归逻辑和安全检查，禁止环。
- 分面 key 是机器协议，显示名只用于 UI 和提示词。
- `input_mode` 决定分面是否进入 AI 提示词。
- 新库、重置标签和空标签恢复库会播种 `subject` / `scene` / `people` 的默认层级；父节点用于浏览和宽泛筛选，AI 输出叶子。
- 核心分面的 AI 新词经人工确认后落到该分面「其他」父类；普通和自建分面保持原有创建语义。
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
- 视觉输出使用 V2 结构：`description`、`peoplePresence`、`tags`、`numbers`；不再解析旧字符串标签和旧顶层分面协议。
- 人物在 `subject` 中统一写「人」；人数档位、性别、年龄段、穿着和动作只进入 `people`，混合人群按图片级原子属性多值输出。
- `subject` / `scene` / `people` 默认上限分别为 3 / 3 / 8；场景只记录空间、环境和地点，不接收树木、水面、楼梯等主体物。
- `confidenceMinSuggest` 保持 0.30；核心分面新词不硬拒绝，但确认时统一归入「其他」，便于后续治理。
- 请求层优先使用结构化输出：本地 Ollama 原生 JSON Schema、OpenAI 兼容 `json_schema`、Anthropic tool use；不可用时按批次缓存并降级为 `json_object` 或纯文本。
- 无效结构、必需分面缺失、描述过短或人物判断矛盾时最多修复一次；网络、鉴权和额度错误不触发修复。
- 主体为空且描述非空时同样最多修复一次；修复后仍为空则保留空值并显示为「未识别」，不生成占位标签。
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

### 5.8 平台适配

- **平台能力初始化**：前端在 `App.tsx` 首次挂载时调用一次 `loadPlatform()`，写入 `platformStore`。`load` 带竞态代际（`loadGeneration`），并发重试或组件卸载后的较旧响应不会覆盖较新状态。载入失败可见且可恢复：设置页在 `status==="error"` 时显示非阻塞提示与「重试」按钮（`load()` 幂等，`error` 允许再次发起），主页面继续可用不白屏；平台敏感操作（如视频代理 ensure）在 `status==="ready"` 前不发起。DTO 只含 `schemaVersion` / `os` / `arch` / `managedOllama` / `preferredVideoProxy`，不承载业务数据。
- **视频代理变体时序**：`ensure/status/cancel` 的 variant 省略时由后端 `platform::preferred_video_proxy()` 决定（Linux `vp8_webm`，Windows/macOS `h264_mp4`），API 层不硬编码。前端组件在平台 ready 前不自动 ensure；variant 变化时复位「已尝试」并用 generation token 丢弃旧变体的迟到响应，避免 H.264 响应覆盖 WebM 状态。
- **视频代理缓存指纹**：`video_proxies` 记录源内容指纹（内容 hash 优先，回退 size+mtime）、编码策略版本常量、ffmpeg 工具指纹（V25 幂等迁移新增列）。ready 命中必须三项全一致才复用；任一变化或无指纹的旧记录一律作废旧派生文件并重新生成。
- **平台相关 keyring**：凭据读写按 Windows / macOS / Linux target feature 分派系统凭据后端；所有 keyring 调用都在数据库 mutex 之外，不与 SQLite 锁交叉。
- **媒体 sidecar**：`services/media_tools.rs` 是 ffmpeg / ffprobe 的唯一解析入口。发布包优先使用 Tauri sidecar，仅开发模式回退 PATH 查找；ffmpeg 与 ffprobe 分开缓存探测结果。版本探测（`-version`）用 spawn + 3s 硬超时 + kill/wait 回收 + 输出上限，损坏/被挂起的工具不会永久卡住调用线程；缓存键含工具文件指纹（size+mtime），换源/升级自动失效。
- **非 Windows Ollama 边界**：仅 Windows 提供托管 Ollama 的安装、进程和模型管理；macOS / Linux 只引导用户自行部署，并在 AI 连接表单填写外部 OpenAI 兼容地址。
- **同源分类统一收口**：`services/kinship.rs` 的 `classify_kinship_group` / `paired_sibling` 是同源判定唯一入口。只有恰好 1 个 RAW + 1 个非 RAW 才自动配对，其余情况归为 `Ambiguous`，不向外扩散配对关系。

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
- `heif-rs`、libheif、libde265、x265 等静态链接组件。
- 其他图像、视频或模型依赖的再分发条款。

内部自用风险与对外商业分发不同。发布安装包前必须核对许可证，并保留动态链接、替换库或更换依赖的备选方案。
