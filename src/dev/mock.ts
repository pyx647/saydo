// 仅开发预览用：浏览器里直接打开(没有 ?t=token)时，transport 会改走这里，用来在没有后端的情况下检查界面。
// 用法：http://localhost:1430/?w=panel&s=result-files   |   ?w=settings
/* eslint-disable @typescript-eslint/no-explicit-any */
import { dispatch, listenerCount } from "../lib/transport";

const emit = dispatch;

const SAMPLE_FILE = { path: "C:\\Users\\user\\Documents\\周报\\week45_report.pdf", name: "week45_report.pdf", dir: "C:\\Users\\user\\Documents\\周报", ext: "pdf", size: 1843200, isDir: false };
const DOCX = { path: "C:\\Users\\user\\Documents\\周报\\week45_report.docx", name: "week45_report.docx", dir: "C:\\Users\\user\\Documents\\周报", ext: "docx", size: 482304, isDir: false };

const CFG = {
  hotkey: "Ctrl+Alt+Space", backend: "claude", lang: "auto", sttEngine: "local", tapOpensInput: true, autoCloseSec: 0, approval: "balanced", fullAccess: true, keepHooks: false, warmClaude: true, extraPrompt: "", profile: "", memories: [],
  claude: { path: "", model: "", extraArgs: "" }, codex: { path: "", model: "", extraArgs: "" }, agy: { path: "", model: "", extraArgs: "" },
  openai: { baseUrl: "https://api.openai.com/v1", apiKey: "", model: "gpt-5-mini" }, cloudStt: { baseUrl: "https://api.openai.com/v1", apiKey: "", model: "gpt-4o-mini-transcribe" }, recentFolders: [],
};

const STATES: Record<string, any> = {
  listening: { phase: "listening", ctx: { kind: "files", label: "week45_report.docx", preview: "", files: [DOCX], app: "explorer.exe", title: "周报" } },
  "listening-text": { phase: "listening", ctx: { kind: "text", label: "已选中 128 字", preview: "The quick brown fox…", files: [], app: "chrome.exe", title: "Paper" } },
  transcribing: { phase: "transcribing" },
  working: { phase: "working", steps: ["启动 Claude Code…", "读取：week45_report.docx", "运行命令：转换为 PDF (Word COM)", "Gmail · search threads"], stepCount: 6, text: "", elapsed: 7 },
  "working-text": { phase: "working", steps: ["思考中…"], stepCount: 1, text: "这段话的意思是：**快速的棕色狐狸**跳过了懒狗。\n\n- 常用于测试字体\n- 包含全部 26 个字母", elapsed: 3 },
  "ask-folder": {
    phase: "ask", id: "a1", kind: "folder", title: "放到哪个文件夹？", detail: "week45_report.docx",
    suggestions: [
      { path: "C:\\Users\\user\\Documents\\周报", name: "源文件所在文件夹", kind: "source" },
      { path: "D:\\Work\\reports", name: "reports", kind: "recent" },
      { path: "C:\\Users\\user\\Desktop", name: "桌面", kind: "known" },
      { path: "C:\\Users\\user\\Documents", name: "文档", kind: "known" },
      { path: "C:\\Users\\user\\Downloads", name: "下载", kind: "known" },
    ],
  },
  "ask-confirm": { phase: "ask", id: "a2", kind: "confirm", title: "允许执行：Gmail · send message？", detail: '{\n  "to": "sam@kribb.re.kr",\n  "subject": "Week 45 report",\n  "body": "Hi Sam, attached is this week\'s report…"\n}', allowAlways: true, danger: true },
  "result-text": { phase: "result", text: "**Translation**\n\n快速的棕色狐狸跳过了懒狗。\n\n> 这是一个包含英文全部字母的经典句子。", files: [], canReplace: true, backend: "claude", elapsed: 4, instruction: "翻译成中文" },
  "result-files": { phase: "result", text: "已转换为 PDF，保留了原有页眉页脚和 3 张图表。", files: [SAMPLE_FILE], canReplace: false, backend: "claude", elapsed: 12, instruction: "把这个文件转成 PDF" },
  "result-screen": { phase: "result", text: "这是 NCBI 的 **Genome 页面**，当前在查看 *Zunongwangia mangrovi* 的组装详情：\n\n1. 组装级别：Complete Genome\n2. 大小：4.2 Mb\n3. GC 含量：35.8%\n\n页面顶部有一条黄色提示：数据正在更新。", files: [], canReplace: false, backend: "claude", elapsed: 9, instruction: "帮我看一下当前页面怎么着的" },
  error: { phase: "error", message: "找不到 claude 命令行。请先安装 Claude Code（设置 → 后端 里可手动指定路径）" },
  input: { phase: "input", ctx: { kind: "text", label: "已选中 128 字", preview: "", files: [], app: "chrome.exe", title: "" }, hasPrev: false },
  "input-files": { phase: "input", ctx: { kind: "files", label: "week45_report.docx", preview: "", files: [DOCX], app: "explorer.exe", title: "" }, hasPrev: false },
};

export function mockInvoke(cmd: string, _args: any): any {
  switch (cmd) {
    case "get_config": return CFG;
    case "detect_clis":
      return [
        { id: "claude", name: "Claude Code", found: true, path: "claude.exe", version: "2.1.272 (Claude Code)" },
        { id: "codex", name: "Codex CLI", found: true, path: "codex.cmd", version: "codex-cli 0.165.0" },
        { id: "agy", name: "Antigravity", found: true, path: "agy.exe", version: "1.5.5" },
      ];
    case "stt_status": return { ready: true, dir: "%APPDATA%/com.user.saydo/stt-model" };
    case "app_info": return { exe: "saydo/main.pyw", configDir: "%APPDATA%/com.user.saydo", version: "0.1.0", paused: false };
    case "history_list":
      return [
        { ts: 1794370000, instruction: "把这个文件转成 PDF", result: "已转换为 PDF。", backend: "claude", context: "week45_report.docx", files: [SAMPLE_FILE.path], ok: true },
        { ts: 1794366000, instruction: "翻译成中文", result: "快速的棕色狐狸跳过了懒狗。", backend: "claude", context: "已选中 128 字", files: [], ok: true },
        { ts: 1794360000, instruction: "帮我看一下当前页面", result: "找不到 claude 命令行", backend: "codex", context: "当前屏幕 / 页面", files: [], ok: false },
      ];
    case "autostart_get": return false;
    case "test_backend": return "成功（3.2s）";
    default: return null;
  }
}

export function installPanelPreview() {
  const q = new URLSearchParams(location.search);
  if ((q.get("w") ?? "panel") !== "panel") return;
  document.body.style.background = "linear-gradient(135deg,#3b4a6b,#1f2a44 60%,#2d1f44)";
  const s = q.get("s") ?? "listening";
  // 等 Panel 的监听器注册好再发
  const wait = setInterval(() => {
    if (!(listenerCount("panel-state") && listenerCount("audio-level"))) return;
    clearInterval(wait);
    setTimeout(() => {
      if (s === "thread") {
        // 一整段对话：听 → 识别 → 说话 → 执行 → 结果
        const seq: [number, any][] = [
          [0, STATES.listening], [900, { phase: "caption", text: "把这个" }], [1700, { phase: "caption", text: "把这个文件转成" }],
          [2500, { phase: "caption", text: "把这个文件转成 PDF 格式" }], [3200, STATES.transcribing],
          [3700, { phase: "heard", text: "把这个文件转成 PDF 格式" }],
          [3800, { phase: "working", steps: ["启动 Claude Code…"], stepCount: 1, text: "", elapsed: 1 }],
          [5200, { phase: "working", steps: ["启动 Claude Code…", "运行命令：转换为 PDF (Word COM)"], stepCount: 2, text: "", elapsed: 3 }],
          [7000, { ...STATES["result-files"], instruction: "把这个文件转成 PDF 格式" }],
        ];
        seq.forEach(([t, st]) => setTimeout(() => emit("panel-state", st), t));
      } else {
        emit("panel-state", STATES[s] ?? STATES.listening);
      }
      if (s.startsWith("listening") || s === "thread") {
        let t = 0;
        setInterval(() => emit("audio-level", Math.max(0.05, Math.abs(Math.sin(t++ / 3)) * 0.8 * Math.random() + 0.1)), 60);
      }
    }, 80);
  }, 50);
}
