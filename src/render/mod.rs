pub(crate) mod blocks;
mod code;
mod command;
mod link;
mod markdown;
pub(crate) mod math;
mod patch;
mod stream;
mod style;
mod table;
mod tool_display;
mod usage;
pub(crate) use code::*;
pub(crate) use command::*;
pub(crate) use link::*;
pub(crate) use markdown::*;
pub(crate) use patch::*;
pub(crate) use stream::*;
pub(crate) use style::*;
pub(crate) use table::*;
pub(crate) use tool_display::*;
pub(crate) use usage::*;

pub(crate) mod wait_spinner;

/// 正文能用多宽。
///
/// 全屏下正文左右各留了页边距，按**整屏**排出来的表格、补丁、代码块会比可视区宽，
/// 落进缓冲被硬折一次，续行从第 0 列开始——屏幕左边就冒出半截边框。`fallback`
/// 是连终端尺寸都问不到时的兜底（各调用点历来的默认值不同，保持原样）。
/// 某个工具在时间线上的图标。面板里的那条时间线也按它来，两处一个样子。
pub(crate) fn tool_glyph_for(name: &str) -> &'static str {
    stream::timeline::tool_glyph(name)
}

pub(crate) fn content_cols(fallback: usize) -> usize {
    if let Some((cols, _)) = crate::cli::content_viewport() {
        return usize::from(cols);
    }
    terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(fallback)
}

use crate::i18n::text as t;
use crate::llm::{ChatResult, ChatStreamChunk, ChatStreamKind, GenerationSpeed, Usage};
use crate::render::wait_spinner::{braille_frame, SpinnerStyle, WaitSpinner, SPINNER_INTERVAL};
use crate::tools::CommandOutputStream;
use anyhow::Result;
use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, terminal};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, IsTerminal, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReasoningDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ReasoningDisplayMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "hidden" => Self::Hidden,
            "full" => Self::Full,
            _ => Self::Summary,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ToolCallDisplayMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "hidden" => Self::Hidden,
            "full" => Self::Full,
            _ => Self::Summary,
        }
    }
}

#[cfg(test)]
mod tests;
