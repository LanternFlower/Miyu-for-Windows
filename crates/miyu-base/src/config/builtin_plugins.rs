//! 内置插件登记表(09-16,core/normal 接口治理 Phase 4)。
//!
//! 以前新增或移除一个内置插件要在四处登记:`PLUGIN_IDS`、`feature_catalog` 的
//! `TOGGLE_PLUGINS` 与 `plugin_label`、`tools::compose_core` 里的那个 `if`——漏一处
//! 编译照过,人格清单校验放行,工具面上却没有它。现在一行写完 id / 种类 / 中文名 /
//! 提示 / 可勾选 / 机器开关:[`PLUGIN_IDS`]、[`TOGGLE_PLUGINS`]、[`plugin_label`] 全部
//! 从这张表派生。
//!
//! 注册函数住在 `tools::builtin_plugins`(config 是底座,不能反向依赖 tools),两张表
//! 按 id 对齐,`tools` 侧的测试钉住「每个 Builtin 都有注册函数、没有多余的注册函数」。
//! 新增一个内置插件 = 实现文件 + 描述 JSON + `include_str!` 一行 + 这里一行 + 那边一行。

use super::AppConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    /// core 件的开关(`files`):关掉只留 `run_command`,由 compose 显式处理。
    Core,
    /// 内置 Rust 插件:往工具面加东西,按 `tools::builtin_plugins` 的注册表挂。
    Builtin,
    /// 外装件的接入机制(scripts / mcp):本身按各自规范再筛,由 compose 显式处理。
    Provider,
}

pub struct BuiltinPluginDescriptor {
    /// persona.toml 里的名字,也是 `manifest.plugin_enabled(id)` 的键。
    pub id: &'static str,
    pub kind: PluginKind,
    pub name_zh: &'static str,
    pub hint_zh: &'static str,
    /// 引导 / 成员人格页给不给开关;常开件不摆出来。
    pub toggleable: bool,
    /// 机器级开关:本机装了 / 开了没有。人格只能在装了的里挑;运行态条件
    /// (如 QQ 是否连着)在注册函数里再判。
    pub installed: fn(&AppConfig) -> bool,
}

fn always(_: &AppConfig) -> bool {
    true
}
fn exchange_rate_installed(config: &AppConfig) -> bool {
    config.plugins.exchange_rate.enabled
}
fn archlinux_installed(config: &AppConfig) -> bool {
    config.plugins.archlinux.enabled
}
fn api_quota_installed(config: &AppConfig) -> bool {
    config.plugins.api_quota.enabled
}
fn memes_installed(config: &AppConfig) -> bool {
    config.plugins.memes.enabled
}
/// 连接状态不在这里:那是运行态,`tools::platform_outreach::qq_connected` 现判。
fn platform_outreach_installed(config: &AppConfig) -> bool {
    config.platforms.terminal_outreach && config.platforms.qq.enabled
}
fn web_images_installed(config: &AppConfig) -> bool {
    config.plugins.web_images.enabled
}
fn image_generation_installed(config: &AppConfig) -> bool {
    config.plugins.image_generation.enabled
}
fn knowledge_base_installed(config: &AppConfig) -> bool {
    config.plugins.knowledge_base.enabled
}
fn mcp_installed(config: &AppConfig) -> bool {
    config.mcp.enabled
}

/// 顺序即 [`PLUGIN_IDS`] 的顺序(校验报错时照这个列)。
pub const BUILTIN_PLUGINS: &[BuiltinPluginDescriptor] = &[
    BuiltinPluginDescriptor {
        id: "files",
        kind: PluginKind::Core,
        name_zh: "文件",
        hint_zh: "读写工作区文件",
        toggleable: false,
        installed: always,
    },
    BuiltinPluginDescriptor {
        id: "usage_query",
        kind: PluginKind::Builtin,
        name_zh: "用量查询",
        hint_zh: "对话里问用了多少 token",
        toggleable: false,
        installed: always,
    },
    BuiltinPluginDescriptor {
        id: "alarm",
        kind: PluginKind::Builtin,
        name_zh: "闹钟",
        hint_zh: "定时提醒",
        toggleable: true,
        installed: always,
    },
    BuiltinPluginDescriptor {
        id: "exchange_rate",
        kind: PluginKind::Builtin,
        name_zh: "汇率",
        hint_zh: "货币换算",
        toggleable: true,
        installed: exchange_rate_installed,
    },
    BuiltinPluginDescriptor {
        id: "archlinux",
        kind: PluginKind::Builtin,
        name_zh: "Arch Linux",
        hint_zh: "AUR 查询与审查安装、Arch 新闻",
        toggleable: true,
        installed: archlinux_installed,
    },
    BuiltinPluginDescriptor {
        id: "api_quota",
        kind: PluginKind::Builtin,
        name_zh: "API 额度",
        hint_zh: "查供应商余额",
        toggleable: true,
        installed: api_quota_installed,
    },
    BuiltinPluginDescriptor {
        id: "print_image",
        kind: PluginKind::Builtin,
        name_zh: "视觉分析",
        hint_zh: "看图片和截图",
        toggleable: false,
        installed: always,
    },
    BuiltinPluginDescriptor {
        id: "memes",
        kind: PluginKind::Builtin,
        name_zh: "表情包",
        hint_zh: "用表情包回复",
        toggleable: true,
        installed: memes_installed,
    },
    BuiltinPluginDescriptor {
        id: "platform_outreach",
        kind: PluginKind::Builtin,
        name_zh: "外发",
        hint_zh: "从对话里给通讯平台发消息",
        toggleable: false,
        installed: platform_outreach_installed,
    },
    BuiltinPluginDescriptor {
        id: "web_images",
        kind: PluginKind::Builtin,
        name_zh: "搜图",
        hint_zh: "网络找图",
        toggleable: false,
        installed: web_images_installed,
    },
    BuiltinPluginDescriptor {
        id: "image_generation",
        kind: PluginKind::Builtin,
        name_zh: "生图",
        hint_zh: "AI 画图",
        toggleable: true,
        installed: image_generation_installed,
    },
    BuiltinPluginDescriptor {
        id: "knowledge_base",
        kind: PluginKind::Builtin,
        name_zh: "知识库",
        hint_zh: "自己的资料库,对话里能查",
        toggleable: false,
        installed: knowledge_base_installed,
    },
    BuiltinPluginDescriptor {
        id: "ledger",
        kind: PluginKind::Builtin,
        name_zh: "记账",
        hint_zh: "记账本",
        toggleable: true,
        installed: always,
    },
    BuiltinPluginDescriptor {
        id: "scripts",
        kind: PluginKind::Provider,
        name_zh: "脚本工具",
        hint_zh: "逐个勾选",
        toggleable: false,
        installed: always,
    },
    // MCP 与脚本同级:插件闸之上还能按服务器 id 逐个勾(`plugins.mcp`)。
    BuiltinPluginDescriptor {
        id: "mcp",
        kind: PluginKind::Provider,
        name_zh: "MCP",
        hint_zh: "外接 MCP 服务器的工具",
        toggleable: false,
        installed: mcp_installed,
    },
];

const fn plugin_ids() -> [&'static str; BUILTIN_PLUGINS.len()] {
    let mut ids = [""; BUILTIN_PLUGINS.len()];
    let mut index = 0;
    while index < BUILTIN_PLUGINS.len() {
        ids[index] = BUILTIN_PLUGINS[index].id;
        index += 1;
    }
    ids
}

/// 插件 id:与 `tools::compose_registry` 里的注册单元一一对应,persona.toml 里
/// 名字的真相源,拼错的名字在 `PersonaManifest::validate` 里能被指出来。
pub const PLUGIN_IDS: &[&str] = &plugin_ids();

const fn toggle_count() -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < BUILTIN_PLUGINS.len() {
        if BUILTIN_PLUGINS[index].toggleable {
            count += 1;
        }
        index += 1;
    }
    count
}

const fn toggle_plugin_ids() -> [&'static str; toggle_count()] {
    let mut ids = [""; toggle_count()];
    let mut filled = 0;
    let mut index = 0;
    while index < BUILTIN_PLUGINS.len() {
        if BUILTIN_PLUGINS[index].toggleable {
            ids[filled] = BUILTIN_PLUGINS[index].id;
            filled += 1;
        }
        index += 1;
    }
    ids
}

/// 引导里给开关的内置插件。其余 [`PLUGIN_IDS`] 一律常开、不摆出来。
pub const TOGGLE_PLUGINS: &[&str] = &toggle_plugin_ids();

pub fn descriptor(id: &str) -> Option<&'static BuiltinPluginDescriptor> {
    BUILTIN_PLUGINS.iter().find(|plugin| plugin.id == id)
}

/// 插件 id → (显示名, 一句话说明)。WebUI 与终端引导共用;不认识的 id 给空串。
pub fn plugin_label(id: &str) -> (&'static str, &'static str) {
    descriptor(id).map_or(("", ""), |plugin| (plugin.name_zh, plugin.hint_zh))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 派生出来的名单必须与 09-16 之前手写的三张表逐字相同——这是把手写表
    /// 换成派生表的等价证明。
    #[test]
    fn derived_lists_match_the_former_hand_written_tables() {
        assert_eq!(
            PLUGIN_IDS,
            [
                "files",
                "usage_query",
                "alarm",
                "exchange_rate",
                "archlinux",
                "api_quota",
                "print_image",
                "memes",
                "platform_outreach",
                "web_images",
                "image_generation",
                "knowledge_base",
                "ledger",
                "scripts",
                "mcp",
            ]
        );
        assert_eq!(
            TOGGLE_PLUGINS,
            [
                "alarm",
                "exchange_rate",
                "archlinux",
                "api_quota",
                "memes",
                "image_generation",
                "ledger",
            ]
        );
        assert_eq!(plugin_label("ledger"), ("记账", "记账本"));
        assert_eq!(plugin_label("mcp"), ("MCP", "外接 MCP 服务器的工具"));
        assert_eq!(plugin_label("nope"), ("", ""));
    }

    #[test]
    fn ids_are_unique_and_every_row_has_a_label() {
        let mut seen = std::collections::BTreeSet::new();
        for plugin in BUILTIN_PLUGINS {
            assert!(seen.insert(plugin.id), "{} 登记了两次", plugin.id);
            assert!(!plugin.name_zh.is_empty(), "{} 没有中文名", plugin.id);
            assert!(!plugin.hint_zh.is_empty(), "{} 没有一句话说明", plugin.id);
        }
    }

    /// 机器开关按配置现算:关掉插件配置,`installed` 立刻为假;常开件恒真。
    #[test]
    fn installed_follows_the_machine_config() {
        let mut config = AppConfig::default();
        config.plugins.exchange_rate.enabled = false;
        config.mcp.enabled = false;
        assert!(!(descriptor("exchange_rate").unwrap().installed)(&config));
        assert!(!(descriptor("mcp").unwrap().installed)(&config));
        assert!((descriptor("alarm").unwrap().installed)(&config));
        assert!((descriptor("files").unwrap().installed)(&config));
    }
}
