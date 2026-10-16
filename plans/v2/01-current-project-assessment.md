# 01 现行项目评估与改造可行性

状态：已核实的静态代码评估与改造建议。基线、文档权威关系见 [目录说明](README.md)。未运行应用、数据库迁移或集成原型。

分期说明：本章同时评估 Agent 与未来插件；2.0/2.5 的实际开发范围按 [08](08-migration-and-delivery.md)。插件隔离与新媒体衍生关系不是本阶段全部必做项。

## 结论

可以改造，适合渐进扩展。项目已有本地素材业务内核，Agent 和插件需要的搜索、导入、AI 建议、导出、凭据、图片解码均有实现。但现有程序以可信单一前端为主要调用方，尚不是可直接暴露给多类不可信调用者的扩展平台。主要工程量在身份授权、持久任务、产物登记、隔离 UI 和进程管理。

不需要替换语言、重建素材库或让 Node/Pi 直接接管 SQLite。新增进程与插件也不能绕开现有 services/db/imaging 约束。

## 可复用的基础与证据

| 当前事实 | 代码证据 | 对 2.0 的意义 |
|---|---|---|
| React 通过统一 API 调用 Rust；commands/services/db 分层 | [架构](../../docs/ARCHITECTURE.md)、[API 客户端](../../src/api/client.ts) | 新增 Adapter 可复用业务，不需要模拟鼠标或页面 DOM |
| SQLite WAL、外键、自定义中文 tokenizer、FTS 规则 | [数据库入口](../../src-tauri/src/db/mod.rs)、[迁移](../../src-tauri/src/db/migrations.rs) | 数据写入仍由同一个受控宿主管理 |
| SearchPlanV3 统一执行搜索；AI 输出受限意图 | [执行契约](../../docs/contracts/search-plan-v3.md)、[AI 契约](../../docs/contracts/super-search-ai-v2.md) | Agent 搜索可以复用校验/编译，不开放 SQL |
| 导入有扫描、预检查、去重、进度和取消 | [导入命令](../../src-tauri/src/commands/import_cmd.rs)、[导入模块](../../src-tauri/src/services/importer.rs) | 在现有管线外增加计划和操作记录，保持同一素材语义 |
| AI 批次和待确认建议已落库，存在中断处理 | [AI 命令](../../src-tauri/src/commands/ai_cmd.rs)、[AI 数据](../../src-tauri/src/db/ai.rs) | 复用批次，正式标签仍遵守人工确认 |
| 导出任务已有数据库记录 | [导出数据](../../src-tauri/src/db/export.rs) | 可被统一操作记录关联，不需复制任务事实 |
| 凭据存 OS keyring，连接可共享限流 | [凭据](../../src-tauri/src/services/credentials.rs)、[限流](../../src-tauri/src/services/ai_rate_limit.rs) | Agent 请求纳入现有连接和额度体系 |
| 图片解码统一入口及 RAW/HEIF 下游 | [imaging](../../src-tauri/src/services/imaging.rs)、[RAW](../../src-tauri/src/services/raw_decode.rs) | 插件产物仍经同一媒体入口校验/生成预览 |
| 数据库生命周期锁支持维护/恢复 | [AppState](../../src-tauri/src/state.rs)、[恢复模块](../../src-tauri/src/services/backup_restore.rs) | 新任务必须纳入维护互斥，不能额外打开数据库绕过 |
| 原生进程统一处理 Windows 无控制台启动 | [进程封装](../../src-tauri/src/utils/process.rs)、[平台契约](../../docs/PLATFORM.md) | Pi 和媒体连接器扩展现有边界 |

## 必须补齐的差距

### 1. 任务持久化不完整

[taskStore](../../src/stores/taskStore.ts) 当前 TaskKind 为 import/export，状态主要是内存展示；AI 进度由自身页面/存储维护。[AppState](../../src-tauri/src/state.rs) 的导入取消/运行标志、AI/导出取消注册表也是内存结构。导出数据库有任务状态、AI 批次能标记中断，不代表整段 Agent 流程已能恢复。

改造：新增跨业务操作记录，链接现有 task/batch；逐项保存结果和失败原因；前端事件只作提示，状态可重新查询。不能把日志、聊天摘要或 Pi JSONL 当业务任务日志。

### 2. 导入预览尚不是授权绑定的持久计划

导入命令接收 paths/task_id/options，内部预检查后在工作线程再次读取 library_root。对于当前手动调用这是现有行为；扩大到可延迟批准的 Agent/插件计划时，要冻结目标和关键配置并在执行前复核。

现有导入结果也不足以直接承担跨系统逐项收据：需可靠保存每个输入/产物对应的 asset ID，以及重复项映射。不能根据成功计数反推具体素材。详细改法见 [媒体工作流](07-media-workflows.md)。

### 3. Tauri 权限以可信主界面为前提

[capability](../../src-tauri/capabilities/default.json) 按 main window 配置权限；[build.rs](../../src-tauri/build.rs) 当前调用 tauri_build::build，未在该入口配置 AppManifest commands ACL。[素材命令](../../src-tauri/src/commands/assets_cmd.rs) 会放行素材 asset URL，[启动代码](../../src-tauri/src/lib.rs) 放行缩略图、预览和代理目录。

这些机制是当前应用资源访问配置，不能直接当成按插件隔离的资源网关。新子 WebView 必须重新设计命令 ACL、调用者识别、资源授权和事件分发。这里评估的是引入不可信插件后的前置工作，不宣称当前产品已有可被外部插件利用的漏洞。

### 4. 缺少跨入口身份与授权

当前业务命令参数没有贯穿式 principal、grant 或批准计划语义。即使 UI 已弹确认框，未来外部入口也不能自动继承该信任。

改造：传输层产生可信 CallerContext；业务入口统一检查，模型/插件 JSON 不得自报身份。普通调用授权和危险操作确认分别管理。

### 5. AI 管线不等于现成 Agent 模型网关

[ai_cloud](../../src-tauri/src/services/ai_cloud.rs) 主要是既有搜索/视觉处理请求，含 blocking send/json/text 与非流式处理；[连接绑定](../../src-tauri/src/db/ai_connections.rs) 当前主要用途为 super_search/tagging。

Pi 需要多轮工具调用和流式事件；新增 agent 用途、流转发和请求策略。复用凭据/限流，并验证 Pi 的普通请求、压缩/总结和重试都被覆盖，不能只接通第一次聊天就认为完成。

### 6. 选择集合与库身份需显式化

[selectionStore](../../src/stores/selectionStore.ts) 包含 100000 项截断及 sticky 标记。模型不能将当前页误作全部，也不能把截断集合当完整集合。后端需要集合引用、数量、截断标志、所属 libraryEpoch 和有效条件。

### 7. 衍生关系不能复用文件名同源规则

[kinship](../../src-tauri/src/services/kinship.rs) 按目录/stem/RAW 类型判断配对，影响标签同步、AI 去重和浏览。显影图、裁剪图或生成图要保存显式来源关系，并防止误触发原规则。

### 8. 外部进程基础可复用，但尚非扩展宿主

现有进程封装提供后台启动和探针超时，不等于已有长期双向 RPC、消息限额、反压、子进程树清理、版本握手、自动恢复和插件隔离。Pi host、MCP bridge 和原生连接器各自需要生命周期契约。

### 9. 单总仓管理、删除和移动需要统一改造

[设置页](../../src/pages/SettingsPage.tsx) 修改 libraryRoot 当前只自动保存设置；不会搬迁文件、更新旧资产绝对路径或切换数据库。[素材数据](../../src-tauri/src/db/assets.rs) 的列表与基于当前 root 的目录分面也并非同一范围限定。新导入已在 [importer](../../src-tauri/src/services/importer.rs) 要求有效总库，因此设置中「未选择时仅建立索引」的旧提示不能当作现有可用能力。

[删除命令](../../src-tauri/src/commands/assets_cmd.rs) 的默认回收站是 deleted_at 软删，不是系统回收站；默认 30 天后的启动清理会真正删除当前 file_path，再删除记录。删除服务尚未验证路径仍属于总仓或有托管来源证据。[导出服务](../../src-tauri/src/services/export_local.rs) 允许 move 到任意目录并更新 asset 路径，移动成功而 DB 更新失败时尚无完整恢复 journal。它们不能直接当作新 Agent 的安全能力。

用户已经确定的目标行为见 [02](02-target-architecture.md)，现有入口须一起迁入新规则。旧库外资产要独立核验和处置，保留 ID、标签等元数据；不能直接用普通导入去重结果代替旧记录迁移。总仓失联、用户已搬仓和真正库外文件必须区分；先建立保护，再允许到期清理。

### 10. 双工作台和个人流程尚无完整基础

[入库页](../../src/pages/ImportPage.tsx) 的 plan、分库、改名模板等仍在页面局部 state；WebView 拖拽监听由该页面生命周期管理，未按聊天附件区域分派。页面切换和双入口需迁移草稿所有权，不能复制两份入库状态。可复用 PendingList/RenameBuilder 等组件，但不等于整页可直接嵌入即完成任务共享。

现有 AI 配置为服务管理及用途绑定，尚无 Agent 用途；打标经 imaging 压缩视觉图，智能搜索发送文字意图/标签词典，不能声称整个 AI 管线都不外发库信息。窗口关闭没有现成的托盘继续运行和通用导入恢复机制，现有 AI 批次恢复不等于全部业务恢复。

内置及个人常用流程需要新增受控定义、版本、参数编辑和实例化；现有 Pi 技能加载设计不能替代用户保存成功流程的产品闭环。工程接法见 [03](03-agent-and-mcp.md)，交付步骤见 [08](08-migration-and-delivery.md)。以上是新增工作，不是当前已经实现的能力。

## 当前产品约束继续有效

- [PRD](../../docs/PRD.md)：本地优先、AI 标签建议需确认、破坏性操作明确确认、失败不假成功。
- [平台](../../docs/PLATFORM.md)：Windows 为正式支持目标，Apple Silicon/macOS 与 Ubuntu 24.04 x64 为预览支持目标；实际等级依证据，不能由构建成功推导。
- [当前计划](../../docs/PROJECT_PLAN.md)：已有公开版本不代表全部真实素材/跨端 UAT 完成。文内存在历史阶段记录，2.0 不据旧段落推导当前测试全部通过。
- 应用版本 2.0 与 SQLite schema 版本无对应关系；当前迁移包含 user_version 与 feature/幂等补齐机制，必须在其基础上追加迁移。

## 风险排序与可行性判断

| 项目 | 判断 | 前置验证 |
|---|---|---|
| Pi 完整 SDK + GUI | 工程可行，生命周期/模型连接有工作量 | P01 |
| MCP 暴露领域能力 | 工程可行；单宿主、鉴权和协议兼容必须补齐 | P04 |
| HTML 网页插件内嵌 | 符合产品需求；Tauri 隔离和三端布局是发布门槛 | P02 |
| 联网、本地工作区、授权文件保存 | 可通过宿主受控能力提供 | P02/P03 |
| 通用原生 Node/Python 扩展 | 易扩展但具有真实 OS 权限，必须单独信任 | P06 |
| 完整 Lightroom 级实时编辑 | 超出首版；稳定 CLI 显影与自己的结果页更实际 | P05 |

实验定义见 [验证计划](09-validation-and-risk.md)，P03 已分为最小原型 P03a 与正式恢复 P03b。以上评估覆盖最终演进方向，不表示全部进入 2.0；按 [08](08-migration-and-delivery.md) 先做 Agent，插件及新增专业媒体能力留到 2.5。以上不是实现或测试通过报告。
