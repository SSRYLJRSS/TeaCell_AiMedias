# Pi Host 隔离原型

这是A0-02的可丢弃原型，尚不是生产Agent，也不连接茶馆业务数据库或真实素材。
实际任务状态只记录在 [PROJECT_PLAN](../../docs/PROJECT_PLAN.md#9-20-agent开发状态)。

在本目录执行：

```powershell
npm ci --ignore-scripts --no-audit --no-fund
cargo build --manifest-path ../model-gateway/Cargo.toml --locked
cargo build --manifest-path ../operation-recovery/Cargo.toml --bin workflow_fixture --locked
npm test
```

首轮开发环境是Node24.19.0。`supervisor.mjs`默认使用当前Node的绝对路径并构造环境白名单，
不依赖子进程PATH查找，不读取用户Pi配置；这还不构成“随安装包提供Node”的验证。
环境白名单显式传空 PATH；Windows 新进程实际环境测试证明不继承父搜索路径。只省略该键曾被观察到路径重新出现，不能只凭白名单对象断言启动环境安全。
后续必须使用校验过的独立Node制品及无系统Node环境重测。

运行时清单固定 Windows x64、macOS Apple Silicon 和 Linux x64 的 Node24.19.0来源/摘要，原始校验表及其摘要也纳入资源。`prepare-runtime.ps1 -Target <目标> -DownloadOnly` 可跨系统下载并验摘要，不解包、不执行；省略此开关则要求匹配本机OS/架构，再按ZIP或tar.gz解包并验证版本。Windows默认目标保持不变；macOS用aarch64-apple-darwin，Ubuntu目标用x86_64-unknown-linux-gnu。其他目标明确拒绝。命令及清单不提升平台支持等级，Mac/Linux原生运行、权限保留和进程寿命仍须在对应主机验收。

Windows原生进程树实验（从本目录执行，所有制品写入Git忽略的目录）：

```powershell
$taskPocNode = pwsh ./prepare-runtime.ps1
cargo build --manifest-path ./native-owner/Cargo.toml
$env:TEACELL_NATIVE_OWNER = (Resolve-Path './native-owner/target/debug/teacell-agent-owner-poc.exe').Path
& $taskPocNode --test test/*.test.mjs
```

每条命令单独核实退出码。未设置TEACELL_NATIVE_OWNER时，原生Job用例明确跳过，不能
据此声称Windows进程树已验证。原生PoC是Windows专用，其他平台明确返回不支持。
`bootstrap.mjs`只加载Node内置模块；收到Rust在Job归属建立后发出的启动信号才导入真实SDK。
Rust转发后续控制输入，已可运行真实Pi宿主的握手/聊天/退出与强杀恢复，不只适用于孙进程fixture。
生产Tauri集成仍需复用并重验这条屏障。Job不是文件/网络/恶意代码沙箱。

`sdk-session.mjs`使用真实Pi SDK1.0.4和本地模拟provider，通过明确ResourceLoader禁止
项目/用户上下文、Skill、扩展与MCP发现，启用工具集合为空。
模拟服务的公开固定值不是生产凭据；生产模型网关在A0-03验证。
`gateway-session.mjs`/`gateway-transport.mjs`与 [模型网关实验](../model-gateway/README.md)
验证真实provider通过Rust转发到HTTP fixture。新增gateway测试需要先构建该独立crate；
这是准备步骤，不将缺少可执行文件改成跳过。普通Host生命周期与模型传输仍是分开的实验入口。
全部同组包通过精确依赖/overrides及锁文件固定，测试防止顶层锁版但子包漂移。

请求正文在 `request-body.mjs` 中逐块读取，累计上限2MiB、读取截止8秒；读取阶段和已经提交的请求共用8个名额。超限、超时、取消或网关关闭时取消producer，在正文完整且UTF-8合法之前不提交Rust请求。累积字节使用单一增长缓冲，不为每块保存对象；每128次读取让出事件循环并用单调时钟检查截止。中断Promise只属于当前一次读取，不能复用长期pending Promise积累reaction。request-body测试在独立开启GC的进程中读取20万空块/1字节块，检查最后一次读取尚未返回时的live heap和定时器执行，而非只看函数结束后内存。该上限约束传输自身积累的字节，不限制调用者已经生成的JSON、Request构造或producer自身分配；正式上下文和费用预算仍须宿主治理。测量脚本同时记录此模块摘要，旧测量不能证明新实现的资源占用。

本地依赖清单入口为 `node ./inventory-runtime.mjs`，输出忽略目录 `artifacts/agent-runtime/dependency-inventory.json`。脚本对已安装包核对锁文件版本，记录 manifest/原生/WASM/根许可证文件摘要，单列未安装的 optional 包和符号链接；不执行依赖脚本。包 metadata 声明的 license 不等于嵌入素材的许可证，根目录没有许可证文本也不直接证明违规；这些都是后续打包复核需定位的事实。该清单不证明安装包提取、运行加载或发布许可已验收。

附加许可证文本及固定来源见 [许可证清单](licenses/manifest.json)。Pi八个1.0.4发布包的gitHead对应同一提交，保存该提交原始许可证；其他条目按各自发布提交保存。data-uri-to-buffer的文本来自README明确License段落，清单记录摘取方式。standardwebhooks仓库根许可证用于规范，JavaScript库按libraries/LICENSE核实，不能把根许可证直接套到组件。实验overlay生成器检查文本摘要、包版本和Pi许可证存在，并将licenses目录加入资源；这不代表其余依赖或示例素材已全部复核。

Windows资源搬迁验证：根目录先运行 inventory-runtime.mjs，再使用固定Node运行 `prototypes/agent-host/verify-packaged-runtime.mjs`。脚本创建新的忽略目录，复制依赖/脚本/许可证，核实10个二进制资产摘要；stage-runtime.ps1重新校验官方Node ZIP并解包。复制后的probe确认实际exe路径、空PATH、SDK模拟回复、测试PNG处理及Windows TUI addon加载；另用复制后的owner/bootstrap验证Job屏障和退出。结果在 `artifacts/agent-runtime/packaged-runtime-probe.json`。它是资源布局实验，不是安装包提取、只读安装目录、干净机器或生产GUI验收。为了保留诊断证据，目录不会自动删除。新增许可证之后需重新验证对应资源或重构建，旧报告只证明当时列出的文件。

`workflow-bridge.mjs` 为 A0-07 的受控个人流程实验。只注册加载指定版本和准备新计划两个工具；目录与本次素材清单来自可信测试宿主，不接受模型传入旧批准或目录句柄。版本保存只在 Rust fixture 的 admin 入口执行。真实 SDK 调用经过同一模型网关；不开放 read/bash、文件写入、自动 Skill 发现或批准工具。源文件和数据库均为临时 fixture，不连接生产数据。运行工作流测试前必须构建上述 workflow_fixture；缺少可执行文件是失败而非跳过。

`protocol.mjs`验证UTF-8分块、逐帧字节限制、污染与截断；`host.mjs`和`supervisor.mjs`
验证双向控制、实例/版本/序列身份、事件上限、停止推理与正常结束区分、错误退出和目录独占。
目录锁记录进程与实例；强杀后可能遗留锁。只有显式请求恢复、原进程不存在且记录完整时
可回收已知锁；活进程、未知内容或链接锁均拒绝恢复。该机制只针对受信任原型，不是恶意同账户程序隔离。
Windows原型已经实测Job回收子孙进程、真实SDK的Rust屏障以及父进程强杀后的显式恢复；
这些机制尚未接入生产应用。不能把普通管道EOF测试或同账户Node当作操作系统隔离证明。原型没有通用Shell、SQL或任意文件业务工具。

所有测试创建系统临时目录并在结束时清理自身夹具，不扫描或操作用户素材。
安装初步跳过依赖脚本，后续制品检查必须验证实际JS/WASM/native资产可加载及许可材料。

测量入口：用固定Node执行`measure.mjs`，结果写到`artifacts/agent-runtime/measurement.json`等忽略目录。它测SDK握手/退出、Host内存和实际依赖资产；并发负载未控制，当前仅为初步测量。安装包增量和批准预算尚无结果时输出null，不把解包总量冒充安装包增量。

包体实验：先在仓库根以现有`desktop:build`获得同配置基线并保留制品与摘要；在本目录用
固定Node执行`package-experiment.mjs`生成忽略目录下的资源overlay，再从仓库根执行：

```powershell
npm run desktop:build -- --target x86_64-pc-windows-msvc --config artifacts/agent-runtime/bundle-poc.conf.json --bundles nsis
```

overlay保留现有许可资源，增加完整未筛选Pi依赖、固定Node及原型脚本。该实验包仍是1.0.2加实验资源，
没有生产Agent入口；用于测量，禁止作为2.0成品发布。资源加入/包体测量不能代替干净环境安装及包内启动验收。
