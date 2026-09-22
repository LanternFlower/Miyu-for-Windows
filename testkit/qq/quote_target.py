#!/usr/bin/env python3
"""自动引用：隔了几条别人的消息，她回话时就该带上引用。

用户 09-19 报「`和原消息间隔几条消息则引用：2` 没生效」，举的例子是群友 @ 她之
后隔了十条消息，她的回复只带了 @ 没有引用。这个走查把那个场景在**真 daemon +
假 NapCat**（假账号、假群、假群友，与真 QQ 彻底隔离）上原样跑一遍，直接看发到
OneBot 的那份 payload 里有没有 `{"type":"reply"}` 段。

两个场景：
  A. 触发之后塞几条别人的消息 → 该带引用，且引用的是触发那条
  B. 中间一条都不隔（她秒回） → 不该带引用（阈值 2）

跑法（要先有跑着的 daemon，会花真模型额度，两轮）：

    python3 testkit/qq/quote_target.py

产物在 ~/.cache/miyu-qq-quote/。
"""

import json
import os
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fake-onebot"))
import run as ob  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-qq-quote"))
# 她想多久算多久：真模型 + 真判官，慢的时候半分钟。
REPLY_TIMEOUT = 120.0
# 插话之间隔多久。太快的话她那一轮可能还没开始，位置就对不上了。
FILLER_GAP = 1.5


def quoted_id(message):
    """这条 payload 引用了谁。None = 没有 reply 段。"""
    for segment in message or []:
        if segment.get("type") == "reply":
            return str(segment.get("data", {}).get("id"))
    return None


def quoted_seq(message):
    """引用段里带没带消息序号。NapCat 优先用它，而它不依赖对端的短号表。"""
    for segment in message or []:
        if segment.get("type") == "reply":
            return segment.get("data", {}).get("seq")
    return None


def mentioned_ids(message):
    return [
        str(segment.get("data", {}).get("qq"))
        for segment in message or []
        if segment.get("type") == "at"
    ]


def wait_for_reply(before, timeout=REPLY_TIMEOUT):
    waited = 0.0
    while waited < timeout and len(ob.REPLIES) == before:
        time.sleep(0.5)
        waited += 0.5
    # 她可能分几条发，给后续几条一点时间
    time.sleep(2.0)
    return ob.REPLIES[before:]


def scenario(ws, title, text, fillers):
    print(f"\n=== {title} ===")
    before = len(ob.REPLIES)
    trigger = ob.group_msg(ws, text, at_self=True)
    print(f"  → @Miyu {text}   (message_id={trigger})")
    for index in range(fillers):
        time.sleep(FILLER_GAP)
        if len(ob.REPLIES) > before:
            # 她已经答完了，再塞也不算「中间隔的」
            print(f"  ! 她在第 {index} 条插话前就答完了")
            break
        ob.group_msg(
            ws,
            f"（插话 {index + 1}）",
            sender=ob.OTHER,
            name="另一个群友",
        )
        print(f"  → 别人插话 {index + 1}")
    replies = wait_for_reply(before)
    for reply in replies:
        print(f"  ← {ob.render(reply)}")
    return {
        "trigger_message_id": str(trigger),
        "fillers": fillers,
        "replies": replies,
        "first_quoted": quoted_id(replies[0]) if replies else None,
        "first_mentions": mentioned_ids(replies[0]) if replies else [],
    }


def busy_scenario(ws, keep=REPLY_TIMEOUT):
    """她正忙着回 A 的时候，B 来 @ 她；之后再塞几条别人的。

    用户 09-19 举的就是这个形状：群友 @ 她那一刻她正在回另一个人，隔了十条
    之后才轮到他。
    """
    print("\n=== 忙的时候被 @（她正在回别人）===")
    before = len(ob.REPLIES)
    first = ob.group_msg(ws, "三加三等于几，只说数字", at_self=True)
    print(f"  → A @Miyu 三加三…   (message_id={first})")
    time.sleep(1.0)
    second = ob.group_msg(
        ws, "四加四等于几，只说数字", at_self=True,
        sender=ob.OTHER, name="另一个群友",
    )
    print(f"  → B @Miyu 四加四…   (message_id={second})")
    for index in range(4):
        time.sleep(FILLER_GAP)
        ob.group_msg(ws, f"（插话 {index + 1}）", sender=800000077, name="第三个人")
        print(f"  → 第三个人插话 {index + 1}")
    # 两个人的答复可能分两轮来，多等一会
    replies = wait_for_reply(before, keep)
    waited = 0.0
    while waited < 40 and len(replies) < 2:
        time.sleep(1.0); waited += 1.0
        replies = ob.REPLIES[before:]
    quoted = []
    for reply in replies:
        print(f"  ← {ob.render(reply)}")
        quoted.append(quoted_id(reply))
    return {
        "first_message_id": str(first),
        "second_message_id": str(second),
        "quoted": quoted,
        "replies": replies,
    }


def dropped_scenario(ws):
    """对端把引用段悄悄扔了——Miyu 这边事后核对该抓得到。

    NapCat 真的会这么干：引用段里的短号在它进程内那张表里查不到时，它 `return`
    掉这一段，消息照发、也照样回一个"成功"。这里让假 NapCat 演一遍，看那条
    告警会不会出现在日志里。
    """
    print("\n=== 对端把引用段扔掉（演 NapCat 的静默丢弃）===")
    ob.DROP_QUOTE = "1"
    try:
        before_log = log_size()
        before = len(ob.REPLIES)
        trigger = ob.group_msg(ws, "五加五等于几，只说数字", at_self=True)
        print(f"  → @Miyu 五加五…   (message_id={trigger})")
        for index in range(3):
            time.sleep(FILLER_GAP)
            ob.group_msg(ws, f"（插话 {index + 1}）", sender=ob.OTHER, name="另一个群友")
        replies = wait_for_reply(before)
        for reply in replies:
            print(f"  ← {ob.render(reply)}")
        # 事后核对是后台跑的，等它一会
        deadline = time.time() + 20
        while time.time() < deadline:
            if "引用段丢掉了" in new_log(before_log):
                break
            time.sleep(1.0)
        return "引用段丢掉了" in new_log(before_log)
    finally:
        ob.DROP_QUOTE = None


LOG = Path.home() / ".miyu" / "cache" / "logs" / f"miyu.{time.strftime('%Y-%m-%d')}.log"


def log_size():
    return LOG.stat().st_size if LOG.exists() else 0


def new_log(since):
    if not LOG.exists():
        return ""
    with LOG.open(encoding="utf-8", errors="replace") as handle:
        handle.seek(since)
        return handle.read()


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    ws = ob.WS.connect(ob.access_token())
    print(f"已连接 Miyu (self_id={ob.SELF_ID}, group={ob.GROUP_ID})")
    threading.Thread(target=ob.pump, args=(ws,), daemon=True).start()
    ws.send({
        "post_type": "meta_event", "meta_event_type": "lifecycle",
        "sub_type": "connect", "self_id": ob.SELF_ID, "time": int(time.time()),
    })
    time.sleep(1)

    report = {}
    # B 放前面：她还没发过言，`last_message_is_own` 一定是假的，
    # 「连发时强制引用」那条兜底不会把这一格染绿。
    near = scenario(ws, "紧挨着回（中间零条）", "一加一等于几，只说数字", fillers=0)
    time.sleep(3)
    far = scenario(ws, "隔了四条别人的消息", "二加二等于几，只说数字", fillers=4)

    time.sleep(4)
    busy = busy_scenario(ws)

    # 她没回话时不能判红：假群不在白名单，连着跑几轮就会被限流，那时候
    # 一个字都不会有——把"没回复"算成"没引用"，红的就是测具自己（09-18 那次
    # "量宽度用字符数"同款：量错了东西）。
    def checked(case, name, verdict):
        if not case["replies"]:
            report[name] = None
            return
        report[name] = verdict()

    report["紧挨着回时不引用"] = near["first_quoted"] is None if near["replies"] else None
    checked(far, "隔了几条就引用", lambda: far["first_quoted"] is not None)
    checked(far, "引用的是触发那条", lambda: far["first_quoted"] == far["trigger_message_id"])
    checked(far, "引用带上了消息序号", lambda: quoted_seq(far["replies"][0]) is not None)
    # 忙的时候被 @：后来那条也该带引用（中间隔了四条别人的）
    report["忙的时候被@也引用"] = (
        any(quote == busy["second_message_id"] for quote in busy["quoted"])
        if busy["replies"]
        else None
    )
    time.sleep(4)
    report["对端丢了引用能抓到"] = dropped_scenario(ws)
    report["_细节"] = {"near": {k: v for k, v in near.items() if k != "replies"},
                       "far": {k: v for k, v in far.items() if k != "replies"},
                       "busy": {k: v for k, v in busy.items() if k != "replies"}}

    (OUT / "report.json").write_text(
        json.dumps({"near": near, "far": far, "busy": busy, "report": report},
                   ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    passed = skipped = failed = 0
    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    for name, ok in checks.items():
        if ok is None:
            print(f"⏭️  {name}（她没回话，多半是限流；隔几分钟再跑）")
            skipped += 1
        else:
            print(f"{'✅' if ok else '❌'} {name}")
            passed += bool(ok)
            failed += not ok
    tail = f"，{skipped} 项没判成（她没回话）" if skipped else ""
    print(f"\n{passed}/{passed + failed} passed{tail}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
