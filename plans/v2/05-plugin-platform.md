# 插件平台提案：网页 UI、宿主 SDK 与独立运行空间

> 分期说明：本章整体移至 **2.5 插件阶段**；文中“首版”指插件首版。2.0 仅按 [08 第5节](08-migration-and-delivery.md#5-20-到底预留哪些接口) 保留业务接点，不实现这里的容器、安装器、工作区、SDK 或额外运行时；具体格式在 B0 重新核实。

> 日期：2026-10-07
> 状态：Proposal；下文的名称、JSON、SDK、状态机和模块边界均为待实现提案，不能据此宣称应用已有这些能力。
> 决策依据：[行业调研](04-plugin-industry-research.md)。本文件是本方案的插件设计唯一落点。
> 边界：通用授权与事务守卫引用 [06](06-permissions-and-data-safety.md)；发布验收引用 [09](09-validation-and-risk.md)。现行实现仍以 [架构](../../docs/ARCHITECTURE.md)、[平台契约](../../docs/PLATFORM.md)、[开发规范](../../docs/DEVELOPMENT.md) 为准。
> 研究方法：只读核查仓库与官方资料；未编译集成、未创建插件、未测试三端 WebView 或原生 connector。

## 1. 目标与建议决定

插件应成为完整的小型应用，而不只是一段提示词或一个菜单按钮。作者可提交本地 HTML/JS/CSS 包，使用普通网页技术构建白板/画布、图片编辑、批次整理或联网生图页面。用户从底栏进入插件中心，在茶馆主窗口使用这些页面；插件拥有自己的配置、项目草稿、缓存与任务入口。

建议确立三个运行等级：

| 等级 | 能力 | 首版定位 | 信任边界 |
|---|---|---|---|
| 普通网页插件 | 任意自身 HTML UI、Canvas、普通 JS；经宿主 SDK 访问素材、工作区、批准目录和服务 | 标准插件格式、2.5 核心目标 | 低权限独立 WebView + 受控 IPC/资源/网络，不接入主 DOM |
| 可选后台计算插件 | 无 DOM 的计算处理器，首选候选为 WASM；纯 JS headless runtime 另经 PoC | 非网页插件的必装前置，可按通过的运行时开放 | host functions/SDK 与独立资源限额；不等于任意 Node |
| 可信原生 connector | Node/Python/本地可执行程序或现有本地服务；可使用完整依赖、GPU、模型/处理引擎 | 单独安装、单独全信任提示，普通包不能偷偷提升为此类 | 未加 OS 沙箱时，按同账户系统权限执行；宿主 broker 只约束 broker 调用 |

较大权限是可支持的产品能力：用户可以批准较大的素材集合、一个目录、多个联网服务、较高的工作区配额或一个可信本地引擎。权限扩大应有真实范围和不同信任等级；不能通过一句“所有插件都不允许”回避需求，也不能把上述三个等级混成同一种安全承诺。

网页插件关闭后继续生图、入库或导出，主要依靠宿主持久任务。任意 JS 后台执行不是与 HTML 插件一起自动附送的能力。运行协议见第 8–10 节。

## 2. 当前仓库核查：必须先收敛的入口

以下为核查时的实现事实，不是新插件平台现状。

| 核查位置 | 读到的事实 | 接入插件前的必做事项 |
|---|---|---|
| [capabilities/default.json](../../src-tauri/capabilities/default.json) | 按 `windows: ["main"]` 授权 core/event/dialog/fs/shell 的若干默认权限 | 迁移为可信主 WebView 的明确 label；不让同主窗口的插件子 WebView继承 |
| [build.rs](../../src-tauri/build.rs) | 调用 `tauri_build::build()`；未见 `AppManifest::commands` 应用命令 ACL 清单 | 对现有注册命令逐项纳入 ACL，新增命令有拒绝默认与自动审计 |
| [lib.rs](../../src-tauri/src/lib.rs) | 注册大量素材、标签、设置、备份恢复和 AI 连接命令 | 插件不直接获这些 commands；只能调用受控网关，后端直接调用同一 services |
| [tauri.conf.json](../../src-tauri/tauri.conf.json) | 单 `main` 窗口，已有主应用 CSP；asset protocol 开启且静态 scope 为空 | 独立制定插件内容 CSP/资源网关，不能照搬主应用已有 asset 授权 |
| [lib.rs](../../src-tauri/src/lib.rs) 的 setup | 放行 thumbnails/previews/proxies 目录 | 这是应用媒体加载范围，不是 per-plugin/per-session 的素材授权 |
| [assets_cmd.rs](../../src-tauri/src/commands/assets_cmd.rs) | `list_assets/get_asset/dedup/get_asset_urls` 相关路径会逐文件放行；返回资产含原路径 | 插件查询只返裁剪 DTO 与短期资源句柄；不能重用原路径和 asset URL作为权限凭据 |
| [super_search_cmd.rs](../../src-tauri/src/commands/super_search_cmd.rs)、[import_cmd.rs](../../src-tauri/src/commands/import_cmd.rs) | 搜索结果和待入库预览也可增加 asset 放行 | 检查全部放行入口，不只检查普通素材列表 |
| [Cargo.toml](../../src-tauri/Cargo.toml)、[Cargo.lock](../../src-tauri/Cargo.lock) | `tauri` 锁定 2.11.5，已开 `protocol-asset`，未开 child WebView 的 `unstable` feature | PoC 必须针对锁定版本；不能用最新文档代替项目实测 |

Tauri 官方明确：按 window 授权会覆盖其所有 webviews；多个 capability 的权限合并。应用通过 `invoke_handler` 注册的自定义命令默认可由所有 windows/webviews 使用，需要 `AppManifest::commands` 将其纳入 ACL。这是目前单可信前端的架构扩展为第三方代码时必须处理的边界，**不代表已经存在一个外部插件漏洞**。[Capability Reference](https://v2.tauri.app/reference/acl/capability/)、[Capabilities](https://v2.tauri.app/security/capabilities/)

当前 `asset_protocol_scope` 的逐文件收敛有价值，但其调用接口没有绑定插件主体。即使插件读不到某个新 URL，也不能证明它无法访问主界面已经放行的 URL。插件入口必须有单独资源检查，而不是把全局 asset scope 当作授权账本。

## 3. 主窗口容器选择与 PoC 门禁

### 3.1 推荐目标：独立 child WebView 嵌入插件内容区域

可信主 WebView 保留底栏、插件列表、权限确认和任务中心。宿主原生代码创建低权限 child WebView，覆盖主窗口指定内容区域，按插件实例分配不可由插件自行指定的 label。

child WebView 拥有自己的 DOM；作者在其中自由实现网页 UI，但无法导入茶馆 React store 或修改可信导航。主界面通过宿主安排显示/隐藏、尺寸、焦点和生命周期；不要让插件自己创建任意 Tauri windows/webviews。

**官方事实。** child WebView 创建仍属于 `unstable`；普通单 WebView 窗口不需要该 feature。Windows 文档提示同步命令/事件处理中的创建可能死锁。[JS WebView](https://v2.tauri.app/reference/javascript/api/namespacewebview/)、[Rust Builder](https://docs.rs/tauri/latest/tauri/webview/struct.WebviewBuilder.html)、[WebviewWindow](https://v2.tauri.app/reference/javascript/api/namespacewebviewwindow/)

PoC 要验证主页面覆盖、滚动、DPI、布局变化、输入法、快捷键、模态遮挡、窗口最小化/全屏、隐藏后任务、关闭后清理。不能把 native child WebView 当成任意 HTML DOM节点；三端行为不能凭 Windows 结果推定。

### 3.2 降级与不采用的捷径

若某平台 PoC 不通过，可以明确禁用内嵌并提供低权限独立窗口；这属于可见降级，而不是偷偷把第三方页面注入主前端。是否满足该平台 2.5 插件验收由 [09](09-validation-and-risk.md) 决定。

不采用主 WebView `dangerouslySetInnerHTML`、动态 `import()` 用户 JS、直接加载主 origin 下的用户脚本作为隔离方式。Tauri 官方还说 Linux/Android 不能区分内嵌 iframe 与窗口自身的 IPC 请求，所以 `sandbox iframe` 不是高权限主 WebView里的充分 IPC 安全边界。[Capabilities 的 remote access 限制](https://v2.tauri.app/security/capabilities/)

可以在已经低权限的插件 WebView 内加 trusted wrapper + sandbox iframe，但它只是附加浏览器层，需额外设计消息、origin、存储和导航；不作为第一条实现路径。MCP Apps 等外部网页互操作也应经过这一宿主隔离入口，不在主高权限 WebView 直接渲染。

上游变更的处理顺序由 B0-01/B0-02负责：锁定待验证的Tauri精确版本和容器Adapter；发现 `unstable` API、ACL、资源或焦点行为变化，先停止受影响的插件接入，保留已有业务功能。优先修复Adapter并重跑P02；内嵌无法满足时评估**低权限独立窗口**的显式降级，仍须通过全部隔离测试。两者均不成立则禁用该平台插件能力并复核发布范围，不能改用高权限主WebView里的iframe来通关。

可短期保留经验证且无适用阻断性安全问题的旧组合，但需登记责任人与复核期限；不能长期锁住已知不安全版本来回避升级。`iframe + postMessage` 若再次评估，只能作为经过新威胁分析与P02的候选，不能视为已获批准的等价回退。兼容性升级流程见 [14 §6](14-review-and-handoff.md#6-依赖升级责任与放行)。

## 4. 插件包与 Manifest 提案

包是完整、不可变的网页构建产物，建议暂称 `.teacell-plugin`；扩展名在实现阶段冻结。内部包括 manifest、入口 HTML、JS/CSS、图标和离线资源；可包含经声明的 compute WASM，但普通网页包不能包含自动执行的安装脚本或偷偷启动的二进制。

下面是**语义草案，非已经冻结/实现的 schema**：

```json
{
  "manifestVersion": 1,
  "id": "example.canvas-generator",
  "name": "画布生图示例",
  "version": "0.1.0",
  "publisher": { "id": "example", "keyId": "publisher-key-1" },
  "host": { "app": ">=2.5.0 <3.0.0", "sdk": "^1.0.0" },
  "ui": { "entry": "ui/index.html", "preferredSize": { "width": 1000, "height": 700 } },
  "storage": { "schemaVersion": 1 },
  "contributes": {
    "commands": [{ "id": "open", "title": "打开画布" }],
    "assetActions": [{ "command": "open", "selection": "images" }],
    "agentTools": []
  },
  "permissions": {
    "required": ["workspace.readWrite", "assets.preview.read"],
    "optional": ["assets.original.read", "directories.readWrite", "artifacts.import", "ai.images.generate"],
    "network": { "mode": "broker", "origins": ["https://api.example.com"], "reason": "生成图片" },
    "background": "host-jobs"
  },
  "requiresCapabilities": ["pluginEmbeddedUi", "canvas2d"],
  "limits": { "workspaceBytes": 1073741824, "concurrentJobs": 2 }
}
```

原始 schema 的未知键处理、SemVer 范围、origin 和权限词汇必须在开发前校验；权限名不允许被任意自定义字符串自动转成能力。`limits` 是作者申请，实际预算取宿主政策与用户授予的交集。可选权限安装时不自动授予。`publisher.keyId` 只是包声明，必须与验证过的签名者对应。

插件贡献入口、菜单、工具等由 manifest 声明，宿主注册；HTML不必用声明式 UI。宿主可以声明式渲染安装权限和设置摘要，同时保留插件内容页的网页自由度。这两种用途不能混淆。

## 5. 身份模型与网关协议

### 5.1 必须由宿主建立的身份

安装身份至少包含 `pluginId + publisherKey + packageDigest + pluginVersion`。更新同名包不能仅凭名字继承授权；作者密钥变化须显式处理。

运行身份由宿主登记：

```text
PluginInstance
  principal: 宿主认定的插件主体
  packageDigest / pluginVersion
  instanceId / sessionId
  runtimeKind: webview | compute | trusted_connector
  actualWebviewLabel 或 actualProcessHandle
  allowedContentLocation
  grantEpoch / libraryEpoch
  status: starting | active | closing | revoked | closed
```

`principal`、授权代次等通用语义由 [06](06-permissions-and-data-safety.md) 定义；本文件规定插件如何绑定到它。Rust 网关使用真实调用 WebView/进程句柄查登记表，不信消息中自报的 pluginId、source=agent、权限列表或任何“已经确认”的 boolean。

webview labels 不由插件选择；仅可信宿主创建，随机实例 ID避免覆盖正在运行的旧实例。创建/导航、RPC验证、撤权必须使用同一登记状态，避免页面跳转后沿用旧权限。

### 5.2 受控 `plugin_rpc`，而不是重导出现有 invoke

插件只有一个狭窄的业务网关和必要的定向回应通道；可信主 WebView 独占审批接口和插件安装管理。不能给普通插件 `core:event:default` 就假定任意事件广播可作私密通道。

建议请求草案：

```json
{
  "protocol": 1,
  "sessionId": "宿主签发会话",
  "requestId": "本会话唯一请求",
  "method": "workspace.read",
  "params": { "fileHandle": "opaque-handle", "offset": 0, "length": 4096 }
}
```

`sessionId` 是会话绑定和防重放辅助，不是替代真实调用者身份的秘密口令。每次 RPC 顺序为：识别 caller → 检查活动安装/实例/URL/epoch → schema/大小限制 → 方法白名单 → 通用授权守卫 → 业务 services。回应匹配 requestId，错误沿用稳定 AppError 分类（如 `unauthorized/conflict/unsupported/cancelled/timeout`），再附安全的失败原因；不得为插件返回完整底层路径、凭据或 SQL错误。

握手返回 SDK/protocol 版本、静态平台能力、实际批准预算和授权摘要。插件不能通过握手把 requiredPermissions 全自动变成 grantedPermissions。API 调用返回主库 DTO 的裁剪版本；请求响应中的信息本身也受授权范围限制。

iframe 若作为附加层，握手要验证准确来源和 `event.source`，再绑定专用 MessagePort；opaque origin 不可仅凭 `"null"` 认身份。消息速率、结构、payload 上限都需守卫。不得向 `targetOrigin: "*"` 发送素材或凭据。独立 native WebView 不是 HTML iframe，其身份按 Rust 真实 WebView确定。[WHATWG 消息规则](https://html.spec.whatwg.org/multipage/web-messaging.html)

## 6. 网页资源、安全策略与导航

### 6.1 包资源网关

宿主只为活动安装和实例提供当前包的 HTML/JS/CSS/assets。响应须正确 Content-Type、禁止目录列举、缓存键包含包摘要；路径规范化后检查仍位于包根，拒绝跨插件资源和解包后的链接越界。资源处理逻辑属于服务层，不能在 command 薄壳内堆文件业务。

per-plugin origin 是设计目标，但其构造须先 PoC：把 `/plugin/id` 放进一个共享 origin 不产生隔离；custom scheme 在各平台的 origin 映射不同。需要至少在实际请求上下文中确认 webview label、当前实例和资源所属包；不能只靠 URL中的 pluginId。若网关无法可靠识别实例，需要隔离资源服务/不可跨主体使用的会话能力，而不是去掉检查。[Tauri custom protocol](https://docs.rs/tauri/latest/tauri/struct.Builder.html#method.register_uri_scheme_protocol)

主应用的 asset protocol 可继续服务可信界面，普通插件内容的 CSP 不允许直接使用它；插件拿到的 preview/original 必须经插件资源网关。验证包括插件尝试使用主界面已放行过的 asset URL。若系统 WebView 在任何受支持目标无法关闭该旁路，插件数据隔离 PoC 不通过。

### 6.2 宿主强制 CSP

建议普通包默认只执行包内 JS，禁远程可变脚本、eval、任意 frame/object，浏览器外网默认关闭。按能力开放 Worker、WASM 或必要 blob，用真实请求验证。字体、样式、图片、媒体、连接、表单、frame、导航需分别考虑；不能只限制 fetch。允许 CSS必要的内联样式与允许任意内联 JS是不同决定。

CSP 由宿主响应层强制，不交给插件删改 meta。Tauri 对编译时打包 assets 的 CSP加工不自动证明运行时下载包已经得到同样保护；官方 `on_web_resource_request` 目前只处理 tauri 协议响应，不拦截全部 external URL资源。[Tauri CSP](https://v2.tauri.app/security/csp/)、[WebviewBuilder](https://docs.rs/tauri/latest/tauri/webview/struct.WebviewBuilder.html)、[CSP](https://www.w3.org/TR/CSP3/)

top-level navigation、new window、download 由宿主处理；退出允许位置立即撤销 UI session，再决定是否在外部浏览器打开链接。禁用未经授权的摄像头、麦克风等浏览器权限。Service Worker、持久 cache 和 browser profile 是否按插件隔离要实际验证；首版不允许它们作为绕过包更新/撤权的隐藏代码来源。

网页 UI 仍可访问浏览器允许的功能；CSP/IPC策略减少暴露，不承诺防 WebView 漏洞或硬 CPU/RAM隔离。[Tauri Security](https://v2.tauri.app/security/)

## 7. 标准 SDK 与独立运行空间

以下接口为功能草案；实际 `.d.ts`、Schema、Rust DTO和测试必须同批冻结。

| SDK 域 | 拟提供接口 | 具体约束 |
|---|---|---|
| `host` | `connect/getCapabilities/getVersion` | 静态平台能力来自后端；不由网页探测 OS推定 |
| `selection` | `snapshot` | 当前选中项冻结为资源集合；不用会变化的“当前全部”作为永久授权 |
| `assets` | `query/getSummary/readPreview/requestOriginal` | 默认无原路径、无主库文件路径；原图权限独立，查询分页且返回最小字段 |
| `workspace` | `kv.get/set/transaction`、`files.list/read/write/remove/stat` | 宿主管理的插件空间；稳定 handle、相对逻辑路径、配额与原子写 |
| `directories` | `request/list/read/write/create/remove` | 文件选择器/可信授权目录 handle；可批准递归与写入，必须分别声明 |
| `connections` | `listAuthorizedProfiles` | 返回可用服务引用和能力，不返回密钥或完整设置 |
| `network` | `request/readChunk/cancel` | 网关代理、逐目标批准、预算；不允许任意系统代理/TLS危险选项 |
| `ai` | `generateImages/inspectJob/cancelJob` | 宿主管理服务请求；成本与上传素材明确，provider capabilities单独验证 |
| `artifacts` | `createUpload/writeChunk/finish/inspect/import` | 上传仅进入临时产物；验证后入库，不能指定任意宿主路径 |
| `tasks` | `create/get/subscribe/cancel/resume` | 持久宿主任务；每步仍复核有效授权，取消不等于回滚 |
| `ui` | `requestOpen/showNotification/saveState/onVisibilityChanged` | UI请求由宿主控制；可信审批不在插件 HTML内实施 |
| `tools` | `invokeContribution` | 已注册工具按 schema与权限调用；插件不能临时注册无限系统工具 |
| `connectors` | `listApproved/invoke/status` | 单独批准可信引擎，原生信任等级向用户显示 |

### 7.1 插件存储不是第二个素材库

建议至少区分：只读安装包、持久配置/KV、工程文件、可清理缓存、临时产物、宿主任务账本。

安装包按版本不可变；工作区按稳定安装主体与 library/workspace 上下文分开。插件工程可以包含画布 JSON、处理参数、引用素材 ID和派生缩略图；正式素材、标签与搜索索引仍由茶馆主库管理。插件只能声明自己的存储 schemaVersion，不能跑主库迁移。

作者通过 SDK可以在自己工作区自由保存项目，不必每个草稿保存都确认；授权目录读写可覆盖用户选定树，不必每个块弹确认。宿主仍检查配额、路径越界、符号链接/重解析点和目录替换，破坏性操作按 [06](06-permissions-and-data-safety.md) 守卫。

持久数据归宿主管理，不依赖可能被清空的 WebView localStorage/IndexedDB作为唯一工程事实源。页面缓存可以使用，但必须能从 SDK持久状态恢复。更新代码不顺便删除工程；卸载工程导出与数据清除见第 12 节。高权限原生程序可接触同账户文件，独立目录不是对它的保密隔离。

### 7.2 文件与媒体 handle

资源 handle至少绑定主体、资源、操作、授权代次、有效期、libraryEpoch；撤权后新操作失败。持久目录授权由宿主保留，网页重开后重新签发 session handle；不能把旧 UI短期令牌永久存在 KV作为恢复凭据。

默认 `preview` 与 `original` 分别授权；元数据、GPS、文件名等也属于可外发数据。图片编辑需要真实像素时可以请求原图 bytes，宿主按需要提供分块/产物引用，不把主库连接或任意路径交给插件。大视频优先宿主可撤销资源流，支持 range而非把全部 bytes塞进 JSON。

Canvas 使用未获 CORS授权的跨域图片可能无法导出。SDK可返回授权 bytes再构造插件自身 Blob，避免把远程图片直接绘制导致 tainted canvas。[Canvas CORS](https://developer.mozilla.org/en-US/docs/Web/HTML/How_to/CORS_enabled_image)

### 7.3 联网与 AI 连接

推荐所有业务外网经过 `network/ai` broker。插件申请准确服务域名/操作；后端持有密钥并执行请求。浏览器网络通过宿主 CSP阻断旁路。Tauri HTTP scope仅约束它的 API，不等于浏览器域名限制；Rust请求不受 CORS，但仍必须有授权。[Tauri HTTP](https://v2.tauri.app/reference/javascript/http/)

broker对目标、重定向、解析地址、协议、method、响应大小、超时、并发和计费预算检查。云服务返回下载链接的 CDN 也需批准，不能“第一跳许可”等于任意后续域名。官方 HTTP API 的 `scopeRedirects` 要明确启用或使用更严格策略；不接受插件的无约束 proxy、关闭证书验证等危险设置。

允许用户配置的本地 AI连接，但它是特定服务授权；不能借任意URL代理访问本机管理接口、内网或凭据服务。已经上传的素材与已经接受的生成请求不可因关闭页面追回；取消必须区分本地停止与 provider 实际支持的远端取消。

### 7.4 初始预算提案

建议 PoC从可调预算开始：RPC JSON 256 KiB、binary chunk 4 MiB、事件缓冲 256条、普通插件默认并发宿主任务2个、KV 10 MiB、工作区申请示例1 GiB。**这些是试验起点，不是性能基线/最终产品承诺。** 真实大图、大工程、批量任务和磁盘不足验证后再冻结。预算取宿主、用户和插件请求交集；不能由 manifest自行抬高。

不要把上述预算写成 HTML页的硬内存上限：网关消息和磁盘可限制，WebView整体资源不等价。达到配额须明确报错，不截断工程假装保存成功。

## 8. 生命周期：UI、插件启用和任务是三个状态

### 8.1 安装/运行状态

```text
安装：absent → staged → verified → installed_disabled → enabled
                    ↘ rejected
升级：enabled → update_pending → draining → switched → enabled
                                         ↘ rolled_back
UI：closed → starting → handshake → ready → hidden ↔ ready → closing → closed
                                  ↘ crashed / revoked
```

状态持久化归宿主插件管理；插件 JS不能把自己标为 verified或 granted。启用只代表可激活，不代表立即创建页面、联网或常驻后台。菜单/工具 lazily注册；同时打开实例数量有宿主上限。

### 8.2 SDK生命周期事件提案

| 事件 | 责任 | 可依赖的保证 |
|---|---|---|
| `connected` | 恢复项目/连接任务列表 | 已完成身份/版本握手；只有摘要所列授权 |
| `visibilityChanged` | 降低画布动画、刷新节奏 | 可见状态通知，不代表页面仍会无限常驻 |
| `contextChanged` | 更新选中项和 library上下文 | 旧集合不被隐式替换为新授权集合 |
| `grantChanged` | 更新功能可用性 | 宿主已经变更授权；插件配合不是执行守卫前提 |
| `taskUpdated` | 展示真实结果 | 定向、有序号，可从 lastSequence重新取快照 |
| `prepareClose` | 保存草稿/返回未保存摘要 | 尽力通知、有短期限；不能作为唯一保存机会 |
| `deactivate` | 清理自有订阅/worker/临时对象 | 宿主无须等待恶意插件永不结束的 Promise |
| `hostShutdown` | 提交已有检查点 | best effort，异常退出可能收不到 |

草稿周期/操作后持久化，关闭时仅作最后检查。插件取消关闭的请求是提示，最终是否关闭由可信宿主决定；重复崩溃可禁止自动激活并进入安全模式。

## 9. 页面关闭后继续执行的明确契约

### 9.1 首版稳定路线：宿主持久任务

网页提交已验证操作参数和产物引用，宿主创建 `operationId/jobId`，把输入、固定资源集合、安装摘要、授权、执行版本、结果和检查点纳入统一任务账本。UI订阅只是观察者。已有 import/tagging/export服务复用自身任务状态，不另建“插件说已完成”的第二状态机。

**前提是 job授权持续有效。** 关闭 UI撤销 UI session，已明确批准的宿主 job可以继续；每个后续副作用、上传、落库仍检查 grantEpoch、libraryEpoch与授权条件。不能写成“后台任务创建后永久免检查”。事件不能返回给已经关闭或新版本不同主体的旧网页。

例：生图页面提交生成请求 → 宿主调用已授权连接 → 用户离开插件 → 任务中心展示进度 → 获取生成产物 → 验证并在批准的导入范围内入库 → 用户重开页面查询 job结果。过程不依赖页面 Promise仍活着。

画布内自定义 JS计算若尚未提交可恢复宿主 job，页面销毁即终止；隐藏页面是否继续不作 durable保证。预先说清“关闭将停止未保存处理”。用户可以选择保留页或将特定任务迁到支持的后台计算层。

### 9.2 任意 headless JS：有价值，但不凭接口名字承诺

后续可增加 `background.entry` 与事件化 handler。只有运行时对异步、模块加载、取消、超时、资源 limits、身份和存储完成 PoC后，才作为 SDK稳定能力。不得首版声明该字段有效却用永远隐藏 WebView勉强实现。

如果采用 QuickJS，需要自行构建事件循环和异步host bridge；如果采用 Deno，需要固定依赖、禁交互提权，明确 subprocess/FFI和静态模块加载边界。首个后台 JS接口宜是可恢复短任务/事件，输入输出通过同一个 SDK，状态外置；不是 unrestricted daemon。[QuickJS](https://bellard.org/quickjs/quickjs.html)、[Deno Security](https://docs.deno.com/runtime/fundamentals/security/)

### 9.3 可选 WASM compute

可定义固定 export和输入/输出 schema，用宿主分块调度；不同 UI与 Agent调用同一 compute contribution。计算产物经宿主验证再用于业务。Extism有 Rust SDK与 JS/TS PDK，但 JS PDK不是完整 Node/浏览器；不用它限制普通 HTML作者。[Extism SDK](https://github.com/extism/extism/blob/main/runtime/README.md)、[JS PDK](https://github.com/extism/js-pdk/blob/main/README.md)

燃料、超时、页数和输入/输出限制每次任务设置；host函数独立超时。Wasm fuel/epoch不取消阻塞host calls；实例 vars不等于重启后持久工程存储。[Wasmtime Config](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html)、[Extism Memory](https://docs.rs/extism-manifest/latest/extism_manifest/struct.MemoryOptions.html)

### 9.4 应用关闭、重启和脱离应用运行

UI关闭 ≠ 应用退出。首版应用退出时停止接新任务，提示仍运行任务，保存可恢复检查点，撤销 UI资源，协作取消owned engine并在有限等待后收尾。任意插件必须停止，不能用 `beforeunload` 阻止退出。

重启后只恢复验证过的任务记录：可本地幂等重试的步骤才自动恢复；外部生成是否已接受、结果是否已取回不确定时进入 `outcome_unknown`，并标记需要核实的原因，禁止盲重发导致二次收费。主状态统一引用 [06](06-permissions-and-data-safety.md)，不另增插件专属恢复状态。令牌重签、版本/库上下文/权限复核后继续；旧进程Promise不会恢复。

应用真正退出后继续执行需要单独后台服务/系统任务产品，涉及安装、退出定义、服务权限与更新。首版不承诺；用户自己运行的外部模型服务可以继续，但茶馆的取消/退出只能控制属于本应用的任务和进程，不能杀用户外部服务。

## 10. 可信高权限 connector：支持大能力，明确全信任

### 10.1 可用能力与安装模式

适用于本地 GPU生成引擎、Python图片处理、Node完整npm依赖或专用可执行程序。提供两种连接：经验证包由茶馆启动的 `AppOwned`，以及用户指定已运行服务的 `External`。普通网页可调用批准connector，不能自己任意启动 shell或加载 native library。

建议描述文件明确 target triple、程序摘要、启动协议、参数schema、数据工作区、协议版本、允许调用方及健康探测。不让 Agent生成一条 arbitrary shell字符串。正式包固定二进制和依赖；开发包明确“不验证来源”，禁自动激活。

安装信任页展示作者/来源/签名、二进制摘要、支持平台、更新策略、full-trust含义。首次native启动需要单独确认；可信 publisher和网页插件已启用不自动等于native全信任。

### 10.2 必须写给用户的真实边界

> 此扩展包含本地程序。未采用经验证的操作系统沙箱时，它可按当前账户权限访问文件、联网或启动其他程序。茶馆可限制它通过茶馆接口的操作，但无法仅靠插件 manifest阻止它直接使用系统能力。

这是架构事实，不是夸大风险。单独进程降低主应用被拖垮的直接概率，便于健康/所有权管理；**不自动减少该程序的OS访问权限**。独立workspace可供其工作，但不是强制cwd牢笼。主库禁止直写为兼容性和支持契约；对全信任同账户程序不能承诺技术上无法找到它。

网页拿到某connector操作权限，也不应拿到程序路径、raw stdin任意命令、系统环境全部内容和真实API Key。高权限引擎的返回结果仍经过宿主 schema/大小/产物校验，授权服务不替换素材事实源。

### 10.3 运行、退出和撤权

宿主通过 [平台统一进程机制](../../docs/PLATFORM.md) 启动AppOwned程序，清理非必需环境，记录真实进程句柄与启动摘要。使用受控本地管道/socket/stdio RPC、独立token和请求ID；stdout协议与stderr日志分开，敏感原文脱敏。固定程序和参数schema避免shell串拼接。

禁用后拒绝新broker请求、撤销token、停止该connector任务，协作终止AppOwned程序；若不响应，按平台可证明的进程所有权回收。Windows进程树/JobObject、Unix子进程组等方案须分别验证，不凭PID杀可能被复用的用户进程。若发现离开所有权范围的后代或无法保证回收，报告“未证明完全停止”。

撤权不是追回数据：程序已经读过的文件、已经发出的网络数据、自己复制的凭据或脱离监管的子进程不会因token失效消失。External服务不因茶馆插件禁用或退出被杀。未经单独设计，不给“卸载后一定无残留”的硬承诺。

Agent默认不能自主启动或新增全信任native扩展。用户在可信界面批准某贡献点可由Agent调用后，仍按操作计划检查资源/预算；不能让网页工具“只读”自声明为低风险。风险等级由实际后端能力决定，详见 [06](06-permissions-and-data-safety.md)。

## 11. 产物入库与业务一致性

插件可生成、裁剪、调色、批量处理，保存产物不必禁止。统一链路是产物上传/句柄 → 临时文件 → 验证大小、真实格式和尺寸 → imaging → importer/去重/metadata → 主库和FTS → 返回实际Asset ID。

默认新素材，关联来源与可选plugin/operation provenance；覆盖原图另有批准与恢复方案。SVG/HTML等主动内容不作为普通图片直接注入可信主页面；支持策略由媒体服务决定。源图处理中的metadata/GPS是否保留要明确，不因转码默默外发元数据。

任务结果区分成功/重复/失败/取消，输出局部成功和产物列表。客户端重发 `requestId` 不应造成重复任务；跨会话恢复依赖 `operationId`和业务幂等标识，不能声称“exactly once任意外部副作用”。最终正式标签仍遵循当前PRD审核规则；插件生成建议不是直接修改FTS。

## 12. 安装、更新、停用、撤权和卸载

### 12.1 安装验证

解包在暂存区，限制压缩/展开大小、文件数和资源类型；拒绝绝对路径、`..`、符号链接/重解析越界、Windows ADS/保留名、大小写冲突及危险覆盖。验证签名、摘要、publisher身份、manifest schema、SDK/platform兼容后原子安装，不执行npm安装/生命周期脚本。

签名验证来源和完整性，不证明无恶意。密钥轮换、丢失、撤销与ID占用策略在分发前冻结。开发目录加载使用单独开关、专用库和明显来源；修改代码后实例重建与授权差异重审，不复用旧包摘要审批。

### 12.2 更新与 SDK兼容

版本分别管理：manifestVersion、插件SemVer、host SDK、RPC protocol、compute ABI、connector protocol、storageSchemaVersion。页面握手只选共同稳定API，缺能力返回unsupported，不靠小版本号猜支持。Proposed APIs只在开发模式；生产作者不能依赖未冻结接口。

更新先下载验包，再比较权限/域名/native/Agent工具和数据schema；扩大权限重新批准，不能先运行新版本后再询问。初期手动更新，保留验证过的上一版。进入draining停止新调用，UI保存后关闭，已有job固定旧执行版本；draining失败显示等待或让用户取消任务，不静默热替换。

旧版本job引用的包在完成/明确取消前不可被GC；新包不自动取得旧job结果。存储升级先备份或复制暂存，验证成功才切换；若不可向下兼容，不把代码回退声称能恢复数据。插件自己的迁移无主库权限。

Tauri应用Updater签的是应用产物，不自动签用户插件包；插件安装器需独立验包机制。[Tauri Updater](https://v2.tauri.app/plugin/updater/)

### 12.3 用户动作与实际效果

| 动作 | UI/工具/资源 | 宿主job | 工程数据/素材 | 真实局限 |
|---|---|---|---|---|
| 关闭页面 | 撤UI session，释放预览/订阅/对象 | 仅仍有有效独立授权的job可继续 | 保留持久草稿与正式素材 | 未保存自定义页面计算可能停止 |
| 禁用插件 | 阻止新实例/工具，增加授权代次，关闭实例 | 停止其后续副作用，取消可取消部分；必要补偿保留 | 不删工程或素材 | 已提交事务/已外发数据不自动回滚 |
| 撤销目录或服务授权 | 使相关handle/操作失效 | 停止后续未授权步骤，撤销pending审批，按06及实际效果结算状态 | 保留合法已有结果 | 已经取得的bytes不能从插件内存“收回” |
| 卸载代码 | 先停用，移除代码/贡献点；旧job需有明确结算策略 | 固定结果保留；不得留下等待不存在代码的无限任务 | 默认保留可导出工程，单独选择清除 | 不清正式素材、不擅删授权目录内容 |
| 清除插件数据 | 独立破坏性确认后清私有namespace | 先检查运行/引用冲突 | 只清插件自有数据，不级联删除正式Asset | 用户共享目录不是插件私有namespace |

通用撤权与事务完成的先后规则引用 [06](06-permissions-and-data-safety.md)。撤权后必要的安全清理/补偿由宿主定义，只允许完成既有安全状态，不能借“补偿”开启新上传、新批量写入或扩大scope。

## 13. 作者体验、贡献点与 Agent互操作

建议提供三个作者模板：最小静态HTML、TS/React画布、host-job生图。模板包含SDK mock、专用测试库、权限摘要、package validator和一条离线构建命令。UI不绑定作者框架；用户安装预构建包，不安装Node或Rust。

首批贡献点：插件主页、素材右键/批量操作、任务入口、可选Agent工具。插件从 `selection.snapshot` 拿固定集合；“对选中项操作”不能在确认后改成当前全库。Agent工具必须有可验证输入/输出schema、真实执行宿主和风险级别；用户能单独允许人工使用与Agent调用。

任意带UI工具不是无头工具。纯UI插件可由Agent打开并填批准参数，但不承诺Agent直接操作任意DOM。可无头执行的贡献点应有明确host-job/compute/connector执行路径，返回结构化结果和产物；Skill只描述怎样调用，不增权限、不加载素材目录里的任意技能为可信指令。

完整第三方网站与本地网页包分开。远程页面代码随时变化，不能继承本地包签名信任；iframe还可能被网站frame-ancestors/X-Frame-Options拒绝。默认无本地素材SDK；合作网页经特定origin和适配器授权。MCP互操作可映射工具/产物，不能绕过插件主体与审批。[CSP frame-ancestors](https://www.w3.org/TR/CSP3/#directive-frame-ancestors)

## 14. 实施切分与完成定义

| 步骤 | 交付物 | 必须证明 |
|---|---|---|
| P0 安全与容器PoC | 未接业务的HTML/Canvas插件容器、ACL审计、资源网关原型 | 主commands拒绝、主asset旁路拒绝、真实调用身份；三平台布局差异记录 |
| P1 网页插件基础 | 安装/启停/签名或开发来源、manifest、握手、SDK workspace/预览 | 不串插件存储，更新/撤权生效，坏包拒绝，离开页工程可恢复 |
| P2 生图与处理闭环 | 两个真实样例插件、网络broker、host jobs、产物入库 | 页面关闭后有效job继续；凭据不出前端，取消/重试/部分失败真实 |
| P3 生态契约 | SDK类型、打包CLI/模板、贡献点、兼容和升级策略 | 旧插件兼容、权限扩大重新批准、存储迁移失败可恢复 |
| P4 可选高级运行层 | 已通过验证的compute或可信connector适配 | 运行时实际支持范围、限额、所有权回收和信任提示完整 |

P0不是可以省略的“优化阶段”。首次引入第三方代码前必须解决应用命令ACL和全局asset资源泄漏路径；P2的核心样例能反证SDK是否实用。市场服务与任意headless JS不阻碍基础网页生态，但不能用暂未实现的高级运行时掩盖核心需求缺口。

## 15. 插件专属验收矩阵

以下为设计验收任务，不是已通过测试。通用发布门禁由 [09](09-validation-and-risk.md) 维护。

| 场景 | 可观察的通过结果 |
|---|---|
| 画布插件运行在主窗口 | 三目标分别验证尺寸/DPI/输入法/焦点/快捷键/模态；降级明确 |
| 尝试调用delete/reset/restore/settings/keyring/shell/fs | 后端拒绝，主库/原文件不变化；不是仅隐藏按钮 |
| 主页面先打开素材，再插件加载其asset URL | 插件仍不能读取未批准原图；global scope不能变成授权 |
| 伪造principal/pluginId/session或跨实例handle | 身份/代次检查拒绝，响应不含额外路径信息 |
| 目录句柄经过link/junction/目录替换越界 | 每次实际访问仍在批准资源范围内，失败关闭 |
| CSP旁路外发（img/font/fetch/ws/form/iframe/popup） | 未授权目的地无请求；若平台无法证明则gate失败 |
| 代理重定向/CDN/localhost/private IP | 每跳范围有效；合法配置的本地服务与任意内网分开 |
| 正常生图、关闭UI、重开查询 | 同一job继续或准确暂停，授权仍有效；不重复生成/收费 |
| 生图超时/崩溃后结果状态不明 | 进入reconciliation，不自动宣称cancelled或盲重发 |
| Canvas编辑大图、坏文件、输出伪装 | 验证成功后才入库；错误不写FTS、不覆盖原图 |
| 禁用/撤权与下一步落库或上传并发 | 无后续未授权副作用；已完成副作用和补偿明确记录 |
| 升级时旧job尚运行/存储迁移失败 | 固定版本或明确drain；保留结果/工程和可解释回退 |
| 卸载代码与清除数据分别操作 | 不删除原图/正式素材/用户授权目录，工程能导出 |
| native connector失联、退出、派生进程 | AppOwned所有权回收有证据；External不被杀；局限显式 |
| 事件洪泛/超限/画布死循环 | 网关限速、有限缓存；可禁用关闭的证据，不能拿单独WebView冒充硬资源沙箱 |
| 应用退出/重启/库恢复 | 会话与旧libraryEpoch失效；恢复任务重新复核，没有后台偷偷继续旧库写入 |

## 16. 待冻结决策与不能宣称的能力

1. custom protocol的origin与真实资源caller定位；若不可可靠实现，选隔离资源服务或降低受支持范围，不能删除授权检查。
2. 三平台child WebView覆盖和主UI模态策略；未通过前只称PoC目标。
3. 发布SDK 1.0的最小方法和权限词汇；manifest/SDK例子本轮均是提案。
4. 工作区初始限额、工程备份/导出及key轮换；实测后冻结。
5. 页面关闭前草稿落盘、任务持久状态与reconciliation适配；不承诺全云服务支持远端取消。
6. 任意headless JS是否进入2.5后续增量；只有实际runtime验证通过才开放，不暗中依赖永远隐藏页面。
7. 可信native扩展的分发、许可证、签名/公证与所有权方案；不宣称普通manifest约束其直接系统调用。

最终产品行为确定后，按仓库文档职责同步PRD、ARCHITECTURE、PLATFORM和正式契约；本提案不静默替代现行规则。研究资料与差异结论见 [04](04-plugin-industry-research.md)。
