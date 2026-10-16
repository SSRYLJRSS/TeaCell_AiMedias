# 10 长期开发执行方法：研究依据

检索日期：2026-10-08。范围：怎样让对项目理解有限、长任务能力较弱的编码 AI 更容易按既定方案完成工作。本文件只提供方法研究和本项目的应用推导，不是新的产品需求、实施授权或进度记录；版本范围和顺序仍由 [08](08-migration-and-delivery.md) 拥有，验收映射由 [09](09-validation-and-risk.md) 拥有，现行规范以 [文档中心](../../docs/README.md) 为准。

## 研究结论

更有依据的做法，是把复杂工程拆成可以恢复上下文、可以观察结果、可以独立复核的小闭环：明确目标及约束 → 定位真实实现 → 完成一个可验证行为 → 执行检查 → 修复或报告阻塞 → 留下交接证据。单独增加提示词长度、重复“必须完成”、自动压缩上下文，都不足以证明长期可靠性。[OpenAI 长任务实践](https://developers.openai.com/blog/run-long-horizon-tasks-with-codex)、[Anthropic 长任务实验](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)

这些来源主要是厂商工程经验、产品指南与个案实验，没有直接证明某套 Markdown 能让任意弱模型独立完成本项目。下面区分来源陈述和本项目推导；任务颗粒度、复核成本与实际成功率仍要通过实施时的首批任务校准。

## 一手资料及适用边界

| 来源 | 已核实的内容 | 如何使用及不能外推的部分 |
|---|---|---|
| [S1 OpenAI：Using PLANS.md for multi-hour problem solving](https://developers.openai.com/cookbook/articles/codex_exec_plans) | 示例要求计划可供无历史记忆的执行者理解，明确文件、命令、预期行为、验证和恢复方式；里程碑应可独立验证。 | 借鉴任务说明的完整性。页面已标记归档，不把其中旧模型推荐或模板格式当作当前强制规范；其中频繁提交、复制全部背景的建议也不能覆盖本仓库规则。 |
| [S2 OpenAI：Run long horizon tasks with Codex](https://developers.openai.com/blog/run-long-horizon-tasks-with-codex) | 2026-02-23 的工程实验采用持久规格、里程碑、执行说明、状态记录和持续验证，并要求验证失败先修复。作者明确结果仍未达到生产就绪。 | 借鉴“计划—执行—验证—修复”循环，不把运行时长、代码量或单次演示当作质量与弱模型能力证据。 |
| [S3 Anthropic：Effective harnesses for long-running agents](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents) | 2025-11-26 的实验观察到一次做太多、交接不完整、过早报完成等问题；通过每轮一个功能、结构化功能状态、启动检查和端到端测试改善。 | 借鉴渐进交付与完成证据。其实验偏全栈网页应用；JSON 比 Markdown 更不易被误改是该实验观察，不是通用定律，也不是新增 JSON 文件的必要条件。 |
| [S4 Anthropic：Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) | 2025-09-29 的文章建议精选高信号上下文、按需获取资料，并用压缩、外部笔记等保持长期连贯；过度压缩可能丢失关键细节。 | 借鉴最小阅读包和可恢复交接。更长上下文不等于更可靠；外部笔记也需要核对当前代码，不能自动晋升为事实来源。 |
| [S5 GitHub：Best practices for using GitHub Copilot to work on tasks](https://docs.github.com/en/copilot/tutorials/cloud-agent/get-the-best-results) | 指南建议任务范围清晰、验收标准完整、提供相关文件方向和可构建测试的环境；对复杂、宽泛、敏感或含糊任务提示慎重选择。 | 借鉴任务卡与环境前置条件；不能直接把一个大型架构改造当成普通 issue 交给能力不足的模型。 |
| [S6 GitHub：Review AI-generated code](https://docs.github.com/en/copilot/tutorials/review-ai-generated-code) | 指南要求自动检查之外复核意图与架构、依赖、虚构 API、被删或跳过的测试，并强调人工判断和协作审查。 | 借鉴实现后的独立复核。第二个 AI 的认可仍不是证明，也不能替代真实执行与涉及领域判断的人工验收。 |

## 对茶馆 2.0 的推导

以下是依据上述资料和当前仓库约束提出的执行设计建议，不是来源已替本项目验证的结论。实际任务指令应在本目录指定的执行入口中维护，本文件不逐卡记录状态。

### 1. 一个任务卡交付一个可观察的行为

每卡至少说明：所属 08 阶段、前置任务、需求/契约链接、目标与不做事项、相关真实代码入口、输入输出与失败情形、允许修改范围、定向验证方式、完成证据、停止条件。文件清单是导航和范围约束；执行前仍需检查路径、签名和调用方，不能把设计稿中的拟议接口认作已有代码。[依据 S1](https://developers.openai.com/cookbook/articles/codex_exec_plans)、[依据 S5](https://docs.github.com/en/copilot/tutorials/cloud-agent/get-the-best-results)

“实现任务恢复”仍然太大；“在既定恢复契约下，进程中断后重新打开同一任务，恢复入口展示正确状态，且不会自动重复执行已经完成的写入”才提供了可检验的方向。具体状态、标识与恢复策略必须由项目契约确定，不能由此示例自行补全。如果一张卡同时引入多项未验证机制，应拆成先证明关键假设、再实现业务行为的顺序；不机械规定每卡必须几小时或几个文件。

### 2. 降低执行时必须临场作出的关键决定

面向较弱执行者，指导书应先确定模块归属、边界输入输出、哪些状态允许转移、失败后保留什么以及什么算通过。仍待实验的问题应明确写成“待验证”，附验证办法与不成立时的处理出口；不要同时列多个候选架构再让编码者边写边猜。[依据 S1](https://developers.openai.com/cookbook/articles/codex_exec_plans)、[依据 S5](https://docs.github.com/en/copilot/tutorials/cloud-agent/get-the-best-results)

这不要求提前虚构所有函数签名。描述应对关键契约足够具体，对局部实现保留空间。发现现行代码、权威文档与方案不一致时，应记录实际证据并暂停受冲突影响的部分，避免静默选边；可独立进行的只读定位和证据收集可以继续。

### 3. 分开全局导航与当前任务阅读包

全局入口告诉执行者去哪里找事实；每卡只列当前需要的权威章节、调用链、既有测试和相关设计段落。不要每轮加载整个方案、所有历史输出和无关源码。交接摘要保存当前卡、未完成部分、关键决定及其依据、已改文件、验证命令和结果、下一步与阻塞；恢复时必须重新核对工作区差异。[依据 S4](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)

此处的外部状态应复用现有任务说明、PR/Issue 或获准的任务交接位置；项目交付状态仍由 [PROJECT_PLAN.md](../../docs/PROJECT_PLAN.md) 拥有。不能照搬厂商示例，再创建一个长期维护的进度文件或第二份 PRD。当前纯规划阶段不修改权威文档，也不写入“已完成实现”状态。

### 4. 完成由证据决定，失败阻断依赖推进

先为关键行为写清正常、边界和失败路径的预期结果，再实现并检查；功能测试应能识别错误行为，不能仅断言实现细节存在。每张卡先运行定向验证，再按 [DEVELOPMENT.md](../../docs/DEVELOPMENT.md) 执行相关完整门禁。指令必须区分“通过”“失败”“未运行”，并保存命令、环境和必要输出摘要。[依据 S2](https://developers.openai.com/blog/run-long-horizon-tasks-with-codex)、[依据 S3](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)

某卡必要验证失败或无法运行时，它不能被标为完成，也不能作为后续依赖已满足的依据。环境问题要有可复现证据；不得靠删除失败测试、放宽验收或跳过断言获得通过。连续尝试没有新证据时，应收敛为最小复现和阻塞说明，再由更有能力的审查者协助，不无限扩大修改范围。

### 5. 实现与复核承担不同职责

实现者负责按契约交付及自测；复核者从验收条件和变更差异出发，寻找遗漏失败路径、越界修改、虚构 API、测试失真与不一致状态。可以由另一位开发者或获授权的独立 AI 会话复核，但不以“另一个模型说可以”替代测试。[依据 S6](https://docs.github.com/en/copilot/tutorials/review-ai-generated-code)

本项目复核宜覆盖两条线：行为是否满足 02/06/09 的对应规则，以及实现是否遵守现行分层、迁移、平台与门禁规范。涉及真实文件、进程中断、原生依赖或桌面交互时，验收须落实到原生平台和真实操作路径；网页实验中的浏览器验证不能直接证明 Tauri 原生链路可用。具体用例继续由 09 和现行 [测试策略](../../docs/TEST_STRATEGY.md)、[真机验收手册](../../docs/QA_PLAYBOOK.md) 定义。

### 6. 用首批任务校准，而不是预设弱模型可靠

先在范围较小、失败可定位的任务上观察：是否遗漏契约、是否修改无关文件、是否真实运行验证、是否需要反复纠偏、是否能在新会话恢复。若反复失败，优先缩小任务、补明确示例或交由更强执行者处理关键设计；不要只追加更强硬的措辞。[依据 S3](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)、[依据 S5](https://docs.github.com/en/copilot/tutorials/cloud-agent/get-the-best-results)

这是本项目建议的校准方式，并非已测得的成功率提升。架构、授权、数据恢复等复杂决策仍需要胜任者判断；目前没有证据支持“详细文档可以完全弥补模型能力不足”这一承诺。

## 本轮研究边界

- 已通过 agent-reach 的 Exa 搜索与网页读取路由读取 Anthropic/GitHub 官方来源；OpenAI 来源依 OpenAI Docs 流程从官方域搜索并读取正文。仅用上述六篇一手来源形成结论。
- 本文未运行任何产品、模型或长任务对照实验，未验证弱模型执行成功率，也未验证跨平台运行效果。
- 没有复制厂商示例中的自动提交、自动回退、另设长期进度库等做法；Git 和文档维护仍服从 [AGENTS.md](../../AGENTS.md) 及现行开发规范。
- 调研内容用于完善指导书；本次没有修改产品代码、配置、依赖、数据库、AGENTS 或权威 docs。
