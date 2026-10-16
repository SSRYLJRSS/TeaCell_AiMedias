# 插件行业调研：网页界面、后台运行与真实权限边界

> 分期说明：本章是 **2.5 插件系统的研究储备**，2.0 不开发这些运行平台。实施时重新核实版本和选型，不提前冻结 SDK；开发顺序见 [08](08-migration-and-delivery.md)。

> 日期：2026-10-07
> 状态：Proposal 的研究依据；不是已实现能力，也不替代 `docs/` 的现行产品、平台和架构契约。
> 范围：官方一手资料比较与技术推导；未安装候选运行时、未运行插件 PoC、未做性能和包体测量。
> 设计落点：[插件平台提案](05-plugin-platform.md)。通用权限见 [权限与数据安全](06-permissions-and-data-safety.md)，验证见 [验收与风险](09-validation-and-risk.md)。

## 1. 研究结论

用户要的是可以安装、运行并持续维护的插件生态：第三方作者能用 HTML/JavaScript 创建画布、联网生图、图片处理和其他工具；用户安装本地包，在茶馆主界面使用；插件拥有自己的工程数据，并能通过授权接口使用素材库和本地资源。

这要求把以下四种能力分别设计：网页 UI、受控业务 API、独立存储、后台任务。单纯增加底栏按钮不构成插件平台；只开放声明式表单也不能满足完整画布；要求所有插件编译成 WASM 会增加网页作者的门槛。反过来，直接开放 Node/原生代码虽然便利，却改变了可信边界。

**推荐判断（提案）：普通插件以网页包 + 宿主 SDK 为标准；持续任务交给宿主；任意后台代码和原生引擎分别作为可选运行层。** 不把“独立页面”“独立目录”“独立进程”写成已经获得操作系统沙箱的证明。具体身份、生命周期和协议只在 [05](05-plugin-platform.md) 定义。

## 2. 五类成熟生态的事实比较

| 生态 | 开发与 UI | 背景运行与宿主接口 | 权限和资源的真实边界 | 可借鉴的部分 |
|---|---|---|---|---|
| VS Code | JS/TS 扩展；自定义 UI 使用 Webview；命令、菜单、设置通过贡献点注册 | 本地/远程通常使用 Node 扩展宿主；Web 扩展使用 Browser WebWorker；后台寿命不绑定单个可见面板 | 本地扩展宿主继承 VS Code 权限，可以文件、网络、子进程操作；UI Webview 的隔离不使整个扩展低权限 | 贡献点、SDK、版本兼容、存储、打包与诊断 |
| Obsidian | TS 编译为 JS；通过 API 添加命令、设置、视图、状态栏等 | 插件启用时加载，卸载时清理事件/计时器；运行在宿主能力范围内 | 官方明确不能可靠细分插件权限；启用社区插件就是信任其作者 | 轻量开发模板、启停管理、作者可理解的 API |
| Figma | JS/HTML；编辑器 API 的执行代码与 UI iframe 分开，用消息通信 | 插件操作由用户发起；官方明确不支持常驻后台插件 | 受限 JS 环境与场景 API；UI 具备浏览器能力；网络域名声明有嵌套网页资源的限制 | 文档 API 与网页 UI 分离、类型定义、命名空间存储 |
| Adobe UXP | JS/HTML 面板和命令，manifest 指定入口、宿主版本及权限；UXP 不等于完整 Chrome 页面 | 有插件与面板生命周期；Hybrid 可加原生 C++ addon | 普通 UXP 有宿主权限 API；`fullAccess` 与 native addon 是更大的信任面，不能笼统称全部插件同样受限 | 专用文件存储、文件选择授权、权限分层、面板生命周期 |
| Chrome 扩展 | HTML 页面、弹窗/侧栏、JS service worker；manifest 声明权限 | MV3 service worker 随事件唤醒，空闲可能终止；DOM 工作可用 offscreen document | 扩展 API/host permissions、独立页面来源与 CSP；native messaging 是另行安装的本地程序通道 | 事件化后台、持久状态、可选权限、UI 与后台分开 |

表中事实依据分别为：[VS Code Extension Host](https://code.visualstudio.com/api/advanced-topics/extension-host)、[VS Code 扩展安全](https://code.visualstudio.com/docs/configure/extensions/extension-runtime-security)、[Obsidian 插件安全](https://obsidian.md/help/plugin-security)、[Obsidian Plugin API](https://docs.obsidian.md/Reference/TypeScript+API/Plugin)、[Figma 运行模型](https://developers.figma.com/docs/plugins/how-plugins-run/)、[Figma Introduction](https://developers.figma.com/docs/plugins/)、[UXP Manifest](https://developer.adobe.com/uxp/guides/explanation/concepts/manifest/)、[UXP Manifest v5 权限说明](https://developer.adobe.com/photoshop/uxp/2022/guides/uxp_guide/uxp-misc/manifest-v5/)、[Chrome SW 生命周期](https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle)、[Chrome 权限](https://developer.chrome.com/docs/extensions/develop/concepts/declare-permissions)。

### 2.1 VS Code：体验值得借鉴，不能照搬全权限宿主

**事实。** VS Code 不允许扩展直接修改主界面 DOM，而是提供稳定 UI API 与 Webview。Webview 必须控制本地资源范围、CSP 和消息输入。扩展声明 `engines.vscode` 兼容范围，使用贡献点注册入口，并可打包为 VSIX。[能力边界](https://code.visualstudio.com/api/extension-capabilities/overview)、[Webview](https://code.visualstudio.com/api/extension-guides/webview)、[Extension Manifest](https://code.visualstudio.com/api/references/extension-manifest)、[发布与打包](https://code.visualstudio.com/api/working-with-extensions/publishing-extension)

**事实。** 宿主管理 workspace/global 状态、数据目录与 secret storage 是不同接口；不能把普通插件目录当密钥库。市场包签名验证来源和完整性，安全扫描与可信发布者管理仍是其他机制。[Common Capabilities](https://code.visualstudio.com/api/extension-capabilities/common-capabilities)、[Runtime Security](https://code.visualstudio.com/docs/configure/extensions/extension-runtime-security)

**对茶馆的推导。** 借鉴“入口声明 + 稳定 SDK + 独立 UI + 调试工具”。如果另提供可使用完整 npm 和 Node fs/net 的扩展宿主，应明确标为可信高权限运行层；不能告诉用户它受到网页插件的目录和域名权限限制。

### 2.2 Obsidian：开发简单来自较宽的信任，不是自动安全

**事实。** 官方样例依赖 Node 开发环境，将 TS 编译为 JS，再从专门开发 vault 加载；文档提醒不要在主 vault 开发。`loadData/saveData` 存在插件目录的 `data.json`，`registerEvent/registerInterval` 帮助卸载时清理。用户可分别安装、启用、禁用、卸载插件；社区插件更新默认需要手动操作。[开发流程](https://docs.obsidian.md/Plugins/Getting+started/Build+a+plugin)、[Plugin API](https://docs.obsidian.md/Reference/TypeScript+API/Plugin)、[社区插件管理](https://obsidian.md/help/community-plugins)

**事实。** `minAppVersion` 与 `versions.json` 处理旧应用可用插件版本。Restricted Mode 阻止社区代码执行，但不是给启用后的插件逐项授予文件、网络权限。[兼容版本](https://docs.obsidian.md/Reference/Versions)、[Plugin Security](https://obsidian.md/help/plugin-security)

**对茶馆的推导。** 学习其轻量模板和管理体验；用户安装一个画布插件不应顺便把全部素材原文件、凭据和数据库交给该插件。独立存储对稳定性有帮助，不能承诺防止同账户可信原生代码或用户本人访问。

### 2.3 Figma：网页 UI 与领域 API 分离，后台限制必须讲清楚

**事实。** Figma 的业务代码位于受限 JS 环境，UI iframe 接触浏览器 API，通过消息桥访问业务代码。插件结束调用 `closePlugin`，用户可以取消；官方 Introduction 明确其插件不是后台常驻机制。[How Plugins Run](https://developers.figma.com/docs/plugins/how-plugins-run/)、[Introduction](https://developers.figma.com/docs/plugins/)

**事实。** manifest 分开声明 `main`、`ui`、API 版本与 `networkAccess`。官方提醒：嵌入第三方网站时，域名限制只约束直接嵌入的网站，不覆盖该网站加载的所有资源。`clientStorage` 以插件 ID 隔离，不与文档协同数据混同；它不是对本机用户的保密存储。[Manifest](https://developers.figma.com/docs/plugins/manifest/)、[Client Storage](https://developers.figma.com/docs/plugins/api/figma-clientStorage/)

**对茶馆的推导。** “素材 API 在宿主，HTML UI 在隔离页面”是强参考。茶馆要允许离开页面后仍继续生成图片，必须另设计宿主任务和恢复，不应从 Figma 的 UI 分离推出它已经提供此能力。

### 2.4 Adobe UXP：最接近本地素材工具，但必须区分普通与 Hybrid

**事实。** UXP manifest 可指定 HTML/JS 入口、命令/面板、宿主兼容条件。权限包含域名网络、剪贴板、文件访问、打开外部应用、插件间通信、Webview 等。默认插件存储与用户文件选择授权、全文件访问是不同等级；`launchProcess` 文档讲的是受声明和同意控制的 `openExternal/openPath`，不能等同任意命令行执行接口。[Manifest](https://developer.adobe.com/uxp/guides/explanation/concepts/manifest/)、[Manifest v5 Permissions](https://developer.adobe.com/photoshop/uxp/2022/guides/uxp_guide/uxp-misc/manifest-v5/)

**事实。** UXP 面板具有 create/show/hide/destroy 等生命周期钩子；网页插件不应把 hide 当作可靠的退出或唯一保存机会。Hybrid 可以加载原生 `.uxpaddon`，因此还涉及不同架构二进制、签名和独立发布工作。官方工具生成 `.ccx` 安装包。[生命周期](https://developer.adobe.com/uxp/guides/how-to/add-lifecycle-hooks/)、[Hybrid Plugins](https://developer.adobe.com/uxp/guides/how-to/hybrid-plugins/)、[打包](https://developer.adobe.com/uxp/guides/how-to/distribution/package/)

**对茶馆的推导。** 借鉴“插件工作区 + 用户选择文件 + 扩大权限单独信任”的设计。原生引擎尽量放到有明确所有权的外部 connector 进程，而非把未经审查动态库加载进持有数据库的 Rust 进程。此取舍减少同进程崩溃与接口污染，不证明原生进程被 OS 沙箱约束。

### 2.5 浏览器扩展：后台状态应耐终止，不能依赖内存常驻

**事实。** Chrome MV3 worker 可能因空闲、过长操作等原因停止，官方要求持久化状态而非依赖全局变量。Offscreen document 提供隐藏 DOM 上下文，其扩展 API 主要经 runtime 消息使用；它不是普通网页现成可用的通用服务。[SW Lifecycle](https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle)、[Offscreen](https://developer.chrome.com/docs/extensions/reference/api/offscreen)

**事实。** 权限分 API permission 与 host permission，支持可选权限；sandbox pages 无扩展 API，需消息桥。扩展 storage 不同于页面 localStorage；native messaging 要另配宿主程序和允许调用的扩展来源。[权限声明](https://developer.chrome.com/docs/extensions/develop/concepts/declare-permissions)、[Sandbox](https://developer.chrome.com/docs/extensions/reference/manifest/sandbox)、[Storage](https://developer.chrome.com/docs/extensions/reference/api/storage)、[Native Messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)

**对茶馆的推导。** 背景任务的真相保存在宿主，UI 只订阅；作者可接可恢复的事件/任务，而不是被要求永久开着页面。Tauri 系统 WebView 没有 Chrome Extensions 平台，不能直接移植 `chrome.offscreen` 或 MV3 管理器。

## 3. 后台计算运行时的必要比较

| 候选 | 官方事实 | 对茶馆的适用判断（提案） |
|---|---|---|
| QuickJS 嵌入式 JS | 引擎提供内存上限、中断回调和模块接口；命令行标准库包含 `std/os` | 可做无 DOM 的作者 JS 后台，但需自建模块加载、事件循环、异步宿主桥与资源边界；未验证前不列为稳定 SDK 能力 |
| Extism / Wasmtime | Rust 宿主加载 WASM，提供 host functions；有 fuel、超时、内存等限制；JS PDK 将 JS 和解释器封装成 WASM | 适合可终止的小型纯计算、批次步骤，WASM可选；复杂异步后台不是现成能力 |
| Deno 进程 | 默认运行期 I/O 限制；可授权路径和网络；`allow-run/allow-ffi` 会引入可绕过 JS 权限的代码 | 可作为后台候选，但独立进程和权限 flags 不足以证明任意代码完全隔离；不默认开放 subprocess/FFI |
| Node / 原生 connector | Node 完整系统 I/O 与本地依赖生态便利；原生代码按 OS 进程权限执行 | 用户明确全信任后可使用完整依赖；manifest只约束宿主broker，不约束其直接系统调用 |

事实依据：[QuickJS](https://bellard.org/quickjs/quickjs.html)、[Extism Rust SDK](https://github.com/extism/extism/blob/main/runtime/README.md)、[Extism JS PDK](https://github.com/extism/js-pdk/blob/main/README.md)、[Deno Security](https://docs.deno.com/runtime/fundamentals/security/)。这里的“Node 完整 I/O”对比亦由 Deno 官方安全文档明确说明；不声称所有 Node 版本均无法配置额外权限机制。

### 3.1 Extism 的易用性不是普通网页插件的易用性

官方 JS PDK 构建需要 bundle 为 CJS/ES2020，再由 `extism-js` 输出 WASM；开发工具还涉及 Binaryen。插件内不是 Node/浏览器完整环境，缺少 DOM、常规定时器、子进程等；当前主分支文档说明异步语法不意味着有可并发的事件循环。消费者安装预编译产物则无须安装编译链。[JS PDK](https://github.com/extism/js-pdk/blob/main/README.md)

因此不要求画布或生图页面作者学这套工具；只有选择 WASM compute contribution 的作者需要它。应对锁定发行版本确认主分支描述是否适用，不能把“某 JS 包能 bundle”说成“任意 npm 包都支持”。

### 3.2 超时、路径白名单和进程隔离各自解决不同问题

Wasmtime WASI 文件访问使用预授权目录的能力模型；默认无预打开目录，网络地址默认拒绝。但 host function 可能自行读库或联网，开放哪些宿主函数仍决定实际权限。fuel/epoch 只能限制正在执行的 Wasm，无法中断阻塞宿主函数。[WASI Builder](https://docs.wasmtime.dev/api/wasmtime_wasi/struct.WasiCtxBuilder.html)、[Wasmtime Config](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html)、[Extism Host Functions](https://extism.org/docs/concepts/host-functions/)

Deno 官方同时提醒：静态模块加载存在权限检查例外，完整不可信执行需要多层防护；子进程、FFI 的代码不被其 JS 权限自动限制。若选择 Deno，应锁定依赖、离线加载、拒绝交互式提权并另验证 OS 层，不能只写“Deno 默认安全”。[Deno Security](https://docs.deno.com/runtime/fundamentals/security/)

## 4. 易开发、易安装、易维护应怎样落地

以下是茶馆提案，不是对行业规模的统计结论。

1. **开发起点。** JS 最小示例可以不经过 WASM；TS/React 模板提供本地打包、类型定义、SDK mock、专用测试素材库。用户写画布时保留网页自由度，主题变量通过 SDK 提供。
2. **安装产物。** 面向用户只发布完整网页包，禁止安装时运行 npm 生命周期脚本；依赖编译与打包在作者侧完成。开发目录加载和正式签名包是两种模式。
3. **可理解权限。** 用“读取选中素材预览”“上传这些图片到此服务”“读写此文件夹”“调用可信本地引擎”表达能力。较大目录、网络授权可以存在，但以真实范围展示，不让含糊的“AI增强”涵盖所有 I/O。
4. **稳定 SDK。** 业务接口返回素材摘要/句柄/任务，不暴露 Rust 内部函数或 SQL。API compatibility、Web SDK、native protocol、WASM ABI 分开版本化。
5. **维护工具。** 包验收、权限差异、配额、日志、任务状态、禁用、回退、卸载保留数据等从首次实现就提供；没有市场服务器也能可靠安装本地包。
6. **生态治理。** 样例、模板和明确许可政策优先于排行榜。插件 ID 与作者来源稳定，审核/签名/运行限制分工清楚；不能用“官方市场”替代技术权限约束。

## 5. 本仓库对选型的约束

**当前事实。** 茶馆是本地素材库而非浏览器标签页或仅存在云端的文档：主数据库有 FTS 触发器和自定义中文 tokenizer，图片解码有统一入口，正式 AI 标签经确认后写入，文件和 DB 结果不能假成功。对应权威来源为 [架构](../../docs/ARCHITECTURE.md)、[产品契约](../../docs/PRD.md)、[平台支持](../../docs/PLATFORM.md)。

**推导。** 第三方插件需要较大能力时，增加明确受控的目录、业务操作和网络服务，而非给主数据库连接。可信 Node/native 程序如果得到同账户 OS 权限，技术上可能自行找到主库；此时“禁止直接写库”只是支持契约，不能宣传成可强制隔离。实际现状审计和改造门禁见 [05](05-plugin-platform.md)。

现行组合包采用 GPL-3.0-or-later；第三方 SDK、网页包、原生 connector 和运行时依赖的分发条款需单列审查，不能根据 WASM/进程边界直接给出闭源许可法律结论。[架构中的分发约束](../../docs/ARCHITECTURE.md)

## 6. 需要停止宣传、先做 PoC 的承诺

- 不承诺 Tauri child WebView 当前已在全部支持目标拥有相同布局和事件行为。
- 不承诺 HTML 页面获得独立目录就具有硬 CPU/内存或操作系统隔离。
- 不承诺网页关闭、应用退出后任意 JS Promise 自动继续。
- 不承诺联网请求停止就能追回已外发素材或取消云端计费。
- 不承诺签名包没有恶意，也不承诺宿主撤权可约束全信任 native connector 的直接 fs/net。
- 不承诺 Figma/UXP/VS Code/Chrome 的插件可不经适配直接装进茶馆。

具体平台方案和进入开发的先决证据见 [05](05-plugin-platform.md)；跨版本发布验收由 [09](09-validation-and-risk.md) 汇总。
