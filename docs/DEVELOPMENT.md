# 开发规范

> 更新日期：2026-10-11
>
> 本文档定义当前仓库的开发、测试、迁移、提交和 AI 协作规范。根级 [AGENTS.md](../AGENTS.md) 是 AI 的最短强制入口。

## 1. 环境

主力环境：

- Windows 10/11。
- Rust 1.98.1（由仓库根目录 `rust-toolchain.toml` 固定）；Windows 使用 MSVC。
- Visual Studio C++ Build Tools，包含 MSVC linker 和 Windows SDK。
- Node.js 22、23 或 24（`package.json` 限定 `>=22 <25`）。
- PowerShell 7 用于本地 smoke；普通 PowerShell 5.1 也可运行基础命令。

辅助依赖：

- `ffmpeg` / `ffprobe`：视频元数据和抽帧，缺失时按降级逻辑运行。
- LLVM/libclang：HEIC 构建链需要；HEIF 原生归档与许可证从固定 manifest 下载并做 SHA256 校验。
- Ollama：仅本地模型功能需要，不是应用编译前置。

如果 `npm` 或 `cargo` 不在 PATH，先修正当前终端环境，不要把机器专用绝对路径写入仓库配置。

## 2. 日常命令

```powershell
npm install
npm run desktop:dev

npm run lint
npm run typecheck
npm run test:unit
npm run test:tooling
npm run test:agent-host # 修改 packages/agent-host、agent_runtime 或 Host 打包时
npm run build
npm run desktop:check
npm run desktop:dev
```

Rust：

```powershell
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

运行 Rust 编译、clippy、测试前，在目标系统本机准备对应的 HEIF 静态库并设置环境变量（Apple Silicon/macOS 和 Ubuntu/Linux 使用各自 triple）：

```powershell
npm run prepare-heif-libraries -- --target x86_64-pc-windows-msvc
$env:HEIF_BINARIES_DIR = (Resolve-Path "src-tauri/native/heif/x86_64-pc-windows-msvc").Path
```

`desktop:dev` / `desktop:build` 会自动准备 HEIF 库；裸 Cargo 命令不进行下载，未设置已校验目录时会立即失败。

桌面入口在准备资源前运行 `scripts/product-identity.mjs`，核对 npm/Cargo 包名、Rust library crate 名、产品名、主窗口名和仓库地址；回归由 `test:tooling` 覆盖。品牌以 [项目计划的品牌迁移说明](PROJECT_PLAN.md#20-v102-品牌迁移与发布状态) 为准。应用数据标识、系统凭据 service 和 MSI 升级标识是持久兼容身份，不能按显示名称批量替换；Rust library crate 是源码/build 身份，应与当前产品名一致。临时运行须使用当前 Cargo 主程序的确切路径并检查 Windows 产品元数据；不能从 `target` 中通配选取旧 EXE，旧生成物可移至隔离临时目录保留摘要，不混入新包或验收。

完整冒烟：

```powershell
pwsh ./scripts/smoke.ps1
```

目标平台构建与媒体 sidecar（每次显式指定交付目标）：

```powershell
npm run prepare-media-tools -- --target x86_64-pc-windows-msvc
npm run prepare-heif-libraries -- --target x86_64-pc-windows-msvc
npm run prepare-agent-host -- --target x86_64-pc-windows-msvc
npm run desktop:check:strict -- --target x86_64-pc-windows-msvc
npm run desktop:build -- --target x86_64-pc-windows-msvc --bundles nsis
```

Apple Silicon macOS 和 Ubuntu 24.04 x64 的 triple 与支持边界见 [PLATFORM.md](PLATFORM.md)。sidecar
仅由固定来源和 SHA256 manifest 准备；`desktop:build` 会拒绝缺失或不匹配的 sidecar。平台本机
构建前置依赖见 [Tauri 官方先决条件](https://v2.tauri.app/start/prerequisites/)。

Windows x64 的 Agent Host 使用固定 Node 24.19.0、锁定的 Pi 包和[内部协议 v1](contracts/agent-host-protocol-v1.md)。
`prepare-agent-host` 在首次准备时从固定的 `nodejs.org` HTTPS 地址下载 Node 官方校验和与归档，先核验 manifest 固定的 SHA256，再解包并校验 Node 可执行文件、许可证；它按 Host 锁文件安装 Pi 依赖并核对资源预算，最后生成忽略目录下的 Tauri 配置 overlay。已存在的缓存会重新验摘要，坏缓存不会被覆盖。`desktop:dev`、`desktop:check` 和 `desktop:build` 会按目标调用准备步骤；Windows CI 也会先准备资源并运行 Host 生命周期测试。缺少网络时，已验证的本机缓存可继续构建，首次准备需要访问 Node 官方站点和 npm registry。开发 Node 与随包 Node 是两套用途不同的制品，Host 子进程不继承系统 PATH。
Windows x64 overlay 还校验 Visual Studio Build Tools 中签名有效的 VC143 Redistributable，并把运行库 DLL 映射到应用安装目录；来源、版本和摘要规则见[平台契约](PLATFORM.md)及 `LICENSES/Microsoft-VC-Runtime.txt`。
Host 改动需跑 `npm run test:agent-host`；该套件固定串行运行，且不得与任何 Cargo build/test/clippy（包括全量和定向命令）或 NSIS 打包重负载任务并发。全量 Cargo 与定向 Cargo 测试也必须串行；重叠运行曾使 Host 子进程在 READY 时限内无协议帧输出，造成测试超时。改变资源清单或 Tauri 资源映射时，先构建实际 NSIS 包，再运行 `npm run test:agent-host-staged`：成功的 `desktop:build` 会记录 Tauri 实际使用的 overlay，probe 按该记录读取资源，将 Node、Host 和 Windows VC Runtime 复制到隔离目录，以空 `PATH` 验证握手、会话 pin、发送、关闭和退出；缺少当前成功构建记录时失败关闭。按[性能预算](PERFORMANCE.md#9-20-agent-windows工程预算已独立复核)记录解包资源量和相对同配置基线的安装包增量。
Host 命令等待不占运行时状态锁，关停可打断挂起请求并在 1 秒内回收进程。Rust 必须在状态变更和事件转发前验证 Host 帧的精确字段、类型、ID 与固定错误类别；拒绝只记录白名单错误码，并映射为不含 Host 原始文本的固定应用错误。Rust 在创建 Host 数据根前拒绝链接/reparse point；规范化后的目录必须仍是应用数据目录的直接子目录，并以不共享删除的 no-follow 目录句柄固定数据根和 `sessions` 目录至进程回收。Node 在一次目录快照中只读扫描名称以目标 `conversationId` 结尾的 `.jsonl` 候选会话；每个候选路径都先通过 `session.pin_request` 由 Rust no-follow 打开、确认普通单链接文件并固定身份，收到允许结果后才能读取会话头或交给 SDK。其他会话文件不得带着目标 ID 发 pin 请求。新会话也须在创建头部后先固定，再交给 SDK；Host 关闭会话时按同一 `conversationId` 释放 pin。不得把 SDK 的全目录 `findById` 用于未固定文件。ACK 必须与请求的 command、conversationId 和 runId 一致，身份不符按结果未知处理并保留 runId。发送 ACK 未确认必须返回原 runId 与 `outcomeUnknown`，禁止自动重放；调用方只能精确取消该 run，A3-01 没有 Host 状态查询或 transcript 恢复接口。显式恢复锁须可识别意外退出的所有者，相关逻辑须覆盖 ACK 未知、拒绝可恢复、恢复锁中断后重试、迟到事件和挂起请求关停回归。

真实文件性能探针按需执行：

```powershell
cd src-tauri
cargo test --test perf_probe -- --ignored --nocapture
```

## 3. 分层规范

### 3.1 前端

```text
pages -> stores -> api -> types
components -> stores/api/hooks
utils/hooks -> 无页面依赖
```

- 页面负责编排，不直接 `invoke`。
- 页面业务状态放在 store 或专用 hook，不在多个组件重复保存。
- API 封装集中处理命令名、参数映射和错误。
- 可恢复的入库草稿只持久化固定计划、资源和操作引用及幂等键，不把本地源路径或未经净化的错误文本复制进草稿；需要显示路径时按当前 GUI 身份重新解析自有计划，失效来源须显示错误并阻止提交。
- 入库提交前先保存幂等键，响应不确定后以相同计划和键重试；重启后若计划预览/来源失效，仍提供不依赖源路径的同键受理核对入口。仅确认请求本身已明确拒绝且未返回 operation 时可释放键，受理后的关联/持久化错误仍须用原键对账。终态结果要从持久逐项状态和最新收据恢复新增、重复映射、失败计数与失败明细，不以易失进度事件作为历史结果。交接只关联原计划或操作，不触发重新准备、提交或重新入库；准备取消绑定准确 `taskId`，运行取消和恢复绑定准确 `operationRef`。
- WebView 原生拖拽监听由应用级单例分派器持有，页面只注册/清理活动回调，应用根卸载时解除原生监听。
- Rust serde 字段变化必须同步 `src/types/`。
- 通用逻辑先放 `utils/`，不能靠复制保持一致性。
- 生产代码禁止裸 `console.log/warn/error`；统一使用 `src/utils/logger.ts`。该模块为了
  避免 `client.invoke` 的递归日志，可以原始调用 `@tauri-apps/api/core`，但只能用于日志回传。

### 3.2 Rust

```text
commands -> services -> db
                   \-> filesystem/network/image/video
```

- `commands/` 只做参数校验、状态编排、事件和异步边界。
- 复杂业务放到 `services/`，纯函数优先，便于单测。
- SQL 和事务放到 `db/`。
- 网络、解码和大文件 IO 必须离开数据库锁。
- 服务层进度使用回调或返回值，`AppHandle.emit` 放在 command。
- 模型外发遵循对应冻结契约中的审批边界和发送门；A2-04 打标规则只在 [实现契约 §4.4](../plans/v2/12-implementation-contracts.md#44-ai-打标计划与外发) 定义，其他能力必须建立自己的契约。

### 3.3 Tauri IPC

- 命令名和参数顺序属于前端契约，修改时全仓搜索调用点。
- 长任务使用 async + `spawn_blocking` 或后台线程。
- 事件名保持域前缀，例如 `ai://progress`、`export://progress`。
- 取消使用 `AppState` 注册表，命令层不向 service 传 `AppHandle`。
- 错误返回必须能形成用户可执行的信息。
- 用户可见的错误与恢复指引只能推荐当前版本、当前状态下已实现且安全可执行的动作；缺少可用入口或副作用无法核实时，明确要求保留哪些证据并说明当前安全下一步。不得建议用户手工移动/删除受控文件、复用失效收据，或按未实现的计划流程重试。

## 4. 代码规范

### 4.1 Rust

- 统一使用 `AppError` / `AppResult`。新增业务校验优先用 `AppError::invalid_arg`、
  `not_found`、`conflict`、`cancelled`、`timeout`、`file_locked`、
  `ai_rate_limited`、`unauthorized`、`unsupported` 或 `internal`，不要继续把可分类错误
  全部塞进 `ERROR`。
- 命令错误序列化契约是 `{ code, message, cause? }`；`cause` 保留底层错误链。前端必须通过
  `src/api/client.ts` 的 `AppError` 读取 `code` 和 `cause`，不要在页面里重新解析字符串。
- Agent/MCP外部边界必须使用 `AppError::Agent` 或先调用 `into_agent_error`，返回固定分类提示并去掉底层cause；不得外发旧IO/JSON错误中的路径、请求或凭据。领域服务须在转换前明确分类未知副作用，不能将未对账的效果猜成可重试失败。协议与有界编解码见[Agent契约](contracts/agent-v1.md)。
- 生产代码禁止裸 `unwrap()`；测试和明确不可失败且有注释的初始化除外。
- 使用 `tracing` 记录失败和关键状态，不刷屏；长任务至少记录开始、结束、取消、失败阶段和
  任务 ID。
- 日志字段优先结构化（`operation`、`stage`、`task_id` / `asset_id`、`error_code`、`duration_ms`），
  不要只在字符串里拼接上下文；前端日志的 `sessionId` / `sequence` 和 IPC 失败日志的
  `requestId` / `command` 属于基础设施字段，业务 context 不得覆盖。
- 禁止记录完整 API Key、Authorization、用户提示词、完整请求体或素材内容；前端回传必须经过
  `logger` 的长度限制和脱敏。
- 不通过聊天、Issue、日志、命令行参数或仓库文件索取/传递真实 API 凭据。真实服务测试只能读取用户在本机为该测试单独创建的系统凭据项；开始前明确 provider/model/endpoint、请求上限、最长时间、费用上限和代理范围，不读取或修改日常凭据及代理配置。预算和配置说明不等于真实外发授权。凭据若意外出现在聊天、日志或工作区，停止使用、不复述/复制，并提醒用户在服务端撤销或轮换；不得把暴露凭据转存为测试凭据。
- 公共接口使用清晰参数结构体，超长参数列表优先收敛为类型。
- 禁止用宽泛 `#[allow]` 掩盖问题；允许项必须带原因。
- 已发布迁移不回改；迁移必须幂等、可重入、可恢复。
- SQL 只允许参数绑定，不拼接用户输入。

### 4.2 TypeScript / React

- TypeScript 保持 strict，不使用无理由的 `any`。
- 组件卸载时清理监听、计时器和拖拽状态。
- 异步 store 动作统一维护 `loading` / `error`，页面负责显示。
- 需要 `preventDefault` 的滚轮交互使用原生非 passive 监听。
- 大列表必须虚拟化，避免把全量对象写入 React state。
- 不把 API 请求细节散落在组件中。
- 用户可见图标按钮必须有 `aria-label` 或可访问名称。
- 全局异常和关键状态使用 `logger`；不得用裸 `console` 作为生产排障通道。

### 4.3 CSS / UI

- UI 规范见 [UI_DESIGN_SYSTEM.md](UI_DESIGN_SYSTEM.md)。
- 颜色、圆角、阴影只使用主题变量。
- 不创建无语义的灰色、圆角或选中态。
- 页面区块使用分隔和留白组织，不把所有内容包成卡片。
- 选中、错误、禁用不能只依赖颜色。
- 1366px 和 1920px 必须检查重叠、截断和长文本换行。

### 4.4 命名与文件

- Rust：`snake_case` 文件和函数，`PascalCase` 类型。
- TypeScript：组件 `PascalCase`，函数和变量 `camelCase`。
- 一个文件保持单一职责。超过合理规模时先确认职责边界，再拆分。
- 不批量转换行尾；避免制造全文件 diff。

## 5. 测试与质量门禁

### 5.1 测试层级

| 层 | 位置 | 目标 |
|---|---|---|
| Rust 单元 | `src-tauri/src/**` 内测试模块 | 纯函数、解析、状态转换、边界 |
| Rust 集成 | `src-tauri/tests/` | DB、迁移、服务、格式、AI/Ollama mock |
| Agent Host 集成 | `packages/agent-host/test/`、`src-tauri/src/services/agent_runtime/` | 随包 Node、Pi 离线会话、协议、租约、Rust 生命周期和取消/退出 |
| 前端单元/组件 | `src/**/*.test.ts(x)` | store、组件状态、交互、错误路径 |
| 冒烟 | `scripts/smoke.ps1`、CI | 关键自动化链路 |
| 真机 UAT | [QA_PLAYBOOK.md](QA_PLAYBOOK.md) | 桌面环境、真实素材、跨模块旅程 |

### 5.2 普通改动

至少执行：

```powershell
npm run lint
npm run typecheck
npm run test:unit
npm run test:tooling

cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

### 5.3 交付改动

额外执行：

```powershell
npm run build
pwsh ./scripts/smoke.ps1
```

涉及以下领域时增加定向验证：

- 图像/RAW/HEIC：格式矩阵和性能探针。
- 搜索：查询契约测试、前端 store/QueryBuilder 测试、真实数据集。
- 分面/标签：迁移、级联、撤销和 FTS 一致性测试。
- AI：真实接口或 mock 的正常、超时、401、空响应和取消路径。
- 备份恢复：真实库、运行中任务和旧版本迁移。
- 日志/可观测性：诊断包内容与脱敏、前端异常回传、panic/fatal 兜底、日志级别热切换。
- UI：1366px、1920px、明暗主题和键盘操作。

### 5.4 禁止行为

- 不得删除失败测试来制造绿色。
- 不得用 skip/only 让单项测试绕过。
- 不得把未运行写成通过。
- 不得用格式化修改掩盖行为 diff。
- 不得把 flaky 失败直接归因于环境而不记录证据。

`fmt`、`clippy`、测试失败应分类处理。只有确认是外部环境前置条件时，才能列为未执行，并写明复现命令和原因。

## 6. 数据库迁移

新增 schema 变化时：

1. 确认改动是最小必要范围。
2. 追加新版本迁移，或实现明确的无版本幂等修复。
3. 考虑全新库、旧库、半完成迁移和重复启动四种场景。
4. 补迁移测试。
5. 检查 FTS 触发器、索引、外键和 schema capability。
6. 同步 [ARCHITECTURE.md](ARCHITECTURE.md) 和相关契约。

硬约束：

- 已发布迁移不改。
- 不依赖列顺序。
- 不在迁移中执行不可恢复的大规模下载或网络任务。
- 旧配置迁移只读旧字段，不建立双写。

## 7. 依赖与性能

- 新依赖必须说明用途、体积、许可和替代方案。
- 图像/编码依赖必须检查 dev/profile 优化配置。
- 网络依赖考虑超时、重试、取消和代理。
- 不为了局部方便引入第二套状态、缓存、解码或 HTTP 管线。
- `zip` 只用于诊断包，不得把日志系统扩散成第二套任务或导出框架。
- 数据库查询变更要检查索引、参数绑定和分页稳定性。
- 新增图像相关 crate 后更新性能手册和探针。

## 8. Git 与提交

- 默认分支为 `main`。
- 新功能分支使用 `codex/` 前缀，除非用户另有要求。
- 不提交 `dist/`、`node_modules/`、`src-tauri/target*/`、日志、缓存和临时测试产物。
- 提交前检查 `git diff --check`。
- 功能、测试和对应文档在同一逻辑提交中。
- 纯格式化与行为变化分开提交。
- 未经用户明确要求，不 commit、不 push。
- 工作区已有改动时不得 reset、checkout、clean 或覆盖用户修改。

提交标题使用简短、具体的表述，说明本次改变了什么；沿用 `fix:`、`feat:`、`docs:` 等前缀即可。一个标题对应一个主要目的，例如 `fix: 取消描述输入字数限制`、`docs: 整理项目首页和图标目录`。避免堆叠阶段编号、功能清单，以及“全链路”“全面优化”“收尾”等无法说明实际改动的词。已发布的提交不因措辞调整而重写历史。

## 9. 文档规范

### 9.1 更新位置

| 变化 | 更新 |
|---|---|
| 产品行为 | [PRD.md](PRD.md) |
| 阶段状态 | [PROJECT_PLAN.md](PROJECT_PLAN.md) |
| 架构和机制 | [ARCHITECTURE.md](ARCHITECTURE.md) |
| 机器协议 | [contracts](contracts/) |
| UI 规范和令牌 | [UI_DESIGN_SYSTEM.md](UI_DESIGN_SYSTEM.md) |
| 测试策略 | [TEST_STRATEGY.md](TEST_STRATEGY.md) |
| 真机验收步骤 | [QA_PLAYBOOK.md](QA_PLAYBOOK.md) |
| 运维和恢复 | [OPERATIONS.md](OPERATIONS.md) |
| 已知故障 | [TROUBLESHOOTING.md](TROUBLESHOOTING.md) |

### 9.2 禁止的文档模式

- 新建“第二轮 PRD”“最终开发指导书”“最新修复计划”等平行权威文档。
- 把同一功能说明复制到多个文档。
- 在长期文档中保留已完成任务的逐日流水。
- 让历史报告与当前规范同时出现，逼迫 AI 猜测优先级。
- 把 `.qoder/`、`.workbuddy/`、`.claude/` 等本地工具缓存当作项目事实源。

任务过程记录写入 commit、PR 或 Issue。只有长期有效的决策、协议和操作方式进入 `docs/`。

## 10. AI 协作流程

1. 读取 `AGENTS.md`、文档中心和任务相关权威文档。
2. 查看 `git status` 和最近 diff，识别用户已有工作。
3. 先定位真实实现和测试，不根据旧报告推断。
4. 写清行为契约、失败路径和验证命令。
5. 小步修改，避免无关重构。
6. 运行定向测试，再运行完整门禁。
7. 检查旧文档路径、测试数量、状态描述和协议是否漂移。
8. 汇报修改、验证、未执行项和残余风险。

阶段或连续开发先从 [PROJECT_PLAN](PROJECT_PLAN.md) 核实当前状态与前置验收证据；`plans/v2/execution/` 只定义目标、依赖和验收，不是进度事实或独立授权。2.0 任务卡的分层证据、依赖补验、连续开发和中断恢复遵循 [任务验收与交接](../plans/v2/14-review-and-handoff.md#1-一张卡通过的条件)。连续授权在每张卡验收后继续推进所有前置已通过的未完成卡；接续开发先补最早未通过的依赖，已有下游改动保留并在前置修订后回归，不因代码存在而解锁下一张卡。

## 11. 完成定义

一个改动只有同时满足以下条件才算完成：

- 行为符合 PRD 和当前契约。
- 代码遵守分层和命名规范。
- 正常、边界和失败路径有相应测试。
- 相关自动化门禁通过。
- 真机依赖项已执行，或明确记录未执行原因。
- 权威文档已同步。
- diff 不包含无关修改。
- 没有通过删除测试、放宽断言或静默异常换取“通过”。
