//! 输入区的几何真相：测量、绘制、光标、清除四处共用同一套数字。
//!
//! 空会话大厅里输入框不是全宽的，而是嵌在艺术字下面的一个居中窄框。以前
//! 「这个框有多宽」在三处各算各的：测行数用整终端宽（`frame.rs`）、画字用
//! 窄框宽（`input.rs`）、算光标又退回整终端宽（`repl_cursor_position`）。
//! 字按 80 列折了行，光标却按 120 列算——于是屏幕上看到的是「文字换了行，
//! 光标还停在右边空地上」。
//!
//! 这里把「框在哪、多宽」收成一处：`EditorBox` 是唯一的横向真相，谁要用谁取。

/// 大厅窄框的上限宽度。再宽就不像「一个框」了，读起来也累。
const LOBBY_BOX_MAX_WIDTH: usize = 84;
/// 窄框的下限：再窄连提示前缀带两三个字都放不下。
const LOBBY_BOX_MIN_WIDTH: usize = 20;

/// 大厅窄框的宽度：终端的三分之二上下，至少比艺术字宽一圈，最多 84 列。
///
/// 只由终端宽度和艺术字宽度决定，**与活动区有多高无关**。这一点是故意的：
/// 调用方因此可以先定宽、再按这个宽度测行数、最后才排纵向位置，不必陷进
/// 「测高要先知道宽、定宽要先知道高」的死循环。
pub(in crate::cli) fn lobby_box_width(cols: usize, art_cols: usize) -> usize {
    (cols * 2 / 3)
        .max(art_cols + 8)
        .min(LOBBY_BOX_MAX_WIDTH)
        .min(cols.saturating_sub(2))
        .max(LOBBY_BOX_MIN_WIDTH)
}

/// 窄框的左边距：整屏居中。
pub(in crate::cli) fn lobby_box_left(cols: usize, width: usize) -> usize {
    cols.saturating_sub(width) / 2
}

/// 输入区在屏幕上的横向位置。`None`（调用方那一侧）表示老样子：从第 0 列
/// 画到终端右边，清行可以直接 `Clear(CurrentLine)`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) struct EditorBox {
    /// 左边距（屏幕列）。
    pub left: u16,
    /// 可用宽度（含提示前缀那两列）。换行、光标、footer 截断都按它算。
    pub width: usize,
}

impl EditorBox {
    /// 大厅窄框。
    pub(in crate::cli) fn lobby(cols: usize, art_cols: usize) -> Self {
        let width = lobby_box_width(cols, art_cols);
        Self {
            left: lobby_box_left(cols, width).min(u16::MAX as usize) as u16,
            width,
        }
    }

    /// 全宽贴左（普通会话、inline）。
    pub(in crate::cli) fn full(cols: usize) -> Self {
        Self {
            left: 0,
            width: cols.max(1),
        }
    }
}

/// 取 `Option<EditorBox>` 的可用宽度；`None` 走全宽。
pub(in crate::cli) fn box_cols(area: Option<EditorBox>, cols: usize) -> usize {
    area.map(|area| area.width).unwrap_or(cols).max(1)
}

/// 取 `Option<EditorBox>` 的左边距；`None` 贴左。
pub(in crate::cli) fn box_left(area: Option<EditorBox>) -> u16 {
    area.map(|area| area.left).unwrap_or(0)
}

/// 旧活动区里**新活动区盖不到**的那几行（半开区间的行号列表）。
///
/// 输入从两行缩回一行时整块会往下挪半行，新的 footer 落在比原来高一行的位置，
/// 旧那行既不在新活动区里、banner 那一行又恰好没变（diff 判「没变」直接跳过），
/// 于是屏幕上同时挂着两条 footer——这就是「行减少时 footer 短暂重复」。
/// 把这几行挑出来交给正文层强制重画，是 inline 那边 `suspend()` 擦旧活动区
/// 的全屏版。
pub(in crate::cli) fn stale_activity_rows(
    previous: Option<(u16, u16)>,
    current: Option<(u16, u16)>,
) -> Vec<u16> {
    let Some((start, rows)) = previous.filter(|(_, rows)| *rows > 0) else {
        return Vec::new();
    };
    let covered = current.filter(|(_, rows)| *rows > 0);
    (start..start.saturating_add(rows))
        .filter(|row| {
            covered.is_none_or(|(at, height)| *row < at || *row >= at.saturating_add(height))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_activity_leaves_the_tail_rows_stale() {
        assert_eq!(stale_activity_rows(Some((10, 6)), Some((10, 5))), vec![15]);
    }

    #[test]
    fn activity_moving_up_leaves_the_bottom_rows_stale() {
        assert_eq!(
            stale_activity_rows(Some((10, 6)), Some((8, 5))),
            vec![13, 14, 15]
        );
    }

    #[test]
    fn growing_activity_covers_everything_old() {
        assert!(stale_activity_rows(Some((10, 4)), Some((9, 6))).is_empty());
    }

    #[test]
    fn no_previous_frame_means_nothing_to_clean() {
        assert!(stale_activity_rows(None, Some((10, 4))).is_empty());
        assert!(stale_activity_rows(Some((10, 0)), Some((10, 4))).is_empty());
    }
}
