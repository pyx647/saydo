import { invoke } from "./transport";

export type FileView = { path: string; name: string; dir: string; ext: string; size: number; isDir: boolean };
export type CtxView = {
  kind: "none" | "text" | "files" | "screen" | "pending";
  label: string;
  preview: string;
  files: FileView[];
  app: string;
  title: string;
};
export type FolderItem = { path: string; name: string; kind: "source" | "recent" | "known" };

export type PanelState =
  | { phase: "idle" }
  | { phase: "listening"; ctx: CtxView; followUp?: boolean }
  | { phase: "ctx"; ctx: CtxView }
  | { phase: "caption"; text: string; final?: boolean }
  | { phase: "transcribing" }
  | { phase: "heard"; text: string }
  | { phase: "working"; steps: string[]; stepCount: number; text: string; elapsed: number }
  | {
      phase: "ask";
      id: string;
      kind: "folder" | "confirm" | "choice" | "text";
      title: string;
      detail?: string;
      options?: string[];
      suggestions?: FolderItem[];
      allowAlways?: boolean;
      danger?: boolean;
    }
  | { phase: "result"; text: string; files: FileView[]; canReplace: boolean; backend: string; elapsed: number; instruction: string }
  | { phase: "error"; message: string }
  | { phase: "input"; ctx: CtxView; hasPrev: boolean };

export type CliCfg = { path: string; model: string; extraArgs: string; effort?: string };
export type ApiCfg = { baseUrl: string; apiKey: string; model: string };
export type Config = {
  hotkey: string;
  backend: "claude" | "codex" | "agy" | "openai";
  lang: string;
  sttEngine: "local" | "cloud";
  tapOpensInput: boolean;
  autoCloseSec: number;
  approval: "careful" | "balanced" | "auto";
  fullAccess: boolean;
  keepHooks: boolean;
  warmClaude: boolean;
  extraPrompt: string;
  profile: string;
  memories: string[];
  claude: CliCfg;
  codex: CliCfg;
  agy: CliCfg;
  openai: ApiCfg;
  cloudStt: ApiCfg;
  recentFolders: string[];
};
export type CliInfo = { id: string; name: string; found: boolean; path: string; version: string };
export type HistoryEntry = { ts: number; instruction: string; result: string; backend: string; context: string; files: string[]; ok: boolean };

export const api = {
  getConfig: () => invoke<Config>("get_config"),
  saveConfig: (config: Config) => invoke<void>("save_config", { config }),
  detectClis: () => invoke<CliInfo[]>("detect_clis"),
  sttStatus: () => invoke<{ ready: boolean; dir: string }>("stt_status"),
  sttDownload: () => invoke<void>("stt_download_model"),
  appInfo: () => invoke<{ exe: string; configDir: string; version: string; paused: boolean }>("app_info"),
  setPaused: (paused: boolean) => invoke<void>("set_paused", { paused }),
  recordHotkey: () => invoke<string>("record_hotkey"),
  testBackend: (backend?: string) => invoke<string>("test_backend", { backend }),
  historyList: () => invoke<HistoryEntry[]>("history_list"),
  historyClear: () => invoke<void>("history_clear"),
  submit: (text: string) => invoke<void>("panel_submit", { text }),
  replace: (text: string) => invoke<void>("panel_replace", { text }),
  layout: (l: { w: number; h: number; dpr: number; offx: number; offy: number; radius: number; parts?: { x: number; y: number; w: number; h: number; r: number }[] }) => invoke<void>("panel_layout", l),
  drag: () => invoke<void>("panel_drag"),
  cursorInside: () => invoke<boolean>("panel_cursor_inside"),
  focus: () => invoke<void>("panel_focus"),
  autostartGet: () => invoke<boolean>("autostart_get"),
  autostartSet: (on: boolean) => invoke<void>("autostart_set", { on }),
  hide: () => invoke<void>("panel_hide"),
  askReply: (id: string, value: unknown) => invoke<void>("ask_reply", { id, value }),
  browseFolder: (start?: string) => invoke<string | null>("browse_folder", { start }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
};

export const BACKEND_NAMES: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  agy: "Antigravity",
  openai: "OpenAI 快速模式",
  memory: "记忆",
};

export function fmtSize(n: number): string {
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(1) + " GB";
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + " MB";
  if (n >= 1 << 10) return Math.round(n / (1 << 10)) + " KB";
  return n + " B";
}
