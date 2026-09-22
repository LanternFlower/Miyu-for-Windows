//! Fullscreen is the default interactive frontend.
use std::ffi::OsStr;

pub(in crate::cli) fn requested() -> bool {
    enabled(std::env::var_os("MIYU_TUI").as_deref())
}

fn enabled(value: Option<&OsStr>) -> bool {
    value.is_none_or(|value| value != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_is_default_with_explicit_inline_escape_hatch() {
        assert!(enabled(None));
        assert!(enabled(Some(OsStr::new("1"))));
        assert!(!enabled(Some(OsStr::new("0"))));
    }
}
