# `/sandbox`：会话级 Landlock 沙盒（2026-09-13 计划）

## 零、需求与已拍板

用户视角：打开 Miyu，新会话，`/sandbox <路径>`，从此这个会话里她做的一切都关在这个目录里，
读不到、写不到别的目录。

09-13 对话里已拍板的：

| 项 | 裁定 |
|---|---|
| 默认 | 不套沙盒，和今天一样 |
| 命令 | 一条 `/sandbox`，**删掉 `/workspace`**（cwd 机制留下，由沙盒根驱动） |
| 尺度 | **读写都锁**，与成员同款；网络不管 |
| 开关 | 没有 on/off。会话只有两态：没绑 / 绑在某目录。换目录=重绑 |
| 缓存 | 沙盒属性进 `<host-environment>`，绑定/解绑各掰一次前缀缓存，明说；不做会掰缓存的每会话微调 |
| macOS | 不支持（Landlock 是 Linux 内核功能），绑定时直接拒绝 |

基线提交：`cdf820f9`。

## 一、现状取证

### 1. 成员沙盒机制（09-11 落地，全部复用）

| 部件 | 位置 | 说明 |
|---|---|---|
| 规则装载 | `src/tools/sandbox.rs` `confine / confine_std / confine_relay` | fork 后 exec 前给**子进程**装 Landlock，daemon 不受限；策略在回合的 tokio task-local 上 |
| 进程内守卫 | 同文件 `guard_read / guard_write` | read/edit/glob/grep/print_image/vision/artifact/memes 读写路径前过一遍，同一份策略 |
| 策略构造 | `src/web/sandbox_scope.rs::member_scope` | 只读：系统目录 + 脚本目录 + miyu 二进制 + 家里 documents/pictures/personas；可写：workspace、artifacts、/tmp、/dev/null、cache_dir、runtime_dir |
| 作用域化 | `src/web/actor/mod.rs`（Create、Redo 两处）、`src/web/session_cmds.rs`（工具桥） | 三处各自写一遍「member_scope → 否则 record.workspace → 否则 cwd」 |
| 后台继承 | `src/tools/subagent.rs::with_turn_scope`、`src/tools/jobs/mod.rs` | 子代理/后台 job 在还看得见的地方抓策略，进后台原样套回 |
| 中转线 | `src/llm/openai_compatible/cli_relay/process.rs` | CLI 整个进程关进去，额外放行 `~/.claude ~/.claude.json ~/.codex ~/.gemini`，HOME 不换 |
| 内核探测 | `sandbox::probe()`，`src/web/server.rs:183` 启动时记日志 | 没有 Landlock → 成员命令失败关闭 |

### 2. 管理员回合今天怎么定 cwd

`record.workspace`（会话表列，`/workspace` 设的）有效则用，否则客户端 cwd（REPL 随 Turn 带上），
否则 daemon cwd。不套沙盒。三个作用域化点各写一遍这段逻辑。

### 3. 环境块

`src/agent/prompt.rs::host_environment_for` 从 `config.accounts.home_dir` 推成员工作区，
`src/host_info.rs:187` 把 `writable="workspace, /tmp" readable="workspace, /tmp, system dirs (…)"`
**写死**。每轮瞬态尾巴 `<runtime now= cwd=>` 已经带 cwd，且不在缓存前缀里。

### 4. `/workspace` 全部触点

| 层 | 文件 |
|---|---|
| 命令表（REPL 与 WebUI 同一张） | `src/slash_commands.rs:115`（`web: false`） |
| REPL 处理 | `src/cli/repl/remote/interactive.rs:430-515`，`src/cli/repl/session.rs`（列表里 `[workspace]` 后缀） |
| CLI 子命令 | `src/cli/args.rs:323 Session::Workspace`，`src/cli/session_cmds.rs`，`src/cli/localize.rs:201,573,595` |
| IPC | `src/ipc/protocol.rs` `SetWorkspace`、`SessionInfo.workspace`；`src/web/session_cmds.rs:481` 处理 |
| 存储 | `sessions.workspace` 列（`rows.rs SESSION_COLUMNS`、`conversation_db/sessions.rs:501`、`state/sessions.rs:328`） |
| WebUI | `web/app.js:2466`（会话列表详情行显示路径）、`:3107,3129`（session.updated 同步） |
| 文档 | `docs/wiki/03-使用方式.md:44`、`04-命令参考.md:29,132`、`docs/architecture.md:161-163` |

## 二、设计

### 1. 命令

| 命令 | 行为 |
|---|---|
| `/sandbox <路径>` | 绑定：路径 canonicalize、必须是目录、内核有 Landlock、会话不归成员；写进会话表；发 `session.updated`。只影响之后的回合 |
| `/sandbox` | 查看：根路径、可写清单、可读清单（清单来自配置）；未绑定就说未绑定、当前用客户端目录 |
| `/sandbox clear` | 解绑 |

CLI：`miyu session sandbox <会话> [目录 | --clear]`，`--json` 直出。
IPC：`SetWorkspace` → `SetSandbox { target, root: Option<PathBuf> }`；校验在 daemon 侧做（probe 也在 daemon 侧，
因为规则是 daemon 装的）。

成员会话：`/sandbox` 显示「已开（固定在 home/<用户>/workspace）」，绑定/解绑一律拒绝。

### 2. 状态

- 会话表 `workspace` 列**原地复用**为沙盒根（列名不动，迁移 append-only、不改 schema）。
- 迁移 v36：`UPDATE sessions SET workspace = NULL` 一次性清老值。理由：老 `/workspace` 只是 cwd，
  升级后若沿用会把以前设过的会话暗中上锁。
- Rust 字段 / IPC / JSON 字段改名 `workspace` → `sandbox`（`SessionRecord`、`SessionInfo`、
  `SessionListEntry`、`session.updated` 载荷、`web/app.js` 三处）。`miyu session show --json` 契约随命令一起变。

### 3. 策略构造（管理员）

`sandbox_scope.rs` 改成一个入口 `session_scope(paths, admin_store, stores, config, session_id) -> Scope { workspace, policy: Option<Arc<SandboxPolicy>> }`：

- 会话归成员 → 现有 `member_scope`，一字不改。
- 管理员且 `record.sandbox` 有效 → `admin_policy(root)`。
- 否则 → 客户端 cwd / daemon cwd，`policy = None`。

三个作用域化点改成调这一处（顺手消掉三份重复）。

`admin_policy(root)`：

| 权限 | 内容 |
|---|---|
| 可写 | `root`、`/tmp`、`/dev/null`、`paths.cache_dir`（脚本缓存）、`paths.runtime_dir()`（IPC socket，MCP 桥要连）、`paths.artifacts_dir()`（管理员 artifact 库）、生图落盘目录、配置 `tools.sandbox.writable` |
| 只读 | 成员同一份系统目录常量（`/usr /bin /sbin /lib /lib64 /etc /proc /sys /dev /run /opt /var`）、`scripts_dir`、`system_scripts_dir`、`miyu_executable()`、配置 `tools.sandbox.readable` |
| 不给 | 家目录其余一切：`~/.ssh`、`~/.config`、`~/.miyu` 的配置与库、Documents/Pictures |

清单里不存在的路径先剔掉（Landlock 对打不开的授权根失败关闭，成员版踩过）。
`SandboxPolicy` 增加 `root: PathBuf` 字段，供环境块与 `/sandbox` 查看用。

### 4. HOME 与工具链直通（决策点 2）

锁读之后，`sh -lc` 登录 shell 若 HOME 还是真家，每条命令 stderr 都会先撞一句
`bash: ~/.bash_profile: Permission denied`，模型看着会犯糊涂；而放行 rc 文件又等于放行里面
`export XXX_KEY=` 这类东西。成员版的解法是 HOME 换成工作区，这里沿用：

- 子进程 HOME = 沙盒根（`SandboxPolicy.home`），登录 shell 读不到 `~/.profile` 就静默跳过，程序默认缓存落根下。
- 工具链靠显式环境变量指回真家，只对可写/可读清单里存在的目录设：
  `CARGO_HOME=~/.cargo`、`RUSTUP_HOME=~/.rustup`、`npm_config_cache=~/.npm`、
  `GIT_CONFIG_GLOBAL=~/.gitconfig`；PATH 头部补 `~/.cargo/bin:~/.local/bin`（存在才补）。
- 默认清单：可写 `~/.cargo ~/.npm`，可读 `~/.rustup ~/.local ~/.gitconfig`。
- 中转线不受影响：`confine_relay` 本来就不换 HOME（CLI 得按真家找 `~/.claude`）。

代价明说：沙盒会话里 `git push` 走 SSH 会失败（`~/.ssh` 不放行），要推就换个没绑沙盒的会话推，
或者用户自己把 `~/.ssh` 加进可读清单。`gh` 等把 token 放 `~/.config` 的工具同理。

### 5. 环境块

`host_environment_for` 不再从 `config.accounts.home_dir` 推，改读 `sandbox::current_sandbox()`
（`Agent::new_for_audience` 在 `run_turn_task_inner` 里，处在 `with_sandbox` 作用域内，读得到）：

```
<host-environment … sandbox="landlock" root="/path" writable="root, /tmp, ~/.cargo, ~/.npm" readable="system dirs, ~/.rustup, ~/.local, ~/.gitconfig"/>
```

- 属性由策略生成，`writable/readable` 只列清单项（路径以 `~` 缩写），系统目录写成 `system dirs`，
  字节随会话恒定（根 + 配置清单都不变）。
- 成员回合走同一条生成路径：成员的环境块字节变一次（计划内冷启动，AGENTS 1.6）。
- `src/agent/tests/prompt.rs` 补断言；`src/host_info.rs` 那串写死的字符串删掉。
- 每轮尾巴 `<runtime cwd=>` 不动。

### 6. 第三方 CLI

机制不变：`confine_relay` + 配置目录放行。管理员策略比成员宽（多了工具链清单），成员能跑的它都能跑。
两条要写进文档的代价：

- 重绑 = 换 cwd → claude 按 cwd 归档会话、codex 的 rollout 绑目录，下一轮续传可能找不到目标，
  两条线已有「忘掉映射整段重放」兜底，那一轮多花 token。
- CLI 的登录态目录仍放行读写，她在 CLI 的 Bash 里读得到，固有代价（架构文档已写）。

### 7. 删除清单（本计划一并动，用户已在 09-13 对话里确认删 `/workspace`）

`ReplSlashCommand::Workspace` 与命令表项、REPL 处理分支、`Session::Workspace` 子命令与三条本地化词条、
`IpcCommand::SetWorkspace`、REPL 会话列表的 `[workspace]` 后缀（改成 `[sandbox]`）、
`docs/wiki/03、04` 与 `architecture.md` 的提法。**留下**：会话表列、`with_workspace` 作用域、客户端 cwd 回退。

### 8. 配置与设置页

`config.tools.sandbox = { readable: Vec<String>, writable: Vec<String> }`，`~` 展开，默认值在
`config/defaults.rs`；`web/settings-schema.js` 「工具」段加两个列表字段（改配置需要 daemon 重载，
和别的工具项一样）。

### 9. 不做

- 每会话 `/sandbox allow <路径>`：会动环境块。
- on/off 开关：同上。
- 网络限制（Landlock ABI 4 有 TCP 钩子）：另案。
- macOS `sandbox-exec` 后端：另案。

## 三、决策点（09-13 已拍板）

| # | 问题 | 裁定 |
|---|---|---|
| 1 | WebUI 也开放 `/sandbox`？ | **开放**（用户：非管理员不需要，他们本来就在自己家的沙盒里） |
| 2 | 工具链直通做不做？ | **做**（用户：不然命令运行不了） |
| 3 | 老会话的 workspace 值 | 按推荐：迁移清空 |
| 4 | JSON/IPC 字段名 | 按推荐：改成 `sandbox`，DB 列名不动 |

原始选项留档：

| # | 问题 | 选项 |
|---|---|---|
| 1 | WebUI 也开放 `/sandbox`？ | **A（推荐）开放**：命令表 `web: true`，同一条 IPC，成员拒绝。B 只给终端 |
| 2 | 工具链直通做不做？ | **A（推荐）做**：§4 那套，单独一个 commit 可整体拿掉。B 先纯锁，两份清单默认空、HOME 换根、不设环境变量 |
| 3 | 老会话的 workspace 值 | **A（推荐）迁移清空**。B 保留，等于旧值自动上锁 |
| 4 | JSON/IPC 字段名 | **A（推荐）改成 `sandbox`**，DB 列名不动。B 一律保留 `workspace` 字样 |

## 四、施工步骤（按 commit 切，从 main 开 worktree）

1. **策略层**：`SandboxPolicy.root`；`sandbox_scope.rs` → `session_scope` + `admin_policy`；系统目录常量抽出与成员共用；三个作用域化点改调一处；`SandboxConfig` + defaults + schema。
2. **命令层**：命令表、REPL 分支、CLI 子命令与本地化、IPC `SetSandbox` 与 daemon 侧校验、`session.updated`、`web/app.js` 显示改名。
3. **提示词**：`host_environment_for` 改读 task-local；`host_info.rs` 生成属性；prompt 测试。
4. **迁移** v36 清老值 + 测试。
5. **工具链直通**（决策点 2 选 A 才做）：`confine / confine_std` 按策略设环境变量与 PATH。
6. **文档**：wiki 03/04/16、architecture.md 工作区与沙盒两节、`next-release-note.md`。

## 五、验证

### 单测 / 门禁
- `sandbox.rs`：管理员策略下 shell 根内可写、根外不可写、家目录不可读、清单目录可读/可写；`guard` 同款。
- `host_info`：属性字节恒定，同一策略两次生成逐字节相等。
- 迁移 v36 测试。
- `cargo fmt`、`scripts/refactor-check.sh` 五道门禁（动了 agent/prompt）。

### 黑盒 `testkit/sandbox/`（MIYU_HOME 沙箱 + 桩 LLM，照 `testkit/cli` 的骨架）
1. 新会话 `/sandbox <临时目录>` → 桩模型发 `run_command: echo hi > x` → 成功。
2. `run_command: cat ~/.ssh/id_ed25519` → tool error，输出含 `outside your workspace`，回合不崩。
3. `read` 家目录文件 → 同上（进程内守卫）。
4. `/sandbox` 显示根与两份清单；`/sandbox clear` 后同样命令成功。
5. 成员会话发 `/sandbox` → 拒绝。
6. `miyu session sandbox … --json` 输出字段。

### 真机（用户可照做的验收流程）
1. `miyu` 进 REPL，`/new`，`/sandbox ~/tmp/sbx`，看到「已绑定沙盒」。
2. 让她「在当前目录建个 hello.txt 写一句话」→ 成功；`ls ~/tmp/sbx` 看到文件。
3. 让她「读一下 ~/.ssh 里有什么」→ 她回复读不到（沙盒外）。
4. 让她「把 ~/Documents 里随便一个文件复制过来」→ 失败，报沙盒外。
5. （决策点 2 选 A）在根里 `cargo new demo && cargo build` 由她执行 → 成功。
6. 切 claude-code 供应商再发一轮 → CLI 能起、能用桥工具、根外写被拒。
7. `/sandbox clear` 后重复第 3 步 → 读得到。
8. 缓存：绑定后连打两轮，`cache-usage.jsonl` 第二轮 `cache_read` 正常（AGENTS 1.6）。

## 六、坑与备忘

- Landlock 授权根不存在→exec 失败关闭；所有清单先 `retain(exists)`。
- 二进制换过后 `current_exe()` 带 ` (deleted)`，用 `miyu_executable()`（成员版已处理，照搬）。
- 绑定/解绑各一次缓存冷启动；成员环境块字节也变一次。
- 主检出起 daemon 会抢 8300，黑盒用 `sandbox-fresh.sh` 那套 8388 纯净环境。
- 重启 daemon 前先 cd 出 worktree。

## 七、施工记录(09-13,分支 worktree-sandbox-2026-09-13)

- 策略层:`SandboxPolicy` 多了 `root / env / path_prepend / writable_summary / readable_summary`;
  `sandbox_scope.rs` 改成 `session_scope`(成员 → 管理员绑定 → 客户端 cwd 三级)+ `admin_scope`,
  三个作用域化点(actor Create/Redo、工具桥)各删掉一份手写逻辑;`tools.sandbox.{readable,writable}`
  进配置与设置页。
- 命令层:`/sandbox`(命令表 `web: true`)、`miyu session sandbox`、IPC `SetSandbox`(daemon 侧
  canonicalize + 目录 + probe + 成员拒绝)、`session.updated` 载荷 `sandbox`、WebUI 列表提示改名。
- 提示词:`host_environment_for` 读 task-local 策略;`host_info` 按摘要生成 `root/writable/readable`,
  写死的成员字符串删除。
- 迁移 v36 清 `sessions.workspace` 老值(带回归测试)。
- 工具链直通:`child_env` 统一给 HOME / 工具链变量 / PATH;中转线保留真 HOME。
- 黑盒:`testkit/multi-user/member_tools_probe.py` 加管理员 `/sandbox` 三幕(绑定→锁住、拒绝、
  解绑→恢复),桩模型按「最后一条是不是 user」决定叫工具,并 dump system 消息验环境块。

## 八、验证结果(09-13)

| 项 | 结果 |
|---|---|
| 单测(release 档,32 项:sandbox / host_block / host_environment / v36 / slash_command / web commands) | 32 过;另两项 `web::tests::commands::web_memory_reset_*` 红是 09-12 `462e2123`(#137)加 `require_local_web_session` 后的存量问题,主干同样红,与本改动无关 |
| 黑盒 `testkit/multi-user/member_tools_probe.py`(成员 + 管理员不绑 + 管理员绑定 + 拒绝 + 解绑) | 48/48 |
| 真机(隔离 daemon 8391、真配置、opencodego/deepseek-v4.1-flash;T4 走 claude-code/haiku) | 见下表 |
| 门禁:格式 / 模型面英文 / 文件规模 / 依赖方向 | 格式过;后三项在纯净 main 上同样红(ledger.json 中文描述、基线行数 +34%、跨层引用),非本改动 |

真机五轮(`miyu session new sbx` → `miyu session sandbox sbx <root>` → `miyu ask --session sbx …`):

| 轮 | 动作 | 结果 | prompt / cache_read |
|---|---|---|---|
| T1 | 在根里建 hello.txt 并 ls | 文件落在根里 | 38,545 / 19,200 |
| T2 | 读根外 `outside/secret.txt` | read 工具报 sandbox 拒绝,她答「沙箱把它拦了」,标记串没漏 | 39,202 / 38,784(98.9%) |
| T3 | `cargo --version; git --version` | cargo 1.99 nightly、git 2.55 都跑通(工具链直通) | 39,602 / 39,168(98.9%) |
| T4 | claude-code/haiku:把根外文件 cp 进来 | CLI 的 Bash 里 `cp` 报「权限不够」,根里没多文件 | 38,737 / 19,109 |
| T5 | `--clear` 后再读根外文件 | 读到 `OUTSIDE-MARKER-4242` | 40,253 / 21,248(解绑掰了一次缓存,符合预期) |

编译备忘:debug 档 `cargo test --lib` 在 MemoryMax=12G 下被 OOM 杀;改 `--release` 档(codegen-units=1)
20G 上限内通过,且与 `cargo build --release` 共用依赖产物。单元跑 `systemd-run --user --unit=…`,
Bash 前台 600s 上限扛不住整包编译。
