//! 渲染测试共用的 fixture：剥 ANSI、跑一遍文档、导出流式预览。

pub(super) fn visible_command_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| strip_ansi_for_test(&line))
        .collect()
}

pub(super) fn strip_ansi_for_test(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}
