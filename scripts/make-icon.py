"""Иконка Стенографа: звуковые столбики переходят в строки текста."""
from PIL import Image, ImageDraw

S = 1024
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
d.rounded_rectangle([40, 40, S - 40, S - 40], radius=230, fill=(14, 14, 16, 255))
ink = (244, 241, 234, 255)
dim = (244, 241, 234, 120)
cy = S // 2
w = 58
# Звук: столбики разной высоты.
for x, h in [(210, 150), (310, 420), (410, 260), (510, 560)]:
    d.rounded_rectangle([x - w // 2, cy - h // 2, x + w // 2, cy + h // 2], radius=w // 2, fill=ink)
# Текст: строки, в которые превращается звук.
for y, x2, c in [(cy - 150, 800, dim), (cy, 840, ink), (cy + 150, 760, dim)]:
    d.rounded_rectangle([600, y - w // 2, x2, y + w // 2], radius=w // 2, fill=c)
img.save("assets/icon.png")
