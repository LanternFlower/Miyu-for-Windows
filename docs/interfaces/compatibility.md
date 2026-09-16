# 兼容规则

## 现状

| 契约 | 版本 | 兼容机制 |
|---|---|---|
| scripts 头部 | `scripts` v1(`config::SUPPORTED_CONTRACTS`) | 未知键跳过;缺省值即契约;只加不删;09-16 加 `Capabilities` 键(旧版本忽略它,脚本照常跑) |
| skills(SKILL.md frontmatter) | `skills` v1 | 未知 metadata 键保留 |
| persona.toml | `persona-manifest` v1 | 缺字段取默认;退役插件 id 容忍 |
| MCP client | `protocolVersion: 2025-03-26` | 只 stdio;server 不认此版本即失败 |
| Miyu MCP server | 回显客户端版本(缺省 2024-11-05) | 不拒绝任何版本 |
| 内置工具描述 JSON | 无 | 字段带 `#[serde(default)]`;改描述 = 缓存冷启动 |
| `PlatformPlugin` / 宿主端口 | 无(编译期 trait) | 改签名要改全部实现 |

## PM 包清单的静态预检(as-built,`src/pm/mod.rs`)

`plan_install` 在写任何文件之前依次查:清单合法、`requires-miyu`(`>=x.y.z`)、
`requires-contracts`(如 `{ scripts = 1, skills = 1 }`:不认识的契约 id 或版本高于宿主支持的拒装)、
`requires-capabilities`(只认 `host_ports::HOST_CAPABILITIES`:`host.info` / `providers.read` / `subsystems.read`;表外的拒装,不静默装上一个运行时拿不到能力的包)、
目标文件不撞别的包;装完把文件清单与 blake3 指纹记进锁文件。

planned:声明写/网络范围与实际执行层的对接、入口文件校验、签名。

## 规则

1. **只加不删**:头部键、JSON 字段、端口方法只能新增且有缺省;删除或改缺省值须另开专项,
   先写迁移,再在 `next-release-note.md` 说明。
2. **缓存证据**:凡改工具描述、schema、注册顺序、system 文案,属"影响缓存前缀"的改动——
   要附 `shape_tests` 结果、`token_diet_baseline` 量尺、真供应商两轮 cache-usage 数据。
3. **依赖门禁**:`arch_dep_check` 白名单为空;要加跨层边先在 `host-capabilities.md`
   设计端口。
4. **文件规模**:`refactor_size_report.py --check`,超标文件不得再长。
5. **失败语义不变**:MCP `isError` 始终是工具失败;旧 tool_flow JSON 逐字节回放。
6. **未知能力**:PM 预检落地后,声明了宿主不认识的 capability / contract version 的包拒绝安装
   或明确 warning,不静默装。
