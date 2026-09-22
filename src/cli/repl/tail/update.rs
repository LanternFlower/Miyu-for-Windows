//! Synchronized terminal updates, including nested redraws.
//!
//! DEC mode 2026 is a boolean, not a stack. Only the outer update may end it.
//! A hidden cursor still exposes its position to multiplexers and cursor trails,
//! so fullscreen redraws must keep the whole frame inside this boundary.

use crate::cli::repl::layout::CursorAfterUpdate;
use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use std::cell::Cell;
use std::io::{self, Write};

thread_local! {
    static UPDATE_DEPTH: Cell<usize> = const { Cell::new(0) };
    static BLOCK_STARTED: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// `MIYU_SYNC_TRACE=1`：每个最外层同步块的时长（微秒）追加到
/// `/tmp/miyu-sync-trace.log`。kitty 在块开着的时间里按活光标给输入法定位，块越
/// 短越不容易撞上（09-17），这把尺子量的就是那个窗口。
static SYNC_TRACE: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("MIYU_SYNC_TRACE").is_some());

fn trace_block(elapsed: std::time::Duration) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/miyu-sync-trace.log")
    {
        let _ = writeln!(file, "{}", elapsed.as_micros());
    }
}

pub(in crate::cli) fn synchronized_terminal_update<T>(
    cursor_after: CursorAfterUpdate,
    update: impl FnOnce() -> Result<T>,
) -> Result<T> {
    // Stdout's lock is reentrant: drawing and nested updates on this thread
    // can still write, while other threads cannot interleave with the frame.
    update_to(io::stdout().lock(), cursor_after, update)
}

fn update_to<T>(
    writer: impl Write,
    cursor_after: CursorAfterUpdate,
    update: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let guard = UpdateGuard::begin(writer, cursor_after)?;
    let result = update();
    let end = guard.finish();
    match result {
        Ok(value) => {
            end?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

struct UpdateGuard<W: Write> {
    writer: W,
    cursor_after: CursorAfterUpdate,
    outermost: bool,
    active: bool,
}

impl<W: Write> UpdateGuard<W> {
    fn begin(mut writer: W, cursor_after: CursorAfterUpdate) -> io::Result<Self> {
        let outermost = UPDATE_DEPTH.get() == 0;
        if outermost && *SYNC_TRACE {
            BLOCK_STARTED.set(Some(std::time::Instant::now()));
        }
        if matches!(
            cursor_after,
            CursorAfterUpdate::Shown | CursorAfterUpdate::Hidden
        ) {
            execute!(writer, Hide)?;
        }
        if outermost {
            execute!(writer, BeginSynchronizedUpdate)?;
        }
        UPDATE_DEPTH.set(UPDATE_DEPTH.get() + 1);
        Ok(Self {
            writer,
            cursor_after,
            outermost,
            active: true,
        })
    }

    fn finish(mut self) -> io::Result<()> {
        self.close()
    }

    fn close(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        UPDATE_DEPTH.set(UPDATE_DEPTH.get() - 1);
        if self.outermost {
            execute!(self.writer, EndSynchronizedUpdate)?;
            if let Some(started) = BLOCK_STARTED.take() {
                trace_block(started.elapsed());
            }
        }
        match self.cursor_after {
            CursorAfterUpdate::Shown => execute!(self.writer, Show),
            CursorAfterUpdate::Hidden => execute!(self.writer, Hide),
            CursorAfterUpdate::Preserve => Ok(()),
        }
    }
}

impl<W: Write> Drop for UpdateGuard<W> {
    fn drop(&mut self) {
        // Also release the terminal if a draw unwinds instead of returning Err.
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.borrow().clone()).unwrap()
        }
    }

    #[test]
    fn nested_redraw_cannot_end_the_outer_frame() {
        let mut sink = Sink::default();
        update_to(sink.clone(), CursorAfterUpdate::Shown, || {
            update_to(sink.clone(), CursorAfterUpdate::Preserve, || {
                sink.write_all(b"body")?;
                Ok(())
            })?;
            assert!(!sink.text().contains("\x1b[?2026l"));
            sink.write_all(b"input and cursor")?;
            Ok(())
        })
        .unwrap();
        let text = sink.text();
        assert_eq!(text.matches("\x1b[?2026h").count(), 1);
        assert_eq!(text.matches("\x1b[?2026l").count(), 1);
        assert!(text.ends_with("input and cursor\x1b[?2026l\x1b[?25h"));
        assert_eq!(UPDATE_DEPTH.get(), 0);
    }

    #[test]
    fn failed_draw_closes_the_frame_and_preserves_its_error() {
        let sink = Sink::default();
        let result: Result<()> = update_to(sink.clone(), CursorAfterUpdate::Shown, || {
            anyhow::bail!("draw failed")
        });
        assert_eq!(result.unwrap_err().to_string(), "draw failed");
        assert!(sink.text().ends_with("\x1b[?2026l\x1b[?25h"));
        assert_eq!(UPDATE_DEPTH.get(), 0);
    }

    #[test]
    fn unwinding_nested_draw_releases_synchronization() {
        let sink = Sink::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = update_to(sink.clone(), CursorAfterUpdate::Preserve, || {
                update_to(sink.clone(), CursorAfterUpdate::Preserve, || {
                    panic!("draw panic");
                })
            });
        }));
        assert!(result.is_err());
        assert_eq!(sink.text().matches("\x1b[?2026l").count(), 1);
        assert_eq!(UPDATE_DEPTH.get(), 0);
    }
}
