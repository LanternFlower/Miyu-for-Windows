/// Regression: the built-in description overlay
/// (`descriptions/subagent.json`) wholesale replaces the subagent schema at
/// register time — a param added only in code silently vanishes from
/// what the LLM sees.
#[test]
fn subagent_definition_includes_tier() {
    let config = miyu_base::config::AppConfig::default();
    let paths = miyu_base::paths::MiyuPaths::new().unwrap();
    let registry = super::builtin_registry(&config, &paths);
    let defs = registry.definitions();
    let subagent = defs
        .iter()
        .find(|d| d.function.name == "subagent")
        .expect("subagent registered");
    let props = subagent.function.parameters.get("properties").unwrap();
    assert!(
        props.get("tier").is_some(),
        "tier missing: {}",
        subagent.function.parameters
    );
    // 全未配置=零追加(08-16 tools 瘦身:三行"未配置"是零信息,还把
    // 动态文本焊进 tools 数组);配置了档位才出现状态。
    assert!(!subagent.function.description.contains("cheap=["));
}

/// The description is constant bytes: configuring tier pools must not
/// change it (a config-derived suffix would re-key the prompt cache on
/// every pool edit), and the tier enum carries the four current names.
#[test]
fn subagent_description_is_constant_and_lists_the_four_tiers() {
    let paths = miyu_base::paths::MiyuPaths::new().unwrap();
    let bare = miyu_base::config::AppConfig::default();
    let bare_subagent = super::builtin_registry(&bare, &paths)
        .definitions()
        .into_iter()
        .find(|d| d.function.name == "subagent")
        .unwrap();

    let mut config = miyu_base::config::AppConfig::default();
    let provider_id = config.active_provider.clone();
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
        .unwrap();
    provider.models.push("mini-a".to_string());
    config
        .toggle_tier_model(miyu_base::config::ModelTier::Cheap, &provider_id, "mini-a")
        .unwrap();
    let subagent = super::builtin_registry(&config, &paths)
        .definitions()
        .into_iter()
        .find(|d| d.function.name == "subagent")
        .unwrap();
    assert_eq!(
        subagent.function.description,
        bare_subagent.function.description
    );
    assert!(!subagent.function.description.contains("cheap=["));
    let schema = serde_json::to_string(&subagent.function.parameters).unwrap();
    for tier in ["lite", "cheap", "standard", "flagship"] {
        assert!(schema.contains(&format!("\"{tier}\"")), "{schema}");
    }
    assert!(
        !schema.contains("balanced") && !schema.contains("strong"),
        "{schema}"
    );
}

/// 量尺：`cargo test --lib token_diet_baseline -- --ignored --nocapture`
///
/// token 瘦身专项的基线：三套 registry 在 stub（默认发送形态）与 full
/// （懒加载展开上限）两种形态下，发给 LLM 的 tools 数组的真实 o200k
/// token 数，附逐工具排行。默认 AppConfig，不含平台插件回合注册的工具。
#[test]
#[ignore]
fn token_diet_baseline_probe() {
    use crate::tools::tests::test_paths;
    use crate::tools::{builtin_registry, dev_registry, restricted_platform_registry, AppConfig};
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    for (label, registry) in [
        ("normal", builtin_registry(&config, &paths)),
        ("dev", dev_registry(&config, &paths)),
        ("restricted", restricted_platform_registry(&config, &paths)),
    ] {
        for (variant, defs) in [
            ("stub", registry.stub_definitions()),
            ("full", registry.definitions()),
        ] {
            let whole = serde_json::to_string(&defs).unwrap();
            let tokens = miyu_base::token_counter::count(&whole);
            eprintln!(
                "[{label}/{variant}] tools={} bytes={} tokens={}",
                defs.len(),
                whole.len(),
                tokens
            );
            let mut rows: Vec<(String, usize, usize)> = defs
                .iter()
                .map(|d| {
                    let s = serde_json::to_string(d).unwrap();
                    (
                        d.function.name.clone(),
                        s.len(),
                        miyu_base::token_counter::count(&s),
                    )
                })
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.2));
            for (name, bytes, toks) in rows {
                eprintln!("  {toks:>6} tok {bytes:>6} B  {name}");
            }
        }
    }
}
