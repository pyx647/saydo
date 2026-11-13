// SenseVoice-Small ONNX 语音识别后端
// 模型：csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17
// 输入：16kHz 单声道 f32 PCM；内部实现 FBANK + LFR，直接调现有 ort crate。

use ndarray::{Array1, Array3};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

// ──────────────────────────────────────────────────────────────────────────────
// 常量
// ──────────────────────────────────────────────────────────────────────────────

const SAMPLE_RATE: usize = 16000;
const NUM_MEL: usize = 80;
const FRAME_LEN: usize = 400; // 25 ms @ 16 kHz
const FRAME_SHIFT: usize = 160; // 10 ms @ 16 kHz
const NFFT: usize = 512; // 下一个 2 的幂 ≥ 400
const LFR_M: usize = 7; // SenseVoice LFR 堆叠帧数
const LFR_N: usize = 6; // SenseVoice LFR 步长
const FEAT_DIM: usize = NUM_MEL * LFR_M; // 560

// ──────────────────────────────────────────────────────────────────────────────
// 基数-2 Cooley-Tukey FFT（就地，长度必须是 2 的幂）
// ──────────────────────────────────────────────────────────────────────────────

fn bit_rev(mut x: usize, bits: u32) -> usize {
    let mut r = 0;
    for _ in 0..bits {
        r = (r << 1) | (x & 1);
        x >>= 1;
    }
    r
}

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = bit_rev(i, bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        let angle = -std::f32::consts::TAU / len as f32;
        let (wre, wim) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut ur, mut ui) = (1.0f32, 0.0f32);
            for k in 0..half {
                let i = start + k;
                let j = start + k + half;
                let tr = ur * re[j] - ui * im[j];
                let ti = ur * im[j] + ui * re[j];
                re[j] = re[i] - tr;
                im[j] = im[i] - ti;
                re[i] += tr;
                im[i] += ti;
                let nr = ur * wre - ui * wim;
                ui = ur * wim + ui * wre;
                ur = nr;
            }
        }
        len <<= 1;
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Mel 滤波器组（80 个三角滤波器，20 Hz–8 kHz）
// ──────────────────────────────────────────────────────────────────────────────

fn hz2mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}
fn mel2hz(mel: f32) -> f32 {
    700.0 * ((mel / 1127.0).exp() - 1.0)
}

fn mel_filterbank() -> Vec<[f32; NFFT / 2 + 1]> {
    let n_bins = NFFT / 2 + 1; // 257
    let low = hz2mel(20.0);
    let high = hz2mel(SAMPLE_RATE as f32 / 2.0);
    let mel_pts: Vec<f32> = (0..=NUM_MEL + 1)
        .map(|i| low + (high - low) * i as f32 / (NUM_MEL + 1) as f32)
        .collect();
    let fft_bins: Vec<usize> = mel_pts
        .iter()
        .map(|&m| {
            let hz = mel2hz(m);
            ((NFFT as f32 + 1.0) * hz / SAMPLE_RATE as f32).floor() as usize
        })
        .collect();

    let mut filters = vec![[0.0f32; NFFT / 2 + 1]; NUM_MEL];
    for m in 0..NUM_MEL {
        let (fl, fc, fr) = (
            fft_bins[m] as f32,
            fft_bins[m + 1] as f32,
            fft_bins[m + 2] as f32,
        );
        for k in fft_bins[m]..=fft_bins[m + 2].min(n_bins - 1) {
            let kf = k as f32;
            filters[m][k] = if kf <= fc {
                if fc > fl {
                    (kf - fl) / (fc - fl)
                } else {
                    0.0
                }
            } else if fr > fc {
                (fr - kf) / (fr - fc)
            } else {
                0.0
            };
        }
    }
    filters
}

// ──────────────────────────────────────────────────────────────────────────────
// FBANK 特征提取（返回 [T][80]）
// ──────────────────────────────────────────────────────────────────────────────

fn extract_fbank(samples: &[f32]) -> Vec<[f32; NUM_MEL]> {
    let mel_fb = mel_filterbank();
    // 预加重
    let mut s = vec![0.0f32; samples.len()];
    s[0] = samples[0];
    for i in 1..samples.len() {
        s[i] = samples[i] - 0.97 * samples[i - 1];
    }
    // Hamming 窗
    let win: Vec<f32> = (0..FRAME_LEN)
        .map(|i| {
            0.54 - 0.46
                * (std::f32::consts::TAU * i as f32 / (FRAME_LEN - 1) as f32).cos()
        })
        .collect();

    let mut frames = Vec::new();
    let mut pos = 0;
    while pos + FRAME_LEN <= s.len() {
        let mut re = [0.0f32; NFFT];
        for i in 0..FRAME_LEN {
            re[i] = s[pos + i] * win[i];
        }
        let mut im = [0.0f32; NFFT];
        fft(&mut re, &mut im);

        let n_bins = NFFT / 2 + 1;
        let power: Vec<f32> = (0..n_bins)
            .map(|k| re[k] * re[k] + im[k] * im[k])
            .collect();

        let mut mel_feat = [0.0f32; NUM_MEL];
        for m in 0..NUM_MEL {
            let e: f32 = mel_fb[m][..n_bins]
                .iter()
                .zip(&power)
                .map(|(&f, &p)| f * p)
                .sum();
            mel_feat[m] = e.max(1e-10_f32).ln();
        }
        frames.push(mel_feat);
        pos += FRAME_SHIFT;
    }
    frames
}

// ──────────────────────────────────────────────────────────────────────────────
// LFR 堆叠（m=7, n=6 → 560 维）
// ──────────────────────────────────────────────────────────────────────────────

fn apply_lfr(feats: &[[f32; NUM_MEL]]) -> Vec<[f32; FEAT_DIM]> {
    let n = feats.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let mut stacked = [0.0f32; FEAT_DIM];
        for j in 0..LFR_M {
            let src_idx = (i + j).min(n - 1);
            stacked[j * NUM_MEL..(j + 1) * NUM_MEL].copy_from_slice(&feats[src_idx]);
        }
        out.push(stacked);
        i += LFR_N;
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Token 词表 + CTC 解码
// ──────────────────────────────────────────────────────────────────────────────

fn load_tokens(path: &Path) -> Result<Vec<String>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    Ok(content
        .lines()
        .map(|line| line.splitn(2, ' ').next().unwrap_or("").to_string())
        .collect())
}

fn ctc_decode(ids: &[i64], vocab: &[String]) -> String {
    let mut out = Vec::new();
    let mut prev = -1i64;
    for &id in ids {
        if id != 0 && id != prev && (id as usize) < vocab.len() {
            let tok = &vocab[id as usize];
            if !tok.starts_with("<|") && !tok.is_empty() {
                out.push(tok.as_str());
            }
        }
        prev = id;
    }
    // 英文等语言用 SentencePiece 分词，"▁" 标记一个词的开头，要还原成空格，否则会原样显示成 please▁save
    out.join("").replace('▁', " ").trim().to_string()
}

// ──────────────────────────────────────────────────────────────────────────────
// 模型目录（跟随 piper_root 同一逻辑）
// ──────────────────────────────────────────────────────────────────────────────

pub fn model_dir(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("stt-model");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn model_ready(app: &AppHandle) -> bool {
    let d = model_dir(app);
    d.join("model.int8.onnx").exists() && d.join("tokens.txt").exists()
}

// ──────────────────────────────────────────────────────────────────────────────
// ORT 会话缓存
// ──────────────────────────────────────────────────────────────────────────────

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn session_cell() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

// CMVN(neg_mean/inv_stddev)：SenseVoice 导出的 onnx 图本身【不】做归一化，训练时用的
// 全局均值/方差统计量被塞进了模型的 custom metadata 里，调用方必须自己在喂给模型前
// 对每帧 560 维特征做 (x + neg_mean) * inv_stddev，漏了这步模型输出会严重跑偏(能听出
// 声音但识别出来的字全是乱的)——这是最初实现里漏掉的一步。
static CMVN: OnceLock<Mutex<Option<(Vec<f32>, Vec<f32>)>>> = OnceLock::new();
fn cmvn_cell() -> &'static Mutex<Option<(Vec<f32>, Vec<f32>)>> {
    CMVN.get_or_init(|| Mutex::new(None))
}

fn parse_csv_floats(s: &str) -> Vec<f32> {
    s.split(',').filter_map(|p| p.trim().parse::<f32>().ok()).collect()
}

/// 最近一次使用语音模型的时间（毫秒时间戳）；0 = 还没加载过
static LAST_USED: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 按下热键时调用：用户说话要一两秒，趁这段时间把模型加载好；识别完不用就由 unload_if_idle 回收
pub fn preload(app: &AppHandle) {
    if model_ready(app) {
        LAST_USED.store(now_ms(), Ordering::Relaxed);
        drop(get_session(app));
    }
}

/// 模型闲置超过 idle_ms 就卸载（约省 300~400 MB 内存）；下次按热键会自动重新加载
pub fn unload_if_idle(idle_ms: u64) -> bool {
    let last = LAST_USED.load(Ordering::Relaxed);
    if last == 0 || now_ms().saturating_sub(last) < idle_ms {
        return false;
    }
    if let Ok(mut g) = session_cell().try_lock() {
        if g.is_some() {
            *g = None;
            return true;
        }
    }
    false
}

fn get_session(app: &AppHandle) -> Result<std::sync::MutexGuard<'static, Option<Session>>, String> {
    let mut guard = session_cell().lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        let model_path = model_dir(app).join("model.int8.onnx");
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 4))
            .unwrap_or(2);
        let s = Session::builder()
            .map_err(|e| e.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .with_intra_threads(threads)
            .map_err(|e| e.to_string())?
            .commit_from_file(&model_path)
            .map_err(|e| e.to_string())?;

        if let Ok(meta) = s.metadata() {
            let neg_mean = meta.custom("neg_mean").ok().flatten().map(|v| parse_csv_floats(&v));
            let inv_stddev = meta.custom("inv_stddev").ok().flatten().map(|v| parse_csv_floats(&v));
            if let (Some(nm), Some(is)) = (neg_mean, inv_stddev) {
                if nm.len() == FEAT_DIM && is.len() == FEAT_DIM {
                    if let Ok(mut c) = cmvn_cell().lock() {
                        *c = Some((nm, is));
                    }
                } else {
                    eprintln!("[stt] cmvn 维度不对: neg_mean={} inv_stddev={} (期望 {})", nm.len(), is.len(), FEAT_DIM);
                }
            } else {
                eprintln!("[stt] 模型 metadata 里没找到 neg_mean/inv_stddev，将跳过 CMVN(识别质量会明显下降)");
            }
        }

        *guard = Some(s);
    }
    Ok(guard)
}

// ──────────────────────────────────────────────────────────────────────────────
// 对外接口
// ──────────────────────────────────────────────────────────────────────────────

/// 语言代码 → 模型语言 ID。数值来自模型 metadata 里的 lang_*：
/// auto=0, zh=3, en=4, yue=7, ja=11, ko=12（不是连续的 1..5，写错了等于没指定语言）
pub fn lang_id(lang: &str) -> i32 {
    match lang {
        "zh" => 3,
        "en" => 4,
        "yue" => 7,
        "ja" => 11,
        "ko" => 12,
        _ => 0, // auto
    }
}

/// text_norm 输入：模型 metadata 里 with_itn=14（带标点、数字规整、英文大小写），without_itn=15
const TEXT_NORM_WITH_ITN: i32 = 14;

/// 转录 16kHz 单声道 f32 PCM，返回文字。
pub fn transcribe(app: &AppHandle, pcm: &[f32], lid: i32) -> Result<String, String> {
    LAST_USED.store(now_ms(), Ordering::Relaxed);
    if pcm.len() < FRAME_LEN {
        return Err("音频太短（不足 25ms）".into());
    }

    let feats = extract_fbank(pcm);
    let mut lfr = apply_lfr(&feats);
    let t = lfr.len();
    if t == 0 {
        return Err("特征提取结果为空".into());
    }

    // get_session 首次加载时顺带把模型 metadata 里的 CMVN 统计量读进 cmvn_cell，
    // 必须在它之后才能拿到；CMVN: (x + neg_mean) * inv_stddev，逐维应用到每一帧。
    let mut guard = get_session(app)?;
    if let Some((neg_mean, inv_stddev)) = cmvn_cell().lock().ok().and_then(|g| g.clone()) {
        for frame in lfr.iter_mut() {
            for d in 0..FEAT_DIM {
                frame[d] = (frame[d] + neg_mean[d]) * inv_stddev[d];
            }
        }
    }

    // 构建 [1, T, 560] f32 张量
    let flat: Vec<f32> = lfr.iter().flat_map(|row| row.iter().copied()).collect();
    let arr3 = Array3::from_shape_vec((1, t, FEAT_DIM), flat).map_err(|e| e.to_string())?;
    let len_arr = Array1::from_vec(vec![t as i32]);
    let lang_arr = Array1::from_vec(vec![lid]);
    let tn_arr = Array1::from_vec(vec![TEXT_NORM_WITH_ITN]);

    let sess = guard.as_mut().ok_or("模型未加载")?;

    let n_inputs = sess.inputs.len();
    let feat_t = Tensor::from_array(arr3).map_err(|e| e.to_string())?;
    let len_t = Tensor::from_array(len_arr).map_err(|e| e.to_string())?;
    let lang_t = Tensor::from_array(lang_arr).map_err(|e| e.to_string())?;
    let tn_t = Tensor::from_array(tn_arr).map_err(|e| e.to_string())?;

    let outputs = match n_inputs {
        1 => sess.run(ort::inputs![feat_t]).map_err(|e| e.to_string())?,
        2 => sess.run(ort::inputs![feat_t, len_t]).map_err(|e| e.to_string())?,
        3 => sess.run(ort::inputs![feat_t, len_t, lang_t]).map_err(|e| e.to_string())?,
        _ => sess
            .run(ort::inputs![feat_t, len_t, lang_t, tn_t])
            .map_err(|e| e.to_string())?,
    };

    let dir = model_dir(app);
    let vocab = load_tokens(&dir.join("tokens.txt"))?;

    // 优先按 int64 token IDs 解码；若是 f32 logits 则先 argmax
    if let Ok((_, ids)) = outputs[0].try_extract_tensor::<i64>() {
        let id_vec: Vec<i64> = ids.iter().copied().collect();
        return Ok(ctc_decode(&id_vec, &vocab));
    }
    let (shape, logits) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| e.to_string())?;
    let vocab_sz = *shape.last().unwrap_or(&1) as usize;
    let token_ids: Vec<i64> = logits
        .chunks(vocab_sz)
        .map(|row| {
            row.iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i as i64)
                .unwrap_or(0)
        })
        .collect();
    Ok(ctc_decode(&token_ids, &vocab))
}

// ──────────────────────────────────────────────────────────────────────────────
// 云端转写：OpenAI 兼容的 POST {base}/audio/transcriptions（OpenAI / Groq / SiliconFlow / 本地 faster-whisper-server…）
// ──────────────────────────────────────────────────────────────────────────────

pub fn transcribe_cloud(cfg: &crate::config::ApiCfg, pcm: &[f32], lang: &str) -> Result<String, String> {
    if cfg.api_key.trim().is_empty() {
        return Err("未配置云端转写的 API Key（设置 → 语音）".into());
    }
    let wav = crate::audio::to_wav(pcm);
    let boundary = "----saydo7f3a9c1e5b";
    let mut body: Vec<u8> = Vec::new();
    let mut field = |name: &str, val: &str| {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{val}\r\n").as_bytes());
    };
    field("model", &cfg.model);
    field("response_format", "json");
    if lang != "auto" && !lang.is_empty() {
        field("language", if lang == "yue" { "zh" } else { lang });
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(&wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let url = format!("{}/audio/transcriptions", cfg.base_url.trim_end_matches('/'));
    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", cfg.api_key.trim()))
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .timeout(std::time::Duration::from_secs(60))
        .send_bytes(&body);
    match resp {
        Ok(r) => {
            let v: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
            Ok(v["text"].as_str().unwrap_or("").trim().to_string())
        }
        Err(ureq::Error::Status(code, r)) => {
            let t = r.into_string().unwrap_or_default();
            Err(format!("云端转写失败 HTTP {code}: {}", t.chars().take(200).collect::<String>()))
        }
        Err(e) => Err(format!("云端转写请求失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_ids_match_model_metadata() {
        assert_eq!(lang_id("auto"), 0);
        assert_eq!(lang_id("zh"), 3);
        assert_eq!(lang_id("en"), 4);
        assert_eq!(lang_id("yue"), 7);
        assert_eq!(lang_id("ja"), 11);
        assert_eq!(lang_id("ko"), 12);
        assert_eq!(lang_id(""), 0);
    }

    #[test]
    fn ctc_decode_restores_word_spaces() {
        let vocab: Vec<String> = ["<blk>", "▁please", "▁save", "<|zh|>", "好"].iter().map(|s| s.to_string()).collect();
        // 连续重复合并、0 是空白、<|…|> 特殊标记丢弃、"▁" 还原成空格
        assert_eq!(ctc_decode(&[1, 1, 0, 2, 3, 4], &vocab), "please save好");
    }
}
