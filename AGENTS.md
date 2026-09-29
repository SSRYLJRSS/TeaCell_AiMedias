# AGENTS.md

本文件是整个仓库的 AI 协作入口。它只定义长期稳定的工作方式，不承载易过时的进度流水。

## 项目一句话

茶馆AI素材管理是一个本地优先的图片/视频素材管理桌面应用，技术栈为 Tauri 2、Rust、SQLite FTS5、React 19、TypeScript、Zustand 和 Tailwind CSS v4。

## 开始工作前

按以下顺序读取上下文，不要默认加载历史报告：

1. `docs/README.md`：确认文档权威边界和当前阅读路径。
2. `docs/ARCHITECTURE.md`：确认模块边界、数据流和设计约束。
3. `docs/DEVELOPMENT.md`：确认编码、测试、迁移和交付规范。
4. 涉及产品行为时读 `docs/PRD.md` 和 `docs/UI_DESIGN_SYSTEM.md`。
5. 涉及路径、进程、凭据、原生依赖、平台能力或打包时读 `docs/PLATFORM.md`。
6. 涉及搜索、分面协议时读 `docs/CONTRACTS.md` 及对应 `docs/contracts/*.md`。
7. 涉及验收或故障时按需读 `docs/TEST_STRATEGY.md`、`docs/QA_PLAYBOOK.md`、`docs/PERFORMANCE.md`、`docs/TROUBLESHOOTING.md`、`docs/OPERATIONS.md`。

只读取当前任务需要的最小上下文。日期化报告、阶段报告、施工稿和一次性修复计划不再属于仓库文档体系，历史内容以 Git 历史为准。

`.qoder/`、`.workbuddy/`、`.claude/` 是本地工具缓存或会话记忆，不是项目事实源；即使其中包含旧路径或旧结论，也不得据此覆盖当前文档。

## 唯一事实来源

| 信息类型 | 权威文件 |
|---|---|
| 产品目标、范围、需求与验收口径 | `docs/PRD.md` |
| 里程碑、当前交付状态、下一阶段 | `docs/PROJECT_PLAN.md` |
| 代码结构、模块职责、关键机制 | `docs/ARCHITECTURE.md` |
| 开发流程、代码规范、测试与门禁 | `docs/DEVELOPMENT.md` |
| 支持平台、支持等级、平台能力差异 | `docs/PLATFORM.md` |
| 视觉与交互规范 | `docs/UI_DESIGN_SYSTEM.md` |
| 搜索计划、分面、AI 查询机器协议 | `docs/contracts/*.md` |
| 测试分层、发布门禁、UAT 口径 | `docs/TEST_STRATEGY.md` |
| 真机验收操作手册 | `docs/QA_PLAYBOOK.md` |
| 性能基线与反模式 | `docs/PERFORMANCE.md` |
| 运维、发布、备份恢复、本地模型 | `docs/OPERATIONS.md` |
| 已知问题与排查路径 | `docs/TROUBLESHOOTING.md` |

同一事实只能在一处定义。其他文档只链接，不复制，不另建“第二份进度”或“第二份 PRD”。

## 不可协商的规则

1. 先读代码和契约，再改代码。实现与文档冲突时，先确认哪一份代表当前产品决策，禁止静默选一边。
2. 不修改已发布迁移。数据库结构变化必须新增或追加幂等迁移，并补迁移测试。
3. `commands/` 是薄壳，只做参数校验、状态编排和事件发送；业务逻辑在 `services/`，SQL 在 `db/`。
4. 前端只能通过 `src/api/` 调用 Tauri，不允许页面或组件直接调用 `invoke`。
5. 图片解码统一经 `services/imaging.rs`。HEIC/RAW 专用解码器只能作为 `imaging` 的分派下游。
6. 数据库锁内禁止解码、网络请求和大文件 IO。
7. 破坏性操作必须明确确认；“删除文件”失败时不得从数据库假装删除成功。
8. 配置迁移只允许在 normalize/迁移链路中单向完成，禁止新旧字段双写。
9. UI 只使用 `src/styles/theme.css` 的语义变量，不新增无语义硬编码颜色。
10. 工作区可能有用户未提交改动。不得 reset、checkout、clean，也不得回滚不是本任务产生的修改。
11. 改代码前把影响分类为数据/业务逻辑、操作系统资源或平台专属能力；平台契约以 `docs/PLATFORM.md` 为准。
12. 前端消费后端统一的平台能力，不自行探测操作系统；Rust 目标依赖使用 `Cargo.toml` 的 target 表。
13. Windows 优先功能合并前必须声明支持范围，为其他平台提供明确禁用或降级行为，并保持核心数据语义一致。
14. 受支持平台的编译或测试失败会阻止合并；临时基础设施豁免必须记录期限和恢复条件。
15. 长期只维护 `main`；平台差异通过能力层、条件依赖和测试管理，不建立长期平台分支。

## 开发闭环

1. 查看 `git status`，识别用户已有改动。
2. 明确行为契约、影响面、测试方法和文档落点。
3. 小步实现，功能与测试同批完成。
4. 先跑定向测试，再跑相关完整门禁。
5. 行为、架构、协议或规范发生变化时，同步更新对应权威文档。
6. 汇报真实结果，包括未执行的测试、环境限制和剩余风险。

## 质量门禁

普通改动至少执行：

```powershell
npm run typecheck
npm run lint
npm run test:unit
npm run test:tooling
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Cargo build/test/clippy 会链接 HEIF 原生静态库。先在当前原生平台准备固定版本，再为当前 shell 指定已验证目录；CI 与 `scripts/smoke.ps1` 自动执行同一准备步骤：

```powershell
npm run prepare-heif-libraries -- --target x86_64-pc-windows-msvc
$env:HEIF_BINARIES_DIR = (Resolve-Path "src-tauri/native/heif/x86_64-pc-windows-msvc").Path
```

交付前执行完整冒烟：

```powershell
pwsh ./scripts/smoke.ps1
```

如果任务只涉及文档，仍应检查 Markdown 链接、路径引用和残留旧文档引用。不得把未运行说成通过。

## 提交与文档维护

- 未经用户明确要求，不提交、不推送。
- 提交按可独立验证的行为拆分，避免把格式化和功能修改混在一起。
- 临时排查记录放在任务说明、PR 或提交信息中，不新增带日期的长期文档。
- 删除过时文档后，必须搜索并清理代码、CI 和现行文档中的旧路径引用。
- 修改本文件前，先确认规范确实需要长期变化，而不是某次任务的临时偏好。
