import { useCallback, useEffect, useLayoutEffect, useReducer, useRef, useState } from "react";
import Md from "../lib/Md";
import {
  Archive, ArrowUp, Check, ClipboardCopy, Eye, File as FileIcon, FileImage, FileSpreadsheet, FileText, Folder, FolderOpen, Languages, Lightbulb, Loader2, Mic,
  Maximize2, MonitorUp, Presentation, Replace, ScanText, ShieldQuestion, Sparkles, TriangleAlert, X,
} from "lucide-react";
import { listenSync } from "../lib/transport";
import { api, BACKEND_NAMES, fmtSize, type CtxView, type FileView, type PanelState } from "../lib/api";

type AskSpec = Extract<PanelState, { phase: "ask" }>;
type UserMsg = { id: number; role: "user"; text: string };
type AsstMsg = {
  id: number; role: "assistant"; status: "working" | "done" | "error"; steps: string[]; stepCount: number; text: string;
  files: FileView[]; canReplace: boolean; backend: string; elapsed: number; ask?: AskSpec;
};
type Msg = UserMsg | AsstMsg;
const WAVE_N = 30;
type View = "hidden" | "listening" | "transcribing" | "working" | "ask" | "result" | "error" | "input";
type St = { view: View; expanded: boolean; caption: string; said: string; msgs: Msg[]; ctx: CtxView | null; nid: number; hasPrev: boolean };
type Act = PanelState | { phase: "__expand" };
const INIT: St = { view: "hidden", expanded: false, caption: "", said: "", msgs: [], ctx: null, nid: 1, hasPrev: false };

function newAsst(id: number): AsstMsg {
  return { id, role: "assistant", status: "working", steps: [], stepCount: 0, text: "", files: [], canReplace: false, backend: "", elapsed: 0 };
}

/** 事件 → 状态。助手消息永远是"最后一条助手消息"在原地更新。
 *  默认(不展开)时只有：胶囊条 + 紧凑卡（提问/结果/报错）；聊天窗只在用户主动展开（轻点热键 / 点"展开"）时出现。 */
function reduce(st: St, s: Act): St {
  const msgs = [...st.msgs];
  let nid = st.nid;
  const lastAsst = (): AsstMsg | null => {
    const m = msgs[msgs.length - 1];
    return m && m.role === "assistant" ? (m as AsstMsg) : null;
  };
  const patchLast = (p: Partial<AsstMsg>) => {
    let m = lastAsst();
    if (!m) {
      m = newAsst(nid++);
      msgs.push(m);
    }
    msgs[msgs.length - 1] = { ...m, ...p };
  };
  const ensureWorking = () => {
    const m = lastAsst();
    if (!m || m.status !== "working") msgs.push(newAsst(nid++));
  };
  switch (s.phase) {
    case "__expand":
      return { ...st, expanded: true };
    case "listening":
      return { ...st, view: "listening", caption: "", ctx: s.ctx, expanded: s.followUp ? st.expanded : false, msgs: s.followUp ? msgs : [], hasPrev: !!s.followUp };
    case "caption":
      return { ...st, caption: s.text };
    case "ctx":
      return { ...st, ctx: s.ctx };
    case "transcribing":
      return { ...st, view: "transcribing" };
    case "heard":
      msgs.push({ id: nid++, role: "user", text: s.text });
      msgs.push(newAsst(nid++));
      return { ...st, view: "working", said: s.text, caption: s.text, msgs, nid, hasPrev: true };
    case "working":
      ensureWorking();
      patchLast({ steps: s.steps, stepCount: s.stepCount, text: s.text, status: "working", ask: undefined });
      return { ...st, view: "working", msgs, nid };
    case "ask":
      ensureWorking();
      patchLast({ ask: s, status: "working" });
      return { ...st, view: "ask", msgs, nid };
    case "result":
      patchLast({ status: "done", text: s.text, files: s.files, canReplace: s.canReplace, backend: s.backend, elapsed: s.elapsed, ask: undefined });
      return { ...st, view: "result", msgs, nid };
    case "error":
      ensureWorking();
      patchLast({ status: "error", text: s.message, ask: undefined });
      return { ...st, view: "error", msgs, nid };
    case "input":
      return { ...st, view: "input", expanded: true, ctx: s.ctx, msgs: s.hasPrev ? msgs : [], hasPrev: s.hasPrev };
    case "idle":
      return { ...st, view: "hidden" };
  }
}

// ───────────────────────── 根组件 ─────────────────────────

export default function Panel() {
  const [st, dispatch] = useReducer(reduce, INIT);
  const [levels, setLevels] = useState<number[]>(() => Array(WAVE_N).fill(0.05));
  const [autoClose, setAutoClose] = useState(0);
  const cardRef = useRef<HTMLDivElement>(null); // 根容器：包含气泡 + 胶囊（或单张卡片）
  const hoverRef = useRef(false);
  const [hover, setHover] = useState(false); // 执行中：鼠标悬停才展开进度
  const leaveTimer = useRef(0);
  const lastLayout = useRef("");

  useEffect(() => {
    const loadCfg = () => api.getConfig().then((c) => setAutoClose(c.autoCloseSec)).catch(() => {});
    loadCfg();
    document.title = "SayDoPanel";
    const un = [
      listenSync<PanelState>("panel-state", (e) => {
        if (e.payload.phase === "listening") setLevels(Array(WAVE_N).fill(0.05));
        // 新一轮开始 / 面板收起时清掉"悬停"状态。窗口被隐藏、或程序不在前台（菜单栏程序大多时候都是）时，
        // 系统不会再发"指针离开"事件，悬停会一直卡在 true，执行中就永远展开成进度条、变不成小球。
        if (e.payload.phase === "listening" || e.payload.phase === "heard" || e.payload.phase === "idle") {
          window.clearTimeout(leaveTimer.current);
          hoverRef.current = false;
          setHover(false);
        }
        dispatch(e.payload);
      }),
      listenSync<number>("audio-level", (e) => setLevels((l) => [...l.slice(1), Math.max(0.05, e.payload)])),
      listenSync("config-changed", loadCfg),
    ];
    return () => un.forEach((f) => f());
  }, []);

  const chat = st.view === "input" || (st.expanded && st.view !== "listening" && st.view !== "transcribing" && st.view !== "hidden");

  // 把卡片尺寸 / 圆角 / 窗口边框厚度上报给后端，由它摆放并裁出圆角或胶囊形状
  const report = useCallback(() => {
    const el = cardRef.current;
    if (!el) return;
    const dpr = window.devicePixelRatio || 1;
    const rb = el.getBoundingClientRect();
    const h = Math.round(rb.height);
    if (h < 10) return; // 隐藏态的占位元素没有高度，不要上报
    // 每个 [data-part]（气泡 / 胶囊 / 卡片）各报一块圆角矩形，后端把它们并成窗口形状
    const parts = Array.from(el.querySelectorAll<HTMLElement>("[data-part]")).map((p) => {
      const b = p.getBoundingClientRect();
      const r = Math.min(parseFloat(getComputedStyle(p).borderTopLeftRadius) || 0, b.width / 2, b.height / 2);
      return { x: b.left - rb.left, y: b.top - rb.top, w: b.width, h: b.height, r };
    });
    const offx = Math.max(0, Math.round(((window.outerWidth - window.innerWidth) / 2) * dpr));
    const offy = Math.max(0, Math.round((window.outerHeight - window.innerHeight) * dpr) - offx);
    const payload = { w: Math.round(rb.width), h, dpr, offx, offy, radius: parts[0]?.r ?? 0, parts };
    const key = JSON.stringify(payload);
    if (key === lastLayout.current) return; // 没变就不重复上报
    lastLayout.current = key;
    api.layout(payload).catch(() => {});
  }, []);
  // 窗口被激活/缩放后，系统边框厚度可能变化（outer-inner），ResizeObserver 不会触发：定时 + 事件补报
  useEffect(() => {
    const t = setInterval(report, 350);
    window.addEventListener("resize", report);
    window.addEventListener("focus", report);
    return () => { clearInterval(t); window.removeEventListener("resize", report); window.removeEventListener("focus", report); };
  }, [report]);
  useLayoutEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    report();
    const ro = new ResizeObserver(report);
    ro.observe(el);
    return () => ro.disconnect();
  }, [report, st.view]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && api.hide();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);

  // 紧凑结果卡 / 报错卡：N 秒后自动收起（鼠标停在上面时暂停）。聊天窗不自动关。
  const autoCloses = !st.expanded && (st.view === "result" || st.view === "error");
  useEffect(() => {
    if (autoClose <= 0 || !autoCloses) return;
    let left = autoClose;
    const t = setInterval(() => {
      if (hoverRef.current) return;
      if (--left <= 0) api.hide();
    }, 1000);
    return () => clearInterval(t);
  }, [autoCloses, st.msgs, autoClose]);

  // 执行中：悬停以"鼠标真实位置"为准（每 0.2 秒问一次后端），不再依赖会丢的指针进出事件。
  // 进入窗口范围立刻展开；离开要连续两次（约 0.4 秒）才收回，避免边缘抖动。
  useEffect(() => {
    if (st.view !== "working") return;
    let outside = 0;
    let alive = true;
    const t = setInterval(() => {
      api.cursorInside().then((inside) => {
        if (!alive) return;
        if (inside) {
          outside = 0;
          window.clearTimeout(leaveTimer.current);
          hoverRef.current = true;
          setHover(true);
        } else if (++outside >= 2) {
          hoverRef.current = false;
          setHover(false);
        }
      }).catch(() => {});
    }, 200);
    return () => { alive = false; clearInterval(t); };
  }, [st.view]);

  const onMouseDown = (e: React.MouseEvent) => {
    const t = e.target as HTMLElement;
    if (e.button === 0 && t.closest("[data-drag]") && !t.closest("button,input,a")) api.drag();
  };

  const last = st.msgs[st.msgs.length - 1];
  const asst = last && last.role === "assistant" ? last : undefined;
  const pillKind: "listening" | "busy" | "working" | null =
    st.view === "listening" ? "listening" : st.view === "transcribing" ? "busy" : st.view === "working" ? "working" : null;
  const handlers = {
    onPointerEnter: () => {
      window.clearTimeout(leaveTimer.current);
      hoverRef.current = true;
      setHover(true);
    },
    onPointerLeave: () => {
      hoverRef.current = false;
      // 稍等一下再收起，免得窗口随内容变大 / 变小时指针边缘抖动造成闪烁
      leaveTimer.current = window.setTimeout(() => setHover(false), 280);
    },
    onMouseDown,
  };
  if (st.view === "hidden") return <div ref={cardRef} className="stack" />;
  // 执行中：平时只留一个小圆圈，不挡你干别的；鼠标悬停才展开"你说的话 + 任务进度 + 取消"
  if (pillKind === "working" && !hover) {
    return (
      <div ref={cardRef} className="stack" {...handlers}>
        <div data-part className="pwin orb-only" data-drag title="SayDo 正在执行（悬停查看进度）">
          <div className="mic-orb busy" data-drag><Loader2 size={15} className="spin" /></div>
        </div>
      </div>
    );
  }
  if (pillKind) {
    // 追问时（结果卡还开着，直接按热键接着说）：上一轮的对话 / 结果继续留在上面，不收起；
    // 只有"执行中"才缩成小球（上面已处理）
    const keepPrev = st.hasPrev && pillKind !== "working" && !!asst && asst.status !== "working";
    return (
      <div ref={cardRef} className="stack" {...handlers}>
        {keepPrev && (
          <div data-part className="pwin" style={{ width: 340, ["--r" as string]: st.expanded ? "24px" : "20px" }}>
            {st.expanded ? <Chat st={st} /> : asst!.status === "error" ? <ErrorCard msg={asst!.text} said={st.said} /> : <ResultCard m={asst!} said={st.said} onExpand={() => dispatch({ phase: "__expand" })} />}
          </div>
        )}
        {pillKind === "working" ? (
          <ProgressBubble said={st.said} asst={asst} />
        ) : (
          <CaptionBubble kind={pillKind} caption={st.caption} said={st.said} ctx={st.ctx} asst={asst} />
        )}
        <Pill kind={pillKind} levels={levels} />
      </div>
    );
  }
  let body: React.ReactNode;
  if (chat) body = <Chat st={st} />;
  else if (st.view === "ask" && asst?.ask) body = <div style={{ padding: 14 }} data-drag><Ask s={asst.ask} /></div>;
  else if (st.view === "result" && asst) body = <ResultCard m={asst} said={st.said} onExpand={() => dispatch({ phase: "__expand" })} />;
  else if (st.view === "error" && asst) body = <ErrorCard msg={asst.text} said={st.said} />;
  else body = null;

  return (
    <div ref={cardRef} className="stack" {...handlers}>
      <div data-part className="pwin" style={{ width: 340, ["--r" as string]: chat ? "24px" : "20px" }}>
        {body}
      </div>
    </div>
  );
}

// ───────────────────────── 胶囊条：聆听(实时字幕) / 识别 / 执行中 ─────────────────────────

/** 声纹曲线：两条错相位的平滑正弦曲线，振幅跟着麦克风音量走（渐变描边 + 微光），静音时是一条轻轻呼吸的细线 */
function VoiceCurve({ level, idle, w = 76, h = 26 }: { level: number; idle?: boolean; w?: number; h?: number }) {
  const p1 = useRef<SVGPathElement>(null);
  const p2 = useRef<SVGPathElement>(null);
  const lvRef = useRef(level);
  lvRef.current = level;
  useEffect(() => {
    let raf = 0, t = 0, amp = 0.08;
    const build = (a: number, k: number, ph: number) => {
      let d = "";
      for (let x = 0; x <= w; x += 2) {
        const env = Math.pow(Math.sin((Math.PI * x) / w), 1.4);
        const y = h / 2 + Math.sin(x * k + ph) * a * (h / 2 - 2) * env;
        d += (x === 0 ? "M" : "L") + x + " " + y.toFixed(2) + " ";
      }
      return d;
    };
    const tick = () => {
      t += 0.14;
      const target = idle ? 0.22 + 0.08 * Math.sin(t * 0.6) : Math.max(0.07, lvRef.current);
      amp += (target - amp) * 0.22;
      p1.current?.setAttribute("d", build(amp, 0.24, t));
      p2.current?.setAttribute("d", build(amp * 0.62, 0.34, -t * 1.25 + 1.3));
      raf = requestAnimationFrame(tick);
    };
    tick();
    return () => cancelAnimationFrame(raf);
  }, [idle, w, h]);
  return (
    <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} style={{ flex: "none", overflow: "visible" }}>
      <defs>
        <linearGradient id="vc" x1="0" x2="1" y1="0" y2="0">
          <stop offset="0" stopColor="#b06cff" />
          <stop offset="0.5" stopColor="#6f8dff" />
          <stop offset="1" stopColor="#36d1ff" />
        </linearGradient>
      </defs>
      <path ref={p2} fill="none" stroke="url(#vc)" strokeWidth="1.4" strokeLinecap="round" opacity="0.45" />
      <path ref={p1} fill="none" stroke="url(#vc)" strokeWidth="2.2" strokeLinecap="round" style={{ filter: "drop-shadow(0 0 3px rgba(111,141,255,.75))" }} />
    </svg>
  );
}

/** 小巧的胶囊条：麦克风球 + 声纹曲线 + 关闭。字幕不在这里，在它上面的气泡里。 */
function Pill({ kind, levels }: { kind: "listening" | "busy" | "working"; levels: number[] }) {
  const level = levels[levels.length - 1] ?? 0.05;
  const spinning = kind !== "listening";
  return (
    <div data-part className="pwin ppill" data-drag>
      <div className={`mic-orb ${spinning ? "busy" : ""}`} data-drag>
        {spinning ? <Loader2 size={15} className="spin" /> : <Mic size={15} />}
      </div>
      {kind === "working" ? (
        <span data-drag style={{ fontSize: 12, color: "var(--muted)", whiteSpace: "nowrap", padding: "0 2px" }}>执行中…</span>
      ) : (
        <VoiceCurve level={level} idle={kind === "busy"} />
      )}
      <button className="xbtn sm" onPointerDown={(e) => { e.stopPropagation(); void api.hide(); }} title="取消 (Esc)"><X size={13} /></button>
    </div>
  );
}

/** 执行中、鼠标悬停时展开的任务进度：你说的话 + 最近几步（已完成打勾，当前一步转圈）+ 已完成步数 */
function ProgressBubble({ said, asst }: { said: string; asst?: AsstMsg }) {
  const steps = asst?.steps ?? [];
  const lastText = asst?.text ? asst.text.split("\n").filter(Boolean).slice(-1)[0] : "";
  return (
    <div data-part className="pwin pbub" data-drag style={{ ["--r" as string]: "16px", width: 300 }}>
      {said && <div className="pbub-main" title={said}>{said}</div>}
      <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: said ? 7 : 0 }}>
        {steps.length === 0 && !lastText && <div className="pbub-sub">思考中…</div>}
        {steps.map((s, i) => {
          const cur = i === steps.length - 1 && !lastText;
          return (
            <div key={i + s} style={{ display: "flex", alignItems: "center", gap: 7, fontSize: 12, color: cur ? "var(--text)" : "var(--muted)" }}>
              {cur ? <Loader2 size={11} className="spin" style={{ flex: "none" }} /> : <Check size={11} style={{ flex: "none", color: "var(--ok)" }} />}
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{s}</span>
            </div>
          );
        })}
        {asst && asst.stepCount > steps.length && <div className="pbub-sub">…已完成 {asst.stepCount} 步</div>}
        {lastText && <div className="pbub-sub" style={{ whiteSpace: "normal", display: "-webkit-box", WebkitLineClamp: 2, WebkitBoxOrient: "vertical" }}>{lastText}</div>}
      </div>
    </div>
  );
}

/** 字幕气泡：浮在胶囊正上方。聆听时是实时字幕；识别/执行时是"你说的话 + 当前步骤"。没有内容就不出现。 */
function CaptionBubble({ kind, caption, said, ctx, asst }: { kind: "listening" | "busy" | "working"; caption: string; said: string; ctx: CtxView | null; asst?: AsstMsg }) {
  const lastLine = asst?.text ? asst.text.split("\n").filter(Boolean).slice(-1)[0] : asst?.steps[asst.steps.length - 1];
  let main = "";
  let sub = "";
  if (kind === "listening") {
    main = caption;
    sub = caption ? "" : ctx && ctx.kind !== "none" ? (ctx.kind === "pending" ? "读取选中内容…" : ctx.label) : "";
    if (!caption && !sub) main = "请说话…";
  } else if (kind === "busy") {
    main = caption || "正在识别…";
  } else {
    main = said || caption;
    sub = lastLine ?? "思考中…";
  }
  if (!main && !sub) return null;
  const hint = !caption && kind === "listening";
  return (
    <div data-part className="pwin pbub" data-drag style={{ ["--r" as string]: "16px" }}>
      {main && (
        <div className={hint ? "pbub-hint" : "pbub-main"} title={main}>{main}</div>
      )}
      {sub && <div className="pbub-sub" title={sub}>{sub}</div>}
    </div>
  );
}

// ───────────────────────── 紧凑结果卡 / 报错卡（默认的"完成"形态，不是聊天窗） ─────────────────────────

function CardHead({ said, right }: { said: string; right?: React.ReactNode }) {
  return (
    <div data-drag style={{ display: "flex", alignItems: "center", gap: 8, padding: "10px 10px 0 16px" }}>
      <Sparkles size={15} style={{ color: "#b79cff", flex: "none" }} />
      <div data-drag style={{ flex: 1, minWidth: 0, fontSize: 12.5, color: "var(--muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={said}>
        {said ? `「${said}」` : "SayDo"}
      </div>
      {right}
      <button className="btn iconbtn" style={{ background: "transparent", border: "none" }} onClick={() => api.hide()} title="关闭 (Esc)"><X size={16} /></button>
    </div>
  );
}

function ResultCard({ m, said, onExpand }: { m: AsstMsg; said: string; onExpand: () => void }) {
  const hasText = m.text.trim().length > 0;
  return (
    <div>
      <CardHead
        said={said}
        right={<button className="btn iconbtn" style={{ background: "transparent", border: "none" }} onClick={onExpand} title="展开成对话窗口，继续追问"><Maximize2 size={14} /></button>}
      />
      <div style={{ padding: "8px 16px 14px" }}>
        {hasText && <div className="md" style={{ maxHeight: 240, overflowY: "auto", paddingRight: 4, fontSize: 14 }}><Md>{m.text}</Md></div>}
        {!hasText && m.files.length === 0 && <div style={{ color: "var(--muted)" }}>已完成。</div>}
        {m.files.length > 0 && (
          <div style={{ marginTop: hasText ? 10 : 0, display: "flex", flexDirection: "column", gap: 6 }}>
            {m.files.map((f) => <FileCard key={f.path} f={f} />)}
          </div>
        )}
        <Actions m={m} />
      </div>
    </div>
  );
}

function ErrorCard({ msg, said }: { msg: string; said: string }) {
  return (
    <div>
      <CardHead said={said} />
      <div style={{ padding: "8px 16px 14px" }}>
        <div style={{ display: "flex", gap: 9, fontSize: 13.5, lineHeight: 1.55, color: "#ffd0d0" }}>
          <TriangleAlert size={17} style={{ flex: "none", marginTop: 2, color: "#ff8f8f" }} />
          <span style={{ userSelect: "text", wordBreak: "break-word" }}>{msg}</span>
        </div>
        {said && (
          <div style={{ marginTop: 10, display: "flex", gap: 8 }}>
            <button className="btn" onClick={() => api.submit(said)}>重试</button>
          </div>
        )}
      </div>
    </div>
  );
}

// ───────────────────────── 聊天窗 ─────────────────────────

function chipsFor(ctx: CtxView | null, hasPrev: boolean): { text: string; icon: React.ReactNode }[] {
  const bulb = <Lightbulb size={14} />;
  if (ctx?.kind === "files") {
    return [
      { text: "转成 PDF", icon: <FileText size={14} /> }, { text: "移动到某个文件夹", icon: <Folder size={14} /> },
      { text: "总结这个文件的内容", icon: <ScanText size={14} /> }, { text: "压缩成 zip", icon: <Archive size={14} /> },
    ];
  }
  if (ctx?.kind === "text") {
    return [
      { text: "翻译成中文", icon: <Languages size={14} /> }, { text: "Translate to English", icon: <Languages size={14} /> },
      { text: "总结", icon: <ScanText size={14} /> }, { text: "润色", icon: <Sparkles size={14} /> }, { text: "解释一下", icon: bulb },
    ];
  }
  const base = [{ text: "看看当前页面", icon: <MonitorUp size={14} /> }, { text: "总结当前页面", icon: bulb }];
  return hasPrev ? [{ text: "再详细一点", icon: bulb }, ...base] : base;
}

function Chat({ st }: { st: St }) {
  const [v, setV] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const last = st.msgs[st.msgs.length - 1];
  const working = !!last && last.role === "assistant" && last.status === "working";
  const asking = working && !!(last as AsstMsg).ask;
  useEffect(() => {
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
  });
  useEffect(() => {
    if (st.msgs.length === 0) {
      api.focus().catch(() => {});
      setTimeout(() => inputRef.current?.focus(), 80);
    }
  }, [st.msgs.length]);
  const go = (t: string) => {
    if (t.trim()) {
      api.submit(t.trim());
      setV("");
    }
  };
  const showChips = !working && (st.msgs.length === 0 || last?.role === "assistant");
  const greeting = st.ctx?.kind === "files" ? `已选中「${st.ctx.label}」，想让我做什么？` : st.ctx?.kind === "text" ? `已选中 ${st.ctx.label.replace("已选中 ", "")}，想让我做什么？` : "你好，想让我做什么？按住热键说话，或直接打字。";
  return (
    <>
      <div className="chat-head" data-drag>
        <Sparkles size={20} style={{ color: "#b79cff", flex: "none" }} fill="#b79cff33" />
        <div className="chat-title" data-drag>SayDo</div>
        <div className="vdiv" />
        <div className="online" data-drag>
          <span className={`dot ${working ? "busy" : ""}`} />
          {asking ? "等你确认" : working ? "执行中" : "在线"}
        </div>
        <div style={{ flex: 1 }} data-drag />
        {st.ctx && st.ctx.kind !== "none" && st.ctx.kind !== "pending" && (
          <span className="chip" title={st.ctx.preview || st.ctx.title} style={{ maxWidth: 150 }}>
            {st.ctx.kind === "files" ? <FileTypeIcon ext={st.ctx.files[0]?.ext ?? ""} dir={st.ctx.files[0]?.isDir} size={12} /> : st.ctx.kind === "screen" ? <MonitorUp size={12} /> : <ScanText size={12} />}
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{st.ctx.label}</span>
          </span>
        )}
        <button className="xbtn" style={{ width: 28, height: 28, background: "transparent" }} onClick={() => api.hide()} title="关闭 (Esc)"><X size={16} /></button>
      </div>

      <div className="msgs" ref={listRef}>
        {st.msgs.length === 0 && <Bubble>{greeting}</Bubble>}
        {st.msgs.map((m) => (m.role === "user" ? <div key={m.id} className="row-u"><div className="bub-u">{m.text}</div></div> : <AsstBubble key={m.id} m={m} />))}
      </div>

      {showChips && (
        <div className="chips">
          {chipsFor(st.ctx, st.hasPrev).map((c) => (
            <button key={c.text} className="chip2" onClick={() => go(c.text)}>{c.icon}{c.text}</button>
          ))}
        </div>
      )}

      <div className="inputbar">
        <input ref={inputRef} value={v} placeholder="输入消息…" onChange={(e) => setV(e.target.value)} onKeyDown={(e) => e.key === "Enter" && go(v)} />
        <Mic size={19} style={{ color: "#8f93a8", flex: "none" }} aria-label="按住热键说话" />
        <div className="vdiv" />
        <button className="send" onClick={() => go(v)} title="发送"><ArrowUp size={20} /></button>
      </div>
    </>
  );
}

function Avatar() {
  return <div className="avatar"><Sparkles size={19} fill="#e6dcff55" /></div>;
}

function Bubble({ children, wide, err }: { children: React.ReactNode; wide?: boolean; err?: boolean }) {
  return (
    <div className="row-a">
      <Avatar />
      <div className={`bub-a ${wide ? "wide" : ""} ${err ? "err" : ""}`}>{children}</div>
    </div>
  );
}

function AsstBubble({ m }: { m: AsstMsg }) {
  if (m.status === "error") {
    return (
      <Bubble err>
        <div style={{ display: "flex", gap: 8 }}>
          <TriangleAlert size={16} style={{ flex: "none", marginTop: 3, color: "#ff8f8f" }} />
          <span style={{ userSelect: "text", wordBreak: "break-word" }}>{m.text}</span>
        </div>
      </Bubble>
    );
  }
  if (m.ask) return <Bubble wide><Ask s={m.ask} /></Bubble>;
  if (m.status === "working") {
    return (
      <Bubble wide>
        {m.steps.length > 0 && (
          <div style={{ display: "flex", flexDirection: "column", gap: 4, marginBottom: m.text ? 8 : 0 }}>
            {m.steps.map((s, i) => {
              const cur = i === m.steps.length - 1 && !m.text;
              return (
                <div key={i + s} style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12.5, color: cur ? "var(--text)" : "var(--muted)" }}>
                  {cur ? <Loader2 size={12} className="spin" style={{ flex: "none" }} /> : <Check size={12} style={{ flex: "none", color: "var(--ok)" }} />}
                  <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{s}</span>
                </div>
              );
            })}
            {m.stepCount > m.steps.length && <div style={{ fontSize: 11, color: "var(--muted)", paddingLeft: 20 }}>…已完成 {m.stepCount} 步</div>}
          </div>
        )}
        {m.text ? <div className="md"><Md>{m.text}</Md></div> : m.steps.length === 0 ? <div className="shimmer" style={{ height: 10, width: 140, borderRadius: 6 }} /> : null}
      </Bubble>
    );
  }
  // done
  const hasText = m.text.trim().length > 0;
  return (
    <Bubble wide>
      {hasText ? <div className="md"><Md>{m.text}</Md></div> : m.files.length === 0 ? <span style={{ color: "var(--muted)" }}>已完成。</span> : null}
      {m.files.length > 0 && (
        <div style={{ marginTop: hasText ? 10 : 0, display: "flex", flexDirection: "column", gap: 6 }}>
          {m.files.map((f) => <FileCard key={f.path} f={f} />)}
        </div>
      )}
      <Actions m={m} />
    </Bubble>
  );
}

function Actions({ m }: { m: AsstMsg }) {
  const [copied, setCopied] = useState(false);
  const hasText = m.text.trim().length > 0;
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10, flexWrap: "wrap" }}>
      {hasText && (
        <button className="btn" onClick={() => { navigator.clipboard.writeText(m.text); setCopied(true); setTimeout(() => setCopied(false), 1200); }}>
          {copied ? <Check size={14} style={{ color: "var(--ok)" }} /> : <ClipboardCopy size={14} />} 复制
        </button>
      )}
      {hasText && m.canReplace && (
        <button className="btn btn-primary" onClick={() => api.replace(m.text)} title="把结果粘贴回刚才选中文字的位置"><Replace size={14} /> 替换原文</button>
      )}
      {m.files.length > 1 && <button className="btn" onClick={() => api.revealPath(m.files[0].path)}><FolderOpen size={14} /> 所在文件夹</button>}
      <span style={{ marginLeft: "auto", fontSize: 11, color: "var(--muted)" }}>{BACKEND_NAMES[m.backend] ?? m.backend} · {m.elapsed}s</span>
    </div>
  );
}

// ───────────────────────── 文件卡 ─────────────────────────

function FileTypeIcon({ ext, dir, size = 18 }: { ext: string; dir?: boolean; size?: number }) {
  if (dir) return <Folder size={size} />;
  if (["xlsx", "xls", "csv", "tsv"].includes(ext)) return <FileSpreadsheet size={size} />;
  if (["pptx", "ppt"].includes(ext)) return <Presentation size={size} />;
  if (["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "tif", "tiff"].includes(ext)) return <FileImage size={size} />;
  if (["zip", "7z", "rar", "tar", "gz"].includes(ext)) return <Archive size={size} />;
  if (["doc", "docx", "pdf", "txt", "md", "rtf"].includes(ext)) return <FileText size={size} />;
  return <FileIcon size={size} />;
}

function FileCard({ f }: { f: FileView }) {
  const [copied, setCopied] = useState(false);
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "8px 9px", borderRadius: 14, background: "rgba(0,0,0,.22)", border: "1px solid var(--line)" }}>
      <div style={{ width: 36, height: 36, borderRadius: 10, display: "grid", placeItems: "center", background: "var(--accent-soft)", color: "var(--accent)", flex: "none" }}>
        <FileTypeIcon ext={f.ext} dir={f.isDir} />
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontWeight: 600, fontSize: 13, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={f.path}>{f.name}</div>
        <div style={{ fontSize: 11, color: "var(--muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", direction: "rtl", textAlign: "left" }} title={f.path}>
          <bdi>{f.dir}{f.isDir ? "" : ` · ${fmtSize(f.size)}`}</bdi>
        </div>
      </div>
      <button className="btn iconbtn" title="打开" onClick={() => api.openPath(f.path)}><Eye size={14} /></button>
      <button className="btn iconbtn" title="在文件夹中显示" onClick={() => api.revealPath(f.path)}><FolderOpen size={14} /></button>
      <button className="btn iconbtn" title="复制路径" onClick={() => { navigator.clipboard.writeText(f.path); setCopied(true); setTimeout(() => setCopied(false), 1200); }}>
        {copied ? <Check size={14} style={{ color: "var(--ok)" }} /> : <ClipboardCopy size={14} />}
      </button>
    </div>
  );
}

// ───────────────────────── 提问（在助手气泡里） ─────────────────────────

function Ask({ s }: { s: AskSpec }) {
  const [text, setText] = useState("");
  const reply = (v: unknown) => api.askReply(s.id, v);

  if (s.kind === "folder") {
    const browse = async () => {
      const p = await api.browseFolder(s.suggestions?.[0]?.path);
      if (p) reply({ path: p });
    };
    return (
      <div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
          <FolderOpen size={16} style={{ color: "var(--accent)" }} />
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: 600 }}>{s.title}</div>
            {s.detail && <div style={{ fontSize: 11.5, color: "var(--muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{s.detail}</div>}
          </div>
          <button className="btn iconbtn" onClick={() => reply({ cancel: true })} title="取消"><X size={14} /></button>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4, maxHeight: 230, overflowY: "auto" }}>
          {(s.suggestions ?? []).slice(0, 8).map((f) => (
            <button key={f.path} className="btn" style={{ height: 38, justifyContent: "flex-start", gap: 10, padding: "0 10px", background: "rgba(0,0,0,.2)" }} onClick={() => reply({ path: f.path })}>
              <Folder size={15} style={{ color: f.kind === "recent" ? "var(--accent)" : f.kind === "source" ? "var(--ok)" : "var(--muted)", flex: "none" }} />
              <span style={{ fontWeight: 600 }}>{f.name}</span>
              <span style={{ color: "var(--muted)", fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", direction: "rtl", textAlign: "left", flex: 1 }}><bdi>{f.path}</bdi></span>
            </button>
          ))}
        </div>
        <button className="btn btn-primary" style={{ width: "100%", marginTop: 8, height: 34 }} onClick={browse}><FolderOpen size={14} /> 浏览其它文件夹…</button>
      </div>
    );
  }
  if (s.kind === "confirm") {
    return (
      <div>
        <div style={{ display: "flex", gap: 8, alignItems: "flex-start" }}>
          <ShieldQuestion size={17} style={{ flex: "none", marginTop: 2, color: s.danger ? "#ffb35c" : "var(--accent)" }} />
          <div style={{ fontWeight: 600, lineHeight: 1.45 }}>{s.title}</div>
        </div>
        {s.detail && (
          <pre style={{ margin: "8px 0 0", maxHeight: 150, overflow: "auto", whiteSpace: "pre-wrap", wordBreak: "break-all", fontSize: 11.5, background: "rgba(0,0,0,.32)", padding: "8px 10px", borderRadius: 10, color: "#c9ccd8", userSelect: "text", fontFamily: "Cascadia Code, Consolas, monospace" }}>{s.detail}</pre>
        )}
        <div style={{ display: "flex", gap: 8, marginTop: 10, justifyContent: "flex-end", flexWrap: "wrap" }}>
          <button className="btn btn-danger" onClick={() => reply({ ok: false })}>拒绝</button>
          {s.allowAlways && <button className="btn" onClick={() => reply({ ok: true, always: true })}>本次都允许</button>}
          <button className="btn btn-primary" onClick={() => reply({ ok: true })}>允许</button>
        </div>
      </div>
    );
  }
  return (
    <div>
      <div style={{ fontWeight: 600, lineHeight: 1.45 }}>{s.title}</div>
      {s.kind === "choice" ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 6, marginTop: 8 }}>
          {(s.options ?? []).map((o) => <button key={o} className="btn" style={{ justifyContent: "flex-start", height: 34 }} onClick={() => reply({ value: o })}>{o}</button>)}
        </div>
      ) : (
        <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
          <input autoFocus className="s-input" style={{ background: "rgba(0,0,0,.25)", color: "var(--text)", borderColor: "var(--line)" }} value={text} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => e.key === "Enter" && reply({ value: text })} />
          <button className="btn btn-primary" onClick={() => reply({ value: text })}>确定</button>
        </div>
      )}
    </div>
  );
}
