use crate::config::{AppConfig, KnowledgeBasePluginConfig, MemoryConfig};
use crate::paths::MiyuPaths;
use crate::platforms::PlatformPrincipal;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

mod organizer;

pub(crate) use organizer::{MemoryOrganizer, MemoryOrganizerHandle};

const SHORT_TERM: &str = "short_term";
const LONG_TERM: &str = "long_term";
const VISIBILITY_PUBLIC: &str = "public";
const VISIBILITY_PRINCIPAL: &str = "principal";
const VISIBILITY_PRIVILEGED: &str = "privileged";
const MAX_ORGANIZED_ITEMS: usize = 20;
const JIEBA_INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jieba.fst"));
static JIEBA: LazyLock<CompactJieba> = LazyLock::new(|| {
    CompactJieba::new().expect("the build-generated compact Jieba index must be valid")
});

struct CompactJieba {
    words: fst::Map<&'static [u8]>,
    log_total: f64,
    max_word_chars: usize,
}

impl CompactJieba {
    fn new() -> Result<Self> {
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

    fn cut<'a>(&self, text: &'a str) -> Vec<&'a str> {
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

    fn cut_block<'a>(&self, block: &'a str, words: &mut Vec<&'a str>) {
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

fn jieba_block_character(character: char) -> bool {
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

#[derive(Clone)]
pub struct MemoryStore {
    config: MemoryConfig,
    kb_config: KnowledgeBasePluginConfig,
    /// Kept whole because the embedding call needs provider lookup and the
    /// knowledge base's timeout setting.
    app_config: AppConfig,
    writes_enabled: bool,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
    data_db: PathBuf,
    state_db: PathBuf,
    skills_dir: PathBuf,
}

/// Read authorization for one agent run. Storage remains persona-global; this
/// value only controls which rows may enter the model context or memory tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MemoryAccess {
    Privileged,
    Principal(String),
}

impl MemoryAccess {
    pub(crate) fn principal(key: impl Into<String>) -> Self {
        Self::Principal(key.into())
    }

    fn principal_key(&self) -> Option<&str> {
        match self {
            Self::Privileged => None,
            Self::Principal(key) => Some(key),
        }
    }
}

#[derive(Clone, Debug)]
struct MemoryOwnership {
    visibility: &'static str,
    owner_principal: String,
    owner_display_name: String,
}

impl MemoryOwnership {
    fn public() -> Self {
        Self {
            visibility: VISIBILITY_PUBLIC,
            owner_principal: String::new(),
            owner_display_name: String::new(),
        }
    }

    fn privileged() -> Self {
        Self {
            visibility: VISIBILITY_PRIVILEGED,
            owner_principal: String::new(),
            owner_display_name: String::new(),
        }
    }

    fn principal(key: impl Into<String>, display_name: impl Into<String>) -> Self {
        let display_name = display_name.into();
        Self {
            visibility: VISIBILITY_PRINCIPAL,
            owner_principal: key.into(),
            owner_display_name: truncate_chars(&compact_line(&display_name), 128),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EvictedTurn {
    pub source_id: String,
    pub timestamp: String,
    pub role: String,
    pub content: String,
    pub visibility: String,
    pub owner_principal: String,
    pub owner_display_name: String,
}

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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MemoryKind {
    Fact,
    Diary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct MemoryOrigin {
    pub(crate) kind: String,
    pub(crate) platform: String,
    pub(crate) account_id: String,
    pub(crate) conversation_kind: String,
    pub(crate) conversation_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_display_name: String,
    pub(crate) session_id: String,
    pub(crate) message_id: String,
}

impl MemoryOrigin {
    pub(crate) fn local(session_id: impl Into<String>) -> Self {
        Self {
            kind: "local".to_string(),
            session_id: session_id.into(),
            ..Self::default()
        }
    }

    fn principal_ownership(&self) -> Option<MemoryOwnership> {
        if self.kind != "platform"
            || self.platform.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.sender_id.trim().is_empty()
        {
            return None;
        }
        Some(MemoryOwnership::principal(
            PlatformPrincipal {
                platform: self.platform.clone(),
                account_id: self.account_id.clone(),
                user_id: self.sender_id.clone(),
            }
            .stable_key(),
            self.sender_display_name.trim(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub id: i64,
    pub kind: MemoryKind,
    pub content: String,
    pub score: f32,
    pub timestamp: String,
    pub source: String,
    pub retention: Option<String>,
    visibility: String,
    owner_principal: String,
    owner_display_name: String,
    subjects: String,
    source_episode_ids: Vec<i64>,
    origin_session_id: String,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub(crate) struct MemorySubject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ShortDiaryRecord {
    pub(crate) id: i64,
    pub(crate) created_at: String,
    pub(crate) user_message: String,
    pub(crate) assistant_message: String,
    pub(crate) force_long_term: bool,
    pub(crate) owner_principal: Option<String>,
    pub(crate) origin: MemoryOrigin,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExistingMemoryRecord {
    pub(crate) id: i64,
    pub(crate) kind: String,
    pub(crate) content: String,
    pub(crate) truth_status: String,
    pub(crate) visibility: String,
    pub(crate) owner_principal: String,
    pub(crate) owner_display_name: String,
}

#[derive(Debug, Clone)]
pub(crate) struct OrganizationBatch {
    pub(crate) database_id: String,
    pub(crate) generation: i64,
    pub(crate) diaries: Vec<ShortDiaryRecord>,
    pub(crate) existing: Vec<ExistingMemoryRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OrganizedOutput {
    #[serde(default)]
    pub(crate) knowledge: Vec<KnowledgeAction>,
    #[serde(default)]
    pub(crate) long_diaries: Vec<LongDiaryDraft>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KnowledgeAction {
    pub(crate) operation: String,
    #[serde(default)]
    pub(crate) target_id: Option<i64>,
    pub(crate) memory_type: String,
    pub(crate) content: String,
    pub(crate) truth_status: String,
    pub(crate) importance: i64,
    pub(crate) confidence: f64,
    #[serde(default)]
    pub(crate) visibility: String,
    #[serde(default)]
    pub(crate) subjects: Vec<MemorySubject>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) diary_ids: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LongDiaryDraft {
    pub(crate) content: String,
    pub(crate) importance: i64,
    pub(crate) confidence: f64,
    #[serde(default)]
    pub(crate) visibility: String,
    #[serde(default)]
    pub(crate) subjects: Vec<MemorySubject>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) diary_ids: Vec<i64>,
}

impl MemoryStore {
    pub fn new(config: &AppConfig, paths: &MiyuPaths) -> Self {
        let data_dir = config.active_persona_memory_data_dir(paths).join("memory");
        let state_dir = config.active_persona_memory_state_dir(paths).join("memory");
        Self {
            config: config.memory_config().clone(),
            kb_config: config.plugins.knowledge_base.clone(),
            app_config: config.clone(),
            writes_enabled: true,
            access: MemoryAccess::Privileged,
            writer_principal: None,
            writer_display_name: String::new(),
            data_db: data_dir.join("memory.db"),
            state_db: state_dir.join("evicted_context.db"),
            skills_dir: config.active_persona_skills_dir(paths),
        }
    }

    pub(crate) fn set_writes_enabled(&mut self, enabled: bool) {
        self.writes_enabled = enabled;
    }

    pub(crate) fn set_request_context(
        &mut self,
        access: MemoryAccess,
        writer_principal: Option<String>,
        writer_display_name: impl Into<String>,
    ) {
        self.access = access;
        self.writer_principal = writer_principal.filter(|value| !value.trim().is_empty());
        self.writer_display_name = writer_display_name.into().trim().to_string();
    }

    pub(crate) fn request_context(&self) -> (MemoryAccess, Option<String>, String) {
        (
            self.access.clone(),
            self.writer_principal.clone(),
            self.writer_display_name.clone(),
        )
    }

    pub(crate) fn with_request_context(
        mut self,
        access: MemoryAccess,
        writer_principal: Option<String>,
        writer_display_name: impl Into<String>,
    ) -> Self {
        self.set_request_context(access, writer_principal, writer_display_name);
        self
    }

    fn automatic_ownership(&self, origin: &MemoryOrigin) -> MemoryOwnership {
        origin
            .principal_ownership()
            .unwrap_or_else(MemoryOwnership::privileged)
    }

    fn writer_ownership(&self) -> MemoryOwnership {
        self.writer_principal
            .as_ref()
            .map(|principal| {
                MemoryOwnership::principal(principal.clone(), self.writer_display_name.clone())
            })
            .unwrap_or_else(MemoryOwnership::privileged)
    }

    pub(crate) fn apply_evicted_ownership(&self, turns: &mut [EvictedTurn]) {
        let ownership = self.writer_ownership();
        for turn in turns {
            turn.visibility = ownership.visibility.to_string();
            turn.owner_principal.clone_from(&ownership.owner_principal);
            turn.owner_display_name
                .clone_from(&ownership.owner_display_name);
        }
    }

    fn manual_fact_ownership(&self) -> MemoryOwnership {
        match self.writer_principal.as_ref() {
            Some(principal) => {
                MemoryOwnership::principal(principal.clone(), self.writer_display_name.clone())
            }
            None => MemoryOwnership::privileged(),
        }
    }

    pub fn init(&self) -> Result<()> {
        if let Some(parent) = self.data_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.state_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        init_data_db(&self.data_conn()?)?;
        init_state_db(&self.state_conn()?)?;
        self.decay_memories()?;
        Ok(())
    }

    pub(crate) fn identity(&self) -> Result<(String, i64)> {
        if !self.data_db.is_file() {
            self.init()?;
        }
        Ok(self.data_conn_existing()?.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    fn init_existing(&self) -> Result<()> {
        let conn = self.data_conn_existing()?;
        init_data_db(&conn)?;
        self.decay_memories_with_conn(&conn)
    }

    #[allow(dead_code)]
    pub fn remember_evicted_turns(&self, turns: &[EvictedTurn]) -> Result<()> {
        if !self.config.enabled
            || !self.writes_enabled
            || !self.config.evicted_context_enabled
            || turns.is_empty()
        {
            return Ok(());
        }
        self.init()?;
        let fallback = self.writer_ownership();
        let mut conn = self.state_conn()?;
        let tx = conn.transaction()?;
        for turn in turns {
            let visibility = if turn.visibility.trim().is_empty() {
                fallback.visibility
            } else {
                turn.visibility.as_str()
            };
            let owner_principal = if turn.owner_principal.trim().is_empty() {
                fallback.owner_principal.as_str()
            } else {
                turn.owner_principal.as_str()
            };
            let owner_display_name = if turn.owner_display_name.trim().is_empty() {
                fallback.owner_display_name.as_str()
            } else {
                turn.owner_display_name.as_str()
            };
            tx.execute(
                "INSERT OR IGNORE INTO evicted_turns (
                    source_id, timestamp, role, content, created_at,
                    visibility, owner_principal, owner_display_name
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    turn.source_id,
                    turn.timestamp,
                    turn.role,
                    turn.content,
                    now(),
                    visibility,
                    owner_principal,
                    owner_display_name,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn prepare_evicted_context_db(&self) -> Result<Option<PathBuf>> {
        if !self.config.enabled || !self.writes_enabled || !self.config.evicted_context_enabled {
            return Ok(None);
        }
        self.init()?;
        Ok(Some(self.state_db.clone()))
    }

    pub fn clear_evicted_context(&self) -> Result<()> {
        self.init()?;
        self.state_conn()?
            .execute("DELETE FROM evicted_turns", [])?;
        Ok(())
    }

    pub fn clear_pending_events(&self) -> Result<()> {
        self.init()?;
        let data = self.data_conn()?;
        data.execute("DELETE FROM pending_events", [])?;
        data.execute(
            "DELETE FROM sqlite_sequence WHERE name = 'pending_events'",
            [],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn search_evicted_context(&self, query: &str, limit: usize) -> Result<Value> {
        self.init()?;
        self.search_evicted_context_existing(query, limit)
    }

    pub fn search_evicted_context_readonly(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Value> {
        if !self.state_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "results": [] }));
        }
        self.search_evicted_context_filtered(query, limit, start, end)
    }

    /// Keyword first, semantics only when the keywords came back weak — the
    /// same shape the knowledge base uses. Exact terms (error codes, package
    /// names) are what keyword matching is best at and what most of these
    /// lookups are; the embedding pass is for "what were we talking about",
    /// where the record says `[ERRO]` and the question says 报错.
    ///
    /// Every embedding step is best effort. The service being unreachable, or
    /// having produced no vectors yet, must never turn a working keyword search
    /// into a failure.
    pub async fn search_evicted_context_hybrid(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Value> {
        let mut base = self.search_evicted_context_readonly(query, limit, start, end)?;
        let strongest = base["results"]
            .as_array()
            .and_then(|hits| hits.first())
            .and_then(|hit| hit["score"].as_f64())
            .unwrap_or(0.0);
        if !self.semantic_enabled() || strongest >= SEMANTIC_SKIP_SCORE {
            return Ok(base);
        }
        let semantic = match self.semantic_evicted_hits(query, limit, start, end).await {
            Ok(hits) => hits,
            Err(error) => {
                tracing::debug!(error = %error, "evicted-context semantic pass unavailable");
                return Ok(base);
            }
        };
        if semantic.is_empty() {
            return Ok(base);
        }
        merge_evicted_hits(&mut base, semantic, limit);
        Ok(base)
    }

    /// Rows are embedded on demand rather than at eviction time: pop must not
    /// wait on a network round trip, and a record nobody ever searches for
    /// never costs an embedding. Each call tops up a bounded slice of the
    /// backlog, so coverage fills in over successive searches.
    async fn semantic_evicted_hits(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Vec<Value>> {
        let embedding = &self.app_config.embedding;
        let mut provider = self
            .config_provider(embedding.provider_id.trim())
            .context("embedding provider is not configured")?;
        let model = embedding.model.trim().to_string();
        provider.default_model = model.clone();

        let corpus = self.semantic_corpus(start, end)?;
        let missing: Vec<(i64, String)> = {
            let conn = self.state_conn()?;
            let mut pending = Vec::new();
            for (id, content) in &corpus {
                if pending.len() >= SEMANTIC_EMBED_BATCH {
                    break;
                }
                let known: Option<String> = conn
                    .query_row(
                        "SELECT model FROM evicted_embeddings WHERE id = ?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .ok();
                if known.as_deref() != Some(model.as_str()) {
                    pending.push((*id, content.clone()));
                }
            }
            pending
        };
        for (id, content) in missing {
            let Ok(vector) = crate::tools::knowledge_base::embed_text(
                &self.app_config,
                &provider,
                &model,
                &content,
            )
            .await
            else {
                break;
            };
            let conn = self.state_conn()?;
            conn.execute(
                "INSERT INTO evicted_embeddings (id, model, embedding_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET
                    model = excluded.model,
                    embedding_json = excluded.embedding_json,
                    created_at = excluded.created_at",
                params![id, model, serde_json::to_string(&vector)?, now()],
            )?;
        }

        let query_vector =
            crate::tools::knowledge_base::embed_text(&self.app_config, &provider, &model, query)
                .await?;
        let conn = self.state_conn()?;
        let mut hits = Vec::new();
        for (id, content) in &corpus {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT embedding_json FROM evicted_embeddings WHERE id = ?1 AND model = ?2",
                    params![id, model],
                    |row| row.get(0),
                )
                .ok();
            let Some(stored) = stored else { continue };
            let Ok(vector) = serde_json::from_str::<Vec<f32>>(&stored) else {
                continue;
            };
            let score = cosine_similarity(&query_vector, &vector);
            if score < self.app_config.embedding.min_score {
                continue;
            }
            hits.push(json!({
                "id": id,
                "score": score * SEMANTIC_SCORE_WEIGHT,
                "semantic": true,
                "snippet": truncate_chars(&compact_line(content), 400),
            }));
        }
        sort_json_hits(&mut hits);
        hits.truncate(limit);
        Ok(hits)
    }

    fn config_provider(&self, id: &str) -> Option<crate::config::ProviderConfig> {
        if id.is_empty() {
            return None;
        }
        self.app_config.provider(Some(id)).ok().cloned()
    }

    /// Newest rows only, and bounded: this pass answers "what were we talking
    /// about", which is a recency question, and an unbounded corpus would make
    /// every miss pay for the whole archive.
    fn semantic_corpus(
        &self,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Vec<(i64, String)>> {
        let conn = self.state_conn()?;
        let mut clauses = Vec::new();
        let mut params: Vec<String> = Vec::new();
        if let Some(principal) = self.access.principal_key() {
            params.push(principal.to_string());
            clauses.push(format!(
                "(visibility='public' OR (visibility='principal' AND owner_principal=?{}))",
                params.len()
            ));
        }
        if let Some(start) = start {
            params.push(start.to_string());
            clauses.push(format!("timestamp >= ?{}", params.len()));
        }
        if let Some(end) = end {
            params.push(end.to_string());
            clauses.push(format!("timestamp <= ?{}", params.len()));
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT id, content FROM evicted_turns {where_clause}
              ORDER BY id DESC LIMIT {SEMANTIC_CORPUS_LIMIT}"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// No switch of its own: an embedding model being configured is what makes
    /// the semantic pass available, and the keyword path stands on its own when
    /// it is not.
    fn semantic_enabled(&self) -> bool {
        self.app_config.embedding.is_configured()
    }

    fn search_evicted_context_existing(&self, query: &str, limit: usize) -> Result<Value> {
        self.search_evicted_context_filtered(query, limit, None, None)
    }

    /// `start`/`end` are RFC 3339 bounds on the stored timestamp. "What were we
    /// talking about this morning" is a question about *when*, and time is a
    /// far stronger signal there than any keyword — the log says `[ERRO]` where
    /// the question says 报错.
    fn search_evicted_context_filtered(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Value> {
        let tokens = query_tokens(query);
        let conn = self.state_conn()?;
        let mut clauses = Vec::new();
        let mut params: Vec<String> = Vec::new();
        if let Some(principal) = self.access.principal_key() {
            params.push(principal.to_string());
            clauses.push(format!(
                "(visibility='public' OR (visibility='principal' AND owner_principal=?{}))",
                params.len()
            ));
        }
        if let Some(start) = start {
            params.push(start.to_string());
            clauses.push(format!("timestamp >= ?{}", params.len()));
        }
        if let Some(end) = end {
            params.push(end.to_string());
            clauses.push(format!("timestamp <= ?{}", params.len()));
        }
        // The trigram index does the filtering, so the scan no longer has to be
        // capped at the newest 1000 rows — those beyond it used to be stored
        // forever and reachable never.
        if !tokens.is_empty() {
            // Trigram index: terms shorter than three characters cannot be
            // matched by it, so those fall through to the scoring pass below
            // rather than narrowing the candidate set.
            let indexed: Vec<String> = tokens
                .iter()
                .filter(|token| token.chars().count() >= 3)
                .cloned()
                .collect();
            if !indexed.is_empty() {
                params.push(build_evicted_fts_query(&indexed));
                clauses.push(format!(
                    "id IN (SELECT rowid FROM evicted_turns_fts WHERE evicted_turns_fts MATCH ?{})",
                    params.len()
                ));
            }
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT id, timestamp, role, content, visibility,
                    owner_principal, owner_display_name
               FROM evicted_turns {where_clause}
              ORDER BY id DESC"
        ))?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let normalized_query = compact_line(query).to_ascii_lowercase();
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            let timestamp = row.get::<_, String>(1)?;
            let role = row.get::<_, String>(2)?;
            let content = row.get::<_, String>(3)?;
            let visibility = row.get::<_, String>(4)?;
            let owner_principal = row.get::<_, String>(5)?;
            let owner_display_name = row.get::<_, String>(6)?;
            let score = score_text(&content, &normalized_query, &tokens);
            if score <= 0.0 {
                continue;
            }
            hits.push(json!({
                "id": id,
                "timestamp": timestamp,
                "role": role,
                "score": score,
                "visibility": visibility,
                "owner_principal": owner_principal,
                "owner_display_name": truncate_chars(&compact_line(&owner_display_name), 128),
                "snippet": snippet(&content, &tokens, self.kb_config.snippet_context_chars),
            }));
        }
        sort_json_hits(&mut hits);
        hits.truncate(limit.clamp(1, 50));
        Ok(json!({ "ok": true, "query": query, "results": hits }))
    }

    pub fn remember_fact(&self, content: &str, source: &str) -> Result<i64> {
        if !self.config.enabled || !self.writes_enabled || content.trim().is_empty() {
            return Ok(0);
        }
        self.init()?;
        let ownership = self.manual_fact_ownership();
        let subjects = ownership_subjects_json(&ownership);
        let conn = self.data_conn()?;
        conn.execute(
            "INSERT INTO facts (
                content, source, status, confidence, recall_count, created_at, updated_at,
                visibility, owner_principal, owner_display_name, subjects
             ) VALUES (?1, ?2, 'active', 1.0, 0, ?3, ?3, ?4, ?5, ?6, ?7)",
            params![
                content.trim(),
                source.trim(),
                now(),
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn remember_pending_event(
        &self,
        user_message: &str,
        assistant_message: &str,
    ) -> Result<()> {
        if !self.config.enabled || !self.writes_enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        self.data_conn()?.execute(
            "INSERT INTO pending_events (user_message, assistant_message, created_at) VALUES (?1, ?2, ?3)",
            params![user_message.trim(), assistant_message.trim(), now()],
        )?;
        Ok(())
    }

    pub fn process_after_turn(
        &self,
        user_message: &str,
        assistant_message: &str,
        origin: &MemoryOrigin,
        expected_database_id: &str,
        expected_generation: i64,
    ) -> Result<bool> {
        if !self.writes_enabled || !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(false);
        }
        if !self.data_db.is_file() {
            self.init()?;
        }
        let created_at = now();
        let expires_at = (Utc::now()
            + ChronoDuration::days(self.config.short_diary_retention_days as i64))
        .to_rfc3339();
        let content = diary_content(&created_at, user_message, assistant_message);
        let ownership = self.automatic_ownership(origin);
        let subjects = ownership_subjects_json(&ownership);
        let mut conn = self.data_conn_existing()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_database_id, current_generation) = tx.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        if current_database_id != expected_database_id || current_generation != expected_generation
        {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO episodes (
                content, source, status, strength, recall_count, created_at, updated_at,
                retention, user_message, assistant_message, expires_at,
                origin_kind, origin_platform, origin_account_id, origin_conversation_kind,
                origin_conversation_id, origin_sender_id, origin_sender_display_name,
                origin_session_id, origin_message_id,
                visibility, owner_principal, owner_display_name, subjects
             ) VALUES (?1, 'episode', 'active', 1.0, 0, ?2, ?2, ?3, ?4, ?5, ?6,
                       ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                content,
                created_at,
                SHORT_TERM,
                user_message.trim(),
                assistant_message.trim(),
                expires_at,
                origin.kind,
                origin.platform,
                origin.account_id,
                origin.conversation_kind,
                origin.conversation_id,
                origin.sender_id,
                origin.sender_display_name,
                origin.session_id,
                origin.message_id,
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
            ],
        )?;
        tx.commit()?;
        self.cleanup_expired_short_diaries()?;
        Ok(true)
    }

    pub fn stats(&self) -> Result<Value> {
        self.init()?;
        self.prune_missing_skill_records()?;
        let data = self.data_conn()?;
        let state = self.state_conn()?;
        Ok(json!({
            "ok": true,
            "data_db": self.data_db.display().to_string(),
            "state_db": self.state_db.display().to_string(),
            "skills_dir": self.skills_dir.display().to_string(),
            "facts": count_rows(&data, "facts")?,
            "episodes": count_rows(&data, "episodes")?,
            "short_diaries": count_where(&data, "episodes", "retention='short_term'")?,
            "long_diaries": count_where(&data, "episodes", "retention='long_term'")?,
            "unconsolidated_diaries": count_where(&data, "episodes", "retention='short_term' AND consolidated_at IS NULL")?,
            "unprocessed_pending_events": count_where(&data, "pending_events", "processed_at IS NULL")?,
            "total_pending_events": count_rows(&data, "pending_events")?,
            "skill_records": count_rows(&data, "skill_records")?,
            "skill_dirs": count_skill_dirs(&self.skills_dir)?,
            "evicted_turns": count_rows(&state, "evicted_turns")?,
        }))
    }

    pub fn reset_all(&self, include_skills: bool) -> Result<()> {
        self.init()?;
        let mut data = self.data_conn()?;
        let tx = data.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE memory_meta SET generation=generation+1 WHERE id=1",
            [],
        )?;
        tx.execute("DELETE FROM facts", [])?;
        tx.execute("DELETE FROM episodes", [])?;
        tx.execute("DELETE FROM pending_events", [])?;
        tx.execute("DELETE FROM skill_records", [])?;
        tx.execute("DELETE FROM memory_revisions", [])?;
        tx.execute(
            "DELETE FROM sqlite_sequence WHERE name IN ('facts', 'episodes', 'pending_events', 'skill_records', 'memory_revisions')",
            [],
        )?;
        tx.commit()?;
        self.clear_evicted_context()?;
        if include_skills {
            self.remove_auto_skills()?;
        }
        Ok(())
    }

    fn remove_auto_skills(&self) -> Result<()> {
        if !self.skills_dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let skill_file = entry.path().join("SKILL.md");
            let raw = std::fs::read_to_string(&skill_file).unwrap_or_default();
            if crate::skills::is_generated_skill(&raw) {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    fn flush_pending_events(&self) -> Result<()> {
        if !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, user_message, assistant_message, created_at FROM pending_events WHERE processed_at IS NULL ORDER BY id LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, user, assistant, created_at) = row?;
            let content = diary_content(&created_at, &user, &assistant);
            let expires_at = (Utc::now()
                + ChronoDuration::days(self.config.short_diary_retention_days as i64))
            .to_rfc3339();
            conn.execute(
                "INSERT INTO episodes (
                    content, source, status, recall_count, created_at, updated_at,
                    retention, user_message, assistant_message, expires_at
                 ) VALUES (?1, 'episode', 'active', 0, ?2, ?2, ?3, ?4, ?5, ?6)",
                params![content, created_at, SHORT_TERM, user, assistant, expires_at],
            )?;
            conn.execute(
                "UPDATE pending_events SET processed_at=?1 WHERE id=?2",
                params![now(), id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn next_organization_batch(&self) -> Result<Option<OrganizationBatch>> {
        if !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(None);
        }
        if !self.data_db.is_file() {
            return Ok(None);
        }
        self.init_existing()?;
        self.cleanup_expired_short_diaries()?;
        let conn = self.data_conn_existing()?;
        let (database_id, generation) = conn.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        // 已被遗忘(主动 forget 或衰减)的日记不参与整理:否则会被组织
        // 批次"复活"为长期记忆。
        let forced = count_where(
            &conn,
            "episodes",
            "retention='short_term' AND promotion_pending=1 AND status != 'forgotten'",
        )?;
        let unconsolidated = count_where(
            &conn,
            "episodes",
            "retention='short_term' AND consolidated_at IS NULL AND status != 'forgotten'",
        )?;
        if forced == 0 && unconsolidated < self.config.diary_batch_size as i64 {
            return Ok(None);
        }

        let (sql, limit) = if forced > 0 {
            (
                "SELECT id, created_at, user_message, assistant_message, 1,
                        origin_kind, origin_platform, origin_account_id,
                        origin_conversation_kind, origin_conversation_id, origin_sender_id,
                        origin_sender_display_name, origin_session_id, origin_message_id
                 FROM episodes
                 WHERE retention='short_term' AND promotion_pending=1
                   AND status != 'forgotten'
                 ORDER BY id LIMIT ?1",
                self.config.diary_batch_size.max(1),
            )
        } else {
            (
                "SELECT id, created_at, user_message, assistant_message, 0,
                        origin_kind, origin_platform, origin_account_id,
                        origin_conversation_kind, origin_conversation_id, origin_sender_id,
                        origin_sender_display_name, origin_session_id, origin_message_id
                 FROM episodes
                 WHERE retention='short_term' AND consolidated_at IS NULL
                   AND status != 'forgotten'
                 ORDER BY id LIMIT ?1",
                self.config.diary_batch_size,
            )
        };
        let mut stmt = conn.prepare(sql)?;
        let diaries = stmt
            .query_map([limit as i64], |row| {
                let origin = MemoryOrigin {
                    kind: row.get(5)?,
                    platform: row.get(6)?,
                    account_id: row.get(7)?,
                    conversation_kind: row.get(8)?,
                    conversation_id: row.get(9)?,
                    sender_id: row.get(10)?,
                    sender_display_name: row.get(11)?,
                    session_id: row.get(12)?,
                    message_id: row.get(13)?,
                };
                Ok(ShortDiaryRecord {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    user_message: row.get(2)?,
                    assistant_message: row.get(3)?,
                    force_long_term: row.get::<_, i64>(4)? != 0,
                    owner_principal: origin
                        .principal_ownership()
                        .map(|ownership| ownership.owner_principal),
                    origin,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if diaries.is_empty() {
            return Ok(None);
        }
        let existing = load_existing_memory_candidates(&conn, &diaries)?;
        Ok(Some(OrganizationBatch {
            database_id,
            generation,
            diaries,
            existing,
        }))
    }

    pub(crate) fn apply_organized_batch(
        &self,
        batch: &OrganizationBatch,
        output: OrganizedOutput,
    ) -> Result<()> {
        if !self.data_db.is_file() {
            bail!("memory database moved or removed while organization was running");
        }
        if output.knowledge.len() + output.long_diaries.len() > MAX_ORGANIZED_ITEMS {
            bail!("memory organizer returned too many items");
        }
        let diary_ids = batch
            .diaries
            .iter()
            .map(|diary| diary.id)
            .collect::<BTreeSet<_>>();
        let forced_ids = batch
            .diaries
            .iter()
            .filter(|diary| diary.force_long_term)
            .map(|diary| diary.id)
            .collect::<BTreeSet<_>>();
        let candidate_fact_ids = batch
            .existing
            .iter()
            .filter(|memory| memory.kind == "knowledge")
            .map(|memory| memory.id)
            .collect::<BTreeSet<_>>();
        let candidate_facts = batch
            .existing
            .iter()
            .filter(|memory| memory.kind == "knowledge")
            .map(|memory| (memory.id, memory))
            .collect::<BTreeMap<_, _>>();
        for action in &output.knowledge {
            validate_knowledge_action(action, &diary_ids, &candidate_fact_ids)?;
            validate_knowledge_visibility(batch, action)?;
            validate_knowledge_update_scope(batch, action, &candidate_facts)?;
        }
        let mut promoted_ids = BTreeSet::new();
        for diary in &output.long_diaries {
            validate_long_diary(batch, diary, &diary_ids)?;
            promoted_ids.extend(diary.diary_ids.iter().copied());
        }
        if !forced_ids.is_subset(&promoted_ids) {
            bail!("memory organizer did not promote every required diary");
        }

        let mut conn = self.data_conn_existing()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_database_id, current_generation) = tx.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        if current_database_id != batch.database_id || current_generation != batch.generation {
            bail!("memory database was moved, replaced, or reset while organization was running");
        }
        let timestamp = now();
        if self.config.auto_fact_enabled {
            for action in output.knowledge {
                let source_ids = normalized_ids_json(&action.diary_ids);
                let tags = normalized_tags_json(&action.tags);
                let ownership = knowledge_ownership(batch, &action);
                let subjects =
                    organized_subjects_json(batch, &action.diary_ids, &action.subjects, &ownership);
                match action.operation.as_str() {
                    "create" => {
                        tx.execute(
                            "INSERT INTO facts (
                                content, source, status, confidence, strength, recall_count,
                                created_at, updated_at, memory_type, truth_status, importance,
                                tags, source_episode_ids,
                                visibility, owner_principal, owner_display_name, subjects
                             ) SELECT ?1, 'diary-organizer', 'active', ?2, 1.0, 0,
                                      ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
                               WHERE NOT EXISTS (
                                    SELECT 1 FROM facts
                                     WHERE content=?1 AND truth_status!='rejected'
                                       AND visibility=?9 AND owner_principal=?10
                                )",
                            params![
                                action.content.trim(),
                                action.confidence,
                                timestamp,
                                action.memory_type,
                                action.truth_status,
                                action.importance,
                                tags,
                                source_ids,
                                ownership.visibility,
                                ownership.owner_principal,
                                ownership.owner_display_name,
                                subjects,
                            ],
                        )?;
                    }
                    "update" => {
                        let target = action
                            .target_id
                            .context("missing knowledge update target")?;
                        let old_content = tx.query_row(
                            "SELECT content FROM facts WHERE id=?1",
                            [target],
                            |row| row.get::<_, String>(0),
                        )?;
                        tx.execute(
                            "INSERT INTO memory_revisions (
                                memory_id, old_content, new_content, source_episode_ids, created_at
                             ) VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                target,
                                old_content,
                                action.content.trim(),
                                source_ids,
                                timestamp
                            ],
                        )?;
                        tx.execute(
                            "UPDATE facts SET content=?1, source='diary-organizer', status='active',
                                confidence=?2, strength=1.0, updated_at=?3, memory_type=?4,
                                truth_status=?5, importance=?6, tags=?7, source_episode_ids=?8,
                                visibility=?9, owner_principal=?10, owner_display_name=?11,
                                subjects=?12
                              WHERE id=?13",
                            params![
                                action.content.trim(),
                                action.confidence,
                                timestamp,
                                action.memory_type,
                                action.truth_status,
                                action.importance,
                                tags,
                                source_ids,
                                ownership.visibility,
                                ownership.owner_principal,
                                ownership.owner_display_name,
                                subjects,
                                target,
                            ],
                        )?;
                    }
                    _ => unreachable!("validated operation"),
                }
            }
        }

        for diary in output.long_diaries {
            let source_ids = normalized_ids_json(&diary.diary_ids);
            let tags = normalized_tags_json(&diary.tags);
            let source_key = format!(
                "{}:{}",
                diary
                    .diary_ids
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                blake3::hash(diary.content.trim().as_bytes()).to_hex()
            );
            let ownership = diary_ownership(batch, &diary.diary_ids);
            let subjects =
                organized_subjects_json(batch, &diary.diary_ids, &diary.subjects, &ownership);
            tx.execute(
                "INSERT OR IGNORE INTO episodes (
                    content, source, status, strength, recall_count, created_at, updated_at,
                    retention, consolidated_at, importance, confidence, tags,
                    source_episode_ids, source_key,
                    visibility, owner_principal, owner_display_name, subjects
                 ) VALUES (?1, 'diary-organizer', 'active', 1.0, 0, ?2, ?2,
                           ?3, ?2, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    diary.content.trim(),
                    timestamp,
                    LONG_TERM,
                    diary.importance,
                    diary.confidence,
                    tags,
                    source_ids,
                    source_key,
                    ownership.visibility,
                    ownership.owner_principal,
                    ownership.owner_display_name,
                    subjects,
                ],
            )?;
        }

        for diary in &batch.diaries {
            tx.execute(
                "UPDATE episodes SET consolidated_at=COALESCE(consolidated_at, ?1),
                    promotion_pending=CASE WHEN ?2 THEN 0 ELSE promotion_pending END,
                    promoted_at=CASE WHEN ?2 THEN COALESCE(promoted_at, ?1) ELSE promoted_at END
                 WHERE id=?3 AND retention='short_term'",
                params![timestamp, promoted_ids.contains(&diary.id), diary.id],
            )?;
        }
        tx.commit()?;
        self.cleanup_expired_short_diaries()?;
        Ok(())
    }

    fn cleanup_expired_short_diaries(&self) -> Result<usize> {
        if !self.data_db.is_file() {
            return Ok(0);
        }
        let conn = self.data_conn_existing()?;
        conn.execute(
            "UPDATE episodes SET status='forgotten'
             WHERE retention='short_term'
               AND consolidated_at IS NULL
               AND promotion_pending=0
               AND expires_at IS NOT NULL
               AND unixepoch(expires_at) IS NOT NULL
               AND unixepoch(expires_at) <= unixepoch('now')",
            [],
        )?;
        Ok(conn.execute(
            "DELETE FROM episodes
             WHERE retention='short_term'
               AND consolidated_at IS NOT NULL
               AND promotion_pending=0
               AND expires_at IS NOT NULL
               AND unixepoch(expires_at) IS NOT NULL
               AND unixepoch(expires_at) <= unixepoch('now')",
            [],
        )?)
    }

    fn prune_missing_skill_records(&self) -> Result<()> {
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare("SELECT id, path FROM skill_records")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut missing = Vec::new();
        for row in rows {
            let (id, path) = row?;
            if !PathBuf::from(path).exists() {
                missing.push(id);
            }
        }
        drop(stmt);
        for id in missing {
            conn.execute("DELETE FROM skill_records WHERE id=?1", params![id])?;
        }
        Ok(())
    }

    pub fn recall_memories(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        self.init()?;
        self.recall_memories_existing(query, limit, include_forgotten)
    }

    pub fn recall_memories_readonly(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        if !self.data_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "facts": [], "episodes": [] }));
        }
        self.recall_memories_existing(query, limit, include_forgotten)
    }

    fn recall_memories_existing(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        let conn = self.data_conn()?;
        let facts = self.search_facts(&conn, query, limit, include_forgotten)?;
        let episodes = self.search_episodes(&conn, query, limit, include_forgotten)?;
        Ok(json!({
            "ok": true,
            "query": query,
            "facts": facts.iter().map(memory_hit_json).collect::<Vec<_>>(),
            "episodes": episodes.iter().map(memory_hit_json).collect::<Vec<_>>(),
        }))
    }

    #[allow(dead_code)]
    pub fn recall_past_events(&self, query: &str, limit: usize) -> Result<Value> {
        self.init()?;
        self.recall_past_events_existing(query, limit)
    }

    pub fn recall_past_events_readonly(&self, query: &str, limit: usize) -> Result<Value> {
        if !self.data_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "episodes": [] }));
        }
        self.recall_past_events_existing(query, limit)
    }

    fn recall_past_events_existing(&self, query: &str, limit: usize) -> Result<Value> {
        let conn = self.data_conn()?;
        let episodes = self.search_episodes(&conn, query, limit, true)?;
        Ok(json!({
            "ok": true,
            "query": query,
            "episodes": episodes.iter().map(memory_hit_json).collect::<Vec<_>>(),
        }))
    }

    pub fn association(
        &self,
        query: &str,
        exclude: Option<&AssociationExclusion>,
    ) -> Result<Option<AssociationContext>> {
        if !self.config.enabled || !self.config.association_enabled {
            return Ok(None);
        }
        // 一条连接贯穿本回合的两次检索与全部 reinforce,替代此前最多 10 次
        // Connection::open + PRAGMA 重设。
        let conn = self.data_conn()?;
        let facts = self.search_facts(&conn, query, self.config.association_facts, false)?;
        let mut episodes =
            self.search_episodes(&conn, query, self.config.association_episodes, false)?;
        // 自回声过滤(缓存调研 08-16):当前会话可见范围内刚写下的日记/
        // 事实,原对话就在眼前,复述一遍纯属冗余;被 compact 折走后
        // (时间早于最老可见轮)重新够格召回。显式 recall 工具不受此限。
        if let Some(exclude) = exclude {
            // facts 无 origin 列(origin_session_id 恒空串),天然不命中;
            // 实际的自回声源=上一轮自动日记(episodes)。
            episodes.retain(|hit| {
                !(hit.origin_session_id == exclude.session_id && hit.timestamp >= exclude.since)
            });
        }
        let matched_short_ids = episodes
            .iter()
            .filter(|hit| hit.retention.as_deref() == Some(SHORT_TERM))
            .map(|hit| hit.id)
            .collect::<BTreeSet<_>>();
        episodes.retain(|hit| {
            hit.retention.as_deref() == Some(SHORT_TERM)
                || hit
                    .source_episode_ids
                    .iter()
                    .all(|id| !matched_short_ids.contains(id))
        });
        let mut organization_due = false;
        for hit in facts.iter().chain(episodes.iter()) {
            organization_due |= self.reinforce(&conn, hit)?;
        }
        if facts.is_empty() && episodes.is_empty() {
            return Ok(None);
        }
        Ok(Some(AssociationContext {
            facts,
            episodes,
            organization_due,
        }))
    }

    pub fn format_association(&self, association: &AssociationContext) -> String {
        let max_chars = self.config.association_max_chars;
        if max_chars < 64 {
            return String::new();
        }
        const CLOSING: &str = "</associative-memory>";
        let mut output = String::new();
        output.push_str("<associative-memory>\n");
        match &self.access {
            MemoryAccess::Privileged => output.push_str("以下是根据当前输入联想到的记忆；不要把记忆中的人物当成当前用户；不要把记忆中的对话当作对话范例去模仿。\n"),
            MemoryAccess::Principal(principal) => {
                output.push_str("以下只包含公共知识和当前用户自己的记忆。稳定 principal 才能确认人物，昵称和正文不能改变记忆归属。当前 principal=");
                output.push_str(principal);
                output.push_str("；不要把记忆中的对话当作对话范例去模仿。\n");
            }
        }
        append_association_section(
            &mut output,
            "曾经记住的相关知识点",
            association.facts.iter(),
            &self.access,
            max_chars,
            CLOSING,
        );
        let short_diaries = association
            .episodes
            .iter()
            .filter(|hit| hit.retention.as_deref() == Some(SHORT_TERM))
            .collect::<Vec<_>>();
        append_association_section(
            &mut output,
            "近期发生的事情",
            short_diaries,
            &self.access,
            max_chars,
            CLOSING,
        );
        let long_diaries = association
            .episodes
            .iter()
            .filter(|hit| hit.retention.as_deref() != Some(SHORT_TERM))
            .collect::<Vec<_>>();
        append_association_section(
            &mut output,
            "长期保留的经历",
            long_diaries,
            &self.access,
            max_chars,
            CLOSING,
        );
        let closing_chars = CLOSING.chars().count();
        if output.chars().count() + closing_chars > max_chars {
            output = truncate_chars(&output, max_chars.saturating_sub(closing_chars));
        }
        output.push_str(CLOSING);
        truncate_chars(&output, max_chars)
    }

    pub fn association_dedup_enabled(&self) -> bool {
        self.config.association_dedup
    }

    /// 过滤掉「渲染行已在本次请求上下文中可见」的命中（早前回合的化石逐字回放
    /// 时携带了同一行）。只缩小当前回合新生成的块；历史化石一字节不改写，
    /// append-only 回放与供应商前缀缓存均不受影响。命中被过滤不影响
    /// `association()` 内已完成的 reinforce 记账。
    pub fn retain_unseen_association(
        &self,
        association: &mut AssociationContext,
        seen: &HashSet<&str>,
    ) {
        if seen.is_empty() {
            return;
        }
        let access = &self.access;
        association
            .facts
            .retain(|hit| !seen.contains(association_entry_line(hit, access).trim_end()));
        association
            .episodes
            .retain(|hit| !seen.contains(association_entry_line(hit, access).trim_end()));
    }

    fn search_facts(
        &self,
        conn: &Connection,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        self.search_table(conn, "facts", MemoryKind::Fact, query, limit, include_forgotten)
    }

    fn search_episodes(
        &self,
        conn: &Connection,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        self.search_table(
            conn,
            "episodes",
            MemoryKind::Diary,
            query,
            limit,
            include_forgotten,
        )
    }

    fn search_table(
        &self,
        conn: &Connection,
        table: &str,
        kind: MemoryKind,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let tokens = query_tokens(query);
        // 归一化与行无关,提到 5000 行循环外做一次。
        let normalized_query = compact_line(query).to_ascii_lowercase();
        let status_filter = if kind == MemoryKind::Fact && include_forgotten {
            "WHERE truth_status!='rejected'"
        } else if kind == MemoryKind::Fact {
            "WHERE status!='forgotten' AND truth_status!='rejected'"
        } else if include_forgotten {
            ""
        } else {
            "WHERE status!='forgotten'"
        };
        let access_filter = if self.access.principal_key().is_some() && status_filter.is_empty() {
            "WHERE visibility='public' OR (visibility='principal' AND owner_principal=?1)"
        } else if self.access.principal_key().is_some() {
            " AND (visibility='public' OR (visibility='principal' AND owner_principal=?1))"
        } else {
            ""
        };
        let sql = format!(
            "SELECT id, content, source, status, created_at, strength,
                     COALESCE(importance, 3), {}, COALESCE(source_episode_ids, '[]'),
                     visibility, owner_principal, owner_display_name, subjects,
                     {}
             FROM {table} {}{} ORDER BY updated_at DESC LIMIT 5000",
            if kind == MemoryKind::Diary {
                "retention"
            } else {
                "NULL"
            },
            // 自回声排除只针对自动日记;facts 表没有 origin 列。
            if kind == MemoryKind::Diary {
                "COALESCE(origin_session_id, '')"
            } else {
                "''"
            },
            status_filter,
            access_filter,
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match self.access.principal_key() {
            Some(principal) => stmt.query([principal])?,
            None => stmt.query([])?,
        };
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            let content = row.get::<_, String>(1)?;
            let source = row.get::<_, String>(2)?;
            let status = row.get::<_, String>(3)?;
            let timestamp = row.get::<_, String>(4)?;
            let strength = row.get::<_, f64>(5)?;
            let importance = row.get::<_, i64>(6)?;
            let retention = row.get::<_, Option<String>>(7)?;
            let source_episode_ids = row.get::<_, String>(8)?;
            let visibility = row.get::<_, String>(9)?;
            let owner_principal = row.get::<_, String>(10)?;
            let owner_display_name = row.get::<_, String>(11)?;
            let subjects = row.get::<_, String>(12)?;
            let origin_session_id = row.get::<_, String>(13)?;
            if !include_forgotten && status == "forgotten" {
                continue;
            }
            let lexical_score = score_text(&content, &normalized_query, &tokens);
            if lexical_score <= 0.0 {
                continue;
            }
            let score = lexical_score
                + strength.clamp(0.0, 1.0) as f32 * 5.0
                + importance.clamp(1, 5) as f32;
            hits.push(MemoryHit {
                id,
                origin_session_id,
                kind,
                content,
                score,
                timestamp,
                source,
                retention,
                visibility,
                owner_principal,
                owner_display_name,
                subjects,
                source_episode_ids: serde_json::from_str(&source_episode_ids).unwrap_or_default(),
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit.min(50));
        Ok(hits)
    }

    fn reinforce(&self, conn: &Connection, hit: &MemoryHit) -> Result<bool> {
        let timestamp = now();
        if hit.kind == MemoryKind::Fact {
            conn.execute(
                "UPDATE facts SET recall_count=recall_count+1,
                    strength=MIN(1.0, strength+?1), last_recalled_at=?2,
                    updated_at=?2, status='active' WHERE id=?3",
                params![self.config.forgetting_review_boost, timestamp, hit.id],
            )?;
            return Ok(false);
        }

        let refreshed_expiry = (Utc::now()
            + ChronoDuration::days(self.config.short_diary_retention_days as i64))
        .to_rfc3339();
        conn.execute(
            "UPDATE episodes SET
                recall_count=recall_count+1,
                strength=MIN(1.0, strength+?1),
                last_recalled_at=?2,
                updated_at=?2,
                status='active',
                expires_at=CASE
                    WHEN retention='short_term' AND promoted_at IS NULL THEN ?3
                    ELSE expires_at END,
                promotion_pending=CASE
                    WHEN retention='short_term' AND promoted_at IS NULL
                         AND recall_count+1>=?4 THEN 1
                    ELSE promotion_pending END
             WHERE id=?5",
            params![
                self.config.forgetting_review_boost,
                timestamp,
                refreshed_expiry,
                self.config.diary_promotion_recalls as i64,
                hit.id
            ],
        )?;
        Ok(conn.query_row(
            "SELECT retention='short_term' AND promotion_pending=1
             FROM episodes WHERE id=?1",
            [hit.id],
            |row| row.get::<_, bool>(0),
        )?)
    }

    fn decay_memories(&self) -> Result<()> {
        if !self.config.enabled || !self.config.forgetting_enabled {
            return Ok(());
        }
        let conn = self.data_conn()?;
        self.decay_memories_with_conn(&conn)
    }

    fn decay_memories_with_conn(&self, conn: &Connection) -> Result<()> {
        if !self.config.enabled || !self.config.forgetting_enabled {
            return Ok(());
        }
        decay_table(conn, "facts", &self.config)?;
        decay_table(conn, "episodes", &self.config)?;
        Ok(())
    }

    fn data_conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.data_db)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(conn)
    }

    fn data_conn_existing(&self) -> Result<Connection> {
        let conn = Connection::open_with_flags(
            &self.data_db,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(conn)
    }

    fn state_conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.state_db)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(conn)
    }
}

fn init_data_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS facts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'active',
            confidence REAL NOT NULL DEFAULT 1.0,
            strength REAL NOT NULL DEFAULT 1.0,
            recall_count INTEGER NOT NULL DEFAULT 0,
            last_recalled_at TEXT,
            last_decay_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            visibility TEXT NOT NULL DEFAULT 'privileged',
            owner_principal TEXT NOT NULL DEFAULT '',
            owner_display_name TEXT NOT NULL DEFAULT '',
            subjects TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE IF NOT EXISTS episodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'episode',
            status TEXT NOT NULL DEFAULT 'active',
            strength REAL NOT NULL DEFAULT 1.0,
            recall_count INTEGER NOT NULL DEFAULT 0,
            last_recalled_at TEXT,
            last_decay_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            visibility TEXT NOT NULL DEFAULT 'privileged',
            owner_principal TEXT NOT NULL DEFAULT '',
            owner_display_name TEXT NOT NULL DEFAULT '',
            subjects TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE IF NOT EXISTS pending_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_message TEXT NOT NULL,
            assistant_message TEXT NOT NULL,
            created_at TEXT NOT NULL,
            processed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS skill_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            path TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_revisions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_id INTEGER NOT NULL,
            old_content TEXT NOT NULL,
            new_content TEXT NOT NULL,
            source_episode_ids TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_meta (
            id INTEGER PRIMARY KEY CHECK(id=1),
            generation INTEGER NOT NULL DEFAULT 0,
            database_id TEXT NOT NULL DEFAULT '',
            access_schema_version INTEGER NOT NULL DEFAULT 2
        );",
    )?;
    add_column_if_missing(
        conn,
        "memory_meta",
        "database_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "memory_meta",
        "access_schema_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_meta (
            id, generation, database_id, access_schema_version
         ) VALUES (1, 0, '', 2)",
        [],
    )?;
    let database_id = conn.query_row(
        "SELECT database_id FROM memory_meta WHERE id=1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    if database_id.is_empty() {
        conn.execute(
            "UPDATE memory_meta SET database_id=?1 WHERE id=1 AND database_id=''",
            [format!("mem-{:032x}", rand::random::<u128>())],
        )?;
    }
    add_column_if_missing(conn, "facts", "strength", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "facts", "last_decay_at", "TEXT")?;
    add_column_if_missing(conn, "facts", "memory_type", "TEXT NOT NULL DEFAULT 'fact'")?;
    add_column_if_missing(
        conn,
        "facts",
        "truth_status",
        "TEXT NOT NULL DEFAULT 'reported'",
    )?;
    add_column_if_missing(conn, "facts", "importance", "INTEGER NOT NULL DEFAULT 3")?;
    add_column_if_missing(conn, "facts", "tags", "TEXT NOT NULL DEFAULT '[]'")?;
    add_column_if_missing(
        conn,
        "facts",
        "source_episode_ids",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    for table in ["facts", "episodes"] {
        add_column_if_missing(
            conn,
            table,
            "visibility",
            "TEXT NOT NULL DEFAULT 'privileged'",
        )?;
        add_column_if_missing(conn, table, "owner_principal", "TEXT NOT NULL DEFAULT ''")?;
        add_column_if_missing(
            conn,
            table,
            "owner_display_name",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        add_column_if_missing(conn, table, "subjects", "TEXT NOT NULL DEFAULT '[]'")?;
    }
    add_column_if_missing(conn, "episodes", "strength", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "episodes", "last_decay_at", "TEXT")?;
    // Existing episodes predate the short/long split and must remain durable.
    add_column_if_missing(
        conn,
        "episodes",
        "retention",
        "TEXT NOT NULL DEFAULT 'long_term'",
    )?;
    add_column_if_missing(conn, "episodes", "user_message", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(
        conn,
        "episodes",
        "assistant_message",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(conn, "episodes", "expires_at", "TEXT")?;
    add_column_if_missing(conn, "episodes", "consolidated_at", "TEXT")?;
    add_column_if_missing(
        conn,
        "episodes",
        "promotion_pending",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "episodes", "promoted_at", "TEXT")?;
    add_column_if_missing(conn, "episodes", "importance", "INTEGER NOT NULL DEFAULT 3")?;
    add_column_if_missing(conn, "episodes", "confidence", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "episodes", "tags", "TEXT NOT NULL DEFAULT '[]'")?;
    add_column_if_missing(
        conn,
        "episodes",
        "source_episode_ids",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(conn, "episodes", "source_key", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "episodes", "origin_kind", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_platform",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_account_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_conversation_kind",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_conversation_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_sender_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_sender_display_name",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_session_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_message_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    migrate_memory_access_v1(conn)?;
    migrate_memory_subjects_v2(conn)?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_episodes_retention_created
             ON episodes(retention, created_at);
         CREATE INDEX IF NOT EXISTS idx_episodes_organization
             ON episodes(retention, promotion_pending, consolidated_at, id);
         CREATE INDEX IF NOT EXISTS idx_memory_revisions_memory
             ON memory_revisions(memory_id, id);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_long_diary_source_key
             ON episodes(source_key) WHERE retention='long_term' AND source_key!='';
         CREATE INDEX IF NOT EXISTS idx_facts_access_updated
             ON facts(visibility, owner_principal, updated_at DESC);
         CREATE INDEX IF NOT EXISTS idx_episodes_access_updated
             ON episodes(visibility, owner_principal, updated_at DESC);",
    )?;
    Ok(())
}

fn init_state_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS evicted_turns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id TEXT,
            timestamp TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            visibility TEXT NOT NULL DEFAULT 'privileged',
            owner_principal TEXT NOT NULL DEFAULT '',
            owner_display_name TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS evicted_embeddings (
            id INTEGER PRIMARY KEY,
            model TEXT NOT NULL,
            embedding_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS evicted_turns_fts USING fts5(
            content,
            content='evicted_turns',
            content_rowid='id',
            tokenize='trigram'
        );
        CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_insert AFTER INSERT ON evicted_turns BEGIN
            INSERT INTO evicted_turns_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_delete AFTER DELETE ON evicted_turns BEGIN
            INSERT INTO evicted_turns_fts(evicted_turns_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_update AFTER UPDATE OF content ON evicted_turns BEGIN
            INSERT INTO evicted_turns_fts(evicted_turns_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
            INSERT INTO evicted_turns_fts(rowid, content) VALUES (new.id, new.content);
        END;",
    )?;
    add_column_if_missing(conn, "evicted_turns", "source_id", "TEXT")?;
    add_column_if_missing(
        conn,
        "evicted_turns",
        "visibility",
        "TEXT NOT NULL DEFAULT 'privileged'",
    )?;
    add_column_if_missing(
        conn,
        "evicted_turns",
        "owner_principal",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "evicted_turns",
        "owner_display_name",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_evicted_turns_source_id
         ON evicted_turns(source_id) WHERE source_id IS NOT NULL",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_evicted_turns_access
         ON evicted_turns(visibility, owner_principal, id DESC)",
        [],
    )?;
    Ok(())
}

fn migrate_memory_access_v1(conn: &Connection) -> Result<()> {
    let version = conn.query_row(
        "SELECT access_schema_version FROM memory_meta WHERE id=1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if version >= 1 {
        return Ok(());
    }

    #[derive(Clone)]
    struct LegacyEpisode {
        id: i64,
        source_episode_ids: String,
        origin: MemoryOrigin,
    }

    let tx = conn.unchecked_transaction()?;
    let episodes = {
        let mut stmt = tx.prepare(
            "SELECT id, source_episode_ids,
                    origin_kind, origin_platform, origin_account_id,
                    origin_conversation_kind, origin_conversation_id, origin_sender_id,
                    origin_sender_display_name, origin_session_id, origin_message_id
               FROM episodes ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(LegacyEpisode {
                id: row.get(0)?,
                source_episode_ids: row.get(1)?,
                origin: MemoryOrigin {
                    kind: row.get(2)?,
                    platform: row.get(3)?,
                    account_id: row.get(4)?,
                    conversation_kind: row.get(5)?,
                    conversation_id: row.get(6)?,
                    sender_id: row.get(7)?,
                    sender_display_name: row.get(8)?,
                    session_id: row.get(9)?,
                    message_id: row.get(10)?,
                },
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut ownerships = BTreeMap::<i64, MemoryOwnership>::new();
    for episode in &episodes {
        if let Some(ownership) = episode.origin.principal_ownership() {
            ownerships.insert(episode.id, ownership);
        } else if episode.origin.kind == "local" {
            ownerships.insert(episode.id, MemoryOwnership::privileged());
        }
    }
    for episode in &episodes {
        if ownerships.contains_key(&episode.id) {
            continue;
        }
        if let Some(ownership) = ownership_from_source_ids(&episode.source_episode_ids, &ownerships)
        {
            ownerships.insert(episode.id, ownership);
        }
    }
    for episode in &episodes {
        let ownership = ownerships
            .get(&episode.id)
            .cloned()
            .unwrap_or_else(MemoryOwnership::privileged);
        let subjects = ownership_subjects_json(&ownership);
        tx.execute(
            "UPDATE episodes SET visibility=?1, owner_principal=?2, owner_display_name=?3,
                                 subjects=?4
              WHERE id=?5",
            params![
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
                episode.id,
            ],
        )?;
    }

    let facts = {
        let mut stmt = tx.prepare("SELECT id, source_episode_ids FROM facts ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, source_ids) in facts {
        let ownership = ownership_from_source_ids(&source_ids, &ownerships)
            .unwrap_or_else(MemoryOwnership::privileged);
        let subjects = ownership_subjects_json(&ownership);
        tx.execute(
            "UPDATE facts SET visibility=?1, owner_principal=?2, owner_display_name=?3,
                              subjects=?4
              WHERE id=?5",
            params![
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
                id,
            ],
        )?;
    }
    tx.execute(
        "UPDATE memory_meta SET access_schema_version=1 WHERE id=1",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate_memory_subjects_v2(conn: &Connection) -> Result<()> {
    let version = conn.query_row(
        "SELECT access_schema_version FROM memory_meta WHERE id=1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if version >= 2 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for table in ["facts", "episodes"] {
        let sql = format!(
            "SELECT id, visibility, owner_principal, owner_display_name
               FROM {table} WHERE subjects='[]' OR subjects=''"
        );
        let rows = {
            let mut stmt = tx.prepare(&sql)?;
            let mapped = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            mapped.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let update = format!("UPDATE {table} SET subjects=?1 WHERE id=?2");
        for (id, visibility, owner_principal, owner_display_name) in rows {
            let ownership = MemoryOwnership {
                visibility: match visibility.as_str() {
                    VISIBILITY_PUBLIC => VISIBILITY_PUBLIC,
                    VISIBILITY_PRINCIPAL => VISIBILITY_PRINCIPAL,
                    _ => VISIBILITY_PRIVILEGED,
                },
                owner_principal,
                owner_display_name,
            };
            tx.execute(&update, params![ownership_subjects_json(&ownership), id])?;
        }
    }
    tx.execute(
        "UPDATE memory_meta SET access_schema_version=2 WHERE id=1",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn ownership_from_source_ids(
    encoded: &str,
    ownerships: &BTreeMap<i64, MemoryOwnership>,
) -> Option<MemoryOwnership> {
    let ids = serde_json::from_str::<Vec<i64>>(encoded).ok()?;
    if ids.is_empty() {
        return None;
    }
    let mut principal: Option<MemoryOwnership> = None;
    for id in ids {
        let ownership = ownerships.get(&id)?;
        if ownership.visibility != VISIBILITY_PRINCIPAL {
            return Some(MemoryOwnership::privileged());
        }
        if principal
            .as_ref()
            .is_some_and(|existing| existing.owner_principal != ownership.owner_principal)
        {
            return Some(MemoryOwnership::privileged());
        }
        principal.get_or_insert_with(|| ownership.clone());
    }
    principal
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

fn decay_table(conn: &Connection, table: &str, config: &MemoryConfig) -> Result<()> {
    let now = Utc::now();
    let mut stmt = conn.prepare(&format!(
        "SELECT id, strength, COALESCE(last_recalled_at, updated_at, created_at), last_decay_at FROM {table} WHERE status='active'{}",
        if table == "episodes" { " AND retention='long_term'" } else { "" }
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut updates = Vec::new();
    for row in rows {
        let (id, strength, recalled_at, last_decay_at) = row?;
        let anchor = last_decay_at.as_deref().unwrap_or(&recalled_at);
        let Ok(anchor) = DateTime::parse_from_rfc3339(anchor) else {
            continue;
        };
        let days = (now - anchor.with_timezone(&Utc)).num_seconds().max(0) as f64 / 86_400.0;
        if days < 0.25 {
            continue;
        }
        let half_life = config.forgetting_half_life_days.max(0.1);
        let new_strength = strength * 2f64.powf(-days / half_life);
        let status = if new_strength < config.forgetting_min_strength {
            "forgotten"
        } else {
            "active"
        };
        updates.push((id, new_strength, status.to_string()));
    }
    drop(stmt);
    for (id, strength, status) in updates {
        conn.execute(
            &format!("UPDATE {table} SET strength=?1, status=?2, last_decay_at=?3 WHERE id=?4"),
            params![strength, status, now.to_rfc3339(), id],
        )?;
    }
    Ok(())
}

fn memory_hit_json(hit: &MemoryHit) -> Value {
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

/// 渲染单条联想记忆行（含结尾换行），与注入块中的字节完全一致。
/// 整行同时充当跨回合去重键：内容或日期变化的记忆会渲染出不同的行，
/// 因而被视为新条目重新注入。
fn association_entry_line(hit: &MemoryHit, access: &MemoryAccess) -> String {
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
    match date {
        Some(date) => format!("- [{date}] [{label}] {content}\n"),
        None => format!("- [{label}] {content}\n"),
    }
}

fn append_association_section<'a>(
    output: &mut String,
    title: &str,
    hits: impl IntoIterator<Item = &'a MemoryHit>,
    access: &MemoryAccess,
    max_chars: usize,
    closing: &str,
) {
    let heading = format!("\n{title}：\n");
    let mut section = String::new();
    for hit in hits {
        let line = association_entry_line(hit, access);
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

fn load_existing_memory_candidates(
    conn: &Connection,
    source_diaries: &[ShortDiaryRecord],
) -> Result<Vec<ExistingMemoryRecord>> {
    let mut allowed_principals = BTreeSet::new();
    let mut privileged_source = false;
    for diary in source_diaries {
        match diary.origin.principal_ownership() {
            Some(ownership) => {
                allowed_principals.insert(ownership.owner_principal);
            }
            None => privileged_source = true,
        }
    }
    let query = source_diaries
        .iter()
        .flat_map(|diary| [&diary.user_message, &diary.assistant_message])
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = query_tokens_with_limit(&query, 256);
    let mut scored = Vec::<(f32, ExistingMemoryRecord)>::new();
    let mut facts = conn.prepare(
        "SELECT id, content, truth_status, visibility, owner_principal, owner_display_name FROM facts
         WHERE status!='forgotten' AND truth_status!='rejected'
         ORDER BY updated_at DESC LIMIT 5000",
    )?;
    let rows = facts.query_map([], |row| {
        Ok(ExistingMemoryRecord {
            id: row.get(0)?,
            kind: "knowledge".to_string(),
            content: row.get(1)?,
            truth_status: row.get(2)?,
            visibility: row.get(3)?,
            owner_principal: row.get(4)?,
            owner_display_name: row.get(5)?,
        })
    })?;
    for row in rows {
        let memory = row?;
        if !organizer_candidate_is_visible(&memory, &allowed_principals, privileged_source) {
            continue;
        }
        let score = score_text(&memory.content, "", &tokens);
        if score > 0.0 {
            scored.push((score, memory));
        }
    }
    drop(facts);

    let mut diaries = conn.prepare(
        "SELECT id, content, visibility, owner_principal, owner_display_name FROM episodes
         WHERE retention='long_term' AND status!='forgotten'
         ORDER BY updated_at DESC LIMIT 5000",
    )?;
    let rows = diaries.query_map([], |row| {
        Ok(ExistingMemoryRecord {
            id: row.get(0)?,
            kind: "long_diary".to_string(),
            content: row.get(1)?,
            truth_status: "accepted".to_string(),
            visibility: row.get(2)?,
            owner_principal: row.get(3)?,
            owner_display_name: row.get(4)?,
        })
    })?;
    for row in rows {
        let memory = row?;
        if !organizer_candidate_is_visible(&memory, &allowed_principals, privileged_source) {
            continue;
        }
        let score = score_text(&memory.content, "", &tokens);
        if score > 0.0 {
            scored.push((score, memory));
        }
    }
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut fact_count = 0usize;
    let mut diary_count = 0usize;
    Ok(scored
        .into_iter()
        .filter_map(|(_, memory)| match memory.kind.as_str() {
            "knowledge" if fact_count < 30 => {
                fact_count += 1;
                Some(memory)
            }
            "long_diary" if diary_count < 20 => {
                diary_count += 1;
                Some(memory)
            }
            _ => None,
        })
        .collect())
}

fn organizer_candidate_is_visible(
    memory: &ExistingMemoryRecord,
    allowed_principals: &BTreeSet<String>,
    privileged_source: bool,
) -> bool {
    match memory.visibility.as_str() {
        VISIBILITY_PUBLIC => true,
        VISIBILITY_PRINCIPAL => allowed_principals.contains(&memory.owner_principal),
        VISIBILITY_PRIVILEGED => privileged_source,
        _ => false,
    }
}

fn validate_knowledge_action(
    action: &KnowledgeAction,
    diary_ids: &BTreeSet<i64>,
    candidate_fact_ids: &BTreeSet<i64>,
) -> Result<()> {
    if !matches!(action.operation.as_str(), "create" | "update") {
        bail!("invalid knowledge operation");
    }
    if action.operation == "update"
        && !action
            .target_id
            .is_some_and(|id| candidate_fact_ids.contains(&id))
    {
        bail!("knowledge update target is not an allowed candidate");
    }
    if action.operation == "create" && action.target_id.is_some() {
        bail!("new knowledge must not have a target id");
    }
    if !matches!(
        action.memory_type.as_str(),
        "fact" | "preference" | "relationship" | "task" | "self" | "other"
    ) {
        bail!("invalid knowledge type");
    }
    if !matches!(
        action.truth_status.as_str(),
        "accepted" | "reported" | "uncertain" | "fictional" | "rejected"
    ) {
        bail!("invalid knowledge truth status");
    }
    validate_organized_content(&action.content, 2_000)?;
    validate_evidence_ids(&action.diary_ids, diary_ids)?;
    if !(1..=5).contains(&action.importance)
        || !action.confidence.is_finite()
        || !(0.0..=1.0).contains(&action.confidence)
    {
        bail!("knowledge importance or confidence is out of range");
    }
    Ok(())
}

fn validate_knowledge_visibility(
    batch: &OrganizationBatch,
    action: &KnowledgeAction,
) -> Result<()> {
    if !matches!(
        action.visibility.as_str(),
        "" | VISIBILITY_PUBLIC | VISIBILITY_PRINCIPAL | VISIBILITY_PRIVILEGED
    ) {
        bail!("invalid knowledge visibility");
    }
    let target_visibility = action.target_id.and_then(|target_id| {
        batch
            .existing
            .iter()
            .find(|memory| memory.kind == "knowledge" && memory.id == target_id)
            .map(|memory| memory.visibility.as_str())
    });
    if target_visibility
        .is_some_and(|target| !action.visibility.is_empty() && action.visibility != target)
    {
        bail!("knowledge updates cannot change memory visibility");
    }
    let effective_visibility = target_visibility.unwrap_or(action.visibility.as_str());
    if effective_visibility == VISIBILITY_PUBLIC && action.memory_type != "fact" {
        bail!("only general facts may become public memories");
    }
    validate_memory_subjects(batch, &action.diary_ids, &action.subjects)?;
    if effective_visibility == VISIBILITY_PUBLIC {
        if !action.subjects.is_empty() {
            bail!("public memories cannot contain person subjects");
        }
        let content = action.content.to_lowercase();
        for diary in batch
            .diaries
            .iter()
            .filter(|diary| action.diary_ids.contains(&diary.id))
        {
            for marker in [
                diary.origin.sender_id.trim(),
                diary.origin.sender_display_name.trim(),
            ] {
                if marker.chars().count() >= 2 && content.contains(&marker.to_lowercase()) {
                    bail!("public memory content contains a source identity marker");
                }
            }
        }
    }
    Ok(())
}

fn validate_knowledge_update_scope(
    batch: &OrganizationBatch,
    action: &KnowledgeAction,
    candidates: &BTreeMap<i64, &ExistingMemoryRecord>,
) -> Result<()> {
    let Some(target_id) = action.target_id else {
        return Ok(());
    };
    let target = candidates
        .get(&target_id)
        .context("knowledge update target disappeared from candidates")?;
    let evidence = diary_ownership(batch, &action.diary_ids);
    let allowed = match target.visibility.as_str() {
        VISIBILITY_PUBLIC => true,
        VISIBILITY_PRINCIPAL => {
            evidence.visibility == VISIBILITY_PRINCIPAL
                && evidence.owner_principal == target.owner_principal
        }
        VISIBILITY_PRIVILEGED => evidence.visibility == VISIBILITY_PRIVILEGED,
        _ => false,
    };
    if !allowed {
        bail!("knowledge update evidence belongs to a different principal");
    }
    Ok(())
}

fn validate_long_diary(
    batch: &OrganizationBatch,
    diary: &LongDiaryDraft,
    diary_ids: &BTreeSet<i64>,
) -> Result<()> {
    validate_organized_content(&diary.content, 3_000)?;
    validate_evidence_ids(&diary.diary_ids, diary_ids)?;
    if !(1..=5).contains(&diary.importance)
        || !diary.confidence.is_finite()
        || !(0.0..=1.0).contains(&diary.confidence)
    {
        bail!("long diary importance or confidence is out of range");
    }
    if !matches!(
        diary.visibility.as_str(),
        "" | VISIBILITY_PRINCIPAL | VISIBILITY_PRIVILEGED
    ) {
        bail!("long diaries cannot be public memories");
    }
    validate_memory_subjects(batch, &diary.diary_ids, &diary.subjects)?;
    Ok(())
}

fn validate_memory_subjects(
    batch: &OrganizationBatch,
    diary_ids: &[i64],
    subjects: &[MemorySubject],
) -> Result<()> {
    if subjects.len() > 32 {
        bail!("organized memory contains too many subjects");
    }
    let allowed_principals = batch
        .diaries
        .iter()
        .filter(|diary| diary_ids.contains(&diary.id))
        .filter_map(|diary| diary.owner_principal.as_deref())
        .collect::<BTreeSet<_>>();
    for subject in subjects {
        let principal = subject
            .principal
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let name = subject
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if principal.is_none() && name.is_none() {
            bail!("memory subject must contain a principal or name");
        }
        if principal.is_some_and(|value| !allowed_principals.contains(value)) {
            bail!("memory subject references an untrusted principal");
        }
        if name
            .is_some_and(|value| value.chars().count() > 128 || value.chars().any(char::is_control))
        {
            bail!("memory subject name is invalid");
        }
    }
    Ok(())
}

fn knowledge_ownership(batch: &OrganizationBatch, action: &KnowledgeAction) -> MemoryOwnership {
    if let Some(target) = action.target_id.and_then(|target| {
        batch
            .existing
            .iter()
            .find(|memory| memory.kind == "knowledge" && memory.id == target)
    }) {
        return MemoryOwnership {
            visibility: match target.visibility.as_str() {
                VISIBILITY_PUBLIC => VISIBILITY_PUBLIC,
                VISIBILITY_PRINCIPAL => VISIBILITY_PRINCIPAL,
                _ => VISIBILITY_PRIVILEGED,
            },
            owner_principal: target.owner_principal.clone(),
            owner_display_name: target.owner_display_name.clone(),
        };
    }
    if action.visibility == VISIBILITY_PUBLIC && action.memory_type == "fact" {
        return MemoryOwnership::public();
    }
    diary_ownership(batch, &action.diary_ids)
}

fn diary_ownership(batch: &OrganizationBatch, diary_ids: &[i64]) -> MemoryOwnership {
    let mut principals = BTreeMap::<String, String>::new();
    let mut privileged_source = false;
    for id in diary_ids {
        let Some(diary) = batch.diaries.iter().find(|diary| diary.id == *id) else {
            privileged_source = true;
            continue;
        };
        match diary.origin.principal_ownership() {
            Some(ownership) => {
                principals
                    .entry(ownership.owner_principal)
                    .or_insert(ownership.owner_display_name);
            }
            None => privileged_source = true,
        }
    }
    if !privileged_source && principals.len() == 1 {
        let (principal, display_name) = principals
            .into_iter()
            .next()
            .expect("one principal was checked");
        MemoryOwnership::principal(principal, display_name)
    } else {
        MemoryOwnership::privileged()
    }
}

fn validate_organized_content(content: &str, max_chars: usize) -> Result<()> {
    let content = content.trim();
    if content.is_empty() || content.chars().count() > max_chars || content.contains('\0') {
        bail!("organized memory content is empty or too long");
    }
    Ok(())
}

fn validate_evidence_ids(ids: &[i64], allowed: &BTreeSet<i64>) -> Result<()> {
    if ids.is_empty() || ids.iter().any(|id| !allowed.contains(id)) {
        bail!("organized memory references invalid diary ids");
    }
    Ok(())
}

fn normalized_ids_json(ids: &[i64]) -> String {
    serde_json::to_string(&ids.iter().copied().collect::<BTreeSet<_>>()).unwrap_or("[]".to_string())
}

fn ownership_subjects_json(ownership: &MemoryOwnership) -> String {
    if ownership.visibility != VISIBILITY_PRINCIPAL {
        return "[]".to_string();
    }
    serde_json::to_string(&[MemorySubject {
        principal: Some(ownership.owner_principal.clone()),
        name: (!ownership.owner_display_name.trim().is_empty())
            .then(|| truncate_chars(&compact_line(&ownership.owner_display_name), 128)),
    }])
    .unwrap_or_else(|_| "[]".to_string())
}

fn organized_subjects_json(
    batch: &OrganizationBatch,
    diary_ids: &[i64],
    declared: &[MemorySubject],
    ownership: &MemoryOwnership,
) -> String {
    if ownership.visibility == VISIBILITY_PUBLIC {
        return "[]".to_string();
    }
    let mut subjects = declared
        .iter()
        .map(|subject| MemorySubject {
            principal: subject
                .principal
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            name: subject
                .name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        })
        .collect::<BTreeSet<_>>();
    for diary in batch
        .diaries
        .iter()
        .filter(|diary| diary_ids.contains(&diary.id))
    {
        if let Some(principal) = diary.owner_principal.as_ref() {
            subjects.insert(MemorySubject {
                principal: Some(principal.clone()),
                name: (!diary.origin.sender_display_name.trim().is_empty())
                    .then(|| truncate_chars(&compact_line(&diary.origin.sender_display_name), 128)),
            });
        }
    }
    serde_json::to_string(&subjects).unwrap_or_else(|_| "[]".to_string())
}

fn normalized_tags_json(tags: &[String]) -> String {
    let tags = tags
        .iter()
        .map(|tag| compact_line(tag))
        .filter(|tag| !tag.is_empty() && tag.chars().count() <= 32)
        .take(8)
        .collect::<BTreeSet<_>>();
    serde_json::to_string(&tags).unwrap_or("[]".to_string())
}

fn sort_json_hits(hits: &mut [Value]) {
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
const SEMANTIC_SKIP_SCORE: f64 = 40.0;
/// Rows embedded per search; the backlog fills in over successive calls rather
/// than making one unlucky search pay for the whole archive.
const SEMANTIC_EMBED_BATCH: usize = 64;
const SEMANTIC_CORPUS_LIMIT: usize = 500;
/// Semantic hits are supporting evidence, not the primary ranking; keyword
/// scores run an order of magnitude higher and should keep the top slots when
/// they matched at all.
const SEMANTIC_SCORE_WEIGHT: f32 = 30.0;

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
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
fn merge_evicted_hits(base: &mut Value, semantic: Vec<Value>, limit: usize) {
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

fn build_evicted_fts_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// `normalized_query` 需已 `compact_line` + 小写化:归一化与被打分的行无关,
/// 调用方在循环外做一次,而不是在每一行上重复三次分配。
fn score_text(text: &str, normalized_query: &str, tokens: &[String]) -> f32 {
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

fn query_tokens(query: &str) -> Vec<String> {
    query_tokens_with_limit(query, 64)
}

fn query_tokens_with_limit(query: &str, limit: usize) -> Vec<String> {
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

fn snippet(text: &str, tokens: &[String], max_chars: usize) -> String {
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

fn compact_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
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

fn count_rows(conn: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn count_where(conn: &Connection, table: &str, condition: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {condition}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn count_skill_dirs(skills_dir: &PathBuf) -> Result<usize> {
    if !skills_dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(skills_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            count += 1;
        }
    }
    Ok(count)
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

/// RFC3339 时间戳 → 本地日期（用于关联记忆展示；解析失败返回 None）
fn association_date(timestamp: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|value| value.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
}

fn diary_content(created_at: &str, user_message: &str, assistant_message: &str) -> String {
    // 第一人称的互动记忆,不是工单:归属(谁说的)由注入行的 [归属=…] 标签
    // 承担,昵称是可改的不可信字段,不进正文。
    format!(
        "{}，对方说：{}；我回：{}",
        created_at,
        truncate_chars(&compact_line(user_message), 260),
        truncate_chars(&compact_line(assistant_message), 520)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::paths::MiyuPaths;

    fn test_paths(temp: &tempfile::TempDir) -> MiyuPaths {
        MiyuPaths {
            root_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            config_file: temp.path().join("config/config.jsonc"),
            skills_dir: temp.path().join("config/skills"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
            state_dir: temp.path().join("state"),
            pictures_dir: temp.path().join("pictures"),
            fish_hook_file: temp.path().join("fish/miyu.fish"),
            bash_hook_file: temp.path().join("shell/bash-hook.sh"),
            zsh_hook_file: temp.path().join("shell/zsh-hook.zsh"),
            scripts_dir: temp.path().join("config/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    #[test]
    fn evicted_search_is_indexed_and_can_be_narrowed_by_time() {
        let temp = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(&AppConfig::default(), &test_paths(&temp));
        store.init().unwrap();
        let rows: Vec<EvictedTurn> = (0..1200)
            .map(|index| EvictedTurn {
                source_id: format!("t{index}:user"),
                timestamp: format!("2026-08-{:02}T10:00:00+00:00", (index % 28) + 1),
                role: "user".to_string(),
                content: format!("第 {index} 轮，聊到了 蓝色小刺猬 这个话题"),
                ..EvictedTurn::default()
            })
            .collect();
        store.remember_evicted_turns(&rows).unwrap();

        // The scan used to stop at the newest 1000 rows, so anything older was
        // stored forever and reachable never.
        let oldest = store
            .search_evicted_context_readonly("第 3 轮", 50, None, None)
            .unwrap();
        assert!(
            oldest["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|hit| hit["snippet"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("第 3 轮")),
            "{oldest}"
        );

        // "What were we talking about that morning" is a question about when.
        let ranged = store
            .search_evicted_context_readonly(
                "蓝色小刺猬",
                50,
                Some("2026-08-05T00:00:00+00:00"),
                Some("2026-08-05T23:59:59+00:00"),
            )
            .unwrap();
        let hits = ranged["results"].as_array().unwrap();
        assert!(!hits.is_empty(), "{ranged}");
        assert!(
            hits.iter().all(|hit| hit["timestamp"]
                .as_str()
                .unwrap_or_default()
                .starts_with("2026-08-05")),
            "{ranged}"
        );
    }

    fn diary_config(batch_size: usize) -> AppConfig {
        let mut config = AppConfig::default();
        config.plugins.memory.diary_batch_size = batch_size;
        config
    }

    fn test_origin() -> MemoryOrigin {
        MemoryOrigin::local("test-session")
    }

    fn platform_origin(user_id: &str, display_name: &str) -> MemoryOrigin {
        MemoryOrigin {
            kind: "platform".to_string(),
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            conversation_kind: "private".to_string(),
            conversation_id: user_id.to_string(),
            sender_id: user_id.to_string(),
            sender_display_name: display_name.to_string(),
            session_id: format!("session-{user_id}"),
            message_id: format!("message-{user_id}"),
        }
    }

    fn scoped_store(
        config: &AppConfig,
        paths: &MiyuPaths,
        origin: &MemoryOrigin,
        privileged: bool,
    ) -> MemoryStore {
        let ownership = origin.principal_ownership().unwrap();
        MemoryStore::new(config, paths).with_request_context(
            if privileged {
                MemoryAccess::Privileged
            } else {
                MemoryAccess::principal(ownership.owner_principal.clone())
            },
            Some(ownership.owner_principal),
            ownership.owner_display_name,
        )
    }

    #[test]
    fn compact_jieba_matches_reference_segmentation() {
        let reference = jieba_rs::Jieba::new();
        for input in [
            "我们中出了一个叛徒",
            "Wayland 输入法需要 XMODIFIERS",
            "Niri窗口规则和中文输入法配置",
            "podman-compose 不能直接重新创建容器",
            "北京烤鸭真好吃，后天天气不好。",
            "Rust 2024 edition与C++20",
        ] {
            assert_eq!(
                JIEBA.cut(input),
                reference.cut(input, false),
                "segmentation differs for {input}"
            );
        }
    }

    fn record_turn(store: &MemoryStore, user: &str, assistant: &str) -> bool {
        let (database_id, generation) = store.identity().unwrap();
        store
            .process_after_turn(user, assistant, &test_origin(), &database_id, generation)
            .unwrap()
    }

    #[test]
    fn remembers_and_recalls_fact() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();
        let result = store.recall_memories("Niri XMODIFIERS", 5, false).unwrap();
        assert!(result.to_string().contains("XMODIFIERS"));
    }

    #[test]
    fn ordinary_principals_recall_only_public_and_owned_memories() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let admin = MemoryStore::new(&config, &paths);
        admin.init().unwrap();
        let timestamp = now();
        admin
            .data_conn()
            .unwrap()
            .execute(
                "INSERT INTO facts (
                    content, source, status, confidence, recall_count, created_at, updated_at,
                    visibility, owner_principal, owner_display_name
                 ) VALUES (?1, 'test', 'active', 1.0, 0, ?2, ?2, 'public', '', '')",
                params!["隔离测试 公共知识", timestamp],
            )
            .unwrap();

        let origin_a = platform_origin("7", "Alice");
        let origin_b = platform_origin("8", "Bob");
        let user_a = scoped_store(&config, &paths, &origin_a, false);
        let user_b = scoped_store(&config, &paths, &origin_b, false);
        user_a
            .remember_fact("隔离测试 Alice 私密事实", "test")
            .unwrap();
        user_b
            .remember_fact("隔离测试 Bob 私密事实", "test")
            .unwrap();
        let (database_id, generation) = user_a.identity().unwrap();
        user_a
            .process_after_turn(
                "隔离测试 Alice 的旧事件",
                "只属于 Alice",
                &origin_a,
                &database_id,
                generation,
            )
            .unwrap();

        let a = user_a
            .recall_memories("隔离测试", 20, false)
            .unwrap()
            .to_string();
        assert!(a.contains("公共知识"));
        assert!(a.contains("Alice 私密事实"));
        assert!(a.contains("Alice 的旧事件"));
        assert!(!a.contains("Bob 私密事实"));

        let b = user_b
            .recall_memories("隔离测试", 20, false)
            .unwrap()
            .to_string();
        assert!(b.contains("公共知识"));
        assert!(b.contains("Bob 私密事实"));
        assert!(!b.contains("Alice 私密事实"));
        assert!(!b.contains("Alice 的旧事件"));
        let b_events = user_b
            .recall_past_events("隔离测试", 20)
            .unwrap()
            .to_string();
        assert!(!b_events.contains("Alice 的旧事件"));
        let a_events = user_a
            .recall_past_events("隔离测试", 20)
            .unwrap()
            .to_string();
        assert!(a_events.contains("Alice 的旧事件"));

        let privileged = admin
            .recall_memories("隔离测试", 20, false)
            .unwrap()
            .to_string();
        assert!(privileged.contains("公共知识"));
        assert!(privileged.contains("Alice 私密事实"));
        assert!(privileged.contains("Bob 私密事实"));
        assert!(privileged.contains("Alice 的旧事件"));
    }

    #[test]
    fn evicted_context_uses_the_same_principal_filter() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let origin_a = platform_origin("7", "Alice");
        let origin_b = platform_origin("8", "Bob");
        let user_a = scoped_store(&config, &paths, &origin_a, false);
        let user_b = scoped_store(&config, &paths, &origin_b, false);
        user_a
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "a:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "淘汰记忆 Alice 专属".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();
        user_b
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "b:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "淘汰记忆 Bob 专属".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();

        let a = user_a
            .search_evicted_context("淘汰记忆", 10)
            .unwrap()
            .to_string();
        assert!(a.contains("Alice 专属"));
        assert!(!a.contains("Bob 专属"));
        let all = MemoryStore::new(&config, &paths)
            .search_evicted_context("淘汰记忆", 10)
            .unwrap()
            .to_string();
        assert!(all.contains("Alice 专属"));
        assert!(all.contains("Bob 专属"));
    }

    #[test]
    fn access_migration_backfills_platform_principals_conservatively() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let origin = platform_origin("7", "Alice");
        let (database_id, generation) = store.identity().unwrap();
        store
            .process_after_turn(
                "迁移归属测试",
                "迁移回答",
                &origin,
                &database_id,
                generation,
            )
            .unwrap();
        let conn = store.data_conn().unwrap();
        let episode_id = conn
            .query_row("SELECT id FROM episodes LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO facts (
                content, source, status, confidence, recall_count, created_at, updated_at,
                source_episode_ids, visibility, owner_principal, owner_display_name
             ) VALUES ('迁移事实', 'test', 'active', 1.0, 0, ?1, ?1, ?2,
                       'privileged', '', '')",
            params![now(), serde_json::to_string(&vec![episode_id]).unwrap()],
        )
        .unwrap();
        conn.execute(
            "UPDATE episodes SET visibility='privileged', owner_principal='', owner_display_name=''",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE memory_meta SET access_schema_version=0 WHERE id=1",
            [],
        )
        .unwrap();
        drop(conn);

        store.init().unwrap();
        let expected = origin.principal_ownership().unwrap().owner_principal;
        let conn = store.data_conn().unwrap();
        let episode_owner = conn
            .query_row(
                "SELECT visibility, owner_principal FROM episodes WHERE id=?1",
                [episode_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        let fact_owner = conn
            .query_row(
                "SELECT visibility, owner_principal FROM facts WHERE content='迁移事实'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(
            episode_owner,
            (VISIBILITY_PRINCIPAL.to_string(), expected.clone())
        );
        assert_eq!(fact_owner, (VISIBILITY_PRINCIPAL.to_string(), expected));
    }

    #[test]
    fn organizer_can_publish_general_facts_but_cannot_update_another_principal() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let origin_a = platform_origin("7", "Alice");
        let origin_b = platform_origin("8", "Bob");
        let user_a = scoped_store(&config, &paths, &origin_a, false);
        let user_b = scoped_store(&config, &paths, &origin_b, false);
        let bob_fact = user_b
            .remember_fact("Linux 隔离主题是 Bob 的私人偏好", "test")
            .unwrap();
        let (database_id, generation) = user_b.identity().unwrap();
        user_b
            .process_after_turn(
                "Linux 隔离主题 Bob 的设置",
                "Bob 使用另一种方式",
                &origin_b,
                &database_id,
                generation,
            )
            .unwrap();
        let (database_id, generation) = user_a.identity().unwrap();
        user_a
            .process_after_turn(
                "Linux 隔离主题与通用命令",
                "使用 systemctl --user",
                &origin_a,
                &database_id,
                generation,
            )
            .unwrap();
        let batch = MemoryStore::new(&config, &paths)
            .next_organization_batch()
            .unwrap()
            .unwrap();
        assert!(batch.existing.iter().any(|memory| memory.id == bob_fact));
        let alice_principal = origin_a.principal_ownership().unwrap().owner_principal;
        let source_id = batch
            .diaries
            .iter()
            .find(|diary| diary.owner_principal.as_deref() == Some(alice_principal.as_str()))
            .unwrap()
            .id;
        let cross_user_update = OrganizedOutput {
            knowledge: vec![KnowledgeAction {
                operation: "update".to_string(),
                target_id: Some(bob_fact),
                memory_type: "preference".to_string(),
                content: "Linux 隔离主题被 Alice 覆盖".to_string(),
                truth_status: "reported".to_string(),
                importance: 3,
                confidence: 0.8,
                visibility: VISIBILITY_PRINCIPAL.to_string(),
                subjects: Vec::new(),
                tags: Vec::new(),
                diary_ids: vec![source_id],
            }],
            long_diaries: Vec::new(),
        };
        assert!(MemoryStore::new(&config, &paths)
            .apply_organized_batch(&batch, cross_user_update)
            .unwrap_err()
            .to_string()
            .contains("different principal"));

        let leaky_public_fact = OrganizedOutput {
            knowledge: vec![KnowledgeAction {
                operation: "create".to_string(),
                target_id: None,
                memory_type: "fact".to_string(),
                content: "Alice 使用 Linux 的私人经历".to_string(),
                truth_status: "reported".to_string(),
                importance: 3,
                confidence: 0.8,
                visibility: VISIBILITY_PUBLIC.to_string(),
                subjects: Vec::new(),
                tags: Vec::new(),
                diary_ids: vec![source_id],
            }],
            long_diaries: Vec::new(),
        };
        assert!(MemoryStore::new(&config, &paths)
            .apply_organized_batch(&batch, leaky_public_fact)
            .unwrap_err()
            .to_string()
            .contains("source identity marker"));

        MemoryStore::new(&config, &paths)
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: vec![KnowledgeAction {
                        operation: "create".to_string(),
                        target_id: None,
                        memory_type: "fact".to_string(),
                        content: "Linux 通用知识使用 systemctl --user".to_string(),
                        truth_status: "accepted".to_string(),
                        importance: 3,
                        confidence: 0.9,
                        visibility: VISIBILITY_PUBLIC.to_string(),
                        subjects: Vec::new(),
                        tags: vec!["Linux".to_string()],
                        diary_ids: vec![source_id],
                    }],
                    long_diaries: Vec::new(),
                },
            )
            .unwrap();
        let bob_recall = user_b
            .recall_memories("systemctl user", 10, false)
            .unwrap()
            .to_string();
        assert!(bob_recall.contains("Linux 通用知识"));
    }

    #[test]
    fn association_excludes_own_sessions_visible_echo() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store.init().unwrap();
        store
            .data_conn()
            .unwrap()
            .execute(
                "INSERT INTO episodes (content, source, status, recall_count, created_at, updated_at, retention, origin_session_id)
                 VALUES ('对方提到自回声话题', 'auto_diary', 'active', 0, ?1, ?1, 'short_term', 's1')",
                [chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        // 无排除:能召回。
        assert!(store.association("自回声", None).unwrap().is_some());
        // 同会话且晚于最老可见轮 → 自回声被滤(原对话就在眼前)。
        let exclusion = AssociationExclusion {
            session_id: "s1".to_string(),
            since: "2000-01-01T00:00:00Z".to_string(),
        };
        assert!(store
            .association("自回声", Some(&exclusion))
            .unwrap()
            .is_none());
        // 别的会话不受排除影响。
        let other = AssociationExclusion {
            session_id: "s2".to_string(),
            since: "2000-01-01T00:00:00Z".to_string(),
        };
        assert!(store.association("自回声", Some(&other)).unwrap().is_some());
    }

    #[test]
    fn unrelated_and_rejected_memories_are_not_associated() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let rejected = store.remember_fact("旧的错误结论", "test").unwrap();
        store
            .data_conn()
            .unwrap()
            .execute(
                "UPDATE facts SET truth_status='rejected' WHERE id=?1",
                [rejected],
            )
            .unwrap();
        assert!(store.association("完全无关的主题", None).unwrap().is_none());
        assert!(store.association("错误结论", None).unwrap().is_none());
    }

    #[test]
    fn association_format_always_keeps_its_closing_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.plugins.memory.association_max_chars = 128;
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let hit = MemoryHit {
            id: 1,
            kind: MemoryKind::Fact,
            content: "很长的知识点".repeat(100),
            score: 1.0,
            timestamp: now(),
            source: "test".to_string(),
            retention: None,
            visibility: VISIBILITY_PUBLIC.to_string(),
            owner_principal: String::new(),
            owner_display_name: String::new(),
            subjects: "[]".to_string(),
            source_episode_ids: Vec::new(),
            origin_session_id: String::new(),
        };
        let formatted = store.format_association(&AssociationContext {
            facts: vec![hit],
            episodes: Vec::new(),
            organization_due: false,
        });
        assert!(formatted.ends_with("</associative-memory>"));
        assert!(formatted.chars().count() <= 128);
    }

    #[test]
    fn association_lines_carry_date_and_dedupe_diary_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let stamp = now();
        let date = association_date(&stamp).unwrap();
        let base = MemoryHit {
            id: 1,
            kind: MemoryKind::Fact,
            content: "知识点内容".to_string(),
            score: 1.0,
            timestamp: stamp.clone(),
            source: "test".to_string(),
            retention: None,
            visibility: VISIBILITY_PUBLIC.to_string(),
            owner_principal: String::new(),
            owner_display_name: String::new(),
            subjects: "[]".to_string(),
            source_episode_ids: Vec::new(),
            origin_session_id: String::new(),
        };
        let diary = MemoryHit {
            id: 2,
            kind: MemoryKind::Diary,
            content: format!("{stamp}，对方说：测试；我回：通过"),
            retention: Some(SHORT_TERM.to_string()),
            ..base.clone()
        };
        let formatted = store.format_association(&AssociationContext {
            facts: vec![base],
            episodes: vec![diary],
            organization_due: false,
        });
        assert!(formatted.contains(&format!("[{date}] [公共知识] 知识点内容")));
        assert!(formatted.contains(&format!("[{date}] [公共知识] 对方说：测试；我回：通过")));
        assert!(!formatted.contains(&stamp));
    }

    #[test]
    fn diary_content_reads_as_a_first_person_exchange() {
        let content = diary_content(
            "2026-08-10T12:00:00+00:00",
            "wps 保存文件默认的编码是gbk吗",
            "分情况：纯文本默认 GBK，docx 内部是 UTF-8",
        );
        assert_eq!(
            content,
            "2026-08-10T12:00:00+00:00，对方说：wps 保存文件默认的编码是gbk吗；我回：分情况：纯文本默认 GBK，docx 内部是 UTF-8"
        );
    }

    #[test]
    fn association_dedup_filters_visible_lines_and_keeps_changed_ones() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(store.association_dedup_enabled());
        let stamp = now();
        let fact = MemoryHit {
            id: 1,
            kind: MemoryKind::Fact,
            content: "AUR 的 GitHub 镜像只读".to_string(),
            score: 1.0,
            timestamp: stamp.clone(),
            source: "test".to_string(),
            retention: None,
            visibility: VISIBILITY_PUBLIC.to_string(),
            owner_principal: String::new(),
            owner_display_name: String::new(),
            subjects: "[]".to_string(),
            source_episode_ids: Vec::new(),
            origin_session_id: String::new(),
        };
        let diary = MemoryHit {
            id: 2,
            kind: MemoryKind::Diary,
            content: "对方说：测试；我回：通过".to_string(),
            retention: Some(SHORT_TERM.to_string()),
            ..fact.clone()
        };
        let updated_fact = MemoryHit {
            id: 1,
            content: "AUR 的 GitHub 镜像只读，推送需走官方地址".to_string(),
            ..fact.clone()
        };
        // 第一回合的注入块回放时携带的行
        let first = store.format_association(&AssociationContext {
            facts: vec![fact.clone()],
            episodes: vec![diary.clone()],
            organization_due: false,
        });
        let seen = first
            .lines()
            .filter(|line| line.starts_with("- ["))
            .collect::<HashSet<_>>();
        assert_eq!(seen.len(), 2);
        // 未变化的 fact 与 diary 被过滤；内容更新过的 fact 保留
        let mut association = AssociationContext {
            facts: vec![fact.clone(), updated_fact],
            episodes: vec![diary],
            organization_due: false,
        };
        store.retain_unseen_association(&mut association, &seen);
        assert_eq!(association.facts.len(), 1);
        assert!(association.facts[0].content.contains("官方地址"));
        assert!(association.episodes.is_empty());
        // 空 seen 集不过滤
        let mut untouched = AssociationContext {
            facts: vec![fact],
            episodes: Vec::new(),
            organization_due: false,
        };
        store.retain_unseen_association(&mut untouched, &HashSet::new());
        assert_eq!(untouched.facts.len(), 1);
    }

    #[test]
    fn reset_all_clears_facts_and_episodes() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();
        store.remember_pending_event("你好", "在呢").unwrap();
        store.flush_pending_events().unwrap();

        let before = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
        assert!(!before["facts"].as_array().unwrap().is_empty());
        assert!(!before["episodes"].as_array().unwrap().is_empty());

        store.reset_all(false).unwrap();

        let after = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
        assert!(after["facts"].as_array().unwrap().is_empty());
        assert!(after["episodes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn evicted_context_can_be_cleared() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "turn-1:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "旧上下文 输入法".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();
        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "turn-1:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "旧上下文 输入法".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();
        assert_eq!(
            store.search_evicted_context("输入法", 5).unwrap()["results"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .search_evicted_context("输入法", 5)
            .unwrap()
            .to_string()
            .contains("旧上下文"));
        store.clear_evicted_context().unwrap();
        assert!(!store
            .search_evicted_context("输入法", 5)
            .unwrap()
            .to_string()
            .contains("旧上下文"));
    }

    #[test]
    fn disabled_writes_block_content_but_allow_recall_reinforcement() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let mut store = MemoryStore::new(&config, &paths);
        let fact_id = store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();

        store.set_writes_enabled(false);
        assert_eq!(store.remember_fact("不应保存", "test").unwrap(), 0);
        assert!(!record_turn(&store, "不应写入日记", "不会写入"));
        assert!(store.prepare_evicted_context_db().unwrap().is_none());

        let association = store.association("Niri XMODIFIERS", None).unwrap();
        assert!(association.is_some());
        let conn = store.data_conn().unwrap();
        let recall_count = conn
            .query_row(
                "SELECT recall_count FROM facts WHERE id=?1",
                [fact_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(recall_count, 1);
        assert_eq!(count_rows(&conn, "facts").unwrap(), 1);
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 0);
        assert_eq!(count_rows(&conn, "pending_events").unwrap(), 0);
    }

    #[test]
    fn diary_batch_starts_only_at_the_configured_turn_count() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(14);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        for index in 0..13 {
            assert!(record_turn(
                &store,
                &format!("问题 {index}"),
                &format!("回答 {index}")
            ));
        }
        assert!(store.next_organization_batch().unwrap().is_none());
        assert!(record_turn(&store, "第十四问", "第十四答"));
        let batch = store.next_organization_batch().unwrap().unwrap();
        assert_eq!(batch.diaries.len(), 14);
        assert_eq!(batch.diaries[0].origin.kind, "local");
        assert_eq!(batch.diaries[0].origin.session_id, "test-session");

        store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .unwrap();
        let conn = store.data_conn().unwrap();
        assert_eq!(
            count_where(
                &conn,
                "episodes",
                "retention='short_term' AND consolidated_at IS NULL"
            )
            .unwrap(),
            0
        );
        assert_eq!(
            count_where(&conn, "episodes", "retention='short_term'").unwrap(),
            14
        );
    }

    #[test]
    fn third_recall_requires_and_applies_long_diary_promotion() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(14);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(record_turn(&store, "Wayland 输入法配置", "设置 XMODIFIERS"));
        for _ in 0..3 {
            assert!(store.association("Wayland 输入法", None).unwrap().is_some());
        }
        let batch = store.next_organization_batch().unwrap().unwrap();
        assert_eq!(batch.diaries.len(), 1);
        assert!(batch.diaries[0].force_long_term);
        let source_id = batch.diaries[0].id;
        store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: vec![LongDiaryDraft {
                        content: "我曾帮助处理 Wayland 输入法配置。".to_string(),
                        importance: 3,
                        confidence: 0.9,
                        visibility: VISIBILITY_PRIVILEGED.to_string(),
                        subjects: Vec::new(),
                        tags: vec!["Wayland".to_string(), "输入法".to_string()],
                        diary_ids: vec![source_id],
                    }],
                },
            )
            .unwrap();

        let conn = store.data_conn().unwrap();
        let (pending, promoted): (i64, Option<String>) = conn
            .query_row(
                "SELECT promotion_pending, promoted_at FROM episodes WHERE id=?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(pending, 0);
        assert!(promoted.is_some());
        assert_eq!(
            count_where(&conn, "episodes", "retention='long_term'").unwrap(),
            1
        );
    }

    #[test]
    fn reset_all_invalidates_an_inflight_organization_batch() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(record_turn(&store, "问题一", "回答一"));
        assert!(record_turn(&store, "问题二", "回答二"));
        let batch = store.next_organization_batch().unwrap().unwrap();
        let stale_database_id = batch.database_id.clone();
        let stale_generation = batch.generation;

        store.reset_all(false).unwrap();
        assert!(!store
            .process_after_turn(
                "重置前启动的问题",
                "不应写回",
                &test_origin(),
                &stale_database_id,
                stale_generation,
            )
            .unwrap());
        assert!(store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .is_err());
        let conn = store.data_conn().unwrap();
        assert_eq!(count_rows(&conn, "facts").unwrap(), 0);
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 0);
    }

    #[test]
    fn cleanup_deletes_only_expired_consolidated_short_diaries() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store.init().unwrap();
        let conn = store.data_conn().unwrap();
        conn.execute(
            "INSERT INTO episodes (
                content, source, status, created_at, updated_at, retention,
                expires_at, consolidated_at
             ) VALUES ('expired', 'episode', 'active', ?1, ?1, 'short_term', ?1, ?1)",
            ["2020-01-01T00:00:00Z"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO episodes (
                content, source, status, created_at, updated_at, retention,
                expires_at, consolidated_at
             ) VALUES ('pending', 'episode', 'active', ?1, ?1, 'short_term', ?1, NULL)",
            ["2020-01-01T00:00:00Z"],
        )
        .unwrap();
        drop(conn);

        assert_eq!(store.cleanup_expired_short_diaries().unwrap(), 1);
        let conn = store.data_conn().unwrap();
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 1);
        assert_eq!(
            conn.query_row("SELECT content FROM episodes", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "pending"
        );
        assert_eq!(
            conn.query_row("SELECT status FROM episodes", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "forgotten"
        );
    }

    #[test]
    fn existing_episodes_migrate_as_long_term_diaries() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        std::fs::create_dir_all(store.data_db.parent().unwrap()).unwrap();
        let conn = Connection::open(&store.data_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE episodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'episode',
                status TEXT NOT NULL DEFAULT 'active',
                strength REAL NOT NULL DEFAULT 1.0,
                recall_count INTEGER NOT NULL DEFAULT 0,
                last_recalled_at TEXT,
                last_decay_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             INSERT INTO episodes (content, created_at, updated_at)
             VALUES ('旧版长期经历', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z');",
        )
        .unwrap();
        drop(conn);

        store.init().unwrap();
        let conn = store.data_conn().unwrap();
        assert_eq!(
            conn.query_row("SELECT retention FROM episodes", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            LONG_TERM
        );
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 1);
    }

    #[test]
    fn organizer_never_recreates_a_moved_persona_database() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(record_turn(&store, "问题一", "回答一"));
        assert!(record_turn(&store, "问题二", "回答二"));
        let batch = store.next_organization_batch().unwrap().unwrap();
        let memory_dir = store.data_db.parent().unwrap().to_path_buf();
        let moved_dir = memory_dir.with_file_name("memory-moved");
        std::fs::rename(&memory_dir, &moved_dir).unwrap();

        assert!(store.next_organization_batch().unwrap().is_none());
        assert!(!memory_dir.exists());
        assert!(store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .is_err());
        assert!(!memory_dir.exists());

        store.init().unwrap();
        assert!(store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .is_err());
    }
}
