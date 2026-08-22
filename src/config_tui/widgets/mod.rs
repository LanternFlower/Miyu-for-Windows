//! TUI 的底层构件：菜单、表单、选择器、输入框、绘制与按键。
//!
//! 上面所有编辑界面都由这几样拼出来，所以这里只管「怎么画、怎么读键」，一条
//! 业务规则都不知道。
//!
//! 宽度处理是重点：中文是双宽字符，emoji 更宽，`display_width` / `pad` /
//! `truncate` 全部按显示宽度算而不是按字符数——按字符数算会让所有含中文的框都
//! 错位。光标移动同理，`byte_index_for_char` 负责在字节与字符之间换算。

mod draw;
mod form;
mod select;
pub(in crate::config_tui) use draw::*;
pub(in crate::config_tui) use form::*;
pub(in crate::config_tui) use select::*;

use crate::config_tui::*;

/// 子界面出错时的兜底提示:错误只作废当次表单输入,绝不让它穿透主循环
/// 把 TUI 崩出(崩出会连带丢掉本次全部未保存修改)。
pub(in crate::config_tui) fn show_tui_error(
    stdout: &mut io::Stdout,
    error: &anyhow::Error,
) -> Result<()> {
    let options = vec![
        format!("{error:#}"),
        t("Press any key to go back", "按任意键返回").to_string(),
    ];
    draw_menu(stdout, t(" ERROR ", " 错误 "), &options, 1, "")?;
    let _ = read_key()?;
    Ok(())
}

pub(in crate::config_tui) fn format_text_file(content: &str) -> String {
    let content = content.trim_end();
    if content.is_empty() {
        String::new()
    } else {
        format!("{content}\n")
    }
}

pub(in crate::config_tui) fn parse_key_list(value: &str) -> Vec<String> {
    value
        .split(|ch| ch == ',' || ch == '\n' || ch == '\r')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

pub(in crate::config_tui) struct FcitxState {
    pub(in crate::config_tui) last_state: Option<char>,
}

impl FcitxState {
    pub(in crate::config_tui) fn new() -> Self {
        let last_state = fcitx5_state();
        run_fcitx5_remote("-c");
        Self { last_state }
    }

    pub(in crate::config_tui) fn enter_editing(&mut self) {
        if self.last_state == Some('2') {
            run_fcitx5_remote("-o");
        }
    }

    pub(in crate::config_tui) fn leave_editing(&mut self) {
        self.last_state = fcitx5_state();
        run_fcitx5_remote("-c");
    }
}

pub(in crate::config_tui) fn fcitx5_state() -> Option<char> {
    let output = Command::new("fcitx5-remote").output().ok()?;
    output.stdout.first().copied().map(char::from)
}

pub(in crate::config_tui) fn run_fcitx5_remote(arg: &str) {
    let _ = Command::new("fcitx5-remote").arg(arg).spawn();
}

pub(in crate::config_tui) fn normalize_base_url(value: &str) -> String {
    let mut url = value.trim().trim_end_matches('/').to_string();
    if url.ends_with("/chat/completions") {
        url.truncate(url.len() - "/chat/completions".len());
    }
    url
}
