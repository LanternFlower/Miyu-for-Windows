# 真终端手测清单

假终端（`matrix.py`）只能验到「chafa 被正确调用」。**终端真正把图画成什么样**
——画在哪一行、光标停在哪、输入框会不会盖住图——只有真终端说了算。

## 准备

```bash
testkit/chafa-compat/sandbox.sh      # 起沙盒（端口 8389，与本机 daemon 隔离）
```

daemon 起在 8389（systemd user unit `miyu-chafa-sandbox`），**所有终端共用它**，
各终端只是各开一个 REPL 客户端——图片渲染发生在 CLI 这一侧，正是要测的地方。

然后在**每一种要测的终端里**开一个窗口，跑：

```bash
~/.cache/miyu-chafa-sandbox/miyu-sb normal
```

**先随便聊两三句把屏幕填满**，再要图——这一步是必须的：图只在"放不进屏幕剩余
空间"时才会丢，而刚进 REPL 时屏幕是空的，怎么测都正常。填满之后连着要三张：

```
显示 ~/.cache/miyu-chafa-sandbox/images/small.png
显示 ~/.cache/miyu-chafa-sandbox/images/wide.png
显示 ~/.cache/miyu-chafa-sandbox/images/tall.png
```

日志里可以自查有没有测到点子上：每条 chafa 行**前面**那条 CPR 的
`fallback=(_, N)` 就是打图前的光标行，**N 越接近屏幕最后一行越是有效样本**；
每次都是二三十行说明屏幕没填满，这轮不算数。

**从 kitty 里启动别的终端**也要各测一次——`KITTY_*` 环境变量会被继承，
2026-09-13 那轮 ptyxis 就是栽在这上面。

走的是用户真正会遇到的那条路：daemon 发 `tool.image` → CLI 侧 `print_image_file`
渲染。**必须是 REPL**——`tool-call` 在 daemon 侧执行，没有终端，只会返回
"image emitted to the host"，测不到渲染。

四张测试图：`small.png`(128²)、`wide.png`(640×240)、`tall.png`(240×640)、
`big.png`(1024×768)。图里有 16px 细网格，缩放、抖动、调色板的毛病一眼能看出来。

## 每个终端要记的

| 项 | 怎么看 |
|---|---|
| 出的是真图还是字符画 | `tail -5 ~/.miyu/cache/logs/image-trace.log` 里的 `format=` |
| 图有没有错位/阶梯 | 直接看 |
| 图下方的正文有没有盖住图 | 图打完之后再问她一句 |
| 输入框有没有画到图上 | 图打完那一瞬间看底部 |
| 卡顿 | trace 行的耗时，超过 500ms 就记下来 |

trace 每行长这样：

```
chafa 1.18.2 TERM=xterm-256color COLORTERM=truecolor TERM_PROGRAM=- KITTY=1 raw=false
  args=["--polite", "on"] size=Some("36x8") stdin=null -> exit status: 0 42ms format=sixel bytes=16606
```

`format=` 是最要紧的一格：`sixel`/`kitty`/`iterm` 是真图，`symbols` 是字符画。
`raw=true` 说明是 REPL 形态（该测的就是它）。

**已知会误导的两个数**（本机用 `script` 造 PTY 验证时撞到的）：

- **耗时 5000ms 左右** = 终端根本没应答能力查询，chafa 卡满了 `--probe` 的默认
  5 秒超时。真终端会应答所以是几十毫秒；**tmux/screen 里 DA1 应答可能被拦，
  那就每张图卡 5 秒**，看到这个数就记下来。
- **size=1x1** = 终端报不出窗口尺寸（`crossterm::terminal::size()` 拿到 0）。
  真终端里不会，出现了说明终端有问题。

## 终端清单与启动命令

先设个短名：

```bash
SB=~/.cache/miyu-chafa-sandbox/miyu-sb
```

### 第一档：本机已装，先测这些

| # | 终端 | 启动命令 | 预期 `format=` |
|---|---|---|---|
| 1 | **xterm（默认）** | `xterm -fa Monospace -fs 12 -e "$SB normal"` | `symbols` |
| 2 | **xterm（开 sixel）** | `xterm -ti vt340 -fa Monospace -fs 12 -e "$SB normal"` | `sixel` |
| 3 | **kitty** | `kitty "$SB" normal` | 不经 chafa（自研协议，trace 里不会有行） |
| 4 | **Konsole** | `konsole -e "$SB" normal` | `sixel` 或 `iterm` |
| 5 | **Alacritty** | `alacritty -e "$SB" normal` | `symbols` |
| 6 | **tmux（在 kitty 里）** | `kitty tmux new -s img "$SB normal"` | `symbols` 或 passthrough |

1 和 2 是**同一个 xterm 的两种身份**，务必都测：默认 `decTerminalID=100`，DA1 里
不报 sixel，chafa 只能退字符画——「xterm 明明支持 sixel 却不行」多半就是这个。
`-ti vt340` 才让它报。

6 要盯耗时：tmux 会拦 DA1 应答，chafa 可能卡满 5 秒。

### 第二档：值得装

```bash
sudo pacman -S --needed foot ghostty wezterm
```

| # | 终端 | 启动命令 | 预期 `format=` |
|---|---|---|---|
| 7 | **foot** | `foot "$SB" normal` | `sixel` |
| 8 | **ghostty** | `ghostty -e "$SB normal"` | `kitty` |
| 9 | **WezTerm** | `wezterm start -- "$SB" normal` | `kitty` 或 `sixel` |

8 顺带验 B10：ghostty 下**图片**走 chafa（`TERM=xterm-ghostty` ≠ `xterm-kitty`），
而**公式**走自研 kitty 协议。同屏问她一条数学公式就能看出两套判定的差别。

### 2026-09-13 实测覆盖

kitty / Konsole / foot / WezTerm / ghostty / xterm / Alacritty / Ptyxis /
GNOME Terminal / xfce4-terminal / VS Code 内置终端——**全部通过**。

### 可跳过

GTK 系终端基本共用同一个 VTE 库（GNOME Terminal、xfce4-terminal、MATE Terminal、
Terminator、Tilix、Guake、Ptyxis 都是），上面已经测了其中三个，其余结论重复。
Yakuake 同理跟 Konsole 同族。

发行版默认终端的对应关系：Ubuntu 和 Linux Mint（Cinnamon/Xfce/MATE）全是 VTE 系，
Kubuntu 是 Konsole，都已覆盖。

## 对照：手跑 chafa

同一个终端里，同一张图，不经 Miyu：

```bash
chafa --polite on --size 36x8 ~/.cache/miyu-chafa-sandbox/images/small.png
```

这是 Miyu 现在传的参数。**手跑和 Miyu 里出的东西应该一模一样**——不一样就说明
差别还在我们这边，把 trace 行贴出来。

## 换旧版 chafa 再来一遍

```bash
testkit/chafa-compat/fetch-old-chafa.sh                       # 先拉历史二进制
~/.cache/miyu-chafa-sandbox/with-chafa 1.14.5 ~/.cache/miyu-chafa-sandbox/miyu-sb
```

1.14.5 = Debian 13 / Ubuntu 24.04+ / Fedora 41 的仓库版本。**修复前这里一张图都
出不来**（`chafa exited with status 2`），修复后应当出字符画。
