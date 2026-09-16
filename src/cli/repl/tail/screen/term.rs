//! 一个够用的终端模拟器：吃 ANSI 字节，维护带样式的行缓冲。
//!
//! 全屏后端拿它替掉「把字节写进 scrollback」这一步。为什么非要模拟而不是
//! 按 `\n` 切行追加：**帧里有光标控制**。spinner 每一帧是「上移 N 行 → 清行
//! → 重画」，命令块的实时输出也是原地刷新的。只追加的话 spinner 会每帧堆一行，
//! 几秒钟就把历史撑爆。
//!
//! 模拟了才能做到这次重做的前提——**输出方一行都不用改**。渲染器、spinner、
//! 命令块、图片照旧往外吐它们那套字节，这里照单全收。
//!
//! 只实现 REPL 真的会发的那些：SGR、换行、回车、上下左右、定列、清行、清屏。
//! 没实现的（滚动区、制表位、备用缓冲切换）在这条路上不会出现，遇到就忽略，
//! 最坏是掉一次样式而不是乱码。

use super::ansi::AnsiSpan;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

/// 一格。宽字符占两格，第二格是 `continuation`，画的时候跳过。
#[derive(Clone, PartialEq)]
struct Cell {
    ch: char,
    /// 跟在 `ch` 后面的组合记号。
    ///
    /// 只有真带记号的格子才分配（正文里几乎没有），所以不必让每一格都背一个
    /// `String`。**kitty 的图片占位符全靠它**：每一格是
    /// `U+10EEEE + 行号记号 + 列号记号`，记号丢了终端就不知道这一格该放图的
    /// 哪一块，整张图都出不来。
    marks: Option<Box<str>>,
    /// 这一格属于哪个链接（OSC 8 的目标）。
    ///
    /// 存了它，全屏下才点得开链接：鼠标被程序捕获走之后终端自己那套"点链接"
    /// 就失效了，得自己认。`Arc` 是为了克隆便宜——一行几十格共享同一个目标。
    link: Option<std::sync::Arc<str>>,
    style: Style,
    continuation: bool,
}

impl Cell {
    fn blank() -> Self {
        Self {
            ch: ' ',
            marks: None,
            link: None,
            style: Style::new(),
            continuation: false,
        }
    }

    /// 这一格画出来是什么——基字符加上它带的组合记号。
    fn push_text(&self, out: &mut String) {
        out.push(self.ch);
        if let Some(marks) = &self.marks {
            out.push_str(marks);
        }
    }
}

/// 只有最近这么多行保持「可随机写入」的格子形态。
///
/// spinner 一帧最多上移十几行，命令块的原地刷新也只动尾巴，所以更早的行
/// 永远不会再被改。把它们压成 span 存档能省一个数量级的内存：一格 `Cell`
/// 要带一份完整 `Style`（约 32 字节），而一整行中文压成 span 通常只有一两段。
const LIVE_ROWS: usize = 256;

/// 行缓冲 + 光标 + 当前样式。
///
/// 两段式：`archive` 是压好的只读历史，`live` 是还可能被光标回头改写的尾巴。
pub(in crate::cli) struct Term {
    archive: Vec<Vec<AnsiSpan>>,
    lines: Vec<Vec<Cell>>,
    row: usize,
    col: usize,
    style: Style,
    /// OSC 8 的目标，跟着写入的格子走。
    link: Option<std::sync::Arc<str>>,
    /// 解析器的状态跨帧保留——一个转义序列被帧边界切断也拦得住。
    parser: Parser,
    /// 屏幕宽度。写到边上要自己折行——真终端的 DECAWM 就是这么干的，
    /// 不折的话长段正文会在画面右边被直接切掉。
    cols: usize,
    /// 每一行的版本号（和 `lines` 平行）。写一次涨一次。
    ///
    /// 画面每帧要把可见的三十几行全部重排一遍（取 span、上色、拼 ANSI、比对），
    /// 而流式输出时**真正变的只有最后一两行**。有了版本号就能一眼看出"这一行
    /// 还是上一帧那一行"，直接跳过——AI 输出时拖选发涩就是这几十行的重排在和
    /// 鼠标抢时间。
    stamps: Vec<u64>,
    clock: u64,
    /// 已经收好的可展开块，按起始行升序。
    blocks: Vec<BlockSpan>,
    /// 刚见到起始标记、还没等到第一个字符落下的块。起始行要等到真有字符
    /// 写出来才算——渲染器会先 `MoveUp` 擦掉旧的那几行再重画，标记发出来
    /// 那一刻光标还停在块的**下面**。
    pending_block: Option<u64>,
}

/// 一块可展开内容在缓冲里占的行。
#[derive(Clone, Copy, Debug)]
pub(in crate::cli) struct BlockSpan {
    pub(in crate::cli) id: u64,
    pub(in crate::cli) start: usize,
    pub(in crate::cli) end: usize,
}

impl Default for Term {
    fn default() -> Self {
        Self {
            archive: Vec::new(),
            lines: vec![Vec::new()],
            stamps: vec![0],
            clock: 0,
            row: 0,
            col: 0,
            style: Style::new(),
            link: None,
            parser: Parser::new(),
            blocks: Vec::new(),
            pending_block: None,
            cols: 80,
        }
    }
}

impl Term {
    /// 吃一帧字节。
    pub(in crate::cli) fn feed(&mut self, bytes: &[u8]) {
        // `Parser` 与 `Perform` 不能同时借 self，先把解析器换出来。
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    /// 最后一行**有内容**的行号 + 1。
    ///
    /// 和 `content_rows` 的区别是它不管光标在哪儿。渲染器常在正文末尾多打两个
    /// 换行，光标于是停在两行空白之下，`content_rows` 为了给"下一段输出"留位置
    /// 会把那两行算进去——贴底排版照它算的话，那两行就变成正文和输入框之间的
    /// 空档。
    pub(in crate::cli) fn filled_rows(&self) -> usize {
        let mut last = self.lines.len();
        while last > 0 {
            let index = self.archive.len() + last - 1;
            if self
                .row_spans(index)
                .iter()
                .any(|span| !span.text.trim().is_empty())
            {
                break;
            }
            last -= 1;
        }
        self.archive.len() + last
    }

    /// 这一行现在是第几版。存档段是只读的，给 0。
    pub(in crate::cli) fn row_stamp(&self, index: usize) -> u64 {
        if index < self.archive.len() {
            return 0;
        }
        self.stamps
            .get(index - self.archive.len())
            .copied()
            .unwrap_or(0)
    }

    /// 记一笔：这一行动过了。
    fn touch(&mut self, row: usize) {
        self.clock = self.clock.wrapping_add(1);
        while self.stamps.len() <= row {
            self.stamps.push(0);
        }
        self.stamps[row] = self.clock;
    }

    pub(in crate::cli) fn line_count(&self) -> usize {
        self.archive.len() + self.lines.len()
    }

    /// 第 `index` 行的 span。
    pub(in crate::cli) fn row_spans(&self, index: usize) -> Vec<AnsiSpan> {
        if index < self.archive.len() {
            return self.archive[index].clone();
        }
        let Some(line) = self.lines.get(index - self.archive.len()) else {
            return Vec::new();
        };
        Self::compress(line)
    }

    /// 相邻同样式的格子合并成一段，画出来才不会每个字一段 SGR。
    fn compress(line: &[Cell]) -> Vec<AnsiSpan> {
        let mut spans: Vec<AnsiSpan> = Vec::new();
        for cell in line {
            if cell.continuation {
                continue;
            }
            let link = cell.link.as_ref().map(|link| link.to_string());
            match spans.last_mut() {
                Some(last) if last.style == cell.style && last.link == link => {
                    cell.push_text(&mut last.text);
                }
                _ => {
                    let mut text = String::new();
                    cell.push_text(&mut text);
                    spans.push(AnsiSpan {
                        text,
                        style: cell.style,
                        link,
                    });
                }
            }
        }
        // 行尾的空白没有意义，去掉能让选区和「这行是不是空的」判断都干净。
        while let Some(last) = spans.last_mut() {
            let trimmed = last.text.trim_end();
            if trimmed.is_empty() {
                spans.pop();
            } else {
                last.text.truncate(trimmed.len());
                break;
            }
        }
        spans
    }

    /// 光标当前在第几行——活动区要接在正文后面画。
    pub(in crate::cli) fn cursor_row(&self) -> usize {
        self.archive.len() + self.row
    }

    /// 丢掉最早的 `count` 行。历史留存上限用它。
    pub(in crate::cli) fn drop_front(&mut self, count: usize) {
        let from_archive = count.min(self.archive.len());
        self.archive.drain(..from_archive);
        let rest = count - from_archive;
        if rest > 0 {
            let rest = rest.min(self.lines.len());
            self.lines.drain(..rest);
            let rest = rest.min(self.stamps.len());
            self.stamps.drain(..rest);
            self.row = self.row.saturating_sub(rest);
        }
        // 整块都被丢掉的就不留了，点不开也没内容可给。
        self.blocks.retain(|block| block.start >= count);
        for block in &mut self.blocks {
            block.start -= count;
            block.end -= count;
        }
    }

    /// 收尾一块：结束标记发出来时光标停在最后一行上，那一行算在块里。
    fn close_block(&mut self) {
        self.pending_block = None;
        // 光标停在**行首**说明上一行刚写完换了行，这一行还没有内容——它不属于
        // 这一块。多算一行的后果很具体：紧跟在收缩行后面的那句话（「已取消」
        // 之类）会被当成块的一部分，能点、一点还把块收起来。
        let end = if self.col == 0 {
            self.cursor_row()
        } else {
            self.cursor_row().saturating_add(1)
        };
        if let Some(block) = self.blocks.last_mut() {
            if block.end <= block.start {
                block.end = end.max(block.start.saturating_add(1));
            }
        }
    }

    /// 画面宽度变了：之后写进来的内容按新宽度折。已经落下的行不重排——
    /// 重排要把每行拆回逻辑段再重走一遍，代价远大于收益。
    pub(in crate::cli) fn set_cols(&mut self, cols: usize) {
        self.cols = cols.max(1);
    }

    pub fn blocks(&self) -> &[BlockSpan] {
        &self.blocks
    }

    /// 把 `live` 里跑远的行压进存档，只留最近 [`LIVE_ROWS`] 行可写。
    fn archive_old(&mut self) {
        while self.lines.len() > LIVE_ROWS && self.row > 0 {
            let line = self.lines.remove(0);
            if !self.stamps.is_empty() {
                self.stamps.remove(0);
            }
            self.archive.push(Self::compress(&line));
            self.row -= 1;
        }
    }

    fn line_mut(&mut self) -> &mut Vec<Cell> {
        while self.lines.len() <= self.row {
            self.lines.push(Vec::new());
        }
        let row = self.row;
        self.touch(row);
        &mut self.lines[row]
    }

    fn put(&mut self, ch: char) {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            // 组合记号：**追加**到前一格上，别自己占位，更别把基字符覆盖掉。
            //
            // 写成 `cell.ch = ch` 的后果很具体：kitty 的占位格是
            // `U+10EEEE + 行号记号 + 列号记号`，覆盖之后只剩最后一个记号，
            // 基字符没了、行列号也没了——终端收到一堆孤零零的记号，图一张都
            // 放不出来。真机截图里就是"占位格铺了几行，图没有"。
            let col = self.col;
            let line = self.line_mut();
            if col > 0 {
                if let Some(cell) = line.get_mut(col - 1) {
                    let mut marks = cell
                        .marks
                        .take()
                        .map_or_else(String::new, |marks| marks.into_string());
                    marks.push(ch);
                    cell.marks = Some(marks.into_boxed_str());
                }
            }
            return;
        }
        // 到边就折。真终端写满最后一格只是挂起「待折行」标志，下一个字符才
        // 真的换行；这里按「放不下就先换」处理，对纯输出流等价。
        if self.col + width > self.cols {
            self.newline();
            self.col = 0;
        }
        let (col, style) = (self.col, self.style);
        let link = self.link.clone();
        let line = self.line_mut();
        while line.len() < col {
            line.push(Cell::blank());
        }
        let cell = Cell {
            ch,
            marks: None,
            link: link.clone(),
            style,
            continuation: false,
        };
        if col < line.len() {
            line[col] = cell;
        } else {
            line.push(cell);
        }
        if width == 2 {
            let tail = Cell {
                ch: ' ',
                marks: None,
                link: link.clone(),
                style,
                continuation: true,
            };
            if col + 1 < line.len() {
                line[col + 1] = tail;
            } else {
                line.push(tail);
            }
        }
        self.col += width;
    }

    fn newline(&mut self) {
        self.row += 1;
        while self.lines.len() <= self.row {
            self.lines.push(Vec::new());
        }
        self.archive_old();
    }

    /// 从光标清到行尾（`CSI K` 的默认形态）。
    fn clear_to_end(&mut self) {
        let col = self.col;
        let line = self.line_mut();
        line.truncate(col.min(line.len()));
    }

    fn clear_line(&mut self) {
        let line = self.line_mut();
        line.clear();
    }

    fn param(params: &Params, index: usize, default: usize) -> usize {
        params
            .iter()
            .nth(index)
            .and_then(|values| values.first().copied())
            .map(usize::from)
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }

    fn apply_sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.style = Style::new();
            return;
        }
        let flat: Vec<&[u16]> = params.iter().collect();
        let mut index = 0;
        while index < flat.len() {
            let Some(&code) = flat[index].first() else {
                index += 1;
                continue;
            };
            match code {
                0 => self.style = Style::new(),
                1 => self.style = self.style.add_modifier(Modifier::BOLD),
                2 => self.style = self.style.add_modifier(Modifier::DIM),
                3 => self.style = self.style.add_modifier(Modifier::ITALIC),
                4 => self.style = self.style.add_modifier(Modifier::UNDERLINED),
                5 | 6 => self.style = self.style.add_modifier(Modifier::SLOW_BLINK),
                7 => self.style = self.style.add_modifier(Modifier::REVERSED),
                8 => self.style = self.style.add_modifier(Modifier::HIDDEN),
                9 => self.style = self.style.add_modifier(Modifier::CROSSED_OUT),
                22 => {
                    self.style = self.style.remove_modifier(Modifier::BOLD | Modifier::DIM);
                }
                23 => self.style = self.style.remove_modifier(Modifier::ITALIC),
                24 => self.style = self.style.remove_modifier(Modifier::UNDERLINED),
                25 => self.style = self.style.remove_modifier(Modifier::SLOW_BLINK),
                27 => self.style = self.style.remove_modifier(Modifier::REVERSED),
                28 => self.style = self.style.remove_modifier(Modifier::HIDDEN),
                29 => self.style = self.style.remove_modifier(Modifier::CROSSED_OUT),
                30..=37 => self.style = self.style.fg(basic(code - 30)),
                38 => {
                    let (color, used) = extended(&flat, index);
                    if let Some(color) = color {
                        self.style = self.style.fg(color);
                    }
                    index += used;
                }
                39 => self.style.fg = None,
                40..=47 => self.style = self.style.bg(basic(code - 40)),
                48 => {
                    let (color, used) = extended(&flat, index);
                    if let Some(color) = color {
                        self.style = self.style.bg(color);
                    }
                    index += used;
                }
                49 => self.style.bg = None,
                90..=97 => self.style = self.style.fg(bright(code - 90)),
                100..=107 => self.style = self.style.bg(bright(code - 100)),
                _ => {}
            }
            index += 1;
        }
    }
}

fn basic(offset: u16) -> Color {
    match offset {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright(offset: u16) -> Color {
    match offset {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

/// `38` / `48` 之后的扩展色。分号和冒号两种写法都认。
fn extended(flat: &[&[u16]], index: usize) -> (Option<Color>, usize) {
    let param = flat[index];
    if param.len() > 1 {
        return (from_parts(&param[1..]), 0);
    }
    let rest: Vec<u16> = flat[index + 1..]
        .iter()
        .filter_map(|values| values.first().copied())
        .collect();
    match rest.first() {
        Some(5) => (from_parts(&rest[..2.min(rest.len())]), 2),
        Some(2) => (from_parts(&rest[..4.min(rest.len())]), 4),
        _ => (None, 0),
    }
}

fn from_parts(parts: &[u16]) -> Option<Color> {
    match parts.first()? {
        5 => parts.get(1).map(|index| Color::Indexed(*index as u8)),
        2 => match (parts.get(1), parts.get(2), parts.get(3)) {
            (Some(r), Some(g), Some(b)) => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
            _ => None,
        },
        _ => None,
    }
}

impl Perform for Term {
    fn print(&mut self, character: char) {
        if let Some(id) = self.pending_block.take() {
            let start = self.cursor_row();
            // 这一行（及其之后）要被重写了：原来记在那儿的块作废。
            // 活动区的实时那几行每一帧都是「上移 → 清行 → 重画」，不作废的话
            // 每 tick 都会多攒一个块，行号还全是错的。
            self.blocks.retain(|block| block.start < start);
            self.blocks.push(BlockSpan {
                id,
                start,
                end: start,
            });
        }
        self.put(character);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => {
                self.newline();
                self.col = 0;
            }
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => {
                let next = (self.col / 8 + 1) * 8;
                self.col = next;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        let count = Self::param(params, 0, 1);
        match action {
            'm' => self.apply_sgr(params),
            'A' => self.row = self.row.saturating_sub(count),
            'B' | 'e' => {
                self.row = self.row.saturating_add(count);
                while self.lines.len() <= self.row {
                    self.lines.push(Vec::new());
                }
            }
            'C' | 'a' => self.col = self.col.saturating_add(count),
            'D' => self.col = self.col.saturating_sub(count),
            'E' => {
                self.row = self.row.saturating_add(count);
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(count);
                self.col = 0;
            }
            'G' | '`' => self.col = count.saturating_sub(1),
            'K' => match Self::param(params, 0, 0) {
                // 0=到行尾（默认），1=到行首，2=整行
                1 | 2 => self.clear_line(),
                _ => self.clear_to_end(),
            },
            'J' => {
                // 清屏：正文区整段作废，但历史留着——全屏模型里「清屏」
                // 的语义是把视口推空，不是把内容删掉。
                self.clear_to_end();
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let Some(b"1337") = params.first().copied() {
            let payload = params
                .get(1)
                .map(|value| String::from_utf8_lossy(value).into_owned())
                .unwrap_or_default();
            match miyu_hosts::render::blocks::parse_marker(&payload) {
                Some(miyu_hosts::render::blocks::BlockMarker::Begin(id)) => {
                    self.pending_block = Some(id);
                }
                Some(miyu_hosts::render::blocks::BlockMarker::End) => self.close_block(),
                None => {}
            }
            return;
        }
        let Some(b"8") = params.first().copied() else {
            return;
        };
        // 直接存成 `Arc`：每写一个字符都要给那一格挂一份，现转的话一行链接就
        // 分配几十次。
        self.link = params.get(2).filter(|uri| !uri.is_empty()).map(|uri| {
            std::sync::Arc::from(String::from_utf8_lossy(uri).into_owned().into_boxed_str())
        });
    }
}
