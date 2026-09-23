# 排障手册

> 更新日期：2026-09-16
>
> 本文档记录高价值故障模式。先取证，再修改；不要根据症状直接删除数据库或缓存。

## 1. 通用排查顺序

1. 记录时间、操作、素材 ID、批次 ID、命令名、错误提示，以及前端提供的 `requestId` /
   `sessionId`（如有）。
2. 收集 `%APPDATA%\bagertea_ai_media_v2\logs\` 中的日志；优先查看 `fatal.log`，
   再查 `app.log.*`。
3. 判断是前端状态、IPC、服务层、数据库还是文件系统问题。
4. 对状态机问题画出实际转移。
5. 对性能问题先确认构建类型和缓存状态。
6. 修复后增加最小回归测试。

外部 SQLite 工具连接应用库时可能缺少自注册的 `cjk_bigram` 函数。外部连接只适合 SELECT 和诊断，禁止直接 UPDATE/DELETE 业务数据。

## 2. 启动与数据库

### 2.1 启动时提示数据库升级失败

可能原因：

- 旧库处于半迁移状态。
- 手工修改导致 schema capability 与实际结构漂移。
- 数据库文件损坏或磁盘不可写。

处理：

1. 保留 `library.db` 和日志，不要反复覆盖。
2. 检查是否有备份。
3. 如果迁移代码已修复，重新启动触发幂等迁移。
4. 无法自动恢复时使用应用内备份恢复。
5. 不要把 `user_version` 手改为更高值来跳过迁移。

### 2.2 查询报 `no such function: cjk_bigram`

原因：`cjk_bigram` 在应用启动时注册，外部 SQLite 客户端没有该函数。

处理：

- 外部连接只读取简单表或做人工取证。
- 清数据、删除、迁移必须通过应用内功能。

### 2.3 设置看似丢失

设置表只有一个业务 key：`app_settings`，值是完整 JSON。

处理：

- 不按旧字段名查表。
- 检查 normalize 迁移是否成功。
- 先备份数据库，再确认是否存在旧配置和新配置同时读取。

### 2.4 应用无窗口退出或启动日志缺失

检查：

1. `logs/fatal.log` 是否有 `database_init_failed` 或 panic 摘要。
2. `logs/app.log.*` 的最后一条启动阶段记录。
3. 数据目录和 `logs/` 是否可写。
4. 是否存在旧进程占用数据库或非阻塞 writer 尚未刷盘的边界场景。

启动阶段不要只依赖异步文件日志；致命错误必须有同步落盘证据。若诊断包可用，导出后一并提供
`diagnostics.json` 和日志文件，但不要手工加入 API Key 或完整请求体。

## 3. 图片与缩略图

### 3.1 大图加载不出或占位框不动

常见根因：

- 解码期间持有数据库锁，其他请求被饿死。
- RAW/HEIC 解码失败。
- 占位层错误进入昂贵真解码。
- 缓存路径不可写。

处理：

1. 查 imaging 和缩略图日志；占位图失败会记录降级并尝试通用占位图。
2. 确认数据库锁外解码。
3. 确认占位层只走快速预览。
4. 高清层再进入 HEIC/RAW 专用解码。

### 3.2 缩略图极慢

历史三真凶：

1. TIFF IFD 偏移按文件头解析，导致内嵌预览找不到。
2. dev 构建没有为关键依赖启用优化。
3. 解码无限并发或串行等待错误。

当前规则：

- TIFF 偏移相对 TIFF 基准。
- 关键图像依赖保持 dev O3。
- 并发有界，数据库锁外执行。

### 3.3 cut_jpeg 误杀内嵌图

某些相机预览尾部有填充字节。JPEG 裁剪需要在合理范围内寻找 SOI/EOI，而不是要求文件起始和末尾严格对齐。

### 3.4 JPG 被误抓成主图

对普通 JPG 做 FFD8 标记扫描可能扫到主图本身。`.jpg` 禁止该兜底，只对 RAW/容器格式使用。

### 3.5 某格式黑图

排查链：

1. 扩展名是否在 `mime.rs` 白名单。
2. 占位层是否有可用内嵌预览。
3. 高清层是否进入正确专用解码器。
4. 文件是否超过大小或像素保护上限。
5. 用 format matrix 和 `perf_probe` 复现。

RAW 没有内嵌预览时，占位层黑图可能是设计结果；高清层应走真解码兜底。

导入页现在会在正式入库前实际尝试生成快速缩略图。能提取 RAW 内嵌预览的文件会标记为“可能受限”并允许导入；连快速缩略图都无法生成的文件会被拦截，需从队列剔除后才能继续。这里的“无法解析”表示当前解码链路不支持或文件损坏，不表示版权保护。

### 3.6 RAW EXIF 缺失

CR3、RW2 等容器不一定能被通用 EXIF 库完整识别。实现会使用 RAW 元数据解析作为补充，只填缺失字段，不覆盖已有 EXIF。

### 3.7 HEIC 构建失败

检查：

- 先运行 `node scripts/desktop.mjs check --target <target-triple>`，确认实际目标和依赖来源。
- Windows x64 可使用本地 `heif-bin/` 兼容布局；macOS/Linux 不得读取该目录中的 MSVC `.lib`。
- 若使用目标专属预编译库，目录必须是 `src-tauri/native/heif/<target-triple>/{include,lib}`。
- `.cargo/config.toml` 不应无条件注入 Windows 的 HEIF 路径；统一由 `scripts/desktop.mjs` 按目标导出。
- LLVM/libclang 是否可用。
- MSVC STL 版本与 shim 是否匹配。
- 网络下载失败时是否使用了完整、未损坏的预编译包。

发布构建还必须通过 sidecar 严格检查：`node scripts/desktop.mjs check --target <target-triple> --strict-release`。
如果报告占位 SHA、`latest` URL、缺少可执行位或许可证材料，不能绕过检查；先在
`src-tauri/binaries/manifest.json` 填写经复核的不可变来源、真实 SHA256 和许可材料。

## 4. 搜索

### 4.1 搜索无结果但标签存在

检查：

1. FTS 触发器是否同步。
2. 普通库条件和超级搜索条件是否混用了两套语义。
3. 分面 key 是否与 UI 展示名混淆。
4. 查询是否错误包含回收站。
5. 特殊字符是否经过 FTS/LIKE 安全处理。

### 4.2 中文“海边”误命中“上海湖边”

原因通常是逐字索引没有加短语约束。中文查询需要短语查询和短查询 LIKE 兜底配合。

### 4.3 超级搜索必须区结果错误

检查 `mustNot` 极性：

- `mustNot` 内只允许正向条件。
- `ExcludeTag` 和 `QueryExpr::Not` 不允许进入 `mustNot`。
- 多层 NOT 必须由计划层统一处理。

### 4.4 优先区顺序不生效

检查：

- plan 是否经过统一 `normalizeSearchPlan`。
- should 是否被截断为 12 条。
- minimumShouldMatch 是否为 0。
- 位置权重是否为 2.0/1.0/0.5。
- 当前 ranking 是否被字段排序掩盖。

验证应使用 relevance，或确保字段排序键值相同。

### 4.5 AI 搜索变红字

三层降级设计下，只有配置错误应真报错。

- 鉴权、连接、超时：检查连接配置。
- 非 JSON、未知字段、空组：应剔除或降级关键词，不应整次失败。
- 若没有降级，检查 `is_config_error` 和 sanitize 路径。

## 5. 标签与分面

### 5.1 分面名称改了但历史数据读不到

分面 key 是机器协议，display name 只是展示。不得把展示名当 key。旧中文名称只用于历史迁移映射。

### 5.2 合并或删除标签后计数/FTS不一致

检查事务是否完整覆盖：

- `asset_tags`
- `tag_ops`
- `ai_suggestion_items`
- `tag_aliases`
- `tags`
- FTS 触发器

### 5.3 撤销 AI 批次误删手工标签

手工覆盖应清理 `source_batch_id`。撤销只处理属于该批次且来源不是 manual 的关联。

## 6. AI 打标与本地模型

### 6.1 按开始打标没反应

检查：

- 批次是否停留在 processing。
- 是否重启后遗留，应由启动维护标记 interrupted。
- 是否没有 pending 项。
- 前端错误是否被吞成小字。

done/cancelled/interrupted 可以续跑 pending，processing 拒绝重复启动。

### 6.2 模型返回空结果

空解析视为单条失败，不写空标签成功。检查模型是否支持视觉输入、模型名是否正确、中转站是否截断响应。

若提示“无法解码素材图片（扩展名 .…）”，先检查原文件是否仍存在、是否损坏，以及对应的 HEIC/RAW 解码依赖；
必要时把该素材重新导入或转换为 JPG/PNG 后再续跑，不要把损坏文件标记为成功。

若 Windows 托管 Ollama 日志或错误同时出现空 `message.content`、非空 `message.thinking`、
`done_reason=length`，说明思考输出吃满了 `num_predict`，最终 JSON 尚未生成。原生
`/api/chat` 请求会发送 `think: false`；其他 localhost、局域网或自定义端口服务不属于本地 Ollama，
不会收到 Ollama 扩展字段。不要通过不断增大超时或重复“校验修复”掩盖此问题；修复后可续跑批次中的未完成项。

如果错误提示包含“上下文过长”，系统会把 AI 图片从 1024px 降到 768px 重试一次。仍失败时应更换上下文更大的视觉模型，
不要直接把 UI 高清缓存或未经处理的原图重新发送。

### 6.3 云端连接失败

检查：

- base URL 是否包含正确 API 路径。
- API mode 与供应商协议是否匹配。
- Key、额度和模型权限。
- 网络代理、超时和证书。
- 如果界面显示请求正在等待限流，检查“在线服务 → 编辑 → 请求限制”中的并发、每分钟和每小时数值是否与供应商额度一致。
  空值或 0 代表不限；较小的值会主动排队，取消当前批次即可停止等待。仍收到 429 时应降低数值并检查供应商是否还对
  token、并发连接或模型分别计费，这些额外维度不在本地计数器内。
- 收到 429 后批次会主动中断且不再重试，界面会显示服务商返回的脱敏错误。对 Agnes 免费模型可先使用并发 1、RPM 16–18，
  再根据账号实际提示调整；修正设置后直接续跑剩余 pending 素材。

### 6.4 Ollama 未检测到

检查：

- `ollama serve` 是否运行。
- 默认端口 11434 是否可访问。
- base URL 的 `/v1` 与原生 `/api` 根地址是否正确区分。
- 模型是否已 pull。

如果设置页显示“正在启动并复检”，先等待页面自动检查完成，不要连续点击启动；首次启动最多等待
90 秒。若仍未就绪，检查 Ollama 安装路径、11434 端口占用和应用日志中的 `ollama_start_service`。
应用自己拉起的服务空闲 2 分钟后会自动停止，下一次本地打标、超级搜索或模型拉取会再次自动启动；外部
已经运行的服务不会被应用停止。

### 6.5 本地模型推荐错误

显存探测优先使用 NVIDIA 工具。Win32 AdapterRAM 存在 32 位上限问题，不能作为唯一依据。探测不到时必须诚实显示未知，不猜测。

## 7. 删除、导出和恢复

### 7.1 删除文件失败但列表消失

这是严重数据一致性问题。必须检查文件删除是否成功、数据库是否只在成功后更新。失败时保留数据库记录和可重试状态。

### 7.2 move 导出后查看器死链

移动文件成功后必须同步素材路径。检查部分成功、取消、同名冲突和数据库更新顺序。

### 7.3 恢复备份后任务状态异常

恢复前应拒绝运行中的导入、导出和 AI 批次。旧库保留为 `library.db.old`，恢复后重新迁移和自检。

### 7.4 外部工具清数据后触发器报错

触发器依赖应用注册函数。不要绕过应用直接 DELETE/UPDATE。使用应用内删除或重置功能。

### 7.5 导入完成但出现警告

扫描阶段的路径不存在、目录不可读等非致命问题进入 `warnings`，不应计入 `failed` 或伪装成
重复。先核对扫描路径和权限；真正的单文件哈希、暂存、写库失败仍进入 `errors` 和失败计数。

## 8. 前端与 UI

### 8.1 右键菜单全部失效

点外关闭监听如果捕获阶段拦截了菜单内部点击，会导致菜单项失效。关闭逻辑必须排除菜单自身节点。

### 8.2 Alt+滚轮仍滚动页面

React `onWheel` 可能是 passive，无法 `preventDefault`。需要原生监听并设置 `{ passive: false }`。

### 8.3 缩放锚点漂移

缩放和平移必须以光标位置为锚，修改后使用大图、自定义缩放比例和窗口尺寸变化复测。

### 8.4 设置页或某页面白屏

检查页面边界是否捕获错误、settings normalize 是否失败、组件是否访问不存在字段。错误页必须提供返回素材库或重试路径。

### 8.5 前端错误没有进入日志

检查：

1. `src/utils/logger.ts` 是否仍在模块初始化早期注册全局 error/rejection 监听。
2. `log_frontend` 是否仍使用原始 Tauri invoke；不要把它改走 `src/api/client.ts`，否则会递归。
3. 日志级别是否为 `info` 以上；`debug` 回传只有切到相应级别后才更容易保留。
4. 用 `requestId` 查同一次 IPC 失败，用 `sessionId` + `sequence` 查页面会话前后事件；这两个
   基础设施字段不应被业务 context 覆盖。
5. 消息是否被长度限制截断。回传失败是刻意静默的，不能把日志通道当成业务错误通道。

### 8.6 暗色模式下状态不可见

检查是否硬编码颜色。修复应回到 `theme.css` 语义变量，不在组件里增加主题分支。

## 9. 工具链

### 9.1 `npm` 或 `cargo` 找不到

修正当前终端 PATH，不要提交个人机器绝对路径。PowerShell 中分别检查：

```powershell
node --version
npm --version
rustc --version
cargo --version
```

### 9.2 Git Bash 下 cargo 链接失败

Windows 上优先在 PowerShell 运行 cargo。若 GNU `link` 抢占 MSVC linker，使用 Developer PowerShell 或修正 PATH。

### 9.3 Vite 端口占用

开发配置要求 1420 端口。找到占用进程或结束旧 `tauri dev`，不要让 Vite 静默换端口，否则 Tauri devUrl 不一致。

### 9.4 后端没有重新编译

确认 cargo watcher 进程存在。不确定时结束旧的 `npm run desktop:dev`，然后重新启动。

### 9.5 测试/构建产物污染 Git 状态

确认 `.gitignore` 覆盖 `src-tauri/target*/`、`dist/`、日志和缓存。不要把这些目录加入提交。

## 10. 修改代码后的最低回归

```powershell
git diff --check
npm run lint
npm run typecheck
npm run test:unit

cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

涉及数据、搜索、AI、文件操作或发布时，追加 `pwsh ./scripts/smoke.ps1` 和真机 UAT。
