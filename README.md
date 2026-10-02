<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/app-icon-black-bg.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/app-icon-white-bg.png">
  <img src="docs/assets/app-icon-square.png" alt="茶馆" width="160"/>
</picture>

# 茶馆 AI 素材管理

本地图片与视频素材管理工具，支持中文搜索、标签整理和 AI 辅助打标。

[![Release](https://img.shields.io/github/v/release/SSRYLJRSS/TeaCell_AiMedias?style=flat-square)](https://github.com/SSRYLJRSS/TeaCell_AiMedias/releases)
[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-green?style=flat-square)](LICENSE)

[下载](#下载) · [快速上手](#快速上手) · [开发](#开发)

</div>

## 功能

- **导入**：扫描文件夹，检查重复文件，预览改名结果；支持常见图片、HEIC、RAW 和视频格式。
- **浏览**：缩略图网格、评分、收藏、图片查看器和视频播放。
- **搜索**：中文关键词、标签和元数据筛选；超级搜索支持必须、优先和排除条件。
- **打标**：手工编辑标签和描述，或使用 AI 生成建议；确认后写入，支持批次撤销。
- **导出**：复制或移动素材，按标签或日期组织目录，并生成 CSV 清单。
- **维护**：重复与相似素材查找、回收站、数据库备份和恢复。

具体需求和格式限制见 [产品说明](docs/PRD.md)，平台差异见 [平台说明](docs/PLATFORM.md)。

## 下载

在 [最新版本下载页](https://github.com/SSRYLJRSS/TeaCell_AiMedias/releases/latest) 选择安装包：

| 系统与架构 | 安装包 |
|---|---|
| Windows 10/11 x64 | `TeaCell_AI_Media_Manager_<版本>_x64-setup.exe` |
| macOS Apple Silicon | `TeaCell_AI_Media_Manager_<版本>_aarch64.dmg` |
| Ubuntu 24.04 x64 | `TeaCell_AI_Media_Manager_<版本>_amd64.AppImage` |

当前为公测版本。macOS、Linux 尚未完成真机验收，macOS 安装包尚未签名与公证。平台支持标准见 [平台说明](docs/PLATFORM.md)，当前验证结果和待验收项见 [项目计划](docs/PROJECT_PLAN.md#20-v102-当前发布状态)。

对应源码、第三方许可证、构建记录和校验值可从各版本的 Release 说明获取。

## 快速上手

1. 安装并打开应用，选择素材库位置。
2. 在入库页添加文件夹，检查清单和改名结果，再确认导入。
3. 在素材库中浏览、评分或搜索素材。
4. 如需 AI 打标，在设置中配置服务，选择素材生成建议，检查后确认。
5. 选中需要的素材，导出到目标文件夹。

## 数据与 AI

素材、标签和索引默认保存在本机。导入、浏览、普通搜索和导出无需连接 AI 服务。

使用云端 AI 打标时，所选素材的预览会发送到你配置的服务；视频打标会发送抽取的视频帧。使用 AI 智能搜索时，查询内容会发送到所配置的服务。本地模型可通过 Ollama 兼容服务接入，应用内托管 Ollama 的功能目前仅面向 Windows。

API 密钥存储在系统凭据管理中。连接、数据目录和备份操作见 [运维手册](docs/OPERATIONS.md)。

## 开发

技术栈：Tauri 2、Rust、SQLite、React 19、TypeScript、Zustand 和 Tailwind CSS v4。

需要 Node.js 22–24、仓库固定版本的 Rust，以及目标系统的原生构建依赖。环境配置见 [开发规范](docs/DEVELOPMENT.md#1-环境) 和 [平台说明](docs/PLATFORM.md)。

```powershell
npm ci
npm run desktop:dev
```

桌面入口会准备并校验当前目标的 HEIF 原生依赖。测试、媒体工具准备和打包命令见 [开发规范](docs/DEVELOPMENT.md#2-日常命令)。

| 目录 | 内容 |
|---|---|
| `src/` | 前端页面、组件、状态和 API |
| `src-tauri/` | Rust 后端、Tauri 配置和原生依赖清单 |
| `scripts/` | 开发、测试和发布工具 |
| `docs/` | 产品、架构、开发和运维文档；展示图标位于 `docs/assets/` |
| `LICENSES/` | 第三方及历史许可证 |

文档阅读路径见 [文档中心](docs/README.md)。修改前请阅读 [AGENTS.md](AGENTS.md)。

## 反馈与贡献

欢迎提交 [Issue](https://github.com/SSRYLJRSS/TeaCell_AiMedias/issues) 或 Pull Request。报告问题时请附上应用版本、操作系统、复现步骤和错误信息；不要上传 API 密钥、私有素材或完整数据库。

## 许可证

项目以 [GPL-3.0-or-later](LICENSE) 发布。分发程序或修改版本时，应保留版权和许可证说明，并提供符合 GPL 要求的对应源码与构建材料。第三方组件许可见 [NOTICE](NOTICE)。此前以 MIT 发布的代码保留原授权，文本见 [历史 MIT 许可证](LICENSES/TeaCell-legacy-MIT.txt)。
