//! 终端内嵌的选择器。
//!
//! 不开全屏、不清屏，就在光标处画几行让人上下选——这样选完之后正文还在原
//! 处，滚上去还能看到之前的对话。模糊匹配、可删除项、单选多选都在这里。

use crate::cli::*;

pub(in crate::cli) fn inline_fuzzy_select(
    items: &[String],
    active: Vec<bool>,
) -> Result<Option<Vec<bool>>> {
    inline_fuzzy_select_with(items, active, None)
}

/// 带一条 Tab 规矩的多选：`toggle` 给了就由它决定按 Tab 时哪几格翻（`/models` 的
/// 「继承」连带，见 `model_cmds::toggle_model_row`）；没给就是翻高亮那一格。
pub(in crate::cli) fn inline_fuzzy_select_with(
    items: &[String],
    mut active: Vec<bool>,
    toggle: Option<&dyn Fn(&mut [bool], usize)>,
) -> Result<Option<Vec<bool>>> {
    let menu_lines = inline_fuzzy_lines(items.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = 0usize;
    let mut scroll = 0usize;
    // 验收三轮:用户搜到模型直接回车,期望"切到它";多选语义却要求
    // Tab 勾选,回车成了"确认没改"→静默未做修改。记住入场快照与是否
    // 表达过意图(搜索/移动),回车时没动过勾选就按单选切换处理。
    let initial = active.clone();
    let mut navigated = false;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let matches = fuzzy_matches(&matcher, items, &query);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }
        let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
        scroll = inline_fuzzy_scroll(selected, scroll, visible);
        draw_inline_fuzzy(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            &query,
            items,
            &matches,
            selected,
            scroll,
            &active,
        )?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('c')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Esc => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(active));
                }
                KeyCode::Char('q') if query.is_empty() => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(active));
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    if active == initial && (navigated || !query.is_empty()) {
                        if let Some((_, index)) = matches.get(selected) {
                            let mut solo = vec![false; active.len()];
                            solo[*index] = true;
                            return Ok(Some(solo));
                        }
                    }
                    return Ok(Some(active));
                }
                KeyCode::Tab => {
                    if let Some((_, index)) = matches.get(selected) {
                        match toggle {
                            Some(toggle) => toggle(&mut active, *index),
                            None => {
                                if let Some(value) = active.get_mut(*index) {
                                    *value = !*value;
                                }
                            }
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    navigated = true;
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    navigated = true;
                    selected = (selected + 1).min(matches.len().saturating_sub(1));
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                    scroll = 0;
                }
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(ch);
                    selected = 0;
                    scroll = 0;
                }
                _ => {}
            }
        }
    }
}

/// Single-select variant of the inline fuzzy menu: Tab marks a row (radio),
/// Enter confirms the marked row (or the highlighted one when nothing is
/// marked); Esc / q cancels.
pub(in crate::cli) fn inline_fuzzy_select_single(
    items: &[String],
    initial: usize,
) -> Result<Option<usize>> {
    let mut active = vec![false; items.len()];
    if let Some(slot) = active.get_mut(initial) {
        *slot = true;
    }
    let menu_lines = inline_fuzzy_lines(items.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = 0usize;
    let mut scroll = 0usize;
    // initial 恒被标记,此前 Enter 无脑取 marked 导致"搜索后回车选不中
    // 高亮项":只有用户 Tab 过才尊重标记,否则搜索/移动后回车确认高亮项。
    let mut marked_by_user = false;
    let mut navigated = false;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let matches = fuzzy_matches(&matcher, items, &query);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }
        let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
        scroll = inline_fuzzy_scroll(selected, scroll, visible);
        draw_inline_fuzzy(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            &query,
            items,
            &matches,
            selected,
            scroll,
            &active,
        )?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('c')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Esc => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Char('q') if query.is_empty() => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    let marked = active.iter().position(|value| *value);
                    let highlighted = matches.get(selected).map(|(_, index)| *index);
                    return Ok(if marked_by_user {
                        marked.or(highlighted)
                    } else if navigated || !query.is_empty() {
                        highlighted.or(marked)
                    } else {
                        marked.or(highlighted)
                    });
                }
                KeyCode::Tab => {
                    if let Some((_, index)) = matches.get(selected) {
                        for slot in active.iter_mut() {
                            *slot = false;
                        }
                        if let Some(slot) = active.get_mut(*index) {
                            *slot = true;
                        }
                        marked_by_user = true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    navigated = true;
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    navigated = true;
                    selected = (selected + 1).min(matches.len().saturating_sub(1));
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                    scroll = 0;
                }
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(ch);
                    selected = 0;
                    scroll = 0;
                }
                _ => {}
            }
        }
    }
}

/// In-place single-choice fuzzy picker; same environment and screen handling
/// as `inline_pop_select` (editor suspended, cooked mode on entry). `lines`
/// are the rendered rows and `search` the parallel fuzzy-match texts.
/// Returns the selected index, or `None` when cancelled.
/// What an inline picker returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum InlineSelectOutcome {
    Cancelled,
    Chosen(usize),
    /// Ctrl+D on a row, confirmed in place. The caller does the deletion and
    /// reopens the picker on the refreshed list.
    Deleted(usize),
}

/// A picker keystroke, resolved away from the draw loop so the delete
/// confirmation flow is testable without a terminal. Every printable character
/// is search input, which is why deletion needs a modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum InlineSelectKey {
    Cancel,
    Accept,
    Up,
    Down,
    Backspace,
    DeleteRequest,
    Char(char),
    Ignore,
}

pub(in crate::cli) fn inline_select_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    deletable: bool,
) -> InlineSelectKey {
    let control = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('c') if control => InlineSelectKey::Cancel,
        KeyCode::Char('d') if control && deletable => InlineSelectKey::DeleteRequest,
        KeyCode::Delete if deletable => InlineSelectKey::DeleteRequest,
        KeyCode::Esc => InlineSelectKey::Cancel,
        KeyCode::Enter => InlineSelectKey::Accept,
        KeyCode::Up | KeyCode::Char('k') if !control => InlineSelectKey::Up,
        KeyCode::Down | KeyCode::Char('j') if !control => InlineSelectKey::Down,
        KeyCode::Backspace => InlineSelectKey::Backspace,
        KeyCode::Char(ch) if !control => InlineSelectKey::Char(ch),
        _ => InlineSelectKey::Ignore,
    }
}

/// Fuzzy picker. Passing `delete_labels` (one per row, used in the inline
/// confirmation) enables Ctrl+D deletion and returns `Deleted` once the user
/// confirms; the caller performs the deletion and decides whether to reopen.
pub(in crate::cli) fn inline_single_select_deletable(
    title: &str,
    lines: &[String],
    search: &[String],
    initial_selected: usize,
    delete_labels: Option<&[String]>,
) -> Result<InlineSelectOutcome> {
    let menu_lines = inline_fuzzy_lines(lines.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = initial_selected.min(lines.len().saturating_sub(1));
    let mut scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    // Ctrl+D **当场删**，没有 y/N（用户 09-20 拍板；全屏那条路同步改了，见
    // `repl::session_picker`）。`delete_labels` 现在只用来判「这个选择器能不能
    // 删」；`draw_inline_single` 的确认表头那一支留着没动，别的选择器将来要
    // 确认还用得上。
    let deletable = delete_labels.is_some();
    loop {
        let matches = fuzzy_matches(&matcher, search, &query);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }
        let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
        scroll = inline_fuzzy_scroll(selected, scroll, visible);
        draw_inline_single(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            title,
            &query,
            lines,
            &matches,
            selected,
            scroll,
            deletable,
            None,
        )?;
        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        else {
            continue;
        };
        match inline_select_key(code, modifiers, deletable) {
            InlineSelectKey::Cancel => {
                clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                return Ok(InlineSelectOutcome::Cancelled);
            }
            InlineSelectKey::Accept => {
                let choice = matches.get(selected).map(|(_, index)| *index);
                clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                return Ok(match choice {
                    Some(index) => InlineSelectOutcome::Chosen(index),
                    None => InlineSelectOutcome::Cancelled,
                });
            }
            InlineSelectKey::DeleteRequest => {
                if let Some((_, index)) = matches.get(selected) {
                    let index = *index;
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(InlineSelectOutcome::Deleted(index));
                }
            }
            InlineSelectKey::Up => selected = selected.saturating_sub(1),
            InlineSelectKey::Down => {
                selected = (selected + 1).min(matches.len().saturating_sub(1));
            }
            InlineSelectKey::Backspace => {
                query.pop();
                selected = 0;
                scroll = 0;
            }
            InlineSelectKey::Char(ch) => {
                query.push(ch);
                selected = 0;
                scroll = 0;
            }
            InlineSelectKey::Ignore => {}
        }
    }
}
