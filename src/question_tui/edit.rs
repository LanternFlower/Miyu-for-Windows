//! 自由输入框的编辑。
//!
//! 光标按**字符**移动、按字节索引切片（`byte_index`），中文才不会被切一半。

use crate::question_tui::*;

pub(in crate::question_tui) fn handle_editing_key(
    request: &QuestionRequest,
    state: &mut QuestionState,
    key: KeyEvent,
) -> Result<bool> {
    match key.code {
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            insert_text(&mut state.edit_buffer, &mut state.edit_cursor, "\n");
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            // Shift+Enter 与 Ctrl+J 相同：自定义答案编辑时插入换行
            insert_text(&mut state.edit_buffer, &mut state.edit_cursor, "\n");
        }
        KeyCode::Esc => {
            state.editing = false;
            state.edit_buffer.clear();
            state.edit_cursor = 0;
        }
        KeyCode::Enter => {
            let value = state.edit_buffer.trim().to_string();
            if value.is_empty() {
                let previous = std::mem::take(&mut state.custom_answers[state.tab]);
                state.answers[state.tab].retain(|answer| answer != &previous);
                state.editing = false;
                state.edit_buffer.clear();
                state.edit_cursor = 0;
                return Ok(false);
            }
            let question = &request.questions[state.tab];
            let previous = std::mem::replace(&mut state.custom_answers[state.tab], value.clone());
            if !previous.is_empty() {
                state.answers[state.tab].retain(|answer| answer != &previous);
            }
            if question.multiple {
                if !state.answers[state.tab].contains(&value) {
                    state.answers[state.tab].push(value);
                }
            } else {
                state.answers[state.tab] = vec![value];
            }
            state.editing = false;
            state.edit_buffer.clear();
            state.edit_cursor = 0;
            if !question.multiple {
                state.advance_after_single(request);
            }
            return Ok(true);
        }
        KeyCode::Left => state.edit_cursor = state.edit_cursor.saturating_sub(1),
        KeyCode::Right => {
            state.edit_cursor = (state.edit_cursor + 1).min(state.edit_buffer.chars().count())
        }
        KeyCode::Home => state.edit_cursor = 0,
        KeyCode::End => state.edit_cursor = state.edit_buffer.chars().count(),
        KeyCode::Backspace => remove_before_cursor(&mut state.edit_buffer, &mut state.edit_cursor),
        KeyCode::Delete => remove_at_cursor(&mut state.edit_buffer, state.edit_cursor),
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            insert_text(
                &mut state.edit_buffer,
                &mut state.edit_cursor,
                &ch.to_string(),
            );
        }
        _ => {}
    }
    Ok(false)
}

pub(in crate::question_tui) fn insert_text(value: &mut String, cursor: &mut usize, text: &str) {
    let remaining = MAX_CUSTOM_ANSWER_CHARS.saturating_sub(value.chars().count());
    if remaining == 0 {
        return;
    }
    let sanitized = text
        .chars()
        .flat_map(|ch| {
            if ch == '\t' {
                "  ".chars().collect::<Vec<_>>()
            } else if ch == '\n' || !ch.is_control() {
                vec![ch]
            } else {
                Vec::new()
            }
        })
        .take(remaining)
        .collect::<String>();
    let byte = byte_index(value, *cursor);
    value.insert_str(byte, &sanitized);
    *cursor += sanitized.chars().count();
}

pub(in crate::question_tui) fn remove_before_cursor(value: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = byte_index(value, *cursor - 1);
    let end = byte_index(value, *cursor);
    value.replace_range(start..end, "");
    *cursor -= 1;
}

pub(in crate::question_tui) fn remove_at_cursor(value: &mut String, cursor: usize) {
    if cursor >= value.chars().count() {
        return;
    }
    let start = byte_index(value, cursor);
    let end = byte_index(value, cursor + 1);
    value.replace_range(start..end, "");
}

pub(in crate::question_tui) fn byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}
