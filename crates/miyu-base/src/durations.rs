//! 时长的展示格式(09-16 从 render 下沉:子代理日志也要写「跑了多久」,工具层不能反向认识渲染层)。

use std::time::Duration;

/// `0.3s` / `12s` / `1m 05s`。
pub fn format_seconds(elapsed: Duration) -> String {
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
