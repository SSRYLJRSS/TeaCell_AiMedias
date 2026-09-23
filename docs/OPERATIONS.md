# 运维与交接手册

> 更新日期：2026-09-22
>
> 本文档覆盖本地运行、数据位置、发布、备份恢复、本地模型和应急处理。性能细节见 [PERFORMANCE.md](PERFORMANCE.md)，故障症状见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)。

## 1. 快速认识项目

茶包素材是 Tauri 桌面应用：

- React 前端负责页面、状态和交互。
- Rust 后端负责文件、SQLite、图像/视频和网络。
- 素材库、标签和索引默认保存在本机。
- 云端 AI 只在用户触发时发送选中素材的预览。

当前 v1.0.1 的主要发布阻塞是真实素材 UAT 和发布证据，不等于代码未实现。

## 2. 本地启动

环境：

- Node.js 22（`>=22 <25`）
- Rust stable MSVC
- Visual Studio C++ Build Tools
- 可选 `ffmpeg` / `ffprobe`

```powershell
npm install
npm run desktop:dev
```

FFmpeg 说明：发布包自带随平台打包的 ffmpeg / ffprobe sidecar，视频元数据和抽帧优先用它；无 sidecar 时仅开发模式回退到 PATH 上的系统 `ffmpeg` / `ffprobe`。sidecar 放置与命名见 [DEVELOPMENT.md](DEVELOPMENT.md#21-桌面构建入口)。

Windows 本地构建（其他目标替换为对应 target triple）：

```powershell
npm run prepare-media-tools -- --target x86_64-pc-windows-msvc
npm run desktop:build -- --target x86_64-pc-windows-msvc
```

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

各平台 `data_dir` 差异（表中 `%APPDATA%` 为 Windows 口径，运行日志统一在 `data_dir/logs`）：

| 平台 | 应用数据目录 |
|---|---|
| Windows | `%APPDATA%\bagertea_ai_media_v2\` |
| macOS | `~/Library/Application Support/bagertea_ai_media_v2/` |
| Linux | `~/.local/share/bagertea_ai_media_v2/`（遵循 `XDG_DATA_HOME`） |

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

### 5.2 构建产物

默认输出：

```text
src-tauri/target/release/bundle/
```

各平台安装包格式：

| 平台 | 安装包 |
|---|---|
| Windows | NSIS（`.exe`）/ MSI |
| macOS | `.app` / `.dmg` |
| Linux | AppImage / `.deb` |

三目标构建入口见 [DEVELOPMENT.md](DEVELOPMENT.md#21-桌面构建入口)。不要提交 `target*/`、`dist/` 或安装包。

FFmpeg sidecar 发布流程：`src-tauri/binaries/manifest.json` 按三目标记录固定版本、归档 URL、归档与二进制 SHA256、必需编码器（Windows/macOS H.264/AAC，Linux VP8/WebM）、许可证。Windows/Linux 归档固定到 BtbN 的不可变 release tag，macOS ARM64 固定到 Martin Riedl 的 7.1 release URL；macOS 可通过 `ffprobeArchive*` 使用独立的 ffprobe 归档。`npm run prepare-media-tools -- --target <triple>` 下载并校验（任一 SHA256 为占位哨兵时拒绝执行）、解压前检查归档成员路径、防 Zip Slip/符号链接、设置可执行位、运行版本与编码器探测，并复制/下载许可证到 `binaries/licenses/<triple>/`。`tauri.sidecar.conf.json` 将许可证目录作为 bundle resource；候选包 workflow 还会把它放入 `third-party-licenses/` 并写入 per-target manifest。Windows/Linux 的最终 sidecar SHA 已从固定归档实测并写入 manifest；Windows 已在本机通过版本、编码器、许可证、strict check 以及 NSIS/MSI bundle，MSI 的 WiX locale 固定为 936 代码页以支持中文产品名；Linux 的运行时可执行性/编码器仍需 Linux runner 复核。候选包工作流（`release-candidate.yml`）在每个 runner 先准备 sidecar，再 `desktop.mjs build`（含 `--strict-release` 校验），产出 per-target `artifact-manifest.json`（目标/版本/commit/安装包名+大小+SHA256/ffmpeg 版本+许可证 SHA256）与 `SHA256SUMS.txt`（只对安装包）；汇总 job 校验三目标 manifest 的目标/版本/commit 与本次 SHA 一致且各含安装包，任一 bundle 失败则候选版本不就绪。日志 sidecar 版本/来源/hash/许可证见构建日志与各 artifact-manifest。

### 5.3 回滚

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

1. 校验备份文件。
2. 拒绝存在运行中导入、导出或 AI 批次时恢复。
3. 保留现有库为 `library.db.old`。
4. 用备份替换主库。
5. 重启并执行迁移和自检。

恢复失败时不要删除 `.old` 文件。

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

托管 Ollama（安装、进程、模型管理）仅在 Windows 提供。macOS / Linux 不管理本机 Ollama：用户自行部署，或连接局域网/在线的 OpenAI 兼容地址，在设置页 AI 连接表单填写后使用（见第 8 节）。

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
- macOS / Linux 不管理本机 Ollama，用户自部署或填局域网/在线的 OpenAI 兼容地址；地址、模型名和凭据都在 AI 连接表单填写。
- API Key 存系统凭据库，不写设置 JSON。
- 连接档案可以绑定到不同用途。
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
