#!/usr/bin/env python3
"""Generate the InpLite icon: assets/icon.png (256), assets/icon.ico (multi-size), assets/icon64.rgba"""
import sys
from PIL import Image, ImageDraw, ImageFont
out = sys.argv[1]
S = 1024
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
# rounded square with vertical gradient
grad = Image.new("RGBA", (S, S))
gd = ImageDraw.Draw(grad)
top, bot = (38, 92, 190), (22, 52, 122)
for y in range(S):
    t = y / (S - 1)
    c = tuple(int(top[i] * (1 - t) + bot[i] * t) for i in range(3)) + (255,)
    gd.line([(0, y), (S, y)], fill=c)
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([40, 40, S - 40, S - 40], radius=210, fill=255)
img.paste(grad, (0, 0), mask)
d = ImageDraw.Draw(img)
# asterisk (keyword marker)
font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf", 520)
d.text((150, 60), "*", font=font, fill=(255, 255, 255, 255))
# "data lines"
W = (255, 255, 255, 235)
for i, (x0, x1) in enumerate([(470, 860), (200, 860), (200, 700), (200, 820)]):
    y = 470 + i * 120
    d.rounded_rectangle([x0, y, x1, y + 62], radius=31, fill=W if i else (255, 214, 102, 255))
big = img.resize((256, 256), Image.LANCZOS)
big.save(f"{out}/icon.png")
img.resize((256, 256), Image.LANCZOS).save(f"{out}/icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
small = img.resize((64, 64), Image.LANCZOS)
open(f"{out}/icon64.rgba", "wb").write(small.tobytes())
print("ok")
