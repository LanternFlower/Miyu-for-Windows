# Miyu MCP server(as-built)

`miyu mcp-serve`(`src/cli/mcp_serve.rs`):方向与 client 相反——把 Miyu 的工具面以 MCP
暴露给外部客户端。主要用途是中转线(claude-code / codex / antigravity)的工具桥:
上游 CLI 忽略请求里的 tools 数组,Miyu 的工具经这条桥被调用。

## 身份与守卫

| 环境变量 | 语义 |
|---|---|
| `MIYU_SESSION` | 以该会话身份列/调工具(daemon 存活时经 IPC);缺省为无会话 |
| `MIYU_MCP_REQUIRE_SESSION` | 为真且无 `MIYU_SESSION` 时,所有 `tools/call` 回 `isError` 文本 "no Miyu session is attached…" |
| `MIYU_MCP_EXCLUDE` | 逗号分隔的工具名;`tools/list` 不列,`tools/call` 回 `isError` |
| `MIYU_MCP_SCHEMA_DIALECT` | `gemini` 时按 `mcp_schema.rs` 整形 schema(去 `additionalProperties`/`pattern`/`default`、空 enum 等);工具自身 schema 不改 |

会话的宿主工具面按平台回合登记的能力位裁决(`host_ports::live_turn_host_tools_allowed`),
桥拿不到 `PlatformTurnContext` 本体。

## 协议

- `initialize`:回显客户端的 `protocolVersion`(没带则 `2024-11-05`),`capabilities: { tools: {} }`,
  `serverInfo: { name: "miyu", version: <crate 版本> }`。不做版本协商拒绝——本服务只用 tools 基本面,各版本同形。
- `tools/list`、`tools/call`;通知(无 id)不应答。
- 工具失败 → `isError: true` 结果而非 JSON-RPC error,失败文本放进 `content[0].text`,模型能读到。
- 不暴露 HTTP `ApiError` 或任何凭据。

## 验收

`cargo test --lib mcp_serve`、`cargo test --lib claude_code`;真机:`testkit/claude-code/`。
