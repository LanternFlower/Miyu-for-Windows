//! 时长的展示格式(09-16 从 render 下沉:子代理日志也要写「跑了多久」,工具层不能反向认识渲染层)。

use std::time::Duration;

/// 不到一秒的读数：`<1ms` / `340ms`。
///
/// 原来这一档一律打成 `0.0s`（没信息量），上层再靠「不到十分之一秒就什么都不报」
/// 绕开它——代价是同一段代码两次跑时有时无，写不出稳定的快照。
pub fn format_sub_second(elapsed: Duration) -> String {
    if elapsed < Duration::from_millis(1) {
        "<1ms".to_string()
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}

/// `340ms` / `0.3s` / `12s` / `1m 05s`。
pub fn format_seconds(elapsed: Duration) -> String {
    // 不到一秒报毫秒：见 `format_sub_second`。
    if elapsed < Duration::from_secs(1) {
        return format_sub_second(elapsed);
    }
    let secs = elapsed.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        let whole = elapsed.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}
