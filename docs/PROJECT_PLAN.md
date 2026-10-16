# 项目计划

> 更新日期：2026-10-14
>
> 本文档记录当前阶段、交付状态和下一阶段范围。产品行为见 [PRD.md](PRD.md)，技术结构见 [ARCHITECTURE.md](ARCHITECTURE.md)。

用户现已明确授权按照 [开发指导书](../plans/v2/11-execution-guide.md) 完成2.0代码开发与验收。按任务卡逐项推进，保留工作区已有改动，提交、推送和发布仍需另外明确授权。实际进度见 [第9节](#9-20-agent开发状态)，既有发布/平台/数据安全门禁继续有效。

## 1. 项目目标

构建一个能在数万级本地素材上长期使用、维护成本可控的个人素材管理系统：

- 入库可托管、可追踪、可取消。
- 浏览和搜索在大数据量下仍稳定、可解释。
- 标签和 AI 打标形成可治理、可撤销的闭环。
- 删除、移动、导出和备份不产生静默数据风险。
- 代码结构和文档上下文足够稳定，便于持续迭代和 AI 协作。

## 2. 当前交付状态

### 2.0 v1.0.2 品牌迁移与发布状态

本轮 v1.0.2 更新按项目负责人明确授权，以 GPL-3.0-or-later 公开发布公测包并覆盖同版本附件；原 MIT 授权继续有效。修复包括入库设置提示布局、Windows 后台控制台闪现、安装器英文产品身份和许可证显示。媒体依赖改为固定源码自建，对应源码包包含应用、Cargo/npm 运行时依赖、前端上游源码和十个原生媒体组件的实际源码及构建脚本，具体流程见 [OPERATIONS.md](OPERATIONS.md#52-三端候选构建)。Windows 新 HEIF 静态库已通过严格 Rust 检查，12 个真实 8/10 位 HEIC 样本经应用 imaging 入口验证为 12 个出图、0 个黑图。逐平台构建记录、源码与安装包摘要以 [Release 附件](https://github.com/SSRYLJRSS/TeaCell_AiMedias/releases/tag/v1.0.2) 为准。

本次公开公测授权不代表 §2.2 的人工 UAT 已通过：macOS/Linux 尚无真机验收，macOS 安装包没有签名/公证；完整用户旅程、隔离账户安装升级及性能验收仍须按原门禁完成。不得将此次公开上传记作这些验收完成。

GitHub 仓库已更名为 [`SSRYLJRSS/TeaCell_AiMedias`](https://github.com/SSRYLJRSS/TeaCell_AiMedias)。v1.0.2 将产品英文全名统一为 `TeaCell AI Media Manager`，中文全名为“茶馆AI素材管理”；桌面短名称继续为“茶馆”。设置页项目、反馈和许可证链接，以及安装器元数据均指向新仓库。Windows 安装界面提供英文和简体中文；各平台包描述和 Release 附件标签采用中英双语。

桌面应用标识符、应用数据目录和系统凭据 service 名继续保留旧值，以便原安装能够读取既有数据库和凭据；这些属于迁移兼容标识，不是面向用户的产品名称。Rust library crate 是源码/build 名称，已随品牌更名为 `teacell_ai_media_manager_lib`，避免新编译日志和测试目标显示旧品牌。v1.0.2 已于 2026-09-30 从提交 `d7f1dc328684457a07be257eb0d3ad8ded5e46b0` 发布：[Release 页面](https://github.com/SSRYLJRSS/TeaCell_AiMedias/releases/tag/v1.0.2)，包含 Windows x64（NSIS 和中英双语 MSI）、macOS Apple Silicon DMG、Linux x64（AppImage 和 DEB）、构建/许可证证据及 SHA-256 清单。旧 v1.0.1 Release 已删除，`v1.0.1` 源码标签保留作历史定位。三端候选 manifest、依赖许可证材料、文件摘要和提交号经 `scripts/verify-candidates.mjs` 通过；同提交 [code-gate](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36662296905) 和 [smoke](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36662296889) 通过。候选工作流 [run 36662320517](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36662320517) 的 Windows 原生打包成功，但 runner 在构建后因工作区状态检查未能收集 artifact；Windows 候选由干净 checkout 的本机同提交构建和收集，与 macOS/Linux runner 候选合并后完成上述三端校验。真实素材 UAT、macOS/Linux 真机验收和媒体依赖再分发许可复核仍未完成，不因发布动作或自动校验通过而视为完成。

### 2.1 已完成的功能基线

下表的“已完成”仅指已有实现，不代表可发布。三端代码治理已完成，集成基线 `f2f4a1f` 已快进合入本地 `main`；代码验证与发布验收分别记录于 §2.3 和 §2.2。当前收尾不新增功能，后续开发边界见第 4 节。

| 领域 | 状态 |
|---|---|
| 核心桌面壳、数据库迁移、日志、主题、错误隔离 | 已完成 |
| 两段式入库、总库/分库托管、改名、任务反馈 | 已完成 |
| 虚拟网格、批量选择、排序筛选、分面标签、查看器 | 已完成 |
| 双层缩略图、全局 imaging、多格式与 RAW/HEIC 解码 | 已完成，真实样本仍需持续验证 |
| 普通中文搜索、超级搜索 SearchPlanV3、AI 搜索 | 已完成 |
| 标签分面 V2、治理、别名、合并、撤销、数值分面 | 已完成 |
| 云端 AI、手动打标、Ollama 本地模型和视频抽帧 | 已完成 |
| 回收站、双策略删除、哈希去重、感知去重、同源组 | 已完成 |
| 本地导出、CSV、数据库备份恢复、运行日志 | 已完成 |

### 2.2 当前发布阻塞

以下项目完成前，不能把当前版本标记为“可放行”：

1. **真实素材 UAT**：超级搜索 relevance 排序、mustNot 排除、should 全不命中仍保留、位置权重和 hydrate 后 plan JSON 需要真实数据证据。
2. **核心用户旅程**：使用真实素材走通导入、浏览、AI 打标、搜索、查看器、评级、导出、删除恢复和备份恢复。
3. **格式与性能**：在目标机器上复测 RAW/HEIC、视频播放、3 万级搜索和网格滚动。
4. **工作区交付边界**：所有应发布改动必须完成审查、测试并形成可回退提交。
5. **发布门禁**：`scripts/smoke.ps1`、严格 Rust 门禁、前端门禁和人工 UAT 必须全部有结果记录。
6. **三端远端门禁**：GitHub `main` 已启用 strict Required checks，要求 `frontend`、三个目标平台 Rust 检查及 `smoke`，并对管理员生效。远端 `main` SHA `d10507925a834a87e2c6816e395940b66b81ee0d` 的最近 `code-gate` 与 `smoke` 通过；当前工作区改动未提交到该 SHA，仍需在干净提交上完成远端门禁。
7. **macOS/Linux 真机验收**：当前没有这两类目标设备的验收证据；自动构建成功也不能标记为支持。
8. **媒体依赖分发材料**：项目负责人已明确授权按 GPL-3.0-or-later 发布；原 MIT 授权保留。HEIF/FFmpeg 已改为固定源码自建，Rust、前端和原生媒体对应源码及构建材料已收集，三端运行时摘要固定在 manifest。Windows HEIF 使用本机 MSVC 构建并完成真实样本验证；macOS 单独重建以排除 runner 自动发现的 libsharpyuv；FFmpeg 三端通过编码、探测和解码验证。安装包与完整对应源码、逐平台构建证据及 SHA256 必须配套发布，不能仅更新许可证文字后分发。流程见 [OPERATIONS.md](OPERATIONS.md#52-三端候选构建)。

当前项目计划不把“自动化全绿”等同于“产品可发布”。真实素材验收仍是独立门禁。

### 2.3 三端统一开发收口状态

| 范围 | 当前状态 | 完成证据/剩余动作 |
|---|---|---|
| 平台契约、AI 协作入口、路径与能力边界 | 集成基线 `f2f4a1f` 已快进合入本地 `main` | 产品代码和工作流与已通过三端 code-gate 及 Windows smoke 的 `b5c25b2` 一致；GitHub `main` 已启用 strict Required checks，当前远端最新成功门禁位于 SHA `d10507925a834a87e2c6816e395940b66b81ee0d`；当前工作区改动尚无远端门禁证据 |
| Windows/macOS/Linux 平台能力、UTF-8 路径告警、视频代理变体 | 工作分支已有实现与回归测试 | 最新提交在三端 clippy、全量 Rust 测试及原生 Tauri 构建通过；目标设备人工验收仍未完成 |
| Rust 1.98.1、统一桌面构建入口、SHA256 sidecar/HEIF manifest | 工作分支已配置 | 三端 HEIF 资源准备及原生应用构建通过；当前提交 `1c33ba899b3db186835667907bd9bc7ee5ada7ec` 的 Windows strict media check 通过，并在本机用 `npm run desktop:build -- --target x86_64-pc-windows-msvc --bundles msi,nsis --ci --no-sign` 成功生成 Windows MSI（110,690,304 bytes，SHA256 `69C464DC2057FBAB72EB7630E229BFFBA0F4DD31B810447E4683417D8A70D9DB`）和 NSIS（81,144,151 bytes，SHA256 `F434373FDB742D0EAB9995EFA1FFF09D42D67BF71890D0ADF2630BF0256DDB72`）；构建日志 SHA256 `6BDB2E3EC65FC782216300990FA8F14262CCBDFEBE2C74687C2A441996768C96`。构建仅用于本地链路验证；未签名、未安装、未启动、未上传、未分发，不是发布候选包。构建结束后原有 `dist/` 已恢复，临时生成内容保留在临时目录且未进入版本控制。三端同提交候选和真实产物 manifest 仍未生成 |
| 三目标 code-gate 与手动候选包 workflow | 代码/工作流验证基线 `b5c25b2` 的 code-gate 与同 SHA Windows smoke 均已通过；GitHub `main` Required checks 已配置 | 近期候选 workflow [run 36662320517](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36662320517) 的 macOS/Linux 打包成功，Windows 打包成功但收集步骤因 runner 工作树不干净而拒绝登记 artifact；当前改动仍需在同一干净提交上重跑三端候选 workflow。`smoke.yml` 的 push/PR 自动触发限于 `main` |
| 三端版本/提交/安装包摘要一致性校验 | manifest 包含 runner OS/架构、逐包大小/SHA256、必需包类型、HEIF 来源/许可证材料及逐目标构建日志 SHA256；本地合成三端产物的聚合校验测试通过，真实三端候选尚未生成 | 需在同一提交的三端 runner 生成并验证真实产物 |
| 固定五步人工验收 | 操作脚本已纳入 QA 手册 | 仍需 Windows、Apple Silicon、Ubuntu 目标设备逐一执行 |
| 候选包交付给熟人测试 | 暂不允许 | 完成媒体许可复核，且目标平台核心五步通过后再发知情测试者 |

代码/工作流验证基线 `b5c25b2fc786cee3ba6a8e417f8423f9f686114c` 的 [code-gate run 36105870606](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36105870606) 四个 jobs 及同 SHA 的 Windows [smoke run 36105890701](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36105890701) 全部通过。后续 `f2f4a1f` 和本次收尾只改项目计划，没有修改产品代码、依赖、测试或工作流，不手动重复派发全套测试；push 自动触发的运行独立保留结果，不以旧 SHA 的通过冒充新 SHA 已验证。Windows MSI/NSIS 仍来自 `1c33ba8`，不是当前三端候选包；尚无三端候选 manifest，也未安装、启动或分发。

合并前直接查询 GitHub 确认远端与本地 `main` 均为 `683628ae25feb610fdd84aeb37b036e18187a5fe`，集成基线 `f2f4a1f79923beb51ebb65a87cc011b53bc0bbca` 领先 21 个提交、不落后，可无冲突快进。本次已按用户授权快进本地 `main`，随后提交本文修正并正常推送主线；实际远端完成状态以 `git ls-remote origin refs/heads/main` 为准。旧记录中的 `c989912` 是未刷新的本地远端跟踪引用，不是当时实时远端主线，所谓领先 78/79 个提交的结论无效。

### 2.4 当前审计结论与证据边界

- 范围审计以 `main` `683628a`、Windows 快照 `4c2e140` 和平台快照 `b59188a` 为基线。收尾已核实远端主线与该审计基线一致，没有另一个遗漏的 58 提交差异范围。逐路径核对结论：差异属于既有 Windows 行为、平台兼容、缺陷修复、回归测试、文档与门禁；未发现无来源的新增产品入口、AI 能力、搜索语法或数据含义。Windows 快照中已有的 SearchIntent V3、AI 图像输入规范化、V25、AI 连接限流和帮助入口不得误判为本轮新增或擅自删除。此结论不等于逐行无缺陷或真机验收通过。
- 源码清理提交 `36a5d352af85490f8dc37ed02e3b321be573c208` 的本机门禁和三端 code-gate 已通过：前端 709 passed/2 skipped、lint 无 warning；Rust all-features 788 passed/8 ignored；完整 smoke 和 Windows strict media check 通过。远端证据：[code-gate run 35986278018](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/35986278018)。本批未新增产品功能。
- workflow run `35969019646` attempt 1 的 `SettingsPage.test.tsx` 失败发生在“初始未修改时不显示未保存状态”断言；日志实际显示该提示元素存在。它紧随本文件内的色条设置测试，而该测试曾留下 800ms 防抖 timer。`36a5d35` 将该前置测试改为假时钟、在本测试中完成防抖保存/回读，并在 `afterEach` 恢复真实时钟；这与失败路径构成具体的跨测试 timer 泄漏解释。修复版该测试文件顺序运行 3 次，均为 37 passed/2 skipped；后续完整前端及三端 run `35992128310` 通过。结论：timer 泄漏是有代码路径支持的根因判断并已做针对性隔离，但历史 attempt 本身未能在修复前稳定重现；若未来复发仍需重新诊断。
- 本次进一步发现同一测试文件的另一个异步测试隔离问题：AI 自动打标测试切页后未等待 `UsageBindingLine` 的绑定加载，单项运行出现 3 条 React `act` 警告。测试现等待已有的“此功能使用的服务”行，不更改生产组件；单项通过、整份文件连续 3 次均 37 passed/2 skipped 且无该警告。`typecheck`、`lint`、`test:tooling`（15/15）、完整前端单测（709 passed/2 skipped）和 `npm run build` 均通过；build 仍有既有约 664 kB JS chunk 体积提示，未做无关拆包。对应源码提交 `fd319ebcfc551b791664fa69eccfc1512faf7be3` 的 [三端 code-gate run 36000987683](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36000987683) 已全部通过。
- 工作分支 SHA `7474d48e7af2c4d48b922538d8956acd5496d9e7` 的首次手动 Windows smoke [run 36094987343 attempt 1](https://github.com/SSRYLJRSS/TeaCell_AiMedias/actions/runs/36094987343) 在 `ViewerPage.test.tsx` 的“fallback 沉浸按 Esc 返回查看器”用例失败：Escape 后未在 waitFor 时限内恢复“返回素材库”按钮。相同 SHA 的 attempt 2 通过；本机单文件 14/14 和完整前端单测 709 passed/2 skipped 也通过，故原失败未能稳定复现。检查到键盘监听器在 passive `useEffect` 中随 `immersiveMode` 重绑，沉浸 portal 出现与新监听器生效之间存在事件空档。`0aa1039` 将监听器改为 `useLayoutEffect`，让处理器在提交后的绘制前更新；修复后本机该文件 14/14、全量前端单测 709/2、lint、typecheck 通过，且该 SHA 的 Windows smoke `36096765334` 与四 job code-gate `36096755139` 均全绿。结论：原始失败为一次真实但未复现的异步时序失败；修复针对代码中可确认的监听器空档，不把 attempt 2 当成覆盖 attempt 1 的证据。
- 本次采用快进方式将集成基线纳入 `main`，保留全部原始提交，不 squash、不强推、不删除恢复分支，也不覆盖快照工作区中的用户改动。代码/工作流验证基线仍为 `b5c25b2`。该次合入当时未创建 PR、未改保护规则、未发布；后续发布准备已为 `main` 配置 Required checks。分支与门禁当前状态见 §2.3；候选包、三端真机 UAT 与许可复核等未关闭项见 §2.2。自动化通过和主线合并都不等于可发布或平台支持等级已升级。

### 2.5 安装测试问题修复状态（2026-09-28）

用户反馈对应的 D1–D8 代码实现、自动化回归和契约文档已在独立工作区完成。复核 D2 时发现原用户句回归夹具把绿色建模成普通标签，未覆盖真实 `dominant_hue` 元数据路径；现已为 AI 协议增加 `preferredMetadata` 软条件，映射到既有 `SearchPlanV3.should`，并对旧模型误把“最好绿色”输出成硬色相筛选的响应做窄范围纠正，同时保护明确要求的颜色条件。软元数据筛选条件在界面使用自然语言 evidence 展示，避免向用户暴露内部字段与区间。协议、Rust/TypeScript 类型、Prompt、QA 和合成库回归已同步。复核用户产品名时发现当前工作树仍配置旧名；现已统一面向用户的应用标题、启动/教程/关于文案及 README 为“茶馆”，保留稳定的 Tauri identifier 与现有数据目录。前端 typecheck/lint/unit（730 passed、2 skipped）/tooling（15 passed）/build、Rust fmt/clippy/all-features tests、完整 smoke、Windows strict media check 均通过；本轮名称与展示修正后的 typecheck/lint、定向组件测试（9 passed）和生产构建也通过。最新 Windows x64 本地 MSI（110,731,264 bytes，SHA256 `A618ACB9CDB4F0951E3BAD8F71838C2EC3938186A2B5D86330C5DBDFFC0C1391`）和 NSIS（81,150,804 bytes，SHA256 `BE28A886C560FC6D92205441829D56A7A5500D9634B928BBEEFB9598588D6580`）已从当前工作树重建，产物名均为“茶馆”；两者均未签名，属于本地验证包，不是可分发候选包。未在当前用户配置安装，未提交或发布。

D9 的 Windows 全新配置、保留数据升级、卸载重装和独立配置验收尚未执行：当前没有可用的隔离 Windows 用户账户或虚拟机，且应用使用系统已知用户数据目录，不能通过改环境变量隔离。系统浏览器/剪贴板、多尺寸/主题/键盘的桌面检查和真实素材旅程也尚未验收。没有安装或触碰当前用户数据；目标平台真机验收、真实素材验收和媒体依赖许可复核仍按 §2.2 阻止发布。该项是安装/产品环境验收限制，不代表 D1–D8 自动化实现未完成。

### 2.6 新一轮安装测试优化状态（2026-09-29）

本轮优化已在集成工作区完成源码修改和自动化验证，等待用户使用开发版走查；未生成安装包、未安装或发布。AI 打标进度已区分本次执行数量与批次累计，系统级 AI 协议固定、分类业务说明可编辑；导入确认流程复用后端预检候选清单，减少服务层重复展开路径；入库提示同宽；关于页、项目/许可证链接与 GitHub Issue 反馈入口已更新。

后续复核补齐了关于页全名“茶馆AI素材管理”、选择器返回后先让左侧既有进度区绘制“正在检查文件与缩略图”，并在超级搜索服务绑定解析后再检查有效 AI 配置。当前本轮继续收紧服务选择：未绑定时只自动选择可用在线服务，绝不读取旧版默认档案或自动切换本机模型；超级搜索设置页显示后端实际解析结果，用户仍可显式绑定本机服务。关于页增加作者主页入口并复用外链失败后的可复制地址提示。分类管理明确为五个公开内置 AI 分类可编辑说明/显式规则、可停用恢复但不可删除；画面摘要与内部兼容 key 不开放管理；用户自建分类可编辑、停用、恢复和删除；旧 `purpose`/`technical` 数据按有无用户数据或自定义配置迁移保留。针对已实测到的 OpenAI 兼容 `finish_reason=length` 与 Anthropic `stop_reason=max_tokens`，请求层返回 `AI_OUTPUT_TRUNCATED`，显示可执行的重试建议并保留现有搜索条件/结果，不把不完整 JSON 降级成整句关键词。真实服务表现、真实素材结果与 200 张混合样本的桌面时序仍待用户走查。

最新本机验证：`npm run typecheck`、`npm run lint`、`npm run test:unit`（736 passed、2 skipped）、`npm run test:tooling`（15/15）、`cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`（523 个库测试与全部集成测试通过）及完整 `pwsh ./scripts/smoke.ps1` 均通过。smoke 的前端生产构建成功，主 JS chunk 为 678.50 kB（gzip 200.70 kB），仍提示超过 500 kB 的体积告警。Rust 链接器输出一条工具链 warning，但所有门禁退出码均为 0。设置页服务解析、作者链接和分类生命周期已通过自动化验证；用户桌面验收需先退出旧实例，再从最新集成工作树启动开发版，并用真实在线连接验证超级搜索。没有生成安装包、安装、发布、提交或推送。导入性能对照、诊断页黑色控制台窗口复现、真实服务/素材表现与桌面外链仍属于待验收项；本轮不等同于发布验收。

## 3. 里程碑

### M1 基础闭环

状态：已完成。

范围：

- 入库、素材库、查看器、标签、搜索、AI 打标、导出、删除、设置。
- 数据库迁移、日志、错误处理和基础自动化测试。

### M2 检索与标签成熟

状态：已完成，等待发布级 UAT。

范围：

- FTS5 中文搜索强化。
- 超级搜索条件树、AI 意图解析、诊断和筛选持久化。
- 分面 V2、标签治理、数值分面和打标撤销。

### M3 媒体与工作流增强

状态：已完成，等待真实媒体验证。

范围：

- RAW/HEIC/TIFF 等格式链路。
- 视频播放代理、悬浮预览、视频 AI 抽帧。
- 感知去重、同源文件组、颜色属性、评级和查看器。

### M4 本地 AI 与数据安全

状态：已完成，等待目标机器验证。

范围：

- Ollama 检测、安装、启动、模型推荐、拉取和配置。
- 数据库备份恢复、运行日志、缓存和本地模型占用管理。

### M5 1.0 发布收口

状态：进行中。

出口条件：

- 真实素材 UAT 完成。
- P0/P1 缺陷清零。
- 性能、格式、视频和备份恢复复测通过。
- 文档、版本号、安装包和恢复流程一致。

### M6 三端统一候选

状态：实施中，尚未完成平台验收。

范围：

- 只以 `main` 为长期主线，用 Rust 平台能力、target 依赖和跨端测试管理差异。
- Windows 10/11 x64 为首发完整功能目标；Apple Silicon macOS 与 Ubuntu 24.04 x64 为知情测试预览目标，须先在目标设备完成核心五步闭环。
- 同一候选版本的各平台包必须来自同一 Git 提交；安装包可分日生成，但不得混用版本和提交。
- App 管理的 Ollama 只在 Windows；macOS/Linux 保留外部兼容 AI 连接，不提供托管安装/启动入口。
- 第一阶段不承诺跨系统搬迁素材库。

该里程碑的 workflow 文件和本地实现不等于发布验收完成；GitHub `main` Required checks 已配置，媒体依赖再分发复核、三端候选包和真实真机 UAT 仍未完成。

## 4. 治理收尾与后续开发边界

### 4.1 本阶段授权和边界

本轮代码范围：**不新增功能，只优化、规范已有代码，修复适配与可靠性问题。** 用户已授权将验证过的集成改动合入主线，并另行明确要求替换上传 v1.0.2 Release。GitHub `main` Required checks 已按发布门禁启用；公开分发仍受 §2.2 的许可、三端候选和真机验收条件约束。文档中的未来方向不是开发授权。

产品基线是 `main` 已有行为加 Windows 恢复快照中既有行为，不是把所有平台分支或旧计划内容无条件搬入。平台差异按 [PLATFORM.md](PLATFORM.md) 收敛；相对 Windows 的行为改变必须能对应既定能力限制或可复现缺陷。

允许：修复现有行为、补回归测试、必要的局部解耦、移出锁内阻塞工作、统一已有平台边界、修正文档与构建门禁。默认保持现有页面、设置项、搜索语义和数据含义。优化需有实际问题和前后证据。

本阶段不开发新入口、新 AI 能力、新搜索语法、网盘、音频、语义搜索、同步、跨系统搬库或批量治理工具；也不进行全仓重构、无关依赖升级或设计系统重做。大组件拆分只在阻碍本次缺陷修复且能证明行为不变时局部实施。自动化先保留固定五步人工脚本；桌面 E2E 框架建设不作为本次收口前置。

### 4.2 每批共同约束

1. 先读取 AGENTS 与相关权威文档，并核对工作树、分支、HEAD、暂存/未暂存/未跟踪项。只在集成工作树施工，两个恢复快照仅作证据。
2. 开工列出一个具体问题、预期行为、文件白名单、三端影响、回归测试和不改项。新增文件或迁移须说明其如何服务现有行为，而非扩展产品。
3. 一次只处理一批；如果发现另一问题，记录后排队，不顺手修复。需要改变产品含义时暂停并请求决策。
4. 保留用户所有既有改动和暂存区；不用 reset/checkout/clean。本次提交、推送与主线合并按 §4.1 的明确授权执行；发布门禁范围内的 `main` Required checks 已设置，安装或公开分发须先关闭 §2.2 阻塞。恢复快照仅保留，不删除、不覆盖。
5. 回改已发布迁移、减少测试断言、跳过失败测试、全局串行化来掩盖失败、放宽三端门禁均不属于可接受修复。
6. 每批交付：实际 diff、基线来源、验证命令及退出码、未执行项、剩余风险。失败结果不得被之后一次成功覆盖。

### 4.3 收口状态

执行过程和历史诊断不在项目计划中重复保存；稳定测试要求见 [TEST_STRATEGY.md](TEST_STRATEGY.md)，操作规范见 [DEVELOPMENT.md](DEVELOPMENT.md)。本阶段当前状态如下：

| 批次 | 状态 | 证据 / 尚未关闭事项 |
|---|---|---|
| P0 范围与来源复核 | 已完成 | 未发现无来源的新功能；结论见 §2.4。 |
| P1 429 批处理状态 | 已修复（故障路径已确认；旧实例未复现） | 基线 `683628a` 虽已将 HTTP 429 映射为 `AI_RATE_LIMITED`，但批处理仍把它和普通请求错误共用退避重试及 rejected 路径；`2e62273` 在首次/重试响应处新增该错误码的提前中断处理，记录错误并保留 pending。`http_429_stops_batch_and_preserves_pending` 覆盖不重试、当前与后续建议均保留 pending；Windows 精确用例 10/10 通过，源码 SHA `fd319eb` 的三端 code-gate 全绿。历史原始运行的随机触发条件未复现，不声称可稳定复现。 |
| P2 平台与数据安全 | 代码复核完成 | 既有平台差异、安全路径和回归修正已纳入当前集成；目标设备人工验收仍按 §2.2 未关闭。 |
| P3 规范和文档收口 | 已完成 | 本地 lint 无 warning；权威文档只记录当前行为、证据和开放门禁。 |
| P4 同一源码验证 | `b5c25b2` 的四个 code-gate jobs 与同 SHA Windows smoke 全绿；本机 MSI/NSIS 构建证据来自各自记录的工作区 | checkout/setup-node 的 Node 24 迁移已验证；候选 workflow 仍需在当前干净提交上通过。当前 Windows Release 包仅是本机未签名验证包，尚未安装验收，也不满足当前 SHA 的三端候选要求。完整证据和未完成发布项见 §2.3。 |
| P5 主线合并与候选验收 | 集成基线 `f2f4a1f` 已快进合入本地 `main`；发布验收仍未完成 | `main` Required checks 已配置；三端候选包 manifest、三端真机 UAT 和媒体许可复核仍需分别完成。当前工作区改动尚未形成对应的远端干净提交，不得把旧 SHA 的门禁结果视为本次发布准入。 |

本阶段结束不代表项目可发布。主线同步完成后，可以从最新 `main` 创建短期功能分支开展用户明确指定的新需求，不再向历史集成分支叠加新功能。每项需求先限定范围、平台行为和验收标准，再执行定向验证与合并门禁。代码、依赖、配置或风险未变时，不为文档收尾手动重复跑全套测试；已有证据必须标明对应 SHA，自动触发的新运行不得被隐瞒或冒称通过。§2.2 的安装包、真机与许可事项继续约束发布，不自动冻结所有后续开发；未经用户明确要求，AI 不得自行挑选新功能、修改保护规则或安装/分发。
## 5. 交付流程

每个功能按以下顺序推进：

1. 确认 PRD 和协议，不接受口头隐含需求。
2. 写清验收标准、失败路径和数据不变量。
3. 实现代码和测试。
4. 跑定向测试与完整门禁。
5. 真实桌面环境走查。
6. 更新权威文档、版本和发布说明。
7. 形成可独立回退的提交。

## 6. 质量目标

| 维度 | 目标 |
|---|---|
| 正确性 | 数据层、文件系统和 UI 状态保持一致 |
| 稳定性 | 长任务可取消/恢复；重启不留下永久 processing |
| 性能 | 3 万素材可浏览；搜索目标 ≤500ms；图像解码有限流 |
| 安全 | 本地优先、凭据进 keyring、路径防穿越、SQL 参数绑定 |
| 可维护性 | commands/services/db 分层；契约集中；无历史文档冲突 |
| 可交付性 | smoke、Rust/前端门禁、真实 UAT 全部可复验 |

## 7. 风险登记

| 风险 | 影响 | 控制 |
|---|---|---|
| 真实素材样本不足 | 自动化通过但产品语义未验收 | 把真实 UAT 作为独立发布门禁 |
| 搜索协议多入口漂移 | 列表、总数、诊断结果不一致 | SearchPlanV3 作为唯一执行计划 |
| 分面双事实源 | AI、手工和筛选结果不同步 | tag_facets 唯一事实源 |
| 长任务和脏工作区 | 出错后难以回滚 | 小批提交、幂等迁移、备份保底 |
| RAW/HEIC 许可和分发 | 对外发布存在合规风险 | 分发前法务复核并保留替换方案 |
| 本地模型环境差异 | 安装成功但推理不可用 | 检测、就绪复检、推荐模型和错误引导 |
| 大组件持续膨胀 | 修改风险和维护成本增加 | 独立治理批次拆分，不与功能混改 |

## 8. 文档维护

- 产品范围变化改 [PRD.md](PRD.md)。
- 阶段状态和排期变化改本文。
- 架构或机制变化改 [ARCHITECTURE.md](ARCHITECTURE.md)。
- 机器协议变化改 [contracts](contracts/)。
- 日期化任务说明不新增长期文档，提交和 PR 即为过程记录。

## 9. 2.0 Agent开发状态

### 当前源码复核与剩余范围

2026-10-14 接续复核绑定 `aeba1776061d8aa176c77274695194ba6d6a3696` 上的当前工作区，包含未提交和未跟踪生产文件；保留此前工作，不提交、不推送、不发布。本次复核 A1/A2 领域基础、已开发 Host 和 A3-02 网关接线，修复以下实际缺陷；本机 mock 接通不等于真实 provider 验收：

- **规范轴 P2 已关闭**：准备、确认前复验、执行源复验及恢复重绑的长哈希遗漏分块授权检查；共享 1 MiB 检查点后，撤权在下一块前停止。生产队列故障注入覆盖首块及后段复验窗口的取消/撤权，无文件或资产新增，终态分别为 `cancelled`/`interrupted`。持久目标跨会话恢复语义保留。独立最终静态复核无阻断。
- **需求轴 P1 已关闭**：扫描/准备/重新准备/提交等待期间切换会话可串写草稿；迟到投影及首次初始化可能卡住页面；同会话旧快照可覆盖读取期间成功保存的新草稿。异步动作绑定发起会话，读写世代防止旧响应覆盖；独立三组隔离复现通过，两个 store 定向 28/28 通过。
- **旧名称生成物与编译标签已收口**：GitHub 当前仓库与产品配置均为 TeaCell；本机 debug/release 及目标 triple/deps 中此前发现的六份旧 `bagertea_ai_media_v2.exe` 已移入系统临时目录并保留摘要及可恢复原件。编译期间又发现 Rust 测试目标仍输出旧 library crate 名，现已改为 `teacell_ai_media_manager_lib`，入口校验也会拒绝旧 crate/package/product/repository 名。Tauri 应用标识、数据目录、系统凭据 service 与 MSI upgrade code 仍保留，以支持既有安装的数据和凭据迁移兼容。指导书补充异步会话、块中撤权及包名与临时产物名验证。

最终工作区本轮门禁（当前未提交工作区）：typecheck、lint、test:unit（85 文件 789 通过/2 跳过）、test:tooling（28/28）、cargo fmt --check、全 target/all-features Clippy、cargo test --all-features（1077 通过/0 失败/8 项既有忽略）、npm run test:agent-host（32/32）和完整 pwsh ./scripts/smoke.ps1 均退出 0。smoke 包含 Windows 后台进程、Rust 稳定组、AI/Ollama 本机 mock、typecheck、生产构建及前端单测；构建主 JS 为 717.37 kB（gzip 212.68 kB），超过 500 kB 的既有体积提示但构建成功。真实 provider/代理、用户素材 UAT、macOS/Linux、当前修复后的新安装包与发布门禁未执行，不把本机检查写成这些验收通过。

下一项是 **A3-02 最终验收证据补齐**：生产 Host↔Rust 本机 mock 网关已接通并通过端到端 fixture；当前只差真实 provider、隔离凭据与代理/费用环境的独立验收，而这些按用户要求暂不进行。A3-02 至 A3-09 共 8 张、A4 共 8 张、A5 共 3 张、A6 共 4 张，合计 **23 张仍有实现或验收工作**；A3-03 依赖 A3-02 验收。A0/A1/A2/A3-01 的本机/设计证据不等于全平台产品验收，也不按卡数声明完成百分比。

当前粗估剩余有效工作量：A3 助手/模型链路 8–14 工作日，A4 文件管理/迁仓/个人流程 8–14 工作日，A5 MCP 3–5 工作日，A6 系统验收与返修 6–12 工作日，合计约 **25–45 个有效工作日（约 5–9 周全职推进）**。这是按当前范围和必须经过的验证/独立复核作出的区间估算，不是交期承诺；真实素材、目标设备及安装/发布复核时间另计。当前用户范围只允许本机 mock，因此 A3-02 的外部 provider/凭据/代理费用验收仍未完成，A3-03 不解锁；在该限制解除前，2.0 最终验收没有可承诺的日历完成日期。后续按实际单卡耗时与新发现修订这一估算。

实现基线：`aeba1776061d8aa176c77274695194ba6d6a3696`，应用仍为1.0.2。当前工作区已有 A1 领域基础、A2 导入/历史库外处置/打标/独立工作台、A3-01 Windows x64 Host，以及 A3-02 Agent 用途绑定、Rust 网关与生产 Host 私有 stdio 的本机 mock 接线；完整助手、领域工具、迁仓、个人流程、MCP 与最终交付尚未完成。2026-10-12复盘撤回 A1-05、A2-02、A2-04、A2-05、A3-01 的当前验收结论。此前发现的本地缺口已修复；A1-05、A2-02、A2-04、A2-05 最终独立复核均无阻断，当前源码安装包预算与 Host 证据见[当前复盘阻断与修复顺序](#当前复盘阻断与修复顺序)。A3-01 已于2026-10-13完成原生产差异设计复核，未发现阻断；2026-10-14隔离首次启动发现安装包缺少 `MSVCP140.dll`，现已修复 Windows x64 VC143 资源 overlay 并在全新 Windows Sandbox 首启通过。运行库打包修复经最终独立复核，无阻断。历史 Host READY 失败输出已从当前任务记录恢复，证据指向全量 Cargo 与定向 Host 测试重叠造成的启动资源争用；串行复跑通过，摘要见 `artifacts/v2-review/a3-01-remediation-20261012/ready-timeout-diagnostic-recovered-20261014.json`。A2-05 于2026-10-13完成最终普通复核，无阻断。A2-03本轮未新增直接缺陷，且已随当前前置链路重跑全量 Rust、前端和 smoke 验证。

用户按卡推进到2.0完成的授权持续有效；A1-05、A2-02、A2-04、A2-05 的本机门禁和最终独立复核均已通过。A3-01 的原设计复核和 Windows Sandbox 首次安装启动子门均已通过；运行库打包修复经最终独立复核无阻断，历史 READY 超时也已定位为全量 Cargo 与定向 Host 测试并发的验证资源争用。A3-02 本轮已接通生产 Host/Rust mock 网关链路，并通过 Rust fixture、Node Host 及 bundled Host↔Rust 端到端测试；连接 revision 改变会在后续响应块前撤销租约，阻塞读取可被精确取消，默认生产 transport 在读取凭据前拒绝请求。A3-02 最终验收仍需要外部 provider、隔离凭据、代理/TUN 和费用环境证据；用户当前要求不连接真实 API，因此 A3-03 仍锁定。不提交、不推送、不发布。A0-03 本机 mock/设计验收与真实 provider/HTTPS/代理验收分开；mock 证据不能替代后者。A0-06 的 Windows Tauri WebView、125% DPI 和原生中文 IME 是原型证据，不是完整助手交付。真实素材/桌面 UAT、macOS/Linux、干净机器安装及全链发布验收仍未完成。

### 当前任务和关口

2026-10-09用户明确授权独立AI复核及资源预算评审。`/root/review_ui_scope`检查需求/UI，`/root/review_agent_runtime`检查宿主/模型/流程/预算，并在数据复核者因额度终止后补完A0-04/A0-05。复核者审查当前源码并执行隔离复现，不以执行者总结替代。以下“设计通过”只放行该方案用于下游冻结契约，不表示生产或三端发布验收。

| 任务 | 当前结论 | 仍需落实 |
|---|---|---|
| A0-01 基线与权威承接 | 独立需求承接设计复核通过 | PRD第8节与UI第11节定义产品/交互目标；ARCHITECTURE第9节区分目标与已实现机制，PLATFORM第9节定义实际平台边界；基线检查不能冒充新代码门禁 |
| A0-02 Pi生命周期 | Windows x64 PoC 生命周期验收完成；宿主方案独立设计复核通过 | 固定 Pi SDK 1.0.4、Node 24.19.0 与锁文件摘要 `66e0bc8d…7809a2fe`；完整原型测试92/92通过，包含真实 SDK 启动、Windows Job 强杀回收、双开、显式恢复、环境隔离、协议及资源校验。10次独立 Host 的 SDK ready 为806–902ms、正常退出18–24ms；另用 Windows `Get-Process WorkingSet64` 采样10次，空闲最大119.95MiB。当前完整 PoC overlay 含17,066个文件、230,034,090 bytes（219.38MiB，低于250MiB）；同工作区 NSIS 基线31,348,301 bytes、PoC包73,872,399 bytes，增量42,524,098 bytes（40.55MiB、135.65%，同时低于50MiB和160%）。修正 staging 解压时限后，重新跑完整资源迁移探针通过。证据在本机忽略目录 `artifacts/agent-runtime/`：`measurement-continuation.json`、`working-set-continuation.json`、`packaged-runtime-probe.json`、`poc-installer-measurement-continuation.json`。这些是当前 Windows 开发机 PoC，未安装实验包，也未验证只读目录或干净机器；完整第三方再分发许可清单、最终生产包与 macOS/Linux 生命周期仍是A3/A6门禁 |
| A0-03 模型网关 | 受控 fetch→私有 IPC→Rust 路线的 loopback HTTP/HTTPS mock 已覆盖（此前完整 Agent Host 95通过/2项 opt-in 跳过；网关专项55/55；Rust 4/4）；连接 revision/目标切换用例曾定向5/5、Rust 4/4通过，随后复核要求固定启动 revision 并加固 Node 私有字段；最终 revision 加固经独立静态复核无新增问题，最终源码 cargo fmt/Clippy/build 通过、Rust单测4/4及六个网关fixture文件56/56通过。30轮本机loopback资源测量（每profile10轮，30次均单次出站）Node空闲最高179.35MiB、流式最高180.65MiB、进程峰值192.21MiB；Rust网关流式最高12.68MiB、进程峰值14.36MiB，均低于现有工程门槛。Agent Host工程资源预算上限已定 | 当前 PoC 明确只连固定 `127.0.0.1` fixture，固定 `no_proxy()` 并使用编译期公开 fixture key，不具备真实 provider 出站模式。HTTPS 证据只验证本地证书信任/拒绝，不是外部 HTTPS/provider 验收。用户提供了 ikik 中转站及 OpenAI 兼容 API base `https://ikik.net/v1`，允许模型仅为 `glm-5.3-flash`；用户已明确本轮停留在 mock、不连接其真实 API。因此真实凭据、外部 HTTPS、系统/环境代理/TUN均未验证，A0-03本机PoC/设计验收通过，真实环境能力仍未验证；真实环境缺口本身不阻断A3-02本地实现，但真实provider/代理验收仍阻塞。本轮发现的 A0-03 本机前置代码阻断已关闭；A3-02 mock codec seam 已实现并通过复核，生产 Host↔Rust 本机 mock 接线现已完成并通过 fixture。此前聊天中暴露的凭据应由用户在服务端撤销；任何后续真实测试都须另行提供本机隔离凭据并明确请求、时长、费用与代理预算，不能从聊天读取或验证 |
| A0-04 操作恢复 | 独立原型设计验收通过 | 正式主体授权、批准期限/配置修订、操作目录所有权与同操作执行锁由A1/A2实现；E01–E08接正式adapter，不削弱断言 |
| A0-05 总仓与搬迁 | 独立原型设计验收通过；目标父junction越界发布P1关闭 | A1平台句柄策略处理敌对TOCTOU；生产资产全关联、旧入口维护门、备份世代、GC、网络盘/空间不足/掉电和三端验证仍未完成 |
| A0-06 助手交互 | Windows 原型验收完成；三项交互缺陷经独立源码/状态复核关闭，Tauri WebView、125% 系统 DPI 与原生中文 IME 已实测 | `node prototypes/assistant-ui/verify-browser.mjs` 退出0：覆盖首用引导跳过、会话切换/附件保留、独立入库交接/准备/确认、悬浮助手/完整审核、任务排队和仅取消排队项、固定搜索结果进入素材库；键盘回归覆盖跨页草稿与 ArrowLeft 调宽，960/1366/1920 下 Logo 居中且聊天区≥300px。另以隔离 Tauri 外壳实际走查 960×640、1366×768、1920×960 窗口，125% DPI；验证浅/深主题、附件→右侧工作台、素材库往返、首用引导跳过、Sogou 中文拼音组合/提交及页面切换后草稿保留。最小窗发现 toast 遮住发送区，已改为停靠右侧空白工作台并在 WebView 复测通过。外壳 `node prototypes/assistant-ui-tauri/dev.mjs` 仅访问 loopback 原型，不含命令、插件、数据库或文件访问；实现与证据说明见 [原型 README](../prototypes/assistant-ui/README.md)。该卡解锁 A3-05 的 A0-06 前置；A3-05 仍等待 A3-04 |
| A0-07 个人流程 | 格式/显式加载/每次新计划获独立设计接受 | 正式注册表、编辑UI、完整能力目录与执行授权由下游实现，不开放脚本或自动Skill发现 |
| A1-01 | 契约设计与本卡代码门禁通过 | 已建立同源枚举/标量字节限制/输入输出/JSON Schema、22项能力目录、封闭typed编解码、64KiB响应与100项页限额、审核范围/期限/预算摘要、提交重放标志及统一脱敏AppError。E01–E08已有映射且保留原断言。独立发现的元数据漏检、清空审核类别混用和非法标量数值已修复并关闭；个人流程明确版本、封闭参数、最多8步前向依赖、复合计划提交及全部能力输入样例/恢复语义已补齐，CallerContext禁止反序列化且具入口可见性检查。独立契约设计复核通过，最终完整门禁和smoke退出0；本卡验收完成。正式主体授权/生产路由及业务效果仍由下游实现 |
| A1-02 | 本卡实现、第二轮设计复核与完整门禁通过 | V27幂等追加libraries/managed_asset_locations、根身份/世代CAS、最多100项核验分页和pending分类ABA保护；不做启动全量回填。普通列表/搜索/分面/去重、ID解析、缩略图与视频代理、导出/AI取图、回填/色板/哈希、标签与数值写回、AI审核写回、设置根保存、恢复世代失效和启动回收站GC均已接入有效范围；导入在资产事务内登记托管证据。失败关闭、根失联和库外记录保留元数据已有定向测试。文件核验到打开/删除窗口的OS句柄、目录授权及敌对TOCTOU仍明确属于A1-03，不把DB范围当OS沙箱 |
| A1-03 | 已实现；本轮独立设计复核通过 | 授权主体、用途/会话/世代、目录与文件身份、撤销、分页和防链接规则见[授权契约](contracts/authorization-v1.md)。最终自动化见本轮收口记录；三端敌对TOCTOU与真机验证未完成 |
| A1-04 | 已实现；本轮独立设计复核通过 | 可信审核、准备计划、原子批准消费与幂等提交已有实现；本轮补外部操作查询会话隔离及恢复快照标志。生产导入消费者已接通，其他领域由后续卡接入 |
| A1-05 可恢复操作队列与逐项收据 | 本卡门禁通过，最终 Spec/Standards 独立复核无阻断 | 计划总量继续取受理时冻结的 step 总量；打标显式启动不会被早先待处理任务卡住，启动对账能把已知成功写成终态收据并保留未知效果，显式续跑只处理仍待处理项。新增混合未知/待处理恢复回归。导入派发已接队列，AI/导出/移动 worker 仍由后续卡接入 |
| A1-06 会话索引与持久工作台草稿 | 已实现与普通复核通过；未交付助手页面 | 会话投影、隔离草稿、附件引用与显式交接已落地；Pi transcript关联、真实素材UAT与页面消费由后续卡。既有证据见下表，当前回归见收口记录 |
| A2-01 导入扫描变固定范围计划 | 准备取消、来源与内容冻结已补齐 | 异步扫描、准确taskId取消及事务回滚，完整摘要/身份/改名预览绑定，旧无摘要计划失效；不复制、不落资产、不外发。最终验证见收口记录 |
| A2-02 执行可靠导入与恢复 | 已修复目录重启恢复、撤权副作用、暂存发布及改名目标恢复缺口；恢复重绑和全量摘要已移入阻塞线程。2026-10-13 最终差异经独立设计复核无阻断，恢复资产路径/逐项回执定向回归通过；当前 Windows 自动化门禁通过 | 同键受理、单槽派发、准确取消、显式继续、文件/DB/收据断点与保守 unknown 机制见[架构](ARCHITECTURE.md#913-可靠导入受理派发与恢复a2-02)。真实素材、断电、macOS/Linux 和其他平台链接/并发边界仍属于未完成的 UAT/平台验收 |
| A2-03 处理历史库外记录且保留整理信息 | Windows 代码验收完成；A2-02修复后本轮全量 Rust、前端和 smoke 复验通过；此前独立规格/规范复核发现的收据状态缺陷已修复并复核关闭 | 具体实现与独立复核见下方“A2-03 Windows 自动化验证与独立复核”。同账户主动并发改写路径仍有平台边界；真实素材/桌面 UAT、macOS/Linux 验证未完成，归入后续平台/UAT门禁 |
| A2-04 连接导入成功项与 AI 建议批次 | 最终独立设计复核无阻断；当前 Windows 完整门禁和 smoke 通过 | 成功收据和重复映射明确纳入、批准与 GUI start 分离、持久预算/截止时间、模型失败与取消保留 pending 均已覆盖。中断、取消和启动恢复对齐 batch/link/step/operation；最终复核补出的 command 层分写缺口已改为同一 DB 事务并以失败回滚测试验证。`aiPrepareImportBatch` 尚无前端消费者，按 A3-07 承接右侧导入/标签审核的收据接线；不把服务/API 完成说成 GUI 流程完成。mock 不证明真实 provider、代理/TUN 或 macOS/Linux；按用户要求未连接真实 API |
| A2-05 重构独立入库为可复用工作台 | 持久草稿、同键受理、精确取消/恢复、结果 revision 竞态与会话隔离已修复；活动会话变化会同步投影，切换首帧及加载期间禁用副作用入口，store 拒绝旧会话选项写入。最终卡级普通复核通过，无阻断 | 终态结果查询按 operation revision 去重，查询失败可按 operation 重试；旧 `import_files` 直写 IPC 已移除。四文件定向52/52通过；真实素材/桌面 UAT、macOS/Linux 仍未完成，不冒称平台验收 |
| A3-01 Pi Host/受控宿主协议 | 2026-10-13原生产差异独立设计复核无阻断；2026-10-14以断网全新 Windows 11 Sandbox 完成当前 NSIS 包静默安装与首次启动窗口观察。历史失败输出已从任务记录恢复：`cargo test --all-features` 与定向 `windows_integration_tests` Cargo 测试并发，进程快照同时观察到两个 Cargo 进程；两项 Rust Host 启动测试超时，Node Host 测试等待 READY 10.07 秒仍无帧、stderr 为空且子进程未退出。相同 Node 场景随后独立运行约0.89秒通过；当前串行 Rust Host 5/5、Node Host 13/13通过。证据支持并发验证负载造成启动资源争用，未复现 Host 代码缺陷；摘要见 `artifacts/v2-review/a3-01-remediation-20261012/ready-timeout-diagnostic-recovered-20261014.json`。当前 staged probe 已从成功构建记录读取确切overlay并通过；本轮完整差异最终独立复核无阻断 | 当前源码重建的 NSIS 为72,487,073 bytes，SHA-256 `AE5CF604AAEB1FBCC2189B4ABFF74975FE3F0BE1C896ED5D1F822D5FC9F5EC00`；较无 Agent 基线31,348,301 bytes增加41,138,772 bytes（39.23 MiB、131.23%），低于50 MiB/160%门槛。测量见忽略目录 `artifacts/v2-review/a3-01-remediation-20261012/sandbox-install-20261014/installer-measurement-vcruntime-sandbox-20261014.json`。断网全新 Sandbox 安装退出0、应用在`WDAGUtilityAccount`配置目录下首次启动，结果见 `guest-output-vcruntime-rebuild-20261014/result.json`；当前用户原有同名产品与卸载注册未触碰。其他平台入口禁用，真实 provider/发布不属于本卡本机 mock 证据 |
| A3-02–A6 | A3-01 验收完成。A3-02 生产 Host↔Rust mock 网关本机实现和自动化验证已接通：schema v3 固定 OpenAI/Anthropic 文本 codec、凭据仅在 Rust、共享限流与预算、分块上传/按需 pull、usage、连接 revision 复核和精确取消。Rust Agent Runtime 定向 39/39、Node Host 32/32、bundled Host↔Rust mock 端到端 1/1 通过；Rust 以常量空间高水位拒绝重复或倒退的 gateway request ID，Host 错误摘要把固定网关类别和白名单 HTTP 状态传到 UI；双协议 length/max_tokens fixture 确认不隐式重试或自动压缩，bundled Host 429 fixture 确认只有一次网关请求；最终独立设计复核无阻断，并确认 schema v3 文档示例与实现一致。默认生产 transport 仍为 disabled，并在读取凭据前失败。A3-02 最终验收尚未完成：冻结契约仍要求外部 provider、隔离凭据、代理/TUN、费用与辅助请求覆盖证据；用户当前保持 mock-only，因此 A3-03 仍锁定 | 当前最终工作区：typecheck、lint、unit（85 文件 789 通过/2 跳过）、tooling 28/28、cargo fmt --check、全 target/all-features Clippy、cargo test --all-features（1077 通过/8 项既有忽略）、Agent Host 32/32 和完整 Windows smoke 均退出 0。生产 Host 测试只注入内存 fixture；没有真实 endpoint/密钥/代理请求。真实素材、三端支持、资源/许可、干净机器安装与发布仍需对应验收
| 2.5插件 | 不在当前开发目标 | 2.0只保留有真实消费者的能力接点、内置导航和百宝箱说明，不建插件宿主占位 |

2026-10-09曾批准“允许有期限结转，先继续A1”，原定期限为2026-10-16或A3-01开始前。此后用户明确授权按卡推进到2.0完成，取代了该结转对开发范围的限制；用户当前要求停留在本机 mock、不连接其真实 API。A0-03本机PoC及设计验收已通过，真实 provider/凭据/外部 HTTPS/系统与环境代理/TUN矩阵明确未验证；该环境缺口本身不阻断A3-02本地实现，但阻断A3-02验收。该段记录的是当时中间态：基于 Pi 1.0.4 实际 codec 建立了占位地址的 OpenAI Chat/Anthropic Messages 本机 mock fetch seam 和请求体、取消、拒绝回归，但尚未接入生产 Host↔Rust 网关；后续已完成生产接线，当前证据见本节 A3-02 行。A1-05、A2-02与A2-04最终独立复核已通过；A3-01隔离安装与首次启动子门已通过，但本轮运行库打包修复经最终独立复核无阻断，且历史 READY 失败输出已恢复并定位为全量与定向 Cargo Host 测试并发造成的验证资源争用；串行复跑通过，A3-01现已验收并解锁 A3-02本机 mock 实现，真实 provider 验证仍保持未连接。A2-05最终普通复核已于2026-10-13通过，无阻断。必要环境验收仍按各卡与最终关口执行。A2-04 导入收据到 GUI 的消费者由 A3-07 接入。当前没有真实 API 请求，也未读取或修改日常密钥/代理配置。A0-06 的 Windows 原型验收已完成，A3-05 仍依赖 A3-04，A6 发布结论仍取决于全链验收。缺项保持未验证，完成条件见对应卡和 [14](../plans/v2/14-review-and-handoff.md#状态依赖与授权)。

### 当前复盘阻断与修复顺序

本轮对象是上述基线上的当前工作区，涵盖未跟踪生产模块；不是只审已提交差异。首轮规格/规范复核及主审调用链检查发现下列问题，代码已据此修复并完成本机门禁。整改后三项修复已完成两轴定向复核且未发现阻断；A1-05、A2-02、A2-04 最终差异经独立复核均未发现阻断；A2-05 最终卡级普通复核已于2026-10-13完成，无阻断。A3-01原生产差异的设计复核于2026-10-13完成，无代码阻断；2026-10-14已通过隔离安装/首次启动子门，但本轮VC运行库打包及probe overlay选择修复经最终独立复核无阻断，历史READY超时失败输出后来从任务记录恢复并定位为验证期间全量与定向 Cargo Host 测试并发造成的资源争用；串行复跑通过。过去的“无阻断”结论只对当时审查范围和已覆盖场景成立。

| 优先级/归属 | 已确认问题及证据 | 修复与重新验收要求 |
|---|---|---|
| P1 / A2-02 | 已修复：授权在每个复制块和进度回调后复检；1 MiB 分块复制、完整 SHA-256、同卷不覆盖硬链接发布及暂存/发布身份收据已覆盖撤权和重启恢复故障。错误目录恢复失败关闭，同目录经可信 GUI 重选后才原子重绑。`prefix_{原名}` 已发布、DB 尚未登记且源已删除时，恢复按持久目标身份/摘要登记，不重复套用模板；重绑和全量哈希置于 `spawn_blocking`。最终设计复核无阻断；新增回归同时断言资产 `file_path` 指向该发布目标、逐项成功及 `item_imported.db_ref=asset:{id}`。 | Windows 自动化与本机支持范围内的代码验收通过；跨平台文件句柄/链接边界、真实素材与断电 UAT 尚未完成。 |
| P1 / A2-02 | 已修复：重启后不复用旧 session/instance 文件句柄。可信 GUI 重新选择后，服务核对目录及未完成项的位置、身份、大小、修改时间和完整摘要，再原子重绑；错误目录保持 interrupted。最终差异的独立设计复核及本轮完整门禁通过。 | 其他支持平台的文件句柄/TOCTOU、真实素材及真实进程强杀/断电边界仍未验收。 |
| P1 / A2-05 | 已修复并经最终普通复核：taskStore 保留最新 operation 快照；终态 revision 查询去重/防旧响应覆盖；会话切换跟进最新 activeRef，切换期间副作用入口禁用，选项写入校验活动会话。52项定向回归通过。 | 真实媒体/桌面 UAT 和 macOS/Linux 验证仍未完成。 |
| P1 / A2-04 | 已修复：恢复前比较连接修订、首次启动截止时间、请求预算和冻结范围；失效后取消旧 operation/step/link 并保留 pending 建议/已花预算，再要求新计划和批准。最终规范复核发现的 command 层 interrupted 分写改为与 operation/step/link 同事务，覆盖正常中断和失败回滚；当前专项17/17、全量 Rust/前端门禁和 smoke 均通过，最终独立复核无阻断。 | `aiPrepareImportBatch` 的 GUI 收据入口转由 A3-07 接入；真实 provider 与代理/TUN 按用户要求未连接，A3-02验收仍被环境证据阻断。 |
| P1 / A3-01 | 固定版本/校验和资源准备、空 PATH staged probe 与 Windows x64 VC143 运行库 overlay 已通过自动化；2026-10-14断网全新 Windows 11 Sandbox 中安装退出0并观察到首次启动窗口。原生产差异的独立设计复核无阻断；本轮打包和 overlay 选择修复经最终独立复核无阻断。历史失败日志从任务记录恢复后，确认两条 Cargo 测试并发期间 Host READY 等待无帧超时；当前串行 Rust Host 5/5 与 Node Host 13/13通过，根因定位为验证进程争用，未复现产品代码缺陷。 | Windows x64 自动化、隔离安装/首启和最终差异复核通过；远端CI、其他平台生命周期仍未执行。诊断摘录见 `artifacts/v2-review/a3-01-remediation-20261012/ready-timeout-diagnostic-recovered-20261014.json`。 |
| P2 / A1-05、A2-02 | 已修复：operation `total` 使用受理时冻结的 step 总量；取消及逐项延迟登记测试通过。A1-05 另修复打标启动被 FIFO 队列阻塞、启动恢复未终结已知成功收据的问题，并覆盖混合未知/待处理显式续跑。最终 cargo test 与 smoke 通过，Spec/Standards 独立复核无阻断。 | 三端和真实断电/媒体验收另属平台/UAT关口。 |
| P2 / A2-05 | 已修复并有回归：会话切换期间慢投影不反向选择旧会话；新会话选择会在加载后收敛，选项写入不会落回旧草稿。最终卡级普通复核通过；四文件定向52/52。 | 真实桌面 UAT、macOS/Linux仍未完成。 |
| P2 / A2-02 | 已修复并有回归证据：暂存对象身份先于复制持久化，逐块检查取消/授权；恢复只截断身份仍匹配的未发布暂存文件，已发布目标按持久化目标、对象身份和摘要核验，不续写正式目标或重新改名。复制撤权和发布前后崩溃恢复测试通过。 | 复核其他目标平台上的硬链接支持、同账户竞态及真实断电/素材旅程；没有这些证据前不扩大平台或 UAT 结论。 |

修复顺序：本轮新发现的 A2-02 改名发布恢复、恢复哈希线程占用、A2-05 旧 revision 结果覆盖、A2-04 command 层中断状态分写，以及 A1-05 FIFO 启动阻塞和启动恢复收据对齐问题均已修复；当前 typecheck、lint、前端单测、tooling、Rust fmt/Clippy/test、完整 smoke 和当前源码 NSIS 预算通过。A1-05、A2-02、A2-04 与 A2-05 最终独立复核均通过并关闭对应代码阻断。A3-01原生产差异独立设计复核未发现代码阻断；本轮修复了缺失VC运行库导致Sandbox首启失败、staged probe按mtime误选overlay，以及缓存并发获胜时遗留暂存目录的问题。隔离安装和首次启动已通过，打包/脚本差异经最终独立复核无阻断；历史 Host READY 超时现已从任务记录恢复出失败日志，确认发生于全量与定向 Cargo Host 测试并发期间；当前串行 Rust Host 5/5、Node Host 13/13通过，证据支持验证资源争用为根因。用户要求停留在 mock，因此 A0-03 的真实 provider/外部网络/系统代理/TUN验证以及依赖它的 A3-02最终验收未完成；A3-03之后依任务索引依赖顺序推进。A2-04 导入收据的 GUI 入口由 A3-07 承接。保留现有下游代码，不索取真实凭据。

本轮最终工作树验证（基线 `aeba1776061d8aa176c77274695194ba6d6a3696`，未提交工作区）：A1-05 定向恢复回归通过；`npm run typecheck`、`npm run lint`、`npm run test:unit`（85文件781通过/2跳过）、`npm run test:tooling`（27/27）、`npm run build`、完整 `pwsh ./scripts/smoke.ps1` 及 `git diff --check` 均退出0。此前 Rust `cargo fmt --check`、全target/all-features Clippy、`cargo test --all-features` 均退出0：736个库单测通过、0失败，集成与文档测试通过，8项既有忽略；A1-05定向混合未知/待处理恢复用例通过。本轮没有修改Rust源；NSIS生产构建再次成功编译当前 Rust 应用。`npm run test:agent-host` 13/13；本轮 `npm run test:agent-host-staged` 退出0，读取成功构建记录中的overlay `tauri-overlay-e2c957a57fe0ee82.json`（SHA-256 `e2c957a57fe0ee82cbe5479a198abd30d16caa3babbc2220d889ae5ed8c6734f`），空 PATH 下随包Node 24.19.0完成握手/会话/发送/关闭/pin/unpin/退出。A1-05最终 Spec/Standards 独立复核以基线 `aeba1776061d8aa176c77274695194ba6d6a3696` 对照，无阻断；A2-02、A2-04、A2-05最终独立复核无阻断；A3-01完整差异最终独立复核无阻断；当时未恢复历史READY失败输出，随后于2026-10-14从任务记录找回并定位为并发验证资源争用。当前源码NSIS为72,487,073 bytes，SHA-256 `AE5CF604AAEB1FBCC2189B4ABFF74975FE3F0BE1C896ED5D1F822D5FC9F5EC00`，增加39.23 MiB/131.23%；断网全新 Windows 11 Sandbox 使用同一 SHA 包安装退出0并首次启动窗口观察通过。Cargo门禁和smoke沿用本轮JavaScript资源脚本/证据修正前的成功结果，未为仅脚本改动重跑。远端CI、真实 provider、真实素材/桌面 UAT和 macOS/Linux仍未执行。A2-04 的导入收据到 GUI 调用点归 A3-07 承接，不能将服务/API 完成当作整条用户流程交付。

文档处理仅校准本文当前状态、架构目标/已实现边界、开发规范中的资源准备说明；AGENTS、PRD、冻结授权/预算契约和任务依赖不因发现缺陷而放宽。历史证据保留在下节，不新建第二份长期复盘报告。

### 已关闭的独立复核缺陷

- 正文输入：20万空块曾产生约167MiB heap；改增长字节缓冲和event-loop yield后，复核又发现共享pending Promise的reaction保留约106MiB。最终改为每次read独立中断Promise并清理当前回调。独立GC复现：20万空块读取中heap增量577416B，20万1字节块89248B，定时器均可执行。18项上传负测/续轮通过；新增2项独立进程GC回归，观察最后一次读取未返回时live heap，旧实现会失败。限额不包含调用者预生成JSON/Request或producer自身内存。
- 搬仓：真实migration-partial退出77后将目标分库换为Windows junction，旧代码先向库外发布再报错。现逐层创建/核验目标父目录，并在发布前复检；新测试确认库外无新文件、原记录/世代/源文件保持，拆链接后用原收据继续。只关闭已存在链接问题，不声称消除敌对检查到使用窗口。
- UI：交接有旧工作时先明确选择新对话，旧附件/计划/任务不覆盖；已受理手动清单不能重建。底栏有同侧前移/后移及稳定焦点ID，Logo不可移动。mock搜索以固定资产ID进入素材库，库选择复制到明确新对话，不受后续live选择改写。独立reducer复核覆盖待审/已批准/运行/完成，浏览器实际验证搜索→库→选择交接、交接冲突弹窗及键盘排序焦点；Logo中心与窗口中心相等。截图 `artifacts/assistant-ui/review-handoff.png` 为内存原型，未接真实库。

### 分卡历史验证证据

| 检查 | 实际结果及范围 |
|---|---|
| A0-03 Windows loopback 模型网关复验 | `cargo build --locked`、`cargo fmt --check`、全目标/all-features Clippy、`cargo test --locked --all-features` 均退出0（Rust 4/4）；Agent Host `npm test` 全量96项中94通过、2项需显式 Windows Job owner 的 opt-in 测试跳过。OpenAI/Anthropic 两个真实 Pi codec 均通过受信 HTTPS loopback fixture；未受信 CA 在首个 HTTP 请求前失败；HTTP/HTTPS 注入环境代理均未触达代理监听器。HTTPS CA 只由隔离测试夹具显式注入；证书和私钥为回环测试身份，固定连接 `127.0.0.1`、固定路径、`no_proxy()` 和公开 fixture key。期间全套测试曾一次遇到 policy fixture 替换 `EPERM`，目标测试单独20/20通过，随后完整套件94通过/0失败；未稳定复现。此证据只覆盖受控 mock 的 SDK 编码/传输/恢复/取消及本地 TLS 信任处理，未请求外部 HTTPS/provider，也未核验真实 Keyring、系统代理、TUN或生产费用 |
| A0-03 2026-10-11本机 mock 专项复验 | cargo build --locked --offline、cargo fmt --check、cargo clippy --locked --offline --all-targets -- -D warnings、cargo test --locked --offline 均退出0（Rust 4/4）；工作目录为 prototypes/model-gateway。npm test 在 prototypes/agent-host 下退出0（95通过、0失败、2项 opt-in 跳过，均需显式 Windows Job owner）；node --test test/gateway*.test.mjs 在 prototypes/agent-host 下退出0（55通过、0失败、0跳过），覆盖 Rust 到 loopback HTTP/HTTPS、OpenAI/Anthropic codec、路由/模型拒绝、公开哨兵、环境代理隔离、连接拒绝/无自动重放、流取消/重试/压缩/总结/截断恢复/上传边界。所有 provider 均为本机 mock；没有访问外网、真实 keyring 或真实 provider。系统代理/TUN 与真实服务仍未验证 |
| A0-03 revision/连接目标切换收口 | 旧 revision 拒绝、新 revision 只到新 loopback 目标；最终实现把 revision 固定到 Rust 启动配置及 Node 私有字段，并在启动临时服务前拒绝非法 revision。独立静态复核无新增问题；最终差异 cargo fmt/Clippy/build 通过、Rust单测4/4、六个网关fixture文件56/56通过。最终源码30轮资源测量报告与二进制摘要一致，指标低于现有工程门槛。真实 API/凭据/外部网络及真实系统代理/TUN仍未验证；按A0验收规则记为明确环境阻塞，A0-03本机PoC/设计关口验收通过。A3-02只解锁本地实现，外部验证仍阻断其最终验收 |
| A2-03 Windows 自动化验证与独立复核 | `cargo test --all-features legacy_assets::tests:: -- --nocapture` 退出0，40通过。首轮独立规格复核发现两项边界：准备态目标越出仍可验证的库根时应转为 `outcome_unknown`；旧迁移若已补齐精度列、但在隔离活动收据前中断，重启会因列已存在而漏隔离。两项均已修复并补回归：前者不创建目标且保留旧记录，后者以版本 marker 和单事务保证列升级、活动收据隔离、marker 一起提交；完整精度缺失的 `prepared` 收据不会继续执行。规范复核发现 join 失败需按 `INTERNAL` 分类，命令层已改用 `AppError::internal`。规格与规范复核按项目基线 `aeba1776061d8aa176c77274695194ba6d6a3696` 对 A2-03 最终差异重审，均无阻断。`cargo fmt --check`、全 target/all-features Clippy、`cargo test --all-features`（1012通过、8项既有手动/性能探针忽略）和完整 `pwsh ./scripts/smoke.ps1` 均退出0；smoke 内 typecheck、build、前端单测（85文件765通过/2跳过）及 AI/Ollama mock 集成均通过；独立 `npm run typecheck`、`npm run lint`、`npm run test:unit`、`npm run test:tooling`（22/22）和 `npm run build` 均通过。证据绑定当前 Windows 未提交工作树。真实素材/桌面 UAT、macOS/Linux 验证及同账户主动改写边界仍未完成 |
| Windows 全仓 lint/构建产物收口 | 初次 `npm run lint` 发现原型 Node 计时器未声明和嵌套 `target` 生成代码被扫描；现为原型显式导入 `node:timers`，让 ESLint 跳过所有 Rust `target` 构建目录，并让 Git 忽略任意层级的 Rust `target` 产物。修改后 `npm run lint` 退出0、`node --check prototypes/assistant-ui-tauri/dev.mjs` 通过，`git check-ignore` 确认嵌套产物被忽略；不改变业务行为 |
| A2-04 Windows 自动化验证与独立设计/规范复核 | 最终工作树 `cargo test --all-features` 退出0：库单元727通过，集成/文档组 AI adversarial 14、AI service 13、DB 71、maintenance 1（1既有忽略）、format 8、foundation 78、Ollama 12、perf 3（6既有忽略）、QA edge 54（1既有忽略）、search 16、services 11、V24 16、W7 8、文档 2；合计1034通过、0失败、8项既有忽略。`cargo fmt --check`、全 target/all-features Clippy、typecheck、lint、前端单测（85文件776通过/2跳过）、tooling（22/22）、build 和完整 `pwsh ./scripts/smoke.ps1` 均退出0；打标专项17/17。最终独立设计/规范复核以 `aeba1776061d8aa176c77274695194ba6d6a3696` 为比较基线，无阻断；非取消错误的 batch interrupted 状态现由 DB 事务与 operation/step/link 同步，并有失败回滚测试。`aiPrepareImportBatch` 暂无 GUI 调用点，由 A3-07 接通。所有 provider 请求均为本机 mock；真实 provider/代理、真实素材、断电/进程强杀、完整桌面旅程和 macOS/Linux 未验收 |
| A2-05 Windows 自动化验证与独立复核 | 最终源码Rust门禁（与A2-04完整门禁相同的Rust差异范围；本轮A2-05闭环仅改前端/架构文档）`cargo fmt --check`、全 target/all-features Clippy、`cargo test --all-features`均通过，1034通过、0失败、8项既有忽略；当前工作区 `typecheck`、`lint`、`test:unit`（85文件781通过/2跳过）、`test:tooling`（22/22）、`npm run build`、完整 `pwsh ./scripts/smoke.ps1` 均退出0。定向四文件52/52；当前 Host 13/13和A3-01最新overlay空PATH staged probe通过。文档链接及 `git diff --check` 通过。最终普通独立复核以基线 `aeba1776061d8aa176c77274695194ba6d6a3696` 对照，无阻断；规范复核记录极端续跑历史下读取完整收据流的非阻断规模风险。证据绑定当前Windows未提交工作树，不替代真实媒体/桌面UAT、macOS/Linux验证或A3-01验收前置 |
| A3-01 Windows Host/安装包/独立复核 | 首次隔离启动发现主程序直接导入 `MSVCP140.dll`，原 NSIS 包未提供该库；现以 `prepare-agent-host` 验证固定为14.44.35211.0的微软VC143 x64运行库签名、Redistributable SHA-256和十个DLL逐项bytes/SHA256 pin，并暂存到仓库盘内容寻址缓存，再映射十个 DLL 至 `$INSTDIR`。当前源码 NSIS 包 72,487,073 bytes，SHA-256 `AE5CF604AAEB1FBCC2189B4ABFF74975FE3F0BE1C896ED5D1F822D5FC9F5EC00`；较无Agent基线31,348,301 bytes增加41,138,772 bytes（39.23 MiB、131.23%），低于50 MiB/160%预算。前端 typecheck、lint、unit（781通过/2跳过）、tooling（27/27）、Host（13/13）均通过；NSIS 构建及 `npm run test:agent-host-staged` 退出0，probe读取该次成功Tauri构建记录的确切overlay `tauri-overlay-e2c957a57fe0ee82.json`（SHA-256 `e2c957a57fe0ee82cbe5479a198abd30d16caa3babbc2220d889ae5ed8c6734f`），在空 PATH 下用随包 Node 24.19.0完成握手、新会话、发送/关闭、pin/unpin，运行库资源10个/1,845,512 bytes。断网全新 Windows 11 Sandbox 中 `WDAGUtilityAccount` 静默安装退出0，首次启动进程具有非零主窗口句柄、标题“茶馆”，结果见 `guest-output-vcruntime-rebuild-20261014/result.json`。本轮打包/overlay修复经最终独立复核无阻断；历史 READY 失败日志已从任务记录恢复并诊断为全量与定向 Cargo Host 测试并发导致的验证资源争用；当前串行复跑通过，A3-01验收完成并解锁 A3-02本机 mock 实现。本轮已串行复验 `cargo fmt --check`、全 target/all-features Clippy、`cargo test --all-features`（1048通过/0失败/8既有忽略）及完整 Windows smoke，均退出0。当前用户已有同名产品与卸载注册未触碰；同账户主动替换数据目录祖先竞态仍未自动化覆盖。制品、VC DLL 摘要和安装证据见忽略目录 `artifacts/v2-review/a3-01-remediation-20261012/sandbox-install-20261014/installer-measurement-vcruntime-sandbox-20261014.json` |
| Host固定Node24.19.0、显式Windows owner完整test/*.test.mjs | 90通过/0失败/0跳过，退出0；此后只新增2个GC测试并单独2通过，未声称完整92套已跑 |
| gateway-upload定向 | 18通过/0失败/0跳过，退出0；复核者独立重跑相同结果 |
| operation-recovery完整cargo test --locked | 62通过/0失败；2项opt-in跨卷ignored，退出0 |
| cross_volume显式--ignored专项 | 修改后重新执行2通过/0失败/0忽略，退出0；Windows系统temp与仓库fixture卷序列号不同 |
| operation-recovery格式及Clippy | fmt --check、clippy --all-targets -- -D warnings退出0 |
| 独立政策mutant | forged-approval开关使E03从预期[0,0,0]变[1,2,1]，退出101；移除进程开关后再次通过，无生产政策修改 |
| 前端工具检查 | 本轮lint退出0；HTML脚本解析及build通过。浏览器检查不等于Tauri/DPI/IME |
| 主应用既有基线 | 前期typecheck/lint退出0、unit81文件748通过/2既有跳过、tooling18通过；Windows HEIF准备后Rustfmt/clippy/test退出0。不借旧基线宣称新增A1代码完整门禁 |
| A1-01最终验证 | 最终源码完整Rust cargo test --all-features退出0：库单元544通过，全部套合计851通过（含doc编译拒绝2项）、0失败、8既有ignored；fmt及Clippy全target/allfeatures退出0。契约14项及错误4项在完整套内。前端typecheck/lint退出0，unit81文件748通过/2既有跳过；tooling22通过/0失败/0跳过，含20种政策/数值负测及8条case映射。独立设计复核通过，复核者生成检查及契约tooling4项通过；不冒称其独立跑过Rust。完整smoke退出0，含后台进程、Rust稳定组/mock网络、typecheck/build/unit。不把类型契约当作生产授权或业务效果 |
| A1-02完整验证 | 最终源码 `cargo fmt --check`、全target/allfeatures Clippy和 `cargo test --all-features` 均退出0：库单元564通过/0失败，集成与文档测试全通过，既有8项ignored；新增/更新范围测试覆盖新资产不继承、缺映射分页、3万旧资产0同步回填、位置ABA、根同epoch禁写、缺标记ready根失败关闭、恢复世代轮换、库外记录不进列表/分面/回填/色板缓存/评分旋转、离线跳过到期清理。前端 `typecheck`、`lint`、`test:unit` 748通过/2既有跳过、`test:tooling` 22通过、`npm run build` 均退出0；`pwsh ./scripts/smoke.ps1`完整P0套退出0。该证据证明当前Windows实现与现有业务回归，不代替真实素材UAT、三端、真实provider/代理或A1-03 OS句柄验收 |
| A1-03完整验证 | `cargo fmt --check`、全target/allfeatures Clippy和`cargo test --all-features`退出0：库单元584通过/0失败，A1-03定向9项覆盖主体可见性、会话/用途/世代、撤销、伪造/过期句柄、目录替换、多层分页、链接逃逸、文件替换、导出覆盖拒绝和托管资产身份；前端`typecheck`、`lint`、unit81文件748通过/2既有跳过、tooling22通过、`npm run build`及`pwsh ./scripts/smoke.ps1`全部退出0。该证据是Windows工程与自动化边界，不替代三端、真实素材或敌对同账户TOCTOU验收 |
| A1-04完整验证 | 最终源码完整`cargo test --all-features`退出0：库单元584通过/0失败；A1-04定向11项覆盖prepare零operation、可信GUI审核、外部计划跨入口审核、会话/实例绑定、PLAN_CHANGED范围/服务/模型变化、计划与批准过期、库离线、批准不误消费、幂等重放/冲突和并发submit一次受理。全target/allfeatures Clippy与fmt退出0；前端和完整smoke沿用同工作区A1-03验证结果 |
| A1-05完整验证 | 最终源码`cargo fmt --check`、全target/allfeatures Clippy与`cargo test --all-features`退出0：736个库单测通过/0失败，集成与文档测试通过、既有8项ignored；新增混合未知/待处理显式续跑恢复回归通过。前端`typecheck`、`lint`、`test:unit`（85文件781通过/2跳过）、`test:tooling`22/22、`npm run build`及`pwsh ./scripts/smoke.ps1`全部退出0。最终 Spec/Standards 独立复核对照基线`aeba1776061d8aa176c77274695194ba6d6a3696`无阻断。该证据是Windows工程边界，不代替真实AI批次/导出接入与三端验证 |
| A1-06完整验证 | 后端`cargo fmt --check`、全target/allfeatures Clippy与`cargo test --all-features`退出0：A1-06定向7项覆盖两会话草稿隔离、草稿重读与revision、非法种类/载荷拒绝、跨principal隔离、改名/归档/标题搜索投影、未结束operation阻断删除且收据保留、附件缺失显式不可用。前端`typecheck`、`lint`退出0，新增`conversationStore`3项测试（两会话交替编辑不串清单、按稳定ref重建、删除只移除投影）通过；`test:unit`/`test:tooling`/`npm run build`/`pwsh ./scripts/smoke.ps1`沿用同工作区A1-05验证结果并通过。该证据不代替真实素材附件与Pi transcript关联的UAT |
| A2-01完整验证 | 最终源码`cargo fmt --check`、全target/allfeatures Clippy与`cargo test --all-features`退出0：库单元599通过/0失败，集成与文档测试全通过、既有8项ignored；A2-01定向4项覆盖准备阶段不写资产/不在总库产生文件且绑定指纹与后端同源改名预览、不支持项在重新准备被剔除后不重现、准备后文件变化被`verify_import_plan`判为stale、未知目录来源与非法分库名返回结构化错误。前端`typecheck`、`lint`、`test:unit`82文件751通过/2既有跳过、`test:tooling`22通过、`npm run build`及`pwsh ./scripts/smoke.ps1`全部退出0。该证据不代替真实媒体UAT与A2-02正式执行/恢复 |
| A2-02完整验证 | 最终源码`cargo fmt --check`、全target/allfeatures Clippy与`cargo test --all-features`退出0：库单元603通过/0失败，集成与文档测试全通过、既有8项ignored；A2-02定向4项覆盖成功导入并记录asset收据、重复项映射既有asset不重复制、准备后源文件变化返回`PLAN_CHANGED`且不落资产、执行前取消不导入。前端`typecheck`、`lint`、`test:unit`82文件751通过/2既有跳过、`test:tooling`22通过、`npm run build`及`pwsh ./scripts/smoke.ps1`全部退出0。smoke包含稳定Rust组603项、网络集成13+12项、生产构建及前端单测；本证据是Windows工程边界，不代替独立设计复核、真实媒体UAT和三端验证 |
| 真实服务/三端真机 | 未执行，不写成通过；完整主应用smoke已执行，结果见上行 |

原型使用独立临时SQLite、字节fixture、mock网络，未用于真实素材。测试假图片不等于imaging验收。源清理不递归删根或未知文件；暂存witness仍保留，不能说已释放全部空间。OS维护锁只协调同canonical fixture DB，不能代表生产单宿主或多个DB同根安全。

### 运行时、制品与资源证据

- Pi八个同组运行时包精确1.0.4、应用专用loader/环境、不发现外部技能/扩展/AGENTS、无默认read/bash。Node24.19.0三目标官方归档摘要核验；只有Windows实际执行。WindowsJob在SDK导入前启动屏障，强杀回收自有后代且保留无关Node，不是恶意代码沙箱。
- 真实Pi两个codec经Rust到mock；普通/辅助压缩/总结/续轮/重试/取消/断流及配置修订已有测试。真实keyring、外发预览批准、持久费用和生产预算尚未实现。持久Pi原生会话新进程恢复打开时0出站/0工具，明确继续后才请求；损坏v3 transcript先只读拒绝保留，生产索引/迁移与路径交换仍由后续处理。
- Windows生命周期旧10次新进程样本ready890.6–961.7ms、正常exit最大28ms、Host RSS最大120.27MiB。旧未筛选NSIS相对无Agent基线增量42503049B（40.53MiB/137.31%）；它不是当前完整overlay验收。依赖库存123451059B、Node完整解包106112876B，不当作最终安装文件总量。
- 固定资源布局probe核验10个二进制资产、SDK/mock回复、PNG及Windows addon/owner屏障；不是最终安装包提取/只读目录/干净机器证明。Pi及另外三项附加许可证文本有固定来源/版本/摘要；全部依赖、示例和媒体许可尚未全部完成复核，不据局部清单宣称发布通过。
- 修复前最新30次网关样本：三profile各10次且各1出站；最高idle136.38/122.11/164.53MiB、streaming136.88/124.95/182.92MiB、进程peak141.21/141.21/187.91MiB，Rust streaming9.07/10.12/12.54MiB。报告保留 `artifacts/agent-runtime/gateway-measurement-before-review-fix.json`，不冒充新正文代码测量。
- 2026-10-11源码摘要复核后曾重新完成资源测量：三profile各10次，30次均各1出站并完整结束。最终 revision 固定修订后重新完成测量（Windows x64、Node 24.19.0、本机 fixture）：30个样本（每profile10次），30次出站均各1次。Node空闲最高179.35MiB、流式最高180.65MiB、进程峰值192.21MiB；Rust网关流式最高12.68MiB、进程峰值14.36MiB，低于[PERFORMANCE第9节](PERFORMANCE.md#9-20-agent-windows工程预算已独立复核)现有门槛。报告 `artifacts/agent-runtime/gateway-measurement.json` 绑定最终源码/可执行文件摘要。报告只代表本机loopback mock；PowerShell采样及fixture占用包含在进程数据中，未验证生产负载或干净机器。此前较早报告副本保留为 `artifacts/agent-runtime/gateway-measurement-before-source-revalidation-2026-10-11.json`。

受支持平台失败仍阻止发布；Windows数字不替代Mac/Linux本机结果。最终完成需按A6-04覆盖整个2.0目标。A0结转只覆盖已批准的环境项目，工具故障不豁免独立复核或业务故障测试。各卡当前验收结论和验证证据以本节为准；后续只从所有前置均已验收的未完成卡继续，不把下游已有文件视为验收。
