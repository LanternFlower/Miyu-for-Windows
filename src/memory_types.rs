//! `state` 与 `memory` 共用的纯数据。
//!
//! 这两层原本互相引用：`state` 落库要 `EvictedTurn`，`memory` 的整理器要
//! `StateStore`。循环一旦成立，改任何一边都要连带重编另一边，而且 `state`
//! 是最底层的存储、`memory` 是它上面的领域逻辑，方向本就该是单向的。
//!
//! 解法是把双方都要的纯数据放到这里，谁都能依赖它，它谁都不依赖。

/// 一条被挤出上下文窗口的对话轮次，等待压缩或归档。
///
/// `state` 负责把它写进库、读出来；`memory` 负责决定哪些该被挤出、挤出后
/// 怎么整理。两边看到的是同一份数据，但谁都不需要知道对方的实现。
#[derive(Debug, Clone, Default)]
pub struct EvictedTurn {
    pub source_id: String,
    pub timestamp: String,
    pub role: String,
    pub content: String,
    /// 可见性：决定这一轮在多主体场景下谁能召回。
    pub visibility: String,
    /// 归属主体的稳定标识（`PlatformPrincipal::stable_key()`）。
    pub owner_principal: String,
    pub owner_display_name: String,
}
