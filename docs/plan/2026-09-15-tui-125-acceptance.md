# TUI 1、2、5 项修复验收

用户已验收光标与 footer 修复，并确认提问功能有效，要求补齐正文与面板之间
的空行后合并 main。本轮补充该间距并重新验证；独立提交保留，发布另行执行。
问题 3（todo 截断）与问题 4（QQ debug 延迟）不在这批实现内。

## 构建

- worktree：`/home/shorin/Documents/github/Miyu-tui-fixes-125-2026-09-15`
- 分支：`fix/tui-125-2026-09-15`
- 二进制：`target/debug/miyu`，dev profile，未优化，保留调试信息。
- 自报版本：`miyu 0.6.0`。
- SHA-256：`d4851122183f2d65ab1904e17de7a710530d54cb0a525347e28011037a15393d`。

## 启动

本次已准备独立 home，复制模型与显示等设置，未复制平台配置和会话数据。
配置位于被 Git 忽略的 target 下，仅本用户可读。测试仍使用配置中的真实模型，
模型调用由用户启动后发生。可在 herdr 或连到这台机器的 SSH 终端内执行：

```bash
MIYU_HOME=/home/shorin/Documents/github/Miyu-tui-fixes-125-2026-09-15/target/manual-tui-home \
MIYU_TUI=1 \
/home/shorin/Documents/github/Miyu-tui-fixes-125-2026-09-15/target/debug/miyu
```

需要测试直连事件路径时，在以上命令中另加 `MIYU_DIRECT=1`。直连核心与 daemon
不能同时占用同一个 home，切换前先用下方命令停止这个测试 home 的 daemon。

测试结束后可停止独立 daemon：

```bash
MIYU_HOME=/home/shorin/Documents/github/Miyu-tui-fixes-125-2026-09-15/target/manual-tui-home \
/home/shorin/Documents/github/Miyu-tui-fixes-125-2026-09-15/target/debug/miyu daemon stop
```

## 人工检查

1. **herdr 光标**：要求连续输出超过一屏的正文，期间输入、滚动、展开时间线。
   光标和输入法预编辑位置应留在输入框，不应间歇跳向其他区域。
2. **提问正文**：要求先输出 23 行带编号的文字，再调用 ask_question 提问。
   面板与正文之间保留一行空白，最新末段仍可见，PgUp/PgDn 能回翻全部正文；切题、缩放、提交或
   取消后布局正常。可重复使用一短一长两个问题。
3. **footer**：SSH 下把窗口缩到 48–80 列，要求启动 `sleep 30` 后台任务，
   同时让前台继续输出正文。footer 应始终一行，后台区不应出现闪烁的 token
   尾巴；扩大窗口后信息恢复。`stty size` 可记录触发时的实际行列数。

## 已完成的自动验证

- CLI 测试 279 项通过，含同步守卫、正文布局、footer 宽度与原有回归。
- question_tui 测试 20 项通过。
- 光标 PTY：daemon/direct 均为 0 次事务外正文写入，同步序列无不配对。
- 问答 PTY：daemon/direct 均通过初显、翻页、长短切题、24/40 行缩放和回答退出。
- 用户验收补充：面板顶部增加空行检查，修前报红；布局与滚动共同为分隔行留空间。
  补充改动的 3 项布局测试及 daemon/direct PTY 检查均通过，包含切题与缩放后的空行。
- footer PTY：48 列中原来的 56 列输出消失，99 次全宽 footer 输出均为 48 列；
  后台任务仍正常刷新，溢出的 token 尾巴消失。
- 三项均已证明基线二进制会失败，修复后二进制通过。
- `cargo fmt --check`、`git diff --check` 和依赖方向检查通过。
- 文件规模检查有既有失败：`src/render/tests/timeline.rs` 超过脚本基线。
  已验证该文件与起点 `85f7dc29` 字节一致，本次未修改；未更新基线掩盖失败。

复跑协议与布局探针：

```bash
cd /home/shorin/Documents/github/Miyu-tui-fixes-125-2026-09-15
python3 testkit/tui/cursor_sync.py
python3 testkit/tui/cursor_sync.py --direct
python3 testkit/tui/question_body.py --binary "$PWD/target/debug/miyu"
python3 testkit/tui/question_body.py --binary "$PWD/target/debug/miyu" --direct
```

每个探针自行建立独立 MIYU_HOME 与本地桩模型。用户已授权在空行调整验证通过
后合并 main；不自动发布或替换生产 daemon。
