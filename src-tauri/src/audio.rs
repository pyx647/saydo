//! 麦克风录音（cpal/WASAPI，在 Rust 里直接录，不经过 WebView，因此没有"麦克风权限弹窗在无焦点窗口里弹不出来"的问题）。
//! 输出：16kHz 单声道 f32 PCM。录音期间每 50ms 向前端推送一次音量（audio-level，0..1）。

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub struct Recorder {
    stop: Sender<()>,
    join: JoinHandle<Vec<f32>>,
    /// 录音缓冲（原始采样率）与采样率：实时字幕要在录音进行中取"到目前为止"的音频
    buf: Arc<Mutex<Vec<f32>>>,
    sr: u32,
}

impl Recorder {
    /// 停止录音，返回 16kHz 单声道 PCM
    pub fn stop(self) -> Vec<f32> {
        let _ = self.stop.send(());
        self.join.join().unwrap_or_default()
    }

    /// 到目前为止录到的音频（16kHz 单声道），不打断录音
    pub fn snapshot(&self) -> Vec<f32> {
        let raw = self.buf.lock().map(|g| g.clone()).unwrap_or_default();
        resample_to_16k(&raw, self.sr)
    }
}

pub fn start(app: AppHandle) -> Result<Recorder, String> {
    let (stop_tx, stop_rx) = channel::<()>();
    let (ready_tx, ready_rx) = channel::<Result<(Arc<Mutex<Vec<f32>>>, u32), String>>();

    let join = std::thread::spawn(move || {
        let host = cpal::default_host();
        let dev = match host.default_input_device() {
            Some(d) => d,
            None => {
                let _ = ready_tx.send(Err("没有找到麦克风（请检查系统的声音设置 / 隐私里的麦克风权限）".into()));
                return vec![];
            }
        };
        let cfg = match dev.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("读取麦克风配置失败: {e}")));
                return vec![];
            }
        };
        let sr = cfg.sample_rate().0;
        let ch = cfg.channels() as usize;
        let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(sr as usize * 10)));
        let level = Arc::new(AtomicU32::new(0));
        let err_fn = |e| eprintln!("[audio] stream error: {e}");

        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let buf = buf.clone();
                let level = level.clone();
                dev.build_input_stream(
                    &cfg.clone().into(),
                    move |data: &[$t], _: &cpal::InputCallbackInfo| {
                        let mut peak = 0f32;
                        let mut g = buf.lock().unwrap();
                        for frame in data.chunks(ch) {
                            let mut s = 0f32;
                            for x in frame {
                                s += $conv(*x);
                            }
                            let m = s / ch as f32;
                            peak = peak.max(m.abs());
                            if g.len() < sr as usize * 180 {
                                g.push(m);
                            }
                        }
                        level.store(peak.to_bits(), Ordering::Relaxed);
                    },
                    err_fn,
                    None,
                )
            }};
        }
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => build!(f32, |x: f32| x),
            cpal::SampleFormat::I16 => build!(i16, |x: i16| x as f32 / 32768.0),
            cpal::SampleFormat::U16 => build!(u16, |x: u16| (x as f32 - 32768.0) / 32768.0),
            f => Err(cpal::BuildStreamError::StreamConfigNotSupported).map_err(|e| {
                eprintln!("[audio] unsupported sample format {f:?}");
                e
            }),
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("打开麦克风失败: {e}")));
                return vec![];
            }
        };
        if let Err(e) = stream.play() {
            let _ = ready_tx.send(Err(format!("启动录音失败: {e}")));
            return vec![];
        }
        let _ = ready_tx.send(Ok((buf.clone(), sr)));

        loop {
            match stop_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let p = f32::from_bits(level.load(Ordering::Relaxed));
                    // 简单的感知压缩，让小声说话也有可见波动
                    let v = (p * 4.0).min(1.0).powf(0.6);
                    let _ = app.emit("audio-level", v);
                }
            }
        }
        drop(stream);
        let raw = std::mem::take(&mut *buf.lock().unwrap());
        resample_to_16k(&raw, sr)
    });

    match ready_rx.recv_timeout(Duration::from_secs(4)) {
        Ok(Ok((buf, sr))) => Ok(Recorder { stop: stop_tx, join, buf, sr }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("打开麦克风超时".into()),
    }
}

/// 箱式平均降采样（48k→16k 恰好 3:1），比直接抽样少混叠
pub fn resample_to_16k(input: &[f32], sr: u32) -> Vec<f32> {
    if sr == 16000 || input.is_empty() {
        return input.to_vec();
    }
    let ratio = sr as f64 / 16000.0;
    let n_out = (input.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(n_out);
    for i in 0..n_out {
        let a = (i as f64 * ratio) as usize;
        let b = (((i + 1) as f64 * ratio) as usize).min(input.len()).max(a + 1);
        let s: f32 = input[a..b].iter().sum();
        out.push(s / (b - a) as f32);
    }
    out
}

/// 峰值归一化（说话太轻时提高识别率），最多放大 8 倍
pub fn normalize(pcm: &mut [f32]) {
    let peak = pcm.iter().fold(0f32, |m, x| m.max(x.abs()));
    if peak > 0.002 && peak < 0.5 {
        let g = (0.8 / peak).min(8.0);
        for x in pcm.iter_mut() {
            *x *= g;
        }
    }
}

/// 16kHz 单声道 f32 → 16-bit PCM WAV 字节（云端转写用）
pub fn to_wav(pcm: &[f32]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&16000u32.to_le_bytes());
    w.extend_from_slice(&32000u32.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for x in pcm {
        let s = (x.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}
