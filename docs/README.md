# 文档中心

> 更新日期：2026-09-22
>
> 本文档只维护“当前有效信息”和阅读路径。一次性实施方案、阶段报告、修复稿、旧版测试报告不再长期保留，历史内容以 Git 历史为准。

## 文档原则

1. **一类信息只有一个权威来源**。其他文档只链接，不复制。
2. **稳定规范与执行状态分开**。架构、开发规范和协议保持稳定；项目计划记录当前阶段状态。
3. **机器协议单独冻结**。搜索计划和分面协议变动必须先改契约，再改代码。
4. **过程材料不进入长期文档树**。施工单、审查报告、诊断记录和阶段总结由提交、PR、Issue 或 Git 历史承载。
5. **过期即删除或合并**。不为历史稿保留“仅供参照”目录，避免 AI 同时读取互相冲突的旧结论。

`.qoder/`、`.workbuddy/`、`.claude/` 由本地工具生成并已被 Git 忽略。它们不是项目文档，也不得作为开发决策依据。

## 默认阅读顺序

AI 或新开发者进入项目时，默认只读以下最小集合：

1. `AGENTS.md`
2. `docs/ARCHITECTURE.md`
3. `docs/DEVELOPMENT.md`
4. 涉及操作系统资源或分发时读取 `docs/PLATFORM.md`
5. 任务相关的产品、UI、契约或测试文档

不要在未确认任务相关性时加载整批历史文档，也不要读取本地 AI 工具的历史缓存来补全项目事实。

## 当前权威文档

| 文档 | 管辖范围 | 何时读取或修改 |
|---|---|---|
| [PRD.md](PRD.md) | 产品定位、用户、需求边界、验收口径 | 需求新增、变更、删除时 |
| [PROJECT_PLAN.md](PROJECT_PLAN.md) | 当前交付状态、里程碑、下一阶段、发布阻塞 | 排期、发布、阶段复盘时 |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 前后端结构、模块职责、数据流、关键机制、设计约束 | 改代码前必读，结构变化时同步 |
| [DEVELOPMENT.md](DEVELOPMENT.md) | 环境、命令、分层、代码规范、测试、迁移、AI 协作流程 | 所有开发任务 |
| [PLATFORM.md](PLATFORM.md) | 支持系统、支持等级、能力矩阵、平台差异边界 | 路径、进程、凭据、原生依赖、打包和跨平台发布 |
| [UI_DESIGN_SYSTEM.md](UI_DESIGN_SYSTEM.md) | 视觉令牌、布局、控件、状态、响应式和可访问性 | 新增或修改 UI 时 |
| [CONTRACTS.md](CONTRACTS.md) | 搜索、分面、AI 查询协议的索引和不变量 | 修改搜索或标签协议前 |
| [contracts/search-plan-v3.md](contracts/search-plan-v3.md) | SearchPlanV3 执行契约 | 超级搜索执行链路 |
| [contracts/facets-v2.md](contracts/facets-v2.md) | 分面数据模型、输入模式、级联规则 | 标签、分面、AI 打标 |
| [contracts/super-search-ai-v2.md](contracts/super-search-ai-v2.md) | SearchIntentV2、降级和剔除协议 | AI 智能搜 |
| [TEST_STRATEGY.md](TEST_STRATEGY.md) | 测试分层、自动化门禁、测试数据、发布标准 | 设计测试和执行回归时 |
| [QA_PLAYBOOK.md](QA_PLAYBOOK.md) | 真机验收步骤、模块用例、探索测试和缺陷模板 | 桌面端验收时 |
| [PERFORMANCE.md](PERFORMANCE.md) | 性能基线、优化机制、回归清单和反模式 | 图像、列表、搜索、并发改动 |
| [OPERATIONS.md](OPERATIONS.md) | 快速启动、数据位置、发布、备份恢复、本地模型、应急 | 运行、交付、部署、运维时 |
| [TROUBLESHOOTING.md](TROUBLESHOOTING.md) | 已知症状、根因和修复路径 | 遇到异常时先查 |

## 根目录文档

| 文档 | 用途 |
|---|---|
| [README.md](../README.md) | 面向使用者和开发者的项目入口 |
| [AGENTS.md](../AGENTS.md) | AI 协作的强制入口和最短规则集 |

## 文档更新矩阵

| 变化 | 必须更新 |
|---|---|
| 产品需求、用户行为、验收标准 | `PRD.md` |
| 里程碑、状态、发布阻塞 | `PROJECT_PLAN.md` |
| 模块、数据流、关键机制、设计约束 | `ARCHITECTURE.md` |
| 命令行、规范、测试门禁、迁移流程 | `DEVELOPMENT.md` |
| 支持范围、能力差异、平台高危区域 | `PLATFORM.md` |
| 颜色、布局、控件、交互范式 | `UI_DESIGN_SYSTEM.md` |
| 搜索计划、分面、AI 查询结构 | 对应 `contracts/*.md` |
| 测试分层、准入准出 | `TEST_STRATEGY.md` |
| 真机验收步骤 | `QA_PLAYBOOK.md` |
| 性能红线 | `PERFORMANCE.md` |
| 部署、备份、恢复、本地模型 | `OPERATIONS.md` |
| 已知故障模式 | `TROUBLESHOOTING.md` |

## 文档生命周期

- **Active**：当前权威文档，允许直接修改并保持与代码一致。
- **Frozen**：机器协议，变更需要同步 Rust、TypeScript 和测试。
- **Ephemeral**：任务说明、诊断、阶段报告和审查稿，完成后进入提交或 Git 历史，不进入长期文档树。

新增文档前必须回答三个问题：

1. 它是否拥有现有文档没有覆盖的长期职责？
2. 它是否会成为第二事实来源？
3. 删除它是否会损失当前必须保留的契约？

如果答案不能支持独立长期存在，应合并到现有权威文档。
