# 02 目标架构与产品范围

性质：目标设计索引，产品交互已按用户逐项确认更新；技术结构、术语/路径/API 须经过对应设计任务验证再冻结。研究时的事实基线见 [项目评估](01-current-project-assessment.md)，当前实现与验收状态只见 [PROJECT_PLAN第9节](../../docs/PROJECT_PLAN.md#9-20-agent开发状态)。产品与架构目标已承接至权威PRD/ARCHITECTURE；本章保留导航引用，不能把待实现目标或独立原型当作已交付功能。

## 产品定义

茶馆分两阶段成为本地优先的智能素材工作台：2.0 通过页面、自然语言 Agent 和受控 stdio MCP 访问同一组业务能力；2.5 再增加插件入口。模型理解目标，程序执行规则。版本范围与步骤只由 [08](08-migration-and-delivery.md) 定义；下图包含目标全貌，不代表全部在 2.0 实现。

## 系统拓扑

该目标设计已承接到 [ARCHITECTURE第9.1节](../../docs/ARCHITECTURE.md#91-系统拓扑)，由权威架构文档维护；目前仍待A0/A1验证与实现。

## Module 与 Interface

该目标设计已承接到 [ARCHITECTURE第9.2节](../../docs/ARCHITECTURE.md#92-module-与-interface)，由权威架构文档维护；目前仍待A0/A1验证与实现。

## 一份能力定义，多个 Adapter

该目标设计已承接到 [ARCHITECTURE第9.3节](../../docs/ARCHITECTURE.md#93-一份能力定义多个-adapter)，由权威架构文档维护；目前仍待A0/A1验证与实现。

## 数据所有权

该目标设计已承接到 [ARCHITECTURE第9.4节](../../docs/ARCHITECTURE.md#94-数据所有权)，由权威架构文档维护；目前仍待A0/A1验证与实现。

## 运行空间

该目标设计已承接到 [ARCHITECTURE第9.5节](../../docs/ARCHITECTURE.md#95-运行空间)，由权威架构文档维护；目前仍待A0/A1验证与实现。

## 并发与故障

该目标设计已承接到 [ARCHITECTURE第9.6节](../../docs/ARCHITECTURE.md#96-并发与故障)，由权威架构文档维护；目前仍待A0/A1验证与实现。

## UI 与分期范围

用户已批准的产品行为已在本轮规划承接到 [PRD第8节](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现)，那里是唯一维护位置。本节保留跳转以兼容任务卡/需求索引，不再重复维护导航、会话、文件边界与个人流程。所有2.0目标仍待实现，当前证据见 [项目计划](../../docs/PROJECT_PLAN.md#9-20-agent开发状态)。

## 方案中的有意限制

不 fork Pi 主线以重写引擎；固定发布版本并隔离 Adapter。不给外部入口任意 SQL/shell。保留手动入口，按本章已经明确变更的产品规则逐步迁入共享业务 Interface，不把旧库外移动追踪等行为当作必须保留的兼容接口。没有需求证据时不引入微服务、消息中间件、全应用事件溯源或多 Agent 群。
