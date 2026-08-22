//! token 估算：优先使用 OpenAI-family `o200k_base` BPE，失败时回退到字符规则。

const CHARS_PER_TOKEN_LATIN: usize = 4;
const CHARS_PER_TOKEN_CJK: usize = 2;

/// 估算单段文本的 token 数（非空文本至少为 1，空串为 0）。
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    tiktoken_tokens(text)
        .unwrap_or_else(|| text_tokens(text))
        .max(1)
}

/// 估算多段文本合计 token 数。
#[allow(dead_code)]
pub fn estimate_texts_tokens(texts: &[&str]) -> u64 {
    let combined: String = texts.iter().copied().collect();
    estimate_tokens(&combined) as u64
}

fn text_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut latin = 0usize;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else {
            latin += 1;
        }
    }
    cjk.div_ceil(CHARS_PER_TOKEN_CJK) + latin.div_ceil(CHARS_PER_TOKEN_LATIN)
}

fn tiktoken_tokens(text: &str) -> Option<usize> {
    std::panic::catch_unwind(|| crate::token_counter::count(text)).ok()
}

fn is_cjk(ch: char) -> bool {
    let code = ch as u32;
    (0x4E00..=0x9FFF).contains(&code)
        || (0x3400..=0x4DBF).contains(&code)
        || (0x20000..=0x2A6DF).contains(&code)
        || (0x3040..=0x30FF).contains(&code)
        || (0xAC00..=0xD7AF).contains(&code)
        || (0xFF00..=0xFFEF).contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_uses_bpe_tokenizer() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 1);
        assert!(estimate_tokens("hello world") >= 2);
    }

    #[test]
    fn cjk_uses_bpe_tokenizer() {
        assert_eq!(estimate_tokens("你好"), 1);
        assert_eq!(estimate_tokens("你好世界"), 2);
        assert_eq!(estimate_tokens("你好世"), 2);
    }

    #[test]
    fn mixed_text_counts_non_empty_tokens() {
        assert_eq!(estimate_tokens("abcd你好"), 2);
        assert!(estimate_tokens("abc你好世") >= 2);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_texts_tokens(&[]), 0);
    }

    #[test]
    fn counts_match_official_tiktoken_vectors() {
        // Literal anchors: small, stable, and hand-checkable, so a wrong
        // expectation here is a real regression rather than a stale number.
        let vectors = [
            ("", 0),
            ("hello world", 2),
            ("你好世界", 2),
            ("Rust + 中文 + emoji 🚀\nsecond line", 10),
            (" \t\n\r\n punctuation: !@#$%^&*()[]{}", 13),
            ("<|endoftext|><|endofprompt|>", 13),
        ];

        for (text, expected) in vectors {
            assert_eq!(estimate_tokens(text), expected);
        }
    }

    #[test]
    fn real_documents_match_the_reference_encoder() {
        // The shipped prompts and README are the large real-world corpus this
        // suite wants to cover: mixed CJK/Latin, markdown, emoji. Their
        // expectations are computed from the reference encoder rather than
        // hard-coded — a literal here turns "edit the persona" into "the
        // build is red until someone updates a magic number".
        let reference = tiktoken_rs::o200k_base().unwrap();
        for text in [
            include_str!("prompts/miyu.md"),
            include_str!("prompts/compact.md"),
            include_str!("../README.md"),
        ] {
            assert_eq!(estimate_tokens(text), reference.count_ordinary(text));
        }
    }

    #[test]
    fn count_only_encoder_matches_full_encoder() {
        use sha2::{Digest, Sha256};

        let atoms = [
            "a",
            "Z",
            " token",
            "你好",
            "世界",
            "かな",
            "한글",
            "🚀",
            "🙂",
            "\n",
            "\r\n",
            "\t",
            "123",
            "0001",
            "_",
            "'s",
            "—",
            "<|endoftext|>",
        ];
        let full = tiktoken_rs::o200k_base().unwrap();
        let mut digest = Sha256::new();
        let mut total = 0usize;

        for seed in 0..8192usize {
            let mut text = String::new();
            for step in 0..(seed % 64 + 1) {
                let index = seed.wrapping_mul(17).wrapping_add(step.wrapping_mul(31)) % atoms.len();
                text.push_str(atoms[index]);
            }
            let count = crate::token_counter::count(&text);
            assert_eq!(count, full.count_ordinary(&text));
            total += count;
            digest.update((count as u64).to_le_bytes());
        }

        assert_eq!(total, 383_718);
        assert_eq!(
            hex::encode(digest.finalize()),
            "64735b07c444d71dcd8977a257c8c029160793f46044ec257187a4707dd9def1"
        );
    }

    #[test]
    #[ignore]
    fn benchmark_count_only_encoder() {
        let atoms = [
            "a",
            "Z",
            " token",
            "你好",
            "世界",
            "かな",
            "한글",
            "🚀",
            "🙂",
            "\n",
            "\r\n",
            "\t",
            "123",
            "0001",
            "_",
            "'s",
            "—",
            "<|endoftext|>",
        ];
        let corpus = (0..8192usize)
            .map(|seed| {
                let mut text = String::new();
                for step in 0..(seed % 64 + 1) {
                    let index =
                        seed.wrapping_mul(17).wrapping_add(step.wrapping_mul(31)) % atoms.len();
                    text.push_str(atoms[index]);
                }
                text
            })
            .collect::<Vec<_>>();
        std::hint::black_box(crate::token_counter::count(&corpus[0]));

        let started = std::time::Instant::now();
        let total = corpus
            .iter()
            .map(|text| crate::token_counter::count(text))
            .sum::<usize>();
        let elapsed = started.elapsed();

        assert_eq!(total, 383_718);
        eprintln!(
            "tokenizer_benchmark elapsed_ns={} corpus_bytes={} tokens={total}",
            elapsed.as_nanos(),
            corpus.iter().map(String::len).sum::<usize>()
        );
    }
}

#[cfg(test)]
mod tool_catalogue_probe {
    /// 量尺：`cargo test --lib tool_catalogue_probe -- --ignored --nocapture`
    ///
    /// C13 说「token 重复计算」。工具目录是**字节稳定**的（那是缓存契约的一
    /// 部分），而 `estimate_tool_definition_tokens` 每次都把整份目录序列化再
    /// 逐段估 token。先量清楚一次多少钱，再谈值不值得缓存。
    #[test]
    #[ignore]
    fn estimating_the_whole_tool_catalogue() {
        // 用真实的描述 JSON 拼一份等体量的语料：61 个文件、约 64 KB
        let mut catalogue = Vec::new();
        let dir = std::path::Path::new("src/tools/descriptions");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                catalogue.push(std::fs::read_to_string(&path).unwrap());
            }
        }
        let bytes: usize = catalogue.iter().map(String::len).sum();

        for _ in 0..3 {
            for text in &catalogue {
                std::hint::black_box(super::estimate_tokens(text));
            }
        }
        let rounds = 50;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            let total: usize = catalogue
                .iter()
                .map(|text| super::estimate_tokens(text))
                .sum();
            std::hint::black_box(total);
        }
        let each = started.elapsed().as_secs_f64() * 1000.0 / rounds as f64;

        // 缓存要先序列化+哈希才能知道「是不是同一份目录」，所以这两步是
        // 省不掉的固定成本。量出来才知道缓存能省几成。
        let values: Vec<serde_json::Value> = catalogue
            .iter()
            .map(|text| serde_json::from_str(text).unwrap())
            .collect();
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            use std::hash::{Hash, Hasher};
            let mut hasher = rustc_hash::FxHasher::default();
            for value in &values {
                serde_json::to_string(value).unwrap().hash(&mut hasher);
            }
            std::hint::black_box(hasher.finish());
        }
        let overhead = started.elapsed().as_secs_f64() * 1000.0 / rounds as f64;

        println!(
            "\n  {} 个工具描述，共 {:.1} KB\n  \
             估一遍全目录        {each:.2} ms\n  \
             序列化+哈希（缓存的固定成本） {overhead:.2} ms\n  \
             → 命中缓存时省下 {:.2} ms（{:.0}%）",
            catalogue.len(),
            bytes as f64 / 1024.0,
            each - overhead,
            100.0 * (each - overhead) / each,
        );
    }
}
