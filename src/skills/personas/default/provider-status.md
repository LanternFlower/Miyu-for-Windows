---
name: provider-status
display_name: 供应商状态排查
summary: 分清是大模型供应商挂了还是本机配置坏了
description: Work out whether a model provider is down or this machine's config is. Use when the user says 模型报错、回不了话、很慢、502、限流、超时、DeepSeek 挂了吗、换个供应商试试, or when you just hit a run of endpoint errors yourself.
---

# 供应商状态排查

## 什么时候用

模型侧出问题时，先分清是**供应商挂了**还是**这台机器的配置/网络坏了**——两者的处理完全相反，猜错会让用户白改配置。

## 第一步：看报错本身

Miyu 的报错已经分好类，先读它再动手：

- 带「冷却」「429」「限流」→ 是配额或速率，不是服务中断，换池子里另一个端点即可。
- 「连接失败 / 超时」→ 先怀疑本机网络，让用户确认别的网站打得开。
- 「401 / 403」→ 凭据问题，不是供应商状态。
- 5xx、或者同一个池子里**多个端点同时报错** → 才值得往下查。

## 第二步：查官方状态

DeepSeek 有专用脚本（不在常驻工具面上，经工具桥调）：

```bash
miyu tool-call query_deepseek_status --stdin <<'JSON'
{"include_incidents": true, "max_incidents": 5}
JSON
```

参数都可省：`include_incidents` 默认 true，`max_incidents` 默认 5、范围 1–20。

其他供应商没有专用脚本，用 `web_fetch` 打它们的状态页；查不到就直说"没找到官方状态页"，别编。

## 第三步：给结论

- **确认是供应商侧**：告诉用户哪家、什么时候开始、官方有没有认，并建议切到池子里的另一家（`/models` 或会话级模型覆盖）。
- **不是供应商侧**：说清判据（比如"官方状态正常，而且只有这一个端点报 401"），再往配置/网络查。

别把"我这边报错了"直接等同于"供应商挂了"——用户会据此去改配置，错一次就是白折腾一轮。
