//! 场所能力位：这个渲染面**能做什么**，而不是「它是哪一档」。
//!
//! 在此之前，过程渲染靠四个模式位（`plain` / `live_summary` / `tool_call_mode`
//! / 全局块开关）现场派生两个谓词（`timeline_enabled` / `timeline_static`），
//! 然后在二十几处就地问它们。问题不在于谓词本身，而在于**同一个谓词在不同地方
//! 回答的是不同的问题**：`timeline_static()` 在一处的意思是「这步跑完立刻落
//! scrollback」，在另一处是「段末不写 `Worked for`」，在第三处是「详情就地印、
//! 没处点开」。三件事碰巧在今天的档位组合下同真同假，于是被写成了一个判断——
//! 哪天要拆开（比如「全屏但不自动收起」），就得把二十几处逐个重读一遍才知道
//! 该改哪些。
//!
//! 这里把它们拆成各自有名字的能力位。**第一步只是改名**：`caps()` 从同样的四个
//! 位算出来，与旧谓词逐字节等价（`surface_caps_match_the_old_predicates` 穷举
//! 24 组合钉死）。收益在下一步——要加「不自动收起」，改的是 `caps()` 里一行，
//! 不是二十几个调用点。
//!
//! ## 一共有几个面，各自谁在用
//!
//! 用户数的是五条路（shellhook、inline、TUI、前台子代理浮层、后台子代理浮层）。
//! 按渲染层真正的判定重新数，**主线是六个面，浮层是两个组装器**——而且
//! shellhook 与 inline **从来不是两个面**：渲染层零分支，`try_run_remote_chat`
//! 也是同一个循环，唯一差别是宿主给不给活动区（`live: Option<&mut LiveReplTail>`）。
//! 过去文档一直拿宿主的名字称呼同一个面，才显得像两样东西
//! （`docs/plan/2026-09-17-render-unification.md` §5）。
//!
//! | 面 | plain | 目的地是终端 | 全屏 | tool_calls | 过程怎么画 | 宿主 |
//! |---|---|---|---|---|---|---|
//! | S0 Plain | 是 | – | 否 | 强制 Hidden | 只有正文 | `--plain` |
//! | S1 Pipe | 否 | 否 | 否 | Summary | 老的一行摘要 `~ 工具×1 ok` | stdout 接管道 |
//! | S2 Cards | 否 | 是 | 否 | Full | 旧的工具卡片，无时间线 | 非全屏 + `tool_calls=full` |
//! | **S3 Static** | 否 | 是 | 否 | Summary | 静态时间线：每步落 scrollback、无块、无 `Worked for` | shellhook／单次 `miyu "…"`／inline REPL／唤醒跟进／daemon 回写 |
//! | **S4 Full** | 否 | 是 | 是 | 任意 | 可展开时间线 + `Worked for` 收缩；`full` 档把详情摆在每一步底下 | 全屏 TUI |
//!
//! 原来这儿还有第六个 S5（全屏 + `tool_calls=full`）：命令走时间线、别的工具打
//! 旧卡片，两种版式在同一屏上叠着。它**不是设计出来的**，是谓词组合的副产品
//! ——非命令那几支只认 `Summary`，而命令那一支先问「有没有时间线」。
//!
//! 现在两个显示开关都改成「**有时间线就收进去，档位只决定详情摆哪**」
//!（`captures_tools` / `captures_reasoning`），S5 不再是一个单独的面。
//!
//! 「目的地是终端」这一列就是 `live_summary`：它问的不是「我的 stdout 是不是
//! 终端」，而是「这些字节最后进不进一个有活动区的终端」——daemon 往 shellhook
//! 的 tty 回写时两者分家，所以外面只能用 `use_terminal_surface` /
//! `use_piped_surface` 选，拿不到那个字段。
//!
//! `caps()` 是方法不是构造时快照：全局块开关在测试里是线程局部、用例内开关
//! （`blocks.rs` 的 `set_enabled`），快照会把那批用例全打翻。

/// 这一步的详情放哪。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetailPlacement {
    /// 就地印在抬头底下——没处点开，看不到就是真看不到。
    Inline,
    /// 收进块里，点开才看。
    Behind,
}

/// 一个渲染面能做什么。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceCaps {
    /// 有登记处、能点开。
    pub expandable: bool,
    /// 步跑完立刻落 scrollback（而不是攒在活动区里等收段）。
    pub commit_immediately: bool,
    /// 段末写 `Worked for …` 收缩行。
    pub fold: bool,
    /// 详情放哪。
    pub detail: DetailPlacement,
}

impl SurfaceCaps {
    /// 详情就地印吗。
    pub fn detail_inline(self) -> bool {
        self.detail == DetailPlacement::Inline
    }
}

/// 两个子代理面板共用的能力位：面板自带登记处（每一步都能点开），内容整块重灌
/// 而不是逐步落地，段末自己写收缩行。
pub const PANEL_CAPS: SurfaceCaps = SurfaceCaps {
    expandable: true,
    commit_immediately: false,
    fold: true,
    detail: DetailPlacement::Behind,
};

impl crate::render::StreamRenderer {
    /// 这个面能做什么。
    ///
    /// 与旧谓词的对应（`surface_caps_match_the_old_predicates` 钉着）：
    ///
    /// | 能力位 | 旧写法 |
    /// |---|---|
    /// | `expandable` | `blocks::enabled()` |
    /// | `commit_immediately` | `timeline_static()` |
    /// | `fold` | `timeline_enabled() && !timeline_static()` |
    /// | `detail == Inline` | `timeline_static()` |
    pub fn caps(&self) -> SurfaceCaps {
        let expandable = crate::render::blocks::enabled();
        // 「没有可点开的详情，全文就不必攒」——这一位读的人是
        // `timeline_push_thought` / `write_tool_result` 那几处：它们据此决定要不
        // 要构造那份展开内容。
        //
        // **它不等于「详情印不印在屏上」**：真正决定那件事的是
        // `commit_static_steps`——凡是 `commit_immediately` 的面，它把每一步的
        // `body` 原样打在抬头底下，而且**不挂块标记**。所以开了「不自动收起」的
        // 全屏，那些步其实是点不开的、详情就地铺开——和 shellhook 一个样子
        //（用户 todolist:21 要的正是「和 shellhook 差不多的效果」）。
        //
        // 阶段 2 的提交信息里我写的是「仍然能点开，详情照旧收在块后面」，那句话
        // **是错的**：`s4-full-open.ansi` 里只有 5 个块标记（不收起那一档的常规
        // 步骤一个都没有），而 `s4-full.ansi` 有 18 个。当时的等价性测试只比了
        // `caps()` 这个结构体，没有比渲染出来的字节，所以那句话没被拦下。
        let inline_detail = self.timeline_static();
        // 用户 todolist:21「TUI 不自动收起 Worked for」——整个需求就落在这一行:
        // 不收段 = 每一步跑完就地落下去,和逐步落地的面一个走法。
        let keep_open = expandable && self.keep_timeline_open;
        let commit_immediately = inline_detail || keep_open;
        SurfaceCaps {
            expandable,
            commit_immediately,
            fold: self.timeline_enabled() && !commit_immediately,
            detail: if inline_detail {
                DetailPlacement::Inline
            } else {
                DetailPlacement::Behind
            },
        }
    }
}
