//! 供应商能力记录(乐观 + 自愈,免探测请求)。
//!
//! 背景:DeepSeek 官方与部分代理支持 Responses 端点但**不支持**
//! `previous_response_id`(固定 null)。续传第二步只发增量,上游没有会话
//! 状态就会撞 `No tool call found for tool output ...` 类 400(opencodego
//! 实锤,任务#16)。"返回了 response id ≠ 支持续传",所以能力不能靠猜:
//! 默认乐观放行,撞到签名错误后**持久记 false**,该供应商此后所有会话
//! 直接走无状态全量回放(lower_responses_messages 的重放是完整的)。
//!
//! 记录按 base_url 键控,只存 false 条目(缺席=乐观)。单 daemon 进程,
//! 读-改-写尽力而为,失败只降级为"下次再撞一遍",不影响正确性。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "provider-capabilities.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct CapabilityFile {
    #[serde(default)]
    providers: std::collections::BTreeMap<String, ProviderCapabilities>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProviderCapabilities {
    /// false = 该端点的 Responses 不支持 previous_response_id 续传。
    #[serde(default)]
    responses_tool_continuation: Option<bool>,
    #[serde(default)]
    updated_at_unix: Option<u64>,
}

pub(crate) fn store_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(FILE_NAME)
}

fn load(path: &Path) -> CapabilityFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// 该供应商是否已被记为"续传不可用"。
pub(crate) fn continuation_unsupported(path: &Path, base_url: &str) -> bool {
    load(path)
        .providers
        .get(base_url)
        .and_then(|caps| caps.responses_tool_continuation)
        == Some(false)
}

/// 持久记录"续传不可用"。尽力而为:写失败仅告警,自愈仍靠进程内原子位生效。
pub(crate) fn record_continuation_unsupported(path: &Path, base_url: &str) {
    let mut file = load(path);
    let entry = file.providers.entry(base_url.to_string()).or_default();
    entry.responses_tool_continuation = Some(false);
    entry.updated_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs());
    let write = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
        .and_then(|_| {
            std::fs::write(
                path,
                serde_json::to_string_pretty(&file).unwrap_or_default(),
            )
        });
    if let Err(error) = write {
        tracing::warn!(error = %error, "failed to persist provider capability record");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_record_is_optimistic_and_false_sticks() {
        let temp = tempfile::tempdir().unwrap();
        let path = store_path(temp.path());
        assert!(!continuation_unsupported(&path, "https://a.example/v1"));
        record_continuation_unsupported(&path, "https://a.example/v1");
        assert!(continuation_unsupported(&path, "https://a.example/v1"));
        // 其他供应商不受影响。
        assert!(!continuation_unsupported(&path, "https://b.example/v1"));
    }
}
