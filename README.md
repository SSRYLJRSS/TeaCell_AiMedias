# 茶包素材 BagerTea AiMedias

本地优先的图片与视频素材管理桌面应用。项目使用 Tauri 2、Rust、SQLite FTS5、React 19、TypeScript、Zustand 和 Tailwind CSS v4。

当前版本：公测版 `1.0`（技术版本 `1.0.0`）。功能代码已完成到搜索计划 V3、分面 V2、本地模型部署和查看器/去重/备份等能力；真实素材 UAT 与发布证据仍需按测试文档补齐。

平台口径：Windows 10/11 x64 是当前正式支持平台；macOS Apple Silicon 与 Linux x64 是同一 `main` 提交上的预览平台。核心闭环统一为“导入 → 浏览 → 搜索 → 查看 → 导出”，平台暂不支持的能力必须在界面中明确说明。

## 能力概览

- 图片/视频两段式入库，支持总库/分库托管、批量改名、占位图优先和后台高清缩略图。
- JPG/PNG/WebP/TIFF/BMP/TGA，以及 HEIC/HEIF 和 25+ RAW 扩展名；RAW 内嵌预览优先，真解码仅进入高清层。
- 虚拟滚动素材库、多选、排序、筛选、收藏评级、查看器、视频播放和悬浮预览。
- FTS5 中文文件名/标签搜索，支持逐字切分、短语和短查询兜底。
- 超级搜索使用 SearchPlanV3，支持必须、优先、排除、元数据、数值分面、AI 解析和诊断。
- 稳定分面协议，区分 AI 可打标分面与仅手工分面，支持别名、治理、合并、撤销和数值分面。
- 云端 OpenAI 兼容/Anthropic 接口与 Ollama 本地模型；AI 建议默认人工确认后写入。
- 重复文件与感知相似图检测、同源文件组、回收站、批量删除和恢复。
- 本地复制/移动导出、目录布局、CSV 清单、数据库备份恢复和运行日志。

素材库和索引默认保存在本机 SQLite。使用云端 AI 时，只有发起请求的素材预览会发送到用户配置的服务商；API Key 使用系统凭据存储，不写入设置 JSON。

## 当前状态

功能代码已完成，但**尚不能标记为可放行**。按[项目计划](docs/PROJECT_PLAN.md)的发布阻塞项，放行前还需完成：

- **真实素材 UAT**：超级搜索 relevance 排序、`mustNot` 排除、`should` 全不命中仍保留、位置权重和 hydrate 后 plan JSON，都需要真实数据证据。
- **核心用户旅程**：用真实素材走通导入、浏览、AI 打标、搜索、查看器、评级、导出、删除恢复和备份恢复。
- **格式与性能复测**：在目标机器上复测 RAW/HEIC、视频播放、3 万级搜索和网格滚动。
- **发布门禁**：`scripts/smoke.ps1`、严格 Rust 门禁、三目标 CI、候选包门和人工 UAT 必须全部留下结果记录。

自动化全绿不等同于产品可发布，真实素材验收是独立门禁。数据位置、备份恢复和发布步骤见[运维手册](docs/OPERATIONS.md)。

## 技术结构

```text
React pages/components
  -> stores
  -> src/api
  -> Tauri invoke / events
  -> Rust commands
  -> services
  -> db / filesystem
```

- 前端：`src/`
- Rust/Tauri：`src-tauri/`
- HEIC 预编译库：按目标放在 `src-tauri/native/heif/<target>/`；Windows 兼容旧的根目录 `heif-bin/`
- 文档：`docs/`
- AI 协作入口：[AGENTS.md](AGENTS.md)

## 快速开始

环境要求：

- Windows 10/11 为主力平台。
- Node.js 22（`package.json` 的 `engines` 约束为 `>=22 <25`）。
- Rust stable MSVC 工具链和 Visual Studio C++ Build Tools。
- 开发时 `ffmpeg`/`ffprobe` 可由 PATH 提供；发布包必须先按 `src-tauri/binaries/manifest.json` 准备并校验三目标 sidecar。

```powershell
npm install
npm run desktop:dev
```

构建安装包：

```powershell
npm run prepare-media-tools -- --target x86_64-pc-windows-msvc
npm run desktop:build -- --target x86_64-pc-windows-msvc

# 仅做不构建的环境检查
npm run desktop:check -- --target x86_64-pc-windows-msvc
```

## 开发命令

```powershell
npm run lint
npm run typecheck
npm run test:unit
npm run build

cd src-tauri
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

完整本地冒烟：

```powershell
pwsh ./scripts/smoke.ps1
```

## 文档入口

- [文档中心](docs/README.md)
- [产品需求](docs/PRD.md)
- [项目计划](docs/PROJECT_PLAN.md)
- [架构说明](docs/ARCHITECTURE.md)
- [开发规范](docs/DEVELOPMENT.md)
- [平台契约](docs/PLATFORM.md)
- [UI 规范](docs/UI_DESIGN_SYSTEM.md)
- [协议契约](docs/CONTRACTS.md)
- [测试策略](docs/TEST_STRATEGY.md)
- [真机验收手册](docs/QA_PLAYBOOK.md)
- [性能手册](docs/PERFORMANCE.md)
- [运维手册](docs/OPERATIONS.md)
- [排障手册](docs/TROUBLESHOOTING.md)

## 发布约束

- 普通改动必须通过 lint、typecheck、前端单测、Rust fmt、clippy 和 cargo test。
- 交付版本必须通过 `scripts/smoke.ps1` 和真实素材 UAT。
- 已发布数据库迁移不得回改；结构变化必须新增或追加幂等迁移。
- 破坏性操作必须可确认、可回滚或明确报告失败，禁止假成功和静默数据丢失。

## License

[MIT License](LICENSE)。项目自有代码允许自由使用、修改、再发布和商业使用；第三方图像/视频编解码依赖仍按各自许可证执行，对外分发前必须复核 [架构说明](docs/ARCHITECTURE.md) 中的许可提示。
