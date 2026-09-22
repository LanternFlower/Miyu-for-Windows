use super::*;
use sha2::Digest;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("../../../../src/tools/fixtures/registry-shapes.json");

fn shape(registry: &ToolRegistry) -> BTreeMap<String, String> {
    registry
        .definitions()
        .into_iter()
        .map(|definition| {
            let payload = serde_json::to_string(&definition).unwrap();
            let digest = sha2::Sha256::digest(payload.as_bytes());
            (definition.function.name.clone(), hex::encode(digest))
        })
        .collect()
}

fn current() -> serde_json::Value {
    let temp = tempfile::tempdir().unwrap();
    let paths = tests::test_paths(temp.path());
    let config = AppConfig::default();
    serde_json::json!({
        "normal": shape(&builtin_registry(&config, &paths)),
        "dev": shape(&dev_registry(&config, &paths)),
        "restricted": shape(&restricted_platform_registry(&config, &paths)),
    })
}

#[test]
fn registry_shapes_match_fixture() {
    let expected: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let actual = current();
    for face in ["normal", "dev", "restricted"] {
        let want = expected[face].as_object().cloned().unwrap_or_default();
        let got = actual[face].as_object().cloned().unwrap_or_default();
        let missing: Vec<_> = want.keys().filter(|k| !got.contains_key(*k)).collect();
        let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
        let changed: Vec<_> = want
            .iter()
            .filter(|(k, v)| got.get(*k).is_some_and(|g| g != *v))
            .map(|(k, _)| k)
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty() && changed.is_empty(),
            "{face} face drifted: missing={missing:?} extra={extra:?} changed={changed:?} \
                 (run `cargo test write_registry_shape_fixture -- --ignored` if intended)"
        );
    }
}

#[test]
#[ignore]
fn write_registry_shape_fixture() {
    let path = std::path::Path::new(miyu_base::WORKSPACE_ROOT)
        .join("src/tools/fixtures/registry-shapes.json");
    std::fs::write(&path, serde_json::to_string_pretty(&current()).unwrap()).unwrap();
    println!("wrote {}", path.display());
}
