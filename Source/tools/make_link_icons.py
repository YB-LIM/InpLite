"""Render the developer-link icons (assets/link_*.rgba, 64x64 white + alpha, tinted gray in the app).
GitHub and ResearchGate: Simple Icons (CC0-1.0, npm package simple-icons).
LinkedIn: Font Awesome Free brands (CC BY 4.0, npm package @fortawesome/fontawesome-free).
Usage: python make_link_icons.py <simple-icons/icons dir> <fontawesome svgs/brands dir>"""
import io, sys
import cairosvg
from PIL import Image

si, fa = sys.argv[1], sys.argv[2]
src = {'github': f'{si}/github.svg', 'researchgate': f'{si}/researchgate.svg', 'linkedin': f'{fa}/linkedin.svg'}
for k, p in src.items():
    im = Image.open(io.BytesIO(cairosvg.svg2png(url=p, output_width=64, output_height=64))).convert('RGBA')
    c = Image.new('RGBA', (64, 64), (0, 0, 0, 0))
    c.paste(im, ((64 - im.size[0]) // 2, (64 - im.size[1]) // 2))
    c = Image.merge('RGBA', (Image.new('L', c.size, 255),) * 3 + (c.split()[3],))
    open(f'assets/link_{k}.rgba', 'wb').write(c.tobytes())
