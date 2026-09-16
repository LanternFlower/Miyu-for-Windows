//! `AppConfig` 深拷贝成本的量尺（默认 `#[ignore]`，手动跑）。

use super::*;
use std::time::Instant;

/// 量尺：`cargo test --lib config::scaling_probe -- --ignored --nocapture`
///
/// `handle_message_with_activity` 第一件事就是深拷贝整份 AppConfig，
/// 在 `qq.enabled` 检查之前、在准入判定之前——每条被丢弃的群消息也照付。
/// 这里量的是「一条消息的入场费」。
#[test]
#[ignore]
fn app_config_clone_cost() {
    // 照着实际配置的规模造:22 个供应商、每家 3 个模型、默认敏感词表
    let mut config = AppConfig::default();
    let template = config.providers[0].clone();
    config.providers.clear();
    for index in 0..22 {
        let mut provider = template.clone();
        provider.id = format!("provider{index}");
        provider.models = (0..3).map(|m| format!("model-{index}-{m}")).collect();
        for model in &provider.models {
            provider
                .model_modalities
                .insert(model.clone(), vec!["text".to_string(), "image".to_string()]);
            provider.model_context_window.insert(model.clone(), 128_000);
        }
        config.providers.push(provider);
    }

    let json = serde_json::to_string(&config).unwrap();
    println!("\n  序列化后 {} KB", json.len() / 1024);

    // 预热
    for _ in 0..100 {
        std::hint::black_box(config.clone());
    }
    let rounds = 10_000;
    let start = Instant::now();
    for _ in 0..rounds {
        std::hint::black_box(config.clone());
    }
    let each_us = start.elapsed().as_secs_f64() * 1e6 / rounds as f64;
    println!("  单次 clone   {each_us:>8.1} µs");
    println!("  一条消息按 3 次算 {:>6.1} µs", each_us * 3.0);
    println!(
        "  1000 条/分钟的群 每分钟 {:>6.1} ms",
        each_us * 3.0 * 1000.0 / 1000.0
    );
}
