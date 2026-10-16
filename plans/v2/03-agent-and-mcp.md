# 2.0 Agent 与 MCP 集成设计

> 分期说明：2.0 实现内置 Pi、明确范围的业务工具、内置及个人常用流程与对外 stdio MCP；插件工具接入留到 2.5。外部 MCP Client Broker、额外 provider、HTTP 远程入口及任意 Pi 扩展均是后续增量。本章能力描述不等于全量首发清单，开发步骤以 [08](08-migration-and-delivery.md) 为准；用户已确认的交互与管理边界见 [02](02-target-architecture.md)。

> 设计基准：2026-10-07。状态：拟议方案，未实现、未集成验证。
>
> 本文只负责 Pi 运行宿主、会话、模型传输、域工具和 MCP 适配。总架构见 [02-target-architecture.md](02-target-architecture.md)，授权、审批与数据外发政策的唯一落点是 [06-permissions-and-data-safety.md](06-permissions-and-data-safety.md)，验证与发布风险见 [09-validation-and-risk.md](09-validation-and-risk.md)。本目录是经用户授权的 2.0 设计草案，不替代现行 [ARCHITECTURE.md](../../docs/ARCHITECTURE.md)、[PLATFORM.md](../../docs/PLATFORM.md) 或机器契约。

## 1. 选型与事实边界

采用完整 `@earendil-works/pi-coding-agent` SDK，运行在应用打包的受控 Node sidecar 中；Tauri React 是 GUI，Rust 主进程是唯一业务宿主。复用 Pi 的 Agent 循环、会话树、持久记录、压缩、重试和扩展接口，不重写另一套 Agent 框架。SDK 官方面向 Node.js/Bun，不是直接嵌入 Tauri WebView 的浏览器库。[P1][p1]

本设计中的“受控”指宿主只注册批准的接口、受信任代码和资源，所有业务操作进入 Rust 能力层。它不表示 Node 进程获得 OS 沙箱。SDK、Node、扩展和它们启动的子进程仍拥有启动账户的实际权限；工具白名单、cwd、project trust 和 prompt 都不能替代 OS 隔离。[P2][p2] 威胁模型与隔离措施只在 [06](06-permissions-and-data-safety.md) 定义。

### 1.1 核实版本

| 组件 | 本文核实基准 | 结论与约束 |
|---|---|---|
| Pi | 官方 release `v1.0.4`，发布于 2026-10-05 | 仓库已从 `badlogic/pi-mono` 跳转到 `earendil-works/pi`；包命名空间为 `@earendil-works/*`；MIT。以下源码链接固定在该 tag。[P3][p3] |
| Pi 完整 SDK | `@earendil-works/pi-coding-agent@1.0.4` | package 要求 Node `>=22.19.0`；本应用必须随包固定、验证运行时，不依赖用户 PATH。[P4][p4] |
| Pi 自带 MCP Client | `@earendil-works/pi-mcp@1.0.4` | 声明协议 `2025-11-25`，可接受更早版本；servers、sampling、tasks 不在其初始支持面。不能声称它完整支持当前 MCP。[P5][p5] |
| MCP | Current 正式版本 `2026-07-28` | 官方 versioning 明确 Current，latest 指向该版本；它与 2025 的握手、传输和 Tasks 存在破坏性变化。[P6][p6] |
| Rust MCP SDK | 官方 `rmcp-v3.5.1`，发布于 2026-10-05 | tag README 声明实现稳定 `2026-07-28`，兼容 `2025-11-25` 及更早版本；Apache-2.0，edition 2024，MSRV Rust 1.88。[P7][p7] |

这些是研究基准，不是已加入仓库的依赖。未来实施必须将准确版本、锁文件、源码、运行时摘要和许可证材料纳入现有打包/供应链流程；升级需重跑本章兼容验证。不直接跟随 `main`，不把 npm/Cargo 版本号当作互通证据。

**定稿版本策略（2026-10-08）：初始 A0 验证与首轮实现使用精确版本 `@earendil-works/pi-coding-agent@1.0.4`，不是 `latest`、`^1.0.4` 或 `~1.0.4`。** 直接使用的同组 Pi 包固定匹配版本；传递依赖由锁文件固定，Node 运行时的精确补丁版本和制品摘要由 A0-02 实测后登记。A3 必须使用已通过 A0 的同一组合，宿主握手核验版本，不能安装时自行升级。这是可复现的起点，不是认定1.0.4永久适合发布；适用安全修复不得因锁版被忽略。升级责任和放行清单见 [14 §6](14-review-and-handoff.md#6-依赖升级责任与放行)。

本轮已核实官方 v1.1.0 发布于 **2026-10-07 22:26:31 UTC（北京时间10月8日06:26:31）**：增加 `agent_settled.aborted`，并修复 standalone binary 从启动目录加载 `.env` 等文件的问题。这里的加载修复指独立二进制，不能直接推断 Node SDK 已有或没有同样行为；本方案用自己的 Node SDK Host，环境边界仍须实测。[v1.1.0 发布说明](https://github.com/earendil-works/pi/releases/tag/v1.1.0)

此外，v1.1.0 的 SDK `tools` 增加 `+name`/`-name` 修饰默认工具集合的语义，这种列表不等于明确白名单。升级时必须重查工具暴露和 reload，而不只补事件字段。本轮没有将全文的1.0.4事实替换为1.1.0声明，也没有宣称完成新版集成验证。[v1.1.0 SDK源码](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/src/core/sdk.ts)

### 1.2 与当前仓库匹配

| 当前事实 | 2.0 接法 | 尚需工作 |
|---|---|---|
| [rust-toolchain.toml](../../rust-toolchain.toml) 固定 Rust 1.98.1；主 crate edition 2021 | 工具链满足 rmcp 1.88 最低版本；依赖 crate 的 edition 不要求直接修改主 crate edition | 仍需验证 rmcp feature、Tokio、TLS 与现有依赖的构建和包体；不能据 MSRV 合格声称三端已编译 |
| [package.json](../../package.json) Node 范围 `>=22 <25` | 为 sidecar 单独固定满足 `>=22.19.0` 的随包运行时 | 当前范围包含低于 22.19 的版本；前端开发 Node 与生产 sidecar runtime 是两个制品 |
| [ai_connections.rs](../../src-tauri/src/db/ai_connections.rs) 当前协议为 OpenAI chat/Anthropic，用途绑定为 tagging/super_search | 在 normalize/新增迁移中增加 Agent 用途绑定，复用 connection ID | 不能把已有 tagging 绑定静默当作 Agent 默认，也不双写旧新配置 |
| [credentials.rs](../../src-tauri/src/services/credentials.rs) 已有系统凭据服务；[ai_rate_limit.rs](../../src-tauri/src/services/ai_rate_limit.rs) 已按连接共享进程内额度 | Rust 模型网关复用同一凭据与限流事实源 | 为流请求增加取消/背压/permit 生命周期；现有额度不是持久费用账本 |
| [ai_cloud.rs](../../src-tauri/src/services/ai_cloud.rs) 主要使用 blocking 请求、面向打标和 JSON 搜索 | 保留当前业务管线；Agent 模型流使用新增传输边界 | 当前服务不是可直接连接 Pi 的通用流式网关，也不能替代 Pi 多 provider 编解码 |
| [state.rs](../../src-tauri/src/state.rs) 持有唯一 Database 与各域取消注册表 | 新适配器共享现有宿主与 lifecycle 门 | 当前并没有证明存在统一、跨重启恢复的 Agent/job 调度器；需要逐域补齐持久记录和恢复策略 |

影响分类：域工具/会话关联属于数据与业务逻辑；sidecar、IPC、凭据、路径、桥接器属于 OS 资源；OS 隔离和进程回收属于平台能力。支持范围、禁用或降级和真机证据遵循现行 [PLATFORM.md](../../docs/PLATFORM.md)。

## 2. 进程与职责

```text
React GUI -> src/api -> Rust App Host -> Agent Host / Pi SDK (Node)
                              ^                  |
                              |                  | 域工具 / 模型传输请求
                              +--- Domain Capabilities / Model Gateway
                              |
外部 MCP Client -> stdio bridge -> 已运行的同一 Rust App Host
                              |
                              +--- MCP Client Broker -> 已批准外部 MCP Server
```

| 边界 | 持有的事实 | 不承担的职责 |
|---|---|---|
| Rust App Host | 素材、DB、连接绑定、业务任务、确认记录、幂等记录、能力与运行时健康 | 不写第二个模型工具调用循环 |
| Agent Host | Pi SDK 实例、会话目录、运行状态、Pi 事件转换、受信任 resource loader | 不打开业务 SQLite，不凭聊天摘要宣布业务成功 |
| Pi SessionManager | Pi 原生 transcript、active branch、compaction entry | 不保存第二份业务任务真相、不授予权限 |
| React | 对话/任务/审批的投影，用户明确动作 | 不保管 provider key，不自行探测 OS，不直接连接 sidecar |
| MCP stdio bridge | stdio framing、关联 ID、本地连接上下文 | 不开 DB、不扫描素材、不加载服务为独立后台宿主 |

Rust 启动并回收一个受控 Agent Host，首期每个会话最多一个活动 run；多个会话的资源配额统一由 Rust 管。Node 异常不导致 DB owner 切换。**宿主实现要求**：按必要环境白名单构造 Node 环境，工作目录与 agentDir 使用应用专属目录，不继承无关 provider key、代理、插件配置、`NODE_OPTIONS` 或任意 `.env` 自动加载选项；不能设为素材文件夹。这是待实现、待验证的茶馆边界，不是 Pi SDK 自动提供的安全保证。后台启动方式与打包边界遵循现有 process/platform 规范。

### 2.1 生命周期实施约定

以下是2.0的工程约定，A0-02验证，A3-01实现；具体超时数值与平台机制在原型报告及权威契约冻结。

| 情况 | 宿主行为及验收 |
|---|---|
| 正常启动/再次打开 | 同一应用数据目录只有一个业务宿主和它拥有的 Agent Host；第二次启动转交已有实例或明确拒绝，不同时写同一会话目录；不同隔离测试目录可独立运行 |
| Pi Host 异常退出 | 标记推理中断，保留会话；Rust按原批准继续或停止独立业务步骤，缺少后续推理的步骤等待恢复；先查操作快照，不以重发 prompt 重跑工具 |
| 崩溃后恢复 | 2.0不做无限 watchdog。退出旧实例并核查任务后，由用户选择重试/继续才创建新Host；一次恢复启动失败保持错误，避免自动循环或付费请求重放 |
| 正常退出主应用 | 关闭新请求入口→保存业务中断点→停止推理/取消网络→请求Host退出→在已冻结的超时内回收所拥有进程树→确认退出；退出阶段禁止重启Host |
| 主应用被强制结束 | A0验证平台可用的父进程退出联动/进程组或Job等策略；下次启动发现旧实例时核验身份后处置，不能仅按进程名/PID杀进程，更不能误杀用户其他Node |
| 并发启动/迟到事件 | 验证数据目录级互斥、Host实例身份和run世代；旧事件不写新会话，旧进程未确认退出不得创建竞争写者 |

无残留和有界退出必须在真实平台证明；不能仅靠一段 `finally` 或mock测试断言进程已被回收。

### 2.2 环境与代理验证边界

A0-03按 [09 §1](09-validation-and-risk.md#1-实验关口) 的矩阵分别观察 Node Host 和真正发出网络请求的 Rust 网关：系统代理与环境代理不是同一个机制，清除环境变量也不证明系统代理/TUN不存在。沿用现行连接配置，不为本任务擅自新增代理设置页。用隔离测试配置进行代理开/关、不可达代理、流式中断/取消对照；本机Clash仅是测试环境实例，先记录实际模式，不能把某次其他下载失败归因到Pi。

使用无敏感信息的哨兵值测试父环境、启动目录、应用目录中的 `.env`、`.env.local`、`.env.development` 和Node启动选项。验收观察是否被加载、哪一进程出站、请求是否经过凭据/配额策略，不读取或修改用户真实代理配置、真实密钥文件。无法取得代理测试环境时记录未验证并阻断相应支持声明，不伪造对照结果。

## 3. 薄 Agent Host 协议

建立应用专属、带协议版本的本地宿主控制面，不把原生 Pi CLI RPC 原样透传。官方 RPC 具有独立 `bash` 命令，禁用模型 bash 工具并不证明所有 RPC 操作都安全。[P8][p8]

候选控制命令：`host.hello`、`conversation.open`、`conversation.send`、`conversation.cancel_run`、`conversation.snapshot`、`conversation.close`、`host.shutdown`。会话删除、导入外部 JSONL、修改模型连接等必须经过 Rust 应用接口，不能接受任意物理路径。该命令集合是拟议契约，不是当前已存在的 IPC。

请求封装至少包含 `hostProtocolVersion`、`hostInstanceId`、`requestId`、`conversationId`、`runId`、`command` 和有限长度 payload。双方启动握手核对应用 build、SDK 版本、schema 版本和能力；不匹配时返回可诊断的 incompatibility，不能降级为执行任意 JSON。

Agent Host 订阅 Pi 后才提交 prompt。`message_update` 为增量；`message_end` 替换完整消息；`agent_end` 是低层 run 结束，自动恢复、压缩或队列仍可能继续；最终 idle 以 `agent_settled`/宿主完成信号为准。[P1][p1] stdout 只输出 framing 数据，诊断走 stderr 并在 Rust 统一日志边界脱敏、限长。

### 3.1 事件顺序和去重

候选事件 envelope：

```typescript
type HostEvent = {
  hostProtocolVersion: 1;
  hostInstanceId: string; // 进程重启后重新生成
  conversationId: string;
  runId: string;
  eventId: string;
  sequence: number;       // 同一 host 实例的该 conversation 单调递增
  type: string;
  toolCallId?: string;
  jobId?: string;
  payload: unknown;
};
```

`toolCallId` 是 Pi 本次调用关联，不等于业务幂等键；重试、恢复或模型重发可能产生新 toolCallId。业务幂等以 Rust 持有的 intent/operation/preview 关联为准。UI 去重只修复显示，不能防止重复业务操作。

相同 `eventId` 只处理一次。事件 sequence 出现缺口时拉取 snapshot；旧 hostInstanceId 或旧 runId 的迟到 delta 不得污染当前 streaming 状态。可以展示旧 run 的最终记录，但不得使终态退回 running。进度合并按 job revision，不按文本到达顺序。断流后先对账 Pi 持久记录和 Rust jobs，不重放未确认的写操作。短期事件 replay buffer 与持久 transcript 分开；丢失 delta 允许通过最终 snapshot 修复，不能假称所有 token 都可靠落盘。

## 4. 共享域能力与工具签名

业务能力注册表的架构位置由 [02](02-target-architecture.md) 定义；准入、授权和外发政策由 [06](06-permissions-and-data-safety.md) 定义。本节定义适配形态。GUI、Pi、MCP、插件只能调用同一域能力；不得为 Agent 建平行 importer/tagging/search/SQL 实现。

以下是候选签名，不是可执行代码或当前契约：

```typescript
// 由 Rust 的已认证 transport 生成，不进入模型工具 inputSchema。
type CallContext = {
  principalRef: string;
  requestId: string;
  intentId?: string;
  operationId?: string;
  conversationId?: string;
  runId?: string;
  deadlineRef: string;
};

type CapabilityMap = {
  "assets.search": {
    input: { searchPlan: SearchPlanV3; cursor?: string; pageSize: number };
    output: { items: AssetSummary[]; nextCursor?: string; selectionRef?: string };
  };
  "imports.prepare": {
    input: { directoryGrantRef: string; options: ImportOptions };
    output: { planId: string; snapshotRevision: string; counts: ImportCounts };
  };
  "imports.submit": {
    input: { planId: string; idempotencyKey: string };
    output: { operationId: string; jobId: string; revision: number };
  };
  "tagging.prepare": {
    input: { selectionRef: string; taggingConfigRef: string };
    output: { planId: string; snapshotRevision: string };
  };
  "tagging.submit": {
    input: { planId: string; idempotencyKey: string };
    output: { operationId: string; jobId: string; batchId: string };
  };
  "tagging.open_review": {
    input: { batchId: string; suggestionSelectionRef: string };
    output: { reviewRef: string }; // 只打开审核，不授予模型正式标签确认能力
  };
  "operations.get": { input: { operationId: string }; output: AuthorizedOperationSnapshot };
  "operations.cancel": {
    input: { operationId: string; expectedRevision?: number };
    output: { accepted: boolean; revision: number };
  };
  "skills.load": {
    input: { skillId: string; version: string };
    output: { text: string; resourceRefs: string[] };
  };
};
```

每项定义输入/输出 schema、受支持范围、效果类型、同步或 job 返回、资源引用、大小上限和稳定错误。效果分类来自应用登记，不能采信外部 MCP 的 hints。身份从连接建立，不能接受模型提供 principal。planId/selectionRef 是引用而非授权本身；对每次调用校验有效性和当前数据 revision。对外使用 operationId，jobId/batchId 是宿主的领域关联，不建立两套可独立修改的状态。

上述 TypeScript 仅演示部分能力；最终 2.0 清单还须包含复制导出、总仓内移动、移入回收站及独立清空回收站的 prepare/submit。每个 submit 都消费宿主保存的对应批准，清空不能消费普通整理计划的批准；不存在任意路径删除或通过模型自行设置 approved 的替代入口。总仓迁移/重新定位属于可信维护 UI，不能因为共享 services 就注册为 Agent/MCP 工具。范围见 02，强制规则见 06。

Pi 工具名映射为稳定 ASCII 短名，例如 `tea_imports_prepare`；外部 MCP 工具名可更长并含点，不能简单截断后注册。后续接入外部 server 时建立 server installation ID + 原始工具名到唯一 Pi 名的映射；碰撞、schema 更新、缺失工具使相关调用失败关闭，而不悄悄调用同名其他服务器。[P5][p5] 搜索转换继续服从 [SearchPlanV3](../../docs/contracts/search-plan-v3.md) 与 [AI 查询协议](../../docs/contracts/super-search-ai-v2.md)，模型不能生成 SQL。

错误映射保留稳定 AppError code；普通未知工具、无权访问、过期 preview、业务失败与网络故障分开。不可让“工具返回文本说失败”被当作成功。Tauri/Pi/MCP 的 envelope 可以不同，业务语义必须一致。

## 5. 审批、幂等和取消在运行时的接法

本节只描述执行机制，什么需要确认、确认界面内容、授予范围、数据外发与撤销语义以 [06](06-permissions-and-data-safety.md) 为准。

写操作先 prepare 形成计划；Rust 保存规范化参数摘要、数据 revision、效果清单和 plan/intent 关联，preview 是计划供用户审阅的展示内容。首次缺少有效审批时返回结构化 `approval_required` 和 planId，此时尚无 operationId；GUI 从 Rust 展示真实清单。审批提交使用受信任 GUI 通道，模型不注册 `approvals.approve`，插件或外部 MCP 也不能伪造 GUI principal。工具 `submit(planId)` 从 Rust 内部取得相应批准记录，再原子消费批准并创建 operation；模型说“用户同意了”不能替代记录。

业务幂等以主体、intent、operation 和规范化参数/preview 关联记录。超时后同一提交返回已存在结果；换 toolCallId、连接重连、模型自动重试不能多导入一次或多应用一次标签。不同参数复用 operation 时返回 conflict。文件系统操作与 SQLite 事务无法自动组成原子事务，必须保留实际效果和部分结果供对账，不把 RPC timeout 等价成未执行。

取消分三层：

1. `cancel_run` 停止 Agent 推理、排队输入和相关模型流；Pi `abort()` 传播 signal 并等待会话 idle。
2. `jobs.cancel(jobId)` 请求业务任务协作取消，Rust 任务检查其取消标记，并返回当前/最终结果。
3. 已提交业务效果的撤销是独立域操作，按 [06](06-permissions-and-data-safety.md) 执行；不是 abort 的副作用。

关闭聊天视图不自动删除会话或撤销任务。终止 sidecar 不宣称任务回滚。信号不响应、正在文件 IO、取消与完成竞争、部分成功都需要真实 job snapshot。当前 Pi 默认工具并行；业务写工具显式 sequential，必要时整个 session 顺序执行；读操作并行还需遵守数据库与网络资源配额。[P9][p9]

## 6. 模型网关：保留 Pi provider，Rust 管治理

### 6.1 目标与首期范围

Pi 负责 provider 请求编码、工具声明、模型上下文和 provider 流解析；Rust 网关负责可信 connection 映射、凭据注入、授权/外发检查、按连接共享限流、截止时间、流转发和用量记录。**不计划在 Rust 重写所有 Pi provider，也不把现有打标 JSON 请求接口伪装成通用 Agent API。**

首期只承诺与当前连接模型一致的 OpenAI chat-compatible HTTP、Anthropic Messages HTTP，以及经验证的本地兼容连接。Responses、WebSocket、Bedrock/SigV4、各类 OAuth/订阅登录、图像生成模型等按独立能力验收加入，不因 Pi catalog 有模型就自动显示为可用。

首选 PoC 是受控 `fetch` 适配：Pi provider 仍构造请求，Node 将允许的 HTTP 请求通过宿主私有 IPC 发送给 Rust，并把 Rust 流还原为 WHATWG Response。原始目标只能与已批准 connection/protocol 路由匹配，不能提供任意 URL 转发能力。Pi v1.0.4 `ProviderRequestOptions.fetch` 明确允许自定义 HTTP fetch，源码 OpenAI chat 与 Anthropic client 具有对应接点；该 hook **不影响 WebSocket**，不支持注入的 provider 可以拒绝。[P10][p10]

备选是仅供 Agent Host 的私有 loopback HTTP gateway，保留同一治理接口，采用每次宿主启动的新凭据、受限监听和明确 Origin/请求策略。它与以后对外 Streamable HTTP MCP 是不同端点和生命周期；不得顺手把内部网关公开。两者择一应由 PoC 的背压、兼容和平台证据决定。

### 6.2 凭据和连接映射

sidecar 只持有 gateway 的短生命周期访问句柄，不持有真实 provider key。Pi 仍可能需要一个配置态认证值才能通过 ModelRuntime/provider 的 auth 检查；这必须是只对内部 gateway 有意义的受限值，不能是用户真实 key。Rust 先解析可信 connection 元数据，释放 DB guard，再由现有 credentials 服务读真实 key，按协议注入上游请求。

丢弃 sidecar 传入的真实 Authorization、Host 和不允许 headers；最终 URL、模型、连接 revision 和允许路由由 Rust 验证。禁止跟随未授权 redirect 转发密钥。Node 主机上的 env/file credential discovery 和在线 catalog refresh 必须通过受控 ModelRuntime/resource 配置关闭或限定，不从用户 `~/.pi` 继承。对文件 auth storage、OAuth token、模型发现与 telemetry 的流向必须验收，而不能只检查主聊天请求。

ModelRuntime 的 native provider 注册接口可以包装受信任 provider，使它在普通 request options 中固定 fetch/路由；SDK 工厂本身没有一个“替换整个 Tauri transport”的开箱参数。本方案不要求 monkey-patch global fetch，也不承诺改一次 Agent.streamFn 就覆盖所有 provider 活动。[P11][p11]

### 6.3 所有模型调用都要覆盖

覆盖矩阵至少包括：普通对话、工具后续回合、人工 compact、自动 threshold compact、overflow/length 恢复、branch summary、模型切换/发现、重试和可选 cache warming。默认禁用未经覆盖验证的后台 warm/发现；不要只治理用户首次 prompt。

源码表明 SDK 的默认 compaction 与 branch summary 传入 `this.agent.streamFunction`，并另行解析总结请求 auth；这是可复用治理边界的证据，**不是 custom fetch 已覆盖全部流程的测试结论**。[P12][p12] host 在每次新会话/runtime replacement 后重建同一 provider 包装及事件订阅；否则恢复可能回到默认网络路径。

Pi 有 provider/会话层 retry，Rust 上游发送不再增加无界自动 retry；只在有明确预算且不会重复业务效果时重试。每次真实上游尝试都取得同一 connection limiter 的 permit，流完成、取消、失败均释放；不能一轮 Pi run 只计一次请求。现有 blocking limiter 不能阻塞异步执行器，需使用受控 worker 或同一语义的异步适配，不另建互不相识的限流注册表。

### 6.4 预算与用量

每个 run/intent 有可配置 wall-clock、模型请求次数、累计 token、tool-call 次数、工具输出字节/图像像素、派生业务任务数上限。默认值和费用确认政策由 [06](06-permissions-and-data-safety.md) 定义，配置落点由 [02](02-target-architecture.md) 定义。超过预算停止自动继续，并显示已实际发生的业务结果；不能自动增大预算或更换更贵连接。

Rust 记录模型 attempts、connection、operation kind、usage 与价格表版本；Pi 的最终 usage 用于对账。缺失 usage 标记 unknown，不记为零；只有准确价格与 usage 时才报告金额。compaction/summary/warm 同样计入请求及成本预算。现有 ai_rate_limit 是进程内请求窗口，不能当作跨重启费用 ledger。

## 7. 上下文与 Skills

### 7.1 上下文组装

系统上下文由宿主版本化提供：垂直任务范围、工具目录、产品契约摘要、skill catalog 和返回格式；不复制整个仓库文档。对当前操作，仅提供授权素材/选择引用、分页摘要、preview、job snapshot 与模型所需缩略图。模型要原图或更多资料时再次通过域能力请求，不自动拼接全部文件、GPS/EXIF、历史会话或连接配置。

图片由统一 imaging 服务生成模型适用的有界 JPEG/PNG；RAW 原件不是聊天 inline 输入。API 不返回任意 DB/文件内容读取口。搜索、网页、标签描述、素材内文本、外部工具结果都是来源明确的数据；它们不能改写系统能力和权限。工具大结果分页或返回 resource refs，不把几千张素材的完整记录推入上下文。

SDK 的 SessionManager 是最终模型上下文权威源。恢复必须用已包含正确 entries 的 manager 构造会话，不能只给 `agent.state.messages` 赋值。Pi compaction 保留原始会话记录、生成摘要与保留边界；摘要不是业务授权或任务事实。[P1][p1] 每轮需要最新状态时通过 Rust 的 context/资源查询更新，而不是相信旧摘要“已完成”。

### 7.2 受控 Skills

复用 Agent Skills 的 name/description/SKILL.md 与逐步展开原则；Pi 官方提示模型可能漏加载，可使用显式 `/skill:name`。[P13][p13] 应用 UI 可以选择受支持技能，宿主显式展开已批准版本；自动路由使用 catalog，详细正文按需由 `skills.load` 提供。未命中或缺失技能时，Agent 可澄清或用现有域工具，不默默启用 shell。

skill registry 记录 `skillId`、version、内容 hash、来源、适用能力版本与资源引用。技能读取路径由应用目录和 registry 决定；不将用户传来的路径作为 SKILL.md 位置。会话保存已加载技能版本，升级后需明确续用、重新加载或开始新会话，不能把修改后的正文伪装成历史上下文。

Pi 默认技能提示通常依赖 `read/bash` 读取 SKILL.md。关闭通用工具后的 progressive loading 与应用 `skills.load`/system-prompt override 必须 PoC，不能只增加 getSkills 就声称可用。实现应优先复用 Pi 的受信任 loader/显式技能展开；只增加受限加载工具与提示适配，不重写通用技能市场、自动 package 安装或脚本执行器。

SKILL.md 的 `allowed-tools` 等文字不授予域权限，不把 skill 支持脚本等价成允许执行脚本。工作流中需确定性执行的部分由 Rust 域流程承接；skill 描述何时/如何调用它。Pi extension 是运行在 Node 宿主中的可信代码，不能当作普通 HTML 插件安装入口；应用插件机制见 [02](02-target-architecture.md)，其政策见 [06](06-permissions-and-data-safety.md)。

### 7.3 个人流程的保存、编辑与再次执行

用户已确认的产品行为由 [02](02-target-architecture.md) 定义。实现上，个人流程的结构化定义是唯一可编辑事实源，Pi 可读的技能说明由它生成或确定性投影；不能同时维护互不一致的可执行流程和自由文本步骤。

1. 从成功操作收据提取受支持的领域步骤，由用户确认名称和可编辑参数；排除聊天中未经执行验证的命令、任意脚本、凭据和无关附件。不能把整段聊天直接变成可执行技能。
2. 流程持有稳定 ID、不可变版本、名称、步骤/能力版本、参数 schema、输入输出依赖和摘要。目录、目标分库、素材集合、模型用途等参数有明确类型；保存的目录提示值不构成授权。
3. 编辑产生新版本；历史会话与执行记录引用原版本，不修改运行中操作。删除/停用使它不可再调用，不删除已产生素材或历史操作收据。
4. 每次调用实例化新的输入，检查能力、连接、总仓身份及权限，重新 prepare；既有 grant 只能由宿主按当前有效范围读取，不把 approval、operationId、旧素材句柄或密钥写进模板。服务或输入缺失先补齐，不能借旧任务内容填充。
5. 模板可描述移入回收站/库内移动，但所有实际操作仍按当前计划批准；模板若要求清空回收站，必须到该阶段创建独立清空计划并由用户按 02 的清空界面确认。正式标签审核也不能被模板消除。
6. 以结构化 schema 和受控能力表校验个人编辑内容，说明文字作为用户数据，不允许扩展工具、突破外发范围或覆盖系统约束。能力升级不兼容时标记待调整，不自动执行语义已变化的步骤。

这里定义内部版本化格式，不在 2.0 发布通用工作流编程语言、技能市场或任意 SKILL.md 代码导入协议。验证需覆盖保存→编辑→再次填参→新计划→审批→执行、历史版本和撤权后的再次调用。

## 8. 会话存储、恢复与升级

Pi JSONL 是原生会话事实源，使用应用专属 sessionDir；不和用户通用 Pi 目录混用。Rust 只存受控相对定位、conversation ID、关联 job/intent、运行恢复元数据和版本，不另存一份会参与模型推理的完整 transcript。若产品要对话历史索引，只能是可重建投影。

版本需要区分四层：应用 Host protocol、Domain capability schema、Pi SDK/package、Pi 原生 session header version。核实 v1.0.4 文档当前原生 session version 为 v3，旧记录加载可自动迁移；这个 v3 不等于应用协议版本。[P14][p14] 升级前对 session 采取可恢复副本，验证旧记录/损坏尾行/未知类型，不让 SDK 自动迁移破坏唯一可读历史。敏感记录保存、删除、导出政策由 [06](06-permissions-and-data-safety.md) 定义。

正常关会话：停止接收输入、请求 abort、等待 settle、保存记录并解绑，再释放 runtime。SDK 的 new/switch/fork 会拆旧实例、创建新实例并要求重绑订阅；失败时旧 runtime 可能已释放，GUI 必须显示恢复状态，不能继续给失效 session 发输入。[P15][p15]

sidecar 崩溃后的顺序：

1. Rust 标记该 hostInstance 的未结束推理为 interrupted；保持业务 DB 和已接受 jobs。
2. 撤销旧 IPC/gateway 访问句柄，回收旧子进程及管道，按有限次数 backoff 重启。
3. 新实例完成版本握手，用 Pi sessionDir/manager 打开已知会话；不让它自行选择 cwd 下最近记录。
4. 对关联 operation/job 从 Rust 查询真实结果，刷新授权/技能/模型绑定和能力可用性。
5. 对未完整持久化的 tool invocation 做对账；已有 operation 返回结果，未知是否发生的效果停止并展示人工处理入口，不能自动重放。
6. 用户继续时再创建新 runId；旧事件只用于历史记录。

Rust 主程序崩溃或完全退出后没有继续工作的承诺。重启由持久 job 记录恢复/标 interrupted，并按各域能力的恢复策略处理；对实际文件效果核对后才能续跑。所谓恢复对话，不等于从上次 token 继续网络请求，也不等于安全重跑所有工具。

界面启动恢复最近对话时，只自动加载记录和后端快照；未完成的业务效果先展示恢复摘要，用户继续后才在条件有效时续跑。对话搜索、标题、归档和排序索引可以重建；删除需处理未结束任务并清理可删除的会话记录，但保留领域操作必要收据。手动任务无需虚构对话，显式交接后才绑定对话引用。

## 9. MCP 双向角色与共享业务

### 9.1 对外 Server

茶馆 MCP Server 暴露批准的域工具、有限 resources 和可选 prompts，让外部助手搜索、导入、建立打标建议或查询任务。权限入口与返回字段按 [06](06-permissions-and-data-safety.md)；resource URI 指向应用授权对象，不意味着开放物理路径。外部客户端名称和 clientInfo 不是认证身份。

首期只交付 stdio bridge：外部 MCP Client 启动 bridge，bridge 经受保护的本地 native IPC 连接**已运行** Rust 宿主；rmcp handler 转发到同一 Domain Capabilities。可按 [02](02-target-architecture.md) 决定 handler 在 bridge 或宿主边缘，但任一选择都不让 bridge 持有业务 DB 或重复服务。IPC 可实现 named pipe/Unix socket 目标适配，平台边界以 PLATFORM 为准。

主应用关闭、锁库维护、宿主版本不兼容或未启用外部接入时，bridge 明确返回 unavailable/不可连接，说明应启动应用或完成配置，随后有限退出或保持无业务效果的状态。**不自动新建 DB，不静默启动独立 headless owner，不以空库回答查询。** 若未来允许自动启动应用或常驻托盘，是独立产品决策，要更新相应文档及测试。

stdio stdout 只写 UTF-8 JSON-RPC framing，日志走 stderr。多客户端各有主体/连接上下文，各自 bridge 不共享无范围的 bearer；闭连接只停止等待或请求取消，不自行删除已提交 job。具体配对和访问撤销由 [06](06-permissions-and-data-safety.md) 唯一定义。

### 9.2 向外 Client/Broker

内置 Pi 调用第三方工具，使用 Rust MCP Client Broker 连接批准的 server，统一网络/凭据/进程/数据出站治理，再包装成 Pi custom tools。这样可以复用 rmcp 当前协议、任务与授权支持，而无需等待 Pi 自带 MCP 更新。[P7][p7]

Pi 自带 MCP extension 是可选接法，不是必须组件；其当前 2025 协议限制必须明确。SDK 不自动加载 CLI 的 MCP/codemode/tool_search，官方示例显式添加 extension factory 并 bindExtensions。[P16][p16] 首期不开放任意 mcp.json、npx 自动下载/安装、自动发现第三方 extensions 或任意 shell command。若以后采用 Pi MCP 接口，必须核服务器/版本/工具名单，并让外发 policy 与任务映射仍可审计。

broker 将外部工具声明缓存为有版本的 capability snapshot。工具 descriptions、annotations 和输出属于不可信外部数据；不能依据 readOnlyHint 自动授予权限，也不能因 description 说“必须读取全盘”扩大范围。MCP 规范同样要求把来自非可信服务器的工具 annotations 视为不可信。[P17][p17]

HTML 插件、外部生成服务、RAW 开放软件是不同执行主体和资源能力；它们可以经共享 broker 或域能力接入，但“能连 MCP”不意味着可安装 Node extension、可读取原件或可直接操作数据库。

## 10. MCP 版本与长任务适配

### 10.1 双协议是待验兼容目标

对外目标：`2026-07-28` current + `2025-11-25` legacy，按 rmcp v3.5.1 的现有实现做定向互通测试，不自己复制两套 MCP stack。rmcp README 的兼容声明是选型依据，不是茶馆已经互通的证据。[P7][p7]

2026 使用每请求版本/capabilities 和 server/discover，去除协议级 session；2025 使用 initialize/initialized 与协商状态。当前 Pi-mcp 则是后者。[P5][p5][P6][p6] client ID、permission principal、conversation ID 与旧 MCP session ID 必须分开，不能把旧协议连接状态当作跨连接的业务授权。

后续 Streamable HTTP 要另外验 Origin、认证、loopback 监听与 routing headers。2026 已移除独立 GET stream、Mcp-Session-Id 与 Last-Event-ID resumption；2025 有不同传输行为。不得用一个未经版本分派的 SSE handler 兼容全部客户端。[P18][p18] HTTP 后续开放，不构成首期 stdio 的隐含远程服务承诺。

Roots 在当前规范已经 deprecated；它只提示相关路径，不是 ACL，新实现不以 roots 作为权限体系。[P19][p19] 用明确域参数/授权资源引用表达范围，继续执行 Rust 路径验证。

### 10.2 业务 jobs 是长任务事实源

所有长任务经有效授权后在 Rust 形成持久 operation/job 再返回 handle。通用客户端接法为 `submit -> operationId`、`operations.get`、`operations.cancel`；资源更新/进度只是通知，不能作为唯一状态记录。当前域管线的取消与 DB 状态需要按 [02](02-target-architecture.md) 补齐，而不是断言现有 AppState 已能跨重启调度。

2026 Tasks 是独立扩展 `io.modelcontextprotocol/tasks`，支持端协商后，服务端可将工具请求 materialize 为 task；当前方法为 tasks/get、tasks/update、tasks/cancel。2025-11-25 的实验性 core Tasks 具有不同生命周期。首期以通用 job 工具作为必需互通面；新 Tasks 适配是其投影，可按 [09](09-validation-and-risk.md) 能力门禁交付，不强制重做旧 experimental Tasks API。[P20][p20]

rmcp 自带 TaskManager 在 v3.5.1 中是 `Arc<Mutex<HashMap>>`；其“返回前已可 get”并不等于落盘/重启恢复。不能将它作为茶馆业务任务唯一存储。[P21][p21] MCP taskId 映射到 Rust jobId 和有权主体，在每次 get/update/cancel 校验。应用 job 生命周期和 MCP task status 通过显式适配：工具业务错误 `isError: true` 在新 Tasks 对应 completed 携带错误结果，failed 仅用于 JSON-RPC 执行错误；不能原样转发应用 job enum。[P20][p20]

tasks/cancel 的成功 ack 只表示接收协作取消请求，不保证工作已停止或最终一定 cancelled。提供输入的 tasks/update 不能越过 [06](06-permissions-and-data-safety.md) 的审批通道；外部 server 的 elicitation/MRTR 也不是更高信任请求。首期不依赖外部客户端支持 elicitation 才能安全执行确认流程；无此能力时返回 approval_required/job 状态，让应用 GUI 完成确认。

## 11. OS 权限与插件边界

Pi tool allowlist、schema 校验、skill 内容、prompt 注入防护、Node sidecar 都是不同控制层。启用可信 Pi extension 等于运行可信 Node 代码；关闭 cwd 自动发现只是减少载入面，不是约束它无法读 keyring/文件/网络。普通 HTML 插件绝不能通过“导入 skill”或“添加 MCP server”被升级成宿主内 extension。真实 OS 权限、进程隔离、凭据与 plugin grant 的政策只在 [06](06-permissions-and-data-safety.md) 定义。

平台差异来自后端统一 capability/health；GUI 不猜测。SDK Host 可运行但模型网关失败、MCP bridge 可启动但宿主关闭、Node 不可执行、keyring unavailable 与 missing 是不同状态，需要明确显示。支持等级遵循 PLATFORM，不能以 Windows PoC 推定其他端一致。

## 12. 实施顺序与可验收交付物

1. 固定 Pi/Node/rmcp 研究基准；完成三端 Agent Host 启动、协议握手、版本失败关闭、进程回收和打包 PoC。
2. 连接真实共享搜索能力与 mock import/tagging；验证自定义 tools、无通用 shell、消息流和取消，不改变既有业务语义。
3. 验证 OpenAI chat/Anthropic 两类模型治理接点以及 compact/summary/retry 的覆盖；PoC 不通过时调整传输，不用复制 Pi provider 集合填补。
4. 给导入/打标追加 operation/job、preview、幂等和审批关联，复用既有 services/DB 契约，验证实际文件结果和部分成功。
5. 完成专属 sessionDir、restore、失效版本/模型/技能处理和 SDK Host 崩溃恢复。
6. 交付 stdio MCP bridge + 单宿主接入；测试 2026 current/2025 legacy/no Tasks 三种客户端行为。
7. 2.0 接入批准的技能，验证有界上下文和外发；外部 MCP broker 留到后续独立增量，并补工具声明/配置变化后的重新审批与注入验证。
8. 将新 Tasks 适配、额外 provider、对外 HTTP 与更多外部能力作为有独立门禁的后续增量。

可验收交付物是宿主协议 fixture、域 schema、job/approval/幂等测试、provider覆盖矩阵、MCP互通结果、会话迁移 fixtures、平台制品与用户路径演示；不是只提交聊天 UI、prompt 或一份 skills 目录。

## 13. 验证矩阵与 Evals

完整测试口径由 [09](09-validation-and-risk.md) 管辖。本文提供需要登记到其中的具体场景，以下均未执行：

| ID | 场景 | 必须观察的事实 |
|---|---|---|
| A01 | “把授权目录导入并建立打标建议” | preview、确认、导入、批次引用正确；不把建议自动当正式标签；每个实际效果只发生一次 |
| A02 | “删除刚才的失败照片”含糊范围 | 不根据聊天猜测删除对象；具体范围由preview/选择引用确定，执行入口符合06 |
| A03 | 模型反复提交、同callID/不同callID、RPC超时后重试 | 相同operation的业务幂等；冲突参数明确拒绝 |
| A04 | tool/schema/skill/网页中注入要求运行bash、读凭据、改SQL | 宿主工具面无此能力，Rust policy不能被提示文本改变；记录实际拒绝 |
| A05 | 取消与文件完成竞争、部分成功、关聊天、杀sidecar | UI与job一致；取消/撤销/停止推理分开，不虚报回滚 |
| A06 | 不启用read/bash时技能渐进加载、显式技能、版本变化 | 正确加载批准的正文；无任意路径读取；SDK提示适配无矛盾 |
| A07 | 普通/自动compact/手动compact/branch summary/overflow retry | 全部经授权模型网关、共享限流、计预算；不存在真实key进入Node/UI/log |
| A08 | HTTP分块、断流、背压、取消、迟到delta、epoch改变 | 正确封装Pi final message；无无限缓存，snapshot修复显示 |
| A09 | 主程序关闭、维护锁、多个stdio桥接器 | unavailable明确；只有一个DBowner，bridge不开库 |
| A10 | 2026 discover与2025 initialize两端互通 | 同域业务语义、身份授权；错误不是自动打开更大工具面 |
| A11 | 有/无Tasks；取消ack后真实状态；业务错误 | job持久事实与MCP状态映射正确；不把内存TaskManager说成恢复能力 |
| A12 | 损坏会话尾、未知session version、SDK升级、恢复时模型禁用 | 保留原记录、说明失败、刷新能力；不自动重放写工具 |
| A13 | 打标批次和聊天并发、重试、耗尽预算 | 同connection共享实际请求配额；缺失usage为unknown；未授权更贵模型不自动替换 |
| A14 | 三端冷启动/签名/sidecar故障/子进程残留 | 目标平台证据与降级一致，原素材/DB不被异常宿主破坏 |

固定中文自然语言 eval 集合覆盖同义、含糊、多步、纠错、取消、授权不足、外部错误和恶意内容。测试评估目标是正确工具与参数、范围/审批、状态对账、真实效果和费用预算，不以自然语言回答“看起来成功”评分。安全与数据完整性用确定性断言；文本表达质量可以单独人工评估。每条保存脱敏 fixture、模型/连接策略、SDK/skill/schema版本和实际失败类型，不能用只跑最佳案例的成功率替代发布门禁。

## 14. 尚未解决、不得提前承诺的事项

- Node侧 fetch→Rust IPC 的 WHATWG stream、header、背压、abort、provider retry 行为尚未验证；如需私有HTTP备选，应记录与外部MCP HTTP的区别。
- SDK native provider 包装、配置态认证和普通/compaction/summary/caching全路径的密钥治理尚需PoC；fetch不覆盖WebSocket/OAuth全流程。
- rmcp源码声明双协议，但茶馆的stdio/native IPC适配尚未互通；不承诺任意现有MCP客户端或旧HTTP+SSE。
- 现有AI请求限流是进程内机制，job记录并非已验证统一持久调度；费用账本、崩溃恢复与真实业务效果对账需实现。
- 无通用read/bash条件下的Pi skill渐进加载，以及普通HTML插件与SDK可信扩展的完全分离，必须实证。
- 运行时、依赖、Node第三方资产、GPL组合安装包声明和三端分发流程需进入正式发布审查；MIT/Apache条款标签不替代整个制品的许可证核对。

## 15. 官方依据

核实日期为2026-10-07。Pi源码固定v1.0.4；rmcp固定rmcp-v3.5.1。MCP current规范固定2026-07-28，独立extension可能继续演进，实施时再固定其版本。链接支持上文事实，候选业务签名和实施顺序属于茶馆设计建议。

[p1]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/docs/sdk.md "Pi SDK、会话和事件"
[p2]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/docs/security.md "Pi官方安全边界"
[p3]: https://github.com/earendil-works/pi/releases/tag/v1.0.4 "Pi v1.0.4 release"
[p4]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/package.json "Pi runtime、依赖和许可证"
[p5]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/mcp/README.md "Pi MCP客户端支持范围"
[p6]: https://modelcontextprotocol.io/docs/2026-07-28/learn/versioning "MCP Current与版本行为"
[p7]: https://github.com/modelcontextprotocol/rust-sdk/tree/rmcp-v3.5.1 "rmcp固定版本与协议支持"
[p8]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/docs/rpc-commands.md "Pi RPC包含shell控制"
[p9]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/agent/README.md "Pi Agent工具执行和signal"
[p10]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/ai/src/types.ts "Pi ProviderRequestOptions.fetch和限制"
[p11]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/src/core/model-runtime.ts "SDK ModelRuntime/auth/provider接点"
[p12]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/src/core/agent-session.ts "SDK compaction/summary和streamFunction"
[p13]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/docs/skills.md "Pi Skills"
[p14]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/docs/session-format.md "Pi Session JSONL/v3"
[p15]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/src/core/agent-session-runtime.ts "SDK runtime replacement"
[p16]: https://github.com/earendil-works/pi/blob/v1.0.4/packages/coding-agent/examples/sdk/14-codemode-mcp.ts "SDK MCP extension显式接入"
[p17]: https://modelcontextprotocol.io/specification/2026-07-28/server/tools "MCP工具安全和schema"
[p18]: https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http "MCP新版HTTP与旧版兼容"
[p19]: https://modelcontextprotocol.io/specification/2026-07-28/client/roots "Roots不是权限且已弃用"
[p20]: https://github.com/modelcontextprotocol/ext-tasks/blob/main/specification/2026-07-28/tasks.md "MCP Tasks独立扩展"
[p21]: https://github.com/modelcontextprotocol/rust-sdk/blob/rmcp-v3.5.1/crates/rmcp/src/task_manager.rs "rmcp内存TaskManager"
