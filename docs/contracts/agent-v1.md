# Agent v1 契约

状态：A1-01契约设计已独立复核通过，完整代码门禁与冒烟通过，v1方案冻结。当前仍是边界契约，不是已开放的生产 API。

## 定义来源

枚举、领域输入字段与能力入口目录的唯一机器定义为 [agent-v1.json](agent-v1.json)。运行 `node scripts/generate-agent-contract.mjs` 生成 Rust 与 TypeScript 枚举及领域输入；`--check` 检查漂移，已纳入 `npm run test:tooling`。不得手改生成文件。

工具JSON Schema也从同一源生成：[agent-v1.schema.json](agent-v1.schema.json)。每项能力的输入/输出引用由 `x-capabilities`定位，所有record拒绝额外属性；`x-maxUtf8Bytes`表示宿主必须另外执行的UTF-8字节上限。JSON Schema的maxLength按字符计量，不能代替Rust字节核验。Schema只是验证形状，不能验证资源归属或给予权限。

生成的边界形状在 `src-tauri/src/contracts/agent_records.rs` 与 `src/types/agent.records.generated.ts`；`agent.rs`提供标量解析和有界解码，`src/types/agent.ts`作为类型入口。Rust 负责运行时解析，TypeScript 类型不构成安全验证。所有输入拒绝多余字段。当前只支持 `schemaVersion: 1`，未知版本拒绝，不尝试解释为旧版本。

`ResourceRef` 为 16–128 字节 ASCII 字母、数字、下划线或连字符的不透明令牌。解析成功仅说明格式合法，不证明身份、归属或授权；后续宿主必须绑定主体、会话、授权世代和库世代核验。禁止把绝对路径、SQL、命令或模型提交的主体当作资源引用。

## 已实现的公共形状

| 类型 | 字段 | 语义 |
|---|---|---|
| SubmitPlanInput | schemaVersion、planId、idempotencyKey | 仅引用已准备的计划；不能附带 approved 或 principalId |
| OperationInput | schemaVersion、operationId | 指定具体操作，不能依赖最近一个全局任务 |
| LibraryScope | libraryId、libraryEpoch | 宿主快照的库身份；epoch 为不重复随机标识，不是备份会恢复的计数器 |

合法提交形状：`{"schemaVersion":1,"planId":"plan_0123456789abcdef","idempotencyKey":"submit_0123456789abcdef"}`。

额外添加 `approved`、`principalId`、`libraryEpoch`、`directory` 或 `sql` 均解析失败。版本 0、2、99 也失败。该测试不替代后续批准消费或主体授权测试。

## 安全与兼容边界

`contracts/caller.rs`定义宿主内部CallerContext，不实现Serialize/Deserialize，不能作为Tauri或模型输入。主体、会话/实例、requestId、授权世代、库身份/世代及委托链都由已认证adapter构造。`check_capability_entry`只按同源目录检查入口可见性：Pi/MCP不能调用批准或恢复入口；这不替代资源所有权、会话范围、目录用途与批准消费核验。

A1-03已把主体世代、目录用途、`remembered`授权和受控资源句柄落到[authorization-v1.md](authorization-v1.md)。`ResourceRef`仍是引用而非授权；导入/导出/移动等能力必须通过该句柄 Module 在每次使用时复核主体、会话、授权世代、库世代和当前文件身份。

领域输入已覆盖搜索、导入、打标、导出、移动、软删/恢复/清空、计划审批与个人流程。`ShortText` 上限256 UTF-8字节，`Instructions` 上限4096字节，均拒绝NUL；纯 `decode_agent_input` 在JSON解析前检查外层65536字节上限，typed header区分未知版本，其余格式错误归为INVALID_INPUT（当前尚未接入生产入口）。可选字段允许缺省/null，序列化时缺省不输出。

`assets.search.text` 是用户查询文本，后续必须调用现有AI搜索解析与SearchPlanV3执行，不接受SQL或另一套结构化查询。`cursor` 只能是宿主产生并绑定查询快照的分页引用，不能由模型指定页码、offset或全选范围。

导出输入不提供move模式，始终复制。移动输入只引用总仓内部的分库。个人流程参数为封闭字段集合，不携带脚本、旧批准或密钥；未配置参数和流程类型不匹配必须由实例化服务拒绝。

机器目录规定 `plans.approve` 和 `operations.resume` 只对可信GUI入口可见。Pi/MCP可以提交已批准计划，但不能批准或绕过恢复审核。`trash.empty.submit` 必须消费独立永久清空批准。目录只是待实现路由的约束，不等同于当前已完成权限校验。

Rust生成 `CapabilityDescriptor`，TypeScript生成目录和输入/输出映射。生成检查按能力身份固定审批/恢复入口及永久清空审批类别，政策/响应限额/标量/错误目录变体负测确保破坏不变量会报错；正文上限、标量字节限额和错误提示也生成到两端。所有计数和revision暂用u32，避免超出JavaScript安全整数；未知total缺省，不用0冒充已知总量。后续revision耗尽必须明确拒绝或通过新operation身份重建，不能wrap复用旧事件序列。

Rust的 `AgentInput` 使用封闭trait，仅已登记生成输入可以使用有界解码；`serde_json::Value`不能作为该入口输入。生成的 `decode_capability_input` 按具体能力解析对应类型，避免手写路由使用另一种shape。解码只返回类型，不检查调用方权限，也不执行业务。

`AgentOutput`同样封闭；`encode_agent_output`先校验，再向最多65536字节的内部缓冲写入，成功后才返回整个正文。SearchPage最多100个摘要；JSON转义膨胀也计入限额，超限返回INTERNAL并丢弃内部部分正文。禁止adapter边生成边发送未校验输出，也不能把部分JSON当作截断成功。

PlanPreview包括确切范围清单的scopeRef、来源/选择/目标引用、外发服务/配置修订/模型、预计项数、expiresAtUnixSeconds和预算。服务、修订、模型必须成组；项数不能大于批准的maxItems。reviewKind从目标submit能力的审批类别核验：永久清空只能permanent_trash，普通submit只能ordinary，query/approve/resume/cancel不能当作写计划目标。scopeRef必须绑定后台保存的完整不可变清单，不能把显示摘要当作确切清单。

预算包括maxRequests/maxTokens/maxSeconds/maxItems，以及可选monetaryLimit。货币是三个大写ASCII字母，amountMicrounits使用u32，最多4,294,967,295微货币单位；缺省表示金额上限未知/未提供，不代表免费。它不是累计费用账本。expiresAtUnixSeconds为Unix秒，不是毫秒，u32表示范围截至2106年；时间有效性和预算实际消费由正式计划/执行服务核验。

SubmissionResult返回原始operation快照和replayed，标示同键同计划重放；不得从聊天文案猜测是否新建。操作快照提供canCancel/canResume和未知total，completed+failed不能超过已知total；嵌套提交结果也执行此核验。状态机和恢复授权仍由后续服务实现，展示标志不能替代权限。

## 个人流程

load和prepare必须指定workflowRef与version，版本为1–4294967295，缺省/0不合法；引用与版本必须由注册表核验一致。编辑产生新版本，不能修改已准备/在途计划的模板快照。WorkflowSummary返回版本、说明、封闭参数描述与步骤；参数key只能来自WorkflowInputs，无任意参数对象/脚本/密钥，最多7项且不得重复。required由本次模板定义，prepare时服务按该版本核验必填项，缺参不能悄悄使用上次授权或旧句柄。

步骤最多8项，依赖索引从0开始，只能指向当前步骤之前；不能嵌套workflows.submit，也不能把查询、审批、取消或恢复作为写步骤。步骤审核类别从能力目录派生，清空只能permanent_trash。WorkflowPlanPreview返回本次不可变版本、父PlanPreview与完整有界步骤摘要，父计划目标为workflows.submit；调用submit仍仅引用本次planId/idempotencyKey，不能夹带模板定义或参数修改。

父流程的普通批准只允许执行已批准的普通步骤。永久清空子步骤先进入waiting_review并释放执行资源，生成固定清单的独立子计划；必须通过可信GUI取得该子计划的permanent_trash批准，不能消费父批准。标签候选写入正式标签也保持独立可信审核。每次实例化重新准备资源/库世代/服务配置/预算与计划，不复制旧批准、收据或完成计数；这些执行约束由A1/A4服务落实，当前类型和校验不构成执行实现。

## 各类入口行为

| 入口类别 | 返回形状与不变量 |
|---|---|
| 搜索 | SearchPage：有界摘要、总量、截断标志、绑定查询快照的selectionRef/cursor；复用现行搜索解析/执行，不接受SQL或assetId |
| 领域prepare | PlanPreview：检查已授权输入并保存确切范围，不创建operation、不执行文件/标签修改或模型外发；准备阶段必要的只读检查必须先授权 |
| 领域/流程submit | SubmissionResult：批准核验、消费与登记操作必须同一短事务；同主体/库世代/能力/幂等键的同计划返回原操作，异计划返回IDEMPOTENCY_CONFLICT |
| 操作get | OperationSnapshot：按操作归属返回真实快照/revision，不能把另一会话的最近任务当目标；断线后以快照补齐事件 |
| 操作cancel | 请求取消指定操作的剩余步骤，返回真实快照；不停止另一操作、不撤销已生效效果；竞争中实际成功项仍记收据 |
| 操作resume | 仅GUI审核恢复摘要后请求；先核实逐项收据，只推进可安全继续的剩余部分；未知效果不能自动重复 |
| plans.approve | 仅GUI，对reviewRef绑定的确切计划/清单/配置批准，返回ApprovalResult；不是模型自然语言或approved字段 |
| 流程load/prepare | 按不可变版本读取/生成WorkflowSummary或WorkflowPlanPreview；load不授予执行权限，prepare每次生成新计划 |

操作计数的单位由该操作的固定收据清单定义，不能将不同子域的processed值拼成另一份事实源；total未知时缺省，禁止假百分比。事件revision只用于排序/去重，不能替代持久收据。数据库事务不能回滚文件和网络效果，不承诺跨资源exactly-once。

## 错误与恢复条件

| 条件 | 可观察错误及恢复规则 |
|---|---|
| 非法输入/未知版本/未知能力/平台不支持 | INVALID_INPUT、UNSUPPORTED_PROTOCOL、UNSUPPORTED_CAPABILITY、UNSUPPORTED_PLATFORM；受理前拒绝，不创建操作或假成功 |
| 未授权/撤销/引用过期 | UNAUTHORIZED、GRANT_REVOKED、REFERENCE_EXPIRED；不继续读取或可停止的外发/写入步骤，用户重新选择/授权；模型不能自行扩权 |
| 换库/失联/归属未核验 | LIBRARY_CHANGED、LIBRARY_OFFLINE、OWNERSHIP_UNVERIFIED；保留记录和工作，不猜测文件已删除；重新核验/准备，旧批准不能跨世代复活 |
| 缺少批准/计划过期/配置或范围变化 | APPROVAL_REQUIRED、PLAN_EXPIRED、PLAN_CHANGED；未消费的批准不能用于变化后的输入，重新准备审核；已发生效果仍保留真实收据 |
| 幂等键冲突/资源冲突/维护门 | IDEMPOTENCY_CONFLICT、RESOURCE_CONFLICT、MAINTENANCE_ACTIVE；前者不创建另一操作，后两者不绕过占用/维护门重试写入 |
| 服务不可用/预算上限 | SERVICE_UNAVAILABLE、BUDGET_EXCEEDED；保留输入、草稿和真实部分结果，用户决定重试/换服务/重新审核预算；换服务使旧配置批准失效 |
| 结果未知/已取消/未分类失败 | OUTCOME_UNKNOWN、CANCELLED、INTERNAL；先对账未知效果，取消不抹掉成功项，内部失败不得携带敏感cause给模型 |

在执行中返回错误不能抹去已接受operation：同时保存真实状态/阶段/逐项收据。网络断线不等于业务失败，查原operation再决定是否恢复；重试模型请求与重放文件/数据库副作用是两种动作。

模型与外部 MCP 调用均不能构造可信 CallerContext 或批准。可信审批由宿主 GUI 独立入口承担；普通整理批准不能代替永久清空的固定清单批准。枚举目录中的 `plans.approve` 不能因此被默认暴露给模型，能力暴露与路由需要在 A1-01 完整目录中显式冻结。

现有 [SearchPlanV3](search-plan-v3.md)、[分面](facets-v2.md) 和 [AI 搜索](super-search-ai-v2.md) 仍是各自唯一协议；Agent 不产生 SQL 或另建查询编译器。

错误使用应用的 `{ code, message, cause? }` 形状。`AppError::Agent`保存typed AgentErrorCode，返回同源固定message、无底层source；`into_agent_error`将旧业务错误分类转换，删除原始message/cause，未明确分类的失败返回INTERNAL。OUTCOME_UNKNOWN须由操作对账明确产生，不能由通用IO错误猜测。Pi/MCP未来adapter必须调用这个转换，禁止直接外发旧AppError::Io/Json等；当前尚未接入生产入口。

## 契约验收与后续边界

契约已通过独立设计复核、本卡最终完整门禁及冒烟，A1-01验收完成；验证结果见[PROJECT_PLAN](../PROJECT_PLAN.md#9-20-agent开发状态)。每项能力已有合法输入和伪造身份/授权字段及未知版本负测；流程与输出还需随具体服务接入验证实际副作用，不能把类型测试当作生产验收。E01–E08原caseId与禁止副作用断言保持不变；机器目录已记录adapter版本与正式能力映射，tooling逐项核对原数量和禁止副作用断言。E01原型preview对应prepare展示，E08 policy-only对应独立清空submit拒绝普通批准；正式adapter与领域效果由下游卡接入。A1-02可据此实施保护性数据基础；生产授权与执行仍须逐卡验收。
