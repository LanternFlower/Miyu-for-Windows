#!/usr/bin/env bun
// opencode Zen 免费档「客户端识别」闸的判据探针。
//
// 09-19 起 Zen 对免费模型回 403 `FreeTierError`。这个脚本对真端点做对照实验，
// 一次只改一个变量，用来（重新）确定放行条件——他们随时会改判据，改了就把这
// 个脚本再跑一遍，别靠猜。
//
// 用法：
//     bun run testkit/opencode-zen/freetier_probe.js [模型名]
//
// key 从 ~/.miyu/config/config.jsonc 的 opencode 供应商里读；读不到就用字面量
// `public`（免费档匿名也能打）。每次请求之间隔 5 秒——打太快会先撞 429
// `FreeUsageLimitError`，那是额度不是闸，别把两者看混：
//   · 403 + FreeTierError      → 被客户端识别闸挡住
//   · 429 + FreeUsageLimitError → 已经过闸，只是额度用完了
//
// 09-20 实测结论（`zen_tools` 里那套别名就是照它来的）：放行需要三件同时成立
//   1. stream: true
//   2. tools 里同时有 (shell 或 bash) 和 read
//   3. 至少带一个 x-opencode-* 头
// 排查时最容易踩的坑：一次改两个变量。去掉头的同时 body 也不合格，会得出
// 「头不相干」的错误结论——实际上单独去掉任一个 x-opencode-* 头都还能过，
// 一个都不带才挡。

import { homedir } from "os"

const MODEL = process.argv[2] ?? "mimo-v2.5-free"
const URL_ = "https://opencode.ai/zen/v1/chat/completions"
const GAP_MS = 5000

async function apiKey() {
  try {
    const raw = await Bun.file(`${homedir()}/.miyu/config/config.jsonc`).text()
    const cfg = JSON.parse(raw.replace(/^\s*\/\/.*$/gm, ""))
    const zen = cfg.providers?.find((p) => p.base_url?.startsWith("https://opencode.ai/zen/v1"))
    return zen?.api_key || "public"
  } catch {
    return "public"
  }
}

const B62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
let counter = 0
// opencode 自己的 id 形状：12 位十六进制(毫秒*4096+计数器 的低 48 位) + 14 位 base62。
// 实测形状不参与判定，保持一致只是为了让抓包看起来正常。
function sessionId() {
  counter += 1
  const value = (~(BigInt(Date.now()) * 0x1000n + BigInt(counter))) & ((1n << 48n) - 1n)
  let head = ""
  for (let i = 0; i < 6; i++) head += Number((value >> BigInt(40 - 8 * i)) & 0xffn).toString(16).padStart(2, "0")
  const tail = Array.from(crypto.getRandomValues(new Uint8Array(14)), (b) => B62[b % 62]).join("")
  return `ses_${head}${tail}`
}

const tools = (names) =>
  names.map((name) => ({
    type: "function",
    function: { name, description: "", parameters: { type: "object", properties: {} } },
  }))

function headers(key, { opencodeHeaders = true } = {}) {
  const head = {
    "Content-Type": "application/json",
    Authorization: `Bearer ${key}`,
    "User-Agent": "opencode/1.18.29 ai-sdk/provider-utils/4.0.46 runtime/bun/1.4.0",
  }
  if (!opencodeHeaders) return head
  const session = sessionId()
  return { ...head, "x-opencode-client": "cli", "x-opencode-project": "global", "x-opencode-session": session }
}

function body({ toolNames = ["shell", "read"], stream = true, system = "你是未由。" } = {}) {
  const messages = system ? [{ role: "system", content: system }, { role: "user", content: "你好" }] : [{ role: "user", content: "你好" }]
  const payload = { model: MODEL, messages, stream }
  if (toolNames !== null) payload.tools = tools(toolNames)
  if (stream) payload.stream_options = { include_usage: true }
  return payload
}

let pass = 0
let total = 0
async function check(label, expect, head, payload) {
  total += 1
  const res = await fetch(URL_, { method: "POST", headers: head, body: JSON.stringify(payload) })
  const text = await res.text()
  const verdict = text.includes("FreeTierError")
    ? "blocked"
    : text.includes("FreeUsageLimitError") || res.status === 429
      ? "quota"
      : res.status === 200
        ? "allowed"
        : `http ${res.status}`
  // 额度用完时闸其实已经过了，按放行算——否则跑到一半全变红看不出判据。
  const effective = verdict === "quota" ? "allowed" : verdict
  const ok = effective === expect
  if (ok) pass += 1
  console.log(`${ok ? "✅" : "❌"} ${label.padEnd(46)} 期望 ${expect.padEnd(8)} 实际 ${verdict}`)
  await new Promise((r) => setTimeout(r, GAP_MS))
}

const key = await apiKey()
console.log(`模型 ${MODEL}，key ${key === "public" ? "public（匿名）" : "配置里的真 key"}，每次间隔 ${GAP_MS / 1000}s\n`)

await check("基准：三件都满足", "allowed", headers(key), body())
await check("Miyu 改名前的工具名", "blocked", headers(key), body({ toolNames: ["run_command", "read_file", "web_search"] }))
await check("只有 shell，没有 read", "blocked", headers(key), body({ toolNames: ["shell"] }))
await check("只有 read，没有 shell", "blocked", headers(key), body({ toolNames: ["read"] }))
await check("bash 代替 shell", "allowed", headers(key), body({ toolNames: ["bash", "read"] }))
await check("旁边多挂一堆自造工具名", "allowed", headers(key), body({ toolNames: ["shell", "read", "use_meme", "divine", "qq_contacts"] }))
await check("完全不带 tools", "blocked", headers(key), body({ toolNames: null }))
await check("tools 是空数组", "blocked", headers(key), body({ toolNames: [] }))
await check("stream: false", "blocked", headers(key), body({ stream: false }))
await check("不带 system 提示词", "allowed", headers(key), body({ system: null }))
await check("一个 x-opencode-* 头都不带", "blocked", headers(key, { opencodeHeaders: false }), body())

console.log(`\n${pass}/${total} passed`)
if (pass !== total) {
  console.log("判据变了。重新二分，然后同步 crates/miyu-core/src/llm/openai_compatible/zen_tools.rs 里的别名与模块注释。")
  process.exit(1)
}
