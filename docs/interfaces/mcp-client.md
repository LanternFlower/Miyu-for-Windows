# MCP client(as-built)

实现 `crates/miyu-engine/src/tools/mcp.rs`:Miyu 作为客户端连接第三方 MCP 服务器,把它们的工具并进工具面。

## 配置

`mcp.enabled` 总开关;`mcp.servers[]`:`id`、`display_name`、`command`、`args`、`env`、
`timeout_seconds`(默认见 `default_mcp_timeout`)、`enabled`、`capabilities`(可选,服务器要向宿主查的信息,与脚本头部
`Capabilities:` 同一张词表;声明了就在拉起时注入 `MIYU_HOST_TOKEN` 等三个环境变量,令牌随服务器进程生灭,见 host-capabilities.md)。人格层再筛一道:
`PersonaManifest.plugins.mcp` 为 `Some(白名单)` 时只连名单里的 server;dev(`core_only`)不挂 MCP。
白名单由引导页(`miyu oobe` 功能屏最后一格「MCP 服务器」,`config::feature_catalog` `FeatureKind::Mcp`)逐台勾选写回:
全勾写 `None`,关过一台才写明细;机器级 `mcp.enabled` 关着整格不摆、手写名单原样保留。

## 协议

- 仅 **stdio** transport;`initialize` 声明 `protocolVersion: "2025-03-26"`;之后 `tools/list`、`tools/call`。
- JSON-RPC 2.0;server 的 stderr 直接丢弃(`Stdio::null()`)。
- **未实现**(planned):HTTP / SSE transport、`resources`、`prompts`、通知订阅。

## 工具映射

- 工具 id `mcp_<server>_<tool>`,两段都经 `sanitize_id`(非字母数字折 `_`、折叠连续 `_`、去首尾)。
  不同原名可能折成同一 id——现状**未检测碰撞**,后注册者覆盖,列为待改项。
- `inputSchema` 非对象时替换成 `{"type":"object","properties":{},"additionalProperties":true}`。
- 权限:MCP 工具注册为 `ToolPermission::ReadOnly` 缺省——只读模式下不会被闸,列为待改项(权限专项)。
- 场所信任缺省 `Owner`:不可信入口看不到 MCP 工具。

## 超时与缓存

| 项 | 值 |
|---|---|
| `tools/list` 预算 | `min(timeout_seconds, 15) + 5s`(与调用超时解耦,一个死 server 不能拖住整张工具面) |
| 列举失败 TTL | 60s 内不重试(`FAILED_LISTING_RETRY_AFTER`) |
| `tools/call` 预算 | `timeout_seconds + 5s`;超时杀子进程 |
| 目录缓存 | 按 server 配置指纹缓存 `tools/list` 结果,多 server 并行列举 |

## 结果映射

`tools/call` 返回 `isError: true` 时按**工具失败**回给模型(不是协议错误,模型能读到错误文本);
非文本 content 项按类型摘要。

## 验收

`cargo test --lib tools::mcp`(含假 server 的 initialize / list / call / isError / 预算用例)。
