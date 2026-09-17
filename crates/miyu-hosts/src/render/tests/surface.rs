//! 能力位与旧谓词的**等价性**：穷举四个模式位的每一种组合，逐位比对。
//!
//! 阶段 2 是「只改名」，那就得先有一张网证明改名前后一个字节都没动。等哪天要
//! 让某个能力位与旧谓词**故意**分家（比如「全屏但不自动收起」），改的是这里的
//! 期望值，而且只改那一位——谁跟着变了，一眼看得见。

use super::timeline::with_blocks;
use crate::render::stream::surface::DetailPlacement;
use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};

fn renderer(plain: bool, live_summary: bool, tool_calls: ToolCallDisplayMode) -> StreamRenderer {
    let mut renderer =
        StreamRenderer::new(ReasoningDisplayMode::Summary, tool_calls, plain, true, 8);
    renderer.use_buffered_output();
    renderer.live_summary = live_summary;
    renderer
}

/// 2 × 2 × 2 × 3 = 24 组。每组都比四位。
#[test]
fn surface_caps_match_the_old_predicates() {
    for blocks_on in [false, true] {
        let check = || {
            for plain in [false, true] {
                for live_summary in [false, true] {
                    for tool_calls in [
                        ToolCallDisplayMode::Hidden,
                        ToolCallDisplayMode::Summary,
                        ToolCallDisplayMode::Full,
                    ] {
                        let renderer = renderer(plain, live_summary, tool_calls);
                        let caps = renderer.caps();
                        let label = format!(
                            "blocks={blocks_on} plain={plain} live={live_summary} tools={tool_calls:?}"
                        );
                        assert_eq!(
                            caps.expandable,
                            crate::render::blocks::enabled(),
                            "expandable 与 blocks::enabled() 不符: {label}"
                        );
                        assert_eq!(
                            caps.commit_immediately,
                            renderer.timeline_static(),
                            "commit_immediately 与 timeline_static() 不符: {label}"
                        );
                        assert_eq!(
                            caps.fold,
                            renderer.timeline_enabled() && !renderer.timeline_static(),
                            "fold 与 `有时间线且不就地落地` 不符: {label}"
                        );
                        assert_eq!(
                            caps.detail == DetailPlacement::Inline,
                            renderer.timeline_static(),
                            "detail 与 timeline_static() 不符: {label}"
                        );
                    }
                }
            }
        };
        if blocks_on {
            with_blocks(check);
        } else {
            check();
        }
    }
}

/// 三个值得钉的面各自长什么样（`docs/plan/2026-09-17-render-unification.md` §1.1）。
/// 这张表是「改名不改行为」的可读版：谁该收缩、谁该就地落地，一眼看得出。
#[test]
fn the_three_surfaces_have_the_shapes_we_expect() {
    // S1 管道:没有时间线,什么都不收不折。
    let pipe = renderer(false, false, ToolCallDisplayMode::Summary).caps();
    assert!(!pipe.expandable && !pipe.commit_immediately && !pipe.fold);

    // S3 静态时间线:每步立刻落地、详情就地印、没有 `Worked for`。
    let static_caps = renderer(false, true, ToolCallDisplayMode::Summary).caps();
    assert!(static_caps.commit_immediately, "静态版该立刻落地");
    assert!(!static_caps.fold, "静态版不该有收缩行");
    assert!(static_caps.detail_inline(), "静态版详情该就地印");
    assert!(!static_caps.expandable, "静态版没有登记处");

    // S4 全屏:能点开、攒着收段。
    with_blocks(|| {
        let full = renderer(false, true, ToolCallDisplayMode::Summary).caps();
        assert!(full.expandable, "全屏该能点开");
        assert!(full.fold, "全屏该收成 Worked for");
        assert!(!full.commit_immediately, "全屏不该逐步落地");
        assert!(!full.detail_inline(), "全屏详情该在块后面");
    });
}

/// 面板那一份是常量，不随全局块开关变——两个子代理面板共用它。
#[test]
fn panel_caps_are_fixed() {
    let caps = crate::render::stream::surface::PANEL_CAPS;
    assert!(caps.expandable && caps.fold);
    assert!(!caps.commit_immediately);
    assert!(!caps.detail_inline());
}

/// 「TUI 不自动收起」开了之后:每一步就地落下去、段末没有 `Worked for …`
/// ——和 shell 无缝对话那条路一个样子(用户 todolist:21)。
///
/// **注意这里比的只是 `caps()` 这个结构体**,不是渲染出来的字节。`expandable`
/// 为真的意思是「这个面有登记处」,不等于「每一步都挂了块标记」:走
/// `commit_static_steps` 落地的那些步**不挂标记**,所以不收起那一档里常规步骤
/// 其实点不开(`s4-full-open.ansi` 只有 5 个标记,`s4-full.ansi` 有 18 个)。
/// 阶段 2 的提交信息把这一点写反了,更正记在 `surface.rs` 的 `caps()` 里。
#[test]
fn keeping_the_timeline_open_stops_folding() {
    with_blocks(|| {
        let mut renderer = renderer(false, true, ToolCallDisplayMode::Summary);
        renderer.keep_timeline_open = true;
        let caps = renderer.caps();
        assert!(caps.expandable, "开着也该能点开");
        assert!(!caps.fold, "开着就不该再收成 Worked for");
        assert!(caps.commit_immediately, "开着该每一步就地落下去");
        assert!(
            !caps.detail_inline(),
            "能点开的面详情仍该收在块后面,不是就地铺开"
        );
    });
}
