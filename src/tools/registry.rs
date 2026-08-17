use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::tools::tool_descriptions::{self, LoadPolicy};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

pub type ToolFuture = Pin<Box<dyn Future<Output = Result<String>> + Send>>;
pub type ToolHandler = Arc<dyn Fn(Value, ToolProgress) -> ToolFuture + Send + Sync>;

/// 单调执行守卫:返回 Some(理由) 即拒绝本次调用,返回 None 放行。
/// 只拒不放——任何 guard 拒绝后,后续 guard 与调用方都无法翻案
/// (dsh ToolGuard 同款语义)。拒绝以普通 tool error 回给模型,轮次存活。
pub type ToolGuard = Arc<dyn Fn(&ToolSpec, &Value, &GuardCtx) -> Option<String> + Send + Sync>;

/// 守卫可见的回合上下文。registry 自身无回合概念,由调用方(agent 主循环、
/// 未来的工具桥)按次构造;拿不到上下文的路径(subagent 的 call)用默认值,
/// 仅参数级守卫生效。
#[derive(Default)]
pub struct GuardCtx<'a> {
    /// 本回合此前已请求过的工具名(含本次,按主循环 push 时序)。
    pub used_tools: &'a [String],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub enum ToolProgressEvent {
    Message(String),
    PrepareForExternalOutput {
        ready: oneshot::Sender<bool>,
    },
    Image {
        path: PathBuf,
        alt: String,
    },
    Artifact {
        path: PathBuf,
        title: String,
    },
    CommandOutput {
        stream: CommandOutputStream,
        chunk: Vec<u8>,
    },
}

#[derive(Clone, Default)]
pub struct ToolProgress {
    sender: Option<mpsc::UnboundedSender<ToolProgressEvent>>,
}

impl ToolProgress {
    pub fn new(sender: mpsc::UnboundedSender<ToolProgressEvent>) -> Self {
        Self {
            sender: Some(sender),
        }
    }

    pub fn report(&self, message: impl Into<String>) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(ToolProgressEvent::Message(message.into()));
        }
    }

    pub fn report_command_output(&self, stream: CommandOutputStream, chunk: Vec<u8>) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(ToolProgressEvent::CommandOutput { stream, chunk });
        }
    }

    pub fn report_image(&self, path: impl Into<PathBuf>, alt: impl Into<String>) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(ToolProgressEvent::Image {
                path: path.into(),
                alt: alt.into(),
            });
        }
    }

    pub fn report_artifact(&self, path: impl Into<PathBuf>, title: impl Into<String>) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(ToolProgressEvent::Artifact {
                path: path.into(),
                title: title.into(),
            });
        }
    }

    pub async fn prepare_for_external_output(&self) -> bool {
        let Some(sender) = &self.sender else {
            return true;
        };
        let (ready, receiver) = oneshot::channel();
        if sender
            .send(ToolProgressEvent::PrepareForExternalOutput { ready })
            .is_ok()
        {
            return receiver.await.unwrap_or(false);
        }
        false
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[tokio::test]
    async fn external_output_waits_for_renderer_acknowledgement() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let progress = ToolProgress::new(sender);
        let prepare = progress.prepare_for_external_output();
        tokio::pin!(prepare);

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut prepare)
                .await
                .is_err()
        );

        let ToolProgressEvent::PrepareForExternalOutput { ready } = receiver.recv().await.unwrap()
        else {
            panic!("expected external output preparation event");
        };
        ready.send(true).unwrap();
        assert!(prepare.await);
    }
}

#[derive(Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub permission: ToolPermission,
    pub display_name: Option<String>,
    pub always_loaded: bool,
    pub is_script: bool,
    pub load_policy: LoadPolicy,
    pub groups: Vec<String>,
    /// 按工具超时（秒）。None=吃 registry 默认兜底；Some(0)=豁免。
    pub timeout_seconds: Option<u64>,
    handler: ToolHandler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolPermission {
    ReadOnly,
    Presentation,
    Writes,
}

impl ToolSpec {
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            permission: ToolPermission::ReadOnly,
            display_name: None,
            always_loaded: true,
            is_script: false,
            load_policy: LoadPolicy::Summary,
            groups: Vec::new(),
            timeout_seconds: None,
            handler: Arc::new(move |args, _progress| Box::pin(handler(args))),
        }
    }

    pub fn new_with_progress<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(Value, ToolProgress) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            permission: ToolPermission::ReadOnly,
            display_name: None,
            always_loaded: true,
            is_script: false,
            load_policy: LoadPolicy::Summary,
            groups: Vec::new(),
            timeout_seconds: None,
            handler: Arc::new(move |args, progress| Box::pin(handler(args, progress))),
        }
    }

    pub fn writes(mut self) -> Self {
        self.permission = ToolPermission::Writes;
        self
    }

    pub fn presentation(mut self) -> Self {
        self.permission = ToolPermission::Presentation;
        self
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    pub fn with_always_loaded(mut self, always_loaded: bool) -> Self {
        self.always_loaded = always_loaded;
        self
    }

    pub fn with_load_policy(mut self, load_policy: LoadPolicy) -> Self {
        self.load_policy = load_policy;
        self
    }

    pub fn with_groups(mut self, groups: Vec<String>) -> Self {
        self.groups = groups
            .into_iter()
            .map(|group| group.trim().to_string())
            .filter(|group| !group.is_empty())
            .collect();
        self
    }

    pub fn script(mut self) -> Self {
        self.is_script = true;
        self
    }

    pub fn with_timeout_seconds(mut self, secs: u64) -> Self {
        self.timeout_seconds = Some(secs);
        self
    }

    pub fn apply_built_in_description(mut self) -> Self {
        if let Some(desc) = crate::tools::tool_descriptions::get(&self.name) {
            // load_skill owns a dynamic catalog description, but still uses
            // the same loading policy, groups, schema, and display metadata
            // as every other built-in tool.
            if self.name != "load_skill" {
                self.description = desc.description.clone();
            }
            self.parameters = desc.parameters.clone();
            self.display_name = Some(desc.display_name.clone());
            self.always_loaded = desc.always_loaded;
            self.load_policy = desc.load_policy;
            self.groups = desc.groups.clone();
            if desc.timeout_seconds.is_some() {
                self.timeout_seconds = desc.timeout_seconds;
            }
        }
        self
    }

    pub fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function",
            function: FunctionDefinition {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.parameters.clone(),
            },
        }
    }

    async fn call(&self, args: Value, progress: ToolProgress) -> Result<String> {
        (self.handler)(args, progress).await
    }

    fn call_future(&self, args: Value, progress: ToolProgress) -> ToolFuture {
        (self.handler)(args, progress)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnregisteredScript {
    pub name: String,
    pub path: String,
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<ToolSpec>>,
    script_tool_names: BTreeSet<String>,
    unregistered_scripts: Vec<UnregisteredScript>,
    skill_catalog_fingerprint: Option<[u8; 32]>,
    /// 兜底超时：工具未声明 timeout_seconds 时生效。None=不兜底（默认构
    /// 造/测试保持旧行为），工厂函数按 config.tools.default_timeout_secs
    /// 注入。防的是 MCP/web/生图这类没有自管超时的工具把回合无限挂死；
    /// run_command 等自管工具用 timeout_seconds=0 豁免。
    default_timeout: Option<std::time::Duration>,
    /// 单调守卫链,按注册序求值,第一个拒绝即终。
    guards: Vec<ToolGuard>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: ToolSpec) {
        let tool = tool.apply_built_in_description();
        self.tools.insert(tool.name.clone(), Arc::new(tool));
    }

    /// 0 = 关闭兜底超时。
    pub fn set_default_timeout_secs(&mut self, secs: u64) {
        self.default_timeout = (secs > 0).then(|| std::time::Duration::from_secs(secs));
    }

    pub fn add_guard(&mut self, guard: ToolGuard) {
        self.guards.push(guard);
    }

    fn guard_denial(&self, tool: &ToolSpec, args: &Value, ctx: &GuardCtx) -> Option<String> {
        self.guards
            .iter()
            .find_map(|guard| guard(tool, args, ctx))
    }

    fn effective_timeout(&self, tool: &ToolSpec) -> Option<std::time::Duration> {
        match tool.timeout_seconds {
            Some(0) => None,
            Some(secs) => Some(std::time::Duration::from_secs(secs)),
            None => self.default_timeout,
        }
    }

    fn timeout_error(name: &str, limit: std::time::Duration) -> anyhow::Error {
        let secs = limit.as_secs();
        if crate::i18n::agent_is_zh() {
            anyhow::anyhow!("工具 `{name}` 执行超时（超过 {secs} 秒）已中止")
        } else {
            anyhow::anyhow!("tool `{name}` timed out after {secs}s and was aborted")
        }
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        self.script_tool_names.remove(name);
        self.tools.remove(name).is_some()
    }

    pub(crate) fn skill_catalog_fingerprint(&self) -> Option<[u8; 32]> {
        self.skill_catalog_fingerprint
    }

    pub(crate) fn set_skill_catalog_fingerprint(&mut self, fingerprint: [u8; 32]) {
        self.skill_catalog_fingerprint = Some(fingerprint);
    }

    /// Appends runtime info to a registered tool's description. Applied
    /// after `apply_built_in_description`, so it survives the built-in
    /// overlay (which wholesale replaces the description). The registry is
    /// rebuilt per turn, keeping such suffixes current with the config.
    pub fn amend_description(&mut self, name: &str, suffix: &str) {
        if suffix.is_empty() {
            return;
        }
        if let Some(tool) = self.tools.get(name) {
            let mut spec = (**tool).clone();
            spec.description.push_str(suffix);
            self.tools.insert(name.to_string(), Arc::new(spec));
        }
    }

    pub fn replace_script_tools(
        &mut self,
        scripts: Vec<ToolSpec>,
        mut unregistered: Vec<UnregisteredScript>,
    ) -> Result<()> {
        let mut names = BTreeSet::new();
        let mut accepted = Vec::new();
        for script in scripts {
            if !script.is_script {
                bail!("script tool is missing script origin: {}", script.name);
            }
            if !names.insert(script.name.clone()) {
                bail!("duplicate script id: {}", script.name);
            }
            // occupant 是否脚本按注册表现状判断,不依赖 script_tool_names:
            // 脚本先注册、同名 MCP 工具后覆盖时,名单还挂着旧名字,按名单
            // 判会把 MCP 工具当成脚本顶掉。
            if script.name == "load_tools"
                || crate::tools::tool_descriptions::get(&script.name).is_some()
                || self
                    .tools
                    .get(&script.name)
                    .is_some_and(|tool| !tool.is_script)
            {
                continue;
            }
            accepted.push(script);
        }

        for name in &self.script_tool_names {
            if self.tools.get(name).is_some_and(|tool| tool.is_script) {
                self.tools.remove(name);
            }
        }
        self.script_tool_names.clear();

        for script in accepted {
            self.script_tool_names.insert(script.name.clone());
            self.tools.insert(script.name.clone(), Arc::new(script));
        }

        unregistered.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
        self.unregistered_scripts = unregistered;
        Ok(())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();
        definitions.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        definitions
    }

    pub fn lazy_definitions(&self, loaded: &BTreeSet<String>) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .filter(|tool| tool.always_loaded || loaded.contains(&tool.name))
            .map(|tool| {
                let mut definition = tool.definition();
                if tool.name == "load_tools" {
                    // v7 Phase 1.3-b: the catalog always lists the full target
                    // set instead of subtracting `loaded`, so the description
                    // stays byte-stable across lazy loads within a session and
                    // the tools array prefix keeps hitting the provider cache.
                    // Re-loading an already-loaded target is tolerated by
                    // expand_load_targets with a clear notice.
                    definition.function.description =
                        super::load_tools::dynamic_description(self, &BTreeSet::new());
                }
                definition
            })
            .collect::<Vec<_>>();
        definitions.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        definitions
    }

    /// Stub loading mode (v7 §八点七): the provider-visible tools array stays
    /// byte-constant for the whole session. always_loaded tools ship their
    /// full contract; every lazy tool ships a stub — real name, one-line
    /// summary, permissive parameter shell — and the full contract is fetched
    /// on demand through `load_tools`, whose result rides the conversation
    /// tail without touching the cached prefix.
    pub fn stub_definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .map(|tool| {
                if tool.always_loaded {
                    let mut definition = tool.definition();
                    if tool.name == "load_tools" {
                        definition.function.description =
                            super::load_tools::stub_mode_description(self);
                    }
                    definition
                } else {
                    stub_definition(tool)
                }
            })
            .collect::<Vec<_>>();
        definitions.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        definitions
    }

    /// Full contracts (name + complete description + JSON Schema) for the
    /// given tool names; unknown names are silently skipped (the caller
    /// reports them through `skipped`).
    pub(super) fn tool_contracts(&self, names: &[String]) -> Vec<serde_json::Value> {
        let mut seen = BTreeSet::new();
        names
            .iter()
            .filter(|name| seen.insert((*name).clone()))
            .filter_map(|name| self.tools.get(name))
            .map(|tool| {
                let definition = tool.definition();
                serde_json::json!({
                    "name": definition.function.name,
                    "description": definition.function.description,
                    "parameters": definition.function.parameters,
                })
            })
            .collect()
    }

    pub fn requires_lazy_load(&self, name: &str, loaded: &BTreeSet<String>) -> bool {
        self.tools
            .get(name)
            .map(|tool| !tool.always_loaded && !loaded.contains(name))
            .unwrap_or(false)
    }

    pub fn can_auto_load_direct_call(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .map(|tool| tool.load_policy == LoadPolicy::Summary && !tool.always_loaded)
            .unwrap_or(false)
    }

    pub fn definitions_except(&self, excluded: &[&str]) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .filter(|tool| !excluded.iter().any(|name| *name == tool.name))
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();
        // Deterministic order: HashMap iteration order would reshuffle the
        // subagent tools array between calls and defeat provider prefix caches.
        definitions.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        definitions
    }

    pub fn permission(&self, name: &str) -> Result<ToolPermission> {
        let Some(tool) = self.tools.get(name) else {
            bail!("unknown tool: {name}");
        };
        Ok(tool.permission)
    }

    pub async fn call(&self, name: &str, arguments: &str) -> Result<String> {
        let Some(tool) = self.tools.get(name) else {
            return Err(self.unknown_tool_error(name));
        };
        let args = if arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments)?
        };
        if name == "load_tools" {
            return super::load_tools::execute(args, self);
        }
        if let Some(reason) = self.guard_denial(tool, &args, &GuardCtx::default()) {
            bail!("{reason}");
        }
        match self.effective_timeout(tool) {
            Some(limit) => {
                match tokio::time::timeout(limit, tool.call(args, ToolProgress::default())).await {
                    Ok(result) => result,
                    Err(_) => Err(Self::timeout_error(name, limit)),
                }
            }
            None => tool.call(args, ToolProgress::default()).await,
        }
    }

    pub fn call_with_progress_future(
        &self,
        name: &str,
        arguments: &str,
        sender: mpsc::UnboundedSender<ToolProgressEvent>,
        guard_ctx: &GuardCtx,
    ) -> Result<ToolFuture> {
        let Some(tool) = self.tools.get(name) else {
            return Err(self.unknown_tool_error(name));
        };
        let args = if arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments)?
        };
        if name == "load_tools" {
            let result = super::load_tools::execute(args, self);
            return Ok(Box::pin(async move { result }));
        }
        if let Some(reason) = self.guard_denial(tool, &args, guard_ctx) {
            return Ok(Box::pin(async move { Err(anyhow::anyhow!("{reason}")) }));
        }
        let future = tool.call_future(args, ToolProgress::new(sender));
        Ok(match self.effective_timeout(tool) {
            Some(limit) => {
                let tool_name = tool.name.clone();
                Box::pin(async move {
                    match tokio::time::timeout(limit, future).await {
                        Ok(result) => result,
                        Err(_) => Err(Self::timeout_error(&tool_name, limit)),
                    }
                })
            }
            None => future,
        })
    }

    pub fn display_name(&self, name: &str) -> Option<String> {
        self.tools.get(name).and_then(|t| t.display_name.clone())
    }

    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.get(name).map(Arc::as_ref)
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// 拼错工具名时的近似候选:子串命中优先,其余按编辑距离,太远不猜。
    /// 供 unknown-tool 报错引导用(dev 实测:裸报错会让调用方盲试一轮)。
    pub fn suggest_similar(&self, name: &str) -> Vec<String> {
        let needle = name.to_ascii_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut scored = self
            .tools
            .keys()
            .filter_map(|candidate| {
                let hay = candidate.to_ascii_lowercase();
                let score = if hay.contains(&needle) || needle.contains(&hay) {
                    0
                } else {
                    let distance = levenshtein(&needle, &hay);
                    if distance > needle.len().max(3) / 3 + 1 {
                        return None;
                    }
                    distance
                };
                Some((score, candidate))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(3)
            .map(|(_, name)| name.clone())
            .collect()
    }

    pub fn unknown_tool_error(&self, name: &str) -> anyhow::Error {
        let suggestions = self.suggest_similar(name);
        if suggestions.is_empty() {
            anyhow::anyhow!("unknown tool: {name}")
        } else {
            anyhow::anyhow!(
                "unknown tool: {name} (did you mean: {}?)",
                suggestions.join(", ")
            )
        }
    }

    pub(crate) fn loadable_tools(&self, loaded: &BTreeSet<String>) -> Vec<&ToolSpec> {
        let mut tools = self
            .tools
            .values()
            .map(Arc::as_ref)
            .filter(|tool| {
                tool.name != "load_tools" && !tool.always_loaded && !loaded.contains(&tool.name)
            })
            .collect::<Vec<_>>();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    /// Expands requested load targets. Individual problem targets never fail
    /// the whole request: they are reported in the returned `skipped` list so
    /// the valid remainder still loads (a model asking for an always-loaded
    /// tool alongside a group must not lose the group).
    pub(crate) fn expand_load_targets(
        &self,
        requested: &[String],
        loaded: &BTreeSet<String>,
    ) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut loaded_targets = BTreeSet::new();
        let mut loaded_tools = BTreeSet::new();
        let mut skipped = Vec::new();
        for target in requested {
            let target = target.trim();
            if target.is_empty() {
                continue;
            }
            if let Some(group) = target.strip_prefix("group:") {
                let group = group.trim();
                if group.is_empty() {
                    skipped.push("group target is missing a group name".to_string());
                    continue;
                }
                let group_tools = self.group_loadable_tool_names(group, loaded);
                if group_tools.is_empty() {
                    skipped.push(format!("group:{group}: unknown or already fully loaded"));
                    continue;
                }
                loaded_targets.insert(format!("group:{group}"));
                loaded_tools.extend(group_tools);
                continue;
            }

            let Some(tool) = self.tools.get(target) else {
                skipped.push(format!("{target}: unknown tool or script"));
                continue;
            };
            if tool.name == "load_tools" || tool.always_loaded {
                skipped.push(format!(
                    "{target}: already available (always loaded); no need to load it"
                ));
                continue;
            }
            if tool.load_policy == LoadPolicy::Hidden {
                skipped.push(format!("{target}: not loadable via load_tools"));
                continue;
            }
            if loaded.contains(&tool.name) {
                skipped.push(format!("{target}: already loaded"));
            } else {
                loaded_targets.insert(tool.name.clone());
                loaded_tools.insert(tool.name.clone());
            }
        }
        (
            loaded_targets.into_iter().collect(),
            loaded_tools.into_iter().collect(),
            skipped,
        )
    }

    fn group_loadable_tool_names(&self, group: &str, loaded: &BTreeSet<String>) -> Vec<String> {
        let mut names = self
            .tools
            .values()
            .filter(|tool| {
                tool.name != "load_tools"
                    && !tool.always_loaded
                    && !loaded.contains(&tool.name)
                    && tool.load_policy != LoadPolicy::Hidden
                    && tool.groups.iter().any(|candidate| candidate == group)
            })
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    pub(crate) fn load_targets_xml(&self, loaded: &BTreeSet<String>) -> String {
        let loadable = self.loadable_tools(loaded);
        let mut groups: std::collections::BTreeMap<String, Vec<&ToolSpec>> =
            std::collections::BTreeMap::new();
        let mut targets = Vec::new();

        for tool in loadable {
            match tool.load_policy {
                LoadPolicy::Summary => targets.push(load_target_tool_xml(tool)),
                LoadPolicy::Group => {
                    for group in &tool.groups {
                        groups.entry(group.clone()).or_default().push(tool);
                    }
                }
                LoadPolicy::Hidden => {}
            }
        }

        for (group, mut tools) in groups {
            tools.sort_by(|a, b| a.name.cmp(&b.name));
            let members = tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let summary = tool_descriptions::group_summary(&group);
            targets.push(format!(
                "  <target>\n    <name>group:{}</name>\n    <type>group</type>\n    <summary>{}</summary>\n    <tools>{}</tools>\n  </target>",
                xml_escape(&group),
                xml_escape(&summary),
                xml_escape(&members),
            ));
        }

        format!(
            "<available_load_targets>\n{}\n</available_load_targets>",
            targets.join("\n")
        )
    }

    pub(crate) fn unregistered_scripts(&self) -> &[UnregisteredScript] {
        &self.unregistered_scripts
    }

    pub(crate) fn script_summary_xml(&self) -> String {
        let mut scripts = self
            .tools
            .values()
            .filter(|tool| tool.is_script)
            .collect::<Vec<_>>();
        scripts.sort_by(|left, right| left.name.cmp(&right.name));
        let always_loaded = scripts.iter().filter(|tool| tool.always_loaded).count();
        let names = scripts
            .iter()
            .map(|tool| super::load_tools::xml_escape(&tool.name))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "<script_summary>\n  <total>{}</total>\n  <always_loaded>{}</always_loaded>\n  <lazy>{}</lazy>\n  <unregistered>{}</unregistered>\n  <registered_names>{names}</registered_names>\n</script_summary>",
            scripts.len(),
            always_loaded,
            scripts.len() - always_loaded,
            self.unregistered_scripts.len(),
        )
    }

    pub fn clone_filtered(&self, allowed: &[&str]) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.default_timeout = self.default_timeout;
        registry.guards = self.guards.clone();
        for name in allowed {
            if let Some(spec) = self.tools.get(*name) {
                registry.tools.insert(spec.name.clone(), Arc::clone(spec));
            }
        }
        registry
    }
}

pub fn empty_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    })
}

fn load_target_tool_xml(tool: &ToolSpec) -> String {
    let kind = if tool.is_script { "script" } else { "tool" };
    format!(
        "  <target>\n    <name>{}</name>\n    <type>{kind}</type>\n    <summary>{}</summary>\n  </target>",
        xml_escape(&tool.name),
        xml_escape(&load_target_summary(&tool.description)),
    )
}

fn stub_definition(tool: &ToolSpec) -> ToolDefinition {
    let summary = load_target_summary(&tool.description);
    // 后缀只留标记:整套"先 load_tools 取契约再调用"的说明在 load_tools
    // 自己的 description 里统一给一次,N 个 stub 各背一遍纯属重复计费
    // (验收 08-16:每条省 ~55B)。
    let description = if summary.is_empty() {
        "（精简条目：契约经 load_tools 获取）".to_string()
    } else {
        format!("{summary}（精简条目）")
    };
    ToolDefinition {
        kind: "function",
        function: crate::llm::FunctionDefinition {
            name: tool.name.clone(),
            description,
            // Permissive shell: real parameters go at the top level exactly as
            // in a normal call, so execution needs no unwrapping; the actual
            // contract arrives via the load_tools result.
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
        },
    }
}

/// Catalog entries carry a bounded one-line summary instead of the full tool
/// description. This keeps the loader catalog small and byte-stable: without
/// the bound, `load_skill`'s description (which embeds the whole skills
/// catalog) was nested wholesale into the loader XML and re-rendered on every
/// skills rescan.
fn load_target_summary(description: &str) -> String {
    let first_line = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut summary: String = first_line.chars().take(200).collect();
    if first_line.chars().count() > 200 {
        summary.push('…');
    }
    summary
}

/// 经典两行 DP 编辑距离,只服务 suggest_similar 的小字符串,不引依赖。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ch_a) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, ch_b) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ch_a != ch_b);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn sleeping_tool(name: &str, sleep_secs: u64) -> ToolSpec {
        ToolSpec::new(
            name,
            "sleeps",
            json!({"type":"object","properties":{}}),
            move |_| async move {
                tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
                Ok("done".to_string())
            },
        )
    }

    /// 兜底超时:未声明 timeout_seconds 的慢工具被中止,错误走普通
    /// tool error 路径(轮次存活),不会无限挂死回合。
    #[tokio::test]
    async fn default_timeout_aborts_undeclared_slow_tools() {
        let mut registry = ToolRegistry::new();
        registry.set_default_timeout_secs(2);
        registry.register(sleeping_tool("slow_tool", 60));
        let error = registry.call("slow_tool", "{}").await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("timed out") || message.contains("超时"),
            "unexpected timeout message: {message}"
        );
    }

    /// timeout_seconds=0 = 豁免(run_command 这类自管超时的工具),
    /// 兜底不生效。
    #[tokio::test]
    async fn zero_timeout_exempts_self_managed_tools() {
        let mut registry = ToolRegistry::new();
        registry.set_default_timeout_secs(1);
        registry.register(sleeping_tool("self_managed", 2).with_timeout_seconds(0));
        let output = registry.call("self_managed", "{}").await.unwrap();
        assert_eq!(output, "done");
    }

    /// 按工具声明覆盖兜底默认。
    #[tokio::test]
    async fn per_tool_timeout_overrides_default() {
        let mut registry = ToolRegistry::new();
        registry.set_default_timeout_secs(3600);
        registry.register(sleeping_tool("tight_tool", 5).with_timeout_seconds(1));
        let error = registry.call("tight_tool", "{}").await.unwrap_err();
        assert!(error.to_string().contains("1"));
    }

    /// unknown tool 报错带近似建议:拼错给候选,子串命中优先,毫不相干
    /// 不硬猜(dev 实测:裸报错会让桥调用方盲试一轮)。
    #[tokio::test]
    async fn unknown_tool_error_suggests_near_matches() {
        let mut registry = ToolRegistry::new();
        registry.register(sleeping_tool("run_command", 0));
        registry.register(sleeping_tool("web_search", 0));
        registry.register(sleeping_tool("web_fetch", 0));
        let error = registry
            .call("run_comand", "{}")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown tool: run_comand"), "{error}");
        assert!(error.contains("run_command"), "{error}");
        let subs = registry.suggest_similar("web");
        assert!(
            subs.contains(&"web_search".to_string()) && subs.contains(&"web_fetch".to_string()),
            "{subs:?}"
        );
        let error = registry
            .call("totally_unrelated_zzz", "{}")
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(error, "unknown tool: totally_unrelated_zzz");
    }

    /// AUR 互斥迁入 guard 后语义不变:同轮先 review 再 install 被拒,
    /// 拒绝理由与原循环特判逐字一致;无 review 上下文时放行。
    #[tokio::test]
    async fn aur_guard_denies_install_after_review_in_same_turn() {
        let mut registry = ToolRegistry::new();
        registry.register(ToolSpec::new(
            "install_aur_package",
            "installs",
            json!({"type":"object","properties":{}}),
            |_| async { Ok("installed".to_string()) },
        ));
        registry.add_guard(crate::tools::aur_review_install_guard());

        let (sender, _receiver) = mpsc::unbounded_channel();
        let used = vec!["review_aur_package".to_string(), "install_aur_package".to_string()];
        let denied = registry
            .call_with_progress_future(
                "install_aur_package",
                "{}",
                sender.clone(),
                &GuardCtx { used_tools: &used },
            )
            .unwrap()
            .await
            .unwrap_err();
        assert!(denied.to_string().contains("cannot run in the same turn"));

        let clean = vec!["install_aur_package".to_string()];
        let allowed = registry
            .call_with_progress_future(
                "install_aur_package",
                "{}",
                sender,
                &GuardCtx { used_tools: &clean },
            )
            .unwrap()
            .await
            .unwrap();
        assert_eq!(allowed, "installed");
    }

    /// 命令拒绝子串:命中即拒(轮次以 tool error 存活),未命中放行;
    /// 只作用于 run_command。
    #[tokio::test]
    async fn command_deny_guard_blocks_matching_commands() {
        let mut registry = ToolRegistry::new();
        registry.register(ToolSpec::new(
            "run_command",
            "runs",
            json!({"type":"object","properties":{}}),
            |_| async { Ok("ran".to_string()) },
        ));
        registry.add_guard(crate::tools::command_deny_guard(vec![
            "rm -rf /".to_string(),
        ]));

        let denied = registry
            .call("run_command", r#"{"command":"sudo rm -rf /"}"#)
            .await
            .unwrap_err();
        let message = denied.to_string();
        assert!(
            message.contains("rm -rf /") || message.contains("denied pattern"),
            "unexpected denial: {message}"
        );
        let allowed = registry
            .call("run_command", r#"{"command":"ls -la"}"#)
            .await
            .unwrap();
        assert_eq!(allowed, "ran");
    }

    /// clone_filtered 继承兜底超时(task 的 Explore 裁剪路径)。
    #[tokio::test]
    async fn clone_filtered_keeps_default_timeout() {
        let mut registry = ToolRegistry::new();
        registry.set_default_timeout_secs(2);
        registry.register(sleeping_tool("slow_tool", 60));
        let filtered = registry.clone_filtered(&["slow_tool"]);
        assert!(filtered.call("slow_tool", "{}").await.is_err());
    }

    #[test]
    fn lazy_definitions_include_loaded_on_demand_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(ToolSpec::new(
            "read_file",
            "old",
            json!({"type":"object","properties":{}}),
            |_| async { Ok(String::new()) },
        ));
        registry.register(
            ToolSpec::new(
                "custom_lazy_tool",
                "old",
                json!({"type":"object","properties":{}}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false),
        );

        let names = |defs: Vec<ToolDefinition>| {
            defs.into_iter()
                .map(|def| def.function.name)
                .collect::<BTreeSet<_>>()
        };

        assert!(names(registry.lazy_definitions(&BTreeSet::new())).contains("read_file"));
        assert!(!names(registry.lazy_definitions(&BTreeSet::new())).contains("custom_lazy_tool"));

        let loaded = BTreeSet::from(["custom_lazy_tool".to_string()]);
        assert!(names(registry.lazy_definitions(&loaded)).contains("custom_lazy_tool"));
    }

    #[test]
    fn lazy_gate_requires_load_for_on_demand_builtin_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolSpec::new(
                "custom_lazy_tool",
                "old",
                json!({"type":"object","properties":{}}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false),
        );
        assert!(registry.requires_lazy_load("custom_lazy_tool", &BTreeSet::new()));

        let loaded = BTreeSet::from(["custom_lazy_tool".to_string()]);
        assert!(!registry.requires_lazy_load("custom_lazy_tool", &loaded));
    }

    #[test]
    fn cloned_registry_shares_immutable_tool_specs() {
        let mut registry = ToolRegistry::new();
        registry.register(ToolSpec::new(
            "shared_tool",
            "description",
            json!({"type":"object","properties":{}}),
            |_| async { Ok(String::new()) },
        ));

        let cloned = registry.clone();

        assert!(Arc::ptr_eq(
            registry.tools.get("shared_tool").unwrap(),
            cloned.tools.get("shared_tool").unwrap()
        ));
    }

    #[test]
    fn cloned_registry_keeps_snapshot_when_tool_is_replaced() {
        let mut registry = ToolRegistry::new();
        registry.register(ToolSpec::new(
            "replaceable_tool",
            "old description",
            json!({"type":"object","properties":{}}),
            |_| async { Ok(String::new()) },
        ));
        let cloned = registry.clone();

        registry.register(ToolSpec::new(
            "replaceable_tool",
            "new description",
            json!({"type":"object","properties":{}}),
            |_| async { Ok(String::new()) },
        ));

        assert_eq!(
            cloned.get("replaceable_tool").unwrap().description,
            "old description"
        );
        assert_eq!(
            registry.get("replaceable_tool").unwrap().description,
            "new description"
        );
    }

    #[test]
    fn unregister_only_changes_the_current_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(ToolSpec::new(
            "remember_fact",
            "remember",
            json!({"type":"object","properties":{}}),
            |_| async { Ok(String::new()) },
        ));
        let cached = registry.clone();

        assert!(registry.unregister("remember_fact"));
        assert!(registry.get("remember_fact").is_none());
        assert!(cached.get("remember_fact").is_some());
        assert!(!registry.unregister("remember_fact"));
    }
}
