# Windows 分支：上游同步的收尾待办（2026-09-21）

上游 v0.6.1 同步已完成并通过实机验收（合并提交 `e2f7eb7d`，落 main 后为 `33993946`，
之后又补了复核发现的三处修复）。本文件记录**复核时发现、按判断本轮未做**的三条，
以及这次用到的复核方法——下次同步可以照着重跑。

## 本轮已修（备查）

| 位置 | 问题 |
|---|---|
| `crates/miyu-engine/src/tools/jobs/ledger.rs` | 上游 v0.6.1 在 `signal_process_group` 新增的 `pid == 0` 守卫，在换平台垫片时被漏掉；`jobs/mod.rs` 那条宣称守卫存在的注释随之失真 |
| `crates/miyu-base/src/config/mod.rs` 等 5 处 | 上游新写的 `directories::BaseDirs` 直调绕过 fork 的 `PlatformDirs` 垫片 |
| `crates/miyu-base/src/shell/{mod,powershell}.rs` | PowerShell hook 未纳入上游「启动时自动刷新已装 hook」机制（且未盖章） |

## 待办

### 1. Windows 的挂断判定缺「控制终端兜底」（需真机复现定性）

- 位置：`src/cli/terminal_guard.rs:73-75`——`#[cfg(windows)]` 分支只有裸的
  `miyu_base::sys::stdin_hung_up()`。
- 对照：Unix 侧同文件 79-98 的 `hangup_watch_fd` / `controlling_tty_fd` 专门避免
  「stdin 是管道 → 误判挂断」，注释记录的就是 09-10 那起 shellhook 误杀案。
- 机制：Windows 上 stdin 为管道且写端关闭时，`PeekNamedPipe` 返回
  `ERROR_BROKEN_PIPE` → `StdinWait::HungUp`（`crates/miyu-base/src/sys.rs:462-478`）。
- 触发面：**管道 stdin + 任意会拉起挂断看门狗的全屏 UI**——引导、设置界面、提问面板、
  行内选择器、REPL 输入/尾部（`spawn_hangup_watchdog` 的 6 个调用点）。
  一次性回合入口**不**拉看门狗（`src/cli/entry_flows.rs:11` 在 `run_oobe_flow` 里），
  所以不是「一进 shellhook 就 5 秒死」。
- 定性：**非本次合并引入**（fork 原来的 `crate::sys::stdin_hung_up()` 行为一致）。
- 下一步：真机复现（Windows 上 `$text | miyu --shell-intercept --stdin`，回合中触发
  提问面板）。确认后再决定：给 Windows 加等价的 tty 判定，还是把判据收紧成
  「只在交互式终端上判挂断」。

### 2. 上游新 TUI 层的 `/tmp/...` 诊断 trace 路径（仅诊断，低危）

- 位置：`MIYU_SCREEN_TRACE` / `MIYU_LOBBY_TRACE` 的落盘路径写死 `/tmp/...`，约 10 处：
  `src/cli/repl/tail/frame.rs:267,479`、`tail/screen/{draw.rs:58,100,192, mod.rs:221,
  select.rs:267, tail_impl.rs:55,358}`、`tail/update.rs:30`。
- 后果：Windows 上写到 `C:\tmp\...`，目录通常不存在 → 诊断日志静默失效，
  **功能不受影响**。
- 修法：统一走 `crate::platform_dirs::PlatformDirs`（fork 已把继承自旧树的
  `tail/mod.rs:81` 改成这样）。

### 3. Windows 上的「当前 shell」探测（让 hook 装完的提示能生效）

- 位置：`crates/miyu-base/src/shell/mod.rs:234-253`（`current_parent_shell` →
  `parent_pid` / `process_name` 读 `/proc`）。
- 后果：Windows 上恒返回 `None`，于是 `print_reload_hint`（同文件 198-215）永远走通用
  提示，「在当前终端运行此命令可立即加载」那句不会出现。**在 `is_known_shell` 里加
  `"powershell"` 改变不了这一点**（本轮因此没有加）。
- 修法：在 `miyu_base::sys` 里加 Windows 版父进程 / 进程名查询
  （`CreateToolhelp32Snapshot` 或 `NtQueryInformationProcess`），让 `parent_pid` /
  `process_name` 走垫片；随后再把 `"powershell"` 补进 `is_known_shell`。

## 复核方法（下次同步复用）

三把尺子，都是纯 git、可脚本化：

1. **fork 的行有没有活下来**：对 fork 自己改过的每个文件
   （`git diff --name-only v0.5.0 <fork 合并提交>`），把它新增的实质代码行逐行在
   工作区文件里做 `Contains` 检查；缺失的行按「路径改写 / 模块搬迁 / 上游重写 /
   主动删除」四类归因。注意排除注释与 `use` 行，并给「目录模块」
   （`x.rs` → `x/mod.rs`）留 basename 兜底映射。
2. **上游的行有没有被吞掉**：`git diff origin/main:<新路径> <合并结果>:<新路径>` 的
   `-` 行就是合并结果里没有的上游行。这类里藏着真回归——**上游新增的守卫/不变量被
   fork 版整段覆盖**（本次的 `pid == 0` 守卫就是这么找出来的）。
3. **每个文件活下来的 fork 适配**：第 2 条那把 diff 的全量内容，等于该文件里
   「fork 特化」的全部；diff 为空就说明这个文件已经与上游完全一致，不必再读。

经验：编译器抓不到的是两类——**两套实现缝在一起**（fork 的垫片与上游的 Unix 直调
交错）和**上游新增的不变量被覆盖**。本次同步的三处真实故障（文件锁、进程终止、
TUI 松键过滤）属于前者的可见部分，`pid == 0` 守卫属于后者。写完补丁记得按平台走一遍
「这个 API 在 Windows 上是什么」——`cfg(windows)` 分支不可达往往就是这么来的。
