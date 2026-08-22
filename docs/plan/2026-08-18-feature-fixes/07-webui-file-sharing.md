# 07 · WebUI 局域网文件分享/下载

## 1. 现状（已定位）

Miyu **已经有大部分机制**：

- Web server 默认 bind `0.0.0.0`，局域网可达（`src/web/server.rs`）。
- AI 在 WebUI 会话可调用 `create_artifact` / `present_artifact`（`src/tools/artifact.rs`）。
- `present_artifact` 会触发 `save_artifact_asset`，把本地文件**快照**到 SQLite `artifact_assets`（上限 20 MiB，`src/state/assets.rs`）。
- `GET /api/artifacts/{asset_id}` 已实现下载，带 `Content-Disposition`（`src/web/assets.rs`）。
- WebUI 已有 Artifact 预览区和下载按钮（`web/app.js`）。

缺口：
1. 工具返回结果不含 `asset_id/url`，AI 不知道可分享链接，只能笼统说“已发布到预览区”。
2. 前端下载按钮只有相对 URL，没有“复制完整局域网下载地址”入口。
3. `Content-Disposition` 对 markdown/text/html/pdf 默认 `inline`，别人拿到链接先预览而不是直接下载。
4. 认证语义没有明确：当前 `/api/artifacts/{id}` 要求 WebUI 登录；若 WebUI 无密码且绑 0.0.0.0，等于全网可下（server 已有警告）。需求“别人可以下载”需要一个明确的分享模型。

## 2. 方案（一期）

### 2.1 让工具返回分享 URL

- 新增 turn 级 task-local `TURN_ID`（与 12 项生图限流共用）。
- `present_artifact` / `create_artifact` 在 WebUI 注册路径直接调用 `StateStore::save_artifact_asset`（或经 `ToolProgress` 的新回调），把返回 JSON 扩展为：

```json
{
  "ok": true,
  "path": "/abs/local/path",
  "filename": "report.pdf",
  "asset_id": "art_...",
  "url": "/api/artifacts/art_...?download=1",
  "published": true
}
```

- `event_map` 的 `AgentEvent::Artifact` 保存路径保持，但增加去重/幂等：同 turn 同 path 重复 publish 复用 asset_id，不产生双记录。

### 2.2 下载语义

- `/api/artifacts/{asset_id}` 增加 `download` query：
  - `download=1` → 永远 `attachment`，并带正确 filename*。
  - 缺省维持现状（可预览类型 inline）。
- 对不可预览类型保持 attachment。
- 保留 CSP、nosniff、认证检查。

### 2.3 前端“复制分享链接”

- Artifact 操作菜单增加 `复制下载链接`，使用 `window.location.origin + artifact.url`，这样局域网用户拿到的就是完整地址。
- 回复消息中的 artifact 卡片可显示小型下载图标/链接。

### 2.4 认证模型（D8，推荐）

- 一期：**仍要求 WebUI 登录**。若用户 WebUI 设了密码，局域网访客需要密码；这是当前安全基线。
- 二期可选：`POST /api/artifacts/{id}/share` 生成短期随机 token，`GET /api/shared/{token}` 免登录但只读、TTL 默认 24h、可撤销；工具可返回该 token URL。默认关闭，需在 config 显式开启。

## 3. 不建议

- **不要**扩展现有 `/api/media` 成任意本地文件下载器：它目前只允许媒体扩展名，是安全边界；任意路径下载会把 WebUI 登录态变成整机文件读取面。
- 分享正确路径是 `present_artifact` 快照：文件复制进托管区，原文件后续变动不影响已分享版本，且不会泄漏路径。

## 4. 修改文件清单

- `src/tools/workspace.rs`：TURN_ID task-local。
- `src/tools/artifact.rs`：返回 asset_id/url，WebUI 注册路径接 StateStore。
- `src/state/assets.rs`：提供 `find_artifact_by_source` 或幂等保存接口。
- `src/web/assets.rs`：`download` query。
- `web/app.js`、`web/index.html`：复制链接按钮与分享说明。
- `src/web/event_map.rs`：幂等保存与事件字段。
- 配置：二期 share token 的开关/TTL。

## 5. 验收

1. WebUI 会话让 AI 分享 `/tmp/xxx.pdf` → 工具结果有 `/api/artifacts/...?download=1`。
2. 从局域网另一台机器访问完整 URL，登录后触发下载，文件名与 Content-Type 正确。
3. `download=1` 对 markdown 也强制 attachment；不带参数行为不回归。
4. 未登录访问仍 401（一期）。
5. 20 MiB 上限、文件名清洗、CSP、nosniff 测试不回归。
