# SayDo · 说即办

选中一个文件 / 一段文字 → **按住热键不放，说你要干什么** → 松开 → 完成。

```
选中一段中文   按住 Ctrl+Alt+Space   "把这段话翻译成英文"   松开   → 胶囊条实时显示字幕 → 结果卡：译文 [复制][替换原文]
选中 report.docx   按住 …           "把它转成 PDF"          松开   → 结果卡：文件卡 [打开][定位][复制路径]
什么都不选         按住 …           "看一下当前页面怎么回事" 松开   → 自动截屏(忽略选中内容) → 结果卡
选中 report.docx   按住 …           "移到某个文件夹"         松开   → 弹出文件夹选择卡 → 移动 → 告诉你新位置
```

**交互原则：快进快出。** 平时只有屏幕底部正中一条小巧的胶囊（声纹曲线）加上方一个**字幕气泡**（聆听时实时显示识别文字，让你确认识别对不对），以及一张几秒后自动收起的结果卡。
**只有 Agent 要问你问题时（选文件夹 / 确认发邮件）才弹出卡片**。对话窗只在你主动要时出现：轻点一下热键，或点结果卡上的"展开"。

## 启动

**主力版本是 Rust / Tauri**（单个 exe，内存小、启动快）。这台电脑的 Smart App Control 会拦截本机新编译的程序，所以**在 GitHub Actions 上编译**，下载产物使用：

```powershell
gh run download <run-id> --repo pyx647/saydo -n SayDo-windows -D build   # 得到 saydo.exe 和 NSIS 安装包
```

首次运行会打开设置页。语音模型在 `%APPDATA%\com.user.saydo\stt-model\`（别的机器在 设置 → 语音 里一键下载）。需要至少一个 CLI（Claude Code / Codex / agy）。

> 原 Python 版（`app/`、`saydo.pyw`）仍保留，功能更少，不再维护。

## 速度与内存（本机实测）

| 项 | 数值 |
|---|---|
| Claude Code，一次性调用（继承全部 MCP/Skills） | 约 9.5 秒 |
| Claude Code，常驻进程，每条 | 约 2~3 秒（启动时静默预热，第一个任务也是热的） |
| Codex，一次性 `exec` | 约 10~12 秒 |
| Codex，常驻 `app-server`，每条 | 约 1.5~3 秒 |
| `saydo.exe` 空闲内存 | 约 10 MB（语音模型按需加载、闲置 5 分钟卸载） |
| 面板 WebView2 | 约 160~180 MB |
| 常驻的 Claude Code 进程 | 约 600 MB（要连你的全部 MCP 和 Skills）；选哪个后端就只常驻哪个 |

## 记忆

设置 → 记忆：写一次"关于我"，再加"记住的事"。按住热键说"记住：……"随口添加，说"忘掉……"删除（本地直接处理，瞬间完成）。每条指令发给 Agent 时都会带上。

## 取消

聆听、识别、执行的任何阶段按 **Esc** 取消整个任务；聆听时字幕实时显示，说错了当场就能看到。

## 为什么主力是 Code CLI

你的 Google 全家桶（Gmail / Drive / Sheets / 日历）、各种 MCP、几百个 Skills 都已配在 Claude Code / Codex / agy 里，SayDo 只负责"抓上下文 + 听你说话 + 弹窗"，**原样继承**这一切（已实测 `claude -p` 加载 Gmail/Drive/Sheets 连接器 + 622 个 Skill，`codex exec` 加载 12 个 MCP + 592 个 Skill）。

| 后端 | 定位 | MCP / Skills | 逐项审批 | 响应 |
|---|---|---|---|---|
| **Claude Code**（默认，**常驻预热**） | 主力 | ✅ | ✅（发邮件等对外动作弹窗确认） | 首条约 10s，之后 4~6s |
| **Codex** | 主力 | ✅ | ❌ 靠"整机权限"开关 | 每次冷启动 |
| **agy**（Antigravity） | 主力 | ✅（它自己的） | ❌ | 冷启动仅 ~4s |
| **OpenAI API** | 快速模式 | ❌ | — | 仅翻译/总结/看屏幕 |

> `claude -p` 单次冷启动要 15~18 秒（实测与 MCP 无关，是 Claude Code 自身启动）。所以 SayDo 启动时就把它常驻起来（设置 → 权限与安全 → 预热），代价约 300MB 内存，每 10 个任务自动换新进程以免上下文累积。

## 功能

- **按住说话 / 轻点打字**：按住 = 录音，松开 = 执行；轻点（<0.3s）= 打开输入对话窗（带按上下文生成的快捷芯片）。热键可改，支持键盘组合键和**鼠标侧键**。
- **实时字幕**（本地引擎）：按住期间每 0.6s 重识别一次，边说边显示；松开后用最终结果执行。云端转写引擎不支持流式，松开后才出字。
- **自动识别上下文**：选中的文件（含桌面/资源管理器多选）、选中的文字、前台程序与窗口标题；抓取后**还原你原来的剪贴板**；终端里用 Ctrl+Insert 避免 SIGINT。
- **按下热键的瞬间同时做三件事**：识别选中的文字、选中的文件、给当前页面截一张图。截图**只保存**（`%APPDATA%\com.user.saydo\shots\`，保留最近 12 张），**不会主动读取**；只有任务确实要看屏幕时，Agent 才去读那个文件。
- **屏幕意图**：说"当前页面/屏幕/截图…"直接用刚才保存的那张截图并丢掉选区。**选文件夹**：说"移到某个文件夹"先弹选择卡。
- **动作审批**（Claude Code）：谨慎 / 平衡（默认）/ 全自动。平衡档下 `rm -r`、`git push`、发邮件、写 Drive 等有副作用的动作弹窗问你。
- **内置 MCP 服务器**（`saydo`）：`pick_folder / confirm / ask_user / screenshot / get_context / approve`，Claude / Codex 自动挂载。
- 本地离线语音（SenseVoice，中英日韩粤混说）、历史记录、托盘、开机自启、暂停热键、可选常驻偏好提示词。

## 架构

```
热键(LL 钩子,吞主键) ─▶ session.py ─▶ capture.py  抓选区/文件/前台窗口（SendInput 带间隔+扫描码）
                          │          ─▶ winapi.Recorder  waveIn 录音 16k ─▶ 实时字幕线程 ─▶ stt.py (SenseVoice/numpy)
                          ├─ router.py  屏幕意图 / 先选文件夹
                          └─ agent.py   提示词 + claude(常驻) | codex | agy | openai ─▶ 流式事件
   ui.py ─ Edge --app 窗口(DWM 关掉 Win11 云母背景与标题栏 + SetWindowRgn 按页面上报的多块圆角区域裁形, 固定停靠屏幕底部正中, NOACTIVATE 不抢焦点) ◀─ server.py (HTTP+SSE, 令牌+Host/Origin 校验)
   mcp_stdio.py ◀─stdio─ Claude Code / Codex ──HTTP(127.0.0.1,桥令牌)─▶ server.py /call ─▶ tools.py ─▶ ui.ask() 弹窗
```

全部是 Python + `ctypes` 直接调 Windows API，**不在本机编译任何可执行文件**（这台机器的 Smart App Control 会拦截新编译的 exe，所以原 Rust/Tauri 版没法编译，保留在 `src-tauri/` 未动）。

## 开发 / 测试

```powershell
pnpm install ; pnpm build                       # 前端 → dist/（Python 直接提供它）
python -m unittest discover -s tests -p "test_*.py"   # 单元测试（路由/审批/FILE 解析/热键规格）
python tests/e2e.py                             # 端到端：合成语音→模拟热键→抓选区→识别→Claude→结果（调用一次真实 Claude）
python tests/visual.py                          # 界面：驱动真实 Edge 弹窗并截取可见区域（VIS_TK=1 模拟"前台是别的应用"）
```

浏览器里预览界面（无需后端）：`pnpm dev` 后打开 `http://localhost:1430/?w=panel&s=thread`（`s=` 可选 `listening` `ask-folder` `ask-confirm` `working` `result-files` `error` `input`…）、`?w=settings`。

## 已在真机验证（2026-11-11）

| 项 | 结果 |
|---|---|
| 选区抓取：文字（含按着 Alt）/ 资源管理器文件（CF_HDROP） | ✅ 原剪贴板与 Alt 状态都能还原 |
| 本地语音识别（SAPI 合成中/英文） | ✅ "请把这个文件转成 pdf 格式" |
| 实时字幕 | ✅ 松开前 ~0.7s 就出字，与最终一致 |
| 端到端：热键→选区→识别→Claude→译文结果 | ✅ 4 次通过；内置 MCP 桥被 Claude 真实调用过 |
| 弹窗真实窗口：胶囊 / 气泡 / 结果卡 / 不抢焦点 / 无标题栏、无灰边 | ✅ 在纯品红背景上截图核对 |
| 单元测试 | ✅ 7 个 |

托盘/应用图标是 `assets/make_icon.py` 生成的 SayDo 专属图标。

**尚未验证**：用真实热键 + 真实麦克风走一遍"按下即截图"的完整流程、底部居中停靠与拖动的手感；文件夹选择卡与审批卡的端到端点击流程（界面在预览里验证过，后端 ask/reply 通路也被 MCP 桥覆盖，但没有真人点过）；Codex / agy 后端联调；真实麦克风（只用合成语音测过；录音器单独录过 1.2s 正常）；对 Word 转 PDF 等具体文件任务的结果。
