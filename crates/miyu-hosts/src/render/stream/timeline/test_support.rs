//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/render/stream/timeline.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl StreamRenderer {
    /// 想完了：收成一步。详情是思考全文。
    /// 时间线上每一步那一行（测试用）：颜色也要能断言。
    pub(crate) fn timeline_step_lines(&self) -> Vec<String> {
        self.timeline
            .steps
            .iter()
            .map(|step| step.line.clone())
            .collect()
    }
}
