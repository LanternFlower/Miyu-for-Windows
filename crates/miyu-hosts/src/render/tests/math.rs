//! 公式在流式渲染里的表现。

use crate::render::*;

fn render_document(document: &str) -> String {
    let mut renderer = MarkdownLineRenderer::new();
    let mut output = String::new();
    for line in document.lines() {
        output.push_str(&renderer.render_line(line));
    }
    output.push_str(&renderer.flush());
    output
}

#[test]
fn block_math_renders_to_halfblocks_and_inline_transliterates() {
    let document = "推导如下:\n$$\nE = mc^2\n$$\n其中 $\\alpha\\in(0,1)$,价格 $5 和 $10 不动。\n";
    let output = render_document(document);
    assert!(
        output.contains('▀') || output.contains('▄'),
        "block math should render halfblocks"
    );
    assert!(
        output.contains("α∈(0,1)"),
        "inline math should transliterate: {output}"
    );
    assert!(output.contains("$5"), "prices must stay literal");
    assert!(!output.contains("mc^2"), "raw tex should be replaced");
}

#[test]
fn table_cells_render_stacked_fractions() {
    let document = "| 方法 | 收敛阶 |\n| --- | --- |\n| 牛顿法 | $q=2$ |\n| 割线法 | $q=\\frac{1+\\sqrt5}{2}$ |\n\n";
    let output = render_document(document);
    assert!(output.contains("q=2"), "{output}");
    assert!(output.contains("1+√5"), "分子应独立成行: {output}");
    assert!(output.contains("───"), "分数线应存在: {output}");
    assert!(!output.contains("\\frac"), "{output}");
}

#[test]
fn unclosed_math_block_replays_verbatim_on_flush() {
    let output = render_document("$$\nE=mc^2\n");
    assert!(output.contains("$$"));
    assert!(output.contains("E=mc^2"));
}

#[test]
fn single_line_display_math_renders() {
    let output = render_document("$$E=mc^2$$\n");
    assert!(output.contains('▀') || output.contains('▄'), "{output}");
}

/// 检视产物:整段 markdown 渲染输出落盘,供 ANSI→PNG 回显人工核看。
#[test]
#[ignore]
fn dump_stream_preview() {
    let document = "偏导与分式:\n\n| 名称 | 表达式 |\n| --- | --- |\n| 偏导数 | $\\frac{\\partial f}{\\partial x}=\\lim_{h\\to 0}\\frac{f(x+h,y)-f(x,y)}{h}$ |\n| 二次方程 | $x=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a}$ |\n| 组合数 | $\\binom{n}{k}=\\frac{n!}{k!(n-k)!}$ |\n| 波函数 | $i\\hbar\\frac{\\partial}{\\partial t}\\Psi=\\hat{H}\\Psi$ |\n| 极限 | $\\lim_{x\\to\\infty}(1+1/x)^x=e$ |\n\n完事～\n";
    let output = render_document(document);
    std::fs::write("/tmp/claude-1000/math-stream.ansi", output).unwrap();
}
