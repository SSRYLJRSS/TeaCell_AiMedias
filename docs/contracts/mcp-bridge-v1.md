# MCP 本机桥接协议 v1

> 状态：A5-01 冻结并实现的 Windows x64 协议。本文只定义**外部 MCP 客户端 ↔ 茶馆宿主**之间的两段链路：Node 桥接进程的 stdio MCP 表面，以及桥接进程 ↔ 已运行 Rust 宿主的本机 IPC。产品范围见 [PRD](../PRD.md)，实际状态见 [PROJECT_PLAN](../PROJECT_PLAN.md#9-20-agent开发状态)。

本契约是外部接入的唯一行为事实。修改帧字段、错误码、能力集合、上限或取消语义必须同时改协议版本、Rust、Node、测试与本文，禁止只改一端。

## 1. 边界

- 宿主（Rust）是唯一写库者。桥接进程没有数据库连接、没有文件写执行权、没有网络访问，只做协议转换。
- 桥接进程由外部 MCP 客户端启动，连接**已经在运行**的宿主。宿主未运行时不新建空库、不静默启动 headless 宿主，只返回可判定错误。
- 调用方身份来自连接建立时的配对记录，不从帧里读取。客户端自报的 `clientInfo.name/version` 只作标签，永远不参与授权判断。
- 计划批准、永久清空回收站的专属确认、总仓迁移/重新定位都不经本协议。`plans.approve`、`operations.resume`、`trash.empty.*` 永不出现在 MCP 工具表里。

## 2. 两段链路

```
外部 MCP 客户端 ──stdio(JSON-RPC/MCP)──▶ Node 桥接进程 ──本机 IPC(本契约)──▶ Rust 宿主
```

- stdio 段是标准 MCP：`stdout` 只写 JSON-RPC 帧，所有诊断写 `stderr`；不得把密钥、令牌或文件路径清单写进日志。
- IPC 段是本契约。宿主在应用数据目录创建端点文件，桥接进程读取它来发现端点并取得本次运行期的签名材料。

### 2.1 MCP 协议版本冻结

stdio 段冻结为 MCP `2025-11-25`，方法面只有 `initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`，不使用实验性 Tasks、sampling、roots 或 prompts。客户端请求更高版本时，`initialize` 仍按 `2025-11-25` 回答（协议允许服务端降级），不额外声明未实现能力。

**已声明的偏差**：2.0 首版不引入第三方 MCP SDK（`@modelcontextprotocol/sdk` 或 `rmcp`），而是在桥接进程内按上述冻结方法面实现最小 JSON-RPC 服务端。理由有三：桥接进程必须零依赖才能随应用打包进安装包预算（SDK 会带来约 95 个传递包）；零依赖使「桥接进程没有独立数据库连接或文件写执行权」在代码层可审计；协议面仅 5 个方法且已被本契约逐字段冻结。代价是失去 SDK 的新版本兼容与 schema 生成，故 `2026-07-28` 目标版本在 2.0 明确标注**未验证**，由 A5-03 用真实客户端记录，不得据此声明兼容。

## 3. IPC 传输与分帧

| 平台 | 端点 |
|---|---|
| Windows x64 | 命名管道 `\\.\pipe\teacell-mcp-v1-<instance>` |
| 其他平台 | 2.0 首版不支持；宿主与桥接进程都必须返回 `UNSUPPORTED` 并说明原因（见 [PLATFORM](../PLATFORM.md)） |

分帧与内部 Host 协议一致：UTF-8、每行一个 JSON 对象、`LF` 为分隔符、单帧**上限 65,536 字节**（不含 LF）。空行、非对象 JSON、非法 UTF-8、超限帧、EOF 时未结束的帧都是协议错误，宿主关闭该连接。

端点文件：`<app_data>/mcp/endpoint.json`，仅当前用户可读，内容为

```json
{ "schemaVersion": 1, "protocol": 1, "instance": "inst_...", "pipeName": "teacell-mcp-v1-...", "key": "<base64url 32 字节>" }
```

`key` 每次宿主启动重新生成，进程退出即失效；它是**运行期**材料，不是长期凭据。

## 4. 握手

桥接进程连上端点后必须立刻发送 `hello`（5 秒内，否则断开）：

```json
{ "type": "hello", "protocol": 1, "connectionId": "mcpconn_<16 位十六进制>", "token": "<64 位小写十六进制>", "clientInfo": { "name": "…", "version": "…" } }
```

`token = HMAC-SHA256(key, connectionId)` 的小写十六进制。`key` 只来自端点文件，绝不进入命令行参数、客户端配置或日志；客户端配置里只有 `connectionId`。

宿主按以下顺序判定，任一失败即回 `close` 帧并断开：

1. 帧字段集合与类型完全匹配、`protocol == 1`；
2. 端点文件里的 `protocol`/`instance` 与当前进程一致（检查客户端是否读到旧端点）；
3. `connectionId` 语法合法、数据库里存在、`revoked_at IS NULL`；
4. `token` 与宿主重算值**常量时间**相等；
5. 连接的能力范围非空，且与 MCP 可发布集合取交集后仍非空；
6. 总仓存在且健康：否则回 `LIBRARY_OFFLINE`。

成功后宿主回 `welcome`：

```json
{ "type": "welcome", "protocol": 1, "instance": "inst_...", "connectionId": "mcpconn_…", "label": "…", "principalId": "principal_…", "capabilities": ["assets.search", "operations.get"], "tools": [ { "name": "tea_assets_search", "capability": "assets.search", "access": "query", "description": "…", "inputSchema": { } } ] }
```

`tools` 只包含**连接范围 ∩ MCP 可发布集合**，顺序稳定、按 `name` 升序。桥接进程不得自行增补工具，也不得把 `welcome` 之外的任何来源当作工具表。

### 4.1 MCP 可发布集合与连接范围

MCP 可发布集合（2.0 首版）是内置 Agent 工具集的**真子集**：工具名与能力 ID 同形（`tea_` + 能力 ID 下划线化），但只取其中能在「没有本机会话、没有目录句柄」的前提下安全成立的那几项。`imports.*`/`exports.*` 因缺少合法的目录句柄来源永久不进这张表（见 [§4.1.1](#411-为什么-20-不发布-imports设计结论不是待办)）：

| 能力 | 工具名 | 访问类别 |
|---|---|---|
| `assets.search` | `tea_assets_search` | 查询 |
| `assets.trash.prepare` | `tea_assets_trash_prepare` | 准备计划 |
| `assets.trash.submit` | `tea_assets_trash_submit` | 提交（消费 0 次新批准） |
| `operations.get` | `tea_operations_get` | 查询 |
| `operations.cancel` | `tea_operations_cancel` | 取消 |

连接创建时默认只勾选查询类（`assets.search`、`operations.get`）。写入类必须由用户在设置界面显式勾选。`assets.trash.submit` 只能消费**用户已在茶馆完整界面批准**的计划；外部调用永远是 `APPROVAL_REQUIRED`，本协议不提供批准帧。

#### 4.1.1 为什么 2.0 不发布 `imports.*`（设计结论，不是待办）

[任务卡 A5-02](../../plans/v2/execution/A5.md#a5-02) 的验收口径含「外部查询 → 准备导入 → 茶馆确认 → 查看收据」。经设计复核，**这条目标在 2.0 的可发布集合上被本协议的有意裁剪收窄为「外部查询 → 准备计划 → 茶馆确认 → 查看收据」，导入与导出仍由本机界面驱动**。理由不是实现难，而是让外部客户端准备导入**没有安全的句柄来源**：

- 目录授权只能由 GUI 通过 Rust 宿主的原生目录选择器创建（[授权契约 §3 第 1 条](authorization-v1.md#3-授权生命周期)），且 Pi/MCP 明确不得调用该入口。
- 每个目录句柄都绑定 principal + instance + session（`authorization::validate_handle_mask` 逐项比对）。MCP 调用方的 `session_id` 恒为 `None`（[授权契约 §2](authorization-v1.md#2-主体与用途) 的主体表），因此任何为 GUI 会话签发的句柄在 MCP 侧都必然解析失败。
- 「让 GUI 替某个外部连接签发 remembered 授权」在机制上可行（`validate_grant` 对 `remembered=true` 只校 principal、不校 session/instance），但代价是：外部客户端自己无法发现目录，用户必须把 `directoryRef` 手工抄进客户端配置；而一旦抄错或抄进别人的配置，就等于把一个可读整个目录的句柄交给了不在本机界面里的进程。这条路径把「客户端不得与本地会话混淆权限」这条不变量，换成了「用户别抄错」。2.0 不接受这个交换。
- 因此 `imports.prepare` 在 MCP 侧**没有合法的 `sourceRef` 来源**，注册它只会得到一个永远 `UNAUTHORIZED` 的工具；[Agent v1 契约](agent-v1.md#导入入口的设计结论) 已把「注册一个只会失败的工具比诚实的缺席更糟」写成原则。

补充两条边界，避免后续实现者绕过本结论：

- **不得让裸路径顶替句柄**。`import_prepare` 的 `paths` 通道（无授权的显式文件路径）是 GUI 工作台重新准备时的回落路径，不对任何 Adapter 开放；评估用例 E32 已把「模型给出的裸路径变成目录授权」列为禁止副作用。
- **不得放宽主体比较来强行打通**。把 `validate_handle_mask` 的 session 比对改成「MCP 例外」会让所有外部连接共享同一套句柄解析语义，等价于取消会话隔离。

本结论不删除 A5-02 的其余验收（查询、准备计划、茶馆确认/拒绝、稳定任务 ID 与收据、未批准零写入、不重放），那些已由 A5-02/A5-03 的既有证据覆盖；它只把「外部准备**导入**」一项明确移出 2.0 交付范围，并在 `docs/PROJECT_PLAN.md` 的 A5-02/A5-03 行保留为显式未验证项。


## 5. 调用

```json
{ "type": "call", "requestId": "req_<16 位十六进制>", "capability": "assets.search", "payload": "<base64>" }
```

- `payload` 是 UTF-8 JSON 对象的标准 Base64；解码后 ≤ 32 KiB、编码串 ≤ 44 KiB，必须是非空对象。
- 宿主丢弃载荷里的 `schemaVersion` 并注入 `1`，再按 [Agent v1 契约](agent-v1.md)校验；载荷里出现 `principal`、`approved`、`libraryId`、`grantEpoch` 等授权字段按 `INVALID_INPUT` 拒绝。
- `capability` 必须在该连接的范围内，否则回 `NOT_ALLOWED`。
- 同一连接允许并发调用，宿主在独立线程回包，`requestId` 必须回显。

成功：

```json
{ "type": "result", "requestId": "req_…", "ok": true, "payload": "<base64>" }
```

失败：

```json
{ "type": "result", "requestId": "req_…", "ok": false, "code": "APPROVAL_REQUIRED", "message": "…" }
```

`code` 取自 Agent v1 错误码枚举；`message` 是脱敏中文说明，不含路径细节、令牌或 SQL。

`{ "type": "ping" }` 回 `{ "type": "pong" }`，用于客户端探测存活。

## 6. 端到端错误码

| 码 | 触发 | 客户端应做什么 |
|---|---|---|
| `HOST_UNAVAILABLE` | 端点文件缺失、管道不存在、宿主已退出 | 提示先启动茶馆并完成本地接入设置，不要重试写操作 |
| `PROTOCOL_UNSUPPORTED` | `protocol` 不是 1、帧不合规 | 提示升级桥接进程或宿主 |
| `PAIRING_REQUIRED` | `connectionId` 不存在或从未配对 | 在茶馆设置里创建本地接入连接 |
| `PAIRING_REVOKED` | 记录已撤销 | 重新创建连接 |
| `NOT_ALLOWED` | 能力不在连接范围、或不在 MCP 可发布集合 | 在设置里扩大范围，或改用其他入口 |
| `LIBRARY_OFFLINE` | 总仓未就绪或迁移维护中 | 提示先在茶馆处理总仓 |
| `UNSUPPORTED` | 非 Windows x64 平台 | 换平台或等后续版本 |
| `INVALID_INPUT` | 载荷非标准 Base64、编码超 44 KiB / 解码超 32 KiB、非 JSON 对象、夹带授权字段，或不符合 Agent v1 的字段规则（例如 `idempotencyKey` 短于 16 字节） | 修正客户端载荷后重试；**这不是宿主不可用，不要提示用户重启茶馆**，也不是权限问题 |

`INVALID_INPUT` 是宿主在 `result` 帧里直接回给桥接的 Agent v1 错误码（见 §5），所以它必须在本表里有一行：否则桥接只能把它归入默认分支，把「你的载荷不合规」误报成「宿主没启动」，客户端会去重启一个本来正常的茶馆，或把它当成权限不足去改设置。八个码都必须由客户端可判定，且不得互相顶替。

## 7. 撤销与在途请求

- 撤销连接立即生效：下一帧握手被拒，已建立会话的下一帧 `call` 也重新校验记录（`revoked_at` 非空即回 `PAIRING_REVOKED` 并断开）。
- 已进入 Operations 队列的写任务不因断开而被回滚或重放；客户端重连后按 `operations.get` 查询原任务，绝不重发 `call` 制造第二条 operation。
- 取消只走 `operations.cancel`（对应任务取消），不会取消外部客户端的上游请求。

## 8. 生命周期与控制

- 默认关闭。用户在设置里启用后宿主才创建端点文件并监听；关闭时删除端点文件并断开所有会话。
- 宿主退出（正常或崩溃）后端点文件最多残留一次启动周期的 `key`，桥接进程读取它会因管道不存在得到 `HOST_UNAVAILABLE`；新一次启动重写端点文件。
- 同时最多 8 个会话；超过时新连接立即回 `close { code: "BUSY" }`。

## 9. 验证要求

Rust 侧必须覆盖：握手字段/类型与超限帧、令牌错误、未知连接、已撤销连接、范围外能力、载荷边界与伪造授权字段、`welcome` 工具集与范围一致、撤销后在途会话被拒、端点文件缺失/旧实例、宿主关闭后桥接得到 `HOST_UNAVAILABLE`、并发调用 `requestId` 配对。

Node 侧必须覆盖：stdio 只出 JSON-RPC、`initialize`/`tools/list`/`tools/call`/`ping`、宿主不可用时的空工具表与可判定错误、宿主撤销与协议错误的透传、日志不污染 stdout、异常输入不崩溃。

真实 MCP 客户端的初始化/查询/写计划/拒绝/状态四项回归属于 A5-03，不得用本卡的假客户端结果替代。

## 10. 真实客户端互操作记录（A5-03）

上面要求的「真实客户端」证据由 `src-tauri/tests/mcp_interop.rs` 提供，它启动**真的子进程**客户端，经 stdio 连桥接、桥接经命名管道连本进程宿主，链路上没有 mock。

| 项目 | 记录 |
|---|---|
| 平台 | Windows x64（`x86_64-pc-windows-msvc`） |
| 客户端一 | `packages/mcp-interop/client-node.mjs` — Node `v24.19.0` + `@modelcontextprotocol/sdk` **1.32.1** |
| 客户端二 | `packages/mcp-interop/client-python.py` — Python `3.13.14` + PyPI `mcp` **1.28.1**（独立实现，不共用代码） |
| MCP 协议 | `2025-11-25`（§2.1） |
| 覆盖场景 | 初始化/工具表一致性、查询、写计划、拒绝、状态获取、断线重连、请求重试、任务完成后恢复、宿主崩溃残留端点、干净退出与重启、撤销配对即时生效、迁仓期写阻断、三来源共用写队列、选择集合快照冻结 |
| 未验证 | 非 Windows x64 平台（`platform_supported()` 为假，套件打印原因后跳过）；Claude Desktop / Cursor / Codex 等商业客户端本机未安装 |

**客户端只做一步**：两条客户端每次调用只完成一件事并向 stdout 写单行 JSON，剧本由 Rust 测试驱动。这不是测试便利，而是结构约束——批准帧只有可信 GUI 能产生，客户端在物理上拿不到批准能力，因此「外部模型伪造批准」没有可走的路径。

**两个实现刻意不同**：用两套独立 SDK 而非同一 SDK 两个版本，是为了让「客户端能力差异只影响体验、不改变业务授权」成为可观测事实（`two_independent_clients_agree_on_protocol_and_tool_table` 断言两者拿到的工具表逐项相同）。

**本卡期间修掉的三处宿主缺陷**（说明这些路径此前从未被真实客户端走过）：命名管道裸名被 Win32 当相对路径拒绝（`os error 123`）；`inputSchema` 缺根 `"type": "object"` 导致官方 SDK 判整份工具表无效；§5 会回 `INVALID_INPUT` 而 §6 码表没有它，被兜底成 `HOST_UNAVAILABLE`，把「你的载荷不合规」误导成「茶馆没启动」。三者都已在本文件 §6 与对应实现处修正，并有回归。

