//! 回合归属随 context 存活，待发记录被替换或消费后仍能识别旧回合。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct TurnOwnership {
    superseded: Arc<AtomicBool>,
}

impl TurnOwnership {
    pub(crate) fn same_turn(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.superseded, &other.superseded)
    }

    pub(crate) fn supersede(&self) {
        self.superseded.store(true, Ordering::Release);
    }

    pub(crate) fn is_superseded(&self) -> bool {
        self.superseded.load(Ordering::Acquire)
    }
}
