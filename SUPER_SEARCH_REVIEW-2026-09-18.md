# 超级搜索全链路复盘报告（2026-09-18）

> 范围：超级搜索整条链路——AiSearchBar → superSearchStore → src/api → commands/super_search_cmd.rs → services/super_search_ai.rs → db/search_plan.rs / query_expr.rs / search_query.rs / search.rs → 前端 QueryBuilder / FilterChips / SuperSearchPage。
> 方法：契约先行（docs/contracts/*.md）→ 全链路代码通读 → **用户视角动态模拟测试**（新增 `src-tauri/tests/search_user_sim.rs`，14 条用例）→ 基线回归。
> 结论先行：**后端执行主干是健康的**（编译器、分页、诊断、降级协议都经受了模拟测试），问题集中在 **8 个确定性 bug（4 个 P1）+ 5 个观察项**，全部有可直接照抄的修复方案，无架构级返工。

---

## 一、动态验证结果

### 新增模拟测试：`src-tauri/tests/search_user_sim.rs`（14 条，全部通过）

| 分组 | 用例 | 验证点 |
|---|---|---|
| 关键词路由 | 特殊字符（%/\_）按字面量 | LIKE 转义不吞全库 |
| | 中文 2 字 / 4 字 / 逆序矩阵 | ≤2 字 LIKE、≥4 字 2 字块 AND、短语保相邻 |
| | FTS 敌对输入（引号/OR/AND/-） | 不 panic 不报错（永不红字底线） |
| 参数绑定 | 大小+关键词混合组 | 编号 `?N` 与裸 `?` 混排不错位 |
| 分页稳定 | 1500 条同值排序键逐页拼接 | == 全选 ID 序列，无丢条无重复 |
| | 300 条 Relevance 分页 | total=全量、hasMore 正确、加分项整体前排 |
| 诊断 | 归零叶子识别 | delta>0 && result=0 正确命中 |
| | delta 量化 | self_count / count_without_leaf / delta 三值语义正确 |
| AI 链（纯函数） | 乱码输入 | 三层降级兜关键词、永不 Err |
| | preferred → should | 加分不淘汰、evidence 真子串守卫、编造依据必丢弃 |
| | 包含∩排除撞车 | 以排除为准 + 可执行 |
| 极性 | must_not 白名单 | ExcludeTag / Not 拒绝、正向 Tag 放行 |
| 压力 | 空 filter + 12 条加分 + 800 条 | 软排序全库可执行 |

### 基线回归（全部通过）

- `cargo test --lib`：**410 passed / 0 failed**
- `cargo test --test qa_edge_tests --test w7_facet_tests --test v24_numbers`：全绿

### 审查中主动排除的两个「疑似 bug」（避免误报）

1. **`offset_placeholders` 不偏移裸 `?`**：看似隐患，实则 SQLite 对裸 `?` 按「当前最大编号 +1」运行时分配，子片段按序拼接时编号天然连续——已用「metadata（编号）+ 关键词（裸 ?）」混合用例实证绑定正确。
2. **诊断 delta 是恒等式**：初读时怀疑 solo=filter AND leaf 恒等于总数，复核源码后发现真实实现是 `count_without_leaf`（把叶子从 AST 摘除后重算），语义正确，已用量化用例背书。

---

## 二、问题清单与修复方案

### P1-1 点击「试试相近的词」后结果仍是 0 条【功能性 bug】

- **位置**：`src/stores/superSearchStore.ts` `applyTermSuggestion`（L506-516）
- **现象**：词查零命中后用户点建议词（如「森材」→「森林」），系统是把这个新词 **AND 进** 条件树——但原来那个编译成 `1=0` 的失效词查叶还在，`1=0 AND 新词` 恒为空。用户点了建议，结果还是 0。
- **根因**：建议落点只做了并入，没有做替换；失效叶没有任何路径被清掉。
- **修复**（最小改动，不动协议）：并入建议前，先摘除所有「纯词查」叶（`tagIds 空 && termQuery 非空`——这类叶要么是零命中源，要么是历史残留，保留意义为零）：

```ts
applyTermSuggestion: (term, match) => {
  const t = term.trim();
  if (!t) return;
  const cur = get();
  const node: QueryExpr = {
    op: "leaf",
    cond: { type: "tag", facetKey: "", tagIds: [], mode: "any", includeDescendants: true, termQuery: t, termMatch: match },
  };
  // 摘除失效的词查叶（原叶编译为 1=0，AND 保留它则建议永远无效）
  const base = removeExprLeaves(
    cur.plan?.filter ?? cur.expr ?? undefined,
    (cond) => cond.type === "tag" && cond.tagIds.length === 0 && Boolean(cond.termQuery?.trim()),
  );
  const merged = normalizeExpr(mergeQueryExpr(base, node) as QueryExpr);
  cur.setPlanFilter(merged);
},
```

### P1-2 手动添加的标签条件，chip 显示「标签 #123」【UX bug】

- **位置**：`src/components/supersearch/FilterChips.tsx`（plan 路径 L103-108、排除区 L159-164）+ `src/utils/queryExprUtils.ts` `leafLabel`
- **现象**：AI 解析的标签条件能显示中文名（resolvedTags 回填），但用户在 QueryBuilder 手动选的标签，chip、排除区、加分区一律显示 `标签 #id`——因为名称只查 `resolvedTags`，而手动选标签从不写 resolvedTags（`setPlanFilter` 只动 plan）。
- **修复**（与 QueryBuilder §9.8 同款策略，组件内兜底，不动 store 协议）：

```tsx
// FilterChips 内：tagStore 兜底合并进名称表（plan 路径同样生效）
const flatTags = useTagStore((s) => s.flatTags);
const namePool = useMemo(() => {
  const merged = new Map<number, ResolvedTag>(resolvedTags.map((r) => [r.tagId, r]));
  for (const t of flatTags) if (!merged.has(t.id)) merged.set(t.id, { tagId: t.id, text: t.name, facetKey: t.facet, path: "" });
  return Array.from(merged.values());
}, [resolvedTags, flatTags]);
// 后续 flattenExprForDisplay(expr, namePool) / bonusLabel(sc.cond, tagNameMap from namePool) 全部换用 namePool
```

### P1-3 归零提示与诊断数字没有标签名【UX bug】

- **位置**：`src-tauri/src/db/search_plan.rs` `cond_label`（L1096-1137）
- **现象**：Tag 叶子的诊断 label 是 `"scene: 标签×1"`——用户在「归零条件」列表和诊断数字里看到的是分面 key + 数量，完全不知道是哪条标签。
- **修复**：`cond_label` 需要 `conn`，对 Tag/ExcludeTag 查一次名称（ids 有上限，单条小查询）：

```rust
fn cond_label(conn: &Connection, cond: &LeafCond) -> String {
    match cond {
        LeafCond::Tag { facet_key, tag_ids, term_query, .. } => {
            if let Some(tq) = term_query.as_deref().filter(|s| !s.is_empty()) {
                format!("{facet_key}: {tq}")
            } else {
                let names = tag_names(conn, tag_ids); // SELECT t.id,t.name,f.display_name JOIN tag_facets
                format!("{facet_key}: {}", names.join("、"))
            }
        }
        // ExcludeTag 同理；其余分支不变
        ...
    }
}
// collect_leaves 已有 conn，改调用点即可
```

### P1-4 用途绑定读取失败会拖垮整次 AI 搜索【可靠性 bug】

- **位置**：`src-tauri/src/commands/super_search_cmd.rs` L52-57
- **现象**：日志写着「读取用途绑定失败，**回退默认档案**」，但代码是 `.map_err(...) ?`——错误直接传播，整次 AI 搜索失败。注释/日志与行为矛盾。
- **修复**：

```rust
// §4.4：超级搜索按用途绑定读取连接档案；无绑定时回退默认 active 档案。
// 读取失败同样回退默认档案（warn 记录），不阻断搜索。
if let Err(e) = crate::db::ai_connections::apply_usage_binding(&conn, "super_search", &mut s.ai) {
    tracing::warn!("超级搜索读取用途绑定失败，回退默认档案: {e}");
}
```

### P2-5 未来版本 plan 被静默丢弃，「已重置」提示永不出现【UX bug】

- **位置**：`src/stores/superSearchStore.ts` `merge`（L699-706）与 `migrate`（L712-740）
- **现象**：`onRehydrateStorage` 里那段「保存的搜索条件来自更新版本，已重置」warning 是**死代码**——`merge`/`migrate` 先于它执行，已经把未来版本 plan 置 null，等 onRehydrate 看到时 `p.plan` 已是 null，直接 return。用户降级应用后永远看不到提示，条件悄悄消失。
- **修复**：版本判断只留一处（onRehydrate），merge/migrate 不再调 `migratePlanV3` 丢弃：

```ts
merge: (persisted, current) => {
  const p = persisted as { plan?: SearchPlanV3 | null; query?: Partial<ResolvedSearchQuery> } | null;
  return {
    ...current,
    plan: p?.plan ?? null, // 版本判断交给 onRehydrateStorage（唯一出口，带用户提示）
    query: { ...defaultQuery(), ...(p?.query ?? {}) },
  } as typeof current;
},
// migrate（v1→v2）同理：expr→plan 补全后原样保留 plan，不调 migratePlanV3
```

### P2-6 服务端不校验 should ≤ 12（契约 §14.1）【协议缺口，有实施冲突需注意】

- **位置**：`src-tauri/src/db/search_plan.rs` `validate_search_plan`（L241）
- **现象**：契约写明「should 最多 12 条」，前端 `normalizeSearchPlan` 也钳了 12，但服务端校验没这条——直接调 `list_assets_by_plan` 传 13 条加分照常执行（契约全局规则 3：所有枚举字段都要有服务端校验）。
- **⚠️ 冲突**：**不能单独加校验**。`build_plan_from_v3` 目前不截断 preferred——模型若给出 >12 个 preferred，加了校验会导致 AI 解析整单失败（validate_search_plan 在 build_plan_from_v3 末尾被调用）。必须捆绑修改：

```rust
// ① build_plan_from_v3：生成侧截断（search_plan 调用点不用改，就在 super_search_ai.rs L1048 收集循环后）
if should.len() > crate::db::search_plan::MAX_SHOULD_CLAUSES {
    warnings.push(format!("加分项超过 {} 条，已保留前 {} 条。", crate::db::search_plan::MAX_SHOULD_CLAUSES, crate::db::search_plan::MAX_SHOULD_CLAUSES));
    should.truncate(crate::db::search_plan::MAX_SHOULD_CLAUSES);
}
// ② validate_search_plan 新增（MAX_SHOULD_CLAUSES = 12 常量单点声明）：
if plan.should.len() > MAX_SHOULD_CLAUSES {
    return Err(AppError::msg(format!("加分项超过上限（最多 {MAX_SHOULD_CLAUSES} 条）")));
}
```

### P2-7 诊断对 Not 包裹的叶子摘除无效【边界 bug】

- **位置**：`src-tauri/src/db/search_plan.rs` `remove_leaf`（L1086-1092）
- **现象**：`collect_leaves` 给 Not 内叶子发的是 `[i, 0]` 路径，但 `remove_leaf` 的 Not 分支在 `path.len()==1` 时返回原树不变——该叶子的 `count_without_leaf` 恒等于总数、delta 恒 0，归零检测对它失效。Not 在 plan.filter 里少见（极性契约后只有旧持久化数据可能有），属边界。
- **修复**：path 指到 Not 内的叶子时，摘除整个 Not 子树（不可能只删内层保留 Not）：

```rust
QueryExpr::Not { child: _ } => {
    // path=[i,0] 指到 Not 内的叶子：摘除整个 Not（Not 无内层可独立删除）
    if path.len() == 1 {
        return None;
    }
    ...
}
```

### P2-8 条件移入优先区超 12 条时静默丢弃【UX】

- **位置**：`src/stores/superSearchStore.ts` `moveConditionBetweenZones`（L571）
- **现象**：`.slice(0, 12)` 超出的加分项无声消失，用户无感知。
- **修复**：截断发生时报一条执行 warning：

```ts
const capped = [...should, { cond: node.cond, weight: 1, label: "", evidence: null }];
const dropped = Math.max(0, capped.length - 12);
should = capped.slice(0, 12);
if (dropped > 0) {
  set((s) => ({ executionWarnings: [...s.executionWarnings, { source: "plan", zone: "should", message: `优先区最多 12 条，${dropped} 条未加入。` }] }));
}
```

### P3 观察项（本次不修，防过度工程化）

| # | 事项 | 判断 |
|---|---|---|
| 1 | prune 探针在**每次分页请求**重复执行（compile probes 每页都跑） | 12 叶以内是毫秒级，先不动；若大库滚动变卡，再在 prepare_execution 加 plan-hash 缓存 |
| 2 | diagnose = 2N+1 次 COUNT 持全局 DB 锁 | 推测：万级素材 + 20 叶时可能秒级持锁阻塞缩略图（未真机验证，按记忆纪律标推测）；届时把 diagnose 命令改 async + spawn_blocking 即可，不动语义 |
| 3 | 契约文档 drift：`super-search-ai-v2.md` 仍写 V2，实现已是 V3（preferred/evidence/termMatch/should）；§4「禁止权重三档」与 §14「位置权重三档」表述需理顺 | 改文档，不动代码 |
| 4 | `is_config_error` 含「请求失败/timeout」宽匹配，上游 5xx/429 可能被当配置错误红字 | 推测：需按 ai_cloud 实际错误文案核实后再收紧；当前宁可误报（真报错可见） |
| 5 | AI 概念回填 `term_query` 后，若模型显式输出 Prefix/Contains termMatch，required 条件会被前缀扩展意外放宽（默认 Alias cap=1 无此问题） | 观察：如后续发现 AI 条件偏宽，在 sanitize 侧强制 Alias 即可 |

---

## 三、修复冲突检查与实施顺序

**冲突矩阵**（全部无冲突，唯一需要注意的是 P2-6 的内部依赖）：

| 修复 | 触及面 | 与其他的关系 |
|---|---|---|
| P1-4 | 后端 cmd 单点 | 独立 |
| P2-7 | 后端 db 单函数 | 与 P1-3 同文件不同函数，可同批 |
| P1-3 | 后端 cond_label 签名 +1 调用点 | 仅 diagnose 内部 |
| **P2-6** | 后端 validate + build_plan_from_v3 | **必须捆绑**（先截断后校验，否则 AI 解析被误杀）；加契约测试 |
| P2-5 | 前端 persist 配置 | 独立 |
| P1-1 | 前端 store action | 独立 |
| P1-2 | 前端 FilterChips | 独立 |
| P2-8 | 前端 store action | 独立 |

**建议实施顺序**（每步跑 `cargo test --test search_user_sim` + `npm run test:unit` 定向验证）：

1. 后端批 1：P1-4 + P2-7 + P1-3（小、独立、同域）
2. 后端批 2：P2-6（validate + build_plan_from_v3 捆绑 + 2 条契约测试）
3. 前端批：P2-5 → P1-1 → P1-2 → P2-8（顺序按依赖无关、风险从低到高）
4. 收尾：`npm run typecheck && npm run lint && cargo clippy` 全门禁 + 契约文档 V3 增补（P3-3）

**回归资产**：`src-tauri/tests/search_user_sim.rs` 已入仓库（未提交），14 条用例作为该链路的永久行为守护，建议随第一批修复一起提交。

---

## 四、总体评价

- **架构契约执行度高**：单一编译器、validate→prune→execute 顺序、must_not 极性白名单、warning 双通道、版本迁移、AI 三层降级——代码与 `search-plan-v3.md` 逐项吻合，35 项契约测试存在且通过。
- **前端状态机健壮**：planRevision 代际丢弃、请求代际失效、append/replace 合并规则与契约 §9 一致；`setQuery` 的 patchExistingPlan 对 AI 嵌套树的保留处理（不变式 1）实现得比契约要求的更细。
- **本次发现的问题全部是「局部确定性 bug」**：没有需要推翻设计的东西，按上面的顺序修即可，总改动量预估 < 150 行。
