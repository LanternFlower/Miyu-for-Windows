use super::{ToolRegistry, ToolSpec};
use crate::config::{AppConfig, McpServerConfig};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const JSONRPC_VERSION: &str = "2.0";

#[derive(Debug, Clone)]
struct McpToolBinding {
    server: McpServerConfig,
    tool_name: String,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ToolsListResult {
    #[serde(default)]
    tools: Vec<McpToolInfo>,
}

#[derive(Debug, Deserialize)]
struct McpToolInfo {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "inputSchema")]
    input_schema: Value,
}

pub fn register(registry: &mut ToolRegistry, config: AppConfig) {
    for server in config
        .mcp
        .servers
        .iter()
        .filter(|server| server.enabled && !server.id.trim().is_empty())
    {
        // 有界列举:一个挂起的 server 不能吊死整个注册流程(启动路径)。
        // 失败必须留日志,否则用户无从排查工具为何消失。
        let tools = match list_server_tools_with_timeout(server) {
            Ok(tools) => tools,
            Err(error) => {
                tracing::warn!(
                    server = %server.id,
                    error = %format!("{error:#}"),
                    "MCP server failed to start or list tools; its tools are skipped"
                );
                continue;
            }
        };
        for tool in tools {
            let tool_id = mcp_tool_id(&server.id, &tool.name);
            let display_name = if server.display_name.trim().is_empty() {
                format!("MCP {} / {}", server.id, tool.name)
            } else {
                format!("MCP {} / {}", server.display_name, tool.name)
            };
            let binding = McpToolBinding {
                server: server.clone(),
                tool_name: tool.name.clone(),
            };
            let description = if tool.description.trim().is_empty() {
                format!("Call MCP tool {} from server {}.", tool.name, server.id)
            } else {
                tool.description.clone()
            };
            registry.register(
                ToolSpec::new(
                    tool_id,
                    description,
                    normalize_schema(tool.input_schema),
                    move |args| {
                        let binding = binding.clone();
                        async move { call_mcp_tool_async(binding, args).await }
                    },
                )
                .with_display_name(display_name)
                .with_always_loaded(false),
            );
        }
    }
}

/// 会话内的 `request` 超时只在两次成功读之间生效:server 完全不吐字节时
/// `read_line` 永久阻塞,轮不到超时检查。外层兜底 = 会话超时 + 5s 余量,
/// 超时后 SIGKILL 子进程让阻塞读收到 EOF,工作线程随之回收。
fn kill_mcp_child(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn overall_timeout(server: &McpServerConfig) -> Duration {
    Duration::from_secs(server.timeout_seconds.max(1)) + Duration::from_secs(5)
}

fn list_server_tools(server: &McpServerConfig) -> Result<Vec<McpToolInfo>> {
    list_server_tools_inner(server, None)
}

fn list_server_tools_inner(
    server: &McpServerConfig,
    pid_notify: Option<std::sync::mpsc::Sender<u32>>,
) -> Result<Vec<McpToolInfo>> {
    let mut session = McpSession::start(server)?;
    if let Some(notify) = pid_notify {
        let _ = notify.send(session.child.id());
    }
    session.initialize()?;
    let result = session.request("tools/list", json!({}))?;
    let parsed: ToolsListResult = serde_json::from_value(result)?;
    Ok(parsed.tools)
}

fn list_server_tools_with_timeout(server: &McpServerConfig) -> Result<Vec<McpToolInfo>> {
    let deadline = overall_timeout(server);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let (pid_tx, pid_rx) = std::sync::mpsc::channel();
    let server_clone = server.clone();
    std::thread::spawn(move || {
        let _ = result_tx.send(list_server_tools_inner(&server_clone, Some(pid_tx)));
    });
    match result_rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(_) => {
            if let Ok(pid) = pid_rx.try_recv() {
                kill_mcp_child(pid);
            }
            bail!(
                "MCP server {} did not answer tools/list within {}s",
                server.id,
                deadline.as_secs()
            )
        }
    }
}

fn call_mcp_tool(binding: McpToolBinding, args: Value) -> Result<String> {
    call_mcp_tool_inner(binding, args, None)
}

fn call_mcp_tool_inner(
    binding: McpToolBinding,
    args: Value,
    pid_notify: Option<std::sync::mpsc::Sender<u32>>,
) -> Result<String> {
    let mut session = McpSession::start(&binding.server)?;
    if let Some(notify) = pid_notify {
        let _ = notify.send(session.child.id());
    }
    session.initialize()?;
    let result = session.request(
        "tools/call",
        json!({
            "name": binding.tool_name,
            "arguments": args,
        }),
    )?;
    Ok(format_mcp_result(&result))
}

/// 同步 MCP 会话移出 async 线程(spawn_blocking):在 actor 的单线程
/// runtime 上直接跑同步 IO 会冻结全部并发 turn;多次挂起还会耗尽 tokio
/// 阻塞池。外层超时 + kill 保证阻塞线程一定能回收。
async fn call_mcp_tool_async(binding: McpToolBinding, args: Value) -> Result<String> {
    let deadline = overall_timeout(&binding.server);
    let server_id = binding.server.id.clone();
    let (pid_tx, pid_rx) = std::sync::mpsc::channel();
    let task = tokio::task::spawn_blocking(move || call_mcp_tool_inner(binding, args, Some(pid_tx)));
    match tokio::time::timeout(deadline, task).await {
        Ok(joined) => joined.context("MCP worker task failed")?,
        Err(_) => {
            if let Ok(pid) = pid_rx.try_recv() {
                kill_mcp_child(pid);
            }
            bail!(
                "MCP server {server_id} did not answer within {}s",
                deadline.as_secs()
            )
        }
    }
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    timeout: Duration,
}

impl McpSession {
    fn start(server: &McpServerConfig) -> Result<Self> {
        if server.command.trim().is_empty() {
            bail!("MCP server {} has no command", server.id);
        }
        let mut command = Command::new(&server.command);
        command.args(&server.args);
        for (key, value) in &server.env {
            command.env(key, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server {}", server.id))?;
        let stdin = child.stdin.take().context("failed to open MCP stdin")?;
        let stdout = child.stdout.take().context("failed to open MCP stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            timeout: Duration::from_secs(server.timeout_seconds.max(1)),
        })
    }

    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "miyu", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&request)?;
        let started = Instant::now();
        loop {
            if started.elapsed() > self.timeout {
                bail!("MCP request timed out: {method}");
            }
            let response = self.read_message()?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            let response: JsonRpcResponse = serde_json::from_value(response)?;
            if let Some(error) = response.error {
                bail!(
                    "MCP error {}: {}{}",
                    error.code,
                    error.message,
                    format_error_data(&error.data)
                );
            }
            return Ok(response.result.unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": method,
            "params": params,
        }))
    }

    fn write_message(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let bytes = self.stdout.read_line(&mut line)?;
        if bytes == 0 {
            bail!("MCP server closed stdout");
        }
        Ok(serde_json::from_str(line.trim())?)
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn mcp_tool_id(server_id: &str, tool_name: &str) -> String {
    format!("mcp_{}_{}", sanitize_id(server_id), sanitize_id(tool_name))
}

fn sanitize_id(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

fn normalize_schema(schema: Value) -> Value {
    if schema.is_object() {
        schema
    } else {
        json!({"type":"object","properties":{},"additionalProperties":true})
    }
}

fn format_mcp_result(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        let parts = content
            .iter()
            .filter_map(format_content_part)
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return parts.join("\n\n");
        }
    }
    serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
}

fn format_content_part(value: &Value) -> Option<String> {
    match value.get("type").and_then(Value::as_str) {
        Some("text") => value
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(kind) => {
            Some(serde_json::to_string_pretty(value).unwrap_or_else(|_| kind.to_string()))
        }
        None => Some(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())),
    }
}

fn format_error_data(data: &Option<Value>) -> String {
    data.as_ref()
        .map(|data| format!(": {}", data))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn sanitizes_mcp_tool_ids() {
        assert_eq!(
            mcp_tool_id("file-system", "read_file"),
            "mcp_file_system_read_file"
        );
    }

    #[test]
    fn formats_text_content_results() {
        let result = json!({"content":[{"type":"text","text":"hello"}]});
        assert_eq!(format_mcp_result(&result), "hello");
    }

    #[test]
    fn lists_and_calls_stdio_mcp_tool() {
        let script = r#"
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if 'id' not in request:
        continue
    if method == 'initialize':
        result = {'protocolVersion':'2025-03-26','capabilities':{},'serverInfo':{'name':'mock','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'echo','description':'Echo text','inputSchema':{'type':'object','properties':{'text':{'type':'string'}}}}]}
    elif method == 'tools/call':
        text = request.get('params', {}).get('arguments', {}).get('text', '')
        result = {'content':[{'type':'text','text':'echo: ' + text}]}
    else:
        result = {}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#;
        let server = McpServerConfig {
            id: "mock".to_string(),
            display_name: String::new(),
            command: "python".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            env: HashMap::new(),
            timeout_seconds: 5,
            enabled: true,
        };

        let tools = list_server_tools(&server).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let output = call_mcp_tool(
            McpToolBinding {
                server,
                tool_name: "echo".to_string(),
            },
            json!({"text":"hi"}),
        )
        .unwrap();
        assert_eq!(output, "echo: hi");
    }
}
