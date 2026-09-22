//! 按扩展名认媒体类型(09-16 从 tools::vision 下沉:剪贴板这种底层模块也要判,不能反向认识工具层)。

/// 按扩展名识别视频并给出 mime;None=按图片处理。
pub fn video_mime(value: &str) -> Option<&'static str> {
    let lower = value
        .split('?')
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase();
    let ext = lower.rsplit('.').next()?;
    Some(match ext {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        // GLM 官方列的三种格式是 mp4 / mkv / mov;mkv 原先不在表里,会被当图片
        // 走(08-27)。mpeg / webm 保留:别的中转吃这些,GLM 自己会退回明确错误。
        "mpeg" | "mpg" => "video/mpeg",
        "mov" => "video/mov",
        "webm" => "video/webm",
        _ => return None,
    })
}

/// 按扩展名识别 PDF。判据跟 [`video_mime`] 摆在一起,是因为附件分流要在同一处
/// 把三种媒体拆开——分散到三个模块去问"这算不算 X",迟早会各有一套答案。
pub fn pdf_mime(value: &str) -> Option<&'static str> {
    let lower = value
        .split('?')
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase();
    (lower.rsplit('.').next()? == "pdf").then_some("application/pdf")
}
