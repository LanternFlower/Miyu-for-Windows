//! 网页端回写到发起它的终端。
//!
//! 从网页发的消息，如果这个会话本来是在某个终端里开的，输出也该出现在那个终端
//! 上。`origin_shell_at_prompt` 判断那个终端此刻是不是停在提示符处——正在跑别的
//! 命令时插一段输出会把人家的界面搅乱。
//!
//! `parse_stat_pgrp_tpgid` 读 /proc 比较进程组与前台进程组，这是「shell 空闲」
//! 的可靠判据。

pub(in crate::web) enum TtyWriteOp {
    Write(String),
    /// 正常收尾:flush 后给 shell 发 SIGWINCH 促使重绘提示符。
    Finish,
    /// 中途收笔(前台被占/超时):已写的留在屏上,不再动那个终端。
    Abort,
}

/// 回写行的三种笔触:正文走 Markdown 渲染;思考用与 REPL 正常思考一致的
/// 绿色(write_full_reasoning_chunk 同款 ANSI 10);注记(工具行/中断标记)暗色。
#[derive(Clone, Copy, PartialEq)]
pub(in crate::web) enum WriteLineStyle {
    Content,
    Reasoning,
    Note,
}

/// 行缓冲落盘:凑满整行才渲染。
pub(in crate::web) fn drain_line_buf(buf: &mut String, style: WriteLineStyle, out: &mut String) {
    while let Some(index) = buf.find('\n') {
        let line: String = buf.drain(..=index).collect();
        let line = line.trim_end_matches(['\n', '\r']);
        push_rendered_line(line, style, out);
    }
}

pub(in crate::web) fn flush_line_buf(buf: &mut String, style: WriteLineStyle, out: &mut String) {
    if buf.trim().is_empty() {
        buf.clear();
        return;
    }
    let line = std::mem::take(buf);
    push_rendered_line(line.trim_end(), style, out);
}

pub(in crate::web) fn push_rendered_line(line: &str, style: WriteLineStyle, out: &mut String) {
    match style {
        WriteLineStyle::Content => out.push_str(&crate::render::render_markdown_line(line)),
        WriteLineStyle::Reasoning => {
            if !line.is_empty() {
                out.push_str(&format!("\x1b[38;5;10m{line}\x1b[0m"));
            }
        }
        WriteLineStyle::Note => {
            if !line.is_empty() {
                out.push_str(&format!("\x1b[2m{line}\x1b[0m"));
            }
        }
    }
    out.push_str("\r\n");
}

/// 三道闸的第 2、3 道:shell 活着、还挂在记录的 tty 上、且自己就是终端前台
/// 进程组(即停在提示符,没在跑别的程序)。
pub(in crate::web) fn origin_shell_at_prompt(origin: &crate::ipc::OriginTty) -> bool {
    let pid = origin.shell_pid;
    let Ok(stdin_target) = std::fs::read_link(format!("/proc/{pid}/fd/0")) else {
        return false;
    };
    if stdin_target != origin.path {
        return false;
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    matches!(parse_stat_pgrp_tpgid(&stat), Some((pgrp, tpgid)) if pgrp == tpgid)
}

/// /proc/pid/stat 的 comm 字段可含空格和括号,必须从最后一个 \')\' 之后再按空白
/// 切:其后第 3 个字段是 pgrp,第 6 个是 tpgid。
pub(in crate::web) fn parse_stat_pgrp_tpgid(stat: &str) -> Option<(i64, i64)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let pgrp = fields.nth(2)?.parse().ok()?;
    let tpgid = fields.nth(2)?.parse().ok()?;
    Some((pgrp, tpgid))
}
