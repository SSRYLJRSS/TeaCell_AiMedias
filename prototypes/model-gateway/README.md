# 模型网关传输实验（A0-03）

问题：Pi 的真实 provider 编码/流解析能否保留，同时把实际 HTTP 请求交给 Rust，
并让辅助请求、取消和限流使用同一条路径？这是隔离实验，不是生产网关。
当前状态及实际验证结果只见 [PROJECT_PLAN](../../docs/PROJECT_PLAN.md#9-20-agent开发状态)。

## 运行

从仓库根逐条运行并核实退出码：

```powershell
cargo build --manifest-path prototypes/model-gateway/Cargo.toml --locked
cargo fmt --manifest-path prototypes/model-gateway/Cargo.toml --check
cargo clippy --manifest-path prototypes/model-gateway/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path prototypes/model-gateway/Cargo.toml
node --test prototypes/agent-host/test/gateway.test.mjs
```

Pi 依赖安装方式见 [agent-host](../agent-host/README.md)。本 crate 的 Cargo.lock
以主项目现有锁文件为起点收敛依赖，避免独立解析意外换入已撤回版本。
全套 Host 测试另按 agent-host README 设置 Windows 原生 owner 后运行，不能把跳过当通过。

## 实际调用链

`Pi SDK → 已注册 provider 包装 → Pi 原始 OpenAI/Anthropic 编码器 → 自定义 fetch`
`→ 私有 stdin/stdout 帧 → Rust → loopback mock 上游 → 按需响应分块 → Pi 流解析`。

Node 使用公开占位值满足 SDK 配置态认证；Rust 只注入编译在本实验中的公开测试值。
没有读取用户凭据、总仓、真实图片或真实模型。上游端口由测试启动者指定，Rust 只允许
loopback；Node 提供的外部 URL 必须匹配固定允许路由，不能改变上游目标。
Anthropic SDK 在本基线使用 `/v1/messages?beta=true`，仅接受这个明确变体。
请求的模型与连接 revision 校验失败时不出站；不跟随重定向，也不转发 Node 的鉴权头。

Rust直接编译复用正式项目 `ai_rate_limit.rs`；最小错误适配只用于独立 crate，
没有修改应用限流器。每次出站前取得该连接的 permit，流结束、取消或错误后释放。
Rust显式设置reqwest::retry::never，关闭默认协议级重试；Pi 的显式有限重试仍逐次计入实验预算。仅禁用Pi provider的重试不足以表达Rust客户端政策。

响应每收到一次 pull 最多向Node交付8KiB，帧64KiB、响应1MiB、并发待处理8项、
每进程最多16次出站。所有数值都是实验上限，不是已经批准的生产产品预算。
尚未消费响应时不持续向 Node 推送字节；不使用全响应拼接来冒充流式背压。
异步HTTP读取可能返回大于8KiB的传输块，Rust在总响应限检查后保留该块，后续切片仍逐次等待pull。不能据此称传输层每次只分配8KiB；未拼接完整响应，超限块不交付给Node。空传输块不会消耗客户端pull后造成悬挂。

`governance.rs` 新增可信 fixture 策略实验：测试宿主可在 Rust 启动时提供独立策略文件，限定 revision/enabled；Node 请求帧不能更改该路径或提交批准字段。Rust 在排队前、取得 permit 后、响应头及分块交付前检查；停用、修订变化、文件损坏或未知字段均拒绝。等待中的请求不会因此新增出站 attempt。策略文件只用于可信测试，既不是生产配置存储，也不构成同账户恶意代码隔离。

配置切换回归通过先提升旧 fixture 策略 revision 来拒绝旧 Host，再以新 revision 启动隔离 Host，并确认请求只到新 loopback fixture。revision 在 Rust 启动时固定，Node 侧也保存在私有字段；旧 bridge 即使出现同名公开属性也不能改请求 revision。它只证明启动时替换测试连接的路由/修订边界，不证明生产连接存储、热切换、真实凭据或外网行为。

目前没有批准图片资源句柄，已知协议中的图片/音频/文件/视频块一律拒绝，包括嵌套工具结果。真实 SDK 的 OpenAI 与 Anthropic 图片请求均可用于验证拒绝路径。图片测试使用仓库自带30px图标，不读取用户照片。`fixtureImageCapability` 只让测试编码器产生图片请求，不授予外发权限；它不是生产模型配置。这个检测不证明任意编码在文本中的数据均可识别，生产必须由 Rust 从授权资源构造预览内容，不能信任模型提交的媒体载荷。

原始 provider SDK 会把自定义 fetch 拒绝改写为通用 Connection error；私有传输因此保留最多64条请求 ID/机器错误码，不保存正文或密钥。正式应用仍需按请求归属将原始治理错误映射到用户提示，不能用通用网络提示掩盖需要批准的状态。

## 验证范围与设计结论

测试运行真实 Pi SDK 和原始协议编码器，模型服务是可控 HTTP fixture。
覆盖两种协议的普通回复、工具续轮、人工/自动压缩、分支摘要、截断恢复、有限重试、
401/429、路由/模型拒绝、重定向、背压、取消与等待取消、断流、请求上限、
网关启动失败，以及重新构造 SDK 会话后继续使用受控包装。

自动压缩在切开一个回合时会产生历史摘要与回合前缀摘要两次请求；截断恢复还会再续轮。
因此应按真实出站 attempt 计数，不能按 prompt/compact 次数计数。
当前实验支持继续选择私有 IPC fetch 路线，没有证据要求增加 loopback gateway 生产服务。
Pi 的 provider wrapper 保留官方编码器/流解析，并让普通、工具续轮和辅助请求共用同一受控 fetch；Rust 是唯一上游网络出口，拥有连接校验、正式限流器、取消与流式背压，因此现有真实 SDK + 本机 fixture 测试能直接观察 attempt 和失败路径。备选的 loopback HTTP 服务会额外增加本地监听端口、目标映射和跨平台传输边界，却不能替代 Pi provider 编码，也没有在这些已测语义上提供新的能力；当前没有足以抵消其端口/进程复杂度的证据。该结论仍需完整 A0 复核，不将它当作已冻结的生产协议。

环境定向测试 `gateway-environment.test.mjs` 使用本地记录代理，显式向 Rust fixture 注入 HTTP/HTTPS/ALL_PROXY 变量，核实固定 loopback 上游收到请求而代理收到0请求；注入入口只接受127.0.0.1代理，普通启动仍使用白名单。另一用例在新 Node 进程的启动目录放置公开 `.env`、`.pi/auth.json`、models、AGENTS、Skill 和会产生标记的扩展；父环境含哨兵启动选项/密钥，白名单启动后检查实际环境、响应、上游请求与标记未生成。整个测试不使用或更改系统代理/真实配置。

Windows 实测发现仅从传入对象删除 PATH 不能保证子进程没有继承路径；`childEnvironment` 现在显式设置空 PATH，真实进程在 SDK 导入前后均检查。启动器仍只用绝对可执行路径。这个变化是原型启动修复，不证明所有原生进程都已完成生产治理；`fixtureProxyUrl`/`fixtureWorkingDirectory` 仅用于隔离测试，不是用户可配置的网络或文件授权 API。

## 尚未证明的部分

- 真实服务、真实 Keyring、生产 connection 配置/撤销、批准预览外发和费用账本。可信 fixture 停用/修订检查不替代这些生产职责。
- 真实系统代理、环境代理、TUN 与哨兵 `.env` 的完整矩阵；实验 Rust 明确 no_proxy，不能据此证明生产代理兼容。
- 本实验已改为每个工作线程中的单线程Tokio异步HTTP，20ms轮询取消/fixture治理，等待响应头或传输块时丢弃同一个Future以中断；连接permit仍在线程内调用正式限流器取得。卡住头/体的取消及撤销测试观察到上游关闭和后续请求成功，按操作后1.2秒实验断言检查，未等待3秒HTTP上限。轮询间隔不是已批准的跨平台生产时限；stdio写入、完整父退出、正式授权存储与宿主事件集成仍需验证。已发出的请求不能靠本地取消撤回上游副作用或收费。
- 任意附加请求头/Anthropic beta能力、批准后的二进制图片请求、跨平台及最终资源预算。文本大上下文的有界上传实验见下节，不能外推至任意请求大小或模型上下文容量。
- 新进程持久 transcript 恢复实验见下节；完整宿主重启/索引与业务副作用恢复尚未验证。
- SDK 内置 provider 列表仍存在；生产选择入口只允许映射到批准连接的受控模型。
  任意 provider 切换、OAuth、WebSocket、catalog 网络刷新、cache warming 均不能因此开放。
- 通道可信启动、完整握手/实例归属与 UI 的治理适配仍须复用 A0-02/A1 结果。

该原型不接受真实目标URL或真实密钥配置，不能直接作为用户可运行的通用代理发布。

## 大请求上传实验协议

单帧继续限制64KiB。小请求使用start，大请求使用upload声明UTF-8字节总数，随后用body逐块发送base64编码的最多16KiB原始字节，最后commit；seq从0递增，每块等待Rust的uploaded确认后才能继续。这使Node不会一次向stdin堆积整份上传，Rust通道容量为1；UTF-8只在完整收齐后解码，避免中文或emoji跨块损坏。测试HTTP服务也使用增量UTF-8解码，避免观测夹具自行损坏文本。

实验总量上限为每请求2MiB，最多8个活动请求；该数值是原型限制，尚未批准为生产预算。Rust上传工作线程独立检查取消、fixture治理和8秒截止时间，缺块/乱序/超长/无效UTF-8均拒绝。上传完成后才进入JSON、模型、媒体和路由检查及出站计数；未批准图片不会因为分块而获得权限。上传超时只结束对应工作，不依赖stdin再次收到输入才能释放活动槽。

Node当前仍通过Request.text读取完整输入，Rust也组装有界完整JSON；这不是任意大文件流式上传，不保证调用者构造Request之前的内存上限。正式宿主仍须在输入构造前限制上下文、预览与工具结果，并确认全局内存/截止时间预算。stdout阻塞、跨平台、生产通道实例认证和实际模型容量仍须独立验收。资源测量报告绑定旧二进制摘要，不能当作新增上传代码的测量。

## 持久会话恢复实验

[测试](../agent-host/test/gateway-persistence.test.mjs)通过新的固定Node进程运行[fixture](../agent-host/fixtures/persisted-session.mjs)，在专属临时sessions目录使用真实Pi SessionManager.create/open。先完成工具回合与压缩，再退出并用另一个进程打开：原session ID、记录数、压缩与工具结果保留；打开本身0模型请求、0工具执行。再次明确prompt后只有新的请求，且两种provider仍通过Rust注入fixture鉴权；历史工具不会重放。不把读取旧工具结果当作新的操作收据或批准。

固定SDK的parseSessionEntries会跳过损坏JSON行，因此恢复fixture先执行[只读结构检查](../agent-host/session-preflight.mjs)。完整UTF-8/JSON行、v3头、唯一记录ID和已存在父记录为实验条件；截断、中间损坏、未知版本、重复记录被拒，原文件内容不变。完整但停在助手toolCall的前缀可读，但不执行工具、不发模型请求。该前缀由测试构造，没有宣称完成此位置的进程强杀注入。

结构检查只针对固定版本的受控原型，16MiB文件上限也只是实验预算；不是Pi全部消息schema验证、旧版本迁移或文件认证。检查后SessionManager会再打开文件，仍存在路径/文件交换窗口，不声明敌对同账户文件系统隔离。正式实现需宿主拥有的会话索引、世代/格式校验、实例锁、坏尾隔离与明确恢复摘要；恢复计划/操作只读取Rust真实收据，不从transcript自动提交。fixtureSessionManager只由测试启动者构造，不向模型开放任意会话路径。

## 资源测量入口

Windows 下从根目录使用校验过的 Node 运行 `prototypes/agent-host/measure-gateway.mjs`，先按上述命令构建网关。脚本仅访问 loopback mock；按已知 PID 分别采样 Node/Rust 的 working set、peak working set、private memory、累计 CPU，并确认采样期间真实 SDK 仍在流式响应。结果写入 Git 忽略的 `artifacts/agent-runtime/gateway-measurement.json`。当前schema2固定短文本、340KB Unicode、1.8MB文本三个profile，各10次；30个请求共用一个Node进程，每轮重建Rust网关，不是宿主冷启动。报告绑定Rust二进制、Cargo manifest/lock、JS transport/session/测量脚本摘要；提供idle/streaming/settled原始数据与实际请求字节数。PowerShell采样自身增加负载，prompt时长包含人为等待及结束后的采样，不代表模型延迟；Node CPU还包含fixture服务。其他平台此脚本明确拒绝执行，不能把Windows样本推广为跨平台预算。测量时不要同时运行测试或构建；外部系统活动仍须如实标为不可控。
