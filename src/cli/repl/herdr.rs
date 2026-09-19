//! 往 herdr 上报这条 REPL 的状态。
//!
//! [herdr](https://herdr.dev/) 是个「知道 agent 在干嘛」的终端多路复用器：每个
//! pane 的 agent 状态（空闲 / 在跑 / 卡住等人）汇总到侧栏，往 tab 和 workspace
//! 上卷。它给外部 agent 留了一条官方接口——`herdr pane report-agent`——`--agent`
//! 是**自由字符串**，不在它二进制里硬编码的那二十来个 kind 之列也能用（官方文档
//! 拿 `docs-bot` 做的示例）。走这条路 Miyu 不需要 herdr 改一行代码。
//!
//! 上报之后能拿到：侧栏出现 `miyu` 行与状态色、她反问时整条 workspace 变红、
//! herdr 自己的桌面通知，以及 `herdr agent attach/wait/prompt` 三条命令。
//!
//! **不在 herdr 里就是彻底的 no-op**：判据是 herdr 自己注入 pane 进程的那几个
//! 环境变量，缺一个都不做事，连进程都不起。
//!
//! 几条来自官方文档、直接影响正确性的边界：
//!
//! - `--source` 要稳定唯一（我们固定 `custom:miyu`）。一个 pane 的生命周期内
//!   最多接受 32 个不同 source 的上报，释放也不回收名额，所以不能每轮换一个。
//! - `--seq` 必须**严格单调递增**，herdr 会丢掉同一 source 的过期序号。计数器挂
//!   在**进程**上：daemon 每回合新建 Agent（见记忆 `miyu-per-turn-agent-latches`），
//!   挂在回合上会归零。
//! - 退出要 `release-agent`，否则那个 pane 的权威一直挂着我们的名字。
//! - 上报是起外部进程，**必须异步且失败静默**：它不能拖慢回合，更不能把错误糊到
//!   画面上。

use std::sync::atomic::{AtomicU64, Ordering};

/// 我们在 herdr 那边的身份。`source` 是权威标识，`agent` 是侧栏上显示的字。
const SOURCE: &str = "custom:miyu";
const AGENT: &str = "miyu";

/// 上报的状态。herdr 的 `--state` 只认这四个；侧栏上那个「done」是它自己派生的
/// （idle 且用户还没看过），不由我们报。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::cli) enum HerdrState {
    /// 等人打字。
    Idle,
    /// 在想、在跑工具。
    Working,
    /// **卡住等人回话**（她反问了一句）。侧栏会把整条 tab / workspace 标红，
    /// 这是这套上报里最值钱的一个状态。
    Blocked,
}

impl HerdrState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
        }
    }
}

/// 这个 pane 在 herdr 里的坐标。`None` = 不在 herdr 里跑，什么都不做。
struct Pane {
    binary: String,
    pane_id: String,
}

fn pane() -> Option<Pane> {
    pane_from(
        std::env::var("HERDR_ENV").ok().as_deref(),
        std::env::var("HERDR_PANE_ID").ok().as_deref(),
        std::env::var("HERDR_BIN_PATH").ok().as_deref(),
    )
}

/// 判定本身单独一支，好让测试直接喂输入——环境变量是进程全局的，测试里改它
/// 会和别的用例打架。
fn pane_from(env: Option<&str>, pane_id: Option<&str>, binary: Option<&str>) -> Option<Pane> {
    // `HERDR_ENV=1` 是 herdr 给 pane 进程盖的章；另外两个缺一不可。
    if env != Some("1") {
        return None;
    }
    let pane_id = pane_id.filter(|id| !id.is_empty())?.to_string();
    // 用它注入的绝对路径而不是裸 `herdr`：不受 PATH 影响，也不会撞上同名脚本。
    let binary = binary.filter(|path| !path.is_empty())?.to_string();
    Some(Pane { binary, pane_id })
}

/// 序号发号器。
static SEQ: AtomicU64 = AtomicU64::new(1);

/// 下一个序号。**基准是进程启动时的毫秒时间戳**。
///
/// herdr 按 `source` 记序号水位并**丢掉过期序号**，而我们的 source 是固定的
/// `custom:miyu`——每个进程从 1 重新数的话，第二次开 Miyu 报的 1/2/3 全都小于
/// herdr 已经见过的水位，**整个进程的上报被静默丢光**（用户 09-20 实测：重开
/// 一次之后侧栏彻底不显示了；第一次能显示是因为那时还没有水位）。
///
/// 所以序号要**跨进程**单调，不只是进程内单调。时间戳天然满足，也不用落盘。
/// （回合内仍要挂进程而不是挂 Agent：daemon 每回合新建 Agent，见记忆
/// `miyu-per-turn-agent-latches`。）
fn next_seq() -> u64 {
    static BASE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let base = *BASE.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or(0)
    });
    base.saturating_add(SEQ.fetch_add(1, Ordering::Relaxed))
}

/// 报一次状态。不在 herdr 里就直接返回。
///
/// `session_id` 是 Miyu 这条会话的 id，交给 herdr 当 `agent_session_id`——侧栏和
/// `agent list` 会带上它，将来做「herdr 重启后恢复」时也是靠它指回来。
pub(in crate::cli) fn report(state: HerdrState, message: Option<&str>, session_id: Option<&str>) {
    let Some(pane) = pane() else {
        return;
    };
    let mut args = vec![
        "pane".to_string(),
        "report-agent".to_string(),
        pane.pane_id,
        "--source".to_string(),
        SOURCE.to_string(),
        "--agent".to_string(),
        AGENT.to_string(),
        "--state".to_string(),
        state.as_str().to_string(),
        "--seq".to_string(),
        next_seq().to_string(),
    ];
    if let Some(message) = message.map(str::trim).filter(|text| !text.is_empty()) {
        // 题干可能很长（她的问题是整段话），侧栏只显示一行，这里先截短：
        // 传一大段过去纯属浪费，herdr 那边也要自己裁。
        let short = message.chars().take(120).collect::<String>();
        args.push("--message".to_string());
        args.push(short);
    }
    if let Some(session_id) = session_id.filter(|id| !id.is_empty()) {
        args.push("--agent-session-id".to_string());
        args.push(session_id.to_string());
    }
    spawn(&pane.binary, args);
}

/// 退出时把这个 pane 的权威还回去。
///
/// 这一处**等它跑完**再返回：别处的上报是丢出去就不管（不能拖慢回合），但退出
/// 那一刻进程马上就没了，丢出去的子进程会跟着被收走，侧栏上就一直挂着一个已经
/// 不存在的 miyu。等一下最多几十毫秒，而且人已经在退出了。
pub(in crate::cli) fn release_blocking() {
    let Some(pane) = pane() else {
        return;
    };
    use std::process::{Command, Stdio};
    // **释放也要带序号**。herdr 按 source 记序号水位、丢掉排不进序的上报——
    // 一串带序号的状态之后跟一条不带序号的释放，它不认，于是侧栏上那行
    // 一直留着（用户 09-20：退出了 TUI，侧栏还在）。
    let seq = next_seq().to_string();
    let _ = Command::new(&pane.binary)
        .args([
            "pane",
            "release-agent",
            &pane.pane_id,
            "--source",
            SOURCE,
            "--agent",
            AGENT,
            "--seq",
            &seq,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 侧栏上那几个自定义字段（模型、上下文占用、人格）。
///
/// herdr 的 `ui.sidebar.agents.rows` 可以写 `$model` 这种 token 来显示它们——
/// 这是 Claude Code 在 herdr 里都没有的东西，而我们本来就有这些数。
pub(in crate::cli) fn report_metadata(tokens: &[(&str, String)]) {
    let Some(pane) = pane() else {
        return;
    };
    let tokens: Vec<_> = tokens
        .iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .collect();
    if tokens.is_empty() {
        return;
    }
    let mut args = vec![
        "pane".to_string(),
        "report-metadata".to_string(),
        pane.pane_id,
        "--source".to_string(),
        SOURCE.to_string(),
        "--agent".to_string(),
        AGENT.to_string(),
    ];
    for (key, value) in tokens {
        args.push("--token".to_string());
        args.push(format!("{key}={}", value.trim()));
    }
    // 同一个 source 共用一本序号账：不带的话这条同样可能被当成排不进序而丢掉。
    args.push("--seq".to_string());
    args.push(next_seq().to_string());
    spawn(&pane.binary, args);
}

/// 起进程、不等、不看结果。
///
/// 失败静默是硬要求：herdr 可能正在重启、socket 可能没了、二进制可能被换掉——
/// 这些都不该让用户的回合慢一拍，更不该在画面上冒出一行错误。
fn spawn(binary: &str, args: Vec<String>) {
    use std::process::{Command, Stdio};
    let _ = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|mut child| {
            // 不 wait 会留一地僵尸（REPL 是长驻进程）。另起一根线程收尸，
            // 主线程一步都不等。
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不在 herdr 里就彻底不做事——连 `HERDR_BIN_PATH` 都不看。
    ///
    /// 这条是这个模块的安全底线：绝大多数用户不跑 herdr，这段代码对他们必须是
    /// 零存在感。
    #[test]
    fn outside_herdr_everything_is_a_no_op() {
        let cases = [
            (None, Some("w4:pP"), Some("/usr/bin/herdr"), false),
            (Some("1"), None, Some("/usr/bin/herdr"), false),
            (Some("1"), Some("w4:pP"), None, false),
            (Some("0"), Some("w4:pP"), Some("/usr/bin/herdr"), false),
            (Some(""), Some("w4:pP"), Some("/usr/bin/herdr"), false),
            (Some("1"), Some(""), Some("/usr/bin/herdr"), false),
            (Some("1"), Some("w4:pP"), Some(""), false),
            (Some("1"), Some("w4:pP"), Some("/usr/bin/herdr"), true),
        ];
        for (env, pane_id, binary, want) in cases {
            assert_eq!(
                pane_from(env, pane_id, binary).is_some(),
                want,
                "判定错了: {env:?} {pane_id:?} {binary:?}"
            );
        }
    }

    #[test]
    fn sequence_numbers_only_go_up_and_never_restart_from_one() {
        let first = next_seq();
        let second = next_seq();
        let third = next_seq();
        assert!(first < second && second < third, "序号必须严格递增");
        // 跨进程也不能回头：新进程从 1 数起的话，herdr 会把它的上报全当过期
        // 丢掉（用户 09-20 实测）。基准是毫秒时间戳，所以必然远大于小计数。
        assert!(
            first > 1_700_000_000_000,
            "序号基准不是时间戳，重开一次就会被 herdr 丢光：{first}"
        );
    }

    #[test]
    fn states_use_the_names_herdr_accepts() {
        // herdr 的 `--state` 只认这三个（外加 unknown）；`done` 是它自己派生的。
        assert_eq!(HerdrState::Idle.as_str(), "idle");
        assert_eq!(HerdrState::Working.as_str(), "working");
        assert_eq!(HerdrState::Blocked.as_str(), "blocked");
    }
}

/// 把当前会话名写进终端标题（OSC 2 / OSC 0）。
///
/// Miyu 一直不打终端标题——全仓找不到一处 OSC 0/2。后果有两处：herdr 侧栏的
/// `terminal_title` token 对 Miyu 恒空（Claude Code 打了，所以它那行显示得出
/// 当前任务名）；kitty 的标签页也只显示 shell 名。
///
/// 这一条和 herdr **无关**，放在这个模块只是因为同一项里一起做的；不在 herdr
/// 里也照打。
pub(in crate::cli) fn set_terminal_title(title: &str) {
    use std::io::Write as _;
    let title = title.trim();
    // 标题里不能有控制字符：OSC 串到 BEL 为止，混进去会把序列截断，剩下的字
    // 直接糊在屏幕上。
    let safe: String = title
        .chars()
        .filter(|ch| !ch.is_control())
        .take(120)
        .collect();
    let safe = if safe.is_empty() {
        "Miyu".to_string()
    } else {
        format!("Miyu · {safe}")
    };
    let mut stdout = std::io::stdout();
    // OSC 2 = 窗口标题，OSC 1 = 图标名（标签页多半看这个）。两条都发，各终端
    // 取用的那条不一样。
    let _ = write!(stdout, "\x1b]2;{safe}\x07\x1b]1;{safe}\x07");
    let _ = stdout.flush();
}

/// 按会话名设终端标题。会话是**首条消息之后**才自动命名的，所以回合收尾时要
/// 再设一次——不然标题永远停在刚开那会儿的空名上。
pub(in crate::cli) fn set_terminal_title_for_session(
    paths: &miyu_base::paths::MiyuPaths,
    session_id: &str,
) {
    let name = miyu_core::state::StateStore::new(paths)
        .ok()
        .and_then(|store| store.session_record(session_id).ok().flatten())
        .map(|record| record.name)
        .unwrap_or_default();
    set_terminal_title(&name);
}

#[cfg(test)]
mod title_tests {
    /// 标题里的控制字符要滤掉：OSC 串到 BEL 为止，混进一个控制符会把序列截断，
    /// 剩下的字直接糊在屏幕上。
    #[test]
    fn a_title_with_control_characters_is_scrubbed() {
        let dirty = "改\x07个\x1b[31m名\n字";
        let safe: String = dirty
            .chars()
            .filter(|ch| !ch.is_control())
            .take(120)
            .collect();
        assert_eq!(safe, "改个[31m名字");
        assert!(!safe.contains('\x07') && !safe.contains('\x1b') && !safe.contains('\n'));
    }
}

/// 一个回合的状态守卫：造出来报 `working`，**不管从哪条路返回**都报回 `idle`。
///
/// 回合有九条出口（正常收尾、`run.failed`、`run.cancelled`、断线、Ctrl+C、脱离、
/// 三处 `bail!`……）。只在正常那条上报 idle 的话，一报错侧栏就永远停在「运行中」
/// ——用户 09-20 实测到的就是这个。用 Drop 收口，新增出口也不会再漏。
pub(in crate::cli) struct TurnGuard {
    session_id: String,
    /// 这个进程跑完就没了（一次性 / shellhook）吗。真时收尾要 `release` 而不是
    /// 报 idle——否则那个 pane 上永远挂着一个已经不存在的 miyu：人在终端里说了
    /// 一句自然语言，侧栏就多出一个赖着不走的 agent。
    transient: bool,
}

impl TurnGuard {
    /// 常驻 REPL 的回合：收尾报 `idle`（人还在这条 REPL 里）。
    pub(in crate::cli) fn begin(session_id: &str) -> Self {
        Self::start(session_id, false)
    }

    /// 一次性 / shellhook 的回合：收尾 `release`（进程马上就没了）。
    pub(in crate::cli) fn begin_transient(session_id: &str) -> Self {
        Self::start(session_id, true)
    }

    fn start(session_id: &str, transient: bool) -> Self {
        report(HerdrState::Working, None, Some(session_id));
        Self {
            session_id: session_id.to_string(),
            transient,
        }
    }

    /// 她反问了：报 `blocked`（侧栏把整条 tab / workspace 标红）。
    pub(in crate::cli) fn blocked(&self, question: Option<&str>) {
        report(HerdrState::Blocked, question, Some(&self.session_id));
    }

    /// 答完了，回合接着跑：报回 `working`。不报的话侧栏会一直红到回合结束。
    pub(in crate::cli) fn resumed(&self) {
        report(HerdrState::Working, None, Some(&self.session_id));
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if self.transient {
            release_blocking();
        } else {
            report(HerdrState::Idle, None, Some(&self.session_id));
        }
    }
}
