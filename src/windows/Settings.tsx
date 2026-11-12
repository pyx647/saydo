import { useCallback, useEffect, useRef, useState } from "react";
import { listenSync } from "../lib/transport";
import {
  AudioLines, Bot, Brain, CheckCircle2, CircleAlert, Download, History, Info, Keyboard, Loader2, MousePointerClick, Play, ShieldCheck, SlidersHorizontal, Trash2, XCircle,
} from "lucide-react";
import Md from "../lib/Md";
import { api, BACKEND_NAMES, type ApiCfg, type CliCfg, type CliInfo, type Config, type HistoryEntry } from "../lib/api";

type Tab = "general" | "backend" | "voice" | "memory" | "safety" | "history" | "about";
const TABS: { id: Tab; label: string; icon: React.ReactNode }[] = [
  { id: "general", label: "常规", icon: <SlidersHorizontal size={16} /> },
  { id: "backend", label: "后端", icon: <Bot size={16} /> },
  { id: "voice", label: "语音", icon: <AudioLines size={16} /> },
  { id: "memory", label: "记忆", icon: <Brain size={16} /> },
  { id: "safety", label: "权限与安全", icon: <ShieldCheck size={16} /> },
  { id: "history", label: "历史", icon: <History size={16} /> },
  { id: "about", label: "使用说明", icon: <Info size={16} /> },
];

// ───────────────────────── 配置：自动保存 ─────────────────────────

function useConfig() {
  const [cfg, setCfg] = useState<Config | null>(null);
  const loaded = useRef(false);
  const timer = useRef<number>(0);
  useEffect(() => {
    api.getConfig().then((c) => { setCfg(c); loaded.current = true; });
  }, []);
  useEffect(() => {
    if (!cfg || !loaded.current) return;
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => api.saveConfig(cfg), 400);
  }, [cfg]);
  const patch = useCallback((p: Partial<Config>) => setCfg((c) => (c ? { ...c, ...p } : c)), []);
  return { cfg, patch };
}

// ───────────────────────── 小组件 ─────────────────────────

function Card({ title, desc, children }: { title?: string; desc?: string; children: React.ReactNode }) {
  return (
    <section className="s-card" style={{ padding: 18, marginBottom: 14 }}>
      {title && <div style={{ fontWeight: 650, fontSize: 14.5 }}>{title}</div>}
      {desc && <div style={{ color: "var(--muted)", fontSize: 12.5, marginTop: 3, lineHeight: 1.55 }}>{desc}</div>}
      <div style={{ marginTop: title || desc ? 14 : 0, display: "flex", flexDirection: "column", gap: 14 }}>{children}</div>
    </section>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 13.5 }}>{label}</div>
        {hint && <div style={{ color: "var(--muted)", fontSize: 12, marginTop: 2, lineHeight: 1.5 }}>{hint}</div>}
      </div>
      <div style={{ flex: "none" }}>{children}</div>
    </div>
  );
}

function Toggle({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  return <button className="toggle" role="switch" aria-checked={on} onClick={() => onChange(!on)} />;
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label style={{ display: "flex", flexDirection: "column", gap: 5 }}>
      <span style={{ fontSize: 12, color: "var(--muted)" }}>{label}</span>
      {children}
    </label>
  );
}

function Seg<T extends string>({ value, options, onChange }: { value: T; options: { v: T; label: string }[]; onChange: (v: T) => void }) {
  return (
    <div style={{ display: "inline-flex", padding: 3, gap: 2, borderRadius: 11, background: "var(--surface-2)", border: "1px solid var(--line)" }}>
      {options.map((o) => (
        <button
          key={o.v}
          onClick={() => onChange(o.v)}
          style={{
            height: 28, padding: "0 12px", borderRadius: 8, border: "none", fontSize: 12.5,
            background: value === o.v ? "var(--accent)" : "transparent", color: value === o.v ? "#fff" : "var(--text)", fontWeight: value === o.v ? 600 : 400,
          }}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

// ───────────────────────── 常规 ─────────────────────────

function hotkeyParts(spec: string): string[] {
  if (spec === "Mouse:XButton1") return ["鼠标侧键 1（后退键）"];
  if (spec === "Mouse:XButton2") return ["鼠标侧键 2（前进键）"];
  return spec.split("+");
}

function General({ cfg, patch }: { cfg: Config; patch: (p: Partial<Config>) => void }) {
  const [rec, setRec] = useState(false);
  const [err, setErr] = useState("");
  const [auto, setAuto] = useState(false);
  useEffect(() => { api.autostartGet().then(setAuto).catch(() => {}); }, []);
  const record = async () => {
    setRec(true); setErr("");
    try { patch({ hotkey: await api.recordHotkey() }); } catch (e) { setErr(String(e)); }
    setRec(false);
  };
  return (
    <>
      <Card title="触发热键" desc="按住 = 录音；松开 = 执行。轻点一下（不到 0.3 秒）= 弹出文字输入框。支持键盘组合键，也支持鼠标侧键（后退/前进键）——侧键最顺手，因为不会和任何程序的快捷键冲突。">
        <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
          {rec ? (
            <span className="keycap" style={{ borderColor: "var(--accent)", color: "var(--accent)" }}><Loader2 size={14} className="spin" style={{ marginRight: 6 }} />请按下新的组合键或鼠标侧键…</span>
          ) : (
            hotkeyParts(cfg.hotkey).map((k, i) => (
              <span key={i} style={{ display: "contents" }}>
                {i > 0 && <span style={{ color: "var(--muted)" }}>+</span>}
                <span className="keycap">{k}</span>
              </span>
            ))
          )}
          <button className="s-btn" onClick={record} disabled={rec} style={{ marginLeft: "auto" }}><Keyboard size={14} /> 重新录制</button>
          <button className="s-btn" onClick={() => patch({ hotkey: "Mouse:XButton1" })}><MousePointerClick size={14} /> 用鼠标侧键</button>
        </div>
        {err && <div style={{ color: "var(--danger)", fontSize: 12.5 }}>{err}</div>}
      </Card>
      <Card title="行为">
        <Row label="开机自动启动" hint="常驻托盘，随时可用">
          <Toggle on={auto} onChange={async (v) => { try { await api.autostartSet(v); setAuto(v); } catch { /* ignore */ } }} />
        </Row>
        <Row label="轻点热键时打开输入框" hint="没法说话（开会、深夜）时，用打字代替语音">
          <Toggle on={cfg.tapOpensInput} onChange={(v) => patch({ tapOpensInput: v })} />
        </Row>
        <Row label="结果弹窗自动关闭" hint="鼠标停在弹窗上时暂停计时">
          <select className="s-input" style={{ width: 120 }} value={cfg.autoCloseSec} onChange={(e) => patch({ autoCloseSec: Number(e.target.value) })}>
            <option value={0}>不自动关闭</option>
            <option value={10}>10 秒</option>
            <option value={20}>20 秒</option>
            <option value={45}>45 秒</option>
          </select>
        </Row>
      </Card>
    </>
  );
}

// ───────────────────────── 后端 ─────────────────────────

function CliCard({ id, info, c, selected, onSelect, onChange }: {
  id: "claude" | "codex" | "agy"; info?: CliInfo; c: CliCfg; selected: boolean; onSelect: () => void; onChange: (c: CliCfg) => void;
}) {
  const [test, setTest] = useState<{ state: "idle" | "run" | "ok" | "err"; msg: string }>({ state: "idle", msg: "" });
  const run = async () => {
    setTest({ state: "run", msg: "" });
    try { setTest({ state: "ok", msg: await api.testBackend(id) }); } catch (e) { setTest({ state: "err", msg: String(e) }); }
  };
  const desc: Record<string, string> = {
    claude: "最完整：继承你配好的 Gmail / Drive / 日历等连接器、所有 MCP 和 Skills；发邮件这类对外动作可逐项弹窗审批。",
    codex: "继承 ~/.codex 里的 MCP 和 Skills；非交互模式下不能逐项审批，权限由\"权限与安全\"里的整机权限开关决定。",
    agy: "Google 的 Antigravity CLI，带 Google Cloud 系 MCP。当前不支持挂载 SayDo 的弹窗工具（选文件夹由 SayDo 在启动前先问）。",
  };
  return (
    <section className="s-card" style={{ padding: 16, marginBottom: 12, borderColor: selected ? "var(--accent)" : undefined, boxShadow: selected ? "0 0 0 3px var(--accent-soft)" : undefined }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <input type="radio" checked={selected} onChange={onSelect} style={{ accentColor: "var(--accent)" }} />
        <div style={{ fontWeight: 650, fontSize: 14.5 }}>{BACKEND_NAMES[id]}</div>
        {id === "claude" && <span className="chip" style={{ background: "var(--accent-soft)", color: "var(--accent)", border: "none", fontSize: 11 }}>推荐</span>}
        <span style={{ marginLeft: "auto", display: "inline-flex", alignItems: "center", gap: 6, fontSize: 12.5, color: info?.found ? "var(--ok)" : "var(--danger)" }}>
          {info ? (info.found ? <CheckCircle2 size={14} /> : <XCircle size={14} />) : <Loader2 size={14} className="spin" />}
          {info ? (info.found ? info.version || "已安装" : "未找到") : "检测中"}
        </span>
      </div>
      <div style={{ color: "var(--muted)", fontSize: 12.5, margin: "8px 0 12px 26px", lineHeight: 1.55 }}>{desc[id]}</div>
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10, marginLeft: 26 }}>
        <Field label="模型（留空 = SayDo 默认）"><input className="s-input" value={c.model} onChange={(e) => onChange({ ...c, model: e.target.value })} placeholder={id === "claude" ? "默认 claude-sonnet-5-5" : id === "codex" ? "默认 gpt-6.1-sol" : "如 gemini-3.8-flash-high"} /></Field>
        {(id === "claude" || id === "codex") && (
          <Field label="思考强度（越低越快）">
            <select className="s-input" value={c.effort ?? ""} onChange={(e) => onChange({ ...c, effort: e.target.value })}>
              <option value="">默认（medium）</option>
              <option value="low">low · 最快</option>
              <option value="medium">medium</option>
              <option value="high">high</option>
              <option value="xhigh">xhigh · 最强最慢</option>
            </select>
          </Field>
        )}
        <Field label="额外命令行参数"><input className="s-input" value={c.extraArgs} onChange={(e) => onChange({ ...c, extraArgs: e.target.value })} placeholder="可选" /></Field>
        <div style={{ gridColumn: "1 / -1" }}>
          <Field label={`可执行文件路径（留空 = 自动查找${info?.found ? "：" + info.path : ""}）`}><input className="s-input" value={c.path} onChange={(e) => onChange({ ...c, path: e.target.value })} placeholder="C:\\...\\claude.exe" /></Field>
        </div>
      </div>
      <div style={{ marginLeft: 26, marginTop: 12, display: "flex", alignItems: "center", gap: 10 }}>
        <button className="s-btn" onClick={run} disabled={test.state === "run"}>{test.state === "run" ? <Loader2 size={14} className="spin" /> : <Play size={14} />} 测试连通</button>
        {test.state === "ok" && <span style={{ color: "var(--ok)", fontSize: 12.5 }}>✓ {test.msg}</span>}
        {test.state === "err" && <span style={{ color: "var(--danger)", fontSize: 12.5, userSelect: "text" }}>{test.msg}</span>}
      </div>
    </section>
  );
}

function ApiBlock({ c, onChange }: { c: ApiCfg; onChange: (c: ApiCfg) => void }) {
  return (
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10 }}>
      <Field label="Base URL"><input className="s-input" value={c.baseUrl} onChange={(e) => onChange({ ...c, baseUrl: e.target.value })} /></Field>
      <Field label="模型"><input className="s-input" value={c.model} onChange={(e) => onChange({ ...c, model: e.target.value })} /></Field>
      <div style={{ gridColumn: "1 / -1" }}>
        <Field label="API Key（只保存在本机 %APPDATA%\\com.user.saydo\\config.json）"><input className="s-input" type="password" value={c.apiKey} onChange={(e) => onChange({ ...c, apiKey: e.target.value })} placeholder="sk-..." /></Field>
      </div>
    </div>
  );
}

function Backend({ cfg, patch }: { cfg: Config; patch: (p: Partial<Config>) => void }) {
  const [clis, setClis] = useState<CliInfo[] | null>(null);
  const [test, setTest] = useState<{ state: "idle" | "run" | "ok" | "err"; msg: string }>({ state: "idle", msg: "" });
  useEffect(() => { api.detectClis().then(setClis); }, []);
  const runTest = async () => {
    setTest({ state: "run", msg: "" });
    try { setTest({ state: "ok", msg: await api.testBackend("openai") }); } catch (e) { setTest({ state: "err", msg: String(e) }); }
  };
  return (
    <>
      <div style={{ color: "var(--muted)", fontSize: 13, lineHeight: 1.6, marginBottom: 14 }}>
        <b style={{ color: "var(--text)" }}>为什么主力是 Code CLI？</b> 它们在你电脑上已经配好了 Google 全家桶连接器、各种 MCP 服务器和几百个 Skills，SayDo 只是把"选中的东西 + 你说的话"交给它们，原样继承这一切。
      </div>
      {(["claude", "codex", "agy"] as const).map((id) => (
        <CliCard key={id} id={id} info={clis?.find((x) => x.id === id)} c={cfg[id]} selected={cfg.backend === id} onSelect={() => patch({ backend: id })} onChange={(c) => patch({ [id]: c } as Partial<Config>)} />
      ))}
      <section className="s-card" style={{ padding: 16, borderColor: cfg.backend === "openai" ? "var(--accent)" : undefined, boxShadow: cfg.backend === "openai" ? "0 0 0 3px var(--accent-soft)" : undefined }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <input type="radio" checked={cfg.backend === "openai"} onChange={() => patch({ backend: "openai" })} style={{ accentColor: "var(--accent)" }} />
          <div style={{ fontWeight: 650, fontSize: 14.5 }}>OpenAI API · 快速模式</div>
        </div>
        <div style={{ color: "var(--muted)", fontSize: 12.5, margin: "8px 0 12px 26px", lineHeight: 1.55 }}>
          直连 OpenAI 兼容接口，响应最快，但<b>没有 MCP、没有 Skills、不能操作文件</b>，只适合翻译 / 总结 / 解释 / 看屏幕。
        </div>
        <div style={{ marginLeft: 26 }}>
          <ApiBlock c={cfg.openai} onChange={(openai) => patch({ openai })} />
          <div style={{ marginTop: 12, display: "flex", alignItems: "center", gap: 10 }}>
            <button className="s-btn" onClick={runTest} disabled={test.state === "run"}>{test.state === "run" ? <Loader2 size={14} className="spin" /> : <Play size={14} />} 测试连通</button>
            {test.state === "ok" && <span style={{ color: "var(--ok)", fontSize: 12.5 }}>✓ {test.msg}</span>}
            {test.state === "err" && <span style={{ color: "var(--danger)", fontSize: 12.5, userSelect: "text" }}>{test.msg}</span>}
          </div>
        </div>
      </section>
    </>
  );
}

// ───────────────────────── 语音 ─────────────────────────

function Voice({ cfg, patch }: { cfg: Config; patch: (p: Partial<Config>) => void }) {
  const [st, setSt] = useState<{ ready: boolean; dir: string } | null>(null);
  const [dl, setDl] = useState<{ file: string; done: number; total: number } | null>(null);
  const [err, setErr] = useState("");
  const refresh = () => api.sttStatus().then(setSt);
  useEffect(() => {
    refresh();
    const un = [
      listenSync<{ file: string; done: number; total: number }>("stt-download-progress", (e) => setDl(e.payload)),
      listenSync("stt-download-done", () => { setDl(null); refresh(); }),
    ];
    return () => un.forEach((f) => f());
  }, []);
  const download = async () => {
    setErr(""); setDl({ file: "", done: 0, total: 0 });
    try { await api.sttDownload(); } catch (e) { setErr(String(e)); setDl(null); }
  };
  const pct = dl && dl.total ? Math.round((dl.done / dl.total) * 100) : 0;
  return (
    <>
      <Card title="语音识别引擎">
        <Seg value={cfg.sttEngine} onChange={(v) => patch({ sttEngine: v })} options={[{ v: "local", label: "本地离线（SenseVoice）" }, { v: "cloud", label: "云端（OpenAI 兼容）" }]} />
        <Row label="识别语言" hint="自动 = 中 / 英 / 日 / 韩 / 粤语混说也能识别">
          <select className="s-input" style={{ width: 130 }} value={cfg.lang} onChange={(e) => patch({ lang: e.target.value })}>
            <option value="auto">自动</option><option value="zh">中文</option><option value="en">English</option><option value="ko">한국어</option><option value="ja">日本語</option><option value="yue">粤语</option>
          </select>
        </Row>
      </Card>
      {cfg.sttEngine === "local" ? (
        <Card title="本地模型" desc="SenseVoice-Small（int8，约 230 MB）。完全离线、不花钱、不上传你的声音。首次使用需要下载一次。">
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            {st?.ready ? (
              <span style={{ color: "var(--ok)", display: "inline-flex", alignItems: "center", gap: 6 }}><CheckCircle2 size={16} /> 模型已就绪</span>
            ) : (
              <span style={{ color: "var(--danger)", display: "inline-flex", alignItems: "center", gap: 6 }}><CircleAlert size={16} /> 还没有模型</span>
            )}
            {!st?.ready && <button className="s-btn s-btn-primary" onClick={download} disabled={!!dl}>{dl ? <Loader2 size={14} className="spin" /> : <Download size={14} />} 下载模型</button>}
          </div>
          {dl && (
            <div>
              <div style={{ height: 6, borderRadius: 6, background: "var(--surface-2)", overflow: "hidden" }}><div style={{ width: `${pct}%`, height: "100%", background: "var(--accent)", transition: "width .2s" }} /></div>
              <div style={{ fontSize: 12, color: "var(--muted)", marginTop: 6 }}>{dl.file} {pct}%</div>
            </div>
          )}
          {err && <div style={{ color: "var(--danger)", fontSize: 12.5 }}>{err}</div>}
          {st && <div style={{ fontSize: 11.5, color: "var(--muted)", userSelect: "text" }}>模型目录：{st.dir}</div>}
        </Card>
      ) : (
        <Card title="云端转写" desc="调用 POST {Base URL}/audio/transcriptions，OpenAI / Groq / SiliconFlow / 本地 faster-whisper-server 都可以。">
          <ApiBlock c={cfg.cloudStt} onChange={(cloudStt) => patch({ cloudStt })} />
        </Card>
      )}
    </>
  );
}

// ───────────────────────── 权限与安全 ─────────────────────────

function Safety({ cfg, patch }: { cfg: Config; patch: (p: Partial<Config>) => void }) {
  const levels: { v: Config["approval"]; title: string; desc: string }[] = [
    { v: "careful", title: "谨慎", desc: "除了读取文件和搜索，其它动作（改文件、跑命令、调用 Gmail/Drive 等）都弹窗问你。" },
    { v: "balanced", title: "平衡（推荐）", desc: "读取、改文件、普通命令自动放行；删除/递归删除、git push、联网下载执行、发邮件、写入 Drive 这类有副作用或对外的动作弹窗问你。" },
    { v: "auto", title: "全自动", desc: "什么都不问，Agent 想做什么就做什么。只在你完全信任指令来源时用。" },
  ];
  return (
    <>
      <Card title="动作审批" desc="仅对 Claude Code 后端逐项生效（它支持把每次授权请求转给 SayDo 弹窗）。Codex / agy 是非交互模式，只能用下面的整机权限开关。">
        {levels.map((l) => (
          <label key={l.v} style={{ display: "flex", gap: 12, padding: "10px 12px", borderRadius: 12, cursor: "pointer", border: `1px solid ${cfg.approval === l.v ? "var(--accent)" : "var(--line)"}`, background: cfg.approval === l.v ? "var(--accent-soft)" : "transparent" }}>
            <input type="radio" checked={cfg.approval === l.v} onChange={() => patch({ approval: l.v })} style={{ accentColor: "var(--accent)", marginTop: 3 }} />
            <div><div style={{ fontWeight: 600 }}>{l.title}</div><div style={{ fontSize: 12.5, color: "var(--muted)", marginTop: 2, lineHeight: 1.55 }}>{l.desc}</div></div>
          </label>
        ))}
      </Card>
      <Card title="CLI 权限范围">
        <Row label="允许 Codex 操作整台电脑" hint="关闭后 Codex 只能写 SayDo 的工作目录，无法把文件移到别的文件夹。">
          <Toggle on={cfg.fullAccess} onChange={(v) => patch({ fullAccess: v })} />
        </Row>
        <Row label="常驻预热（强烈建议）" hint="选中的后端（Claude Code 或 Codex）会在 SayDo 启动时就常驻起来：Claude Code 冷启动 7~10 秒 → 之后约 3 秒；Codex 约 12 秒 → 之后约 2.5~4.5 秒。只常驻当前选中的那一个，切换后端会自动换。代价是常驻进程约 600 MB 内存（它要连你的全部 MCP 和 Skills），并且每 10 个任务自动换新对话以免上下文累积。">
          <Toggle on={cfg.warmClaude} onChange={(v) => patch({ warmClaude: v })} />
        </Row>
        <Row label="保留我在 ~/.claude 里配置的 hooks" hint="默认关闭：这类 hooks（会话启动/结束脚本）会让每次调用多花 10 秒以上。MCP 和 Skills 不受影响。">
          <Toggle on={cfg.keepHooks} onChange={(v) => patch({ keepHooks: v })} />
        </Row>
      </Card>
      <Card title="常驻偏好" desc="会追加到每次发给 Agent 的提示词末尾。例如：翻译默认译成中文；转 PDF 的文件放进 Documents\\PDF；回答尽量简短。">
        <textarea className="s-input" rows={4} value={cfg.extraPrompt} onChange={(e) => patch({ extraPrompt: e.target.value })} placeholder="可选" />
      </Card>
    </>
  );
}

// ───────────────────────── 记忆 ─────────────────────────

function Memory({ cfg, patch }: { cfg: Config; patch: (p: Partial<Config>) => void }) {
  const [draft, setDraft] = useState("");
  // 语音"记住……"在后端写入配置后会广播 config-changed：重新读取，列表马上更新
  useEffect(() => listenSync("config-changed", () => api.getConfig().then((c) => patch({ memories: c.memories, profile: c.profile }))), [patch]);
  const add = () => {
    const t = draft.trim();
    if (!t || cfg.memories.includes(t)) return;
    patch({ memories: [...cfg.memories, t] });
    setDraft("");
  };
  return (
    <>
      <Card title="关于我" desc="告诉 SayDo 你是谁、在哪、做什么，写一次就行，之后每条指令它都知道，不用再解释。">
        <textarea
          className="s-input" rows={6} value={cfg.profile} onChange={(e) => patch({ profile: e.target.value })}
          placeholder={"例如：我叫小明，在某大学做生物信息研究。常用语言：中文、英文。\n文件主要放在 D:\\Work。回答尽量简短，翻译默认译成中文。"}
        />
      </Card>
      <Card title="记住的事" desc="按住热键说「记住：……」就能随口添加，说「忘掉……」删除；也可以在这里手动管理。">
        <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
          <input
            className="s-input" style={{ flex: 1 }} value={draft} placeholder="例如：我下周三在首尔出差"
            onChange={(e) => setDraft(e.target.value)} onKeyDown={(e) => e.key === "Enter" && add()}
          />
          <button className="btn" onClick={add}>添加</button>
        </div>
        {cfg.memories.length === 0 && <div style={{ color: "var(--muted)", fontSize: 13 }}>还没有记住任何事。</div>}
        {cfg.memories.map((m, i) => (
          <div key={i} style={{ display: "flex", alignItems: "center", gap: 8, padding: "7px 0", borderTop: i ? "1px solid var(--line)" : "none" }}>
            <span style={{ flex: 1, fontSize: 13.5, lineHeight: 1.5 }}>{m}</span>
            <button className="btn" title="忘掉这条" aria-label="忘掉这条" onClick={() => patch({ memories: cfg.memories.filter((_, j) => j !== i) })}><Trash2 size={14} /></button>
          </div>
        ))}
      </Card>
    </>
  );
}

// ───────────────────────── 历史 ─────────────────────────

function fmtTime(ts: number) {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function HistoryTab() {
  const [items, setItems] = useState<HistoryEntry[] | null>(null);
  const [open, setOpen] = useState<number | null>(null);
  useEffect(() => { api.historyList().then(setItems); }, []);
  if (!items) return null;
  return (
    <>
      <div style={{ display: "flex", alignItems: "center", marginBottom: 12 }}>
        <div style={{ color: "var(--muted)", fontSize: 13 }}>最近 {items.length} 次（最多保留 100 条，仅存本机）</div>
        <button className="s-btn" style={{ marginLeft: "auto" }} onClick={async () => { await api.historyClear(); setItems([]); }}><Trash2 size={14} /> 清空</button>
      </div>
      {items.length === 0 && <div className="s-card" style={{ padding: 30, textAlign: "center", color: "var(--muted)" }}>还没有记录。选中点什么，按住热键说一句试试。</div>}
      {items.map((h, i) => (
        <div key={h.ts + "-" + i} className="s-card" style={{ padding: "12px 14px", marginBottom: 8, cursor: "pointer" }} onClick={() => setOpen(open === i ? null : i)}>
          <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
            {h.ok ? <CheckCircle2 size={15} style={{ color: "var(--ok)", flex: "none" }} /> : <XCircle size={15} style={{ color: "var(--danger)", flex: "none" }} />}
            <div style={{ flex: 1, minWidth: 0, fontWeight: 600, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{h.instruction}</div>
            <span style={{ fontSize: 11.5, color: "var(--muted)", flex: "none" }}>{BACKEND_NAMES[h.backend] ?? h.backend} · {fmtTime(h.ts)}</span>
          </div>
          <div style={{ fontSize: 12, color: "var(--muted)", marginTop: 4, marginLeft: 25 }}>{h.context}{h.files.length > 0 && ` · ${h.files.length} 个结果文件`}</div>
          {open === i && (
            <div style={{ marginTop: 10, marginLeft: 25, fontSize: 13, lineHeight: 1.6, userSelect: "text" }}>
              <div className="md"><Md>{h.result}</Md></div>
              {h.files.map((f) => (
                <div key={f} style={{ marginTop: 6 }}><button className="s-btn" onClick={(e) => { e.stopPropagation(); api.revealPath(f); }}>{f}</button></div>
              ))}
            </div>
          )}
        </div>
      ))}
    </>
  );
}

// ───────────────────────── 使用说明 ─────────────────────────

function About({ cfg }: { cfg: Config }) {
  const [info, setInfo] = useState<{ exe: string; configDir: string; version: string; paused: boolean } | null>(null);
  useEffect(() => { api.appInfo().then(setInfo); }, []);
  const examples: [string, string][] = [
    ["选中一个文件", "“把它转成 PDF” · “压缩一下” · “总结这个文件讲了什么” · “移到某个文件夹”（会弹出文件夹选择器）"],
    ["选中一段文字", "“翻译成中文” · “润色一下” · “解释这是什么意思” · “纠错”（结果可一键替换原文）"],
    ["什么都不选", "“帮我看一下当前页面怎么回事”（自动截屏，不管选中的内容）· “总结当前页面”"],
    ["连着你的 Google / MCP", "“把这段发给 Sam”（Gmail，发送前弹窗确认）· “把这个文件传到 Drive”· “明天下午有什么日程”"],
  ];
  return (
    <>
      <Card title="怎么用">
        <div style={{ fontSize: 13.5, lineHeight: 1.8 }}>
          <b>① 选中</b>文件 / 文字（或什么都不选）→ <b>② 按住</b> <span className="keycap" style={{ height: 24, minWidth: 0 }}>{hotkeyParts(cfg.hotkey).join(" + ")}</span> 不放，说出你要做什么 → <b>③ 松开</b>，弹窗里出结果。
        </div>
        {examples.map(([t, d]) => (
          <div key={t} style={{ display: "flex", gap: 14 }}>
            <div style={{ width: 120, flex: "none", fontWeight: 600, fontSize: 13 }}>{t}</div>
            <div style={{ color: "var(--muted)", fontSize: 13, lineHeight: 1.65 }}>{d}</div>
          </div>
        ))}
      </Card>
      <Card title="内置 MCP 服务器" desc="SayDo 会把自己注册成名为 saydo 的 MCP 服务器挂给 Claude Code / Codex，提供 pick_folder（弹出文件夹选择器）、confirm（弹窗确认）、ask_user、screenshot、get_context 和审批通道。你不需要手动配置。">
        {info && (
          <div style={{ fontSize: 12, color: "var(--muted)", lineHeight: 1.9, userSelect: "text" }}>
            版本：{info.version}<br />程序：{info.exe}<br />配置目录：{info.configDir}
          </div>
        )}
      </Card>
    </>
  );
}

// ───────────────────────── 主体 ─────────────────────────

export default function Settings() {
  const { cfg, patch } = useConfig();
  const [tab, setTab] = useState<Tab>("general");
  const [paused, setPaused] = useState(false);
  useEffect(() => {
    api.appInfo().then((i) => setPaused(i.paused));
    return listenSync<boolean>("paused-changed", (e) => setPaused(e.payload));
  }, []);
  if (!cfg) return <div className="s-root" style={{ height: "100%" }} />;
  return (
    <div className="s-root" style={{ display: "flex", height: "100%" }}>
      <aside style={{ width: 200, padding: 14, borderRight: "1px solid var(--line)", display: "flex", flexDirection: "column", gap: 4, background: "var(--surface)" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "6px 8px 16px" }}>
          <div style={{ width: 32, height: 32, borderRadius: 10, display: "grid", placeItems: "center", background: "linear-gradient(135deg,#7c8cff,#4fd1c5)", color: "#fff", fontWeight: 800 }}>S</div>
          <div><div style={{ fontWeight: 750, fontSize: 15 }}>SayDo</div><div style={{ fontSize: 11, color: "var(--muted)" }}>说即办</div></div>
        </div>
        {TABS.map((t) => (
          <button key={t.id} className="navbtn" aria-current={tab === t.id} onClick={() => setTab(t.id)}>{t.icon}{t.label}</button>
        ))}
        <div style={{ marginTop: "auto", padding: 8 }}>
          <div style={{ fontSize: 12, color: "var(--muted)", marginBottom: 6 }}>热键 {paused ? "已暂停" : "运行中"}</div>
          <Toggle on={!paused} onChange={(on) => api.setPaused(!on)} />
        </div>
      </aside>
      <main style={{ flex: 1, overflowY: "auto", padding: "22px 26px" }}>
        <h1 style={{ margin: "0 0 16px", fontSize: 20, fontWeight: 700 }}>{TABS.find((t) => t.id === tab)?.label}</h1>
        {tab === "general" && <General cfg={cfg} patch={patch} />}
        {tab === "backend" && <Backend cfg={cfg} patch={patch} />}
        {tab === "voice" && <Voice cfg={cfg} patch={patch} />}
        {tab === "memory" && <Memory cfg={cfg} patch={patch} />}
        {tab === "safety" && <Safety cfg={cfg} patch={patch} />}
        {tab === "history" && <HistoryTab />}
        {tab === "about" && <About cfg={cfg} />}
      </main>
    </div>
  );
}
