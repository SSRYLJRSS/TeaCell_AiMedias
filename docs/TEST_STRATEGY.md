# 测试策略

> 更新日期：2026-09-23
>
> 本文档定义测试分层、自动化门禁、数据要求和发布标准。详细真机步骤见 [QA_PLAYBOOK.md](QA_PLAYBOOK.md)。

## 1. 目标

测试要守住七条产品承诺：

1. 用户文件不因删除、移动、导出或恢复而静默丢失。
2. 数据库、FTS、标签计数和磁盘状态保持一致。
3. 搜索条件、结果、总数、排序和诊断使用同一语义。
4. 长任务可取消、可恢复，并有真实进度。
5. 数万级素材下列表和搜索可用。
6. 云端 AI 和本地模型失败时能解释、降级或安全停止。
7. Windows、macOS 和 Linux 从同一代码主线构建，平台差异不会破坏核心五步闭环。

## 2. 测试分层

| 层级 | 范围 | 位置 | 执行方式 |
|---|---|---|---|
| L1 Rust 单元 | 纯函数、解析、状态转换、边界 | `src-tauri/src/**` | `cargo test` |
| L2 Rust 集成 | DB、迁移、搜索、导入导出、格式、AI/Ollama mock | `src-tauri/tests/` | `cargo test --all-features` |
| L3 前端单元/组件 | store、工具函数、组件交互 | `src/**/*.test.ts(x)` | `npm run test:unit` |
| L4 构建与静态门禁 | 类型、lint、格式化、clippy、打包 | 全仓 | 见第 4 节 |
| L5 冒烟 | 稳定组 + 网络 mock + 前端全链路 | `scripts/smoke.ps1` | 交付前 |
| L6 真机 UAT | 桌面窗口、真实素材、跨模块旅程 | [QA_PLAYBOOK.md](QA_PLAYBOOK.md) | 发布前 |
| L7 性能探针 | 真实文件解码、数据规模、响应时间 | `perf_probe` | 图像/搜索/大数据改动 |

自动化通过不代表产品可发布，L6 是独立发布门禁。

## 3. 风险覆盖

| 风险域 | 必测内容 | 主要防线 |
|---|---|---|
| 数据库迁移 | 新库、旧库、重复启动、半完成恢复 | 迁移单测、V24/幂等测试 |
| 数据一致性 | 素材、关联、FTS、计数、回收站 | DB 集成和跨模块 UAT |
| 文件操作 | 导入取消、重复名、move、删除失败、恢复 | services 集成 + E2E/UAT |
| 图片格式 | JPG/PNG/WebP/TIFF/BMP/TGA、HEIC、RAW | 格式矩阵 + 真机样本 |
| 视频 | 元数据、封面、播放、代理、抽帧 | mock、真机视频样本 |
| 普通搜索 | 中文、ASCII 子串、短查询、特殊字符 | 搜索回归和 DB 测试 |
| 超级搜索 | 三区、mustNot、SQL 白名单、诊断、位置权重 | 契约测试 + 真实数据 UAT |
| 分面/标签 | 父子关系、别名、合并、级联、撤销 | Rust + 前端契约测试 |
| AI 打标 | 正常、空解析、401、超时、取消、续跑 | mock 集成 + 真模型 UAT |
| 本地模型 | 未装、已装未启动、就绪、拉取失败 | 纯函数、mock、目标机器 UAT |
| 设置迁移 | 旧配置、单源读取、重启保持 | normalize 单测 + 真机验证 |
| 日志/诊断 | 脱敏、长度限制、日志保留/截断、诊断包内容、关联 ID、panic/fatal、级别切换 | Rust 单测 + 前端组件测试 + UAT |
| UI | 主题、窄窗、键盘、长文本、无重叠 | 组件测试 + 视觉走查 |
| 安全 | 路径穿越、SQL 注入、凭据存储 | 单元、代码审查、UAT |
| 平台 | 路径/编码、目标依赖、能力门控、安装启动、核心五步闭环 | 三端 CI + 目标真机 UAT |

## 4. 自动化门禁

### 4.1 普通改动

```powershell
npm run lint
npm run typecheck
npm run test:unit

cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

判定：

- 命令退出码必须为 0。
- ESLint 允许存在已登记警告，但不得新增错误。
- 不得通过删除测试、skip、only 或放宽契约消除失败。
- 环境前置导致的 ignored 测试必须列出原因。

### 4.2 交付门禁

```powershell
npm run build
pwsh ./scripts/smoke.ps1
```

CI 对应 `.github/workflows/smoke.yml`。网络 mock 类目标允许按脚本设计串行重试一次，其他失败不自动忽略。

### 4.3 跨平台门禁目标

跨平台门禁完成接入后，每个合并候选至少包含：

| 平台 | 合并前必需证据 |
|---|---|
| Windows 10/11 x64 | 完整前端门禁、Rust fmt/clippy/test、现有 smoke |
| Apple Silicon macOS | 前端门禁、Rust fmt/clippy/test、目标平台编译和平台专属测试 |
| Ubuntu 24.04 x64 | 安装系统依赖后执行前端门禁、Rust fmt/clippy/test、平台专属测试 |

候选发布另需三个平台实际打包。日常 PR 可以不生成安装包，但不能跳过目标平台编译与测试。
具体支持范围见 [PLATFORM.md](PLATFORM.md)。CI 配置尚未达到本节时，应把缺项列为发布阻塞，
不能把目标规则写成已经通过的结果。

## 5. 定向测试要求

| 改动 | 必须增加或运行 |
|---|---|
| 迁移 | 新库、旧库、重复执行、中断恢复 |
| 搜索协议 | Rust 契约测试 + TypeScript 计划/store 测试 |
| 分面协议 | 三输入模式、级联、撤销、FTS 一致性 |
| imaging/RAW/HEIC | format matrix + perf probe + 真实样本 |
| AI 请求/解析 | 正常、空、无效 JSON、401、超时、取消 |
| 长任务 | 成功、取消、部分失败、重启恢复 |
| 文件删除/导出 | 文件名冲突、权限失败、部分成功、数据库同步 |
| 备份恢复 | 有效备份、损坏备份、运行中任务拒绝、旧库保底 |
| 日志/诊断 | 前端异常回传、JSON/Bearer/Basic 脱敏、诊断包不含 API Key/提示词/素材、30 个日志文件保留且总量不超过 50 MB、诊断包总量 20 MB/单文件 4 MB 截断、`requestId`/`sessionId` 关联 |
| 错误协议 | `AppError` 业务 code、`cause` source 链、前端 `{ code, message, cause }` 还原 |
| UI 交互 | 组件测试 + 键盘 + 1366/1920 + light/dark |

## 6. 测试数据

### 6.1 最小样本集

| 包 | 内容 |
|---|---|
| 基础 | JPG、PNG、WebP、TIFF、中文/英文/数字/emoji 文件名 |
| 相机 | RAW、HEIC、不同相机 EXIF、有无内嵌预览 |
| 视频 | H.264、HEVC、损坏视频、短视频、长视频 |
| 异常 | 0 字节、截断图、伪扩展名、只读、超长路径 |
| 相似 | 同图不同尺寸/压缩、连拍、裁剪、翻转、同源组 |
| 搜索 | 只命中 must、只命中 should A/B、同时命中、全不命中、被 mustNot 排除 |
| 规模 | 1000 条用于快速回归；3 万条用于性能和虚拟滚动 |

小型、无隐私且许可明确的合成样本可以提交到仓库。大型 RAW/HEIC/视频与真实素材包放在仓库外，
记录版本、文件清单和校验值；探针通过本机路径读取。测试库应在每轮 UAT 前可重置。

### 6.2 搜索 UAT 证据

必须保存：

- 素材 ID、关键字段、标签和预期顺序。
- 请求或 localStorage 中的 plan JSON。
- relevance 模式的实际结果 ID 顺序。
- mustNot 排除结果。
- should 全不命中仍保留的证据。
- hydrate 后 `minimumShouldMatch=0` 和位置权重 `2/1/0.5`。

## 7. 发布标准

### 7.1 准入

- 代码冻结在可审查的状态。
- 自动化门禁全绿。
- 测试库和素材包已准备。
- 当前版本迁移、备份和恢复路径明确。

### 7.2 准出

- `scripts/smoke.ps1` 通过。
- Rust/前端门禁全部通过。
- P0 缺陷为 0。
- P1 缺陷为零或有明确绕行方案和签字批准。
- 黄金用户旅程和真实素材搜索 UAT 完成。
- 性能清单通过或明确记录目标机器限制。
- 安装包、数据目录、日志、备份和回滚步骤已复核。
- 随包媒体工具的许可证及对应源代码材料、构建信息和再分发方式已完成审查；许可证文本单独不足以关闭此项。
- 三个平台候选包来自同一提交；Windows 完整 UAT 通过，macOS/Linux 当前版本的核心五步闭环
  已在目标真机通过。

任何一项未完成，状态只能是“未放行”。

## 8. CI

### 8.1 当前状态

仓库已接入 `.github/workflows/code-gate.yml`：前端门禁运行一次，Rust fmt/clippy/test/native build
按 Windows x64、Apple Silicon macOS、Ubuntu 24.04 x64 矩阵运行；`.github/workflows/release-candidate.yml`
按同一提交生成三端候选包并核对目标工件。首次远端运行和仓库设置中的 required checks 仍是发布阻塞，
在它们完成前不能把本地 Windows 结果表述为“三端通过”。

### 8.2 目标状态

- push/PR 到 `main`：运行三端代码门禁；仓库保护必须要求以下四个检查全部通过，任一失败即阻止合并：
  - `frontend`
  - `rust (x86_64-pc-windows-msvc)`
  - `rust (aarch64-apple-darwin)`
  - `rust (x86_64-unknown-linux-gnu)`
- 手动或候选标签：从同一提交生成 Windows、macOS Apple Silicon、Ubuntu x64 安装包并上传为
  可追溯 artifact。
- 默认不使用 `continue-on-error`。确认属于 runner 或上游基础设施故障时，豁免必须限时并记录
  原因、责任人和恢复条件。
- CI 不替代真实素材和桌面窗口验收。

## 9. 缺陷管理

缺陷至少记录：

- ID、发现日期和执行人。
- 关联需求或契约。
- 严重度：P0、P1、P2、P3。
- 最小复现步骤。
- 预期、实际和复现概率。
- 环境和素材说明。
- 截图、日志、plan JSON 或测试输出。
- 修复、回归和关闭状态。

严重度定义：

| 级别 | 定义 |
|---|---|
| P0 | 数据丢失、崩溃、安全漏洞、核心链路完全不可用 |
| P1 | 主流程受阻，有绕行；搜索结果明显错误；备份恢复不可靠 |
| P2 | 次要功能、体验或性能问题 |
| P3 | 文案和轻微视觉问题 |

## 10. 回归纪律

修复后必须执行：

1. 缺陷对应的最小用例。
2. 同模块相邻用例。
3. 受影响的数据流和 UI 状态。
4. 普通门禁；涉及数据或协议时执行完整交付门禁。

P0 修复后必须重跑黄金用户旅程。回归证据缺失时不得关闭缺陷。
