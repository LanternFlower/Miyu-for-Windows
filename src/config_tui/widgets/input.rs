//! Configuration input: resizing wakes the current view without submitting it.

use crate::config_tui::*;
use std::time::Instant;

#[derive(Debug, PartialEq, Eq)]
enum Input {
    Key(KeyCode),
    Resize,
}

fn read_input(timeout: Option<Duration>) -> Result<Option<Input>> {
    let started = Instant::now();
    read_input_with(
        timeout,
        |remaining| {
            if let Some(remaining) = remaining {
                if !event::poll(remaining)? {
                    return Ok(None);
                }
            }
            event::read().map(Some)
        },
        || started.elapsed(),
    )
}

fn read_input_with(
    timeout: Option<Duration>,
    mut next: impl FnMut(Option<Duration>) -> io::Result<Option<Event>>,
    mut elapsed: impl FnMut() -> Duration,
) -> Result<Option<Input>> {
    loop {
        let remaining = timeout.map(|budget| budget.saturating_sub(elapsed()));
        match next(remaining)? {
            Some(Event::Key(KeyEvent { code, .. })) => return Ok(Some(Input::Key(code))),
            Some(Event::Resize(..)) => return Ok(Some(Input::Resize)),
            None => return Ok(None),
            _ => {}
        }
        if timeout.is_some_and(|budget| elapsed() >= budget) {
            return Ok(None);
        }
    }
}

/// Existing menu/editor loops ignore `Null` and then draw again. Keep this
/// adapter limited to those loops; an "any key" dialog needs `wait_for_key`
/// so it can distinguish a resize from a real (including Null) key event.
pub(in crate::config_tui) fn read_key() -> Result<KeyCode> {
    match read_input(None)?.expect("blocking input read must return an event") {
        Input::Key(key) => Ok(key),
        Input::Resize => Ok(KeyCode::Null),
    }
}

pub(in crate::config_tui) fn read_key_with_timeout(
    timeout: Option<Duration>,
) -> Result<Option<KeyCode>> {
    Ok(match read_input(timeout)? {
        Some(Input::Key(key)) => Some(key),
        Some(Input::Resize) | None => None,
    })
}

pub(in crate::config_tui) fn wait_for_key(mut draw: impl FnMut() -> Result<()>) -> Result<()> {
    loop {
        draw()?;
        if matches!(read_input(None)?, Some(Input::Key(_))) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_and_real_null_key_remain_distinct() {
        let mut events = [Event::Resize(80, 24), Event::Key(KeyCode::Null.into())].into_iter();
        let mut next = |_| Ok(events.next());
        assert_eq!(
            read_input_with(None, &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Resize)
        );
        assert_eq!(
            read_input_with(None, &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Key(KeyCode::Null))
        );
    }

    #[test]
    fn ignored_events_do_not_restart_timeout_budget() {
        let mut polls = Vec::new();
        let mut events = [Some(Event::FocusGained), None].into_iter();
        let mut times = [0, 40, 40].into_iter();
        let result = read_input_with(
            Some(Duration::from_millis(100)),
            |remaining| {
                polls.push(remaining.unwrap().as_millis());
                Ok(events.next().unwrap())
            },
            || Duration::from_millis(times.next().unwrap()),
        )
        .unwrap();
        assert_eq!(result, None);
        assert_eq!(polls, [100, 60]);
    }

    #[test]
    fn resize_does_not_consume_the_next_editing_key() {
        let mut events = [Event::Resize(80, 24), Event::Key(KeyCode::Char('x').into())].into_iter();
        let mut next = |_| Ok(events.next());
        assert_eq!(
            read_input_with(None, &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Resize)
        );
        assert_eq!(
            read_input_with(None, &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Key(KeyCode::Char('x')))
        );
    }
}
