# A0-04：操作恢复原型

这是临时文件、独立 SQLite 数据库中的可丢弃原型，未接生产业务或正式迁移。实际进度只记录在 [PROJECT_PLAN](../../docs/PROJECT_PLAN.md)。

SQL 与短事务在 `src/db.rs`；读取、摘要、复制、发布在 `src/lib.rs`，不持 SQLite 事务。批准只由可信 adapter 构造的 Caller 接受；模型的 `approved` 不创建批准。批准消费、操作和步骤创建同一事务；提交键与计划冲突明确失败。本原型使用固定测试主体、固定库身份、单一导入能力，生产多主体/世代/授权撤销由 A1 冻结并实现。

每项有固定 operation/step 身份。部分复制恢复重建暂存；完整暂存校验后先持久登记 staged，再以同卷 hard-link 不覆盖发布，保留暂存链接用于证明已发布文件身份。恢复不以相同哈希证明文件归属。登记 asset 和 registered 同一短事务；本地模拟子批次有 operation 唯一关联。源文件始终保留。

测试 subprocess 在 accepted、partial-copy、published、registered、batch-created 与 external-replied 六处直接 exit(77)，父测试重新打开数据库、验证固定提交键及文件和批次结果。外部调用仅以本地请求计数替身验证：响应未落盘时保持 outcome_unknown，不重新提交。exit 无栈清理，模拟进程突然结束；它不等同真实断电、存储故障或强制 OS 终止。恢复必须显式调用；cancel 持久保留，审阅后 continue 才处理剩余项。

从本目录逐条运行并分别记录退出码：

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

初始 E01–E08 集与故意错误验证见 [eval 说明](../../tests/agent-evals/README.md)。

## A0-05：总仓身份与搬迁实验

`managed_library.rs` 与 `library_db.rs` 为同一独立crate中的第二个实验，使用另一份临时library.sqlite，与操作恢复表及正式主库分开。它比较绝对路径与相对定位：既有绝对路径记录保持待核验；带可信测试导入收据的素材采用库ID+相对位置，实际根路径与epoch单独保存。搬仓只切换定位，不重建资产，所以相同哈希的两个素材仍保留各自ID、标签和评分。单库唯一索引拒绝第二个库注册。

测试分类为Verified、ExternalPending、Unknown、Offline、WrongLibrary。Verified只代表受信任fixture中的收据、根标记、相对路径与内容检查通过，不是完成生产文件所有权证明。根失联时保留全体记录；错误库标记与归属不明记录不被清理。Windows所有reparse-point入口拒绝，junction逃逸有真实fixture测试；Unix分支尚未在目标系统运行。

复制迁仓顺序：源清单验证 → 目标非重叠且空目录预检 → SQLite持久维护记录 → 独立暂存/校验/排他发布 → 全量目标校验 → 短事务切换根与epoch → 保留源端残留。暂存身份通过保留hard-link witness核验，部分复制恢复不凭哈希相同认领陌生文件。测试在第一项完成、部分复制、发布后、切换前、切换后直接exit(77)，重新打开库验证恢复；切换后的重试不重复提升epoch。

已手动搬好时只验证并重新定位，不重新导入。目标缺素材或标记错误则保持原定位；原型不处理真实盘符修改，但临时目录改名后定位恢复有实测。未追踪文件保留在源端并不加入复制清单。

源端清理实验：复制前在源端`.migration-source`创建并保留托管素材的hard-link identity witness；该目录也成为保留路径，不能作为素材位置。切换成功后`prepare_source_cleanup`固化源/目标/epoch与精确记录，目标须同时匹配复制暂存/witness身份与摘要，源须匹配原witness身份。可信fixture的单独审核入口批准，执行才消费审核状态；不把迁仓复制批准视为清理批准。清理只unlink清单内文件与对应witness，不递归删除旧目录，不删除根标记或未知文件，不修改素材ID/标签/评分。

逐项intent→源unlink→removed收据→witness unlink→complete均有持久状态。测试子进程在前四个边界exit(77)，重新打开本身不继续清理，显式调用后从固定清单核实并完成余项。removed收据后witness与源都已不存在可完成收据；若源重新出现或身份不符则保留并拒绝。同哈希源/目标替换、目标离线、审核后清单变化与无批准均有负测，不以摘要相同认领陌生文件。审核和执行期间全部文件/哈希IO在数据库事务之外。

连续维护实验：清理批准/执行均显式绑定预期库epoch，旧世代请求不能批准或执行新清单。清理完成后，或可信fixture明确选择`retain_source_fixture`保留副本后，才能调用`archive_finished_fixture`。归档在同一短事务保存不可变历史（源/目标/epoch/精确记录/逐项收据/complete或retained结果），再撤下当前维护记录，供下一次维护使用；归档写入或删除失败全部回滚。copying、prepared、running不能被下一次搬仓自动覆盖。历史只读fixture不执行文件操作，不把retained记录显示为已删除。两次连续复制/清理、保留副本后再次复制、旧epoch拒绝及触发器事务回滚有实际DB/文件测试。

这仍不等同完整迁仓向导交付：审核/保留/归档入口是可信fixture方法，不是生产授权票据或GUI；部分清理后失败保留收据与残留，正式取消/撤权/坏尾/清理summary仍待集成。路径检查与unlink之间仍有敌对并发交换窗口，未实现OS句柄相对删除，不声明同账户沙箱。复制暂存hard-link witness仍保留，连续迁仓后的旧`.migration-stage`也未回收，因此不把素材路径unlink宣称为释放了全部磁盘空间；已保留备份的后续受控处置/暂存GC需另验证。磁盘空间不足、跨卷、维护门阻止正式任务、备份恢复、历史仓外记录复制回仓/解除关联与GUI仍未验证。根标记不是秘密或认证凭据；trusted_receipt仅由测试host赋值，正式接口不能接收模型自声明。仅在独立临时fixture执行本实验，不能直接用于真实总仓。

定向命令：`cargo test --test managed_library`。实际结果唯一记录在PROJECT_PLAN，不在本文维护通过列表。

### 旧库外记录解除关联实验

`prepare_external_unlink`只为健康总仓下已分类ExternalPending的固定记录准备计划。独立external_unlinks表保存原ID/路径/标签/评分等完整记录及库ID/根/epoch，可信fixture按planId和预期epoch单独审核。`submit_external_unlink`不调用文件删除；短IMMEDIATE事务再次比对精确记录、当前库定位与维护冲突，消费批准、完成收据和删除活动管理关系同批提交。失败则批准/活动记录均保留；相同计划重复读取同一完成收据。并发先按维护锁互斥，忙返回MAINTENANCE_BUSY而非假成功；显式重试后查询原完成收据。相同哈希的仓内另一记录不被合并或删除。

新[定向测试](tests/legacy_unlink.rs)观察真实SQLite及临时库外原文件，覆盖无批准、审核后记录改变、DELETE触发器回滚、总仓失联/换epoch、两连接并发；子进程在提交前/提交后exit(77)，重新打开后显式提交可核实完成，不重删其他记录。原记录保存在计划收据，未将解除关联描述为素材物理删除。根和路径分类IO在事务外，SQL受理时再次校验库世代/精确记录。

该入口仍是可信维护fixture，不向Pi/MCP注册，也没有生产GUI或授权主体。正式实现需独立维护门、关系/关联表的一致处置、保留/恢复说明、库实例与备份世代、状态过期及完整旧库迁移；只验证AssetRecord字段，不能宣称已处理生产全部关系。未知/失联项保持待检查。定向命令：`cargo test --test legacy_unlink`。

### 旧库外记录复制回仓实验

[legacy_return](src/legacy_return.rs)为另一条可信维护fixture链路，prepare固定原记录、库定位/epoch、目标相对位置及源文件句柄身份，单独审核后复制。Windows身份来自volume/file index/creation time，Unix候选为dev/inode/ctime；只读源文件，不在库外创建witness或删除/改写原文件。复制暂存与hard-link witness都在当前总仓`.legacy-stage`，完整校验后排他发布，保留暂存身份用于重启核验。WindowsFoundation依赖仅在Cargo target表增加功能。

短事务把原AssetRecord改为相同ID/标签/评分的受控相对定位，同时完成回仓收据，不走普通导入按hash去重。同hash的另一条仓内素材保持独立记录；同名目标哪怕hash相同也拒绝，等待另外选择，不自动覆盖或合并。DB登记失败保留原库外记录和已发布暂存收据，显式重试后登记原ID。部分复制、发布后未登记、登记后未回复三处真实子进程exit(77)均可恢复；路径逃逸/保留目录、无批准、源身份或内容变化、陌生同hash暂存、失联根、旧epoch都有文件/DB负测。

这是固定字段和受信任协作进程的实验，不是生产完整迁移：Windows当前有实测，Unix代码尚未在目标机验证；真实跨卷/空间不足/断电/原生权限/源文件并发改写/敌对路径交换未验证。执行仍需要可打开且身份匹配的源，源丢失时保留待处理，不自动认领已有目标。下节的锁仅在同一规范化fixture DB路径上协调维护，不替代生产单宿主/资源注册/维护队列或完整撤权取消。暂存GC、生产关联表、备份世代、GUI冲突预览及实际imaging兼容仍待A1/A2/A4；样本字节只证明复制，不冒充图片解码验收。定向命令：`cargo test --test legacy_return`。

### 维护进程互斥实验

[maintenance_guard](src/maintenance_guard.rs)使用固定Rust标准库File.try_lock，锁文件按规范化fixture DB路径命名。guard覆盖复制迁仓/重新定位、源清理、旧记录回仓/解除关联及可信审核/保留/归档；业务查询不取维护锁。它在短SQLite事务之外取得并持有，文件IO期间不持数据库事务。等待用户审核时guard已释放；竞争返回MAINTENANCE_BUSY，不自动排队、取消或假报完成。

lock文件不存批准、不根据PID猜测存活、不在释放时unlink；正常drop、exit(77)与子进程强杀由OS释放锁。Windows显式不共享删除权限，协作进程可打开但不能换掉活锁文件。测试观察同DB互斥、不同DB独立、持锁时查询可用、正常退出/强杀后重新取得原lock，以及忙碌回仓没有创建暂存或消费批准。已有中断恢复测试也通过此guard进入真实文件链路。

它不是全应用资源锁或恶意同账户程序隔离：DB硬链接别名/另一个DB指向相同素材根、网络盘、锁路径交换、生产配置/导入/打标/导出等旧入口仍需单宿主与共享能力层治理；不取guard的外部程序不受本实验约束。Windows有实际进程证据，Mac/Linux仅有源码，未宣称相应平台通过。定向命令：`cargo test --test maintenance_guard`。

### 真实跨卷fixture

[cross_volume](tests/cross_volume.rs)为Windows显式启用的专项测试。源在系统temp，目标由TEACELL_CROSS_VOLUME_FIXTURE指定，但只接受仓库artifacts/agent-cross-volume的规范化路径；其中再创建自身TempDir，不使用用户素材、改挂载或填满磁盘。通过GetFileInformationByHandle卷序列号不等断言，确保确实是两个文件系统卷，不能把不同目录冒充跨卷。测试结束自动清理自身TempDir，保留可复用的空父目录。

普通测试运行时此专项明确ignored，不能把ignored当通过。本机显式专项已经有迁仓/回仓正常及五个exit77边界的观察；准确结果见PROJECT_PLAN。它只证明当前Windows机器这对卷、受信任临时目录和字节复制，不能外推到其他文件系统/网络盘/断电/空间不足/权限故障或其他平台。源码前面的未验证列表指这些更广场景；基本跨卷路径已有本节限定证据。

从仓库根运行以下准备，再分别检查命令退出码；artifact路径必须解析到当前仓库，未配置/同卷/目录不符会失败而非跳过：

```powershell
New-Item -ItemType Directory -Path artifacts/agent-cross-volume -Force | Out-Null
$env:TEACELL_CROSS_VOLUME_FIXTURE = (Resolve-Path artifacts/agent-cross-volume).Path
cargo test --manifest-path prototypes/operation-recovery/Cargo.toml --locked --test cross_volume -- --ignored
```

## 原型限制与后续责任

A0-07 的 `workflow.rs`/`workflow_db.rs` 使用独立流程数据库，验证结构化个人流程的不可变版本、参数替换和每次实例化的新计划。模型只可加载和准备，不能保存版本、授予目录权限或批准执行。文件摘要在 SQL 之外计算；创建计划的单条 SQL 再次校验流程仍启用且版本摘要匹配。停用不删除历史计划。重复输入、失联目录、撤销授权、库世代变化、未知字段和不兼容能力版本拒绝创建计划。

流程格式是实验候选，只含导入、标签建议和独立清空确认屏障；不宣称完整生产步骤目录。`workflow_fixture` 的 admin/agent 参数用于可信测试宿主区分入口，不是恶意同账户程序隔离。目录授权是 fixture 对象，尚未接生产授权存储；准备后的素材变化、执行前重新验证、长期撤权竞态和实际任务执行仍由 A1/A4 完成。清空步骤只产生独立确认屏障，不执行删除。真实 Pi 调用说明见 [Host 原型](../agent-host/README.md)。

- 测试假图仅验证字节复制，不证明 imaging/解码/真实导入兼容。
- 工作目录为独立受信任 fixture；Windows当前两卷的迁仓/回仓及进程退出恢复已有独立opt-in专项验证，准确结果见项目计划。敌对目录、链接交换、网络盘、其他卷组合及掉电持久性尚未证明。生产路径和总仓身份属于 A0-05/A1。
- 普通摘要防止计划输入发生变化；已登记目标当前只验证内容，完整文件身份/修订和撤权执行前检查仍待正式契约。
- batch 是本地唯一关联替身，没有真实 AI 请求、pending 标签或正式标签写入；域 batch 的生产集成属于 A2-04。
- E08 仅为政策替身，测试不执行永久清空；正式固定回收站清单和确认属于 A4-04。
- 外部真实服务对账、长期队列、完整状态机、取消竞态、批准有效期/配置修订还需补验证。不能据当前原型关闭 A0-04，或放行 A1/A3。
