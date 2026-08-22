//! 检索：分词、全文、语义与它们的融合。
//!
//! 中文要先分词才能进 FTS，所以内置了一个精简的 jieba（`CompactJieba`）——不引
//! 完整词典是因为它占的内存比整个记忆库还大，而这里只需要切得够用。
//!
//! 语义检索是**可选的补充**而非替代：`SEMANTIC_SCORE_WEIGHT` 把它和关键词分数
//! 加权融合，嵌入服务不可用时退回纯关键词，功能不该整个停摆。

use crate::memory::*;

pub(crate) const JIEBA_INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jieba.fst"));

pub(crate) struct CompactJieba {
    pub(crate) words: fst::Map<&'static [u8]>,
    pub(crate) log_total: f64,
    pub(crate) max_word_chars: usize,
}

impl CompactJieba {
    pub(crate) fn new() -> Result<Self> {
        let total_bytes: [u8; 8] = JIEBA_INDEX
            .get(..8)
            .context("compact Jieba index is truncated")?
            .try_into()
            .expect("the total-frequency slice has a fixed length");
        let total = u64::from_le_bytes(total_bytes);
        if total == 0 {
            bail!("compact Jieba index has an empty frequency total");
        }
        let max_word_chars = u32::from_le_bytes(
            JIEBA_INDEX
                .get(8..12)
                .context("compact Jieba index has no maximum word length")?
                .try_into()
                .expect("the maximum-word slice has a fixed length"),
        ) as usize;
        if max_word_chars == 0 {
            bail!("compact Jieba index has an invalid maximum word length");
        }
        Ok(Self {
            words: fst::Map::new(&JIEBA_INDEX[12..]).context("opening compact Jieba index")?,
            log_total: (total as f64).ln(),
            max_word_chars,
        })
    }

    pub(crate) fn cut<'a>(&self, text: &'a str) -> Vec<&'a str> {
        let mut words = Vec::new();
        let mut block_start = None;
        for (index, character) in text.char_indices() {
            if jieba_block_character(character) {
                block_start.get_or_insert(index);
                continue;
            }
            if let Some(start) = block_start.take() {
                self.cut_block(&text[start..index], &mut words);
            }
            let end = index + character.len_utf8();
            words.push(&text[index..end]);
        }
        if let Some(start) = block_start {
            self.cut_block(&text[start..], &mut words);
        }
        words
    }

    pub(crate) fn cut_block<'a>(&self, block: &'a str, words: &mut Vec<&'a str>) {
        let mut boundaries = block
            .char_indices()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        boundaries.push(block.len());
        if boundaries.len() <= 1 {
            return;
        }
        let character_count = boundaries.len() - 1;
        let mut route = vec![(0.0_f64, character_count); character_count + 1];
        for start in (0..character_count).rev() {
            let mut best = (-self.log_total + route[start + 1].0, start + 1);
            let candidate_end = start
                .saturating_add(self.max_word_chars)
                .min(character_count);
            for end in start + 1..=candidate_end {
                let candidate = &block[boundaries[start]..boundaries[end]];
                let Some(frequency) = self.words.get(candidate) else {
                    continue;
                };
                let score = (frequency.max(1) as f64).ln() - self.log_total + route[end].0;
                if score > best.0 || (score == best.0 && end > best.1) {
                    best = (score, end);
                }
            }
            route[start] = best;
        }

        let mut start = 0;
        let mut ascii_start = None;
        while start < character_count {
            let end = route[start].1;
            let token = &block[boundaries[start]..boundaries[end]];
            if token.len() == 1 && token.as_bytes()[0].is_ascii_alphanumeric() {
                ascii_start.get_or_insert(boundaries[start]);
            } else {
                if let Some(byte_start) = ascii_start.take() {
                    words.push(&block[byte_start..boundaries[start]]);
                }
                words.push(token);
            }
            start = end;
        }
        if let Some(byte_start) = ascii_start {
            words.push(&block[byte_start..]);
        }
    }
}

pub(crate) fn jieba_block_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '+' | '#' | '&' | '.' | '_' | '%' | '-')
        || matches!(
            character as u32,
            0x3400..=0x4dbf
                | 0x4e00..=0x9fff
                | 0xf900..=0xfaff
                | 0x20000..=0x2fa1f
        )
}

pub(crate) fn memory_hit_json(hit: &MemoryHit) -> Value {
    json!({
        "id": hit.id,
        "kind": match hit.kind { MemoryKind::Fact => "knowledge", MemoryKind::Diary => "diary" },
        "retention": hit.retention,
        "timestamp": hit.timestamp,
        "score": hit.score,
        "source": hit.source,
        "visibility": hit.visibility,
        "owner_principal": hit.owner_principal,
        "owner_display_name": truncate_chars(&compact_line(&hit.owner_display_name), 128),
        "subjects": serde_json::from_str::<Value>(&hit.subjects).unwrap_or_else(|_| json!([])),
        "content": hit.content,
    })
}

pub(crate) fn sort_json_hits(hits: &mut [Value]) {
    hits.sort_by(|a, b| {
        b.get("score")
            .and_then(Value::as_f64)
            .unwrap_or_default()
            .partial_cmp(&a.get("score").and_then(Value::as_f64).unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// FTS5 terms are OR-ed: a paraphrase usually shares only part of its wording
/// with the record, and requiring every term would push recall to zero on the
/// exact queries this is for.
/// Keyword hits at or above this are already good enough that the embedding
/// round trip would only add latency.
pub(crate) const SEMANTIC_SKIP_SCORE: f64 = 40.0;

/// Rows embedded per search; the backlog fills in over successive calls rather
/// than making one unlucky search pay for the whole archive.
pub(crate) const SEMANTIC_EMBED_BATCH: usize = 64;

pub(crate) const SEMANTIC_CORPUS_LIMIT: usize = 500;

/// Semantic hits are supporting evidence, not the primary ranking; keyword
/// scores run an order of magnitude higher and should keep the top slots when
/// they matched at all.
pub(crate) const SEMANTIC_SCORE_WEIGHT: f32 = 30.0;

pub(crate) fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (a, b) in left.iter().zip(right.iter()) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

/// Semantic hits reinforce a record the keywords already found rather than
/// displacing it; a record only the embedding saw joins on its own.
pub(crate) fn merge_evicted_hits(base: &mut Value, semantic: Vec<Value>, limit: usize) {
    let Some(hits) = base["results"].as_array_mut() else {
        return;
    };
    for item in semantic {
        let id = item["id"].clone();
        if let Some(existing) = hits.iter_mut().find(|hit| hit["id"] == id) {
            let boost = item["score"].as_f64().unwrap_or(0.0) * 0.6;
            let score = existing["score"].as_f64().unwrap_or(0.0) + boost;
            existing["score"] = json!(score);
            existing["semantic"] = json!(true);
        } else {
            hits.push(item);
        }
    }
    sort_json_hits(hits);
    hits.truncate(limit);
}

pub(crate) fn build_evicted_fts_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// `normalized_query` 需已 `compact_line` + 小写化:归一化与被打分的行无关,
/// 调用方在循环外做一次,而不是在每一行上重复三次分配。
pub(crate) fn score_text(text: &str, normalized_query: &str, tokens: &[String]) -> f32 {
    if tokens.is_empty() {
        return 0.0;
    }
    let lower = text.to_ascii_lowercase();
    let mut score = 0.0;
    let mut matched = HashSet::new();
    for token in tokens {
        if lower.contains(token) {
            score += 8.0 + token.chars().count().min(8) as f32;
            matched.insert(token);
        }
    }
    if !normalized_query.is_empty() && lower.contains(normalized_query) {
        score += 20.0;
    }
    score + matched.len() as f32 / tokens.len() as f32 * 24.0
}

pub(crate) fn query_tokens(query: &str) -> Vec<String> {
    query_tokens_with_limit(query, 64)
}

pub(crate) fn query_tokens_with_limit(query: &str, limit: usize) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    for token in JIEBA.cut(query) {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty()
            || !token
                .chars()
                .any(|character| character.is_alphanumeric() || !character.is_ascii())
        {
            continue;
        }
        let chars = token.chars().count();
        if chars >= 2 || (chars == 1 && !token.is_ascii()) {
            tokens.insert(token);
        }
    }
    for token in
        query.split(|character: char| character.is_whitespace() || character.is_ascii_punctuation())
    {
        let token = token.trim().to_ascii_lowercase();
        if token.chars().count() >= 2 {
            tokens.insert(token);
        }
    }
    tokens.into_iter().take(limit).collect()
}

pub(crate) fn snippet(text: &str, tokens: &[String], max_chars: usize) -> String {
    let lower = text.to_ascii_lowercase();
    let start = tokens
        .iter()
        .filter_map(|token| lower.find(token))
        .min()
        .unwrap_or(0);
    let start = text[..start.min(text.len())]
        .char_indices()
        .rev()
        .nth(max_chars / 4)
        .map(|(index, _)| index)
        .unwrap_or(0);
    truncate_chars(&text[start..], max_chars)
}

pub(crate) fn compact_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    format!(
        "{}...",
        text.chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
}
