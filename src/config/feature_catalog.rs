//! 引导里「自选功能」那一屏的真相源。终端引导与 WebUI 成员引导共用一份：
//! 哪些插件给开关、哪些永远开着不摆出来、显示名和一句话说明都在这里。
//!
//! 分三档：
//!
//! - **core 与必开项**不出现在引导里：文件读写、看图、搜图、用量、脚本插件
//!   本身、知识库、MCP、记忆、技能。它们是「能用」的底线，关掉只会让人以为坏了。
//! - **可开关的内置插件**（[`TOGGLE_PLUGINS`]）：闹钟、汇率、Arch、API 额度、
//!   表情包、生图、记账——生活助理的配件，不是每个人都要。
//! - **逐个勾的外装件**：每个内置/全局脚本、每个非平台级技能；语音只在本机
//!   装了 `miyu-voice` 时才给开关。
//!
//! 内置脚本与内置技能对**自定义人格**是可选件：默认不勾（换上自定义人格仍是
//! 纯净状态，09-01），勾了就写进白名单。默认人格（Miyu 本人）默认全勾。
//!
//! 选择最终落到 [`PersonaManifest`]：`plugins.enabled` / `plugins.scripts` /
//! `plugins.skills` 三个白名单与 `subsystems.voice`。全开时白名单写 `None`
//! （= 以后装进来的也自动可见），只有关过东西、或自定义人格勾了内置件才写明细。

use super::persona_manifest::{PersonaManifest, PLUGIN_IDS};

/// 引导里给开关的内置插件。其余 `PLUGIN_IDS` 一律常开、不摆出来。
pub const TOGGLE_PLUGINS: &[&str] = &[
    "alarm",
    "exchange_rate",
    "archlinux",
    "api_quota",
    "memes",
    "image_generation",
    "ledger",
];

/// 引导里不摆开关、永远开着的插件 id。
pub fn always_on_plugins() -> impl Iterator<Item = &'static str> {
    PLUGIN_IDS
        .iter()
        .copied()
        .filter(|id| !TOGGLE_PLUGINS.contains(id))
}

/// 插件 id → (显示名, 一句话说明)。WebUI 与终端引导共用。
pub fn plugin_label(id: &str) -> (&'static str, &'static str) {
    match id {
        "files" => ("文件", "读写工作区文件"),
        "usage_query" => ("用量查询", "对话里问用了多少 token"),
        "alarm" => ("闹钟", "定时提醒"),
        "exchange_rate" => ("汇率", "货币换算"),
        "archlinux" => ("Arch Linux", "AUR 查询与审查安装、Arch 新闻"),
        "api_quota" => ("API 额度", "查供应商余额"),
        "print_image" => ("视觉分析", "看图片和截图"),
        "memes" => ("表情包", "用表情包回复"),
        "platform_outreach" => ("外发", "从对话里给通讯平台发消息"),
        "web_images" => ("搜图", "网络找图"),
        "image_generation" => ("生图", "AI 画图"),
        "knowledge_base" => ("知识库", "自己的资料库,对话里能查"),
        "ledger" => ("记账", "记账本"),
        "scripts" => ("脚本工具", "逐个勾选"),
        "mcp" => ("MCP", "外接 MCP 服务器的工具"),
        _ => ("", ""),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureKind {
    Subsystem,
    Plugin,
    Script,
    Skill,
}

/// 引导表里的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureItem {
    pub kind: FeatureKind,
    pub id: String,
    pub name: String,
    pub hint: String,
    pub on: bool,
    /// 内置件（Miyu 出厂脚本/技能）。自定义人格下默认不勾，勾了要写进白名单。
    pub builtin: bool,
}

/// 调用方探到的外装件：脚本 (id, 显示名, 描述, 是否内置)、技能 (名字, 描述, 是否内置)、
/// 语音装没装。配置层不扫目录也不探二进制，谁调谁给。
#[derive(Clone, Debug, Default)]
pub struct FeatureSources {
    pub voice_available: bool,
    pub scripts: Vec<(String, String, String, bool)>,
    pub skills: Vec<(String, String, bool)>,
}

/// 白名单里点没点名。
fn listed(list: &Option<Vec<String>>, id: &str) -> bool {
    list.as_ref()
        .is_some_and(|list| list.iter().any(|item| item == id))
}

/// 一件外装件此刻开没开：内置件在自定义人格下只看白名单点名，其余 None = 全开。
fn extension_on(
    list: &Option<Vec<String>>,
    id: &str,
    builtin: bool,
    default_persona: bool,
) -> bool {
    if builtin && !default_persona {
        listed(list, id)
    } else {
        list.is_none() || listed(list, id)
    }
}

/// 按人格清单当前的状态摆出整张表。顺序：语音 → 内置插件 → 脚本 → 技能。
pub fn catalog(
    manifest: &PersonaManifest,
    sources: &FeatureSources,
    default_persona: bool,
) -> Vec<FeatureItem> {
    let mut items = Vec::new();
    if sources.voice_available {
        items.push(FeatureItem {
            kind: FeatureKind::Subsystem,
            id: "voice".into(),
            name: "语音".into(),
            hint: "唤醒对话、听写、朗读".into(),
            on: manifest.subsystems.voice,
            builtin: false,
        });
    }
    for id in TOGGLE_PLUGINS {
        let (name, hint) = plugin_label(id);
        items.push(FeatureItem {
            kind: FeatureKind::Plugin,
            id: (*id).into(),
            name: name.into(),
            hint: hint.into(),
            on: manifest.plugin_enabled(id),
            builtin: false,
        });
    }
    for (id, name, hint, builtin) in &sources.scripts {
        items.push(FeatureItem {
            kind: FeatureKind::Script,
            id: id.clone(),
            name: if name.trim().is_empty() {
                id.clone()
            } else {
                name.clone()
            },
            hint: hint.clone(),
            on: extension_on(&manifest.plugins.scripts, id, *builtin, default_persona),
            builtin: *builtin,
        });
    }
    for (name, hint, builtin) in &sources.skills {
        items.push(FeatureItem {
            kind: FeatureKind::Skill,
            id: name.clone(),
            name: name.clone(),
            hint: hint.clone(),
            on: extension_on(&manifest.plugins.skills, name, *builtin, default_persona),
            builtin: *builtin,
        });
    }
    items
}

/// 把表上的勾选写回清单。全开 = 白名单留空（`None`），关过才写明细；自定义人格
/// 勾了内置件也得写明细（None 对它意味着「内置一件不挂」）。
///
/// 表里没出现的内置插件（core 与必开项）一律算开——它们本来就不给关。
pub fn apply_selection(
    manifest: &mut PersonaManifest,
    items: &[FeatureItem],
    default_persona: bool,
) {
    for item in items {
        if item.kind == FeatureKind::Subsystem && item.id == "voice" {
            manifest.subsystems.voice = item.on;
        }
    }
    let plugins_off = items
        .iter()
        .any(|item| item.kind == FeatureKind::Plugin && !item.on);
    manifest.plugins.enabled = plugins_off.then(|| {
        PLUGIN_IDS
            .iter()
            .copied()
            .filter(|id| {
                items
                    .iter()
                    .find(|item| item.kind == FeatureKind::Plugin && item.id == *id)
                    .is_none_or(|item| item.on)
            })
            .map(str::to_string)
            .collect()
    });
    manifest.plugins.scripts = allowlist(items, FeatureKind::Script, default_persona);
    manifest.plugins.skills = allowlist(items, FeatureKind::Skill, default_persona);
}

fn allowlist(
    items: &[FeatureItem],
    kind: FeatureKind,
    default_persona: bool,
) -> Option<Vec<String>> {
    let listed: Vec<&FeatureItem> = items.iter().filter(|item| item.kind == kind).collect();
    if listed.is_empty() {
        return None;
    }
    // 默认人格:全开才留空。自定义人格:目录里的全开**且**内置一件没勾才留空
    // (None 对它意味着「内置一件不挂」,勾了内置件就必须写明细)。
    let all_on = listed.iter().all(|item| item.on);
    let regular_all_on = listed
        .iter()
        .filter(|item| !item.builtin)
        .all(|item| item.on);
    let builtin_any_on = listed.iter().any(|item| item.builtin && item.on);
    let keep_none = if default_persona {
        all_on
    } else {
        regular_all_on && !builtin_any_on
    };
    if keep_none {
        return None;
    }
    Some(
        listed
            .iter()
            .filter(|item| item.on)
            .map(|item| item.id.clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> FeatureSources {
        FeatureSources {
            voice_available: true,
            scripts: vec![
                ("s1".into(), "脚本一".into(), String::new(), false),
                ("b1".into(), "内置一".into(), String::new(), true),
            ],
            skills: vec![
                ("k1".into(), "技能一".into(), false),
                ("bk".into(), "内置技能".into(), true),
            ],
        }
    }

    #[test]
    fn toggle_plugins_are_all_known_ids() {
        for id in TOGGLE_PLUGINS {
            assert!(PLUGIN_IDS.contains(id), "{id} is not a plugin id");
            assert!(!plugin_label(id).0.is_empty(), "{id} has no label");
        }
        let always: Vec<&str> = always_on_plugins().collect();
        assert_eq!(always.len() + TOGGLE_PLUGINS.len(), PLUGIN_IDS.len());
        assert!(always.contains(&"mcp"));
        assert!(always.contains(&"knowledge_base"));
    }

    #[test]
    fn default_persona_all_on_leaves_allowlists_empty() {
        let mut manifest = PersonaManifest::all();
        let items = catalog(&manifest, &sources(), true);
        assert!(items.iter().all(|item| item.on));
        apply_selection(&mut manifest, &items, true);
        assert_eq!(manifest, PersonaManifest::all());
    }

    #[test]
    fn default_persona_turning_things_off_writes_explicit_lists() {
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources(), true);
        for item in &mut items {
            if ["voice", "memes", "b1", "k1"].contains(&item.id.as_str()) {
                item.on = false;
            }
        }
        apply_selection(&mut manifest, &items, true);
        assert!(!manifest.subsystems.voice);
        let enabled = manifest.plugins.enabled.clone().unwrap();
        assert!(!enabled.contains(&"memes".to_string()));
        assert!(enabled.contains(&"files".to_string()));
        assert!(enabled.contains(&"mcp".to_string()));
        assert_eq!(manifest.plugins.scripts, Some(vec!["s1".to_string()]));
        assert_eq!(manifest.plugins.skills, Some(vec!["bk".to_string()]));
        // 再摆一遍表,勾选状态回得来。
        assert_eq!(catalog(&manifest, &sources(), true), items);
    }

    #[test]
    fn custom_persona_builtins_default_off_and_opt_in() {
        let mut manifest = PersonaManifest::all();
        let items = catalog(&manifest, &sources(), false);
        let by_id = |id: &str| items.iter().find(|item| item.id == id).unwrap().on;
        assert!(by_id("s1") && !by_id("b1") && by_id("k1") && !by_id("bk"));
        // 什么都不动:清单不落盘,内置照样不挂。
        apply_selection(&mut manifest, &items, false);
        assert_eq!(manifest.plugins.scripts, None);
        assert_eq!(manifest.plugins.skills, None);
        // 勾一个内置脚本:必须写明细,且把目录里的也一起点名。
        let mut items = items;
        items.iter_mut().find(|item| item.id == "b1").unwrap().on = true;
        apply_selection(&mut manifest, &items, false);
        assert_eq!(
            manifest.plugins.scripts,
            Some(vec!["s1".to_string(), "b1".to_string()])
        );
        assert_eq!(manifest.plugins.skills, None);
        assert_eq!(catalog(&manifest, &sources(), false), items);
    }
}
