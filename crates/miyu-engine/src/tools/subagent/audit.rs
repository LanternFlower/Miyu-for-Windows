use super::*;

/// Persists an audit session for a subagent run: a hidden `kind='subagent'`
/// session linked to the parent turn's session, holding one turn (prompt →
/// result JSON) plus the model identity and token usage on the session row.
/// Best-effort: audit failures never fail the task itself.
/// 一趟子代理的审计会话：开跑之前就建好，边跑边记账，跑完写结果。
///
/// 原来是**跑完才写**的一锤子买卖——中途被打断（Ctrl+C、超时、daemon 重启）
/// 这一趟烧掉的词元就彻底没了，会话累计里查无此事（用户问：万一中断了不就
/// 丢失数据了吗）。现在开跑就有一行，量报每来一次就更新它。
pub(super) struct SubagentAudit {
    store: miyu_core::state::StateStore,
    session_id: String,
    turn_id: String,
    context_window: Option<i64>,
}

impl SubagentAudit {
    pub(super) fn open(
        context: &SubagentContext,
        anchor: &AuditAnchor,
        description: &str,
        prompt: &str,
    ) -> Option<Self> {
        let outcome = (|| -> Result<Self> {
            let store = miyu_core::state::StateStore::new(&context.paths)?;
            let name: String = description.chars().take(40).collect();
            let record = store.create_session(
                &anchor.persona,
                &name,
                "subagent",
                anchor.parent.as_deref(),
            )?;
            let turn_id = format!(
                "sat_{}_{:08x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis())
                    .unwrap_or(0),
                rand::random::<u32>()
            );
            store
                .pinned(&record.session_id)
                .start_turn(&turn_id, prompt, std::process::id())?;
            Ok(Self {
                store,
                session_id: record.session_id,
                turn_id,
                context_window: None,
            })
        })();
        match outcome {
            Ok(audit) => Some(audit),
            Err(error) => {
                tracing::warn!(error = %error, "{}", miyu_base::i18n::text("failed to open the subagent audit session", "建立子代理审计会话失败"));
                None
            }
        }
    }

    pub(super) fn usage_sink(&self) -> std::sync::Arc<dyn Fn(&SubagentStats) + Send + Sync> {
        let store = self.store.clone();
        let session_id = self.session_id.clone();
        let context_window = self.context_window;
        std::sync::Arc::new(move |stats: &SubagentStats| {
            let _ = store.record_subagent_usage(
                &session_id,
                None,
                None,
                context_window,
                stats.prompt_tokens as i64,
                stats.completion_tokens as i64,
                stats.total_tokens.max(stats.token_estimate) as i64,
                stats.cache_read_tokens as i64,
            );
        })
    }

    /// 收尾：写结果、补上端点与最终用量。
    pub(super) fn finish(
        &self,
        context: &SubagentContext,
        output: &str,
        stats: Option<&SubagentStats>,
        model_choice: &Option<(String, String)>,
    ) {
        let outcome = (|| -> Result<()> {
            self.store
                .pinned(&self.session_id)
                .complete_turn(&self.turn_id, output, None)?;
            let (provider_id, model) = match model_choice.as_ref() {
                Some((provider_id, model)) => (Some(provider_id.as_str()), Some(model.as_str())),
                None => (None, None),
            };
            let context_window = match (provider_id, model) {
                (Some(provider), Some(model)) => context
                    .config
                    .context_window_for_provider_model(provider, model)
                    .ok()
                    .flatten()
                    .map(|window| window as i64),
                _ => None,
            };
            let (prompt_tokens, completion_tokens, total_tokens, cache_read_tokens) = match stats {
                Some(stats) => (
                    stats.prompt_tokens as i64,
                    stats.completion_tokens as i64,
                    stats.total_tokens.max(stats.token_estimate) as i64,
                    stats.cache_read_tokens as i64,
                ),
                None => (0, 0, 0, 0),
            };
            self.store.record_subagent_usage(
                &self.session_id,
                provider_id,
                model,
                context_window,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_read_tokens,
            )
        })();
        if let Err(error) = outcome {
            tracing::warn!(error = %error, "{}", miyu_base::i18n::text("failed to record subagent audit session", "记录子代理审计会话失败"));
        }
    }
}

pub(super) fn record_subagent_audit(
    context: &SubagentContext,
    anchor: &AuditAnchor,
    description: &str,
    prompt: &str,
    output: &str,
    stats: Option<&SubagentStats>,
    model_choice: &Option<(String, String)>,
) {
    let outcome = (|| -> Result<()> {
        let store = miyu_core::state::StateStore::new(&context.paths)?;
        let parent = anchor.parent.clone();
        let persona = anchor.persona.clone();
        let name: String = description.chars().take(40).collect();
        let record = store.create_session(&persona, &name, "subagent", parent.as_deref())?;
        let pinned = store.pinned(&record.session_id);
        let turn_id = format!(
            "sat_{}_{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        pinned.start_turn(&turn_id, prompt, std::process::id())?;
        pinned.complete_turn(&turn_id, output, None)?;
        let (provider_id, model) = match model_choice.as_ref() {
            Some((provider_id, model)) => (Some(provider_id.as_str()), Some(model.as_str())),
            None => (None, None),
        };
        let context_window = match (provider_id, model) {
            (Some(provider), Some(model)) => context
                .config
                .context_window_for_provider_model(provider, model)
                .ok()
                .flatten()
                .map(|window| window as i64),
            _ => None,
        };
        let (prompt_tokens, completion_tokens, total_tokens, cache_read_tokens) = match stats {
            Some(stats) => (
                stats.prompt_tokens as i64,
                stats.completion_tokens as i64,
                stats.total_tokens.max(stats.token_estimate) as i64,
                stats.cache_read_tokens as i64,
            ),
            None => (0, 0, 0, 0),
        };
        store.record_subagent_usage(
            &record.session_id,
            provider_id,
            model,
            context_window,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
        )
    })();
    if let Err(error) = outcome {
        tracing::warn!(error = %error, "{}", miyu_base::i18n::text("failed to record subagent audit session", "记录子代理审计会话失败"));
    }
}
