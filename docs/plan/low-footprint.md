# 低占用专项（perf/low-footprint）

目标：在**不改变任何功能与语义**的前提下降低 Miyu 的内存 / CPU / GPU 占用。
原则沿用 08-18 专项：**没有测量数字不合并**；方案是嫌疑人，不是证词。

## 0. 实测基线（2026-08-20，本机 v0.4.4 线上环境）

| 进程 | RSS | 构成 | CPU |
|---|---|---|---|
| daemon | 81.7 MB | anon 40 MB + file 41.6 MB（二进制页） | 空闲窗口实测 0.4%（51min 累计 0.24%） |
| REPL（空载） | 53 MB | anon 26.5 MB + file 26.5 MB（与 daemon 共享物理页，PSS 36 MB） | 空闲 0 输出字节/0 重绘 |
| __renderer-worker | 30.5 MB | anon 9.4 MB + file 21.1 MB | 0%（渲染后常驻 1 小时才退） |
| `miyu daemon logs` | 16 MB | — | — |

二进制 62 MB：.text 41.4 MB / .rodata 11.3 MB / .eh_frame+gcc_except_table 5.4 MB。

daemon anon 40MB 的已归属部分：o200k BPE 表 6.4 MB（另 +1.8 MB rodata 全驻留）、
glibc 主 arena 7.6 MB、未归属 anon 16.5 MB（嫌疑：models.dev 3.6 MB JSON 全量解析×2、
每轮 `request_messages.clone()`、碎片；VmData 110 MB vs RssAnon 40 MB = 典型 arena 保留形态）。

GPU 结论（排除性调查）：daemon/CLI 进程**零 GPU**（纯 CPU 光栅化，kitty 缩放交给终端）。
三条使用路径的真实 GPU 面：
- REPL：空闲 0 重绘 → 0 GPU；**回合进行中** 40ms/25Hz spinner + 流式重绘 → 终端模拟器持续重画;
- WebUI：15 处 infinite CSS 动画 + 1 处 backdrop-filter，页面开着就持续合成（即 08-18 遗留的 G2）;
- shell-hook：短命进程，无常驻面。

## 1. 已裁定不做（历史结论，不再重提）

- AppConfig→Arc 全面快照：08-16 用户裁定永久搁置（61 处引用跨 6 文件，收益不抵风险）。
  本专项用"先判后拷/单点消除"绕开，不做全局 Arc。
- keepalive 快照 Arc 化、kitty blake3 去重、M4-6/7、C26 全量 spawn_blocking、C28/C31：08-18 已量，判不做。
- usage-history rollup：产品决定，不在本专项。

## 2. 三轮迭代

### 第 1 轮：零语义风险快赢（CPU 空闲 + 内存常数项）

| # | 项 | 位置 | 预期收益 | 验证 |
|---|---|---|---|---|
| 1.1 | 定时消息 worker 先判后拷（现状：每 20s 无条件深拷贝整份 AppConfig，QQ 关着也拷） | plugins/scheduled_messages/mod.rs:42-49 | 空闲 tick 成本≈0；分配抖动消失 | 单测 + 语义不变（真要发时行为逐字节一致） |
| 1.2 | ~~NapCat 心跳帧短路~~ **判不做**：心跳 5-15s 一帧、单帧全量解析仅数 µs，省它需 peek 双扫（真消息路径变慢）或文本嗅探（含特定字面量的真消息会被误杀），性价比为负 | onebot/connection.rs:505 | — | — |
| 1.3 | token_counter 正则池 128→按需（thread-local 或 8） | token_counter.rs:11,63 | −0.1 MB 直接 + 避免多份 lazy DFA cache（2MB/份上限） | count 结果差分不变 |
| 1.4 | renderer worker 空闲回收 1h→10min | renderer/worker.rs:13 | 渲一次图后 30 MB 提前 50 分钟归还 | 现有 worker 测试；超时后再渲染会自动重拉 |
| 1.5 | TurnResourceCache 槽 16→4 | runtime/state.rs:120 | 掐掉 12–29 MB 的内存悬崖上限 | 现有缓存测试；LRU 语义不变 |
| 1.6 | ~~出站图解码上限 256MB→64MB~~ **判不做**：该限额同时是允许发送的图片尺寸包络（64MB=4096×4096 RGBA），降了会拒发今天能发的大图（多列长文转图 78MB 形态首当其冲），属功能语义变化 | onebot/outbound.rs:16 | — | — |
| 1.5b | TurnResourceCache 槽定为 8（非 4）：留出会话路由/模型池分裂出的多 key 余量，仍掐掉悬崖上限一半 | runtime/state.rs | 上限 29MB→14MB | 同 1.5 |
| 1.7 | 删 syntect 依赖（src 全仓 0 引用） | Cargo.toml | 二进制/rodata 减重，待实测 | cargo build + 全量测试 |
| 1.8 | ConversationDb 显式 PRAGMA cache_size=-1024 | state/conversation_db/mod.rs:112 | 页缓存 2MB→1MB，查询规模（63 行/256 行）无感 | 现有 DB 测试 |
| 1.9 | main 入口 mallopt(M_ARENA_MAX, 2)（代码内，免打包改动） | main.rs | 地址空间/碎片收敛（VmData 110MB 形态） | 前后 smaps 对比 |

### 第 2 轮：活跃期 CPU + 分配抖动（小改代码，语义敏感点逐一钉住）

| # | 项 | 位置 | 预期收益 | 语义风险与对策 |
|---|---|---|---|---|
| 2.1 | SpinnerTick 分频：headless（daemon/平台回合/goal 轮）路径 40ms→200ms 或挂起；CLI 保持 40ms 不动 | turn_loop/mod.rs:169,764,857 + parallel.rs:105 | 活跃回合期间 80% 定时器唤醒消失（daemon 侧 tick 是空实现，仅给 journal 当节拍） | journal 自有 80ms+字节双阈值，200ms 兜底不劣化崩溃恢复；CLI 流式冲刷靠 tick，故 CLI 侧不动 |
| 2.2 | job 唤醒等待 500ms×10min lock-poll → 事件订阅/Notify | web/actor/job_wake.rs:100-111 | 唤醒等待期零轮询 | 订阅 run 完成事件，超时兜底保留 |
| 2.3 | 回合取消等待 100ms poll → Notify | web/turns/mod.rs:500-509 | 同上 | 同上 |
| 2.4 | models.dev 解析过滤前置（现状：3.6MB JSON 先建全表再裁，启动+后台线程各一遍） | models_cache/api.rs:123, mod.rs:76-153 | 峰值 −10–20 MB；REPL 空载堆直接受益 | 差分测试：裁剪后结果与现实现逐字段一致 |
| 2.5 | EventHub 加字节水位（现状 4096 条×2 份，无字节上限） | runtime/events.rs:22-60 | 流式高峰 1.6–10MB 有界化 | 重放语义：超水位丢最旧，与现有"条数满丢最旧"同语义 |
| 2.6 | VERSIONED_INDEX 去重复制、prompts XOR 解码缓存等零碎分配 | web/assets.rs:58, prompts.rs:8-18 | 小，顺手 | 无 |
| 2.7 | observe_ingress 移出 WS 读循环（顺序由既有 ingress_order 保证） | onebot/connection.rs:568-573 | 读循环不再被 SQLite 写事务往返卡住 | 需确认 ingress_order 消费侧完全不依赖到达序；不确定则降级为只把 store 往返改 send-only |

### 第 3 轮：结构性项（按第 1、2 轮实测结果与用户拍板取舍）

用户 08-21 拍板：3.1 做；3.3 做（codegen-units=1，不上 fat LTO）；3.4 **不做**（保持 panic 隔离语义）；3.5 做。

| # | 项 | 预期收益 | 备注 |
|---|---|---|---|
| 3.1 | 全局分配器换 mimalloc | RSS −15–30%（经验值，须 A/B 实测） | ✅拍板做，A/B 实测后合并 |
| 3.2 | o200k RankMap 键改 (u32,u16) 偏移 | −3–4 MB 常驻 | 单文件，差分测试护住 |
| 3.3 | codegen-units=1 | .text 5–15% | ✅拍板做，量完 size 不降就回退 |
| 3.4 | panic="abort" + 移除 catch_unwind | .text/.eh_frame −8–15 MB | ❌拍板不做 |
| 3.5 | alarm.wav/jieba.fst/o200k/web 资源外置 mmap | 二进制 −7.6 MB、驻留转 page cache | ✅拍板做，开发态从 CARGO_MANIFEST_DIR 回退 |
| 3.6 | G2 WebUI 合成层治理（document.hidden 暂停动画 + reduced-motion + content-visibility） | 页面挂后台时 GPU→0 | 08-18 留存方案照抄；验收需浏览器读数 |
| 3.7 | heaptrack 钉死剩余未归属 anon | 决定是否有第 4 轮 | 诊断性 |

## 2.5 执行记录（08-21）

第 1 轮落地：1.1 / 1.3（改 thread-local 惰性克隆，顺带删 ThreadId transmute hack）/ 1.4 / 1.5b(槽=8) / 1.7 / 1.8 / 1.9；1.2、1.6 判不做（见表）。1.7 实测：syntect 从未进最终二进制（LTO 早已剔除），删除对体积零影响，只省编译时间——诚实记录。

第 2 轮落地：2.1（Agent.spinner_interval 字段 + with_headless_pacing()，daemon 四处构造点挂上；CLI direct 不动）/ 2.2、2.3（ManagerState.runs_changed: Notify，finish_run 通知，notified() 先于条件检查注册堵死漏通知竞态）/ 2.4 的便宜半步（models_cache 两处解析后 trim_process_memory，该函数移到 runtime 供全进程用）/ 2.5（EventHub 4MB 字节水位，淘汰方向与条数满相同）。2.6、2.7 判不做（2.7：observe_ingress 出读循环会改变消息入库对同回合的可见时序，语义敏感）。全量测试 1637/0。

第 3 轮：
- 3.1 mimalloc **实测后不合并**：默认参数 daemon 落定 104MB（glibc 60MB）、HWM 105 vs 63.5；调参（PURGE_DELAY=0 等）后落定 58.5 但回合后 65.1 vs glibc 50.3——负载形态不合 + env 调参无法随包分发。数据在 testkit/low-footprint/results/。
- 3.3 codegen-units=1 落地：二进制 62.0→50.8MB(−18%)，.text −20%，release 编译 1m35s→5m36s。
- 3.2 o200k RankTable 落地：24B/词 HashMap → 12B 槽开放寻址+tag 字节+字节校验（无碰撞语义风险），8192 样本差分与全文档参考比对全绿，release 基准 21MB/s 与参考编码器同量级。
- G2 半落地：Xvfb+Chrome 实测**隐藏窗口合成负载与可见完全相同**（renderer 1.93%/gpu 0.77% 不变）→ pause-on-hidden 有真实价值；已实现 body.miyu-paused CSS + braille/jobs 两个 JS ticker 的 hidden 短路；后效待新二进制重测。
- 3.5 资源外置：**收益重估**——嵌入资产本来就是 file-backed 可回收页，外置只赢二进制体积不赢 RSS；排到最后做或与用户再确认。

A/B 实测（testkit/low-footprint/run.py，隔离 home+桩 LLM，5 流式回合；round3-cu1=第1+2轮+cu1，final=再加词表压缩+G2 资产）：
| 指标 | 基线 v0.4.4 | 第1+2轮 | +cu1 | final |
|---|---|---|---|---|
| daemon 落定 RSS | 62.8MB | 60.0MB | 46.0MB | 46.6MB |
| daemon 回合后 RSS | 51.7MB | 50.3MB | 48.3MB | 49.8MB |
| daemon 峰值 HWM | 67.4MB | 63.5MB | 61.0MB | **56.4MB** |
| REPL 回合后 RSS | 52.9MB | 52.6MB | 44.1MB | **43.6MB** |
| ctx-switch/流式回合 | 428 | 261 | 266 | 264 |
| 5 回合 CPU 秒 | 0.20 | 0.18 | 0.18 | 0.18 |

G2 终局（Xvfb+Chrome，30s 窗）：切走标签后 renderer 2.0→0.07%、gpu 0.93→0.13%，**新旧 CSS 完全相同**——是 Chrome 自带的隐藏标签节流,不是我们的 pause-on-hidden 起效；Xvfb 无 WM 无法复现最小化/遮挡场景。按「无测量数字不合并」原则,10 行防御性 pause-on-hidden(+2 处 JS ticker hidden 短路)的去留交用户验收时拍板。可见空闲的 renderer ~2%(软件渲染口径)来自侧栏状态点呼吸动画,属视觉设计,未动。backdrop-filter 仅 lightbox 打开时存在,空闲无成本。

3.5 资源外置**判不做（用户 08-21 拍板）**：嵌入资产本来就是 file-backed 可回收页,外置不降 RSS,只再减二进制 ~7.6MB,代价是动打包布局+破坏单文件自包含;嵌在二进制更方便。G2 的 10 行 pause-on-hidden 防御代码**用户拍板保留**。3.7 heaptrack 归因同理搁置(REPL anon 已从 26.4 降到 20.4MB,剩余大头是 BPE 表 3.4MB+正则+渲染缓存,归因边际收益低)。

## 3. 验证方法

- CPU：/proc/pid/stat jiffies 差分（空闲窗口 + 心跳注入窗口）；stub-llm + 隔离 MIYU_HOME 跑真实回合量活跃期。
- 内存：smaps_rollup 前后对比（anon/file 分开）；scaling probe（#[ignore]）钉单项；改前改后同场景。
- GPU：REPL 用 PTY 量空闲/活跃输出字节速率（重绘次数代理指标）；WebUI 用浏览器任务管理器/chrome://gpu 前后读数。
- 语义：全量 cargo test 基线对齐；字节敏感路径（请求组装、出站文本）差分测试。
