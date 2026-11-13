//! 意图路由（本地规则，零延迟、零成本）：
//! - 屏幕意图："帮我看一下当前页面…" → 先截屏，并且丢掉选区（用户明确说的）。
//! - 选目标文件夹："把这个文件移到某个文件夹" → 先弹文件夹选择器，选完再交给 agent。

use crate::capture::Selection;

const SCREEN_WORDS: &[&str] = &[
    "当前页面", "这个页面", "本页", "这一页", "当前界面", "这个界面", "屏幕", "截图", "截屏", "当前窗口", "这个窗口", "当前网页", "这个网页", "网页上", "页面上", "界面上", "画面",
    "this page", "current page", "the page", "on screen", "on the screen", "on my screen", "screenshot", "this screen", "this window", "current window", "this tab",
    "화면", "이 페이지", "현재 페이지", "스크린샷", "이 창",
];

pub fn is_screen_intent(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    SCREEN_WORDS.iter().any(|w| c.contains(w))
}

const MOVE_WORDS: &[&str] = &[
    "移动到", "移到", "挪到", "搬到", "放到", "放进", "存到", "保存到", "复制到", "拷贝到", "拷到", "另存到", "归档到", "移动", "挪动", "move to", "move it", "move this", "copy to", "copy it", "copy this",
    "save to", "save it", "put it", "이동", "옮겨", "복사",
];
const KNOWN_PLACES: &[&str] = &["桌面", "下载", "文档", "图片", "视频", "音乐", "desktop", "downloads", "documents", "pictures", "videos", "movies", "影片", "바탕화면", "다운로드", "문서"];

pub fn needs_folder(cmd: &str, sel: &Selection) -> bool {
    if !matches!(sel, Selection::Files(_)) {
        return false;
    }
    let c = cmd.to_lowercase();
    if !MOVE_WORDS.iter().any(|w| c.contains(w)) {
        return false;
    }
    // 已经说清楚目标（盘符路径 / 常见位置）就不用再问
    let has_path = c.contains(":\\") || c.contains(":/") || c.contains("\\\\") || c.contains("~/") || c.contains("/users/") || c.contains("/volumes/");
    let has_place = KNOWN_PLACES.iter().any(|p| c.contains(p));
    !(has_path || has_place)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screen() {
        assert!(is_screen_intent("帮我看一下当前页面怎么回事"));
        assert!(is_screen_intent("What's on my screen"));
        assert!(!is_screen_intent("把这个文件转成PDF"));
    }
    #[test]
    fn folder() {
        let f = Selection::Files(vec!["C:\\a.docx".into()]);
        assert!(needs_folder("把这个文件移到某一个文件夹", &f));
        assert!(!needs_folder("把这个文件移到桌面", &f));
        assert!(!needs_folder("转成PDF", &f));
        assert!(!needs_folder("移到某个文件夹", &Selection::Text("x".into())));
    }
}

// ───────────────────────── 记忆指令 ─────────────────────────

pub enum MemCmd {
    Remember(String),
    Forget(String),
}

fn strip_any<'a>(s: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes.iter().find_map(|p| s.strip_prefix(p))
}

/// "记住：我在韩国大田" / "忘掉 xxx" 这类指令本地直接处理，不用去问 Agent（瞬间完成）
pub fn memory_command(text: &str) -> Option<MemCmd> {
    let mut s = text.trim().trim_start_matches(['，', ',', '。', '.', ' ']);
    for _ in 0..2 {
        if let Some(r) = strip_any(s, &["请你", "请", "帮我", "麻烦你", "你"]) {
            s = r.trim_start();
        }
    }
    let clean = |x: &str| x.trim().trim_start_matches(['：', ':', '，', ',', '、', ' ']).trim().trim_end_matches(['。', '.']).trim().to_string();
    if let Some(rest) = strip_any(s, &["记住", "记一下", "记下来", "记下"]) {
        let m = clean(rest);
        if m.chars().count() >= 2 {
            return Some(MemCmd::Remember(m));
        }
    }
    let low = s.to_lowercase();
    if let Some(rest) = low.strip_prefix("remember ") {
        let m = clean(rest.trim_start_matches("that ").trim_start_matches("this: "));
        if m.chars().count() >= 3 {
            // 取原文（保留大小写）
            let cut = s.len() - rest.len();
            return Some(MemCmd::Remember(clean(s[cut..].trim_start_matches("that ").trim_start_matches("this: "))));
        }
    }
    if let Some(rest) = strip_any(s, &["忘掉", "忘记", "删除记忆", "删掉记忆", "清除记忆"]) {
        let k = clean(rest.trim_start_matches("记忆里的").trim_start_matches("记忆里").trim_start_matches("关于"));
        let all = ["所有记忆", "全部记忆", "所有", "全部", "一切", ""].contains(&k.as_str()) && (s.starts_with("忘掉") || s.contains("记忆"));
        if all {
            return Some(MemCmd::Forget("*".into()));
        }
        if !k.is_empty() {
            return Some(MemCmd::Forget(k));
        }
    }
    None
}
