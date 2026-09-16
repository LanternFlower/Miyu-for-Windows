//! chafa 的调用约定：版本能力探测、参数组装、取证。
//!
//! 图片（`tools/vision/print.rs`）和公式（`render/math/raster.rs`）此前各写一
//! 套 chafa 参数，于是两边踩了同一个坑：**传了新版才有的选项，旧 chafa 认不
//! 出来就直接退出码 2，一张图都不出**。
//!
//! 时间线（实测，见 `testkit/chafa-compat`）：
//!
//! | 引入 | 选项 | chafa 版本 | 发布 |
//! |---|---|---|---|
//! | 08-18 | `--relative` | ≥1.14.0 | 2024-01-08 |
//! | 08-18 | `--probe`    | ≥1.16.0 | 2025-05-18 |
//! | 09-12 | `--probe-mode` | ≥**1.18.1** | **2026-02-08** |
//!
//! 09-12 那次把门槛抬到 1.18.1 之后，Debian 12/13、Ubuntu 22.04~25.10、
//! Fedora ≤41、openSUSE Leap、Alpine ≤3.23 的仓库版本**全部**在这之下——
//! 那些用户的 `print_image` 一张都出不来，只收到 "chafa exited with status 2"。
//! 开发机是 Arch（1.18.2），永远复现不了。
//!
//! 所以这里探测一次、组装一次，两条路共用。原则是**只传真正必要的选项**：
//!
//! - `--polite on`（≥1.10.0）：默认 **off**，不传的话 chafa 会发 `ESC[?25l`
//!   隐藏光标，REPL 里光标就此消失。这是唯一非传不可的。
//! - `--probe-mode ctty`（≥1.18.1）：**不传**。实测 1.18.2 的默认 `any` 本来
//!   就会 `openat("/dev/tty")`，输出字节与显式传它完全一致（16606 = 16606）。
//!   它买不到任何东西，只会把 1.18.1 以下全打死。
//! - `--relative off`（≥1.14.0）：**不传**。`off` 本来就是默认值。
//!
//! 1.16.0~1.18.0 这一档只有 stdio 探测（没有 `--probe-mode` 可以改走 ctty），
//! 而调用方把 chafa 的 stdin 接到 `/dev/null`，探测必然失败、退回字符画。
//! `stdin_should_be_tty()` 就是为这一档准备的。

use std::sync::OnceLock;

/// chafa 的版本与选项能力。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    /// 解析出的版本号；`chafa` 不存在或输出不认识时为 `None`。
    pub version: Option<(u32, u32, u32)>,
    /// `--polite`，1.10.0 起。
    pub polite: bool,
    /// `--relative`，1.14.0 起。
    pub relative: bool,
    /// `--probe`，1.16.0 起。有它才有"主动探测终端能力"这回事。
    pub probe: bool,
    /// `--probe-mode`，1.18.1 起。有它才能让探测走控制终端而不占 stdin。
    pub probe_mode: bool,
}

impl Capabilities {
    fn from_version(version: Option<(u32, u32, u32)>) -> Self {
        let at_least = |want: (u32, u32, u32)| version.is_some_and(|have| have >= want);
        Self {
            version,
            polite: at_least((1, 10, 0)),
            relative: at_least((1, 14, 0)),
            probe: at_least((1, 16, 0)),
            probe_mode: at_least((1, 18, 1)),
        }
    }

    /// chafa 装了没。
    pub fn present(&self) -> bool {
        self.version.is_some()
    }

    pub fn version_text(&self) -> String {
        match self.version {
            Some((major, minor, patch)) => format!("{major}.{minor}.{patch}"),
            None => "(未安装或版本无法识别)".to_string(),
        }
    }
}

/// 探测一次，全进程缓存。
///
/// 拿 `--version` 当靶子：它只验参数解析、不产生任何图像输出，一次进程启动
/// 就够。逐个选项试要起四次进程，没必要。
pub fn capabilities() -> &'static Capabilities {
    static CAPABILITIES: OnceLock<Capabilities> = OnceLock::new();
    CAPABILITIES.get_or_init(|| Capabilities::from_version(detect_version()))
}

fn detect_version() -> Option<(u32, u32, u32)> {
    let output = std::process::Command::new("chafa")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

/// 从 `chafa --version` 的输出里抠版本号。
///
/// 各版本第一行都是 `Chafa version 1.18.2` 这个形状（1.12.4 起实测一致）。
/// 抠不出来就当没有——宁可少传选项也不要传了它不认的。
pub(crate) fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let line = text.lines().next()?;
    let digits: Vec<&str> = line
        .split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()) && word.contains('.'))?
        .split('.')
        .collect();
    let number = |index: usize| -> u32 {
        digits
            .get(index)
            .map(|part| {
                part.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .and_then(|part| part.parse().ok())
            .unwrap_or(0)
    };
    if digits.is_empty() {
        return None;
    }
    Some((number(0), number(1), number(2)))
}

/// 图片路径的参数。
///
/// `--polite on` 抑制 chafa 的四个序列（实测同一张图的字节 diff）：
///
/// ```text
/// 默认：        ESC[?25l  ESC[?80l  ESC[?8452l  <sixel>  ESC[?25h
/// --polite on：                                <sixel>
/// ```
///
/// 前后两个是光标显隐，中间两个是 sixel 的模式位（`ESC[?80l` 要求 scrolling
/// 模式、`ESC[?8452l` 要求画完光标落到图下方）。查"图在 Konsole 里整张消失"
/// 时一度怀疑是抑制这两位惹的祸，去掉它验过——**无关**（真因是图放不进屏幕
/// 剩余空间、绘制中滚动，见 `print.rs` 的 `make_room_for_image`）。既然无关，
/// 就按原样留着：它本来的用处是别让 chafa 把 Miyu 自己管的光标藏起来。
pub fn direct_args() -> Vec<String> {
    // 调参口子：真终端上的图像问题只能在真终端上试，而每试一组参数就重编一次
    // 二进制要八分钟。`MIYU_CHAFA_ARGS="--probe off --format sixels"` 直接顶替。
    if let Ok(extra) = std::env::var("MIYU_CHAFA_ARGS") {
        return extra
            .split_whitespace()
            .map(str::to_string)
            .filter(|arg| !arg.is_empty())
            .collect();
    }
    captured_args().into_iter().map(str::to_string).collect()
}

/// 公式路径的参数：输出要被捕获、再嵌进 markdown 流里逐行写出去。
///
/// 那条路上光标显隐序列会混进正文、乱动光标，所以这里非 `--polite on` 不可；
/// 代价是 sixel 的两个模式位也一起没了——公式图只有 2~8 行、紧跟在文字后面，
/// 不像图片那样会触发滚动，暂时受得住。
pub fn captured_args() -> Vec<&'static str> {
    let mut args = Vec::new();
    if capabilities().polite {
        args.extend_from_slice(&["--polite", "on"]);
    }
    args
}

/// chafa 的 stdin 该不该接到控制终端。
///
/// 1.16.0~1.18.0 有主动探测（`--probe`）但没有 `--probe-mode`，探测只能走
/// stdio；调用方把 stdin 接 `/dev/null` 的话它必然探测不到，支持 sixel 的终端
/// 也会退回字符画（实测：1.16.2/1.18.0 在 stdin=null 下出 symbols，接上 tty
/// 后出 sixel）。
///
/// 1.18.1+ 的默认 `--probe-mode any` 自己会走 `/dev/tty`，不必占 stdin；
/// 1.16 以下根本没有主动探测，占了也没用。所以只有中间这一档需要。
///
/// 代价是探测窗口里 chafa 会和 REPL 抢按键——比"支持 sixel 却只给字符画"
/// 划算。
pub fn stdin_should_be_tty() -> bool {
    let caps = capabilities();
    caps.probe && !caps.probe_mode
}

/// 取证：`MIYU_IMAGE_TRACE=1` 时把这一次 chafa 调用的全貌写进日志。
///
/// 图片渲染的报障几乎全是"我这儿不对、开发机上好好的"，而差别藏在 chafa 版
/// 本、终端应答、选中的输出格式这三层里，光看屏幕分不出来。日志落在
/// `~/.miyu/cache/logs/image-trace.log`，与 kitty 那条路同一个文件。
pub fn trace_enabled() -> bool {
    std::env::var_os("MIYU_IMAGE_TRACE").is_some()
}

/// 从 chafa 的输出字节判断它最终选了哪种格式。
///
/// 报障时最想知道的就是这个：出真图还是退回了字符画。
pub fn detect_format(output: &[u8]) -> &'static str {
    let contains = |needle: &[u8]| output.windows(needle.len()).any(|window| window == needle);
    if contains(b"\x1b_G") {
        return "kitty";
    }
    if contains(b"\x1b]1337;File=") {
        return "iterm";
    }
    if output
        .windows(2)
        .enumerate()
        .any(|(index, window)| window == b"\x1bP" && starts_sixel(&output[index + 2..]))
    {
        return "sixel";
    }
    if contains(b"\x1b[48;2;") || contains(b"\x1b[48;5;") || contains("▀".as_bytes()) {
        return "symbols";
    }
    "?"
}

/// `ESC P` 之后是不是一段 sixel：参数段只有数字和分号，收在一个 `q` 上。
fn starts_sixel(tail: &[u8]) -> bool {
    let params = tail
        .iter()
        .take_while(|byte| byte.is_ascii_digit() || **byte == b';')
        .count();
    params < 16 && tail.get(params) == Some(&b'q')
}

pub fn trace(line: &str) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = std::path::Path::new(&home).join(".miyu/cache/logs/image-trace.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 落文件而不是 stderr：live REPL 里 stderr 会直接糊在画面上。
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(file, "{line}");
    }
}

/// 报障时想知道的终端身份，一行。
pub fn terminal_identity() -> String {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| "-".to_string());
    format!(
        "TERM={} COLORTERM={} TERM_PROGRAM={} KITTY={} raw={}",
        var("TERM"),
        var("COLORTERM"),
        var("TERM_PROGRAM"),
        if std::env::var_os("KITTY_WINDOW_ID").is_some() {
            "1"
        } else {
            "0"
        },
        crossterm::terminal::is_raw_mode_enabled().unwrap_or(false),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parses_the_shape_every_release_prints() {
        // 1.12.4 到 1.18.2 实测都是这一行。
        assert_eq!(
            parse_version("Chafa version 1.18.2\n\nLoaders: PNG\n"),
            Some((1, 18, 2))
        );
        assert_eq!(parse_version("Chafa version 1.12.4\n"), Some((1, 12, 4)));
        // 发行版打的补丁号后缀不该干扰判定。
        assert_eq!(parse_version("Chafa version 1.14.5-1\n"), Some((1, 14, 5)));
        // 两段式也得有个说法，别 panic。
        assert_eq!(parse_version("Chafa version 1.18\n"), Some((1, 18, 0)));
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn capabilities_follow_the_version_each_option_landed_in() {
        let caps = Capabilities::from_version(Some((1, 18, 2)));
        assert!(caps.polite && caps.relative && caps.probe && caps.probe_mode);

        // 1.18.0 差一个补丁版本就没有 --probe-mode——09-12 的回归正在这里。
        let caps = Capabilities::from_version(Some((1, 18, 0)));
        assert!(caps.probe && !caps.probe_mode);

        // Debian 13 / Ubuntu 24.04+ / Fedora 41 的仓库版本。
        let caps = Capabilities::from_version(Some((1, 14, 5)));
        assert!(caps.polite && caps.relative && !caps.probe && !caps.probe_mode);

        // Debian 12 / openSUSE Leap。
        let caps = Capabilities::from_version(Some((1, 12, 4)));
        assert!(caps.polite && !caps.relative);

        // Ubuntu 22.04 LTS 连 --polite 都没有。
        let caps = Capabilities::from_version(Some((1, 8, 0)));
        assert!(!caps.polite && !caps.relative && !caps.probe);

        // 认不出版本就一个选项都不传。
        let caps = Capabilities::from_version(None);
        assert!(!caps.polite && !caps.relative && !caps.probe && !caps.probe_mode);
        assert!(!caps.present());
    }

    #[test]
    fn only_the_middle_band_needs_stdin_on_the_tty() {
        let band = |version: (u32, u32, u32)| {
            let caps = Capabilities::from_version(Some(version));
            caps.probe && !caps.probe_mode
        };
        assert!(!band((1, 14, 5)), "没有主动探测，占了 stdin 也没用");
        assert!(band((1, 16, 2)), "只有 stdio 探测，必须占 stdin");
        assert!(band((1, 18, 0)), "同上");
        assert!(!band((1, 18, 1)), "默认就走 ctty，不该占 stdin");
    }

    #[test]
    fn format_detection_tells_a_real_image_from_a_fallback() {
        assert_eq!(detect_format(b"\x1b_Ga=T,f=32;AAAA\x1b\\"), "kitty");
        assert_eq!(detect_format(b"\x1bP0;1;0q#0;2;0;0;0"), "sixel");
        assert_eq!(detect_format(b"\x1bPq#0;2;0;0;0"), "sixel");
        assert_eq!(detect_format(b"\x1b]1337;File=inline=1:AAAA\x07"), "iterm");
        assert_eq!(detect_format("\x1b[48;2;1;2;3m▀".as_bytes()), "symbols");
        assert_eq!(detect_format(b"plain"), "?");
    }
}
