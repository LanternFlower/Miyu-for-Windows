//! 注入对话的联想段。
//!
//! 这段文本会进上下文，所以**格式必须稳定**：`association_format_always_keeps_its_closing_boundary`
//! 守着闭合边界，因为半截的块会被后面的化石回放当成正文的一部分。
//!
//! 去重是必须的（`association_dedup_filters_visible_lines_and_keeps_changed_ones`）：
//! 已经在可见历史里出现过的内容再注一遍，既浪费 token 又会让模型以为被强调了。

use crate::memory::*;

#[derive(Debug, Clone)]
/// 联想召回的自回声排除:`session_id` 会话里、`since`(最老可见轮的
/// 时间戳,Utc RFC3339,与记忆行同源可比)之后产生的记忆不注入。
pub struct AssociationExclusion {
    pub session_id: String,
    pub since: String,
}

pub struct AssociationContext {
    pub facts: Vec<MemoryHit>,
    pub episodes: Vec<MemoryHit>,
    pub(crate) organization_due: bool,
}

/// 渲染单条联想记忆行（含结尾换行），与注入块中的字节完全一致。
/// 整行同时充当跨回合去重键：内容或日期变化的记忆会渲染出不同的行，
/// 因而被视为新条目重新注入。
pub(crate) fn association_entry_line(hit: &MemoryHit, access: &MemoryAccess, entry_max_chars: usize) -> String {
    let label = match (access, hit.visibility.as_str()) {
        (_, VISIBILITY_PUBLIC) => "公共知识".to_string(),
        (MemoryAccess::Privileged, VISIBILITY_PRINCIPAL) => format!(
            "归属={}{}",
            hit.owner_principal,
            if hit.owner_display_name.trim().is_empty() {
                String::new()
            } else {
                format!(
                    "，记录昵称={}",
                    truncate_chars(&compact_line(&hit.owner_display_name), 128)
                )
            }
        ),
        (MemoryAccess::Principal(_), VISIBILITY_PRINCIPAL) => "当前用户记忆".to_string(),
        _ => "仅管理员".to_string(),
    };
    let mut content = compact_line(&hit.content);
    // 短期日记正文自带 RFC3339 前缀（diary_content），加日期标签后去重
    if let Some(rest) = content
        .strip_prefix(hit.timestamp.as_str())
        .and_then(|rest| rest.strip_prefix('，'))
    {
        content = rest.to_string();
    }
    let date = association_date(&hit.timestamp);
    // organizer 写的日记常以「YYYY-MM-DD，」开头，与日期标签相同时也去重
    if let Some(date) = date.as_deref() {
        if let Some(rest) = content
            .strip_prefix(date)
            .and_then(|rest| rest.strip_prefix('，'))
        {
            content = rest.to_string();
        }
    }
    // 单条上限(08-17):日记正文常把当时那条完整回复整段存了进来,实测一条
    // 400+ 字符。截断后带上 id,模型要看全文就 recall_memories(id=…)。
    // 知识点天然短,同一把尺子对它基本不生效。
    if entry_max_chars > 0 && content.chars().count() > entry_max_chars {
        content = format!(
            "{}…（全文：recall_memories id={}）",
            content.chars().take(entry_max_chars).collect::<String>(),
            hit.id
        );
    }
    let id = match hit.kind {
        MemoryKind::Diary => format!("[e{}] ", hit.id),
        MemoryKind::Fact => String::new(),
    };
    match date {
        Some(date) => format!("- {id}[{date}] [{label}] {content}\n"),
        None => format!("- {id}[{label}] {content}\n"),
    }
}

pub(crate) fn append_association_section<'a>(
    output: &mut String,
    title: &str,
    hits: impl IntoIterator<Item = &'a MemoryHit>,
    access: &MemoryAccess,
    max_chars: usize,
    entry_max_chars: usize,
    closing: &str,
) {
    let heading = format!("\n{title}：\n");
    let mut section = String::new();
    for hit in hits {
        let line = association_entry_line(hit, access, entry_max_chars);
        let total = output.chars().count()
            + heading.chars().count()
            + section.chars().count()
            + line.chars().count()
            + closing.chars().count();
        if total <= max_chars {
            section.push_str(&line);
        }
    }
    if !section.is_empty() {
        output.push_str(&heading);
        output.push_str(&section);
    }
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339()
}

/// RFC3339 时间戳 → 本地日期（用于关联记忆展示；解析失败返回 None）
pub(crate) fn association_date(timestamp: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|value| value.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
}

pub(crate) fn diary_content(created_at: &str, user_message: &str, assistant_message: &str) -> String {
    // 第一人称的互动记忆,不是工单:归属(谁说的)由注入行的 [归属=…] 标签
    // 承担,昵称是可改的不可信字段,不进正文。
    format!(
        "{}，对方说：{}；我回：{}",
        created_at,
        truncate_chars(&compact_line(user_message), 260),
        truncate_chars(&compact_line(assistant_message), 520)
    )
}
