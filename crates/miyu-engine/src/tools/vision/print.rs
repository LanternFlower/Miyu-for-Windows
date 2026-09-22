//! 把图片打到终端上。
//!
//! 尺寸按终端宽高的百分比算（`configured_print_size`），模型可以覆盖但要限幅
//! （`requested_print_size`）——一张占满整屏的图会把上下文全推走。

use crate::tools::vision::*;

pub fn register_print(registry: &mut ToolRegistry, config: AppConfig) {
    if !config.plugins.print_image.enabled {
        return;
    }
    registry.register(ToolSpec::new_with_progress(
        "print_image",
        "Print/render local images directly in the current terminal output, several at once. Use this when the user asks to show, print, render, or preview images, or when you need to look at them yourself before answering.",
        json!({
            "type": "object",
            "properties": {
                "image": { "type": "array", "items": { "type": "string" }, "description": "Local image paths, in display order." },
                "size": { "type": "string", "description": "Optional chafa size, e.g. 80x40. Use this or width/height to avoid oversized output." },
                "width": { "type": "integer", "description": "Optional output width in terminal cells, e.g. 80." },
                "height": { "type": "integer", "description": "Optional output height in terminal cells, e.g. 40." }
            },
            "required": ["image"],
            "additionalProperties": false
        }),
        move |args, progress| {
            let print_config = config.plugins.print_image.clone();
            async move { print_image(args, &print_config, progress).await }
        },
    ));
}

/// 参数里的图片路径清单。
///
/// 声明是字符串数组,但也收单个字符串和「数组被写成 JSON 字符串」两种形态——
/// 和 `send_message_to_user.images` / `generate_image.reference_images` 同款
/// 宽松解析,真机上模型确实会写成那样。
fn requested_paths(args: &Value) -> Result<Vec<String>> {
    let raw = match args.get("image") {
        Some(Value::Array(values)) => values.clone(),
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(values)) => values,
            // 就是一个裸路径。
            _ => vec![Value::String(text.clone())],
        },
        None | Some(Value::Null) => bail!("{}", "image is required"),
        Some(_) => bail!("{}", "image must be a path or a list of paths"),
    };
    let paths = raw
        .iter()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        bail!("{}", "image is required")
    }
    Ok(paths)
}

pub(crate) async fn print_image(
    args: Value,
    print_config: &PrintImagePluginConfig,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let requested = requested_paths(&args)?;
    // 路径先全部校验一遍再动手:打了一半才发现第三张不存在,屏幕上会留下
    // 半截结果,而模型收到的是一个 Err,它看不出前两张已经打出去了。
    let mut paths = Vec::with_capacity(requested.len());
    let mut errors = Vec::new();
    for image in &requested {
        let path = expand_path(image);
        match miyu_base::sandbox::guard_read(&path).and_then(|()| {
            let metadata = std::fs::metadata(&path)
                .with_context(|| format!("{} {}", "failed to stat image", path.display()))?;
            if metadata.is_file() {
                Ok(())
            } else {
                bail!("{}: {}", "image path is not a file", path.display())
            }
        }) {
            Ok(()) => paths.push(path),
            // 一张坏路径不掀翻整批:剩下的照打,坏的那张单独报给模型。
            Err(error) => errors.push(format!("{image}: {error}")),
        }
    }
    if paths.is_empty() {
        bail!("{}", errors.join("; "))
    }
    // 模型显式要的尺寸要随事件带走:daemon 模式下真正画图的是终端那一侧,
    // 这里 print_image_file 的参数它看不见。
    let size = requested_print_size(&args);
    for path in &paths {
        progress.report_sized_image(
            path.clone(),
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("image"),
            size.clone(),
        );
    }
    // 活动区只冻一次,整批打完再放开——每张各冻一次会在图之间反复 resume。
    let printed = if progress.prepare_for_external_output().await {
        let size = print_size(&args, print_config);
        for path in &paths {
            if let Err(error) = print_image_file(path, size.clone()).await {
                errors.push(format!("{}: {error}", path.display()));
            }
        }
        true
    } else {
        false
    };
    let listed = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mut report = if printed {
        format!("printed {} image(s) in terminal: {listed}", paths.len())
    } else {
        // 无终端会话(WebUI/桥/平台)里谎报 "printed in terminal" 会让模型
        // 以为用户看到了图(08-21 生图 bug 的帮凶);说真话:图已 emit,
        // 由宿主决定怎么展示。
        format!(
            "{} image(s) emitted to the host for display: {listed}",
            paths.len()
        )
    };
    if !errors.is_empty() {
        report.push_str(&format!("; failed: {}", errors.join("; ")));
    }
    Ok(report)
}

/// 图片在**全屏**下怎么出：返回 `(发给终端的, 进缓冲的)`。
///
/// 直接打屏的话下一帧重画就把它抹了——全屏的正文是从缓冲重建的，不在缓冲里的
/// 东西不存在。kitty 走占位格，其余走 chafa 的字符画（本来就是文字）。
pub async fn image_parts_for_buffer(path: &Path, size: Option<String>) -> Result<(String, String)> {
    if miyu_base::terminal::kitty::is_native_kitty_terminal()
        && miyu_base::terminal::kitty::supports_path(path)
    {
        return miyu_base::terminal::kitty::split_for_buffer(path, size.as_deref());
    }
    let mut command = Command::new("chafa");
    command.args(["--probe", "off", "--relative", "off"]);
    if let Some(size) = size {
        command.arg("--size").arg(size);
    }
    command.kill_on_drop(true);
    let output = command
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .with_context(|| "failed to run chafa; install chafa or disable terminal image printing")?;
    if !output.status.success() {
        bail!("chafa exited with status {}", output.status);
    }
    // chafa 吐的是 `\n` 断行；缓冲按终端语义走，要 `\r\n` 才回到行首。
    //
    // 藏/显光标那两条序列顺手摘掉：进缓冲这一路光标归 TUI 管，而末尾那条
    // `?25h` 落在最后一行的换行**之后**，于是自己占掉一行——图下面凭空多
    // 一行空（09-19）。
    let art = String::from_utf8_lossy(&output.stdout)
        .replace("\x1b[?25l", "")
        .replace("\x1b[?25h", "")
        .replace('\n', "\r\n");
    Ok((String::new(), art))
}

pub async fn print_image_file(path: &Path, size: Option<String>) -> Result<()> {
    // 不再自带前导空行:所有调用方都紧跟 prepare_for_external_output,
    // 摘要冻结(write_activity_summary)已留了一个空行,这里再空一行就是
    // 图片上方空两行(用户 08-20 实测点名)。
    if crossterm::terminal::is_raw_mode_enabled().unwrap_or(false)
        && miyu_base::terminal::kitty::is_native_kitty_terminal()
        && miyu_base::terminal::kitty::supports_path(path)
    {
        miyu_base::terminal::kitty::print(path, size.as_deref())?;
        println!();
        io::stdout().flush()?;
        return Ok(());
    }
    run_chafa(path, size).await
}

/// 打图之前把它需要的空间先腾出来。
///
/// sixel 是整图一个 blob：终端画到屏幕底部会自己滚动，而 **Konsole 在绘制过程
/// 中滚动会把整张图丢掉**。实测（屏幕 67 行）：
///
/// | 打图前光标 | 剩余 | 图高 | 结果 |
/// |---|---|---|---|
/// | 第 42 行 | 24 行 | 23 行 | 出图 |
/// | 第 61 行 | 5 行 | 7 / 12 / 23 行 | **全都没出** |
///
/// 判据是"放不放得下"，不是图有多高——REPL 聊过几轮之后光标本来就贴着屏幕
/// 底部，于是"用着用着图就不出了"。而单独一张能出（屏幕空）、手动在 shell 里
/// 跑一直正常（光标位置随便），全是同一件事的不同侧面。
///
/// 所以在图之前先滚够：那时屏幕上还没有图，怎么滚都安全。滚完光标跟着上移，
/// 图正好画到屏幕底部为止。
fn make_room_for_image(rows_needed: u16) -> Result<()> {
    use crossterm::{cursor::MoveTo, QueueableCommand as _};

    let (_, terminal_rows) = crossterm::terminal::size()?;
    let last_row = terminal_rows.saturating_sub(1);
    let (_, row) = crossterm::cursor::position()?;
    let overflow = row.saturating_add(rows_needed).saturating_sub(last_row);
    if overflow == 0 {
        return Ok(());
    }
    let mut stdout = io::stdout();
    stdout.queue(MoveTo(0, last_row))?;
    for _ in 0..overflow {
        write!(stdout, "\n")?;
    }
    // 内容整体上移了 overflow 行，光标回到原来那一行现在所在的位置。
    stdout.queue(MoveTo(0, row.saturating_sub(overflow)))?;
    stdout.flush()?;
    Ok(())
}

/// 把 `--size` 收窄到图片自己的大小：只缩不放。
///
/// chafa 的 `--size` 是"最大尺寸"，**小图会被放大填满这个框**——一张 128×128
/// 的表情包在 60x30 的框里被撑成 540×540px（4.2 倍），既糊，又平白占掉 27 行
/// 而不是 7 行。kitty 那条路早就写着"只缩不放：放大不会凭空长出细节，纯粹是
/// 拿显存和串口带宽换零收益"，这里补上同一条规矩。
///
/// 只读图片头拿尺寸（`into_dimensions` 不解码像素）。拿不到就原样返回——宁可
/// 按老样子画，也不要因为一张认不出的图就不显示。
fn shrink_only(path: &Path, size: Option<String>) -> Option<String> {
    let size = size?;
    let (want_cols, want_rows) = size.split_once('x')?;
    let (want_cols, want_rows): (u32, u32) = (want_cols.parse().ok()?, want_rows.parse().ok()?);
    let (width, height) = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let (cell_width, cell_height) = miyu_base::terminal::kitty::cell_pixel_size();
    let natural_cols = width.div_ceil(u32::from(cell_width.max(1))).max(1);
    let natural_rows = height.div_ceil(u32::from(cell_height.max(1))).max(1);
    Some(format!(
        "{}x{}",
        want_cols.min(natural_cols),
        want_rows.min(natural_rows)
    ))
}

/// 把图交给 chafa 画。
///
/// 参数由 `terminal::chafa` 按**探测到的 chafa 版本**组装——此前这里写死了
/// `--probe-mode ctty --polite on --relative off`，其中 `--probe-mode` 要
/// chafa ≥1.18.1（2026-02-08），而 Debian 12/13、Ubuntu 22.04~25.10、
/// Fedora ≤41、openSUSE Leap、Alpine ≤3.23 的仓库版本全在这之下：它们收到
/// 一个不认识的选项就退出码 2，**一张图都不出**。开发机是 Arch，永远复现不了。
async fn run_chafa(path: &Path, size: Option<String>) -> Result<()> {
    use tokio::io::AsyncReadExt as _;

    let caps = miyu_base::terminal::chafa::capabilities();
    if !caps.present() {
        bail!(
            "{}",
            t(
                "chafa is not installed; install it or turn off terminal image printing",
                "没有安装 chafa:装上它,或在设置里关掉终端图片显示"
            )
        )
    }
    let args = miyu_base::terminal::chafa::direct_args();
    let size = shrink_only(path, size);
    // 只在 REPL（raw 模式）里腾：那才是活动区贴着屏幕底部、光标常年在下面的
    // 形态；一次性回合的终端是 cooked，输出本来就自然往下走。
    if crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
        if let Some(rows) = size
            .as_deref()
            .and_then(|size| size.split_once('x'))
            .and_then(|(_, rows)| rows.parse::<u16>().ok())
        {
            // 图之外再留一行：chafa 画完自己还会发一个换行。
            let _ = make_room_for_image(rows.saturating_add(1));
        }
    }
    let mut command = Command::new("chafa");
    command.args(&args);
    // 从 kitty 里启动别的终端时，`KITTY_WINDOW_ID` 这些会**被子进程继承**，
    // chafa 据此认定自己在 kitty 里、输出 kitty 图形协议——而真正在跑的那个
    // 终端（ptyxis、foot、alacritty…）根本不认，图就此消失（ptyxis 实测：
    // `KITTY=1` + `format=kitty`，三张全丢）。当前终端的真身份写在 `TERM` 里，
    // 它不是 xterm-kitty 就把这些痕迹摘掉，别让 chafa 被上一层终端骗了。
    if !miyu_base::terminal::kitty::is_native_kitty_terminal() {
        for leftover in [
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "KITTY_INSTALLATION_DIR",
            "KITTY_PUBLIC_KEY",
            "KITTY_LISTEN_ON",
        ] {
            command.env_remove(leftover);
        }
    }
    if let Some(size) = &size {
        command.arg("--size").arg(size);
    }
    command.arg(path);

    // 1.16.0~1.18.0 有主动探测却没有 `--probe-mode`,探测只能走 stdio——stdin
    // 接 /dev/null 的话,支持 sixel 的终端也只会拿到字符画。其余版本不必占。
    let stdin = if miyu_base::terminal::chafa::stdin_should_be_tty() {
        std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null())
    } else {
        Stdio::null()
    };
    // 取证模式下把 stdout 也收下来,好判定 chafa 最终选了哪种格式;正常路径
    // 直接 inherit,零拷贝零开销。stderr 一律收下:chafa 的抱怨要进错误信息,
    // 而不是糊在 REPL 画面上。
    let tracing = miyu_base::terminal::chafa::trace_enabled();
    let started = std::time::Instant::now();
    let mut child = command
        .stdin(stdin)
        .stdout(if tracing {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| "failed to run chafa; install chafa or disable terminal image printing")?;

    // 两条管子并发抽干:串行读的话,chafa 若在我们还没读到的那一条上写满
    // 管道缓冲就再也不会退出。
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let (mut captured, mut complaint) = (Vec::new(), Vec::new());
    tokio::join!(
        async {
            if let Some(pipe) = stdout_pipe.as_mut() {
                let _ = pipe.read_to_end(&mut captured).await;
            }
        },
        async {
            if let Some(pipe) = stderr_pipe.as_mut() {
                let _ = pipe.read_to_end(&mut complaint).await;
            }
        },
    );
    let complaint = String::from_utf8_lossy(&complaint).into_owned();
    let status = child.wait().await.context("failed to wait for chafa")?;
    // chafa 探测终端能力时会改控制终端的 termios。它被 kill_on_drop 杀在中途
    // (回合取消、超时)就不会恢复现场,而 ONLCR 一旦丢了,往后整个 REPL 的输出
    // 都沿对角线错位。补一次,幂等。只在 raw 模式下补:非 REPL 形态终端本来
    // 就是 cooked,不该去动用户的 termios。
    if crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
        let _ = miyu_base::terminal::restore_output_processing();
    }

    if tracing {
        io::stdout().write_all(&captured)?;
        miyu_base::terminal::chafa::trace(&format!(
            "chafa {ver} {identity} args={args:?} size={size:?} stdin={stdin_kind} \
             -> {status} {elapsed}ms format={format} bytes={bytes} {complaint}",
            ver = caps.version_text(),
            identity = miyu_base::terminal::chafa::terminal_identity(),
            stdin_kind = if miyu_base::terminal::chafa::stdin_should_be_tty() {
                "tty"
            } else {
                "null"
            },
            elapsed = started.elapsed().as_millis(),
            format = miyu_base::terminal::chafa::detect_format(&captured),
            bytes = captured.len(),
            complaint = complaint.trim(),
        ));
    }

    if !status.success() {
        // 光说 "exited with status 2" 没人查得下去。版本和参数都带上:这个
        // 失败十有八九就是"这个版本不认识这个选项"。
        bail!(
            "chafa {} {} ({}): {} {}",
            caps.version_text(),
            t("failed", "调用失败"),
            status,
            complaint.trim(),
            t(
                "— run the same command by hand to compare",
                "— 可以手动跑同样的命令对照"
            )
        )
    }
    println!();
    io::stdout().flush()?;
    Ok(())
}

pub fn configured_print_size(print_config: &PrintImagePluginConfig) -> Option<String> {
    let (cols, rows) = display_grid()?;
    let width = ((cols as u32 * print_config.width_percent as u32) / 100).max(1);
    let height = ((rows as u32 * print_config.height_percent as u32) / 100).max(1);
    Some(format!("{}x{}", width.min(300), height.min(200)))
}

#[cfg(not(any(test, feature = "testkit")))]
fn display_grid() -> Option<(u16, u16)> {
    if let Some(viewport) = miyu_base::terminal::content_viewport() {
        return Some(viewport);
    }
    crossterm::terminal::size().ok()
}

/// 模型显式要的尺寸，没要就是 None。
///
/// 和 `configured_print_size` 分开是因为两者只能在不同的地方解析：百分比
/// 依赖 `crossterm::terminal::size()`，daemon 量到的不是用户的终端，只能
/// 在 CLI 那侧算；而显式值只有 daemon 手里的工具参数才知道，必须随事件带
/// 过去，否则模型写了 width 也会被无声吃掉。
pub fn requested_print_size(args: &Value) -> Option<String> {
    let width = args
        .get("width")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(300);
    let height = args
        .get("height")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(200);
    match (width, height) {
        (0, 0) => args
            .get("size")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        (width, 0) => Some(format!("{width}x")),
        (0, height) => Some(format!("x{height}")),
        (width, height) => Some(format!("{width}x{height}")),
    }
}

pub(crate) fn print_size(args: &Value, print_config: &PrintImagePluginConfig) -> Option<String> {
    requested_print_size(args).or_else(|| configured_print_size(print_config))
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    fn png(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        path
    }

    async fn run(args: Value) -> Result<String> {
        print_image(
            args,
            &PrintImagePluginConfig::default(),
            crate::tools::ToolProgress::default(),
        )
        .await
    }

    /// 用户 09-22 拍板改成批量。声明是字符串数组,但裸字符串和「数组被写成
    /// JSON 字符串」两种形态也要收——真机上模型会那么写(与
    /// `send_message_to_user.images` 同款宽松解析)。
    #[tokio::test]
    async fn a_batch_accepts_a_list_a_bare_path_and_a_stringified_list() {
        let temp = tempfile::tempdir().unwrap();
        let one = png(temp.path(), "one.png");
        let two = png(temp.path(), "two.png");
        let (one, two) = (one.display().to_string(), two.display().to_string());

        let listed = run(json!({ "image": [one.clone(), two.clone()] }))
            .await
            .expect("数组该收");
        assert!(listed.contains("2 image(s)"), "{listed}");
        assert!(listed.contains(&one) && listed.contains(&two), "{listed}");

        let bare = run(json!({ "image": one.clone() }))
            .await
            .expect("裸路径该收");
        assert!(bare.contains("1 image(s)"), "{bare}");

        let stringified = run(json!({ "image": format!("[{:?},{:?}]", one, two) }))
            .await
            .expect("JSON 字符串数组该收");
        assert!(stringified.contains("2 image(s)"), "{stringified}");
    }

    /// 一张坏路径不掀翻整批:好的照打,坏的单独报。全坏才是 Err。
    #[tokio::test]
    async fn one_bad_path_does_not_sink_the_batch() {
        let temp = tempfile::tempdir().unwrap();
        let good = png(temp.path(), "good.png").display().to_string();
        let missing = temp.path().join("nope.png").display().to_string();

        let mixed = run(json!({ "image": [good.clone(), missing.clone()] }))
            .await
            .expect("还有能打的就不该整批失败");
        assert!(mixed.contains("1 image(s)"), "{mixed}");
        assert!(mixed.contains("failed:"), "坏的那张没报出来:{mixed}");

        let all_bad = run(json!({ "image": [missing] })).await;
        assert!(all_bad.is_err(), "一张能打的都没有该是 Err");
    }

    /// 不设上限(用户 09-22 拍板):一次给多少就打多少。
    ///
    /// 总量本来也拦不住——她可以接着再调一次,封顶只是个能绕过去的减速带。
    #[tokio::test]
    async fn a_long_batch_is_not_capped() {
        let temp = tempfile::tempdir().unwrap();
        let paths = (0..12)
            .map(|i| png(temp.path(), &format!("p{i}.png")).display().to_string())
            .collect::<Vec<_>>();
        let long = run(json!({ "image": paths, "size": "4x2" }))
            .await
            .expect("给多少打多少,不该有封顶报错");
        assert!(long.contains("12 image(s)"), "{long}");
    }

    /// 空与缺失都要有明确报错,别静默打 0 张。
    #[tokio::test]
    async fn an_empty_request_is_an_error() {
        assert!(run(json!({})).await.is_err());
        assert!(run(json!({ "image": [] })).await.is_err());
        assert!(run(json!({ "image": "   " })).await.is_err());
        assert!(run(json!({ "image": 7 })).await.is_err());
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
