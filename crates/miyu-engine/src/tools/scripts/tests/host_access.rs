//! 头部 `Capabilities:` 与宿主查询令牌的授权规则(09-16)。

use crate::tools::scripts::*;
use crate::tools::ToolTrust;

#[test]
fn capabilities_header_is_parsed_with_aliases() {
    let raw =
        "#!/usr/bin/env python3\n# Description: x\n# Capabilities: providers.read, host.info\n";
    assert_eq!(
        extract_metadata(raw).capabilities,
        vec!["providers.read".to_string(), "host.info".to_string()]
    );
    let raw = "#!/usr/bin/env python3\n# 能力：subsystems.read\n";
    assert_eq!(
        extract_metadata(raw).capabilities,
        vec!["subsystems.read".to_string()]
    );
    assert!(extract_metadata("#!/bin/sh\n# Description: y\n")
        .capabilities
        .is_empty());
}

/// 只认词表里的 id,且只给 `Trust: owner` 的脚本——外部场所也会跑的脚本拿不到令牌。
#[test]
fn host_capabilities_only_for_owner_scripts_and_known_ids() {
    let mut entry: ScriptEntry =
        serde_json::from_value(serde_json::json!({ "id": "probe", "path": "probe.py" })).unwrap();
    entry.capabilities = vec!["providers.read".to_string(), "bogus".to_string()];
    assert_eq!(
        crate::tools::scripts::index::host_capabilities_for(&entry),
        vec!["providers.read".to_string()]
    );
    entry.trust = ToolTrust::External;
    assert!(crate::tools::scripts::index::host_capabilities_for(&entry).is_empty());
}

/// 非 daemon 进程里不签令牌:`issue_host_grant` 为 None,脚本环境里就没有
/// MIYU_HOST_TOKEN。(daemon 内签发的正向路径见 web::tests::ipc_bridge。)
#[test]
fn detected_scripts_carry_their_capabilities_into_the_entry() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("probe.py");
    std::fs::write(
        &path,
        "#!/usr/bin/env python3\n# Description: Probe the host.\n# Capabilities: host.info\nprint('x')\n",
    )
    .unwrap();
    let entry = index::auto_detect_script(&path).expect("described script registers");
    assert_eq!(entry.capabilities, vec!["host.info".to_string()]);
}
