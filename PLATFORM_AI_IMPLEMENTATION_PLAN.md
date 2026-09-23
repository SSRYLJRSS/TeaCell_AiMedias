# 三端统一版本：AI 实施计划

> 文档类型：一次性 AI 交接计划（Ephemeral）
>
> 适用执行者：Luna 或其他代码代理
>
> 创建日期：2026-09-22
>
> 本文不是产品、架构或平台事实源。发生冲突时依次服从 `AGENTS.md`、`docs/README.md` 指向的
> 权威文档和当前代码。完成实施后，把仍然有效的状态更新到 `docs/PROJECT_PLAN.md`，把过程记录
> 留在提交、PR 或任务中；本文件不长期维护。

## 1. 任务目标

在不新增产品功能的前提下，把当前 Windows 功能基线整理为一个可复验的统一三端版本：

- Windows 10/11 x64：正式支持目标，保留现有完整功能。
- Apple Silicon macOS：预览支持目标，至少通过导入、浏览、搜索、查看、导出核心闭环。
- Ubuntu 24.04 LTS x64：预览支持目标，至少通过同一核心闭环。
- 三端来自同一 `main`、同一候选提交和同一最终标签。
- Windows 专属能力允许保留；非支持端必须明确禁用或降级，不能假入口、假成功或后台盲调。

本计划不包含新业务功能、跨系统搬库、Intel Mac、其他 Linux 发行版、账号同步或完整桌面 E2E。

## 2. 执行方式

一次只执行一个“批次”。每批都遵守：

1. 先读 `AGENTS.md`、`docs/README.md`、`docs/PLATFORM.md`、`docs/ARCHITECTURE.md` 和
   `docs/DEVELOPMENT.md`。
2. 运行 `git status --short --branch`，识别两个 worktree 的用户改动。
3. 写出本批行为契约、影响文件、失败路径和验证命令，再修改。
4. 只改本批范围；发现相邻问题先登记，不顺手扩张。
5. 定向测试通过后再跑本批要求的完整门禁。
6. 汇报真实结果：修改、测试、未执行项、阻塞、残余风险。
7. 未经用户明确授权，不 commit、不 push、不合并、不删除 worktree 或报告文件。

完成标准是可观察结果，不以“代码看起来完成”或“Windows 本地能跑”代替。

## 3. 已核实的现场

执行前必须重新核实，以下只用于帮助定位：

### 3.1 `main` 工作树

- 当前提交为 `683628a`，本地 `main` 领先 `origin/main` 58 个提交。
- 工作树有大量已修改和未跟踪文件，包含业务代码、测试、依赖和权威文档。
- 当前受版本控制的 `.github/workflows/smoke.yml` 只运行 `windows-latest`。
- 现有 workflow 没有三端 matrix、桌面安装包、artifact 上传、macOS 签名或 Linux 打包。
- `heif-bin/` 约 67 MB，含 MSVC `.lib`，当前仍被 Git 跟踪。

### 3.2 `wt/three_gogogo` 工作树

路径：`C:/Users/33887/.dsh/worktrees/chabaosucai-a8ac193e/three_gogogo`

- 分支比 `main` 已提交状态前进 9 个提交，merge-base 是 `683628a`。
- 9 个提交已经覆盖：原生路径、keyring 目标依赖、平台能力入口、Ollama 能力门控、恢复失败
  安全、维护测试隔离、前端路径显示和非 UTF-8 文件名拒绝。
- 该 worktree 同样有大量未提交修改；不能把“9 个已提交改动”和“额外 WIP”混为一批。
- 未提交内容包含 `code-gate.yml`、`release-candidate.yml`、桌面构建脚本、媒体 sidecar 清单、
  `rust-toolchain.toml` 以及更多业务改动。

### 3.3 已知草稿风险

- `code-gate.yml` 与 `release-candidate.yml` 草稿已删除超范围的 Intel Mac，仅保留 Windows x64、
  Apple Silicon macOS 与 Linux x64；Apple Silicon runner 使用 `macos-15`，仍必须由 GitHub 实际
  runner 输出 ARM64 断言，不能仅凭标签名称宣布通过。旧的 `macos-14` 已进入退役窗口，不再作为新门禁标签。
- Linux 系统依赖清单需要同时覆盖 Tauri/WebKit 与 keyring Secret Service，必须以实际 runner
  失败和官方文档为证据补齐。
- `prepare-media-tools.mjs` 的 Windows/Linux URL 已固定到不可变 BtbN release tag，macOS ARM64 已固定到
  Martin Riedl 的 7.1 release URL；三目标归档、最终 sidecar 和许可证 SHA 已填入并经过来源校验。
  Windows sidecar 已在本机通过版本、编码器、许可证和 strict check；Linux/macOS 仍需目标 runner
  执行运行时能力验证。macOS 的 ffmpeg/ffprobe 归档可分开声明，许可证 URL/SHA 也会进入严格门禁。
- 该脚本自带本地 `basename` helper，已通过 `node --check`；同时拒绝占位 URL、占位 SHA 和 `/latest/`
  漂移 URL，并在解压前检查归档成员路径、跳过符号链接，因此维护者填入真实最终 SHA 和确认许可证前，候选包会明确失败。
- 候选 workflow 尚无 macOS 签名/公证；无签名产物只能标记为实验构建。
- `src-tauri/tauri.conf.json` 已加入仓库已有的 `icons/icon.icns`；macOS 实际 DMG 仍须由候选流水线
  和真机验收核对。

### 3.4 2026-09-22 执行结果

- `main` 工作树：文档约束、平台事实源、路径/同源规则、目标凭据依赖、搜索计划 clippy 修复和
  Windows 完整门禁已落地；`pwsh ./scripts/smoke.ps1`、前端门禁、Rust fmt/clippy/test、Windows
  release native build 均通过。
- `wt/three_gogogo`：9 个平台基础提交之上已完成前端同源回归、三目标 code-gate 与候选打包草稿、
  HEIF/sidecar 安全门和 macOS icon；前端全量单测、Rust 全量测试、Windows native build、NSIS
  与 MSI bundle 已通过。MSI 使用仓库内固定的 WiX 936 代码页 locale，解决中文产品名无法写入
  默认 1252 数据库的问题。
- 三端 workflow 尚未在 GitHub runner 实际跑过；macOS/Linux runner 运行时、候选三端安装包和真机
  核心闭环仍是发布阻塞。Windows/Linux/macOS sidecar SHA 已不再是占位值；脚本仍拒绝 `latest`
  URL，并会在目标 runner 上重新验证归档、最终二进制、编码器与许可证。
- 历史 Windows runner 曾在 MSVC 14.51 因 `msvc_stl_shim.cpp` 重复声明 `__std_*` 失败；当前工作树
  已按 `_MSC_VER < 1945` 门控 shim，旧版工具链仍补符号，新版 STL 自动跳过，且本机 `cargo check`
  / clippy / test 已复核通过。该修复仍需随下一次真实 Windows runner 运行确认。
- Rust 工具链已从可漂移的 `stable` 收敛为 `1.98.1`，`rust-toolchain.toml` 与两个 workflow 同步锁定；
  后续升级必须单独变更并重跑三端门禁。
- Windows 主机上的交叉 `cargo check` 不能替代目标 runner：Apple Silicon 被缺少 `cc` 阻断，Linux
  被缺少目标 sysroot/pkg-config（`libdbus-1-dev`）阻断；这两项已确认是环境限制，未放宽 workflow。
- 未 commit、未 merge、未 push；主工作树已有改动保持原状，跨平台代码只在隔离工作树继续演进。
- 审查报告指出的 `src/stores/libraryStore.ts` 全路径小写和反斜杠替换问题已在当前工作树复核并
  修正：仅折叠 stem、保留 Unix 反斜杠文件名语义，并补齐响应式路径展示测试；仍需随目标平台 UAT
  验证真实文件名和同源组行为。

## 4. 目标架构与门禁

### 4.1 代码结构

```text
Cargo target dependencies / cfg
            -> services 平台能力与所属 OS service
            -> commands 薄壳
            -> src/api + platformStore
            -> UI 禁用、降级或原生行为
```

- 业务数据、数据库 schema、搜索语义和状态机三端一致。
- 静态能力不执行网络、数据库、外部进程或大文件 IO。
- 运行时健康由所属服务探测，不混入静态 capability DTO。
- 前端不使用 UA、`navigator.platform` 或散落的操作系统判断。
- capability 未加载或加载失败时保守禁用，不猜 Windows。

### 4.2 合并门禁

每个 PR 的必需检查目标：

- 前端：`npm ci`、lint、typecheck、unit test、build。
- Windows x64：fmt、严格 clippy、all-features test、Tauri/Rust native build、现有 smoke。
- macOS Apple Silicon：fmt、严格 clippy、all-features test、目标 native build、平台测试。
- Ubuntu 24.04 x64：安装系统依赖后执行同类 Rust 门禁和 native build。

默认不允许 `continue-on-error`。确认属于外部基础设施故障时，只能建立有期限和恢复条件的
临时豁免。

### 4.3 候选门禁

- 同一 SHA 生成 Windows 安装包、Apple Silicon DMG、Ubuntu x64 AppImage/DEB。
- 每个 artifact 附版本、SHA、目标 triple、SHA256 和构建日志。
- 自动化完成后状态只是“实验构建”。
- Windows 完整 UAT 与 macOS/Linux 核心五步真机验收完成后，才按平台提升支持状态。

## 5. 批次 0：冻结现场并建立可恢复边界

### 5.1 目标

在不丢失任何用户改动的前提下，把两个脏 worktree 分成可审查的交付单元。此批不改业务行为。

### 5.2 操作

1. 分别记录两个 worktree 的：
   - `git status --short --branch`
   - `git diff --stat`
   - `git diff --name-status`
   - 未跟踪文件清单
   - 当前 HEAD、merge-base 和 `main..wt/three_gogogo` 提交列表
2. 按域把未提交改动分类：
   - 当前 Windows 功能开发
   - 已提交的 9 个平台基础提交
   - 未提交的平台/CI/sidecar WIP
   - 文档与测试
   - 无关或生成产物
3. 检查两个 worktree 修改同一文件的交集，特别是 Cargo、package、平台 service、设置 UI、文档。
4. 提出快照方案。推荐新建独立备份分支和逻辑提交，但执行 branch/commit 前必须得到用户明确授权。
5. 禁止使用 `reset --hard`、`checkout --`、`clean` 或覆盖复制来“整理”。

### 5.3 完成标准

- 每个修改文件都有归属。
- 两个 worktree 的内容都有可恢复方案。
- 尚未获得提交授权时停在报告阶段，不进入批次 1。

## 6. 批次 1：建立干净的集成分支

### 6.1 目标

从 `683628a` 或用户确认的新基线建立 `codex/platform-integration`，先复用 9 个已提交平台改动，
不混入未审查 WIP。

### 6.2 操作

1. 在新的干净 worktree 创建集成分支，保持原两个 worktree 不动。
2. 按原顺序引入并逐个审查：
   - `12f80c3`：原生路径和同源判定
   - `8927a4b`：目标 keyring 后端与凭据三态
   - `4821c88`：统一平台能力入口
   - `f65253a`：Ollama 三端能力门控
   - `ec5e46b`：非托管平台保留历史 local 连接可见性
   - `de0ada5`：恢复库失败路径安全
   - `40e3885`：破坏性维护测试隔离
   - `cccb84a`：前端平台化文件名显示
   - `f645dc4`：拒绝非 UTF-8 路径的有损入库
3. 每个提交检查分层、错误协议、测试和文档一致性；不要因为已有提交信息就跳过代码审查。
4. 如果新基线包含同域修改，采用语义合并并重新测试，不机械 cherry-pick 后接受冲突结果。

### 定向验证

- Rust：`path`、`kinship`、`credentials`、`platform`、恢复故障注入、importer 测试。
- 前端：`platformStore`、路径显示、Settings/LocalModel/AiConnection 组件测试。
- 静态搜索：前端不得新增 `navigator.platform`、UA 或页面直接 `invoke`。

### 6.3 完成标准

- 9 个行为分别可解释、可回退、测试通过。
- 非 Windows 不会调用 Windows Ollama 安装/进程管理命令。
- 未混入批次 0 标记的其他 WIP。

## 7. 批次 2：补齐剩余跨平台正确性缺陷

### 7.1 前端同源键

复核 `src/stores/libraryStore.ts` 的路径处理：

- Unix 不能把反斜杠当分隔符。
- Linux 目录大小写不能全路径折叠。
- 前后端同源键必须有同一行为契约，不能各写一套近似规则。
- 加入 Windows、macOS/Linux 和特殊文件名测试。

优先考虑把共享规则变成后端结果或明确的纯函数契约；不要在组件里新增平台分支。

### 7.2 原生依赖与凭据

- 实际在三个目标运行 keyring 编译与最小探测测试。
- Linux 区分“未配置”和 Secret Service/会话总线不可用。
- macOS 区分钥匙串不可用和凭据不存在。
- 所有凭据 IO 在数据库锁外。

### 7.3 路径和文件操作

- 搜索 `split(/[\\\\/]/)`、`replace('\\\\','/')`、全路径 `toLowerCase()`、`to_string_lossy`。
- 每个命中按数据路径、展示路径或比较键分类；只有 Windows 分隔语义可以同时认 `\\` 和 `/`。
- 检查外部打开、打开所在目录、导入、导出、缩略图、视频代理和备份恢复。

### 7.4 完成标准

- 已知路径缺陷有回归测试。
- 三端核心路径不依赖 Windows 字符串假设。
- 不支持的文件名明确拒绝且原文件不动。

## 8. 批次 3：三端代码门

### 8.1 目标

把三端编译和测试变成 `main` 必需检查。复用 `wt/three_gogogo` 的 `code-gate.yml` 草稿，但按
当前产品范围重审，不直接复制。

### 8.2 操作

1. 只保留 Windows x64、Apple Silicon macOS、Ubuntu 24.04 x64 三个目标。
2. 从 GitHub 官方文档确认 runner 标签、CPU 架构、可用额度和镜像版本；在 workflow 输出
   `rustc -vV`、`uname`/系统信息、Node platform/arch 并断言。
3. Linux 依赖以 Tauri 2、keyring/Secret Service 和实际 linker 错误为依据；不要凭记忆堆包。
4. 前端公共门只跑一次；Rust 平台门按目标分别运行。
5. Windows 保留现有网络 mock 串行重试逻辑，但先证明失败确属已知环境波动；其他失败不重试掩盖。
6. 明确 job 名称，便于在 GitHub branch protection 中设置必需检查。
7. 先用 `workflow_dispatch` 验证，再配置 push/PR；不要在未跑过时声称门禁已经有效。

### 8.3 完成标准

- 三个目标真实 runner 全绿至少一次。
- Unix 专属测试实际执行，不只是交叉 `cargo check`。
- 所有必需 job 失败会使 workflow 失败。
- `docs/TEST_STRATEGY.md` 当前状态从“未接入”更新为真实结果。

## 9. 批次 4：HEIF 与媒体 sidecar 可复现化

### 9.1 HEIF

1. 移除 `.cargo/config.toml` 对仓库根 `heif-bin` 的无条件注入。
2. 为三个目标定义可复验来源：目标专属预编译包或系统依赖；不能让 macOS/Linux 读取 MSVC `.lib`。
3. 对来源记录版本、许可证、URL 和 SHA256；构建失败时输出目标 triple 与准备方法。
4. 在新提交中把 `heif-bin/` 加入 `.gitignore` 并从索引移除，保留本地文件供 Windows 现有开发使用。
5. 不在本批重写 Git 历史。历史清理会改变提交 SHA，必须另行备份并获得用户明确授权。

### 9.2 FFmpeg/FFprobe sidecar

1. 审查 `scripts/prepare-media-tools.mjs`、`scripts/desktop.mjs`、sidecar manifest 和 Tauri config。
2. 修复脚本的实际运行错误，包括 import、路径、解压、防 Zip Slip、Unix 可执行位和 Windows shell。
3. 所有下载固定版本和 SHA256；占位值必须让发布失败，不能自动抓 `latest`。
4. 每个目标验证必需编码器、版本和许可证材料。
5. 原生大二进制不提交 Git；通过缓存或候选 workflow 准备，并上传构建 provenance。

### 9.3 完成标准

- 三端构建不共享错误 ABI 的本机库。
- 候选打包所用原生二进制可追溯、可校验、许可可复核。
- 缺失依赖时快速失败并给出可执行错误，不在运行时才暴露。

## 10. 批次 5：候选安装包流水线

### 10.1 目标

从同一 SHA 生成三个目标的实验安装包。复用 `release-candidate.yml` 草稿，删除 Intel Mac 并修正
未经验证的假设。

### 10.2 操作

1. 首轮只允许手动 `workflow_dispatch`，验证稳定后再决定是否响应候选标签。
2. Windows 输出当前 Tauri 支持的安装包；macOS 只收集 DMG；Ubuntu 收集 AppImage 与 DEB。
3. 每目标生成 manifest：版本、commit、target、runner OS/arch、安装包列表、大小和 SHA256。
4. 汇总 job 必须检查三个目标内容与 SHA 一致；缺任何安装包时整体失败。
5. 检查 macOS icon、bundle identifier、最低系统版本和 Gatekeeper 行为。
6. 第一阶段不伪造签名/公证完成状态；未签名 DMG 明确标注实验构建。
7. 不自动创建公开 Release，不自动覆盖同名 artifact。

### 10.3 完成标准

- 三端 artifact 来自同一 SHA，文件与校验值可下载复核。
- workflow 失败不会留下“候选已就绪”的误导性汇总。
- 安装包还未完成真机验收时只标实验构建。

## 11. 批次 6：目标真机核心验收

### 11.1 准备

- 建立体积受控、无隐私、许可明确的 S1 小样本。
- 大型 RAW/HEIC/视频包置于仓库外，记录版本、文件清单和 SHA256。
- 找到至少一位 Apple Silicon macOS 测试者和一位 Ubuntu 24.04 x64 测试者。

### 11.2 执行

按 `docs/QA_PLAYBOOK.md` 2.5 执行：

1. 安装与启动。
2. 导入。
3. 浏览与搜索。
4. 查看图片与短视频。
5. 复制导出并核对。
6. 检查平台专属入口不会误调用。

反馈必须包含版本、SHA、系统、架构、素材包版本、失败步骤、预期/实际和脱敏诊断包。

### 11.3 完成标准

- Windows 完整 UAT 有结果。
- macOS/Linux X-01~X-06 有逐项证据。
- P0 为 0；核心闭环失败全部修复并重新生成候选。
- 每个平台按实际证据标记正式、预览或实验，不能统一写“已支持”。

## 12. 批次 7：最小桌面 E2E（后置）

只有人工五步脚本稳定、选择器和测试数据固定后再开始：

- 自动化一条黄金路径，不追求一次覆盖全部页面。
- E2E 必须启动真实 Tauri 壳，不能把 jsdom 组件测试称为桌面 E2E。
- 先在一个平台稳定，再评估三端 runner 的窗口会话和驱动成本。
- E2E 失败不得替代 Rust/前端单测定位职责。

本批不阻塞首个实验候选，但应进入后续里程碑。

## 13. 每批报告模板

```markdown
## 批次 <编号> 结果

### 范围
- 本批目标：
- 实际修改：
- 明确未改：

### 平台影响
- Windows：
- macOS：
- Ubuntu：

### 验证
- `<命令>`：通过 / 失败 / 未执行

### 工作树
- 修改文件：
- 用户原有修改是否保持：
- 是否 commit/push：否，除非用户明确授权

### 阻塞与风险
- 阻塞：
- 残余风险：
- 下一批建议：
```

## 14. 停止条件

遇到以下情况立即停下并向用户报告，不自行扩大权限：

- 无法判断某项未提交修改属于哪个工作流或作者。
- 需要 reset、clean、覆盖复制、历史重写或强推。
- 需要创建提交、合并分支或推送，但用户尚未授权。
- GitHub runner 架构或额度不满足 Apple Silicon 目标。
- 原生二进制没有可验证来源、SHA256 或许可证。
- 需要 Apple 签名身份、notary 凭据或其他秘密。
- 文档契约与当前产品决策冲突，且无法从权威来源判断。
- 为了让 CI 变绿必须删除测试、放宽断言、跳过平台或设置永久 `continue-on-error`。

## 15. 给 Luna 的首轮提示词

```text
阅读 AGENTS.md、docs/README.md、docs/PLATFORM.md、docs/ARCHITECTURE.md、
docs/DEVELOPMENT.md，以及根目录 PLATFORM_AI_IMPLEMENTATION_PLAN.md。

只执行计划中的“批次 0：冻结现场并建立可恢复边界”。不要修改业务代码，不要 commit、push、
merge、reset、checkout 或 clean。分别审计 main 与 wt/three_gogogo 两个 worktree，把全部修改按
领域归类，列出重叠文件和推荐的可恢复快照方案。完成后按计划第 13 节模板汇报并停下，等待确认。
```
