//! 点击展开：折叠的块点一下摊开，再点收起，**可以嵌套**。
//!
//! 展开**不改缓冲**。`Term` 里那份是「终端真的收到过什么」，改了它光标行号、
//! 存档边界、正在流入的那一行全要跟着算，得不偿失。这里在缓冲之上加一层视图
//! 映射：视图行 = 缓冲行 + 前面所有已展开块撑出来的行数。折叠回去只是把映射
//! 撤掉，缓冲从头到尾没动过。
//!
//! **嵌套**靠一个巧劲：展开内容本身也是一段 ANSI，里面照样可以带块标记。把它
//! 喂进一个临时 [`Term`]，拿回来的就是同样的「行 + 块区间」结构——于是展开一层
//! 和展开顶层走的是同一套代码，递归下去就行。`Worked for 12s · 3 tools` 展开成
//! 时间线、时间线里每一项再展开看详情，就是这么来的。
//!
//! 画面、选区、点击都只认视图行号，[`Screen::view_row`] 是唯一的取行入口。

use super::ansi::AnsiSpan;
use super::term::{BlockSpan, Term};
use super::Screen;
use std::collections::HashMap;

/// 一段内容：行，加上它内部的块区间。顶层是 `Term` 自己，展开出来的每一段
/// 也是这个形状——所以能递归。
pub(in crate::cli) struct Body {
    rows: Vec<Vec<AnsiSpan>>,
    blocks: Vec<BlockSpan>,
    /// 取到的是登记处的哪一版。内容还在长的块（正在想的那一步）要靠它知道
    /// 该重取了——展开着却一直显示刚点开那一瞬的内容，等于没在流。
    version: u64,
}

impl Body {
    /// 把一段 ANSI 解析成行 + 块。复用终端模拟器，不另写解析。
    ///
    /// `cols` 必须是**画面真实宽度**：临时缓冲的默认宽度和屏幕不一样的话，
    /// 展开出来的长行会按错误的宽度折，看着像随机断行。
    pub(in crate::cli) fn parse(text: &str, cols: usize) -> Self {
        let mut term = Term::default();
        term.set_cols(cols);
        term.feed(text.as_bytes());
        let rows = (0..term.line_count())
            .map(|index| term.row_spans(index))
            .collect();
        Self {
            rows,
            blocks: term.blocks().to_vec(),
            version: 0,
        }
    }
}

/// 已展开的块：id → 解析好的内容。
pub(in crate::cli) type Expanded = HashMap<u64, Body>;

/// 一层内容的只读视图。顶层看 `Term`，展开层看 `Body`——两者行为一致，
/// 递归代码只写一遍。
pub(in crate::cli) enum Layer<'a> {
    Live(&'a Term),
    Body(&'a Body),
}

impl Layer<'_> {
    fn len(&self) -> usize {
        match self {
            // 用 `filled_rows` 而不是 `content_rows`：两者只差"光标停在正文
            // 末尾几行空白之下"的那几行。把它们算进视图的话，跟随会一路滚到
            // 它们那儿，屏幕底下于是空出几行，正文被顶得老高。
            Layer::Live(term) => term.filled_rows(),
            Layer::Body(body) => body.rows.len(),
        }
    }

    fn row(&self, index: usize) -> Vec<AnsiSpan> {
        match self {
            Layer::Live(term) => term.row_spans(index),
            Layer::Body(body) => body.rows.get(index).cloned().unwrap_or_default(),
        }
    }

    fn blocks(&self) -> &[BlockSpan] {
        match self {
            Layer::Live(term) => term.blocks(),
            Layer::Body(body) => &body.blocks,
        }
    }
}

/// 这一层展开后一共多少行。
pub(in crate::cli) fn layer_len(layer: &Layer, expanded: &Expanded) -> usize {
    // 一个块都没展开时长度就是原样。不早退的话这儿要把**所有**块走一遍、
    // 每个做一次哈希查找——而这个函数每帧要调好几次（算滚动上限、算贴底垫多少）。
    // 聊久了缓冲里攒着几百个块，这一下就是几千次无用功。
    if expanded.is_empty() {
        return layer.len();
    }
    let mut total = layer.len();
    for block in layer.blocks() {
        let Some(body) = expanded.get(&block.id) else {
            continue;
        };
        let collapsed = block.end.saturating_sub(block.start);
        total = total
            .saturating_add(layer_len(&Layer::Body(body), expanded))
            .saturating_sub(collapsed);
    }
    total
}

/// 递归取行。`index` 是这一层展开后的行号。
pub(in crate::cli) fn layer_row(layer: &Layer, expanded: &Expanded, index: usize) -> Vec<AnsiSpan> {
    // 同 `layer_len`：没展开就没有偏移可算。这个函数**每行每帧**都要调。
    if expanded.is_empty() {
        return layer.row(index);
    }
    let mut offset = 0usize;
    for block in layer.blocks() {
        let Some(body) = expanded.get(&block.id) else {
            continue;
        };
        let start = block.start.saturating_add(offset);
        if index < start {
            break;
        }
        let inner = Layer::Body(body);
        let height = layer_len(&inner, expanded);
        if index < start.saturating_add(height) {
            return layer_row(&inner, expanded, index - start);
        }
        let collapsed = block.end.saturating_sub(block.start);
        offset = offset.saturating_add(height).saturating_sub(collapsed);
    }
    layer.row(index.saturating_sub(offset))
}

/// 这一块展开之后里面还有别的块吗？
///
/// 有（`Worked for …` 展开成一条时间线）就只有**表头**可点：里面每一项都是
/// 自己的把手，整片都能收起来的话，想点开某一步反而把整条线收没了。
/// 没有（一步的详情正文）就整片可点——那一整块讲的是同一件事。
fn has_children(body: &Body) -> bool {
    !body.blocks.is_empty()
}

/// 递归命中测试。返回**最内层**命中的块——嵌套时点到哪一层就收哪一层。
pub(in crate::cli) fn layer_hit(
    layer: &Layer,
    expanded: &Expanded,
    index: usize,
) -> Option<(u64, usize)> {
    // 没展开时块的位置就是它自己的位置，直接二分。线性扫的代价很具体：
    // 鼠标悬浮和选区都按行调它，一屏三十几行、缓冲里几百个块，一帧就是上万次
    // 比较——AI 正在输出时两边叠在一起，手上就是"选文本好卡"。
    if expanded.is_empty() {
        let blocks = layer.blocks();
        let found = blocks.partition_point(|block| block.start <= index);
        let block = blocks.get(found.checked_sub(1)?)?;
        return (index < block.end).then_some((block.id, block.start));
    }
    let mut offset = 0usize;
    for block in layer.blocks() {
        let start = block.start.saturating_add(offset);
        if index < start {
            return None;
        }
        let collapsed = block.end.saturating_sub(block.start);
        match expanded.get(&block.id) {
            Some(body) => {
                let inner = Layer::Body(body);
                let height = layer_len(&inner, expanded);
                if index < start.saturating_add(height) {
                    // 先问内层：点在嵌套块上就收那一个。
                    if let Some(hit) = layer_hit(&inner, expanded, index - start) {
                        return Some((hit.0, start.saturating_add(hit.1)));
                    }
                    // 表头永远可点：它是把手。
                    if index == start {
                        return Some((block.id, start));
                    }
                    // 里面没有别的块时整片都算这一块——**空行也算**。
                    //
                    // 不算的话，鼠标在展开的正文里扫过空行时高亮一闪一闪，点上去
                    // 还没反应；可那一片明明看着就是一整块（用户原话「在思考内容的
                    // 空行的地方会是非交互状态，导致鼠标划过会一闪一闪的」）。
                    if !has_children(body) {
                        return Some((block.id, start));
                    }
                    return None;
                }
                offset = offset.saturating_add(height).saturating_sub(collapsed);
            }
            None => {
                if index < start.saturating_add(collapsed) {
                    return Some((block.id, start));
                }
            }
        }
    }
    None
}

/// 展开一块：从登记处取内容、解析成 `Body`。取不到就没得展开。
pub(in crate::cli) fn load_body(id: u64, cols: usize) -> Option<Body> {
    let lines = miyu_hosts::render::blocks::get(id)?;
    if lines.is_empty() {
        return None;
    }
    // 不补结尾换行：补了会在缓冲里多出一行空的。
    let mut body = Body::parse(&lines.join("\r\n"), cols);
    body.version = miyu_hosts::render::blocks::version(id);
    (!body.rows.is_empty()).then_some(body)
}

/// 在一份 `expanded` 表上开合一块。返回真表示视图变了。
/// 正文和覆盖层用的是同一套，各自带自己的表。
pub(in crate::cli) fn toggle_in(expanded: &mut Expanded, id: u64, cols: usize) -> bool {
    if expanded.remove(&id).is_some() {
        return true;
    }
    match load_body(id, cols) {
        Some(body) => {
            expanded.insert(id, body);
            true
        }
        None => false,
    }
}

/// 把展开着的那几块的内容重取一遍（块还在长的时候用）。
///
/// `Expanded` 存的是**点开那一刻的快照**：块后来被更新，屏幕上那一片还是旧的。
/// 面板里的子代理一边跑一边写，不重取就等于点开之后冻住了。取不到的（块被
/// 淘汰了）直接丢掉。
pub(in crate::cli) fn reload_expanded(expanded: &mut Expanded, cols: usize) {
    if expanded.is_empty() {
        return;
    }
    let ids = expanded.keys().copied().collect::<Vec<_>>();
    for id in ids {
        match load_body(id, cols) {
            Some(body) => {
                expanded.insert(id, body);
            }
            None => {
                expanded.remove(&id);
            }
        }
    }
}

/// 这一行是不是**展开出来的内容**。画暗底用：展开的那一片和正文得分得开。
/// 面板用：这一行是不是它自己那张展开表里展开出来的内容。
pub(in crate::cli) fn body_in_expansion(body: &Body, expanded: &Expanded, index: usize) -> bool {
    layer_in_expansion(&Layer::Body(body), expanded, index)
}

fn layer_in_expansion(layer: &Layer, expanded: &Expanded, index: usize) -> bool {
    // 没展开就不可能"在展开区里"。少了这一句，画每一行都要把所有块走一遍。
    if expanded.is_empty() {
        return false;
    }
    let mut offset = 0usize;
    for block in layer.blocks() {
        let Some(body) = expanded.get(&block.id) else {
            continue;
        };
        let start = block.start.saturating_add(offset);
        if index < start {
            return false;
        }
        let inner = Layer::Body(body);
        let height = layer_len(&inner, expanded);
        if index < start.saturating_add(height) {
            // 只有**叶子块**画底。`Worked for` 展开出来的是一条时间线——给整条
            // 加底等于把正文一大片染色，喧宾夺主；真正需要"这是一整块"的是点开
            // 某一步之后那一片。里面还有块就继续往里问。
            if has_children(body) {
                return layer_in_expansion(&inner, expanded, index - start);
            }
            return true;
        }
        let collapsed = block.end.saturating_sub(block.start);
        offset = offset.saturating_add(height).saturating_sub(collapsed);
    }
    false
}

impl Screen {
    /// 这一视图行是不是展开出来的内容。
    pub(in crate::cli) fn in_expansion(&self, index: usize) -> bool {
        layer_in_expansion(&self.layer(), &self.expanded, index)
    }

    fn layer(&self) -> Layer<'_> {
        Layer::Live(&self.term)
    }

    /// 视图一共多少行。
    pub(in crate::cli) fn view_len(&self) -> usize {
        layer_len(&self.layer(), &self.expanded)
    }

    /// 缓冲行 → 视图行。只对块**之外**的行有意义（块内的行折叠时压根不存在）。
    pub(in crate::cli) fn view_of(&self, buffer_row: usize) -> usize {
        if self.expanded.is_empty() {
            return buffer_row;
        }
        let mut offset = 0usize;
        for block in self.term.blocks() {
            if block.start >= buffer_row {
                break;
            }
            let Some(body) = self.expanded.get(&block.id) else {
                continue;
            };
            let collapsed = block.end.saturating_sub(block.start);
            offset = offset
                .saturating_add(layer_len(&Layer::Body(body), &self.expanded))
                .saturating_sub(collapsed);
        }
        buffer_row.saturating_add(offset)
    }

    /// 取视图里的一行。
    pub(in crate::cli) fn view_row(&self, index: usize) -> Vec<AnsiSpan> {
        layer_row(&self.layer(), &self.expanded, index)
    }

    /// 视图行落在哪一块里（最内层）。返回块 id 和它在视图里的起始行。
    pub(in crate::cli) fn block_at(&self, index: usize) -> Option<(u64, usize)> {
        layer_hit(&self.layer(), &self.expanded, index)
    }

    /// 展开 / 收起。返回真表示视图变了，得重画。
    ///
    /// 展开时**钉住视口**：块的头行留在原处，后面的内容往下撑。
    ///
    /// 收起之后跟随要能**自己回来**。写死 `follow = false` 的代价很隐蔽：
    /// 点开又收起以后视口就再也不跟新输出了，命令结果落在视口下面，
    /// 看起来像「命令没反应」。按「是不是已经贴底」重算才对。
    pub(in crate::cli) fn toggle_block(&mut self, id: u64) -> bool {
        // 展开前在底部，展开后还该在底部——否则新撑出来的几十行会把**正文**
        // 顶到视口下面去，看着像"一展开正文就没了"。不在底部时才钉住原位。
        let following = self.following();
        if self.expanded.remove(&id).is_some() {
            self.restore_follow(following);
            self.invalidate();
            return true;
        }
        let Some(body) = load_body(id, usize::from(self.cols)) else {
            return false;
        };
        self.expanded.insert(id, body);
        self.restore_follow(following);
        self.invalidate();
        true
    }

    /// 展开着的块如果内容变了就重取。正在想的那一步点开之后要能**继续**流，
    /// 不然点开的一瞬间就定格了。
    pub(in crate::cli) fn refresh_expanded(&mut self) -> bool {
        let stale: Vec<u64> = self
            .expanded
            .iter()
            .filter(|(id, body)| miyu_hosts::render::blocks::version(**id) != body.version)
            .map(|(id, _)| *id)
            .collect();
        if stale.is_empty() {
            return false;
        }
        let cols = usize::from(self.cols);
        for id in stale {
            match load_body(id, cols) {
                Some(body) => {
                    self.expanded.insert(id, body);
                }
                None => {
                    self.expanded.remove(&id);
                }
            }
        }
        self.invalidate();
        true
    }

    /// 丢掉已经不在缓冲里的块的展开内容——留着只是占内存，视图早看不见了。
    pub(in crate::cli) fn prune_expanded(&mut self) {
        let alive: std::collections::HashSet<u64> =
            self.term.blocks().iter().map(|block| block.id).collect();
        self.expanded.retain(|id, _| alive.contains(id));
    }
}
