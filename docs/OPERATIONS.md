# 运维与交接手册

> 更新日期：2026-09-23
>
> 本文档覆盖本地运行、数据位置、发布、备份恢复、本地模型和应急处理。性能细节见 [PERFORMANCE.md](PERFORMANCE.md)，故障症状见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)。

## 1. 快速认识项目

茶馆是 Tauri 桌面应用：

- React 前端负责页面、状态和交互。
- Rust 后端负责文件、SQLite、图像/视频和网络。
- 素材库、标签和索引默认保存在本机。
- 云端 AI 只在用户触发时发送选中素材的预览。

当前发布阻塞和验收状态见 [项目计划 §2.2](PROJECT_PLAN.md#22-当前发布阻塞)，不等于代码未实现。

## 2. 本地启动

环境：

- Node.js 22–24
- Rust 1.98.1（仓库根目录 toolchain 固定）
- Visual Studio C++ Build Tools
- 开发环境可选安装 `ffmpeg` / `ffprobe` 并配置 `PATH`；发布构建不使用系统 `PATH`，只使用经严格检查的包内 sidecar

```powershell
npm install
npm run desktop:dev
```

桌面入口会从 [固定 HEIF manifest](../src-tauri/native/heif-manifest.json) 准备当前目标静态库，并校验归档、头文件版本和许可证摘要。直接运行 Cargo 测试时，需先运行 `npm run prepare-heif-libraries -- --target <当前目标>`，再将 `HEIF_BINARIES_DIR` 指向 `src-tauri/native/heif/<当前目标>`。

构建：

```powershell
npm run prepare-media-tools -- --target x86_64-pc-windows-msvc
npm run desktop:check:strict -- --target x86_64-pc-windows-msvc
npm run desktop:build -- --target x86_64-pc-windows-msvc
```

Windows NSIS 安装器包含英文与简体中文界面并允许选择语言；MSI 分别构建 `en-US` 与 `zh-CN` 版本。WiX locale 文件使用 Windows-936 code page，以保留中英文产品名称；不要移除 `src-tauri/wix/*.wxl` 的 locale 覆盖或把 `TauriCodepage` 改回 1252。

打包产品名和新安装默认目录使用 PRD 定义的英文全名，中文 UI 短名保持不变。MSI 固定沿用旧中文产品名对应的 UpgradeCode `30bc1910-3467-5659-bcff-284af99c9896`，禁止因改名重新生成。NSIS 通过 `nsis/installer-hooks.nsh` 在当前安装用户上下文检查旧中文安装：验证发布者及卸载器、明确确认后正常卸载旧应用文件，再安装新包；无法安全识别或卸载时中止，不删除用户数据。首次从旧中文 NSIS 产品身份迁移须交互安装，静默迁移拒绝执行；改名后的后续升级仍走 Tauri 原有更新路径。安装目录可以含中文用户名、用户自选中文目录或空格，不能靠禁止中文路径掩盖编码问题。
应用 identifier 与下文的数据根目录保持兼容，不随产品展示名变化。实际安装与旧版升级仍需按 [QA_PLAYBOOK.md](QA_PLAYBOOK.md) 验收，配置正确不代表已完成安装验收。

正式交付前：

```powershell
node --version
npm --version
rustc --version
cargo --version
npm run lint
npm run typecheck
npm run test:unit
npm run build
pwsh ./scripts/smoke.ps1
```

## 3. 数据与目录

默认应用数据目录：

```text
%APPDATA%\bagertea_ai_media_v2\
```

| 内容 | 默认位置 |
|---|---|
| SQLite 数据库 | `%APPDATA%\bagertea_ai_media_v2\library.db` |
| 占位/高清缩略图 | `thumbnails/` |
| 待入库/查看预览 | `previews/` |
| 视频兼容代理 | `proxies/` |
| 运行日志 | `logs/app.log.*`、`logs/fatal.log`、`logs/panic.log`（存在时） |
| Ollama 安装器临时目录 | `ollama/` |
| 备份文件 | 由用户选择目标位置 |
| 素材总库 | 设置页配置，分库位于总库子目录 |

设置页可打开数据目录、日志目录和缓存管理。不要直接删除数据库或缓存目录来“修复”问题，先备份并查看日志。

## 4. 日志与诊断

- 日志按天滚动，保留 30 个文件；启动后清理过期日志，并把 `app.log*` 总量压到 50 MB
  以内（始终保留最新文件）。Release 构建没有控制台，文件日志是主要证据。
- 默认级别是 `info`。设置页「关于 → 诊断日志级别」可切换 `info`、`debug`、`trace`，
  保存后立即生效；排查完成后应切回 `info`，避免日志体积和 IO 持续放大。
- 前端异常、未处理 Promise rejection 和 IPC 关键失败会回传 Rust；回传自身失败不会改变业务结果。
  前端日志带 `sessionId` / `sequence`，IPC 失败日志带 `requestId` / `command` / `durationMs`，
  排查时优先用这些字段串联同一页面会话或同一次命令调用。
- `fatal.log` 记录启动致命错误和 panic 的同步兜底行；非阻塞文件日志未刷盘时优先看它。
- 排查问题时记录：时间、操作、素材 ID、批次 ID、错误信息、命令名、日志文件。
- 设置页「导出诊断包」生成 ZIP，包含 `diagnostics.json` 和日志文件。摘要仅包含版本、OS/架构、
  schema 版本、素材/回收站/标签数量、日志级别和最近 20 条 AI 批次状态。日志收集总量上限
  20 MB、单文件上限 4 MB；超限时只保留最新尾部，摘要中的 `truncatedLogs` 会列出被截断文件。
- 诊断包不包含 API Key、Authorization、用户提示词、完整请求体、素材内容或数据库原文件。
  可以发送给支持人员前先按组织数据规范复核。

常见日志关键词：

- `数据库升级失败`
- `导入扫描警告`
- `frontend`
- `panic`
- `缩略图`
- `AI`
- `Ollama`
- `export`
- `migration`

## 5. 发布流程

### 5.1 发布前

1. 确认 `AGENTS.md`、`PRD.md`、`ARCHITECTURE.md` 和契约无漂移。
2. 更新版本号：
   - `package.json`
   - `src-tauri/Cargo.toml`
   - `src-tauri/tauri.conf.json`
3. 运行所有自动化门禁。
4. 运行 [QA_PLAYBOOK.md](QA_PLAYBOOK.md) 的 P0 场景。
5. 用真实素材验证搜索、导入、查看器、AI、导出、回收站、备份恢复。
6. 检查数据库从上一版本升级。
7. 检查安装包和 License。

### 5.2 三端候选构建

一般本地安装包构建必须在目标原生系统上进行，并显式提供 target：

```powershell
# 以 Windows x64 为例；macOS/Linux 应在对应目标机或受支持 runner 上执行
npm ci
npm run prepare-media-tools -- --target x86_64-pc-windows-msvc
npm run prepare-heif-libraries -- --target x86_64-pc-windows-msvc
npm run desktop:check:strict -- --target x86_64-pc-windows-msvc
npm run desktop:build -- --target x86_64-pc-windows-msvc
```

手动候选工作流 `.github/workflows/release-candidate.yml` 从同一个 workflow commit 并行生成三端
安装包，并附 `build-manifest.json`、runner OS/架构、每个安装包的大小与 SHA256、整体 SHA256 清单、FFmpeg 与 HEIF 许可证文本及其固定来源。验证 job 检查目标、版本、提交、必需包类型、runner 架构和摘要；artifact 只保留 7 天，不会发布到 Releases。macOS 包目前没有签名/公证流程。

**分发许可门禁**：HEIF 依赖包含静态链接的 LGPL 组件和 GPL 许可的 x265。许可证文本、版本、来源和 SHA256 不能代替对应二进制的源码、构建参数、许可义务和再分发条件审查。在这些材料被项目负责人独立审查确认之前，不得把 artifact 交给测试者，也不得公开分发。生成 artifact 本身不是发布授权。其他当前发布状态见 [项目计划 §2.2](PROJECT_PLAN.md#22-当前发布阻塞)。

### 5.3 构建产物

三端目标输出目录：

```text
src-tauri/target/<target-triple>/release/bundle/
```

不要提交 `target*/`、`dist/` 或安装包。

### 5.4 回滚

应用回滚不等于数据库自动回滚。发布前必须明确：

- 上一版本安装包。
- 数据库升级是否向后兼容。
- 是否需要在升级前备份。
- 失败时如何恢复旧安装包和旧数据库。

如果新版本迁移不可逆，发布说明必须提示升级前备份。

## 6. 数据库备份与恢复

### 6.1 备份

应用内备份使用 SQLite `VACUUM INTO`：

- 单文件快照，不复制正在写入的 WAL。
- 备份前确认当前无不可中断的写操作。
- 建议在重大导入、标签治理和版本升级前创建备份。

### 6.2 恢复

恢复流程：

1. 校验备份文件，在当前主库仍可用时复制到数据目录同卷的唯一暂存文件，并再次校验副本。
2. 拒绝正在运行的导入、媒体回填、导出、AI 批次或待处理 AI 批次；取得独占数据库生命周期门后再次检查，避免校验与执行间隙启动任务。
3. 若 `library.db.old` 已存在，恢复会停止并保留该文件；必须由操作者确认其来源、另行安全归档后，才能重试。应用不会覆盖或自动删除它。
4. 保留现库为 `library.db.old`，再把校验过的暂存副本原子移动为 `library.db`，执行迁移和启动自检。切换时其他数据库命令等待，但连接 mutex 不跨复制、重命名或迁移文件 IO。
5. 成功后重启应用并加载新库。成功恢复前的库仍保留为 `.old`；后续恢复若仍占用该名称会被拒绝。

恢复失败时不要删除或覆盖 `.old`、失败恢复副本或暂存副本。可回滚时应用从 `.old` 复制回 `library.db` 并重新打开；无法证明数据库连接有效时应用会封锁后续数据库命令并明确报告保留路径。若 `library.db` 缺失，启动只会在 `.old` 校验通过后复制恢复，绝不会悄悄创建空库，也不会移除 `.old`。此时不要手工新建同名空数据库。

### 6.3 重置数据

设置页“重置数据”按勾选项分类清理，数据库变更是单事务；原始文件先在数据库锁外逐项处理，缓存和日志按结果清理。

- “标签与分类”会删除全部标签、素材标签关联、别名、词条、打标流水、自建分面和数值分面值，然后重建系统分面；不会删除素材原文件或素材记录。
- “素材库记录”会删除素材记录、搜索索引、关联导出任务和派生缓存，不会删除磁盘上的素材原文件；导出任务历史也可单独清理。
- “原始素材文件”会扫描在库与回收站中的全部素材，逐项永久删除磁盘文件；成功项同步删除素材记录，删除失败项保留记录并在结果中报告。该项必须单独确认。
- “导出任务记录”“搜索条件与界面草稿”“诊断日志”可以独立清理；全选代表恢复出厂设置。Ollama 模型和安装包由外部服务/缓存管理，不随全选删除。
- 重置期间会拒绝入库、媒体回填、导出和 AI 批次等运行中任务，避免与重置事务互相覆盖。若勾选“AI 打标任务”，允许直接删除遗留的待处理批次；仅重置其他数据时仍会阻止这类批次存在，避免留下不一致的任务状态。
- 重置成功后前端会清空旧标签树、失效的标签筛选和素材选中状态并重新拉取；不应把旧界面缓存误判为数据库仍有残留。

## 7. 本地模型

### 7.1 当前能力

设置页“本地模型”支持：

- 检测 Ollama 是否安装和运行。
- 应用内下载、安装和启动 Ollama。
- 探测 NVIDIA 显存并推荐模型。
- 拉取模型并写回当前本地 AI 档案。
- 查看已安装模型和模型目录。
- 切换下载源和测速。

### 7.2 推荐档位

| 显存 | 推荐 |
|---|---|
| 12GB 及以上 | `qwen3.5:9b` |
| 6GB 至 12GB | `qwen3.5:4b` |
| 6GB 以下或未知 | `qwen3.5:2b`，明确提示可能走 CPU |

`qwen3.5` 是当前 Ollama registry 的统一多模态模型系列：0.8B 约 1.0GB、2B 约 2.7GB、4B 约 3.4GB、9B 约 6.6GB，均支持图片输入（详见 [Ollama 模型页](https://ollama.com/library/qwen3.5)）。模型名称以 Ollama registry 实际可用 tag 为准；拉取前先检测已安装模型，避免重复下载。8GB 显存优先选择 4B，9B 仅作为显存允许时的备选。

### 7.3 故障判断

- 检测不到：Ollama 未安装、未启动或不在默认路径。
- 已安装未就绪：服务尚未启动，轮询 `/api/version`。
- 拉取失败：网络、registry 或磁盘空间问题；Ollama 支持断点续传。
- 打标失败：确认模型支持视觉输入、请求模型名正确、服务可访问。
- 安装被杀毒软件拦截：允许安装器或手动运行保留的安装包。

## 8. AI 连接与凭据

- 支持 OpenAI 兼容接口和 Anthropic 模式。
- API Key 存系统凭据库，不写设置 JSON。
- 连接档案可以绑定到不同用途。
- 运行时密钥读取发生在数据库 guard 释放后；系统密钥服务不可用时会明确报错，不会当成“尚未配置”。更新/删除失败会尽量恢复原密钥；若补偿也失败，必须保留错误信息并停止后续操作。
- 设置页重置“AI 服务配置”时，若任一凭据不可读/不可删会中止连接表清理；数据库重置失败则会尝试恢复已删除凭据。
- 修改连接后应验证打标和超级搜索都读取同一绑定。
- 日志和截图中不得暴露完整凭据。

## 9. 应急处理

| 状况 | 处理 |
|---|---|
| 应用启动显示数据库升级失败 | 保留数据库和日志，不要反复启动覆盖；用备份恢复或收集日志定位 |
| 配置错乱 | 先备份 `library.db`，再检查 settings；不要只删单行 JSON 后继续使用 |
| 缩略图全黑/加载不出 | 查 imaging 日志、缓存目录和数据库锁，参考 [TROUBLESHOOTING.md](TROUBLESHOOTING.md) |
| AI 打标全部失败 | 检查连接、模型视觉能力、额度、网络、服务和日志 |
| 导出后库记录异常 | 立即停止继续 move，检查目标文件和数据库路径，保留日志 |
| 误删且已入回收站 | 停止清理操作，在回收站恢复 |
| 数据库损坏 | 使用应用内备份恢复；不要用普通 SQLite 客户端随手 UPDATE/DELETE |
| 磁盘接近满 | 清理缩略图/代理缓存和 Ollama 安装器，不要直接删素材总库 |

## 10. 交接清单

- 能启动应用并定位数据目录。
- 能运行自动化门禁和 smoke。
- 理解 commands/services/db 分层。
- 知道搜索和分面契约的位置。
- 能执行备份、恢复和日志收集。
- 知道当前发布阻塞和未完成 UAT。
- 知道发布安装包的构建和回滚方式。
- 知道对外分发前需要复核第三方许可证。

## 11. 安全与隐私

- 本地素材默认不出本机。
- 云端 AI 请求会发送用户选择的素材预览，界面和文档必须明确这一点。
- 不把 API Key、token、用户素材路径或数据库上传到公共 Issue。
- 路径操作防目录穿越，SQL 参数绑定。
- 对外发布前复核 RAW/HEIC 等依赖的许可。
