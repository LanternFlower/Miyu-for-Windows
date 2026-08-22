# 08 · 天气查询移除自动定位

## 1. 根因

`src/tools/weather/mod.rs::get_weather`：

```rust
if request.location.is_empty() && request.query_type == WeatherQueryType::Forecast {
    return get_weather_wttr(&client, "", "auto_location").await;
}
```

同时工具契约明确承诺“空字符串自动定位”（`TOOL_DESC` 与 `descriptions/get_weather.json`）。模型因此有正当理由不传城市；wttr.in 的 IP 定位在用户网络下不准确。

## 2. 方案

1. 删除 forecast 空 location 的自动定位分支。
2. 所有 query_type 空 location 统一报错：

```text
location is required. Ask the user which city or place to query, then call get_weather again with that location.
```

3. 更新 `src/tools/weather/mod.rs::TOOL_DESC` 与 `src/tools/descriptions/get_weather.json`：
   - 删除“空字符串自动定位 / fallback wttr.in”。
   - `location` 参数描述改为 `Required city, place, postal code, or airport code. Do not guess; ask the user when it is missing.`
4. wttr.in 仍保留为**显式 location 的 fallback**，但删除/禁用 `auto_location` 调用形态；若该形态无其它调用方则一并移除。
5. 若存在 CLI 直接入口（当前 `rg weather src/cli` 未发现），同样要求 `location` 参数，缺失时打印英文提示；当前无此入口。

## 3. 修改文件清单

- `src/tools/weather/mod.rs`
- `src/tools/descriptions/get_weather.json`
- 对应测试：`src/tools/weather/mod.rs` 内测试新增空 location 报错断言。

## 4. 验收

1. `get_weather({})` 返回明确错误，不发起 wttr.in 自动定位请求（mock 或网络日志验证零请求）。
2. `get_weather({"location":"Tokyo"})` 行为不回归。
3. 模型面描述中不再出现“自动定位”。
4. `bash scripts/refactor-check.sh` 全绿。
