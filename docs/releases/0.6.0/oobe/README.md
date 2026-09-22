# Miyu 0.6.0 OOBE 实际画面

这四张图来自真实 `miyu 0.6.0` 二进制的交互式终端输出。捕获脚本通过 PTY 驱动引导，使用 pyte 解析 ANSI 字符、颜色与光标位置，再由 Pillow 渲染终端单元格。画面内容、选项和进度均来自程序实际输出；额外添加的只有窗口边距和顶部标题栏。

| 画面 | 文件 | 实际操作 |
|---|---|---|
| 欢迎 | [01-welcome.png](01-welcome.png) | 等待欢迎提示和动画完整帧 |
| 人格 | [02-persona.png](02-persona.png) | 回车进入人格选择，保留内置 Miyu |
| 功能 | [03-features.png](03-features.png) | 接受内置人格，显示默认功能选项 |
| 接模型 | [04-provider.png](04-provider.png) | 跳过自述，终端集成选择“不装”，停在供应商选择页 |

截图时间为 2026-09-13 17:03:09 UTC（日本时间 2026-09-14 02:03:09）。终端为 108 列 × 38 行、真彩色；每张 PNG 为 1828 × 1440 像素。捕获二进制 SHA256：

```text
559ab9e0cff1bd27cb7b887824ac5571dd447e9999501df8db302fcfb5ded615
```

详细的二进制版本、字体及图片哈希、终端帧编号和原始输出位置保存在 [capture-provenance.json](capture-provenance.json)。字符使用仓库中的 JetBrains Mono、Noto Sans CJK；缺少的星形符号使用 DejaVu Sans 字体回退。星空和扫光是程序动画，重新捕获时所处帧可能不同。

整个产品进程运行在独立的 Linux 用户和网络命名空间里，无法连接外部服务或宿主机回环地址。HOME、MIYU_HOME、XDG 目录均为带归属标记的临时目录；关闭了输入法探测，没有安装 shell hook，也没有提交供应商凭据或请求真实模型。捕获后 PTY 进程和临时目录均已回收。

这些图片展示 OOBE 界面。功能选中、供应商列出不代表相应模型、语音硬件或安装包已通过运行验收；发布验收由独立报告证明。本次来源是本地 0.6.0 debug 二进制。

原始 ANSI 会话、逐页截取文本及私有取证副本保存在不入库的 `out/distribution/oobe-capture/`。

复现需要 Linux 的 `unshare`、Python 3.11+、pyte、Pillow 和 fonttools。在仓库根目录执行，输出目录必须为空或不存在：

```bash
python3 packaging/ci/probes/capture_oobe.py \
  --binary "$(pwd)/target/debug/miyu" \
  --out out/distribution/oobe-recapture/png \
  --evidence out/distribution/oobe-recapture/evidence
```

若 DejaVu Sans 安装在其他位置，使用 `--symbol-font /字体文件的绝对路径/DejaVuSans.ttf`。脚本先检查二进制真实版本，按屏幕内容和完整绘制帧就绪后发送按键；超时或版本错误会失败，不会生成成功报告。
