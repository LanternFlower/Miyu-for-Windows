//! Full-screen session selection. The lobby keeps its input and captions;
//! conversations share the question panel's scrollable body viewport.

use crate::cli::*;

#[derive(Clone, Copy, Debug)]
struct Panel {
    left: u16,
    top: u16,
    width: u16,
    rows: u16,
}

pub(super) fn pick(
    live: &mut LiveReplTail,
    entries: &[SessionListEntry],
    active: &str,
    cursor: Option<usize>,
) -> Result<SessionPick> {
    let result = run(live, entries, active, cursor);
    live.lobby_panel_rows = 0;
    // Restore both geometry and the cursor after cancellation, selection, or
    // an input error. The transcript itself never belongs to the picker.
    synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
    result
}

fn run(
    live: &mut LiveReplTail,
    entries: &[SessionListEntry],
    active: &str,
    cursor: Option<usize>,
) -> Result<SessionPick> {
    let _raw = LiveRawMode::start()?;
    let lines: Vec<_> = entries
        .iter()
        .map(|entry| session_select_line(entry, Some(active)))
        .collect();
    let search: Vec<_> = entries.iter().map(session_select_search).collect();
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = cursor.unwrap_or_else(|| session_initial_selection(entries, Some(active)));
    let mut scroll = 0;
    let mut confirming: Option<usize> = None;
    let mut body_delta = 0;
    let mut layout: Option<((u16, u16, u16), Panel)> = None;
    loop {
        if expire_toast(live) {
            layout = None;
        }
        let matches = fuzzy_matches(&matcher, &search, &query);
        selected = selected.min(matches.len().saturating_sub(1));
        let mut page_rows = 1;
        synchronized_terminal_update(CursorAfterUpdate::Hidden, || {
            let (cols, rows) = terminal::size().unwrap_or((80, 24));
            let geometry = (cols, rows, inline_fuzzy_lines(matches.len()));
            let panel = match layout {
                Some((previous, panel)) if previous == geometry && body_delta == 0 => panel,
                _ => prepare(live, matches.len(), body_delta)?,
            };
            layout = Some((geometry, panel));
            page_rows = panel.top.max(1) as isize;
            let visible = matches.len().min(panel.rows.saturating_sub(2) as usize);
            scroll = inline_fuzzy_scroll(selected, scroll, visible);
            let bar = input_prompt_bar(live.mode());
            let width = usize::from(panel.width).saturating_sub(visible_width(&bar));
            let header = match confirming {
                Some(index) => {
                    inline_single_confirm_header(display_session_name(&entries[index].name), width)
                }
                None => inline_single_header(t("Select session", "选择会话"), &query, width),
            };
            let mut content = vec![header];
            if matches.is_empty() {
                content.push(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")));
            } else {
                content.extend(matches.iter().skip(scroll).take(visible).enumerate().map(
                    |(row, (_, index))| {
                        inline_single_item_line(&lines[*index], scroll + row == selected, width)
                    },
                ));
            }
            content.resize(panel.rows.saturating_sub(1) as usize, String::new());
            content.push(inline_single_help_line(width, true));
            let mut stdout = io::stdout();
            for (row, line) in content.iter().take(panel.rows as usize).enumerate() {
                queue!(
                    stdout,
                    MoveTo(panel.left, panel.top + row as u16),
                    Print(" ".repeat(panel.width as usize)),
                    MoveTo(panel.left, panel.top + row as u16),
                    Print(render::clip_to_display_width(
                        &format!("{bar}{line}"),
                        panel.width as usize
                    ))
                )?;
            }
            stdout.flush()?;
            Ok(())
        })?;
        body_delta = 0;
        // The regular REPL pump is paused while the selector owns input.
        // Only an expired toast invalidates the layout during an idle wait.
        while !event::poll(std::time::Duration::from_millis(100))? {
            if expire_toast(live) {
                layout = None;
                break;
            }
        }
        if layout.is_none() {
            continue;
        }
        let event = event::read()?;
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            if let Event::Mouse(mouse) = event {
                body_delta = match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => -3,
                    crossterm::event::MouseEventKind::ScrollDown => 3,
                    _ => 0,
                };
            }
            continue;
        };
        if kind == crossterm::event::KeyEventKind::Release {
            continue;
        }
        if let Some(index) = confirming.take() {
            if matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                return Ok(SessionPick::Delete {
                    session_id: entries[index].id.clone(),
                    index,
                });
            }
            continue;
        }
        match code {
            KeyCode::PageUp => {
                body_delta = -page_rows;
                continue;
            }
            KeyCode::PageDown => {
                body_delta = page_rows;
                continue;
            }
            _ => {}
        }
        match inline_select_key(code, modifiers, true) {
            InlineSelectKey::Cancel => return Ok(SessionPick::Cancelled),
            InlineSelectKey::Accept => {
                return Ok(matches
                    .get(selected)
                    .map_or(SessionPick::Cancelled, |(_, index)| {
                        SessionPick::Switch(crate::ipc::SessionRef::Id {
                            id: entries[*index].id.clone(),
                        })
                    }))
            }
            InlineSelectKey::DeleteRequest => confirming = matches.get(selected).map(|(_, i)| *i),
            InlineSelectKey::Up => selected = selected.saturating_sub(1),
            InlineSelectKey::Down => selected = (selected + 1).min(matches.len().saturating_sub(1)),
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

fn expire_toast(live: &mut LiveReplTail) -> bool {
    live.screen
        .as_mut()
        .is_some_and(|screen| screen.expire_toast())
}

fn prepare(live: &mut LiveReplTail, count: usize, delta: isize) -> Result<Panel> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let desired = inline_fuzzy_lines(count);
    if live.banner.is_some() {
        live.lobby_panel_rows = desired.saturating_add(1);
        live.resume()?;
        let lobby = live
            .banner
            .as_ref()
            .expect("lobby was not changed")
            .lobby_with_bottom_space(
                usize::from(cols),
                usize::from(rows),
                usize::from(live.tail_rows),
                usize::from(live.lobby_panel_rows),
            );
        // Very short terminals may have less room than the desired list. Keep
        // the visible list bounded rather than writing beyond the last row.
        let top = lobby.below.saturating_add(1).min(rows.saturating_sub(1));
        return Ok(Panel {
            left: lobby.left,
            top,
            width: lobby.width.min(cols.saturating_sub(lobby.left)),
            rows: desired.min(rows.saturating_sub(top)),
        });
    }
    let panel = body_panel(cols, rows, desired);
    if let Some(screen) = &mut live.screen {
        screen.scroll_question_body(delta, panel.rows.saturating_add(1))?;
    }
    let mut stdout = io::stdout();
    for row in panel.top..rows {
        queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    }
    Ok(panel)
}

fn body_panel(cols: u16, rows: u16, desired: u16) -> Panel {
    let height = desired.min(rows.saturating_sub(2).max(1)).min(rows);
    Panel {
        left: 0,
        top: rows.saturating_sub(height).saturating_sub(1),
        width: cols,
        rows: height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_panel_reserves_only_its_height_and_bottom_padding() {
        let panel = body_panel(100, 32, 4);
        assert_eq!(panel.top, 27);
        assert_eq!(panel.rows, 4);
        for rows in 0..50 {
            let panel = body_panel(80, rows, 12);
            assert!(panel.top + panel.rows <= rows);
        }
    }
}
