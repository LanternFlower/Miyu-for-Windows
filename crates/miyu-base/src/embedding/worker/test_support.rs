//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/embedding/worker.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

/// Under `cargo test` the current executable is the test harness, so spawning
/// "ourselves" would talk to the wrong program; tests encode in-process
/// instead (the process form is covered by testkit/embedding). Load failures
/// still surface as `Err`, which is what the degradation tests exercise.
pub async fn embed_via_worker(
    model: &LocalModel,
    _idle: Duration,
    texts: &[String],
) -> Result<Vec<Vec<f32>>> {
    static ENCODER: OnceLock<std::sync::Mutex<Option<(String, LocalEncoder)>>> = OnceLock::new();
    let runtime_lib = runtime_lib_or_hint()?;
    let key = format!("{}|{}", model.dir.display(), runtime_lib.display());
    let mut guard = ENCODER.get_or_init(Default::default).lock().unwrap();
    if guard.as_ref().map(|(k, _)| k != &key).unwrap_or(true) {
        *guard = Some((key, LocalEncoder::load(model, &runtime_lib)?));
    }
    let (_, encoder) = guard.as_mut().expect("encoder was just loaded");
    texts.iter().map(|text| encoder.encode(text)).collect()
}
