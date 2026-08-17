mod alarm;
mod api_quota;
mod apply_patch;
mod archlinux;
mod artifact;
mod ask_question;
mod calculator;
mod caniplayonlinux_query;
mod clipboard;
mod deep_research;
mod deepseek_status;
mod default_tools;
pub(crate) use default_tools::TOOL_SUMMARY_PREFIX;
mod diagnostics;
mod edit_replace;
mod exchange_rate;
mod fcitx_wiki;
mod hash_codec;
mod html_conversion;
mod http_response;
mod image_generation;
pub mod jobs;
pub(crate) mod kitty_image;
pub mod knowledge_base;
mod load_tools;
mod man;
mod mcp;
pub(crate) mod memes;
mod memory;
mod moegirl;
mod package_advisor;
mod patch_preview;
mod protondb_query;
mod registry;
pub(crate) mod repeat_reminder;
mod scripts;
mod skills;
mod subagent_runner;
mod task;
mod todowrite;
pub(crate) mod usage_query;
pub mod tool_descriptions;
pub mod vision;
mod weather;
mod web;
mod web_images;
pub mod workspace;
mod write;
mod xuanxue;

use crate::config::AppConfig;
use crate::i18n::{is_zh, text as t};
use crate::paths::MiyuPaths;
use std::collections::HashMap;
use std::sync::RwLock;

#[allow(unused_imports)]
pub use registry::{
    empty_parameters, CommandOutputStream, GuardCtx, ToolFuture, ToolGuard, ToolPermission,
    ToolProgress, ToolProgressEvent, ToolRegistry, ToolSpec,
};
pub(crate) use scripts::rescan_scripts;
pub(crate) use skills::{apply_skill_refresh, prepare_skill_refresh};
pub use skills::{register_authoring as register_skill_authoring, register_skills};

pub fn register_ask_question(registry: &mut ToolRegistry) {
    ask_question::register(registry);
}

static SCRIPT_DISPLAY_NAMES: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

pub fn register_script_display_names(registry: &ToolRegistry) {
    let mut map = HashMap::new();
    for name in registry.tool_names() {
        if let Some(dn) = registry.display_name(&name) {
            map.insert(name, dn);
        }
    }
    *SCRIPT_DISPLAY_NAMES.write().unwrap() = Some(map);
}

pub fn readable_tool_name(name: &str) -> String {
    if let Some(skill) = name.strip_prefix("load_skill:") {
        return if is_zh() {
            format!("加载技能：{skill}")
        } else {
            format!("Load skill: {skill}")
        };
    }
    if let Some(tools) = name.strip_prefix("load_tools:") {
        let targets = tools
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        let display = targets
            .iter()
            .map(|name| readable_load_target_name(name))
            .collect::<Vec<_>>()
            .join(if is_zh() { "、" } else { ", " });
        return if is_zh() {
            format!("加载：{display}")
        } else {
            format!("Load: {display}")
        };
    }
    if let Some(display_name) = builtin_readable_tool_name(name) {
        return display_name.to_string();
    }
    if let Ok(guard) = SCRIPT_DISPLAY_NAMES.read() {
        if let Some(map) = guard.as_ref() {
            if let Some(dn) = map.get(name) {
                return dn.clone();
            }
        }
    }
    name.to_string()
}

fn readable_load_target_name(name: &str) -> String {
    if let Some(group) = name.strip_prefix("group:") {
        return builtin_readable_group_name(group)
            .map(str::to_string)
            .unwrap_or_else(|| format!("group:{group}"));
    }
    readable_tool_name(name)
}

/// Phase text for the "still receiving arguments" hint, or `None` for tools
/// that stream too fast to be worth one.
///
/// The tool name is decoded from the stream well before its arguments finish,
/// so this is what keeps a multi-kilobyte patch or file write from looking
/// frozen. Deliberately a short list: flashing a hint for a `read_file` whose
/// arguments arrive in one chunk is noise. `task` is absent because subagents
/// already get their own timed block.
pub fn preparing_phase(name: &str) -> Option<&'static str> {
    Some(match name {
        "apply_patch"
        | "apply_artifact_patch"
        | "create_artifact"
        | "write_file"
        | "edit_file"
        | "edit_string" => t("Preparing edit", "准备编辑"),
        "run_command" => t("Preparing command", "准备执行"),
        // 批量删的参数是一整串路径,条数一多就是几百字节,正好落在
        // 「工具名已解码、参数还在流」的那个窗口里。
        "trash_path" => t("Preparing delete", "准备删除"),
        // A subagent brief is long, and its own timed block only appears once
        // the arguments have all arrived.
        "task" | "deep_research" => t("Preparing task", "准备任务"),
        "ask_question" => t("Preparing question", "准备问题"),
        _ => return None,
    })
}

fn builtin_readable_tool_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "run_command" => t("Run command", "运行命令"),
        "job_status" => t("Check background tasks", "查询后台任务"),
        "job_stop" => t("Stop background task", "停止后台任务"),
        "apply_patch" => t("Edit files", "编辑文件"),
        "apply_artifact_patch" => t("Edit preview file", "修改预览文件"),
        "create_artifact" => t("Create preview file", "创建预览文件"),
        "read_artifact" => t("Read preview file", "读取预览文件"),
        "present_artifact" => t("Preview file", "预览文件"),
        "ask_question" => t("Ask user", "询问用户"),
        "task" => t("Subagent", "子代理"),
        "read_file" => t("Read file", "读取文件"),
        "write_file" => t("Write file", "写入文件"),
        "edit_file" => t("Edit file", "编辑文件"),
        "edit_string" => t("Edit string", "字符串编辑"),
        "list_directory" => t("List directory", "列目录"),
        "create_directory" => t("Create directory", "创建目录"),
        "trash_path" => t("Move to trash", "移入回收站"),
        "glob" => t("Find files", "查找文件"),
        "grep" => t("Search text", "搜索文本"),
        "get_current_directory" => t("Current directory", "当前目录"),
        "get_current_time" => t("Current time", "当前时间"),
        "check_issue" => t("Check issue", "检查问题"),
        "check_os_info" => t("System information", "查看系统信息"),
        "read_clipboard" => t("Read clipboard", "读取剪贴板"),
        "web_search" => t("Web search", "网络搜索"),
        "web_fetch" => t("Fetch webpage", "读取网页"),
        "fcitx5_input_method_wiki_qurey" => t("Query Fcitx5 Wiki", "查询 Fcitx5 Wiki"),
        "search_web_images" => t("Search images", "搜索图片"),
        "analyze_image" | "vision_analyze" => t("Analyze image", "分析图片"),
        "print_image" => t("Display image", "显示图片"),
        "generate_image" => t("Generate image", "生成图片"),
        "search_meme" => t("Search memes", "搜索表情包"),
        "show_meme" => t("Send meme", "发送表情"),
        "add_meme" => t("Add meme", "添加表情包"),
        "update_meme" => t("Update meme", "更新表情包"),
        "delete_meme" => t("Delete meme", "删除表情包"),
        "deep_research" => t("Deep research", "深度研究"),
        "upload_knowledge_base_file" | "upload_text_to_knowledge_base" => {
            t("Import knowledge base", "导入知识库")
        }
        "read_knowledge_base_file" => t("Read knowledge base", "读取知识库"),
        "search_knowledge_base" => t("Search knowledge base", "搜索知识库"),
        "search_knowledge_base_by_name" => t("Search knowledge base by name", "按名称搜索知识库"),
        "edit_knowledge_base_file" => t("Edit knowledge base", "编辑知识库"),
        "remove_knowledge_base_file" => t("Remove from knowledge base", "移除知识库"),
        "list_knowledge_base_files" => t("List knowledge base", "列出知识库"),
        "set_alarm" => t("Set alarm", "设置闹钟"),
        "list_alarms" => t("List alarms", "列出闹钟"),
        "cancel_alarm" => t("Cancel alarm", "取消闹钟"),
        "remember_fact" => t("Remember fact", "记录记忆"),
        "search_evicted_context" => t("Search old context", "搜索旧上下文"),
        "recall_past_events" => t("Recall past events", "回忆往事"),
        "recall_memory" | "recall_memories" => t("Recall memories", "召回记忆"),
        "forget_memory" | "forget_memories" => t("Forget memories", "删除记忆"),
        "list_memory" | "list_memories" => t("List memories", "列出记忆"),
        "aur_search_packages" => t("Search AUR", "搜索 AUR"),
        "aur_get_package_info" => t("View AUR package", "查看 AUR 包"),
        "aur_check_status" => t("Check AUR status", "查询 AUR 状态"),
        "archlinux_official_package_query" => t("Query Arch package", "查询 Arch 官方包"),
        "query_deepseek_status" => t("Check DeepSeek status", "查询 DeepSeek 状态"),
        "query_api_quota" => t("Query API quota", "查询大模型 API 额度"),
        "pacman_search" => t("Search packages", "搜索软件包"),
        "archwiki_query" => t("Query ArchWiki", "查询 ArchWiki"),
        "archlinux_news" => t("Arch news", "Arch 新闻"),
        "online_man_search" | "man_search" => t("Search online manuals", "搜索在线手册"),
        "online_man_get_page" | "man_read" => t("Read online manual", "读取在线手册"),
        "moegirl_query" | "query_moegirl" => t("Query Moegirlpedia", "查询萌娘百科"),
        "calculate" | "calculator" | "scientific_calculator" => {
            t("Scientific calculation", "科学计算")
        }
        "calculate_hash" => t("Calculate hash", "计算哈希"),
        "decode_encoded_text" => t("Decode text", "解码文本"),
        "exchange_rate" | "get_exchange_rate" => t("Exchange rates", "汇率查询"),
        "weather" | "get_weather" => t("Weather", "天气查询"),
        "query_caniplayonlinux" => t("Check Linux compatibility", "查询是否能在Linux上玩"),
        "protondb_query" => t("Query ProtonDB", "查询 ProtonDB"),
        "xuanxue_pick" => t("Divination choice", "玄学选择"),
        "xuanxue_divine" => t("Divination", "玄学占卜"),
        "draw_zhouyi_hexagram" => t("Draw I Ching hexagram", "周易起卦"),
        "draw_tarot_card" => t("Draw tarot card", "抽塔罗牌"),
        "draw_fortune_lot" => t("Draw fortune", "吉凶占"),
        "roll_dice" => t("Roll dice", "掷骰子"),
        "load_skill" => t("Load skill", "加载技能"),
        "create_skill" => t("Create skill draft", "创建技能草稿"),
        "update_skill" => t("Update skill draft", "更新技能草稿"),
        "delete_skill" => t("Delete skill", "删除技能"),
        "publish_skill" => t("Publish skill", "发布技能"),
        "list_skill_drafts" => t("Skill drafts", "技能草稿"),
        "load_tools" => t("Load", "加载"),
        "register_script" => t("Register script", "注册脚本"),
        "unregister_script" => t("Unregister script", "注销脚本"),
        "todowrite" => t("Todo list", "任务列表"),
        "todoupdate" => t("Update todos", "更新任务"),
        "review_aur_package" => t("Review AUR package", "审查 AUR 包"),
        "install_aur_package" => t("Install AUR package", "安装 AUR 包"),
        "review_pkgbuild_directory" => t("Review PKGBUILD directory", "审查 PKGBUILD 目录"),
        "register_deep_research_topic_title" => t("Register research title", "注册研究标题"),
        "register_deep_research_reference" => t("Register reference", "注册引用来源"),
        "remove_deep_research_reference" => t("Remove reference", "移除引用来源"),
        _ => return None,
    })
}

fn builtin_readable_group_name(group: &str) -> Option<&'static str> {
    Some(match group {
        "acg" => t("ACG tools", "ACG 工具组"),
        "agent" => t("Subagent tools", "子代理工具组"),
        "alarms" => t("Alarm tools", "闹钟工具组"),
        "arch" => t("Arch / AUR tools", "Arch / AUR 工具组"),
        "dev" => t("Development tools", "开发修改工具组"),
        "dev-read" => t("Code search tools", "代码检索工具组"),
        "diagnostics" => t("Diagnostic tools", "诊断工具组"),
        "divination" => t("Divination tools", "玄学工具组"),
        "gaming" => t("Gaming tools", "游戏工具组"),
        "images" => t("Image tools", "图片工具组"),
        "knowledge" => t("Knowledge base tools", "知识库工具组"),
        "knowledge-admin" => t("Knowledge base management", "知识库管理工具组"),
        "linux-docs" => t("Linux documentation", "Linux 文档工具组"),
        "memory" => t("Memory tools", "记忆工具组"),
        "memes" => t("Meme tools", "表情包工具组"),
        "planning" => t("Planning tools", "任务规划工具组"),
        "research" => t("Research tools", "深度研究工具组"),
        "scripts" => t("Script tools", "脚本工具组"),
        "scripting" => t("Script management", "脚本管理工具组"),
        "shell" => t("Shell tools", "Shell 工具组"),
        "shopping" => t("Shopping tools", "购物工具组"),
        "skills" => t("Skill tools", "技能工具组"),
        "systeminfo" => t("System information", "系统信息工具组"),
        "utility" => t("Utility tools", "实用工具组"),
        "web" => t("Web tools", "联网工具组"),
        _ => return None,
    })
}

pub fn clear_aur_review_state(paths: &MiyuPaths) -> anyhow::Result<()> {
    package_advisor::clear_aur_review_state(paths)
}

/// AUR 装包互斥:review 与 install 不同轮,逼一次"给用户看过再装"的确认。
/// 原为 chat_with_tools 循环里的硬编码特判,迁入 guard 层后对所有分发路径
/// (主循环/子代理/工具桥)一致生效。
pub(crate) fn aur_review_install_guard() -> ToolGuard {
    std::sync::Arc::new(|tool, _args, ctx| {
        (tool.name == "install_aur_package"
            && ctx.used_tools.iter().any(|name| name == "review_aur_package"))
        .then(|| {
            "install_aur_package cannot run in the same turn as review_aur_package; \
             ask the user to confirm installation first"
                .to_string()
        })
    })
}

/// run_command 命令拒绝子串(config.tools.command_deny)。命中即拒,
/// 防提示注入与模型手滑;拒绝以 tool error 回给模型,轮次存活。
pub(crate) fn command_deny_guard(patterns: Vec<String>) -> ToolGuard {
    std::sync::Arc::new(move |tool, args, _ctx| {
        if tool.name != "run_command" {
            return None;
        }
        let command = args.get("command").and_then(serde_json::Value::as_str)?;
        patterns
            .iter()
            .find(|pattern| !pattern.is_empty() && command.contains(pattern.as_str()))
            .map(|pattern| {
                crate::i18n::agent_is_zh()
                    .then(|| format!("命令包含被禁止的模式 `{pattern}`,已拒绝执行"))
                    .unwrap_or_else(|| {
                        format!("command contains the denied pattern `{pattern}` and was rejected")
                    })
            })
    })
}

fn install_builtin_guards(registry: &mut ToolRegistry, config: &AppConfig) {
    registry.add_guard(aur_review_install_guard());
    registry.add_guard(command_deny_guard(config.tools.command_deny.clone()));
}

pub fn builtin_registry(config: &AppConfig, paths: &MiyuPaths) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.set_default_timeout_secs(config.tools.default_timeout_secs);
    install_builtin_guards(&mut registry, config);
    default_tools::register(&mut registry, config.skills.allow_command_execution);
    jobs::register_management(&mut registry);
    usage_query::register(&mut registry, paths.state_dir.join("usage-history.jsonl"), config.clone());
    // 编辑器只留 apply_patch(聚合增/改/删,diff 渲染载体);write_file/
    // edit_file/edit_string 与 dev 同步退场(验收四轮:normal 也去冗余)。
    apply_patch::register(&mut registry);
    todowrite::register(&mut registry, paths.clone());
    alarm::register(&mut registry, paths.clone());
    clipboard::register(&mut registry, paths.clone());
    web::register_fetch(&mut registry);
    fcitx_wiki::register(&mut registry);
    weather::register(&mut registry);
    caniplayonlinux_query::register(&mut registry);
    protondb_query::register(&mut registry);
    exchange_rate::register(&mut registry, config.plugins.exchange_rate.clone());
    xuanxue::register(&mut registry);
    if config.plugins.archlinux.enabled {
        archlinux::register(&mut registry, paths);
    }
    if config.plugins.man.enabled {
        man::register(&mut registry);
    }
    moegirl::register(&mut registry);
    hash_codec::register(&mut registry);
    calculator::register(&mut registry);
    deepseek_status::register(&mut registry);
    if config.plugins.api_quota.enabled {
        api_quota::register(
            &mut registry,
            config.plugins.api_quota.clone(),
            config.providers.clone(),
        );
    }
    vision::register_print(&mut registry, config.clone());
    if config.plugins.memes.enabled {
        memes::register(&mut registry, config.clone(), paths.clone());
    }
    if config.plugins.web.enabled {
        web::register(&mut registry, config.plugins.web.clone());
    }
    if config.plugins.web_images.enabled {
        web_images::register(&mut registry, config.clone(), paths.clone(), true);
    }
    if config.plugins.deep_research.enabled {
        let research_tools = registry.clone();
        deep_research::register(&mut registry, config.clone(), paths.clone(), research_tools);
    }
    if config.plugins.vision.enabled {
        vision::register(&mut registry, config.clone(), paths.clone(), true);
    }
    if config.plugins.image_generation.enabled {
        image_generation::register(&mut registry, config.clone());
    }
    if config.plugins.knowledge_base.enabled {
        knowledge_base::register(&mut registry, config.clone(), paths.clone());
    }
    if config.plugins.package_advisor.enabled {
        package_advisor::register(&mut registry, paths.clone());
    }
    if config.plugins.diagnostics.enabled {
        diagnostics::register(&mut registry, config.clone());
    }
    if config.memory_config().enabled {
        memory::register(&mut registry, config.clone(), paths.clone());
    }
    let task_tools = registry.clone();
    task::register(&mut registry, config.clone(), paths.clone(), task_tools);
    scripts::register(&mut registry, paths);
    if config.mcp.enabled {
        mcp::register(&mut registry, config.clone());
    }
    if uses_load_tools(&config.tools.loading_mode) {
        load_tools::register(&mut registry);
    }
    registry
}

pub fn register_webui_artifact_tools(
    registry: &mut ToolRegistry,
    paths: &MiyuPaths,
    session_id: &str,
) {
    artifact::register_webui(registry, paths, session_id);
}

pub fn webui_artifact_manifest(paths: &MiyuPaths, session_id: &str) -> anyhow::Result<String> {
    artifact::managed_manifest(paths, session_id)
}

pub(crate) fn rescope_platform_memory_tools(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    context: &crate::platforms::PlatformTurnContext,
    readonly: bool,
) {
    if !config.tools.enabled || !config.memory_config().enabled {
        return;
    }
    for name in [
        "remember_fact",
        "search_evicted_context",
        "recall_past_events",
        "recall_memories",
    ] {
        registry.unregister(name);
    }
    let principal = context.principal().stable_key();
    let access = if context.is_admin {
        crate::memory::MemoryAccess::Privileged
    } else {
        crate::memory::MemoryAccess::principal(principal.clone())
    };
    if readonly {
        memory::register_readonly_with_context(
            registry,
            config.clone(),
            paths.clone(),
            access,
            Some(principal),
            context.sender_display_name.clone(),
        );
    } else {
        memory::register_with_context(
            registry,
            config.clone(),
            paths.clone(),
            access,
            Some(principal),
            context.sender_display_name.clone(),
        );
    }
}

pub fn is_hybrid_loading_mode(mode: &str) -> bool {
    matches!(mode.trim(), "hybrid" | "lazy")
}

/// Stub loading mode (v7 §八点七): every lazy tool stays registered as a
/// permanently visible stub (real name + one-line summary + permissive
/// parameter shell), so the provider-visible tools array is byte-constant for
/// the whole session; full contracts are fetched on demand through
/// `load_tools` as a tool result that rides the conversation tail.
pub fn is_stub_loading_mode(mode: &str) -> bool {
    mode.trim() == "stub"
}

/// Modes that need the `load_tools` catalog tool registered.
pub fn uses_load_tools(mode: &str) -> bool {
    is_hybrid_loading_mode(mode) || is_stub_loading_mode(mode)
}

/// Build/Dev 模式工具目录:极简开发形态,"模型可见表面积最小化"。
/// 只注册:run_command+后台任务管理、apply_patch(唯一编辑器,聚合
/// 增/改/删文件,存在意义=diff 渲染)、todo、goal、task 子代理、web
/// 检索与 MCP。读检索(read_file/glob/grep)、check_os_info、
/// trash_path、知识库、多余编辑器都不注册——bash+coreutils 干得更好
/// (验收三轮裁剪)。人格/娱乐/平台类一概不存在。记忆工具**注册**,
/// 但作用域切到保留人格 "dev" 的独立命名空间(`dev_scoped`)。
/// ask_question 等界面胶水与 normal 同路,由 daemon/CLI 按 surface 追加。
pub fn dev_registry(config: &AppConfig, paths: &MiyuPaths) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.set_default_timeout_secs(config.tools.default_timeout_secs);
    install_builtin_guards(&mut registry, config);
    // 验收三轮裁剪定稿:凡 coreutils 干得更好的都不注册——读检索
    // (read_file/glob/grep)、check_os_info、trash_path、知识库全砍;
    // 编辑只留 apply_patch(Add/Update/Delete File 全聚合,留它是为了
    // diff 渲染),write_file/edit_file/edit_string 一并退场。
    default_tools::register_run_command(&mut registry, config.skills.allow_command_execution);
    jobs::register_management(&mut registry);
    apply_patch::register(&mut registry);
    todowrite::register(&mut registry, paths.clone());
    web::register_fetch(&mut registry);
    if config.plugins.web.enabled {
        web::register(&mut registry, config.plugins.web.clone());
    }
    if config.plugins.vision.enabled {
        // 看图对 coding 是刚需(UI 截图排错、设计稿、测试产出的图表);
        // 聊天模型不带眼睛时由 vision 插件路由给专用视觉模型。
        vision::register(&mut registry, config.clone(), paths.clone(), true);
    }
    if config.memory_config().enabled {
        // dev 独立记忆:同一套工具,库切到保留人格 "dev" 的命名空间,
        // 与默认人格的记忆互不可见(验收问题一:开发模式也要有记忆)。
        memory::register(&mut registry, config.dev_scoped(), paths.clone());
    }
    let task_tools = registry.clone();
    task::register(&mut registry, config.clone(), paths.clone(), task_tools);
    if config.mcp.enabled {
        mcp::register(&mut registry, config.clone());
    }
    if uses_load_tools(&config.tools.loading_mode) {
        load_tools::register(&mut registry);
    }
    registry
}

/// Tools exposed to an untrusted messaging-platform conversation. This list
/// deliberately excludes shell, filesystem, local-image inspection, memory,
/// knowledge-base, MCP, scripts, and tools that persist arbitrary downloads.
/// `generate_image` is the one Writes exception: it only saves its own API
/// output under the plugin's output directory, never an arbitrary host path.
pub fn restricted_platform_registry(config: &AppConfig, paths: &MiyuPaths) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.set_default_timeout_secs(config.tools.default_timeout_secs);
    web::register_fetch(&mut registry);
    weather::register(&mut registry);
    caniplayonlinux_query::register(&mut registry);
    protondb_query::register(&mut registry);
    exchange_rate::register(&mut registry, config.plugins.exchange_rate.clone());
    xuanxue::register(&mut registry);
    moegirl::register(&mut registry);
    hash_codec::register(&mut registry);
    calculator::register(&mut registry);
    deepseek_status::register(&mut registry);
    if config.plugins.web.enabled {
        web::register(&mut registry, config.plugins.web.clone());
    }
    if config.plugins.memes.enabled {
        memes::register_chat(&mut registry, config.clone(), paths.clone());
    }
    if config.plugins.image_generation.enabled {
        image_generation::register(&mut registry, config.clone());
    }
    if config.skills.enabled {
        if let Err(error) = skills::register_skills(&mut registry, config, paths) {
            tracing::warn!(error = %error, "failed to register skills for restricted platform registry");
        }
    }
    if uses_load_tools(&config.tools.loading_mode) {
        load_tools::register(&mut registry);
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归:dev 模式要有看图(vision_analyze),且随 vision 插件开关走。
    #[test]
    fn dev_registry_vision_follows_plugin_switch() {
        let paths = crate::paths::MiyuPaths::new().unwrap();
        let mut config = crate::config::AppConfig::default();
        let names = |registry: &ToolRegistry| -> Vec<String> {
            registry
                .definitions()
                .iter()
                .map(|d| d.function.name.clone())
                .collect()
        };
        assert!(names(&dev_registry(&config, &paths)).contains(&"vision_analyze".to_string()));
        config.plugins.vision.enabled = false;
        assert!(!names(&dev_registry(&config, &paths)).contains(&"vision_analyze".to_string()));
    }

    fn test_paths(root: &std::path::Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("config/fish/conf.d/miyu.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: root.join("system-scripts"),
        }
    }

    #[test]
    fn preparing_phase_covers_the_slow_argument_tools_only() {
        for name in [
            "apply_patch",
            "apply_artifact_patch",
            "create_artifact",
            "write_file",
            "edit_file",
            "edit_string",
        ] {
            assert_eq!(
                preparing_phase(name),
                Some(crate::i18n::text("Preparing edit", "准备编辑")),
                "{name}"
            );
        }
        assert_eq!(
            preparing_phase("run_command"),
            Some(crate::i18n::text("Preparing command", "准备执行"))
        );
        assert_eq!(
            preparing_phase("trash_path"),
            Some(crate::i18n::text("Preparing delete", "准备删除"))
        );
        for name in ["task", "deep_research"] {
            assert_eq!(
                preparing_phase(name),
                Some(crate::i18n::text("Preparing task", "准备任务")),
                "{name}"
            );
        }
        assert_eq!(
            preparing_phase("ask_question"),
            Some(crate::i18n::text("Preparing question", "准备问题"))
        );
        // Arguments arrive in one chunk: a hint would only flicker.
        for name in ["read_file", "grep", "list_directory"] {
            assert_eq!(preparing_phase(name), None, "{name}");
        }
    }

    #[test]
    fn readable_names_cover_all_built_in_tools_and_groups() {
        let mut missing_tools = tool_descriptions::all()
            .keys()
            .filter(|name| builtin_readable_tool_name(name).is_none())
            .cloned()
            .collect::<Vec<_>>();
        missing_tools.sort();
        assert!(
            missing_tools.is_empty(),
            "missing tool names: {missing_tools:?}"
        );

        let mut missing_groups = tool_descriptions::group_names()
            .into_iter()
            .filter(|group| builtin_readable_group_name(group).is_none())
            .collect::<Vec<_>>();
        missing_groups.sort();
        assert!(
            missing_groups.is_empty(),
            "missing tool group names: {missing_groups:?}"
        );
    }

    #[test]
    fn ui_language_does_not_change_agent_tool_definitions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut english = AppConfig::default();
        english.display.language = "en".to_string();
        let mut chinese = english.clone();
        chinese.display.language = "zh".to_string();

        let english =
            serde_json::to_value(builtin_registry(&english, &paths).definitions()).unwrap();
        let chinese =
            serde_json::to_value(builtin_registry(&chinese, &paths).definitions()).unwrap();

        assert_eq!(english, chinese);
    }

    #[test]
    fn restricted_platform_registry_has_no_host_or_write_tools() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let registry = restricted_platform_registry(&AppConfig::default(), &paths);
        let names = registry.tool_names();

        for forbidden in [
            "run_command",
            "read_file",
            "write_file",
            "apply_patch",
            "vision_analyze",
            "task",
        ] {
            assert!(!names.iter().any(|name| name == forbidden), "{forbidden}");
        }
        for name in names {
            // generate_image is the deliberate Writes exception: it only
            // saves its own API output under the plugin's output directory.
            if name == "generate_image" {
                continue;
            }
            assert_eq!(
                registry.permission(&name).unwrap(),
                ToolPermission::ReadOnly
            );
        }
        assert!(registry.contains("load_skill"));
        assert!(registry.contains("load_tools"));
        // With the plugin enabled, image generation is exposed to platforms.
        let mut config = AppConfig::default();
        config.plugins.image_generation.enabled = true;
        let registry = restricted_platform_registry(&config, &paths);
        assert!(registry.contains("generate_image"));
        let visible = registry.lazy_definitions(&Default::default());
        assert!(visible
            .iter()
            .any(|definition| definition.function.name == "load_tools"));
        assert!(!visible
            .iter()
            .any(|definition| definition.function.name == "draw_zhouyi_hexagram"));
    }

    #[test]
    fn skill_authoring_tools_are_normal_mode_only() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let normal = crate::cli::build_tool_registry(
            &config,
            &paths,
            crate::agent::AgentMode::Normal,
            false,
        )
        .unwrap();
        let dev =
            crate::cli::build_tool_registry(&config, &paths, crate::agent::AgentMode::Dev, false)
                .unwrap();

        for name in [
            "create_skill",
            "update_skill",
            "delete_skill",
            "publish_skill",
            "list_skill_drafts",
        ] {
            assert!(normal.contains(name), "{name}");
            assert!(!dev.contains(name), "{name}");
        }
        assert!(normal.contains("load_skill"));
        assert!(dev.contains("load_skill"));
    }

    #[test]
    fn artifact_tools_are_only_added_by_the_webui_registration_step() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let mut registry = builtin_registry(&config, &paths);
        assert!(!registry.contains("create_artifact"));
        assert!(!registry.contains("present_artifact"));

        register_webui_artifact_tools(&mut registry, &paths, "sess_webui");
        for name in [
            "create_artifact",
            "read_artifact",
            "apply_artifact_patch",
            "present_artifact",
        ] {
            assert_eq!(
                registry.permission(name).unwrap(),
                ToolPermission::Presentation,
                "{name}"
            );
        }
        let definitions = registry.definitions();
        assert!(definitions
            .iter()
            .any(|definition| definition.function.name == "create_artifact"));
        assert!(definitions
            .iter()
            .any(|definition| definition.function.name == "present_artifact"));
    }

    #[tokio::test]
    async fn restricted_platform_can_load_the_divination_group() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let registry = restricted_platform_registry(&AppConfig::default(), &paths);

        let output = registry
            .call("load_tools", r#"{"names":["group:divination"]}"#)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        let loaded = value["loaded_tools"].as_array().unwrap();
        assert!(loaded.iter().any(|name| name == "draw_zhouyi_hexagram"));
        assert!(loaded.iter().any(|name| name == "draw_tarot_card"));
        assert!(loaded.iter().any(|name| name == "draw_fortune_lot"));
    }
}

#[cfg(test)]
mod tier_schema_probe {
    /// Regression: the built-in description overlay
    /// (`descriptions/task.json`) wholesale replaces the task schema at
    /// register time — a param added only in code silently vanishes from
    /// what the LLM sees.
    #[test]
    fn task_definition_includes_tier() {
        let config = crate::config::AppConfig::default();
        let paths = crate::paths::MiyuPaths::new().unwrap();
        let registry = super::builtin_registry(&config, &paths);
        let defs = registry.definitions();
        let task = defs
            .iter()
            .find(|d| d.function.name == "task")
            .expect("task registered");
        let props = task.function.parameters.get("properties").unwrap();
        assert!(
            props.get("tier").is_some(),
            "tier missing: {}",
            task.function.parameters
        );
        // 全未配置=零追加(08-16 tools 瘦身:三行"未配置"是零信息,还把
        // 动态文本焊进 tools 数组);配置了档位才出现状态。
        assert!(!task.function.description.contains("cheap=["));
    }

    /// The description suffix lists the concrete models per tier pool.
    #[test]
    fn task_description_lists_configured_tier_models() {
        let mut config = crate::config::AppConfig::default();
        let provider_id = config.providers[0].id.clone();
        config.providers[0].models.push("mini-a".to_string());
        config.providers[0].models.push("mini-b".to_string());
        config
            .toggle_subagent_tier_model(crate::config::ModelTier::Cheap, &provider_id, "mini-a")
            .unwrap();
        config
            .toggle_subagent_tier_model(crate::config::ModelTier::Balanced, &provider_id, "mini-a")
            .unwrap();
        config
            .toggle_subagent_tier_model(crate::config::ModelTier::Balanced, &provider_id, "mini-b")
            .unwrap();
        let paths = crate::paths::MiyuPaths::new().unwrap();
        let registry = super::builtin_registry(&config, &paths);
        let defs = registry.definitions();
        let task = defs.iter().find(|d| d.function.name == "task").unwrap();
        assert!(task.function.description.contains("cheap=[mini-a]"));
        assert!(task
            .function
            .description
            .contains("balanced=[mini-a, mini-b]"));
        assert!(task.function.description.contains("strong=["));
    }
}
