//! 设置界面的画布：一块 ratatui 的备用屏 + 动画节拍 + 面包屑。
//!
//! 2026-09-20 重做。以前这里是一堆 `queue!(MoveTo, Print)` 手画的 `┌─┐` 框；
//! 现在每屏只说「有哪些行」（[`View`]），版面交给
//! [`miyu_base::terminal::chrome`]——和引导（OOBE）同一份，两边的脸才一样。
//!
//! 三件只在这儿管的事：
//! 1. **动画**。星空闪烁与 banner 扫光要 30ms 一帧。业务侧全是
//!    `loop { 画; 等键 }`，所以节拍藏在 [`Ui::wait_key`] 里：等键等超时就把
//!    **上一帧原样**再画一遍，业务循环一行都不用改。帧号按时间算，连着按键
//!    也不会把动画顶停。
//! 2. **面包屑**。每屏都带个标题，进层是 push、回层是弹到那一层——靠标题自己
//!    维护，业务侧不用到处 push/pop。
//! 3. **光标**。ratatui 写内容那一路光标跟着每个 MoveTo 跳，所以整帧包在一个
//!    同步块里，帧末再把光标摆到插入点（输入法候选框靠它定位）。

use crate::config_tui::*;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use miyu_base::terminal::chrome::{self, compose, Anchor, Chrome, Cx, Stop, View, BODY_MAX};
use miyu_base::terminal::palette::Theme;
use miyu_base::terminal::starfield::BannerArt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::time::Instant;

/// 一帧多久。跟引导同一个节拍，两边的星空才是同一片星空。
pub(in crate::config_tui) const TICK: Duration = Duration::from_millis(30);

/// 设置界面正文比引导宽：这里的行是「名字 + 当前值」两列，62 列压不下
/// 「Mixed 时显示本次供应商/模型」这种长字段。
const CONFIG_BODY_MAX: usize = 84;

/// 面包屑最多显示几层。再深就把中间的折成 `…`——轨要压得住中轴，不能换行。
const TRAIL_MAX: usize = 4;

pub(in crate::config_tui) struct Ui {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    theme: Theme,
    art: BannerArt,
    /// 界面是什么时候开的。动画帧号按**时间**算而不是数帧：数帧的话，按住
    /// j/k 连着切菜单时每一帧都被按键顶掉，星空和扫光就停在那儿不动了
    /// （2026-09-20 用户报的第一条）。
    started: Instant,
    scroll: usize,
    /// 走到哪一层了。`["配置", "插件配置", "记账"]`。
    trail: Vec<String>,
    /// 上一帧画的东西，动画重绘靠它。
    last: Option<View>,
    last_drawn: Instant,
    /// 不在编辑态时光标停在上一次插入点，不往左上角跑（kitty 的 cursor_trail
    /// 连隐藏光标的位移都画）。
    parked: (u16, u16),
}

impl Ui {
    pub(in crate::config_tui) fn new(paths: &MiyuPaths) -> Result<Self> {
        let theme = Theme::detect();
        let art = chrome::load_banner(&paths.config_dir, theme.ascii);
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        // 备用屏理应是空的，但不是每个终端都真有备用屏（tmux 关了
        // alternate-screen、pyte 之类的模拟器）：先整屏擦一次，ratatui 的 diff
        // 才不会把旧字留在屏上。
        terminal.clear()?;
        Ok(Self {
            terminal,
            theme,
            art,
            started: Instant::now(),
            scroll: 0,
            trail: Vec::new(),
            last: None,
            last_drawn: Instant::now(),
            parked: (0, 0),
        })
    }

    pub(in crate::config_tui) fn cx(&self) -> Cx {
        Cx::new(self.theme)
    }

    pub(in crate::config_tui) fn theme(&self) -> Theme {
        self.theme
    }

    /// 正文区有多宽。业务侧算两列的列宽要用它。
    pub(in crate::config_tui) fn body_width(&self) -> usize {
        let cols = terminal::size()
            .map(|(cols, _)| usize::from(cols))
            .unwrap_or(80);
        CONFIG_BODY_MAX.min(cols.saturating_sub(10)).max(20)
    }

    /// 还没画就想知道正文能放几行（并排的几列要自己分页）。跟 compose 用的是
    /// 同一个算法，所以按键条折不折行、报错占几行都得先算进来。
    pub(in crate::config_tui) fn viewport(
        &self,
        sticky: usize,
        footer: usize,
        keys: &[(String, String)],
        counter: Option<&str>,
        content: usize,
    ) -> usize {
        let rows = terminal::size()
            .map(|(_, rows)| usize::from(rows))
            .unwrap_or(24);
        chrome::viewport_estimate(
            self.art.rows(),
            rows,
            Anchor::Centered,
            sticky,
            !self.trail.is_empty(),
            footer,
            keys,
            counter,
            content,
        )
    }

    /// 画一屏。`title` 同时是这一层的面包屑。
    pub(in crate::config_tui) fn show(&mut self, title: &str, view: View) -> Result<()> {
        self.enter(title);
        self.last = Some(view);
        self.paint()
    }

    /// 进/退一层。标题已经在栈里就弹回那一层，否则压进去；换层要重新淡入。
    fn enter(&mut self, title: &str) {
        let title = title.trim().to_string();
        // 空标题 = 「就在这一层上说句话」（提示屏、错误屏），不进面包屑。
        if title.is_empty() || self.trail.last().is_some_and(|top| *top == title) {
            return;
        }
        match self.trail.iter().position(|stop| *stop == title) {
            Some(index) => self.trail.truncate(index + 1),
            None => self.trail.push(title),
        }
        self.scroll = 0;
    }

    /// 面包屑：走过的层是实心点，当前层是空心点——和引导的进度轨同一套。
    fn rail(&self) -> Vec<Stop> {
        let total = self.trail.len();
        let mut stops = Vec::new();
        if total > TRAIL_MAX {
            stops.push(Stop::done("…"));
        }
        let skip = total.saturating_sub(TRAIL_MAX);
        for (index, name) in self.trail.iter().enumerate().skip(skip) {
            if index + 1 == total {
                stops.push(Stop::here(name.clone()));
            } else {
                stops.push(Stop::done(name.clone()));
            }
        }
        stops
    }

    fn paint(&mut self) -> Result<()> {
        let Some(view) = self.last.as_ref() else {
            return Ok(());
        };
        let (cols, rows) = terminal::size()?;
        let rail = self.rail();
        let mut chrome = Chrome::new(self.theme, &self.art, &rail);
        chrome.tick = (self.started.elapsed().as_millis() / TICK.as_millis()) as usize;
        // 设置界面不做「内容逐行落下」：这儿是来回切菜单的地方，每换一层都等
        // 它铺开反而碍事（用户 2026-09-20 拍板去掉）。引导那边照旧。
        chrome.fade = usize::MAX;
        chrome.body_max = CONFIG_BODY_MAX.max(BODY_MAX);
        chrome.anchor = Anchor::Centered;
        let mut scroll = self.scroll;
        let composed = compose(
            usize::from(cols),
            usize::from(rows),
            &chrome,
            view,
            &mut scroll,
        );
        self.scroll = scroll;

        // 一帧一个同步块：先藏光标再写，终端只按帧末的位置算，光标不会在写内容
        // 的路上一路留下轨迹。
        let _ = execute!(io::stdout(), BeginSynchronizedUpdate, Hide);
        let draw = self.terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(
                Paragraph::new(composed.lines),
                Rect::new(0, 0, area.width, area.height),
            );
        });
        let caret = composed.caret;
        if let Some((x, y)) = caret {
            self.parked = (x, y);
            let _ = execute!(io::stdout(), MoveTo(x, y), Show, EndSynchronizedUpdate);
        } else {
            let _ = execute!(
                io::stdout(),
                MoveTo(self.parked.0, self.parked.1),
                Hide,
                EndSynchronizedUpdate
            );
        }
        draw?;
        self.last_drawn = Instant::now();
        Ok(())
    }

    /// 动画的一帧：把上一帧原样再画一遍。帧号按时间算，这里只管重画。
    fn animate(&mut self) -> Result<()> {
        self.paint()
    }

    /// 终端尺寸变了：ratatui 的 diff 建立在旧尺寸上，先整屏擦掉。
    pub(in crate::config_tui) fn on_resize(&mut self) -> Result<()> {
        self.terminal.clear()?;
        self.paint()
    }

    /// 子界面往 stdout 打了东西（`$EDITOR` 回来、装 shell hook 的提示）：
    /// 整屏重画盖掉。
    pub(in crate::config_tui) fn invalidate(&mut self) {
        let _ = self.terminal.clear();
    }

    /// 等一个按键；`timeout` 为 `None` 就一直等。等的过程中每 [`TICK`] 画一帧，
    /// 星空与扫光才动得起来。
    pub(in crate::config_tui) fn wait_key(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Option<Input>> {
        let deadline = timeout.map(|budget| Instant::now() + budget);
        loop {
            let until_frame = TICK.saturating_sub(self.last_drawn.elapsed());
            let window = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(None);
                    }
                    until_frame.min(left)
                }
                None => until_frame,
            };
            match poll_window(window)? {
                // 尺寸变了先把屏擦干净再回报：调用方会重算内容，ratatui 的 diff
                // 建立在旧尺寸上，不擦会把旧字留在屏上。
                Some(Input::Resize) => {
                    self.on_resize()?;
                    return Ok(Some(Input::Resize));
                }
                Some(input) => return Ok(Some(input)),
                None => {
                    if self.last_drawn.elapsed() >= TICK {
                        self.animate()?;
                    }
                }
            }
        }
    }
}
