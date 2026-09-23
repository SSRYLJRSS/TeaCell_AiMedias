# 茶包素材 BagerTea AiMedias — 跨平台开发流程审查报告

> 审查日期：2026-09-21
> 审查范围：`main`（683628a）+ `wt/three_gogogo`（f645dc4）双分支现状
> 审查方式：全仓源码审计（70 个 Rust 文件 / 198 个 TS/TSX 文件）+ 依赖链溯源 + 官方仓库实测对照
> 结论摘要：**架构方向正确，但存在 4 个真实缺陷，且当前没有任何机制能发现它们。**

---

## 一、一句话结论

你的项目**不需要**按平台拆分分支（业界无此做法，成本呈平方级增长）。
但**必须**补上两样东西：**平台 CI 矩阵** + **平台差异收敛纪律**。
在补齐之前，"Windows 上测试全绿"与"Mac/Linux 能正常工作"之间**没有任何因果关系**。

---

## 二、现状评估

### 2.1 做对的部分（这些请保持）

| 项 | 实测证据 | 评价 |
|---|---|---|
| 平台差异已收敛 | 全仓仅 18 处 `cfg(target_os)`，集中在 5 个文件 | 密度健康 |
| 前端零平台探测 | `navigator.platform` / `process.platform` / `isWindows` 全仓 **0 命中** | 设计正确 |
| 平台能力层设计 | `services/platform.rs` 为纯函数、零副作用、有单测；`platformStore.ts` 带代际防竞态 | 优秀 |
| 路径双实现 | `utils/path.rs` 用 `#[cfg(windows)]` / `#[cfg(not(windows))]` 分两套 | 正确做法 |
| 子进程超时保护 | `video.rs` 给 ffmpeg 加 30s 墙 + 60s 探测缓存 | 健壮 |
| 外部工具探测 | `resolve_binary` 走 PATH 裸名查找，三端通用 | 正确 |
| GPU 探测降级 | `probe_gpu()` 失败返回 `unknown` 不猜测 | 保守正确 |
| 依赖条件化 | `Cargo.toml` 已按 target 精确选择 keyring 后端 | 正确 |

**特别是前端零平台探测这一点。** 大多数跨平台项目都栽在"组件里到处写 `if (isMac)`"，你从设计上避免了。

### 2.2 阻塞性问题

#### 【P0-1】`heif-bin/` 进了 git 仓库，且是 Windows 专属产物

**证据**：
```
$ git ls-files heif-bin/ | head
heif-bin/heif.zip
heif-bin/include/libheif/heif.h
heif-bin/lib/heif.lib          ← MSVC 专属静态库
heif-bin/lib/libde265.lib
heif-bin/lib/x265-static.lib

$ du -sh heif-bin/
65M	heif-bin/
```

**机制溯源**（heif-rs 官方 README）：
> "The first build downloads the prebuilt static binaries **for your platform**, so an internet connection is required."

**问题**：
1. `.lib` 是 MSVC 格式，macOS/Linux **无法使用**，但会被 git 一起分发
2. 这 65MB 极可能就是此前 Clash 代理下 git push 失败的原因
3. 更严重：它给 Mac/Linux 构建制造了**虚假预期**——"库已经在仓库里了"，实际对那两端无效

**建议**：`heif-bin/` 加入 `.gitignore` 并从索引移除（保留本地文件）。
三端构建由 `heif-rs` 自行下载对应平台预编译库；离线构建用 `HEIF_BINARIES_DIR` 环境变量。

> ⚠️ 从 git 移除 65MB 需要处理历史（`git filter-repo` 或接受历史仍含大文件）。
> 若只做 `git rm --cached`，**历史体积不变**，clone 仍要下载。详见第六节。

---

#### 【P0-2】Unix 专属代码从未被执行过一次

**证据**：
```
$ grep -rn "cfg(not(windows))" src-tauri/src src-tauri/tests   # 在 wt/three_gogogo 上
src-tauri/src/services/ollama_runtime.rs:186
src-tauri/src/utils/path.rs:36     ← Unix 版 normalize_path
src-tauri/src/utils/path.rs:93     ← Unix 版 path_contains_nul
src-tauri/src/utils/path.rs:110    ← 测试：normalize_unix_keeps_backslash_and_case
src-tauri/src/utils/path.rs:135    ← 测试：native_absolute_unix
src-tauri/src/utils/path.rs:145    ← 测试：encode_native_path_rejects_non_utf8
```

**问题**：`cfg(not(windows))` 的代码在 Windows 上**不编译**。
你的 CI 只有 `runs-on: windows-latest`。因此：
- 那 3 个 Unix 测试**从未运行**
- Unix 版 `normalize_path` 的正确性**从未被验证**
- 谁也无法证明 Linux 分支能编译通过

**这是"分平台分支"根本无法解决的**——就算你分成三个分支，只要没有 Linux 机器跑 CI，问题照旧。

**建议**：加 CI 矩阵（详见第五节），第一次跑极可能直接爆出编译错误。

---

#### 【P0-3】Linux 的 keyring 依赖需要系统库，当前无人验证

**证据**（crates.io API 查询 keyring 3.6.3）：
```
target=cfg(target_os = "linux") | dbus-secret-service ^4.0.0-rc.2 optional=True
target=cfg(target_os = "linux") | secret-service ^4 optional=True
target=cfg(target_os = "linux") | zbus ^4 optional=True
```

**问题**：`secret-service` 走 D-Bus 通信，Linux 上编译需要 `libdbus-1-dev` 等系统开发包。
这正是 Tauri 官方 CI 有这一步的原因：
```yaml
- name: install Linux dependencies
  if: contains(matrix.platform.target, 'unknown-linux')
```

你的 `Cargo.toml` 选了 `features = ["sync-secret-service", "crypto-rust"]`——**选择是对的**，但从未在真实 Linux 环境验证过能否编译。

---

#### 【P0-4】前端 `libraryStore.ts` 存在与后端不一致的平台 bug

**证据**：`src/stores/libraryStore.ts:17`
```ts
function kinshipKeyOf(a: Asset): { key: string; isRaw: boolean } {
  const p = (a.filePath ?? "").split("\\").join("/").toLowerCase();
  //                              ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ 两个问题
```

**对比后端已修复版本**（`src-tauri/src/services/kinship.rs`）：
```rust
// 三端复核 B2 收紧（修复 /A 与 /a 被错误合并）：
// - 目录部分不做全路径 lowercase
// - 不做 \ → / 替换：Unix 侧反斜杠是合法文件名字符
```

**两个缺陷**：
1. `.split("\\").join("/")` 把 `\` 当分隔符 —— Unix 上 `\` 是合法文件名字符，会**截断文件名**
2. `.toLowerCase()` 作用于**全路径** —— Linux 路径大小写敏感，会把不同目录**错误合并**

**后果**：同一批素材，后端判定"不同源"，前端判定"同源"。
`collapseKinship` 的折叠结果与后端 `is_kinship` 不一致 —— **UI 显示与数据库语义相矛盾**。

**已确认**：`libraryStore.ts` 在 `main` 和 `wt/three_gogogo` 上**都未被修改**。这是一个漏网 bug。

**有趣的是**，你的分支上已经存在正确的解决方案 `src/utils/pathDisplay.ts`，其头注明确写着：
> "缺陷背景：各处用 `path.split(/[\\/]/).pop()` 把反斜杠也当分隔符。这在 Windows 正确，但在 Unix，反斜杠是**合法文件名字符**"

**说明你已系统性修过一遍这类问题，但 `libraryStore.ts` 漏了。**

---

#### 【P1-1】平台适配无权威文档

`docs/` 下 12 个文档，**没有任何一份描述平台差异**。
新加入者（或未来的你）无法知道：哪些差异是设计决策、哪些是妥协、改哪里合法。

**建议**：新增 `docs/PLATFORM.md`，并在 `AGENTS.md` 补一条硬约束（详见第五节）。

---

#### 【P1-2】工作区未提交改动积压严重

```
main 工作区：       82 个已修改 + 4 个未跟踪 = 86
wt/three_gogogo：    72 个
main 领先 origin/main：58 个提交（未推送）
```

**风险**：跨平台审计是在"未提交状态"做的，若这些改动丢失，审计结论的基础也会变化。
且 58 个提交积压意味着本地与远端长期不一致 —— 出问题时无法用远端做对照。

---

## 三、四个缺陷的共同点

这四个问题**有一个完全相同的特征**：

> **它们在 Windows 上都表现为"完全正常"。**

| 缺陷 | Windows 上的表现 |
|---|---|
| heif-bin 入库 | 能编译，只是仓库胖 |
| Unix 代码没跑过 | 不影响，根本不编译 |
| Linux keyring | 不影响，不编译 |
| libraryStore 路径 bug | 结果**恰好正确**（`\` 确实是分隔符，大小写确实不敏感） |

**这解释了为什么"分平台分支"是个陷阱**：
你以为分了分支就能"分别在每个平台上验证"，但**真正缺的不是分支，是能跑起来的 Linux/macOS 环境**。
分三个分支之后，那三个分支上的问题依旧一模一样地存在 —— 只是从"一个不知道坏没坏的版本"变成"三个不知道坏没坏的版本"。

---

## 四、业界实测对照（`gh` 直读官方仓库，非搜索结果）

| 仓库 | 分支构成 | 平台分支数 |
|---|---|---|
| `tauri-apps/tauri` | 版本分支 `1.x`/`2.11` + `feat/*` + `chore/*` | **0** |
| `spacedriveapp/spacedrive` | 仅 `main`、`v1`、`v2`、`pr-3037` | **0** |
| `AppFlowy-IO/AppFlowy` | `main` + `feat/*` + `chore/*` | **0** |

Tauri 官方 `test-core.yml`：
```yaml
runs-on: ${{ matrix.platform.os }}
matrix:
  platform:
    - { os: windows-latest, ... }
    - { os: ubuntu-latest,  ... }
    - { os: macos-14,       ... }   # ARM
    - { os: macos-latest,   ... }   # x86
    - { os: ubuntu-latest,  ... }   # 交叉编译
```

**关键观察**：`AppFlowy-IO/AppFlowy` 有一个分支叫 `feat/billing-client-windows`。
名字含 "windows"，但它是**功能维度**分支（Windows 端计费功能），**不是平台分支**。
这个区分是理解整个问题的关键。

**结论**：跨平台项目的分支按**功能/版本**划分，平台差异靠**条件编译 + CI 矩阵**处理。无例外。

---

## 五、建议的落地动作

### 5.1 立即修（P0）

| # | 动作 | 具体操作 |
|---|---|---|
| 1 | 修 `libraryStore.ts:17` | 改为按平台判定分隔符 + 只对 stem 折叠大小写（对齐后端 `kinship.rs`） |
| 2 | 加 CI 矩阵 | 见下方配置 |
| 3 | 处理 `heif-bin/` | 加入 `.gitignore` + `git rm -r --cached heif-bin/` |
| 4 | 清理工作区 | 处理 86 + 72 个未提交改动，推送积压的 58 个提交 |

### 5.2 CI 矩阵配置建议

改造 `.github/workflows/smoke.yml`，保留现有 Windows 完整流程，新增轻量平台校验：

```yaml
jobs:
  full-test:
    runs-on: windows-latest        # 保持现有完整测试不变
    # ... 现有全部步骤

  platform-check:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: actions/setup-node@v4
        with: { node-version: 22, cache: npm }

      - name: 安装 Linux 系统依赖
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get update
          sudo apt-get install -y libdbus-1-dev pkg-config \
            libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev

      - run: npm ci
      - name: Rust 编译校验（含 Unix 专属代码路径）
        run: cargo check --manifest-path src-tauri/Cargo.toml --all-targets
      - name: Rust 格式与 lint
        run: |
          cargo fmt --manifest-path src-tauri/Cargo.toml --check
          cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
      - name: Rust 测试（关键：跑 Unix 专属测试）
        run: cargo test --manifest-path src-tauri/Cargo.toml --lib
      - run: npm run typecheck
      - run: npm run test:unit
```

**注意**：`cargo check --all-targets` 会编译**测试代码**，这是让 `#[cfg(not(windows))]` 测试真正被执行的关键。

### 5.3 新增开发纪律（写入 `AGENTS.md`）

在"不可协商的规则"里追加两条：

```markdown
11. 平台差异只允许出现在两个位置：`services/platform.rs`（能力探测）与
    `Cargo.toml` 的 `[target.'cfg(...)'.dependencies]`（依赖差异）。
    其余任何位置出现 `cfg(target_os)`，必须先说明为何不能收敛到 platform.rs。
12. 涉及「操作系统资源」的代码必须显式考虑三端差异，并写平台分派测试。
    涉及「数据结构」的代码不得引入平台假设。
```

### 5.4 新增 `docs/PLATFORM.md`

建议内容提纲：
- 三端支持矩阵（功能 × 平台）
- 已知平台差异清单及原因（Ollama 托管仅 Windows、Linux 视频代理用 VP8 等）
- 平台差异代码的放置规则
- 各平台构建前置条件（Linux 系统包、macOS 签名等）
- 真机验收清单

---

## 六、关于 `heif-bin/` 的历史清理

`git rm --cached` **不会**减小历史体积。两种方案：

| 方案 | 效果 | 代价 |
|---|---|---|
| 仅 `rm --cached` + gitignore | 新提交不含，**历史仍 65MB** | 零风险，clone 仍慢 |
| `git filter-repo` 重写历史 | 彻底清除，clone 变快 | **重写所有 commit hash**，需强推，其他人需重新 clone |

**建议**：
- 若仓库只有你一个人用 → 值得做 `filter-repo`
- 若已有人 clone 过 → 先做前者，等有明确需求再重写

⚠️ `filter-repo` 会改变所有历史 hash，属于破坏性操作，**执行前必须完整备份仓库**。

---

## 七、以后怎么开发（操作手册）

### 7.1 日常改功能（占 90% 时间）

```
1. 从 main 开短分支（或直接改，单人可以简化）
2. 写代码 —— 见下方「判别法」决定要不要想平台
3. 遇到平台差异 → 不要写 if，去 platform.rs 加能力开关
4. 本地跑：npm run typecheck && npm run lint
           cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all-features
5. push → CI 自动在三端校验
6. 绿了合回 main，删分支
```

### 7.2 判别法：这段代码要不要考虑平台？

**问自己一句话：这段逻辑接触的是「数据」还是「操作系统」？**

| 接触 | 举例 | 要不要管平台 |
|---|---|---|
| **数据** | 业务逻辑、状态机、React 组件、SQL 查询、图片解码、网络请求、类型定义 | **不用管，照常写** |
| **操作系统** | 文件路径、文件名编码、进程调用、系统 API、凭据存储、文件权限 | **必须管，且必须查三端差异** |

你的 18 处 `cfg(target_os)` 全部落在"接触操作系统"一侧 —— 这个分工是健康的。

### 7.3 高危清单（每次碰到都要停一下）

| 高危点 | 你的项目实例 | 写错的后果 |
|---|---|---|
| 路径拼接与分隔符 | `utils/path.rs`、`libraryStore.ts:17` | Unix 上文件名被改写 |
| 大小写敏感性 | `kinship.rs`、`libraryStore.ts:17` | 误判同源，**静默扩散标签** |
| 文件名编码 | `importer.rs`（X-18） | 非 UTF-8 文件名有损入库 |
| 调用外部进程 | `video.rs`、`ollama_*` | Mac/Linux 上找不到二进制 |
| 系统 API 与凭据 | keyring 三后端 | 编译失败或存不进去 |
| 文件锁与权限 | SQLite WAL、临时文件 | Linux 上权限不足 |

### 7.4 回答"这样真的可以吗"

**可以，但有两个前提，缺一不可：**

1. **CI 矩阵必须真的在跑** —— 这是"下限保障"。
   平台无关区写错（如误用 `std::os::windows::...`）→ ubuntu 编译报错，立刻知道。
2. **高危清单必须自己守** —— 这是"纪律"。
   高危区写错（如路径没分平台）→ **CI 有时查不出**，因为它在 Windows 上恰好正确。

**如果 CI 不配，那"不用考虑平台"就真的只是碰运气了。**

---

## 八、TODO（本次审查未能验证的事项）

以下内容受环境限制无法验证，**不代表它们没问题**：

- [ ] 代码在真实 macOS 上的编译与运行（无机器）
- [ ] 代码在真实 Linux 上的编译与运行（无机器）
- [ ] Linux 上 `libdbus-1-dev` 等系统包的确切清单（需实际跑一次才知道）
- [ ] macOS 代码签名与公证流程（发布阶段问题，本次未涉及）
- [ ] `heif-rs` 在 macOS ARM 上是否有预编译库（未查 binaries-heif releases）
- [ ] 前端 `vitest` 用例在非 Windows 环境的表现（CI 装配后才能确认）
- [ ] 86 + 72 个未提交改动的完整内容审查（仅统计了数量与分布）

---

## 附录：本次审查的实测命令

```bash
# 分支关系
git merge-base main wt/three_gogogo          # == main tip → fast-forward 可合
git rev-list --left-right --count main...wt/three_gogogo   # 0  9

# 平台代码分布
grep -rn "cfg(target_os\|cfg(windows)\|cfg(not(windows))" src-tauri/src

# 前端平台探测（预期 0 命中）
grep -rn "navigator.platform\|isWindows\|isMac\|isLinux" src

# 平台产物入库检查
git ls-files heif-bin/ && du -sh heif-bin/

# 依赖平台条件
curl -s "https://crates.io/api/v1/crates/keyring/3.6.3/dependencies"

# 业界对照
gh api repos/tauri-apps/tauri/branches --jq '.[].name'
gh api repos/spacedriveapp/spacedrive/branches --jq '.[].name'
gh api repos/AppFlowy-IO/AppFlowy/branches --jq '.[].name'
gh api repos/tauri-apps/tauri/contents/.github/workflows/test-core.yml --jq '.content' | base64 -d
```
