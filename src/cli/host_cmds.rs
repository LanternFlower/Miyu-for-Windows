//! `miyu host <method> [params-json]`:脚本这类进程外扩展查宿主信息的入口。
//!
//! 令牌由 Miyu 拉起脚本时写进 `MIYU_HOST_TOKEN`(只有头部声明了 `Capabilities:`
//! 且在 daemon 里跑的脚本才有);这里只是把它连同方法名经 IPC 送给 daemon,
//! 原样打印 `{"ok":true,"data":…}` 或 `{"ok":false,"error":{"code","message"}}`,
//! 失败退出码 1。契约见 `docs/interfaces/host-capabilities.md`。

use crate::cli::*;

#[derive(Debug, Args)]
pub struct HostArgs {
    /// 方法名:host.info / providers.list / providers.get / subsystems.enabled
    pub method: String,
    /// 参数,JSON 对象(如 '{"provider_id":"deepseek"}');缺省 {}
    pub params: Option<String>,
    /// 令牌;缺省读环境变量 MIYU_HOST_TOKEN
    #[arg(long)]
    pub token: Option<String>,
}

pub(in crate::cli) async fn run_host(paths: &MiyuPaths, args: HostArgs) -> Result<()> {
    let token = args
        .token
        .or_else(|| std::env::var("MIYU_HOST_TOKEN").ok())
        .filter(|token| !token.trim().is_empty());
    let params: serde_json::Value = match args.params.as_deref().map(str::trim) {
        None | Some("") => serde_json::json!({}),
        Some(raw) => serde_json::from_str(raw).context("params must be a JSON object")?,
    };
    let outcome = match token {
        None => serde_json::json!({
            "ok": false,
            "error": {
                "code": "permission_denied",
                "message": "MIYU_HOST_TOKEN is not set: only scripts that declare `Capabilities:` and run inside the Miyu daemon get one",
            }
        }),
        Some(token) => {
            match send_ipc_admin(
                paths,
                IpcCommand::HostQuery {
                    token,
                    method: args.method,
                    params,
                },
            )
            .await
            {
                Ok((_, data)) => data,
                Err(error) => serde_json::json!({
                    "ok": false,
                    "error": { "code": "unavailable", "message": format!("{error:#}") }
                }),
            }
        }
    };
    println!("{outcome}");
    if outcome.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        std::process::exit(1);
    }
    Ok(())
}
