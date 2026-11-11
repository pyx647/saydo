"""意图路由（本地规则，零延迟零成本）：
- 屏幕意图："帮我看一下当前页面…" → 先截屏，并且丢掉选区（用户明确说的）。
- 选目标文件夹："把这个文件移到某个文件夹" → 先弹文件夹选择器，选完再交给 agent。"""

SCREEN_WORDS = [
    "当前页面", "这个页面", "本页", "这一页", "当前界面", "这个界面", "屏幕", "截图", "截屏", "当前窗口", "这个窗口", "当前网页", "这个网页", "网页上", "页面上", "界面上", "画面",
    "this page", "current page", "the page", "on screen", "on the screen", "on my screen", "screenshot", "this screen", "this window", "current window", "this tab",
    "화면", "이 페이지", "현재 페이지", "스크린샷", "이 창",
]
MOVE_WORDS = [
    "移动到", "移到", "挪到", "搬到", "放到", "放进", "存到", "保存到", "复制到", "拷贝到", "拷到", "另存到", "归档到", "移动", "挪动",
    "move to", "move it", "move this", "copy to", "copy it", "copy this", "save to", "save it", "put it", "이동", "옮겨", "복사",
]
KNOWN_PLACES = ["桌面", "下载", "文档", "图片", "视频", "音乐", "desktop", "downloads", "documents", "pictures", "videos", "바탕화면", "다운로드", "문서"]


def is_screen_intent(cmd: str) -> bool:
    c = cmd.lower()
    return any(w in c for w in SCREEN_WORDS)


def needs_folder(cmd: str, sel_kind: str) -> bool:
    if sel_kind != "files":
        return False
    c = cmd.lower()
    if not any(w in c for w in MOVE_WORDS):
        return False
    has_path = ":\\" in c or ":/" in c or "\\\\" in c
    has_place = any(p in c for p in KNOWN_PLACES)
    return not (has_path or has_place)
