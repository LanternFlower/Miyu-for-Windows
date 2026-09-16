# 疑问

- 关于用户触发回复后的续聊窗口和AI发送消息后的观察窗口，如果重叠了会怎么样？

# BUG

- TUI，命令的预览渲染似乎展开后不是流式输出的，点击后台命令状态行打开的浮层也是没有流式输出

- rm -f 没有被拦下
- 用debug二进制的时候miyu在腾讯平台的回复特别特别慢
- /config中全局参数设置的`显示思考过程`不是把timeline的思考内容自动展开，而是用的旧版本的inline。`显示工具调用信息`也是一样的问题。子代理浮层里的也要跟随这两个开关。![image-20260914150119508](/home/shorin/.config/Typora/typora-user-images/image-20260914150119508.png)

- TUI里todolist渲染出来的表格被`Worked for`缩起截断![image-20260915152322213](/home/shorin/.config/Typora/typora-user-images/image-20260915152322213.png)

## 已有功能优化

- 把思考从 Worked for 的 timeline 中分离，做到 Think work think work 这样的输出，非常make sens；

- 把命令从Worked for 中分离

- 需要将TUI配置整理到一个菜单中，新增一个配置，让TUI中不自动收起Worked for，也就是做到和shellhook触发差不多的效果。

- 运行命令的输出的语法高亮

- 思考过程展开后的流式内容增加md渲染？

- goal运行中没有任何提示，也许输入框的右上方可以有一个提示，就像claudecode那样。

- 现在当我在QQ里跟我的AI私聊说：你把这个消息发到xxxx交流群里，它是做不到发送消息的。我认为通讯平台，应当允许AI主动给任意一个好友/群聊发送消息。因为没有合适的工具，我的AI甚至是直接调用接口发送的

  ```
  【工具：run_command】
  运行：run_tplQLbj-n06QUrPQSIlBl3NR
  调用 ID：agy-141
  显示名称：运行命令
  参数：
  {
    "command": "python3 -c '\nimport urllib.request\nimport json\n\nurl = \"http://127.0.0.1:3000/send_group_msg\"\nheaders = {\n    \"Authorization\": \"Bearer -182QYM2CnsfnLmTst3LnvtiwarqW8WGyYCvTKNQNqI\",\n    \"Content-Type\": \"application/json\"\n}\nmsg = \"\"\"9月15日，丙午年八月初五，星期二，工作愉快，幸福生活\n1、豆包手机助手消费者版发布，将搭载努比亚NaviX Ultra，9月16日发布，支持跨App多步任务、本地数据授权检索、录音转录与记忆；\n2、苹果发布新一代Apple Intelligence，Siri AI重构，支持个人上下文和屏幕感知，英文Beta上线，欧盟中国暂不可用；\n3、ChatGPT桌面端语音功能降价约60%，Voice可用量增至2.4倍，限Codex和Work；\n4、ChatGPT礼品卡美国上线，可充值钱包用于订阅、续费和购买额度，兑换仅限美国美元账户；\n5、DeepSeek网页端与Ap…"
  }
  ```

  ![image-20260915145720693](/home/shorin/.config/Typora/typora-user-images/image-20260915145720693.png)

- 新增沙箱目录可读写目录配置项目

- 开启一个miyu的TUI运行一个会话对话，然后在AI输出中再开启另一个TUI，进入同一个会话，这个时候我期望两个会话的内容是相同的，都在流式渲染相同内容，类似opencode的设计；不过，如果这时候两个会话同时发送消息会怎么样？AI收到的会是什么消息。或者，能让发消息这个行为也同步渲染么。例如TUIA中发消息进入排队，TUIB也能看到这样就不存在冲突了。也许还有别的我没考虑到的问题。

- 把多发行版打包从ubuntu25.10+改成24.04+

- 优化数据统计页面，提升可读性和美观度

- 报错信息处理优化，例如 429 这样的额度报错码并没有处理并可视化输出给用户。

### core层和normal重构

剔除无用代码、解耦耦合代码、提升代码可维护性。core层和normal层是这个软件的核心，一定要

#### 接口规范优化

目前只有scripts接口有规范，有相关的文档和skill；对于MCP还有内置插件都没有统一的规范，旁人难以进行扩展编写。也许可以仿照astrbot，或者deepseek harness的形式，将大部分东西给出接口，例如供应商配置、各种信息，都可以通过接口调用，插件有了信息，也就有了扩展的空间。
现在Miyu的架构分层分core、从core扩展出来的各个子系统：preset（persona）、scripts、mcp、扩展等等，子系统又会提供接口，例如scripts是一个接脚本的接口，有一套自己的规范；mcp这个扩展又是一个接口，有一套自己的规范等等。

现在的问题在于，Miyu的core-->子系统这里的接口缺乏清晰的规范，子系统和子系统之间的通信又如何处理？这也是问题。

接口做规范了，后续miyu pm的“包管理”才有办法推进。

我希望你能够解决这部分问题。

### config TUI 前端修改

# 新增功能

- 多发行版打包工作流，MacOS适配
- Live2D
- 支持QQ官方机器人
- 支持telegram
- 首次使用TUI OOBE
- computer use
- 子代理允许开启子代理，但子代理的子代理不允许开启子代理。这主要是为了让开发子代理能开子代理帮忙干活
- tab切换只读模式，不断缓存。

### MacOS适配

> PR #43（yxxbc）已合：语音 / worker / 迁移 / 4 个内置脚本的兼容修复，一批 `cfg(unix)` 收紧成 `cfg(target_os="linux")`。真机效果还没人在 Mac 上验过。

- sandbox-exec替换landlock
- 所有脚本是否在macos上正常使用
- 所有工具是否在macos上正常使用

