# Windows 分发件

产出两个东西，都在 `out/windows/`：

| 产物 | 说明 |
|---|---|
| `Miyu-<版本>-windows-x64.zip` | 便携包，解压即用（把 `bin\` 加进 PATH 后 `miyu` 可用） |
| `MiyuSetup-<版本>-x64.exe` | Inno Setup 安装器：装到 `%LOCALAPPDATA%\Programs\Miyu`、写用户级 PATH、带卸载器 |

打包逻辑只有一份：`package.ps1`。本地与 CI（`.github/workflows/windows-release.yml`）都调它。

## 本地怎么打

```powershell
cd <仓库根>
cargo build --release
pwsh -File packaging/windows/package.ps1 -Version 0.6.1
```

- 没装 Inno Setup 时会跳过安装器、只出 zip（脚本会说明）。装了就在标准路径找到 `ISCC.exe`；装在别处用 `-Iscc <路径>`。
- 发版机器不方便联网时，用 `-RipgrepPath <rg.exe>` 指一份本地 ripgrep，或 `-SkipRipgrep` 先只验布局。
- `-Version` 必须与二进制自报的版本一致，否则脚本直接报错——防止打错标签的包。

## 为什么是这个布局

`crates/miyu-base/src/paths/resources.rs` 里 `installation_prefix = exe.parent().parent()`，
资源去 `<前缀>/share/miyu/<kind>` 找。所以：

```
<安装目录>\bin\miyu.exe
<安装目录>\bin\rg.exe
<安装目录>\share\miyu\fonts\{NotoSansCJK-Regular.ttc, JetBrainsMono-Regular.ttf, NotoColorEmoji.ttf}
<安装目录>\share\miyu\memes\...
<安装目录>\share\miyu\scripts\...
<安装目录>\share\licenses\miyu\...
```

**`bin\` 与 `share\` 两层都不能省**（前缀是 exe 的上两级）。zip 解压到任何目录都成立。

## 捆了什么、为什么

| 内容 | 理由 |
|---|---|
| `NotoSansCJK-Regular.ttc` | 渲染器**硬要求**它（`renderer/fonts.rs` 找不到就报错）；Windows 侧没有 `/usr/share/fonts` 那类系统兜底 |
| `JetBrainsMono-Regular.ttf` / `NotoColorEmoji.ttf` | 代码体与 emoji 的回退字体，缺了代码块不等宽、emoji 变豆腐块 |
| `rg.exe` | 文件搜索类工具直接 spawn `rg`（走 PATH），不捆的话新装机器上那几件工具直接报错 |
| `memes` / `scripts` | 人格表情库与内置脚本，按资源路径分发（Linux 包同样装它们） |
| 各许可证 | Noto / JetBrains Mono 的 OFL 要求随二进制分发 |

**没有捆**：

- **`chafa`**（图片与 mermaid 渲染）：缺了是静默降级（代码里 `.ok()?` 兜底），不报错也不崩。要完整跑图片渲染就自己装一份并放进 PATH。
- **`models`**（本地嵌入模型，约 23 MB）：不捆也能用，`miyu embed` 会按需下载。
- **语音组件**：默认特性不含 `miyu-voice`（Windows 暂无发行包）。想带上就 `cargo build --release --features voice`，脚本会把 `miyu-voice.exe` 一并放进 `bin\`。

## 已知取舍

- **没有代码签名**：首次运行会挨一次 SmartScreen（「更多信息 → 仍要运行」）。要消除得买 OV/EV 证书，与代码无关。
- **安装器 UI 是英文**：官方 Inno 不带简体中文语言文件。要中文得把 unofficial 的
  `ChineseSimplified.isl` 放进 Inno 的 `Languages\` 目录，再在 `miyu.iss` 的 `[Languages]` 里加一行。
- **`ArchitecturesAllowed=x64compatible`** 需要 Inno Setup **6.3+**（GitHub runner 自带的满足；过老的 ISCC 会报未知指令，届时改成 `x64` 或删掉这行）。
- **`rg` 走 PATH**：所以 `miyu` 本身在 PATH 上是前提（安装器会自动加；便携包要自己加）。

## 安装/升级/卸载会动什么

- **装之前先停 daemon**：先调 `miyu daemon stop`，再 `taskkill /IM miyu.exe /F` 兜底——Windows 上运行中的 exe 覆盖不了（本地 `cargo build` 也被这个坑过一次）。
- **只写用户级 PATH**（`HKCU\Environment`），并且放在**最前面**——后装的版本优先，不会再被
  PATH 里已有的旧副本（典型是从源码构建后手动加进去的 `target\release`）盖住。
  `ChangesEnvironment=yes` 让新开的终端立刻认得；卸载时把这一项摘掉。
- **卸载保留 `%USERPROFILE%\.miyu`**：配置、会话库、记忆、人格都在里面。
- 可选项（默认不勾）：装 PowerShell 终端集成（`miyu powershell-init`，写 `$PROFILE`，可用 `miyu remove-shell-hook` 撤销）、桌面快捷方式。

## CI

`.github/workflows/windows-release.yml`：

- 推 `v*` 标签 → 自动按标签版本打包，产物作为 workflow artifact。
- 手动 dispatch → 可指定 `version` / `ripgrep-version`；勾 `publish` 则挂到对应 tag 的 GitHub Release（草稿不存在时创建一个草稿）。
- runner 用 `vars.WINDOWS_X64_RUNNER`（缺省 `windows-2022`），Rust 工具链从
  `packaging/common/toolchain.lock.json` 读，不在这里另写版本号。
- **没有 build cache**：加 `actions/cache` 能省十几分钟，但那需要按 SHA 钉一个 action；
  等真有需要时再补（本仓库的约定是 action 一律钉 SHA）。

## 验收（改完打包件后照这个走）

```powershell
# 1. 打出来（版本号必须与二进制自报的一致，脚本会拦）
cargo build --release
pwsh -File packaging/windows/package.ps1 -Version <版本>

# 2. 便携包：解到干净目录，从那儿跑
Expand-Archive out/windows/Miyu-<版本>-windows-x64.zip -DestinationPath $env:TEMP\miyu-portable -Force
& "$env:TEMP\miyu-portable\bin\miyu.exe" --version        # 期望与 <版本> 一致
#   再把 $env:TEMP\miyu-portable\bin 加进 PATH，起 daemon、说一轮

# 3. 安装器：装 → 验收 → 卸载，重点看四件事
#    a) 装完新开一个终端，`miyu` 能直接跑（PATH 生效）
#    b) `miyu --version` 就是刚装的那一版——即使 PATH 里本来还有别的 miyu
#       （安装目录被放在最前面；2026-09-23 就是因为追加写入而输给旧副本，才改成前置）
#    c) 升级安装时旧 daemon 被自动停掉（不再出现文件占用失败）
#    d) 卸载后 %USERPROFILE%\.miyu 还在、PATH 里的那一段没了
```
