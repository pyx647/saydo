// 前端 ↔ Python 后端的通信层：命令走 POST /api/<cmd>，事件走 SSE /events。
// 没有 ?t=token（浏览器里直接开 dev 预览）时退回到 dev/mock，用来在没有后端的情况下检查界面。
const q = new URLSearchParams(location.search);
export const TOKEN = q.get("t") ?? "";
export const WINDOW = q.get("w") ?? "panel";
// Tauri（Rust 后端）：走 IPC；否则（Python 后端 / 浏览器预览）走 HTTP + SSE
export const IS_TAURI = typeof (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== "undefined";
const live = !!TOKEN || IS_TAURI;

type Handler = (p: unknown) => void;
const handlers = new Map<string, Set<Handler>>();
let es: EventSource | null = null;

export const listenerCount = (event: string) => handlers.get(event)?.size ?? 0;

export function dispatch(event: string, payload: unknown) {
  handlers.get(event)?.forEach((h) => h(payload));
}

function ensureStream() {
  if (es || !live || IS_TAURI) return;
  es = new EventSource(`/events?t=${encodeURIComponent(TOKEN)}`);
  es.onmessage = (m) => {
    try {
      const { event, payload } = JSON.parse(m.data);
      dispatch(event, payload);
    } catch {
      /* ignore */
    }
  };
}

/** 同步注册、同步注销（React 开发模式会把 effect 跑两遍，异步注销会漏掉第一轮的监听器，导致事件被处理两次） */
export function listenSync<T>(event: string, cb: (e: { payload: T }) => void): () => void {
  const h: Handler = (p) => cb({ payload: p as T });
  let set = handlers.get(event);
  if (!set) handlers.set(event, (set = new Set()));
  set.add(h);
  ensureStream();
  if (IS_TAURI) {
    // 事件由 Rust 推送；订阅是异步的，注销时要等订阅完成
    const un = import("@tauri-apps/api/event").then((m) => m.listen<unknown>(event, (e) => h(e.payload)));
    return () => {
      set!.delete(h);
      void un.then((f) => f());
    };
  }
  return () => void set!.delete(h);
}

export function listen<T>(event: string, cb: (e: { payload: T }) => void): Promise<() => void> {
  return Promise.resolve(listenSync(event, cb));
}

export async function invoke<T = unknown>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (IS_TAURI) return (await import("@tauri-apps/api/core")).invoke<T>(cmd, args);
  if (!live) return (await import("../dev/mock")).mockInvoke(cmd, args) as Promise<T>;
  const r = await fetch(`/api/${cmd}`, { method: "POST", headers: { "Content-Type": "application/json", "X-Token": TOKEN }, body: JSON.stringify(args) });
  const j = await r.json();
  if (!j.ok) throw new Error(j.error || "请求失败");
  return j.result as T;
}
