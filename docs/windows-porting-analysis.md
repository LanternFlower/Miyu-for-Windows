 # Miyu 项目 Windows 移植分析报告

 ## 一、项目概况

 Miyu 是一个基于 Rust 的命令行 AI 助手，单 crate、单二进制，约 17.9 万行代码。当前主要运行于 Arch Linux，对 macOS 也有部分支持。技术栈包括 tokio 异步运行时、SQLite（rusqlite bundled 特性）及大量第三方 crate。

 本文档分析将 Miyu 移植到 Windows 所需的全部改动，按优先级排列。

---

 ## 二、各模块改动分析

 ### 1. IPC 通信层（ipc.rs, web.rs, cli.rs）

 **现状**：守护进程与 CLI 客户端之间通过 Unix Domain Socket（`core.sock`）通信，使用 `tokio::net::UnixStream` 和 `tokio::net::UnixListener`。

 **问题**：Windows 不支持传统的 Unix Domain Socket（Win10 1803+ 支持 AF_UNIX，但 tokio 的 UnixStream/UnixListener 在 Windows 上未实现）。

 **方案**：
 - 方案 A：替换为 Named Pipe（`tokio::net::windows::named_pipe`），性能好、原生支持
 - 方案 B：替换为 TCP loopback（`127.0.0.1`），实现简单但需管理端口
 - `DaemonProcessIdentity` 结构体中的 Linux `start_time` 字段需条件编译或替换为 Windows 等价实现

 **工作量**：大。这是核心通信机制，影响 daemon、CLI、web 三个模块。

---

 ### 2. 进程管理（tools/jobs.rs, tools/default_tools.rs, tools/mcp.rs）

 **现状**：大量使用 Unix 进程控制 API：
 - `libc::killpg()` / `libc::kill()` 发送 SIGTERM、SIGKILL
 - `process_group(0)` 创建独立进程组
 - `libc::kill(pid, 0)` 检测进程存活

 **问题**：Windows 无 Unix 信号机制，无进程组概念。

 **方案**：
 - 进程终止：使用 `std::process::Command::kill()` 或 `TerminateProcess`
 - 进程组替代：使用 Windows Job Objects 管理子进程树
 - 进程存活检测：使用 `OpenProcess` + `WaitForSingleObject` 或 `tasklist` 查询

 **工作量**：大。涉及后台任务管理和 MCP 子进程生命周期。

---

 ### 3. 文件权限控制（多个文件）

 **现状**：在以下位置使用 Unix 文件权限：
 - `shell/mod.rs`、`state/mod.rs`、`tools/artifact.rs`、`tools/apply_patch.rs`
 - `tools/scripts.rs`、`logging.rs`、`transfer/export.rs`、`skills.rs`、`llm/cache_log.rs`
 - 使用 `PermissionsExt`、`Permissions::from_mode(0o600/0o700)` 设置文件权限

 **问题**：Windows 使用 ACL 而非 Unix 权限位，`from_mode` 在 Windows 上编译失败。

 **方案**：
 - 使用 `#[cfg(unix)]` / `#[cfg(windows)]` 条件编译
 - Windows 上大部分场景可使用 no-op（默认 ACL 已足够安全）
 - 特殊场景（如敏感文件）可使用 `windows` crate 的 ACL API

 **工作量**：中。改动点分散但模式统一，可批量处理。

---

 ### 4. 文件锁（skills.rs, llm/openai_compatible.rs, paths.rs）

 **现状**：使用 `libc::flock()` 实现劝告式文件锁，用于迁移锁、技能安装锁、LLM 缓存锁。

 **问题**：Windows 无 `flock`。

 **方案**：
 - 引入 `fs2` 或 `fs3` crate 实现跨平台文件锁（`lock_exclusive` / `unlock`）
 - 或直接使用 Windows `LockFileEx` / `UnlockFileEx`
 - 推荐使用 `fs2`，改动最小且已有成熟的跨平台抽象

 **工作量**：小。替换 API 调用即可。

---

 ### 5. Shell 集成（shell/mod.rs, shell/bash.rs, shell/zsh.rs, shell/fish.rs）

 **现状**：仅支持 bash、zsh、fish 三种 Unix shell，通过注入 `.bashrc` / `.zshrc` / `fish/conf.d/` 实现 hook。

 **问题**：Windows 无这些 shell，路径和机制完全不同。

 **方案**：
 - Windows 上禁用 shell hook 功能（条件编译排除）
 - 或提供 PowerShell profile 注入（优先级低）
 - `parent_pid()` 和 `process_name()` 读取 `/proc/{pid}/stat` 和 `/proc/{pid}/comm`，需替换为 Windows API

 **工作量**：中。可先以功能降级方式处理。

---

 ### 6. /proc 文件系统访问（tools/diagnostics.rs, shell/mod.rs）

 **现状**：读取 `/proc/{pid}/stat`、`/proc/{pid}/comm`、`/proc/{pid}/maps`、`/proc/{pid}/environ`、`/proc/{pid}/cmdline`、`/proc/net/unix`、`/proc/version` 等。

 **问题**：Windows 无 /proc 文件系统。

 **方案**：
 - 进程信息：使用 `windows` crate 的 `OpenProcess` + `QueryFullProcessImageName` 等 API
 - 进程参数：使用 WMI 查询（`Win32_Process`）
 - 内核版本：使用 `std::env::consts` + Windows 版本 API
 - Wayland/X11 检测：Windows 上不需要，直接返回 None

 **工作量**：中。改动点明确，可用 Windows API 逐一替换。

---

 ### 7. 主机信息检测（host_info.rs, tools/default_tools.rs）

 **现状**：读取 `/etc/os-release`、`/etc/arch-release`、`/etc/debian_version` 等文件获取系统信息，使用 `libc::uname()` 获取内核版本。

 **问题**：Windows 无这些文件。

 **方案**：
 - 使用 `std::env::consts`（`OS`、`ARCH`）获取基本信息
 - 使用 `sysinfo` crate 获取详细系统信息
 - 使用 Windows Registry 或 WMI 获取 OS 版本详情
 - 现有 `#[cfg(not(unix))]` stub 返回 None，可在此基础上扩展

 **工作量**：小。

---

 ### 8. 原子文件操作（skills.rs）

 **现状**：
 - Linux: `libc::SYS_renameat2` + `RENAME_NOREPLACE`
 - macOS: `libc::renameatx_np` + `RENAME_EXCL`
 - 其他平台：已有 `#[cfg]` 回退，使用非原子 rename

 **问题**：Windows 上使用非原子回退，丢失原子性保证。

 **方案**：
 - 短期：使用现有回退逻辑，可接受
 - 长期：使用 Windows `MoveFileEx` + `MOVEFILE_REPLACE_EXISTING` 的组合实现原子替换，或引入 `tempfile` + rename 模式

 **工作量**：小。现有回退已可用。

---

 ### 9. 信号处理（web.rs, cli.rs）

 **现状**：使用 `tokio::signal::unix::{signal, SignalKind}` 监听 SIGTERM、SIGHUP。

 **问题**：Windows 无 SIGTERM/SIGHUP。

 **方案**：已有 `#[cfg(unix)]` / `#[cfg(not(unix))]` 条件编译，Windows 上仅使用 `ctrl_c()`。已处理，无需额外工作。

 **工作量**：无。已通过条件编译解决。

---

 ### 10. PTY / 终端渲染（render/math.rs, kitty_image.rs）

 **现状**：使用 `libc::openpty()`、`libc::termios`、`libc::TIOCGWINSZ` 进行终端尺寸获取和 PTY 操作。

 **问题**：Windows 无传统 PTY。

 **方案**：
 - 终端尺寸：使用 `crossterm::terminal::size()`（已跨平台）
 - PTY 功能：使用 Windows ConPTY API，或在 Windows 上禁用 PTY 相关功能
 - Kitty 图像协议：需确认 Windows Terminal 的兼容性

 **工作量**：中。

---

 ### 11. 系统资源限制（platforms/plugins/renderer.rs）

 **现状**：使用 `libc::setrlimit(libc::RLIMIT_AS, ...)` 限制地址空间。

 **方案**：已有 `#[cfg(not(unix))]` no-op 回退。已处理，无需额外工作。

 **工作量**：无。已通过条件编译解决。

---

 ### 12. 文件路径与目录（多处硬编码）

 **现状**：硬编码 Linux 路径：
 - `/usr/share/miyu/scripts`、`/usr/share/miyu/fonts`、`/usr/share/miyu/memes`、`/usr/share/miyu/default-kb`
 - `/usr/share/fonts/noto-cjk`、`/usr/share/applications`
 - XDG 目录约定通过 `directories` crate（已支持 Windows）

 **方案**：
 - 使用 `directories` crate 的 `ProjectDirs` 获取跨平台路径
 - Windows 上资源目录：`C:\ProgramData\miyu` 或 `%LOCALAPPDATA%\miyu`
 - 使用 `#[cfg]` 条件编译处理路径差异

 **工作量**：中。需逐一排查硬编码路径。

---

 ### 13. Linux 专属工具（条件编译排除）

 **现状**：以下工具仅适用于 Linux：
 - `archlinux.rs`：AUR 查询、Arch 新闻、官方包查询
 - `caniplayonlinux_query.rs`、`protondb_query.rs`：Linux 游戏兼容性查询
 - `package_advisor.rs`：AUR 包审查/安装
 - `fcitx_wiki.rs`：Fcitx 输入法文档查询

 **方案**：
 - 使用 `#[cfg(target_os = "linux")]` 条件编译排除
 - 后续可添加 Windows 等价工具（winget/scoop 查询等），非必须

 **工作量**：小。添加条件编译属性即可。

---

 ### 14. 依赖 crate 兼容性

 大部分依赖 crate 已支持 Windows，需重点关注：

 | crate | Windows 支持 | 备注 |
 |-------|-------------|------|
 | `libc` | 支持 | API 子集不同，需逐项确认 |
 | `trash` | 支持 | 使用 Windows 回收站 |
 | `rustyline` | 支持 | 行编辑正常工作 |
 | `crossterm` | 支持 | 终端操作正常 |
 | `rodio` | 支持 | 音频播放正常 |
 | `if-addrs` | 支持 | 网络接口枚举正常 |
 | `vte` | 支持 | 终端解析正常 |
 | `rusqlite` (bundled) | 支持 | 使用 MSVC 编译 SQLite |
 | `tokio` (net) | 部分 | UnixStream/UnixListener 不支持 Windows，需用 named_pipe |

 **工作量**：小。仅 `tokio::net` 的 Unix socket 部分需替换。

---

 ### 15. 构建系统（build.rs）

 **现状**：当前 build.rs 无平台特定构建步骤，依赖均为交叉平台。

 **方案**：
 - 应可在 Windows MSVC 或 MinGW 工具链下直接编译
 - 需确认 `bundled` 特性的 SQLite 编译在 MSVC 下正常
 - CI 中添加 Windows 构建目标

 **工作量**：小。

---

 ## 三、优先级总览

 | 优先级 | 模块 | 问题描述 | 阻塞程度 | 工作量 |
 |--------|------|----------|----------|--------|
 | P0 | IPC 通信层 | Unix Domain Socket 无法在 Windows 编译 | 编译阻塞 | 大 |
 | P0 | 进程管理 | libc 信号/进程组 API 不可用 | 编译阻塞 | 大 |
 | P0 | 文件权限 | `PermissionsExt` / `from_mode` 不可用 | 编译阻塞 | 中 |
 | P1 | 文件锁 | `libc::flock` 不可用 | 编译阻塞 | 小 |
 | P1 | /proc 访问 | Linux 专属文件系统 | 运行时失败 | 中 |
 | P1 | Shell 集成 | Unix shell 不可用 | 功能缺失 | 中 |
 | P1 | 硬编码路径 | Linux 路径不存在 | 运行时失败 | 中 |
 | P1 | 终端渲染 | PTY/termios 不可用 | 编译阻塞 | 中 |
 | P2 | 主机信息 | `/etc/os-release` 等不存在 | 运行时失败 | 小 |
 | P2 | 原子文件操作 | 原子 rename 不可用 | 功能降级 | 小 |
 | P2 | Linux 专属工具 | AUR/Fcitx 等不适用 | 功能缺失 | 小 |
 | P3 | 信号处理 | 已有条件编译回退 | 已解决 | 无 |
 | P3 | 资源限制 | 已有条件编译回退 | 已解决 | 无 |
 | P3 | 构建系统 | 应可直接编译 | 待验证 | 小 |

 **P0 = 编译阻塞，必须解决才能在 Windows 上编译通过**
 **P1 = 运行时阻塞，编译通过但无法正常工作**
 **P2 = 功能降级，可运行但部分功能缺失**
 **P3 = 已解决或仅需验证**

---

 ## 四、建议实施路线

 ### 阶段一：基础可编译（预计 2-3 周）

 1. 引入 `fs2` crate 替换 `libc::flock`
 2. 所有 `PermissionsExt` / `from_mode` 调用添加 `#[cfg(unix)]` 条件编译
 3. IPC 层替换为 Named Pipe 或 TCP loopback
 4. 进程管理替换为 Windows API（Job Objects + TerminateProcess）
 5. PTY/termios 调用添加条件编译

 ### 阶段二：功能可用（预计 1-2 周）

 1. 硬编码路径替换为 `directories` crate 跨平台路径
 2. /proc 访问替换为 Windows API
 3. 主机信息检测实现 Windows 版本
 4. Shell 集成在 Windows 上禁用或实现 PowerShell 支持
 5. Linux 专属工具添加 `#[cfg(target_os = "linux")]`

 ### 阶段三：质量保证（预计 1 周）

 1. Windows 全功能测试
 2. CI 添加 Windows 构建目标
 3. 文档更新，注明平台支持情况

 **总估计工作量：4-6 周**，取决于对 Windows API 的熟悉程度。

---

 ## 五、现有有利条件

 1. 项目已有部分 `#[cfg(unix)]` / `#[cfg(not(unix))]` 条件编译，说明作者已考虑跨平台
 2. 核心依赖（tokio、rusqlite、crossterm 等）均支持 Windows
 3. `directories` crate 已处理 XDG/AppData 路径差异
 4. 单 crate 架构，无需处理 workspace 级别的平台差异
 5. 模块化程度高，各功能模块相对独立

---

 ## 六、结论

 Miyu 移植到 Windows 的主要障碍集中在 IPC 通信层和进程管理两个核心模块。文件权限和 /proc 访问虽分散但模式统一，可通过条件编译批量处理。项目已有的条件编译基础设施为移植工作提供了良好起点。建议按三个阶段逐步推进，优先确保编译通过，再逐步恢复功能完整性。
