# 测试策略

> 更新日期：2026-09-23
>
> 本文档定义测试分层、自动化门禁、数据要求和发布标准。详细真机步骤见 [QA_PLAYBOOK.md](QA_PLAYBOOK.md)。

## 1. 目标

测试要守住六条产品承诺：

1. 用户文件不因删除、移动、导出或恢复而静默丢失。
2. 数据库、FTS、标签计数和磁盘状态保持一致。
3. 搜索条件、结果、总数、排序和诊断使用同一语义。
4. 长任务可取消、可恢复，并有真实进度。
5. 数万级素材下列表和搜索可用。
6. 云端 AI 和本地模型失败时能解释、降级或安全停止。

## 2. 测试分层

| 层级 | 范围 | 位置 | 执行方式 |
|---|---|---|---|
| L1 Rust 单元 | 纯函数、解析、状态转换、边界 | `src-tauri/src/**` | `cargo test` |
| L2 Rust 集成 | DB、迁移、搜索、导入导出、格式、AI/Ollama mock | `src-tauri/tests/` | `cargo test --all-features` |
| L3 前端单元/组件 | store、工具函数、组件交互 | `src/**/*.test.ts(x)` | `npm run test:unit` |
| L4 构建与静态门禁 | 类型、lint、格式化、clippy、打包与候选工具元数据契约 | 全仓、`scripts/*.mjs` | `npm run test:tooling` 及第 4 节 |
| L5 冒烟 | 稳定组 + 网络 mock + 前端全链路 | `scripts/smoke.ps1` | 交付前 |
| L6 真机 UAT | 桌面窗口、真实素材、跨模块旅程 | [QA_PLAYBOOK.md](QA_PLAYBOOK.md) | 发布前 |
| L7 性能探针 | 真实文件解码、数据规模、响应时间 | `perf_probe` | 图像/搜索/大数据改动 |

自动化通过不代表产品可发布，L6 是独立发布门禁。

Windows 后台控制台回归使用 `pwsh ./scripts/test-background-process.ps1`：GUI subsystem 测试父进程经真实 helper 启动控制台 fixture，验证未分配控制台，并验证卡住的探针按期限终止/回收。该检查进入本地完整冒烟与 Windows CI；它证明进程构造策略，不能代替安装包中维护页、媒体与 GPU 操作的真机验收。

## 3. 风险覆盖

| 风险域 | 必测内容 | 主要防线 |
|---|---|---|
| 数据库迁移 | 新库、旧库、重复启动、半完成恢复、视频代理旧记录重建 | 迁移单测、V24/V25/V26 幂等测试 |
| 数据一致性 | 素材、关联、FTS、计数、回收站 | DB 集成和跨模块 UAT |
| 文件操作 | 导入取消、重复名、move、删除失败、恢复替换/回滚/重开及旧库保留 | services 集成 + E2E/UAT |
| 图片格式 | JPG/PNG/WebP/TIFF/BMP/TGA、HEIC、RAW | 格式矩阵 + 真机样本 |
| 视频 | 元数据、封面、播放、代理、抽帧；发布构建不得回退 PATH；源文件、源路径、编码器策略或工具变化后不得复用旧代理 | 发布/开发解析器 seam、旧代理重建与指纹服务测试、mock、三端构建和真机视频样本 |
| 普通搜索 | 中文、ASCII 子串、短查询、特殊字符 | 搜索回归和 DB 测试 |
| 超级搜索 | 三区、mustNot、SQL 白名单、诊断、位置权重 | 契约测试 + 真实数据 UAT |
| 分面/标签 | 父子关系、别名、合并、级联、撤销 | Rust + 前端契约测试 |
| AI 打标 | 正常、空解析、401、超时、取消、续跑 | mock 集成 + 真模型 UAT |
| 本地模型 | 未装、已装未启动、就绪、拉取失败 | 纯函数、mock、目标机器 UAT |
| 设置迁移 | 旧配置、单源读取、重启保持 | normalize 单测 + 真机验证 |
| 日志/诊断 | 脱敏、长度限制、日志保留/截断、诊断包内容、关联 ID、panic/fatal、级别切换 | Rust 单测 + 前端组件测试 + UAT |
| UI | 主题、窄窗、键盘、长文本、无重叠 | 组件测试 + 视觉走查 |
| 安全 | 路径穿越、SQL 注入、凭据存储与 DB 锁顺序、删除/更新补偿、keyring 不可用 | 注入式凭据后端单测、代码审查、UAT |

## 4. 自动化门禁

### 4.1 普通改动

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

Windows P0 冒烟由 `.github/workflows/smoke.yml` 执行。三目标代码门的配置位于
`.github/workflows/code-gate.yml`：前端 typecheck/lint/unit/build 一次，Rust fmt、clippy、tests
和本机 Tauri 二进制编译分别在 Windows x64、Apple Silicon macOS、Ubuntu 24.04 x64 执行。
工作流保留 OS、CPU 架构与 Rust target 断言；网络 mock 类目标按脚本设计串行重试一次，其他失败不自动忽略。
本地测试、远端 workflow 成功和 GitHub Required checks 是不同证据：只有实际生效于 `main` 的规则
才构成合并阻断门禁。当前 workflow 所在分支、已执行结果和 `main` 规则状态只记录在
[PROJECT_PLAN.md](PROJECT_PLAN.md)，不在此处复制易过时的运行状态。
参见 [GitHub-hosted runners reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)。

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
| 备份恢复 | 有效/损坏备份、暂存复制失败、rename 失败、已有 `.old` 冲突、新库重开失败、旧库回滚与保留、独占门期间普通 DB 访问阻塞、主库缺失时从校验通过的 `.old` 启动恢复且不创建空库 |
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
| 相似 | 同图不同尺寸/压缩、连拍、裁剪、翻转、同源组；同源自动折叠必须是唯一 RAW/非 RAW 配对，歧义组不得误合并 |
| 搜索 | 只命中 must、只命中 should A/B、同时命中、全不命中、被 mustNot 排除 |
| 规模 | 1000 条用于快速回归；3 万条用于性能和虚拟滚动 |

真实素材不得提交到仓库。探针通过本机路径读取，测试库应在每轮 UAT 前可重置。

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

任何一项未完成，状态只能是“未放行”。

## 8. CI 与候选包

目标配置是在 push/PR 到 `main` 时并行执行 Windows P0 smoke 与三目标 code-gate。工作流进入默认分支后，
Windows、Apple Silicon macOS 和 Ubuntu 24.04 x64 任一受支持 runner 的编译或测试失败都应阻止合并；
只有有期限、恢复条件和责任人的基础设施临时豁免可以例外，不能设置永久 `continue-on-error`。
仓库管理员还须将 `smoke` 与 code-gate 的必要 job 配置为 `main` Required checks，并只读核实规则
实际适用于 `main`。当前工作流是否已进入默认分支、远端结果和 Required checks 生效状态见
[PROJECT_PLAN.md](PROJECT_PLAN.md)。

`.github/workflows/release-candidate.yml` 仅允许手动运行，从同一个 workflow commit 分别打包三个
目标；收集前必须断言当前 `HEAD` 等于 workflow commit，且源码工作树没有已跟踪或未跟踪改动，避免
把未提交代码冒充成该 SHA 的构建。收集脚本验证 package/Cargo/Tauri 版本一致，为包、媒体与 HEIF
许可证材料计算 SHA256，并把每目标的原生安装包构建日志连同其 SHA256 一并收集；聚合 job 核对三端
版本、提交和所有文件摘要。`npm run test:tooling` 还会用合成三端 artifact 目录验证聚合校验器的成功、
提交不一致、构建日志缺失/篡改与文件篡改失败路径。该 workflow 只保存
7 天 artifact，不创建公开 Release，也不
表示安装包已签名、许可已获准或真机验收通过。

CI 不替代 [QA_PLAYBOOK.md](QA_PLAYBOOK.md) 的三端真实安装验收。真实素材和桌面窗口验收仍是独立门禁。

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
