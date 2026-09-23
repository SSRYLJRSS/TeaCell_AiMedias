# 运维与交接手册

> 更新日期：2026-09-23
>
> 本文档覆盖本地运行、数据位置、发布、备份恢复、本地模型和应急处理。性能细节见 [PERFORMANCE.md](PERFORMANCE.md)，故障症状见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)。

## 1. 快速认识项目

茶包素材是 Tauri 桌面应用：

- React 前端负责页面、状态和交互。
- Rust 后端负责文件、SQLite、图像/视频和网络。
- 素材库、标签和索引默认保存在本机。
- 云端 AI 只在用户触发时发送选中素材的预览。

当前公测版 1.0（技术版本 1.0.0）的主要发布阻塞是真实素材 UAT 和发布证据，不等于代码未实现。

## 2. 本地启动

环境：

- Node.js 22（`>=22 <25`）
- Rust stable MSVC
- Visual Studio C++ Build Tools
- 开发时可选 `ffmpeg` / `ffprobe`（PATH 回退）；发布包必须自带经 manifest 校验的 sidecar

```powershell
npm install
npm run desktop:dev
```

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
8. 从同一提交生成 Windows、macOS 和 Ubuntu 候选包；支持目标与等级见
   [PLATFORM.md](PLATFORM.md)。
9. Windows 执行完整 UAT，macOS/Linux 执行 [QA_PLAYBOOK.md](QA_PLAYBOOK.md) 的核心五步闭环。
10. 任一目标平台核心闭环失败时修复并生成新候选，不在旧候选上替换二进制。

### 5.2 构建产物

默认输出：

```text
src-tauri/target/release/bundle/
```

不要提交 `target*/`、`dist/` 或安装包。

第一阶段候选产物限定为：Windows 当前 Tauri 安装包、Apple Silicon macOS DMG、Ubuntu 24.04
x64 的 AppImage 和 DEB。构建 artifact 必须携带版本、提交 SHA 和目标三元组，不能只用
`latest` 命名。

候选构建统一从仓库入口执行：

```powershell
node scripts/desktop.mjs check --target x86_64-pc-windows-msvc --strict-release
node scripts/desktop.mjs build --target x86_64-pc-windows-msvc
```

macOS 使用 `aarch64-apple-darwin`，Ubuntu 使用 `x86_64-unknown-linux-gnu`。发布前必须先为目标
准备 `src-tauri/binaries/manifest.json` 所列的固定版本 sidecar；占位 SHA、`latest` URL、缺失
许可证或缺少 Unix 可执行位都会让严格检查失败。CI 候选工作流为
`.github/workflows/release-candidate.yml`，只在三个目标安装包和同一提交校验均齐全时报告
“构建材料齐全”。许可证文本会随安装包资源和候选 artifact 一并收集。

当前固定的 FFmpeg 构建包含 GPL 编码器（例如 `libx264`）。许可证文本本身不等于完整的再分发合规证据：
在把候选包交给任何测试者前，必须针对实际二进制及其链接组件确认对应源代码、构建脚本/配置和提供方式，
并由负责人完成许可证审查；若无法提供这些材料，应更换为已核实的许可构建。FFmpeg 对 GPL 组件及其
许可证检查项的说明见[官方法律说明](https://ffmpeg.org/legal.html)。候选构建通过、安装包齐全或测试者已知情，
都不能替代这一检查。当前自动工作流会验证许可证文本随包和摘要一致，但不会代替源代码完整性或法律判断。

### 5.3 候选状态与发布

- 自动化构建通过但没有当前版本真机证据：标记为“实验构建”。
- 具有 GPL FFmpeg sidecar 的候选包还必须完成对应源代码材料和再分发方式审查；在此之前不得发给测试者。
- 核心五步闭环在目标真机通过：该平台可标记为“预览支持”。
- Windows 只有在完整 UAT 与发布门禁通过后才标记为“正式支持”。
- 三端可以不同时间向测试者开放，但同一版本的公开包必须来自同一最终标签。
- 未签名、未公证的 macOS 包只交给明确知情的测试者；进入更正式的预览分发前完成签名和公证。
- 第一阶段按需发版，不设置固定周期。

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

设置页“本地模型”支持：

- 检测 Ollama 是否安装和运行。
- 应用内下载、安装和启动 Ollama。
- 探测 NVIDIA 显存并推荐模型。
- 拉取模型并同步当前本机 AI 连接。
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
- `qwen3.5` 等思考模型用于打标时由应用关闭独立 thinking 输出，避免思考 token 吃满预算后只返回空 `content`；若日志仍显示 `done_reason=length` 和空最终内容，先升级 Ollama，再重试未完成项。
- 安装被杀毒软件拦截：允许安装器或手动运行保留的安装包。

### 7.4 服务生命周期

- 设置页的“启动服务并复检”只负责发起启动；Windows 首次启动可能较慢，页面会持续检查，最多等待 90 秒。
- AI 打标、超级搜索和模型拉取开始前都会再次保证默认 `localhost:11434` 就绪，因此服务在页面后台被回收后也能自动恢复。
- 应用自己启动的 Ollama 连续空闲 2 分钟后自动关闭进程；这与 Ollama 的模型 `keep_alive` 不同，前者回收服务进程，后者只释放模型内存。
- 应用不会停止外部已运行的服务。本地打标只保留 Windows 默认 Ollama；LM Studio、自定义 Ollama 端口、局域网兼容服务以及任何 `localhost` 地址都应在“在线服务”中配置，并按外部 OpenAI 兼容接口处理。

## 8. AI 连接与凭据

- 支持 OpenAI 兼容接口和 Anthropic 模式。
- “在线服务”表示外部 API 链路，不要求服务一定在公网；localhost 和局域网地址也归入此处，OpenAI 兼容服务可不填写 API Key。
- API Key 存系统凭据库，不写设置 JSON。
- 连接档案可以绑定到不同用途。
- 在线服务编辑页可设置最大并发、每分钟请求数和每小时请求数；留空或填 0 表示不限。
  限制按打标和 AI 搜索的实际请求计数，普通失败重试、结构化降级和校验修复也会计入；达到本地限制时应用会等待，取消批次可中断等待。
- 服务返回 HTTP 429/额度限流时，应用不会再次自动重试，也不会把当前素材误判为内容失败；会记录脱敏后的服务商错误并中断批次，前端可直接看到原因，调整 RPM/并发后再继续。
- 该限流只作用于外部在线服务。Windows 应用托管的默认 Ollama 不需要配置；其他 localhost、局域网、LM Studio
  或自定义 Ollama 地址属于在线服务，应在在线页设置供应商给出的额度。
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
