# 目录授权与资源句柄 v1

状态：A1-03 已实现当前 Windows 工程链路和定向负测；三端敌对 TOCTOU/平台句柄矩阵仍由后续平台验收覆盖。本文定义 2.0 宿主内部授权与资源引用语义，不是模型可自行扩权的协议。

## 1. 唯一事实源

- Rust 实现：`src-tauri/src/services/authorization.rs`。
- SQL/持久状态：`src-tauri/src/db/authorization.rs`。
- GUI 可信选择器与命令：`src-tauri/src/commands/authorization_cmd.rs`。
- 前端调用封装：`src/api/authorization.ts`。
- `CallerContext` 的不可反序列化边界仍由 [agent-v1.md](agent-v1.md) 和 `contracts/caller.rs` 定义。

`ResourceRef` 只是 16–128 字节的不透明令牌。解析成功不证明主体、目录、资产或权限；每次使用都必须回到宿主查询句柄并重新核对主体、实例、会话、用途、访问类型、授权世代、库世代和当前文件系统身份。

## 2. 主体与用途

主体只由已验证 Adapter 构造，不从工具 JSON 取字段：

| Adapter | principal | session | instance |
|---|---|---|---|
| GUI | 固定宿主 GUI 主体 | 本次 WebView 会话 | 本次应用运行实例 |
| Pi | 宿主启动并握手得到的主体 | 指定对话会话 | Pi Host 连接实例 |
| MCP | 配对/认证后的连接主体 | 无（连接实例承担会话隔离） | 已认证连接实例 |

2.0 首批目录用途：

| purpose | 授权访问 | 用途 |
|---|---|---|
| `import_source` | read | 只读取用户选择目录中的导入来源 |
| `export_destination` | write + export | 只向用户选择目录复制导出，并明确允许外发写入判断 |

用途不是显示文案。读取目录不等于写入或外发；外发路径必须单独检查 `export` 位。

## 3. 授权生命周期

1. 生产入口只允许 GUI 通过 Rust 宿主中的原生目录选择器创建目录授权。前端命令不接收任意目录路径，Pi/MCP 也不能调用该入口。
2. 选择目录后，宿主规范化真实路径、拒绝以链接/重解析点作为根，并记录可供后续复核的目录身份。
3. `this task` 授权绑定当前 GUI session/instance，并设置有限有效期；`remembered` 授权可跨会话列出并重新签发句柄，但旧句柄仍绑定原 session/instance。
4. 每个 principal 有单调替换的 `grantEpoch`。撤销任意目录授权或撤销全部授权时，在同一短事务内标记授权与句柄失效并提升世代；旧句柄、旧审批和后续可停止步骤必须重新授权。
5. 授权和句柄同时绑定创建时的 `libraryId` 与 `libraryEpoch`。恢复、换库或重新定位产生新世代后，旧句柄返回 `LIBRARY_CHANGED`，不得跨世代复用。

记住目录只表示可以减少下一次选择动作，不自动创建计划、不批准写入、不允许网络外发，也不扩大为所选目录之外的路径。

第 1 条是**不可绕过的**：本契约不提供委派入口，也不允许把 GUI 签发的句柄直接交给 Pi/MCP。模型侧要使用导入来源目录，只能由用户在本会话中把该目录句柄显式交给它，并且每次使用都由宿主重新核验主体、会话、授权世代与库世代——句柄的 `principalId` 与调用者不一致时必须返回 `UNAUTHORIZED`，不得为此新增例外分支或放宽比较。裸路径字符串不构成授权，模型也不能通过选择「两个都已授权目录中的一个」来推断用户意图。该交接的签发方、跨会话边界与协议形状尚未冻结，冻结前 Pi/MCP 工具表不发布 `imports.*`（结论与理由见[Agent v1 §导入入口的设计结论](agent-v1.md#导入入口的设计结论)）。

## 4. 句柄类型与访问

| kind | 描述 | 约束 |
|---|---|---|
| `directory` | 授权目录根或已记住授权的本次实例句柄 | 只能按 purpose 对应的访问位使用 |
| `file` | 目录页内已核实身份的具体文件 | 必须能追溯到同一 grant，且只允许读取 |
| `asset` | 当前总仓内已核验托管素材 | 必须仍满足 `VERIFIED_ASSET_SCOPE`，不能是库外/未知记录 |

句柄持久字段至少包含主体、instance、session（如有）、grant、用途、访问掩码、有效期、grantEpoch、libraryId、libraryEpoch。文件句柄还保存相对路径、文件身份、大小和观察时间。

无句柄、错误主体、错误 session/instance、伪造或过期句柄分别失败关闭；错误主体返回 `UNAUTHORIZED`，撤销返回 `GRANT_REVOKED`，过期/被替换资源返回 `REFERENCE_EXPIRED`。

## 5. 路径和打开边界

目录分页最多返回 100 项，单页最多扫描 10,000 个条目，递归深度最多 64。实现必须：

1. 只接受绝对目录路径，先检查根目录本身不是符号链接或 Windows reparse point，再生成规范化真实路径和目录身份。
2. 对相对路径只接受普通组件，拒绝绝对路径、`..`、设备命名空间、NUL 和超长相对路径。
3. 对每一级组件检查链接/reparse point，分页遍历不允许跟随链接或通过链接目录逃逸。
4. 打开文件后再次核验打开句柄的真实文件身份、大小、观察时间、规范化位置仍在授权根下；被替换则使旧句柄失效。
5. 长读取只在有界块之间调用重新核验；撤权或换库后下一次可停止步骤失败，已经写入的收据保留。

当前实现使用 `canonicalize`、链接/reparse 检查、打开句柄身份和打开后复核，不声明等同于操作系统沙箱。相同账户的敌对进程、检查到使用窗口以及 O_NOFOLLOW/平台专属句柄策略仍需按 [PLATFORM.md](../PLATFORM.md) 的后续平台矩阵验证，不能仅用字符串前缀或前端检查替代。

## 6. GUI 命令

| 命令 | 行为 |
|---|---|
| `pick_directory_grant` | Rust 原生选择器；创建本次或记住授权 |
| `list_directory_grants` | 列出当前 principal 的 remembered 授权及其可用状态；目录离线时展示不可用 |
| `reissue_directory_grant` | 为当前 session/instance 重新签发 remembered 授权句柄 |
| `revoke_directory_grant` | 撤销单个授权并提升 principal 世代 |
| `revoke_all_directory_grants` | 撤销该 principal 的本机目录授权并提升世代 |
| `page_authorized_directory` | 对已授权目录按 cursor/limit 返回有界文件引用 |

命令层只做参数、短锁和调用编排；路径发现、真实身份核验和访问判断在服务层。授权错误不把本地路径、文件内容、凭据或原始 IO cause 写入诊断。

## 7. 验收负测

A1-03 定向覆盖：

- GUI 可创建授权；Pi/MCP 不能创建目录授权。
- 错误 principal、跨 session/instance、伪造和过期句柄均拒绝。
- 撤销和撤权全部会提升世代，旧句柄不再可用。
- 恢复/换库世代后旧句柄回到重新授权。
- 目录根被替换、文件被替换、链接/父目录逃逸和目录外文件读取均失败关闭。
- 读取、写入和外发的访问位分别判断。
- 托管资产句柄仍要求当前核验范围，文件身份变化后拒绝继续读取。

该独立测试证明当前实现边界，不替代三端真机、敌对同账户或最终发布验收。
