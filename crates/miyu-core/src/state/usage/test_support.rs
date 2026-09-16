//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/usage.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn record_usage(path: &Path, usage: &Usage, meta: UsageMeta<'_>, aux: bool) -> Result<()> {
    record_usage_at(path, usage, meta, aux, chrono::Utc::now().timestamp())
}

/// 持 `usage_lock` 只挡住同进程的并发写:`rename_provider` 要整文件重写,
/// 重写期间的追加会被新文件盖掉。跨进程(`MIYU_DIRECT=1` 直连模式的另一个
/// 进程、TUI 改名)仍有理论上的竞态,接受——账本是统计口径,不是账务。
pub fn record_usage_at(
    path: &Path,
    usage: &Usage,
    meta: UsageMeta<'_>,
    aux: bool,
    ts: i64,
) -> Result<()> {
    record_usage_for_account_at(path, usage, meta, aux, "", ts)
}

/// 最近 `limit` 条调用记录,新的在前;可按来源/模型过滤。按 ts 排序而非
/// 文件顺序:追加序通常就是时间序,但时钟回拨或手工并档后也要给出正确的"最近"。
pub fn usage_details(
    path: &Path,
    limit: usize,
    src: Option<&str>,
    model: Option<&str>,
    price: PriceFn<'_>,
) -> Result<Vec<UsageRecord>> {
    usage_details_for_account(path, limit, src, model, price, None)
}
