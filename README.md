# 茶馆AI素材管理 / TeaCell AI Media Manager

本地优先的图片与视频素材管理桌面应用。项目使用 Tauri 2、Rust、SQLite FTS5、React 19、TypeScript、Zustand 和 Tailwind CSS v4。

当前版本：`1.0.2`。功能代码已完成到搜索计划 V3、分面 V2、本地模型部署和查看器/去重/备份等能力；真实素材 UAT 与发布证据仍需按测试文档补齐。

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
- **发布门禁**：`scripts/smoke.ps1`、严格 Rust 门禁、前端门禁和人工 UAT 必须全部留下结果记录。

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
- HEIC 原生依赖：从固定的三端 `binaries-heif` release 下载，SHA256、目标 ABI、源码版本与许可证来源见 [`src-tauri/native/heif-manifest.json`](src-tauri/native/heif-manifest.json)；目标缓存不纳入 Git。
- 文档：`docs/`
- AI 协作入口：[AGENTS.md](AGENTS.md)

## 快速开始

环境要求：

- Windows 10/11 x64 为首发完整功能目标；Apple Silicon macOS、Ubuntu 24.04 x64 为预览目标，当前验收状态见 [平台文档](docs/PLATFORM.md)。
- Node.js 22–24。
- Rust 1.98.1（Windows 另需 MSVC 与 Visual Studio C++ Build Tools）。
- 开发时系统 `ffmpeg`/`ffprobe` 可选；安装包构建使用按目标校验的 sidecar。

```powershell
npm ci
npm run desktop:dev
```

构建安装包：

```powershell
npm run prepare-media-tools -- --target x86_64-pc-windows-msvc
npm run prepare-heif-libraries -- --target x86_64-pc-windows-msvc
npm run desktop:check:strict -- --target x86_64-pc-windows-msvc
npm run desktop:build -- --target x86_64-pc-windows-msvc
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

直接运行 Cargo 编译/测试前，需要准备当前原生目标的 HEIF 库，并在当前 shell 将 `HEIF_BINARIES_DIR` 指向该目录；缺失或目标不符会由 `build.rs` 明确失败，不再回退到 `heif-rs` 未校验的联网下载。`npm run desktop:dev` 和 `npm run desktop:build` 会自动准备目标库。

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

MIT。项目中涉及的第三方图像/视频编解码依赖另有许可证要求，对外分发前必须复核 [架构说明](docs/ARCHITECTURE.md) 中的许可提示。
