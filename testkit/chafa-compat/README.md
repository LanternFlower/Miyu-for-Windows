# chafa 兼容量尺

图片渲染的报障几乎全是「我这儿不对、开发机上好好的」。差别藏在三层里：
**chafa 的版本**、**终端对能力查询的应答**、**chafa 最终选中的输出格式**。
光看屏幕分不出来，所以有了这套量尺。

## 为什么需要它

chafa 的命令行选项随版本增减，**传了它不认的选项就是退出码 2，一张图都不出**：

| 选项 | 起始版本 | 发布 |
|---|---|---|
| `--polite` | 1.10.0 | 2022-03-20 |
| `--relative` | 1.14.0 | 2024-01-08 |
| `--probe` | 1.16.0 | 2025-05-18 |
| `--probe-mode` | **1.18.1** | **2026-02-08** |

而各发行版仓库里的版本（2026-09 实测 repology）：

| 发行版 | chafa |
|---|---|
| Ubuntu 22.04 LTS | 1.8.0 |
| Ubuntu 24.04 LTS | 1.14.0 |
| Ubuntu 25.04 / 25.10 | 1.14.5 |
| Debian 12 | 1.12.4 |
| Debian 13（当前 stable） | 1.14.5 |
| Fedora 41 | 1.14.5 |
| openSUSE Leap 15.5/15.6 | 1.12.x |
| Alpine ≤3.23 | ≤1.18.0 |
| Arch / Tumbleweed / Fedora 42+ / homebrew / nix / FreeBSD | 1.18.2 |

开发机是 Arch，**永远是最新版**——所以只在本机测等于没测。

## 用法

```bash
./fetch-old-chafa.sh          # 从 Arch 归档拉 1.12.4/1.14.5/1.16.2/1.18.0 真二进制
python3 matrix.py             # 版本 × 参数集 矩阵
python3 rawmode.py            # raw 模式、慢应答、不应答 三种终端脾气
python3 queries.py            # chafa 到底发了哪些能力查询
```

`matrix.py` 的输出形如（`×退出2` = 图片完全不显示）：

```
调用形态                                         1.12.4  1.14.5  1.16.2  1.18.0  1.18.2
A 裸调用                       stdin=null       symbols symbols symbols symbols   sixel
B --probe off --relative off   stdin=null        ×退出2  ×退出2 symbols symbols symbols
C --probe-mode ctty --polite on --relative off   ×退出2  ×退出2  ×退出2  ×退出2   sixel
D --polite on                  stdin=null       symbols symbols symbols symbols   sixel
E --polite on                  stdin=tty        symbols symbols   sixel   sixel   sixel
```

读法：

- **C 行**是 09-12~09-13 的线上参数。除了最新版全崩。
- **D 行**是现在的参数。旧版从「崩」退化成「字符画」，最新版照样出真图。
- **E 行**说明 1.16~1.18.0 那一档只要把 stdin 接到控制终端就能探测成功——
  `terminal::chafa::stdin_should_be_tty()` 就是为这一档写的。

## 假终端怎么造

`pty_probe.py` 开一个 PTY，父进程扮演终端：收到 DA1 就应答 `ESC[?63;1;2;4;...c`
（4 = sixel），收到 XTVERSION/sixel-geometry/cell-px 也各给一个像样的回答。
子进程的 stdout/stderr 接 PTY、stdin 可选 `/dev/null` 或同一个 PTY——**完全复刻
`src/tools/vision/print.rs` 的调用形态**。

`rawmode.py` 额外把 PTY 设成 raw（`tty.setraw`），并能模拟「应答慢 0.3s」和
「完全不应答」。后者实测 chafa 会**卡满 5 秒**才放弃（`--probe` 的默认超时）。

## 真终端那一段

这套量尺只能验到「chafa 被正确调用」。**终端真正把图画成什么样，得用真终端测**
（见 `HANDTEST.md`）。2026-09-13 那轮排查里，真正定位到根因的是真终端 + 字节转录，
不是这里的假终端。

## 那一轮最终查出来的四件事

| 问题 | 怎么定位的 | 现象 |
|---|---|---|
| 旧版 chafa 收到新参数直接退出码 2 | `matrix.py` 版本矩阵 | Debian 13 / Ubuntu 24.04+ / Fedora ≤41 / Alpine 一张图都出不来 |
| `--size` 会把小图**放大**填满框 | 逐图量渲染尺寸 | 128px 表情包撑成 540px、占 27 行 |
| **图放不进屏幕剩余空间时整张丢失** | CPR 取证日志里 `fallback=(0,61)` 对 `(2,42)` | 终端要在绘制中滚动，Konsole 就丢图；REPL 聊久了光标贴底，此后每张都丢 |
| **`KITTY_*` 环境变量泄漏给子终端** | ptyxis 的 trace 里 `KITTY=1` + `format=kitty` | 从 kitty 里启动的任何终端，chafa 都以为自己在 kitty，输出对方不认的协议 |

后两条**假终端一个都测不出来**——它们只在真终端的真实屏幕状态下发生。

## 各脚本的实际战果

| 脚本 | 有用吗 |
|---|---|
| `matrix.py` + `fetch-old-chafa.sh` | **有**，版本兼容那条就是它查出来的 |
| `relay.py` + `relay_report.py` | **最有用**。双向字节转录，"套上它问题就不复现"这个反常直接把排查引到了真因 |
| `pty_probe.py` / `rawmode.py` / `queries.py` | 有，`--polite` 抑制了哪几个序列是它们量出来的 |
| `sandbox.sh` | **有**，整轮手测都靠它 |
| `trace_ab.py` | 有限。它和 relay 有同一个盲区（一次读 64KB，把分块抹平） |

## 两条走不通的路（脚本已删，教训留着）

- **无头 Konsole**：Xvfb 里的 Konsole 压根不画 sixel。光把它启动起来就要
  `dbus-run-session` + `QT_QPA_PLATFORM=xcb` + 摘掉 `WAYLAND_DISPLAY`（否则 Qt
  连的是你的真实 Wayland 会话，窗口开在真桌面上、Xvfb 里拍到一片黑），起来之后
  截图依然是空的。**真终端的图像问题只能在真终端测。**
- **拿 `miyu tool-call print_image` 当端到端入口**：`tool-call` 在 daemon 侧执行，
  那边没有终端，只会返回 `image emitted to the host`，测不到任何渲染。要走真实
  路径就得进 REPL（见 `HANDTEST.md`）。
