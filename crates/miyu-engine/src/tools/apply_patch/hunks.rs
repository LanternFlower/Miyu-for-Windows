use super::*;

pub(super) fn preflight_operations(operations: Vec<Operation>) -> Result<Vec<FileChange>> {
    let mut staged: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut changes = Vec::new();

    for operation in operations {
        match operation {
            Operation::Add { path, lines } => {
                if staged
                    .get(&path)
                    .and_then(|content| content.as_ref())
                    .is_some()
                    || (!staged.contains_key(&path) && path.exists())
                {
                    bail!(
                        "apply_patch verification failed: file already exists: {}",
                        path.display()
                    )
                }
                let after = ensure_trailing_newline(lines.join("\n"));
                staged.insert(path.clone(), Some(after.clone()));
                changes.push(FileChange {
                    path,
                    before: String::new(),
                    after,
                    kind: ChangeKind::Add,
                });
            }
            Operation::Delete { path } => {
                let before = staged_content(&path, &staged)?;
                staged.insert(path.clone(), None);
                changes.push(FileChange {
                    path,
                    before,
                    after: String::new(),
                    kind: ChangeKind::Delete,
                });
            }
            Operation::Update {
                path,
                move_to,
                hunks,
            } => {
                if move_to.is_some() {
                    bail!("apply_patch verification failed: Move to is not supported yet")
                }
                let before = staged_content(&path, &staged)?;
                // CRLF 文件的匹配靠 TrimEnd 模式成功,但替换进来的新行是
                // LF:统一按 LF 应用,写回前还原原文件的行尾风格,避免混合。
                let crlf = before.contains("\r\n");
                let mut after = if crlf {
                    before.replace("\r\n", "\n")
                } else {
                    before.clone()
                };
                for hunk in hunks {
                    after = apply_hunk(&path, &after, &hunk)?;
                }
                if crlf {
                    after = after.replace('\n', "\r\n");
                }
                staged.insert(path.clone(), Some(after.clone()));
                changes.push(FileChange {
                    path,
                    before,
                    after,
                    kind: ChangeKind::Update,
                });
            }
        }
    }

    Ok(changes)
}

fn staged_content(path: &Path, staged: &HashMap<PathBuf, Option<String>>) -> Result<String> {
    if let Some(content) = staged.get(path) {
        return content.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "apply_patch verification failed: file was deleted earlier in patch: {}",
                path.display()
            )
        });
    }
    std::fs::read_to_string(path).map_err(|err| {
        anyhow::anyhow!(
            "apply_patch verification failed: failed to read file to update {}: {err}",
            path.display()
        )
    })
}

pub(super) fn apply_hunk(path: &Path, content: &str, hunk: &Hunk) -> Result<String> {
    let old = hunk_text(&hunk.lines, false);
    let new = hunk_text(&hunk.lines, true);
    if old.is_empty() {
        return apply_insertion_hunk(path, content, hunk, &new);
    }
    let current_lines = content_lines(content);
    let mut pattern = content_lines(&old);
    let new_lines = content_lines(&new);
    let start_index = hunk
        .context
        .as_ref()
        .and_then(|context| seek_sequence(&current_lines, &[context.to_string()], 0, false))
        .map(|index| index + 1)
        .unwrap_or(0);
    // 无 @@ 锚点的 hunk 在文件中多处匹配时,首命中会落到错误位置还返回
    // ok:与 fallback 子串路径的 count>1 拒绝对齐。带锚点/EOF 的视为已消歧。
    let ambiguous = |found: usize, pattern: &[String]| {
        hunk.context.is_none()
            && !hunk.end_of_file
            && seek_sequence(&current_lines, pattern, found + 1, false).is_some()
    };
    if let Some(found) = seek_sequence(&current_lines, &pattern, start_index, hunk.end_of_file) {
        if ambiguous(found, &pattern) {
            bail!(
                "apply_patch verification failed: hunk matches multiple locations in {}; add a @@ context anchor",
                path.display()
            )
        }
        let mut result = current_lines.clone();
        result.splice(found..found + pattern.len(), new_lines);
        return Ok(join_lines(result));
    }
    if pattern.last().is_some_and(|line| line.is_empty()) {
        pattern.pop();
        if let Some(found) = seek_sequence(&current_lines, &pattern, start_index, hunk.end_of_file)
        {
            if ambiguous(found, &pattern) {
                bail!(
                    "apply_patch verification failed: hunk matches multiple locations in {}; add a @@ context anchor",
                    path.display()
                )
            }
            let mut result = current_lines.clone();
            result.splice(found..found + pattern.len(), content_lines(&new));
            return Ok(join_lines(result));
        }
    }

    // Fallback for legacy simple hunks: keep exact substring replacement.
    if old.is_empty() {
        bail!(
            "apply_patch verification failed: empty update context for {}",
            path.display()
        )
    }
    let count = count_occurrences(content, &old);
    if count == 0 {
        bail!(
            "apply_patch verification failed: hunk does not match {}",
            path.display()
        )
    }
    if count > 1 {
        bail!(
            "apply_patch verification failed: hunk matches multiple locations in {}",
            path.display()
        )
    }
    Ok(content.replacen(&old, &new, 1))
}

fn apply_insertion_hunk(path: &Path, content: &str, hunk: &Hunk, new: &str) -> Result<String> {
    let mut lines = content_lines(content);
    let insert = content_lines(new);
    let index = if hunk.end_of_file {
        lines.len()
    } else if let Some(context) = &hunk.context {
        seek_sequence(&lines, &[context.to_string()], 0, false)
            .map(|index| index + 1)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "apply_patch verification failed: failed to find context '{}' in {}",
                    context,
                    path.display()
                )
            })?
    } else {
        lines.len()
    };
    lines.splice(index..index, insert);
    Ok(join_lines(lines))
}

fn hunk_text(lines: &[HunkLine], new_side: bool) -> String {
    let selected = lines
        .iter()
        .filter_map(|line| match (new_side, line) {
            (_, HunkLine::Context(text)) => Some(text.as_str()),
            (false, HunkLine::Delete(text)) => Some(text.as_str()),
            (true, HunkLine::Insert(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure_trailing_newline(selected.join("\n"))
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.is_empty() && !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

fn content_lines(content: &str) -> Vec<String> {
    let mut lines = content.split('\n').map(str::to_string).collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn join_lines(mut lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    lines.push(String::new());
    lines.join("\n")
}

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    if eof {
        let from_end = lines.len().checked_sub(pattern.len())?;
        if from_end >= start_index && lines_match_at(lines, pattern, from_end, CompareMode::Exact) {
            return Some(from_end);
        }
    }
    for mode in [
        CompareMode::Exact,
        CompareMode::TrimEnd,
        CompareMode::Trim,
        CompareMode::Normalize,
    ] {
        for index in start_index..=lines.len().saturating_sub(pattern.len()) {
            if lines_match_at(lines, pattern, index, mode) {
                return Some(index);
            }
        }
    }
    None
}

#[derive(Clone, Copy)]
enum CompareMode {
    Exact,
    TrimEnd,
    Trim,
    Normalize,
}

fn lines_match_at(lines: &[String], pattern: &[String], index: usize, mode: CompareMode) -> bool {
    pattern.iter().enumerate().all(|(offset, expected)| {
        let Some(actual) = lines.get(index + offset) else {
            return false;
        };
        match mode {
            CompareMode::Exact => actual == expected,
            CompareMode::TrimEnd => actual.trim_end() == expected.trim_end(),
            CompareMode::Trim => actual.trim() == expected.trim(),
            CompareMode::Normalize => normalize_match(actual) == normalize_match(expected),
        }
    })
}

fn normalize_match(value: &str) -> String {
    value
        .trim()
        .replace(['‘', '’', '‚', '‛'], "'")
        .replace(['“', '”', '„', '‟'], "\"")
        .replace(['‐', '‑', '‒', '–', '—', '―'], "-")
        .replace('…', "...")
        .replace('\u{00a0}', " ")
}

fn count_occurrences(content: &str, search: &str) -> usize {
    if search.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut offset = 0;
    while let Some(pos) = content[offset..].find(search) {
        count += 1;
        offset += pos + search.len();
    }
    count
}
