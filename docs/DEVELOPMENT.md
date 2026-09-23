# 开发规范

> 更新日期：2026-09-22
>
> 本文档定义当前仓库的开发、测试、迁移、提交和 AI 协作规范。根级 [AGENTS.md](../AGENTS.md) 是 AI 的最短强制入口。

## 1. 环境

主力环境：

- Windows 10/11。
- Rust stable MSVC。
- Visual Studio C++ Build Tools，包含 MSVC linker 和 Windows SDK。
- Node.js 22（`package.json` 的 `engines` 约束为 `>=22 <25`）。
- PowerShell 7 用于本地 smoke；普通 PowerShell 5.1 也可运行基础命令。

辅助依赖：

- `ffmpeg` / `ffprobe`：开发时可由 PATH 提供；发布包必须使用经过 manifest 校验的 sidecar。
- LLVM/libclang：HEIC 构建链需要。
- Ollama：仅本地模型功能需要，不是应用编译前置。

如果 `npm` 或 `cargo` 不在 PATH，先修正当前终端环境，不要把机器专用绝对路径写入仓库配置。

## 2. 日常命令

```powershell
npm install
npm run desktop:dev

npm run lint
npm run typecheck
npm run test:unit
npm run build
```

Rust：

```powershell
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

完整冒烟：

```powershell
pwsh ./scripts/smoke.ps1
```

真实文件性能探针按需执行：

```powershell
cd src-tauri
cargo test --test perf_probe -- --ignored --nocapture
```

### 2.1 桌面构建入口

三目标（Windows x64、macOS Apple Silicon、Linux x64）统一走 `scripts/desktop.mjs`：

```powershell
npm run desktop:dev            # node scripts/desktop.mjs dev [--target <triple>]
npm run desktop:build          # node scripts/desktop.mjs build [--target <triple>]
npm run desktop:check          # node scripts/desktop.mjs check [--target <triple>]
```

- `.cargo/config.toml` 不再无条件注入 `HEIF_BINARIES_DIR`；由 `desktop.mjs` 按实际 target triple 决定：仅 Windows x64 有本地 `heif-bin/` 覆盖时导出。
- HEIF 依赖来源（现状，非“系统库”）：未设置 `HEIF_BINARIES_DIR` 时，`heif-rs 26.7.0` 的 `build.rs` 从其**固定 release（26.7.0）**下载对应目标的预编译归档，不解析系统 libheif。若要离线/可控构建，先准备本地目录并导出 `HEIF_BINARIES_DIR`（Windows x64 兼容旧布局 `heif-bin/`）。构建日志会打印实际来源。
- ffmpeg / ffprobe sidecar 放在 `src-tauri/binaries/`，命名为 `<name>-<triple>[.exe]`。用 `npm run prepare-media-tools -- --target <triple>` 按 `src-tauri/binaries/manifest.json`（版本化 + SHA256 + 编码器 + 许可证）下载校验放置；macOS 可通过 `ffprobeArchive*` 使用独立的 ffprobe 归档。脚本会在解压前检查归档成员路径，拒绝 Zip Slip 和符号链接，并复核已存在 sidecar 的编码器/许可证；`build` 与 `desktop:check:strict` 会强校验（存在 + 可执行位 + hash 匹配 + 编码器齐全 + 许可证），缺失或哨兵 SHA256 即失败。许可证目录由 `tauri.sidecar.conf.json` 作为 bundle resource 打包，并由候选包 workflow 单独归档。
- 工具链固定：`src-tauri/rust-toolchain.toml`（Rust 1.98.1 + rustfmt/clippy）与 `package.json` 的 `engines.node`（>=22 <25）记录已验证版本；跨目标由 CI 按 runner 安装，本机不强制下载他平台目标。升级 Rust 必须同步修改两个 workflow 并重跑三端门禁。
- 三目标 CI：`.github/workflows/code-gate.yml`（每个 PR 的代码门，含 Tauri native 非 bundle 构建 + 二进制架构断言）与 `release-candidate.yml`（候选包门：sidecar 准备 + pipefail 捕获构建状态 + 硬校验安装包 + per-target `artifact-manifest.json` + 汇总校验目标/版本/commit/校验和）。

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

### 3.3 Tauri IPC

- 命令名和参数顺序属于前端契约，修改时全仓搜索调用点。
- 长任务使用 async + `spawn_blocking` 或后台线程。
- 事件名保持域前缀，例如 `ai://progress`、`export://progress`。
- 取消使用 `AppState` 注册表，命令层不向 service 传 `AppHandle`。
- 错误返回必须能形成用户可执行的信息。

## 4. 代码规范

### 4.1 Rust

- 统一使用 `AppError` / `AppResult`。新增业务校验优先用 `AppError::invalid_arg`、
  `not_found`、`conflict`、`cancelled`、`timeout`、`file_locked`、
  `ai_rate_limited`、`unauthorized`、`unsupported` 或 `internal`，不要继续把可分类错误
  全部塞进 `ERROR`。
- 命令错误序列化契约是 `{ code, message, cause? }`；`cause` 保留底层错误链。前端必须通过
  `src/api/client.ts` 的 `AppError` 读取 `code` 和 `cause`，不要在页面里重新解析字符串。
- 生产代码禁止裸 `unwrap()`；测试和明确不可失败且有注释的初始化除外。
- 使用 `tracing` 记录失败和关键状态，不刷屏；长任务至少记录开始、结束、取消、失败阶段和
  任务 ID。
- 日志字段优先结构化（`operation`、`stage`、`task_id` / `asset_id`、`error_code`、`duration_ms`），
  不要只在字符串里拼接上下文；前端日志的 `sessionId` / `sequence` 和 IPC 失败日志的
  `requestId` / `command` 属于基础设施字段，业务 context 不得覆盖。
- 禁止记录完整 API Key、Authorization、用户提示词、完整请求体或素材内容；前端回传必须经过
  `logger` 的长度限制和脱敏。
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
| 前端单元/组件 | `src/**/*.test.ts(x)` | store、组件状态、交互、错误路径 |
| 冒烟 | `scripts/smoke.ps1`、CI | 关键自动化链路 |
| 真机 UAT | [QA_PLAYBOOK.md](QA_PLAYBOOK.md) | 桌面环境、真实素材、跨模块旅程 |

### 5.2 普通改动

至少执行：

```powershell
npm run lint
npm run typecheck
npm run test:unit

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
