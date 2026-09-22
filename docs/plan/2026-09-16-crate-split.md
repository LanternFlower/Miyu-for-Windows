# 拆 crate 施工单(2026-09-16,逆向边烧尽之后)

前置已满足:`test_scripts/arch_dep_check.py` 的 `TIERS` 层序下,生产代码里没有低层引高层
(白名单只剩 `agent/tests/shared.rs` 两行测试夹具)。按层切 crate 从此不会撞 Cargo 的循环依赖。

## 1. 目标与量尺

**目标**:开发编译从「改一行等整个 29 万行 crate 重编」变成「只重编改动的 crate 与其上游」;
`cargo test` 的峰值内存下降;release 体积与 REPL 常驻内存**不变**(变了就切 `lto = "fat"` 再量)。

**拆前基线(拆前必须先量,写进下表)**:

| 量尺(`scratchpad/measure_build.py`,峰值读 cgroup `memory.peak`;本机没有 `/usr/bin/time`) | 拆前 | 拆后 |
|---|---|---|
| 热身 `cargo check --all-targets` | 22.1 s / 峰值 4.7 GB | 1.6 s / 1.2 GB |
| 增量 check(改 agent 一行) | 18.3 s / 2.4 GB | **7.2 s** / 1.5 GB |
| 增量 build(改 cli 一行) | 42.2 s / 10.3 GB | **8.8 s** / 6.8 GB |
| 增量 build(改 tools 一行) | 29.1 s / 6.3 GB | **9.6 s** / 6.0 GB |
| `cargo test --no-run`(改 agent 一行) | 55.5 s / **18.4 GB** | 23.5 s / **20.0 GB(顶到 cgroup 上限,含页缓存;五个测试二进制并行链接)** |
| release 全量 | 峰值 7.7 GB;二进制 68,099,992 B,`size` text 66,456,971 | |
| REPL 常驻内存 | `testkit/low-footprint` 的量法 | |

全部套 `systemd-run --user --scope -p MemoryMax=20G -p MemorySwapMax=0`,一次只跑一个 cargo。

## 2. crate 划分(按层合并,五个)

| crate | 收哪些顶层模块(= `TIERS` 的层) | 行数 | `pub(crate)` |
|---|---|---|---|
| `miyu-base` | 基础 + 配置:i18n logging json_extract token_estimate token_counter prompts default_models provider_url memory_types platform_types host_info paths shell notify clipboard terminal question process durations media_mime http_response workspace sandbox tool_names random_id / config provider_catalog models_cache embedding host_ports | 29k | 666 |
| `miyu-core` | 存储与协议 + 子系统 + 传输:alarm ledger state llm / memory skills persona_hint / ipc args slash_commands | 55k | 528 |
| `miyu-engine` | 工具与引擎:tools voice transfer agent default_kb | 61k | 515 |
| `miyu-hosts` | 场所与展示:platforms runtime web daemon render | 91k | 1744 |
| `miyu`(bin) | 入口:cli config_tui oobe pm question_tui + `src/main.rs` + `src/bin/voice.rs` | 55k | 11 |

同一层里的模块互引不受限,所以层内环(`paths ↔ shell`、`config ↔ models_cache`、`tools ↔ agent`…)都不需要再动。
五个而不是八个:少四次 `pub(crate)` → `pub` 的边界工作,而并行度与增量收益主要来自把 hosts / 入口 与 engine / core 分开。

依赖方向:`miyu` → `miyu-hosts` → `miyu-engine` → `miyu-core` → `miyu-base`(每个 crate 也可以直接依赖更低的)。

## 3. 布局(定稿:根包 + `crates/`)

```
Cargo.toml                 仍是 `miyu` 包(入口层 + 两个 bin)——打包脚本读根 `[package].version`、PKGBUILD 的
                           `cargo build --release --locked` 与 `target/release/miyu` 一个字不用改;
                           同一文件里加 [workspace] members = ["crates/*"] 与 [workspace.dependencies]
src/                       入口层 rs(cli / config_tui / oobe / pm / question_tui、main.rs、bin/voice.rs、lib.rs)
                           + **所有非 rs 资源留在原处**(src/prompts/*.md、src/skills/*.md、src/memes、src/scripts、src/assets)
crates/miyu-base/{Cargo.toml,build.rs,src/…}   基础 + 配置层的 rs
crates/miyu-core/…         存储与协议 + 子系统 + 传输
crates/miyu-engine/…       工具与引擎(`voice` 特性住这里,根包 `voice = ["miyu-engine/voice"]`)
crates/miyu-hosts/…        场所与展示
```

只搬 `.rs`:资源不动,打包 / `paths::resources` 的 `src/memes`、`src/scripts` 约定全保留;搬走文件里的
`include_str!` / `include_bytes!` 相对路径由脚本按新位置重算(`../../../src/prompts/x.md` 这种)。
`build.rs` 两份:`crates/miyu-base/build.rs` 只烘资源(默认提示词 / o200k / jieba 进它的 `OUT_DIR`,只对资源文件 rerun);
构建 id 由根包 `build.rs` 算(唯一对 `src/ crates/ web/` 整树 rerun 的脚本),`miyu::run()` 一进来 `miyu_base::install_build_id`
装入,下层运行时读 `miyu_base::build_id()`(`ipc::build_id()` 转发;web 里 `concat!(env!)` 改运行时 `format!`)。
**第一版把 id 烘在 base 的 `OUT_DIR/build_id` 里,等于改任何一层都从 base 起全量重编——量尺前改掉。**
`paths::resources` 的开发态资源根改读 `MIYU_WORKSPACE_ROOT`(base 的 build.rs 从自己的 manifest 目录算出 workspace 根)。

- `voice` 特性:`[features] voice` 声明在 `miyu-engine`(`voice` 模块在那儿),`miyu` 的 `voice = ["miyu-engine/voice"]`,
  `miyu-voice` 二进制的 `required-features` 照旧。
- `#![cfg_attr(test, allow(dead_code))]` 每个 crate 根都要。
- 依赖表:先让五个 `Cargo.toml` 都列全今天的 84 个依赖(用 `workspace.dependencies` 统一版本),编译通过后再用
  `cargo udeps`(或逐个删试)收窄;未用依赖不会多编译,只是难看。

## 4. 步骤(脚本驱动,每步可回滚)

1. 量基线(§1 表),存检查点 `crate-split-pre`。
2. 脚本 `split_crates.py`(scratchpad):按 §2 表 `git mv` 模块;生成五份 `Cargo.toml`、五份 `lib.rs/main.rs`
   (原 `src/lib.rs` 的 `mod` 声明按归属拆开,`pub mod` 保持);把每个文件里的 `crate::<顶层模块>`
   改成所属 crate 的名字(`crate::config::` → `miyu_base::config::`,同 crate 内的不动);
   `use crate::{a, b, …}` 混着不同 crate 的模块的,拆成多条 `use`。
3. 可见性:`cargo check` 报 `E0603`(私有)/`E0616`(私有字段)的条目逐个 `pub(crate)` → `pub`、`pub(in crate::x)` → `pub`,
   直到零 error。这一步机械、次数多,**委派**;规则:只放宽编译器点名的条目,不批量替换。
4. 测试:`#[cfg(test)]` 的 `test_support` / `tests::shared` 若被别的 crate 的测试用到(现在没有,门禁量过),
   改成 `#[cfg(any(test, feature = "testkit"))] pub mod`,上游 crate 在 `[dev-dependencies]` 里开 `testkit` 特性。
5. 门禁跟着改:`refactor_size_report.py` 的 `SRC` 改成遍历 `crates/*/src`;`arch_dep_check.py` 的 `SRC` 同理,
   `TIERS` 保留(层内规则仍要管:memory / skills / persona_hint 互不 use);`refactor-check.sh` 的 `cargo` 命令加 `--workspace`;
   `.test-count` 基线按 workspace 总数重算(用例数不许少)。
6. 验收:`cargo check --workspace --all-targets` 零警告;`cargo test --workspace` 用例数 ≥ 拆前 2427;
   `refactor-check.sh` 全绿;`request_shape_probe` 五张脸逐字节相同;`cargo build` 后 `testkit/host-query/run.py` 9/9、
   `testkit/oobe/mcp_probe.py` 7/7;§1 表拆后一列填满,体积或内存涨了就切 `lto = "fat"` 复量。
7. 收尾:`cargo udeps` 收窄依赖;`docs/architecture.md` 加「九、crate」一节;`AGENTS.md` 里 `cargo` 命令若写死了单 crate 形态,只报不改。

## 5. 风险与回滚

- 最大的风险是第 3 步的量:hosts 有 1744 个 `pub(crate)`、1637 个 `pub(in crate::…)`,其中多少要放宽只有编译器知道;
  预计几百处,每轮 `cargo check` 一分钟内。
- 相对路径的资产引用与 `build.rs` 的 `rerun-if-changed=src` 语义(构建 id 随源码变)是两处容易静默出错的地方,
  验收里的端到端与 `BUILD_ID` 变化检查(改一行 web/app.js 重建,`miyu --version` 或 daemon 换代)专门盯它。
- 回滚 = 检查点 `crate-split-pre`(`git diff HEAD` + untracked tar);拆 crate 期间不合任何别的改动。

## 6. 施工记录(09-16/17)

**做了什么**:`split_crates.py` 按 TIERS 搬 .rs;`fix_visibility.py` 编译器驱动放宽 19 轮到零错误;警告清零
(test_support 模块改私有消歧、`#![allow(dead_code)]`、private_interfaces 点名放宽、`scripts` 再导出降 pub(crate)、
`runtime` 里 `random_token` 重复导出去掉);构建 id 挪到根包 `build.rs`(见 §3 说明,第一版烘在 base 让增量白拆);
下层 17 处 `cfg(test)` 行为开关改 `any(test, feature = "testkit")`,`CARGO_MANIFEST_DIR` 5 处改 `miyu_base::WORKSPACE_ROOT`,
hosts 补 `testkit` 特性、根包 dev-deps 打开;文档路径全改到 `crates/…`(AGENTS §8.2、architecture §9、wiki 06/14/15/18、docs/interfaces)。

**验证**(`scratchpad/post_split_validate.sh`,日志 `~/.cache/miyu-refactor-2026-09-16/logs/post-split-*.log`):
fmt 零改动;`cargo check --workspace --all-targets` 零警告;`cargo test --workspace` **2427 过 / 0 败**(与拆前相同);
refactor-check 五道门禁全绿(用例数 2427,总行数 291,905,无新增跨层引用);`request_shape_probe` 五张脸归一后逐字节相同
(唯一差异是探针进程 cwd 从仓库根变成 `crates/miyu-engine`,已纳入归一化);`cargo build` + host-query 9/9 + oobe 7/7;
改一行 `web/app.js` 重建后 `MIYU_BUILD_ID` 换代(1789571720… → 1789571792…);量尺见 §1 表。

**依赖收口**:`cargo udeps` 本机没装,用「源码里没出现过这个 crate 名」的启发式清了 `split_crates.py` 复制到每个 crate 的全量依赖:五个 Cargo.toml 共去掉 139 行(base 25、core 32、engine 28、hosts 22、根包 32),`cargo check --workspace --all-targets` 零错误零警告;`[workspace.dependencies]` 仍是版本真相源。

**没做 / 注意**:`cargo test --no-run` 峰值
顶到 20 GB 上限(五个测试二进制并行链接,含页缓存),要压可以 `-j 4` 或 `[profile.dev] debug = "line-tables-only"`,
这是配置决定,留给用户拍板;打包脚本零改动(`cargo build --release` → `target/release/miyu`)。
