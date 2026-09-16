# TUI 通知与会话列表首轮验收记录

本批基于 main `661f9082`，保留此前已验收的光标、提问布局和 footer 修复。
通知已通过用户验收，以 `6a6dd483` 独立提交，经 `f08bc80f` 合并 main。
会话列表首轮布局未通过验收：用户要求空会话保留大厅、列表放在模式提示下方，
非空会话参照提问面板预留正文空间。以下保留首轮记录，新版流程见
`docs/plan/2026-09-15-tui-followup-acceptance.md`。本轮未发布。

## 修复范围

### 回复期间通知不消失

根因：通知有 2200ms 的存活期限，但清理仅在空闲输入循环执行。流式回复
期间不进入该循环，过期通知仍被每帧绘制，直到回复结束才消失。

方案：在共享 `Screen::paint` 入口检查过期通知，沿用已有清理和画面失效逻辑。
空闲定时清理仍保留，保证没有新内容时通知也会消失。通知时长不变。

### 空会话 `/session` 布局覆盖

根因：会话选择器直接向 stdout 输出，却没有先暂停原来的活动区。
菜单从输入框光标处起画，旧大厅及输入框边框留在屏上；已有正文时菜单被挤到屏底。

方案：打开选择器前暂停活动区，将光标交还正文末尾；选择器返回后恢复原画布。
取消、选择以及输入错误都经过恢复路径。删除后重新打开列表沿用同一路径。
空正文让屏从第 0 行开始，不把初始光标算成一行正文，避免残留第一行星空。

## 手动验收

独立 worktree：`/home/shorin/Documents/github/Miyu-tui-quick-2026-09-15`。
测试 home 仅沿用上一批测试的模型与显示配置，不复制会话或平台数据。
debug 构建保留调试信息、不启用优化，自报版本 `miyu 0.6.0`。
二进制 SHA-256：`cb4ca289531721f9f837eef8338fa9548dfb0bedc8bffd703b1021835dcbbbb2`。

```bash
MIYU_HOME=/home/shorin/Documents/github/Miyu-tui-quick-2026-09-15/target/manual-tui-home \
MIYU_TUI=1 \
/home/shorin/Documents/github/Miyu-tui-quick-2026-09-15/target/debug/miyu
```

1. 空会话输入 `/session`：列表完整可见，不与大厅或输入框叠在一起。
   按 Esc 后大厅和输入框恢复。
2. 完成一轮对话后再输入 `/session`：列表接在正文末尾。分别测试 Esc 取消和
   Enter 选择当前会话，正文和输入框都应恢复，随后能正常发送消息。
3. 在没有运行中的任务、草稿为空时按 Ctrl+C，看到“要退出请按 Ctrl+D”通知。
   立即发送一条需要较长回复的消息，通知应约两秒后消失，不必等 AI 回复结束。
   空闲时同一通知也应自行消失。可先写好提示词再用终端粘贴，缩短操作间隔。
4. 在 herdr 和 SSH 终端各试一次，留意上一批修复的输入光标与 footer 是否仍正常。

## 自动回归

所有黑盒探针使用临时 MIYU_HOME、本地桩模型及真 PTY，不访问生产 daemon。

- `cargo test --lib cli::tests`：241 项通过。
- 通知修前：daemon 与 direct 均报 `notification remained throughout streaming`。
  记录分别位于 `/tmp/miyu-quick-toast-before-daemon.log`、
  `/tmp/miyu-quick-toast-before-direct.log`。
- 会话列表修前：报 `Session picker left the lobby logo behind`。
  记录位于 `/tmp/miyu-session-picker-before.log`。
- 空正文边界补充：回归检查在补齐前报 `Session picker left lobby pixels above the menu`，
  记录位于 `/tmp/miyu-quick-session-empty-before.log`。
- 通知修后：daemon/direct PTY 均通过，通知消失后正文仍继续增长。
  日志：`/tmp/miyu-quick-toast-after-daemon.log`、`/tmp/miyu-quick-toast-after-direct.log`。
- 会话列表修后：大厅完整让屏、正文后定位、取消恢复、选择恢复均通过。
  日志：`/tmp/miyu-quick-session-after.log`。
- 共享让屏边界调整后，提问面板的正文、空行、滚动、切题、缩放、回答退出回归通过。
  日志：`/tmp/miyu-quick-question-after.log`。
- `cargo fmt --check`、`git diff --check` 通过。

```bash
cd /home/shorin/Documents/github/Miyu-tui-quick-2026-09-15
python3 testkit/tui/toast_expiry.py --binary "$PWD/target/debug/miyu"
python3 testkit/tui/toast_expiry.py --binary "$PWD/target/debug/miyu" --direct
python3 testkit/tui/session_picker.py --binary "$PWD/target/debug/miyu"
```

## 未纳入本批

- todo 表格截断：截图确认有问题，但现有窄屏、长表、多批次及展开回翻探针未稳定
  复现。旧 JSON 回放存在输出顺序差异，尚不能证明就是截断根因。
- QQ debug 回复慢：缺少能定位具体耗时阶段的对照数据，未作猜测性优化。

通知的 next release note 已记录。会话列表新版及后续新增修复已于 2026-09-16 全部通过用户验收，按问题独立提交。
