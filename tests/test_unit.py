"""单元测试：路由规则、审批规则、FILE: 行解析、热键规格解析、配置合并。运行：python -m unittest discover -s tests -p "test_*.py" （在 saydo 目录）"""
import os
import tempfile
import unittest

from app import agent, router, tools
from app import winapi as w


class RouterTests(unittest.TestCase):
    def test_screen_intent(self):
        self.assertTrue(router.is_screen_intent("帮我看一下当前页面怎么回事"))
        self.assertTrue(router.is_screen_intent("What's on my screen"))
        self.assertTrue(router.is_screen_intent("이 페이지 요약해줘"))
        self.assertFalse(router.is_screen_intent("把这个文件转成PDF"))

    def test_needs_folder(self):
        self.assertTrue(router.needs_folder("把这个文件移到某一个文件夹", "files"))
        self.assertTrue(router.needs_folder("move this somewhere", "files"))
        self.assertTrue(router.needs_folder("move it to a folder", "files"))
        self.assertFalse(router.needs_folder("把这个文件移到桌面", "files"))
        self.assertFalse(router.needs_folder("移到 D:\\backup", "files"))
        self.assertFalse(router.needs_folder("转成PDF", "files"))
        self.assertFalse(router.needs_folder("移到某个文件夹", "text"))


class ApproveTests(unittest.TestCase):
    def test_balanced(self):
        d = tools.decide
        self.assertEqual(d("balanced", "Bash", {"command": "ffmpeg -i a.mp4 b.gif"}), tools.ALLOW)
        self.assertEqual(d("balanced", "Bash", {"command": "Remove-Item -Recurse C:\\x"}), tools.ASK)
        self.assertEqual(d("balanced", "Bash", {"command": "git push origin main"}), tools.ASK)
        self.assertEqual(d("balanced", "Edit", {}), tools.ALLOW)
        self.assertEqual(d("balanced", "mcp__claude_ai_Gmail__search_threads", {}), tools.ALLOW)
        self.assertEqual(d("balanced", "mcp__claude_ai_Gmail__send_message", {}), tools.ASK)
        self.assertEqual(d("balanced", "mcp__x__create_draft", {}), tools.ASK)
        self.assertEqual(d("balanced", "mcp__saydo__pick_folder", {}), tools.ALLOW)

    def test_careful_and_auto(self):
        self.assertEqual(tools.decide("careful", "Edit", {}), tools.ASK)
        self.assertEqual(tools.decide("careful", "Read", {}), tools.ALLOW)
        self.assertEqual(tools.decide("auto", "mcp__x__send_message", {}), tools.ALLOW)


class ExtractFilesTests(unittest.TestCase):
    def test_file_lines(self):
        with tempfile.NamedTemporaryFile(delete=False, suffix=".pdf") as f:
            path = f.name
        try:
            text, files = agent.extract_files(f"已完成。\n\nFILE: {path}\nFILE: C:\\不存在\\x.pdf\n")
            self.assertEqual(files, [path])
            self.assertEqual(text, "已完成。")
        finally:
            os.remove(path)


class HotkeySpecTests(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(w.HotkeyHook.parse("Ctrl+Alt+Space"), ("kb", True, True, False, False, 0x20))
        self.assertEqual(w.HotkeyHook.parse("Mouse:XButton2"), ("mouse", 2))
        self.assertEqual(w.HotkeyHook.parse("Shift+F9"), ("kb", False, False, True, False, 0x78))
        self.assertIsNone(w.HotkeyHook.parse("Ctrl+???"))


class WavTests(unittest.TestCase):
    def test_resample_passthrough_and_wav(self):
        from app import stt
        pcm = b"\x00\x01" * 1600
        wav = stt.to_wav(pcm)
        self.assertTrue(wav.startswith(b"RIFF"))
        self.assertEqual(len(wav), 44 + len(pcm))


if __name__ == "__main__":
    unittest.main()
