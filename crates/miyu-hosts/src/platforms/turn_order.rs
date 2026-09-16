//! 平台回合的到达顺序闸(08-26)。
//!
//! 主动回复判断挪到串行票据**之前**以后,回合进入串行队列的时刻由"判断何时
//! 结束"决定,而不再是"消息何时到达"。判断是一次 LLM 调用,耗时随模型、正文
//! 长度和端点重试波动,快的会插到慢的前面——群里就会看到后到的消息先被回复。
//!
//! 这张闸按 `ingress_order` 排序。那个序号在连接层的单线程收帧循环里递增,
//! 严格等于到达顺序,是全仓唯一可信的"先来后到"。每条消息在判断**之前**登记
//! 序号,判断通过后必须等到自己是这条会话里最早的未完成消息,才允许去抢串行
//! 名额;判断不通过的直接掉登记,不占位、也不拖后面的人。
//!
//! 闸按**会话标识(conversation scope)**而不是 session id 建表,为的是能在
//! 派发链的最前面就登记。session id 要查库才拿得到,而在它之前还横着展开合并
//! 转发、查群名、查被 @ 的人、取被引用消息四个网络往返——等到那时候再登记,
//! 两条消息谁先登记已经由这几个请求的快慢决定了,闸管不到还没登记的人
//! (08-26 二轮审查)。QQ 的会话与会话标识是一一对应的,按哪个排等价。
//!
//! 拿到名额后立刻让出顺序位:名额本身才是串行/并行的真正闸门,顺序位只决定
//! "谁先去抢"。继续占着会把并行模式(running > 1)一并拖成串行。
//!
//! 积压上限也搬到了这里。原先满队丢弃发生在抢名额时(判断之后),现在提前到
//! 登记时——超额的消息在花掉一次判断调用之前就被丢掉,比原来更省。

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, Weak};

/// 按会话标识分表的顺序闸。表里只存 `Weak`——闸的存活由在跑的登记位决定,
/// 会话闲下来自然回收,和 `session_turn_locks` 同一套路数。
#[derive(Clone, Default)]
pub(crate) struct TurnOrderRegistry {
    gates: Arc<Mutex<HashMap<String, Weak<TurnOrderGate>>>>,
}

impl TurnOrderRegistry {
    /// 登记一个到达序号。`None` = 这条会话的积压已满。
    pub(crate) fn enter(&self, scope: &str, order: i64, capacity: usize) -> Option<TurnOrderSlot> {
        let gate = {
            let mut gates = self.gates.lock().unwrap();
            match gates.get(scope).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    // 建新闸时顺手清掉已死的表项:没有别的时机会碰到它们。
                    gates.retain(|_, gate| gate.strong_count() > 0);
                    let gate = Arc::new(TurnOrderGate::default());
                    gates.insert(scope.to_string(), Arc::downgrade(&gate));
                    gate
                }
            }
        };
        gate.enter(order, capacity)
    }

    /// 这条会话上还压着多少条。`/stop` 用它报排队量。
    pub(crate) fn backlog(&self, scope: &str) -> usize {
        let gates = self.gates.lock().unwrap();
        gates
            .get(scope)
            .and_then(Weak::upgrade)
            .map(|gate| gate.backlog())
            .unwrap_or(0)
    }
}

/// 一条会话的到达顺序闸。
#[derive(Default)]
struct TurnOrderGate {
    live: Mutex<BTreeSet<i64>>,
    changed: tokio::sync::Notify,
}

impl TurnOrderGate {
    /// 登记一个到达序号。返回 `None` 表示这条会话的积压已满,调用方应当按
    /// "队列已满"处理(静默丢弃 + 日志)。
    fn enter(self: &Arc<Self>, order: i64, capacity: usize) -> Option<TurnOrderSlot> {
        let mut live = self.live.lock().unwrap();
        if live.len() >= capacity {
            return None;
        }
        // 序号来自全局单调计数器,重复只可能是调用方传错;插入失败当作满队
        // 处理,好过让两条消息互相等对方让位。
        if !live.insert(order) {
            return None;
        }
        Some(TurnOrderSlot {
            gate: self.clone(),
            order,
            released: false,
        })
    }

    /// 当前登记在闸上的条数(判断中 + 等名额)。`SessionTurnState::waiting`
    /// 只数卡在信号量上的,而顺序闸让那个数恒为 0 或 1,积压全在这里。
    fn backlog(&self) -> usize {
        self.live.lock().unwrap().len()
    }
}

/// 顺序位的 RAII 登记。掉落即让位——判断不通过、任务被取消、panic 都走同一条
/// 路,不需要调用方记得收尾。
pub(crate) struct TurnOrderSlot {
    gate: Arc<TurnOrderGate>,
    order: i64,
    released: bool,
}

impl TurnOrderSlot {
    /// 等到自己是这条会话里最早的未完成消息。
    pub(crate) async fn wait_turn(&self) {
        loop {
            // 先挂上通知再查状态:反过来的话,两次操作之间的让位通知会丢,
            // 这条消息就永远等下去了。
            let changed = self.gate.changed.notified();
            if self
                .gate
                .live
                .lock()
                .unwrap()
                .iter()
                .next()
                .is_none_or(|first| *first >= self.order)
            {
                return;
            }
            changed.await;
        }
    }

    /// 主动让出顺序位(拿到串行名额之后调用)。
    pub(crate) fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.gate.live.lock().unwrap().remove(&self.order);
        self.gate.changed.notify_waiters();
    }
}

impl Drop for TurnOrderSlot {
    fn drop(&mut self) {
        self.release();
    }
}
