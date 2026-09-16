//! 麦克风采集:cpal 输入流 → 单声道 f32 @16kHz 帧。
//!
//! cpal 的 Stream 不是 Send,因此流的创建与存活都圈在这里自带的
//! 线程里;对外只暴露一个帧 channel 和一个 drop 即停的句柄。

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

use super::pipeline::SAMPLE_RATE;

/// 采集句柄,drop 即停止采集线程。
pub struct Capture {
    /// 实际打开的设备与格式描述,诊断输出用。
    pub description: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// ALSA 在枚举/打开设备时会向 stderr 喷一批无害错误(dmix、/dev/dsp、
/// chmap),cpal 不暴露 error handler,只能在窗口期把 fd2 临时指向
/// /dev/null。进程级副作用,窗口毫秒级,RAII 恢复。
struct SilencedStderr {
    saved: i32,
}

impl SilencedStderr {
    fn new() -> Option<Self> {
        unsafe {
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if devnull < 0 {
                return None;
            }
            let saved = libc::dup(2);
            if saved < 0 {
                libc::close(devnull);
                return None;
            }
            libc::dup2(devnull, 2);
            libc::close(devnull);
            Some(Self { saved })
        }
    }
}

impl Drop for SilencedStderr {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved, 2);
            libc::close(self.saved);
        }
    }
}

/// 一个可选的输入源:`name` 写进配置,`label` 给人看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSource {
    pub name: String,
    pub label: String,
}

/// 枚举输入源,给配置界面的选择列表用。优先问 PipeWire/PulseAudio
/// (`pactl`):名字和描述与系统设置里看到的一致(如「fifine Microphone」),
/// 打开时经 `PIPEWIRE_NODE` 指到这个源。没有 pactl 时退回 cpal 的 ALSA
/// 设备名。"系统默认"由配置里的空值表达,不列 default。
pub fn list_input_sources() -> Vec<InputSource> {
    let pipewire = list_pipewire_sources();
    if !pipewire.is_empty() {
        return pipewire;
    }
    list_input_devices()
        .into_iter()
        .map(|name| InputSource {
            label: name.clone(),
            name,
        })
        .collect()
}

fn list_pipewire_sources() -> Vec<InputSource> {
    let Ok(output) = std::process::Command::new("pactl")
        .args(["-f", "json", "list", "sources"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|source| {
            let name = source.get("name")?.as_str()?.trim();
            // 输出设备的 monitor 不是麦克风。
            if name.is_empty() || name.ends_with(".monitor") {
                return None;
            }
            let label = source
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or(name);
            Some(InputSource {
                name: name.to_string(),
                label: label.to_string(),
            })
        })
        .collect()
}

/// cpal 经 ALSA 枚举出的设备名(退路)。大半是采样率转换/路由插件
/// (lavrate、speexrate、upmix…),只留真正指向声卡的条目。
///
/// `CARD=` 是 **ALSA 的设备名约定**,只在 Linux 上成立。macOS 的 CoreAudio
/// 设备叫「MacBook Air麦克风」这种名字,永远不含 `CARD=`——套这条过滤器会把
/// 所有设备滤光,`miyu-voice devices` 于是永远是空的(09-13 macOS 实测)。
pub fn list_input_devices() -> Vec<String> {
    let _quiet = SilencedStderr::new();
    let host = cpal::default_host();
    host.input_devices()
        .map(|devices| {
            devices
                .filter_map(|device| device.name().ok())
                .filter(|name| {
                    if cfg!(target_os = "linux") {
                        name.contains("CARD=") && !name.starts_with("surround")
                    } else {
                        !name.is_empty()
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 优先按 [`SAMPLE_RATE`](super::pipeline::SAMPLE_RATE) 开流,设备不支持才退回默认格式。
///
/// **为什么必须先问一句**(09-13 macOS 实测定位):`LinearResampler` 是纯线性插值,
/// 没有抗混叠低通。48k→16k 是 3:1 抽取,8kHz 以上的能量会整体折叠回语音带内,
/// 而汉语声母里 q/x/sh 这类擦音的判别特征正好在高频——KWS 于是永不命中。
///
/// Linux 走 PipeWire(`PIPEWIRE_NODE`)拿到的就是 16kHz,`ratio == 1.0` 时重采样器
/// 直通,这条烂路**在 Linux 上根本不执行**;macOS 走 cpal 拿原生 48kHz,每次都过。
/// 直接向设备要 16kHz,由 CoreAudio 自己的 HAL 做带正经滤波器的速率转换。
fn preferred_input_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    let wanted = cpal::SampleRate(SAMPLE_RATE);
    if let Ok(ranges) = device.supported_input_configs() {
        let native = device.default_input_config().ok();
        let native_format = native.as_ref().map(|config| config.sample_format());
        let mut fallback = None;
        for range in ranges {
            if range.min_sample_rate() > wanted || range.max_sample_rate() < wanted {
                continue;
            }
            let config = range.with_sample_rate(wanted);
            // 同格式优先,免得为了采样率换掉样本类型。
            if Some(config.sample_format()) == native_format {
                return Ok(config);
            }
            fallback.get_or_insert(config);
        }
        if let Some(config) = fallback {
            return Ok(config);
        }
    }
    device
        .default_input_config()
        .context("查询输入设备默认格式失败")
}

/// 打开麦克风开始采集。返回 16kHz 单声道帧的接收端。
/// 设备打开失败在这里同步报错,不留给后台线程默默死掉。
pub fn start_capture(device_name: Option<&str>) -> Result<(Receiver<Vec<f32>>, Capture)> {
    let (frame_tx, frame_rx) = mpsc::sync_channel::<Vec<f32>>(64);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let device_name = device_name.map(str::to_string);

    let thread = std::thread::Builder::new()
        .name("miyu-voice-mic".into())
        .spawn(move || {
            let stream = match open_stream(device_name.as_deref(), frame_tx) {
                Ok((stream, description)) => {
                    let _ = ready_tx.send(Ok(description));
                    stream
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            while !stop_thread.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            drop(stream);
        })?;

    let description = ready_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .context("等待麦克风就绪超时")?
        .context("打开麦克风失败")?;

    Ok((
        frame_rx,
        Capture {
            description,
            stop,
            thread: Some(thread),
        },
    ))
}

fn open_stream(
    device_name: Option<&str>,
    frame_tx: SyncSender<Vec<f32>>,
) -> Result<(cpal::Stream, String)> {
    let _quiet = SilencedStderr::new();
    let host = cpal::default_host();
    let mut pipewire_target: Option<String> = None;
    let device = match device_name {
        Some(name) => match host
            .input_devices()?
            .find(|device| device.name().map(|n| n == name).unwrap_or(false))
        {
            Some(device) => device,
            None => {
                // 不是 ALSA 设备名,当作 PipeWire 源名:让 pipewire-alsa 的
                // default 设备接到这个源上。环境变量要在打开 PCM 之前设好。
                std::env::set_var("PIPEWIRE_NODE", name);
                pipewire_target = Some(name.to_string());
                host.default_input_device()
                    .with_context(|| format!("找不到麦克风「{name}」,且没有默认输入设备"))?
            }
        },
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("没有可用的输入设备"))?,
    };
    let config = preferred_input_config(&device)?;
    let source_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let description = format!(
        "{} ({source_rate}Hz {channels}ch {:?})",
        pipewire_target.unwrap_or_else(|| device.name().unwrap_or_else(|_| "unknown".to_string())),
        config.sample_format()
    );
    let stream_config: cpal::StreamConfig = config.clone().into();

    let mut resampler = LinearResampler::new(source_rate, SAMPLE_RATE);
    let error_handler = |error| tracing::warn!("语音采集流错误: {error}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &_| {
                push_frame(data, channels, &mut resampler, &frame_tx);
            },
            error_handler,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &_| {
                let floats: Vec<f32> = data.iter().map(|&s| f32::from(s) / 32768.0).collect();
                push_frame(&floats, channels, &mut resampler, &frame_tx);
            },
            error_handler,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &_| {
                let floats: Vec<f32> = data
                    .iter()
                    .map(|&s| (f32::from(s) - 32768.0) / 32768.0)
                    .collect();
                push_frame(&floats, channels, &mut resampler, &frame_tx);
            },
            error_handler,
            None,
        )?,
        other => anyhow::bail!("不支持的采样格式 {other:?}"),
    };
    stream.play().context("启动输入流失败")?;
    Ok((stream, description))
}

fn push_frame(
    data: &[f32],
    channels: usize,
    resampler: &mut LinearResampler,
    frame_tx: &SyncSender<Vec<f32>>,
) {
    let mono: Vec<f32> = if channels <= 1 {
        data.to_vec()
    } else {
        data.chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    let resampled = resampler.process(&mono);
    if !resampled.is_empty() {
        // 消费端阻塞时丢帧而不是阻塞音频回调线程。
        let _ = frame_tx.try_send(resampled);
    }
}

/// 线性插值重采样。管线里 VAD/ASR 对音质不敏感,线性足够
/// (sherpa 自带的重采样器也是线性),自实现免去 FFI 类型跨线程的约束。
pub struct LinearResampler {
    ratio: f64,
    /// 输出侧游标在输入流上的绝对位置(以输入采样为单位)。
    position: f64,
    /// 上一批的最后一个样本,用于跨批插值。
    carry: Option<f32>,
    consumed: u64,
    /// 抗混叠滑动平均的窗长(1 = 不降采样,不滤波)。
    taps: usize,
    /// 滤波窗口,跨批保留,批边界不留断点。
    history: VecDeque<f32>,
}

impl LinearResampler {
    pub fn new(source_rate: u32, target_rate: u32) -> Self {
        let ratio = f64::from(source_rate) / f64::from(target_rate);
        // 降采样才需要抗混叠:窗长取抽取比,把 target_rate/2 以上压下去。
        // 线性插值自己那点低通远远不够——48k→16k 是 3:1 抽取,8kHz 以上会整体
        // 折叠回语音带,毁掉 q/x/sh 这类擦音的判别特征(09-13 macOS 实测)。
        // 只有在拿不到 16kHz 输入时才走到这儿,见 `preferred_input_config`。
        let taps = if ratio > 1.0 {
            (ratio.round() as usize).clamp(2, 16)
        } else {
            1
        };
        Self {
            ratio,
            position: 0.0,
            carry: None,
            consumed: 0,
            taps,
            history: VecDeque::new(),
        }
    }

    /// 抽取前的滑动平均(boxcar FIR)。窗口跨批保留,批边界不会留下断点。
    fn antialias(&mut self, input: &[f32]) -> Vec<f32> {
        if self.taps <= 1 {
            return input.to_vec();
        }
        let mut out = Vec::with_capacity(input.len());
        for &sample in input {
            self.history.push_back(sample);
            while self.history.len() > self.taps {
                self.history.pop_front();
            }
            let sum: f32 = self.history.iter().copied().sum();
            out.push(sum / self.history.len() as f32);
        }
        out
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        if (self.ratio - 1.0).abs() < f64::EPSILON {
            return input.to_vec();
        }
        let filtered = self.antialias(input);
        let input = filtered.as_slice();
        // 拼上跨批 carry 后,本批可插值的绝对区间是
        // [consumed-1(有 carry 时), consumed+input.len()-1)。
        let base = self.consumed as f64 - if self.carry.is_some() { 1.0 } else { 0.0 };
        let mut samples: Vec<f32> = Vec::with_capacity(input.len() + 1);
        if let Some(carry) = self.carry {
            samples.push(carry);
        }
        samples.extend_from_slice(input);
        let mut output = Vec::with_capacity((input.len() as f64 / self.ratio) as usize + 2);
        while self.position + 1.0 < base + samples.len() as f64 {
            if self.position < base {
                // 起步阶段游标落在 carry 之前(理论上只在首批出现)。
                self.position = base;
            }
            let offset = self.position - base;
            let index = offset as usize;
            let fraction = (offset - index as f64) as f32;
            let current = samples[index];
            let next = samples[index + 1];
            output.push(current + (next - current) * fraction);
            self.position += self.ratio;
        }
        self.consumed += input.len() as u64;
        self.carry = input.last().copied();
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_halves_rate() {
        let mut resampler = LinearResampler::new(32_000, 16_000);
        let input: Vec<f32> = (0..3200).map(|i| (i as f32 / 100.0).sin()).collect();
        let mut total = 0usize;
        for chunk in input.chunks(480) {
            total += resampler.process(chunk).len();
        }
        // 3200 输入 @2:1 → 约 1600 输出(边界差 1~2 个可接受)。
        assert!((1598..=1600).contains(&total), "{total}");
    }

    #[test]
    fn resampler_identity_passthrough() {
        let mut resampler = LinearResampler::new(16_000, 16_000);
        let input = vec![0.5f32; 1000];
        assert_eq!(resampler.process(&input).len(), 1000);
    }

    #[test]
    fn resampler_48k_to_16k_is_monotonic_and_bounded() {
        let mut resampler = LinearResampler::new(48_000, 16_000);
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 / 500.0).sin()).collect();
        let mut total = 0usize;
        for chunk in input.chunks(1024) {
            let out = resampler.process(chunk);
            assert!(out.iter().all(|v| v.abs() <= 1.0));
            total += out.len();
        }
        assert!((15_990..=16_000).contains(&total), "{total}");
    }
}
