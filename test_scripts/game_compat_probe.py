#!/usr/bin/env python3
"""Linux 游戏兼容性三源查询 —— game_compat 加 AreWeAntiCheatYet 的验证脚本。

三个数据源并发查询，输出清洗后的合并结果：

  protondb           ProtonDB 评级 + 玩家报告（经 Algolia 解析 Steam AppID）
  caniplayonlinux    caniplayonlinux.com 的结论快照
  areweanticheatyet  AWACY 的反作弊状态（Supported/Running/Planned/Broken/Denied）

用法:
  ./game_compat_probe.py "黑神话悟空"
  ./game_compat_probe.py 2358720 -n 20          # 多要几条评论
  ./game_compat_probe.py Valorant -n 0          # 只要结论，不要评论
  ./game_compat_probe.py "Elden Ring" --refresh # 忽略缓存
  ./game_compat_probe.py "Apex Legends" -f xml  # 换格式
  ./game_compat_probe.py "Apex Legends" --json  # 调试用

三个源恒定全查——总共约 2 秒、1.5KB，为省这点再分单源查是过度设计。唯一的
调节旋钮是 -n（ProtonDB 评论条数），因为只有它的体积随游戏热度线性增长。

输出即工具返回值：默认输出 Markdown，可原样喂给模型——同样的信息比 JSON 省约
一半 token。正文一律英文：三个数据源本身都是英文，混排中文只多一层翻译损耗。

说明:
  * Algolia 认中文名（"黑神话悟空" 可直接查到），但非 Steam 游戏（如 Valorant）
    查不到；此时脚本会警告搜索结果可能不是同一个游戏，并继续返回其余两源。
  * AWACY 未收录 != 该游戏没有反作弊，输出里会显式区分。
  * AWACY 全表与 caniplayonlinux 的 sitemap 会缓存到 ~/.cache/miyu-game-compat/，
    默认 24 小时过期。
"""

from __future__ import annotations

import argparse
import difflib
import html
import json
import re
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

UA = {"User-Agent": "miyu-game-compat-probe/0.1"}
TIMEOUT = 25

ALGOLIA_URL = "https://94he6yatei-dsn.algolia.net/1/indexes/steamdb/query"
ALGOLIA_KEY = "9ba0e69fb2974316cdaec8f5f257088f"
ALGOLIA_APP = "94HE6YATEI"
PROTONDB = "https://www.protondb.com"
CIPOL = "https://caniplayonlinux.com"
AWACY_JSON = (
    "https://raw.githubusercontent.com/AreWeAntiCheatYet/AreWeAntiCheatYet"
    "/HEAD/games.json"
)

CACHE_DIR = Path.home() / ".cache" / "miyu-game-compat"
CACHE_TTL = 24 * 3600

# ProtonDB 的 fault 字段 → 简称。只有取值为 "yes" 的才会进输出。
FAULTS = [
    ("audioFaults", "audio"),
    ("graphicalFaults", "graphics"),
    ("windowingFaults", "windowing"),
    ("inputFaults", "input"),
    ("saveGameFaults", "save_game"),
    ("performanceFaults", "performance"),
    ("stabilityFaults", "stability"),
    ("significantBugs", "bugs"),
]


# ── HTTP ────────────────────────────────────────────────────────────

def _fetch(url: str, payload=None, headers=None) -> bytes:
    hdrs = dict(UA)
    hdrs.update(headers or {})
    body = None
    if payload is not None:
        body = json.dumps(payload).encode()
        hdrs["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=body, headers=hdrs)
    with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
        return resp.read()


def get_json(url, payload=None, headers=None):
    return json.loads(_fetch(url, payload, headers))


def get_text(url) -> str:
    return _fetch(url).decode("utf-8", "replace")


def cached(name: str, url: str, refresh: bool, parse_json: bool):
    """带 TTL 的磁盘缓存。AWACY 全表 ~460KB、sitemap ~460KB，都是低频变动。"""
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    path = CACHE_DIR / name
    if not refresh and path.exists() and time.time() - path.stat().st_mtime < CACHE_TTL:
        raw = path.read_bytes()
    else:
        raw = _fetch(url)
        path.write_bytes(raw)
    return json.loads(raw) if parse_json else raw.decode("utf-8", "replace")


# ── 工具函数 ─────────────────────────────────────────────────────────

def norm(s: str) -> str:
    return re.sub(r"[^a-z0-9]", "", (s or "").lower())


def slugify(s: str) -> str:
    """caniplayonlinux 的 slug 规则。撇号和 & 直接删除，不转成连字符。

    实测 145 条真值样本命中率 96%；朴素实现（撇号→连字符）只有 80%。
    """
    s = (s or "").lower()
    for ch in "'’&＆":
        s = s.replace(ch, "")
    return re.sub(r"[^a-z0-9]+", "-", s).strip("-")


def is_mostly_ascii(s: str) -> bool:
    return all(ord(c) < 128 for c in s)


def fmt_date(ts: int) -> str:
    if not ts:
        return "unknown"
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%d")


def parse_awacy_date(s: str):
    """AWACY 的 updates[].date 格式不统一，ISO 和 'Oct 31, 2024, 4:07 PM UTC' 混用。"""
    if not s:
        return datetime.min.replace(tzinfo=timezone.utc)
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00"))
    except ValueError:
        pass
    for fmt in ("%b %d, %Y, %I:%M %p %Z", "%b %d, %Y, %I:%M %p GMT%z"):
        try:
            d = datetime.strptime(s, fmt)
            return d if d.tzinfo else d.replace(tzinfo=timezone.utc)
        except ValueError:
            continue
    return datetime.min.replace(tzinfo=timezone.utc)


def strip_html(raw: str) -> str:
    """标签转行分隔符。行结构必须保住——value_after_label 靠它界定字段末尾。"""
    raw = re.sub(r"(?is)<(script|style|svg)[^>]*>.*?</\1>", " ", raw)
    text = re.sub(r"(?s)<[^>]+>", "\n", raw)
    text = re.sub(r"[ \t]+", " ", text)
    return re.sub(r"\n\s*\n+", "\n", text).strip()


# ── ProtonDB ────────────────────────────────────────────────────────

def algolia_search(query: str, hits: int = 5):
    resp = get_json(
        ALGOLIA_URL,
        {
            "query": query,
            "facetFilters": [["appType:Game"]],
            "hitsPerPage": hits,
            "attributesToRetrieve": ["name", "objectID", "oslist"],
            "page": 0,
        },
        {
            "x-algolia-api-key": ALGOLIA_KEY,
            "x-algolia-application-id": ALGOLIA_APP,
            "Referer": PROTONDB,
        },
    )
    return resp.get("hits") or []


def _hash_r(e: int, t: int, n: int) -> str:
    return f"{t}p{(e * (t % n)) & 0xFFFFFFFFFFFFFFFF}"


def _hash_i(s: str) -> int:
    """复刻 ProtonDB 前端 JS 的 `| 0` 32 位有符号截断。"""
    v = 0
    for ch in list(s) + ["m"]:
        v = ((v << 5) - v + ord(ch)) & 0xFFFFFFFF
        if v >= 2**31:
            v -= 2**32
    return abs(v)


def protondb_reports(appid: int):
    counts = get_json(f"{PROTONDB}/data/counts.json")
    n, ts = counts.get("reports", 0), counts.get("timestamp", 0)
    if not n or not ts:
        raise RuntimeError("invalid counts.json")
    pid = _hash_i(f"p{_hash_r(appid, n, ts)}*vRT{_hash_r(1, appid, ts)}undefined")
    return get_json(f"{PROTONDB}/data/reports/all-devices/app/{pid}.json")


def clean_report(r: dict, note_cap: int = 300) -> dict:
    rs = r.get("responses") or {}
    notes = rs.get("notes") or {}
    starts = rs.get("startsPlay")
    verdict = rs.get("verdictOob") or rs.get("verdict")
    if starts != "yes":
        rec = "broken"
    elif verdict == "yes":
        rec = "recommended"
    elif verdict:
        rec = "not_recommended"
    else:
        rec = "unknown"

    variant = rs.get("variant")
    if variant == "experimental":
        proton = "Proton Experimental"
    elif variant == "ge":
        proton = rs.get("customProtonVersion")
    else:
        proton = rs.get("protonVersion")

    out = {"date": fmt_date(r.get("timestamp", 0)), "rec": rec}
    if proton:
        out["proton"] = proton
    launch = (rs.get("launchOptions") or "").strip()
    if launch:
        out["launch"] = launch
    faults = [label for key, label in FAULTS if rs.get(key) == "yes"]
    if faults:
        out["faults"] = faults
    note = (notes.get("concludingNotes") or rs.get("concludingNotes") or "").strip()
    if note:
        out["note"] = note[:note_cap] + ("…" if len(note) > note_cap else "")
    return out


def query_protondb(query: str, max_reports: int):
    if query.isdigit():
        appid = int(query)
        hits = algolia_search(query, 1)
        hit = hits[0] if hits and hits[0].get("objectID") == query else {}
        name = hit.get("name") or query
        oslist = hit.get("oslist") or []
        match = {"method": "app_id", "confidence": "exact"}
    else:
        hits = algolia_search(query, 5)
        if not hits:
            raise RuntimeError(f"Algolia 无结果: {query!r}")
        hit = hits[0]
        appid, name = int(hit["objectID"]), hit["name"]
        oslist = hit.get("oslist") or []
        # 相关性校验：Valorant 这类非 Steam 游戏会静默返回毫不相关的第一条。
        # 中文查询无法直接比对（"黑神话悟空" vs "Black Myth: Wukong"），跳过。
        if is_mostly_ascii(query):
            ratio = difflib.SequenceMatcher(None, norm(query), norm(name)).ratio()
            if ratio < 0.5:
                match = {
                    "method": "algolia",
                    "confidence": "low",
                    "warning": f"search for {query!r} returned {name!r}; "
                    "may not be the same game (it might not be on Steam)",
                    "alternatives": [h["name"] for h in hits[1:4]],
                }
            else:
                match = {"method": "algolia", "confidence": "high" if ratio > 0.8 else "medium"}
        else:
            match = {"method": "algolia", "confidence": "medium", "note": "非拉丁查询，未做相关性校验"}

    summary = get_json(f"{PROTONDB}/api/v1/reports/summaries/{appid}.json")
    try:
        raw = protondb_reports(appid)
        fetch_error = None
    except Exception as exc:
        raw, fetch_error = {"reports": [], "total": 0}, f"{type(exc).__name__}: {exc}"

    items = [clean_report(r) for r in (raw.get("reports") or [])[:max_reports]]
    return {
        "app_id": appid,
        "game_name": name,
        "oslist": oslist,
        "native_linux": "linux" in [o.lower() for o in oslist],
        "match": match,
        "tier": summary.get("tier"),
        "trending": summary.get("trendingTier"),
        "best": summary.get("bestReportedTier"),
        "confidence": summary.get("confidence"),
        "score": summary.get("score"),
        "reports_total": raw.get("total", 0),
        "reports_shown": len(items),
        "reports": items,
        "reports_fetch_error": fetch_error,
        "url": f"{PROTONDB}/app/{appid}",
    }


# ── caniplayonlinux ─────────────────────────────────────────────────

def cipol_resolve(name: str, refresh: bool):
    """先按 slug 直连（~60ms，实测 96% 命中），404 再退 sitemap 索引（~145ms）。

    现有 Rust 实现是扫 108 页目录，实测 1.8s / 108 个请求。
    """
    slug = slugify(name)
    # 纯中文名 slugify 后为空，拼出的 /games// 会命中目录首页并返回 200，
    # 静默产出一份通用简介。必须挡住。
    if not slug:
        raise RuntimeError(f"cannot derive slug from {name!r} (needs an English name)")
    url = f"{CIPOL}/games/{slug}/"
    try:
        return url, get_text(url), {"method": "slug_direct", "confidence": "exact"}
    except urllib.error.HTTPError as exc:
        if exc.code != 404:
            raise

    sitemap = cached("cipol-sitemap.xml", f"{CIPOL}/sitemap-0.xml", refresh, False)
    urls = [
        u
        for u in re.findall(r"<loc>(.*?)</loc>", sitemap)
        if "/games/" in u and not re.search(r"/games/\d+/?$", u)
    ]
    table = {u.rstrip("/").split("/")[-1]: u for u in urls}
    close = difflib.get_close_matches(slug, list(table), n=1, cutoff=0.75)
    if not close:
        raise RuntimeError(f"not listed on caniplayonlinux: {name!r}")
    hit = close[0]
    return table[hit], get_text(table[hit]), {
        "method": "sitemap_fuzzy",
        "confidence": "medium",
        "matched_slug": hit,
    }


def query_cipol(name: str, refresh: bool):
    url, raw, match = cipol_resolve(name, refresh)
    text = html.unescape(strip_html(raw))

    # meta description 的信息密度远高于正文分节抽取（后者大量混入导航与 FAQ），
    # 所以以它为主字段。
    m = re.search(r'<meta name="description" content="([^"]*)"', raw)
    summary = html.unescape(m.group(1)) if m else None

    def after(label, cap=80, want_date=False):
        pat = re.escape(label) + r"\s*:?\s*(.{1,%d}?)(?:\n|$)" % cap
        vals = [m.group(1).strip() for m in re.finditer(pat, text, re.I)]
        vals = [v for v in vals if v]
        if want_date:
            # 页面同时给相对时间（"1 month ago"）和绝对日期，取后者：
            # 相对时间一旦缓存就失真，也没法和 AWACY 的日期比对。
            for v in vals:
                if re.search(r"\b(19|20)\d{2}\b", v):
                    return v
        return vals[0] if vals else None

    verified = after("Last verified", 40, want_date=True)
    for fmt in ("%b %d, %Y", "%B %d, %Y"):
        try:
            verified = datetime.strptime(verified, fmt).strftime("%Y-%m-%d")
            break
        except (ValueError, TypeError):
            continue
    return {
        "summary": summary,
        "last_verified": verified,
        "developer": after("Developer", 60),
        "match": match,
        "url": url,
    }


# ── AreWeAntiCheatYet ───────────────────────────────────────────────

def query_awacy(refresh: bool, appid=None, name=None):
    games = cached("awacy-games.json", AWACY_JSON, refresh, True)

    entry, match = None, {"method": "none", "confidence": "none"}
    if appid:
        for g in games:
            if (g.get("storeIds") or {}).get("steam") == str(appid):
                entry, match = g, {"method": "steam_appid", "confidence": "exact"}
                break
    if entry is None and name:
        target = norm(name)
        for g in games:
            if norm(g["name"]) == target:
                entry, match = g, {"method": "name_exact", "confidence": "high"}
                break
        if entry is None:
            pool = {g["name"]: g for g in games}
            close = difflib.get_close_matches(name, list(pool), n=1, cutoff=0.85)
            if close:
                entry = pool[close[0]]
                match = {
                    "method": "name_fuzzy",
                    "confidence": "low",
                    "matched_name": close[0],
                }

    if entry is None:
        return {
            "found": False,
            "note": "Not listed does not mean the game has no anti-cheat.",
        }

    updates = sorted(
        entry.get("updates") or [], key=lambda u: parse_awacy_date(u.get("date", "")), reverse=True
    )[:2]
    out = {
        "found": True,
        "match": match,
        "status": entry["status"],
        "anticheats": entry.get("anticheats") or [],
        "native": entry.get("native"),
        "date_changed": (entry.get("dateChanged") or "")[:10],
        "url": f"https://areweanticheatyet.com/game/{entry['slug']}",
    }
    if updates:
        out["recent_updates"] = [{"what": u.get("name"), "when": u.get("date")} for u in updates]
    notes = [t for t, _ref in (entry.get("notes") or [])][:3]
    if notes:
        out["notes"] = notes
    return out


# ── 编排 ────────────────────────────────────────────────────────────

def run(query: str, max_reports: int, refresh: bool):
    """两阶段：ProtonDB 先把查询解析成英文名/AppID，另两个源再拿它去匹配。

    中文名（"黑神话悟空"）无法直接生成 caniplayonlinux 的 slug，也匹配不上 AWACY
    的英文条目，所以必须等 ProtonDB 的解析结果。AWACY 全表（~460KB）与 ProtonDB
    并发预热，不占额外时间。
    """
    # 顺序即输出顺序：先身份（游戏是谁），再各源明细。
    result = {"query": query, "game_name": None, "app_id": None,
              "native_linux": None, "sources": {}}
    pdb = None

    with ThreadPoolExecutor(max_workers=3) as pool:
        # AWACY 全表下载不依赖游戏名，与 ProtonDB 解析重叠。
        f_warm = pool.submit(cached, "awacy-games.json", AWACY_JSON, refresh, True)
        try:
            pdb = query_protondb(query, max_reports)
            result["protondb"] = pdb
            result["sources"]["protondb"] = "ok"
        except Exception as exc:
            result["sources"]["protondb"] = f"error: {type(exc).__name__}: {exc}"
        f_warm.exception()  # 让缓存落盘，异常留给下面的正式查询报

        # 解析出的英文名优先；ProtonDB 失败时退回原始查询（非 Steam 游戏走这条）。
        resolved = pdb["game_name"] if pdb else query

        f_cip = pool.submit(query_cipol, resolved, refresh)
        f_awacy = pool.submit(query_awacy, refresh, pdb["app_id"] if pdb else None, resolved)

        for key, fut in (("areweanticheatyet", f_awacy), ("caniplayonlinux", f_cip)):
            try:
                result[key] = fut.result()
                result["sources"][key] = "ok"
            except Exception as exc:
                result["sources"][key] = f"error: {type(exc).__name__}: {exc}"

    if pdb:
        result["game_name"] = pdb["game_name"]
        result["app_id"] = pdb["app_id"]
        result["native_linux"] = pdb["native_linux"]
        # 身份字段已提到顶层，各源块里不再重复。
        for key in ("game_name", "app_id", "native_linux", "oslist"):
            pdb.pop(key, None)
        if pdb.get("reports_fetch_error") is None:
            pdb.pop("reports_fetch_error")
    else:
        for key in ("game_name", "app_id", "native_linux"):
            result.pop(key)
    return result


# ── 输出 ────────────────────────────────────────────────────────────

C = {
    "dim": "\033[2m", "b": "\033[1m", "r": "\033[0m",
    "green": "\033[32m", "yellow": "\033[33m", "red": "\033[31m", "cyan": "\033[36m",
}
if not sys.stdout.isatty():
    C = {k: "" for k in C}

TIER_COLOR = {
    "platinum": "green", "gold": "green", "silver": "yellow",
    "bronze": "yellow", "borked": "red", "pending": "dim",
}
STATUS_COLOR = {
    "Supported": "green", "Running": "yellow", "Planned": "yellow",
    "Broken": "red", "Denied": "red",
}


def paint(text, color):
    return f"{C.get(color, '')}{text}{C['r']}"


def render(res: dict) -> str:
    """唯一的输出格式：人类可读，同时直接作为工具返回值喂给模型。

    不带匹配方法/置信度这类元数据——它们对判断"这游戏能不能玩"没有帮助。
    保留的例外是搜索结果明显不匹配时的警告，那是要人看见并采取行动的。
    管道输出时 ANSI 颜色自动关闭，所以同一份文本两种用途通用。
    """
    L = []
    head = f"{C['b']}{res.get('game_name') or res['query']}{C['r']}"
    if res.get("app_id"):
        head += f"  {C['dim']}appid={res['app_id']}{C['r']}"
    if res.get("native_linux"):
        head += f"  {paint('★ native Linux build', 'green')}"
    L.append(head)

    for src, status in res["sources"].items():
        if status != "ok":
            reason = re.sub(r"^error: \w+Error: ", "", status)
            L.append(f"  {paint('✗', 'red')} {src}  {C['dim']}{reason}{C['r']}")

    pdb = res.get("protondb")
    if pdb:
        warn = (pdb.get("match") or {}).get("warning")
        if warn:
            L.append(f"\n{paint('⚠ ' + warn, 'red')}")
            alts = pdb["match"].get("alternatives")
            if alts:
                L.append(f"  {C['dim']}other candidates: {', '.join(alts)}{C['r']}")

        tier = pdb.get("tier") or "?"
        meta = []
        if pdb.get("trending") and pdb["trending"] != tier:
            meta.append(f"trending {pdb['trending']}")
        if pdb.get("best") and pdb["best"] != tier:
            meta.append(f"best {pdb['best']}")
        meta.append(f"{pdb.get('reports_total')} reports")
        L.append(f"\n{C['b']}protondb{C['r']}  "
                 f"{paint(tier.upper(), TIER_COLOR.get(tier, 'dim'))}"
                 f"  {C['dim']}{' · '.join(meta)}{C['r']}")
        if pdb.get("reports_fetch_error"):
            L.append(f"  {paint('reports fetch failed: ' + pdb['reports_fetch_error'], 'red')}")
        for r in pdb.get("reports", []):
            mark = {"recommended": paint("✓", "green"),
                    "not_recommended": paint("~", "yellow"),
                    "broken": paint("✗", "red")}.get(r["rec"], "?")
            line = f"  {mark} {C['dim']}{r['date']}{C['r']}  {r.get('proton') or 'unknown'}"
            if r.get("faults"):
                line += f"  {paint('[' + ','.join(r['faults']) + ']', 'yellow')}"
            L.append(line)
            if r.get("launch"):
                L.append(f"      {C['cyan']}{r['launch']}{C['r']}")
            if r.get("note"):
                L.append(f"      {C['dim']}{r['note']}{C['r']}")
        L.append(f"  {C['dim']}{pdb['url']}{C['r']}")

    ac = res.get("areweanticheatyet")
    if ac:
        if ac["found"]:
            L.append(f"\n{C['b']}areweanticheatyet{C['r']}  "
                     f"{paint(ac['status'], STATUS_COLOR.get(ac['status'], 'dim'))}"
                     f"  {C['dim']}{', '.join(ac['anticheats']) or 'none listed'}"
                     f" · changed {ac['date_changed']}{C['r']}")
            for u in ac.get("recent_updates", []):
                L.append(f"  {C['dim']}· {u['what']} ({u['when']}){C['r']}")
            for n in ac.get("notes", []):
                L.append(f"  {C['dim']}· {n}{C['r']}")
            L.append(f"  {C['dim']}{ac['url']}{C['r']}")
        else:
            L.append(f"\n{C['b']}areweanticheatyet{C['r']}  {C['dim']}not listed; "
                     f"does not mean the game has no anti-cheat{C['r']}")

    cip = res.get("caniplayonlinux")
    if cip:
        head = f"\n{C['b']}caniplayonlinux{C['r']}"
        if cip.get("last_verified"):
            head += f"  {C['dim']}verified {cip['last_verified']}{C['r']}"
        L.append(head)
        if cip.get("summary"):
            L.append(f"  {cip['summary']}")
        L.append(f"  {C['dim']}{cip['url']}{C['r']}")

    return "\n".join(L)


def _attr(value) -> str:
    return str(value).replace("&", "&amp;").replace('"', "&quot;").replace("<", "&lt;")


def _tag(name: str, **attrs) -> str:
    parts = "".join(f' {k}="{_attr(v)}"' for k, v in attrs.items() if v not in (None, "", []))
    return f"<{name}{parts}>"


def render_xml(res: dict) -> str:
    """标签化输出。标量事实进属性，自由文本进内容。

    比纯文本多花约 10% token，换来的是块边界明确：玩家评论是自由文本，
    可能自带换行和任意标点，靠缩进和分隔符区分字段并不可靠。
    标签名用 ASCII——中文标签每个块要出现两次，CJK 分词成本更高。
    """
    L = [_tag("game_compat", game=res.get("game_name") or res["query"],
              appid=res.get("app_id"),
              native_linux="yes" if res.get("native_linux") else None)]

    failed = {k: v for k, v in res["sources"].items() if v != "ok"}
    for src, status in failed.items():
        L.append(f"{_tag(src, error='yes')}{re.sub(r'^error: \w+Error: ', '', status)}</{src}>")

    pdb = res.get("protondb")
    if pdb:
        warn = (pdb.get("match") or {}).get("warning")
        if warn:
            alts = ", ".join(pdb["match"].get("alternatives") or [])
            L.append(f"{_tag('warning')}{warn}"
                     + (f" other candidates: {alts}" if alts else "") + "</warning>")
        L.append(_tag("protondb", tier=pdb.get("tier"),
                      trending=pdb.get("trending") if pdb.get("trending") != pdb.get("tier") else None,
                      best=pdb.get("best") if pdb.get("best") != pdb.get("tier") else None,
                      reports=pdb.get("reports_total"), url=pdb.get("url")))
        if pdb.get("reports_fetch_error"):
            L.append(f"reports fetch failed: {pdb['reports_fetch_error']}")
        for r in pdb.get("reports", []):
            bits = [r["date"], r["rec"], r.get("proton") or "unknown"]
            if r.get("faults"):
                bits.append(f"[{','.join(r['faults'])}]")
            if r.get("launch"):
                bits.append(f"launch: {r['launch']}")
            if r.get("note"):
                # 评论原文可能含换行，压平成一行，块内一行一条报告。
                bits.append(re.sub(r"\s*\n\s*", " ", r["note"]))
            L.append(" | ".join(bits))
        L.append("</protondb>")

    ac = res.get("areweanticheatyet")
    if ac:
        if ac["found"]:
            L.append(_tag("areweanticheatyet", status=ac["status"],
                          anticheats=", ".join(ac["anticheats"]) or None,
                          changed=ac["date_changed"], url=ac.get("url")))
            for u in ac.get("recent_updates", []):
                L.append(f"{u['what']} ({u['when']})")
            for n in ac.get("notes", []):
                L.append(n)
            L.append("</areweanticheatyet>")
        else:
            L.append(f"{_tag('areweanticheatyet', status='not_listed')}"
                     f"Not listed does not mean the game has no anti-cheat."
                     f"</areweanticheatyet>")

    cip = res.get("caniplayonlinux")
    if cip:
        L.append(_tag("caniplayonlinux", verified=cip.get("last_verified"), url=cip.get("url")))
        if cip.get("summary"):
            L.append(cip["summary"])
        L.append("</caniplayonlinux>")

    L.append("</game_compat>")
    return "\n".join(L)


def _md(text) -> str:
    """把自由文本压成一行并中和行首结构字符。

    玩家评论和站点摘要都是不可控的用户生成内容，可能自带换行，也可能以
    `#` / `-` / `>` 开头。Markdown 只标记块的开始、不标记结束，这类内容会
    直接串出所在的列表项或小节，所以进 Markdown 前必须先钝化。
    """
    s = re.sub(r"\s*\n\s*", " ", str(text)).strip()
    return re.sub(r"^([#>\-*+|=]|\d+\.)", r"\\\1", s)


def render_md(res: dict) -> str:
    """Markdown。标题行比 XML 的开闭标签省，但只标记块的开始、不标记结束。"""
    L = [f"# {res.get('game_name') or res['query']}"]
    meta = []
    if res.get("app_id"):
        meta.append(f"appid {res['app_id']}")
    if res.get("native_linux"):
        meta.append("native Linux build")
    if meta:
        L.append(" · ".join(meta))

    for src, status in res["sources"].items():
        if status != "ok":
            L.append(f"\n## {src}\n{_md(re.sub(r'^error: \w+Error: ', '', status))}")

    pdb = res.get("protondb")
    if pdb:
        warn = (pdb.get("match") or {}).get("warning")
        if warn:
            L.append(f"\n> ⚠ {_md(warn)}")
        bits = [pdb.get("tier") or "?"]
        if pdb.get("trending") and pdb["trending"] != pdb.get("tier"):
            bits.append(f"trending {pdb['trending']}")
        if pdb.get("best") and pdb["best"] != pdb.get("tier"):
            bits.append(f"best {pdb['best']}")
        bits.append(f"{pdb.get('reports_total')} reports")
        L.append(f"\n## protondb — {' · '.join(bits)}\n{pdb['url']}")
        if pdb.get("reports_fetch_error"):
            L.append(f"reports fetch failed: {pdb['reports_fetch_error']}")
        if pdb.get("reports"):
            L.append("\n### reports")
            for r in pdb["reports"]:
                seg = [r["date"], r["rec"], r.get("proton") or "unknown"]
                if r.get("faults"):
                    seg.append(f"[{','.join(r['faults'])}]")
                # 日期/结论/Proton 版本走标题行，启动参数和评论正文各自缩进续行：
                # 正文长度不可控，挤在一行读不动。两空格缩进仍属同一列表项。
                L.append(f"- {' '.join(seg)}")
                if r.get("launch"):
                    L.append(f"  launch: {_md(r['launch'])}")
                if r.get("note"):
                    L.append(f"  {_md(r['note'])}")

    ac = res.get("areweanticheatyet")
    if ac:
        if ac["found"]:
            head = f"{ac['status']} · {', '.join(ac['anticheats']) or 'none listed'}"
            L.append(f"\n## areweanticheatyet — {head}\nchanged {ac['date_changed']} · {ac['url']}")
            for u in ac.get("recent_updates", []):
                L.append(f"- {_md(u['what'])} ({u['when']})")
            for n in ac.get("notes", []):
                L.append(f"- {_md(n)}")
        else:
            L.append("\n## areweanticheatyet — not listed\n"
                     "Not listed does not mean the game has no anti-cheat.")

    cip = res.get("caniplayonlinux")
    if cip:
        head = "\n## caniplayonlinux"
        if cip.get("last_verified"):
            head += f" — verified {cip['last_verified']}"
        L.append(head)
        if cip.get("summary"):
            L.append(_md(cip["summary"]))
        L.append(cip["url"])

    return "\n".join(L)


def main():
    ap = argparse.ArgumentParser(
        description="Linux 游戏兼容性三源查询（ProtonDB / caniplayonlinux / AreWeAntiCheatYet）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("用法:")[1] if "用法:" in __doc__ else None,
    )
    ap.add_argument("query", nargs="+", help="游戏名（支持中文）或 Steam AppID")
    ap.add_argument("-n", "--reports", type=int, default=8,
                    help="ProtonDB 评论条数，默认 8；-n 0 只要结论不要评论")
    ap.add_argument("-f", "--format", default="md",
                    choices=["md", "text", "xml"],
                    help="输出格式。md=Markdown（默认），text=终端彩色，xml=标签化")
    ap.add_argument("--json", action="store_true", help="输出原始 JSON（调试用）")
    ap.add_argument("--refresh", action="store_true", help="忽略缓存，强制重新拉取")
    ap.add_argument("-v", "--verbose", action="store_true",
                    help="额外打印耗时与缓存路径")
    args = ap.parse_args()

    query = " ".join(args.query)
    started = time.perf_counter()
    try:
        res = run(query, max(args.reports, 0), args.refresh)
    except KeyboardInterrupt:
        return 130
    elapsed = time.perf_counter() - started

    if args.json:
        print(json.dumps(res, ensure_ascii=False, indent=2))
    else:
        renderer = {"xml": render_xml, "md": render_md}.get(args.format, render)
        print(renderer(res))
        if args.verbose:
            print(f"\n{C['dim']}{elapsed:.2f}s  cache={CACHE_DIR}{C['r']}")

    return 0 if any(v == "ok" for v in res["sources"].values()) else 1


if __name__ == "__main__":
    sys.exit(main())
