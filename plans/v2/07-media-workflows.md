# 2.5 媒体扩展草案：画板、生成、处理与 RAW 显影

> 分期说明：新增画板、生图、工作区与专业 RAW 属于 **2.5 及后续扩展**，本章“首版”指各媒体能力的第一版。2.0 只实现其 Agent 链路实际需要的导入收据/恢复，保留现有媒体功能；不提前建立全部 artifact/derivation 结构。版本取舍以 [08](08-migration-and-delivery.md) 为准。

> 状态：讨论方案，非当前实现、非已冻结协议、非交付承诺。
>
> 调研复核日期：2026-10-07。本文只新增方案文档，没有修改代码、配置、数据库或安装依赖；未运行集成、性能、原生引擎及三端真机验证。
>
> 总体边界见 [目标架构](02-target-architecture.md)，授权、外发与持久执行规则见 [权限与数据安全](06-permissions-and-data-safety.md)，发布验证见 [验证与风险](09-validation-and-risk.md)。本文定义媒体成果如何进入同一业务管线，不另建权限系统。

## 1. 推荐决策与产品边界

建议把生成图片、画板合成、图片处理与 RAW 显影视为同一类能力：**读取明确授权的输入，保存可继续编辑的工作内容，运行引擎，在茶馆页面预览，最后把合格成果注册为素材。** 用户界面、Agent 与开发者插件均调用相同业务接口。

2.5 先通过画板/普通处理与联网生图样例验证插件；联网生成选一个有明确任务查询接口的服务适配器，并可连接用户已有的 ComfyUI。RAW 作为独立媒体增量，优先验证 RawTherapee CLI，通过后才开放已验证预设和有限参数；未通过不阻塞基础插件系统。完整 Lightroom 级交互显影属于后续独立产品范围。

首版不同时承担模型下载、Python 环境安装、任意 ComfyUI 自定义节点安装、第三方原生 GUI 嵌入和公开插件市场。这些不是接收生成成果的前置条件。已有开源引擎的成熟图像处理能力可以复用，但软件名称或一个成功示例不能代替质量、包体和三端验收。

## 2. 当前实现提供了什么，还缺什么

以下是 2026-10-07 阅读仓库所得事实，后续施工前应重新核对：

| 现有落点 | 已有行为 | 后续媒体扩展的限制或增量 |
|---|---|---|
| [`imaging.rs`](../../src-tauri/src/services/imaging.rs) | 全局图片解码、预览策略链、并发许可；AI 图片使用独立的稳定 JPEG 编码入口 | 预览和视觉模型输入不能成为高位深最终成果；扩展仍从此入口读取库内图片 |
| [`raw_decode.rs`](../../src-tauri/src/services/raw_decode.rs) | 半分辨率 Bayer binning、简化 sRGB u8 管线；非 2×2 CFA 可降级灰度；主要用于缺失预览时兜底 | 不是专业 RAW 显影引擎，不能据此承诺完整分辨率、专业去马赛克或色准 |
| [`importer.rs`](../../src-tauri/src/services/importer.rs) | 锁外复制和元数据提取，路径/hash 去重，事务内二次查重，`create_new` 避免覆盖 | 没有统一媒体 operation journal；不提供跨进程重启的成果注册恢复协议 |
| [`assets.rs`](../../src-tauri/src/db/assets.rs) | 素材 CRUD，`ImportResult` 返回导入/失败/重复计数与错误、警告 | 当前返回值没有逐项 artifact→asset 的 receipt，不能用计数可靠绑定衍生关系 |
| [`kinship.rs`](../../src-tauri/src/services/kinship.rs) | 同目录同 stem、完整组恰好一 RAW 一非 RAW 时安全配对 | 显影输出不能无意进入这个摄影同源规则，见第 6 节 |
| [`db/ai.rs`](../../src-tauri/src/db/ai.rs) | 持久 AI 批次、待确认建议、批次中断标记、同源组去重 | 不应拿它充当图片生成记录；打标与生成是不同任务，成果打标复用现有管线 |
| [`process.rs`](../../src-tauri/src/utils/process.rs) | 后台进程构造，Windows 无控制台；探针的超时与双管道读取 | 长期显影 worker 还需专用资源、取消、退出和进程树管理，不能误把短探针当作完整任务 supervisor |

现有 importer 注释中的阶段叙述不代表跨系统事务保证。`copy_create_new` 先创建目的文件再复制，解决的是名称争抢，不等同于完整文件原子发布。文件复制与 SQLite 提交也不在同一事务中；已存在的失败清理不能覆盖断电/崩溃发生在每一阶段的情况。2.0 必须补结果登记与恢复，而不是宣称既有导入已经实现 exactly-once。

当前产品和平台约束仍以 [PRD](../../docs/PRD.md)、[架构](../../docs/ARCHITECTURE.md)、[平台](../../docs/PLATFORM.md) 为准。特别是数据库锁内禁止网络、图像解码和大文件 IO；正式标签仍由现有建议确认流程写入。

## 3. 工作内容与最终素材是两类对象

建议区分以下对象，字段和表名均为待设计示意：

| 对象 | 语义 | 持久及清理规则 |
|---|---|---|
| `MediaWorkspace` | 用户的生成/画板/显影工作空间 | 本地持久，不与一次聊天或临时目录同寿命；可重开、另存版本 |
| `RecipeRevision` | 固定版本的 scene、参数、PP3 或 provider 模板 | 每次提交绑定 revision；修改参数创建新 revision，不改已运行历史 |
| `MediaOperation` | 一次有授权、限额和执行状态的运行 | 使用总体方案的 operation 记录，不另造平行任务事实源 |
| `Artifact` | 已生成但未必入库的输出或预览 | 有角色、状态、实际格式、指纹和持久文件位置；预览不能冒充正式成果 |
| `OutputReceipt` | 一次成果注册的逐项结果 | 记录新建、复用重复项、失败和对应 asset 引用；重试必须可查询 |
| `AssetDerivation` | 素材的生成/处理来源 | 多输入、多输出；不与摄影 kinship 合并；可保留来源已删除的历史摘要 |

工作空间包含编辑状态、已批准输入引用、受控附件、配方、执行引用和成果列表。路径由宿主解析，前端/Agent/插件获得有限期资源引用，不能用项目 JSON 中的任意路径打开文件。网页画板附带的 JSON 也必须校验版本、大小、元素数量、资源引用和外部 URL。

预览缓存可以清理；用户尚未入库的合格成果、PP3/scene 和失败恢复证据不能按缓存清理。用户主动放弃成果后才进入明确的清理策略。备份应覆盖用户要继续编辑的工作空间与配方，或明确提供独立导出；仅备份 SQLite 不足以恢复文件附件。

所有源素材默认不可变。显影/裁剪/重绘创建新配方和衍生资产，不覆盖源图，不覆盖已有 `.xmp`/`.pp3`。外部引擎可能创建 sidecar 的输入在隔离目录使用副本；“向引擎传源路径”并不能保证进程真的只读。大 RAW 输入复制的耗时、磁盘配额及取消需要真实测试。

用于显影或上传的副本是宿主管理的输入 snapshot：完整复制后计算强摘要，并与批准计划的输入条件核对，recipe/operation 固定其身份。可以在 prepare 阶段完成有配额的快照准备并以快照摘要供用户批准；若先批准后复制，摘要不符必须 PLAN_STALE，不能继续使用旧批准。后续处理均读该快照，不重新打开可能已被修改的源路径；asset ID、mtime 或现有截短去重 hash 不能单独证明实际处理字节与批准来源一致。普通网页不能改写已封存快照；可信原生引擎仍受全信任边界约束。

## 4. 共享成果管线与跨崩溃恢复

```text
明确输入/配方/外发/预算
    → prepare：校验、生成计划与授权范围
    → submit：持久化 operation，再执行 provider/worker
    → staging：在宿主管理目录产生 .partial 文件
    → validate：检验完成输出，固定 artifact 指纹
    → 生成 operation 完成：合格 artifact 保存在 workspace

用户随后选择成果，或已有明确自动保存计划
    → prepare/submit：受理独立登记 operation
    → register：复用入库业务，返回逐项 receipt
    → link：保存来源关系及执行结果
    → succeeded / partially_succeeded / failed / cancelled / outcome_unknown
```

`outcome_unknown` 代表外部提交结果不明，不代表可以自动重新执行；总体 operation 状态定义以 [权限与数据安全](06-permissions-and-data-safety.md) 为准。这里只要求能表达该事实，避免在媒体层复制另一套状态机。

生成成功与入库成功分别呈现：合格成果就绪可结束生成任务，用户数天不点击保存也不会让它永久 running。未入库成果及配方留在持久工作空间；稍后选择成果和目的地时建立登记计划及独立 operation，并关联生成任务和固定 artifact。只有原计划已明确授权输出槽位、数量与目的地规则时，编排层才自动派发登记步骤；总工作流成功仍要求约定的登记与来源关系完整。

### 4.1 暂存、校验与发布

1. 宿主分配 operation 专属目录、输出名和输出槽；插件、provider 不能指定正式素材路径。目录不允许逃逸，符号链接/重解析点、硬链接及附件来源按安全总方案复检。
2. 下载或渲染写 `.partial`，限制响应字节、像素数、资源消耗和超时；校验响应实际格式，不信任 MIME/后缀。缺字节、零尺寸或无法解码的输出不能提交。
3. 合格文件在同卷以可验证的完整文件发布协议进入已就绪位置。flush、文件同步和目录同步按平台实现及实际掉电需求确定；不能以一次 rename 笼统宣称三端掉电安全。
4. 引擎返回的成功/路径是证据输入，还需宿主复检真实文件；文件修改时间或提供商预览帧不是最终结果指纹。
5. 所有库内解码和预览经 `imaging`；新增高位深/ICC 探测也应收敛到 imaging 的受控能力，不让插件直接引入独立预览主链。外部 RAW 引擎是显式变换服务，不把其专业全解码加入普通入库扫描。
6. 人工可检查预览与规格；自动保存可在计划中明确授权已确定数量/规格/位置，不能因为 Agent 接收到生成成功就默认保存全批。

### 4.2 入库不是一个 count 回调

生成成果不使用临时目录的原位索引，否则 staging 清理会导致死路径。必须复制到持久成果目录/用户已授权的托管目标，再登记正式素材；允许原位索引的输入只能是已经标记为持久、不会被清理的正式输出。

计划采用 `(registration_operation_id, output_slot)` 作为登记幂等身份，并记录来源 generation operation/artifact；同一保存意图重试取得原登记 operation，不另建任务。内容指纹用于验证与复用，不能以 seed 或文件名判断外部生成已执行。现有素材 hash 是 SHA-256 前 16 字节的 hex，源文件完整性证明与现有去重键应分开，不静默改变已有 hash 语义。

优先在复用 importer 的写库 seam 后，让单项素材登记、artifact 映射与 derivation 在同一短数据库事务完成；文件准备仍在锁外。当前 importer 不支持这一组合，施工需要明确的小范围抽取及迁移/测试。如果阶段性交付先注册再挂关系，必须有 `relation_pending` 的可恢复步骤，未补齐不能宣布工作流完全成功。

重复内容找到已有 asset 时记录“已存在/复用”，返回该 asset 的明确引用；不静默覆盖它的标签或把旧素材改为唯一的新来源。一个内容可有多个来源事件，来源事件链接 operation，不把不同生成任务合并成一条不可追踪记录。没有逐项 receipt 时不能仅凭相同数量猜匹配关系。

### 4.3 崩溃窗口与恢复行为

| 崩溃/中断位置 | 恢复时检查 | 允许继续的动作 |
|---|---|---|
| 外部提交中，未取得 job ID | 已保存 request identity、provider 查询能力、授权和预算 | 能证明已提交则绑定任务；不能证明则 `outcome_unknown`，用户决策后才新提交 |
| 有 job ID，进度断流 | 查询历史/任务状态，确认该 ID 属于本 operation | 重连或轮询；不重复生成 |
| 输出完成，artifact journal 未更新 | 专属输出目录、槽位、完整性和配方 identity | 复检后登记 artifact；不依赖单纯文件存在 |
| 正式文件已发布，素材尚未注册 | 完整输出、登记身份和已有资产映射 | 恢复入库；不重渲染、不重付费 |
| asset 已提交，调用响应丢失 | 幂等 receipt、事务结果、对应资产状态 | 返回已完成结果；不追加新 asset |
| 资产成功、关系或缩略图失败 | receipt、derivation/preview 状态 | 补关系/预览，保留真实部分成功 |
| 用户取消部分批处理 | 已提交/运行/完成/未启动项各自状态 | 保留完成成果与已入库记录，明确取消中；不伪称全部回滚 |
| 备份恢复或主库切换 | library generation、输入引用、目标 identity | 旧登记计划失效；重新核对，避免旧 asset ID 指向新对象 |

恢复只清理经 journal 能证明归属本任务的临时文件；陌生文件、无法判定归属或完成状态不明的成果进入人工处理，不递归清理用户素材目录。

## 5. 联网生图与 ComfyUI

### 5.1 首版连接方式

官方 ComfyUI 自建 API 提供 `/prompt` 提交、`/ws` 事件、`/history/{prompt_id}` 结果与 `/view` 文件读取，官方示例推荐 WebSocket 配合 History，并要求使用 API-format workflow。此协议可由 Rust 实现，示例使用 Python 不表示茶馆必须安装 Python。[ComfyUI 官方接口](https://docs.comfy.org/development/comfyui-server/comms_routes)、[官方示例](https://docs.comfy.org/development/comfyui-server/api-examples)

首版连接用户已有实例，模板使用校验过的节点和命名参数。绑定服务、模板版本、输入素材、数量、尺寸、模型/seed，以及图生图的上传内容；配置模型或模板发生变化时重新预览计划。ComfyUI 服务端自定义节点是运行在对方进程中的代码，不能把“本地服务/开源节点”当成茶馆沙箱的延伸。

监听只接受本 operation 的 job ID，预览消息用作临时展示；完成后查询历史并下载实际输出到宿主 staging。服务端文件名和 subfolder 只作为 provider 定义的请求参数，不能拼成本地写入路径；不支持的输出格式应明确失败或按已公开转换策略处理。

### 5.2 提交不明、重试和取消

通用 provider 接口至少区分提交、查询、下载和取消的语义。幂等提交、取消隔离、费用估算、输出有效期是 **每个 provider 的能力**，不能因为都有 HTTP 就默认相同。

- 提交前保存 operation 与 request identity，成功后保存 provider job ID。写本地记录成功不表示服务端执行 exactly-once。
- ComfyUI 官方示例可以传 `prompt_id`，但这些资料没有给出重复 POST 的通用幂等保证；锁定服务版本后用契约测试核实，不能把重复 ID 视作绝不二次执行。
- 超时、连接断开或 5xx 可能发生在服务已接受请求之后。先查 job/queue/history；无可靠查询关联时显示“结果未知”，不能自动重试付费生成。
- 只对已证明未接受的提交，或 provider 明确支持的幂等键，执行受限重试。429 的额度恢复不意味着前一任务没有执行。
- 官方 `/interrupt` 停止当前 workflow，不能直接当作茶馆 operation 的独占取消。共享实例上先核对当前 job 归属；没有任务级取消保障时，只停止茶馆等待或取消尚未开始的本任务，显示服务端仍可能运行。[ComfyUI 取消接口语义](https://docs.comfy.org/development/comfyui-server/comms_routes)
- 已生成但下载失败先重试下载；下载 URL 过期应查询成果。不得为补一次下载再次生成。相同 seed 也不承诺跨硬件/模型/版本完全相同成果。

### 5.3 网络与凭据

网络请求、上传、限额和密钥均经宿主连接服务；插件不获得原始凭据。参考图上传单独列出实际发送图片、是否缩放、可见元数据与目标服务。prompt、scene 文本和路径摘要也可能敏感，外发政策不只针对图片。

下载 URL 的域、重定向、响应大小与连接类型校验由宿主负责。明确配置本地服务不等于允许插件访问任意 localhost/内网服务。云端输出、SVG/HTML/提示词和工具描述都作为不可信数据，不能以有权限的页面执行；不可信媒体资源走受控展示路径。

## 6. 衍生来源与现有 kinship 的兼容

现有 kinship 是“相机同源 RAW/非 RAW”派生规则，不是通用编辑历史。它影响浏览折叠、标签/数值同步和 AI 去重；由完整组判断，不能仅按选中子集算安全配对。

首版所有衍生图使用独立成果目录和含 output identity 的文件名，避免直接创建 `原图目录/原 stem.jpg`。source→derived 明确关系独立存储，不通过相同 basename 推断。

跨后续 move/rename/export 的防线也要设计：仅分目录不能永远防误配对。正式支持用户把衍生图移动/改名为源 RAW 配对名称之前，应让已标记的衍生资产从 kinship 自动配对中排除，或在落库前拒绝会引入歧义的操作；浏览、打标建批、标签及数值同步、相似排除必须使用同一排除契约，不在各层打补丁。该行为是对已有产品规则的显式增量，需在开发时同步权威文档及测试，本轮不修改现行代码或 PRD。

首版默认不继承正式标签；可以根据选择形成“建议继承”，经现有审核确认。特别是裁剪、背景替换、多图合成、生成变体，不能推定与源图视觉内容相同。模型/种子/配方/来源属于来源记录，不向已有分面塞入未经用户确认的隐式标签；需要打标时，使用现有批次和 pending 建议，正式数据语义不变。

## 7. RAW 显影适配器与颜色

### 7.1 原型优先级

| 方案 | 官方事实 | 对茶馆的建议 |
|---|---|---|
| RawTherapee CLI | 独立无 GUI CLI、PP3 参数、显式输出、16-bit TIFF/PNG；完整浮点处理引擎 | 首版优先原型；清晰预设文本更适合有限参数映射；以样本和包体测试决定最终采用 |
| darktable CLI | 稳定版无 GUI 导出；XMP 历史、styles、ICC、位深配置 | 第二适配器候选；隔离 config/cache/library，不依赖用户最后一次 GUI 导出设置 |
| LibRaw | 读取 RAW 像素、元数据和预览；基础转换不以生产质量渲染为范围 | 仅解码不会得到完整显影；不为“Lightroom 效果”替换当前预览解码器 |
| darktable MCP | 官方开发分支有模块 schema、render、stats、export 等工具 | 后续实验；不作为首版稳定版本前提；必须锁定具体行为 |

依据：[RawTherapee 官网](https://www.rawtherapee.com/)、[官方 CLI 文档源码](https://github.com/RawTherapee/RawPedia/blob/master/content/command-line_options/index.md)、[darktable 5.6 CLI](https://docs.darktable.org/usermanual/5.6/en/special-topics/program-invocation/darktable-cli/)、[LibRaw 定位](https://www.libraw.org/about)。截至复核，RawTherapee 官方展示 5.13；darktable 最新稳定 release 为 5.6.2，发布于 2026-10-04；后者 release 的 `src/mcp/README.md` 查询为 404，与开发分支有 MCP 的事实必须分开。[darktable 稳定 release](https://github.com/darktable-org/darktable/releases/tag/release-5.6.2)

### 7.2 RAW 首版实现边界

锁定引擎版本与探测结果，使用完整已验证配方，不让默认用户设置、相邻 sidecar、GUI 最近状态或任意 CLI 参数悄悄改变结果。PP3 是引擎参数，不是通用标准；官方明确不同版本会变参数、默认值或处理效果。保存引擎版本、配方原文/摘要和最终成果，升级后重新处理是新 revision，不覆盖历史结果。[PP3 版本与存储](https://github.com/RawTherapee/RawPedia/blob/master/content/Sidecar_Files_-_Processing_Profiles/index.md)

首版支持选择已验证的预设、有限曝光/白平衡/输出规格，以及生成低分辨率预览和最终成果。滑块实时响应、局部蒙版、镜头校正细粒度参数、降噪算法和复杂曲线分后续验收。LLM 不直接生成任意 PP3/XMP/blob；宿主从白名单和已校验 typed 参数编译配方。标准化 EV/裁剪等用户字段仍需说明 engine 映射、范围及单位；不承诺所有引擎参数互换。

darktable 会保存 XMP 并维护独立 catalog，catalog 与 XMP 有优先顺序和覆盖风险；不连接茶馆主库，也不同时让用户 GUI 与后台 worker 写同一 catalog。引擎环境使用独立配置和临时工作文件。[darktable sidecar](https://docs.darktable.org/usermanual/5.6/en/overview/sidecar-files/sidecar/)

2026-10-07 检索到的 darktable master 源码与 development 手册存在行为差异：手册说临时副本，而源 README 明确 `render` 的 stack 在 catalog `imgid` 上会持久改历史，`history_end + stack` 可截断历史。因此工具名字和只读标注不足以判断副作用；MCP 原型使用隔离 catalog，并逐工具测试，禁止直接暴露 catalog 修改工具。[锁定源码说明](https://github.com/darktable-org/darktable/blob/18cf557fbd44e9a244567cb690abab91e5d64209/src/mcp/README.md)、[开发手册](https://docs.darktable.org/usermanual/development/en/special-topics/program-invocation/darktable-mcp/)

### 7.3 颜色与高位深

源 RAW、专业处理缓冲、最终 16-bit TIFF 和页面 sRGB 预览是不同表示。`imaging::encode_ai_image` 的 JPEG 输入和当前 RAW u8 查看结果只能用于展示/模型，不能再编码成高质量 master 冒充完整显影。

首版预览统一明确的 SDR/sRGB 策略，最终输出可保留引擎支持的 16-bit TIFF、ICC 和配方。检查实际位深、ICC、尺寸和方向，而非仅记录请求参数。色域转换在受控引擎/imaging 完成，不能只给像素贴新 ICC。输入 profile、工作空间、输出 profile 和显示器 profile 应分别记录/控制；相机 JPEG 预览与重新显影色调不同必须清楚展示。

WebView 显示能力不等于专业色彩验证。跨屏 profile、HDR、广色域与印刷 soft-proofing 属于后续独立验收；首版不宣传专业色准。官方 darktable 说明正确屏幕呈现依赖适当显示 profile。[显示 profile](https://docs.darktable.org/usermanual/5.6/en/special-topics/color-management/display-profile/)

## 8. 网页画板与应用内页面

“在茶馆页面显示”优先理解为自己的 WebView 工作台展示控件、进度和结果，不是把第三方桌面窗口塞进页面。darktable/RawTherapee 的官方入口是 GUI 或 CLI；目前调查未确认稳定的 Tauri 跨平台原生 GUI 嵌入 SDK。操作系统重设窗口父子关系不能作为三端默认实现。

画板首版可提供素材参考、拖动布局、文字/标注、有限裁剪、遮罩、导出和保存工程。原型可评估打包进应用的 Excalidraw 组件：官方提供 scene JSON 序列化和 PNG/Blob/SVG 导出。它适合构图/标注，不代表其拥有 RAW 处理管线；最终库内产物先使用已经支持的位图格式，SVG/工程文件单独保存并采用安全展示策略。[官方 scene 工具](https://docs.excalidraw.com/docs/@excalidraw/excalidraw/api/utils)、[官方导出](https://docs.excalidraw.com/docs/@excalidraw/excalidraw/api/utils/export)

编辑器适配器将供应商 scene 数据留在 workspace 中，公共业务 API 只依赖 scene revision、输入引用和导出规格。避免整个公共 SDK 暴露某个画板库的类。不能直接把远程画板网站或 ComfyUI 页面放进有 Tauri IPC 权限的主窗口；第三方网页/插件 UI 的信任边界见安全总方案。

拖入素材获取宿主批准的预览资源，不直接读文件 URL。合成有多父来源，导出同时保存 scene revision 和所有必要附件。工程 JSON 不保证包含图片二进制，不能只存 JSON 宣称可恢复。撤销仅改变工程版本，不撤销已经入库的成果；删除导出图也不删除来源素材。

所有控件遵循现有 [UI 规范](../../docs/UI_DESIGN_SYSTEM.md)，检查 1366/1920 与最小窗口、键盘操作、加载/失败状态和明暗主题。生成结果卡片、前后比较和最终保存保持可见，不以缩小字号挤出空间。

## 9. Developer/Agent 共用接口与原生信任

接口名称仅示意：`media.prepare`、`media.submit`、`media.get_status`、`media.cancel`、`media.get_artifact`、`media.register_outputs`、`workspace.save_revision`。SDK、Agent 工具适配器和自带页面均走宿主业务层，调用者身份由宿主绑定，不信任 JSON 自报身份。确认结果由宿主生成，Agent 不能发送 `approved: true` 直接执行。可读输入、输出配额、网络目标和注册目的地绑定计划，并在实际执行时复检。

原生 CLI connector 是可信应用组件，拥有操作系统进程权限，**不属于受限开发者插件沙箱**。首版只接受官方验证并锁定的 adapter 与二进制/安装路径；用户配置外部程序必须明示这是本地程序执行并进行版本/能力探测，不允许插件或模型随意指定 executable、shell、环境变量或任意参数。是否随包/单独安装由技术原型和分发审查决定。

进程参数使用原生参数列表，不拼 shell。后台使用现有 `utils/process` 的构造规则，并新增长任务超时、输出截断、线程/内存/并发限制、取消及进程树回收；OpenCL/GPU 不可用时明确 CPU 降级或失败，不能长期占用全部资源影响浏览。显影、生成、普通预览应共享可观测的资源预算，不在数据库锁中等 worker。

RawTherapee/darktable 的 GPL 分发、依赖素材/相机配置许可、三端构建源码与签名、公证需要交付审查。当前项目已有 GPL 组合包，不代表新增组件自动满足分发要求；具体发布门禁沿用 [平台](../../docs/PLATFORM.md) 和总体验证方案。

## 10. 三端支持目标与降级

以下仅为建议目标，尚无集成证据。平台承诺不能因为上游声称跨平台就自动升级。

| 能力 | Windows 10/11 x64 | Apple Silicon macOS | Ubuntu 24.04 x64 | 缺失或不健康行为 |
|---|---|---|---|---|
| 工作空间/网页画板/受控入库 | 正式目标范围 | 预览目标范围 | 预览目标范围 | 维持核心导入→浏览→搜索→查看→导出，不因可选编辑器阻断 |
| 用户已有远程/本地 ComfyUI | HTTP/WS 集成目标 | 同协议集成目标 | 同协议集成目标 | 明确离线/版本/节点缺失，保留工作内容，不自动安装环境 |
| RawTherapee CLI | 优先原型/真机矩阵 | 验证 arm64 引擎与签名后启用 | 验证 CLI 与动态依赖后启用 | 后端探测 capability/health，UI 禁用并说明，仍可浏览 RAW |
| 专业 ICC/HDR 显示 | 后续专项 | 后续专项 | 后续专项 | 首版使用明确 SDR/sRGB 展示范围 |
| darktable MCP/原生 GUI 嵌入 | 实验/后续 | 实验/后续 | 实验/后续 | 不作 2.0 核心交付前提 |

Windows 测试无后台控制台、文件锁、长路径及大小写；macOS 测 arm64 产物、签名/公证、首次执行权限和 app bundle 路径；Ubuntu 测动态库、WebKitGTK 展示、执行权限、非 UTF-8 路径和大小写目录。静态能力由后端统一声明，实际引擎健康由 adapter 检查，前端不自行检测 OS。不可无损表示的路径沿用现有明确拒绝并保留源文件规则。

## 11. 真实端到端旅程与验收

### 旅程 A：对话生成三张封面并只保存一张

用户在助手输入需求并选一张参考图；宿主展示目标服务、实际上传内容、数量、尺寸和预算，获得具体授权后提交。三张成果进入持久 workspace，用户在茶馆比较并保存第二张到“封面”分库，再发起打标建议。

验收：服务查询得到正确 job ID；所有事件只影响本任务；正式库仅新建/复用所选一张；其余成果仍能重开但不计正式素材；参考图和原标签不变；标签建议 pending；提交响应丢失不自动再付费；regenerate 是明确新 operation；图片及 prompt 均不进入普通日志/诊断包。

### 旅程 B：摄影 RAW 预设显影、比较、保存 TIFF

用户选择 RAW 与相机 JPEG，要求用暖色预设开发 RAW。宿主固定 RAW 输入和引擎/PP3 revision，展示可调范围，生成 sRGB 预览；用户比较后保存 16-bit TIFF。用户后来调曝光产生第二 revision，不覆盖第一 TIFF。

验收：源 RAW、相机 JPEG、原 sidecar 指纹不变；Bayer、X-Trans、线性 DNG 样本均按真实引擎输出验证，失败明确；TIFF 具有真实位深/ICC/完整分辨率；输出有显式 RAW→TIFF 来源；现有相机 RAW+JPEG kinship 继续工作，TIFF 不参与该同步/AI去重；引擎升级不能静默改旧成果；CPU fallback 与取消不阻塞素材浏览。

### 旅程 C：页面画板合成后崩溃恢复

用户将两张素材放入画板，标注文字并存工程；导出 PNG 时在“文件就绪、asset 尚未提交”窗口强制退出。重启后打开 workspace，恢复注册该 PNG，展示两父来源及 scene revision，不重新要求绘制。

验收：scene、附件和已完成输出完整可恢复；没有跨域图片/陌生附件注入；仅一个登记 receipt；两张来源原文件不变；正式输出不依赖会被清理的 staging；缺附件明确错误，不能输出看似成功但缺图的封面；画板撤销不删除已入库 PNG。

### 旅程 D：开发者插件批量处理与共享服务取消

插件申请读取选中十张图并产生缩小副本，用户授权后调用共享媒体接口；Agent 对同一插件功能使用相同 interface。部分已完成时取消；共享 ComfyUI 上另一个客户端同时运行任务。

验收：插件拿不到主库/密钥/任意进程接口；只能处理授权集合和分配输出槽；批量成功、失败、未开始与取消中逐项可见；重复注册返回已有 receipt；茶馆不调用会打断他人任务的全局 interrupt；更改插件参数、输入集合或目的地后原计划授权失效。

## 12. 施工顺序、准入和后续

1. 在 2.5 实现 workspace、artifact、receipt 和来源关系的最小持久契约，先以简单画布/处理输出打通 staging→校验→入库→关系；迁移只能新增/追加，补崩溃注入和兼容测试。
2. 接入一个生成 provider，分别验证成功、提交未知、已完成下载失败、取消隔离和预算。任务级幂等能力未确认时禁用自动提交重试。
3. RAW 作为独立增量，用固定样本验证 CLI 引擎的全分辨率、颜色、PP3 行为、冷启动、取消、内存、包体和三端分发。未通过不开放该引擎，也不阻塞已通过的基础插件；未验证前不实现大规模滑块界面。
4. 做自带页面与 Agent/插件共用接口；覆盖上述旅程，并验证 kinship、FTS、打标建议、导出 move、恢复互斥和资源限额。
5. 满足 [验证与风险](09-validation-and-risk.md) 的相关完整门禁和三端真机口径后才声明交付；本轮未执行这些测试。

后续独立增量：低延迟持续渲染 worker、专业局部处理、ICC/HDR/soft-proofing、其他 RAW adapter、稳定 darktable MCP、受管生成运行环境和更复杂的多步图像工作流。每个增量继续共用成果与权限接口，不让对话/插件各造一套文件写入、来源或任务系统。

未决项：RawTherapee 是否随包还是可选下载安装；首个云 provider 的取消/幂等/费用 API；实际工作空间配额和保留默认值；图像 ICC/位深探测实现范围；画板组件许可/包体/React 19 兼容；来源与素材登记能否同事务完成的 importer seam。上述均需要原型证据或产品决策，不以本方案文字视为已解决。
