# Agent Host 控制协议 v1

> 状态：A3-01 已实现的 Windows x64 内部进程协议；仅用于 Rust 宿主与应用自带 Node/Pi Host，不是外部 Agent API。
>
> 主实现：`src-tauri/src/services/agent_runtime/protocol.rs`、`src-tauri/src/services/agent_runtime/mod.rs`、`packages/agent-host/src/protocol.mjs`、`packages/agent-host/src/host.mjs`。

## 1. 传输和身份

- stdin/stdout 使用 UTF-8、每行一个 JSON 对象的 framing；LF 是帧分隔符，帧大小上限为 65,536 字节（不含 LF）。空行、非对象 JSON、非法 UTF-8、超限帧和 EOF 时未结束的帧均为协议错误。
- stdout 只允许协议帧。Node 诊断只写 stderr；Rust 在状态变更或 UI 事件转发前校验每种帧的字段集合、字段类型、引用格式和错误类别，未知字段或类别按协议错误关闭 Host。普通命令拒绝仅将固定类别写入结构化脱敏日志，并映射成固定 AppError；不得记录或转发 Host 原始异常、路径、凭据、提示词或请求内容。Host 按序列队写帧；stdout 到达背压水位时等待 `drain` 再写后续帧，不得把 `write()` 返回 `false` 当成已失败或丢弃帧。管道实际报错时才关闭 Host。
- 每个方向的帧都包含 `protocol: 1` 和当前 Host 的 `instance`。Host 发出的每一帧还包含从 1 开始严格递增的 `sequence`；重复、倒退或缺失序号会使 Host 失效。
- 请求包含 `requestId`。实例、请求、会话和运行引用均是 16–128 字节的 ASCII token，只允许字母、数字、`_`、`-`；引用不是文件路径。
- 应用每次启动 Host 都生成新的 UUID 实例 ID。响应和事件必须同时匹配当前实例与协议版本；ACK 还必须匹配原请求的 command 及 conversationId/runId。身份不匹配时按结果未知处理，不得返回成功或清除待核对 run；旧实例或旧 run 的事件不得更新当前运行。

## 2. 启动和能力握手

Rust 先写内部 `start\n` 屏障，然后发送 `host.hello`。Host 在取得数据目录单实例租约后开始读协议，握手的字段必须完全匹配：

```json
{"protocol":1,"instance":"<host UUID>","requestId":"request_<token>","type":"host.hello","appBuild":"<应用版本>","schemaVersion":6,"sdkVersion":"1.0.4"}
```

成功只返回带 `type: "ready"` 的能力声明：`protocol`、`instance`、`sequence`、`requestId`、`schemaVersion: 6`、`sdk: "1.0.4"`、相同的 `appBuild`、`tools`、`modelNetwork: false`、`gatewayProtocol: 1`。外层 stdio framing 仍是 protocol v1；schema v2 增加受限 Rust 网关能力声明和租约字段，schema v3 增加可选且封闭的网关失败摘要，schema v4 增加能力调用帧和精确工具声明，schema v5 增加会话权限档位（`conversation.open` 携带 `permission` 与裁剪后的 `tools`）。schema v6 增加 `settled` 事件的可选 `stopReason`/`rawStopReason`/`usage` 字段（见 §5），用于区分正常结束、长度截断与失败，并透出 provider token 用量；schema v6 同时新增只读的 `conversation.history` 命令（ACK 回 transcript 投影，见 §3）。`tools` 必须是**当前 Rust 侧已注册工具名的精确集合**（不重复、不多、不少），本轮为 `["tea_assets_move_prepare", "tea_assets_move_submit", "tea_assets_restore_prepare", "tea_assets_restore_submit", "tea_assets_search", "tea_assets_trash_prepare", "tea_assets_trash_submit", "tea_exports_prepare", "tea_exports_submit", "tea_operations_cancel", "tea_operations_get", "tea_tagging_prepare", "tea_tagging_submit", "tea_workflows_load", "tea_workflows_prepare", "tea_workflows_submit"]`；声明集合与注册集合不一致、重复 ready、版本不符或未知能力均失败关闭。Host ready 必须在 5 秒内到达。**`ready` 的 `tools` 始终是上面这 16 项全集**：握手发生在任何 `conversation.open` 之前，此时还没有会话档位；按档位裁剪发生在 `conversation.open`（见 §3），越档由 Rust 侧的能力门兜底。

Node Host 没有直接网络能力：`modelNetwork` 必须保持 `false`，SDK 请求只能通过固定的私有 gateway frames 转给 Rust。`conversation.send` 可携带短期 `gatewayLease`；租约的字段、校验和流式状态机见[实现契约 §6.1](../../plans/v2/12-implementation-contracts.md#61-a3-02-agent-模型网关接线)。Rust 当前生产配置使用拒绝上游请求的 disabled transport；本机端到端测试只注入内存 fixture，不接触外部服务、真实凭据、用户全局 Pi 配置或模型缓存。

## 3. 命令

成功命令以相同 `requestId` 返回 `ack`；`conversation.open` 的 ack 还含 `conversationId`、SDK `sessionId` 和回读的 `tools`（schema v5）。失败返回固定类别的 `error`。命令拒绝类别为 `INVALID_CONVERSATION_ID`、`INVALID_OPEN_MODE`、`RUN_BUSY`、`CONVERSATION_EXISTS`、`SESSION_LIMIT`、`SESSION_OPEN_FAILED`、`SESSION_NOT_FOUND`、`INVALID_PROMPT`、`INVALID_RUN_ID`、`INVALID_MODEL_LEASE`、`CONVERSATION_NOT_OPEN`、`RUN_MISMATCH`、`VERSION_MISMATCH` 和 `INVALID_COMMAND`；运行终态使用 `RUN_FAILED`；启动失败诊断使用 `HOST_START_FAILED`。Rust 只接受这些固定值，并将它们映射为安全的用户错误或固定诊断字段。`SESSION_NOT_FOUND` 表示「Host 上确实没有这个会话文件」（`conversation.open` 时 SDK 明确报 `CONVERSATION_NOT_FOUND`），属于可恢复类别：Rust 映射为 `AGENT_SESSION_NOT_FOUND`，前端只在这一个码上用 `mode: "new"` 重建会话；其它打开失败（会话文件损坏、被占用、Host 未启动）仍是 `SESSION_OPEN_FAILED`，不得静默新建以免丢掉上下文。`CONVERSATION_NOT_OPEN` 表示「会话存在但当前没打开」，Rust 映射为 `AGENT_CONVERSATION_NOT_OPEN`，前端先重新打开再重发，不新建。未知命令、非法身份或无法继续处理的协议错误会关闭 Host。

| 命令 | 必需字段 | 行为 |
|---|---|---|
| `conversation.open` | `conversationId`、`mode`、`permission`、`tools` | `mode` 为 `new`、`open` 或 `resume`；`permission` 为 `read`、`standard` 或 `full`；`tools` 是 Rust 按档位算好的**精确注册集合**（`read` 4 项、`standard` 12 项、`full` 16 项），Host 只校验它是 `ready` 那 16 项的非空去重子集并照单注册，不做档位判断。最多同时打开 8 个会话；`new` 不得覆盖已存在会话。ack 额外回读 `tools`，Rust 必须断言它与请求集合逐项相等，否则拒绝这个会话（档位语义只在 Rust 落地）。 |
| `conversation.send` | `conversationId`、`runId`、`text`；可选 `gatewayLease` | 文本最多 4,096 UTF-8 字节且不能含 NUL。全 Host 同时只允许一个活动 run；有租约时先通过 Pi `session.setModel` 选择其固定模型，完成后才 ack 并开始生成。错误租约返回 `INVALID_MODEL_LEASE`，不能退回离线模型或自动换服务。 |
| `conversation.cancel_run` | `conversationId`、`runId` | 必须精确匹配活动 run；等待 Pi 停止当前生成后 ack。不会取消茶馆业务 operation。 |
| `conversation.close` | `conversationId` | 先停止该会话的活动 run、dispose SDK session，再移除会话并 ack。 |
| `conversation.history` | `conversationId` | **只读**返回该会话的 transcript 投影（来自 Pi JSONL，唯一事实源）。ACK 携带 `messages`：每项 `{id, role:"user"\|"assistant", text, stopReason?}`，`text` 为纯文本、`stopReason` 仅助手消息可选。不写会话文件、不写库、不重放业务收据；会话未打开时由 Host 只读打开 JSONL 后投影，读毕立即解 pin。会话文件不存在返回 `SESSION_NOT_FOUND`，其它打开/校验失败返回 `SESSION_OPEN_FAILED`。 |
| `host.shutdown` | 无 | ack 后停止生成、dispose 所有 session、释放单实例租约并退出。 |

普通命令的 Rust 等待上限为 10 秒；`conversation.send` 的接受 ACK 等待上限为 2 秒。发送 ACK 到达时，`agent_host_send` 返回 `{runId, state: "accepted"}`；请求可能已写入管道但 ACK 未确认时，返回 `{runId, state: "outcomeUnknown"}`，保留该 runId 供 GUI 精确调用 `conversation.cancel_run`，不得重放提示词。明确的 Host 拒绝和写出前失败会清除尚未发送的 run 并返回错误。其他命令超时只表示结果未知；A3-01 仍没有会话状态查询命令；transcript 只读投影由 `conversation.history` 提供（不回放业务收据，调用方不得声称已核对 Host 内部运行状态）。

Host 请求等待不持有运行时状态锁；关停会阻止新请求、唤醒本地挂起请求，再通过绕过普通请求队列的专用关停路径发送 `host.shutdown`。启动异常不自动拉起新 Host。已有 Host 异常后，只有可信 GUI 明确选择“检查状态后恢复”才可用 `recover-stranded` 模式启动；它只回收过期租约，不重放提示词或业务操作。退出期不再启动 Host，正常关闭须在 1 秒内完成；预算超时后由 Windows Job 回收属于强制清理，不计为正常退出成功。

Gateway frames 继续使用本协议的有界 framing、实例 ID 和 Host 全局 sequence；细化字段、双向 requestId 配对、上传 credit、按需响应 pull、取消、限流和预算由[实现契约 §6.1](../../plans/v2/12-implementation-contracts.md#61-a3-02-agent-模型网关接线)定义。Rust 只向 Host 返回安全错误类别和流数据；真实凭据、端点和授权头只存在于 Rust。

## 4. 能力调用（schema v4）

Host 通过私有帧把模型发起的工具调用转给 Rust；Rust 用可信 Host 身份和当前活动 run 构造调用方上下文，Host 不能提交 principal、library、epoch、`schemaVersion` 或任何授权字段。

请求帧（Host → Rust）字段必须完全匹配，且 `ready` 之前一律无效：

| 字段 | 规则 |
|---|---|
| `capabilityId` | 只能取 A1 契约中、且本运行时已注册的能力 ID（本轮为 `assets.search`、`tagging.prepare`、`tagging.submit`、`exports.prepare`、`exports.submit`、`assets.move.prepare`、`assets.move.submit`、`assets.trash.prepare`、`assets.trash.submit`、`assets.restore.prepare`、`assets.restore.submit`、`operations.get`、`operations.cancel`、`workflows.load`、`workflows.prepare`、`workflows.submit`；`imports.prepare`/`imports.submit` 因模型调用方拿不到自己的目录句柄，仍是显式缺口，见 [A3-03 契约缺口](../../plans/v2/execution/A3.md)）；其他值回 `ok: false, code: "UNSUPPORTED_CAPABILITY"`。真机永不接受 `plans.approve`、`operations.resume`、`trash.empty.*`。 |
| `payload` | 模型工具参数的 UTF-8 JSON 对象再做标准 Base64；解码后不得超过 32 KiB，编码串不得超过对应长度。Rust 丢弃模型给出的 `schemaVersion` 并注入 `1`，再按 A1 契约输入校验。 |
| `requestId` | 16–128 字节 ASCII token；Host 每次调用生成新值，Rust 原样回传。 |

结果帧（Rust → Host）字段要么是 `type: "capability.result"` 加 `ok: true` 与 `payload`（结果记录 Base64），要么是 `ok: false` 与 `code`；`requestId`、`capabilityId` 必须回显请求值，`payload` 解码后同样不超过 32 KiB。错误码只使用 A1 契约的 `AgentErrorCode` 固定集合，不带原始异常、路径、SQL、凭据或提示词。

调用只在存在活动 run 时合法：没有活动 run 的能力请求视为协议错误并关闭 Host；Rust 在独立执行线程回包，不阻塞 stdout 读取。工具名到能力 ID 的映射来自 A1 契约（`tea_` 前缀）；Node 只注册 Rust 已注册的工具，握手已强制两端集合一致。Host 不得在 `ready.tools` 中声明未实现的能力，也不得向模型暴露批准类入口（`plans.approve`、`operations.resume` 仍只属于 GUI）。

## 5. 事件与运行归属

| Host 帧 | 字段 | 消费语义 |
|---|---|---|
| `delta` | `conversationId`、`runId`、`text` | 当前活动 run 的文本增量；text 必须为字符串且不超过一帧上限。 |
| `settled` | `conversationId`、`runId`、`aborted`；可选 `stopReason`、`rawStopReason`、`usage` | 该 run 已结束；`aborted` 表示停止/失败结束，不表示茶馆业务操作已取消。`stopReason`（schema v6）为 `stop`/`length`/`toolUse`/`error`/`aborted`/`deferred`/`pending` 之一：`length` 表示因 `max_tokens` 截断，界面可提示「因长度停止，可继续」，不得静默当作正常完成；`rawStopReason` 是 provider 原始 `finish_reason`/`stop_reason`（≤64 字符）；`usage` 是 provider 上报的 `{input,output,cacheRead,cacheWrite,totalTokens}` 有限非负数，未上报时缺省（不伪造 0）。 |
| `error` | 固定 `code`，响应错误可带 `requestId`；运行错误可带 `conversationId`、`runId` 和可选 `gatewayError: {code, status?}` | 运行错误仍以 `RUN_FAILED` 表示；`gatewayError.code` 只能是 §6.1 的固定类别，`status` 只能是 100–599 的 HTTP 状态。不得传原始异常、路径、提示词或凭据。运行终态仍以 `settled` 为准。 |

Rust 对每种事件执行精确字段白名单和类型校验，再按 `(hostInstanceId, conversationId, runId)` 校验归属。`delta` 只含受限文本；`settled.aborted` 必须为布尔值；`settled` 的可选 `stopReason` 只能取上述封闭集合、`rawStopReason` 为 ≤64 字符字符串、`usage` 必须恰好五个有限非负数字段（schema v6）；运行错误只能使用 `RUN_FAILED`，可选的 `gatewayError` 只保留网关安全类别和 HTTP 状态，不携带额外消息。迟到事件仍消耗 Host sequence，但不会发给 UI 或清除新 run。合法 `settled` 才关闭对应活动 run。本协议事件不充当持久状态；上层须从权威会话/任务来源恢复，且 A3-01 尚未交付 transcript 投影或 UI 恢复行为。

### 5.1 界面状态帧 `host.status`（**不是协议帧**）

stdout 读线程在 Host 流结束时（进程退出、帧损坏、超时）自己生成一条 `{"type": "host.status", "instance", "state": "interrupted", "reason", "conversationId"}`，沿同一条 Tauri 事件发给界面。它**不进入协议通道**：没有 `sequence`、不参与 `(instance, runId, sequence)` 去重、不被 `validate_host_frame` 接受、也不会被 Node 侧收发。

存在意义是 A3-04 的可见性要求：此前读线程只置 `failed` 并杀进程，界面必须等到下一次 IPC 失败才知道 Host 已死，期间会一直显示「正在回答」且下一次发送被活动 run 守卫挡住。加上这条状态帧后，界面立刻把仍处于流式状态的会话落定为「已中断」、清空活动 run 并单列异常。

它不改变本协议的帧集合，因此不触发 §7 的版本升级要求；若将来要让 Node 侧参与 Host 生命周期报告，则必须走版本升级流程，而不是在同一条通道里塞新 `type`。

## 6. 资源、路径和生命周期

- Windows x64 正式包从应用资源目录启动固定版本 Node 与锁定的 Pi Host；开发模式使用仓库内已校验的 Node 和 Host 源码。不得调用系统 PATH 中的 `node`。
- Node 子进程从空环境启动，只额外传入 Windows 运行必需环境变量及语言/临时目录变量；`PATH` 为空。当前 Host 不读取凭据目录或模型文件目录。
- 只读运行资源位于应用包 `agent-host/host` 和 `agent-host/runtime/node-v24.19.0-win-x64`。可写会话/租约数据放在应用数据目录 `agent-runtime`。Rust 在创建目录前检查目录叶子，拒绝符号链接/reparse point；规范化父目录后再创建，并验证规范化结果仍是该目录的直接子目录，固定句柄打开后再次比较目录身份。随后以不共享删除的 no-follow 目录句柄固定数据根和 `sessions` 目录，直至 Host 进程回收，防止检查后被替换。Node Host 启动时再次核验根目录，会话目录 `sessions` 在创建或打开 SDK 会话前拒绝符号链接/reparse point。Node 对一次目录快照只检查名称以目标 `conversationId` 结尾的 `.jsonl` 候选；读取候选会话头前，先发送 `session.pin_request`。Rust 只接受无路径分隔符、且文件名以对应 `conversationId` 结尾的候选，通过 no-follow 句柄确认其是普通且只有一个硬链接的文件，并以不共享删除的方式固定。Rust 回答 `session.pin_result` 后 Node 才能读取会话头并交给 SDK 打开；其他会话文件不会带着当前会话 ID 发 pin 请求。新会话先用独占创建写入会话头，再走相同 pin 握手。Node 不把 SDK 的全目录 `findById` 用于生产发现，避免 SDK自行打开未固定目录项。会话关闭时仅释放该 `conversationId` 对应文件的 pin；Host 回收时释放所有剩余句柄。检查失败时不得把链接目标交给 SDK。

内部 pin 帧也受严格字段与身份校验：`session.pin_request` 包含 `pinId`、`conversationId`、`fileName`；Rust 回复仅含 `pinId`、布尔 `approved` 和固定拒绝码。`session.unpin_request` 必须同时匹配会话引用和文件名，不能释放其他会话的句柄。
- 同一数据目录只允许一个活动 Host。正常启动遇到过期租约时保留现场并失败；明确恢复时才清理可确认过期的租约。恢复互斥锁持久记录 PID 与实例 ID，进程意外退出后下一次明确恢复可回收其锁；活跃所有者和未知/损坏锁文件都不得被抢占或覆盖。
- Windows 使用隐藏控制台和应用所有的 Job Object；应用退出或协议损坏时仅回收该 Job 中的自有进程树，不按进程名杀进程。
- 首个实现只在 Windows x64 启用；macOS、Linux、其他架构必须返回 `unsupported`。A3-01 的通过不改变这些平台的支持等级。

## 7. 演进和验证

任何帧字段、能力、命令、错误、上限或恢复语义变化都要升级协议/Schema 版本，或明确证明 v1 向后兼容；同步 Rust、Node、测试和本契约。禁止仅修改一端或通过忽略未知事件“兼容”。

变更至少验证分片 UTF-8、多帧、空/超限/截断/非法帧、事件额外字段与错误类型、未知错误类别、实例和版本不匹配、会话/运行错配、目录链接拒绝、取消、旧 run 迟到事件、租约双开和显式恢复、异常退出与正常退出预算。真实 provider、Tauri UI、真实素材和其他平台另按其卡验收；离线 Host 测试不能替代。
