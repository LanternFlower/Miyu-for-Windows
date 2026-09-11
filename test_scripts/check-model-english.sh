#!/usr/bin/env bash
# 模型可见面无 CJK 门禁。
#
# 检查范围刻意收窄到「模型会读到的提示性文案」：
#   - src/tools/descriptions/*.json 中键名为 description / summary /
#     stub_example 的字符串值（递归，含 parameters 嵌套）。
# display_name 豁免（UI 用）；enum 值、示例数据等允许中文——那是数据不是提示。
# UI 文案（i18n::text / localize）不在本门禁范围内。
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - "$@" <<'PY'
import glob
import json
import re
import sys

CJK = re.compile(r"[㐀-䶿一-鿿豈-﫿]")
CHECKED_KEYS = {"description", "summary", "stub_example"}

# 白名单:描述里的中文是功能性的,不是提示语——
#   get_exchange_rate: 示例 "USD or 美元" 告诉模型该工具接受中文货币名;
#   glob: 示例 *ai*测试* 演示模式匹配支持 CJK 文件名;
#   manage_script: "Description:/描述：" 是脚本头被逐字解析的协议标记。
ALLOWED = {
    ("get_exchange_rate.json", "$.parameters.properties.base.description"),
    ("get_exchange_rate.json", "$.parameters.properties.target.description"),
    ("glob.json", "$.parameters.properties.pattern.description"),
    ("manage_script.json", "$.parameters.properties.description.description"),
}

def walk(node, path, hits):
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "display_name":
                continue
            if key in CHECKED_KEYS and isinstance(value, str):
                if CJK.search(value):
                    hits.append(f"{path}.{key}")
            walk(value, f"{path}.{key}", hits)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            walk(value, f"{path}[{index}]", hits)

failed = False
for file in sorted(glob.glob("src/tools/descriptions/*.json")):
    try:
        data = json.load(open(file, encoding="utf-8"))
    except Exception as error:
        print(f"INVALID JSON: {file}: {error}")
        failed = True
        continue
    hits = []
    walk(data, "$", hits)
    import os
    base = os.path.basename(file)
    hits = [hit for hit in hits if (base, hit) not in ALLOWED]
    if hits:
        failed = True
        print(f"CJK in model-facing text: {file}")
        for hit in hits:
            print(f"  {hit}")

if failed:
    sys.exit(1)
print("model-facing descriptions: no CJK, all JSON valid")
PY
