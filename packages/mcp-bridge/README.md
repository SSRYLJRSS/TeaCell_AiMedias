# teacell-mcp-bridge

茶馆宿主（TeaCell）与外部 MCP 客户端之间的 **stdio 段** 桥接进程。

```
外部 MCP 客户端 ──stdio(JSON-RPC 2.0 / MCP)──▶ mcp-bridge ──本机命名管道──▶ 茶馆宿主
```

行为契约唯一事实源：[`docs/contracts/mcp-bridge-v1.md`](../../docs/contracts/mcp-bridge-v1.md)。
本 README 只讲**怎么配**，不重复契约条文。

## 设计约束（不为性能，是安全边界）

- **零第三方依赖**：只用 `node:` 内置模块。不引入 `@modelcontextprotocol/sdk`（约 95 个传递包），也不走 `rmcp`。
- 进程内**没有数据库连接、没有文件写操作、没有出站网络**。唯一允许的 fs 操作是 `readFile` 端点文件。
- `stdout` 只承载 JSON-RPC 帧；所有诊断写 `stderr`。日志经过脱敏，不出现 `key`、`token`、`connectionId` 或文件路径。
- 只实现契约 §2.1 的 5 个方法：`initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`。
  不实现 `sampling` / `roots` / `prompts` / 实验性 `Tasks`，也不提供任何批准能力。
- 平台：**仅 Windows x64**。其他平台返回 `UNSUPPORTED`。

## 客户端配置

桥接进程需要两个值，全部**不是密钥**：

| 值 | 来源 | 说明 |
|---|---|---|
| `endpoint` | 茶馆自动创建 | 端点文件 `<appData>/mcp/endpoint.json` 的路径 |
| `connection` | **茶馆设置界面** | 本地接入连接的 `connectionId`，形如 `mcpconn_0123456789abcdef` |

`key` **只由端点文件提供**，桥接进程自己读取，**绝不写进客户端配置、命令行参数或环境变量**。

### Claude Desktop / 兼容 `mcpServers` 的客户端

```json
{
  "mcpServers": {
    "teacell": {
      "command": "node",
      "args": [
        "F:\\vibecode\\chabaosucai\\packages\\mcp-bridge\\src\\server.mjs",
        "--endpoint",
        "C:\\Users\\<你>\\AppData\\Roaming\\TeaCell\\mcp\\endpoint.json",
        "--connection",
        "mcpconn_0123456789abcdef"
      ]
    }
  }
}
```

等价的环境变量写法（二选一，命令行为准）：

```json
{
  "mcpServers": {
    "teacell": {
      "command": "node",
      "args": ["F:\\vibecode\\chabaosucai\\packages\\mcp-bridge\\src\\server.mjs"],
      "env": {
        "TEACELL_MCP_ENDPOINT": "C:\\Users\\<你>\\AppData\\Roaming\\TeaCell\\mcp\\endpoint.json",
        "TEACELL_MCP_CONNECTION": "mcpconn_0123456789abcdef"
      }
    }
  }
}
```

> 端点文件里含本次启动周期生成的 `key`，宿主退出即失效。不要把 `key` 复制进客户端配置，
> 也不要把它粘进任何聊天、Issue 或日志里。

## 宿主未启动时会发生什么（预期行为）

**空工具表是预期行为，不是故障。**

1. 桥接进程照常启动，`initialize` 正常返回（`protocolVersion: "2025-11-25"`），MCP 会话能建立；
2. `tools/list` 返回 `{ "tools": [] }`，并在 `stderr` 打一条可判定日志（含 `HOST_UNAVAILABLE`）；
3. 任何 `tools/call` 返回 `isError: true`，文本以 `HOST_UNAVAILABLE:` 开头。

这样配置写错也不会让你的 MCP 会话起不来——你只需要先启动茶馆，然后重连。

## 错误码速查（契约 §6）

| 码 | 客户端该做什么 |
|---|---|
| `HOST_UNAVAILABLE` | 先启动茶馆；不要重试写操作 |
| `PROTOCOL_UNSUPPORTED` | 桥接或宿主版本不匹配，升级其中一端 |
| `PAIRING_REQUIRED` | 到茶馆设置里创建本地接入连接，拿到 `connectionId` |
| `PAIRING_REVOKED` | 连接已被撤销，重新创建一条 |
| `NOT_ALLOWED` | 该能力不在连接范围或不在可发布集合内 |
| `LIBRARY_OFFLINE` | 总仓未就绪或正在迁移维护 |
| `UNSUPPORTED` | 当前平台不支持（仅 Windows x64） |
| `INVALID_INPUT` | 你的载荷不合规（Base64、大小、字段规则）。改请求，不要重启茶馆，也不要改权限设置 |

## 开发

```powershell
cd packages/mcp-bridge
npm test        # node --test --test-concurrency=1 test/*.test.mjs，离线可跑
npm run check   # node --check 全部源文件
```

目录：

```
src/protocol.mjs   分帧（UTF-8 / 每行一个 JSON / LF / 单帧 ≤65,536 字节）、常量、payload 编解码
src/ipc.mjs        本机命名管道客户端 connectPipe + 测试用 createMemoryTransport
src/bridge.mjs     纯逻辑状态机：discover / connect / handshake / call / close
src/server.mjs     MCP stdio 服务端 + 进程装配（宿主不可用也能启动）
src/log.mjs        脱敏日志，只写 stderr
test/              node:test，无测试框架依赖
```
