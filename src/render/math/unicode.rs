//! 把 LaTeX 退化成 Unicode 文本。
//!
//! 终端画不了图时的兜底：`\alpha` → `α`，上下标用 Unicode 的上下标字符。
//! 嵌套有深度上限（`MAX_MATH_NESTING`）——公式由模型生成，递归解析没有上限就是
//! 一个爆栈入口。
//!
//! 括号按需补（`fully_parenthesized`）：`\frac{a}{b}` 退化成 `a/b` 时，
//! `a+1` 必须变成 `(a+1)`，否则优先级就错了。

use crate::render::math::*;

/// 转写递归的嵌套深度上限:正常公式远低于此,超限说明是构造出的
/// 深嵌套(如上万层 `{`),递归转写会栈溢出 abort,回退原文。
pub(crate) const MAX_MATH_NESTING: usize = 64;

/// 统计裸 `{` 的最大嵌套深度(`\{` 转义不计,与递归转写的分组语义一致)。
pub(crate) fn max_brace_depth(chars: &[char]) -> usize {
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut escaped = false;
    for &ch in chars {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '{' => {
                depth += 1;
                max_depth = max_depth.max(depth);
            }
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max_depth
}

/// 把 LaTeX 行内公式尽力转成 Unicode 数学文本。
pub(crate) fn unicode_math(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    if max_brace_depth(&chars) > MAX_MATH_NESTING {
        return tex.to_string();
    }
    let mut cursor = 0usize;
    let output = convert_sequence(&chars, &mut cursor, None);
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 递归转写,遇到 `stop`(组结束的 '}')返回。
pub(crate) fn convert_sequence(chars: &[char], cursor: &mut usize, stop: Option<char>) -> String {
    let mut output = String::new();
    while *cursor < chars.len() {
        let ch = chars[*cursor];
        if Some(ch) == stop {
            *cursor += 1;
            return output;
        }
        match ch {
            '\\' => {
                *cursor += 1;
                output.push_str(&convert_command(chars, cursor));
            }
            '^' => {
                *cursor += 1;
                let script = read_group(chars, cursor);
                output.push_str(&to_script(&script, SUPERSCRIPTS, '^'));
            }
            '_' => {
                *cursor += 1;
                let script = read_group(chars, cursor);
                output.push_str(&to_script(&script, SUBSCRIPTS, '_'));
            }
            '{' => {
                *cursor += 1;
                output.push_str(&convert_sequence(chars, cursor, Some('}')));
            }
            '\'' => {
                *cursor += 1;
                output.push('′');
            }
            '~' => {
                *cursor += 1;
                output.push(' ');
            }
            _ => {
                *cursor += 1;
                output.push(ch);
            }
        }
    }
    output
}

/// 读取一个参数组:`{...}`(递归转写)或单个字符/命令。
pub(crate) fn read_group(chars: &[char], cursor: &mut usize) -> String {
    match chars.get(*cursor) {
        Some('{') => {
            *cursor += 1;
            convert_sequence(chars, cursor, Some('}'))
        }
        Some('\\') => {
            *cursor += 1;
            convert_command(chars, cursor)
        }
        Some(ch) => {
            *cursor += 1;
            ch.to_string()
        }
        None => String::new(),
    }
}

pub(crate) fn convert_command(chars: &[char], cursor: &mut usize) -> String {
    // 单字符转义:\{ \} \, \; 等。
    if let Some(&ch) = chars.get(*cursor) {
        if !ch.is_ascii_alphabetic() {
            *cursor += 1;
            return match ch {
                ',' | ';' | ':' | ' ' | '!' => " ".to_string(),
                _ => ch.to_string(),
            };
        }
    }
    let start = *cursor;
    while chars.get(*cursor).is_some_and(|ch| ch.is_ascii_alphabetic()) {
        *cursor += 1;
    }
    let name: String = chars[start..*cursor].iter().collect();
    // 吃掉命令后的一个空格(TeX 语义)。
    if chars.get(*cursor) == Some(&' ') {
        *cursor += 1;
    }
    match name.as_str() {
        "frac" | "dfrac" | "tfrac" => {
            let numerator = read_group(chars, cursor);
            let denominator = read_group(chars, cursor);
            format!("{}/{}", parenthesize(&numerator), parenthesize(&denominator))
        }
        "sqrt" => {
            let radicand = read_group(chars, cursor);
            format!("√{}", parenthesize(&radicand))
        }
        "operatorname" | "text" | "mathrm" | "mathbf" | "mathit" | "textbf" | "textit"
        | "mathsf" | "mathcal" => read_group(chars, cursor),
        "binom" | "dbinom" | "tbinom" => {
            let upper = read_group(chars, cursor);
            let lower = read_group(chars, cursor);
            format!("C({},{})", upper.trim(), lower.trim())
        }
        "hat" | "bar" | "overline" | "vec" | "tilde" | "dot" | "ddot" | "check" | "breve" => {
            let argument = read_group(chars, cursor);
            let mark = match name.as_str() {
                "hat" => '\u{0302}',
                "bar" | "overline" => '\u{0304}',
                "vec" => '\u{20d7}',
                "tilde" => '\u{0303}',
                "dot" => '\u{0307}',
                "ddot" => '\u{0308}',
                "check" => '\u{030c}',
                _ => '\u{0306}',
            };
            // 组合附标跟在末字符后;单字符最自然,多字符也可读。
            format!("{argument}{mark}")
        }
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "displaystyle" | "textstyle"
        | "limits" | "nolimits" => String::new(),
        "quad" | "qquad" => " ".to_string(),
        other => symbol_for(other).map(str::to_string).unwrap_or_else(|| format!("\\{other}")),
    }
}

/// 单 token(字母数字或已括)不再加括号。
pub(crate) fn parenthesize(text: &str) -> String {
    let trimmed = text.trim();
    let simple = trimmed.chars().count() <= 1
        || trimmed.chars().all(|ch| ch.is_alphanumeric() || ch == '.' || ch == '′')
        || fully_parenthesized(trimmed);
    if simple { trimmed.to_string() } else { format!("({trimmed})") }
}

/// 整体被同一对匹配括号包裹才算"已括":`(a)+(b)` 两端虽是括号但
/// 首括号在中途就闭合,仍需外层加括号,否则 `\frac{(a)+(b)}{c}`
/// 会转写成 `(a)+(b)/c`,数学语义反转。
pub(crate) fn fully_parenthesized(text: &str) -> bool {
    if !text.starts_with('(') || !text.ends_with(')') {
        return false;
    }
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + ch.len_utf8() == text.len();
                }
            }
            _ => {}
        }
    }
    false
}

pub(crate) const SUPERSCRIPTS: &[(char, char)] = &[
    ('0', '⁰'), ('1', '¹'), ('2', '²'), ('3', '³'), ('4', '⁴'), ('5', '⁵'), ('6', '⁶'),
    ('7', '⁷'), ('8', '⁸'), ('9', '⁹'), ('+', '⁺'), ('-', '⁻'), ('−', '⁻'), ('=', '⁼'),
    ('(', '⁽'), (')', '⁾'), ('n', 'ⁿ'), ('i', 'ⁱ'), ('T', 'ᵀ'), ('t', 'ᵗ'), ('k', 'ᵏ'),
    ('m', 'ᵐ'), ('a', 'ᵃ'), ('b', 'ᵇ'), ('c', 'ᶜ'), ('d', 'ᵈ'), ('e', 'ᵉ'), ('x', 'ˣ'),
    ('y', 'ʸ'), ('p', 'ᵖ'), ('r', 'ʳ'), ('s', 'ˢ'), ('u', 'ᵘ'), ('v', 'ᵛ'), ('*', '*'),
    ('′', '′'), ('⊤', 'ᵀ'),
];

pub(crate) const SUBSCRIPTS: &[(char, char)] = &[
    ('0', '₀'), ('1', '₁'), ('2', '₂'), ('3', '₃'), ('4', '₄'), ('5', '₅'), ('6', '₆'),
    ('7', '₇'), ('8', '₈'), ('9', '₉'), ('+', '₊'), ('-', '₋'), ('−', '₋'), ('=', '₌'),
    ('(', '₍'), (')', '₎'), ('a', 'ₐ'), ('e', 'ₑ'), ('h', 'ₕ'), ('i', 'ᵢ'), ('j', 'ⱼ'),
    ('k', 'ₖ'), ('l', 'ₗ'), ('m', 'ₘ'), ('n', 'ₙ'), ('o', 'ₒ'), ('p', 'ₚ'), ('r', 'ᵣ'),
    ('s', 'ₛ'), ('t', 'ₜ'), ('u', 'ᵤ'), ('v', 'ᵥ'), ('x', 'ₓ'),
];
