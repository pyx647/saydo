"""语音识别：本地 SenseVoice-Small(ONNX，离线，中英日韩粤) 或云端 OpenAI 兼容转写。
本地实现是 PromptPop 里 Rust 版(FBANK + LFR + CMVN + CTC 贪心解码)的 numpy 移植。输入：16kHz 单声道 int16 PCM bytes。"""
import io
import json
import os
import threading
import urllib.error
import urllib.request
import uuid
import wave

import numpy as np

from . import config

SR = 16000
NUM_MEL, FRAME_LEN, FRAME_SHIFT, NFFT = 80, 400, 160, 512
LFR_M, LFR_N = 7, 6
FEAT_DIM = NUM_MEL * LFR_M
LANG_ID = {"auto": 0, "zh": 1, "en": 2, "yue": 3, "ja": 4, "ko": 5}


def model_dir() -> str:
    d = os.path.join(config.config_dir(), "stt-model")
    os.makedirs(d, exist_ok=True)
    return d


def model_ready() -> bool:
    d = model_dir()
    return os.path.exists(os.path.join(d, "model.int8.onnx")) and os.path.exists(os.path.join(d, "tokens.txt"))


def pcm_to_float(pcm16: bytes) -> np.ndarray:
    return np.frombuffer(pcm16, dtype="<i2").astype(np.float32) / 32768.0


def normalize(x: np.ndarray) -> np.ndarray:
    """说话太轻时提高识别率：峰值归一化，最多放大 8 倍"""
    if x.size == 0:
        return x
    peak = float(np.abs(x).max())
    if 0.002 < peak < 0.5:
        x = x * min(0.8 / peak, 8.0)
    return x


# ───────────────────────── 特征提取 ─────────────────────────

def _hz2mel(hz):
    return 1127.0 * np.log(1.0 + hz / 700.0)


def _mel2hz(m):
    return 700.0 * (np.exp(m / 1127.0) - 1.0)


_FB = None


def _mel_fb() -> np.ndarray:
    global _FB
    if _FB is not None:
        return _FB
    n_bins = NFFT // 2 + 1
    low, high = _hz2mel(20.0), _hz2mel(SR / 2.0)
    pts = low + (high - low) * np.arange(NUM_MEL + 2) / (NUM_MEL + 1)
    bins = np.floor((NFFT + 1.0) * _mel2hz(pts) / SR).astype(int)
    fb = np.zeros((NUM_MEL, n_bins), dtype=np.float32)
    for m in range(NUM_MEL):
        fl, fc, fr = bins[m], bins[m + 1], bins[m + 2]
        for k in range(fl, min(fr, n_bins - 1) + 1):
            if k <= fc:
                fb[m, k] = (k - fl) / (fc - fl) if fc > fl else 0.0
            else:
                fb[m, k] = (fr - k) / (fr - fc) if fr > fc else 0.0
    _FB = fb
    return fb


def fbank(x: np.ndarray) -> np.ndarray:
    s = np.empty_like(x)
    s[0] = x[0]
    s[1:] = x[1:] - 0.97 * x[:-1]
    win = (0.54 - 0.46 * np.cos(2 * np.pi * np.arange(FRAME_LEN) / (FRAME_LEN - 1))).astype(np.float32)
    n_frames = 1 + (len(s) - FRAME_LEN) // FRAME_SHIFT
    idx = np.arange(FRAME_LEN)[None, :] + FRAME_SHIFT * np.arange(n_frames)[:, None]
    frames = s[idx] * win
    spec = np.fft.rfft(frames, n=NFFT, axis=1)
    power = (spec.real ** 2 + spec.imag ** 2).astype(np.float32)
    mel = power @ _mel_fb().T
    return np.log(np.maximum(mel, 1e-10)).astype(np.float32)


def lfr(feats: np.ndarray) -> np.ndarray:
    n = feats.shape[0]
    out = []
    for i in range(0, n, LFR_N):
        idx = np.minimum(np.arange(i, i + LFR_M), n - 1)
        out.append(feats[idx].reshape(-1))
    return np.stack(out).astype(np.float32)


# ───────────────────────── ONNX 会话 ─────────────────────────

_lock = threading.Lock()
_sess = None
_cmvn = None
_vocab = None


def _load():
    global _sess, _cmvn, _vocab
    if _sess is not None:
        return
    import onnxruntime as ort
    so = ort.SessionOptions()
    so.intra_op_num_threads = max(2, min(4, os.cpu_count() or 2))
    _sess = ort.InferenceSession(os.path.join(model_dir(), "model.int8.onnx"), so, providers=["CPUExecutionProvider"])
    meta = _sess.get_modelmeta().custom_metadata_map
    nm, isd = meta.get("neg_mean"), meta.get("inv_stddev")
    if nm and isd:
        a = np.array([float(v) for v in nm.split(",")], dtype=np.float32)
        b = np.array([float(v) for v in isd.split(",")], dtype=np.float32)
        if a.size == FEAT_DIM and b.size == FEAT_DIM:
            _cmvn = (a, b)
    with open(os.path.join(model_dir(), "tokens.txt"), encoding="utf-8") as f:
        _vocab = [ln.rstrip("\n").split(" ", 1)[0] for ln in f]


def transcribe_local(pcm16: bytes, lang: str = "auto") -> str:
    x = normalize(pcm_to_float(pcm16))
    if x.size < FRAME_LEN:
        raise ValueError("音频太短")
    with _lock:
        _load()
        f = lfr(fbank(x))
        if _cmvn is not None:
            f = (f + _cmvn[0]) * _cmvn[1]
        names = [i.name for i in _sess.get_inputs()]
        vals = [f[None].astype(np.float32), np.array([f.shape[0]], dtype=np.int32),
                np.array([LANG_ID.get(lang, 0)], dtype=np.int32), np.array([0], dtype=np.int32)]
        out = _sess.run(None, dict(zip(names, vals)))[0]
    if out.dtype.kind in "iu":
        ids = out.reshape(-1)
    else:
        ids = np.argmax(out[0], axis=-1)
    toks, prev = [], -1
    for i in np.asarray(ids).reshape(-1).tolist():
        if i != 0 and i != prev and i < len(_vocab):
            t = _vocab[i]
            if t and not t.startswith("<|"):
                toks.append(t)
        prev = i
    return "".join(toks).replace("▁", " ").strip()


def warm():
    try:
        if model_ready():
            transcribe_local(b"\x00\x01" * 8000)
    except Exception:
        pass


# ───────────────────────── 云端 ─────────────────────────

def to_wav(pcm16: bytes) -> bytes:
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(pcm16)
    return buf.getvalue()


def transcribe_cloud(pcm16: bytes, cfg: dict, lang: str = "auto") -> str:
    key = (cfg.get("apiKey") or "").strip()
    if not key:
        raise RuntimeError("未配置云端转写的 API Key（设置 → 语音）")
    boundary = "----saydo" + uuid.uuid4().hex
    parts = []

    def field(name, val):
        parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{val}\r\n'.encode())

    field("model", cfg.get("model", ""))
    field("response_format", "json")
    if lang not in ("auto", ""):
        field("language", "zh" if lang == "yue" else lang)
    parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="speech.wav"\r\nContent-Type: audio/wav\r\n\r\n'.encode())
    parts.append(to_wav(pcm16))
    parts.append(f"\r\n--{boundary}--\r\n".encode())
    req = urllib.request.Request(
        cfg.get("baseUrl", "").rstrip("/") + "/audio/transcriptions", data=b"".join(parts),
        headers={"Authorization": f"Bearer {key}", "Content-Type": f"multipart/form-data; boundary={boundary}"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return (json.loads(r.read().decode("utf-8")).get("text") or "").strip()
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"云端转写失败 HTTP {e.code}: {e.read().decode('utf-8', 'ignore')[:200]}")
    except Exception as e:
        raise RuntimeError(f"云端转写请求失败: {e}")


def transcribe(pcm16: bytes) -> str:
    c = config.get()
    if c["sttEngine"] == "cloud":
        return transcribe_cloud(pcm16, c["cloudStt"], c["lang"])
    if not model_ready():
        raise RuntimeError("本地语音模型还没下载（设置 → 语音 → 下载模型），或改用云端转写")
    return transcribe_local(pcm16, c["lang"])
