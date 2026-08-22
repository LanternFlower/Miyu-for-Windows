//! 终端显示宽度计算。
//!
//! 「一个字符占几列」在终端里不是常识问题：CJK 与 emoji 占两列，组合记号占
//! 零列，ANSI 转义序列一列都不占。REPL 的换行、截断、对齐全靠这几个函数算
//! 准，算错就是光标错位。

// 编辑器还用着一批留在 cli::mod 的辅助（光标插入、粘贴占位、命令补全等）。
// 它们本身也该继续往下拆，但那是后面的步骤——先用 super::* 把编译撑住，
// 免得一次改动横跨太多文件而无法定位问题。

pub(in crate::cli) fn pad_visible_width(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(visible_width(value)))
    )
}

pub(in crate::cli) fn wrap_visible_width(value: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut width = 0usize;
    for ch in value.chars() {
        let char_width = visible_width(&ch.to_string());
        if width > 0 && width.saturating_add(char_width) > max_width {
            lines.push(std::mem::take(&mut current));
            width = 0;
        }
        current.push(ch);
        width = width.saturating_add(char_width);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

pub(in crate::cli) fn visible_width(value: &str) -> usize {
    let mut width = 0usize;
    let mut escape = false;
    for ch in value.chars() {
        if escape {
            if ch == 'm' {
                escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            escape = true;
        } else if (ch as u32) >= 0x2e80 {
            width += 2;
        } else {
            width += 1;
        }
    }
    width
}

pub(in crate::cli) fn truncate_visible_width(value: &str, max_width: usize) -> String {
    if visible_width(value) <= max_width {
        return value.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let mut output = String::new();
    let mut width = 0usize;
    let ellipsis_width = visible_width("...");
    let budget = max_width.saturating_sub(ellipsis_width);
    for ch in value.chars() {
        let ch_width = visible_width(&ch.to_string());
        if width.saturating_add(ch_width) > budget {
            break;
        }
        output.push(ch);
        width = width.saturating_add(ch_width);
    }
    output.push_str("...");
    output
}
