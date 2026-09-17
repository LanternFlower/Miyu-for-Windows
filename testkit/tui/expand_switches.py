#!/usr/bin/env python3
"""三个显示开关的真机走查（09-17 用户拍板的那一版语义）。

设置里只剩三位布尔，互不相干：

- `展开思考内容` / `展开工具内容`：开着的话那一步**出来就是展开的**，不用点；
  再点一次收回去。关着就只有抬头。
- `过程收起成 Worked for`：只管收不收段。关掉的话每一步就地留着，**照样点得开**。

单元测试钉的是字节（`miyu-block-open=` 那个标记）和视图映射；这份钉的是**人眼
看到的那一屏**——标记对了但 `paint` 没去开、或者开了又被下一帧顶回去，字节那层
一个都照不出来。

    cargo build
    python3 testkit/tui/expand_switches.py
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

# 第一段思考的正文（桩模型写死的那句，见 `repl-smoke/stub_llm.py`）。
# 它只在**展开之后**才看得到——抬头上只有「已思考 · N 词元 · 1.2s」。
THINK_BODY = "先想一句"
THOUGHT_HEAD = "已思考"
FOLD_HEAD = "Worked for"

STUB = {
    "STUB_REASONING": "1",
    "STUB_TOOL": "1",
    "STUB_TOOL_COMMAND": "printf 'out-%s\\n' one",
    "STUB_CHUNK_SLEEP": "0.02",
}


def display(**flags):
    return {"display": flags}


def row_of(screen, needle):
    for index, line in enumerate(screen):
        if needle in line:
            return index
    return None


def ask(master, sink):
    os.write(master, h.PROMPT.encode())
    h.drain_until(master, sink, h.PROMPT, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=1.2, timeout=40)
    return h.render(bytes(sink))


def screen_after(master, sink, quiet=0.6, timeout=8):
    h.settle(master, sink, quiet=quiet, timeout=timeout)
    return h.render(bytes(sink))


def scenario_expanded(report):
    """两个「展开」开关都开着：那一步出来就是展开的，点一下收回去。

    这一档要和「收起成 Worked for」分开看——那是另一位开关。这里把它关掉，
    好让那几步留在屏上，断言才不用去数收缩行里面。
    """
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(
            expand_reasoning=True, expand_tool_calls=True, fold_timeline=False
        ),
    )
    try:
        screen = ask(master, sink)
        r.save("expand-open", screen)
        # 一次都没点，思考正文就该在屏上。
        report["expanded_body_visible_without_a_click"] = any(
            THINK_BODY in line for line in screen
        )
        head = row_of(screen, THOUGHT_HEAD)
        report["expanded_head_present"] = head is not None
        if head is None:
            return
        # 再点一次收回去——它是把手，不是一截死预览。
        h.click(master, sink, 5, head)
        screen = screen_after(master, sink)
        r.save("expand-collapsed-again", screen)
        report["clicking_the_head_collapses_it"] = not any(
            THINK_BODY in line for line in screen
        )
        # 收起来之后不许被下一帧顶开（活动区每 tick 重写同样的标记）。
        screen = screen_after(master, sink, quiet=1.0)
        report["stays_collapsed_across_frames"] = not any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_expanded_survives_the_fold(report):
    """收成 `Worked for …` 之后再点开：里面那几步**还是**展开态。

    收缩行里的那份原来是另写的一套拼行，不走 `step_rows`——这一位（以及命令那
    一步抬头底下露着的几行）一收段就没了。
    """
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(expand_reasoning=True),
    )
    try:
        screen = ask(master, sink)
        fold = row_of(screen, FOLD_HEAD)
        report["folded_even_with_expand_on"] = fold is not None
        if fold is None:
            return
        # 段是收着的，所以这会儿屏上不该有思考正文。
        report["fold_hides_the_expanded_body"] = not any(
            THINK_BODY in line for line in screen
        )
        h.click(master, sink, 3, fold)
        screen = screen_after(master, sink)
        r.save("expand-fold-opened", screen)
        # 点开收缩行**就够了**：里面那一步不用再点。
        report["step_inside_the_fold_is_still_open"] = any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_collapsed(report):
    """出厂档位：那一步出来是合着的，点开才看得到。"""
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(expand_reasoning=False, expand_tool_calls=False),
    )
    try:
        screen = ask(master, sink)
        r.save("expand-default", screen)
        report["collapsed_body_hidden_by_default"] = not any(
            THINK_BODY in line for line in screen
        )
        # 收成 `Worked for …` 了，得先点开它才看得到那几步。
        fold = row_of(screen, "Worked for")
        report["default_folds_the_segment"] = fold is not None
        if fold is None:
            return
        h.click(master, sink, 3, fold)
        screen = screen_after(master, sink)
        head = row_of(screen, THOUGHT_HEAD)
        if head is None:
            report["collapsed_step_expands_on_click"] = False
            return
        h.click(master, sink, 5, head)
        screen = screen_after(master, sink)
        r.save("expand-default-clicked", screen)
        report["collapsed_step_expands_on_click"] = any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_no_fold(report):
    """关掉「过程收起成 Worked for」：不收段，但每一步**照样点得开**。

    09-17 之前这一档顺手把那些步变成点不开、正文铺一地——用户原话：「即使不自动
    收起过程为 true，也不应该以 tag 行下预览的形式出现 tag 行的内容」。
    """
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(fold_timeline=False),
    )
    try:
        screen = ask(master, sink)
        r.save("expand-nofold", screen)
        report["no_fold_line"] = not any("Worked for" in line for line in screen)
        report["no_fold_body_not_spilled"] = not any(
            THINK_BODY in line for line in screen
        )
        head = row_of(screen, THOUGHT_HEAD)
        report["no_fold_step_present"] = head is not None
        if head is None:
            return
        h.click(master, sink, 5, head)
        screen = screen_after(master, sink)
        r.save("expand-nofold-clicked", screen)
        report["no_fold_step_still_clickable"] = any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def main():
    report = {}
    for scenario in (
        scenario_expanded,
        scenario_expanded_survives_the_fold,
        scenario_collapsed,
        scenario_no_fold,
    ):
        try:
            scenario(report)
        except Exception as error:  # noqa: BLE001 - 走查脚本，报出来就行
            report[f"{scenario.__name__}_crashed"] = False
            print(f"{scenario.__name__} 炸了: {error}", file=sys.stderr)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [key for key, value in report.items() if value is not True]
    print(f"\n{len(report) - len(bad)}/{len(report)} 通过")
    if bad:
        print("红:", bad)
    print(f"产物：{h.OUT}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
