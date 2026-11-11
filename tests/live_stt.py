"""用 Windows 自带的 SAPI 合成中/英文语音 → 喂给本地 SenseVoice，验证识别链路（不需要麦克风/真人）。"""
import os
import tempfile
import time
import wave

import win32com.client

from app import stt

assert stt.model_ready(), "模型不在 %APPDATA%/com.user.saydo/stt-model"
v = win32com.client.Dispatch("SAPI.SpVoice")
vs = v.GetVoices()
voices = [(i, vs.Item(i).GetDescription()) for i in range(vs.Count)]
print("可用 SAPI 声音:", voices)


def pick(lang):
    for i, d in voices:
        dl = d.lower()
        if lang == "zh" and any(k in dl for k in ("chinese", "huihui", "yaoyao", "kangkang", "xiaoxiao")):
            return i
        if lang == "en" and any(k in dl for k in ("english", "zira", "david", "mark")):
            return i
    return None


for text, lang in (("请把这个文件转成PDF格式", "zh"), ("Translate this paragraph into Chinese", "en")):
    i = pick(lang)
    if i is None:
        print(f"[{lang}] 没有对应的 SAPI 声音，跳过")
        continue
    v.Voice = vs.Item(i)
    p = os.path.join(tempfile.gettempdir(), "saydo_tts.wav")
    fs = win32com.client.Dispatch("SAPI.SpFileStream")
    fmt = win32com.client.Dispatch("SAPI.SpAudioFormat")
    fmt.Type = 22  # SAFT16kHz16BitMono
    fs.Format = fmt
    fs.Open(p, 3)
    v.AudioOutputStream = fs
    v.Speak(text)
    fs.Close()
    with wave.open(p) as w:
        pcm = w.readframes(w.getnframes())
        print(f"[{lang}] 合成 {w.getnframes() / w.getframerate():.1f}s @ {w.getframerate()}Hz")
    t0 = time.time()
    out = stt.transcribe_local(pcm, "auto")
    print(f"   原文: {text}\n   识别: {out}   ({time.time() - t0:.2f}s)")
