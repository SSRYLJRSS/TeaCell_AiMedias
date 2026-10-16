# 初始 Agent 政策评估集

`cases.json` 固定 schemaVersion=1 与 E01–E08 case ID。A0 adapter 是 Rust 政策/恢复原型，假模型输出来自 fixture，不调用网络或真实模型。A1-01 冻结正式 schema；A3-03 接入生产 adapter 后保持 case ID，记录 adapter/version 与生产结果，不能把原型通过当作生产通过。

每条包含适用能力、初始库、选择、权限、用户话语、假模型输出、允许结果、禁止副作用、确认点与 DB/文件断言。当前 fixture 均为临时独立库与文件；expected operations/assets/batches 是实际 SQL 计数，来源字节必须保留。E06 检查取消持久化和已完成文件不重写；E08 只验证独立清空政策，不删除任何文件。

从仓库根运行：

```powershell
cargo test --manifest-path prototypes/operation-recovery/Cargo.toml --test evals -- --nocapture
```

故意错误应使 E03 失败，用于证明断言会检出“模型 approved=true 被信任”的缺陷。环境开关仅存在于测试 runner，不能进入生产 adapter：

```powershell
$env:TEACELL_EVAL_MUTANT_FORGED_APPROVAL = '1'
cargo test --manifest-path prototypes/operation-recovery/Cargo.toml --test evals -- --nocapture
# 预期非零：E03 实际产生操作/文件，而 expected 为零。
Remove-Item Env:TEACELL_EVAL_MUTANT_FORGED_APPROVAL
cargo test --manifest-path prototypes/operation-recovery/Cargo.toml --test evals -- --nocapture
# 恢复正常政策后预期退出0。
```

当前 runner 是单个测试遍历8条 case，报告时须区分“1个 Rust 测试、8条案例”。文案中的禁止副作用是评估要求；当前实现只覆盖上述具体断言，尚不证明完整提示注入防护、模型工具选择质量、权限撤销或 GUI。正式 adapter 应扩充可观察工具/模型请求计数和所有禁用行为断言。
