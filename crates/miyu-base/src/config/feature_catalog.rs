//! 引导里「自选功能」那一屏的真相源。终端引导与 WebUI 成员引导共用一份：
//! 哪些插件给开关、哪些永远开着不摆出来、显示名和一句话说明都在这里。
//!
//! 分三档：
//!
//! - **core 与必开项**不出现在引导里：文件读写、看图、搜图、用量、脚本插件
//!   本身、知识库、MCP、记忆、技能。它们是「能用」的底线，关掉只会让人以为坏了。
//! - **可开关的内置插件**（[`TOGGLE_PLUGINS`]）：闹钟、汇率、Arch、API 额度、
//!   表情包、生图、记账——生活助理的配件，不是每个人都要。
//! - **逐个勾的外装件**：每个内置/全局脚本、每个非平台级技能、每台配置里开着的
//!   MCP 服务器（机器级 `mcp.enabled` 关着就整格不摆）；语音只在本机装了
//!   `miyu-voice` 时才给开关。
//!
//! 内置脚本与内置技能对**自定义人格**是可选件：默认不勾（换上自定义人格仍是
//! 纯净状态，09-01），勾了就写进白名单。默认人格（Miyu 本人）默认全勾。
//!
//! 选择最终落到 [`PersonaManifest`]：`plugins.enabled` / `plugins.scripts` /
//! `plugins.skills` / `plugins.mcp` 四个白名单与 `subsystems.*`。全开时白名单写 `None`
//! （= 以后装进来的也自动可见），只有关过东西、或自定义人格勾了内置件才写明细。

use super::persona_manifest::{PersonaManifest, PLUGIN_IDS};

pub use super::builtin_plugins::{plugin_label, TOGGLE_PLUGINS};

/// 引导里不摆开关、永远开着的插件 id。
pub fn always_on_plugins() -> impl Iterator<Item = &'static str> {
    PLUGIN_IDS
        .iter()
        .copied()
        .filter(|id| !TOGGLE_PLUGINS.contains(id))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureKind {
    Subsystem,
    Plugin,
    Script,
    Skill,
    /// 配置里的一台 MCP 服务器(`mcp.servers[].id`),写回 `plugins.mcp`。
    Mcp,
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
    /// 机器侧 `prompt.persona_reminder` 开着才摆开关(人格只能在装了的里挑)。
    pub persona_reminder_available: bool,
    /// 情绪与好感度只在通讯平台层生效,QQ 没开就不摆。
    pub emotion_available: bool,
    pub scripts: Vec<(String, String, String, bool)>,
    pub skills: Vec<(String, String, bool)>,
    /// MCP 服务器 (id, 显示名, 一句话说明):调用方只列机器级 `mcp.enabled` 开着、
    /// 且 `servers[].enabled` 的;机器级关着就传空,整格不摆、清单里的白名单也不动。
    pub mcp_servers: Vec<(String, String, String)>,
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

/// 按人格清单当前的状态摆出整张表。顺序：语音 → 内置插件 → 脚本 → 技能 → MCP 服务器。
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
    if sources.persona_reminder_available {
        items.push(FeatureItem {
            kind: FeatureKind::Subsystem,
            id: "persona_reminder".into(),
            name: "人格提醒".into(),
            hint: "隔几轮提醒模型保持人设".into(),
            on: manifest.subsystems.persona_reminder,
            builtin: false,
        });
    }
    if sources.emotion_available {
        items.push(FeatureItem {
            kind: FeatureKind::Subsystem,
            id: "emotion".into(),
            name: "情绪与好感度".into(),
            hint: "通讯平台里的情绪状态与好感度".into(),
            on: manifest.subsystems.emotion,
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
    // MCP 服务器没有「内置件」一说:None = 全连,写了名单就只连名单上的
    // (与 `tools/mcp.rs::register` 同一判据)。
    for (id, name, hint) in &sources.mcp_servers {
        items.push(FeatureItem {
            kind: FeatureKind::Mcp,
            id: id.clone(),
            name: if name.trim().is_empty() {
                id.clone()
            } else {
                name.clone()
            },
            hint: hint.clone(),
            on: extension_on(&manifest.plugins.mcp, id, false, default_persona),
            builtin: false,
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
        if item.kind == FeatureKind::Subsystem {
            match item.id.as_str() {
                "voice" => manifest.subsystems.voice = item.on,
                "persona_reminder" => manifest.subsystems.persona_reminder = item.on,
                "emotion" => manifest.subsystems.emotion = item.on,
                _ => {}
            }
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
    // 表上没有 MCP 一格(机器级关着)不等于用户决定全连:手写的白名单原样保留。
    if items.iter().any(|item| item.kind == FeatureKind::Mcp) {
        manifest.plugins.mcp = allowlist(items, FeatureKind::Mcp, default_persona);
    }
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
            persona_reminder_available: true,
            emotion_available: true,
            scripts: vec![
                ("s1".into(), "脚本一".into(), String::new(), false),
                ("b1".into(), "内置一".into(), String::new(), true),
            ],
            skills: vec![
                ("k1".into(), "技能一".into(), false),
                ("bk".into(), "内置技能".into(), true),
            ],
            mcp_servers: vec![
                ("m1".into(), "服务器一".into(), "npx server-one".into()),
                ("m2".into(), String::new(), "uvx server-two".into()),
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

    /// MCP 逐服务器勾选:None 全勾;关一台就写明细;显示名空的用 id;机器级关着整格不摆、
    /// 手写的白名单原样保留。
    #[test]
    fn mcp_servers_get_per_server_toggles_that_write_the_allowlist() {
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources(), true);
        let mcp: Vec<(&str, &str, bool)> = items
            .iter()
            .filter(|item| item.kind == FeatureKind::Mcp)
            .map(|item| (item.id.as_str(), item.name.as_str(), item.on))
            .collect();
        assert_eq!(mcp, [("m1", "服务器一", true), ("m2", "m2", true)]);
        assert_eq!(
            items.last().unwrap().kind,
            FeatureKind::Mcp,
            "MCP 排在最后一格"
        );

        items.iter_mut().find(|item| item.id == "m1").unwrap().on = false;
        apply_selection(&mut manifest, &items, true);
        assert_eq!(manifest.plugins.mcp, Some(vec!["m2".to_string()]));
        assert_eq!(manifest.plugins.scripts, None, "别的白名单不受影响");
        assert_eq!(catalog(&manifest, &sources(), true), items);

        // 自定义人格同一套判据(MCP 没有内置件):None 照样全勾。
        let custom = catalog(&PersonaManifest::all(), &sources(), false);
        assert!(custom
            .iter()
            .filter(|item| item.kind == FeatureKind::Mcp)
            .all(|item| item.on));

        // 机器级 MCP 关着:不摆,也不碰手写的名单。
        let mut hidden = sources();
        hidden.mcp_servers.clear();
        let items = catalog(&manifest, &hidden, true);
        assert!(items.iter().all(|item| item.kind != FeatureKind::Mcp));
        apply_selection(&mut manifest, &items, true);
        assert_eq!(manifest.plugins.mcp, Some(vec!["m2".to_string()]));
    }

    /// 人格提醒与情绪两个开关从此有 UI 入口:摆表能看见、关掉能写回清单、机器没装就不摆。
    #[test]
    fn persona_reminder_and_emotion_toggles_round_trip() {
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources(), true);
        let ids: Vec<&str> = items
            .iter()
            .filter(|item| item.kind == FeatureKind::Subsystem)
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["voice", "persona_reminder", "emotion"]);
        for item in &mut items {
            if item.id == "persona_reminder" || item.id == "emotion" {
                item.on = false;
            }
        }
        apply_selection(&mut manifest, &items, true);
        assert!(!manifest.subsystems.persona_reminder && !manifest.subsystems.emotion);
        assert!(manifest.subsystems.voice, "没动的开关不受影响");
        assert_eq!(catalog(&manifest, &sources(), true), items);

        let mut hidden = sources();
        hidden.persona_reminder_available = false;
        hidden.emotion_available = false;
        let items = catalog(&manifest, &hidden, true);
        assert!(items
            .iter()
            .all(|item| item.kind != FeatureKind::Subsystem || item.id == "voice"));
    }
}
