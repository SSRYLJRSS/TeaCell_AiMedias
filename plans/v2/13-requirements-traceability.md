# 已确认要求与实施任务的覆盖索引

本文件用于查缺补漏与验收定位，**不是第二份 PRD，也不是完成状态表**。R01–R47 是用户需求与其必要业务解释的定位；R48–R61 是实现这些需求的工程约束索引。完整行为以“来源”列所指文件为准；承接至权威文档后更新链接，不复制维护两套定义。当前各项均是待实施要求，表中有关联卡不代表已实现。

## 使用与版本解释

- 派工从 [11](11-execution-guide.md) 和 [任务索引](execution/README.md) 开始；完成证据按 [14](14-review-and-handoff.md) 提交。
- A 卡是 2.0；B 卡是 2.5。R05 的插件实现只在 B，R09 分成2.0说明页和2.5真实管理/固定入口；R57 包含2.0既有媒体回归与2.5可选专业媒体能力，不能把可选项当2.0阻塞。
- 使用对话中**最后确认的选择**：入库入口最终保留且可隐藏，独立入库页与助手右侧并存；百宝箱2.0就显示说明页；只管理总仓内素材，外部导出只复制；清空回收站是单独受控的永久操作；个人常用流程在2.0实现。
- 界面草图、参考产品或较早报告不能覆盖这些最终选择。仍待工程验证的接口/路径在 [12](12-implementation-contracts.md) 明示，经 A0/A1 固定后实施。

## 覆盖表

| ID | 要求定位（简述） | 来源 | 对应任务 | 最小验收场景 |
|---|---|---|---|---|
| R01 | 2.0 Agent、2.5 插件分期 | [08](08-migration-and-delivery.md) | [A0-01](execution/A0.md#a0-01)、[A6-04](execution/A6.md#a6-04)、[B0-01](execution/B.md#b0-01)、[B4-02](execution/B.md#b4-02) | 2.0安装包没有真实插件运行能力；2.5才启用插件链路。 |
| R02 | 完整 Pi SDK 与受控宿主 | [03](03-agent-and-mcp.md) | [A0-02](execution/A0.md#a0-02)、[A3-01](execution/A3.md#a3-01) | 真实Pi会话/工具运行成功；没有任意shell或用户扩展自动加载。 |
| R03 | GUI、Pi、MCP 共用 Rust 领域能力 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-01](execution/A1.md#a1-01)、[A3-01](execution/A3.md#a3-01)、[A3-03](execution/A3.md#a3-03)、[A5-01](execution/A5.md#a5-01) | 三种入口对同一非法写入返回相同业务拒绝。 |
| R04 | 2.0 对外 stdio MCP | [03](03-agent-and-mcp.md) | [A5-01](execution/A5.md#a5-01)、[A5-02](execution/A5.md#a5-02)、[A5-03](execution/A5.md#a5-03) | 外部客户端查询和准备计划，茶馆确认后执行并查真实结果。 |
| R05 | 2.5 网页插件生态及受控宿主服务 | [05](05-plugin-platform.md) | [B0-01](execution/B.md#b0-01)、[B0-02](execution/B.md#b0-02)、[B1-01](execution/B.md#b1-01)、[B1-02](execution/B.md#b1-02)、[B2-01](execution/B.md#b2-01)、[B2-02](execution/B.md#b2-02)、[B3-01](execution/B.md#b3-01)、[B3-02](execution/B.md#b3-02)、[B4-01](execution/B.md#b4-01)、[B4-02](execution/B.md#b4-02) | 第三方包安装、授权、联网/工作区、产物保存和维护形成闭环。 |
| R06 | 沿用统一视觉体系和完整助手界面 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-05](execution/A3.md#a3-05)、[A6-01](execution/A6.md#a6-01) | 现有素材功能与新助手的主题、布局和交互一致。 |
| R07 | 默认五入口与中央 Logo | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-05](execution/A3.md#a3-05) | 入库、素材库、Logo、打标、百宝箱均可达；Logo始终居中。 |
| R08 | 2.0 可调整内置底栏入口 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-05](execution/A3.md#a3-05) | 隐藏/恢复/排序内置功能可持久化，不运行任意路由脚本。 |
| R09 | 百宝箱占位与后续插件快捷方式 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A3-05](execution/A3.md#a3-05)、[B1-02](execution/B.md#b1-02) | 2.0显示尚未开放；2.5固定入口后仍能在百宝箱管理。 |
| R10 | 恢复最近对话并可新建 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-06](execution/A1.md#a1-06)、[A3-04](execution/A3.md#a3-04) | 启动回最近对话，新会话内容为空。 |
| R11 | 统一历史、按最近使用、搜索改名 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-06](execution/A1.md#a1-06)、[A3-04](execution/A3.md#a3-04) | 悬浮窗与完整页使用同一会话列表和名称。 |
| R12 | 对话归档和删除 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-06](execution/A1.md#a1-06)、[A3-04](execution/A3.md#a3-04) | 归档可找回；删除前明确影响，不能误删素材或撤销已执行任务。 |
| R13 | 右侧默认素材预览、列表/单图切换 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-07](execution/A3.md#a3-07) | 查看结果不离开聊天，切换单图与列表不丢选择上下文。 |
| R14 | 工作台可调宽、窄窗收起历史 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-07](execution/A3.md#a3-07) | 导入工作台变宽时聊天仍可见，用户能手动调整。 |
| R15 | 左上工作状态轮播与旁边的设置 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-05](execution/A3.md#a3-05) | 缓慢轮播、悬停暂停、手动滚动有效，进度来自真实任务。 |
| R16 | 仅应用内的可开关悬浮 Logo | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-09](execution/A3.md#a3-09) | 跨茶馆页面可用、拖动吸边，切到其他应用不出现桌面悬浮窗。 |
| R17 | 小聊天简洁，复杂批准回完整助手 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-09](execution/A3.md#a3-09) | 打开最近对话/新建；查看并确认转到对应完整计划。 |
| R18 | 独立入库页与助手双入口 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-06](execution/A1.md#a1-06)、[A2-05](execution/A2.md#a2-05)、[A3-07](execution/A3.md#a3-07) | 两个入口复用同一业务任务，不重复导入。 |
| R19 | 拖入先作附件，显式导入 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A2-01](execution/A2.md#a2-01)、[A2-05](execution/A2.md#a2-05)、[A3-07](execution/A3.md#a3-07) | 拖文件不自动入库或上传；附件旁导入进入预检。 |
| R20 | 没有配置 AI 也能手动导入 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A2-02](execution/A2.md#a2-02)、[A2-05](execution/A2.md#a2-05)、[A3-06](execution/A3.md#a3-06) | 纯手动选择、设置、确认可完成导入，不被模型配置阻塞。 |
| R21 | 自然语言指定文件/目录导入 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A2-01](execution/A2.md#a2-01)、[A2-02](execution/A2.md#a2-02)、[A3-03](execution/A3.md#a3-03) | 授权范围内识别输入，缺权限先请求，完成后准确定位成功项。 |
| R22 | 导入预检、分库、改名与失败说明 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A2-01](execution/A2.md#a2-01)、[A2-02](execution/A2.md#a2-02)、[A2-05](execution/A2.md#a2-05)、[A3-07](execution/A3.md#a3-07) | 右侧或独立页均能修改设置，未知/不支持文件解释原因。 |
| R23 | 显式交给助手 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-06](execution/A1.md#a1-06)、[A2-05](execution/A2.md#a2-05)、[A3-08](execution/A3.md#a3-08) | 当前清单/设置随交接进入指定会话，不自动执行或重建在途任务。 |
| R24 | 各会话附件、计划、工作台保留 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-06](execution/A1.md#a1-06)、[A2-05](execution/A2.md#a2-05)、[A3-04](execution/A3.md#a3-04)、[A3-08](execution/A3.md#a3-08) | 切回旧会话状态仍在，新会话不会继承旧待执行清单。 |
| R25 | 长写任务默认排队 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-05](execution/A1.md#a1-05) | 新任务显示等待且可取消；聊天和只读浏览继续可用。 |
| R26 | 完整计划一次批准，变更重新审查 | [06](06-permissions-and-data-safety.md) | [A1-04](execution/A1.md#a1-04)、[A2-01](execution/A2.md#a2-01)、[A3-03](execution/A3.md#a3-03)、[A4-08](execution/A4.md#a4-08)、[A5-02](execution/A5.md#a5-02) | 批准绑定确切范围；模型/文本伪造批准无效。 |
| R27 | 标签建议在右侧审核并可跳打标页 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A2-04](execution/A2.md#a2-04)、[A3-07](execution/A3.md#a3-07) | 切图、编辑、批量确认写正式标签；模型不能替用户确认。 |
| R28 | 实时任务状态与重要异常提示 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-05](execution/A1.md#a1-05)、[A3-04](execution/A3.md#a3-04)、[A3-05](execution/A3.md#a3-05) | 任务更新同一状态卡；关键异常清晰呈现，未知总量不编百分比。 |
| R29 | 停止回答与取消业务任务分开 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-05](execution/A1.md#a1-05)、[A2-02](execution/A2.md#a2-02)、[A3-04](execution/A3.md#a3-04)、[A3-09](execution/A3.md#a3-09) | 停止模型输出不自动取消已批准任务；取消不假装回滚已成功项。 |
| R30 | 重开先恢复摘要，手动继续剩余项 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-05](execution/A1.md#a1-05)、[A2-02](execution/A2.md#a2-02)、[A3-04](execution/A3.md#a3-04) | 重启无自动重放，核实成功收据后才处理剩余部分。 |
| R31 | 在素材库查看本次结果并可退出 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A3-08](execution/A3.md#a3-08) | 结果视图对应真实集合，可回到正常浏览，不变成全局永久筛选。 |
| R32 | 首次可跳过的分步引导 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-06](execution/A0.md#a0-06)、[A3-06](execution/A3.md#a3-06) | 介绍素材库/AI配置/主要操作后进入助手，跳过不妨碍手动功能。 |
| R33 | 设置服务，聊天旁选服务和模型 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-03](execution/A0.md#a0-03)、[A3-02](execution/A3.md#a3-02)、[A3-06](execution/A3.md#a3-06) | 只切换已配置可用服务，聊天显示当前选择。 |
| R34 | 服务失败保留工作、用户选择重试或切换 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A3-02](execution/A3.md#a3-02)、[A3-06](execution/A3.md#a3-06) | 不会暗中换供应商，已批准任务的服务变更需重新核实范围。 |
| R35 | 默认压缩预览，高清另提示 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A2-04](execution/A2.md#a2-04)、[A3-02](execution/A3.md#a3-02)、[A3-06](execution/A3.md#a3-06) | 外发图尺寸/服务符合显示范围，附件拖入不自动外发。 |
| R36 | 目录默认本次授权，可记住并撤销 | [06](06-permissions-and-data-safety.md) | [A1-03](execution/A1.md#a1-03)、[A3-06](execution/A3.md#a3-06) | 记住读取范围不等于任意写入/上传许可；撤销后旧句柄不可扩权。 |
| R37 | 只追踪当前总仓内托管素材 | [06](06-permissions-and-data-safety.md) | [A0-05](execution/A0.md#a0-05)、[A1-02](execution/A1.md#a1-02)、[A1-03](execution/A1.md#a1-03)、[A4-05](execution/A4.md#a4-05) | 外部附件只是任务输入；正式资产记录和所有入口遵循同一管理边界。 |
| R38 | 移动仅总仓内，库外导出只复制 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A4-01](execution/A4.md#a4-01)、[A4-02](execution/A4.md#a4-02) | 旧UI/API也不能库外移动并继续追踪；导出保留原素材。 |
| R39 | 旧库外记录复制回仓或解除关联 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-02](execution/A1.md#a1-02)、[A2-03](execution/A2.md#a2-03)、[A4-06](execution/A4.md#a4-06) | 用户确认后处理，外部原文件不删，回仓保留身份及元数据。 |
| R40 | 总仓离线和归属不明时保护数据 | [06](06-permissions-and-data-safety.md) | [A0-05](execution/A0.md#a0-05)、[A1-02](execution/A1.md#a1-02)、[A2-03](execution/A2.md#a2-03)、[A4-05](execution/A4.md#a4-05)、[A4-06](execution/A4.md#a4-06) | 失联不触发逐项清空记录/自动清理，提供恢复定位入口。 |
| R41 | 两种总仓迁移/重新定位向导 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A0-05](execution/A0.md#a0-05)、[A4-05](execution/A4.md#a4-05)、[A4-06](execution/A4.md#a4-06) | 区分软件搬迁与已搬好重定位，校验后切换，失败可恢复。 |
| R42 | 助手允许移入素材回收站 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A4-03](execution/A4.md#a4-03) | 常规删除计划仅软删，保留恢复和单独手动永久删除边界。 |
| R43 | 清空回收站是独立受限能力 | [06](06-permissions-and-data-safety.md) | [A1-04](execution/A1.md#a1-04)、[A4-04](execution/A4.md#a4-04)、[A4-08](execution/A4.md#a4-08) | 普通计划批准不能授权任意永久删除，清空必须独立审核。 |
| R44 | 永久清空这 N 项，绑定固定清单 | [PRD §8](../../docs/PRD.md#8-20-agent-与25插件的已批准目标待实现) | [A1-04](execution/A1.md#a1-04)、[A4-04](execution/A4.md#a4-04) | 确认后的新增项、恢复项、归属变化项不被旧批准误处理。 |
| R45 | 默认30天清理，仅限明确托管文件 | [06](06-permissions-and-data-safety.md) | [A4-03](execution/A4.md#a4-03)、[A4-04](execution/A4.md#a4-04) | 界面显示预计时间；到期检查归属、健康及真实删除结果。 |
| R46 | 命名、编辑、保存个人常用流程 | [03](03-agent-and-mcp.md) | [A0-07](execution/A0.md#a0-07)、[A4-07](execution/A4.md#a4-07)、[A4-08](execution/A4.md#a4-08) | 保存可复用参数/步骤，使用受控能力而非任意脚本。 |
| R47 | 个人流程每次新输入、新授权、新版本引用 | [03](03-agent-and-mcp.md) | [A0-07](execution/A0.md#a0-07)、[A4-07](execution/A4.md#a4-07)、[A4-08](execution/A4.md#a4-08) | 再次调用不能继承旧批准，编辑不更改在途实例。 |
| R48 | 无原始SQL/shell/密钥/伪造批准能力 | [06](06-permissions-and-data-safety.md) | [A1-01](execution/A1.md#a1-01)、[A1-03](execution/A1.md#a1-03)、[A3-03](execution/A3.md#a3-03)、[A4-04](execution/A4.md#a4-04)、[A5-01](execution/A5.md#a5-01)、[A5-02](execution/A5.md#a5-02)、[B0-02](execution/B.md#b0-02)、[B2-01](execution/B.md#b2-01) | 各入口负向测试拒绝越权，模型不能成为授权事实源。 |
| R49 | 不把外部内容中的指令当权限 | [06](06-permissions-and-data-safety.md) | [A0-04](execution/A0.md#a0-04)、[A1-03](execution/A1.md#a1-03)、[A3-03](execution/A3.md#a3-03)、[A4-08](execution/A4.md#a4-08) | 恶意文件名/标签/技能描述/模型响应不能扩大工具作用范围。 |
| R50 | 所有模型请求受传输网关与预算约束 | [03](03-agent-and-mcp.md) | [A0-03](execution/A0.md#a0-03)、[A3-02](execution/A3.md#a3-02)、[A6-03](execution/A6.md#a6-03)、[B3-02](execution/B.md#b3-02) | 含总结/压缩/重试的请求均受连接、限流、外发和取消规则控制。 |
| R51 | 幂等、真实收据、取消竞争和结果未知 | [06](06-permissions-and-data-safety.md) | [A0-04](execution/A0.md#a0-04)、[A1-04](execution/A1.md#a1-04)、[A1-05](execution/A1.md#a1-05)、[A2-02](execution/A2.md#a2-02)、[A2-04](execution/A2.md#a2-04)、[A4-01](execution/A4.md#a4-01)、[A4-02](execution/A4.md#a4-02)、[A4-03](execution/A4.md#a4-03)、[A4-04](execution/A4.md#a4-04)、[A4-05](execution/A4.md#a4-05)、[A5-03](execution/A5.md#a5-03)、[A6-02](execution/A6.md#a6-02)、[B2-02](execution/B.md#b2-02) | 丢响应/重试/重启不重复文件效果或收费提交，未知结果先对账。 |
| R52 | 追加迁移、单一领域状态所有者 | [12](12-implementation-contracts.md) | [A0-04](execution/A0.md#a0-04)、[A1-02](execution/A1.md#a1-02)、[A1-05](execution/A1.md#a1-05)、[A2-03](execution/A2.md#a2-03)、[A2-04](execution/A2.md#a2-04)、[A6-02](execution/A6.md#a6-02)、[B2-02](execution/B.md#b2-02) | 升级幂等，SQL和IO分层，UI/Operation不成为第二份子任务真相。 |
| R53 | 平台支持与明确降级 | [09](09-validation-and-risk.md) | [A5-03](execution/A5.md#a5-03)、[A6-03](execution/A6.md#a6-03)、[A6-04](execution/A6.md#a6-04)、[B0-02](execution/B.md#b0-02)、[B3-03](execution/B.md#b3-03)、[B4-02](execution/B.md#b4-02) | 证据按平台登记，未验证不宣称通过；支持平台失败阻止相应交付。 |
| R54 | 受控运行时与干净机器打包 | [03](03-agent-and-mcp.md) | [A0-02](execution/A0.md#a0-02)、[A3-01](execution/A3.md#a3-01)、[A6-03](execution/A6.md#a6-03) | 无需用户配置系统Node，启动/退出/升级不遗留错误进程。 |
| R55 | 备份恢复与旧授权失效 | [06](06-permissions-and-data-safety.md) | [A4-05](execution/A4.md#a4-05)、[A6-02](execution/A6.md#a6-02) | 恢复/迁仓更换代次，旧票据不能重新执行写入。 |
| R56 | 只保留有当前消费者的插件接点 | [08](08-migration-and-delivery.md) | [A3-05](execution/A3.md#a3-05)、[B0-01](execution/B.md#b0-01)、[B4-01](execution/B.md#b4-01) | 2.0无空插件表、任意动态注册器或提前公开SDK。 |
| R57 | 既有媒体功能保留，专业RAW独立验证 | [07](07-media-workflows.md) | [A6-02](execution/A6.md#a6-02)（2.0数据/既有媒体回归）、[A6-03](execution/A6.md#a6-03)（2.0媒体与打包回归）、[B0-01](execution/B.md#b0-01)（2.5范围复核）、[B3-03](execution/B.md#b3-03)（2.5可选专业RAW） | HEIC/RAW预览/视频等回归；专业显影未验证不宣传支持。 |
| R58 | 统一API、搜索契约和精确选择集合 | [12](12-implementation-contracts.md) | [A1-01](execution/A1.md#a1-01)、[A3-03](execution/A3.md#a3-03)、[A3-08](execution/A3.md#a3-08)、[A5-02](execution/A5.md#a5-02) | 前端仅src/api，沿用搜索/分面协议；分页全选不误当当前页。 |
| R59 | 真实测试证据与独立复核 | [14](14-review-and-handoff.md) | [A0-01](execution/A0.md#a0-01)、[A0-04](execution/A0.md#a0-04)、[A6-01](execution/A6.md#a6-01)、[A6-03](execution/A6.md#a6-03)、[A6-04](execution/A6.md#a6-04)、[B4-02](execution/B.md#b4-02) | 每卡区分通过/失败/未运行，必要检查失败不能解锁依赖。 |
| R60 | 文档权威与唯一真实进度 | [11](11-execution-guide.md) | [A0-01](execution/A0.md#a0-01)、[A6-01](execution/A6.md#a6-01)、[A6-04](execution/A6.md#a6-04)、[B4-02](execution/B.md#b4-02) | 待实现规格与已交付事实分开；进度只在PROJECT_PLAN维护。 |
| R61 | Agent 会话权限档位只收窄能力、不扩大审批：模型在各档位可用的工具集是该档能力集的稳定子集，档位由 Rust 在 dispatch 前独立判定，Host/前端回传档位字段不被采信；批准、恢复、永久清空与打标开始在任何档位均 GUI 专属 | [12](12-implementation-contracts.md) | [A3-10](execution/A3.md#a3-10)、[A3-11](execution/A3.md#a3-11) | Read 4 / Standard 12 / Full 16 三档裁剪只是体验门；越档调用由 `capabilities::execute` 独立能力门拒绝。 |

## 如何补充实际证据

每张卡的报告引用本表 ID，并列出真实用例/命令/环境/结果。A6-01 汇总2.0适用条目的证据，B4-02再汇总2.5。报告和 PROJECT_PLAN 指向测试与验收记录，本文件只维护关系，不累计每天进度或放“已完成”勾选。

若一条要求找不到真实测试或可重复人工场景，先补验证或说明阻塞；不能只凭文件/按钮/类型已经创建就判定功能完整。若改动触及多条规则，其负向与故障场景同样需要覆盖。

