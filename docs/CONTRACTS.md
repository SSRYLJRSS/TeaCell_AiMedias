# 协议契约

> 更新日期：2026-10-11
>
> 本目录中的契约描述机器协议，不是实现建议。协议变化必须同步 Rust、TypeScript、数据库迁移、UI 行为和测试。

## 1. 契约清单

| 契约 | 覆盖范围 | 主要落点 |
|---|---|---|
| [agent-v1.md](contracts/agent-v1.md) | Agent 跨入口协议，v1边界契约冻结；业务接入待实现 | `contracts/agent.rs`、`src/types/agent.ts`、生成枚举与漂移测试 |
| [agent-host-protocol-v1.md](contracts/agent-host-protocol-v1.md) | Rust 与应用自带 Node/Pi Host 的内部进程协议；当前仅 Windows x64、离线模型 fixture | `services/agent_runtime/`、`packages/agent-host/`、Host 集成测试 |
| [authorization-v1.md](contracts/authorization-v1.md) | 目录授权、主体/会话/世代、受控文件与素材句柄；生产入口按 A1-03 接入 | `services/authorization.rs`、`db/authorization.rs`、`commands/authorization_cmd.rs` |
| [search-plan-v3.md](contracts/search-plan-v3.md) | SearchPlanV3、must_not 极性、warning、诊断、位置权重 | `db/search_plan.rs`、`superSearchStore.ts`、`QueryBuilder.tsx` |
| [facets-v2.md](contracts/facets-v2.md) | 分面 key、input_mode、级联删除、标签不变量 | `db/tag_facets.rs`、`tagStore.ts`、`Workbench.tsx` |
| [super-search-ai-v2.md](contracts/super-search-ai-v2.md) | SearchIntentV2、JSON Schema、三层降级、facet_has_any/missing | `services/super_search_ai.rs`、`db/query_expr.rs`、`AiSearchBar.tsx` |

## 2. 全局协议规则

1. 机器 key 一旦发布不可重命名；改变语义必须创建新 key 或新版本。
2. UI 展示名不是协议，不参与持久化匹配。
3. 所有枚举、字段和操作符都要有服务端校验，不信任 AI 或前端输入。
4. 计划、查询、分面和迁移变更必须有版本策略。
5. 列表、总数、全选、诊断和排序必须共享同一执行语义。
6. 非法条件应明确报错或按契约剔除；不得静默变成另一条查询。
7. 协议文档与测试同时更新，不允许只改实现。

## 3. 变更流程

1. 在对应契约中说明动机、兼容性和失败行为。
2. 更新 Rust 类型与编译。
3. 更新 TypeScript 类型、store 和 UI。
4. 更新数据库迁移或兼容读取。
5. 增加契约测试和回归测试。
6. 更新 [ARCHITECTURE.md](ARCHITECTURE.md) 中的机制摘要。
7. 在发布说明中记录用户可见变化。

## 4. 禁止事项

- 禁止在 QueryBuilder、store 和后端各维护一份计划语义。
- 禁止把 `ExcludeTag` 或 `QueryExpr::Not` 放入 `must_not`。
- 禁止恢复已经删除的权重三档、手工 minimumShouldMatch 或平行筛选入口。
- 禁止让 `tag_facets` 与旧 JSON 配置同时成为读写事实源。
- 禁止让 AI 直接输出 SQL、tagId、assetId 或分页参数。
