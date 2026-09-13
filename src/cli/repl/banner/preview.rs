//! `miyu --banner`：只看 banner。全屏画空会话那一帧的星空与渐变字（不带输入框），
//! Tab 切换模式行看两种颜色，其余任意键退出。调 `config/banner.txt` 时用它对样。

use super::BannerScene;
use crate::agent::AgentMode;
use crate::config::AppConfig;
use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::{bail, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::Print;
use crossterm::terminal::{
    self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

pub(crate) fn run(config: &AppConfig, paths: &MiyuPaths) -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!(
            "{}",
            t("--banner needs a terminal", "--banner 需要在终端里跑")
        );
    }
    let Some(mut scene) = BannerScene::load(config, paths, AgentMode::Normal) else {
        bail!(
            "{}",
            t(
                "banner is disabled (display.banner = false)",
                "banner 已关闭(display.banner = false)"
            )
        );
    };
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide, Clear(ClearType::All))?;
    let result = (|| -> Result<()> {
        let mut painted: Vec<String> = Vec::new();
        loop {
            let (cols, rows) = terminal::size().unwrap_or((80, 24));
            let lobby = scene.lobby(usize::from(cols), usize::from(rows), 0);
            if painted.len() != lobby.rows.len() {
                painted = vec!["\u{0}".into(); lobby.rows.len()];
            }
            queue!(stdout, BeginSynchronizedUpdate)?;
            for (y, line) in lobby.rows.iter().enumerate() {
                if painted[y] == *line {
                    continue;
                }
                queue!(
                    stdout,
                    MoveTo(0, y as u16),
                    Clear(ClearType::UntilNewLine),
                    Print(line)
                )?;
                painted[y] = line.clone();
            }
            queue!(stdout, MoveTo(0, 0), EndSynchronizedUpdate)?;
            stdout.flush()?;
            if event::poll(Duration::from_millis(40))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Tab => {
                            let next = match scene_mode(&scene) {
                                AgentMode::Normal => AgentMode::Dev,
                                AgentMode::Dev => AgentMode::Normal,
                            };
                            scene.set_mode(next);
                        }
                        _ => return Ok(()),
                    },
                    Event::Resize(_, _) => {
                        painted.clear();
                        execute!(stdout, Clear(ClearType::All))?;
                    }
                    _ => {}
                }
            }
            scene.tick();
        }
    })();
    let _ = execute!(stdout, Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    result
}

fn scene_mode(scene: &BannerScene) -> AgentMode {
    scene.mode()
}
