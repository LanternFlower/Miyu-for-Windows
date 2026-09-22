# 供应商模型目录(as-built,v0)

领域模块 `src/provider_catalog/`,WebUI、配置 TUI、OOBE 三个入口共用同一套查询;
以前 Web 反向调用 `config_tui` 的 helper,2026-09-16 起断掉。

## 入口

| 函数 | 作用 |
|---|---|
| `fetch_models(&ProviderConfig, cli_binary: Option<&str>) -> Result<Vec<String>>` | 拉模型 id 列表。内置 CLI 供应商走 `cli.rs`,其余走 `http.rs` |
| `builtin_cli_binary(&AppConfig, &ProviderConfig) -> Option<String>` | 内置 CLI 供应商该跑哪个二进制;HTTP 供应商为 `None` |
| `auto_configure_model_tags(&MiyuPaths, &mut ProviderConfig, model)` | 从 models.dev 目录补模态与上下文窗口,**只补空缺不覆盖手填**,不写价格 |
| `catalog_entry(&MiyuPaths, &ProviderConfig, model) -> Option<ModelCatalogEntry>` | 单条目录条目(表单预填用) |

调用方:`web/providers_api.rs`(管理员鉴权 + HTTP DTO)、`config_tui/mod.rs::ProviderBrowser`
(后台线程 + 序号防串扰)、`oobe/providers.rs::CatalogJob`(探到 CLI 就提前拉)。

## HTTP 路

- 地址:`provider_url::models_url(base_url)`——去尾 `/chat/completions`,以 `/v1` 结尾则加
  `/models`,否则加 `/v1/models`。
- 鉴权:`api_key` 以 `$env:NAME` 开头时读环境变量;opencode Zen 免 key 时用 `public`。
  Bearer 头只在 key 非空时带。
- 超时 `provider.timeout_seconds`;`User-Agent: miyu-config`;非 2xx 报 `"{status}: {body}"`。
- 解析 `{ "data": [{ "id": … }] }`,空 id 丢弃,**顺序照服务端**。

## CLI 路(`cli.rs`)

`claude` / `codex` / `agy` 各自的列模型子命令,单次 20 秒硬超时(`CLI_LIST_TIMEOUT`),
起不来、退出非零、解析失败、超时四类错误分别报文。结果并上 `provider.models` 里手填的名字
(只返回目录会让用户激活一个模型后下次只剩那一个可选,09-03)。

## 边界

- 只读:不写配置、不改 `active_provider`、不碰模型缓存库;写回由调用方决定。
- 不返回凭据:结果只有模型 id 字符串。
- 无授权层:三个调用方都是属主侧入口(Web 已做管理员鉴权),成员/扩展不能调。

## planned(未实现)

- 脱敏 DTO(`ProviderSummary` / `ModelSummary`)与 `ProviderQueryScope` 授权;
- 给 scripts / MCP-aware 扩展的进程外只读查询(`host.providers.list/get`);
- stub HTTP / 假 CLI fixture 锁顺序、超时、脱敏(现只有 `provider_url` 单测)。
