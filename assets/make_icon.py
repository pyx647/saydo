"""生成 SayDo 自己的图标（assets/icon.ico + icon.png）：紫蓝渐变圆角方块 + 白色四角星 + 小星。
python assets/make_icon.py"""
import math
import os

from PIL import Image, ImageDraw, ImageFilter

S = 1024
HERE = os.path.dirname(os.path.abspath(__file__))


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def gradient():
    img = Image.new("RGB", (S, S))
    px = img.load()
    c0, c1, c2 = (183, 156, 255), (122, 92, 240), (66, 52, 190)
    for y in range(S):
        for x in range(S):
            t = (x * 0.35 + y * 0.65) / S   # 左上亮 → 右下深
            px[x, y] = lerp(c0, c1, t / 0.5) if t < 0.5 else lerp(c1, c2, (t - 0.5) / 0.5)
    return img


def star(draw, cx, cy, r, fill):
    """四角星（凹边）"""
    pts = []
    n = 4
    for i in range(n * 2):
        ang = -math.pi / 2 + i * math.pi / n
        rad = r if i % 2 == 0 else r * 0.26
        pts.append((cx + rad * math.cos(ang), cy + rad * math.sin(ang)))
    draw.polygon(pts, fill=fill)


bg = gradient()
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle((24, 24, S - 24, S - 24), radius=230, fill=255)

# 高光
hl = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(hl).ellipse((-200, -420, S + 200, 380), fill=(255, 255, 255, 46))
bg = bg.convert("RGBA")
bg.alpha_composite(hl)

# 星（带柔和阴影）
glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
gd = ImageDraw.Draw(glow)
star(gd, 470, 520, 330, (40, 20, 120, 130))
glow = glow.filter(ImageFilter.GaussianBlur(26))
bg.alpha_composite(glow)
fg = Image.new("RGBA", (S, S), (0, 0, 0, 0))
fd = ImageDraw.Draw(fg)
star(fd, 462, 508, 330, (255, 255, 255, 255))
star(fd, 748, 292, 118, (255, 255, 255, 235))
star(fd, 770, 742, 70, (255, 255, 255, 200))
bg.alpha_composite(fg)

out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
out.paste(bg, (0, 0), mask)
out.resize((256, 256), Image.LANCZOS).save(os.path.join(HERE, "icon.png"))
out.save(os.path.join(HERE, "icon.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
print("ok")
