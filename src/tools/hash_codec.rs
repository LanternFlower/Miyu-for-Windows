use super::{ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use base64::Engine;
use blake2::{Blake2b512, Blake2s256};
use serde_json::{json, Value};
use sha1::Digest as Sha1Digest;

/// 哈希与解码合并成一件 `codec`(08-17):两者都是"给一段输入换个表示",
/// 分成两个工具只是让 tools 数组多背一份外壳。
pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "codec",
        "编解码工具。op=hash 计算哈希（md5/sha1/sha224/sha256/sha384/sha512/sha3_*/blake2b/b2sum/blake2s/blake3/crc32/adler32，或 all/mainstream 全算）；op=decode 解码 base64、hex、url、html 或 rot13。要匹配 shell echo 的语义，input_text 里要带上末尾换行。",
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["hash", "decode"], "description": "hash 计算哈希，decode 解码。" },
                "input_text": { "type": "string", "description": "输入文本字节。匹配 echo 这类 shell 命令时要带 \\n。" },
                "input_format": { "type": "string", "enum": ["text", "hex", "base64", "url", "html", "rot13"], "description": "输入编码。op=hash 时可选 text/hex/base64（默认 text）；op=decode 时必填，指明待解码的编码。" },
                "algorithms": { "type": "string", "description": "仅 op=hash：算法名，逗号或空格分隔；all/mainstream 表示全算。默认 sha256。" }
            },
            "required": ["op", "input_text"],
            "additionalProperties": false
        }),
        |args| async move {
            match args.get("op").and_then(Value::as_str).unwrap_or_default() {
                "hash" => calculate(args),
                "decode" => decode(args),
                other => anyhow::bail!("unknown op: {other}; expected hash or decode"),
            }
        },
    ));
}

fn calculate(args: Value) -> Result<String> {
    let input = args
        .get("input_text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let fmt = args
        .get("input_format")
        .and_then(Value::as_str)
        .unwrap_or("text");
    let data = bytes(input, fmt)?;
    let algorithms = args
        .get("algorithms")
        .and_then(Value::as_str)
        .unwrap_or("sha256");
    let algs: Vec<&str> =
        if algorithms.trim().is_empty() || algorithms == "all" || algorithms == "mainstream" {
            vec![
                "md5", "sha1", "sha224", "sha256", "sha384", "sha512", "sha3_224", "sha3_256",
                "sha3_384", "sha3_512", "blake2b", "blake2s", "b2sum", "blake3", "crc32",
                "adler32",
            ]
        } else {
            algorithms
                .split([',', ' '])
                .filter(|item| !item.is_empty())
                .collect()
        };
    let mut results = serde_json::Map::new();
    let mut unsupported = Vec::new();
    for alg in algs {
        let value = match alg.to_lowercase().as_str() {
            "md5" => format!("{:x}", md5_compat::compute(&data)),
            "sha1" => format!("{:x}", sha1::Sha1::digest(&data)),
            "sha224" => format!("{:x}", sha2::Sha224::digest(&data)),
            "sha256" => format!("{:x}", sha2::Sha256::digest(&data)),
            "sha384" => format!("{:x}", sha2::Sha384::digest(&data)),
            "sha512" => format!("{:x}", sha2::Sha512::digest(&data)),
            "sha3_224" | "sha3-224" => format!("{:x}", sha3::Sha3_224::digest(&data)),
            "sha3_256" | "sha3-256" => format!("{:x}", sha3::Sha3_256::digest(&data)),
            "sha3_384" | "sha3-384" => format!("{:x}", sha3::Sha3_384::digest(&data)),
            "sha3_512" | "sha3-512" => format!("{:x}", sha3::Sha3_512::digest(&data)),
            "blake2b" | "b2sum" => format!("{:x}", Blake2b512::digest(&data)),
            "blake2s" => format!("{:x}", Blake2s256::digest(&data)),
            "blake3" => blake3::hash(&data).to_hex().to_string(),
            "crc32" => format!("{:08x}", crc32fast::hash(&data)),
            "adler32" => format!("{:08x}", adler32(&data)),
            other => {
                unsupported.push(other.to_string());
                format!("unsupported algorithm: {other}")
            }
        };
        results.insert(alg.to_string(), Value::String(value));
    }
    // 拼错的算法名单独点名,不再无声混在 success 结果里。
    Ok(serde_json::to_string_pretty(&json!({
        "success": unsupported.is_empty(),
        "byte_length": data.len(),
        "results": results,
        "unsupported_algorithms": unsupported,
    }))?)
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + *byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

fn decode(args: Value) -> Result<String> {
    let input = args
        .get("input_text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let fmt = args
        .get("input_format")
        .and_then(Value::as_str)
        .unwrap_or("base64");
    let output = match fmt {
        "base64" => String::from_utf8_lossy(
            &base64::engine::general_purpose::STANDARD.decode(input.trim())?,
        )
        .to_string(),
        "hex" => String::from_utf8_lossy(&hex::decode(input.trim())?).to_string(),
        "url" => urlencoding::decode(input)?.to_string(),
        "html" => input
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
            .replace("&quot;", "\"")
            .replace("&#39;", "'"),
        "rot13" => input.chars().map(rot13).collect(),
        other => bail!("unsupported input_format: {other}"),
    };
    Ok(serde_json::to_string_pretty(
        &json!({"success": true, "decoded_text": output}),
    )?)
}

fn bytes(input: &str, fmt: &str) -> Result<Vec<u8>> {
    Ok(match fmt {
        "text" => input.as_bytes().to_vec(),
        "hex" => hex::decode(input.trim())?,
        "base64" => base64::engine::general_purpose::STANDARD.decode(input.trim())?,
        other => bail!("unsupported input_format: {other}"),
    })
}

fn rot13(ch: char) -> char {
    match ch {
        'a'..='z' => (((ch as u8 - b'a' + 13) % 26) + b'a') as char,
        'A'..='Z' => (((ch as u8 - b'A' + 13) % 26) + b'A') as char,
        _ => ch,
    }
}

mod md5_compat {
    pub use md5::compute;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b2sum_matches_coreutils_blake2b_with_echo_newline() {
        let output = calculate(json!({
            "input_text": "arch\n",
            "algorithms": "b2sum",
            "input_format": "text"
        }))
        .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["byte_length"], 5);
        assert!(value["results"]["b2sum"]
            .as_str()
            .unwrap()
            .starts_with("67989d"));

        let output = calculate(json!({
            "input_text": "debian\n",
            "algorithms": "b2sum",
            "input_format": "text"
        }))
        .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["byte_length"], 7);
        assert!(value["results"]["b2sum"]
            .as_str()
            .unwrap()
            .starts_with("28364b"));
    }

    #[test]
    fn b2sum_and_blake3_are_not_aliases() {
        let output = calculate(json!({
            "input_text": "arch\n",
            "algorithms": "b2sum,blake3",
            "input_format": "text"
        }))
        .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_ne!(value["results"]["b2sum"], value["results"]["blake3"]);
    }
}
