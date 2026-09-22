#!/usr/bin/env python3
"""违规关键词表在**真实聊天记录**上的命中审计。

违规词是子串匹配（ASCII 不分大小写），在技术群里极容易误命中：`OD` 会中在
`model`/`code` 里、`64` 中在 `x86_64`、`盒` 中在「沙盒」、`节点` 中在「背光节
点」。每一次误命中都是一次白烧的判断调用。

这个脚本拿你自己的群聊库跑一遍，告诉你每个词命中了多少条、命中的都长什么样，
改完词表再跑一遍就知道有没有改对。

跑法：

    python3 testkit/qq/keyword_audit.py                      # 用配置里的词表
    python3 testkit/qq/keyword_audit.py --top 40             # 多看几条
    python3 testkit/qq/keyword_audit.py --keywords a.txt     # 试一份新词表（一行一个）

只读，不碰库也不碰配置。
"""

import argparse
import json
import os
import re
import sqlite3
import sys
from pathlib import Path


def miyu_home():
    return Path(os.environ.get("MIYU_HOME") or (Path.home() / ".miyu"))


def load_config_keywords():
    """配置里自定义过就用那份，否则读代码里的默认表。"""
    config = miyu_home() / "config" / "config.jsonc"
    if config.exists():
        raw = re.sub(r"^\s*//.*$", "", config.read_text(encoding="utf-8"), flags=re.M)
        found = []

        def walk(node):
            if isinstance(node, dict):
                for key, value in node.items():
                    if key == "moderation_keywords" and isinstance(value, list):
                        found.append(value)
                    walk(value)
            elif isinstance(node, list):
                for value in node:
                    walk(value)

        try:
            walk(json.loads(raw))
        except json.JSONDecodeError:
            pass
        if found:
            return found[0], "配置文件"
    source = (
        Path(__file__).resolve().parents[2]
        / "crates/miyu-base/src/config/platform_plugins/real_context.rs"
    )
    text = source.read_text(encoding="utf-8")
    start = text.index("const KEYWORDS: &[&str] = &[")
    end = text.index("];", start)
    return re.findall(r'"((?:[^"\\]|\\.)*)"', text[start:end]), "代码默认表"


def history_db():
    path = miyu_home() / "data/platforms/onebot/message_history/history.sqlite3"
    if not path.exists():
        path = miyu_home() / "data/platforms/onebot/real_context/history.sqlite3"
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keywords", type=Path, help="换一份词表试（一行一个）")
    parser.add_argument("--top", type=int, default=25, help="列前几名（默认 25）")
    parser.add_argument("--samples", type=int, default=2, help="每个词看几条原文")
    args = parser.parse_args()

    if args.keywords:
        words = [w.strip() for w in args.keywords.read_text(encoding="utf-8").splitlines()]
        origin = str(args.keywords)
    else:
        words, origin = load_config_keywords()
    words = [w for w in words if w]

    database = history_db()
    if not database.exists():
        print(f"! 没找到聊天记录库：{database}", file=sys.stderr)
        return 2
    con = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    texts = [row[0] or "" for row in con.execute("SELECT text FROM messages")]
    lowered = [text.lower() for text in texts]

    hits = {}
    samples = {}
    for word in words:
        # `find_keyword` 的规则：ASCII 词大小写不敏感，其余直接子串。
        ascii_word = word.isascii()
        needle = word.lower() if ascii_word else word
        pool = lowered if ascii_word else texts
        matched = [index for index, text in enumerate(pool) if needle in text]
        if matched:
            hits[word] = len(matched)
            samples[word] = [texts[i][:70].replace("\n", " ") for i in matched[: args.samples]]

    total = sum(hits.values())
    print(f"词表来源：{origin}（{len(words)} 条）")
    print(f"消息总数：{len(texts)}")
    print(f"命中过的词：{len(hits)} 条；从没命中过：{len(words) - len(hits)} 条")
    print(f"命中总量：{total} 次（≈ 每 {len(texts) / max(total, 1):.0f} 条消息触发一次判断）\n")
    for word, count in sorted(hits.items(), key=lambda item: -item[1])[: args.top]:
        print(f"{count:6}  {word!r}")
        for line in samples[word]:
            print(f"          {line}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
