//! `miyu tool` 与 `miyu tool-call`：在命令行里直接调工具。
//!
//! 调试与脚本化用的入口。工具的产出（图片、artifact）在终端里要能看见，所以
//! 这里有一小段远端图片预览的处理。

use crate::cli::*;

pub(in crate::cli) async fn run_tool(paths: &MiyuPaths, mode: AgentMode, args: ToolArgs) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let registry = build_tool_registry(&config, paths, mode, false)?;
    let output = registry
        .call(&args.name, args.arguments.as_deref().unwrap_or("{}"))
        .await?;
    println!("{output}");
    Ok(())
}

/// 工具桥客户端(任务#12)。bash 就是编排层:脚本里循环/管道串结构化
/// 工具,中间数据本地流动、不经模型上下文往返;每次内层调用都以本回合的
/// 会话身份与来源在 daemon 侧过 guard/超时管线。daemon 不在(直连调试
/// 形态)则本地执行,语义一致但 jobs 等 daemon 态不可见。
pub(in crate::cli) async fn run_tool_call(paths: &MiyuPaths, args: ToolCallArgs) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let env_mode = std::env::var("MIYU_TURN_MODE").unwrap_or_default();
    let mode = if env_mode == "dev" {
        AgentMode::Dev
    } else {
        AgentMode::Normal
    };
    if args.list || args.describe {
        if args.describe && args.name.is_none() {
            bail!("--describe 需要工具名");
        }
        // daemon 存活时目录走 IPC:与 ToolCall 同一条会话→模式→registry
        // 解析链,--list 列出的就是本会话真能调的集合。此前本地建表按
        // MIYU_TURN_MODE 环境变量定模式(run_command 并不注入它),dev 会话
        // 里 --list 展示普通人格全量目录,实测逐个调用全报 unknown tool。
        if ipc::daemon_info(paths).await.is_some() {
            let session = std::env::var("MIYU_SESSION").ok().filter(|s| !s.is_empty());
            let (_, data) = send_ipc_admin(
                paths,
                IpcCommand::ToolCatalog {
                    session,
                    name: args.describe.then(|| args.name.clone()).flatten(),
                    full: false,
                },
            )
            .await?;
            let mode_label = data
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            if args.list {
                let tools = data
                    .get("tools")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                // 来源说明走 stderr,stdout 保持纯 name\tdisplay 供脚本管道。
                eprintln!(
                    "{}",
                    if is_zh() {
                        format!("# 本会话目录({mode_label} 模式,{} 个工具);列出即可调用", tools.len())
                    } else {
                        format!("# session catalog ({mode_label} mode, {} tools); listed = callable", tools.len())
                    }
                );
                for tool in &tools {
                    let name = tool
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    match tool
                        .get("display_name")
                        .and_then(serde_json::Value::as_str)
                        .filter(|display| !display.is_empty())
                    {
                        Some(display) => println!("{name}\t{display}"),
                        None => println!("{name}"),
                    }
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        data.get("tool").unwrap_or(&serde_json::Value::Null)
                    )?
                );
            }
            return Ok(());
        }
        // 直连回退:本地建表,与下方调用回退同一构建方式,依旧同源。
        let registry = build_tool_registry(&config, paths, mode, false)?;
        if args.list {
            let mut names = registry.tool_names();
            names.sort();
            eprintln!(
                "{}",
                if is_zh() {
                    format!("# 直连目录(daemon 不在,{} 个工具)", names.len())
                } else {
                    format!("# direct catalog (daemon absent, {} tools)", names.len())
                }
            );
            for name in names {
                let display = registry.display_name(&name).unwrap_or_default();
                if display.is_empty() {
                    println!("{name}");
                } else {
                    println!("{name}\t{display}");
                }
            }
            return Ok(());
        }
        let name = args.name.as_deref().expect("checked above");
        let Some(spec) = registry.get(name) else {
            return Err(registry.unknown_tool_error(name));
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters,
            }))?
        );
        return Ok(());
    }
    let Some(name) = args.name.clone() else {
        // 裸 `miyu tool-call` 是来问路的,给完整帮助而不是一行报错。
        localized_command()
            .find_subcommand_mut("tool-call")
            .expect("tool-call subcommand exists")
            .print_help()?;
        return Ok(());
    };
    let arguments = if args.args_stdin {
        let mut raw = String::new();
        use std::io::Read as _;
        io::stdin().read_to_string(&mut raw)?;
        raw
    } else if let Some(file) = &args.args_file {
        std::fs::read_to_string(file)?
    } else {
        args.arguments.clone().unwrap_or_else(|| "{}".to_string())
    };
    let session = std::env::var("MIYU_SESSION").ok().filter(|s| !s.is_empty());
    let origin = std::env::var("MIYU_TURN_ORIGIN").ok().filter(|s| !s.is_empty());
    let depth: u32 = std::env::var("MIYU_BRIDGE_DEPTH")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(0);

    if ipc::daemon_info(paths).await.is_some() {
        let (_, data) = send_ipc_admin(
            paths,
            IpcCommand::ToolCall {
                session,
                name,
                arguments,
                origin,
                depth,
            },
        )
        .await?;
        let output = data
            .get("output")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        println!("{output}");
        return Ok(());
    }

    // 直连回退:本地建 registry(guard/超时同源),会话与来源按环境作用域化。
    // jobs 等 daemon 内存态在本地进程不可见,直连调试形态可接受。
    if depth >= crate::tools::workspace::MAX_BRIDGE_DEPTH {
        bail!("tool bridge recursion limit reached (depth {depth})");
    }
    let registry = build_tool_registry(&config, paths, mode, false)?;
    if !registry.contains(&name) {
        bail!(
            "{:#}. {}",
            registry.unknown_tool_error(&name),
            t(
                "run `miyu tool-call --list` to see tools callable in this session",
                "用 `miyu tool-call --list` 查看本会话可调用的工具"
            )
        );
    }
    let turn_origin: crate::tools::workspace::TurnOrigin = origin
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(crate::tools::workspace::TurnOrigin::Human);
    let invoke = crate::tools::workspace::with_turn_origin(
        turn_origin,
        crate::tools::workspace::with_bridge_depth(depth + 1, async {
            registry.call(&name, &arguments).await
        }),
    );
    let output = match session {
        Some(session) => {
            let session: std::sync::Arc<str> = session.into();
            crate::tools::workspace::with_session(session, invoke).await?
        }
        None => invoke.await?,
    };
    println!("{output}");
    Ok(())
}

/// daemon 模式下真正画图的是终端这一侧，尺寸也只能在这里定。
///
/// 此前只有表情包工具算尺寸，别的工具一律传 None，而 `parse_size(None)`
/// 的语义是「铺满整个终端」——生图、print_image、搜图预览于是全都印成满
/// 屏。百分比默认值依赖 `crossterm::terminal::size()`，daemon 量到的不是
/// 用户的终端，所以必须在这里算；模型显式要的尺寸则随事件带过来。
pub(in crate::cli) fn remote_tool_image_size(
    tool_name: &str,
    requested: &str,
    config: &crate::config::AppConfig,
) -> Option<String> {
    let requested = requested.trim();
    if !requested.is_empty() {
        return Some(requested.to_string());
    }
    if tool_name == "use_meme" {
        return tools::memes::configured_meme_size(&config.plugins.memes);
    }
    tools::vision::configured_print_size(&config.plugins.print_image)
}

pub(in crate::cli) async fn render_remote_tool_image(
    state: &StateStore,
    event: &serde_json::Value,
    size: Option<String>,
) -> Result<()> {
    let asset_id = remote_tool_image_asset_id(event)
        .ok_or_else(|| anyhow::anyhow!("tool image event did not contain an asset id"))?;
    let asset = state
        .load_image_asset(asset_id)?
        .ok_or_else(|| anyhow::anyhow!("tool image asset is no longer available"))?;
    let preview = remote_image_preview(&asset)?;
    tools::vision::print_image_file(preview.path(), size).await
}

pub(in crate::cli) fn remote_tool_image_asset_id(event: &serde_json::Value) -> Option<&str> {
    event
        .get("asset")
        .and_then(|asset| asset.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

pub(in crate::cli) fn remote_image_preview(asset: &crate::state::ImageAssetData) -> Result<tempfile::NamedTempFile> {
    let suffix = if asset.asset.mime == "image/gif" {
        ".png"
    } else {
        match asset.asset.mime.as_str() {
            "image/jpeg" => ".jpg",
            "image/png" => ".png",
            "image/webp" => ".webp",
            "image/bmp" => ".bmp",
            _ => ".img",
        }
    };
    let mut preview = tempfile::Builder::new().suffix(suffix).tempfile()?;
    if asset.asset.mime == "image/gif" {
        image::load_from_memory(&asset.bytes)?
            .write_to(preview.as_file_mut(), image::ImageFormat::Png)?;
    } else {
        preview.write_all(&asset.bytes)?;
        preview.flush()?;
    }
    Ok(preview)
}

#[cfg(test)]
mod remote_tool_image_tests {
    use crate::cli::tool_cmds::*;
    use crate::ipc::Frame as IpcFrame;
    use image::{Delay, Frame, Rgba, RgbaImage};

    #[test]
    fn web_tool_image_event_exposes_asset_id_to_remote_cli() {
        let event = serde_json::json!({
            "run_id": "run-1",
            "name": "use_meme",
            "asset": { "id": "img-1", "mime": "image/gif" }
        });
        assert_eq!(remote_tool_image_asset_id(&event), Some("img-1"));
        assert_eq!(remote_tool_image_asset_id(&serde_json::json!({})), None);
    }

    #[test]
    fn ipc_command_response_distinguishes_errors_and_closed_connections() {
        assert!(validate_ipc_command_response(Some(IpcFrame::Ack)).is_ok());
        let rejected = validate_ipc_command_response(Some(IpcFrame::Error {
            code: None,
            message: "Miyu is busy with another operation".to_string(),
        }))
        .unwrap_err();
        assert!(rejected.to_string().contains("busy with another operation"));

        let closed = validate_ipc_command_response(None).unwrap_err();
        assert!(closed
            .to_string()
            .contains("closed the connection without a response"));

        let unexpected = validate_ipc_command_response(Some(IpcFrame::Accepted {
            turn_id: None,
            run_id: "run-test".to_string(),
        }))
        .unwrap_err();
        assert!(unexpected.to_string().contains("unexpected response"));
    }

    #[test]
    fn remote_gif_asset_is_converted_to_static_png() {
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder
                .encode_frames((0..2).map(|value| {
                    Frame::from_parts(
                        RgbaImage::from_pixel(32, 32, Rgba([value, 20, 40, 255])),
                        0,
                        0,
                        Delay::from_numer_denom_ms(100, 1),
                    )
                }))
                .unwrap();
        }
        let asset = crate::state::ImageAssetData {
            asset: crate::state::ImageAsset {
                asset_id: "img-gif".to_string(),
                turn_id: "turn-1".to_string(),
                tool_id: Some("tool-1".to_string()),
                mime: "image/gif".to_string(),
                width: 32,
                height: 32,
                alt: "animated meme".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            bytes: gif,
        };
        let preview = remote_image_preview(&asset).unwrap();
        let bytes = std::fs::read(preview.path()).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 32);
    }
}
