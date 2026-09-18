//! 可展开块的登记处。
//!
//! 全屏下点一块折叠的内容就能摊开。摊开的那份**不进字节流**——命令输出动辄几十
//! 行，每块都塞一遍既费带宽又会把历史撑大；流里只放一个 id，内容留在进程里按 id
//! 取。渲染方和全屏后端本来就在同一个进程，没必要绕终端一圈。
//!
//! inline 模式下整套不启用：`enabled()` 为假时 `register` 直接返回 `None`，一个
//! 标记字节都不会多出来，真终端看到的输出和以前逐字节相同。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// 登记处最多攥多少**行**。超出就丢最老的块——回翻到很久以前的块点不开，
/// 总好过把整段会话的命令输出都留在内存里（低占用是这次重构的前提）。
///
/// 按行而不是按块计数：一块可能是一行摘要，也可能是上百行命令输出，按块限
/// 根本限不住。
///
/// 09-17 从 4000 抬到 16000：一份编辑步的 diff 就有两三千行（本机会话库实测最
/// 大 3084 行），4000 的额度一落地就占掉四分之三，之后每次登记都在淘汰别人——
/// 自动展开的思考块静默收起、正在跑的块停更（BUG-07 初诊 §E）。一行按百来字节
/// 算，16000 行也就一两兆，比"点开的东西莫名消失"划算。
const MAX_LINES: usize = 16_000;

static ENABLED: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 一块的内容。`overlay` 为真表示它**不在正文里就地展开**，而是点开一个盖住
/// 整屏的面板——子代理那种「里面还在流」的东西塞进正文行里没法看。
struct Entry {
    lines: Vec<String>,
    overlay: bool,
    /// 覆盖层的标题栏文字。就地展开的块用不上。
    title: String,
    /// 每次更新 +1。覆盖层开着时靠它判断要不要重画。
    version: u64,
    /// 最后一次被登记／更新／读取的序号。淘汰按它来，见 [`evict`]。
    touched: u64,
}

fn registry() -> &'static Mutex<HashMap<u64, Entry>> {
    static REGISTRY: std::sync::OnceLock<Mutex<HashMap<u64, Entry>>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

// 测试里这个开关必须是**线程局部**的：一个进程一个前端，产品里全局就够，
// 但 `cargo test` 是多线程并发跑的，一个用例把它打开会让同时在跑的别的用例
// 改变行为（时间线开了之后命令块、正文缩进全都不一样）。
#[cfg(any(test, feature = "testkit"))]
thread_local! {
    static ENABLED_LOCAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 全屏后端进出场时开关。关掉时顺手清空，免得退出全屏后还攥着历史输出。
pub fn set_enabled(on: bool) {
    #[cfg(any(test, feature = "testkit"))]
    ENABLED_LOCAL.with(|flag| flag.set(on));
    ENABLED.store(on, Ordering::Relaxed);
    // 测试里不清：登记处是进程共享的，清掉会把并发跑的别的用例一起端了。
    #[cfg(not(any(test, feature = "testkit")))]
    if !on {
        if let Ok(mut map) = registry().lock() {
            map.clear();
        }
    }
}

pub fn enabled() -> bool {
    #[cfg(any(test, feature = "testkit"))]
    return ENABLED_LOCAL.with(std::cell::Cell::get);
    #[cfg(not(any(test, feature = "testkit")))]
    ENABLED.load(Ordering::Relaxed)
}

/// 存一份展开内容，拿到它的 id。没开全屏就什么都不做。
pub fn register(lines: Vec<String>) -> Option<u64> {
    insert(lines, false)
}

/// 登记一块「点开是覆盖层」的内容。允许先登记空的，之后用 [`update`] 往里灌。
pub fn register_overlay(title: String, lines: Vec<String>) -> Option<u64> {
    if !enabled() {
        return None;
    }
    insert_entry(lines, true, title)
}

fn insert(lines: Vec<String>, overlay: bool) -> Option<u64> {
    if !enabled() || lines.is_empty() {
        return None;
    }
    insert_entry(lines, overlay, String::new())
}

fn insert_entry(lines: Vec<String>, overlay: bool, title: String) -> Option<u64> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut map = registry().lock().ok()?;
    map.insert(
        id,
        Entry {
            lines,
            overlay,
            title,
            version: 0,
            touched: touch(),
        },
    );
    evict(&mut map);
    Some(id)
}

/// 往一块里灌新内容（子代理边跑边更新）。标题跟着一起更新——它带着耗时。
pub fn update(id: u64, title: String, lines: Vec<String>) {
    let Ok(mut map) = registry().lock() else {
        return;
    };
    if let Some(entry) = map.get_mut(&id) {
        entry.lines = lines;
        if !title.is_empty() {
            entry.title = title;
        }
        entry.version = entry.version.wrapping_add(1);
        entry.touched = touch();
    }
    evict(&mut map);
}

/// 覆盖层的标题。
pub fn title(id: u64) -> Option<String> {
    let title = registry().lock().ok()?.get(&id)?.title.clone();
    (!title.is_empty()).then_some(title)
}

/// 淘汰按**最久没碰过**来，不是按 id 从小到大。
///
/// 按 id 淘汰看着等价（id 单调递增，小的就是老的），其实正好挑中最要命的那个：
/// 子代理的覆盖层块在它跑起来的第一刻就登记了，id 最小；它整场都在被更新、还
/// 可能正开在屏幕上，却总是第一个被端掉——表现出来就是"面板不刷新了""工具行
/// 点不开了"（用户实测）。一直在用的块不该被当成最老的。
fn evict(map: &mut HashMap<u64, Entry>) {
    let mut total: usize = map.values().map(|entry| entry.lines.len()).sum();
    while total > MAX_LINES && map.len() > 1 {
        let Some(stalest) = map
            .iter()
            .min_by_key(|(id, entry)| (entry.touched, **id))
            .map(|(id, _)| *id)
        else {
            break;
        };
        total -= map.remove(&stalest).map_or(0, |entry| entry.lines.len());
    }
}

/// 单调递增的"碰过"序号。
fn touch() -> u64 {
    static TOUCH: AtomicU64 = AtomicU64::new(1);
    TOUCH.fetch_add(1, Ordering::Relaxed)
}

pub fn get(id: u64) -> Option<Vec<String>> {
    let mut map = registry().lock().ok()?;
    let stamp = touch();
    let entry = map.get_mut(&id)?;
    entry.touched = stamp;
    Some(entry.lines.clone())
}

/// 这一块点开是覆盖层还是就地展开。
pub fn is_overlay(id: u64) -> bool {
    registry()
        .lock()
        .ok()
        .and_then(|map| map.get(&id).map(|entry| entry.overlay))
        .unwrap_or(false)
}

/// 一批块的内容版本号，一把锁问完。不在登记处的给 0（和 [`version`] 一致）。
pub fn versions(ids: &[u64]) -> Vec<u64> {
    let Ok(map) = registry().lock() else {
        return vec![0; ids.len()];
    };
    ids.iter()
        .map(|id| map.get(id).map(|entry| entry.version).unwrap_or(0))
        .collect()
}

/// 内容版本号。覆盖层开着时用它判断要不要重画。
pub fn version(id: u64) -> u64 {
    registry()
        .lock()
        .ok()
        .and_then(|map| map.get(&id).map(|entry| entry.version))
        .unwrap_or(0)
}

/// 块的起止标记。用私有 OSC：真终端不认得就整条吞掉，不会漏字符上屏——
/// inline 下本来也不会发出来，这只是双保险。
pub fn begin_marker(id: u64) -> String {
    format!("\x1b]1337;miyu-block={id}\x07")
}

/// 「这一块默认是**开着**的」。
///
/// `显示思考过程 = 完整` / `显示工具调用信息 = 完整` 要的就是这个：那一步出来
/// 就是展开态，不用点，再点一次才收回去（用户 09-17：「不用我点击他的 tag 行，
/// 他出来就是展开的效果」）。
///
/// 它**不是**另一种块：登记处存的内容、点击的开合、嵌套全都一样，区别只在视图
/// 第一次见到这个 id 时要不要先替它开一次。所以做成标记的一个变体，而不是给
/// `Entry` 加状态——展开态本来就只活在视图那一侧（`screen::expand::Expanded`），
/// 登记处存的是内容。
pub fn begin_marker_open(id: u64) -> String {
    format!("\x1b]1337;miyu-block-open={id}\x07")
}

/// 按「默认开着吗」挑一个起始标记。
pub fn begin_marker_in(id: u64, open: bool) -> String {
    if open {
        begin_marker_open(id)
    } else {
        begin_marker(id)
    }
}

pub const END_MARKER: &str = "\x1b]1337;miyu-block-end\x07";

/// 「一轮从这儿开始」的标记：提交回显、回放里每一轮开头各埋一个。全屏后端记下
/// 那一刻的行号，`/undo` 把正文缓冲截回去——只截撤掉的那一轮，前面的滚动历史
/// 原样留着（用户 09-18：整段回放会把往上翻的历史丢掉）。真终端不认得就整条吞掉。
pub const TURN_START_MARKER: &str = "\x1b]1337;miyu-turn-start\x07";

/// 「一块压缩结果从这儿开始」的标记:`/compact` 在全屏里写那块「上下文已压缩」
/// 之前埋一个。`/undo` 撤掉压缩时按它把那一块截掉——它不是一轮,不能拿轮标记截
/// (会把前一轮真正的对话一起截掉),也不能留着(用户 09-18:撤了它还在,还能点开)。
pub const COMPACT_START_MARKER: &str = "\x1b]1337;miyu-compact-start\x07";

/// OSC 载荷 → 块 id。`Term` 解析时用。
pub fn parse_marker(payload: &str) -> Option<BlockMarker> {
    if payload == "miyu-block-end" {
        return Some(BlockMarker::End);
    }
    if payload == "miyu-turn-start" {
        return Some(BlockMarker::TurnStart);
    }
    if payload == "miyu-compact-start" {
        return Some(BlockMarker::CompactStart);
    }
    if let Some(id) = payload.strip_prefix("miyu-block-open=") {
        return id
            .parse()
            .ok()
            .map(|id| BlockMarker::Begin { id, open: true });
    }
    payload
        .strip_prefix("miyu-block=")
        .and_then(|id| id.parse().ok())
        .map(|id| BlockMarker::Begin { id, open: false })
}

pub enum BlockMarker {
    Begin {
        id: u64,
        /// 视图第一次见到它时先替用户开一次。见 [`begin_marker_open`]。
        open: bool,
    },
    End,
    /// 一轮从这儿开始。见 [`TURN_START_MARKER`]。
    TurnStart,
    /// 一块压缩结果从这儿开始。见 [`COMPACT_START_MARKER`]。
    CompactStart,
}

/// 一段纯文本按块的样式切成行。空块返回空 `Vec`，`register` 那边会当作
/// 「没什么可展开的」跳过。
pub(crate) fn expandable_text_lines(text: &str, style: crate::render::SummaryStyle) -> Vec<String> {
    if !enabled() || text.trim().is_empty() {
        return Vec::new();
    }
    text.lines()
        .map(|line| crate::render::style_summary_text(line, style))
        .collect()
}

/// 摘要行 + 详情，拼成展开后的整块。
///
/// 头行要留着：展开之后它还是这块的把手，点它就收起来；只给详情的话用户
/// 会以为摘要那行被吃掉了。详情为空就返回空——没东西可展开。
pub fn expand_under(
    summary: &str,
    detail: Vec<String>,
    style: crate::render::SummaryStyle,
) -> Vec<String> {
    if !enabled() || detail.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![crate::render::style_summary_text(summary, style)];
    lines.extend(detail);
    lines.push(String::new());
    lines
}

/// 把一块内容包起来写出去：折叠的那份照常上屏，展开的那份只留 id。
pub fn write_expandable<W: std::io::Write>(
    writer: &mut W,
    expanded: Vec<String>,
    collapsed: impl FnOnce(&mut W) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let id = register(expanded);
    if let Some(id) = id {
        write!(writer, "{}", begin_marker(id))?;
    }
    collapsed(writer)?;
    if id.is_some() {
        write!(writer, "{END_MARKER}")?;
    }
    Ok(())
}
