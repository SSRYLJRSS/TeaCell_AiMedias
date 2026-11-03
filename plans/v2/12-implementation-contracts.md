# 执行前需要固定的接口与数据约定

性质：工程实施约束与设计任务输入，不是已经生成的类型/表/代码。产品由 [PRD第8节](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) 定义，安全不变量由 [06](06-permissions-and-data-safety.md) 定义，版本由 [08](08-migration-and-delivery.md) 定义。本文件把它们转换成待 A0/A1 验证、冻结的 Interface；执行 AI 不得越过前置任务自行发明第二套协议。

## 1. 必须先拿到的设计结论

| 结论 | 产出任务 | 最少证据 | 使用前提 |
|---|---|---|---|
| Pi/Node 精确版本、启动/回收与资源预算 | A0-02，A0-03补资源数据 | 依03 §1.1锁定；退出/双开/强杀/恢复实测、制品清单及09批准预算 | A3-01 |
| 模型传输 Adapter 与 provider 支持矩阵 | A0-03 | 正常/压缩/总结/重试/取消均受治理，未支持路径禁用 | A3-02 |
| 操作/收据/子任务幂等与失败窗口 | A0-04、A1-04/A1-05 | 临时文件/SQLite 故障注入和批准消费竞争结果 | A2/A4 副作用 |
| 总仓身份、路径定位、托管证据、迁移状态 | A0-05、A1-02 | 同名错目录、失联、链接逃逸、半切换均能解释 | A2-03、A4-05/A4-06 |
| 工作台最小窗口与跨页状态归属 | A0-06、A1-06 | 原型流程和状态草图，不依赖真实模型 | A3 UI |
| 个人流程内部结构和 Pi 加载 | A0-07、A4-07 | 模板参数验证、再次实例化、无脚本/旧批准 | A4-08 |
| 初始评估集与生产契约映射 | A0-04、A1-01 | 09 §5的8条case、可运行runner及故意错误检出；A3接真实工具 | A3-03、A4-08、A6-03 |

记录到对应权威架构/契约和任务报告，附方案选择原因与否决条件。缺失时返回设计任务，不能在实现中留 `any`、固定假成功或安全检查 TODO 作为替代。

## 2. Module 与数据唯一归属

目标Module、Interface和数据归属已在本轮规划承接到 [ARCHITECTURE第9节](../../docs/ARCHITECTURE.md#9-20目标架构待原型验证与实现)。该处统一维护模块和状态所有者；本文件继续提供待A0/A1冻结的最低输入/输出语义及实施检查，不另建第二份归属表。

## 3. 计划、批准、执行的传递

以下字段是最低语义要求，最终命名/序列化在 A1-01 固定，不直接照抄成未经测试的生产类型。

| 对象 | 必须可表达 | 校验重点 |
|---|---|---|
| CallerContext | 宿主生成的主体、会话/实例、委托链、requestId、当前授权与库世代 | 不取模型提交的 principal/approved |
| SelectionRef | 库身份、确切资产集合/版本、实际总量、截断/有效条件 | 不把当前页和旧聊天中的“那些”当完整集合 |
| Plan | 主体、能力版本、输入快照、目标/规则、外发服务、上限、关键条件、hash | 准备期间可读/检查，尚不执行批准后的效果 |
| Approval | 对应 plan/hash、主体/范围/期限/限制、消费状态 | 可信 UI 提交；hash/范围变化失效 |
| Operation | operationId、意图/幂等键、批准关联、步骤依赖、状态/阶段/版本 | 批准消费与 operation 建立原子化；同键同计划返回原 ID |
| Receipt | 固定 step/item 身份、输入指纹、目标占用、文件效果、DB/子任务关联与错误 | 崩溃后先对账；不是“已经处理 N 个”的计数猜测 |
| Event/Snapshot | operation/run/host 身份、revision/sequence、数据和终态 | 迟到/重复事件不倒退；缺事件时拉快照 |

主状态精确定义只在 06；UI 使用显示映射，不自创一套会被持久化的同名状态。准备计划的待审不提前创建 operation；任务运行到新增批准/标签审核时记录等待原因并释放执行资源。失败、取消请求中、已取消、部分成功和结果未知必须可区分。

取消的三个动作保持独立：停止 Pi 推理、取消业务剩余步骤、撤销已生效结果。复核者应以“停止回答后任务是否仍按批准运行”“取消竞争后真实成功项是否保留”检查，而非只看按钮标题。

## 4. 文件效果与托管定位

### 4.1 总仓与待处理记录

拟议最小数据概念为 LibraryIdentity、RootLocator、ManagedAssetLocation、OwnershipEvidence、LegacyResolution、MaintenanceReceipt；它们不必各自成为一张表。A0-05 比较绝对路径映射与受控相对定位，A1-02 固定一种可迁移方案。关键是稳定资产 ID 与正确定位，不以改字段名代替所有权验证。

旧记录分为：可验证托管、库外待处理、归属不明、当前总仓暂时不可用。失联是库健康状态，不能逐项误分类成用户待解除关联。迁移结构后先保护未核验项，再渐进核验，核验未完成不进行自动清理。

A2-03 处置规则：仅当前根已核验且路径已确认在根外的 `external_pending` 项可准备复制回仓；`unknown`、`missing` 只展示并要求人工核对，根离线/错误时所有逐项处置均禁用。列表和逐项操作都要重新核验当前总仓身份；数据库缓存的 `ready` 不足以放行总仓已失联后的操作。列表应区分路径不可访问（`offline`）与链接、标记或身份不匹配（`wrong_root`），两者都禁用处置。准备和确认解除关联时，还要复核原路径仍是库外普通文件且文件身份、大小和修改时间未变化；文件缺失或快照变化时保留记录并降为 `unknown`，不能解除关联。复制准备必须由可信文件选择器重新选中旧记录对应文件，并将当前打开文件的身份、大小、修改时间和完整 SHA-256 与路径及旧资产快照核对；大小或修改时间与旧记录不符时保留记录并降为 `unknown`，不创建处置收据。处置收据还必须持久化操作系统可报告的完整修改时间精度（Windows FILETIME 的 100ns 刻度可由纳秒值精确表达），并在解除关联确认、回仓复制/复核/发布/登记前后逐次比较；旧活动收据缺少该字段时迁为 `outcome_unknown`，不得按低精度时间继续执行。确认阶段在复制前与复制后、目标发布/资产登记前，都要复核源文件身份、原路径、大小、修改时间和摘要；任一项变化都不得登记目标，结果保留为待人工核实。`published` 收据恢复时，目标缺失、身份改变或内容无法证明必须转为 `outcome_unknown`；正式目标一律只读打开，Windows 登记期间拒绝写/删共享。登记所用目标身份、摘要必须与收据一致，数据库资产时间取固定目标句柄的最终快照；资产条件或事务 CAS 不符时回滚资产登记并将活动收据转为 `outcome_unknown`，避免反复确认卡在活动状态。可访问的 ready 总仓与收据根身份/epoch 不符、目标路径越根或发布证据失效时，prepared/copying/published 收据必须条件更新为终态未知；offline/wrong_root 时不改变收据，等待总仓恢复核验。确认前任何文件/资产状态不变。相同完整 SHA-256 的现有托管资产在预览中逐项列出 ID、名称、标签、收藏和评分。2.0 首版不合并不同资产记录：用户确认后作为独立资产复制，保留原 asset ID、全部标签/评分/关系，并使用不覆盖的唯一托管路径；延后则继续待处理。此类精确重复继续出现在重复素材分组中。

解除关联只写持久处置收据并从待处理投影中隐藏该条记录；不删除或改写原文件、assets 行及其标签/评分/关系。收据可将记录重新打开为待处理。复制回仓通过逐项维护收据记录准备快照、暂存对象身份/摘要、排他发布与数据库登记阶段；文件 IO 在 DB 锁外。复制只写入总仓保留的 `.legacy-stage` 下、由收据身份绑定的临时对象；完整验证源和暂存内容后，才以同卷硬链接原子且不覆盖地发布到正式路径。发布前重开暂存路径复核收据对象身份，发布后复核目标对象；目标身份不符或发布窗口内被替换时进入 `outcome_unknown` 并保留该目标，不根据刚观察到的路径身份删除无法证明归属的对象。源快照在复制前、复制后、发布前和资产登记前复核；任何变化都进入 `outcome_unknown`。重试只可重写身份仍匹配的暂存对象，正式目标永不打开写入；发布后登记失败可凭收据核验正式目标并继续。暂存身份未持久化、暂存对象被替换、正式目标被占用或替换、根快照变化、发布内容无法验证时进入 `outcome_unknown`，不覆盖或认领文件。路径身份复核不隔离同一 OS 账户下主动并发改写目录的对手，硬链接支持范围见 [PLATFORM](../../docs/PLATFORM.md#9-20-agent平台增量)。服务只接受 GUI 可信入口，不注册 Agent/MCP。

托管验证覆盖真实路径、目录链接/大小写、文件替换和目标创建竞争。路径字符串校验后仍需防时间间隔内的替换；采用平台可用的句柄/不跟随链接/创建排他语义，在平台矩阵中证明。若某平台无法满足某动作，明确禁用该动作，不放开范围。

### 4.2 逐项副作用最小顺序

| 操作 | 行为顺序与恢复判据 |
|---|---|
| 复制导入 | 固定来源/目标规则 → 暂存复制并核验 → 排他发布目标 → 登记资产/收据 → 展示结果；输入变化或重复请求不得写错素材 |
| 复制导出 | 固定库内来源与授权输出目录 → 新目标创建/校验 → 记录收据；始终保留库内文件及资产定位 |
| 库内移动 | 核验两端与资产版本 → 保留可恢复意图 → 文件移动/受控复制阶段 → 更新定位/收据；任何半完成状态可核实 |
| 软删/恢复 | 固定资产集合和版本 → DB 更新回收站状态 → 查询真实快照；恢复前核实可用文件，不假装恢复被外部删除的文件 |
| 独立清空 | 固定已软删且归属明确的集合 → 独立批准 → 每项复检 → 永久文件效果及收据 → DB/缓存清理；出现新同名文件时不误删 |
| 到期清理 | 宿主维护身份查询到期合格项 → 同一删除核验/收据；模型不能伪造维护身份或更改到期条件 |

具体断点在 09 测试。SQL 事务只覆盖 DB，自始至终不承诺磁盘效果自动回滚。副作用未发生前若输入/目标变化，形成新计划；已经发生部分效果时返回真实部分结果，后续调整是新计划。

### 4.3 迁仓和重新定位

维护门依次执行：冻结冲突新任务 → 建立当前清单/备份 → 预检目标/归属 → 可恢复搬迁或已搬内容核验 → 原子切换定位/新世代 → 重建必要投影 → 报告残留。具体源端清理时机由 A0-05 冻结并由可信 UI 明示，不删除无托管证据的夹杂文件。

重新定位不复制，不通过清空再重新导入实现；保留 ID、标签、评分和历史关联。软件迁移也不把缓存/凭据/会话误当作都在素材目录中。库迁移后旧计划/句柄失效，已记住目录不能因根变化扩权。跨操作系统迁仓不是首版承诺。

### 4.4 AI 打标计划与外发

A2-04 的唯一入口契约由 `services::tagging` 承载。导入来源按 `(sourceOperationId, sourceStepId)` 读取逐项 operation item 与该项最新同 revision 收据；仅接受 `succeeded + registered` 且事件为 `item_imported` 的素材。`duplicate_mapped` 只有在可信 GUI 显式纳入对应 item ID 后接受。失败、未知、未完成、收据不匹配和已不属于当前总仓的资产不得进入集合；asset ID 去重时仍保存全部来源收据关联。

计划快照冻结准确 asset/receipt 列表、重复项纳入选择、连接元数据（不含密钥）、模型、提示词版本、图片处理、视频策略、分类上下文、候选词快照、请求次数上限及 3600 秒批准时长。HTTP(S) 连接端点的用户信息、查询参数值和 fragment 不写入快照或审批摘要；连接修订指纹仍覆盖真实端点，启动时必须与当前配置一致，再由宿主读取真实连接和凭据用于请求。以上摘要参与计划 hash。连接/分类/素材来源发生变化时，submit 或 start 返回 `PLAN_CHANGED`，重新准备并审核。请求上限按当前图像请求协议的最坏路径计算并在每次真实网络发送（含 Ollama 恢复请求）前以条件更新持久计数；批准时长从首次显式启动起持久计时，恢复不重置，限流等待与单次 HTTP 超时均受剩余时长限制。发送结果不明时不退还。供应商请求与 Ollama 恢复请求统一通过 `authorize_tagging_request_send` 执行取消检查和持久预算扣减，新增外发路径必须复用该门禁。模型失败只记录 `last_error` 并保持建议 pending，不能替代用户拒绝；模型结果与用户确认/拒绝并发时，仅当建议仍 pending 才允许写入。取消完成后 operation/step/link 同步为 cancelled，未完成建议继续保留 pending，后续执行需从剩余范围重新准备并单独批准。

获批 submit 在同一 SQLite 事务消费批准、创建 operation/step、创建 AI pending batch（仅导入来源）或关联用户已建的 pending batch，并写入唯一范围化 child-task 和来源映射。相同来源步骤重放取回同一 operation/batch；相同来源但素材/重复项集合不同则冲突。已完成的有界批次范围之外仍有待生成素材时，批次保持 pending，剩余素材可通过新范围、新计划和新 operation 再次审批；一个批次可关联多个按序完成的范围。该事务不执行网络 IO。只有可信 GUI 的 `start_tagging(operationId, stepId)` 能启动网络；原裸 `ai_start_batch(batchId)` 不得作为联网入口。AI batch 是建议与执行状态的唯一事实，operation 只保存编排和 child-task 引用。

## 5. 前端状态及界面组合

建议持久关联键：conversationId、draftId、planId、operationId、batchId、workflowId/version。哪些对象需要独立表在 A1-06/A4-07 冻结；每个 UI 通过这些 ID 订阅，不按“最近一个全局任务”取消或刷新。

- 手动入库草稿与会话草稿可经显式交接绑定；交接不重建已经受理的操作。两个页面读取同一绑定后共享状态，不同会话默认隔离。
- WebView 拖拽由一个入口分派到附件/导入区域，重渲染不重复订阅。附件可以不入库；持久附件记录引用与可访问性，不能默认上传或保存整盘授权。
- 右侧按当前会话/用户明确选中的工作台显示；后台进度更新任务卡，不抢当前右栏。素材库临时搜索结果视图有明确进入和退出语义。
- 标签到正式库的确认由可信共享审核入口完成，Agent 只能准备和打开；批次版本变化时重新读取候选，不能覆盖用户后来的编辑。
- 导航贡献项在 2.0 为封闭内置目录。配置只保存稳定功能 ID、显示和顺序，未知/失效 ID 安全忽略并可恢复；无任意路由/脚本字符串执行。
- 工作状态区与完整任务详情消费同一快照；进度未知时不计算假百分比；轮播暂停、键盘焦点和减少动态效果按现行可访问性规范落实。

已确认具体UI见PRD第8节（02保留跳转）；控件尺寸、溢出形式等用 A0-06 原型决定，A3 继承结果，不借实现改回旧导航或删除独立入库页。

## 6. 模型与个人流程

模型传输候选为 A0-03 验证过的自定义 `fetch` → 宿主私有 IPC → Rust 网关路线。A0-03 使用独立 PoC 可执行文件，并未验证生产 Agent Host 的 stdio 复用；生产拓扑须经 A3-02 设计复核和本卡生产路径 fixture 测试。第一批只展示经验证的服务协议/模型能力；检查普通对话和辅助请求，真实请求都计入连接限流/预算。Pi 资源加载限定应用目录，凭据只经宿主，模型读取到的文件名/标签/图片文字保持数据角色。

`super_search`、`tagging` 与 `agent` 是三个互不继承的连接用途。新增 `agent` 绑定必须使用追加幂等迁移，保留既有用途绑定但不把旧 active profile、搜索或打标连接自动设为 Agent 默认；未显式绑定时返回未配置状态，不静默回退。每次模型发送都由 Rust 按当前用途绑定重新核实连接启用状态、协议、模型、规范化端点和配置修订，再从系统凭据服务取密钥并使用同一 connection limiter；Node 和前端只收到短期网关访问材料，不接收真实密钥。

### 6.1 A3-02 Agent 模型网关接线

**证据边界。** A0-03 的 `custom fetch` 通过另一个 PoC Rust 可执行文件的 stdio 做过隔离 loopback HTTP/HTTPS 测试；它没有调用生产 `packages/agent-host` 或 `AgentRuntime`，不能代替 A3-02 生产路径证据。当前 Windows x64 实现已将 Pi provider 自定义 `fetch`、Host 私有 stdio、Rust `AgentRuntime` 与 `GatewayBroker` 接通；锁定 Pi 1.0.4 的生产 Host 本机 fixture 已通过普通文本请求端到端测试。Rust 生产配置装配 `SwitchableGatewayTransport`（真实 HTTP），fixture transport 只在测试中访问内存数据，不出本机。该证据证明本机 codec/IPC/租约/流桥接，不证明外部 endpoint 行为、真实凭据、代理/TUN、安全 DNS 或费用政策。

**2026-11-01 产品负责人裁决（解除用户可见的联网开关）。** 本节原先以「生产 capability 必须保持关闭」挡住正向外发；该挂起理由现已作废，因为用户要求与其他 Agent 一致：**配置好 API 即可直接对话**，权限分级只用于控制文件操作档位（§6.2）。因此：

- 外发准入改为**由配置推导**：`agent` 用途存在绑定、连接启用、密钥可用即视为就绪；
- `settings.agent_model_network_enabled` 字段、「设置 → AI 服务 → Agent 联网」开关与 `get/set_agent_model_network` 命令一并删除；`SwitchableGatewayTransport` 只保留代理这一个用户配置项（`settings.agent_model_proxy`，命令 `get/set_agent_model_proxy`）；
- `GatewayBroker::preflight` 仍先做廉价检查并按固定错误码失败关闭：网络出口未就绪 → `AGENT_MODEL_NETWORK_DISABLED`（历史码名，语义改为「配置未就绪」）、未绑定 → `AGENT_SERVICE_UNBOUND`、云端缺密钥 → `AGENT_CREDENTIAL_MISSING`；
- 未关闭的开放项移交 [A3-11](execution/A3.md#a3-11)：真实 provider 与真机证据、DNS/TUN 边界结论、run 与费用预算的用户可配置默认值。

**握手与 lease。** 外层继续使用 `protocol: 1`、Host 实例 ID 和 Host→Rust 严格递增的全局 `sequence`；Rust→Host 命令/回复继续用唯一 `requestId` 配对，不虚构 v1 不存在的反向全局序号。Host 以精确 `schemaVersion: 6` 握手，`ready` 精确声明 Host 侧注册的完整工具集合（16 项，见 §6.2 与 A3-03）、`modelNetwork: false`、`gatewayProtocol: 1`；`conversation.open` 另带 `permission` 与按档位裁剪的 `tools`，Host 照单注册并在 ack 回读，Rust 逐项断言（见 §6.2）。schema v6 增加 `settled` 事件的可选 `stopReason`/`rawStopReason`/`usage` 字段与只读 `conversation.history` 命令（见[内部协议 §3/§5](../../docs/contracts/agent-host-protocol-v1.md#3-命令)）；schema v5 增加会话权限档位（见[内部协议 §3](../../docs/contracts/agent-host-protocol-v1.md#3-命令)）；schema v4 增加能力调用帧（见[内部协议 §4](../../docs/contracts/agent-host-protocol-v1.md#4-能力调用schema-v4)），Rust 只用可信 Host 身份与当前活动 run 构造调用方上下文。schema v3 为 run 错误增加可选且封闭的 `gatewayError: {code, status?}`，以便把固定网关类别和安全 HTTP 状态送到 UI；schema v2 不得发送该字段。`modelNetwork: false` 仍表示 Node 没有直接网络能力；新增 gateway capability 只表示可向 Rust 请求带 lease 的有限 HTTP 流，不授予 Node socket、DNS、代理或任意 URL 能力。旧 schema/未知字段必须失败关闭。Rust 只在可信 GUI 对话发送受理前读取 `agent` 用途唯一显式绑定，并固定 hostInstanceId、conversationId、runId、connectionId、`config_revision`、协议、模型和 run 截止点。Rust 发给 Host 的 `conversation.send` 必须含精确 `gatewayLease: {leaseId, provider, protocol, model, contextWindow, maxOutputTokens, reasoning}`，`reasoning` 为真且本次 run 显式选了思考档位时另带 `thinkingLevel`；只有 leaseId 是不透明随机引用，provider/protocol/model 只供 SDK 选择受限 codec，`contextWindow`/`maxOutputTokens` 来自连接配置（`ai_connections` 的幂等列，缺省回落 32768/4096、上限 4,000,000/8192），`reasoning` 来自连接的 `reasoning_supported`，`thinkingLevel` 取值域 `off/minimal/low/medium/high/xhigh`（Rust 归一化，未知档丢弃不下发）。Host 用这些值设置 Pi `model.contextWindow`/`model.maxTokens`/`model.reasoning`，并在 run 开始前用已注册的 Pi `session.setModel` 切换到 lease 对应 model；无法解析的 provider/model 失败关闭。每个 gateway frame 都带 `conversationId`、`runId`、`leaseId` 与唯一 `gatewayRequestId`，并与对应 Host 实例严格匹配；分块/拉取另用每 request 连续序号。所有 Host frame 每次通过内存 lease 状态确认归属；每个 request commit 在出网前再次读绑定/连接并校验 revision、enabled、protocol、model；每个响应 pull 在读取和返回块之前也只读复核当前绑定与 revision，变化或 run 截止即撤销整个 lease，并丢弃尚未送达的块。校验失败时不读取 Keyring、不计上游 attempt、不自动切换连接。验证配置后释放 SQLite 锁，再由 Rust 凭据服务读取 Keyring 并在 Rust 内注入上游头。Node、前端和日志均不得获得真实凭据、服务端 URL 或完整鉴权头。生产传输响应必须提供可重复调用的取消钩子，使正在阻塞的 body read 能及时中断；取消后释放响应和 limiter permit，不能让 Host shutdown 或 run cancel 等待网络读取结束。

**v2 私有 stdio 帧和状态机。** 所有帧继续服从 v1 有界 newline-JSON 与精确字段校验；Host→Rust 帧有 Host 实例和严格递增全局 `sequence`，Rust→Host 回复继续以 `requestId` 配对，不新增全局反向 sequence。除此 envelope 外，所有 gateway frame 必须精确携带 `conversationId`、`runId`、`leaseId` 和 `gatewayRequestId`；表格列出各类型额外字段。Host 每个 gateway 请求 ID 使用 `gateway_<base36 counter>_<Host instance 前12字符>`，counter 在本实例内严格递增；Rust 以常量空间保存高水位并拒绝重复/倒退 ID，避免为长会话保存无界集合。单个 HTTP attempt 使用一个从不复用的 `gatewayRequestId`，状态只按 `opening → uploading → committed → streaming → ended` 前进；任意非终态可进入 `cancelled` 或 `failed`，终态帧和迟到数据只能被丢弃/诊断，不能重新打开 attempt。

| 方向与 type | 精确新增字段 | 规则 |
|---|---|---|
| Host→Rust `gateway.request.open` | `route`, `method` | `route` 只可为 `openai_chat` 或 `anthropic_messages`；method 必须是 `POST`。禁止 URL、headers、代理、重定向和凭据字段。Rust 以同一 `requestId` 回复首个 `gateway.request.credit`。 |
| Rust→Host `gateway.request.credit` | `nextChunk`, `maxBytes` | 只确认下一个连续上传块；任一时刻最多一个未消费 credit。必须配对触发该 credit 的 Host requestId。 |
| Host→Rust `gateway.request.chunk` | `chunkSequence`, `base64` | 按 credit 逐块上传；拒绝重复/跳号、无 credit、非法 base64 或过长块。Rust 以同一 requestId 回复下一块 credit。 |
| Host→Rust `gateway.request.commit` | `chunkCount`, `byteLength` | 仅在 Rust 实收长度精确匹配且 JSON/路由/lease/body 全部验证通过时提交上游请求；Rust 以同一 requestId 回复 `gateway.response.head` 或固定错误。 |
| Rust→Host `gateway.response.head` | `status`, `contentType` | 仅返回安全状态和 `text/event-stream`；禁止上游敏感 headers/body 错误文本。重定向作为固定拒绝错误。 |
| Host→Rust `gateway.response.pull` | `pullSequence` | Pi 的 Response reader 每读取一块才发下一个连续 pull；最多一个未满足 pull。Rust 以同一 requestId 回复 chunk/end/error。 |
| Rust→Host `gateway.response.chunk` | `pullSequence`, `base64` | 只为相符 pull 返回一个有界块；禁止未请求预取。 |
| Rust→Host `gateway.response.end` | `usage` | 关闭 reader、释放限流 permit并结束 attempt。usage 为可空快照 `{inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens}`；Rust 仅从支持的 OpenAI/Anthropic SSE usage 事件生成非负整数，缺失或无效即 `null`，绝不由 Node 补报为零。 |
| Rust→Host `gateway.response.error` | `code`, 可选 `status` | `code` 只能是固定类别：`ROUTE_DENIED`、`LEASE_REVOKED`、`CONNECTION_CHANGED`、`REQUEST_INVALID`、`REQUEST_LIMIT`、`BUDGET_EXCEEDED`、`UPSTREAM_UNAVAILABLE`、`UPSTREAM_HTTP`、`REDIRECT_DENIED`、`STREAM_FAILED`、`CANCELLED`、`TIMEOUT`。不传异常原文、URL、上游 body 或响应头。 |
| Host→Rust `gateway.request.cancel` | 无 | Host 取消精确 attempt 时发送；Rust 以同一 requestId 回复 `gateway.request.cancelled` 并关闭该 attempt 的请求体、上游响应和 pull wait。run 取消还由 Rust 撤销该 run 的全部 lease/request。取消 ACK 只证明本地停止读取/发送，不证明已送达上游的生成被撤回。 |

Request frame 最大 64 KiB；上传块解码后不超过 44 KiB（base64 与 envelope 后仍须小于帧限制），总请求体不超过 2 MiB；每 Host 最多 8 个活动 attempt，每个响应最多 1 MiB。额度是本契约的实现硬上限；A3-02 必须以生产 Host fixture 测试和内存/延迟测量确认，不能用 A0-03 PoC 数字代替。Host→Rust 全局 sequence 与每请求 chunk/pull sequence 均单调；request ID 是单次 Host 实例内 one-shot。Host 重启、run 重试或 UI 重发均新建 run/lease/request ID；发送结果未知时保留原 run 并精确取消/核对，不重放提示词。

**对话请求：文本保真 + 撤销正向 allowlist（2026-11-02 用户裁决，取代旧正向 allowlist）。** 用户裁决：对话内容与联网搜索不受限，只有本机文件操作（尤其移动/删除）需人工确认；权限分级只控制文件操作档位（§6.2），不再用来卡对话。因此网关不再用“只接受某几个顶层键”的正向 allowlist，改为**黑名单 + 透传**：请求体经 `serde_json` 解析后，顶层与消息级对象只要不含危险键即整体透传，未知或新增的 provider 参数不再一律拒绝。保留的安全边界有三类，且都是结构性约束而非枚举白名单：(1) **内容保持纯文本**——`content`/`system` 仍只接受字符串或精确 `{type:"text", text}` 块，不接受图片/音频/视频/文件等二进制块（防止不可信 Node 把任意本地字节 base64 外发；图片参与处理走打标计划，见 §9.14）；(2) **身份/流/预算不放松**——`stream: true`、`model` 与 lease 逐字一致、最多 512 条消息、字符串/数组受 2 MiB 总上限、恰好一个 max-token 字段且 `1..=8192`、temperature 有界，`stream_options` 若出现仍强制 `{include_usage: true}`；(3) **危险键黑名单 + 成本/legacy 拒绝**——键名归一化（去非字母数字并小写）后命中 `apikey/authorization/auth/headers/url/endpoint/baseurl/secret/password/token/credential/bearer/proxy` 等即拒（凭据/端点/代理/header 在当前架构结构性不可达，此层防未来误用），并拒绝成本放大与 legacy 入口（`n>1`、`best_of`、`functions`、`function_call`）；黑名单只作用于请求/消息/工具对象的直接键，不递归扫描 `tools[].parameters`（其安全由能力门负责）。理由：旧 allowlist 让任何 provider codec 的新字段（首当其冲是 **assistant 回合回放的 `reasoning_details`**）触发 `REQUEST_INVALID`，使多轮对话必然中断。**推理元数据放行**：assistant 消息允许 `reasoning_details`（数组≤64、每元素须为 object 且有 string `type`、单元素≤64KiB）与 `reasoning_content`（string）；Anthropic assistant 消息允许 `thinking`/`redacted_thinking` 块（`thinking` 需 `type`+`thinking` 字符串、`signature` 可选 string≤64KiB；`redacted_thinking` 需 `type`+`data` 字符串≤64KiB）。**工具声明**仍受能力门：名称须属本轮注册集合、唯一、描述有界，但 `parameters`/`input_schema` 作为任意 JSON 对象透传（≤32KiB），并**允许空 `tools: []`**（Pi 在「有工具历史但本轮无工具」时会发空数组）；越档或未注册调用仍由 `capabilities::execute` 的能力门与档位门在 dispatch 前独立拒绝。每次 Pi `streamSimple` 由可信 adapter 显式指定 `cacheRetention:"none"` 和固定 `maxTokens≤8192`（自适应默认 4096），忽略而不读取 `PI_CACHE_RETENTION` 等用户环境覆写；fixture 必须真实调用锁定的 Pi 1.0.4 codec，并断言完整请求体不包含 cache hints、危险键或二进制内容块。

| `route` | endpoint 与 request body 处理（黑名单 + 透传；content 保持纯文本） | 明确拒绝 |
|---|---|---|
| `openai_chat` | Rust 将连接 base URL 的路径前缀原样保留并直接追加 `/chat/completions`，不再自动补 `/v1`（必要时由用户自行在 base URL 中内含版本前缀，如 `/v1`、`/api/paas/v4`、`/api/v3`），与 `docs/ARCHITECTURE.md` 和 `gateway.rs` 的 `Route::endpoint_url` 实现一致（2026-11-02 契约负责人裁决）。顶层透传并按黑名单拒绝危险键与成本/legacy 参数；`stream` 必须为 `true`、`model` 与 lease 逐字一致、≤512 条消息。`stream_options` 仅 `{include_usage: true}`。消息 role 为 `system/developer/user/assistant/tool`：`system/developer` 的 `content` 必须字符串；`user` 为字符串或精确 `{type:"text", text}` 数组；`assistant` 的 `content` 可为 `null`（须带 `tool_calls`）/字符串/文本块，并允许 `reasoning_details`、`reasoning_content`；`tool` 需字符串 `content` 与 `tool_call_id`。`content` 保持纯文本，图片/音频/视频/文件块一律拒绝。恰好一个 max-token 字段，范围 `1..=8192`；temperature 为有限数且 `0..=2`。`tools` 声明见 §6.1 上文规则（可为空数组）。Pi model definition 仍禁止 prompt-cache 与额外 payload 字段。 | 危险键（`api_key`/`authorization`/`headers`/`url`/`endpoint`/`base_url`/`secret`/`token`/`credential`/`bearer`/`proxy` 等，键名归一化后匹配）；`n>1`、`best_of`、legacy `functions`/`function_call`；非文本 `content` 块（`image_url`/`input_audio`/`file`）；跨档或未注册的工具名；缺 `stream`/`model`/`messages` 或 token 越界。 |
| `anthropic_messages` | Rust 将连接 base URL 的路径前缀原样保留并直接追加 `/messages`，不再自动补 `/v1`（必要时由用户自行在 base URL 中内含版本前缀，如 `/v1`），与 `docs/ARCHITECTURE.md` 和 `gateway.rs` 的 `Route::endpoint_url` 实现一致（2026-11-02 契约负责人裁决）。Pi SDK 1.0.4 的 Anthropic beta client 会在 Host 占位地址上生成精确 `?beta=true`，受信 fetch 只映射这个固定组合，IPC 只传 `route` enum，Rust 不转发该 query。顶层透传并按黑名单拒绝危险键与成本/legacy 参数；`stream` 为 `true`、`model` 与 lease 一致；`max_tokens` 必须恰好一个且 `1..=8192`。`system` 可省略或为最多 64 个精确 `{type:"text", text}` 块。消息 role 只 `user/assistant`，`content` 为纯文本字符串或精确 `{type:"text", text}` 块数组；assistant 消息允许 `thinking`/`redacted_thinking` 块。temperature 为有限数且 `0..=1`。每次 Pi `streamSimple` 必须显式设置 `cacheRetention:"none"`（Pi 1.0.4 默认 `short` 会加入 `cache_control`）；Pi model definition 仍禁止 OAuth 与额外 payload 字段。 | 危险键；`n>1`/`best_of`；`betas`、cache control、未知 beta header；图像/文档块；OAuth；跨档或未注册的工具名；缺 `stream`/`model`/`max_tokens` 或越界。 |

Rust 不信任 Node 的 `url`、headers、model 或内容类型。URL 由规范化且已批准的连接与固定 endpoint 生成；HTTP client 关闭 redirect、自动 retry、环境代理继承与不受控 DNS 重解析，TLS 证书验证开启。非 2xx 响应只给 Host 固定错误类别与安全 HTTP 状态；不把供应商错误 body 送入 UI/日志。对外部 DNS、系统代理、TUN、loopback/私网和绑定到公共地址的 DNS rebinding 尚无生产证据。按 2026-11-01 裁决，这些不再作为产品门禁，而是 [A3-11](execution/A3.md#a3-11) 的开放项；现有缓解是：endpoint 只由用户自己填写的连接生成（不存在「不可信输入驱动 URL」的 SSRF 面）、`redirect::Policy::none()` 不跟随跳转、默认 `no_proxy()` 不继承系统/环境代理、代理地址经 `validate_agent_proxy_url` 拒绝账号密码/路径/query/非 http(s)，且 loopback 被**显式允许**以支持本机 Ollama。fixture-only 传输仍不得访问非 fixture 目标。

**SDK 调用覆盖与关闭表。** 下表是实现范围；测试必须把 Pi 1.0.4 各路线请求映射到同一 lease、schema 与 connection limiter，不能只测首轮 prompt。

| 请求来源 | A3-02 初始行为 | 测试/后续 |
|---|---|---|
| 普通文本对话 | 通过上述黑名单+透传与自定义 fetch 开放（content 仍纯文本） | 每次 attempt 同 lease、同 connection limiter 和同 run deadline；仅隔离 fixture 端到端测试。 |
| 自动压缩与人工 compact | 自动压缩开启（复用 Pi 内置三路：阈值/overflow/可恢复 length；不自研）；Host 命令面仍不暴露人工 compact | Pi 1.0.4 阈值、overflow 与 `length` 三路均须 fixture 证明：压缩只走 Host 内置 `compaction_start`/`compaction_end`，不新增自研摘要；人工 compact 无入口。若未来新增人工入口，须另立契约并证明与自动压缩互斥。 |
| 分支 summary | 未由生产 Host 暴露，禁用 | 不得在主聊天 custom fetch 外发请求；要开放时加同协议 fixture。 |
| overflow/length 自动 retry 与 Pi provider retry | Pi 内置压缩的 overflow/可恢复 length 恢复只允许一次（`_overflowRecoveryAttempted` 防二次），复用同一 lease/预算/limiter；429 与 Pi provider retry 关闭，不重复发送结果未知请求 | 合成 429 断言只产生一个 `gateway.request.open`；双协议 Pi fixture 确认 `length` stop 后最多一次压缩恢复（同 lease），且 429 无重试。显式重试必须新建 run/lease。 |
| 工具 follow-up | 开启，`tea_assets_search`、`tea_assets_trash_prepare`、`tea_assets_trash_submit`、`tea_operations_get`、`tea_operations_cancel` | A3-03 的工具声明、tool result 与后续轮次均由能力契约和内部协议 §4 约束；Node 只注册 Rust 已注册的工具，声明集合不一致即握手失败。`assets.trash.prepare` 只生成待审计划、不删除任何素材，`assets.trash.submit` 必须消费 GUI 批准并在同一事务内软删精确选择集并结算操作，未批准返回 `APPROVAL_REQUIRED`、集合漂移返回 `PLAN_CHANGED`；`operations.get`/`operations.cancel` 复用 `services::plans`/`services::operations` 的归属与取消语义，不新增写路径；其余能力随各自 adapter 逐项加入。 |
| model discovery/catalog、OAuth/token refresh、telemetry | 禁止；不读取 Pi 用户配置、credential store 或在线模型缓存 | 所有出网均需由 A3-02 专门 fixture 覆盖后另行解锁。 |
| model switch、用户明确 retry | 已运行 run 固定原 lease；下一次明确 send 新建 lease，并在启动 prompt 前通过 Pi `session.setModel` 更新已选 model；不自动切换/重发 | 连接解绑、停用、revision 变化后旧 lease 立即失败关闭；若新 protocol provider 未预注册则拒绝并要求重开 session。 |
| cache warming、WebSocket、未知 endpoint/provider | 禁止 | 未列入正向路由不得触网。 |

**限流、预算与观测。** 单次 attempt 顺序是：固定 lease/配置快照并校验 → 受控 worker 等待 connection ID 共享 `ai_rate_limit` permit → 最终复核 lease/连接 revision、取消状态和截止时间 → 原子消费 run 的 attempt slot → 开始一次上游 HTTP 请求。额度等待不能阻塞 Tauri 异步执行器；等待取消/超时不消耗 attempt slot。attempt slot 一旦提交即使连接失败或结果未知也保留；permit 在成功、HTTP 错误、取消和 transport 失败时均只释放一次。配置更新不能替换 limiter 或清空仍有效的并发与时间窗。A3-02 允许的本地 fixture run 固定硬预算为 8 次 HTTP attempt、单 run 180 秒、每 attempt 请求体 2 MiB/响应体 1 MiB、每 attempt max output 默认 4,096、硬上限 8,192 provider tokens；预算和计数在 Rust 持有，重置只由新 run 完成。供应商 usage 原样解析为有限整数用于对账；缺失/解析失败记为 `unknown`，不得按零计算费用。连接限流不是持久费用账本。生产 run/费用上限的用户可配置默认值和确认政策以 [06 §8](06-permissions-and-data-safety.md#8-网络凭据与预算) 的「Agent 外发默认预算」为唯一权威出处，本契约不复制数值。按 2026-11-01 裁决，该项缺席不再阻止外发：当前实现的硬上限即为事实上限（均由 Rust 常量持有）。[A3-11](execution/A3.md#a3-11) 负责在该表基础上补齐用户可配置默认值与确认政策。

**验收边界。** 本卡可在本地测试中使用随机 fixture 响应、内存凭据和明确 `cfg(test)` transport，验证 OpenAI/Anthropic 文本格式、cancel、backpressure、redirect 拒绝、代理哨兵不读取、绑定/revision 变化失败、每 attempt permit 和 run 预算。开发阶段不得连接真实 provider、用户 Keyring、系统/环境代理、TUN、素材或用户现有 endpoint。真实 provider/隔离新凭据、系统代理、TUN、费用与上线前辅助请求覆盖仍须独立验收；按 2026-11-01 裁决，这些是 [A3-11](execution/A3.md#a3-11) 的开放项，不阻断 A3-02 关闭与 A3-03 解锁。历史/PoC 成功不代替生产路径证据。

个人流程格式只表达受支持能力与参数、输入输出依赖、版本和说明；不预先做任意脚本 DSL。一次成功任务的具体批准、素材句柄和连接密钥不进入模板。每次实例化重取输入/权限、生成计划；清空与正式标签审核保持单独用户动作。流程编辑使用新版本，历史引用原版本，不“热改”运行中步骤。

2.5 插件格式仍由 B0 重新评估。现在可固定的只有真实业务 Interface 与封闭内置入口配置，不创建无消费者的插件运行器/安装表。

### 6.2 Agent 会话权限档位（A3-10）

本节是「Agent 会话权限档位」的**唯一权威定义处**，其他文档只链接，不复制。

**定位。** 档位回答「这次对话里模型能用哪一层工具」，**不回答**「数据能不能离开本机」。后者由 §6.1 的网关准入判定，两者正交：`Read` 档只调用 `assets.search`，而它恰恰必须外发。因此不设「联网开档/闭档」，也不把档位并入网关开关。

**三档枚举。** `AgentPermission::{Read, Standard, Full}`，中文名「仅阅读 / 可执行改动 / 完全权限」，默认 `Standard`。判据取自能力描述表的 `AccessKind`（`src-tauri/src/contracts/agent_enums.rs`），`Standard` 在此之上再排除会改变素材库文件布局的两族：

| 档位 | 判据 | 暴露给模型的工具 | 数量 |
|---|---|---|---|
| `Read` | `AccessKind ∈ {Query, Cancel}` | `tea_assets_search`、`tea_operations_get`、`tea_operations_cancel`、`tea_workflows_load` | 4 |
| `Standard` | 除 `assets.move.*`、`assets.trash.*` 外的全部 | 上述 4 项 + `tea_tagging_prepare/submit`、`tea_exports_prepare/submit`、`tea_assets_restore_prepare/submit`、`tea_workflows_prepare/submit` | 12 |
| `Full` | 全部 | 上述 12 项 + `tea_assets_move_prepare/submit`、`tea_assets_trash_prepare/submit` | 16 |

`Standard` 排除移动与删除的理由：这两族是唯一会改变既有素材在库内位置的写操作，其余写操作只改数据库记录（打标、流程）或只写出副本（导出）、只把素材从回收站放回（恢复）。因此默认档足以完成「搜索 → 打标 → 导出」这条主链路，而「挪动或删除用户的文件」必须由用户显式升档。**三档都只是能力可见性上限，不改变任何审批语义**（见下段），三档各自都能独立走完自己允许的完整流程——每个被暴露的 `*.prepare` 都同时暴露其 `*.submit`，不存在「能准备不能提交」的死档。

**档位不允许三层同时缺一。** 判据是「`descriptor.access` + 一个显式声明的两族排除集」，排除集写在 `src-tauri/src/services/agent_runtime/permission.rs` 并由测试逐名固定（`Read` 精确 4 项、`Standard` 精确 12 项、`Full` 精确 16 项）。这条测试是防漂移的唯一手段：将来新增能力必须同时回答它属于哪一档，否则测试立刻失败。

**档位不改变任何审批语义。** `plans.approve`、`operations.resume`、`trash.empty.prepare/submit`、打标批次开始在任何档位下都保持 GUI 专属，`Full` 亦不例外。理由有三：其一，`approval: Plan` 与 `approval: PermanentTrash` 是描述表的静态属性，运行时按档位改写它等于让模型行为改写契约；其二，`approve_plan` 的归属链要求 `review_ref`（仅经 `PlanPreview` 返回给准备方）与 `origin_kind == "pi"` 时的 `principal_id == PI_HOST_PRINCIPAL` 且会话匹配，该链是结构性的，不依赖档位检查；其三，[06 §10](06-permissions-and-data-safety.md#10-提示注入与外部内容) 要求外部内容不能「批准自己」，`Full` 若等于自批，一次文件元数据注入即可触发批量写入。若要自动批准，须作为独立第四档另立契约并单独安全评审，不在本节范围。

**两道门。** `conversation.open` 按档位裁剪工具表为**体验门**，使模型不尝试不可用工具；`capabilities::execute` 在 `dispatch` 前按档位再判为**安全门且权威**。安全门必须存在，因为 Host 是独立子进程，`capability.request` 帧内的 `capabilityId` 属外部输入，Node 侧被篡改或 SDK 意外暴露未裁剪工具时 Rust 仍须拒绝。安全门判据由 `descriptor.access` 推导，描述表缺项时**不放行**，手法与 `ensure_maintenance_allows` 一致。

**注入与作用域。** 档位由用户在当前会话显式选择，随 `conversation.open` 帧下发，**不接受 Host 或前端在能力调用帧中回传的档位字段**。Rust 把「会话 → 档位」记在 Host 进程句柄的内存表里，不写入 `conversations` 表持久化，关闭会话即删表项；新会话回到 `Standard`，不跨会话记忆，避免宽档被静默沿用。

**变更时序。** 换档一律走「关闭该 Host 会话 → 用新档位重开」，因为 Pi 会话的工具表只在创建时裁剪。重开前安全门仍按旧档判定，且该窗口内不允许发起新 run；在途 run 期间换档入口禁用，不中断在途 run，也不清空输入草稿。重开失败时档位表项已被关闭动作删除，安全门按「无档位」拒绝一切能力调用并提示重新打开会话——显示值与真实生效档位因此不会分叉。

**无档位即拒绝。** 安全门只放行「有明确档位记录」的会话；档位表缺项时拒绝全部能力调用，而不是回落到某个更宽的档。

**握手校验。** `ready` 帧的 `tools` 保持**等于 Rust 侧注册集合的精确 16 项**不变：`host.hello` 早于任何 `conversation.open`，此时不存在会话档位，把 `ready` 改成档位子集会让握手与档位互相循环依赖。档位裁剪发生在 `conversation.open`（Host 按 `permission` 创建只含该档工具的 Pi 会话），`ready` 的校验因此不随档位变化；被篡改的 Node 若在能力调用帧里送来档位外能力，由安全门拒绝。版本仲裁见[内部协议 §7](../../docs/contracts/agent-host-protocol-v1.md)。

## 7. 候选代码落点

下列名称最初用于**拟建落点**，不作为实现进度或事实状态；任务卡开始后以代码、对应权威架构/契约和 `docs/PROJECT_PLAN.md` 为准。文件已存在时应复用其职责，不因本表旧路径重新建模块或扩成空壳。

| 候选新增职责 | 现有接入源码 | 建议位置 |
|---|---|---|
| 授权/计划/操作 | `src-tauri/src/state.rs`、`src-tauri/src/commands/`、`src-tauri/src/services/importer.rs` | `src-tauri/src/services/authorization.rs`、`src-tauri/src/services/operations/`、`src-tauri/src/db/operations.rs` |
| 单仓管理/维护 | `src-tauri/src/db/assets.rs`、`src-tauri/src/db/settings.rs`、`src-tauri/src/services/export_local.rs` | `src-tauri/src/services/managed_library/`、`src-tauri/src/db/libraries.rs` |
| 会话及工作台关联 | `src/App.tsx`、`src/stores/taskStore.ts`、`src/pages/ImportPage.tsx` | `src/api/agent.ts`、`src/stores/agentStore.ts`、宿主索引 Module |
| Agent Host/传输 | `src-tauri/src/utils/process.rs`、`src-tauri/src/services/credentials.rs`、`src-tauri/src/services/ai_rate_limit.rs` | A3-01 Host 生命周期和帧语义见[协议 v1](../../docs/contracts/agent-host-protocol-v1.md)；实现位于 `packages/agent-host/`、`src-tauri/src/services/agent_runtime/` |
| 个人流程 | 操作收据与 Agent 受控 loader | `src-tauri/src/services/workflows/`、`src-tauri/src/db/workflows.rs`、`src/components/agent/` |
| MCP bridge | 唯一宿主生命周期与领域 Interface | A5 固定 crate/可执行入口；不独立打开业务 DB |

不按表批量创建空文件。每张卡允许范围是上限；实际只修改完成当前行为必需的位置。
