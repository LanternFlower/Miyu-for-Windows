//! 内置脚本头部 = 迁移前 index.json 的契约(09-05 迁移)。
//!
//! 内置脚本的描述/参数/超时/分组从 index.json 搬进了各脚本头部,index.json 删除。
//! 夹具是迁移前那份 index,这里逐条比对:参数 schema(去掉 description 文案后)
//! 逐字节相同,超时、分组、显示名不变;描述换成英文后首句 ≤60 字符。

use crate::tools::scripts::*;

const LEGACY_INDEX: &str = include_str!(
    "../../../../../../src/tools/scripts/tests/fixtures/bundled-index-2026-09-05.json"
);

fn bundled_dir() -> PathBuf {
    Path::new(miyu_base::WORKSPACE_ROOT).join("src/scripts/personas/default")
}

fn strip_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("description");
            for nested in map.values_mut() {
                strip_descriptions(nested);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_descriptions),
        _ => {}
    }
}

fn first_sentence_chars(text: &str) -> usize {
    text.split_inclusive(['.', '!', '?'])
        .next()
        .unwrap_or(text)
        .trim()
        .chars()
        .count()
}

#[test]
fn bundled_headers_match_the_legacy_index_contracts() {
    let dir = bundled_dir();
    let scan = scan_scripts(&[dir.as_path()]).unwrap();
    assert!(scan.unregistered.is_empty(), "{:?}", scan.unregistered);
    assert!(
        !dir.join("index.json").exists(),
        "内置目录不该再有 index.json"
    );

    let legacy: ScriptIndex = serde_json::from_str(LEGACY_INDEX).unwrap();
    assert_eq!(legacy.scripts.len(), 8);
    for old in legacy.scripts {
        let new = scan
            .entries
            .iter()
            .find(|entry| entry.id == old.id)
            .unwrap_or_else(|| panic!("{} missing from header scan", old.id));
        let mut old_params = old.parameters.clone();
        strip_descriptions(&mut old_params);
        let mut new_params = new.parameters.clone();
        strip_descriptions(&mut new_params);
        assert_eq!(old_params, new_params, "{}: parameters drifted", old.id);
        assert_eq!(old.timeout_seconds, new.timeout_seconds, "{}", old.id);
        assert_eq!(old.groups, new.groups, "{}", old.id);
        assert!(matches!(new.load_policy, LoadPolicy::Group), "{}", old.id);
        assert_eq!(new.always_loaded, None, "{}", old.id);
        // 迁移前的 index 只有中文名,所以比中文槽而不是 `entry.display_name`:
        // 后者按运行时 locale 选,测试在英文 locale 下跑会挑出英文名,拿它跟
        // 中文契约比是假挂。
        let zh_name = metadata_from_script(Path::new(&new.path)).display_names.zh;
        assert_eq!(
            zh_name.as_deref(),
            Some(old.display_name.as_str()),
            "{}",
            old.id
        );
    }
}

/// 出厂脚本必须有中文显示名;英文名可选。显示名是给人看的,而工具 id 本来就是
/// 英文,英文界面按 id 兜一个就够(`xhs_search` → `Xhs search`),不值得再维护一份
/// 英文文案。中文名反过来是必填的:没有它,中文界面只能端出 id。
/// 这条闸顺带挡住把中文写进 `Display name:` 的老毛病——两份文档的示例曾经
/// 就是 `# Display name: 番组日历`,照抄的人把中文塞进英文槽,中文界面靠 en→zh
/// 回退才显示对,英文界面反倒露出中文。
#[test]
fn bundled_scripts_carry_a_chinese_display_name() {
    let scan = scan_scripts(&[bundled_dir().as_path()]).unwrap();
    assert!(!scan.entries.is_empty());
    for entry in &scan.entries {
        let names = metadata_from_script(Path::new(&entry.path)).display_names;
        let english = names.en.unwrap_or_default();
        let chinese = names.zh.unwrap_or_default();
        assert!(
            !chinese.is_empty(),
            "{}: 缺中文显示名,给头部加一行 `# 显示名称：...`",
            entry.id
        );
        assert!(
            english.is_ascii(),
            "{}: `Display name:` 是英文槽,中文名要写在 `显示名称:` 上: {english}",
            entry.id
        );
        assert!(
            chinese
                .chars()
                .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character)),
            "{}: `显示名称:` 该写中文: {chinese}",
            entry.id
        );
    }
}

#[test]
fn bundled_descriptions_follow_the_header_style_rules() {
    let scan = scan_scripts(&[bundled_dir().as_path()]).unwrap();
    let ids: Vec<&str> = scan.entries.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "bangumi",
            "battery_care",
            "bilibili_live_stream",
            "codec",
            "crack_search",
            "divine",
            "flight_deals",
            "game_compat",
            "get_weather",
            "goofish_search",
            "hotel_deals",
            "online_man",
            "query_deepseek_status",
            "query_moegirl",
            "read_clipboard",
            "reddit_search",
            "scientific_calculator",
            "xhs_search",
            "zhihu_search",
        ]
    );
    for entry in &scan.entries {
        assert!(
            entry
                .description
                .starts_with(|character: char| character.is_ascii_alphabetic()),
            "{}: description must be English: {}",
            entry.id,
            entry.description
        );
        assert!(
            first_sentence_chars(&entry.description) <= 60,
            "{}: first sentence over 60 chars: {}",
            entry.id,
            entry.description
        );
        if let Some(properties) = entry
            .parameters
            .get("properties")
            .and_then(Value::as_object)
        {
            for (name, property) in properties {
                // 没有说明是允许的,而且往往是对的:参数名加 enum 已经说清的
                // (`format: md|json`)再写一句是每回合常驻的纯开销(09-21 瘦身)。
                // 这条规则管的是文风——写了就必须是英文(AGENTS §1.5)。
                let Some(description) = property.get("description").and_then(Value::as_str) else {
                    continue;
                };
                assert!(
                    description.starts_with(|character: char| character.is_ascii_alphabetic()),
                    "{}.{name}: parameter description must be English: {description}",
                    entry.id
                );
            }
        }
    }
}
